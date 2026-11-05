//! Private stand-ins for the web's settings layout (`settingsLayout.tsx`,
//! `FoldedSettingsSection.tsx`, `itemRows.ts`) and `ui/button.tsx`, until the
//! shared settings widgets land.

use std::rc::Rc;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window, div, px, rgb, svg,
};

use crate::theme::Tokens;

pub(super) fn text_xs<E: Styled>(el: E) -> E {
    el.text_size(px(12.)).line_height(px(16.))
}

pub(super) fn text_sm<E: Styled>(el: E) -> E {
    el.text_size(px(14.)).line_height(px(20.))
}

pub(super) fn text_2xs<E: Styled>(el: E) -> E {
    el.text_size(px(11.)).line_height(px(16.))
}

/// `--success` (emerald-500); the theme carries only its foreground shade.
pub(super) fn success() -> Hsla {
    rgb(0x00bc7d).into()
}

pub(super) fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView + 'static {
    let text = text.into();
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// `SettingsPageContainer`: the scrolling page, 896px wide at most, sections 32px apart.
pub(super) fn page(id: &'static str, children: impl IntoIterator<Item = AnyElement>) -> AnyElement {
    div()
        .id(id)
        .size_full()
        .overflow_y_scroll()
        .child(
            v_flex()
                .mx_auto()
                .w_full()
                .max_w(px(896.))
                .px(px(24.))
                .pt(px(24.))
                .pb(px(48.))
                .gap(px(32.))
                .children(children),
        )
        .into_any_element()
}

/// `SettingsGroup`: the rounded card, rows divided by hairlines.
pub(super) fn group(t: &Tokens) -> Div {
    v_flex()
        .rounded(px(12.))
        .border_1()
        .border_color(t.border.opacity(0.6))
        .bg(t.card.opacity(0.4))
        .text_color(t.foreground)
}

/// Adds the group's top hairline to every child after the first.
pub(super) fn divided(children: Vec<AnyElement>, t: &Tokens) -> Vec<AnyElement> {
    children
        .into_iter()
        .enumerate()
        .map(|(index, child)| {
            if index == 0 {
                child
            } else {
                div().border_t_1().border_color(t.border.opacity(0.5)).child(child).into_any_element()
            }
        })
        .collect()
}

/// `SettingsSection`: a header with an optional icon and action over a group card.
pub(super) fn section(
    title: impl Into<SharedString>,
    icon: Option<AnyElement>,
    action: Option<AnyElement>,
    rows: Vec<AnyElement>,
    t: &Tokens,
) -> AnyElement {
    v_flex()
        .gap(px(10.))
        .child(
            h_flex()
                .min_h(px(28.))
                .px(px(16.))
                .gap(px(16.))
                .justify_between()
                .child(
                    h_flex()
                        .min_h(px(28.))
                        .gap(px(8.))
                        .text_size(px(16.))
                        .line_height(px(24.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.foreground)
                        .children(icon)
                        .child(title.into()),
                )
                .child(h_flex().min_h(px(28.)).min_w(px(28.)).justify_end().children(action)),
        )
        .child(group(t).children(divided(rows, t)))
        .into_any_element()
}

/// `SettingsRow`: title (with the reset slot), description and status on the left, control on the right.
pub(super) fn row(
    title: impl IntoElement,
    description: Option<AnyElement>,
    status: Option<AnyElement>,
    control: Option<AnyElement>,
    t: &Tokens,
) -> Div {
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
                        .child(text_sm(div().font_weight(FontWeight::MEDIUM).text_color(t.foreground)).child(title))
                        .child(div().size(px(20.)).flex_none()),
                )
                .when_some(description, |el, description| {
                    el.child(text_xs(div().max_w(px(672.)).text_color(t.muted_foreground)).child(description))
                })
                .when_some(status, |el, status| {
                    el.child(text_xs(div().pt(px(2.)).text_color(t.muted_foreground)).child(status))
                }),
        )
        .when_some(control, |el, control| el.child(h_flex().flex_none().gap(px(8.)).justify_end().child(control)))
}

/// `ITEM_ROW_CLASSNAME` + `ITEM_ROW_INNER_CLASSNAME`: details left, actions right.
pub(super) fn item_row(details: impl IntoElement, actions: Option<AnyElement>) -> Div {
    div().px(px(16.)).py(px(12.)).child(
        h_flex()
            .gap(px(12.))
            .justify_between()
            .child(v_flex().flex_1().min_w_0().gap(px(4.)).child(details))
            .when_some(actions, |el, actions| el.child(h_flex().flex_none().gap(px(8.)).child(actions))),
    )
}

/// `ConnectionStatusDot`, without the ping animation (no continuous repaints).
pub(super) fn status_dot(id: impl Into<ElementId>, color: Hsla, tip: Option<SharedString>) -> AnyElement {
    let dot = div().size(px(8.)).rounded_full().bg(color);
    let frame = div().id(id.into()).size(px(12.)).flex_none().flex().items_center().justify_center().child(dot);
    match tip {
        Some(tip) => frame.tooltip(tooltip(tip)).into_any_element(),
        None => frame.into_any_element(),
    }
}

/// A chevron that points right when folded and down when open.
pub(super) fn chevron(open: bool, size: f32, color: Hsla) -> AnyElement {
    let name = if open { gpui_kit::assets::IconName::ChevronDown } else { gpui_kit::assets::IconName::ChevronRight };
    Icon::new(name).size(px(size)).flex_none().text_color(color).into_any_element()
}

pub(super) fn glyph(path: &'static str, size: f32, color: Hsla) -> AnyElement {
    svg().path(path).size(px(size)).flex_none().text_color(color).into_any_element()
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Variant {
    Primary,
    Outline,
    DestructiveOutline,
    Ghost,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Size {
    /// `size="xs"`: 24px tall, text-xs.
    Xs,
    /// The default 32px button, used in dialog footers.
    Default,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// `Button` from `ui/button.tsx` for the variants and sizes these sections use.
#[derive(IntoElement)]
pub(super) struct WebButton {
    id: ElementId,
    label: SharedString,
    icon: Option<gpui_kit::assets::IconName>,
    variant: Variant,
    size: Size,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl WebButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            variant: Variant::Outline,
            size: Size::Xs,
            disabled: false,
            on_click: None,
        }
    }

    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn icon(mut self, icon: gpui_kit::assets::IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for WebButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = *crate::theme::tokens(cx);
        let outline_bg = if t.dark { t.input.opacity(0.32) } else { t.popover };
        let (bg, border, text, hover_bg, hover_border) = match self.variant {
            Variant::Primary => (t.primary, t.primary, t.primary_foreground, t.primary_hover, t.primary),
            Variant::Outline => {
                (outline_bg, t.input, t.foreground, if t.dark { t.input.opacity(0.64) } else { t.accent }, t.input)
            }
            Variant::DestructiveOutline => {
                (outline_bg, t.input, t.error_foreground, t.error.opacity(0.04), t.error.opacity(0.32))
            }
            Variant::Ghost => (
                gpui_kit::transparent_black(),
                gpui_kit::transparent_black(),
                t.foreground,
                t.accent,
                gpui_kit::transparent_black(),
            ),
        };
        let (height, pad, gap, icon_size) = match self.size {
            Size::Xs => (24., 7., 4., 14.),
            Size::Default => (32., 11., 8., 16.),
        };
        let base = h_flex()
            .id(self.id)
            .flex_none()
            .h(px(height))
            .px(px(pad))
            .gap(px(gap))
            .justify_center()
            .rounded(px(8.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(text)
            .font_weight(FontWeight::MEDIUM)
            .whitespace_nowrap();
        let base = match self.size {
            Size::Xs => text_xs(base),
            Size::Default => text_sm(base),
        };
        base.when_some(self.icon, |el, icon| el.child(Icon::new(icon).size(px(icon_size)))).child(self.label).map(
            |el| match (self.disabled, self.on_click) {
                (true, _) | (_, None) => el.opacity(0.64),
                (false, Some(handler)) => el
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover_bg).border_color(hover_border))
                    .on_click(move |event, window, cx| handler(event, window, cx)),
            },
        )
    }
}
