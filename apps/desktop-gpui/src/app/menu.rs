//! The macOS menu bar, as Electron's `DesktopApplicationMenu` builds it for darwin: the app
//! menu (About, Check for Updates, Settings, Services, Hide, Quit), File › Close Window, Edit
//! (with Paste as Text, Delete and Speech), View (Reload, Force Reload, Toggle Developer Tools,
//! Actual Size, Zoom In, Zoom Out, Toggle Full Screen), Window and Help. Windows and Linux hide
//! Electron's menu bar, so they get none, but keep its accelerators: Settings (Ctrl+,), Paste as
//! Text (Ctrl+Shift+V), Reload (Ctrl+R, Ctrl+Shift+R), the zoom keys (Ctrl+0, Ctrl+=, Ctrl+-, and
//! Ctrl+wheel) and Toggle Full Screen (F11).
//!
//! App zoom scales the whole interface (`Window::set_ui_scale`) and is saved with the desktop
//! settings. The keypad's 0, + and - arrive as the same keys as the number row's.
//!
//! The app has no web contents to reload: Reload and Force Reload start the backend connection
//! over, which reads every stream again. Toggle Developer Tools is GPUI's inspector, which only
//! debug builds carry.
//!
//! Inside a window, ⌘Q goes through the workspace's hold-to-quit gate (`confirmQuit`), so Quit's
//! key binding only matches outside every window's `Root` context. It still becomes the menu's
//! ⌘Q, which quits directly when no window has focus, like Electron's `role: "quit"`.

use std::io::Write as _;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gpui_kit::component::input;
use gpui_kit::{
    App, Context, DispatchPhase, Div, Focusable as _, Global, InteractiveElement as _, IntoElement, KeyBinding, Menu,
    MenuItem, OsAction, PromptLevel, ScrollDelta, ScrollEvent, Styled as _, SystemMenuType, Window, canvas,
};

use super::Workspace;
use crate::desktop::Desktop;
use crate::desktop::zoom::{self, ZoomStep};

gpui_kit::actions!(
    app_menu,
    [
        About,
        CheckForUpdates,
        OpenSettings,
        HideApp,
        HideOtherApps,
        ShowAllApps,
        QuitApp,
        CloseWindow,
        PasteAsText,
        StartSpeaking,
        StopSpeaking,
        Reload,
        ForceReload,
        ActualSize,
        ZoomIn,
        ZoomOut,
        ToggleFullScreen,
        Minimize,
        Zoom,
        BringAllToFront,
    ]
);

const APP_NAME: &str = "T3 Code";

/// Installs the menu bar (macOS only), its key bindings and its handlers.
pub fn init(cx: &mut App) {
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-h", HideApp, None),
            KeyBinding::new("alt-cmd-h", HideOtherApps, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
            KeyBinding::new("cmd-m", Minimize, None),
            KeyBinding::new("cmd-q", QuitApp, Some("!Root")),
            KeyBinding::new("cmd-shift-v", PasteAsText, Some("Input")),
            KeyBinding::new("cmd-r", Reload, None),
            KeyBinding::new("cmd-shift-r", ForceReload, None),
            // The menu shows each action's first binding.
            KeyBinding::new("cmd-0", ActualSize, None),
            KeyBinding::new("cmd-=", ZoomIn, None),
            KeyBinding::new("cmd-+", ZoomIn, None),
            KeyBinding::new("cmd-shift-=", ZoomIn, None),
            KeyBinding::new("cmd--", ZoomOut, None),
        ]);
    } else {
        // The terminal keeps Ctrl+R and Ctrl+Shift+R, as xterm takes them before Electron's menu.
        cx.bind_keys([
            KeyBinding::new("ctrl-,", OpenSettings, None),
            KeyBinding::new("ctrl-shift-v", PasteAsText, Some("Input")),
            KeyBinding::new("ctrl-r", Reload, Some("!Terminal")),
            KeyBinding::new("ctrl-shift-r", ForceReload, Some("!Terminal")),
            KeyBinding::new("f11", ToggleFullScreen, None),
            KeyBinding::new("ctrl-0", ActualSize, None),
            KeyBinding::new("ctrl-=", ZoomIn, None),
            KeyBinding::new("ctrl-+", ZoomIn, None),
            // xterm sends Ctrl+- to the shell (^_), before Electron's menu sees it.
            KeyBinding::new("ctrl--", ZoomOut, Some("!Terminal")),
        ]);
    }
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOtherApps, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
    cx.on_action(|_: &QuitApp, cx| cx.quit());
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, gpui_kit::Window::remove_window));
    cx.on_action(|_: &ToggleFullScreen, cx| with_active_window(cx, |window| window.toggle_fullscreen()));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |window| window.zoom_window()));
    cx.on_action(|_: &OpenSettings, cx| with_workspace(cx, |workspace, _, cx| workspace.open_settings("general", cx)));
    cx.on_action(|_: &Reload, cx| with_workspace(cx, Workspace::reconnect));
    cx.on_action(|_: &ForceReload, cx| with_workspace(cx, Workspace::reconnect));
    cx.on_action(|_: &ActualSize, cx| zoom_app(ZoomStep::Reset, cx));
    cx.on_action(|_: &ZoomIn, cx| zoom_app(ZoomStep::In, cx));
    cx.on_action(|_: &ZoomOut, cx| zoom_app(ZoomStep::Out, cx));
    cx.on_action(|_: &StopSpeaking, cx| stop_speaking(cx));
    cx.on_action(|_: &About, _| show_about_panel());
    cx.on_action(|_: &CheckForUpdates, cx| {
        with_workspace(cx, |workspace, window, cx| {
            // `handleCheckForUpdatesMenuClick` while updates are disabled.
            let detail = workspace.about_update_description();
            // The prompt stays up without its answer receiver; there is nothing to do after OK.
            drop(window.prompt(
                PromptLevel::Info,
                "Automatic updates are not available right now.",
                Some(&detail),
                &["OK"],
                cx,
            ));
        });
    });
    if cfg!(target_os = "macos") {
        cx.set_menus(menus());
    }
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: APP_NAME.into(),
            items: vec![
                MenuItem::action(format!("About {APP_NAME}"), About),
                MenuItem::action("Check for Updates...", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::action("Settings...", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(format!("Hide {APP_NAME}"), HideApp),
                MenuItem::action("Hide Others", HideOtherApps),
                MenuItem::action("Show All", ShowAllApps),
                MenuItem::separator(),
                MenuItem::action(format!("Quit {APP_NAME}"), QuitApp),
            ],
            disabled: false,
        },
        // ⌘W belongs to the workspace's tab close; the menu item has no key equivalent.
        Menu { name: "File".into(), items: vec![MenuItem::action("Close Window", CloseWindow)], disabled: false },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
                MenuItem::action("Paste as Text", PasteAsText),
                MenuItem::action("Delete", input::Delete),
                MenuItem::separator(),
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::submenu(Menu {
                    name: "Speech".into(),
                    items: vec![
                        MenuItem::action("Start Speaking", StartSpeaking),
                        MenuItem::action("Stop Speaking", StopSpeaking),
                    ],
                    disabled: false,
                }),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Reload", Reload),
                MenuItem::action("Force Reload", ForceReload),
                // ⌥⌘I, bound by gpui-component.
                #[cfg(debug_assertions)]
                MenuItem::action("Toggle Developer Tools", gpui_kit::component::ToggleInspector),
                MenuItem::separator(),
                MenuItem::action("Actual Size", ActualSize),
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::separator(),
                MenuItem::action("Toggle Full Screen", ToggleFullScreen),
            ],
            disabled: false,
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Bring All to Front", BringAllToFront),
            ],
            disabled: false,
        },
        Menu {
            name: "Help".into(),
            items: vec![MenuItem::action("Check for Updates...", CheckForUpdates)],
            disabled: false,
        },
    ]
}

impl Workspace {
    /// The Edit menu's actions that act on the focused field.
    pub(super) fn on_menu_actions(&self, root: Div, cx: &mut Context<Self>) -> Div {
        root.on_action(cx.listener(|this, _: &PasteAsText, window, cx| this.paste_as_text(window, cx))).on_action(
            cx.listener(|this, _: &StartSpeaking, window, cx| {
                if let Some(text) = this.composer_text_to_speak(window, cx) {
                    start_speaking(text, cx);
                }
            }),
        )
    }

    /// Paste as Text: the clipboard's text goes into the composer as typed, never folded into an
    /// attachment and never attaching what the clipboard also holds (`consumePasteAsTextShortcut`).
    /// Other fields only paste text anyway.
    fn paste_as_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.composer.read(cx).focus_handle(cx).is_focused(window) {
            window.dispatch_action(Box::new(input::Paste), cx);
            return;
        }
        if !self.question_takes_text(super::side_chat::ChatPane::Main) {
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()).filter(|text| !text.is_empty()) else {
            return;
        };
        self.composer.update(cx, |state, cx| state.insert(text, window, cx));
    }

    /// What Start Speaking reads in the focused composer: its selection, else all of it, as
    /// AppKit's text views do.
    fn composer_text_to_speak(&self, window: &Window, cx: &App) -> Option<String> {
        let state = self.composer.read(cx);
        if !state.focus_handle(cx).is_focused(window) {
            return None;
        }
        let value = state.value();
        let selected = state.selected_range();
        let text = value.get(selected).filter(|text| !text.is_empty()).unwrap_or(&value);
        (!text.trim().is_empty()).then(|| text.to_owned())
    }
}

/// One Start Speaking's `say` process, shared with the background task that feeds it.
#[derive(Default)]
struct Utterance {
    child: Option<Child>,
    stopped: bool,
}

/// The utterance Start Speaking began, which Stop Speaking (or the next start) ends.
struct Speech(Arc<Mutex<Utterance>>);

impl Global for Speech {}

fn lock(utterance: &Mutex<Utterance>) -> MutexGuard<'_, Utterance> {
    utterance.lock().unwrap_or_else(PoisonError::into_inner)
}

/// AppKit's `startSpeaking:`, through macOS's `say`, which reads the text from its input. The
/// process starts, is fed and is reaped off the UI thread: a long text fills the pipe while `say`
/// is still speaking its start.
fn start_speaking(text: String, cx: &mut App) {
    stop_speaking(cx);
    if !cfg!(target_os = "macos") {
        return;
    }
    let utterance = Arc::new(Mutex::new(Utterance::default()));
    cx.set_global(Speech(utterance.clone()));
    cx.background_executor().spawn(async move { speak(&utterance, &text) }).detach();
}

fn speak(utterance: &Mutex<Utterance>, text: &str) {
    let spawned =
        Command::new("/usr/bin/say").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            log::warn!("could not start speaking: {error}");
            return;
        }
    };
    let input = child.stdin.take();
    {
        let mut state = lock(utterance);
        if state.stopped {
            drop(state);
            end_speech(child);
            return;
        }
        state.child = Some(child);
    }
    // Dropping the input closes it, so `say` reads all of it and speaks. Stopping breaks the pipe.
    if let Some(mut input) = input
        && let Err(error) = input.write_all(text.as_bytes())
        && !lock(utterance).stopped
    {
        log::warn!("could not hand the text to speech: {error}");
    }
}

/// Already finished is fine; the wait reaps it either way.
fn end_speech(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn stop_speaking(cx: &mut App) {
    let Some(utterance) = cx.try_global::<Speech>().map(|speech| speech.0.clone()) else { return };
    let child = {
        let mut state = lock(&utterance);
        state.stopped = true;
        state.child.take()
    };
    if let Some(child) = child {
        cx.background_executor().spawn(async move { end_speech(child) }).detach();
    }
}

/// `zoomWindow`: half a zoom level in or out, or back to Actual Size, saved and applied to the
/// main window once GPUI hands the active window back.
pub(super) fn zoom_app(step: ZoomStep, cx: &mut App) {
    cx.defer(move |cx| {
        let settings = &mut cx.global_mut::<Desktop>().settings;
        let level = zoom::step(settings.zoom_level, step);
        if (level - settings.zoom_level).abs() < f64::EPSILON {
            return;
        }
        settings.zoom_level = level;
        settings.save();
        let Some(window) = cx.try_global::<super::main_window::MainWindow>().map(|main| main.window) else { return };
        if let Err(error) = window.update(cx, |_, window, cx| apply_zoom(level, window, cx)) {
            log::warn!("zoom could not reach the window: {error:#}");
        }
    });
}

/// Ctrl+wheel off macOS zooms the app instead of scrolling (Chromium's `ContentsZoomChange`), a
/// step per wheel notch; touchpads' pixel scrolls don't zoom. The image viewer takes its own.
pub(super) fn wheel_zoom_listener(image_viewer_open: bool) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |_, (), window, _| {
            window.on_mouse_event(move |event: &ScrollEvent, phase, _, cx| {
                let ScrollDelta::Lines(lines) = event.delta else { return };
                if phase != DispatchPhase::Capture || !event.modifiers.control || lines.y == 0. || image_viewer_open {
                    return;
                }
                cx.stop_propagation();
                zoom_app(if lines.y > 0. { ZoomStep::In } else { ZoomStep::Out }, cx);
            });
        },
    )
    .absolute()
    .size_0()
}

/// Scales `window`'s interface to `level`. macOS's traffic lights keep their size and stay
/// centered on the zoomed top row (`syncMacosWindowButtons`).
pub(super) fn apply_zoom(level: f64, window: &mut Window, cx: &mut App) {
    let scale = zoom::factor(level);
    window.set_ui_scale(scale, cx);
    #[cfg(target_os = "macos")]
    window.set_traffic_light_position(super::titlebar::traffic_lights(scale));
}

/// Acts on the focused window. GPUI runs app-level action handlers while it holds that window
/// (`App::dispatch_action`), so the update waits until it is handed back.
fn with_active_window(cx: &mut App, act: impl FnOnce(&mut gpui_kit::Window) + 'static) {
    cx.defer(|cx| {
        let Some(window) = cx.active_window() else { return };
        if let Err(error) = window.update(cx, |_, window, _| act(window)) {
            log::warn!("menu action could not reach the active window: {error:#}");
        }
    });
}

/// Acts on the main window's workspace, after GPUI hands the active window back; opens the
/// window again if macOS closed it.
fn with_workspace(
    cx: &mut App,
    act: impl FnOnce(&mut Workspace, &mut gpui_kit::Window, &mut gpui_kit::Context<Workspace>) + 'static,
) {
    cx.defer(move |cx| {
        let Some((window, workspace)) = super::main_window::ensure(cx) else { return };
        if let Err(error) =
            window.update(cx, |_, window, cx| workspace.update(cx, |workspace, cx| act(workspace, window, cx)))
        {
            log::warn!("menu action could not reach the workspace: {error:#}");
        }
    });
}

/// Electron's `role: "about"`: AppKit's standard About panel, which reads the bundle's name and
/// version.
fn show_about_panel() {
    #[cfg(target_os = "macos")]
    {
        let Some(main_thread) = objc2_foundation::MainThreadMarker::new() else {
            log::warn!("the About panel can only open from the main thread");
            return;
        };
        objc2_app_kit::NSApplication::sharedApplication(main_thread).orderFrontStandardAboutPanel(None);
    }
}
