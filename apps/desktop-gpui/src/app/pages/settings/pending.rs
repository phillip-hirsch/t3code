//! What the Threads, Worktrees and Source control sections draw with until they move onto
//! `widgets.rs`: the web's `SettingsSection` and `SettingsRow` look (`settingsLayout.tsx`),
//! a select trigger, the model and effort pickers (`ProviderModelPicker`, `TraitsPicker`),
//! and text fields that save on blur. Plain rows are `SettingRowSpec`s, so moving them onto
//! the shared widgets and scopes is mechanical. Writes are plain global `server.updateSettings`
//! patches; the web writes the same patch to every selected environment or project.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{
    Input, InputEvent, InputState, NumberInputEvent, StepAction, Textarea, TextareaState,
};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window,
    div, px, svg,
};
use serde_json::{Map, Value};

use crate::app::Workspace;
use crate::model::{ModelSelection, Provider};
use crate::model_label;
use crate::theme::{Tokens, tokens};

pub(in crate::app) type OnPick<T> = Rc<dyn Fn(&mut Workspace, T, &mut Context<Workspace>)>;

/// One plain setting: the value at `path` in `ServerSettings`, edited by one control.
pub(in crate::app) struct SettingRowSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// Dot path into `ServerSettings`; the patch has the same shape.
    pub path: &'static str,
    pub control: Control,
    /// The schema default as JSON. A different value shows the reset button; `None` never does.
    pub default: Option<&'static str>,
    /// Where the web edits it: every `serverScoped` row also writes to projects and checkouts.
    /// Read by the shared scope layer once these rows move onto `widgets.rs`.
    #[allow(dead_code)]
    pub scopes: &'static [&'static str],
}

pub(in crate::app) enum Control {
    Switch,
    /// A single-line field saved on blur or Enter, trimmed.
    Text(&'static str),
    /// A four-row field under the row, saved on blur, trimmed.
    Textarea(&'static str),
}

pub(in crate::app) const SERVER_SCOPES: &[&str] = &["environment", "project", "checkout"];

/// The value at a dot path, `Null` when missing.
pub(in crate::app) fn at<'a>(settings: &'a Value, path: &str) -> &'a Value {
    path.split('.').try_fold(settings, |value, key| value.get(key)).unwrap_or(&Value::Null)
}

/// A patch that sets one dot path.
pub(in crate::app) fn patch(path: &str, value: Value) -> Value {
    path.rsplit('.').fold(value, |inner, key| Value::Object(Map::from_iter([(key.to_owned(), inner)])))
}

impl Workspace {
    pub(in crate::app) fn setting(&self, path: &str) -> &Value {
        at(&self.server_info.settings, path)
    }

    pub(in crate::app) fn pending_update_settings(&mut self, patch: Value, cx: &mut Context<Self>) {
        self.update_settings_then(patch, cx, |_, _, _| {});
    }

    /// `server.updateSettings`; the server echoes the result as `settingsUpdated`.
    pub(in crate::app) fn update_settings_then(
        &mut self,
        patch: Value,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
    ) {
        let Some(client) = self.client() else { return };
        cx.spawn(async move |this, cx| {
            let result = client.request("server.updateSettings", serde_json::json!({ "patch": patch })).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = &result {
                    this.error = Some(format!("Could not save settings: {error}").into());
                }
                done(this, result.is_ok(), cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::app) fn render_spec(
        &mut self,
        spec: &'static SettingRowSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Row {
        let value = self.setting(spec.path).clone();
        let path = spec.path;
        let reset = spec.default.and_then(|default| {
            let default: Value = serde_json::from_str(default).ok()?;
            let unset = value.is_null() && default.is_null();
            (!unset && value != default).then(|| {
                reset_button(
                    spec.id,
                    cx.listener(move |this, _, _, cx| this.pending_update_settings(patch(path, default.clone()), cx)),
                )
            })
        });
        let row = Row::new(spec.id, spec.label).description(spec.description).reset(reset);
        match spec.control {
            Control::Switch => {
                let checked = value.as_bool().unwrap_or(false);
                row.control(switch(spec.id, checked, cx, move |this, on, cx| {
                    this.pending_update_settings(patch(path, Value::Bool(on)), cx)
                }))
            }
            Control::Text(placeholder) | Control::Textarea(placeholder) => {
                let multiline = matches!(spec.control, Control::Textarea(_));
                let saved = value.as_str().unwrap_or_default();
                let field =
                    self.text_field(spec.id, saved, placeholder, multiline, None, window, cx, move |this, text, cx| {
                        this.pending_update_settings(patch(path, Value::String(text)), cx)
                    });
                if multiline { row.body(textarea_body(&field)) } else { row.control(field.sized(px(224.))) }
            }
        }
    }

    /// A text field kept across frames, synced to `saved` and committing trimmed text on blur
    /// or Enter when it was edited (`DraftInput` and the uncontrolled `Textarea`s in the web).
    /// `normalize` rewrites each change from the previous text, as `resolvePastedPath` does.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn text_field(
        &mut self,
        key: &str,
        saved: &str,
        placeholder: &str,
        multiline: bool,
        normalize: Option<fn(&str, &str) -> String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, String, &mut Context<Self>) + 'static,
    ) -> FieldInput {
        let kind = if multiline { FieldKind::Area } else { FieldKind::Line };
        self.field(key, saved, placeholder, kind, normalize, window, cx, on_commit)
    }

    /// A masked field for a credential. Settings only ever hold it redacted, so the field
    /// starts empty and an empty field saves nothing (`SecretInput`).
    pub(in crate::app) fn secret_field(
        &mut self,
        key: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, String, &mut Context<Self>) + 'static,
    ) -> FieldInput {
        self.field(key, "", placeholder, FieldKind::Secret, None, window, cx, on_commit)
    }

    #[allow(clippy::too_many_arguments)]
    fn field(
        &mut self,
        key: &str,
        saved: &str,
        placeholder: &str,
        kind: FieldKind,
        normalize: Option<fn(&str, &str) -> String>,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, String, &mut Context<Self>) + 'static,
    ) -> FieldInput {
        let workspace = cx.weak_entity();
        let on_commit: OnPick<String> = Rc::new(on_commit);
        let state = window.use_keyed_state(SharedString::from(format!("settings-field-{key}")), cx, |window, cx| {
            TextField::new(kind, placeholder, saved, normalize, workspace, window, cx)
        });
        state.update(cx, |field, cx| {
            field.on_commit = Some(on_commit);
            if let (FieldInput::Line(input), true) = (&field.input, field.placeholder != placeholder) {
                field.placeholder = placeholder.to_owned();
                let placeholder = field.placeholder.clone();
                input.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
            }
            if field.saved != saved {
                field.saved = saved.to_owned();
                field.previous = saved.to_owned();
                field.input.set_value(saved, window, cx);
            }
        });
        state.read(cx).input.clone()
    }

    /// A whole-number stepper kept across frames (`NumberField`): typing stays local until
    /// blur, Enter or a step commits it, clamped to `min..=max`; anything else reverts.
    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn number_field(
        &mut self,
        key: &str,
        value: i64,
        min: i64,
        max: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_commit: impl Fn(&mut Self, i64, &mut Context<Self>) + 'static,
    ) -> Entity<InputState> {
        let workspace = cx.weak_entity();
        let on_commit: OnPick<i64> = Rc::new(on_commit);
        let state = window.use_keyed_state(SharedString::from(format!("settings-number-{key}")), cx, |window, cx| {
            NumberField::new(value, workspace, window, cx)
        });
        state.update(cx, |field, cx| {
            field.on_commit = Some(on_commit);
            (field.min, field.max) = (min, max);
            if field.value != value {
                field.value = value;
                field.input.update(cx, |input, cx| input.set_value(value.to_string(), window, cx));
            }
        });
        state.read(cx).input.clone()
    }
}

pub(in crate::app) struct NumberField {
    input: Entity<InputState>,
    value: i64,
    min: i64,
    max: i64,
    on_commit: Option<OnPick<i64>>,
    workspace: WeakEntity<Workspace>,
    _subscriptions: [Subscription; 2],
}

impl NumberField {
    fn new(value: i64, workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |input, cx| input.set_value(value.to_string(), window, cx));
        let subscriptions = [
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    let typed = this.input.read(cx).value().trim().parse::<f64>().ok();
                    this.commit(typed.map(|typed| typed.round() as i64), window, cx);
                }
            }),
            cx.subscribe_in(&input, window, |this, _, NumberInputEvent::Step(action), window, cx| {
                let typed = this.input.read(cx).value().trim().parse::<f64>();
                let current = typed.map_or(this.value, |typed| typed.round() as i64);
                let step = if matches!(action, StepAction::Increment) { 1 } else { -1 };
                this.commit(Some(current + step), window, cx);
            }),
        ];
        Self { input, value, min: i64::MIN, max: i64::MAX, on_commit: None, workspace, _subscriptions: subscriptions }
    }

    fn commit(&mut self, next: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        let next = next.map_or(self.value, |next| next.clamp(self.min, self.max));
        self.input.update(cx, |input, cx| input.set_value(next.to_string(), window, cx));
        if next == self.value {
            return;
        }
        let (Some(on_commit), Some(workspace)) = (self.on_commit.clone(), self.workspace.upgrade()) else { return };
        workspace.update(cx, |this, cx| on_commit(this, next, cx));
    }
}

#[derive(Clone)]
pub(in crate::app) enum FieldInput {
    Line(Entity<InputState>),
    Area(Entity<TextareaState>),
}

impl FieldInput {
    pub(in crate::app) fn value(&self, cx: &gpui_kit::App) -> String {
        match self {
            Self::Line(input) => input.read(cx).value().to_string(),
            Self::Area(input) => input.read(cx).value().to_string(),
        }
    }

    fn set_value(&self, value: &str, window: &mut Window, cx: &mut gpui_kit::App) {
        let value = value.to_owned();
        match self {
            Self::Line(input) => input.update(cx, |input, cx| input.set_value(value, window, cx)),
            Self::Area(input) => input.update(cx, |input, cx| input.set_value(value, window, cx)),
        }
    }

    pub(in crate::app) fn element(&self) -> AnyElement {
        match self {
            Self::Line(input) => Input::new(input).small().into_any_element(),
            Self::Area(input) => Textarea::new(input).into_any_element(),
        }
    }

    /// The field at a fixed width, as the web's `w-*` classes set it.
    pub(in crate::app) fn sized(&self, width: gpui_kit::Pixels) -> Div {
        div().w(width).child(self.element())
    }
}

pub(in crate::app) struct TextField {
    input: FieldInput,
    saved: String,
    /// The text before the latest change, for `normalize`.
    previous: String,
    placeholder: String,
    /// Credentials clear once saved; settings only ever hold them redacted.
    secret: bool,
    normalize: Option<fn(&str, &str) -> String>,
    edited: bool,
    on_commit: Option<OnPick<String>>,
    workspace: WeakEntity<Workspace>,
    _subscription: Subscription,
}

#[derive(Clone, Copy)]
enum FieldKind {
    Line,
    Area,
    Secret,
}

impl TextField {
    fn new(
        kind: FieldKind,
        placeholder: &str,
        saved: &str,
        normalize: Option<fn(&str, &str) -> String>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let shown_placeholder = placeholder.to_owned();
        let placeholder = placeholder.to_owned();
        let (input, subscription) = if matches!(kind, FieldKind::Area) {
            let input = cx.new(|cx| TextareaState::new(window, cx).rows(4).placeholder(placeholder));
            let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                this.on_event(event, window, cx)
            });
            (FieldInput::Area(input), subscription)
        } else {
            let masked = matches!(kind, FieldKind::Secret);
            let input = cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(placeholder));
            let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                this.on_event(event, window, cx)
            });
            (FieldInput::Line(input), subscription)
        };
        input.set_value(saved, window, cx);
        Self {
            input,
            saved: saved.to_owned(),
            previous: saved.to_owned(),
            placeholder: shown_placeholder,
            secret: matches!(kind, FieldKind::Secret),
            normalize,
            edited: false,
            on_commit: None,
            workspace,
            _subscription: subscription,
        }
    }

    fn on_event(&mut self, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        let enter = matches!(event, InputEvent::PressEnter { .. }) && matches!(self.input, FieldInput::Line(_));
        match event {
            InputEvent::Change => {
                self.edited = true;
                let current = self.input.value(cx);
                let next =
                    self.normalize.map_or_else(|| current.clone(), |normalize| normalize(&self.previous, &current));
                if next != current {
                    self.input.set_value(&next, window, cx);
                }
                self.previous = next;
                return;
            }
            InputEvent::Blur => {}
            _ if enter => {}
            _ => return,
        }
        if !std::mem::take(&mut self.edited) {
            return;
        }
        let value = self.input.value(cx).trim().to_owned();
        if value == self.saved {
            return;
        }
        let (Some(on_commit), Some(workspace)) = (self.on_commit.clone(), self.workspace.upgrade()) else { return };
        workspace.update(cx, |this, cx| on_commit(this, value, cx));
        if self.secret {
            self.input.set_value("", window, cx);
        }
    }
}

/// A textarea set below the row's text, `mt-3 max-w-2xl pb-3.5`.
pub(in crate::app) fn textarea_body(field: &FieldInput) -> AnyElement {
    div().mt(px(12.)).pb(px(14.)).max_w(px(672.)).child(field.element()).into_any_element()
}

/// `SettingsPageContainer`: a scrolling column, `max-w-4xl`, sections 32px apart.
pub(in crate::app) fn page(id: &'static str, sections: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    v_flex()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .items_center()
        .child(
            v_flex()
                .flex_none()
                .w_full()
                .max_w(px(896.))
                .px(px(24.))
                .pt(px(24.))
                .pb(px(48.))
                .gap(px(32.))
                .children(sections),
        )
        .into_any_element()
}

/// `SettingsSection` with the `grouped` card.
pub(in crate::app) fn section(
    title: impl Into<SharedString>,
    header_action: Option<AnyElement>,
    rows: impl IntoIterator<Item = AnyElement>,
    t: &Tokens,
) -> AnyElement {
    v_flex()
        .gap(px(10.))
        .child(
            h_flex()
                .min_h(px(28.))
                .px(px(16.))
                .justify_between()
                .gap(px(16.))
                .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(title.into()))
                .children(header_action),
        )
        .child(group(rows, t))
        .into_any_element()
}

/// `SettingsGroup`: rounded card, rows split by hairlines.
pub(in crate::app) fn group(rows: impl IntoIterator<Item = AnyElement>, t: &Tokens) -> Div {
    let mut card = v_flex().rounded(px(12.)).border_1().border_color(t.border.opacity(0.6)).bg(t.card.opacity(0.4));
    for (index, row) in rows.into_iter().enumerate() {
        card = card.child(div().when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.5))).child(row));
    }
    card
}

/// `SettingsRow`: title, reset, description and status on the left, the control on the right,
/// and an optional body (a textarea, a list) below.
pub(in crate::app) struct Row {
    id: SharedString,
    title: SharedString,
    description: Option<SharedString>,
    status: Option<SharedString>,
    reset: Option<AnyElement>,
    control: Option<AnyElement>,
    body: Option<AnyElement>,
}

impl Row {
    pub(in crate::app) fn new(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: None,
            status: None,
            reset: None,
            control: None,
            body: None,
        }
    }

    pub(in crate::app) fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub(in crate::app) fn status(mut self, status: Option<impl Into<SharedString>>) -> Self {
        self.status = status.map(Into::into);
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

    pub(in crate::app) fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }

    pub(in crate::app) fn render(self, t: &Tokens) -> AnyElement {
        let has_body = self.body.is_some();
        v_flex()
            .id(ElementId::Name(format!("settings-row-{}", self.id).into()))
            .px(px(16.))
            .pt(px(12.))
            .pb(px(if has_body { 4. } else { 12. }))
            .child(
                h_flex()
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
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(t.foreground)
                                            .child(self.title),
                                    )
                                    .child(
                                        div().size(px(20.)).flex().items_center().justify_center().children(self.reset),
                                    ),
                            )
                            .children(self.description.map(|description| {
                                div()
                                    .max_w(px(672.))
                                    .text_xs()
                                    .line_height(px(18.))
                                    .text_color(t.muted_foreground)
                                    .child(description)
                            }))
                            .children(
                                self.status.map(|status| {
                                    div().pt(px(2.)).text_xs().text_color(t.muted_foreground).child(status)
                                }),
                            ),
                    )
                    .children(
                        self.control.map(|control| {
                            h_flex().flex_none().min_w(px(160.)).justify_end().gap(px(8.)).child(control)
                        }),
                    ),
            )
            .children(self.body)
            .into_any_element()
    }
}

/// `SettingResetButton`: a 20px ghost button with an undo arrow.
pub(in crate::app) fn reset_button(
    id: &str,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> Button {
    Button::new(SharedString::from(format!("reset-{id}")))
        .ghost()
        .xsmall()
        .size(px(20.))
        .icon(Icon::new(Lucide::Undo2).with_size(px(12.)))
        .tooltip("Reset to default")
        .on_click(on_click)
}

pub(in crate::app) fn switch(
    id: &str,
    checked: bool,
    cx: &mut Context<Workspace>,
    on_change: impl Fn(&mut Workspace, bool, &mut Context<Workspace>) + 'static,
) -> Switch {
    let view = cx.entity();
    Switch::new(SharedString::from(format!("switch-{id}"))).checked(checked).on_click(move |on, _, cx| {
        let on = *on;
        view.update(cx, |this, cx| on_change(this, on, cx));
    })
}

/// `SelectTrigger size="sm"` with its popup: a bordered 28px button listing `options`.
pub(in crate::app) fn select(
    id: &str,
    label: impl Into<SharedString>,
    options: Vec<(SharedString, SharedString)>,
    current: Option<SharedString>,
    cx: &mut Context<Workspace>,
    on_pick: impl Fn(&mut Workspace, SharedString, &mut Context<Workspace>) + 'static,
) -> AnyElement {
    let view = cx.entity();
    let on_pick: OnPick<SharedString> = Rc::new(on_pick);
    trigger(SharedString::from(format!("select-{id}")), tokens(cx))
        .child(trigger_label(label.into()))
        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
            for (value, label) in &options {
                let (value, on_pick) = (value.clone(), on_pick.clone());
                menu = menu.item(
                    PopupMenuItem::new(label.clone())
                        .checked(current.as_ref() == Some(&value))
                        .on_click(window.listener_for(&view, move |this, _, _, cx| on_pick(this, value.clone(), cx))),
                );
            }
            menu
        })
        .into_any_element()
}

/// The bordered trigger shared by selects and pickers (`SETTINGS_PICKER_TRIGGER_CLASSNAME`).
pub(in crate::app) fn trigger(id: impl Into<ElementId>, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .h(px(28.))
        .min_w(px(144.))
        .px(px(9.))
        .gap(px(6.))
        .rounded(px(8.))
        .border_1()
        .border_color(t.input)
        .bg(if t.dark { t.input.opacity(0.32) } else { t.background })
        .text_sm()
        .text_color(t.foreground)
}

fn trigger_label(label: SharedString) -> impl IntoElement {
    h_flex()
        .flex_1()
        .min_w_0()
        .justify_between()
        .gap(px(6.))
        .child(div().truncate().child(label))
        .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)).flex_none().opacity(0.7))
}

/// Text-generation providers a model row can pick from (`supportsTextGeneration !== false`).
pub(in crate::app) fn generates_text(provider: &Provider) -> bool {
    provider.supports_text_generation != Some(false)
}

impl Workspace {
    /// `ProviderModelPicker` in a settings row: provider icon, short model name and a menu of
    /// every model of every usable provider that `include` keeps.
    pub(in crate::app) fn model_picker(
        &self,
        id: &'static str,
        selection: &ModelSelection,
        label: Option<&str>,
        include: fn(&Provider) -> bool,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, ModelSelection, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let t = *tokens(cx);
        let provider = self.server_info.provider(&selection.instance_id);
        let model = self.server_info.model(selection);
        let name: SharedString = match (label, provider.zip(model)) {
            (Some(label), _) => label.to_owned().into(),
            (None, Some((provider, model))) => model_label::trigger_model_name(model, &provider.driver).into(),
            (None, None) => selection.model.clone().into(),
        };
        let icon = provider.and_then(|p| crate::assets::provider_icon(&p.driver));
        let groups: Vec<(SharedString, Vec<(SharedString, ModelSelection)>)> = self
            .server_info
            .providers
            .iter()
            .filter(|p| p.is_selectable() && include(p))
            .map(|p| {
                let models = p.models.iter().map(|m| {
                    let pick =
                        ModelSelection { instance_id: p.instance_id.clone(), model: m.slug.clone(), options: None };
                    (SharedString::from(m.name.clone().unwrap_or_else(|| m.slug.clone())), pick)
                });
                (p.display_name.clone().unwrap_or_else(|| p.instance_id.clone()).into(), models.collect())
            })
            .collect();
        let current = selection.clone();
        let view = cx.entity();
        let on_pick: OnPick<ModelSelection> = Rc::new(on_pick);
        trigger(id, &t)
            .min_w(px(0.))
            .children(icon.map(|(path, tint)| {
                svg().path(path).size(px(16.)).flex_none().text_color(tint.unwrap_or(t.foreground))
            }))
            .child(trigger_label(name))
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu: PopupMenu, window, _| {
                for (index, (group, models)) in groups.iter().enumerate() {
                    if index > 0 {
                        menu = menu.separator();
                    }
                    menu = menu.label(group.clone());
                    for (name, pick) in models {
                        let checked = pick.instance_id == current.instance_id && pick.model == current.model;
                        let (pick, on_pick) = (pick.clone(), on_pick.clone());
                        menu = menu.item(PopupMenuItem::new(name.clone()).checked(checked).on_click(
                            window.listener_for(&view, move |this, _, _, cx| on_pick(this, pick.clone(), cx)),
                        ));
                    }
                }
                menu.max_h(px(420.)).scrollable(true)
            })
            .into_any_element()
    }

    /// `TraitsPicker` with prompt-injected effort left out: the model's effort levels and its
    /// on/off options. `None` when the model has neither.
    pub(in crate::app) fn traits_picker(
        &self,
        id: &'static str,
        selection: &ModelSelection,
        cx: &mut Context<Self>,
        on_pick: impl Fn(&mut Self, ModelSelection, &mut Context<Self>) + 'static,
    ) -> Option<AnyElement> {
        let t = *tokens(cx);
        let descriptors = self.server_info.model(selection)?.capabilities.as_ref()?.option_descriptors.clone();
        let effort = model_label::reasoning(&descriptors).filter(|d| d.kind == "select").cloned();
        let toggles: Vec<_> = descriptors.iter().filter(|d| d.kind == "boolean").cloned().collect();
        if effort.is_none() && toggles.is_empty() {
            return None;
        }
        let label = model_label::effort_label(&descriptors, selection).unwrap_or_else(|| "Options".into());
        let current = selection.clone();
        let view = cx.entity();
        let on_pick: OnPick<ModelSelection> = Rc::new(on_pick);
        Some(
            trigger(id, &t)
                .min_w(px(0.))
                .child(trigger_label(label.into()))
                .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu: PopupMenu, window, _| {
                    if let Some(effort) = &effort {
                        let value = model_label::current_value(effort, &current);
                        menu = menu.label(effort.label.clone());
                        for option in effort.options.iter().filter(|o| !effort.prompt_injected_values.contains(&o.id)) {
                            let pick = model_label::with_option(&current, &effort.id, Value::String(option.id.clone()));
                            let on_pick = on_pick.clone();
                            menu = menu.item(
                                PopupMenuItem::new(option.label.clone())
                                    .checked(value.as_ref().and_then(Value::as_str) == Some(&option.id))
                                    .on_click(
                                        window
                                            .listener_for(&view, move |this, _, _, cx| on_pick(this, pick.clone(), cx)),
                                    ),
                            );
                        }
                    }
                    for toggle in &toggles {
                        let on =
                            model_label::current_value(toggle, &current).and_then(|v| v.as_bool()).unwrap_or(false);
                        let pick = model_label::with_option(&current, &toggle.id, Value::Bool(!on));
                        let on_pick = on_pick.clone();
                        menu = menu.separator().item(PopupMenuItem::new(toggle.label.clone()).checked(on).on_click(
                            window.listener_for(&view, move |this, _, _, cx| on_pick(this, pick.clone(), cx)),
                        ));
                    }
                    menu
                })
                .into_any_element(),
        )
    }
}

/// A muted line in a row's control slot, as the web's `text-sm text-muted-foreground` spans.
pub(in crate::app) fn muted(text: impl Into<SharedString>, t: &Tokens) -> Div {
    div().text_sm().text_color(t.muted_foreground).child(text.into())
}

/// `AdvancedSettings`: a fold at the end of a section, closed until clicked.
pub(in crate::app) fn advanced(
    id: &'static str,
    open: bool,
    rows: Vec<AnyElement>,
    t: &Tokens,
    on_toggle: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    v_flex()
        .child(
            h_flex()
                .id(id)
                .min_h(px(40.))
                .px(px(16.))
                .gap(px(6.))
                .cursor_pointer()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.muted_foreground)
                .hover(|el| el.text_color(t.foreground))
                .on_click(on_toggle)
                .child(Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight }).with_size(px(14.)))
                .child("Advanced"),
        )
        .when(open, |el| {
            el.child(
                group(rows, t)
                    .rounded(px(0.))
                    .border_0()
                    .border_t_1()
                    .border_color(t.border.opacity(0.5))
                    .bg(gpui_kit::transparent_black()),
            )
        })
        .into_any_element()
}

/// `DEFAULT_TEXT_GENERATION_MODEL_BY_PROVIDER`: the cheap generation model per driver.
fn default_text_generation_model(driver: &str) -> Option<&'static str> {
    match driver {
        "codex" => Some("gpt-6-luna"),
        "antigravity" => Some("antigravity-default"),
        "claudeAgent" => Some("claude-haiku-5-5"),
        "cursor" => Some("composer-2"),
        "opencode" => Some("opencode/big-pickle"),
        _ => None,
    }
}

/// `resolveAppModelSelectionState`: the stored text-generation model while its provider can
/// generate text, else the first such provider's cheap default. Options carry over only when
/// the stored provider is kept.
pub(in crate::app) fn resolve_text_generation(
    selection: Option<&ModelSelection>,
    providers: &[Provider],
) -> Option<ModelSelection> {
    let usable = |p: &&Provider| p.is_selectable() && generates_text(p);
    let selected = selection.and_then(|s| providers.iter().filter(usable).find(|p| p.instance_id == s.instance_id));
    let provider = selected.or_else(|| providers.iter().find(usable))?;
    let has = |slug: &&str| provider.models.iter().any(|m| m.slug == *slug);
    let kept = selected.and(selection).map(|s| s.model.as_str()).filter(has);
    let model = kept
        .or_else(|| default_text_generation_model(&provider.driver).filter(has))
        .or_else(|| provider.default_model())?;
    Some(ModelSelection {
        instance_id: provider.instance_id.clone(),
        model: model.to_owned(),
        options: selected.and(selection).and_then(|s| s.options.clone()),
    })
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with(r"\\")
        || (bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/'))
}

fn is_host_path(path: &str) -> bool {
    path.starts_with('/') || path.starts_with("~/") || is_windows_absolute(path)
}

/// A shell-quoted path ending `text`, `'…'` (with `'\''` escapes) or `"…"`, and where it starts.
fn quoted_path_at_end(text: &str) -> Option<(usize, String)> {
    let trimmed = text.trim_end();
    let quote = trimmed.chars().last().filter(|c| *c == '\'' || *c == '"')?;
    let end = trimmed.len() - 1;
    trimmed[..end].match_indices(quote).find_map(|(start, _)| {
        let inner = &trimmed[start + 1..end];
        let valid = !inner.is_empty() && !inner.replace(r"'\''", "").contains(quote);
        let unescaped = if quote == '\'' { inner.replace(r"'\''", "'") } else { inner.to_owned() };
        valid.then_some((start, unescaped))
    })
}

const SHELL_ESCAPED: &str = "'\"\\()[]{}<>|&;$`!*?#~^=";

fn unescape_shell(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match chars.peek() {
            Some(&next) if c == '\\' && (next.is_whitespace() || SHELL_ESCAPED.contains(next)) => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// `resolvePastedPath`: a full path pasted into a folder box replaces it, even after the text
/// already there, and shell quoting or escapes from Finder or Explorer come off.
pub(in crate::app) fn resolve_pasted_path(previous: &str, next: &str) -> String {
    let typing = if next.starts_with(previous) {
        next.chars().count() == previous.chars().count() + 1
    } else {
        previous.starts_with(next)
    };
    if typing {
        return next.to_owned();
    }
    if let Some((start, quoted)) = quoted_path_at_end(next) {
        // Text before the quote that wasn't in the box is a search, not a folder to paste after.
        if previous.starts_with(next[..start].trim_end()) && is_host_path(&quoted) {
            return quoted;
        }
    }
    let after_folder = (previous.ends_with('/') || previous.ends_with('\\')) && next.starts_with(previous);
    let path = if after_folder { &next[previous.len()..] } else { next };
    if !is_host_path(path) {
        return next.to_owned();
    }
    if is_windows_absolute(path) { path.to_owned() } else { unescape_shell(path) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dot_paths_read_and_patch() {
        let settings = json!({"storageCleanup": {"worktreeOnMerge": true}});
        assert_eq!(at(&settings, "storageCleanup.worktreeOnMerge"), &json!(true));
        assert!(at(&settings, "storageCleanup.missing.deeper").is_null());
        assert_eq!(
            patch("storageCleanup.worktreeOnMerge", json!(false)),
            json!({"storageCleanup": {"worktreeOnMerge": false}})
        );
    }

    #[test]
    fn pasted_paths_replace_the_folder_box() {
        assert_eq!(resolve_pasted_path("~/cod", "~/code"), "~/code", "typing is kept");
        assert_eq!(resolve_pasted_path("~/", r"~//Users/me/My\ Repos"), "/Users/me/My Repos");
        assert_eq!(resolve_pasted_path("~/code", "~/code'/a b'"), "/a b");
        assert_eq!(resolve_pasted_path("", r#""C:\Users\me""#), r"C:\Users\me");
        assert_eq!(resolve_pasted_path("", r"'/it'\''s'"), "/it's");
        assert_eq!(resolve_pasted_path("", r"D:\worktrees"), r"D:\worktrees");
        assert_eq!(resolve_pasted_path("ab", "abc def"), "abc def", "a paste that isn't a path is left alone");
    }

    #[test]
    fn text_generation_falls_back_to_a_cheap_default() {
        let providers: Vec<Provider> = serde_json::from_value(json!([
            {"instanceId": "codex", "driver": "codex", "enabled": false, "models": [{"slug": "gpt-6-luna"}]},
            {"instanceId": "replay", "driver": "acpRegistry", "enabled": true, "supportsTextGeneration": false,
             "models": [{"slug": "r"}]},
            {"instanceId": "claude", "driver": "claudeAgent", "enabled": true,
             "models": [{"slug": "claude-opus-5-5"}, {"slug": "claude-haiku-5-5"}]}
        ]))
        .unwrap();
        let effort = vec![json!({"id": "reasoningEffort", "value": "low"})];
        let stored = ModelSelection { instance_id: "codex".into(), model: "gpt-6-luna".into(), options: Some(effort) };
        let resolved = resolve_text_generation(Some(&stored), &providers).unwrap();
        assert_eq!(
            (resolved.instance_id.as_str(), resolved.model.as_str(), resolved.options),
            ("claude", "claude-haiku-5-5", None)
        );
        let kept = ModelSelection { instance_id: "claude".into(), model: "claude-opus-5-5".into(), options: None };
        assert_eq!(resolve_text_generation(Some(&kept), &providers).unwrap(), kept);
    }
}
