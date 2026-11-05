//! Placeholder section and row frames for this file's sections, after `SettingsSection`,
//! `SettingsGroup`, `SettingsRow` and `FoldedSettingsSection` in `settingsLayout.tsx`.
//! The parent swaps these for the shared widgets in `settings/widgets.rs` at merge.

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Div, ElementId, FontWeight, HighlightStyle, InteractiveElement as _, InteractiveText, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, UnderlineStyle, Window,
    div, prelude::FluentBuilder as _, px,
};

use crate::theme::Tokens;

/// A titled section; `grouped` draws its rows in one divided card, else they stack loose.
pub(super) fn section(
    title: impl Into<SharedString>,
    icon: Option<AnyElement>,
    rows: Vec<AnyElement>,
    grouped: bool,
    t: &Tokens,
) -> AnyElement {
    v_flex()
        .gap(px(10.))
        .child(
            h_flex()
                .min_h(px(28.))
                .px_4()
                .gap_2()
                .text_size(px(16.))
                .line_height(px(24.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.foreground)
                .children(icon)
                .child(title.into()),
        )
        .child(if grouped { group(rows, t) } else { v_flex().gap_1().children(rows).into_any_element() })
        .into_any_element()
}

/// The rounded, divided card rows sit in (`SettingsGroup`).
pub(super) fn group(rows: Vec<AnyElement>, t: &Tokens) -> AnyElement {
    let divider = t.border.opacity(0.5);
    v_flex()
        .rounded(px(12.))
        .border_1()
        .border_color(t.border.opacity(0.6))
        .bg(t.card.opacity(0.4))
        .children(
            rows.into_iter()
                .enumerate()
                .map(|(i, row)| div().when(i > 0, |el| el.border_t_1().border_color(divider)).child(row)),
        )
        .into_any_element()
}

/// One row: title, description and status on the left, the control on the right.
pub(super) fn row(
    title: impl IntoElement,
    description: Option<AnyElement>,
    status: Option<SharedString>,
    control: Option<AnyElement>,
    t: &Tokens,
) -> Div {
    h_flex()
        .px_4()
        .py_3()
        .gap_8()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    h_flex()
                        .min_h(px(20.))
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(title),
                )
                .children(description.map(|description| {
                    div()
                        .max_w(px(672.))
                        .text_xs()
                        .line_height(px(18.))
                        .text_color(t.muted_foreground)
                        .child(description)
                }))
                .children(status.map(|status| div().pt(px(2.)).text_xs().text_color(t.muted_foreground).child(status))),
        )
        .children(control.map(|control| h_flex().flex_none().gap_2().justify_end().child(control)))
}

/// A footnote line inside a group (`px-3 py-2.5 text-xs text-muted-foreground`).
pub(super) fn note(text: impl Into<SharedString>, t: &Tokens) -> AnyElement {
    div().px_4().py(px(10.)).text_xs().text_color(t.muted_foreground).child(text.into()).into_any_element()
}

/// A piece of a description: plain text, a link to another settings section, or a URL.
#[derive(Clone, Copy)]
pub(super) enum Part {
    Text(&'static str),
    Section(&'static str, &'static str),
    Url(&'static str, &'static str),
}

/// Description text with its links underlined and clickable. `open_section` runs for
/// [`Part::Section`] links; URLs open in the browser.
pub(super) fn rich_text(
    id: impl Into<ElementId>,
    parts: &'static [Part],
    open_section: impl Fn(&'static str, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let mut text = String::new();
    let mut links = Vec::new();
    for part in parts {
        let (label, target) = match *part {
            Part::Text(label) => (label, None),
            Part::Section(label, section) => (label, Some(Part::Section(label, section))),
            Part::Url(label, url) => (label, Some(Part::Url(label, url))),
        };
        let start = text.len();
        text.push_str(label);
        if let Some(target) = target {
            links.push((start..text.len(), target));
        }
    }
    let underline = HighlightStyle {
        underline: Some(UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
        ..Default::default()
    };
    let styled = StyledText::new(text).with_highlights(links.iter().map(|(range, _)| (range.clone(), underline)));
    let ranges = links.iter().map(|(range, _)| range.clone()).collect();
    InteractiveText::new(id, styled)
        .on_click(ranges, move |index, window, cx| match links[index].1 {
            Part::Section(_, section) => open_section(section, window, cx),
            Part::Url(_, url) => cx.open_url(url),
            Part::Text(_) => {}
        })
        .into_any_element()
}

/// The header of a fold inside a card (`FoldedSettingsSection`, header inside).
pub(super) fn fold_header(
    id: impl Into<ElementId>,
    title: &'static str,
    summary: SharedString,
    open: bool,
    t: &Tokens,
    on_toggle: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    h_flex()
        .id(id)
        .min_h(px(44.))
        .px_4()
        .py_2()
        .gap_2()
        .cursor_pointer()
        .on_click(move |_, window, cx| on_toggle(window, cx))
        .child(
            Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                .size(px(16.))
                .text_color(t.muted_foreground),
        )
        .child(div().flex_none().text_sm().font_weight(FontWeight::MEDIUM).child(title))
        .when(!summary.is_empty(), |el| {
            el.child(div().min_w_0().truncate().text_xs().text_color(t.muted_foreground).child(summary))
        })
        .into_any_element()
}
