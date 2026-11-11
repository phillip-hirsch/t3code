//! Mouse reports for programs that track the mouse. Ghostty's encoder picks the tracking mode
//! and format (X10, UTF-8, SGR, urxvt, SGR pixels); this decides when to report and drops
//! repeated motion, as the web's surface does.

use gpui_kit::{Modifiers, MouseButton as GpuiButton};

use super::core::{self, MouseAction};

/// `shouldReportTerminalMouse`: Shift, Ctrl or ⌘ ask for local selection instead.
pub(super) fn reports(tracking: bool, modifiers: Modifiers) -> bool {
    tracking && !modifiers.shift && !modifiers.control && !modifiers.platform
}

/// `ghosttyMouseButton`: the buttons a terminal can report.
pub(super) fn button(button: GpuiButton) -> Option<core::MouseButton> {
    match button {
        GpuiButton::Left => Some(core::MouseButton::Left),
        GpuiButton::Middle => Some(core::MouseButton::Middle),
        GpuiButton::Right => Some(core::MouseButton::Right),
        GpuiButton::Navigate(_) => None,
    }
}

pub(super) fn modifiers(modifiers: Modifiers) -> core::Modifiers {
    core::Modifiers { shift: modifiers.shift, ctrl: modifiers.control, alt: modifiers.alt, meta: modifiers.platform }
}

/// `resolveTerminalMouseData`: whether to send `data`, and the motion report to compare the
/// next one with. Any other action starts over.
pub(super) fn resolve(action: MouseAction, data: &str, last_motion: &str) -> (bool, String) {
    let next = if action == MouseAction::Motion { data.to_owned() } else { String::new() };
    (!data.is_empty() && (action != MouseAction::Motion || data != last_motion), next)
}

#[cfg(test)]
mod tests {
    use super::super::core::tests::core;
    use super::super::core::{Core, Modifiers, MouseButton, MouseInput};
    use super::*;

    /// A report at cell (`column`, `row`) of an 8×16 grid with no padding.
    fn report(core: &mut Core, action: MouseAction, button: Option<MouseButton>, column: f32, row: f32) -> String {
        core.encode_mouse(&MouseInput {
            action,
            button,
            mods: Modifiers::default(),
            x: column * 8. + 1.,
            y: row * 16. + 1.,
            width: 800.,
            height: 400.,
            cell_width: 8.,
            cell_height: 16.,
            padding_left: 0.,
            padding_top: 0.,
            any_button_pressed: button.is_some() && action != MouseAction::Release,
        })
    }

    #[test]
    fn sgr_reports_presses_releases_and_the_wheel() {
        let mut core = core(100, 25);
        assert_eq!(report(&mut core, MouseAction::Press, Some(MouseButton::Left), 4., 1.), "", "nothing until asked");
        core.write(b"\x1b[?1000h\x1b[?1006h");
        assert!(core.mouse_tracking() && !core.any_event_mouse());
        assert_eq!(report(&mut core, MouseAction::Press, Some(MouseButton::Left), 4., 1.), "\x1b[<0;5;2M");
        assert_eq!(report(&mut core, MouseAction::Release, Some(MouseButton::Left), 4., 1.), "\x1b[<0;5;2m");
        assert_eq!(report(&mut core, MouseAction::Press, Some(MouseButton::WheelDown), 0., 0.), "\x1b[<65;1;1M");
        assert_eq!(report(&mut core, MouseAction::Motion, None, 1., 1.), "", "click tracking sends no motion");
    }

    #[test]
    fn drag_and_any_motion_modes() {
        let mut core = core(100, 25);
        core.write(b"\x1b[?1002h\x1b[?1006h");
        assert_eq!(report(&mut core, MouseAction::Motion, Some(MouseButton::Left), 2., 3.), "\x1b[<32;3;4M");
        assert_eq!(report(&mut core, MouseAction::Motion, None, 5., 3.), "", "drags only");
        core.write(b"\x1b[?1003h");
        assert!(core.any_event_mouse());
        assert_eq!(report(&mut core, MouseAction::Motion, None, 0., 0.), "\x1b[<35;1;1M");
    }

    #[test]
    fn legacy_reports_offset_by_32_and_release_as_button_3() {
        let mut core = core(100, 25);
        core.write(b"\x1b[?1000h");
        assert_eq!(report(&mut core, MouseAction::Press, Some(MouseButton::Right), 0., 9.), "\x1b[M\"!*");
        assert_eq!(report(&mut core, MouseAction::Release, Some(MouseButton::Right), 0., 9.), "\x1b[M#!*");
        let shift = gpui_kit::Modifiers { shift: true, ..Default::default() };
        assert!(reports(true, Default::default()) && !reports(true, shift) && !reports(false, Default::default()));
    }

    #[test]
    fn repeated_motion_drops_until_another_action_resets_it() {
        let (send, last) = resolve(MouseAction::Motion, "\x1b[<35;8;4M", "");
        assert!(send);
        let (send, last) = resolve(MouseAction::Motion, "\x1b[<35;8;4M", &last);
        assert!(!send);
        let (send, last) = resolve(MouseAction::Press, "\x1b[<0;8;4M", &last);
        assert!(send && last.is_empty());
        assert!(resolve(MouseAction::Motion, "\x1b[<35;8;4M", &last).0);
    }
}
