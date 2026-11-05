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
mod inbox;
mod keybindings;
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

use gpui_kit::{AppContext as _, TitlebarOptions, WindowOptions, point, px, size};

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

/// Set for the whole process: WebView2 is a child window, and GPUI's DirectComposition swap chain would cover it.
#[cfg(windows)]
const DISABLE_DIRECT_COMPOSITION: &str = "GPUI_DISABLE_DIRECT_COMPOSITION";

/// GPUI reads [`DISABLE_DIRECT_COMPOSITION`] only from the environment it starts with, so a launch
/// without it starts this executable again with it set, hands that process the foreground, and exits.
#[cfg(windows)]
fn relaunched_with_composition_disabled() -> bool {
    if std::env::var_os(DISABLE_DIRECT_COMPOSITION).is_some() {
        return false;
    }
    let child = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .env(DISABLE_DIRECT_COMPOSITION, "true")
            .spawn()
    });
    match child {
        Ok(child) => {
            let _ = winsafe::AllowSetForegroundWindow(Some(child.id()));
            true
        }
        Err(error) => {
            log::warn!("could not restart with {DISABLE_DIRECT_COMPOSITION} set: {error}");
            false
        }
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    #[cfg(windows)]
    if relaunched_with_composition_disabled() {
        return;
    }
    // A second launch hands its command line to the running app and exits.
    let launches = match settings::t3_home() {
        Some(home) => {
            let args = desktop::launch_args();
            match desktop::activation::claim(&home, &args, runtime().handle()) {
                desktop::activation::Instance::Forwarded => return,
                desktop::activation::Instance::Primary(launches) => Some(launches),
            }
        }
        None => None,
    };
    gpui_kit::application().with_assets(assets::Assets).run(|cx| {
        gpui_kit::init(cx);
        // Windows shows OS notifications under this identity.
        cx.set_app_identity("com.t3tools.t3code.gpui", "T3 Code");
        cx.text_system()
            .add_fonts(theme::FONTS.iter().map(|font| std::borrow::Cow::Borrowed(*font)).collect())
            .expect("bundled fonts");
        cx.bind_keys(app::terminal_key_bindings());
        let desktop = desktop::Desktop::load(launches, cx);
        let window_bounds = desktop.window_bounds(cx);
        cx.set_global(desktop);
        // The app draws its own 40px titlebar; Windows keeps the native resize frame.
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("T3 Code".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.), px(13.))),
            }),
            window_bounds: Some(window_bounds),
            window_min_size: Some(size(px(840.), px(620.))),
            ..Default::default()
        };
        gpui_kit::open_window(options, cx, |window, cx| {
            theme::sync(window, cx);
            cx.new(|cx| app::Workspace::new(window, cx))
        })
        .expect("failed to open window");
    });
}
