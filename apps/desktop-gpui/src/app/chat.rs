use gpui_kit::component::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, Hsla, IntoElement, ParentElement as _, StyleRefinement, Styled as _, Window,
    div, px,
};

use gpui_kit::component::message_scroller::MessageScroller;
use gpui_kit::component::text::TextView;

use super::{CONTENT_WIDTH, Target, Workspace};
use crate::timeline::{Row, WorkKind};

impl Workspace {
    pub(super) fn render_body(&self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let empty = |text: &'static str| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(text)
                .into_any_element()
        };
        match (&self.target, &self.thread) {
            (None, _) => empty("Pick a thread, or press + next to a project to start one."),
            (Some(Target::Draft { .. }), _) => empty("Describe the task to start a new thread."),
            (Some(Target::Thread(_)), Some(thread)) if thread.state.projection.is_none() => empty("Loading thread…"),
            (Some(Target::Thread(_)), _) => {
                let rows = self.rows.clone();
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        MessageScroller::new("timeline", self.scroller.clone(), move |index, _, cx| {
                            match rows.get(index) {
                                Some(row) => render_row(row, cx),
                                None => div().into_any_element(),
                            }
                        })
                        .with_list_style(StyleRefinement::default().py_4())
                        // Rows pad themselves; the scroller's default 32px gap is too loose.
                        .with_row_style(StyleRefinement::default().pb(px(0.)))
                        .with_bottom_fade(theme.background),
                    )
                    .into_any_element()
            }
        }
    }
}

fn render_row(row: &Row, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let frame = || div().w_full().max_w(px(CONTENT_WIDTH)).mx_auto().px_4().py_1p5();
    match row {
        Row::User { text, .. } => frame()
            .child(h_flex().justify_end().child(
                div().max_w(px(CONTENT_WIDTH * 0.8)).px_3().py_2().rounded_lg().bg(theme.secondary).child(text.clone()),
            ))
            .into_any_element(),
        Row::Assistant { id, text, .. } => frame()
            .child(TextView::markdown(ElementId::Name(id.clone()), text.clone()).selectable(true))
            .into_any_element(),
        Row::Plan { id, markdown } => frame()
            .child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .child(div().text_xs().font_medium().text_color(theme.muted_foreground).child("Proposed plan"))
                    .child(TextView::markdown(ElementId::Name(id.clone()), markdown.clone()).selectable(true)),
            )
            .into_any_element(),
        Row::Work { kind, label, detail, status, .. } => {
            let (icon, color) = work_icon(*kind, status, cx);
            frame()
                .child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(icon).xsmall().text_color(color))
                        .child(div().min_w_0().truncate().child(label.clone()))
                        .children(detail.clone().map(|d| div().flex_none().child(d))),
                )
                .into_any_element()
        }
    }
}

fn work_icon(kind: WorkKind, status: &str, cx: &App) -> (IconName, Hsla) {
    let theme = cx.theme();
    if status == "failed" {
        return (IconName::CircleX, theme.danger);
    }
    let icon = match kind {
        WorkKind::Thinking => IconName::Bot,
        WorkKind::Command => IconName::SquareTerminal,
        WorkKind::Edit => IconName::FileText,
        WorkKind::Search => IconName::Search,
        WorkKind::Tool => IconName::Settings2,
        WorkKind::Approval => IconName::TriangleAlert,
        WorkKind::Question => IconName::Info,
        WorkKind::Error => return (IconName::CircleAlert, theme.danger),
        WorkKind::Notice => IconName::Info,
        WorkKind::Agent => IconName::Bot,
    };
    (icon, theme.muted_foreground)
}
