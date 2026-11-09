//! What each keybinding command does, as the web's scopes register them
//! (`AppSidebarLayout.tsx`, `chatRouteCommands.ts`, `commandPaletteCommands.ts`,
//! `useChatShortcuts.ts` and its groups, `Sidebar.tsx`, `UsagePage.tsx`,
//! `ModelPickerContent.tsx`). A handler returns `false` to leave the key to whatever
//! is focused, as the web's handlers do. Commands this client has no surface for fall
//! through the same way.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, Context, FocusHandle, Focusable as _, KeystrokeEvent, Window};

use super::keymap::{self, CAPTURE_CONTEXT};
use super::{Target, Workspace, pages, palette, send, terminal};
use crate::settings::ThemePreference;

impl Workspace {
    /// The window's keystroke interceptor: the composer's menus first, then the keymap,
    /// then the composer's own keys. Returns whether the key was consumed.
    pub(super) fn on_keystroke(&mut self, event: &KeystrokeEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        if self.snap_shot_record_key(keystroke, cx) {
            return true;
        }
        if keymap::focus_in(&event.context_stack, CAPTURE_CONTEXT) || self.recording_shortcut(cx) {
            return false;
        }
        if self.composer_menu_key(keystroke, window, cx)
            || self.pr_completion_key(keystroke, window, cx)
            || self.palette_tab(keystroke, window, cx)
        {
            return true;
        }
        let held = self.held_key.as_ref() == Some(keystroke);
        let mut context = self.shortcut_context(&event.context_stack, window, cx);
        // The sidebar's undo notice claims Mod+Z from text fields while it shows.
        if context.editable_focus && self.sidebar_open.shown() && self.sidebar.has_undo() {
            context.editable_focus = false;
        }
        let typing = context.editable_focus || context.combobox_focus || context.terminal_focus;
        if self.launcher_letter_key(keystroke, typing || context.layer_open, window, cx) {
            return true;
        }
        let keymap = self.keymap.clone();
        // `scriptKeybindings`: the open thread's environment binds its project scripts.
        let scripts = self.server_info().keybindings.clone();
        let command = keymap::resolve_chat(&keymap, &scripts, keystroke, &context);
        if let Some(command) = command {
            log::trace!("shortcut resolves to {command}");
        }
        // A confirmation keeps the app's shortcuts from acting behind it (`isInsideConfirmation`).
        if command.is_some() && keymap::focus_in(&event.context_stack, "Dialog") {
            return true;
        }
        if let Some(command) = command
            && self.run_command(command, held, &context, window, cx)
        {
            self.held_key = Some(keystroke.clone());
            return true;
        }
        // Inside a diff, Mod+F opens the diff's own find, whose handler runs first on the web.
        if !keymap::focus_in(&event.context_stack, "Diff") && self.pull_request_find_key(keystroke, window, cx) {
            return true;
        }
        self.composer_key(keystroke, &context, window, cx)
    }

    /// Runs `command`; `held` is a key repeat, which most commands swallow without acting.
    fn run_command(
        &mut self,
        command: &str,
        held: bool,
        context: &keymap::ShortcutContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let chat = self.page.is_none() && self.settings.is_none();
        // Any chat route not under a modal page, such as Pull requests (`installChatRouteCommands`).
        let chat_route = self.settings.is_none() && !self.page.is_some_and(pages::Page::is_modal);
        let palette_open = self.palette.is_some();
        // `useChatShortcuts` stands down while the palette is open (`isCommandPaletteOpen`).
        if palette_open && is_chat_shortcut(command) {
            return false;
        }
        if let Some(index) = keymap::jump_index(command, "modelPicker") {
            return self.model_picker_jump(index, window, cx);
        }
        if let Some(index) = keymap::jump_index(command, "thread") {
            return !held && !palette_open && !context.model_picker_open && self.go_to_thread_at(index, window, cx);
        }
        if let Some(script) = keymap::script_id(command) {
            return chat && self.run_script_shortcut(script, window, cx);
        }
        if let Some(handled) = self.usage_shortcut(command, context, cx) {
            return handled;
        }
        if self.page == Some(pages::Page::PullRequests)
            && self.settings.is_none()
            && !palette_open
            && self.pull_requests_page_command(command, held, window, cx)
        {
            return true;
        }
        match command {
            "sidebar.toggle" => self.toggle_sidebar(cx),
            "threadPanel.toggle" if chat => self.toggle_details(window, cx),
            "navigation.back" => self.go_back(window, cx),
            "navigation.forward" => self.go_forward(window, cx),
            "usage.open" => {
                self.close_palette(window, cx);
                self.open_page(pages::Page::Usage, cx);
            }
            "commandPalette.toggle" => self.open_palette(palette::View::Root, window, cx),
            "chat.new" if chat_route && !palette_open => self.new_thread(false, window, cx),
            "chat.newLocal" if chat_route && !palette_open => self.new_thread(true, window, cx),
            "chat.newWithoutProject" if chat_route && !palette_open => {
                let Some(environment) = self.scratch_environment() else { return false };
                if !held {
                    self.start_scratch_thread(environment, None, window, cx);
                }
            }
            "preview.toggle" if chat && !palette_open => self.toggle_preview(window, cx),
            "preview.refresh" | "preview.focusUrl" | "preview.zoomIn" | "preview.zoomOut" | "preview.resetZoom"
                if chat && !palette_open =>
            {
                self.preview_command(command, window, cx);
            }
            "rightPanel.toggle" if chat => self.toggle_right_panel(window, cx),
            "rightPanel.new" if chat => return self.open_add_surface_menu(context.layer_open, held, cx),
            "rightPanel.toggleMaximized" if chat => return self.toggle_right_panel_maximized(cx),
            "rightPanel.close" if chat => return held || self.close_active_surface(window, cx),
            "diff.toggle" if chat => self.toggle_diff(window, cx),
            "terminal.toggle" if chat => {
                if !self.terminal_open() && !self.terminal_available() {
                    return false;
                }
                window.dispatch_action(Box::new(terminal::ToggleTerminal), cx);
            }
            "terminal.split" | "terminal.splitVertical" | "terminal.new" | "terminal.close"
                if chat && self.panel_terminal_command(command, held, window, cx) => {}
            "terminal.split" if chat => window.dispatch_action(Box::new(terminal::SplitTerminal), cx),
            "terminal.splitVertical" if chat => window.dispatch_action(Box::new(terminal::SplitTerminalVertical), cx),
            "terminal.new" if chat => window.dispatch_action(Box::new(terminal::NewTerminal), cx),
            "terminal.close" if chat => {
                if !held {
                    window.dispatch_action(Box::new(terminal::CloseTerminal), cx);
                }
            }
            "thread.undo" => {
                if held {
                    return true;
                }
                if !chat || palette_open || context.model_picker_open || !self.sidebar.has_undo() {
                    return false;
                }
                self.undo_thread_action(window, cx);
            }
            "thread.previous" | "thread.next" => {
                return !held
                    && !palette_open
                    && !context.model_picker_open
                    && self.go_to_adjacent_thread(command == "thread.next", window, cx);
            }
            // The queue control's own guards decide; with nothing to act on the key falls through.
            "thread.steerQueuedMessage" if chat => return held || self.steer_next_queued(cx),
            "thread.editQueuedMessage" if chat => return held || self.edit_latest_queued(window, cx),
            "thread.stop" if chat => {
                if !self.turn_running() {
                    return false;
                }
                if !held {
                    self.stop(cx);
                }
            }
            "thread.settle" if chat => {
                let Some(Target::Thread { id, .. }) = self.target.clone() else { return false };
                let Some(thread) = self.shell.threads.iter().find(|t| t.id == id) else { return true };
                if thread.settled_override.as_deref() == Some("settled") {
                    self.unsettle_thread(&id, cx);
                } else {
                    self.settle_thread(&id, true, window, cx);
                }
            }
            "thread.pin" if chat => {
                let Some(Target::Thread { id, .. }) = self.target.clone() else { return false };
                let Some(thread) = self.shell.threads.iter().find(|t| t.id == id) else { return true };
                if thread.pinned_at.is_some() {
                    self.unpin_thread(&id, window, cx);
                } else {
                    self.pin_thread(&id, cx);
                }
            }
            "thread.copyReference" if chat => {
                if !held {
                    self.copy_thread_reference(window, cx);
                }
            }
            "modelPicker.toggle" | "composer.effort" | "composer.mode" if chat => {
                if held {
                    return true;
                }
                match command {
                    "composer.mode" if matches!(self.target, Some(Target::Draft { .. })) => {
                        self.draft_workspace_open_permissions(cx);
                    }
                    "modelPicker.toggle" | "composer.effort" if self.composer_picker_open() => {
                        self.close_model_picker(window, cx);
                    }
                    _ => self.open_model_picker(window, cx),
                }
            }
            "modelPicker.previousProvider" | "modelPicker.nextProvider" => {
                let step = if command == "modelPicker.nextProvider" { 1 } else { -1 };
                return self.model_picker_step_provider(step, window, cx);
            }
            "composer.host" if chat => {
                if !held {
                    return self.open_host_picker(cx);
                }
            }
            "composer.cycleHost" if chat => {
                if !held {
                    return self.cycle_host(window, cx);
                }
            }
            "pullRequest.copyNumber" if self.settings.is_none() && !palette_open => {
                return self.copy_pull_request_number(held, window, cx);
            }
            "composer.workspace" if chat => {
                if !held {
                    self.open_workspace_picker(cx);
                }
            }
            "composer.branch" if chat => {
                if !held {
                    self.open_branch_picker(window, cx);
                }
            }
            "composer.previousWorktree" if chat => {
                if !held {
                    self.use_previous_worktree(window, cx);
                }
            }
            "composer.sendAlternate" | "composer.sendBackground" | "composer.sendAndNewThread" => {
                let intent = match command {
                    "composer.sendAlternate" if context.turn_running => send::Intent::Alternate,
                    "composer.sendBackground" if context.draft_thread_route => send::Intent::Alternate,
                    "composer.sendAndNewThread" if !context.draft_thread_route => send::Intent::AndNewThread,
                    _ => return false,
                };
                if !context.composer_focus || held {
                    return context.composer_focus;
                }
                self.submit_with(intent, window, cx);
            }
            "editor.openFavorite" if chat => {
                if held {
                    return true;
                }
                return self.open_favorite_editor(window, cx);
            }
            "appearance.cycle" => {
                if !held {
                    self.cycle_appearance(window, cx);
                }
            }
            "workspace.focusNext" | "workspace.focusPrevious" => {
                self.move_workspace_focus(command == "workspace.focusNext", window, cx);
            }
            // Over the palette, find closes it and says why find can't open (`CommandPalette.tsx`).
            "chat.find" if palette_open => {
                self.close_palette(window, cx);
                self.open_thread_find_or_explain(window, cx);
            }
            "chat.find" if chat => {
                self.open_thread_find(window, cx);
            }
            "filePicker.toggle" => {
                self.toggle_file_overlay(super::files::OverlayMode::Files, window, cx);
            }
            "projectSearch.toggle" => {
                self.toggle_file_overlay(super::files::OverlayMode::Content, window, cx);
            }
            "composer.stash" if chat && !palette_open => self.stash_shortcut(window, cx),
            "theme.select" => {
                self.open_palette(palette::View::Themes, window, cx);
            }
            "themeEditor.toggle" => self.toggle_theme_editor(window, cx),
            _ => return false,
        }
        true
    }

    /// `thread.previous` / `thread.next` along the sidebar's order (`resolveAdjacentThreadId`).
    fn go_to_adjacent_thread(&mut self, next: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let current = match &self.target {
            Some(Target::Thread { id, .. }) => Some(id.as_str()),
            _ => None,
        };
        let order: Vec<&str> = self.sidebar.ordered_thread_ids().collect();
        let Some(id) = adjacent(&order, current, next).map(str::to_owned) else { return false };
        self.select_thread(id, window, cx);
        true
    }

    fn go_to_thread_at(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(id) = self.sidebar.ordered_thread_ids().nth(index).map(str::to_owned) else { return false };
        self.select_thread(id, window, cx);
        true
    }

    /// `script.<id>.run`: runs the open project's script with that id in the thread's terminal.
    fn run_script_shortcut(&mut self, script_id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(project) = self.target_project().filter(|_| self.can_run_scripts()) else { return false };
        let project_id = project.id.clone();
        let scripts = super::pages::settings::code::projects::scripts::resolve_project_scripts(
            &self.server_info().settings,
            project,
        );
        let (Some(script), Some(cwd)) = (scripts.into_iter().find(|s| s.id == script_id), self.git_cwd()) else {
            return false;
        };
        self.remember_script(&project_id, script_id);
        window.dispatch_action(Box::new(super::RunInTerminal::script(cwd, &script)), cx);
        true
    }

    /// `appearance.cycle`: System → Light → Dark, with a short toast.
    fn cycle_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        struct AppearanceCycle;
        let mut choice = self.client_settings.theme_choice();
        let (mode, label) = match choice.mode {
            ThemePreference::System => (ThemePreference::Light, "Light"),
            ThemePreference::Light => (ThemePreference::Dark, "Dark"),
            ThemePreference::Dark => (ThemePreference::System, "System"),
        };
        choice.mode = mode;
        self.set_theme_choice(choice, window, cx);
        window.push_notification(Notification::new().title(format!("Appearance: {label}")).id::<AppearanceCycle>(), cx);
    }
}

/// The commands `useChatShortcuts` runs for the open chat (its terminal, right panel, thread
/// lifecycle and composer groups, and project scripts), besides `chat.find`, which the palette
/// takes over.
fn is_chat_shortcut(command: &str) -> bool {
    keymap::script_id(command).is_some()
        || matches!(
            command,
            "terminal.toggle"
                | "terminal.split"
                | "terminal.splitVertical"
                | "terminal.close"
                | "terminal.new"
                | "rightPanel.toggle"
                | "rightPanel.toggleMaximized"
                | "rightPanel.close"
                | "threadPanel.toggle"
                | "diff.toggle"
                | "thread.settle"
                | "thread.pin"
                | "thread.copyReference"
                | "thread.stop"
                | "thread.steerQueuedMessage"
                | "thread.editQueuedMessage"
                | "composer.cycleHost"
                | "modelPicker.toggle"
                | "composer.increaseEffort"
                | "composer.decreaseEffort"
                | "composer.host"
                | "composer.effort"
                | "composer.mode"
                | "composer.workspace"
                | "composer.branch"
                | "composer.previousWorktree"
        )
}

/// A stop of the F6 cycle, in `workspaceAreaProps` order: the sidebar, the page's content (the
/// conversation, home or the Pull requests list), the composer, the thread details, the right
/// panel and the terminal. Settings has its own two: its sections and its page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Area {
    Sidebar,
    Content,
    Composer,
    Details,
    Panel,
    Terminal,
    SettingsNav,
    SettingsPage,
}

/// Focus targets for the F6 stops that are regions, not controls (`tabIndex: -1`).
pub(super) struct WorkspaceAreas {
    /// The Pull requests list.
    pub(super) content: FocusHandle,
    pub(super) details: FocusHandle,
    /// The right panel, or the Pull requests page's panel.
    pub(super) panel: FocusHandle,
    pub(super) settings_page: FocusHandle,
}

impl WorkspaceAreas {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            content: cx.focus_handle(),
            details: cx.focus_handle(),
            panel: cx.focus_handle(),
            settings_page: cx.focus_handle(),
        }
    }
}

/// The next shown area after `current` in cycle order, wrapping; from outside every area, the first.
fn next_area(shown: &[Area], current: Option<Area>, forward: bool) -> Option<Area> {
    let at = current.and_then(|area| shown.iter().position(|a| *a == area));
    let len = shown.len();
    if len == 0 {
        return None;
    }
    let index = match (at, forward) {
        (Some(at), true) => (at + 1) % len,
        (Some(at), false) => (at + len - 1) % len,
        (None, true) => 0,
        (None, false) => len - 1,
    };
    Some(shown[index])
}

impl Workspace {
    /// The areas on screen, in cycle order; a maximized panel hides the column beside it.
    fn shown_areas(&self, window: &Window) -> Vec<Area> {
        if self.settings.is_some() {
            return vec![Area::SettingsNav, Area::SettingsPage];
        }
        let sidebar = (Area::Sidebar, self.sidebar_open.shown());
        let areas = match self.page {
            // Usage is a modal with no areas of its own.
            Some(page) if page.is_modal() => return Vec::new(),
            Some(pages::Page::PullRequests) => {
                let list = !self.pull_request_panel_maximized();
                vec![sidebar, (Area::Content, list), (Area::Panel, self.pull_request_panel_shown())]
            }
            Some(_) => vec![sidebar],
            None => {
                let chat = !self.right_panel_maximized();
                vec![
                    sidebar,
                    (Area::Content, chat && matches!(self.target, None | Some(Target::Thread { .. }))),
                    (Area::Composer, chat && self.target.is_some()),
                    (Area::Details, chat && self.details_shown(window)),
                    (Area::Panel, self.right_panel_open()),
                    (Area::Terminal, chat && self.terminal_open()),
                ]
            }
        };
        areas.into_iter().filter_map(|(area, on)| on.then_some(area)).collect()
    }

    /// The area holding focus; a terminal tab in the right panel belongs to the panel.
    fn focused_area(&self, window: &Window, cx: &App) -> Option<Area> {
        let areas = &self.areas;
        if self.settings.is_some() {
            return if self.settings_nav_focused(window, cx) {
                Some(Area::SettingsNav)
            } else {
                areas.settings_page.contains_focused(window, cx).then_some(Area::SettingsPage)
            };
        }
        if self.sidebar.list_focus.is_focused(window) {
            Some(Area::Sidebar)
        } else if self.composer.focus_handle(cx).is_focused(window) {
            Some(Area::Composer)
        } else if areas.panel.contains_focused(window, cx)
            || (self.page.is_none() && keymap::focus_in(&window.context_stack(), keymap::PREVIEW_CONTEXT))
        {
            Some(Area::Panel)
        } else if areas.details.contains_focused(window, cx) {
            Some(Area::Details)
        } else if keymap::focus_in(&window.context_stack(), "Terminal") {
            Some(Area::Terminal)
        } else if areas.content.contains_focused(window, cx) || self.focus.is_focused(window) {
            Some(Area::Content)
        } else {
            None
        }
    }

    /// `moveWorkspaceFocus`: F6 moves focus to the next shown area; dialogs keep it.
    pub(super) fn move_workspace_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() || self.sidebar_open.sheet() || window.has_active_dialog(cx) {
            return;
        }
        let shown = self.shown_areas(window);
        let current = self.focused_area(window, cx);
        match next_area(&shown, current, forward) {
            Some(Area::Sidebar) => self.sidebar.list_focus.focus(window, cx),
            Some(Area::Content) if self.page.is_some() => self.areas.content.focus(window, cx),
            Some(Area::Content) => self.focus.focus(window, cx),
            Some(Area::Composer) => self.focus_composer(window, cx),
            Some(Area::Details) => self.areas.details.focus(window, cx),
            Some(Area::Panel) if matches!(self.active_surface(), Some(super::right_panel::Surface::Browser(_))) => {
                self.preview_command("preview.focusUrl", window, cx);
            }
            Some(Area::Panel) => self.areas.panel.focus(window, cx),
            Some(Area::Terminal) => {
                self.focus_terminal_drawer(window, cx);
            }
            Some(Area::SettingsNav) => self.focus_settings_nav(window, cx),
            Some(Area::SettingsPage) => self.areas.settings_page.focus(window, cx),
            None => {}
        }
        self.notify_sidebar(cx);
    }
}

/// `resolveAdjacentThreadId`: no wrap; from no thread, the first (or last) one.
fn adjacent<'a>(order: &[&'a str], current: Option<&str>, next: bool) -> Option<&'a str> {
    let Some(current) = current else {
        return if next { order.first() } else { order.last() }.copied();
    };
    let at = order.iter().position(|id| *id == current)?;
    let target = if next { at.checked_add(1) } else { at.checked_sub(1) };
    target.and_then(|i| order.get(i)).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_cycles_through_the_shown_areas() {
        let shown = [Area::Sidebar, Area::Composer, Area::Terminal];
        assert_eq!(next_area(&shown, Some(Area::Sidebar), true), Some(Area::Composer));
        assert_eq!(next_area(&shown, Some(Area::Terminal), true), Some(Area::Sidebar));
        assert_eq!(next_area(&shown, Some(Area::Sidebar), false), Some(Area::Terminal));
        assert_eq!(next_area(&shown, None, true), Some(Area::Sidebar));
        assert_eq!(
            next_area(&shown, Some(Area::Panel), false),
            Some(Area::Terminal),
            "a hidden area counts as outside"
        );
        assert_eq!(next_area(&[], None, true), None);
        let chat = [Area::Sidebar, Area::Content, Area::Composer, Area::Details, Area::Panel, Area::Terminal];
        assert_eq!(next_area(&chat, Some(Area::Composer), true), Some(Area::Details));
        assert_eq!(next_area(&chat, Some(Area::Details), true), Some(Area::Panel));
        assert_eq!(next_area(&chat, Some(Area::Sidebar), true), Some(Area::Content));
    }

    #[test]
    fn the_palette_suspends_the_chat_shortcuts_but_not_the_app_ones() {
        assert!(is_chat_shortcut("thread.settle") && is_chat_shortcut("terminal.toggle"));
        assert!(is_chat_shortcut("modelPicker.toggle") && is_chat_shortcut("script.build.run"));
        assert!(!is_chat_shortcut("chat.find"), "the palette runs find itself");
        assert!(!is_chat_shortcut("sidebar.toggle") && !is_chat_shortcut("commandPalette.toggle"));
        assert!(!is_chat_shortcut("chat.new"), "the palette runs its own new-thread rows");
    }

    #[test]
    fn thread_traversal_stops_at_the_ends() {
        let order = ["a", "b", "c"];
        assert_eq!(adjacent(&order, Some("b"), true), Some("c"));
        assert_eq!(adjacent(&order, Some("b"), false), Some("a"));
        assert_eq!(adjacent(&order, Some("c"), true), None);
        assert_eq!(adjacent(&order, Some("a"), false), None);
        assert_eq!(adjacent(&order, None, true), Some("a"));
        assert_eq!(adjacent(&order, None, false), Some("c"));
        assert_eq!(adjacent(&order, Some("gone"), true), None);
        assert_eq!(adjacent(&[], None, true), None);
    }
}
