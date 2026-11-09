//! What a keystroke does in the terminal: the terminal's own shortcuts (copy, paste, select
//! all, history), or a key for Ghostty's encoder (`onKeyDown` and `keyCodes.ts` in the web's
//! Ghostty surface). Text the encoder would only type back arrives through the input handler
//! instead, so IME and dead keys keep working.

use gpui_kit::Keystroke;

use super::core::{KeyAction as Press, KeyInput, Modifiers};

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
    // macOS Option composes the layout's characters, which arrive as text (`ghosttyConsumedMods`).
    if mac && alt && !ctrl && produces_character(keystroke) {
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

/// The keystroke as Ghostty's encoder reads a `KeyboardEvent` (`encodeKey`).
pub(super) fn key_input(keystroke: &Keystroke, action: Press, mac: bool) -> KeyInput<'_> {
    let m = &keystroke.modifiers;
    let mods = Modifiers { shift: m.shift, ctrl: m.control, alt: m.alt, meta: m.platform };
    let base = single_char(&keystroke.key);
    // `event.key`: the produced character, or the key's own one when the platform omits it.
    let text = keystroke
        .key_char
        .as_deref()
        .filter(|text| single_char(text).is_some_and(|c| !c.is_control()))
        .or_else(|| base.filter(|_| !m.shift).map(|_| keystroke.key.as_str()));
    // `ghosttyConsumedMods`: Shift that produced the character, and macOS Option that composed it.
    let consumed = if m.control || m.platform || text.is_none() || (m.alt && !mac) {
        Modifiers::default()
    } else {
        Modifiers { shift: m.shift, alt: m.alt, ..Modifiers::default() }
    };
    KeyInput {
        action,
        key: &keystroke.key,
        mods,
        consumed,
        text,
        unshifted: base.map(|c| c.to_ascii_lowercase()).filter(|c| !c.is_control()),
    }
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
        match key_action_on(keystroke, false, core.alt_screen(), false, mac)? {
            KeyAction::Send(data) => Some(data.to_owned()),
            KeyAction::Encode(input) => {
                let data = core.encode_key(&input);
                (!data.is_empty() && Some(data.as_str()) != input.text).then_some(data)
            }
            _ => None,
        }
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
        core.write(b"\x1b[<u");
        assert_eq!(sent(&mut core, &a, false), None, "popping the flags restores plain text");
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
