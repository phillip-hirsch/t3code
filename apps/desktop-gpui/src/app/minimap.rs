//! The timeline minimap (`TimelineMinimap.tsx`): a tick per user message down the
//! left gutter. Hovering magnifies the ticks near the pointer and previews that
//! turn; clicking scrolls to its message.

use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use super::Workspace;
use crate::theme;
use crate::timeline::Row;

/// The column the minimap owns at the timeline's left edge (`w-18`).
const WIDTH: f32 = 72.;
const PITCH: f32 = 8.;
/// A gutter this wide keeps the minimap on screen; a narrower one shows it on hover.
const PERSISTENT_GUTTER: f32 = 48.;

/// The row index of each user message.
pub(super) fn ticks(rows: &[Row]) -> Vec<usize> {
    rows.iter().enumerate().filter(|(_, row)| matches!(row, Row::User { .. })).map(|(index, _)| index).collect()
}

/// Fisheye width factor of tick `index` with `hovered` under the pointer.
fn scale(index: usize, hovered: Option<usize>) -> f32 {
    match hovered.map(|hovered| index.abs_diff(hovered)) {
        Some(0) => 1.,
        Some(1) => 0.667,
        Some(2) => 0.417,
        _ => 0.333,
    }
}

/// The turn under the viewport's top: the last tick at or above `top_row`.
fn current(ticks: &[usize], top_row: usize) -> Option<usize> {
    ticks.iter().rposition(|row| *row <= top_row)
}

/// The hover preview: the message on one line, then the turn's last reply as plain text.
fn preview(rows: &[Row], tick: usize) -> (SharedString, Option<SharedString>) {
    let collapse = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some(Row::User { text, .. }) = rows.get(tick) else { return ("User message".into(), None) };
    let user = collapse(text);
    let reply = rows[tick + 1..]
        .iter()
        .take_while(|row| !matches!(row, Row::User { .. }))
        .filter_map(|row| if let Row::Assistant { text, .. } = row { Some(text) } else { None })
        .last()
        .map(|reply| crate::timeline::plain_detail(&reply.chars().take(1500).collect::<String>()).into());
    (if user.is_empty() { "User message".into() } else { user.into() }, reply)
}

impl Workspace {
    pub(super) fn render_minimap(&self, gutter: f32, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ticks = ticks(&self.rows);
        if ticks.len() < 2 {
            return None;
        }
        let t = *theme::tokens(cx);
        let last = (ticks.len() - 1) as f32;
        let height = (last * PITCH).min(f32::from(window.viewport_size().height) - 288.).max(PITCH);
        let hovered = self.minimap_hover.filter(|index| *index < ticks.len());
        let current = current(&ticks, self.scroller.read(cx).logical_scroll_top().item_ix);
        let tint = |alpha: f32| Hsla { a: t.muted_foreground.a * alpha, ..t.muted_foreground };
        let mut strip = div().relative().w(px(40.)).h(px(height)).child(
            div().absolute().left_0().top_0().bottom_0().w(px(1.)).bg(Hsla { a: t.border.a * 0.15, ..t.border }),
        );
        for (index, row) in ticks.iter().copied().enumerate() {
            let y = index as f32 / last * height;
            let color = if hovered == Some(index) {
                tint(0.6)
            } else if current == Some(index) {
                Hsla { a: t.foreground.a * 0.55, ..t.foreground }
            } else {
                tint(0.25)
            };
            strip = strip.child(
                div()
                    .id(ElementId::Name(format!("minimap:{row}").into()))
                    .absolute()
                    .left_0()
                    .top(px(y - PITCH / 2.))
                    .w(px(40.))
                    .h(px(PITCH))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(div().h(px(2.)).w(px(24. * scale(index, hovered))).rounded_full().bg(color))
                    .on_hover(cx.listener(move |this, hovering: &bool, _, cx| {
                        let next = if *hovering { Some(index) } else { this.minimap_hover.filter(|h| *h != index) };
                        if this.minimap_hover != next {
                            this.minimap_hover = next;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.scroller.update(cx, |scroller, cx| scroller.scroll_to_item(row, cx));
                    })),
            );
        }
        let card = hovered.map(|index| {
            let (user, reply) = preview(&self.rows, ticks[index]);
            div()
                .absolute()
                .left(px(20.))
                .top(px(index as f32 / last * height - 12.))
                .w(px(320.))
                .p(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                .bg(t.popover)
                .shadow_xl()
                .child(
                    div()
                        .truncate()
                        .text_size(px(14.))
                        .line_height(px(20.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(user),
                )
                .children(reply.map(|reply| {
                    div().mt(px(4.)).line_clamp(3).text_size(px(14.)).text_color(t.muted_foreground).child(reply)
                }))
        });
        Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(WIDTH))
                .flex()
                .items_center()
                .group("minimap")
                .when(gutter < PERSISTENT_GUTTER, |minimap| {
                    minimap.opacity(0.).group_hover("minimap", |style| style.opacity(1.))
                })
                .child(div().relative().ml(px(12.)).child(strip).children(card))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_magnify_near_the_pointer_and_track_the_current_turn() {
        assert_eq!([0, 1, 2, 3, 9].map(|i| scale(i, Some(1))), [0.667, 1., 0.667, 0.417, 0.333]);
        assert_eq!(scale(4, None), 0.333);
        let ticks = [0, 4, 9];
        assert_eq!([0, 3, 4, 20].map(|top| current(&ticks, top)), [Some(0), Some(0), Some(1), Some(2)]);
    }
}
