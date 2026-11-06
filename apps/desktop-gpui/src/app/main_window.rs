//! The app's main window: opening it, bringing it back after macOS closed it (Dock reopen,
//! later launches, menu actions), and handing later launches' app links to it.

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Entity, Global, TitlebarOptions, WeakEntity, WindowDecorations,
    WindowOptions, point, px, size,
};

use super::Workspace;
use crate::desktop::{self, Desktop};
use crate::environments::PrimaryBackend;

/// The main window and its workspace, while it is open.
pub struct MainWindow {
    pub window: AnyWindowHandle,
    pub workspace: WeakEntity<Workspace>,
}

impl Global for MainWindow {}

/// Opens the app's window where it was last closed. The app draws its own titlebar: Windows
/// keeps the native resize frame, macOS its traffic lights, and Linux draws client-side
/// decorations like Electron's frameless `titleBarOverlay` window, on Wayland and X11, with or
/// without a compositor.
pub fn open(cx: &mut App) -> Option<(AnyWindowHandle, Entity<Workspace>)> {
    // The window reuses the app's backend: the first window starts it, a later one restarts
    // it at once when it is down.
    match cx.try_global::<PrimaryBackend>() {
        Some(backend) => backend.restart(),
        None => cx.set_global(PrimaryBackend::start(crate::runtime().handle())),
    }
    let (x, y) = super::TRAFFIC_LIGHTS;
    let (min_width, min_height) = desktop::settings::MIN_WINDOW_SIZE;
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("T3 Code".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(x), px(y))),
        }),
        window_bounds: Some(cx.global::<Desktop>().window_bounds(cx)),
        window_min_size: Some(size(px(min_width as f32), px(min_height as f32))),
        window_decorations: cfg!(target_os = "linux").then_some(WindowDecorations::Client),
        // Linux docks group the window under its desktop entry by this id (Wayland `app_id`, X11 `WM_CLASS`).
        app_id: Some(desktop::APP_ID.to_owned()),
        #[cfg(target_os = "linux")]
        icon: desktop::window_icon(),
        ..Default::default()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        crate::theme::sync(window, cx);
        cx.new(|cx| Workspace::new(window, cx))
    });
    let (window, workspace) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            log::error!("could not open the window: {error:#}");
            return None;
        }
    };
    cx.set_global(MainWindow { window, workspace: workspace.downgrade() });
    Some((window, workspace))
}

/// The main window brought forward (Electron's `revealWindow`), or opened again where macOS
/// closed it (`activate`), without this launch's app links.
pub fn ensure(cx: &mut App) -> Option<(AnyWindowHandle, Entity<Workspace>)> {
    let open_window = cx.try_global::<MainWindow>().and_then(|main| {
        let workspace = main.workspace.upgrade()?;
        cx.windows()
            .iter()
            .any(|window| window.window_id() == main.window.window_id())
            .then_some((main.window, workspace))
    });
    // `app.focus({ steal: true })` on macOS, then the window.
    #[cfg(target_os = "macos")]
    cx.activate(true);
    let Some((window, workspace)) = open_window else {
        cx.global_mut::<Desktop>().prepare_reopen();
        return open(cx);
    };
    if let Err(error) = window.update(cx, |_, window, _| window.activate_window()) {
        log::warn!("could not bring the window forward: {error:#}");
    }
    Some((window, workspace))
}

/// Later launches' command lines ([`desktop::activation`]) and, on macOS, URLs the OS opens
/// with the app: each brings the main window forward, reopening it if needed, and opens its
/// app links there.
pub fn receive_launches(launches: async_channel::Receiver<Vec<String>>, cx: &mut App) {
    cx.spawn(async move |cx| {
        while let Ok(args) = launches.recv().await {
            cx.update(|cx| {
                let Some((window, workspace)) = ensure(cx) else { return };
                let received = window
                    .update(cx, |_, window, cx| workspace.update(cx, |this, cx| this.receive_launch(args, window, cx)));
                if let Err(error) = received {
                    log::warn!("a later launch could not reach the window: {error:#}");
                }
            });
        }
    })
    .detach();
}
