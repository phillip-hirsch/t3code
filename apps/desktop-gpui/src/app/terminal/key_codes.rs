//! A key event's physical key for Ghostty's encoder (`ghosttyKeyForEvent` in `keyCodes.ts`, which
//! reads `KeyboardEvent.code`). Browsers derive `code` from the platform's key code; this does the
//! same from [`NativeKeyCode`], with Chromium's tables (`ui/events/keycodes/dom/dom_code_data.inc`),
//! and falls back to GPUI's layout-dependent key name when the platform reports no code.

use gpui_kit::NativeKeyCode;
use libghostty_vt::key::Key;

/// No key in that platform's column.
const NONE: u16 = u16::MAX;

/// A physical key and its evdev, Windows and macOS codes.
type Row = (Key, u16, u16, u16);

/// The physical keys the encoder knows that GPUI dispatches as key events (modifiers and lock
/// keys arrive as modifier changes), one row per key as in Chromium's table: the evdev code
/// (Linux reports it plus 8 as the XKB keycode), the Windows scan code (`0xE0` prefix for
/// extended keys) and the macOS virtual key code.
#[rustfmt::skip]
const KEYS: &[Row] = &[
    (Key::A, 30, 0x001e, 0x00),
    (Key::B, 48, 0x0030, 0x0b),
    (Key::C, 46, 0x002e, 0x08),
    (Key::D, 32, 0x0020, 0x02),
    (Key::E, 18, 0x0012, 0x0e),
    (Key::F, 33, 0x0021, 0x03),
    (Key::G, 34, 0x0022, 0x05),
    (Key::H, 35, 0x0023, 0x04),
    (Key::I, 23, 0x0017, 0x22),
    (Key::J, 36, 0x0024, 0x26),
    (Key::K, 37, 0x0025, 0x28),
    (Key::L, 38, 0x0026, 0x25),
    (Key::M, 50, 0x0032, 0x2e),
    (Key::N, 49, 0x0031, 0x2d),
    (Key::O, 24, 0x0018, 0x1f),
    (Key::P, 25, 0x0019, 0x23),
    (Key::Q, 16, 0x0010, 0x0c),
    (Key::R, 19, 0x0013, 0x0f),
    (Key::S, 31, 0x001f, 0x01),
    (Key::T, 20, 0x0014, 0x11),
    (Key::U, 22, 0x0016, 0x20),
    (Key::V, 47, 0x002f, 0x09),
    (Key::W, 17, 0x0011, 0x0d),
    (Key::X, 45, 0x002d, 0x07),
    (Key::Y, 21, 0x0015, 0x10),
    (Key::Z, 44, 0x002c, 0x06),
    (Key::Digit1, 2, 0x0002, 0x12),
    (Key::Digit2, 3, 0x0003, 0x13),
    (Key::Digit3, 4, 0x0004, 0x14),
    (Key::Digit4, 5, 0x0005, 0x15),
    (Key::Digit5, 6, 0x0006, 0x17),
    (Key::Digit6, 7, 0x0007, 0x16),
    (Key::Digit7, 8, 0x0008, 0x1a),
    (Key::Digit8, 9, 0x0009, 0x1c),
    (Key::Digit9, 10, 0x000a, 0x19),
    (Key::Digit0, 11, 0x000b, 0x1d),
    (Key::Enter, 28, 0x001c, 0x24),
    (Key::Escape, 1, 0x0001, 0x35),
    (Key::Backspace, 14, 0x000e, 0x33),
    (Key::Tab, 15, 0x000f, 0x30),
    (Key::Space, 57, 0x0039, 0x31),
    (Key::Minus, 12, 0x000c, 0x1b),
    (Key::Equal, 13, 0x000d, 0x18),
    (Key::BracketLeft, 26, 0x001a, 0x21),
    (Key::BracketRight, 27, 0x001b, 0x1e),
    (Key::Backslash, 43, 0x002b, 0x2a),
    (Key::Semicolon, 39, 0x0027, 0x29),
    (Key::Quote, 40, 0x0028, 0x27),
    (Key::Backquote, 41, 0x0029, 0x32),
    (Key::Comma, 51, 0x0033, 0x2b),
    (Key::Period, 52, 0x0034, 0x2f),
    (Key::Slash, 53, 0x0035, 0x2c),
    (Key::IntlBackslash, 86, 0x0056, 0x0a),
    (Key::IntlRo, 89, 0x0073, 0x5e),
    (Key::IntlYen, 124, 0x007d, 0x5d),
    (Key::Convert, 92, 0x0079, NONE),
    (Key::KanaMode, 93, 0x0070, NONE),
    (Key::NonConvert, 94, 0x007b, NONE),
    (Key::F1, 59, 0x003b, 0x7a),
    (Key::F2, 60, 0x003c, 0x78),
    (Key::F3, 61, 0x003d, 0x63),
    (Key::F4, 62, 0x003e, 0x76),
    (Key::F5, 63, 0x003f, 0x60),
    (Key::F6, 64, 0x0040, 0x61),
    (Key::F7, 65, 0x0041, 0x62),
    (Key::F8, 66, 0x0042, 0x64),
    (Key::F9, 67, 0x0043, 0x65),
    (Key::F10, 68, 0x0044, 0x6d),
    (Key::F11, 87, 0x0057, 0x67),
    (Key::F12, 88, 0x0058, 0x6f),
    (Key::F13, 183, 0x0064, 0x69),
    (Key::F14, 184, 0x0065, 0x6b),
    (Key::F15, 185, 0x0066, 0x71),
    (Key::F16, 186, 0x0067, 0x6a),
    (Key::F17, 187, 0x0068, 0x40),
    (Key::F18, 188, 0x0069, 0x4f),
    (Key::F19, 189, 0x006a, 0x50),
    (Key::F20, 190, 0x006b, 0x5a),
    (Key::F21, 191, 0x006c, NONE),
    (Key::F22, 192, 0x006d, NONE),
    (Key::F23, 193, 0x006e, NONE),
    (Key::F24, 194, 0x0076, NONE),
    (Key::PrintScreen, 99, 0xe037, NONE),
    (Key::ScrollLock, 70, 0x0046, NONE),
    (Key::Pause, 119, 0x0045, NONE),
    (Key::Insert, 110, 0xe052, 0x72),
    (Key::Home, 102, 0xe047, 0x73),
    (Key::PageUp, 104, 0xe049, 0x74),
    (Key::Delete, 111, 0xe053, 0x75),
    (Key::End, 107, 0xe04f, 0x77),
    (Key::PageDown, 109, 0xe051, 0x79),
    (Key::ArrowRight, 106, 0xe04d, 0x7c),
    (Key::ArrowLeft, 105, 0xe04b, 0x7b),
    (Key::ArrowDown, 108, 0xe050, 0x7d),
    (Key::ArrowUp, 103, 0xe048, 0x7e),
    (Key::ContextMenu, 127, 0xe05d, 0x6e),
    (Key::Help, 138, NONE, NONE),
    (Key::NumLock, 69, 0xe045, 0x47),
    (Key::NumpadDivide, 98, 0xe035, 0x4b),
    (Key::NumpadMultiply, 55, 0x0037, 0x43),
    (Key::NumpadSubtract, 74, 0x004a, 0x4e),
    (Key::NumpadAdd, 78, 0x004e, 0x45),
    (Key::NumpadEnter, 96, 0xe01c, 0x4c),
    (Key::Numpad1, 79, 0x004f, 0x53),
    (Key::Numpad2, 80, 0x0050, 0x54),
    (Key::Numpad3, 81, 0x0051, 0x55),
    (Key::Numpad4, 75, 0x004b, 0x56),
    (Key::Numpad5, 76, 0x004c, 0x57),
    (Key::Numpad6, 77, 0x004d, 0x58),
    (Key::Numpad7, 71, 0x0047, 0x59),
    (Key::Numpad8, 72, 0x0048, 0x5b),
    (Key::Numpad9, 73, 0x0049, 0x5c),
    (Key::Numpad0, 82, 0x0052, 0x52),
    (Key::NumpadDecimal, 83, 0x0053, 0x41),
    (Key::NumpadEqual, 117, 0x0059, 0x51),
    (Key::NumpadComma, 121, 0x007e, 0x5f),
    (Key::NumpadParenLeft, 179, NONE, NONE),
    (Key::NumpadParenRight, 180, NONE, NONE),
];

/// The key the encoder hears for a key event (`event.code`, or `event.key` without one).
///
/// A keypad key keeps its keypad identity; with Num Lock off a keypad digit is the keypad's
/// navigation key (keypad 1 named `end` is `NumpadEnd`), as Ghostty's encoder models it, so a
/// plain shell gets End's sequence and a Kitty program hears `KP_END`. Browsers report `Numpad1`
/// there, which Ghostty encodes as nothing outside Kitty mode. A layout-independent name the
/// physical key does not match means the key was remapped (XKB's `caps:escape`), and the name
/// wins: Ghostty encodes a bare `CapsLock` as nothing too.
pub(super) fn key_code(name: &str, native: Option<NativeKeyCode>) -> Key {
    let Some(physical) = native.and_then(physical) else { return from_name(name) };
    if is_keypad(physical) {
        return keypad_navigation(name).unwrap_or(physical);
    }
    let named = from_name(name);
    if name.chars().nth(1).is_some() && named != Key::Unidentified && named != physical { named } else { physical }
}

/// The physical key a platform's key code names.
fn physical(code: NativeKeyCode) -> Option<Key> {
    let (column, code): (fn(&Row) -> u16, u16) = match code {
        NativeKeyCode::Xkb(code) => (|row| row.1, code.checked_sub(8).and_then(|code| u16::try_from(code).ok())?),
        NativeKeyCode::Windows(code) => (|row| row.2, code),
        NativeKeyCode::Mac(code) => (|row| row.3, code),
    };
    KEYS.iter().find(|row| code != NONE && column(row) == code).map(|row| row.0)
}

fn is_keypad(key: Key) -> bool {
    matches!(
        key,
        Key::Numpad0
            | Key::Numpad1
            | Key::Numpad2
            | Key::Numpad3
            | Key::Numpad4
            | Key::Numpad5
            | Key::Numpad6
            | Key::Numpad7
            | Key::Numpad8
            | Key::Numpad9
            | Key::NumpadDecimal
            | Key::NumpadDivide
            | Key::NumpadMultiply
            | Key::NumpadSubtract
            | Key::NumpadAdd
            | Key::NumpadEnter
            | Key::NumpadEqual
            | Key::NumpadComma
            | Key::NumpadParenLeft
            | Key::NumpadParenRight
    )
}

/// A keypad digit's NumLock-off key, by the name the platform gives it then (Linux: its keysym
/// without `KP_`).
fn keypad_navigation(name: &str) -> Option<Key> {
    Some(match name {
        "insert" => Key::NumpadInsert,
        "delete" => Key::NumpadDelete,
        "end" => Key::NumpadEnd,
        "home" => Key::NumpadHome,
        "pagedown" => Key::NumpadPageDown,
        "pageup" => Key::NumpadPageUp,
        "down" => Key::NumpadDown,
        "up" => Key::NumpadUp,
        "left" => Key::NumpadLeft,
        "right" => Key::NumpadRight,
        "begin" => Key::NumpadBegin,
        _ => return None,
    })
}

/// GPUI's key names as W3C key codes (`ghosttyKeyForCode(event.key)`), with shifted US symbols on
/// the key that types them.
pub(super) fn from_name(name: &str) -> Key {
    use Key as K;
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        const LETTERS: [K; 26] = [
            K::A,
            K::B,
            K::C,
            K::D,
            K::E,
            K::F,
            K::G,
            K::H,
            K::I,
            K::J,
            K::K,
            K::L,
            K::M,
            K::N,
            K::O,
            K::P,
            K::Q,
            K::R,
            K::S,
            K::T,
            K::U,
            K::V,
            K::W,
            K::X,
            K::Y,
            K::Z,
        ];
        const DIGITS: [K; 10] = [
            K::Digit0,
            K::Digit1,
            K::Digit2,
            K::Digit3,
            K::Digit4,
            K::Digit5,
            K::Digit6,
            K::Digit7,
            K::Digit8,
            K::Digit9,
        ];
        let offset = |from: char| usize::try_from(u32::from(c.to_ascii_lowercase()) - u32::from(from)).unwrap_or(0);
        return match c.to_ascii_lowercase() {
            'a'..='z' => LETTERS[offset('a')],
            '0'..='9' => DIGITS[offset('0')],
            ')' => K::Digit0,
            '!' => K::Digit1,
            '@' => K::Digit2,
            '#' => K::Digit3,
            '$' => K::Digit4,
            '%' => K::Digit5,
            '^' => K::Digit6,
            '&' => K::Digit7,
            '*' => K::Digit8,
            '(' => K::Digit9,
            '`' | '~' => K::Backquote,
            '\\' | '|' => K::Backslash,
            '[' | '{' => K::BracketLeft,
            ']' | '}' => K::BracketRight,
            ',' | '<' => K::Comma,
            '=' | '+' => K::Equal,
            '-' | '_' => K::Minus,
            '.' | '>' => K::Period,
            '\'' | '"' => K::Quote,
            ';' | ':' => K::Semicolon,
            '/' | '?' => K::Slash,
            ' ' => K::Space,
            _ => K::Unidentified,
        };
    }
    match name {
        "enter" => K::Enter,
        "tab" => K::Tab,
        "space" => K::Space,
        "escape" => K::Escape,
        "backspace" => K::Backspace,
        "delete" => K::Delete,
        "insert" => K::Insert,
        "home" => K::Home,
        "end" => K::End,
        "pageup" => K::PageUp,
        "pagedown" => K::PageDown,
        "up" => K::ArrowUp,
        "down" => K::ArrowDown,
        "left" => K::ArrowLeft,
        "right" => K::ArrowRight,
        "f1" => K::F1,
        "f2" => K::F2,
        "f3" => K::F3,
        "f4" => K::F4,
        "f5" => K::F5,
        "f6" => K::F6,
        "f7" => K::F7,
        "f8" => K::F8,
        "f9" => K::F9,
        "f10" => K::F10,
        "f11" => K::F11,
        "f12" => K::F12,
        "f13" => K::F13,
        "f14" => K::F14,
        "f15" => K::F15,
        "f16" => K::F16,
        "f17" => K::F17,
        "f18" => K::F18,
        "f19" => K::F19,
        "f20" => K::F20,
        "f21" => K::F21,
        "f22" => K::F22,
        "f23" => K::F23,
        "f24" => K::F24,
        "menu" => K::ContextMenu,
        _ => K::Unidentified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_platform_column_names_a_key_once() {
        let columns: [fn(&Row) -> u16; 3] = [|row| row.1, |row| row.2, |row| row.3];
        for (column, code) in columns.iter().enumerate() {
            for (index, row) in KEYS.iter().enumerate() {
                let value = code(row);
                let repeated = KEYS[index + 1..].iter().any(|other| value != NONE && code(other) == value);
                assert!(!repeated, "column {column}: {value:#x} ({:?}) appears twice", row.0);
            }
        }
    }

    #[test]
    fn platform_codes_name_physical_keys() {
        use NativeKeyCode::{Mac, Windows, Xkb};
        for (native, name, key) in [
            // German Shift+7 types `/` on the 7 key; AZERTY's A key is US Q.
            (Windows(0x08), "/", Key::Digit7),
            (Xkb(16), "/", Key::Digit7),
            (Mac(0x1a), "/", Key::Digit7),
            (Xkb(24), "a", Key::Q),
            // The keypad, with NumLock on and off.
            (Windows(0x4f), "1", Key::Numpad1),
            (Windows(0x4f), "end", Key::NumpadEnd),
            (Xkb(87), "end", Key::NumpadEnd),
            (Xkb(84), "begin", Key::NumpadBegin),
            (Windows(0x53), "delete", Key::NumpadDelete),
            (Mac(0x53), "1", Key::Numpad1),
            (Windows(0xe01c), "enter", Key::NumpadEnter),
            (Windows(0x1c), "enter", Key::Enter),
            (Mac(0x4c), "enter", Key::NumpadEnter),
            // The dedicated navigation keys are extended on Windows.
            (Windows(0xe04f), "end", Key::End),
            // Caps Lock remapped to Escape (`caps:escape`) is Escape.
            (Xkb(66), "escape", Key::Escape),
            // No code from the platform: the name.
            (Windows(0x00ff), "?", Key::Slash),
        ] {
            assert_eq!(key_code(name, Some(native)), key, "{native:?} {name}");
        }
        assert_eq!(key_code("f5", None), Key::F5);
    }
}
