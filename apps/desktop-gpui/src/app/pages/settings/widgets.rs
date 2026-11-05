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
//! values from `view` (`SettingsView::value`). See `general.rs` for both kinds. Rows that are
//! not one setting (a keybinding, a host, a task) draw the same chrome with [`RowFrame`].

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
    AnyElement, AppContext as _, Context, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use serde_json::{Map, Value};

use super::code::projects::icon::project_icon;
use crate::app::{Workspace, project_name};
use crate::project_groups::ProjectGroup;
use crate::settings::scope::{self, InheritanceState, Plan, Scope, ScopeKind, Target};
use crate::settings::{Settings, contract};
use crate::theme::{self, Tokens};

/// What a settings page reads: the selected scope, its targets, and both documents.
pub(in crate::app) struct SettingsView {
    pub(in crate::app) scope: Scope,
    pub(in crate::app) targets: Vec<Target>,
    /// Settings' project groups, archived ones included (`useSettingsProjectGroups`).
    pub(in crate::app) groups: Vec<ProjectGroup>,
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

    /// The group a checkout belongs to.
    fn group_of(&self, project_id: &str) -> Option<&ProjectGroup> {
        self.groups.iter().find(|g| g.members.iter().any(|m| m.project.id == project_id))
    }
}

/// One settings row (`SettingsRow`).
pub(in crate::app) struct SettingRow {
    id: SharedString,
    title: SharedString,
    description: Option<AnyElement>,
    status: Option<SharedString>,
    body: Option<AnyElement>,
    keys: Vec<&'static str>,
    control: Control,
    /// A reset the row works out itself, replacing the bound keys' default.
    reset: Option<Option<Value>>,
    unavailable: bool,
}

enum Control {
    None,
    Switch,
    Select {
        options: &'static [(&'static str, &'static str)],
        width: f32,
    },
    Slider {
        min: f32,
        max: f32,
        step: f32,
        unit: &'static str,
    },
    /// A field saved trimmed on blur or Enter (`DraftInput`), `width` px wide.
    Text {
        placeholder: &'static str,
        width: f32,
    },
    /// A four-row field under the row's text, saved trimmed on blur.
    Textarea {
        placeholder: &'static str,
    },
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
            status: None,
            body: None,
            keys: Vec::new(),
            control: Control::None,
            unavailable: false,
            reset: None,
        }
    }

    pub(in crate::app) fn description(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }

    /// A muted line under the description, such as a save in flight.
    pub(in crate::app) fn status(mut self, status: Option<impl Into<SharedString>>) -> Self {
        self.status = status.map(Into::into);
        self
    }

    /// Content under the row's text and control, such as a list.
    pub(in crate::app) fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }

    /// `SettingsUnavailableGroup`: dimmed text and an inert control.
    pub(in crate::app) fn unavailable(mut self) -> Self {
        self.unavailable = true;
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

    /// A select over `(value, label)` pairs, `width` px wide like the web's `sm:w-*`; 0 sizes it to its label.
    pub(in crate::app) fn select(mut self, options: &'static [(&'static str, &'static str)], width: f32) -> Self {
        self.control = Control::Select { options, width };
        self
    }

    /// A range slider with the value and `unit` beside it, like the web's `.settings-slider` rows.
    pub(in crate::app) fn slider(mut self, min: f32, max: f32, step: f32, unit: &'static str) -> Self {
        self.control = Control::Slider { min, max, step, unit };
        self
    }

    pub(in crate::app) fn text(mut self, placeholder: &'static str, width: f32) -> Self {
        self.control = Control::Text { placeholder, width };
        self
    }

    pub(in crate::app) fn textarea(mut self, placeholder: &'static str) -> Self {
        self.control = Control::Textarea { placeholder };
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
    id: impl Into<ElementId>,
    title: impl IntoElement,
    rows: impl IntoIterator<Item = AnyElement>,
    t: &Tokens,
) -> AnyElement {
    section_with_action(id, title, None, rows, t)
}

/// A section whose header carries an action on the right, such as "Add source".
pub(in crate::app) fn section_with_action(
    id: impl Into<ElementId>,
    title: impl IntoElement,
    action: Option<AnyElement>,
    rows: impl IntoIterator<Item = AnyElement>,
    t: &Tokens,
) -> AnyElement {
    section_frame(id, title, action, card(rows, t))
}

/// A section of loose blocks (cards, notes) instead of one card (`SettingsSection` ungrouped).
pub(in crate::app) fn loose_section(
    id: impl Into<ElementId>,
    title: impl IntoElement,
    blocks: impl IntoIterator<Item = AnyElement>,
) -> AnyElement {
    section_frame(id, title, None, v_flex().gap(px(4.)).children(blocks).into_any_element())
}

fn section_frame(
    id: impl Into<ElementId>,
    title: impl IntoElement,
    action: Option<AnyElement>,
    body: AnyElement,
) -> AnyElement {
    v_flex()
        .id(id)
        .gap(px(10.))
        .child(
            h_flex()
                .min_h(px(28.))
                .px(px(16.))
                .justify_between()
                .gap(px(16.))
                .child(
                    h_flex()
                        .min_w_0()
                        .gap(px(8.))
                        .text_base()
                        .line_height(px(24.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .children(action),
        )
        .child(body)
        .into_any_element()
}

/// A piece of a description: plain text, a link to another settings section, or a URL.
#[derive(Clone, Copy)]
pub(in crate::app) enum Part {
    Text(&'static str),
    Section(&'static str, &'static str),
    Url(&'static str, &'static str),
}

/// Description text with its links underlined and clickable; section links open that section.
pub(in crate::app) fn rich_text(
    id: impl Into<ElementId>,
    parts: &'static [Part],
    workspace: gpui_kit::WeakEntity<Workspace>,
) -> AnyElement {
    let mut text = String::new();
    let mut links = Vec::new();
    for part in parts {
        let label = match *part {
            Part::Text(label) | Part::Section(label, _) | Part::Url(label, _) => label,
        };
        let start = text.len();
        text.push_str(label);
        if !matches!(part, Part::Text(_)) {
            links.push((start..text.len(), *part));
        }
    }
    let underline = gpui_kit::HighlightStyle {
        underline: Some(gpui_kit::UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
        ..Default::default()
    };
    let styled =
        gpui_kit::StyledText::new(text).with_highlights(links.iter().map(|(range, _)| (range.clone(), underline)));
    let ranges = links.iter().map(|(range, _)| range.clone()).collect();
    gpui_kit::InteractiveText::new(id, styled)
        .on_click(ranges, move |index, _, cx| match links[index].1 {
            Part::Section(_, section) => {
                let _ = workspace.update(cx, |this, cx| this.open_settings(section, cx));
            }
            Part::Url(_, url) => cx.open_url(url),
            Part::Text(_) => {}
        })
        .into_any_element()
}

/// `SettingsRow`'s chrome: the title line with an optional marker and reset, the description
/// and a status line on the left, the control on the right, and a body below.
pub(in crate::app) struct RowFrame {
    title: AnyElement,
    marker: Option<AnyElement>,
    reset: Option<AnyElement>,
    description: Option<AnyElement>,
    status: Option<AnyElement>,
    control: Option<AnyElement>,
    body: Option<AnyElement>,
    dimmed: bool,
}

impl RowFrame {
    pub(in crate::app) fn new(title: impl IntoElement) -> Self {
        Self {
            title: title.into_any_element(),
            marker: None,
            reset: None,
            description: None,
            status: None,
            control: None,
            body: None,
            dimmed: false,
        }
    }

    pub(in crate::app) fn description(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }

    pub(in crate::app) fn status(mut self, status: Option<impl IntoElement>) -> Self {
        self.status = status.map(IntoElement::into_any_element);
        self
    }

    pub(in crate::app) fn reset(mut self, reset: Option<impl IntoElement>) -> Self {
        self.reset = reset.map(IntoElement::into_any_element);
        self
    }

    pub(in crate::app) fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// `SettingsUnavailableGroup` dims titles and descriptions to 64%.
    pub(in crate::app) fn dimmed(mut self, dimmed: bool) -> Self {
        self.dimmed = dimmed;
        self
    }

    pub(in crate::app) fn render(self, t: &Tokens) -> gpui_kit::Div {
        let text = if self.dimmed { 0.64 } else { 1. };
        let has_body = self.body.is_some();
        v_flex()
            .px(px(16.))
            .pt(px(12.))
            .pb(px(if has_body { 4. } else { 12. }))
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
                                            .text_sm()
                                            .line_height(px(20.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(t.foreground.opacity(text))
                                            .child(self.title),
                                    )
                                    .children(self.marker)
                                    .child(
                                        div().size(px(20.)).flex().items_center().justify_center().children(self.reset),
                                    ),
                            )
                            .children(self.description.map(|description| {
                                div()
                                    .max_w(px(672.))
                                    .text_xs()
                                    .line_height(px(18.))
                                    .text_color(t.muted_foreground.opacity(text))
                                    .child(description)
                            }))
                            .children(
                                self.status.map(|status| {
                                    div().pt(px(2.)).text_xs().text_color(t.muted_foreground).child(status)
                                }),
                            ),
                    )
                    .children(
                        self.control.map(|control| h_flex().flex_none().justify_end().gap(px(8.)).child(control)),
                    ),
            )
            .children(self.body)
    }
}

/// A footnote line inside a card (`px-3 py-2.5 text-xs text-muted-foreground`).
pub(in crate::app) fn note(text: impl Into<SharedString>, t: &Tokens) -> AnyElement {
    div().px(px(16.)).py(px(10.)).text_xs().text_color(t.muted_foreground).child(text.into()).into_any_element()
}

/// `SettingsGroup` grouped: a bordered card whose rows are divided by hairlines.
pub(in crate::app) fn card(rows: impl IntoIterator<Item = AnyElement>, t: &Tokens) -> AnyElement {
    hairlines(rows, t)
        .rounded(px(12.))
        .border_1()
        .border_color(t.border.opacity(0.6))
        .bg(t.card.opacity(0.4))
        .into_any_element()
}

/// Rows divided like a card's, without the card around them.
pub(in crate::app) fn hairlines(rows: impl IntoIterator<Item = AnyElement>, t: &Tokens) -> gpui_kit::Div {
    v_flex().children(
        rows.into_iter().enumerate().map(|(index, row)| {
            div().when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.5))).child(row)
        }),
    )
}

/// The 16px heading of a section without a card, such as "Color scheme".
pub(in crate::app) fn subheading(text: impl Into<SharedString>) -> impl IntoElement {
    h_flex()
        .min_h(px(28.))
        .px(px(16.))
        .text_base()
        .line_height(px(24.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}

/// Where a row's control goes: beside its text, or under it like a textarea.
enum RowControl {
    Side(AnyElement),
    Body(AnyElement),
}

/// Sections whose rows apply to this device only show no scope sentence
/// (`SETTINGS_DEVICE_ONLY_PATHS`).
pub(super) fn device_only(section: &str) -> bool {
    matches!(section, "general" | "appearance" | "chat" | "diffs" | "snap-shot" | "connections" | "access" | "about")
}

impl Workspace {
    pub(in crate::app) fn settings_view(&self) -> SettingsView {
        let groups = self.settings_project_groups();
        let scope_groups: Vec<scope::Group> = groups
            .iter()
            .map(|group| scope::Group {
                key: group.key.clone(),
                name: group.display_name.clone(),
                members: group
                    .members
                    .iter()
                    .map(|m| scope::Project {
                        id: m.project.id.clone(),
                        name: project_name(&m.project).to_string(),
                        legacy_model_selection: m.project.default_model_selection.as_ref().map(|m| m.to_json()),
                        legacy_thread_env_mode: m.project.default_thread_env_mode.clone(),
                    })
                    .collect(),
            })
            .collect();
        let connected = self.client().is_some() && self.server_info.environment_id.is_some();
        let mut view = SettingsView {
            scope: Scope { kind: ScopeKind::All, label: String::new() },
            targets: Vec::new(),
            groups,
            environment_id: self.server_info.environment_id.clone().unwrap_or_default(),
            environment_label: self.server_info.environment_label.clone().unwrap_or_else(|| "this environment".into()),
            environment_settings: connected.then(|| self.server_info.settings.clone()),
            client: self.client_settings.values.clone(),
        };
        view.scope = scope::resolve(&self.settings_ui.scope, &scope_groups, &view.environment());
        view.targets = scope::targets(&view.scope, &view.environment());
        view
    }

    /// Writes a unified patch (`useUpdateScopedSettings`).
    pub(in crate::app) fn update_settings(&mut self, patch: Value, window: &mut Window, cx: &mut Context<Self>) {
        self.update_settings_then(patch, window, cx, |_, _, _| {});
    }

    /// [`Self::update_settings`], then `done` with whether the environment saved it.
    pub(in crate::app) fn update_settings_then(
        &mut self,
        patch: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
    ) {
        let Value::Object(patch) = patch else { return };
        let view = self.settings_view();
        let plan = scope::plan_patch(&view.scope, &view.environment(), &patch);
        self.run_settings_plan(plan, window, cx, done);
    }

    /// A plain `server.updateSettings` on the connected environment, for settings the web keeps
    /// per environment whatever the scope (source control discovery, hosts and tokens).
    pub(in crate::app) fn update_environment_settings(
        &mut self,
        patch: Value,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
    ) {
        let Some(client) = self.client() else { return };
        cx.spawn(async move |this, cx| {
            let result = client.request("server.updateSettings", serde_json::json!({ "patch": patch })).await;
            let _ = this.update(cx, |this, cx| {
                match &result {
                    Ok(saved) => {
                        if let Err(error) = this.server_info.apply_settings(saved) {
                            log::warn!("bad saved settings: {error}");
                        }
                    }
                    Err(error) => this.error = Some(format!("Could not save settings: {error}").into()),
                }
                done(this, result.is_ok(), cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Resets the selected project's overrides of `keys` (`useClearScopedSettings`).
    pub(in crate::app) fn clear_setting_overrides(
        &mut self,
        keys: &[&str],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = self.settings_view();
        let plan = scope::plan_clear(&view.scope, &view.environment(), keys);
        self.run_settings_plan(plan, window, cx, |_, _, _| {});
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
        self.run_settings_plan(plan, window, cx, |_, _, _| {});
    }

    /// Applies device keys now and sends the server's part (`persistScopedSettingsPatch`).
    fn run_settings_plan(
        &mut self,
        plan: Plan,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
    ) {
        if let Some(reason) = plan.unavailable {
            window.push_notification(Notification::warning(reason).title("Setting not saved"), cx);
            done(self, false, cx);
            return;
        }
        if !plan.client_patch.is_empty() {
            self.client_settings.apply(&plan.client_patch);
            theme::set_appearance(theme::Appearance::from_settings(&self.client_settings.values), window, cx);
            // `composerRichTextEnabled` and the theme's colors.
            self.sync_composer_marks(cx);
            cx.notify();
            // The sidebar is a cached view and reads several device settings.
            self.notify_sidebar(cx);
            self.report_browser_profiles(cx);
        }
        let Some(patch) = plan.server_patch else { return done(self, true, cx) };
        let Some(client) = self.client() else { return done(self, false, cx) };
        let previous = self.server_info.settings.clone();
        let label = self.server_info.environment_label.clone().unwrap_or_else(|| "this environment".into());
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", serde_json::json!({ "patch": patch })).await;
            let _ = this.update_in(cx, |this, window, cx| {
                done(this, result.is_ok(), cx);
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
        self.settings_page_with_gap(section, view, 32., content, cx)
    }

    /// [`Self::settings_page`] with its own gap between blocks, like the web's `className="gap-6"`.
    pub(in crate::app) fn settings_page_with_gap(
        &mut self,
        section: &str,
        view: &SettingsView,
        gap: f32,
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
                    .gap(px(gap))
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
        let project_scope = view.scope.is_project();
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
        let SettingRow { id, title, description, status, body, control, unavailable, .. } = row;
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
        let (control, body) = match control {
            Some(RowControl::Body(field)) => (None, Some(field)),
            Some(RowControl::Side(control)) => (Some(control), body),
            None => (None, body),
        };
        let inert = if unavailable { Some("") } else { inert };
        let group: SharedString = format!("settings-row-{id}").into();
        let target = self.settings_ui.target.as_ref() == Some(&id);
        let mut frame = RowFrame::new(title).dimmed(unavailable).status(status);
        frame.marker = inheritance.map(|marker| {
            div()
                .size(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .when(quiet_marker, |el| el.invisible().group_hover(group.clone(), |s| s.visible()))
                .child(marker)
                .into_any_element()
        });
        frame.reset = reset.filter(|_| !unavailable);
        frame.description = description;
        frame.body = body;
        frame.control = control.map(|control| match inert {
            Some(message) => div()
                .id(SharedString::from(format!("{id}-inert")))
                .flex_none()
                .opacity(0.5)
                .occlude()
                .when(!message.is_empty(), |el| el.tooltip(move |window, cx| Tooltip::new(message).build(window, cx)))
                .child(control)
                .into_any_element(),
            None => control,
        });
        frame
            .render(&t)
            .id(group.clone())
            .group(group)
            .relative()
            .when(target, |el| el.bg(t.accent).child(self.scroll_target_marker()))
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
        if disconnected {
            return None;
        }
        // At project scope a scoped row only resets its override, whatever its own reset.
        if view.scope.is_project() && !scoped_keys.is_empty() {
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
        if let Some(custom) = &row.reset {
            let patch = custom.clone()?;
            return Some(
                reset_button(&row.id, "Reset to default", cx)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.update_settings(patch.clone(), window, cx);
                    }))
                    .into_any_element(),
            );
        }
        if row.keys.is_empty() || (!mixed && row.keys.iter().all(|k| view.is_default(k))) {
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
                            let name = view.group_of(project).map_or(project.as_str(), |g| g.display_name.as_str());
                            format!("{} · {name}", view.environment_label)
                        }
                        None => view.environment_label.clone(),
                    };
                    (label, scope::inheritance_layers(target, &environment_settings, key))
                })
                .collect(),
        );
        // (project id, its group's key and name, value); overrides of ungrouped ids are skipped.
        let overriding: Rc<Vec<(String, String, String, String)>> = Rc::new(
            overriding
                .iter()
                .filter_map(|project| {
                    let group = view.group_of(project)?;
                    let value =
                        environment_settings.get("projectSettingsOverrides").get(project).and_then(|o| o.get(key));
                    Some((
                        project.clone(),
                        group.key.clone(),
                        group.display_name.clone(),
                        value.map(|v| scope::format_value(key, v)).unwrap_or_default(),
                    ))
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
                let open_project = |key: String| {
                    let workspace = workspace.clone();
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
                        workspace.update(cx, |this, cx| {
                            this.settings_ui.scope = this.settings_ui.scope.with_project(Some(key.clone()));
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
                                    .text_xs()
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
                                        .text_xs()
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
                                .children(overriding.iter().map(|(id, group, name, value)| {
                                    h_flex()
                                        .px(px(8.))
                                        .py(px(4.))
                                        .gap(px(12.))
                                        .text_sm()
                                        .child(
                                            div()
                                                .id(SharedString::from(format!("override-{id}")))
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .cursor_pointer()
                                                .hover(|s| s.underline())
                                                .child(name.clone())
                                                .on_click(open_project(group.clone())),
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

    #[allow(clippy::too_many_arguments)]
    fn render_control(
        &mut self,
        view: &SettingsView,
        id: &SharedString,
        key: Option<&'static str>,
        control: Control,
        mixed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<RowControl> {
        let t = *theme::tokens(cx);
        let element = match (control, key) {
            (Control::Text { placeholder, width }, Some(key)) => {
                let field = self.setting_field(id, key, view.str(key), placeholder, false, window, cx);
                field.sized(px(width)).into_any_element()
            }
            (Control::Textarea { placeholder }, Some(key)) => {
                let field = self.setting_field(id, key, view.str(key), placeholder, true, window, cx);
                return Some(RowControl::Body(super::fields::textarea_body(&field)));
            }
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
                            .font_family(t.fonts.mono)
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child(format!("{value}{unit}")),
                    )
                    .child(div().flex_1().min_w_0().child(Slider::new(&state).horizontal()))
                    .into_any_element()
            }
            _ => return None,
        };
        Some(RowControl::Side(element))
    }

    /// A select over JSON values that writes `key` through the scope; `trigger` is what the
    /// closed select shows.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn value_select(
        &self,
        id: impl Into<SharedString>,
        key: &'static str,
        current: &Value,
        options: Vec<(Value, SharedString)>,
        trigger: impl IntoElement,
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = current.clone();
        let options = options.into_iter().map(|(value, label)| (value.clone(), label, value == current)).collect();
        self.menu_select(id, options, trigger, width, cx, move |this, value, window, cx| {
            this.update_settings(serde_json::json!({ key: value }), window, cx)
        })
    }

    /// `SelectTrigger size="sm"` with its menu of `(value, label, checked)` items.
    pub(in crate::app) fn menu_select<T: Clone + 'static>(
        &self,
        id: impl Into<SharedString>,
        options: Vec<(T, SharedString, bool)>,
        trigger: impl IntoElement,
        width: f32,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let workspace = cx.entity();
        let on_pick = Rc::new(on_pick);
        select_trigger(&id.into(), trigger, width, &t)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                for (value, label, checked) in &options {
                    let (value, on_pick) = (value.clone(), on_pick.clone());
                    menu = menu.item(PopupMenuItem::new(label.clone()).checked(*checked).on_click(
                        window.listener_for(&workspace, move |this, _, window, cx| {
                            on_pick(this, value.clone(), window, cx)
                        }),
                    ));
                }
                menu
            })
            .into_any_element()
    }

    /// The text field of a `.text()` or `.textarea()` row, writing through the scope.
    #[allow(clippy::too_many_arguments)]
    fn setting_field(
        &mut self,
        id: &SharedString,
        key: &'static str,
        saved: &str,
        placeholder: &str,
        multiline: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> super::fields::FieldInput {
        self.text_field(id, saved, placeholder, multiline, None, window, cx, move |this, text, window, cx| {
            this.update_settings(serde_json::json!({ key: text }), window, cx)
        })
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
        let selected = search.project.as_ref().and_then(|key| view.groups.iter().find(|g| &g.key == key));
        let project_label = match (&search.project, selected) {
            (_, Some(group)) => group.display_name.clone(),
            (Some(_), None) => "Unavailable project".to_owned(),
            (None, None) => "All projects".to_owned(),
        };
        let environment_label = match &search.machine {
            Some(id) if *id == view.environment_id => view.environment_label.clone(),
            Some(_) => "Unavailable environment".to_owned(),
            None => "All environments".to_owned(),
        };
        // Groups come sorted by name; a name shared by several shows where each lives, and
        // archived ones say so (`ProjectScopeMenu`).
        let projects: Rc<Vec<ScopeProject>> = Rc::new(
            view.groups
                .iter()
                .map(|group| {
                    let shared = view.groups.iter().any(|o| o.key != group.key && o.display_name == group.display_name);
                    let representative = group.representative();
                    let location =
                        shared.then(|| format!("{} · {}", view.environment_label, representative.workspace_root));
                    let secondary = match (group.archived, location) {
                        (true, Some(location)) => Some(format!("Archived · {location}")),
                        (true, None) => Some("Archived".to_owned()),
                        (false, location) => location,
                    };
                    ScopeProject {
                        key: group.key.clone(),
                        name: group.display_name.clone(),
                        secondary,
                        icon: representative.project_icon.clone(),
                        title: representative.title.clone(),
                        favicon: self.favicon(representative),
                    }
                })
                .collect(),
        );
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
            let icon = selected.map(|group| {
                let project = group.representative();
                project_icon(project.project_icon.as_ref(), &project.title, self.favicon(project), 14., &t)
            });
            scope_trigger("settings-scope-project", icon, project_label, &t).dropdown_menu_with_anchor(
                gpui_kit::Anchor::TopLeft,
                move |mut menu, _, _| {
                    menu = menu
                        .item(
                            PopupMenuItem::new("All projects")
                                .checked(search.project.is_none())
                                .on_click(pick(workspace.clone(), search.with_project(None))),
                        )
                        .separator();
                    for project in projects.iter() {
                        let shown = project.clone();
                        let item = PopupMenuItem::element(move |_, cx| {
                            let t = theme::tokens(cx);
                            let ScopeProject { name, secondary, icon, title, favicon, .. } = &shown;
                            h_flex()
                                .min_w_0()
                                .gap(px(8.))
                                .child(project_icon(icon.as_ref(), title, favicon.clone(), 14., t))
                                .child(v_flex().min_w_0().child(div().truncate().child(name.clone())).children(
                                    secondary.clone().map(|secondary| {
                                        div().truncate().text_xs().text_color(t.muted_foreground).child(secondary)
                                    }),
                                ))
                        });
                        menu = menu.item(
                            item.checked(search.project.as_deref() == Some(project.key.as_str()))
                                .on_click(pick(workspace.clone(), search.with_project(Some(project.key.clone())))),
                        );
                    }
                    menu.max_w(px(320.)).max_h(px(360.)).scrollable(true)
                },
            )
        };
        let environment_menu = {
            let (search, workspace) = (search.clone(), workspace.clone());
            scope_trigger("settings-scope-environment", None, environment_label, &t).dropdown_menu_with_anchor(
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
            .text_sm()
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
        .text_sm()
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
pub(in crate::app) fn select_trigger(id: &SharedString, label: impl IntoElement, width: f32, t: &Tokens) -> Button {
    Button::new(SharedString::from(format!("{id}-select")))
        .ghost()
        .when(width > 0., |b| b.w(px(width)))
        .h(px(28.))
        .px(px(9.))
        .rounded(px(8.))
        .border_1()
        .border_color(t.input)
        .bg(if t.dark { t.input.opacity(0.32) } else { t.background })
        .text_sm()
        .font_weight(FontWeight::NORMAL)
        .text_color(t.foreground)
        .child(
            h_flex()
                .w_full()
                .justify_between()
                .gap(px(8.))
                .child(h_flex().min_w_0().gap(px(8.)).overflow_hidden().child(label))
                .child(Icon::new(IconName::ChevronDown).size(px(14.)).text_color(t.muted_foreground.opacity(0.7))),
        )
}

/// A project group as the scope sentence's menu lists it.
#[derive(Clone)]
struct ScopeProject {
    key: String,
    name: String,
    secondary: Option<String>,
    icon: Option<Value>,
    title: String,
    favicon: Option<std::sync::Arc<gpui_kit::Image>>,
}

/// The outline `sm` button the scope sentence's menus open from.
fn scope_trigger(id: &'static str, icon: Option<AnyElement>, label: String, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .max_w(px(288.))
        .h(px(28.))
        .px(px(9.))
        .rounded(px(8.))
        .border_1()
        .border_color(t.input)
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.foreground)
        .child(
            h_flex()
                .gap(px(6.))
                .children(icon)
                .child(div().min_w_0().truncate().child(label))
                .child(Icon::new(IconName::ChevronDown).size(px(14.)).text_color(t.muted_foreground.opacity(0.7))),
        )
}

/// `SettingResetButton`: a 20px ghost button with the undo arrow.
pub(in crate::app) fn reset_button(
    id: &SharedString,
    tooltip: impl Into<SharedString>,
    cx: &Context<Workspace>,
) -> Button {
    let t = theme::tokens(cx);
    Button::new(SharedString::from(format!("{id}-reset")))
        .ghost()
        .size(px(20.))
        .p_0()
        .rounded(px(4.))
        .child(Icon::new(IconName::Undo2).size(px(12.)).text_color(t.muted_foreground))
        .tooltip(tooltip.into())
}
