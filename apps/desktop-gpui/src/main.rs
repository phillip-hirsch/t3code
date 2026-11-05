#![forbid(unsafe_code)]

mod app;
mod assets;
mod backend;
mod browser_history;
mod composer_markdown;
mod composer_search;
mod composer_text;
mod desktop;
mod device;
mod environments;
mod file_icons;
mod inbox;
mod keybindings;
mod load_balancing;
mod model;
mod model_label;
mod new_thread;
mod notifications;
mod open_vsx;
mod preview;
mod project_groups;
mod proposed_plan;
mod pull_request_detail;
mod pull_request_diff;
mod pull_requests;
mod queue;
mod quit_shortcut;
mod requests;
mod rpc;
mod search;
mod secure_storage;
mod settings;
mod snap_shot;
mod terminal_text;
mod tex;
mod theme;
mod theme_color;
mod theme_file;
mod themes;
mod thread_details;
mod thread_now;
mod thread_pull_requests;
mod timeline;
mod timestamp;
mod usage;
mod vscode_theme;
mod work_log;
mod workflow;
mod worktree_setup;

use std::sync::OnceLock;

use gpui_kit::{AppContext as _, TitlebarOptions, WindowDecorations, WindowOptions, point, px, size};

/// Network and process I/O run here; GPUI owns the UI thread.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("t3-io")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    // WebView2 is a child window, and GPUI's DirectComposition swap chain would cover it.
    #[cfg(windows)]
    gpui_windows::disable_direct_composition();
    // A second launch hands its command line to the running app and exits.
    let (links, launches) = async_channel::unbounded();
    if let Some(home) = settings::t3_home() {
        let args = desktop::launch_args();
        if desktop::activation::claim(&home, &args, links.clone(), runtime().handle())
            == desktop::activation::Instance::Forwarded
        {
            return;
        }
    }
    let app = gpui_kit::application().with_assets(assets::Assets);
    // macOS hands `t3code://` links to the running app as URLs (Electron's `open-url`).
    app.on_open_urls(move |urls| {
        let _ = links.try_send(urls);
    });
    // A Dock click with the window closed opens it again (Electron's `activate`).
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            cx.global_mut::<desktop::Desktop>().prepare_reopen();
            open_main_window(cx);
        } else {
            cx.activate(true);
        }
    });
    app.run(|cx| {
        gpui_kit::init(cx);
        // Windows shows OS notifications under this identity.
        cx.set_app_identity(desktop::APP_ID, "T3 Code");
        cx.text_system()
            .add_fonts(theme::FONTS.iter().map(|font| std::borrow::Cow::Borrowed(*font)).collect())
            .expect("bundled fonts");
        // Quitting stops the local server, as Electron's backend manager does. GPUI waits only
        // briefly for this; `main` waits for the rest after `run` returns.
        cx.on_app_quit(|_| backend::stop_local_servers()).detach();
        #[cfg(unix)]
        desktop::quit_on_signals(cx);
        cx.bind_keys(app::terminal_key_bindings());
        app::menu::init(cx);
        #[cfg(all(unix, not(target_os = "macos")))]
        desktop::linux_url_handler::register();
        cx.set_global(desktop::Desktop::load(launches, cx));
        open_main_window(cx);
    });
    // Linux and Windows return here once the app quits (macOS exits inside `run`).
    let stopped = runtime()
        .block_on(async { tokio::time::timeout(backend::STOP_TIMEOUT * 2, backend::stop_local_servers()).await });
    if stopped.is_err() {
        log::warn!("the local server had not stopped when the app exited");
    }
}

/// Opens the app's window where it was last closed. The app draws its own titlebar: Windows
/// keeps the native resize frame, macOS its traffic lights, and Linux draws client-side
/// decorations like Electron's frameless `titleBarOverlay` window, on Wayland and X11, with or
/// without a compositor.
fn open_main_window(cx: &mut gpui_kit::App) {
    let (x, y) = app::TRAFFIC_LIGHTS;
    let options = WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("T3 Code".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(x), px(y))),
        }),
        window_bounds: Some(cx.global::<desktop::Desktop>().window_bounds(cx)),
        window_min_size: Some(size(px(840.), px(620.))),
        window_decorations: cfg!(target_os = "linux").then_some(WindowDecorations::Client),
        // Linux docks group the window under its desktop entry by this id (Wayland `app_id`, X11 `WM_CLASS`).
        app_id: Some(desktop::APP_ID.to_owned()),
        #[cfg(target_os = "linux")]
        icon: desktop::window_icon(),
        ..Default::default()
    };
    let (window, workspace) = gpui_kit::open_window(options, cx, |window, cx| {
        theme::sync(window, cx);
        cx.new(|cx| app::Workspace::new(window, cx))
    })
    .expect("failed to open window");
    cx.set_global(app::menu::MainWindow { window, workspace: workspace.downgrade() });
}
