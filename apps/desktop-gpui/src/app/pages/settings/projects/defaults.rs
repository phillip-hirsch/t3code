//! New-thread defaults (`ProjectDefaultsSettings.tsx`), described as a row table.
//!
//! Every row here is scoped in the web (`SettingsRow serverScoped`): at project scope it reads and
//! writes `projectSettingsOverrides[projectId]`, elsewhere the environment's settings. Inheritance
//! badges, "Mixed" across environments, and the scope sentence come from the shared settings
//! widgets at merge; this file only renders the rows plainly. "Fallback model" is environment-only.

use std::rc::Rc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::h_flex;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::{
    AnyElement, Context, ElementId, IntoElement, ParentElement as _, SharedString, Styled as _, Window, px,
};
use serde_json::{Value, json};

use super::scripts::rpc_message;
use crate::app::Workspace;
use crate::app::pages::settings::model_select::muted;
use crate::app::pages::settings::widgets::{RowFrame, reset_button, settings_section};
use crate::model::ModelSelection;
use crate::theme;

enum Control {
    /// A provider/model picker (`ProviderModelPicker`).
    Model,
    Switch,
    /// `(stored value, label)`; `None` is a stored `null`.
    Select(&'static [(Option<&'static str>, &'static str)]),
}

struct SettingRowSpec {
    id: &'static str,
    label: &'static str,
    /// Shown at project scope.
    description: &'static str,
    global_description: &'static str,
    /// The `ServerSettings` key.
    path: &'static str,
    control: Control,
    scopes: &'static [&'static str],
    /// `ProjectSettingsCategory` values that list the row, in table order.
    categories: &'static [&'static str],
    /// What a stored `null` displays as (`PROJECT_FILE_BACKED_SETTINGS` built-ins, schema defaults).
    unset: Option<&'static str>,
    /// The reset target as JSON, and the scopes that offer it.
    reset: Option<(&'static str, &'static [&'static str])>,
}

const ALL_SCOPES: &[&str] = &["global", "environment", "project", "checkout"];
const ENVIRONMENT_SCOPES: &[&str] = &["global", "environment"];

const ROWS: &[SettingRowSpec] = &[
    SettingRowSpec {
        id: "default-model",
        label: "Model",
        description: "Model for new threads in this project.",
        global_description: "Default model for new threads. Projects can override it.",
        path: "defaultModelSelection",
        control: Control::Model,
        scopes: ALL_SCOPES,
        categories: &["project", "general"],
        unset: None,
        reset: Some(("null", ALL_SCOPES)),
    },
    SettingRowSpec {
        id: "fallback-model",
        label: "Fallback model",
        description: "",
        global_description: "Used for new threads when the default model's provider is out of usage.",
        path: "fallbackModelSelection",
        control: Control::Model,
        scopes: ENVIRONMENT_SCOPES,
        categories: &["project", "general"],
        unset: None,
        reset: Some(("null", ENVIRONMENT_SCOPES)),
    },
    SettingRowSpec {
        id: "default-permissions",
        label: "Permissions",
        description: "Permissions for new threads in this project.",
        global_description: "Default permissions for new threads. Projects can override them.",
        path: "defaultRuntimeMode",
        control: Control::Select(&[
            (Some("approval-required"), "Supervised"),
            (Some("auto-accept-edits"), "Auto-accept edits"),
            (Some("auto"), "Auto"),
            (Some("full-access"), "Full access"),
        ]),
        scopes: ALL_SCOPES,
        categories: &["general"],
        unset: Some("full-access"),
        reset: Some(("\"full-access\"", ALL_SCOPES)),
    },
    SettingRowSpec {
        id: "new-threads",
        label: "Workspace",
        description: "Where new threads in this project start.",
        global_description: "Where new threads start. Projects and their t3.json can override it.",
        path: "defaultThreadEnvMode",
        control: Control::Select(&[(Some("local"), "Current checkout"), (Some("worktree"), "New worktree")]),
        scopes: ALL_SCOPES,
        categories: &["project", "general"],
        unset: Some("local"),
        reset: Some(("null", ENVIRONMENT_SCOPES)),
    },
    SettingRowSpec {
        id: "automatic-pull",
        label: "Automatically pull",
        description: "Keeps this project's default branch current when the checkout has no local changes or commits.",
        global_description: "Keeps the default branch current when the checkout has no local changes or commits. Projects can override it.",
        path: "defaultAutoPull",
        control: Control::Switch,
        scopes: ALL_SCOPES,
        categories: &["source-control"],
        unset: None,
        reset: Some(("false", ALL_SCOPES)),
    },
    SettingRowSpec {
        id: "remove-agent-credits-on-merge",
        label: "Remove agent credits when merging",
        description: "Remove recognized agent credit lines from merge and squash messages on hosts that support it, keeping human co-authors. Includes auto-merge. Excludes merge queues, stack merges, and existing commits.",
        global_description: "Remove recognized agent credit lines from merge and squash messages on hosts that support it, keeping human co-authors. Includes auto-merge. Excludes merge queues, stack merges, and existing commits.",
        path: "removeAgentCreditsOnMerge",
        control: Control::Switch,
        scopes: ALL_SCOPES,
        categories: &["source-control"],
        unset: None,
        reset: Some(("false", ALL_SCOPES)),
    },
    SettingRowSpec {
        id: "pull-request-merge-method",
        label: "Default merge method",
        description: "Pull requests in this project start with this method.",
        global_description: "Pull requests start with this method. Last selected reuses whatever you chose most recently on this device.",
        path: "pullRequestMergeMethod",
        control: Control::Select(&[
            (None, "Last selected"),
            (Some("merge"), "Merge"),
            (Some("squash"), "Squash and merge"),
            (Some("rebase"), "Rebase and merge"),
        ]),
        scopes: ALL_SCOPES,
        categories: &["source-control"],
        unset: None,
        reset: Some(("null", ALL_SCOPES)),
    },
];

/// Override keys that can store `null`; for the rest an inherit (`null`) drops the key.
const NULLABLE_OVERRIDES: &[&str] = &["defaultModelSelection", "pullRequestMergeMethod"];

/// The value a row shows: the project's override when it has one, else the environment's.
fn effective<'a>(settings: &'a Value, project_id: Option<&str>, key: &str) -> &'a Value {
    project_id
        .and_then(|id| settings.pointer(&format!("/projectSettingsOverrides/{id}/{key}")))
        .or_else(|| settings.get(key))
        .unwrap_or(&Value::Null)
}

/// `planScopedSettingsPatch` for one key: a project rewrites its whole override entry.
fn setting_patch(settings: &Value, project_id: Option<&str>, key: &str, value: Value) -> Value {
    let Some(project_id) = project_id else { return json!({ key: value }) };
    let mut entry = settings
        .pointer(&format!("/projectSettingsOverrides/{project_id}"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if value.is_null() && !NULLABLE_OVERRIDES.contains(&key) {
        entry.remove(key);
    } else {
        entry.insert(key.to_owned(), value);
    }
    json!({"projectSettingsOverrides": {project_id: entry}})
}

impl Workspace {
    /// The "New threads" section of a project's settings page.
    pub(super) fn render_project_defaults(&mut self, project_id: &str, cx: &mut Context<Self>) -> AnyElement {
        self.render_defaults_section("project", Some(project_id), cx)
    }

    /// `ProjectDefaultsSettings` for one category, at project scope or (with `None`) the environment's.
    fn render_defaults_section(
        &mut self,
        category: &str,
        project_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let scope = if project_id.is_some() { "project" } else { "global" };
        let rows: Vec<AnyElement> = ROWS
            .iter()
            .filter(|spec| spec.categories.contains(&category) && spec.scopes.contains(&scope))
            .map(|spec| {
                let value = effective(&self.server_info.settings, project_id, spec.path).clone();
                let control = self.render_row_control(spec, &value, project_id, cx);
                let reset = spec
                    .reset
                    .filter(|(_, scopes)| scopes.contains(&scope))
                    .and_then(|(target, _)| serde_json::from_str::<Value>(target).ok())
                    .filter(|target| &value != target)
                    .map(|target| {
                        let project_id = project_id.map(str::to_owned);
                        reset_button(&SharedString::from(spec.id), "Reset to default", cx).on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.write_setting(project_id.as_deref(), spec.path, target.clone(), window, cx)
                            },
                        ))
                    });
                let description = if scope == "project" { spec.description } else { spec.global_description };
                RowFrame::new(spec.label)
                    .description(description)
                    .reset(reset)
                    .control(control)
                    .render(&t)
                    .into_any_element()
            })
            .collect();
        let title = if category == "source-control" { "Repositories" } else { "New threads" };
        settings_section(title, title, rows, &t)
    }

    fn render_row_control(
        &self,
        spec: &'static SettingRowSpec,
        value: &Value,
        project_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = SharedString::from(format!("setting-{}", spec.id));
        let project = project_id.map(str::to_owned);
        match spec.control {
            Control::Switch => Switch::new(id)
                .checked(value.as_bool() == Some(true))
                .on_change(cx.listener(move |this, checked: &bool, window, cx| {
                    this.write_setting(project.as_deref(), spec.path, Value::Bool(*checked), window, cx)
                }))
                .into_any_element(),
            Control::Select(options) => {
                let current: Option<String> = value.as_str().or(spec.unset).map(str::to_owned);
                let label = options.iter().find(|(v, _)| *v == current.as_deref()).map_or("Mixed", |(_, label)| label);
                let view = cx.entity();
                select_trigger(id, label)
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                        for (option, label) in options {
                            let project = project.clone();
                            menu =
                                menu.item(PopupMenuItem::new(*label).checked(*option == current.as_deref()).on_click(
                                    window.listener_for(&view, move |this, _, window, cx| {
                                        let value = option.map_or(Value::Null, Value::from);
                                        this.write_setting(project.as_deref(), spec.path, value, window, cx)
                                    }),
                                ));
                        }
                        menu
                    })
                    .into_any_element()
            }
            Control::Model => {
                if self.server_info.model_choices().is_empty() {
                    return muted("No providers available", &theme::tokens(cx)).into_any_element();
                }
                let stored: Option<ModelSelection> = serde_json::from_value(value.clone()).ok();
                let fallback = spec.path == "fallbackModelSelection";
                // An unset default is picked automatically; an unset fallback shows the default as "None".
                let selection = stored.clone().or_else(|| {
                    let default = effective(&self.server_info.settings, project_id, "defaultModelSelection");
                    serde_json::from_value(default.clone())
                        .ok()
                        .filter(|_| fallback)
                        .or_else(|| self.server_info.new_thread_model(None))
                });
                let label = (fallback && stored.is_none()).then_some("None");
                let path = spec.path;
                let write = Rc::new(
                    move |this: &mut Workspace, value: Value, window: &mut Window, cx: &mut Context<Workspace>| {
                        this.write_setting(project.as_deref(), path, value, window, cx)
                    },
                );
                let pick = write.clone();
                let traits = if fallback { stored.as_ref() } else { selection.as_ref() };
                h_flex()
                    .gap(px(6.))
                    .child(self.model_picker(
                        id,
                        selection.as_ref(),
                        label,
                        |_| true,
                        cx,
                        move |this, picked, window, cx| {
                            pick(this, json!({"instanceId": picked.instance_id, "model": picked.model}), window, cx)
                        },
                    ))
                    .children(traits.and_then(|traits| {
                        let traits_id = SharedString::from(format!("setting-{}-traits", spec.id));
                        self.traits_picker(traits_id, traits, cx, move |this, picked, window, cx| {
                            write(this, picked.to_json(), window, cx)
                        })
                    }))
                    .into_any_element()
            }
        }
    }

    /// Writes one setting at the row's scope; failures show as a toast.
    fn write_setting(
        &mut self,
        project_id: Option<&str>,
        key: &str,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let patch = setting_patch(&self.server_info.settings, project_id, key, value);
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", json!({ "patch": patch })).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(settings) => {
                    let _ =
                        this.server_info.apply(&json!({"type": "settingsUpdated", "payload": {"settings": settings}}));
                    cx.notify();
                }
                Err(error) => {
                    use gpui_kit::component::WindowExt as _;
                    use gpui_kit::component::notification::Notification;
                    let note = Notification::error(rpc_message(&error)).title("Setting not saved");
                    window.push_notification(note, cx);
                }
            });
        })
        .detach();
    }
}

/// `SelectTrigger size="sm"` with the settings picker look.
fn select_trigger(id: impl Into<ElementId>, label: &'static str) -> Button {
    let button = Button::new(id).outline().small().dropdown_caret(true);
    if label.is_empty() { button } else { button.label(label) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_rows_read_overrides_before_the_environment() {
        let settings = json!({"defaultThreadEnvMode": "worktree", "projectSettingsOverrides": {"p": {"defaultThreadEnvMode": "local"}}});
        assert_eq!(effective(&settings, Some("p"), "defaultThreadEnvMode"), "local");
        assert_eq!(effective(&settings, Some("q"), "defaultThreadEnvMode"), "worktree");
        assert_eq!(effective(&settings, None, "defaultModelSelection"), &Value::Null);
    }

    #[test]
    fn project_writes_rewrite_the_whole_entry_and_inherit_by_dropping_the_key() {
        let settings = json!({"projectSettingsOverrides": {"p": {"defaultProjectScripts": [], "defaultThreadEnvMode": "worktree"}}});
        assert_eq!(
            setting_patch(&settings, Some("p"), "defaultThreadEnvMode", Value::Null),
            json!({"projectSettingsOverrides": {"p": {"defaultProjectScripts": []}}})
        );
        assert_eq!(
            setting_patch(&settings, Some("p"), "defaultModelSelection", Value::Null),
            json!({"projectSettingsOverrides": {"p": {"defaultProjectScripts": [], "defaultThreadEnvMode": "worktree", "defaultModelSelection": null}}})
        );
        assert_eq!(
            setting_patch(&settings, None, "defaultAutoPull", Value::Bool(true)),
            json!({"defaultAutoPull": true})
        );
    }

    #[test]
    fn every_category_lists_its_web_rows_in_order() {
        let ids = |category: &str, scope: &str| -> Vec<&str> {
            ROWS.iter()
                .filter(|r| r.categories.contains(&category) && r.scopes.contains(&scope))
                .map(|r| r.id)
                .collect()
        };
        assert_eq!(ids("project", "project"), ["default-model", "new-threads"]);
        assert_eq!(ids("general", "global"), ["default-model", "fallback-model", "default-permissions", "new-threads"]);
        assert_eq!(
            ids("source-control", "project"),
            ["automatic-pull", "remove-agent-credits-on-merge", "pull-request-merge-method"]
        );
    }
}
