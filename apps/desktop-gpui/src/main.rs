mod app;
mod assets;
mod backend;
mod device;
mod inbox;
mod keybindings;
mod model;
mod model_label;
mod new_thread;
mod pull_requests;
mod requests;
mod rpc;
mod settings;
mod search;
mod theme;
mod timeline;
mod usage;
mod work_log;

use std::sync::OnceLock;

use gpui_kit::{AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size};

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
    gpui_kit::application().with_assets(assets::Assets).run(|cx| {
        gpui_kit::init(cx);
        cx.text_system()
            .add_fonts(theme::FONTS.iter().map(|font| std::borrow::Cow::Borrowed(*font)).collect())
            .expect("bundled fonts");
        // The defaults until the server's keybindings arrive.
        app::keymap::install(&[], cx);
        cx.bind_keys([gpui_kit::KeyBinding::new("ctrl-z", app::UndoThreadAction, None)]);
        // The app draws its own 40px titlebar; Windows keeps the native resize frame.
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some("T3 Code".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(16.), px(13.))),
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(820.)), cx))),
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
