//! Placeholder section/row chrome (`SettingsSection`, `SettingsGroup`, `SettingsRow` in
//! `apps/web/src/components/settings`). At merge these become the shared settings widgets.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{AnyElement, ElementId, FontWeight, IntoElement, ParentElement as _, Styled as _, div, px};

use crate::theme::Tokens;

/// A titled card whose rows are divided by hairlines.
pub(super) fn section(title: &'static str, rows: Vec<AnyElement>, t: &Tokens) -> AnyElement {
    v_flex()
        .gap(px(10.))
        .child(
            div()
                .min_h(px(28.))
                .px(px(16.))
                .flex()
                .items_center()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.foreground)
                .child(title),
        )
        .child(
            v_flex().rounded(px(12.)).border_1().border_color(t.border.opacity(0.6)).bg(t.card.opacity(0.4)).children(
                rows.into_iter().enumerate().map(|(index, row)| {
                    div().when(index > 0, |d| d.border_t_1().border_color(t.border.opacity(0.5))).child(row)
                }),
            ),
        )
        .into_any_element()
}

/// Title and description on the left, the control on the right.
pub(super) fn row(
    title: impl IntoElement,
    description: Option<AnyElement>,
    control: Option<AnyElement>,
    t: &Tokens,
) -> gpui_kit::Div {
    h_flex()
        .px(px(16.))
        .py(px(12.))
        .gap(px(32.))
        .justify_between()
        .child(
            v_flex()
                .min_w_0()
                .flex_1()
                .gap(px(4.))
                .child(
                    div().min_h(px(20.)).flex().items_center().text_sm().font_weight(FontWeight::MEDIUM).child(title),
                )
                .children(description.map(|d| {
                    div().max_w(px(672.)).text_xs().line_height(px(18.)).text_color(t.muted_foreground).child(d)
                })),
        )
        .children(control.map(|c| h_flex().flex_none().gap(px(8.)).justify_end().child(c)))
}

/// `SettingResetButton`: an icon-only undo beside the control.
pub(super) fn reset_button(id: impl Into<ElementId>, label: &str) -> Button {
    Button::new(id).ghost().xsmall().icon(Lucide::Undo2).tooltip(format!("Reset {label}"))
}
