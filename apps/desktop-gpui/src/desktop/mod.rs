//! Desktop shell features of the Electron main process (`apps/desktop/src`) that this
//! native client owns itself: its settings file, window state, single-instance activation
//! and app links.

pub mod activation;
pub mod badge;
pub mod cli;
pub mod exposure;
pub mod links;
#[cfg(all(unix, not(target_os = "macos")))]
pub mod linux_url_handler;
pub mod settings;
pub mod shell_env;
pub mod snap_shot;
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
    /// Later launches' command lines ([`activation`]) and, on macOS, URLs the OS opens with the app.
    pub launches: async_channel::Receiver<Vec<String>>,
}

impl Global for Desktop {}

/// This process's top-level window whose handle is `hwnd`, as winsafe's handle type.
#[cfg(windows)]
pub fn top_level_window(hwnd: isize) -> Option<winsafe::HWND> {
    let mut found = None;
    // Returning false stops the walk, which EnumWindows reports as a failure.
    let _ = winsafe::EnumWindows(|window: winsafe::HWND| {
        if window.ptr() as isize != hwnd {
            return true;
        }
        found = Some(window);
        false
    });
    found
}

/// This launch's arguments; one that isn't valid Unicode (an unpaired surrogate) is read lossily.
pub fn launch_args() -> Vec<String> {
    std::env::args_os().skip(1).map(|arg| arg.to_string_lossy().into_owned()).collect()
}

impl Desktop {
    pub fn load(launches: async_channel::Receiver<Vec<String>>, cx: &App) -> Self {
        let settings = DesktopSettings::load();
        let displays: Vec<WindowRect> = cx.displays().iter().map(|display| rect(display.bounds())).collect();
        let initial = window_state::initial_bounds(&settings, &displays);
        if settings.main_window_bounds.is_some() && matches!(initial, InitialBounds::Default { .. }) {
            log::warn!("saved main window bounds could not be restored; using defaults");
        }
        Self { settings, initial, launch_args: launch_args(), launches }
    }

    /// Before macOS opens the closed window again: no replay of this launch's app links, and
    /// the bounds it was closed with.
    pub fn prepare_reopen(&mut self) {
        self.launch_args.clear();
        if let Some(rect) = self.settings.main_window_bounds {
            self.initial = InitialBounds::Restored { rect, maximized: self.settings.main_window_maximized };
        }
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
    #[expect(clippy::cast_possible_truncation, reason = "window positions in whole pixels")]
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
