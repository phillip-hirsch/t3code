//! Keystrokes to the bytes a PTY expects (xterm encoding). Plain printable
//! characters are not handled here: they arrive through the input handler, so
//! IME and dead keys work.

use std::borrow::Cow;

use gpui_kit::Keystroke;

/// What the key does in the terminal itself, before anything is sent.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum KeyAction {
    Send(Cow<'static, str>),
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
    pub fn scroll(self) -> alacritty_terminal::grid::Scroll {
        use alacritty_terminal::grid::Scroll;
        match self {
            History::PageUp => Scroll::PageUp,
            History::PageDown => Scroll::PageDown,
            History::Top => Scroll::Top,
            History::Bottom => Scroll::Bottom,
        }
    }
}

/// `app_cursor` is DECCKM: arrows send SS3 instead of CSI. The alternate screen has no
/// scrollback, so its history keys go to the app.
pub(super) fn key_action(
    keystroke: &Keystroke,
    app_cursor: bool,
    alt_screen: bool,
    has_selection: bool,
) -> Option<KeyAction> {
    let m = &keystroke.modifiers;
    if m.platform || m.function {
        return None;
    }
    let key = keystroke.key.as_str();
    let (ctrl, alt, shift) = (m.control, m.alt, m.shift);
    if ctrl && shift && !alt && key == "a" {
        return Some(KeyAction::SelectAll);
    }
    if shift && !alt && !alt_screen {
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
    if has_selection && ctrl && !alt && (key == "c" || (key == "insert" && !shift)) {
        return Some(KeyAction::Copy { keep_selection: shift });
    }
    if (ctrl && !alt && key == "v") || (shift && !ctrl && !alt && key == "insert") {
        return Some(KeyAction::Paste);
    }
    // `terminalNavigationShortcutData`: Alt+arrows move by word.
    if alt && !ctrl && !shift {
        match key {
            "left" => return Some(KeyAction::Send("\x1bb".into())),
            "right" => return Some(KeyAction::Send("\x1bf".into())),
            _ => {}
        }
    }
    encode(key, ctrl, alt, shift, app_cursor, keystroke.key_char.as_deref()).map(KeyAction::Send)
}

fn encode(
    key: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    app_cursor: bool,
    key_char: Option<&str>,
) -> Option<Cow<'static, str>> {
    // xterm's modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    let modifier = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let alt_prefix = |s: &'static str| -> Cow<'static, str> { if alt { format!("\x1b{s}").into() } else { s.into() } };
    let cursor = |letter: char| -> Cow<'static, str> {
        match (modifier, app_cursor) {
            (1, true) => format!("\x1bO{letter}").into(),
            (1, false) => format!("\x1b[{letter}").into(),
            _ => format!("\x1b[1;{modifier}{letter}").into(),
        }
    };
    let tilde = |code: u8| -> Cow<'static, str> {
        if modifier == 1 { format!("\x1b[{code}~").into() } else { format!("\x1b[{code};{modifier}~").into() }
    };
    let ss3 = |letter: char| -> Cow<'static, str> {
        if modifier == 1 { format!("\x1bO{letter}").into() } else { format!("\x1b[1;{modifier}{letter}").into() }
    };
    Some(match key {
        "enter" => alt_prefix("\r"),
        "tab" if shift => "\x1b[Z".into(),
        "tab" => alt_prefix("\t"),
        "escape" => alt_prefix("\x1b"),
        "backspace" if ctrl => alt_prefix("\x08"),
        "backspace" => alt_prefix("\x7f"),
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => ss3('P'),
        "f2" => ss3('Q'),
        "f3" => ss3('R'),
        "f4" => ss3('S'),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        _ if ctrl && !alt => return control_code(key, shift).map(|byte| Cow::Owned(char::from(byte).to_string())),
        _ if ctrl && alt => {
            // AltGr on Windows arrives as Ctrl+Alt: let its character through as text.
            if key_char.is_some_and(|c| c != key) {
                return None;
            }
            let byte = control_code(key, shift)?;
            format!("\x1b{}", char::from(byte)).into()
        }
        _ if alt && !ctrl => {
            let text = key_char.filter(|c| !c.is_empty()).unwrap_or(key);
            if text.chars().count() != 1 {
                return None;
            }
            format!("\x1b{text}").into()
        }
        _ => return None,
    })
}

/// The C0 byte Ctrl+`key` sends.
fn control_code(key: &str, shift: bool) -> Option<u8> {
    let mut chars = key.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return (key == "space").then_some(0);
    };
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '2' | '@' | '`' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '6' | '^' => 0x1e,
        '-' | '_' | '7' | '/' => 0x1f,
        '8' | '?' => 0x7f,
        _ if shift => return None,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(spec: &'static str) -> Keystroke {
        Keystroke::parse(spec).unwrap()
    }

    fn send(spec: &'static str) -> Option<String> {
        match key_action(&key(spec), false, false, false) {
            Some(KeyAction::Send(bytes)) => Some(bytes.into_owned()),
            _ => None,
        }
    }

    #[test]
    fn control_and_navigation_keys() {
        assert_eq!(send("ctrl-c").as_deref(), Some("\x03"));
        assert_eq!(send("ctrl-l").as_deref(), Some("\x0c"));
        assert_eq!(send("ctrl-space").as_deref(), Some("\x00"));
        assert_eq!(send("ctrl-[").as_deref(), Some("\x1b"));
        assert_eq!(send("enter").as_deref(), Some("\r"));
        assert_eq!(send("backspace").as_deref(), Some("\x7f"));
        assert_eq!(send("ctrl-backspace").as_deref(), Some("\x08"));
        assert_eq!(send("shift-tab").as_deref(), Some("\x1b[Z"));
        assert_eq!(send("up").as_deref(), Some("\x1b[A"));
        assert_eq!(send("ctrl-right").as_deref(), Some("\x1b[1;5C"));
        assert_eq!(send("shift-home").as_deref(), Some("\x1b[1;2H"));
        assert_eq!(send("delete").as_deref(), Some("\x1b[3~"));
        assert_eq!(send("ctrl-pagedown").as_deref(), Some("\x1b[6;5~"));
        assert_eq!(send("f1").as_deref(), Some("\x1bOP"));
        assert_eq!(send("f5").as_deref(), Some("\x1b[15~"));
        assert_eq!(send("alt-left").as_deref(), Some("\x1bb"));
        assert_eq!(send("alt-x").as_deref(), Some("\x1bx"));
        assert_eq!(send("alt-enter").as_deref(), Some("\x1b\r"));
        // Printable text goes through the input handler.
        assert_eq!(send("a"), None);
        assert_eq!(send("shift-a"), None);
    }

    #[test]
    fn application_cursor_mode_uses_ss3() {
        let up = key_action(&key("up"), true, false, false);
        assert_eq!(up, Some(KeyAction::Send("\x1bOA".into())));
        let ctrl_up = key_action(&key("ctrl-up"), true, false, false);
        assert_eq!(ctrl_up, Some(KeyAction::Send("\x1b[1;5A".into())));
    }

    #[test]
    fn copy_needs_a_selection_and_paste_always_works() {
        assert_eq!(key_action(&key("ctrl-c"), false, false, true), Some(KeyAction::Copy { keep_selection: false }));
        assert_eq!(
            key_action(&key("ctrl-shift-c"), false, false, true),
            Some(KeyAction::Copy { keep_selection: true })
        );
        assert_eq!(
            key_action(&key("ctrl-insert"), false, false, true),
            Some(KeyAction::Copy { keep_selection: false })
        );
        assert_eq!(key_action(&key("ctrl-v"), false, false, false), Some(KeyAction::Paste));
        assert_eq!(key_action(&key("ctrl-shift-v"), false, false, false), Some(KeyAction::Paste));
        assert_eq!(key_action(&key("shift-insert"), false, false, false), Some(KeyAction::Paste));
    }

    #[test]
    fn select_all_and_history_keys() {
        assert_eq!(key_action(&key("ctrl-shift-a"), false, false, false), Some(KeyAction::SelectAll));
        assert_eq!(key_action(&key("shift-pageup"), false, false, false), Some(KeyAction::Scroll(History::PageUp)));
        assert_eq!(key_action(&key("ctrl-shift-end"), false, false, false), Some(KeyAction::Scroll(History::Bottom)));
        // Full-screen apps get the keys.
        assert_eq!(key_action(&key("shift-pageup"), false, true, false), Some(KeyAction::Send("\x1b[5;2~".into())));
    }
}
