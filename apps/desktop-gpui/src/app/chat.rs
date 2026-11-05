//! The chat timeline: the virtualized message list and its rows. Geometry and
//! colors follow `apps/web/src/components/chat/MessagesTimeline.tsx`, `WorkLog.tsx`
//! and `index.css` (`.chat-markdown`).

use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::message_scroller::MessageScroller;
use gpui_kit::component::text::{TextView, TextViewStyle};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, ElementId, FontWeight, HighlightStyle, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, WeakEntity, Window, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, relative, rems,
};

use gpui_kit::assets::IconName;

use super::layout::Lane;
use super::{Target, Workspace, project_name};
use crate::theme::{self, FONT_MONO, Tokens};
use crate::timeline::{self, Row, Steer, WorkRow, WorkRowKind};
use crate::work_log::{self, Activity, Entry, Status, WorkIcon};

/// Prose: `text-sm` at `leading-relaxed`.
const TEXT: f32 = 14.;
const LINE: f32 = 22.75;
/// A user message longer than this collapses (`shouldCollapseUserMessage`).
const COLLAPSE_CHARS: usize = 600;
const COLLAPSE_LINES: usize = 8;

impl Workspace {
    pub(super) fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let (lane, _) = self.chat_layout(window);
        match (&self.target, &self.thread) {
            (None, _) => no_thread(&t),
            (Some(Target::Draft { project_id }), _) => {
                let project = self.shell.projects.iter().find(|p| &p.id == project_id).map(project_name);
                draft_hero(project, lane, &t)
            }
            (Some(Target::Thread(_)), Some(thread)) if thread.state.projection.is_none() => {
                centered_note("Loading thread…", &t)
            }
            (Some(Target::Thread(_)), _) if self.rows.is_empty() => {
                centered_note("Send a message to start the conversation.", &t)
            }
            (Some(Target::Thread(_)), _) => {
                let rows = self.rows.clone();
                let this = cx.weak_entity();
                let background = t.background;
                div()
                    .flex_1()
                    .min_h_0()
                    .child(
                        MessageScroller::new("timeline", self.scroller.clone(), move |index, window, cx| {
                            match rows.get(index) {
                                Some(row) => render_row(row, &this, window, cx),
                                None => div().into_any_element(),
                            }
                        })
                        // Top spacer `h-4`; rows carry their own bottom space.
                        // The list's side padding becomes every row's inset: that is the lane.
                        .with_list_style(
                            StyleRefinement::default().pt(px(16.)).pb(px(16.)).pl(px(lane.left)).pr(px(lane.right)),
                        )
                        .with_row_style(StyleRefinement::default().pb(px(0.)))
                        .with_jump_button_label("Scroll to end")
                        .with_jump_button_renderer(move |button| scroll_to_end_button(button, &t))
                        .with_bottom_fade(background),
                    )
                    .into_any_element()
            }
        }
    }

    /// Opens or closes a work group, turn fold or entry.
    fn toggle_expanded(&mut self, key: SharedString, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        self.refresh_rows(cx);
        cx.notify();
    }

    /// Keeps a once-a-second repaint alive while a "Working for …" row shows.
    pub(super) fn sync_working_tick(&mut self, rows: &[Row], cx: &mut Context<Self>) {
        let working = rows.iter().any(|row| matches!(row, Row::Working { started_at: Some(_) }));
        if !working {
            self._working_tick = None;
        } else if self._working_tick.is_none() {
            self._working_tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
    }
}

fn scroll_to_end_button(
    button: gpui_kit::component::button::Button,
    t: &Tokens,
) -> gpui_kit::component::button::Button {
    use gpui_kit::component::{IconName as ComponentIcon, Sizable as _};
    button
        .xsmall()
        .icon(Icon::new(ComponentIcon::ChevronDown).size(px(14.)))
        .label("Scroll to end")
        .h(px(24.))
        .px(px(7.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
        .bg(Hsla { a: 0.8, ..t.background })
        .text_color(t.foreground)
}

fn no_thread(t: &Tokens) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p(px(48.))
        .child(
            v_flex()
                .w(px(512.))
                .max_w_full()
                .px(px(48.))
                .py(px(32.))
                .items_center()
                .child(
                    div()
                        .text_size(px(20.))
                        .line_height(px(28.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.foreground)
                        .child("Pick a thread to continue"),
                )
                .child(
                    div()
                        .mt(px(4.))
                        .text_size(px(TEXT))
                        .line_height(px(20.))
                        .text_color(t.muted_foreground)
                        .child("Select an existing thread or create a new one to get started."),
                ),
        )
        .into_any_element()
}

/// "What should we build in **project**?" above the composer.
fn draft_hero(project: Option<SharedString>, lane: Lane, t: &Tokens) -> AnyElement {
    let headline = match project {
        Some(project) => h_flex()
            .justify_center()
            .flex_wrap()
            .child("What should we build in\u{a0}")
            .child(div().text_color(Hsla { a: 0.6, ..t.muted_foreground }).child(project))
            .child("?"),
        None => h_flex().justify_center().child("What should we work on?"),
    };
    div()
        .flex_1()
        .flex()
        .flex_col()
        .justify_end()
        .pl(px(lane.left))
        .pr(px(lane.right))
        .pb(px(16.))
        .child(div().w_full().text_size(px(30.)).line_height(px(36.)).text_color(t.foreground).child(headline))
        .into_any_element()
}

fn centered_note(text: &'static str, t: &Tokens) -> AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(TEXT))
        .line_height(px(20.))
        .text_color(t.muted_foreground)
        .child(text)
        .into_any_element()
}

/// A row in the lane (the scroller insets rows to it); owns its bottom space.
fn lane(bottom: f32, child: impl IntoElement) -> gpui_kit::Div {
    div().w_full().pb(px(bottom)).child(child)
}

fn alpha(color: Hsla, factor: f32) -> Hsla {
    Hsla { a: color.a * factor, ..color }
}

fn toggle(this: &WeakEntity<Workspace>, key: SharedString) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let this = this.clone();
    move |_, _, cx| {
        let _ = this.update(cx, |workspace, cx| workspace.toggle_expanded(key.clone(), cx));
    }
}

fn render_row(row: &Row, this: &WeakEntity<Workspace>, _: &mut Window, cx: &mut App) -> AnyElement {
    let t = *theme::tokens(cx);
    match row {
        Row::User { id, text, steer, grouped_with_previous, grouped_with_next, continues_steer } => {
            let expanded =
                this.upgrade().is_some_and(|workspace| workspace.read(cx).expanded.contains(&timeline::entry_key(id)));
            user_row(
                id,
                text,
                steer.filter(|_| !*continues_steer),
                *grouped_with_previous,
                *grouped_with_next,
                expanded,
                this,
                &t,
            )
        }
        Row::Assistant { id, text, streaming, terminal } => {
            let body = div()
                .px(px(4.))
                .py(px(2.))
                .text_size(px(TEXT))
                .line_height(px(LINE))
                .text_color(alpha(t.foreground, 0.9))
                .child(markdown(id, text, *streaming, &t));
            // The finished reply's copy row (`AssistantMessageMeta`) holds its space while hidden.
            let meta = terminal.then(|| {
                h_flex()
                    .mt(px(6.))
                    .gap(px(8.))
                    .opacity(0.)
                    .group_hover("timeline-row", |style| style.opacity(1.))
                    .child(copy_button(format!("copy:{id}"), text.clone(), &t))
            });
            lane(if *terminal { 16. } else { 8. }, v_flex().group("timeline-row").child(body).children(meta))
                .into_any_element()
        }
        Row::Plan { id, markdown: text } => plan_card(id, text, this, cx, &t),
        Row::Work(work) => work_row(work, this, &t),
        Row::Working { started_at } => {
            let label = match started_at.as_deref().and_then(work_log::parse_iso_ms) {
                Some(start) => format!("Working for {}", working_duration(now_ms() - start)),
                None => "Working...".into(),
            };
            lane(6., rule_row(&t).child(h_flex().h(px(24.)).px(px(4.)).text_color(t.muted_foreground).child(label)))
                .into_any_element()
        }
        Row::TurnFold { key, label, expanded } => {
            let chevron = if *expanded { IconName::ChevronDown } else { IconName::ChevronRight };
            let button = h_flex()
                .id(ElementId::Name(key.clone()))
                .h(px(24.))
                .px(px(4.))
                .gap(px(4.))
                .cursor_pointer()
                .text_color(t.muted_foreground)
                .hover(|style| style.text_color(t.foreground))
                .child(label.clone())
                .child(Icon::new(chevron).size(px(14.)))
                .on_click(toggle(this, key.clone()));
            lane(6., rule_row(&t).child(button)).into_any_element()
        }
        Row::Divider { label, failed, .. } => {
            let color = if *failed { t.error } else { t.muted_foreground };
            let line_color = if *failed { alpha(t.error, 0.3) } else { alpha(t.border, 0.7) };
            let line = || div().flex_1().h(px(1.)).bg(line_color);
            let content = h_flex()
                .gap(px(12.))
                .py(px(6.))
                .text_xs()
                .text_color(color)
                .child(line())
                .child(h_flex().gap(px(6.)).child(Icon::new(IconName::Minimize2).size(px(12.))).child(label.clone()))
                .child(line());
            lane(16., content).into_any_element()
        }
        Row::Event { label, .. } => {
            let line = work_line(WorkIcon::Bot, icon_color(&t), label.clone(), t.muted_foreground, None);
            lane(8., div().px(px(2.)).py(px(2.)).child(line)).into_any_element()
        }
    }
}

/// The working row and turn folds: a hairline under a 24px label line.
fn rule_row(t: &Tokens) -> gpui_kit::Div {
    div()
        .pt(px(4.))
        .pb(px(8.))
        .border_b_1()
        .border_color(alpha(t.border, 0.6))
        .text_size(px(TEXT))
        .line_height(px(LINE))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// `formatWorkingTimer`: whole seconds, then `formatDuration`.
fn working_duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds < 60 { format!("{seconds}s") } else { work_log::format_duration(seconds * 1000) }
}

// ---------------------------------------------------------------------------
// Messages

fn user_row(
    id: &SharedString,
    text: &SharedString,
    steer: Option<Steer>,
    grouped_with_previous: bool,
    grouped_with_next: bool,
    expanded: bool,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let collapsible = text.chars().count() > COLLAPSE_CHARS || text.lines().count() > COLLAPSE_LINES;
    let collapsed = collapsible && !expanded;
    let mut bubble = div()
        .relative()
        .max_w(relative(0.8))
        .p(px(12.))
        .rounded(px(16.))
        .bg(t.message_surface)
        .text_color(t.foreground)
        .text_size(px(TEXT))
        .line_height(px(LINE))
        .when(grouped_with_previous, |this| this.rounded_tr(px(6.)))
        .when(grouped_with_next, |this| this.rounded_br(px(6.)))
        .child(div().when(collapsed, |this| this.max_h(px(176.)).overflow_hidden()).child(text.clone()));
    if collapsed {
        bubble = bubble.child(div().absolute().left_0().right_0().bottom(px(12.)).h(px(28.)).bg(linear_gradient(
            180.,
            linear_color_stop(alpha(t.message_surface, 0.), 0.),
            linear_color_stop(t.message_surface, 1.),
        )));
    }
    let steer_label = steer.map(|_| {
        h_flex()
            .mr(px(4.))
            .gap(px(4.))
            .text_xs()
            .line_height(px(12.))
            .text_color(t.muted_foreground)
            .child(Icon::new(IconName::Redo2).size(px(12.)))
            .child("Steer")
    });
    let toggle_button = collapsible.then(|| {
        h_flex()
            .id(ElementId::Name(format!("user-collapse:{id}").into()))
            .mt(px(6.))
            .h(px(24.))
            .px(px(7.))
            .rounded(px(8.))
            .cursor_pointer()
            .text_xs()
            .text_color(t.muted_foreground)
            .hover(|style| style.bg(t.accent).text_color(t.foreground))
            .child(if expanded { "Show less" } else { "Show full message" })
            .on_click(toggle(this, timeline::entry_key(id)))
    });
    // Hover actions under the bubble keep their 24px row while hidden.
    let actions = h_flex()
        .w_full()
        .max_w(relative(0.8))
        .justify_end()
        .pr(px(4.))
        .opacity(0.)
        .group_hover("timeline-row", |style| style.opacity(1.))
        .child(copy_button(format!("copy:{id}"), text.clone(), t));
    let column = v_flex()
        .group("timeline-row")
        .items_end()
        .gap(px(4.))
        .children(steer_label)
        .child(bubble)
        .children(toggle_button)
        .child(actions);
    lane(if grouped_with_next { 4. } else { 16. }, column).into_any_element()
}

/// A ghost-muted `icon-xs` button that copies `text`.
fn copy_button(id: String, text: SharedString, t: &Tokens) -> impl IntoElement {
    div()
        .id(ElementId::Name(id.into()))
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .text_color(t.muted_foreground)
        .hover(|style| style.bg(t.accent).text_color(t.foreground))
        .child(Icon::new(IconName::Copy).size(px(12.)))
        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.to_string())))
}

fn markdown(id: &SharedString, text: &SharedString, streaming: bool, t: &Tokens) -> TextView {
    let inline_code = StyleRefinement::default()
        .bg(alpha(t.foreground, 0.1))
        .rounded(px(4.))
        .px(px(4.9))
        .py(px(1.4))
        .text_size(px(11.9));
    let label_color = alpha(t.foreground, 0.72);
    let button_hover = t.accent;
    TextView::markdown(ElementId::Name(id.clone()), text.clone())
        .style(markdown_style(t))
        .inline_code_style(inline_code)
        .selectable(true)
        .stream_fade(streaming)
        .code_block_actions(move |block, _, _| {
            let code = block.code();
            h_flex()
                .gap(px(6.))
                .children(
                    block.lang().filter(|lang| !lang.is_empty()).map(|lang| {
                        div().font_family(FONT_MONO).text_size(px(11.)).text_color(label_color).child(lang)
                    }),
                )
                .child(
                    div()
                        .id("copy")
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .cursor_pointer()
                        .text_color(label_color)
                        .hover(|style| style.bg(button_hover))
                        .child(Icon::new(IconName::Copy).size(px(12.)))
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()))),
                )
        })
}

/// `.chat-markdown` (`index.css:1866-2280`) and `MarkdownCodeBlock.tsx`.
fn markdown_style(t: &Tokens) -> TextViewStyle {
    let (code_background, code_border) = if t.dark {
        (alpha(t.input, 0.32), gpui_kit::transparent_black())
    } else {
        (t.secondary, alpha(t.border, 0.7))
    };
    // The web's header row (6px + 24px toolbar) sits above the code's own 12.8px.
    let code_block = StyleRefinement::default()
        .pt(px(42.8))
        .pb(px(12.8))
        .px(px(14.4))
        .rounded(px(8.))
        .bg(code_background)
        .border_color(code_border)
        .text_size(px(13.))
        .line_height(relative(1.375));
    let mut style = TextViewStyle::default()
        .paragraph_gap(rems(0.65))
        .heading_font_size(|level, _| {
            px(match level {
                1 => 20.,
                2 => 18.,
                3 => 16.,
                _ => 14.,
            })
        })
        .code_block(code_block)
        .inline_code(HighlightStyle { background_color: Some(alpha(t.foreground, 0.1)), ..Default::default() })
        // The web draws only row hairlines: no frame, no column rules, no header fill.
        .table(StyleRefinement::default().border_0().rounded(px(0.)).bg(gpui_kit::transparent_black()))
        .table_cell(StyleRefinement::default().border_color(gpui_kit::transparent_black()))
        .table_head(
            StyleRefinement::default()
                .bg(gpui_kit::transparent_black())
                .text_color(t.foreground)
                .font_weight(FontWeight::SEMIBOLD),
        );
    style.highlight_theme = if t.dark { PIERRE_DARK.clone() } else { PIERRE_LIGHT.clone() };
    style.is_dark = t.dark;
    style
}

/// Shiki's `pierre-dark` / `pierre-light` token colors on tree-sitter scopes.
/// The web's light theme is `t3-light`, pierre-light darkened to 4.5:1; these
/// pierre-light colors already sit close to it.
fn pierre_theme(dark: bool) -> Arc<HighlightTheme> {
    let [
        fg,
        comment,
        string,
        number,
        keyword,
        variable,
        parameter,
        function,
        type_,
        punctuation,
        tag,
        attribute,
        bold,
        regex,
        escape,
    ] = if dark {
        [
            "#fafafa", "#737373", "#5ecc71", "#68cdf2", "#ff678d", "#ffa359", "#a3a3a3", "#9d6afb", "#d568ea",
            "#636363", "#ff855e", "#60d199", "#ffd452", "#64d1db", "#61d5c0",
        ]
    } else {
        [
            "#0a0a0a", "#737373", "#199f43", "#1ca1c7", "#d32a61", "#d47628", "#636363", "#693acf", "#a631be",
            "#636363", "#d5512f", "#18a46c", "#d5a910", "#17a5af", "#16a994",
        ]
    };
    let mut syntax = serde_json::Map::new();
    for (scope, hex) in [
        ("attribute", attribute),
        ("boolean", number),
        ("comment", comment),
        ("comment.doc", comment),
        ("constant", number),
        ("constructor", type_),
        ("emphasis.strong", bold),
        ("enum", type_),
        ("function", function),
        ("keyword", keyword),
        ("label", variable),
        ("link_text", tag),
        ("link_uri", string),
        ("number", number),
        ("operator", punctuation),
        ("property", fg),
        ("punctuation", punctuation),
        ("punctuation.bracket", punctuation),
        ("punctuation.delimiter", punctuation),
        ("punctuation.list_marker", punctuation),
        ("punctuation.special", punctuation),
        ("string", string),
        ("string.escape", escape),
        ("string.regex", regex),
        ("string.special", string),
        ("string.special.symbol", number),
        ("tag", tag),
        ("text.literal", string),
        ("title", tag),
        ("type", type_),
        ("variable", fg),
        ("variable.special", variable),
        ("variant", type_),
        ("preproc", keyword),
        ("embedded", fg),
        ("primary", fg),
        ("hint", parameter),
    ] {
        syntax.insert(scope.into(), serde_json::json!({ "color": hex }));
    }
    syntax.insert("emphasis".into(), serde_json::json!({ "color": keyword, "font_style": "italic" }));
    let theme = serde_json::json!({
        "name": if dark { "pierre-dark" } else { "pierre-light" },
        "appearance": if dark { "dark" } else { "light" },
        "style": { "editor.foreground": fg, "syntax": syntax },
    });
    Arc::new(serde_json::from_value(theme).expect("valid highlight theme"))
}

/// Shared so every text view reuses one highlighter cache.
static PIERRE_DARK: LazyLock<Arc<HighlightTheme>> = LazyLock::new(|| pierre_theme(true));
static PIERRE_LIGHT: LazyLock<Arc<HighlightTheme>> = LazyLock::new(|| pierre_theme(false));

/// `ProposedPlanCard.tsx`.
fn plan_card(id: &SharedString, text: &SharedString, this: &WeakEntity<Workspace>, cx: &App, t: &Tokens) -> AnyElement {
    let title: SharedString = text
        .lines()
        .find_map(|line| line.trim_start().strip_prefix('#').map(|h| h.trim_start_matches('#').trim().to_owned()))
        .unwrap_or_else(|| "Proposed plan".into())
        .into();
    let key = timeline::entry_key(id);
    let expanded = this.upgrade().is_some_and(|workspace| workspace.read(cx).expanded.contains(&key));
    let collapsible = text.lines().count() > 18;
    let collapsed = collapsible && !expanded;
    let card_background = alpha(t.card, 0.7);
    let badge = div()
        .h(px(18.))
        .px(px(3.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .bg(t.secondary)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child("Plan");
    let mut body = div()
        .relative()
        .mt(px(16.))
        .text_size(px(TEXT))
        .line_height(px(LINE))
        .text_color(alpha(t.foreground, 0.9))
        .child(markdown(id, text, false, t));
    if collapsed {
        body = body.max_h(px(416.)).overflow_hidden().child(
            div().absolute().left_0().right_0().bottom_0().h(px(96.)).bg(linear_gradient(
                180.,
                linear_color_stop(alpha(card_background, 0.), 0.),
                linear_color_stop(alpha(t.card, 0.95), 1.),
            )),
        );
    }
    let toggle_button = collapsible.then(|| {
        h_flex().w_full().justify_center().mt(px(16.)).child(
            div()
                .id(ElementId::Name(format!("plan-toggle:{id}").into()))
                .h(px(32.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded(px(8.))
                .border_1()
                .border_color(t.input)
                .cursor_pointer()
                .text_size(px(TEXT))
                .hover(|style| style.bg(t.accent))
                .child(if expanded { "Collapse plan" } else { "Expand plan" })
                .on_click(toggle(this, key.clone())),
        )
    });
    let card = v_flex()
        .p(px(20.))
        .rounded(px(16.))
        .border_1()
        .border_color(alpha(t.border, 0.8))
        .bg(card_background)
        .child(
            h_flex()
                .gap(px(12.))
                .child(badge)
                .child(div().min_w_0().truncate().text_size(px(TEXT)).font_weight(FontWeight::MEDIUM).child(title)),
        )
        .child(body)
        .children(toggle_button);
    lane(16., card).into_any_element()
}

// ---------------------------------------------------------------------------
// Work log

fn icon_path(icon: WorkIcon) -> Icon {
    let name = match icon {
        WorkIcon::T3 => return Icon::empty().path("icons/app/t3.svg"),
        WorkIcon::Browser => return Icon::empty().path("icons/app/browser.svg"),
        WorkIcon::Terminal => IconName::Terminal,
        WorkIcon::SquarePen => IconName::SquarePen,
        WorkIcon::Eye => IconName::Eye,
        WorkIcon::Search => IconName::Search,
        WorkIcon::Globe => IconName::Globe,
        WorkIcon::Device => IconName::Smartphone,
        WorkIcon::PullRequest => IconName::GitPullRequestArrow,
        WorkIcon::Wrench => IconName::Wrench,
        WorkIcon::Hammer => IconName::Hammer,
        WorkIcon::Brain => IconName::Brain,
        WorkIcon::Bot => IconName::Bot,
        WorkIcon::Zap => IconName::Zap,
        WorkIcon::MessageCircle => IconName::MessageCircle,
        WorkIcon::ListTodo => IconName::ListTodo,
        WorkIcon::Code => IconName::CodeXml,
        WorkIcon::CircleAlert => IconName::CircleAlert,
        WorkIcon::Check => IconName::Check,
    };
    Icon::new(name)
}

/// `text-icon-muted` with the svg's `opacity-70` (and `brightness(0.6)` in light mode).
fn icon_color(t: &Tokens) -> Hsla {
    if t.dark { alpha(t.muted_foreground, 0.7) } else { alpha(gpui_kit::rgb(0x44444a).into(), 0.7) }
}

/// `text-tool-error-icon/40` under the same 70% opacity.
fn failed_icon_color(t: &Tokens) -> Hsla {
    let base: Hsla = if t.dark { gpui_kit::rgb(0xfca5a5).into() } else { t.error };
    alpha(base, 0.4 * 0.7)
}

/// `WorkLogLine`: a 24px icon slot, then a truncated label.
fn work_line(
    icon: WorkIcon,
    icon_color: Hsla,
    label: impl IntoElement,
    label_color: Hsla,
    trailing: Option<AnyElement>,
) -> gpui_kit::Div {
    h_flex()
        .min_h(px(24.))
        .w_full()
        .min_w_0()
        .gap(px(6.))
        .text_size(px(TEXT))
        .line_height(px(LINE))
        .child(
            div()
                .w(px(24.))
                .h(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(icon_path(icon).size(px(16.)).text_color(icon_color)),
        )
        .child(div().flex_1().min_w_0().truncate().text_color(label_color).child(label))
        .children(trailing)
}

/// The space under a work row (`WorkLogBlock`).
fn work_bottom(work: &WorkRow) -> f32 {
    let header = match &work.kind {
        WorkRowKind::Toggle { expanded, .. } | WorkRowKind::Live { expanded, .. } => *expanded,
        WorkRowKind::Thinking { group, expanded } => group.is_some() && *expanded,
        _ => false,
    };
    if matches!(work.kind, WorkRowKind::Details { .. }) {
        4.
    } else if work.continues || header {
        0.
    } else {
        8.
    }
}

fn work_row(work: &WorkRow, this: &WeakEntity<Workspace>, t: &Tokens) -> AnyElement {
    let muted = t.muted_foreground;
    let row = div().w_full().min_w_0().rounded(px(6.)).px(px(2.)).py(px(2.));
    let content: AnyElement = match &work.kind {
        WorkRowKind::Single { entry, label, expanded, pending_approval } => {
            single_entry(entry, label, *expanded, *pending_approval, this, t)
        }
        WorkRowKind::Toggle { group, summary, icon, .. } => row
            .id(ElementId::Name(work.id.clone()))
            .cursor_pointer()
            .child(work_line(*icon, icon_color(t), summary.clone(), muted, None))
            .on_click(toggle(this, group.clone()))
            .into_any_element(),
        WorkRowKind::Details { entries } => v_flex()
            .id(ElementId::Name(work.id.clone()))
            .max_h(px(288.))
            .overflow_y_scroll()
            .rounded(px(6.))
            .children(entries.iter().map(|(entry, expanded)| detail_entry(entry, *expanded, this, t)))
            .into_any_element(),
        WorkRowKind::Live { group, entry, label, thought, expanded, .. } => {
            let failed = entry.failed();
            let color = if failed { failed_icon_color(t) } else { icon_color(t) };
            let thought_line = thought.as_ref().filter(|_| !*expanded).map(|thought| {
                h_flex()
                    .items_start()
                    .gap(px(6.))
                    .text_size(px(TEXT))
                    .line_height(px(LINE))
                    .child(
                        div()
                            .w(px(24.))
                            .h(px(24.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon_path(WorkIcon::Brain).size(px(16.)).text_color(icon_color(t))),
                    )
                    .child(div().flex_1().min_w_0().line_clamp(4).text_color(muted).child(thought.clone()))
            });
            v_flex()
                .children(thought_line)
                .child(
                    row.id(ElementId::Name(work.id.clone()))
                        .cursor_pointer()
                        .child(work_line(entry.icon(), color, label.clone(), muted, None))
                        .on_click(toggle(this, group.clone())),
                )
                .into_any_element()
        }
        WorkRowKind::Thinking { group, .. } => {
            let line = work_line(WorkIcon::Brain, icon_color(t), "Thinking", muted, None);
            match group {
                Some(group) => row
                    .id(ElementId::Name(work.id.clone()))
                    .cursor_pointer()
                    .child(line)
                    .on_click(toggle(this, group.clone()))
                    .into_any_element(),
                None => row.child(line).into_any_element(),
            }
        }
    };
    lane(work_bottom(work), content).into_any_element()
}

/// What an entry shows when opened (`buildToolCallExpandedBody`), besides reasoning.
fn entry_body(entry: &Entry, label: &str) -> Option<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut add = |text: &str| {
        let text = text.trim();
        if !text.is_empty() && text != label.trim() && !blocks.iter().any(|b| b == text) {
            blocks.push(text.to_owned());
        }
    };
    if let Some(command) = &entry.command {
        add(command);
    }
    if let Some(detail) = &entry.detail {
        add(detail);
    }
    let files: Vec<String> = entry.changed_files.iter().map(|f| f.replace('\\', "/")).collect();
    if !files.is_empty() {
        add(&files.join("\n"));
    }
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

fn chevron(expandable: bool, expanded: bool, t: &Tokens) -> AnyElement {
    let icon = Icon::new(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
        .size(px(12.))
        .text_color(alpha(t.muted_foreground, 0.7));
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .when(expandable, |this| this.child(icon))
        .into_any_element()
}

fn entry_details(entry: &Entry, label: &str, t: &Tokens) -> Option<AnyElement> {
    if entry.is_reasoning() {
        let text = entry.detail.clone()?;
        return Some(
            div()
                .id(ElementId::Name(format!("thought:{}", entry.id).into()))
                .ml(px(28.))
                .max_h(px(384.))
                .overflow_y_scroll()
                .px(px(2.))
                .py(px(4.))
                .text_size(px(TEXT))
                .line_height(px(LINE))
                .text_color(t.foreground)
                .child(markdown(&format!("thought-md:{}", entry.id).into(), &text, false, t))
                .into_any_element(),
        );
    }
    let body = entry_body(entry, label)?;
    Some(
        div()
            .id(ElementId::Name(format!("detail:{}", entry.id).into()))
            .mt(px(2.))
            .mb(px(6.))
            .pb(px(8.))
            .max_h(px(256.))
            .overflow_y_scroll()
            .font_family(FONT_MONO)
            .text_size(px(13.))
            .line_height(relative(1.625))
            .text_color(alpha(t.foreground, 0.85))
            .child(body)
            .into_any_element(),
    )
}

fn single_entry(
    entry: &Rc<Entry>,
    label: &SharedString,
    expanded: bool,
    pending_approval: bool,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let muted = t.muted_foreground;
    let row = div().w_full().min_w_0().rounded(px(6.)).px(px(2.)).py(px(2.));
    let severe = entry.item_type == "error" || entry.activity == Some(Activity::Error);
    if severe || entry.activity == Some(Activity::Warning) {
        let color = if entry.activity == Some(Activity::Warning) { t.warning } else { t.error };
        let title = div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).text_color(color).child(label.clone());
        let message = entry.detail.clone().filter(|d| !d.trim().is_empty() && d != label);
        return row
            .child(
                h_flex()
                    .items_start()
                    .gap(px(6.))
                    .text_size(px(TEXT))
                    .line_height(px(LINE))
                    .child(
                        div()
                            .w(px(24.))
                            .h(px(24.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::CircleAlert).size(px(16.)).text_color(color)),
                    )
                    .child(title),
            )
            .children(message.map(|message| {
                div()
                    .ml(px(28.))
                    .py(px(4.))
                    .text_size(px(TEXT))
                    .line_height(px(LINE))
                    .text_color(alpha(t.foreground, 0.8))
                    .child(message)
            }))
            .into_any_element();
    }
    if pending_approval {
        let waiting = h_flex()
            .ml(px(30.))
            .pb(px(4.))
            .gap(px(4.))
            .text_xs()
            .text_color(t.warning)
            .child(Icon::new(IconName::Shield).size(px(12.)))
            .child("Waiting for approval");
        return row
            .child(work_line(entry.icon(), icon_color(t), label.clone(), muted, None))
            .child(waiting)
            .into_any_element();
    }
    let failed = entry.failed() && entry.status != Some(Status::Declined);
    let color = if failed { failed_icon_color(t) } else { icon_color(t) };
    let label_text: SharedString = if expanded && entry.is_reasoning() {
        if entry.status == Some(Status::InProgress) { "Thinking".into() } else { "Thought".into() }
    } else {
        label.clone()
    };
    let label_color = if entry.is_tool_like() { muted } else { alpha(t.foreground, 0.8) };
    let details = expanded.then(|| entry_details(entry, label, t)).flatten();
    let expandable = if entry.is_reasoning() { entry.detail.is_some() } else { entry_body(entry, label).is_some() };
    let line = work_line(entry.icon(), color, label_text, label_color, Some(chevron(expandable, expanded, t)));
    let row = row.id(ElementId::Name(entry.id.clone())).child(line).children(details);
    if expandable {
        row.cursor_pointer().on_click(toggle(this, timeline::entry_key(&entry.id))).into_any_element()
    } else {
        row.into_any_element()
    }
}

/// An entry inside an open group (`WorkEntryLogRow`): no vertical padding.
fn detail_entry(entry: &Rc<Entry>, expanded: bool, this: &WeakEntity<Workspace>, t: &Tokens) -> AnyElement {
    let label: SharedString = work_log::collapse_whitespace(&work_log::display_label(entry)).into();
    let failed = entry.failed() && entry.status != Some(Status::Declined);
    let color = if failed { failed_icon_color(t) } else { icon_color(t) };
    let label_color = if entry.is_tool_like() { t.muted_foreground } else { alpha(t.foreground, 0.8) };
    let expandable = if entry.is_reasoning() { entry.detail.is_some() } else { entry_body(entry, &label).is_some() };
    let shown: SharedString = if expanded && entry.is_reasoning() { "Thought".into() } else { label.clone() };
    let details = expanded.then(|| entry_details(entry, &label, t)).flatten();
    div()
        .id(ElementId::Name(format!("group-entry:{}", entry.id).into()))
        .w_full()
        .min_w_0()
        .px(px(2.))
        .rounded(px(6.))
        .when(expandable, |this| this.cursor_pointer())
        .child(work_line(entry.icon(), color, shown, label_color, Some(chevron(expandable, expanded, t))))
        .children(details)
        .when(expandable, |row| row.on_click(toggle(this, timeline::entry_key(&entry.id))))
        .into_any_element()
}
