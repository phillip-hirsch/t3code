//! Settings rows, sections and their controls (`settingsLayout.tsx`, `SettingsGroup.tsx`,
//! `SettingInheritance.tsx`, `SettingsScopeSentence.tsx`), bound to setting keys and the scope.
//!
//! A section builds `let view = self.settings_view()`, then returns `self.settings_page(section, &view,
//! sections, cx)` over `settings_section(id, title, rows, t)` cards (`self.advanced_fold` for folds).
//! Each row is `self.setting_row(&view, SettingRow::new("search-id").description(…).setting("key")
//! .switch(), window, cx)`: the search id gives the title and the search anchor, `.setting(key)`
//! binds the value. Writes go through `self.update_settings(json!({...}), window, cx)`, which
//! routes device keys to `client-settings.json` and server keys to `server.updateSettings`, or to
//! the selected project's overrides (`crate::settings::scope`). Rows on server keys show where
//! their value comes from, "Mixed", and reset to the inherited value under a project scope;
//! other rows reset to the built-in default. Custom controls go in `.control(element)` and read
//! values from `view` (`SettingsView::value`). See `general.rs` for both kinds.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use serde_json::{Map, Value};

use crate::app::{Workspace, project_name};
use crate::settings::scope::{self, InheritanceState, Plan, Scope, ScopeKind, Target};
use crate::settings::{Settings, contract};
use crate::theme::{self, Tokens};

/// What a settings page reads: the selected scope, its targets, and both documents.
pub(in crate::app) struct SettingsView {
    pub(in crate::app) scope: Scope,
    pub(in crate::app) targets: Vec<Target>,
    projects: Vec<scope::Project>,
    environment_id: String,
    environment_label: String,
    /// `None` while disconnected.
    environment_settings: Option<Settings>,
    client: Settings,
}

impl SettingsView {
    pub(in crate::app) fn environment(&self) -> scope::Environment<'_> {
        scope::Environment {
            id: &self.environment_id,
            label: &self.environment_label,
            settings: self.environment_settings.as_ref(),
        }
    }

    /// The value a row shows: this device's for client keys, else the scope's first target's
    /// (`useScopedSettings`), else the built-in default.
    pub(in crate::app) fn value(&self, key: &str) -> &Value {
        if contract().is_client_key(key) {
            return self.client.get(key);
        }
        match self.targets.first() {
            Some(target) => target.settings.get(key),
            None => contract().default_value(key),
        }
    }

    pub(in crate::app) fn bool(&self, key: &str) -> bool {
        self.value(key).as_bool().unwrap_or(false)
    }

    pub(in crate::app) fn str(&self, key: &str) -> &str {
        self.value(key).as_str().unwrap_or_default()
    }

    pub(in crate::app) fn mixed(&self, keys: &[&str]) -> bool {
        scope::are_mixed(&self.targets, keys)
    }

    fn is_default(&self, key: &str) -> bool {
        self.value(key) == contract().default_value(key)
    }

    fn project_name(&self, id: &str) -> Option<&str> {
        self.projects.iter().find(|p| p.id == id).map(|p| p.name.as_str())
    }
}

/// One settings row (`SettingsRow`).
pub(in crate::app) struct SettingRow {
    id: SharedString,
    title: SharedString,
    description: Option<SharedString>,
    keys: Vec<&'static str>,
    control: Control,
    /// A reset the row works out itself, replacing the bound keys' default.
    reset: Option<Option<Value>>,
}

enum Control {
    None,
    Switch,
    Select { options: &'static [(&'static str, &'static str)], width: f32 },
    Slider { min: f32, max: f32, step: f32, unit: &'static str },
    Element(AnyElement),
}

impl SettingRow {
    /// A row titled and anchored by its search item (`searchableSetting(id)`).
    pub(in crate::app) fn new(id: &'static str) -> Self {
        let title = contract().search_item(id).map(|item| item.title.clone()).unwrap_or_else(|| id.to_owned());
        Self::titled(id, title)
    }

    /// A row with no search item.
    pub(in crate::app) fn titled(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            keys: Vec::new(),
            control: Control::None,
            reset: None,
        }
    }

    pub(in crate::app) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Binds the row to a setting; the first bound key is the one built-in controls edit.
    pub(in crate::app) fn setting(mut self, key: &'static str) -> Self {
        self.keys.push(key);
        self
    }

    pub(in crate::app) fn switch(mut self) -> Self {
        self.control = Control::Switch;
        self
    }

    /// A select over `(value, label)` pairs, `width` px wide like the web's `sm:w-*`.
    pub(in crate::app) fn select(mut self, options: &'static [(&'static str, &'static str)], width: f32) -> Self {
        self.control = Control::Select { options, width };
        self
    }

    /// A range slider with the value and `unit` beside it, like the web's `.settings-slider` rows.
    pub(in crate::app) fn slider(mut self, min: f32, max: f32, step: f32, unit: &'static str) -> Self {
        self.control = Control::Slider { min, max, step, unit };
        self
    }

    /// A control the section builds itself.
    pub(in crate::app) fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Control::Element(control.into_any_element());
        self
    }

    /// A reset button that writes `patch`, shown only when there is one; for values whose default
    /// is not a plain per-key default (`SettingResetButton` passed as `resetAction`).
    pub(in crate::app) fn reset(mut self, patch: Option<Value>) -> Self {
        self.reset = Some(patch);
        self
    }
}

/// A titled card of rows (`SettingsSection` with the grouped `SettingsGroup`).
pub(in crate::app) fn settings_section(
    id: &'static str,
    title: &'static str,
    rows: impl IntoIterator<Item = AnyElement>,
    t: &Tokens,
) -> AnyElement {
    v_flex()
        .id(id)
        .gap(px(10.))
        .child(
            h_flex()
                .min_h(px(28.))
                .px(px(16.))
                .child(div().text_size(px(16.)).line_height(px(24.)).font_weight(FontWeight::SEMIBOLD).child(title)),
        )
        .child(card(rows, t))
        .into_any_element()
}

/// `SettingsGroup` grouped: a bordered card whose rows are divided by hairlines.
pub(in crate::app) fn card(rows: impl IntoIterator<Item = AnyElement>, t: &Tokens) -> AnyElement {
    v_flex()
        .rounded(px(12.))
        .border_1()
        .border_color(t.border.opacity(0.6))
        .bg(t.card.opacity(0.4))
        .children(rows.into_iter().enumerate().map(|(index, row)| {
            div().when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.5))).child(row)
        }))
        .into_any_element()
}

/// The 16px heading of a section without a card, such as "Color scheme".
pub(in crate::app) fn subheading(text: impl Into<SharedString>) -> impl IntoElement {
    h_flex()
        .min_h(px(28.))
        .px(px(16.))
        .text_size(px(16.))
        .line_height(px(24.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

/// Sections whose rows apply to this device only show no scope sentence
/// (`SETTINGS_DEVICE_ONLY_PATHS`).
pub(super) fn device_only(section: &str) -> bool {
    matches!(section, "general" | "appearance" | "chat" | "diffs" | "snap-shot" | "connections" | "access" | "about")
}

impl Workspace {
    pub(in crate::app) fn settings_view(&self) -> SettingsView {
        let projects: Vec<scope::Project> = self
            .shell
            .projects
            .iter()
            .filter(|p| p.archived_at.is_none())
            .map(|p| scope::Project {
                id: p.id.clone(),
                name: project_name(p).to_string(),
                legacy_model_selection: p.default_model_selection.as_ref().map(|m| m.to_json()),
                legacy_thread_env_mode: p.default_thread_env_mode.clone(),
            })
            .collect();
        let connected = self.client().is_some() && self.server_info.environment_id.is_some();
        let mut view = SettingsView {
            scope: Scope { kind: ScopeKind::All, label: String::new() },
            targets: Vec::new(),
            projects,
            environment_id: self.server_info.environment_id.clone().unwrap_or_default(),
            environment_label: self.server_info.environment_label.clone().unwrap_or_else(|| "this environment".into()),
            environment_settings: connected.then(|| self.server_info.settings.clone()),
            client: self.client_settings.values.clone(),
        };
        view.scope = scope::resolve(&self.settings_ui.scope, &view.projects, &view.environment());
        view.targets = scope::targets(&view.scope, &view.projects, &view.environment());
        view
    }

    /// Writes a unified patch (`useUpdateScopedSettings`).
    pub(in crate::app) fn update_settings(&mut self, patch: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Value::Object(patch) = patch else { return };
        let view = self.settings_view();
        let plan = scope::plan_patch(&view.scope, &view.projects, &view.environment(), &patch);
        self.run_settings_plan(plan, window, cx);
    }

    /// Resets the selected project's overrides of `keys` (`useClearScopedSettings`).
    fn clear_setting_overrides(&mut self, keys: &[&str], window: &mut Window, cx: &mut Context<Self>) {
        let view = self.settings_view();
        let plan = scope::plan_clear(&view.scope, &view.environment(), keys);
        self.run_settings_plan(plan, window, cx);
    }

    /// Resets other projects' overrides of `keys` (`useClearProjectOverrides`).
    fn clear_project_overrides(
        &mut self,
        project_ids: &[String],
        keys: &[&str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = self.settings_view();
        let ids: Vec<&str> = project_ids.iter().map(String::as_str).collect();
        let plan = scope::plan_clear_projects(&view.environment(), &ids, keys);
        self.run_settings_plan(plan, window, cx);
    }

    /// Applies device keys now and sends the server's part (`persistScopedSettingsPatch`).
    fn run_settings_plan(&mut self, plan: Plan, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(reason) = plan.unavailable {
            window.push_notification(Notification::warning(reason).title("Setting not saved"), cx);
            return;
        }
        if !plan.client_patch.is_empty() {
            self.client_settings.apply(&plan.client_patch);
            cx.notify();
        }
        let (Some(patch), Some(client)) = (plan.server_patch, self.client()) else { return };
        let previous = self.server_info.settings.clone();
        let label = self.server_info.environment_label.clone().unwrap_or_else(|| "this environment".into());
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", serde_json::json!({ "patch": patch })).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let message = match result {
                    Ok(saved) => {
                        let saved_settings = Settings::layered(&contract().server_defaults, &saved);
                        let dropped = scope::dropped_keys(&patch, &previous, &saved_settings);
                        if let Err(error) = this.server_info.apply_settings(&saved) {
                            log::warn!("bad saved settings: {error}");
                        }
                        cx.notify();
                        if dropped.is_empty() {
                            return;
                        }
                        "This environment does not support this setting. Update T3 Code on it.".to_owned()
                    }
                    Err(error) => error.to_string(),
                };
                window.push_notification(
                    Notification::error(format!("Could not save on {label}: {message}")).title("Setting not saved"),
                    cx,
                );
            });
        })
        .detach();
    }

    /// The scrolling page a section renders into (`SettingsPageContainer`), with the scope
    /// sentence above the section's content on scoped sections.
    pub(in crate::app) fn settings_page(
        &mut self,
        section: &str,
        view: &SettingsView,
        content: impl IntoIterator<Item = AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id("settings-page-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.settings_ui.scroll)
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(896.))
                    .mx_auto()
                    .px(px(24.))
                    .pt(px(24.))
                    .pb(px(48.))
                    .gap(px(32.))
                    .when(!device_only(section), |el| el.child(self.render_scope_sentence(view, cx)))
                    .children(content),
            )
            .into_any_element()
    }

    /// One row: title with inheritance marker and reset, description, then the control.
    pub(in crate::app) fn setting_row(
        &mut self,
        view: &SettingsView,
        row: SettingRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let keys = row.keys.clone();
        let server_scoped = keys.iter().any(|k| contract().is_server_key(k));
        let project_scope = view.scope.project_id().is_some();
        let scoped_keys: Vec<&'static str> = keys.iter().copied().filter(|k| contract().is_project_scoped(k)).collect();
        let mixed = !keys.is_empty() && view.mixed(&keys);
        let environment_wide = project_scope && server_scoped && scoped_keys.is_empty();
        let disconnected = server_scoped && view.targets.is_empty();
        let overriding: Vec<String> = match (&view.environment_settings, project_scope) {
            (Some(settings), false) => scope::overriding_projects(settings, &scoped_keys),
            _ => Vec::new(),
        };
        // Values set right here explain nothing new, so their marker waits for hover.
        let quiet_marker = overriding.is_empty();
        let reset = self.render_reset(view, &row, &scoped_keys, mixed, disconnected, cx);
        let SettingRow { id, title, description, control, .. } = row;
        let (state, summary) = scope::row_inheritance(&view.scope, &view.targets, &view.environment(), &keys);
        let inheritance = (server_scoped && !keys.is_empty() && !view.targets.is_empty())
            .then(|| self.render_inheritance(view, &id, &keys, state, summary, overriding, &t, cx));
        let quiet_marker = quiet_marker
            && self.settings_ui.open_marker.as_ref() != Some(&id)
            && matches!(state, InheritanceState::Default | InheritanceState::Environment);
        let inert = if disconnected {
            Some("Reconnect the selected environment to change this setting.")
        } else if environment_wide {
            Some("Environment-wide setting. Select an environment to change it.")
        } else {
            None
        };
        let control = self.render_control(view, &id, keys.first().copied(), control, mixed, window, cx);
        let group: SharedString = format!("settings-row-{id}").into();
        let target = self.settings_ui.target.as_ref() == Some(&id);
        v_flex()
            .id(group.clone())
            .group(group.clone())
            .px(px(16.))
            .relative()
            .py(px(12.))
            .when(target, |el| el.bg(t.accent).child(self.scroll_target_marker()))
            .child(
                h_flex()
                    .w_full()
                    .gap(px(32.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .child(
                                h_flex()
                                    .min_h(px(20.))
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .text_size(px(14.))
                                            .line_height(px(20.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(title),
                                    )
                                    .children(inheritance.map(|marker| {
                                        div()
                                            .size(px(20.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(quiet_marker, |el| {
                                                el.invisible().group_hover(group.clone(), |s| s.visible())
                                            })
                                            .child(marker)
                                    }))
                                    .child(div().size(px(20.)).flex().items_center().justify_center().children(reset)),
                            )
                            .children(description.map(|description| {
                                div()
                                    .max_w(px(672.))
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(t.muted_foreground)
                                    .child(description)
                            })),
                    )
                    .children(control.map(|control| {
                        match inert {
                            Some(message) => div()
                                .id(SharedString::from(format!("{id}-inert")))
                                .flex_none()
                                .opacity(0.5)
                                .occlude()
                                .tooltip(move |window, cx| Tooltip::new(message).build(window, cx))
                                .child(control)
                                .into_any_element(),
                            None => h_flex().flex_none().justify_end().gap(px(8.)).child(control).into_any_element(),
                        }
                    })),
            )
            .into_any_element()
    }

    fn render_reset(
        &self,
        view: &SettingsView,
        row: &SettingRow,
        scoped_keys: &[&'static str],
        mixed: bool,
        disconnected: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if let Some(custom) = &row.reset {
            let patch = custom.clone().filter(|_| !disconnected)?;
            return Some(
                reset_button(&row.id, "Reset to default", cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.update_settings(patch.clone(), window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if row.keys.is_empty() || disconnected {
            return None;
        }
        if view.scope.project_id().is_some() && !scoped_keys.is_empty() {
            let source = scope::row_source(&view.targets, scoped_keys);
            if !matches!(source, scope::RowSource::Project | scope::RowSource::Mixed) {
                return None;
            }
            let keys = scoped_keys.to_vec();
            return Some(
                reset_button(&row.id, "Reset to inherited value", cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.clear_setting_overrides(&keys, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if !mixed && row.keys.iter().all(|k| view.is_default(k)) {
            return None;
        }
        let patch: Map<String, Value> =
            row.keys.iter().map(|k| ((*k).to_owned(), contract().default_value(k).clone())).collect();
        Some(
            reset_button(&row.id, "Reset to default", cx)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.update_settings(Value::Object(patch.clone()), window, cx);
                }))
                .into_any_element(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_inheritance(
        &self,
        view: &SettingsView,
        id: &SharedString,
        keys: &[&'static str],
        state: InheritanceState,
        summary: String,
        overriding: Vec<String>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = keys[0];
        let summary = match overriding.len() {
            0 => summary,
            1 => format!("{summary} · 1 project override"),
            n => format!("{summary} · {n} project overrides"),
        };
        let color = match state {
            InheritanceState::Overridden => t.primary,
            InheritanceState::Mixed => t.warning,
            _ => t.muted_foreground,
        };
        let environment_settings = view.environment_settings.clone().unwrap_or_default();
        let chains: Rc<Vec<(String, Vec<scope::Layer>)>> = Rc::new(
            view.targets
                .iter()
                .map(|target| {
                    let label = match &target.project_id {
                        Some(project) => {
                            format!("{} · {}", view.environment_label, view.project_name(project).unwrap_or(project))
                        }
                        None => view.environment_label.clone(),
                    };
                    (label, scope::inheritance_layers(target, &environment_settings, key))
                })
                .collect(),
        );
        let overriding: Rc<Vec<(String, String, String)>> = Rc::new(
            overriding
                .iter()
                .map(|project| {
                    let value =
                        environment_settings.get("projectSettingsOverrides").get(project).and_then(|o| o.get(key));
                    (
                        project.clone(),
                        view.project_name(project).unwrap_or(project).to_owned(),
                        value.map(|v| scope::format_value(key, v)).unwrap_or_default(),
                    )
                })
                .collect(),
        );
        let scoped_keys: Rc<Vec<&'static str>> =
            Rc::new(keys.iter().copied().filter(|k| contract().is_project_scoped(k)).collect());
        let workspace = cx.entity();
        let tooltip: SharedString = summary.into();
        let t = *t;
        let marker = id.clone();
        let on_open_change = {
            let workspace = workspace.clone();
            move |open: &bool, _: &mut Window, cx: &mut gpui_kit::App| {
                let marker = open.then(|| marker.clone());
                workspace.update(cx, |this, cx| {
                    this.settings_ui.open_marker = marker;
                    cx.notify();
                });
            }
        };
        Popover::new(SharedString::from(format!("{id}-inheritance")))
            .anchor(gpui_kit::Anchor::TopLeft)
            .p_0()
            .on_open_change(on_open_change)
            .trigger(
                Button::new(SharedString::from(format!("{id}-inheritance-trigger")))
                    .ghost()
                    .size(px(20.))
                    .p_0()
                    .rounded(px(4.))
                    .child(Icon::new(IconName::Layers).size(px(12.)).text_color(color))
                    .tooltip(tooltip),
            )
            .content(move |_, _, _| {
                let open_project = |id: String| {
                    let workspace = workspace.clone();
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
                        workspace.update(cx, |this, cx| {
                            this.settings_ui.scope = this.settings_ui.scope.with_project(Some(id.clone()));
                            cx.notify();
                        });
                        window.refresh();
                    }
                };
                let clear = {
                    let workspace = workspace.clone();
                    let ids: Vec<String> = overriding.iter().map(|(id, ..)| id.clone()).collect();
                    let keys = scoped_keys.clone();
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
                        workspace.update(cx, |this, cx| this.clear_project_overrides(&ids, &keys, window, cx));
                    }
                };
                v_flex()
                    .w(px(320.))
                    .children(chains.iter().map(|(label, layers)| {
                        v_flex()
                            .px(px(12.))
                            .py(px(10.))
                            .child(
                                div()
                                    .pb(px(6.))
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.muted_foreground)
                                    .child(label.clone()),
                            )
                            .children(layers.iter().map(|layer| layer_row(layer, &t)))
                    }))
                    .when(!overriding.is_empty(), |el| {
                        el.child(
                            v_flex()
                                .mx(px(12.))
                                .mb(px(10.))
                                .pt(px(8.))
                                .border_t_1()
                                .border_color(t.border.opacity(0.6))
                                .child(
                                    h_flex()
                                        .px(px(8.))
                                        .justify_between()
                                        .text_size(px(12.))
                                        .text_color(t.muted_foreground)
                                        .child("Overridden by")
                                        .child(
                                            div()
                                                .id("inheritance-reset-overrides")
                                                .cursor_pointer()
                                                .text_color(t.foreground)
                                                .hover(|s| s.underline())
                                                .child(if overriding.len() == 1 { "Reset it" } else { "Reset all" })
                                                .on_click(clear.clone()),
                                        ),
                                )
                                .children(overriding.iter().map(|(id, name, value)| {
                                    h_flex()
                                        .px(px(8.))
                                        .py(px(4.))
                                        .gap(px(12.))
                                        .text_size(px(14.))
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("override-{id}")))
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .cursor_pointer()
                                                .hover(|s| s.underline())
                                                .child(name.clone())
                                                .on_click(open_project(id.clone())),
                                        )
                                        .child(
                                            div()
                                                .max_w(px(128.))
                                                .truncate()
                                                .text_color(t.muted_foreground)
                                                .child(value.clone()),
                                        )
                                })),
                        )
                    })
            })
            .into_any_element()
    }

    fn render_control(
        &mut self,
        view: &SettingsView,
        id: &SharedString,
        key: Option<&'static str>,
        control: Control,
        mixed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = *theme::tokens(cx);
        let element = match (control, key) {
            (Control::Element(element), _) => element,
            (Control::Switch, Some(key)) => {
                let checked = view.bool(key);
                switch(id, checked, mixed, &t)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.update_settings(serde_json::json!({ key: !checked || mixed }), window, cx);
                    }))
                    .into_any_element()
            }
            (Control::Select { options, width }, Some(key)) => {
                let current = view.str(key).to_owned();
                let label = if mixed {
                    "Mixed".to_owned()
                } else {
                    options.iter().find(|(v, _)| *v == current).map_or(current.clone(), |(_, l)| (*l).to_owned())
                };
                let workspace = cx.entity();
                select_trigger(id, label, width, &t)
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                        for (value, label) in options {
                            let picked = (*value).to_owned();
                            menu = menu.item(PopupMenuItem::new(*label).checked(!mixed && current == *value).on_click(
                                window.listener_for(&workspace, move |this, _, window, cx| {
                                    this.update_settings(serde_json::json!({ key: picked }), window, cx);
                                }),
                            ));
                        }
                        menu
                    })
                    .into_any_element()
            }
            (Control::Slider { min, max, step, unit }, Some(key)) => {
                let value = view.value(key).as_f64().unwrap_or_default() as f32;
                let state = self.settings_slider(key, min, max, step, value, window, cx);
                h_flex()
                    .w(px(208.))
                    .gap(px(12.))
                    .child(
                        div()
                            .min_w(px(48.))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .bg(t.muted)
                            .text_center()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("{value}{unit}")),
                    )
                    .child(div().flex_1().min_w_0().child(Slider::new(&state).horizontal()))
                    .into_any_element()
            }
            _ => return None,
        };
        Some(element)
    }

    /// The slider entity for `key`, kept in sync with the setting while it is not being dragged.
    #[allow(clippy::too_many_arguments)]
    fn settings_slider(
        &mut self,
        key: &'static str,
        min: f32,
        max: f32,
        step: f32,
        value: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Entity<SliderState> {
        if let Some((state, _)) = self.settings_ui.sliders.get(key) {
            let state = state.clone();
            if (state.read(cx).value().start() - value).abs() > f32::EPSILON {
                state.update(cx, |state, cx| state.set_value(value, window, cx));
            }
            return state;
        }
        let state = cx.new(|_| SliderState::new().min(min).max(max).step(step).default_value(value));
        let subscription = cx.subscribe_in(&state, window, move |this, _, event: &SliderEvent, window, cx| {
            let SliderEvent::Change(value) = event else { return };
            let value = value.start().round();
            if this.settings_view().value(key).as_f64() != Some(value as f64) {
                this.update_settings(serde_json::json!({ key: value as i64 }), window, cx);
            }
        });
        self.settings_ui.sliders.insert(key, (state.clone(), subscription));
        state
    }

    /// Marks the row a search jumped to; scrolls it into view on its first paint.
    fn scroll_target_marker(&self) -> impl IntoElement {
        let pending = self.settings_ui.scroll_pending.clone();
        let scroll = self.settings_ui.scroll.clone();
        gpui_kit::canvas(
            move |bounds, _, _| {
                if !pending.replace(false) {
                    return;
                }
                let viewport = scroll.bounds();
                let offset = scroll.offset();
                let centered = viewport.center().y - bounds.center().y;
                scroll.set_offset(gpui_kit::point(offset.x, (offset.y + centered).min(px(0.))));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full()
    }

    /// "Applying settings for [project] across [environment]" (`SettingsScopeSentence`).
    fn render_scope_sentence(&self, view: &SettingsView, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let search = self.settings_ui.scope.clone();
        let workspace = cx.entity();
        let project_label = match (&search.project, &view.scope.kind) {
            (Some(id), _) => view.project_name(id).map_or_else(|| "Unavailable project".to_owned(), str::to_owned),
            (None, _) => "All projects".to_owned(),
        };
        let environment_label = match &search.machine {
            Some(id) if *id == view.environment_id => view.environment_label.clone(),
            Some(_) => "Unavailable environment".to_owned(),
            None => "All environments".to_owned(),
        };
        // Sorted by name; a name shared by several projects shows where each lives.
        let mut projects: Vec<(String, String, Option<String>)> = view
            .projects
            .iter()
            .map(|project| {
                let shared = view.projects.iter().filter(|other| other.name == project.name).count() > 1;
                let root = shared
                    .then(|| self.shell.projects.iter().find(|p| p.id == project.id))
                    .flatten()
                    .map(|p| format!("{} · {}", view.environment_label, p.workspace_root));
                (project.id.clone(), project.name.clone(), root)
            })
            .collect();
        projects.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
        let projects = Rc::new(projects);
        let environment = (view.environment_id.clone(), view.environment_label.clone());
        let pick = |workspace: gpui_kit::Entity<Workspace>, next: scope::ScopeSearch| {
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                workspace.update(cx, |this, cx| {
                    this.settings_ui.scope = next.clone();
                    this.settings_ui.target = None;
                    cx.notify();
                });
            }
        };
        let project_menu = {
            let (search, workspace, projects) = (search.clone(), workspace.clone(), projects.clone());
            scope_trigger("settings-scope-project", project_label, &t).dropdown_menu_with_anchor(
                gpui_kit::Anchor::TopLeft,
                move |mut menu, _, _| {
                    menu = menu
                        .item(
                            PopupMenuItem::new("All projects")
                                .checked(search.project.is_none())
                                .on_click(pick(workspace.clone(), search.with_project(None))),
                        )
                        .separator();
                    for (id, name, location) in projects.iter() {
                        let item = match location.clone() {
                            None => PopupMenuItem::new(name.clone()),
                            Some(location) => {
                                let name = name.clone();
                                PopupMenuItem::element(move |_, cx| {
                                    let t = theme::tokens(cx);
                                    v_flex().min_w_0().child(div().truncate().child(name.clone())).child(
                                        div()
                                            .truncate()
                                            .text_size(px(12.))
                                            .text_color(t.muted_foreground)
                                            .child(location.clone()),
                                    )
                                })
                            }
                        };
                        menu = menu.item(
                            item.checked(search.project.as_deref() == Some(id.as_str()))
                                .on_click(pick(workspace.clone(), search.with_project(Some(id.clone())))),
                        );
                    }
                    menu.max_w(px(320.)).max_h(px(360.)).scrollable(true)
                },
            )
        };
        let environment_menu = {
            let (search, workspace) = (search.clone(), workspace.clone());
            scope_trigger("settings-scope-environment", environment_label, &t).dropdown_menu_with_anchor(
                gpui_kit::Anchor::TopLeft,
                move |menu, _, _| {
                    menu.item(
                        PopupMenuItem::new("All environments")
                            .checked(search.machine.is_none())
                            .on_click(pick(workspace.clone(), search.with_machine(None))),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new(environment.1.clone())
                            .checked(search.machine.as_deref() == Some(environment.0.as_str()))
                            .on_click(pick(workspace.clone(), search.with_machine(Some(environment.0.clone())))),
                    )
                },
            )
        };
        let joiner = if search.machine.is_some() { "on" } else { "across" };
        h_flex()
            .flex_wrap()
            .gap_x(px(6.))
            .gap_y(px(4.))
            .px(px(16.))
            .text_size(px(14.))
            .text_color(t.muted_foreground)
            .child(h_flex().gap(px(6.)).child("Applying settings for").child(project_menu))
            .child(h_flex().gap(px(6.)).child(joiner).child(environment_menu))
            .into_any_element()
    }
}

fn layer_row(layer: &scope::Layer, t: &Tokens) -> impl IntoElement {
    h_flex()
        .px(px(8.))
        .py(px(4.))
        .gap(px(12.))
        .rounded(px(6.))
        .text_size(px(14.))
        .when(layer.effective, |el| el.bg(t.foreground.opacity(0.06)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(layer.effective, |el| el.font_weight(FontWeight::MEDIUM).text_color(t.foreground))
                .when(!layer.effective, |el| el.text_color(t.muted_foreground))
                .child(layer.label.clone()),
        )
        .child(
            h_flex()
                .gap(px(6.))
                .text_color(if layer.effective {
                    t.foreground
                } else if layer.set {
                    t.muted_foreground
                } else {
                    t.muted_foreground.opacity(0.6)
                })
                .child(div().max_w(px(128.)).truncate().child(layer.value.clone()))
                .child(div().size(px(14.)).when(layer.effective, |el| {
                    el.child(Icon::new(IconName::Check).size(px(14.)).text_color(t.primary))
                })),
        )
}

/// The web's `Switch` at desktop size: a 30×18 track and a 14px thumb.
pub(in crate::app) fn switch(
    id: &SharedString,
    checked: bool,
    mixed: bool,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let on = checked && !mixed;
    let thumb: Hsla = if on || !t.dark { t.background } else { t.muted_foreground };
    div()
        .id(SharedString::from(format!("{id}-switch")))
        .flex_none()
        .w(px(30.))
        .h(px(18.))
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .bg(if on { t.primary } else { t.input })
        .flex()
        .items_center()
        .map(|el| {
            if mixed {
                el.justify_center()
            } else if on {
                el.justify_end()
            } else {
                el.justify_start()
            }
        })
        .child(div().size(px(14.)).rounded_full().bg(thumb).when(mixed, |el| el.opacity(0.7)))
}

/// The web's `SelectTrigger size="sm"`.
pub(in crate::app) fn select_trigger(
    id: &SharedString,
    label: impl Into<SharedString>,
    width: f32,
    t: &Tokens,
) -> Button {
    Button::new(SharedString::from(format!("{id}-select")))
        .ghost()
        .w(px(width))
        .h(px(28.))
        .px(px(9.))
        .rounded(px(8.))
        .border_1()
        .border_color(t.input)
        .bg(if t.dark { t.input.opacity(0.32) } else { t.background })
        .text_size(px(14.))
        .font_weight(FontWeight::NORMAL)
        .text_color(t.foreground)
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap(px(8.))
                .child(div().min_w_0().truncate().child(label.into()))
                .child(Icon::new(IconName::ChevronDown).size(px(14.)).text_color(t.muted_foreground.opacity(0.7))),
        )
}

/// The outline `sm` button the scope sentence's menus open from.
fn scope_trigger(id: &'static str, label: String, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .max_w(px(288.))
        .h(px(28.))
        .px(px(9.))
        .rounded(px(8.))
        .border_1()
        .border_color(t.input)
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.foreground)
        .child(
            h_flex()
                .gap(px(6.))
                .child(div().min_w_0().truncate().child(label))
                .child(Icon::new(IconName::ChevronDown).size(px(14.)).text_color(t.muted_foreground.opacity(0.7))),
        )
}

/// `SettingResetButton`: a 20px ghost button with the undo arrow.
fn reset_button(id: &SharedString, tooltip: &'static str, cx: &Context<Workspace>) -> Button {
    let t = theme::tokens(cx);
    Button::new(SharedString::from(format!("{id}-reset")))
        .ghost()
        .size(px(20.))
        .p_0()
        .rounded(px(4.))
        .child(Icon::new(IconName::Undo2).size(px(12.)).text_color(t.muted_foreground))
        .tooltip(tooltip)
}
