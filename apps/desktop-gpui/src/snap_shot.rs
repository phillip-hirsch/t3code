//! SnapShots: the capture shortcut model and the copy the Settings page and the capture
//! flow show, per platform as the web and Electron word them (`packages/contracts/src/settings.ts`, `apps/desktop/src/snapShot/snapShot.ts`,
//! `DesktopSnapShot.ts`, `apps/web/src/lib/snapShotShortcut.ts`, `SnapShotSettings.logic.ts`).

use serde_json::{Value, json};

use crate::keybindings::{self, Shortcut as Chord};

/// The platform a label or a shortcut is resolved for: `mod` is Cmd on macOS and Ctrl elsewhere,
/// and each names its modifier keys its own way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Mac,
    Windows,
    Linux,
}

impl Os {
    pub const CURRENT: Self = if cfg!(target_os = "macos") {
        Self::Mac
    } else if cfg!(windows) {
        Self::Windows
    } else {
        Self::Linux
    };
}

/// `SnapShotModifier`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Shift,
    Meta,
    Control,
    Alt,
}

impl Modifier {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "shift" => Self::Shift,
            "meta" => Self::Meta,
            "control" => Self::Control,
            "alt" => Self::Alt,
            _ => return None,
        })
    }

    fn id(self) -> &'static str {
        match self {
            Self::Shift => "shift",
            Self::Meta => "meta",
            Self::Control => "control",
            Self::Alt => "alt",
        }
    }

    /// `APPLE_MODIFIER_LABELS` and `OTHER_MODIFIER_LABELS`.
    fn label(self, os: Os) -> &'static str {
        match (self, os) {
            (Self::Shift, _) => "Shift",
            (Self::Meta, Os::Mac) => "Command",
            (Self::Control, Os::Mac) => "Control",
            (Self::Alt, Os::Mac) => "Option",
            (Self::Meta, _) => "Super",
            (Self::Control, _) => "Ctrl",
            (Self::Alt, _) => "Alt",
        }
    }

    /// `modifierKeyLabel`.
    fn key_label(self, os: Os) -> &'static str {
        match (self, os) {
            (Self::Shift, _) => "⇧",
            (Self::Meta, Os::Mac) => "⌘",
            (Self::Control, Os::Mac) => "⌃",
            (Self::Alt, Os::Mac) => "⌥",
            (Self::Meta, Os::Windows) => "⊞",
            (Self::Meta, Os::Linux) => "Super",
            (Self::Control, _) => "Ctrl",
            (Self::Alt, _) => "Alt",
        }
    }
}

/// `SnapShotShortcut`: both keys of one modifier, or a chord with at least one modifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Pair(Modifier),
    Chord(Chord),
}

impl Default for Shortcut {
    /// `DEFAULT_SNAP_SHOT_SHORTCUT`.
    fn default() -> Self {
        Self::Pair(Modifier::Shift)
    }
}

impl Shortcut {
    /// Decodes the stored setting; anything the schema rejects is the default.
    pub fn from_value(value: &Value) -> Self {
        let pair = match value["kind"].as_str() {
            Some("both-shift-keys") => Some(Modifier::Shift),
            Some("modifier-pair") => value["modifier"].as_str().and_then(Modifier::parse),
            _ => None,
        };
        if let Some(modifier) = pair {
            return Self::Pair(modifier);
        }
        serde_json::from_value::<Chord>(value.clone())
            .ok()
            .filter(|chord| !chord.key.is_empty() && has_modifier(chord))
            .map_or_else(Self::default, Self::Chord)
    }

    pub fn to_value(&self) -> Value {
        match self {
            Self::Pair(Modifier::Shift) => json!({ "kind": "both-shift-keys" }),
            Self::Pair(modifier) => json!({ "kind": "modifier-pair", "modifier": modifier.id() }),
            Self::Chord(chord) => json!({
                "key": chord.key, "metaKey": chord.meta_key, "ctrlKey": chord.ctrl_key,
                "shiftKey": chord.shift_key, "altKey": chord.alt_key, "modKey": chord.mod_key,
            }),
        }
    }

    /// `sameSnapShotShortcut`: pairs by modifier, chords by the keys they resolve to.
    pub fn same(&self, other: &Self) -> bool {
        self.same_on(other, Os::CURRENT)
    }

    fn same_on(&self, other: &Self, os: Os) -> bool {
        match (self, other) {
            (Self::Pair(left), Self::Pair(right)) => left == right,
            (Self::Chord(left), Self::Chord(right)) => resolved(left, os) == resolved(right, os),
            _ => false,
        }
    }

    /// `snapShotShortcutKeyLabels`, one per key cap.
    pub fn key_labels(&self) -> Vec<String> {
        self.key_labels_on(Os::CURRENT)
    }

    fn key_labels_on(&self, os: Os) -> Vec<String> {
        match self {
            Self::Pair(modifier) => vec![modifier.key_label(os).into(); 2],
            Self::Chord(chord) => {
                let keys = resolved(chord, os);
                [(keys.ctrl, Modifier::Control), (keys.alt, Modifier::Alt), (keys.shift, Modifier::Shift)]
                    .into_iter()
                    .chain([(keys.meta, Modifier::Meta)])
                    .filter(|(on, _)| *on)
                    .map(|(_, modifier)| modifier.key_label(os).to_owned())
                    .chain([key_label(&chord.key)])
                    .collect()
            }
        }
    }

    /// `formatSnapShotShortcutLabel`.
    pub fn label(&self) -> String {
        self.label_on(Os::CURRENT)
    }

    fn label_on(&self, os: Os) -> String {
        match self {
            Self::Pair(modifier) => pair_label(*modifier, os),
            Self::Chord(chord) => {
                let keys = resolved(chord, os);
                if os == Os::Mac {
                    let glyph = |on: bool, glyph: &'static str| if on { glyph } else { "" };
                    return format!(
                        "{}{}{}{}{}",
                        glyph(keys.ctrl, "⌃"),
                        glyph(keys.alt, "⌥"),
                        glyph(keys.shift, "⇧"),
                        glyph(keys.meta, "⌘"),
                        key_label(&chord.key)
                    );
                }
                let mut parts: Vec<String> =
                    [(keys.ctrl, "Ctrl"), (keys.alt, "Alt"), (keys.shift, "Shift"), (keys.meta, "Meta")]
                        .into_iter()
                        .filter(|(on, _)| *on)
                        .map(|(_, name)| name.to_owned())
                        .collect();
                parts.push(key_label(&chord.key));
                parts.join("+")
            }
        }
    }
}

fn has_modifier(chord: &Chord) -> bool {
    chord.meta_key || chord.ctrl_key || chord.shift_key || chord.alt_key || chord.mod_key
}

/// The keys a chord holds, with `mod` resolved for `os` (`shortcutConflictKey`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Keys {
    key: String,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
}

fn resolved(chord: &Chord, os: Os) -> Keys {
    let mac = os == Os::Mac;
    Keys {
        key: chord.key.clone(),
        ctrl: chord.ctrl_key || (chord.mod_key && !mac),
        alt: chord.alt_key,
        shift: chord.shift_key,
        meta: chord.meta_key || (chord.mod_key && mac),
    }
}

/// `snapShotModifierPairLabel`.
fn pair_label(modifier: Modifier, os: Os) -> String {
    format!("{0} + {0}", modifier.label(os))
}

/// `formatShortcutKeyLabel`.
pub fn key_label(key: &str) -> String {
    match key {
        " " => "Space".into(),
        "escape" => "Esc".into(),
        "arrowup" => "Up".into(),
        "arrowdown" => "Down".into(),
        "arrowleft" => "Left".into(),
        "arrowright" => "Right".into(),
        _ if key.chars().count() == 1 => key.to_uppercase(),
        _ => {
            let mut chars = key.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    }
}

/// A recorded keybinding input (`keybindingFromKeyboardEvent`), or `None` when it has no
/// modifier: a global shortcut without one would take that key from every app.
pub fn chord_from_input(input: &str) -> Option<Shortcut> {
    keybindings::parse_shortcut(input).filter(has_modifier).map(Shortcut::Chord)
}

/// `snapShotShortcutRegistrationFailureMessage`.
pub fn registration_failure_message(shortcut: &Shortcut) -> String {
    match shortcut {
        Shortcut::Pair(modifier) => format!("{} is not available on this system.", pair_label(*modifier, Os::CURRENT)),
        Shortcut::Chord(_) => "This shortcut is already used by the system or another app.".into(),
    }
}

/// `observedPairMessage`.
pub fn observed_pair_message(modifier: Modifier) -> String {
    observed_pair_message_on(modifier, Os::CURRENT)
}

fn observed_pair_message_on(modifier: Modifier, os: Os) -> String {
    let base = format!("{} is observed and cannot be reserved exclusively.", pair_label(modifier, os));
    match (modifier, os) {
        (Modifier::Meta, Os::Windows | Os::Linux) => format!("{base} This key can also open the system's own menu."),
        (Modifier::Alt, Os::Windows) => format!("{base} This key can also activate app menu bars."),
        _ => base,
    }
}

/// `snapShotShortcutSystemConflict`.
pub fn system_conflict(chord: &Chord) -> Option<&'static str> {
    let modifiers = [chord.mod_key, chord.meta_key, chord.ctrl_key, chord.alt_key, chord.shift_key];
    if modifiers.iter().filter(|on| **on).count() != 1 {
        return None;
    }
    if chord.shift_key {
        return Some("Shift combinations are used for typing and text selection. Add another modifier.");
    }
    let key = chord.key.to_lowercase();
    if chord.mod_key {
        return match key.as_str() {
            "a" => Some("This shortcut is Select All in most apps."),
            "c" => Some("This shortcut is Copy in most apps."),
            "f" => Some("This shortcut is Find in most apps."),
            "n" => Some("This shortcut is New in most apps."),
            "o" => Some("This shortcut is Open in most apps."),
            "p" => Some("This shortcut is Print in most apps."),
            "q" => Some("This shortcut is Quit in most apps."),
            "s" => Some("This shortcut is Save in most apps."),
            "t" => Some("This shortcut is New Tab in most apps."),
            "v" => Some("This shortcut is Paste in most apps."),
            "w" => Some("This shortcut is Close Window in most apps."),
            "x" => Some("This shortcut is Cut in most apps."),
            "z" => Some("This shortcut is Undo in most apps."),
            _ => None,
        };
    }
    if chord.ctrl_key && matches!(key.as_str(), "c" | "d" | "z") {
        return Some("This shortcut controls running commands in terminals.");
    }
    if chord.alt_key && key == "tab" {
        return Some("The system uses Alt+Tab to switch apps.");
    }
    if chord.meta_key && matches!(key.as_str(), "l" | " ") {
        return Some("The system already uses this shortcut.");
    }
    None
}

/// `RegisterHotKey`'s modifier flags and virtual key for a chord, or `None` for a key it can't
/// bind; macOS and Linux map the same pair to their own key codes.
#[cfg(any(windows, target_os = "macos"))]
pub fn hotkey(chord: &Chord) -> Option<(u32, u32)> {
    hotkey_on(chord, Os::CURRENT)
}

#[cfg(any(windows, target_os = "macos", test))]
fn hotkey_on(chord: &Chord, os: Os) -> Option<(u32, u32)> {
    let Keys { ctrl, alt, shift, meta, .. } = resolved(chord, os);
    let flags = [(alt, 0x1), (ctrl, 0x2), (shift, 0x4), (meta, 0x8)]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(0, |flags, (_, flag)| flags | flag);
    let key = chord.key.as_str();
    let vk = match key {
        " " => 0x20,
        "enter" => 0x0d,
        "tab" => 0x09,
        "escape" => 0x1b,
        "backspace" => 0x08,
        "delete" => 0x2e,
        "insert" => 0x2d,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "arrowleft" => 0x25,
        "arrowup" => 0x26,
        "arrowright" => 0x27,
        "arrowdown" => 0x28,
        "pause" => 0x13,
        "printscreen" => 0x2c,
        ";" => 0xba,
        "=" | "+" => 0xbb,
        "," => 0xbc,
        "-" => 0xbd,
        "." => 0xbe,
        "/" => 0xbf,
        "`" => 0xc0,
        "[" => 0xdb,
        "\\" => 0xdc,
        "]" => 0xdd,
        "'" => 0xde,
        _ => match key.as_bytes() {
            [c @ b'a'..=b'z'] => u32::from(c.to_ascii_uppercase()),
            [c @ b'0'..=b'9'] => u32::from(*c),
            [b'f', digits @ ..] => match std::str::from_utf8(digits).ok()?.parse::<u32>().ok()? {
                n @ 1..=24 => 0x6f + n,
                _ => return None,
            },
            _ => return None,
        },
    };
    Some((flags, vk))
}

/// `DesktopSnapShotState` as this client knows it: capture is always direct here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub shortcut_registered: bool,
    pub shortcut_message: Option<String>,
    /// Why the last capture failed; a later success clears it.
    pub message: Option<String>,
    /// The shortcut fired since it was registered.
    pub shortcut_verified: bool,
}

/// `snapShotStatus` for direct capture.
pub fn status(state: &State, enabled: bool) -> &'static str {
    if !enabled {
        "Turn this on to set up snapshots."
    } else if state.message.is_some() {
        "Capture needs attention"
    } else if state.shortcut_verified {
        "Ready to capture"
    } else if state.shortcut_registered {
        // A direct shortcut has no desktop label to confirm it by.
        "Shortcut saved"
    } else {
        "Finish shortcut setup"
    }
}

/// `snapShotShortcutStatus` for direct capture.
pub fn shortcut_status(state: &State) -> Option<String> {
    if state.shortcut_registered { Some("Shortcut saved.".into()) } else { state.shortcut_message.clone() }
}

/// `DesktopSnapShotError` messages the capture flow shows.
#[cfg(any(windows, target_os = "macos"))]
pub const WINDOW_UNAVAILABLE: &str = "The active window is not available for capture.";
pub const CAPTURE_FAILED: &str = "Could not capture the active window.";
#[cfg(any(windows, target_os = "macos"))]
pub const TOO_LARGE: &str = "The captured window is too large to attach.";

#[cfg(any(windows, target_os = "macos"))]
/// `PROVIDER_SEND_TURN_MAX_IMAGE_BYTES`.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[cfg(any(windows, target_os = "macos", test))]
/// The capture's longest side stays within Electron's thumbnail bounds.
pub const MAX_CAPTURE: (u32, u32) = (2_560, 1_600);

#[cfg(any(windows, target_os = "macos", test))]
/// The size a `width`×`height` capture scales down to so it fits [`MAX_CAPTURE`].
pub fn fit_capture(width: u32, height: u32) -> (u32, u32) {
    let scale = (f64::from(MAX_CAPTURE.0) / f64::from(width.max(1)))
        .min(f64::from(MAX_CAPTURE.1) / f64::from(height.max(1)))
        .min(1.);
    (((f64::from(width) * scale).round() as u32).max(1), ((f64::from(height) * scale).round() as u32).max(1))
}

#[cfg(any(windows, test))]
/// `snapShotAppName` on Windows: the executable's name without `.exe`, else "Window".
pub fn app_name(image_path: &str) -> String {
    let name = image_path.rsplit(['\\', '/']).next().unwrap_or_default().trim();
    let stem = match name.len().checked_sub(4) {
        Some(at) if name.is_char_boundary(at) && name[at..].eq_ignore_ascii_case(".exe") => name[..at].trim(),
        _ => name,
    };
    let name = if stem.is_empty() { name } else { stem };
    bounded(name, 255).unwrap_or_else(|| "Window".into())
}

/// `boundedSnapShotString`: trimmed, NULs dropped, cut to `max` UTF-16 units without splitting a character.
pub fn bounded(value: &str, max: usize) -> Option<String> {
    let value = value.replace('\0', "");
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut units = 0;
    let mut end = value.len();
    for (at, c) in value.char_indices() {
        if units + c.len_utf16() > max {
            end = at;
            break;
        }
        units += c.len_utf16();
    }
    Some(value[..end].trim_end().to_owned())
}

/// The attachment name for a capture taken at `captured_at` (`window-<ISO time with dashes>.png`).
pub fn capture_name(captured_at: &str, extension: &str) -> String {
    format!("window-{}.{extension}", captured_at.replace(':', "-"))
}

/// `SnapShotSource` for a capture.
pub fn source(
    captured_at: &str,
    app_name: &str,
    window_title: &str,
    icon_png: Option<&[u8]>,
    accessibility: Option<Value>,
) -> Value {
    use base64::Engine as _;
    let mut source = json!({
        "kind": "snap-shot",
        "capturedAt": captured_at,
        "appName": app_name,
        "windowTitle": bounded(window_title, 1_000).unwrap_or_default(),
    });
    if let Some(Value::Object(accessibility)) = accessibility {
        source.as_object_mut().expect("an object").extend(accessibility);
    }
    let icon =
        icon_png.map(|png| format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)));
    if let Some(icon) = icon.filter(|icon| icon.len() <= 100_000) {
        source["appIconDataUrl"] = icon.into();
    }
    source
}

/// `SNAP_SHOT_ACCESSIBLE_TEXT_MAX_CHARS` and `SNAP_SHOT_ACCESSIBILITY_MAX_NODES`.
pub const ACCESSIBLE_TEXT_MAX: usize = 32_000;
#[cfg(windows)]
pub const ACCESSIBILITY_MAX_NODES: usize = 10_000;

/// `accessibleWindowText`: element names and values in tree order, each once, one per line,
/// cut to `max` UTF-16 units without splitting a character.
#[cfg(any(windows, test))]
pub fn accessible_text(candidates: impl IntoIterator<Item = String>, max: usize) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut text = String::new();
    let mut units = 0;
    for candidate in candidates {
        let candidate = candidate.replace('\0', "");
        let candidate = candidate.trim();
        if candidate.is_empty() || seen.contains(candidate) {
            continue;
        }
        let separator = usize::from(!text.is_empty());
        if units + separator >= max {
            break;
        }
        if separator == 1 {
            text.push('\n');
            units += 1;
        }
        for c in candidate.chars() {
            if units + c.len_utf16() > max {
                return text;
            }
            text.push(c);
            units += c.len_utf16();
        }
        seen.insert(candidate.to_owned());
    }
    text
}

/// The source's accessibility fields for a flat read (`accessibilityReadSnapshot` when only
/// the flat text completed).
pub fn flat_accessibility(text: &str) -> Option<Value> {
    let text = text.trim();
    let truncated = text.encode_utf16().count() >= ACCESSIBLE_TEXT_MAX;
    (!text.is_empty()).then(|| {
        json!({
            "accessibleText": text,
            "accessibility": { "format": "flat-text", "text": text, "truncated": truncated },
        })
    })
}

/// `snapShotAccessibilityDetails`: a tree as JSON, else the window's text.
pub fn accessibility_details(source: &Value) -> Option<String> {
    let accessibility = &source["accessibility"];
    if accessibility["format"] == "element-tree" {
        return serde_json::to_string_pretty(accessibility).ok();
    }
    let text = if accessibility["format"] == "flat-text" { &accessibility["text"] } else { &source["accessibleText"] };
    text.as_str().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned)
}

/// `snapShotIncludesAccessibility`.
pub fn includes_accessibility(source: &Value) -> bool {
    !source["accessibility"].is_null() || source["accessibleText"].as_str().is_some_and(|text| !text.trim().is_empty())
}

/// `snapShotSoundPatch` and the sound menu's current pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    Off,
    Whoosh,
    Click,
}

impl Sound {
    pub fn selected(play: bool, sound: &str) -> Self {
        match (play, sound) {
            (false, _) => Self::Off,
            (true, "camera-shutter") => Self::Click,
            _ => Self::Whoosh,
        }
    }

    pub fn patch(self) -> Value {
        match self {
            Self::Off => json!({ "snapShotPlaySound": false }),
            Self::Whoosh => json!({ "snapShotPlaySound": true, "snapShotSound": "soft-pop" }),
            Self::Click => json!({ "snapShotPlaySound": true, "snapShotSound": "camera-shutter" }),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Whoosh => "Whoosh",
            Self::Click => "Click",
        }
    }

    pub fn bytes(self) -> Option<&'static [u8]> {
        match self {
            Self::Off => None,
            Self::Whoosh => Some(include_bytes!("../assets/sounds/snap-shot-whoosh.mp3")),
            Self::Click => Some(include_bytes!("../assets/sounds/snap-shot-click.mp3")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(input: &str) -> Shortcut {
        chord_from_input(input).expect("a chord")
    }

    #[test]
    fn stored_shortcuts_round_trip_and_bad_ones_fall_back_to_both_shift_keys() {
        for value in [
            json!({ "kind": "both-shift-keys" }),
            json!({ "kind": "modifier-pair", "modifier": "alt" }),
            json!({ "key": "2", "metaKey": false, "ctrlKey": false, "shiftKey": true, "altKey": false, "modKey": true }),
        ] {
            assert_eq!(Shortcut::from_value(&value).to_value(), value);
        }
        // `SnapShotKeyChord` requires a modifier; a pair of an unknown modifier is no pair.
        for bad in [json!({ "key": "a" }), json!({ "kind": "modifier-pair", "modifier": "fn" }), json!(null)] {
            assert_eq!(Shortcut::from_value(&bad), Shortcut::Pair(Modifier::Shift));
        }
        assert_eq!(
            Shortcut::from_value(&json!({ "kind": "modifier-pair", "modifier": "shift" })).to_value()["kind"],
            "both-shift-keys"
        );
    }

    #[test]
    fn mod_is_ctrl_off_macos_and_cmd_on_it() {
        assert!(chord("mod+shift+2").same_on(&chord("ctrl+shift+2"), Os::Windows));
        assert!(chord("mod+shift+2").same_on(&chord("ctrl+shift+2"), Os::Linux));
        assert!(!chord("mod+shift+2").same_on(&chord("ctrl+shift+2"), Os::Mac));
        assert!(chord("mod+shift+2").same_on(&chord("meta+shift+2"), Os::Mac));
        assert!(!chord("mod+shift+2").same_on(&chord("alt+shift+2"), Os::Windows));
        assert!(!Shortcut::Pair(Modifier::Shift).same(&Shortcut::Pair(Modifier::Alt)));
        assert!(chord_from_input("a").is_none());
    }

    #[test]
    fn labels_follow_each_platforms_formatting() {
        assert_eq!(Shortcut::default().key_labels_on(Os::Windows), ["⇧", "⇧"]);
        assert_eq!(Shortcut::default().label_on(Os::Windows), "Shift + Shift");
        assert_eq!(Shortcut::Pair(Modifier::Meta).key_labels_on(Os::Windows), ["⊞", "⊞"]);
        assert_eq!(chord("mod+alt+shift+arrowup").key_labels_on(Os::Windows), ["Ctrl", "Alt", "⇧", "Up"]);
        assert_eq!(chord("mod+shift+space").label_on(Os::Windows), "Ctrl+Shift+Space");
        assert_eq!(chord("meta+f12").label_on(Os::Windows), "Meta+F12");

        assert_eq!(Shortcut::Pair(Modifier::Meta).key_labels_on(Os::Linux), ["Super", "Super"]);
        assert_eq!(Shortcut::Pair(Modifier::Meta).label_on(Os::Linux), "Super + Super");

        assert_eq!(Shortcut::Pair(Modifier::Alt).label_on(Os::Mac), "Option + Option");
        assert_eq!(Shortcut::Pair(Modifier::Meta).key_labels_on(Os::Mac), ["⌘", "⌘"]);
        assert_eq!(chord("mod+alt+shift+arrowup").key_labels_on(Os::Mac), ["⌥", "⇧", "⌘", "Up"]);
        assert_eq!(chord("mod+shift+space").label_on(Os::Mac), "⇧⌘Space");
        assert_eq!(chord("ctrl+f12").label_on(Os::Mac), "⌃F12");
    }

    #[test]
    fn single_modifier_chords_warn_about_the_shortcut_they_shadow() {
        let conflict = |input: &str| match chord(input) {
            Shortcut::Chord(chord) => system_conflict(&chord),
            Shortcut::Pair(_) => None,
        };
        assert_eq!(conflict("mod+c"), Some("This shortcut is Copy in most apps."));
        assert_eq!(conflict("ctrl+d"), Some("This shortcut controls running commands in terminals."));
        assert_eq!(conflict("alt+tab"), Some("The system uses Alt+Tab to switch apps."));
        assert!(conflict("shift+a").is_some_and(|m| m.starts_with("Shift combinations")));
        assert_eq!(conflict("mod+shift+c"), None);
        assert_eq!(conflict("mod+k"), None);
    }

    #[test]
    fn chords_map_to_register_hotkey_arguments() {
        let hotkey_of = |input: &str| match chord(input) {
            Shortcut::Chord(chord) => hotkey_on(&chord, Os::Windows),
            Shortcut::Pair(_) => None,
        };
        assert_eq!(hotkey_of("mod+shift+2"), Some((0x2 | 0x4, 0x32)));
        assert_eq!(hotkey_of("alt+meta+k"), Some((0x1 | 0x8, 0x4b)));
        assert_eq!(hotkey_of("ctrl+f12"), Some((0x2, 0x7b)));
        assert_eq!(hotkey_of("ctrl+arrowdown"), Some((0x2, 0x28)));
        assert_eq!(hotkey_of("ctrl+f25"), None);
        let mac = match chord("mod+shift+2") {
            Shortcut::Chord(chord) => hotkey_on(&chord, Os::Mac),
            Shortcut::Pair(_) => None,
        };
        assert_eq!(mac, Some((0x8 | 0x4, 0x32)), "mod is Cmd on macOS");
    }

    #[test]
    fn status_reads_like_the_web_for_direct_capture() {
        let mut state = State::default();
        assert_eq!(status(&state, false), "Turn this on to set up snapshots.");
        assert_eq!(status(&state, true), "Finish shortcut setup");
        state.shortcut_registered = true;
        assert_eq!(status(&state, true), "Shortcut saved");
        assert_eq!(shortcut_status(&state).as_deref(), Some("Shortcut saved."));
        state.shortcut_verified = true;
        assert_eq!(status(&state, true), "Ready to capture");
        state.message = Some(CAPTURE_FAILED.into());
        assert_eq!(status(&state, true), "Capture needs attention");
        let failed = State { shortcut_message: Some("taken".into()), ..State::default() };
        assert_eq!(shortcut_status(&failed).as_deref(), Some("taken"));
    }

    #[test]
    fn pair_messages_name_the_side_effects_windows_has() {
        assert_eq!(
            registration_failure_message(&Shortcut::default()),
            "Shift + Shift is not available on this system."
        );
        assert!(
            observed_pair_message_on(Modifier::Alt, Os::Windows).ends_with("This key can also activate app menu bars.")
        );
        assert_eq!(
            observed_pair_message_on(Modifier::Alt, Os::Mac),
            "Option + Option is observed and cannot be reserved exclusively."
        );
        assert!(observed_pair_message_on(Modifier::Meta, Os::Linux).ends_with("open the system's own menu."));
        assert_eq!(
            observed_pair_message_on(Modifier::Shift, Os::Windows),
            "Shift + Shift is observed and cannot be reserved exclusively."
        );
    }

    #[test]
    fn captures_fit_the_thumbnail_bounds_and_name_their_app() {
        assert_eq!(fit_capture(1920, 1080), (1920, 1080));
        assert_eq!(fit_capture(5120, 1440), (2560, 720));
        assert_eq!(fit_capture(3840, 3200), (1920, 1600));
        assert_eq!(app_name(r"C:\Program Files\Code\Code.EXE"), "Code");
        assert_eq!(app_name(""), "Window");
        assert_eq!(app_name("/usr/bin/.exe"), ".exe");
        assert_eq!(capture_name("2026-11-05T10:20:30.123Z", "png"), "window-2026-11-05T10-20-30.123Z.png");
        assert_eq!(bounded("  a\0b😀c  ", 3).as_deref(), Some("ab"));
        assert_eq!(bounded(" \0 ", 10), None);
    }

    #[test]
    fn sources_carry_the_window_and_skip_an_oversized_icon() {
        let source = source("2026-11-05T10:20:30.123Z", "Code", "  main.rs  ", Some(&[1, 2, 3]), None);
        assert_eq!(source["kind"], "snap-shot");
        assert_eq!(source["windowTitle"], "main.rs");
        assert_eq!(source["appIconDataUrl"], "data:image/png;base64,AQID");
        let big = vec![0; 80_000];
        let accessibility = json!({ "accessibleText": "Hello" });
        let source = super::source("t", "App", "", Some(&big), Some(accessibility));
        assert!(source.get("appIconDataUrl").is_none());
        assert_eq!(source["accessibleText"], "Hello");
    }

    #[test]
    fn accessible_text_keeps_each_name_once_within_the_limit() {
        let lines = ["Editor", " Editor ", "", "a\0b", "Save"].map(String::from);
        assert_eq!(accessible_text(lines.clone(), 100), "Editor\nab\nSave");
        assert_eq!(accessible_text(lines, 9), "Editor\nab");
        assert_eq!(accessible_text(["ab😀".to_owned()], 3), "ab");
        assert_eq!(flat_accessibility("  "), None);
        let flat = flat_accessibility("Editor\nSave").expect("text");
        assert_eq!(flat["accessibility"], json!({ "format": "flat-text", "text": "Editor\nSave", "truncated": false }));
        assert_eq!(flat["accessibleText"], "Editor\nSave");
    }

    #[test]
    fn accessibility_details_prefer_the_tree_then_the_text() {
        let flat = source("t", "App", "", None, flat_accessibility("Hello"));
        assert_eq!(accessibility_details(&flat).as_deref(), Some("Hello"));
        assert!(includes_accessibility(&flat));
        let tree = json!({ "accessibility": { "format": "element-tree", "root": {} }, "accessibleText": "x" });
        assert!(accessibility_details(&tree).is_some_and(|details| details.starts_with('{')));
        let bare = source("t", "App", "", None, None);
        assert_eq!(accessibility_details(&bare), None);
        assert!(!includes_accessibility(&bare));
    }

    #[test]
    fn the_sound_menu_writes_the_play_flag_and_the_sound() {
        assert_eq!(Sound::selected(false, "camera-shutter"), Sound::Off);
        assert_eq!(Sound::selected(true, "camera-shutter"), Sound::Click);
        assert_eq!(Sound::selected(true, "soft-pop"), Sound::Whoosh);
        assert_eq!(Sound::Off.patch(), json!({ "snapShotPlaySound": false }));
        assert_eq!(Sound::Click.patch()["snapShotSound"], "camera-shutter");
    }
}
