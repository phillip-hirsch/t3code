//! The macOS menu bar, as Electron's `DesktopApplicationMenu` builds it for darwin: the app
//! menu (About, Check for Updates, Settings, Services, Hide, Quit), File › Close Window, Edit,
//! View › Toggle Full Screen, Window and Help. Windows and Linux hide Electron's menu bar,
//! so they get none.
//!
//! Quit has no key binding here: ⌘Q goes through the workspace's hold-to-quit gate
//! (`confirmQuit`), which a menu key equivalent would bypass.

use gpui_kit::component::input;
use gpui_kit::{
    AnyWindowHandle, App, Global, KeyBinding, Menu, MenuItem, OsAction, PromptLevel, SystemMenuType, WeakEntity,
};

use super::Workspace;

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
        ToggleFullScreen,
        Minimize,
        Zoom,
        BringAllToFront,
    ]
);

const APP_NAME: &str = "T3 Code";

/// The window the menu acts on, and its workspace.
pub struct MainWindow {
    pub window: AnyWindowHandle,
    pub workspace: WeakEntity<Workspace>,
}

impl Global for MainWindow {}

/// Installs the menu bar and its handlers; nothing off macOS.
pub fn init(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-h", HideApp, None),
        KeyBinding::new("alt-cmd-h", HideOtherApps, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOtherApps, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApps, cx| cx.unhide_other_apps());
    cx.on_action(|_: &QuitApp, cx| cx.quit());
    cx.on_action(|_: &BringAllToFront, cx| cx.activate(true));
    cx.on_action(|_: &CloseWindow, cx| with_active_window(cx, |window| window.remove_window()));
    cx.on_action(|_: &ToggleFullScreen, cx| with_active_window(cx, |window| window.toggle_fullscreen()));
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |window| window.zoom_window()));
    cx.on_action(|_: &OpenSettings, cx| with_workspace(cx, |workspace, _, cx| workspace.open_settings("general", cx)));
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
        })
    });
    cx.set_menus(menus());
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
                MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
            ],
            disabled: false,
        },
        Menu {
            name: "View".into(),
            items: vec![MenuItem::action("Toggle Full Screen", ToggleFullScreen)],
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

/// Acts on the main window's workspace, after GPUI hands the active window back.
fn with_workspace(
    cx: &mut App,
    act: impl FnOnce(&mut Workspace, &mut gpui_kit::Window, &mut gpui_kit::Context<Workspace>) + 'static,
) {
    let Some(main) = cx.try_global::<MainWindow>() else { return };
    let (window, workspace) = (main.window, main.workspace.clone());
    cx.defer(move |cx| {
        let updated = window.update(cx, |_, window, cx| {
            window.activate_window();
            workspace.update(cx, |workspace, cx| act(workspace, window, cx))
        });
        if let Err(error) = updated.and_then(|updated| updated) {
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
