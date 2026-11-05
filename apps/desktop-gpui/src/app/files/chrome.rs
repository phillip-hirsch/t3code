//! `fileSurfaceChrome.tsx`: the pieces every file viewer shares.

use gpui_kit::assets::IconName;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Div, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::theme::Tokens;

/// `FileSurfaceAction`: a 24px ghost icon button with its label as tooltip.
pub(super) fn action(
    id: impl Into<ElementId>,
    icon: IconName,
    label: impl Into<SharedString>,
    pressed: bool,
    tokens: &Tokens,
) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .cursor_pointer()
        .text_color(if pressed { tokens.foreground } else { tokens.muted_foreground })
        .when(pressed, |el| el.bg(tokens.accent))
        .hover(|s| s.bg(tokens.accent).text_color(tokens.foreground))
        .child(Icon::new(icon).size(px(14.)))
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
}

/// `FileSurfaceLoading`.
pub(super) fn loading(tokens: &Tokens) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .items_center()
        .justify_center()
        .child(Spinner::new().large().color(tokens.muted_foreground))
        .into_any_element()
}

/// `FileSurfaceNotice`.
pub(super) fn notice(text: impl Into<SharedString>, tokens: &Tokens) -> Div {
    div()
        .flex_none()
        .px(px(12.))
        .py(px(6.))
        .border_b_1()
        .border_color(tokens.warning.opacity(0.2))
        .bg(tokens.warning.opacity(0.08))
        .text_size(gpui_kit::rems(0.625))
        .text_color(tokens.warning_foreground)
        .child(text.into())
}

/// `FileSurfaceFailure`; the caller adds `on_click` to the returned retry button, if any.
pub(super) fn failure(message: impl Into<SharedString>, retry: Option<Stateful<Div>>, tokens: &Tokens) -> AnyElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .px(px(24.))
        .text_center()
        .text_xs()
        .line_height(gpui_kit::relative(1.625))
        .child(div().text_color(tokens.error).child(message.into()))
        .children(retry)
        .into_any_element()
}

/// The "Try again" button of a failure.
pub(super) fn retry_button(id: impl Into<ElementId>, tokens: &Tokens) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(10.))
        .py(px(4.))
        .rounded(px(6.))
        .border_1()
        .border_color(tokens.input)
        .text_color(tokens.foreground)
        .cursor_pointer()
        .hover(|s| s.bg(tokens.accent))
        .child("Try again")
}

/// The "No preview for this file" notice of `AttachmentFilePreview` and `FontViewer`.
pub(super) fn no_preview(detail: impl Into<SharedString>, tokens: &Tokens) -> AnyElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .px(px(24.))
        .text_center()
        .child(div().text_sm().font_weight(gpui_kit::FontWeight::MEDIUM).child("No preview for this file"))
        .child(
            div()
                .max_w(px(384.))
                .text_xs()
                .line_height(gpui_kit::relative(1.625))
                .text_color(tokens.muted_foreground)
                .child(detail.into()),
        )
        .into_any_element()
}
