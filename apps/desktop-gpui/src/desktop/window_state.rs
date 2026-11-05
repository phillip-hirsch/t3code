//! Main window bounds restore, as `apps/desktop/src/window/DesktopWindow.ts` does it.

use super::settings::{DEFAULT_WINDOW_SIZE, DesktopSettings, WindowRect};

/// What to open with: saved bounds that fit a connected display, else the default size centered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialBounds {
    Restored { rect: WindowRect, maximized: bool },
    Default { width: i32, height: i32 },
}

/// `resolveInitialMainWindowBounds`.
pub fn initial_bounds(settings: &DesktopSettings, displays: &[WindowRect]) -> InitialBounds {
    match settings.main_window_bounds {
        Some(rect) if displays.iter().any(|display| rect.fits_within(display)) => {
            InitialBounds::Restored { rect, maximized: settings.main_window_maximized }
        }
        _ => InitialBounds::Default { width: DEFAULT_WINDOW_SIZE.0, height: DEFAULT_WINDOW_SIZE.1 },
    }
}

/// Decides which bounds changes get saved. Saved bounds that could not be restored (their
/// display is gone) are kept until the user moves, resizes or maximizes the window.
pub struct BoundsPersistence {
    enabled: bool,
    fallback: Option<(WindowRect, bool)>,
}

impl BoundsPersistence {
    /// `opened` is the window's normal bounds and maximized state right after it opened.
    pub fn new(settings: &DesktopSettings, initial: InitialBounds, opened: (WindowRect, bool)) -> Self {
        let restored = matches!(initial, InitialBounds::Restored { .. });
        let enabled = settings.main_window_bounds.is_none() || restored;
        Self { enabled, fallback: (!enabled).then_some(opened) }
    }

    /// Whether this change should be written.
    pub fn should_save(&mut self, current: (WindowRect, bool)) -> bool {
        if !self.enabled && self.fallback == Some(current) {
            return false;
        }
        self.enabled = true;
        true
    }
}

/// Stores the change; false when nothing differs.
pub fn record(settings: &mut DesktopSettings, rect: WindowRect, maximized: bool) -> bool {
    if settings.main_window_bounds == Some(rect) && settings.main_window_maximized == maximized {
        return false;
    }
    settings.main_window_bounds = Some(rect);
    settings.main_window_maximized = maximized;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: WindowRect = WindowRect { x: 0, y: 0, width: 1920, height: 1080 };
    const LEFT: WindowRect = WindowRect { x: -1920, y: 0, width: 1920, height: 1080 };

    fn saved(rect: WindowRect, maximized: bool) -> DesktopSettings {
        let mut settings = DesktopSettings::default();
        settings.main_window_bounds = Some(rect);
        settings.main_window_maximized = maximized;
        settings
    }

    #[test]
    fn saved_bounds_restore_only_when_a_display_holds_them() {
        let rect = WindowRect { x: -1500, y: 100, width: 1200, height: 800 };
        assert_eq!(
            initial_bounds(&saved(rect, true), &[DISPLAY, LEFT]),
            InitialBounds::Restored { rect, maximized: true }
        );
        // The left monitor was unplugged.
        assert_eq!(initial_bounds(&saved(rect, true), &[DISPLAY]), InitialBounds::Default { width: 1100, height: 780 });
        // Straddling two displays fits neither.
        let straddling = WindowRect { x: -300, y: 0, width: 1200, height: 800 };
        assert!(matches!(initial_bounds(&saved(straddling, false), &[DISPLAY, LEFT]), InitialBounds::Default { .. }));
    }

    #[test]
    fn unrestorable_bounds_survive_until_the_user_changes_the_window() {
        let rect = WindowRect { x: -1500, y: 100, width: 1200, height: 800 };
        let settings = saved(rect, false);
        let initial = initial_bounds(&settings, &[DISPLAY]);
        let opened = (WindowRect { x: 410, y: 150, width: 1100, height: 780 }, false);
        let mut persistence = BoundsPersistence::new(&settings, initial, opened);
        assert!(!persistence.should_save(opened));
        let moved = (WindowRect { x: 420, ..opened.0 }, false);
        assert!(persistence.should_save(moved));
        // Once the user moved it, every change counts, even back to where it opened.
        assert!(persistence.should_save(opened));

        let mut fresh = BoundsPersistence::new(&DesktopSettings::default(), initial, opened);
        assert!(fresh.should_save(opened));
    }
}
