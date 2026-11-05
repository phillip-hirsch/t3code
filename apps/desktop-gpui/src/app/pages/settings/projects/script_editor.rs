//! The add/edit action dialog (`ProjectScriptEditorDialog` in `apps/web/src/components/projectScriptEditor.tsx`).
//! Settings and the chat's "Add project script" both open it through
//! [`Workspace::open_project_script_editor`]; it saves into the project's settings overrides.

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity,
    Window, div, px,
};

use super::scripts::{
    SCRIPT_ICONS, ScriptInput, command_for_project_script, decode_keybinding, keybinding_from_keystroke,
    next_project_script_id, resolve_project_scripts, script_icon,
};
use crate::app::Workspace;
use crate::theme::{self, Tokens};

pub(super) struct ScriptEditor {
    workspace: WeakEntity<Workspace>,
    project_id: String,
    script_id: Option<String>,
    /// The ids of the project's current scripts, to validate a new script's shortcut.
    script_ids: Vec<String>,
    initial_keybinding: Option<String>,
    name: Entity<InputState>,
    command: Entity<TextareaState>,
    preview_url: Entity<InputState>,
    keybinding: String,
    keybinding_focus: FocusHandle,
    icon: String,
    icon_picker_open: bool,
    run_on_worktree_create: bool,
    wait_for_setup: bool,
    run_on_settle: bool,
    auto_open_preview: bool,
    error: Option<SharedString>,
    saving: bool,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    /// Opens the editor for one of the project's scripts, or for a new one with `None`.
    pub(in crate::app) fn open_project_script_editor(
        &mut self,
        project_id: &str,
        script_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.shell.projects.iter().find(|p| p.id == project_id) else { return };
        let initial = match script_id {
            None => ScriptInput::default(),
            Some(id) => {
                match resolve_project_scripts(&self.server_info.settings, project).iter().find(|s| s.id == id) {
                    Some(script) => ScriptInput::from_script(script, &self.server_info.keybindings_json),
                    None => return,
                }
            }
        };
        self.open_script_editor_with(project_id, script_id, initial, None, window, cx);
    }

    pub(super) fn open_script_editor_with(
        &mut self,
        project_id: &str,
        script_id: Option<&str>,
        initial: ScriptInput,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.shell.projects.iter().find(|p| p.id == project_id) else { return };
        let script_ids =
            resolve_project_scripts(&self.server_info.settings, project).into_iter().map(|s| s.id).collect();
        let workspace = cx.weak_entity();
        let editor =
            cx.new(|cx| ScriptEditor::new(workspace, project_id, script_id, script_ids, initial, error, window, cx));
        let title = if script_id.is_some() { "Edit Action" } else { "Add Action" };
        editor.update(cx, |editor, cx| editor.name.update(cx, |name, cx| name.focus(window, cx)));
        let footer =
            cx.new(|cx| EditorFooter { _observe: cx.observe(&editor, |_, _, cx| cx.notify()), editor: editor.clone() });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.w(px(512.)).title(title).child(editor.clone()).footer(footer.clone())
        });
    }
}

impl ScriptEditor {
    #[allow(clippy::too_many_arguments)]
    fn new(
        workspace: WeakEntity<Workspace>,
        project_id: &str,
        script_id: Option<&str>,
        script_ids: Vec<String>,
        initial: ScriptInput,
        error: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Test").default_value(initial.name.clone()));
        let command = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("bun test")
                .default_value(initial.command.clone())
        });
        let preview_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("http://localhost:5173")
                .default_value(initial.preview_url.clone().unwrap_or_default())
        });
        let submit_on_enter = |this: &mut Self,
                               _: &Entity<InputState>,
                               event: &InputEvent,
                               window: &mut Window,
                               cx: &mut Context<Self>| match event {
            InputEvent::PressEnter { .. } => this.save(window, cx),
            // The preview switch enables with the URL.
            InputEvent::Change => cx.notify(),
            _ => {}
        };
        let initial_keybinding = initial.keybinding.clone().flatten();
        Self {
            workspace,
            project_id: project_id.to_owned(),
            script_id: script_id.map(str::to_owned),
            script_ids,
            keybinding: initial_keybinding.clone().unwrap_or_default(),
            initial_keybinding,
            _subscriptions: vec![
                cx.subscribe_in(&name, window, submit_on_enter),
                cx.subscribe_in(&preview_url, window, submit_on_enter),
            ],
            name,
            command,
            preview_url,
            keybinding_focus: cx.focus_handle(),
            icon: initial.icon,
            icon_picker_open: false,
            run_on_worktree_create: initial.run_on_worktree_create,
            wait_for_setup: initial.wait_for_setup,
            run_on_settle: initial.run_on_settle,
            auto_open_preview: initial.auto_open_preview,
            error: error.map(Into::into),
            saving: false,
        }
    }

    fn has_preview_url(&self, cx: &Context<Self>) -> bool {
        !self.preview_url.read(cx).value().trim().is_empty()
    }

    /// `captureKeybinding`: Tab moves focus, Backspace/Delete clears, anything else records a shortcut.
    fn capture_keybinding(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "tab" || (key == "escape" && !event.keystroke.modifiers.modified()) {
            return;
        }
        cx.stop_propagation();
        if key == "backspace" || key == "delete" {
            self.keybinding.clear();
        } else if let Some(next) = keybinding_from_keystroke(&event.keystroke) {
            self.keybinding = next;
        }
        cx.notify();
    }

    /// The form's `submit`: validates, saves, and closes on success.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let name = self.name.read(cx).value().trim().to_owned();
        let command = self.command.read(cx).value().trim().to_owned();
        if name.is_empty() {
            return self.fail("Name is required.", cx);
        }
        if command.is_empty() {
            return self.fail("Command is required.", cx);
        }
        let validation_id = self
            .script_id
            .clone()
            .unwrap_or_else(|| next_project_script_id(&name, self.script_ids.iter().map(String::as_str)));
        let key = match decode_keybinding(Some(&self.keybinding), command_for_project_script(&validation_id).as_deref())
        {
            Ok(key) => key,
            Err(error) => return self.fail(&error, cx),
        };
        let changes_keybinding =
            key != self.initial_keybinding.as_deref().map(str::trim).filter(|k| !k.is_empty()).map(str::to_owned);
        let preview_url = Some(self.preview_url.read(cx).value().trim().to_owned()).filter(|url| !url.is_empty());
        let input = ScriptInput {
            name,
            command,
            icon: self.icon.clone(),
            run_on_worktree_create: self.run_on_worktree_create,
            wait_for_setup: self.run_on_worktree_create && self.wait_for_setup,
            run_on_settle: self.run_on_settle,
            keybinding: (self.script_id.is_none() || changes_keybinding).then_some(key),
            auto_open_preview: preview_url.is_some() && self.auto_open_preview,
            preview_url,
        };
        let (project_id, script_id) = (self.project_id.clone(), self.script_id.clone());
        let Ok(task) = self
            .workspace
            .update(cx, |workspace, cx| workspace.save_project_script(&project_id, script_id.as_deref(), &input, cx))
        else {
            return;
        };
        self.error = None;
        self.saving = true;
        self.icon_picker_open = false;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(()) => window.close_dialog(cx),
                    Err(error) => this.error = Some(error.into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn fail(&mut self, message: &str, cx: &mut Context<Self>) {
        self.error = Some(SharedString::from(message.to_owned()));
        cx.notify();
    }

    /// The delete confirmation; confirming closes both dialogs and deletes in the background.
    fn request_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(script_id) = self.script_id.clone() else { return };
        let name = self.name.read(cx).value().to_string();
        let (workspace, project_id) = (self.workspace.clone(), self.project_id.clone());
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (workspace, project_id, script_id) = (workspace.clone(), project_id.clone(), script_id.clone());
            alert
                .title(format!("Delete action \"{name}\"?"))
                .description("This action cannot be undone.")
                .confirm()
                .ok_text("Delete action")
                .ok_variant(ButtonVariant::Danger)
                .cancel_text("Cancel")
                .on_ok(move |_, window, cx| {
                    window.close_all_dialogs(cx);
                    let _ = workspace.update(cx, |workspace, cx| {
                        let task = workspace.delete_project_script(&project_id, &script_id, cx);
                        workspace.report_scripts_failure(task, window, cx);
                    });
                    false
                })
        });
    }

    fn render_icon_picker(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let current = self.icon.clone();
        let tokens = *t;
        Popover::new("script-icon-picker")
            .open(self.icon_picker_open)
            .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                this.icon_picker_open = *open;
                cx.notify();
            }))
            .trigger(
                Button::new("script-icon")
                    .outline()
                    .size(px(36.))
                    .flex_none()
                    .disabled(self.saving)
                    .tooltip("Choose icon")
                    .child(Icon::new(script_icon(&self.icon)).with_size(px(18.))),
            )
            .content(move |_, _, _| {
                let t = tokens;
                div().grid().grid_cols(3).gap(px(8.)).children(SCRIPT_ICONS.iter().map(|(id, label)| {
                    let selected = *id == current;
                    let view = view.clone();
                    div()
                        .id(SharedString::from(format!("script-icon-{id}")))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(6.))
                        .border_1()
                        .px(px(8.))
                        .py(px(8.))
                        .text_xs()
                        .cursor_pointer()
                        .map(|d| {
                            if selected {
                                d.border_color(t.primary.opacity(0.7)).bg(t.primary.opacity(0.1))
                            } else if t.dark {
                                d.border_color(gpui_kit::transparent_black())
                                    .bg(gpui_kit::white().opacity(0.035))
                                    .hover(|s| s.bg(t.accent.opacity(0.6)))
                            } else {
                                d.border_color(t.border.opacity(0.7)).hover(|s| s.bg(t.accent.opacity(0.6)))
                            }
                        })
                        .on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.icon = (*id).to_owned();
                                this.icon_picker_open = false;
                                cx.notify();
                            })
                        })
                        .child(Icon::new(script_icon(id)).with_size(px(16.)))
                        .child(*label)
                }))
            })
    }

    fn render_keybinding(&self, t: &Tokens, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.keybinding_focus.is_focused(window);
        let empty = self.keybinding.is_empty();
        div()
            .id("script-keybinding")
            .track_focus(&self.keybinding_focus)
            .on_key_down(cx.listener(Self::capture_keybinding))
            .h(px(36.))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(8.))
            .border_1()
            .border_color(if focused { t.ring } else { t.input })
            .text_sm()
            .text_color(if empty { t.muted_foreground } else { t.foreground })
            .cursor_text()
            .child(if empty { "Press shortcut".to_owned() } else { self.keybinding.clone() })
    }
}

/// A switch handler that writes one field of the form.
fn set(
    view: Entity<ScriptEditor>,
    apply: fn(&mut ScriptEditor, bool),
) -> impl Fn(&bool, &mut Window, &mut gpui_kit::App) + 'static {
    move |checked, _, cx| {
        view.update(cx, |this, cx| {
            apply(this, *checked);
            cx.notify();
        })
    }
}

/// A form label (`Label`).
fn label(text: &'static str) -> impl IntoElement {
    div().text_sm().font_weight(FontWeight::MEDIUM).child(text)
}

fn help(text: &'static str, t: &Tokens) -> impl IntoElement {
    div().text_xs().text_color(t.muted_foreground).child(text)
}

/// A bordered switch row (`label.flex … rounded-md border px-3 py-2`).
fn toggle(
    id: &'static str,
    text: &'static str,
    checked: bool,
    enabled: bool,
    t: &Tokens,
    on_change: impl Fn(&bool, &mut Window, &mut gpui_kit::App) + 'static,
) -> impl IntoElement {
    h_flex()
        .justify_between()
        .gap(px(12.))
        .rounded(px(6.))
        .border_1()
        .px(px(12.))
        .py(px(8.))
        .text_sm()
        .map(|d| {
            if t.dark {
                d.border_color(gpui_kit::transparent_black()).bg(gpui_kit::white().opacity(0.035))
            } else {
                d.border_color(t.border.opacity(0.7))
            }
        })
        .when(!enabled, |d| d.opacity(0.6))
        .child(text)
        .child(Switch::new(id).checked(checked).disabled(!enabled).on_change(on_change))
}

impl Render for ScriptEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let has_preview = self.has_preview_url(cx);
        v_flex()
            .gap(px(16.))
            .child(div().text_sm().text_color(t.muted_foreground).child(
                "Actions are project-scoped commands you can run from the top bar or keybindings.",
            ))
            .child(
                v_flex().gap(px(6.)).child(label("Name")).child(
                    h_flex()
                        .gap(px(8.))
                        .child(self.render_icon_picker(&t, cx))
                        .child(div().flex_1().child(Input::new(&self.name).disabled(self.saving))),
                ),
            )
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(label("Keybinding"))
                    .child(self.render_keybinding(&t, window, cx))
                    .child(help(
                        "Press a shortcut. Use Backspace to clear. Shortcuts are environment-wide. Projects using the same action share its shortcut.",
                        &t,
                    )),
            )
            .child(v_flex().gap(px(6.)).child(label("Command")).child(Textarea::new(&self.command).disabled(self.saving)))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(label("Preview URL (optional)"))
                    .child(Input::new(&self.preview_url).disabled(self.saving))
                    .child(help("Open this URL in the in-app preview when this action runs.", &t)),
            )
            .child(toggle(
                "script-run-on-create",
                "Run automatically on worktree creation",
                self.run_on_worktree_create,
                !self.saving,
                &t,
                set(cx.entity(), |this, on| this.run_on_worktree_create = on),
            ))
            .child(toggle(
                "script-wait-for-setup",
                "Wait for it to finish before the agent starts",
                self.wait_for_setup,
                !self.saving && self.run_on_worktree_create,
                &t,
                set(cx.entity(), |this, on| this.wait_for_setup = on),
            ))
            .child(toggle(
                "script-run-on-settle",
                "Run in the thread's worktree when the thread settles",
                self.run_on_settle,
                !self.saving,
                &t,
                set(cx.entity(), |this, on| this.run_on_settle = on),
            ))
            .child(toggle(
                "script-auto-open-preview",
                "Open preview automatically when this action runs",
                self.auto_open_preview,
                !self.saving && has_preview,
                &t,
                set(cx.entity(), |this, on| this.auto_open_preview = on),
            ))
            .children(self.error.clone().map(|error| div().text_sm().text_color(t.error).child(error)))
    }
}

/// The dialog's pinned footer (`DialogFooter variant="bare"`), a view of its own so it follows the form.
struct EditorFooter {
    editor: Entity<ScriptEditor>,
    _observe: Subscription,
}

impl Render for EditorFooter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let form = self.editor.read(cx);
        let (editing, saving) = (form.script_id.is_some(), form.saving);
        let editor = self.editor.clone();
        let on_editor = move |run: fn(&mut ScriptEditor, &mut Window, &mut Context<ScriptEditor>)| {
            let editor = editor.clone();
            move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
                editor.update(cx, |form, cx| run(form, window, cx))
            }
        };
        h_flex()
            .w_full()
            .gap(px(8.))
            .justify_end()
            .when(editing, |row| {
                row.child(
                    Button::new("delete-script")
                        .danger()
                        .outline()
                        .label("Delete")
                        .disabled(saving)
                        .on_click(on_editor(ScriptEditor::request_delete)),
                )
                .child(div().flex_1())
            })
            .child(
                Button::new("cancel-script")
                    .outline()
                    .label("Cancel")
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            )
            .child(
                Button::new("save-script")
                    .primary()
                    .disabled(saving)
                    .label(if saving {
                        "Saving…"
                    } else if editing {
                        "Save changes"
                    } else {
                        "Save action"
                    })
                    .on_click(on_editor(ScriptEditor::save)),
            )
    }
}
