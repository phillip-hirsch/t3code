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

/// The app's id: its Windows notification identity and, on Linux, the window's app id, which
/// names the desktop entry (`linux_url_handler::DESKTOP_ENTRY`).
pub const APP_ID: &str = "com.t3tools.t3code.gpui";

/// The window icon X11 docks show (`_NET_WM_ICON`); Wayland docks show the desktop entry's.
#[cfg(target_os = "linux")]
pub fn window_icon() -> Option<std::sync::Arc<image::RgbaImage>> {
    let png = include_bytes!("../../assets/icons/app-icon.png");
    match image::load_from_memory_with_format(png, image::ImageFormat::Png) {
        Ok(icon) => Some(std::sync::Arc::new(icon.into_rgba8())),
        Err(error) => {
            log::warn!("could not decode the window icon: {error}");
            None
        }
    }
}

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

/// SIGTERM and SIGINT quit the app as its Quit command does, so the local server stops first.
#[cfg(unix)]
pub fn quit_on_signals(cx: &mut App) {
    use tokio::signal::unix::{SignalKind, signal};
    let (received, signals) = async_channel::bounded(1);
    crate::runtime().spawn(async move {
        let (mut terminate, mut interrupt) = match (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) {
            (Ok(terminate), Ok(interrupt)) => (terminate, interrupt),
            (Err(error), _) | (_, Err(error)) => {
                log::warn!("could not listen for SIGTERM and SIGINT: {error}");
                return;
            }
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        let _ = received.send(()).await;
    });
    cx.spawn(async move |cx| {
        if signals.recv().await.is_ok() {
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
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
