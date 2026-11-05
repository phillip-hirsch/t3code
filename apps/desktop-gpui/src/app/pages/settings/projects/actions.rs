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
    ScriptInput, command_for_project_script, has_project_scripts_override, importable_scripts, resolve_project_scripts,
    script_icon, shortcut_label_for_command,
};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::{RowFrame, reset_button, settings_section};
use crate::model::{ProjectScript, T3ProjectFile};
use crate::theme::{self, FONT_MONO, Tokens};

const ROW_GROUP: &str = "project-action-row";

impl Workspace {
    /// The "Actions" section of a project's settings page.
    pub(super) fn render_project_actions(
        &mut self,
        project_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(project) = self.shell.projects.iter().find(|p| p.id == project_id) else {
            return div().into_any_element();
        };
        let scripts = resolve_project_scripts(&self.server_info.settings, project);
        let overridden = has_project_scripts_override(&self.server_info.settings, project_id);
        let root = project.workspace_root.clone();
        let file = self.t3_project_file(&root, cx).clone();
        let importable: Vec<ScriptInput> =
            importable_scripts(&file, &scripts).into_iter().map(ScriptInput::from_file_script).collect();
        let pid = project_id.to_owned();

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
                                        this.import_project_script(&pid, &input, window, cx)
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
        if scripts.is_empty() {
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
        for script in &scripts {
            list.push(self.render_action_row(project_id, script, &t, window, cx));
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
        project_id: &str,
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
        let command = div().max_w_full().truncate().font_family(FONT_MONO).child(script.command.clone());
        let shortcut = shortcut_label_for_command(
            self.server_info.keybindings_json(),
            command_for_project_script(&script.id).as_deref(),
        );
        let (pid, sid) = (project_id.to_owned(), script.id.clone());
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
    fn import_project_script(
        &mut self,
        project_id: &str,
        input: &ScriptInput,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.save_project_script(project_id, None, input, cx);
        let (project_id, input) = (project_id.to_owned(), input.clone());
        cx.spawn_in(window, async move |this, cx| {
            if let Err(error) = task.await {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.open_script_editor_with(&project_id, None, input, Some(error), window, cx)
                });
            }
        })
        .detach();
    }
}
