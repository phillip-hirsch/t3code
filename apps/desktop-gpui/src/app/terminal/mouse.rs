//! Mouse reports for full-screen apps that ask for them (xterm modes 1000/1002/1003, with
//! the SGR 1006 and UTF-8 1005 encodings), as the web's Ghostty mouse encoder sends them.

use alacritty_terminal::term::TermMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseAction {
    Press,
    Release,
    Motion,
}

/// xterm button codes: 0 left, 1 middle, 2 right, 3 none (motion), 64/65 wheel up/down.
pub(super) const NO_BUTTON: u8 = 3;
pub(super) const WHEEL_UP: u8 = 64;
pub(super) const WHEEL_DOWN: u8 = 65;

/// `shouldReportTerminalMouse`: tracking is on and no modifier asks for local selection.
pub(super) fn reports(mode: TermMode, shift: bool, ctrl: bool) -> bool {
    mode.intersects(TermMode::MOUSE_MODE) && !shift && !ctrl
}

/// The bytes for one mouse event at a 0-based cell, or `None` when the mode doesn't report it.
pub(super) fn encode(
    mode: TermMode,
    action: MouseAction,
    button: u8,
    alt: bool,
    column: usize,
    row: usize,
) -> Option<String> {
    if !mode.intersects(TermMode::MOUSE_MODE) {
        return None;
    }
    if action == MouseAction::Motion
        && !(mode.contains(TermMode::MOUSE_MOTION) || (mode.contains(TermMode::MOUSE_DRAG) && button != NO_BUTTON))
    {
        return None;
    }
    let mut code = button + if alt { 8 } else { 0 };
    if action == MouseAction::Motion {
        code += 32;
    }
    let (x, y) = (column + 1, row + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if action == MouseAction::Release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{end}"));
    }
    if action == MouseAction::Release {
        // The legacy encodings can't say which button went up.
        code = NO_BUTTON + if alt { 8 } else { 0 };
    }
    let utf8 = mode.contains(TermMode::UTF8_MOUSE);
    let limit = if utf8 { 2015 } else { 223 };
    if x > limit || y > limit {
        return None;
    }
    let coordinate = |v: usize| char::from_u32(32 + v as u32);
    let mut out = String::from("\x1b[M");
    out.push(char::from(32 + code));
    // Legacy coordinates above 127 go out as their Latin-1 code points.
    out.push(coordinate(x)?);
    out.push(coordinate(y)?);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sgr_reports_presses_releases_and_wheel() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(encode(mode, MouseAction::Press, 0, false, 4, 1).as_deref(), Some("\x1b[<0;5;2M"));
        assert_eq!(encode(mode, MouseAction::Release, 0, false, 4, 1).as_deref(), Some("\x1b[<0;5;2m"));
        assert_eq!(encode(mode, MouseAction::Press, WHEEL_DOWN, true, 0, 0).as_deref(), Some("\x1b[<73;1;1M"));
        // Click tracking alone sends no motion.
        assert_eq!(encode(mode, MouseAction::Motion, 0, false, 1, 1), None);
    }

    #[test]
    fn drag_and_motion_modes() {
        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(encode(drag, MouseAction::Motion, 0, false, 2, 3).as_deref(), Some("\x1b[<32;3;4M"));
        assert_eq!(encode(drag, MouseAction::Motion, NO_BUTTON, false, 2, 3), None);
        let motion = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        assert_eq!(encode(motion, MouseAction::Motion, NO_BUTTON, false, 0, 0).as_deref(), Some("\x1b[<35;1;1M"));
        assert_eq!(encode(TermMode::empty(), MouseAction::Press, 0, false, 0, 0), None);
    }

    #[test]
    fn legacy_encoding_offsets_by_32_and_releases_as_button_3() {
        let mode = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(encode(mode, MouseAction::Press, 2, false, 0, 9).as_deref(), Some("\x1b[M\"!*"));
        assert_eq!(encode(mode, MouseAction::Release, 2, false, 0, 9).as_deref(), Some("\x1b[M#!*"));
        assert_eq!(encode(mode, MouseAction::Press, 0, false, 300, 0), None);
        assert!(reports(mode, false, false));
        assert!(!reports(mode, true, false) && !reports(mode, false, true));
    }
}
