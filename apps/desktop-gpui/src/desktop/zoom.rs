//! App zoom as Electron's `zoomWindow` (`apps/desktop/src/window/DesktopWindow.ts`) sets it:
//! Chromium zoom levels, half a level per step, where level `n` scales the interface by `1.2ⁿ`.

/// A View menu zoom command, or Ctrl+wheel (`MainWindowZoomDirection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomStep {
    In,
    Out,
    Reset,
}

/// The half-level steps inside Chromium's page zoom limits, 25% to 500%.
const MIN_LEVEL: f64 = -7.5;
const MAX_LEVEL: f64 = 8.5;

/// The level after `step`; Actual Size is level 0.
pub fn step(level: f64, step: ZoomStep) -> f64 {
    match step {
        ZoomStep::In => (level + 0.5).min(MAX_LEVEL),
        ZoomStep::Out => (level - 0.5).max(MIN_LEVEL),
        ZoomStep::Reset => 0.,
    }
}

/// A saved level, kept inside the limits; anything that isn't a number is Actual Size.
pub fn normalize(level: f64) -> f64 {
    if level.is_finite() { level.clamp(MIN_LEVEL, MAX_LEVEL) } else { 0. }
}

/// The interface scale for `level` (`Window::set_ui_scale`).
#[expect(clippy::cast_possible_truncation, reason = "a factor between 0.25 and 5")]
pub fn factor(level: f64) -> f32 {
    1.2_f64.powf(level) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_move_half_a_level_and_stop_at_chromiums_limits() {
        assert_eq!(step(0., ZoomStep::In), 0.5);
        assert_eq!(step(0.5, ZoomStep::Out), 0.);
        assert_eq!(step(3., ZoomStep::Reset), 0.);
        assert_eq!(step(MAX_LEVEL, ZoomStep::In), MAX_LEVEL);
        assert_eq!(step(MIN_LEVEL, ZoomStep::Out), MIN_LEVEL);
        assert!(factor(MIN_LEVEL) >= 0.25 && factor(MAX_LEVEL) <= 5.);
        assert!((factor(1.) - 1.2).abs() < 1e-6);
        assert_eq!(normalize(f64::NAN), 0.);
        assert_eq!(normalize(40.), MAX_LEVEL);
    }
}
