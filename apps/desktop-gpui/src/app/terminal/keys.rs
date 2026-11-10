//! What a keystroke does in the terminal: the terminal's own shortcuts (copy, paste, select
//! all, history), or a key for Ghostty's encoder (`onKeyDown` and `keyCodes.ts` in the web's
//! Ghostty surface). Text the encoder would only type back arrives through the input handler
//! instead, so IME and dead keys keep working.

use std::borrow::Cow;

use gpui_kit::{Keystroke, NativeKeyCode};
use libghostty_vt::key::Key;

use super::core::{Core, KeyAction as Press, KeyInput, Locks, Modifiers};
use super::key_codes;

/// A key's identity across its press and release (`event.code`): the platform's code for the
/// physical key, so a release matches its press whatever Shift, the layout or Num Lock did in
/// between. Without one, its W3C code guessed from the name, or the name.
#[derive(Clone, Debug, PartialEq, Eq)]
enum KeyId {
    Native(NativeKeyCode),
    Code(Key),
    Name(String),
}

/// A key event's physical key: the code the encoder hears and the identity its release matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PhysicalKey {
    id: KeyId,
    code: Key,
}

impl PhysicalKey {
    /// The key of a keystroke and the platform's code for it (`Window::native_key`).
    pub fn of(keystroke: &Keystroke, native: Option<NativeKeyCode>) -> Self {
        let code = key_codes::key_code(&keystroke.key, native);
        let id = match native {
            Some(native) => KeyId::Native(native),
            None if code != Key::Unidentified => KeyId::Code(code),
            None => KeyId::Name(keystroke.key.clone()),
        };
        Self { id, code }
    }
}

/// Typing across key events: the keys whose press the program heard, and an IME or dead-key
/// composition in progress.
#[derive(Debug, Default)]
pub(super) struct Keyboard {
    /// `encodedKeyCodes`: these keys' releases go to the program too, as the key they pressed.
    encoded: Vec<(KeyId, Key)>,
    /// The marked text's length in UTF-16 units.
    preedit: usize,
    /// A Windows dead key waits for the key that completes it; Windows marks no text for it.
    dead_key: bool,
}

impl Keyboard {
    /// A fresh press proves a release was missed.
    pub fn forget_missed_release(&mut self, key: &PhysicalKey, held: bool) {
        if !held {
            self.forget(&key.id);
        }
    }

    /// The code the key's press was encoded with, if it was.
    fn forget(&mut self, id: &KeyId) -> Option<Key> {
        let index = self.encoded.iter().position(|(held, _)| held == id)?;
        Some(self.encoded.remove(index).1)
    }

    /// The input handler's marked text: non-empty while the IME or a dead key composes.
    pub fn set_preedit(&mut self, text: &str) {
        self.preedit = text.encode_utf16().count();
    }

    /// Focus left mid-composition: the next keys belong to the program.
    pub fn cancel_composition(&mut self) {
        self.preedit = 0;
        self.dead_key = false;
    }

    /// The marked text's range, while there is one.
    pub fn preedit(&self) -> Option<std::ops::Range<usize>> {
        (self.preedit > 0).then_some(0..self.preedit)
    }

    /// The composition's committed text, which the terminal sends as typed.
    pub fn commit<'a>(&mut self, text: &'a str) -> Option<&'a str> {
        self.preedit = 0;
        self.dead_key = false;
        (!text.is_empty()).then_some(text)
    }

    /// `isTerminalCompositionKey`: keys mid-composition belong to the IME. X11 Compose sequences
    /// mark no text, but GPUI strips the character from each key they swallow. A Windows dead
    /// key carries its spacing character and asks for character input, and the next key that
    /// types carries the composed character; encoding either would skip the `TranslateMessage`
    /// that composes and commits it. Keys that type nothing leave the dead key waiting.
    pub fn composing(&mut self, keystroke: &Keystroke, prefer_character_input: bool) -> bool {
        let m = &keystroke.modifiers;
        let plain = !(m.control || m.alt || m.platform || m.function);
        let completes_dead_key = keystroke.key_char.is_some() && std::mem::take(&mut self.dead_key);
        if plain && prefer_character_input {
            self.dead_key = !completes_dead_key;
            return true;
        }
        self.preedit > 0
            || (plain && completes_dead_key)
            || (plain && keystroke.key_char.is_none() && single_char(&keystroke.key).is_some_and(|c| !c.is_control()))
    }

    /// `encodeKey` for a press: bytes for the program, or `None` when there is nothing to send
    /// or the input handler types the text, so IME and dead keys work. The program hears the
    /// release either way.
    pub fn press(&mut self, core: &mut Core, key: &PhysicalKey, input: KeyInput<'_>) -> Option<String> {
        // The US table's guess at Shift's unshifted character, where the key shows it wrong
        // (German Shift+8 types `(`, which US types with 9), would report another key.
        let fits = |c: char| c.is_alphabetic() || key_codes::from_name(c.encode_utf8(&mut [0; 4])) == key.code;
        let unshifted = input.unshifted.filter(|&c| !input.mods.shift || fits(c));
        let input = KeyInput { code: key.code, unshifted, ..input };
        let data = core.encode_key(&input);
        if data.is_empty() {
            return None;
        }
        if !self.encoded.iter().any(|(held, _)| *held == key.id) {
            self.encoded.push((key.id.clone(), key.code));
        }
        (Some(data.as_str()) != input.text.as_deref()).then_some(data)
    }

    /// Kitty sessions that asked for releases hear them, for the keys whose press they heard.
    pub fn release(
        &mut self,
        core: &mut Core,
        keystroke: &Keystroke,
        key: &PhysicalKey,
        locks: Locks,
        mac: bool,
    ) -> Option<String> {
        let code = self.forget(&key.id)?;
        if self.preedit > 0 {
            return None;
        }
        let data = core.encode_key(&KeyInput { code, locks, ..key_input(keystroke, Press::Release, mac) });
        (!data.is_empty()).then_some(data)
    }
}

/// The keystroke with the Shift the window saw held: GPUI on Windows and Linux reports Shift+/
/// as `?` with Shift consumed, where the web still sees Shift (Kitty reports it).
pub(super) fn with_held_shift(keystroke: &Keystroke, shift: bool) -> Keystroke {
    let mut keystroke = keystroke.clone();
    let caseless =
        single_char(&keystroke.key).is_some_and(|c| !c.is_control() && !c.is_lowercase() && !c.is_uppercase());
    if shift && caseless {
        keystroke.modifiers.shift = true;
    }
    keystroke
}

/// What the key does in the terminal itself, before anything is sent.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum KeyAction<'a> {
    Send(&'static str),
    /// A key for the encoder.
    Encode(KeyInput<'a>),
    /// Ctrl+C or Ctrl+Insert over a selection; Shift keeps the selection.
    Copy {
        keep_selection: bool,
    },
    Paste,
    /// Ctrl+Shift+A.
    SelectAll,
    /// Shift+PageUp/PageDown by a page, Ctrl+Shift+Home/End to either end of the scrollback.
    Scroll(History),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum History {
    PageUp,
    PageDown,
    Top,
    Bottom,
}

impl History {
    /// The scrollbar key that moves the same way.
    pub fn key(self) -> &'static str {
        match self {
            History::PageUp => "pageup",
            History::PageDown => "pagedown",
            History::Top => "home",
            History::Bottom => "end",
        }
    }
}

/// The alternate screen has no scrollback, so its history keys go to the program.
pub(super) fn key_action(
    keystroke: &Keystroke,
    held: bool,
    alt_screen: bool,
    has_selection: bool,
) -> Option<KeyAction<'_>> {
    key_action_on(keystroke, held, alt_screen, has_selection, cfg!(target_os = "macos"))
}

/// [`key_action`] as on macOS, where ⌘ (not Ctrl) copies, pastes, selects all and jumps
/// through history, and every Ctrl chord reaches the shell (`isTerminalCopyShortcut`).
#[expect(clippy::fn_params_excessive_bools, reason = "the terminal's modes, and the platform as a test seam")]
fn key_action_on(
    keystroke: &Keystroke,
    held: bool,
    alt_screen: bool,
    has_selection: bool,
    mac: bool,
) -> Option<KeyAction<'_>> {
    let m = &keystroke.modifiers;
    let key = keystroke.key.as_str();
    if mac && m.platform && !m.control && !m.alt && !m.function {
        return match (key, m.shift) {
            ("a", false) => Some(KeyAction::SelectAll),
            // macOS keeps the selection after a copy.
            ("c", _) if has_selection => Some(KeyAction::Copy { keep_selection: true }),
            ("v", false) => Some(KeyAction::Paste),
            ("home", true) if !alt_screen => Some(KeyAction::Scroll(History::Top)),
            ("end", true) if !alt_screen => Some(KeyAction::Scroll(History::Bottom)),
            // `isTerminalClearShortcut`, `terminalNavigationShortcutData`, `terminalDeleteShortcutData`.
            ("k", false) => Some(KeyAction::Send("\x0c")),
            ("left", false) => Some(KeyAction::Send("\x01")),
            ("right", false) => Some(KeyAction::Send("\x05")),
            ("backspace", false) => Some(KeyAction::Send("\x15")),
            _ => None,
        };
    }
    if m.platform || m.function {
        return None;
    }
    let (ctrl, alt, shift) = (m.control, m.alt, m.shift);
    // Off macOS, Ctrl (with Shift where the shell needs the plain chord) is the terminal's own.
    let shortcuts = !mac;
    if shortcuts && ctrl && shift && !alt && key == "a" {
        return Some(KeyAction::SelectAll);
    }
    if shift && !alt && !alt_screen && (shortcuts || !ctrl) {
        let scroll = match (key, ctrl) {
            ("pageup", false) => Some(History::PageUp),
            ("pagedown", false) => Some(History::PageDown),
            ("home", true) => Some(History::Top),
            ("end", true) => Some(History::Bottom),
            _ => None,
        };
        if let Some(scroll) = scroll {
            return Some(KeyAction::Scroll(scroll));
        }
    }
    // `isTerminalCopyShortcut` / `isTerminalPasteShortcut` on Windows and Linux.
    if shortcuts && has_selection && ctrl && !alt && (key == "c" || (key == "insert" && !shift)) {
        return Some(KeyAction::Copy { keep_selection: shift });
    }
    if shortcuts && ((ctrl && !alt && key == "v") || (shift && !ctrl && !alt && key == "insert")) {
        return Some(KeyAction::Paste);
    }
    // `terminalNavigationShortcutData`: Alt+arrows move by word.
    if alt && !ctrl && !shift {
        match key {
            "left" => return Some(KeyAction::Send("\x1bb")),
            "right" => return Some(KeyAction::Send("\x1bf")),
            _ => {}
        }
    }
    // A macOS Option dead key composes through the input handler; a character Option typed is
    // encoded with Option consumed (`ghosttyConsumedMods`), so plain sessions still get the text.
    if mac && alt && !ctrl && keystroke.key_char.is_none() && produces_character(keystroke) {
        return None;
    }
    // `isTerminalAltGraphText`: AltGr arrives as Ctrl+Alt; its character is text.
    if ctrl && alt && keystroke.key_char.as_deref().is_some_and(|c| c != key) && produces_character(keystroke) {
        return None;
    }
    Some(KeyAction::Encode(key_input(keystroke, if held { Press::Repeat } else { Press::Press }, mac)))
}

fn single_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    chars.next().filter(|_| chars.next().is_none())
}

/// The key types one printable character, or a dead key's base letter.
fn produces_character(keystroke: &Keystroke) -> bool {
    let text = keystroke.key_char.as_deref().filter(|c| !c.is_empty()).unwrap_or(&keystroke.key);
    single_char(text).is_some_and(|c| !c.is_control())
}

/// The keystroke as Ghostty's encoder reads a `KeyboardEvent` (`encodeKey`); its code is the one the
/// name guesses until [`Keyboard`] gives it the physical key.
pub(super) fn key_input(keystroke: &Keystroke, action: Press, mac: bool) -> KeyInput<'_> {
    let m = &keystroke.modifiers;
    let mods = Modifiers { shift: m.shift, ctrl: m.control, alt: m.alt, meta: m.platform };
    let base = key_character(&keystroke.key);
    // `event.key`: the produced character, or the key's own one with Shift applied when the
    // platform omits it (Ctrl chords on Windows and Linux).
    let text = match keystroke.key_char.as_deref().filter(|text| single_char(text).is_some_and(|c| !c.is_control())) {
        Some(text) => Some(Cow::Borrowed(text)),
        None => base.map(|c| match c {
            ' ' => Cow::Borrowed(" "),
            _ if m.shift => Cow::Owned(c.to_uppercase().collect()),
            _ => Cow::Borrowed(keystroke.key.as_str()),
        }),
    };
    // `ghosttyConsumedMods`: Shift that produced the character, and macOS Option that composed it.
    let consumed = if m.control || m.platform || text.is_none() || (m.alt && !mac) {
        Modifiers::default()
    } else {
        Modifiers { shift: m.shift, alt: m.alt, ..Modifiers::default() }
    };
    KeyInput {
        action,
        code: key_codes::from_name(&keystroke.key),
        mods,
        locks: Locks::default(),
        consumed,
        text,
        unshifted: base.and_then(|c| unshifted(c, m.shift)),
    }
}

/// The character a key names: its own, or a space for `"space"`.
fn key_character(key: &str) -> Option<char> {
    if key == "space" {
        return Some(' ');
    }
    single_char(key).filter(|c| !c.is_control())
}

/// `ghosttyUnshiftedCodepoint` without layout data: GPUI names letters in lower case, and a
/// shifted symbol by the US key under it; other shifted symbols stay unknown, as on the web.
fn unshifted(c: char, shift: bool) -> Option<char> {
    if !shift {
        return Some(c.to_ascii_lowercase());
    }
    if c.is_lowercase() || c.is_uppercase() {
        return single_char(&c.to_lowercase().to_string());
    }
    let mut buf = [0; 4];
    crate::app::keymap::unshifted(c.encode_utf8(&mut buf)).and_then(single_char)
}

#[cfg(test)]
mod tests {
    use super::super::core::Core;
    use super::super::core::tests::core;
    use super::*;

    fn key(spec: &'static str) -> Keystroke {
        Keystroke::parse(spec).unwrap()
    }

    /// A keystroke that outlives the test's closures, which return actions borrowing it.
    fn held(spec: &'static str) -> &'static Keystroke {
        Box::leak(Box::new(key(spec)))
    }

    fn typed(spec: &'static str, text: Option<&str>) -> Keystroke {
        Keystroke { key_char: text.map(str::to_owned), ..key(spec) }
    }

    /// The bytes a keystroke sends on Windows and Linux (`mac` false) or macOS; `None` when
    /// the terminal does nothing with it, or the input handler types it.
    fn sent(core: &mut Core, keystroke: &Keystroke, mac: bool) -> Option<String> {
        pressed(&mut Keyboard::default(), core, keystroke, mac)
    }

    /// [`sent`], with the keyboard's state carried across keys.
    fn pressed(keyboard: &mut Keyboard, core: &mut Core, keystroke: &Keystroke, mac: bool) -> Option<String> {
        pressed_as(keyboard, core, keystroke, Platform { mac, ..Platform::default() })
    }

    /// What the platform said about a key besides its keystroke.
    #[derive(Clone, Copy, Default)]
    struct Platform {
        mac: bool,
        /// `KeyDownEvent::prefer_character_input`.
        character_input: bool,
        /// `Window::native_key`.
        native: Option<NativeKeyCode>,
        /// `Window::capslock` and `NativeKey::num_lock`.
        locks: Locks,
    }

    impl Platform {
        fn key(native: NativeKeyCode) -> Self {
            Self { native: Some(native), mac: matches!(native, NativeKeyCode::Mac(_)), ..Self::default() }
        }
    }

    /// The session's `key_down` for a platform-shaped event.
    fn pressed_as(
        keyboard: &mut Keyboard,
        core: &mut Core,
        keystroke: &Keystroke,
        platform: Platform,
    ) -> Option<String> {
        let key = PhysicalKey::of(keystroke, platform.native);
        keyboard.forget_missed_release(&key, false);
        if keyboard.composing(keystroke, platform.character_input) {
            return None;
        }
        match key_action_on(keystroke, false, core.alt_screen(), false, platform.mac)? {
            KeyAction::Send(data) => Some(data.to_owned()),
            KeyAction::Encode(input) => keyboard.press(core, &key, KeyInput { locks: platform.locks, ..input }),
            _ => None,
        }
    }

    /// The session's `key_up` for a platform-shaped event.
    fn released_as(
        keyboard: &mut Keyboard,
        core: &mut Core,
        keystroke: &Keystroke,
        platform: Platform,
    ) -> Option<String> {
        keyboard.release(core, keystroke, &PhysicalKey::of(keystroke, platform.native), platform.locks, platform.mac)
    }

    fn released(keyboard: &mut Keyboard, core: &mut Core, keystroke: &Keystroke) -> Option<String> {
        released_as(keyboard, core, keystroke, Platform::default())
    }

    fn sent_as(core: &mut Core, keystroke: &Keystroke, platform: Platform) -> Option<String> {
        pressed_as(&mut Keyboard::default(), core, keystroke, platform)
    }

    fn send(spec: &'static str) -> Option<String> {
        sent(&mut core(80, 24), &key(spec), false)
    }

    #[test]
    fn macos_keys_use_command() {
        let mac = |spec: &'static str, selection: bool| key_action_on(held(spec), false, false, selection, true);
        assert_eq!(mac("cmd-c", true), Some(KeyAction::Copy { keep_selection: true }));
        assert_eq!(mac("cmd-c", false), None);
        assert_eq!(mac("cmd-v", false), Some(KeyAction::Paste));
        assert_eq!(mac("cmd-a", false), Some(KeyAction::SelectAll));
        assert_eq!(mac("cmd-shift-home", false), Some(KeyAction::Scroll(History::Top)));
        assert_eq!(mac("cmd-k", false), Some(KeyAction::Send("\x0c")));
        assert_eq!(mac("cmd-left", false), Some(KeyAction::Send("\x01")));
        assert_eq!(mac("cmd-right", false), Some(KeyAction::Send("\x05")));
        assert_eq!(mac("cmd-backspace", false), Some(KeyAction::Send("\x15")));
        assert_eq!(mac("cmd-shift-left", false), None);
        assert_eq!(mac("alt-left", false), Some(KeyAction::Send("\x1bb")));
        assert_eq!(mac("cmd-j", false), None, "other ⌘ chords are the app's");
        assert_eq!(mac("shift-pageup", false), Some(KeyAction::Scroll(History::PageUp)));
        let mut core = core(80, 24);
        assert_eq!(sent(&mut core, &key("ctrl-c"), true).as_deref(), Some("\x03"), "Ctrl+C interrupts on macOS");
        assert_eq!(sent(&mut core, &key("ctrl-v"), true).as_deref(), Some("\x16"));
    }

    #[test]
    fn macos_option_text_reaches_the_input_handler() {
        let mut core = core(80, 24);
        let mut mac = |keystroke: Keystroke| sent(&mut core, &keystroke, true);
        assert_eq!(mac(typed("alt-l", Some("@"))), None);
        assert_eq!(mac(typed("alt-shift-a", Some("Å"))), None);
        assert_eq!(mac(typed("alt-space", Some("\u{a0}"))), None);
        assert_eq!(mac(typed("alt-e", None)), None, "a dead key composes through the input handler");
        assert_eq!(mac(typed("alt-enter", Some("\r"))).as_deref(), Some("\x1b\r"));
        assert_eq!(mac(typed("alt-backspace", None)).as_deref(), Some("\x1b\x7f"));
        assert_eq!(mac(typed("alt-left", None)).as_deref(), Some("\x1bb"));
        assert_eq!(mac(typed("ctrl-alt-x", None)).as_deref(), Some("\x1b\x18"));
        // Alt stays Meta off macOS.
        assert_eq!(sent(&mut core, &typed("alt-l", Some("l")), false).as_deref(), Some("\x1bl"));
        assert_eq!(sent(&mut core, &typed("ctrl-alt-q", Some("@")), false), None, "AltGr types its character");
        assert_eq!(sent(&mut core, &typed("ctrl-alt-q", Some("q")), false).as_deref(), Some("\x1b\x11"));
        // A Kitty session that asked for every key hears Option characters as keys, not text.
        core.write(b"\x1b[>11u");
        assert_eq!(sent(&mut core, &typed("alt-l", Some("@")), true).as_deref(), Some("\x1b[108;3u"));
        assert_eq!(sent(&mut core, &typed("alt-e", None), true), None, "a dead key still composes");
        assert_eq!(sent(&mut core, &typed("ctrl-alt-q", Some("@")), false), None, "AltGr still types");
    }

    #[test]
    fn control_and_navigation_keys() {
        for (spec, bytes) in [
            ("ctrl-a", "\x01"),
            ("ctrl-c", "\x03"),
            ("ctrl-l", "\x0c"),
            ("ctrl-space", "\x00"),
            // Ghostty, like Kitty, keeps Ctrl+[, Ctrl+I and Ctrl+M apart from Esc, Tab and Enter.
            ("ctrl-[", "\x1b[91;5u"),
            ("enter", "\r"),
            ("tab", "\t"),
            ("escape", "\x1b"),
            ("backspace", "\x7f"),
            ("ctrl-backspace", "\x08"),
            ("shift-tab", "\x1b[Z"),
            ("up", "\x1b[A"),
            ("ctrl-right", "\x1b[1;5C"),
            ("shift-home", "\x1b[1;2H"),
            ("home", "\x1b[H"),
            ("end", "\x1b[F"),
            ("pageup", "\x1b[5~"),
            ("pagedown", "\x1b[6~"),
            ("delete", "\x1b[3~"),
            ("ctrl-pagedown", "\x1b[6;5~"),
            ("f1", "\x1bOP"),
            ("f5", "\x1b[15~"),
            ("alt-left", "\x1bb"),
            ("alt-x", "\x1bx"),
            ("alt-enter", "\x1b\r"),
            // Windows and Linux give Ctrl+Shift chords no character; the web's `event.key` is "D".
            ("ctrl-shift-d", "\x1b[100;6u"),
        ] {
            assert_eq!(send(spec).as_deref(), Some(bytes), "{spec}");
        }
        // Printable text goes through the input handler.
        assert_eq!(send("a"), None);
        assert_eq!(sent(&mut core(80, 24), &typed("shift-a", Some("A")), false), None);
    }

    #[test]
    fn application_cursor_mode_uses_ss3() {
        let mut core = core(80, 24);
        core.write(b"\x1b[?1h");
        assert_eq!(sent(&mut core, &key("up"), false).as_deref(), Some("\x1bOA"));
        assert_eq!(sent(&mut core, &key("ctrl-up"), false).as_deref(), Some("\x1b[1;5A"));
    }

    #[test]
    fn kitty_sessions_encode_text_presses_repeats_and_releases() {
        let mut core = core(80, 24);
        core.write(b"\x1b[>11u");
        let a = typed("a", Some("a"));
        assert_eq!(sent(&mut core, &a, false).as_deref(), Some("\x1b[97u"));
        assert_eq!(core.encode_key(&key_input(&a, Press::Repeat, false)), "\x1b[97;1:2u");
        assert_eq!(core.encode_key(&key_input(&a, Press::Release, false)), "\x1b[97;1:3u");
        assert_eq!(sent(&mut core, &key("escape"), false).as_deref(), Some("\x1b[27u"));
        assert_eq!(sent(&mut core, &key("ctrl-space"), false).as_deref(), Some("\x1b[32;5u"));
        assert_eq!(sent(&mut core, &typed("space", Some(" ")), false).as_deref(), Some("\x1b[32u"));
        core.write(b"\x1b[<u");
        assert_eq!(sent(&mut core, &a, false), None, "popping the flags restores plain text");
        // Report-events without report-all: the press is text for the input handler, the release a key.
        core.write(b"\x1b[>3u");
        let mut keyboard = Keyboard::default();
        assert_eq!(pressed(&mut keyboard, &mut core, &a, false), None);
        assert_eq!(released(&mut keyboard, &mut core, &a).as_deref(), Some("\x1b[97;1:3u"));
        assert_eq!(released(&mut keyboard, &mut core, &a), None, "one release per press");
    }

    #[test]
    fn shifted_punctuation_keeps_its_shift() {
        let mut core = core(80, 24);
        // GPUI on Windows and Linux reports Shift+/ as "?" with Shift consumed.
        let question = with_held_shift(&typed("?", Some("?")), true);
        assert_eq!(sent(&mut core, &question, false), None, "legacy sessions get the typed text");
        core.write(b"\x1b[>15u");
        assert_eq!(sent(&mut core, &question, false).as_deref(), Some("\x1b[47:63;2u"));
        let unshifted = with_held_shift(&typed("?", Some("?")), false);
        assert_eq!(sent(&mut core, &unshifted, false).as_deref(), Some("\x1b[63::47u"), "a layout's own ?, unshifted");
        // Shift let go first: the key comes up as "/", still the key whose press was heard.
        let mut keyboard = Keyboard::default();
        assert_eq!(pressed(&mut keyboard, &mut core, &question, false).as_deref(), Some("\x1b[47:63;2u"));
        assert_eq!(released(&mut keyboard, &mut core, &typed("/", Some("/"))).as_deref(), Some("\x1b[47;1:3u"));
    }

    #[test]
    fn a_release_matches_its_press_on_every_layout() {
        use NativeKeyCode::{Mac, Windows, Xkb};
        let mut core = core(80, 24);
        core.write(b"\x1b[>15u");
        // German Shift+7 types `/`; Shift let go first, the key comes up as `7`. Without the
        // layout's map (`getLayoutMap`) the press is the typed text, as on the web without one.
        for native in [Windows(0x08), Xkb(16), Mac(0x1a)] {
            let platform = Platform::key(native);
            let mut keyboard = Keyboard::default();
            let slash = with_held_shift(&typed("/", Some("/")), true);
            assert_eq!(pressed_as(&mut keyboard, &mut core, &slash, platform), None, "{native:?}");
            let seven = typed("7", Some("7"));
            let release = released_as(&mut keyboard, &mut core, &seven, platform);
            assert_eq!(release.as_deref(), Some("\x1b[55;1:3u"), "{native:?}");
        }
        // German Shift+8 types `(`, which US types with 9: the key is not 9.
        let paren = with_held_shift(&typed("(", Some("(")), true);
        assert_eq!(sent_as(&mut core, &paren, Platform::key(Windows(0x09))), None);
        assert_eq!(sent_as(&mut core, &paren, Platform::key(Windows(0x0a))).as_deref(), Some("\x1b[57:40;2u"), "US 9");
    }

    #[test]
    fn keypad_keys_keep_their_identity() {
        use NativeKeyCode::{Mac, Windows, Xkb};
        let mut core = core(80, 24);
        let keypad = |core: &mut Core, keystroke: Keystroke, native| sent_as(core, &keystroke, Platform::key(native));
        // Keypad 1 and Enter on each platform.
        let ones = [Windows(0x4f), Xkb(87), Mac(0x53)];
        let enters = [Windows(0xe01c), Xkb(104), Mac(0x4c)];
        for native in ones {
            assert_eq!(keypad(&mut core, typed("1", Some("1")), native), None, "legacy sessions get the typed digit");
        }
        for native in enters {
            assert_eq!(keypad(&mut core, key("enter"), native).as_deref(), Some("\r"), "{native:?}");
        }
        core.write(b"\x1b[>11u");
        for native in ones {
            assert_eq!(keypad(&mut core, typed("1", Some("1")), native).as_deref(), Some("\x1b[57400u"), "{native:?}");
        }
        for native in enters {
            assert_eq!(keypad(&mut core, key("enter"), native).as_deref(), Some("\x1b[57414u"), "{native:?}");
        }
        // Linux names keypad keys by keysym (KP_Add is "add"); Windows and macOS by character.
        assert_eq!(keypad(&mut core, typed("add", Some("+")), Xkb(86)).as_deref(), Some("\x1b[57413u"));
        assert_eq!(keypad(&mut core, typed("+", Some("+")), Windows(0x4e)).as_deref(), Some("\x1b[57413u"));
        assert_eq!(keypad(&mut core, typed("+", Some("+")), Mac(0x45)).as_deref(), Some("\x1b[57413u"));
        assert_eq!(
            keypad(&mut core, typed("1", Some("1")), Mac(0x12)).as_deref(),
            Some("\x1b[49u"),
            "the main row's 1"
        );
    }

    #[test]
    fn keypad_keys_with_numlock_off_are_its_navigation_keys() {
        use NativeKeyCode::{Windows, Xkb};
        let mut core = core(80, 24);
        let end = key("end");
        for native in [Windows(0x4f), Xkb(87)] {
            assert_eq!(sent_as(&mut core, &end, Platform::key(native)).as_deref(), Some("\x1b[F"), "a shell's End");
        }
        let keypad_one = Platform::key(Windows(0x4f));
        assert_eq!(sent_as(&mut core, &key("up"), Platform::key(Windows(0x48))).as_deref(), Some("\x1b[A"));
        // Application keypad mode (DECKPAM).
        core.write(b"\x1b=");
        assert_eq!(sent_as(&mut core, &end, keypad_one).as_deref(), Some("\x1b[F"));
        assert_eq!(sent_as(&mut core, &typed("1", Some("1")), keypad_one), None, "digits while mode 1035 is set");
        core.write(b"\x1b[?1035l");
        assert_eq!(sent_as(&mut core, &typed("1", Some("1")), keypad_one).as_deref(), Some("\x1bOq"));
        assert_eq!(sent_as(&mut core, &end, keypad_one).as_deref(), Some("\x1b[F"));
        core.write(b"\x1b>\x1b[>11u");
        assert_eq!(sent_as(&mut core, &end, keypad_one).as_deref(), Some("\x1b[57424u"), "KP_END, not END");
        // NumLock turned off while keypad 1 is held: it comes up as End, still the key pressed.
        let mut keyboard = Keyboard::default();
        let one = typed("1", Some("1"));
        assert_eq!(pressed_as(&mut keyboard, &mut core, &one, keypad_one).as_deref(), Some("\x1b[57400u"));
        assert_eq!(released_as(&mut keyboard, &mut core, &end, keypad_one).as_deref(), Some("\x1b[57400;1:3u"));
        // A layout-free name on a remapped key wins over its position (XKB's caps:escape).
        assert_eq!(sent_as(&mut core, &key("escape"), Platform::key(Xkb(66))).as_deref(), Some("\x1b[27u"));
    }

    #[test]
    fn composition_keeps_its_keys_from_the_encoder() {
        let mut core = core(80, 24);
        // Report-all: every key would otherwise be encoded.
        core.write(b"\x1b[>11u");
        let mut keyboard = Keyboard::default();
        let e = typed("e", Some("e"));
        keyboard.set_preedit("´");
        assert_eq!(pressed(&mut keyboard, &mut core, &e, false), None);
        assert_eq!(pressed(&mut keyboard, &mut core, &key("enter"), false), None, "Enter commits");
        assert_eq!(released(&mut keyboard, &mut core, &e), None);
        assert_eq!(keyboard.commit("é"), Some("é"));
        // X11 Compose sequences show no marked text; their keys carry no character until composed.
        assert_eq!(pressed(&mut keyboard, &mut core, &key("a"), false), None);
        assert_eq!(pressed(&mut keyboard, &mut core, &key("shift-a"), false), None);
    }

    #[test]
    fn a_windows_dead_key_composes_with_the_key_that_completes_it() {
        use NativeKeyCode::Windows;
        let mut core = core(80, 24);
        // Report-all: every key would otherwise be encoded.
        core.write(b"\x1b[>11u");
        let mut keyboard = Keyboard::default();
        // US-International: the dead key carries its spacing character and asks for character
        // input; the key after it carries the composed character. Both must reach
        // `TranslateMessage`, which commits `é` through the input handler.
        let quote = typed("'", Some("'"));
        let dead = Platform { character_input: true, ..Platform::key(Windows(0x28)) };
        let e_key = Platform::key(Windows(0x12));
        let e_acute = typed("e", Some("é"));
        assert_eq!(pressed_as(&mut keyboard, &mut core, &quote, dead), None);
        assert_eq!(keyboard.preedit(), None, "Windows marks no text for a dead key");
        assert_eq!(pressed_as(&mut keyboard, &mut core, &e_acute, e_key), None);
        assert_eq!(released_as(&mut keyboard, &mut core, &e_acute, e_key), None);
        assert_eq!(keyboard.commit("é"), Some("é"), "the committed text goes out once");
        let e = typed("e", Some("e"));
        assert_eq!(pressed_as(&mut keyboard, &mut core, &e, e_key).as_deref(), Some("\x1b[101u"), "composition over");
        // A key that types nothing leaves the dead key waiting, and still reaches the program.
        let left = key("left");
        let left_key = Platform::key(Windows(0xe04b));
        assert_eq!(pressed_as(&mut keyboard, &mut core, &quote, dead), None);
        assert_eq!(pressed_as(&mut keyboard, &mut core, &left, left_key), sent_as(&mut core, &left, left_key));
        assert_eq!(pressed_as(&mut keyboard, &mut core, &e_acute, e_key), None);
        assert_eq!(keyboard.commit("é"), Some("é"));
        // Leaving the terminal cancels it.
        assert_eq!(pressed_as(&mut keyboard, &mut core, &quote, dead), None);
        keyboard.cancel_composition();
        assert_eq!(pressed_as(&mut keyboard, &mut core, &e, e_key).as_deref(), Some("\x1b[101u"));
    }

    #[test]
    fn lock_keys_reach_kitty_programs_that_ask_for_every_key() {
        use NativeKeyCode::{Windows, Xkb};
        let mut core = core(80, 24);
        let caps = Platform { locks: Locks { caps: true, num: false }, ..Platform::key(Xkb(38)) };
        let a = typed("a", Some("A"));
        assert_eq!(sent_as(&mut core, &a, caps), None, "legacy sessions get the typed A");
        core.write(b"\x1b[>11u");
        let mut keyboard = Keyboard::default();
        assert_eq!(pressed_as(&mut keyboard, &mut core, &a, caps).as_deref(), Some("\x1b[97;65u"));
        assert_eq!(released_as(&mut keyboard, &mut core, &a, caps).as_deref(), Some("\x1b[97;65:3u"));
        let num = Platform { locks: Locks { caps: false, num: true }, ..Platform::key(Windows(0x4f)) };
        assert_eq!(sent_as(&mut core, &typed("1", Some("1")), num).as_deref(), Some("\x1b[57400;129u"));
    }

    #[test]
    fn copy_needs_a_selection_and_paste_always_works() {
        let action = |spec: &'static str, selection: bool| key_action_on(held(spec), false, false, selection, false);
        assert_eq!(action("ctrl-c", true), Some(KeyAction::Copy { keep_selection: false }));
        assert_eq!(action("ctrl-shift-c", true), Some(KeyAction::Copy { keep_selection: true }));
        assert_eq!(action("ctrl-insert", true), Some(KeyAction::Copy { keep_selection: false }));
        assert_eq!(action("ctrl-v", false), Some(KeyAction::Paste));
        assert_eq!(action("ctrl-shift-v", false), Some(KeyAction::Paste));
        assert_eq!(action("shift-insert", false), Some(KeyAction::Paste));
    }

    #[test]
    fn select_all_and_history_keys_stay_local_except_on_the_alternate_screen() {
        let action = |spec: &'static str, alt_screen: bool| key_action_on(held(spec), false, alt_screen, false, false);
        assert_eq!(action("ctrl-shift-a", false), Some(KeyAction::SelectAll));
        assert_eq!(action("shift-pageup", false), Some(KeyAction::Scroll(History::PageUp)));
        assert_eq!(action("ctrl-shift-end", false), Some(KeyAction::Scroll(History::Bottom)));
        let mut core = core(80, 24);
        core.write(b"\x1b[?1049h");
        assert_eq!(sent(&mut core, &key("shift-pageup"), false).as_deref(), Some("\x1b[5;2~"));
        assert_eq!(sent(&mut core, &key("ctrl-shift-end"), false).as_deref(), Some("\x1b[1;6F"));
    }
}
