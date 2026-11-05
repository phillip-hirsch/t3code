//! Desktop shell features of the Electron main process (`apps/desktop/src`) that this
//! native client owns itself: its settings file, window state, single-instance activation
//! and app links.

pub mod activation;
pub mod cli;
pub mod links;
pub mod settings;
pub mod window_state;

use gpui_kit::{App, Bounds, Global, Pixels, WindowBounds, point, px, size};

use settings::{DesktopSettings, WindowRect};
use window_state::InitialBounds;

/// The desktop settings and how the window opened, read once before the window exists.
pub struct Desktop {
    pub settings: DesktopSettings,
    pub initial: InitialBounds,
    /// This launch's command line, which may carry an app link.
    pub launch_args: Vec<String>,
    /// Later launches' command lines; see [`activation`].
    pub launches: Option<async_channel::Receiver<Vec<String>>>,
}

impl Global for Desktop {}

impl Desktop {
    pub fn load(launches: Option<async_channel::Receiver<Vec<String>>>, cx: &App) -> Self {
        let settings = DesktopSettings::load();
        let displays: Vec<WindowRect> = cx.displays().iter().map(|display| rect(display.bounds())).collect();
        let initial = window_state::initial_bounds(&settings, &displays);
        if settings.main_window_bounds.is_some() && matches!(initial, InitialBounds::Default { .. }) {
            log::warn!("saved main window bounds could not be restored; using defaults");
        }
        Self { settings, initial, launch_args: std::env::args().skip(1).collect(), launches }
    }

    pub fn window_bounds(&self, cx: &App) -> WindowBounds {
        match self.initial {
            InitialBounds::Restored { rect, maximized } => {
                let bounds = Bounds::new(
                    point(px(rect.x as f32), px(rect.y as f32)),
                    size(px(rect.width as f32), px(rect.height as f32)),
                );
                if maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) }
            }
            InitialBounds::Default { width, height } => {
                WindowBounds::Windowed(Bounds::centered(None, size(px(width as f32), px(height as f32)), cx))
            }
        }
    }
}

pub fn rect(bounds: Bounds<Pixels>) -> WindowRect {
    let round = |value: Pixels| f32::from(value).round() as i32;
    WindowRect {
        x: round(bounds.origin.x),
        y: round(bounds.origin.y),
        width: round(bounds.size.width),
        height: round(bounds.size.height),
    }
}

/// The normal (restored) bounds and whether the window is maximized. Fullscreen keeps the
/// bounds it will return to, like Electron's `getNormalBounds`.
pub fn normal_bounds(bounds: WindowBounds) -> (WindowRect, bool) {
    match bounds {
        WindowBounds::Windowed(bounds) | WindowBounds::Fullscreen(bounds) => (rect(bounds), false),
        WindowBounds::Maximized(bounds) => (rect(bounds), true),
    }
}
