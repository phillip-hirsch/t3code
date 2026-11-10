//! A project's actions (`ProjectActionsSettings.tsx`, `ProjectActionsList.tsx`).
//!
//! The list is the scoped `defaultProjectScripts` setting; at project scope it lives in
//! `projectSettingsOverrides[projectId]`. Its inheritance reset is a plain button here until the
//! shared settings row takes over at merge.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};

use super::scripts::{
    ScriptInput, command_for_project_script, importable_scripts, resolve_project_scripts, scoped_scripts_state,
    script_icon, shortcut_label_for_command,
};
use crate::app::Workspace;
use crate::app::pages::settings::connections::logic::SETTINGS_WRITE;
use crate::app::pages::settings::widgets::{RowFrame, reset_button, settings_section};
use crate::model::{ProjectScript, ProjectShell, ScopedId, T3ProjectFile};
use crate::theme::{self, Tokens};

const ROW_GROUP: &str = "project-action-row";

impl Workspace {
    /// The "Actions" section of a project's settings page.
    pub(super) fn render_project_actions(
        &mut self,
        project_key: &ScopedId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(project) = self.shell.project(project_key) else {
            return div().into_any_element();
        };
        let scripts = resolve_project_scripts(&self.project_info(project).settings, project);
        // Every scoped checkout is a target, read on its own environment; differing lists are
        // edited one environment at a time.
        let targets: Vec<(&serde_json::Value, &ProjectShell)> = self
            .script_targets(project_key)
            .iter()
            .filter_map(|target| self.shell.project(target))
            .map(|p| (&*self.project_info(p).settings, p))
            .collect();
        let (mixed, overridden) = scoped_scripts_state(&scripts, &targets);
        let (environment, root) = (project.environment_id.clone(), project.workspace_root.clone());
        let file = self.t3_project_file(&environment, &root, cx).clone();
        let importable: Vec<ScriptInput> =
            importable_scripts(&file, &scripts).into_iter().map(ScriptInput::from_file_script).collect();
        let pid = project_key.clone();

        let mut controls = h_flex().gap(px(6.)).flex_wrap();
        if overridden {
            let pid = pid.clone();
            controls = controls.child(
                reset_button(&"reset-project-actions".into(), "Reset actions to the environment's", cx).on_click(
                    cx.listener(move |this, _, window, cx| {
                        let task = this.reset_project_scripts(&pid, cx);
                        this.report_scripts_failure(task, window, cx);
                    }),
                ),
            );
        }
        if !importable.is_empty() {
            let view = cx.entity();
            let pid = pid.clone();
            controls = controls.child(
                Button::new("import-scripts")
                    .ghost()
                    .xsmall()
                    .label("Import scripts")
                    .dropdown_caret(true)
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                        menu = menu
                            .label("Import from t3.json")
                            .label("Add actions declared by this checkout without editing them first.")
                            .separator();
                        for input in &importable {
                            let (pid, input) = (pid.clone(), input.clone());
                            let label = format!("{}  ·  {}", input.name, input.command);
                            menu = menu.item(
                                PopupMenuItem::new(label).icon(Icon::new(script_icon(&input.icon))).on_click(
                                    window.listener_for(&view, move |this, _, window, cx| {
                                        this.import_project_script(&pid, &input, window, cx);
                                    }),
                                ),
                            );
                        }
                        menu
                    }),
            );
        }
        controls = controls.child(
            Button::new("add-project-action").outline().xsmall().icon(Lucide::Plus).label("Add action").on_click(
                cx.listener(move |this, _, window, cx| this.open_project_script_editor(&pid, None, window, cx)),
            ),
        );

        let mut list = vec![
            RowFrame::new("Actions")
                .description("Commands that run in this project's checkout or its worktree, with optional shortcuts.")
                .control(controls)
                .render(&t)
                .into_any_element(),
        ];
        if mixed {
            list.push(
                RowFrame::new("Different actions across environments")
                    .description("Choose one environment to edit its list. Adding an action here adds it on every selected environment.")
                    .render(&t)
                    .into_any_element(),
            );
        } else if scripts.is_empty() {
            list.push(
                div()
                    .px(px(16.))
                    .py(px(8.))
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("No actions configured.")
                    .into_any_element(),
            );
        }
        for script in scripts.iter().filter(|_| !mixed) {
            list.push(self.render_action_row(project_key, script, &t, window, cx));
        }
        if file == T3ProjectFile::Invalid {
            list.push(
                RowFrame::new(div().text_color(t.warning_foreground).child("t3.json is invalid"))
                    .description("A t3.json exists in this checkout but fails to parse, so every action and icon it declares is ignored. Check the JSON syntax and icon values.")
                    .render(&t)
                    .into_any_element(),
            );
        }
        settings_section("project-actions", "Actions", list, &t)
    }

    fn render_action_row(
        &self,
        project_key: &ScopedId,
        script: &ProjectScript,
        t: &Tokens,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let badge = |label: &'static str| {
            div()
                .flex_none()
                .rounded(px(4.))
                .border_1()
                .border_color(t.border.opacity(0.6))
                .px(px(6.))
                .py(px(1.))
                .text_size(px(10.))
                .font_weight(FontWeight::NORMAL)
                .text_color(t.muted_foreground)
                .child(label)
        };
        let title = h_flex()
            .min_w_0()
            .gap(px(8.))
            .child(Icon::new(script_icon(&script.icon)).with_size(px(16.)).flex_none().text_color(t.muted_foreground))
            .child(div().min_w_0().truncate().child(script.name.clone()))
            .children(script.run_on_worktree_create.then(|| badge("setup")))
            .children((script.run_on_settle == Some(true)).then(|| badge("on settle")))
            .children(script.preview_url.is_some().then(|| badge("preview · desktop only")));
        let command = div().max_w_full().truncate().font_family(t.fonts.mono).child(script.command.clone());
        // Shortcuts live on the project's environment, like its list (`representativeConfig`).
        let keybindings = self
            .shell
            .project(project_key)
            .map_or(&serde_json::Value::Null, |project| self.project_info(project).keybindings_json());
        let shortcut = shortcut_label_for_command(keybindings, command_for_project_script(&script.id).as_deref());
        let (pid, sid) = (project_key.clone(), script.id.clone());
        let edit = Button::new(SharedString::from(format!("edit-action-{}", script.id)))
            .ghost()
            .xsmall()
            .icon(Lucide::Settings)
            .tooltip(format!("Edit {}", script.name))
            .on_click(
                cx.listener(move |this, _, window, cx| this.open_project_script_editor(&pid, Some(&sid), window, cx)),
            );
        let control = h_flex()
            .gap(px(8.))
            .children(shortcut.map(|s| div().text_xs().text_color(t.muted_foreground).child(s)))
            .child(div().opacity(0.).group_hover(ROW_GROUP, |s| s.opacity(1.)).child(edit));
        v_flex()
            .id(SharedString::from(format!("project-action-{}", script.id)))
            .group(ROW_GROUP)
            .child(RowFrame::new(title).description(command).control(control).render(t).py(px(8.)))
            .into_any_element()
    }

    /// Adds a `t3.json` script as is; a failed save reopens it in the editor with the error.
    pub(in crate::app) fn import_project_script(
        &mut self,
        project: &ScopedId,
        input: &ScriptInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut input = input.clone();
        // `keybinding: null` only with `settings:write`; otherwise the save leaves shortcuts alone.
        if !self.env_grants(&project.env, SETTINGS_WRITE) {
            input.keybinding = None;
        }
        let task = self.save_project_script(project, None, &input, cx);
        let project = project.clone();
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.open_script_editor_with(&project, None, input, Some(error), window, cx);
                });
            }
        })
        .detach();
    }
}
