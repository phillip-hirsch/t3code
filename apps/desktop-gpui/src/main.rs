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
mod hex;
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
    let socket = match settings::t3_home() {
        Some(home) => {
            match desktop::activation::claim(&home, &desktop::launch_args(), links.clone(), runtime().handle()) {
                desktop::activation::Instance::Forwarded => return,
                desktop::activation::Instance::Primary { socket } => socket,
            }
        }
        None => None,
    };
    let app = gpui_kit::application().with_assets(assets::Assets);
    // macOS hands `t3code://` links to the running app as URLs (Electron's `open-url`).
    app.on_open_urls(move |urls| {
        let _ = links.try_send(urls);
    });
    // A Dock click brings the window forward, or opens it again when closed (Electron's `activate`).
    app.on_reopen(|cx| {
        app::main_window::ensure(cx);
    });
    app.run(move |cx| {
        gpui_kit::init(cx);
        // Windows shows OS notifications under this identity.
        cx.set_app_identity(desktop::APP_ID, "T3 Code");
        cx.text_system()
            .add_fonts(theme::FONTS.iter().map(|font| std::borrow::Cow::Borrowed(*font)).collect())
            .expect("bundled fonts");
        // Quitting stops the local server, as Electron's backend manager does. GPUI gives quit
        // handlers' futures only 200 ms, and macOS and Windows exit right after them, so the
        // handler itself waits for the SIGTERM, the grace period and any kill.
        cx.on_app_quit(|_| {
            stop_local_servers_before_exit();
            async {}
        })
        .detach();
        #[cfg(unix)]
        desktop::quit_on_signals(cx);
        cx.bind_keys(app::terminal_key_bindings());
        app::menu::init(cx);
        if let Some(socket) = socket {
            cx.on_app_quit(move |_| {
                desktop::activation::remove_socket(&socket);
                async {}
            })
            .detach();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        desktop::linux_url_handler::register();
        cx.set_global(desktop::Desktop::load(cx));
        app::main_window::open(cx);
        app::main_window::receive_launches(launches, cx);
    });
}

/// Blocks until every local server has exited: at most [`backend::STOP_TIMEOUT`] for SIGTERM,
/// then as long again for the kill.
fn stop_local_servers_before_exit() {
    let stopped = runtime()
        .block_on(async { tokio::time::timeout(backend::STOP_TIMEOUT * 2, backend::stop_local_servers()).await });
    if stopped.is_err() {
        log::warn!("the local server had not stopped when the app exited");
    }
}
