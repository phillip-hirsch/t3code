//! Stand-ins for the shared settings widgets the Chat, Diffs, Keybindings and SnapShots
//! sections use until `widgets.rs` lands: `SettingsPageContainer`, `SettingsSection`,
//! `SettingsRow`, `SettingResetButton` and `AdvancedSettings` from
//! `apps/web/src/components/settings/{settingsLayout,AdvancedSettings}.tsx`, plus a renderer
//! for declarative client-setting rows ([`SettingRowSpec`]).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, div, px,
};
use serde_json::{Map, Value};

use crate::app::Workspace;
use crate::theme::{self, Tokens};

/// One client-setting row, as the web's `SettingsRow` + `searchableSetting(id)` declare it.
pub(super) struct SettingRowSpec {
    /// The `searchableSetting` id.
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    /// The `ClientSettings` key.
    pub path: &'static str,
    pub control: Control,
    /// Scopes the web offers. Client settings are device-local, so these rows have none.
    #[expect(dead_code, reason = "read when the rows move onto the shared scoped widgets")]
    pub scopes: &'static [&'static str],
}

pub(super) enum Control {
    Switch,
    Select(&'static [Choice]),
}

pub(super) struct Choice {
    pub value: Literal,
    /// The menu item; `{mod}` reads as the platform's modifier key.
    pub label: &'static str,
    /// The trigger's text when it differs from the menu item's.
    pub short: Option<&'static str>,
}

#[derive(Clone, Copy)]
pub(super) enum Literal {
    Bool(bool),
    Str(&'static str),
}

impl Literal {
    fn json(self) -> Value {
        match self {
            Literal::Bool(value) => Value::Bool(value),
            Literal::Str(value) => Value::String(value.into()),
        }
    }
}

pub(super) const MOD_LABEL: &str = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };

fn with_mod(label: &'static str) -> SharedString {
    if label.contains("{mod}") { label.replace("{mod}", MOD_LABEL).into() } else { SharedString::new_static(label) }
}

/// `SettingsPageContainer` (`max-w-4xl px-6 pt-6 pb-12 gap-8`), scrolling on its own.
pub(super) fn page(id: &'static str, children: impl IntoIterator<Item = AnyElement>) -> Stateful<Div> {
    div().id(id).size_full().overflow_y_scroll().child(
        v_flex().mx_auto().w_full().max_w(px(896.)).px(px(24.)).pt(px(24.)).pb(px(48.)).gap(px(32.)).children(children),
    )
}

/// `SettingsSection`: a 16px semibold title over a grouped card.
pub(super) fn section(
    title: Option<&'static str>,
    header_action: Option<AnyElement>,
    rows: impl IntoIterator<Item = AnyElement>,
    t: &Tokens,
) -> Div {
    v_flex()
        .gap(px(10.))
        .when_some(title, |el, title| {
            el.child(
                h_flex()
                    .min_h(px(28.))
                    .px(px(16.))
                    .justify_between()
                    .gap(px(16.))
                    .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(title))
                    .children(header_action),
            )
        })
        .child(group(rows, t))
}

/// `SettingsGroup variant="grouped"`: a rounded card with a divider between children.
pub(super) fn group(rows: impl IntoIterator<Item = AnyElement>, t: &Tokens) -> Div {
    group_rows(rows, t).rounded(px(12.)).border_1().border_color(t.border.opacity(0.6)).bg(t.card.opacity(0.4))
}

/// `SettingsRow`: title (with an optional reset button), description and status on the
/// left, the control on the right.
pub(super) struct Row {
    pub title: AnyElement,
    pub description: Option<AnyElement>,
    pub status: Option<SharedString>,
    pub reset: Option<AnyElement>,
    pub control: Option<AnyElement>,
    /// `SettingsUnavailableGroup` dims titles and descriptions to 64%.
    pub dimmed: bool,
}

impl Row {
    pub(super) fn new(title: impl IntoElement) -> Self {
        Self {
            title: title.into_any_element(),
            description: None,
            status: None,
            reset: None,
            control: None,
            dimmed: false,
        }
    }

    pub(super) fn render(self, t: &Tokens) -> Div {
        let text_opacity = if self.dimmed { 0.64 } else { 1. };
        h_flex()
            .px(px(16.))
            .py(px(12.))
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
                                    .text_color(t.foreground.opacity(text_opacity))
                                    .child(self.title),
                            )
                            .child(div().size(px(20.)).flex().items_center().justify_center().children(self.reset)),
                    )
                    .children(self.description.map(|description| {
                        div()
                            .max_w(px(672.))
                            .text_xs()
                            .line_height(px(18.))
                            .text_color(t.muted_foreground.opacity(text_opacity))
                            .child(description)
                    }))
                    .children(
                        self.status
                            .map(|status| div().pt(px(2.)).text_xs().text_color(t.muted_foreground).child(status)),
                    ),
            )
            .children(self.control.map(|control| h_flex().flex_none().gap(px(8.)).justify_end().child(control)))
    }
}

/// `SettingResetButton`: an icon-micro ghost button with an Undo icon.
pub(super) fn reset_button(id: impl Into<ElementId>, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .size(px(20.))
        .text_color(t.muted_foreground)
        .icon(Icon::new(IconName::Undo2).size(px(12.)))
        .tooltip("Reset to default")
}

/// A `SelectTrigger size="sm"`: a bordered 28px button with a chevron.
pub(super) fn select_trigger(id: impl Into<ElementId>, t: &Tokens) -> Button {
    Button::new(id)
        .outline()
        .small()
        .h(px(28.))
        .px(px(10.))
        .gap(px(8.))
        .text_sm()
        .font_weight(FontWeight::NORMAL)
        .text_color(t.foreground)
}

pub(super) fn chevron(t: &Tokens) -> Icon {
    Icon::new(IconName::ChevronDown).size(px(16.)).text_color(t.muted_foreground.opacity(0.7))
}

impl Workspace {
    /// A declarative row bound to its client setting, with the web's reset button.
    pub(super) fn render_client_row(
        &self,
        spec: &'static SettingRowSpec,
        description: SharedString,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme::tokens(cx);
        let value = self.client_settings.values.get(spec.path).clone();
        let mut row = Row::new(spec.label);
        row.description = Some(description.into_any_element());
        row.dimmed = disabled;
        let default = crate::settings::contract().default_value(spec.path).clone();
        if !disabled && value != default {
            row.reset = Some(
                reset_button(SharedString::from(format!("reset-{}", spec.id)), t)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_client_setting(spec.path, default.clone(), cx)),
                    )
                    .into_any_element(),
            );
        }
        row.control = Some(match &spec.control {
            Control::Switch => Switch::new(SharedString::from(format!("switch-{}", spec.id)))
                .checked(value.as_bool().unwrap_or(false))
                .disabled(disabled)
                .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                    this.set_client_setting(spec.path, Value::Bool(*checked), cx)
                }))
                .into_any_element(),
            Control::Select(choices) => {
                let selected = choices.iter().find(|choice| choice.value.json() == value);
                let label = selected.map(|choice| with_mod(choice.short.unwrap_or(choice.label))).unwrap_or_default();
                let view = cx.entity();
                select_trigger(SharedString::from(format!("select-{}", spec.id)), t)
                    .disabled(disabled)
                    .when(spec.path == "diffColorScheme", |el| el.child(diff_color_dots(&value, t)))
                    .child(label)
                    .child(chevron(t))
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                        for choice in choices.iter() {
                            let picked = choice.value.json();
                            menu = menu.item(
                                PopupMenuItem::new(with_mod(choice.label)).checked(picked == value).on_click(
                                    window.listener_for(&view, move |this, _, _, cx| {
                                        this.set_client_setting(spec.path, picked.clone(), cx)
                                    }),
                                ),
                            );
                        }
                        menu
                    })
                    .into_any_element()
            }
        });
        row.render(t).into_any_element()
    }

    pub(super) fn set_client_setting(&mut self, key: &'static str, value: Value, cx: &mut Context<Self>) {
        self.client_settings.apply(&Map::from_iter([(key.to_owned(), value)]));
        cx.notify();
    }

    /// `AdvancedSettings`: a collapsed "Advanced" fold at the end of a section's card.
    pub(super) fn render_advanced_fold(
        &self,
        section: &'static str,
        rows: Vec<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = theme::tokens(cx);
        let open = self.settings_folds.contains(section);
        v_flex()
            .child(
                h_flex()
                    .id(SharedString::from(format!("advanced-{section}")))
                    .min_h(px(40.))
                    .px(px(16.))
                    .gap(px(6.))
                    .cursor_pointer()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted_foreground)
                    .hover(|style| style.text_color(t.foreground))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).size(px(14.)))
                    .child("Advanced")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.settings_folds.remove(section) {
                            this.settings_folds.insert(section);
                        }
                        cx.notify();
                    })),
            )
            .when(open, |el| {
                el.child(div().border_t_1().border_color(t.border.opacity(0.5)).child(group_rows(rows, t)))
            })
            .into_any_element()
    }
}

/// Rows divided like a group's, without the card around them.
fn group_rows(rows: impl IntoIterator<Item = AnyElement>, t: &Tokens) -> Div {
    v_flex().children(
        rows.into_iter().enumerate().map(|(index, row)| {
            div().when(index > 0, |el| el.border_t_1().border_color(t.border.opacity(0.5))).child(row)
        }),
    )
}

/// The trigger's deletion/addition dots; `blue-orange` reverses them.
fn diff_color_dots(scheme: &Value, t: &Tokens) -> Div {
    let blue_orange = scheme.as_str() == Some("blue-orange");
    let (deletion, addition) = if blue_orange {
        let (orange, blue) = if t.dark { (0xff8904, 0x51a2ff) } else { (0xf54900, 0x155dfc) };
        (gpui_kit::rgb(orange).into(), gpui_kit::rgb(blue).into())
    } else {
        (t.error, gpui_kit::rgb(0x00bc7d).into())
    };
    let dot = |color: gpui_kit::Hsla| div().size(px(8.)).rounded_full().bg(color);
    h_flex().flex_none().gap(px(4.)).map(|el| {
        if blue_orange {
            el.child(dot(addition)).child(dot(deletion))
        } else {
            el.child(dot(deletion)).child(dot(addition))
        }
    })
}
