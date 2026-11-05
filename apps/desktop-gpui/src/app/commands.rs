//! What each keybinding command does, as the web's scopes register them
//! (`AppSidebarLayout.tsx`, `chatRouteCommands.ts`, `commandPaletteCommands.ts`,
//! `useChatShortcuts.ts` and its groups, `Sidebar.tsx`, `UsagePage.tsx`,
//! `ModelPickerContent.tsx`). A handler returns `false` to leave the key to whatever
//! is focused, as the web's handlers do. Commands this client has no surface for fall
//! through the same way.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{Context, Focusable as _, KeystrokeEvent, Window};

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
        if self.composer_menu_key(keystroke, window, cx) {
            return true;
        }
        let held = self.held_key.as_ref() == Some(keystroke);
        let mut context = self.shortcut_context(&event.context_stack, window, cx);
        // The sidebar's undo notice claims Mod+Z from text fields while it shows.
        if context.editable_focus && self.sidebar_open && self.sidebar.has_undo() {
            context.editable_focus = false;
        }
        let keymap = self.keymap.clone();
        let command = keymap::resolve(&keymap, keystroke, &context);
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
        let palette_open = self.palette.is_some();
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
        match command {
            "sidebar.toggle" => {
                self.sidebar_open = !self.sidebar_open;
                cx.notify();
            }
            "threadPanel.toggle" if chat => self.toggle_details(window, cx),
            "navigation.back" => self.go_back(window, cx),
            "navigation.forward" => self.go_forward(window, cx),
            "usage.open" => {
                self.close_palette(window, cx);
                self.open_page(pages::Page::Usage, cx);
            }
            "commandPalette.toggle" => self.open_palette(palette::View::Root, window, cx),
            "chat.new" if chat && !palette_open => self.new_thread(false, window, cx),
            "chat.newLocal" if chat && !palette_open => self.new_thread(true, window, cx),
            "preview.toggle" if chat && !palette_open => self.toggle_preview(window, cx),
            "preview.refresh" | "preview.focusUrl" | "preview.zoomIn" | "preview.zoomOut" | "preview.resetZoom"
                if chat && !palette_open =>
            {
                self.preview_command(command, window, cx);
            }
            "rightPanel.toggle" if chat => self.toggle_right_panel(window, cx),
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
                        self.draft_workspace_open_permissions(cx)
                    }
                    "modelPicker.toggle" | "composer.effort" if self.composer_picker_open() => {
                        self.close_model_picker(window, cx)
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
                return self.open_favorite_editor(cx);
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
        let Some(project) = self.target_project() else { return false };
        let scripts = super::pages::settings::code::projects::scripts::resolve_project_scripts(
            &self.server_info().settings,
            project,
        );
        let (Some(script), Some(cwd)) = (scripts.into_iter().find(|s| s.id == script_id), self.git_cwd()) else {
            return false;
        };
        window.dispatch_action(Box::new(super::RunInTerminal { cwd, command: script.command }), cx);
        true
    }

    /// `appearance.cycle`: System → Light → Dark, with a short toast.
    fn cycle_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut choice = self.client_settings.theme_choice();
        let (mode, label) = match choice.mode {
            ThemePreference::System => (ThemePreference::Light, "Light"),
            ThemePreference::Light => (ThemePreference::Dark, "Dark"),
            ThemePreference::Dark => (ThemePreference::System, "System"),
        };
        choice.mode = mode;
        self.set_theme_choice(choice, window, cx);
        struct AppearanceCycle;
        window.push_notification(Notification::new().title(format!("Appearance: {label}")).id::<AppearanceCycle>(), cx);
    }
}

/// A stop of the F6 cycle (`workspaceAreaProps` orders), skipping details, which take no focus here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Area {
    Sidebar,
    Conversation,
    Composer,
    Panel,
    Terminal,
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
    /// `moveWorkspaceFocus`: F6 moves focus to the next shown area; dialogs keep it.
    fn move_workspace_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() || self.palette.is_some() {
            return;
        }
        let chat = self.page.is_none() && self.target.is_some();
        let shown: Vec<Area> = [
            (Area::Sidebar, self.sidebar_open),
            (Area::Conversation, chat && matches!(self.target, Some(Target::Thread { .. }))),
            (Area::Composer, chat),
            (Area::Panel, matches!(self.active_surface(), Some(super::right_panel::Surface::Browser(_)))),
            (Area::Terminal, self.terminal_open()),
        ]
        .into_iter()
        .filter_map(|(area, on)| on.then_some(area))
        .collect();
        let stack = window.context_stack();
        let current = if self.sidebar.list_focus.is_focused(window) {
            Some(Area::Sidebar)
        } else if self.composer.focus_handle(cx).is_focused(window) {
            Some(Area::Composer)
        } else if keymap::focus_in(&stack, "Terminal") {
            Some(Area::Terminal)
        } else if keymap::focus_in(&stack, keymap::PREVIEW_CONTEXT) {
            Some(Area::Panel)
        } else if self.focus.is_focused(window) {
            Some(Area::Conversation)
        } else {
            None
        };
        match next_area(&shown, current, forward) {
            Some(Area::Sidebar) => self.sidebar.list_focus.focus(window, cx),
            Some(Area::Conversation) => self.focus.focus(window, cx),
            Some(Area::Composer) => self.focus_composer(window, cx),
            Some(Area::Panel) => self.preview_command("preview.focusUrl", window, cx),
            Some(Area::Terminal) => {
                self.focus_terminal_drawer(window, cx);
            }
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
