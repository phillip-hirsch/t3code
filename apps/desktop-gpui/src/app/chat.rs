//! The chat timeline: the virtualized message list and its rows. Geometry and
//! colors follow `apps/web/src/components/chat/MessagesTimeline.tsx`, `WorkLog.tsx`
//! and `index.css` (`.chat-markdown`).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::message_scroller::MessageScroller;
use gpui_kit::component::text::{TextView, TextViewState, TextViewStyle};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, Entity, FontWeight, HighlightStyle, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, Subscription, WeakEntity, Window, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, relative, rems,
};

use gpui_kit::assets::IconName;

use super::{Target, Workspace, message_actions, project_name};
use crate::theme::{self, Tokens};
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
            // The draft stage (`render_draft_stage`) draws drafts; nothing else does.
            (Some(Target::Draft { .. }), _) => div().into_any_element(),
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
                let minimap = self.render_minimap(lane.left, window, cx);
                let lane_width = lane.width;
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        MessageScroller::new("timeline", self.scroller.clone(), move |index, window, cx| {
                            match rows.get(index) {
                                Some(row) => render_row(row, &this, lane_width, window, cx),
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
                    .children(minimap)
                    .into_any_element()
            }
        }
    }

    /// The parsed markdown for `key`, kept across frames; see [`MarkdownStates`].
    fn markdown_state(
        &mut self,
        key: SharedString,
        text: &SharedString,
        breaks: bool,
        cx: &mut Context<Self>,
    ) -> Entity<TextViewState> {
        let source = |text: &SharedString| if breaks { hard_breaks(text) } else { text.to_string() };
        if let Some(state) = self.markdown.0.get_mut(&key) {
            if state.text != *text {
                state.text = text.clone();
                state.view.update(cx, |view, cx| view.set_text(&source(text), cx));
            }
            return state.view.clone();
        }
        let view = cx.new(|cx| TextViewState::markdown(&source(text), cx));
        let row_key = key.clone();
        let _changed = cx.observe(&view, move |this, _, cx| {
            if let Some(index) = this.rows.iter().position(|row| shows_markdown(row, &row_key)) {
                this.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
            }
        });
        self.markdown.0.insert(key, MarkdownState { view: view.clone(), text: text.clone(), nowrap: false, _changed });
        view
    }

    fn toggle_code_wrap(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(state) = self.markdown.0.get_mut(key) {
            state.nowrap = !state.nowrap;
            if let Some(index) = self.rows.iter().position(|row| shows_markdown(row, key)) {
                self.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
            }
            cx.notify();
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
        let working = rows.iter().any(|row| match row {
            Row::Working { started_at: Some(_) } => true,
            Row::Now(line) => line.ticks(),
            _ => false,
        });
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

/// Each message's markdown, owned here instead of in GPUI's per-frame element
/// state. That state drops when a row leaves the list's rendered range, and a
/// document over 4 KiB parses on a background thread, so a row measured off
/// screen reported a near-empty height that the list kept: the scroll extent
/// came up short, the tail never resumed and "Scroll to end" stuck. Owned, a
/// parse survives scrolling, and a landed parse remeasures its row.
#[derive(Default)]
pub(super) struct MarkdownStates(HashMap<SharedString, MarkdownState>);

struct MarkdownState {
    view: Entity<TextViewState>,
    text: SharedString,
    /// The code-block wrap toggle is off (`wordWrap` defaults on).
    nowrap: bool,
    _changed: Subscription,
}

/// The key a row's markdown is cached under: its message id, or `thought:{entry}`.
fn thought_key(entry_id: &str) -> SharedString {
    format!("thought:{entry_id}").into()
}

fn shows_markdown(row: &Row, key: &str) -> bool {
    match row {
        Row::User { id, .. } | Row::Assistant { id, .. } | Row::Plan { id, .. } => id == key,
        Row::Work(WorkRow { kind: WorkRowKind::Single { entry, .. }, .. }) => thought_key(&entry.id) == key,
        Row::Work(WorkRow { kind: WorkRowKind::Details { entries }, .. }) => {
            entries.iter().any(|(entry, _)| thought_key(&entry.id) == key)
        }
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MarkdownKind {
    Assistant {
        streaming: bool,
    },
    /// `lineBreaks` and `headingLevelOffset={3}`: a typed newline is a line break.
    User,
}

/// `ChatMarkdown` for `text`, parsed once per `key` for as long as the thread is open.
fn markdown(
    this: &WeakEntity<Workspace>,
    key: SharedString,
    text: &SharedString,
    streaming: bool,
    t: &Tokens,
    cx: &mut App,
) -> AnyElement {
    markdown_with(this, key, text, MarkdownKind::Assistant { streaming }, None, t, cx)
}

fn markdown_with(
    this: &WeakEntity<Workspace>,
    key: SharedString,
    text: &SharedString,
    kind: MarkdownKind,
    width: Option<f32>,
    t: &Tokens,
    cx: &mut App,
) -> AnyElement {
    let user = kind == MarkdownKind::User;
    let key_for_view = key.clone();
    match this.update(cx, |workspace, cx| {
        let wrap = !workspace.markdown.0.get(&key).is_some_and(|state| state.nowrap);
        (workspace.markdown_state(key, text, user, cx), wrap)
    }) {
        Ok((state, wrap)) => markdown_view(&state, kind, key_for_view, wrap, width, this, t).into_any_element(),
        Err(_) => div().into_any_element(),
    }
}

/// remark-breaks: single newlines outside code fences become hard breaks.
fn hard_breaks(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut fenced = false;
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let fence = line.trim_start().starts_with("```") || line.trim_start().starts_with("~~~");
        if fence {
            fenced = !fenced;
        }
        out.push_str(line);
        if let Some(next) = lines.peek() {
            if !fenced && !fence && !line.trim().is_empty() && !next.trim().is_empty() {
                out.push('\\');
            }
            out.push('\n');
        }
    }
    out
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
                        .text_xl()
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

/// "What should we build in **project**?" (`DraftHeroHeadline`).
fn draft_headline(project: Option<SharedString>, t: &Tokens) -> gpui_kit::Div {
    let headline = match project {
        Some(project) => h_flex()
            .justify_center()
            .flex_wrap()
            .child("What should we build in\u{a0}")
            .child(div().text_color(Hsla { a: 0.6, ..t.muted_foreground }).child(project))
            .child("?"),
        None => h_flex().justify_center().child("What should we work on?"),
    };
    div().w_full().text_3xl().line_height(px(36.)).text_color(t.foreground).child(headline)
}

impl Workspace {
    /// A draft centers its composer in the chat area and hangs the headline
    /// above it, out of flow like `ChatView`'s draft hero, so the composer sits
    /// in the middle however tall the headline wraps.
    pub(super) fn render_draft_stage(
        &mut self,
        project_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let (lane, _) = self.chat_layout(window);
        let project = self.shell.projects.iter().find(|p| p.id == project_id).map(project_name);
        div()
            .size_full()
            .flex()
            .items_center()
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .bottom_full()
                            .pl(px(lane.left))
                            .pr(px(lane.right))
                            .pb(px(16.))
                            .child(draft_headline(project, &t)),
                    )
                    .child(self.render_composer(window, cx)),
            )
            .into_any_element()
    }
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
pub(super) fn lane(bottom: f32, child: impl IntoElement) -> gpui_kit::Div {
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

fn render_row(row: &Row, this: &WeakEntity<Workspace>, lane_width: f32, _: &mut Window, cx: &mut App) -> AnyElement {
    let t = *theme::tokens(cx);
    match row {
        Row::User {
            id,
            text,
            steer,
            grouped_with_previous,
            grouped_with_next,
            continues_steer,
            created_at,
            revert,
            sent_by_t3code,
        } => {
            let expanded =
                this.upgrade().is_some_and(|workspace| workspace.read(cx).expanded.contains(&timeline::entry_key(id)));
            let actions = message_actions::user_actions(this, id, text, created_at, revert.as_ref(), &t, cx);
            let body = markdown_with(this, id.clone(), text, MarkdownKind::User, None, &t, cx);
            user_row(
                id,
                text,
                body,
                actions,
                steer.filter(|_| !*continues_steer),
                *grouped_with_previous,
                *grouped_with_next,
                expanded,
                *sent_by_t3code,
                this,
                &t,
            )
        }
        Row::Assistant { id, text, streaming, terminal, updated_at, status, fork } => {
            let body = div()
                .px(px(4.))
                .py(px(2.))
                .text_size(px(TEXT))
                .line_height(px(LINE))
                .text_color(alpha(t.foreground, 0.9))
                .child(markdown_with(
                    this,
                    id.clone(),
                    text,
                    MarkdownKind::Assistant { streaming: *streaming },
                    Some(lane_width - 8.),
                    &t,
                    cx,
                ));
            // The finished reply's actions (`AssistantMessageMeta`) hold their space while hidden.
            let meta = terminal.then(|| {
                message_actions::assistant_meta(this, id, text, *streaming, updated_at, status, fork.as_ref(), &t, cx)
                    .opacity(0.)
                    .group_hover("timeline-row", |style| style.opacity(1.))
            });
            lane(if *terminal { 16. } else { 8. }, v_flex().group("timeline-row").child(body).children(meta))
                .into_any_element()
        }
        Row::Plan { id, markdown: text } => plan_card(id, text, this, lane_width, cx, &t),
        Row::Work(work) => work_row(work, this, &t, cx),
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
        Row::Divider { label, failed, failure, .. } => {
            let color = if *failed { t.error } else { t.muted_foreground };
            let line_color = if *failed { alpha(t.error, 0.3) } else { alpha(t.border, 0.7) };
            let line = || div().flex_1().h(px(1.)).bg(line_color);
            let content = h_flex()
                .gap(px(12.))
                .py(px(6.))
                .text_xs()
                .text_color(color)
                .child(line())
                .child(
                    h_flex()
                        .min_w_0()
                        .gap(px(6.))
                        .child(
                            Icon::new(if *failed { IconName::CircleAlert } else { IconName::Minimize2 }).size(px(12.)),
                        )
                        .children(compaction_label(label))
                        .children(
                            failure
                                .as_ref()
                                .map(|failure| div().min_w_0().truncate().opacity(0.8).child(format!("· {failure}"))),
                        ),
                )
                .child(line());
            lane(16., content).into_any_element()
        }
        Row::Event { label, .. } => {
            let line = work_line(WorkIcon::Bot, icon_color(&t), label.clone(), t.muted_foreground, None);
            lane(8., div().px(px(2.)).py(px(2.)).child(line)).into_any_element()
        }
        // Subagent rows carry no bottom space of their own.
        Row::Subagent(card) => lane(0., subagent_card(card, this, &t)).into_any_element(),
        Row::Embed(embed) => lane(8., embed_card(embed, this, &t)).into_any_element(),
        Row::AttemptFold { key, expanded } => {
            let chevron = if *expanded { IconName::ChevronDown } else { IconName::ChevronRight };
            let button = h_flex()
                .id(ElementId::Name(key.clone()))
                .w_full()
                .gap(px(8.))
                .px(px(10.))
                .py(px(8.))
                .rounded(px(6.))
                .border_1()
                .border_color(alpha(t.border, 0.6))
                .bg(alpha(t.muted, 0.2))
                .hover(|style| style.bg(alpha(t.muted, 0.35)))
                .cursor_pointer()
                .child(Icon::new(chevron).size(px(14.)).text_color(t.muted_foreground))
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(alpha(t.foreground, 0.8))
                        .child("Superseded attempt"),
                )
                .child(div().text_size(px(11.)).text_color(t.muted_foreground).child("Cut off by a steer"))
                .on_click(toggle(this, key.clone()));
            lane(8., button).into_any_element()
        }
        Row::Queued { message, last } => super::queue::queued_row(message, *last, this, &t, cx),
        Row::QueueToggle { total, expanded } => super::queue::toggle_row(*total, *expanded, this, &t),
        Row::Now(line) => super::thread_now::now_row(line, this, &t, cx),
    }
}

/// `SubagentTimelineLink`: avatar, title and detail, status, elapsed time, and a
/// chevron when the subagent's thread can open.
fn subagent_card(card: &timeline::SubagentCard, this: &WeakEntity<Workspace>, t: &Tokens) -> AnyElement {
    let muted = t.muted_foreground;
    let detail_color = if card.failed { t.error } else { muted };
    let dot = match card.dot {
        timeline::Dot::Info => t.info,
        timeline::Dot::Muted => alpha(muted, 0.6),
        timeline::Dot::Success => t.success_foreground,
        timeline::Dot::Destructive => t.error,
    };
    let logo = crate::assets::provider_icon(&card.driver);
    let avatar = div()
        .relative()
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(t.secondary)
        .child(match logo {
            Some((path, tint)) => Icon::empty().path(path).size(px(14.)).text_color(tint.unwrap_or(t.foreground)),
            None => Icon::new(IconName::Bot).size(px(14.)).text_color(muted),
        })
        .child(
            div()
                .absolute()
                .right(px(-1.))
                .bottom(px(-1.))
                .size(px(8.))
                .rounded_full()
                .border_2()
                .border_color(t.background)
                .bg(dot),
        );
    let elapsed = card.started_at.as_deref().and_then(work_log::parse_iso_ms).and_then(|start| {
        let end = match card.completed_at.as_deref().and_then(work_log::parse_iso_ms) {
            Some(end) => end,
            None if card.live => now_ms(),
            None => return None,
        };
        Some(timeline::agent_elapsed(end - start))
    });
    let text = v_flex()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .truncate()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.foreground)
                .child(card.title.clone()),
        )
        .child(
            div()
                .truncate()
                .text_size(px(11.))
                .line_height(relative(1.625))
                .text_color(detail_color)
                .child(card.detail.clone().unwrap_or_else(|| card.status_label.into())),
        );
    let row = h_flex()
        .id(ElementId::Name(format!("subagent:{}", card.id).into()))
        .w_full()
        .min_w_0()
        .gap(px(10.))
        .px(px(8.))
        .py(px(6.))
        .rounded(px(6.))
        .child(avatar)
        .child(text)
        .children(
            card.status_beside_detail
                .then(|| div().flex_none().text_xs().text_color(detail_color).child(card.status_label)),
        )
        .children(elapsed.map(|elapsed| div().flex_none().text_xs().text_color(muted).child(elapsed)));
    match card.child_thread_id.clone() {
        Some(child) => {
            let this = this.clone();
            row.cursor_pointer()
                .hover(|style| style.bg(alpha(t.accent, 0.2)))
                .child(Icon::new(IconName::ChevronRight).size(px(14.)).text_color(alpha(muted, 0.6)))
                .on_click(move |_, window, cx| {
                    let _ = this.update(cx, |workspace, cx| workspace.select_thread(child.to_string(), window, cx));
                })
                .into_any_element()
        }
        None => row.into_any_element(),
    }
}

/// A rendered page or MCP app. The web embeds it in a frame; here it opens in the browser.
fn embed_card(embed: &Rc<timeline::Embed>, this: &WeakEntity<Workspace>, t: &Tokens) -> AnyElement {
    let open = {
        let (this, embed) = (this.clone(), embed.clone());
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |workspace, cx| workspace.open_embed(&embed, cx));
        }
    };
    h_flex()
        .w_full()
        .min_w_0()
        .gap(px(10.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(alpha(t.border, 0.6))
        .bg(alpha(t.card, 0.3))
        .child(
            Icon::new(if embed.app_server.is_some() { IconName::AppWindow } else { IconName::Globe })
                .size(px(16.))
                .text_color(icon_color(t)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().truncate().text_size(px(TEXT)).font_weight(FontWeight::MEDIUM).child(embed.title.clone()))
                .child(div().text_xs().text_color(t.muted_foreground).child(match &embed.app_server {
                    Some(_) => "MCP app",
                    None => "HTML page",
                })),
        )
        .child(
            h_flex()
                .id(ElementId::Name(format!("embed-open:{}", embed.id).into()))
                .flex_none()
                .h(px(24.))
                .px(px(7.))
                .gap(px(4.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.input)
                .cursor_pointer()
                .text_xs()
                .hover(|style| style.bg(t.accent))
                .child(Icon::new(IconName::ExternalLink).size(px(12.)))
                .child("Open in browser")
                .on_click(open),
        )
        .into_any_element()
}

impl Workspace {
    /// Mints a signed URL for the page (`assets.createUrl`) and opens it.
    fn open_embed(&mut self, embed: &timeline::Embed, cx: &mut Context<Self>) {
        let (Some(client), Some(base)) = (self.client(), self.http_base().map(str::to_owned)) else { return };
        let mut resource = serde_json::json!({
            "_tag": "attachment",
            "attachmentId": embed.attachment_id,
            "mimeType": "text/html",
            "disposition": "inline",
        });
        match &embed.app_server {
            Some(_) => resource["renderIntent"] = "mcp-app".into(),
            None => resource["fileName"] = embed.file_name().into(),
        }
        cx.spawn(async move |this, cx| {
            let result = client.request("assets.createUrl", serde_json::json!({"resource": resource})).await;
            let _ = this.update(cx, |this, cx| {
                match result.as_ref().ok().and_then(|r| r.get("relativeUrl")).and_then(|u| u.as_str()) {
                    Some(url) => cx.open_url(&format!("{base}{url}")),
                    None => this.error = Some("Page unavailable. Reconnect to the environment and try again.".into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// "Context compacted 120K → 40K tokens", with the arrow drawn as `ArrowRight`.
fn compaction_label(label: &SharedString) -> Vec<AnyElement> {
    match label.split_once(" → ") {
        Some((before, after)) => vec![
            div().child(before.to_owned()).into_any_element(),
            Icon::new(IconName::ArrowRight).size(px(12.)).into_any_element(),
            div().child(after.to_owned()).into_any_element(),
        ],
        None => vec![div().child(label.clone()).into_any_element()],
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
    body: AnyElement,
    actions: gpui_kit::Div,
    steer: Option<Steer>,
    grouped_with_previous: bool,
    grouped_with_next: bool,
    expanded: bool,
    sent_by_t3code: bool,
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
        .child(div().when(collapsed, |this| this.max_h(px(176.)).overflow_hidden()).child(body));
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
        .child(actions);
    // `text-2xs`: 0.6875rem.
    let attribution = sent_by_t3code.then(|| {
        div().mr(px(4.)).text_size(rems(0.6875)).text_color(alpha(t.muted_foreground, 0.7)).child("Sent by T3 Code")
    });
    let column = v_flex()
        .group("timeline-row")
        .items_end()
        .gap(px(4.))
        .children(attribution)
        .children(steer_label)
        .child(bubble)
        .children(toggle_button)
        .child(actions);
    lane(if grouped_with_next { 4. } else { 16. }, column).into_any_element()
}

fn markdown_view(
    state: &Entity<TextViewState>,
    kind: MarkdownKind,
    key: SharedString,
    wrap: bool,
    width: Option<f32>,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> TextView {
    let streaming = matches!(kind, MarkdownKind::Assistant { streaming: true });
    let inline_code = StyleRefinement::default()
        .bg(alpha(t.foreground, 0.1))
        .rounded(px(4.))
        .px(px(4.9))
        .py(px(1.4))
        .text_size(px(11.9));
    let t = *t;
    let (header_this, link_this) = (this.clone(), this.clone());
    TextView::new(state)
        .style(markdown_style(&t, kind == MarkdownKind::User, wrap))
        .inline_code_style(inline_code)
        .selectable(true)
        .stream_fade(streaming)
        .code_block_actions(move |block, _, cx| {
            code_block_header(&header_this, &key, block, wrap, streaming, width, &t, cx)
        })
        .on_link_click(move |url, event, window, cx| {
            let _ = link_this.update(cx, |workspace, cx| match thread_link(url) {
                Some(thread_id) => workspace.select_thread(thread_id, window, cx),
                // A project's pull request opens in its panel (`pullRequestLinksOpenInApp`); Ctrl-click and
                // every other link follow `browserLinkTarget`.
                None if !event.modifiers().secondary(cx) && workspace.open_change_request_link(url, cx) => {}
                None => workspace.open_link(url, event, window, cx),
            });
        })
}

/// `parseThreadLink`: `t3-thread://v1/{thread}` or `t3-thread://v2/{environment}/{thread}`.
fn thread_link(url: &str) -> Option<String> {
    let rest = url.strip_prefix("t3-thread://")?;
    let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    match parts.as_slice() {
        ["v1", thread] | ["v2", _, thread] if !thread.is_empty() => Some((*thread).to_owned()),
        _ => None,
    }
}

/// `MarkdownCodeBlock`'s header: the language on the left; wrap and copy on the right.
/// TextView pins the actions 8px into the block's top-right corner over its own fill,
/// so given the block's `width` the row spans it and paints the block's color.
fn code_block_header(
    this: &WeakEntity<Workspace>,
    key: &SharedString,
    block: &gpui_kit::base::text::CodeBlock,
    wrap: bool,
    streaming: bool,
    width: Option<f32>,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
    let label_color = alpha(t.foreground, 0.72);
    let code: SharedString = block.code();
    let id: SharedString = format!("code:{key}:{:x}", fxhash(&code)).into();
    let wrap_key = key.clone();
    let wrap_this = this.clone();
    let wrap_button = div()
        .id("wrap")
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .text_color(label_color)
        .when(wrap, |button| button.bg(t.secondary).text_color(t.foreground))
        .hover(|style| style.bg(t.accent).text_color(t.foreground))
        .child(Icon::new(IconName::TextWrap).size(px(12.)))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(if wrap { "Disable line wrap" } else { "Wrap lines" })
                .build(window, cx)
        })
        .on_click(move |_, _, cx| {
            let _ = wrap_this.update(cx, |workspace, cx| workspace.toggle_code_wrap(&wrap_key, cx));
        });
    let lang = block.lang();
    let run = runnable_command(lang.as_deref().unwrap_or_default(), &code)
        .filter(|_| !streaming && this.upgrade().is_some_and(|w| w.read(cx).git_cwd().is_some()))
        .map(|command| {
            let this = this.clone();
            div()
                .id("run")
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .text_color(label_color)
                .hover(|style| style.bg(t.accent).text_color(t.foreground))
                .child(Icon::new(IconName::Play).size(px(12.)))
                .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Run in terminal").build(window, cx))
                .on_click(move |_, window, cx| {
                    if let Some(cwd) = this.upgrade().and_then(|w| w.read(cx).git_cwd()) {
                        let command = command.clone();
                        window.dispatch_action(Box::new(super::RunInTerminal { cwd, command }), cx);
                    }
                })
        });
    let copy = message_actions::code_copy_button(this, id, code, t, cx);
    let fill = t.background.blend(code_background(t));
    h_flex()
        .when_some(width, |row, width| row.w(px(width - 8.)).pl(px(10.)).justify_between())
        .mt(px(-2.))
        .mr(px(-2.))
        .rounded(px(6.))
        .bg(fill)
        .gap(px(8.))
        .child(
            div()
                .font_family(t.fonts.mono)
                .text_size(px(11.))
                .text_color(label_color)
                .children(block.lang().filter(|lang| !lang.is_empty())),
        )
        .child(h_flex().gap(px(2.)).child(wrap_button).children(run).children((!streaming).then_some(copy)))
        .into_any_element()
}

/// `MarkdownCodeBlock`'s `canRun`: a shell block whose command reads the same in the terminal.
fn runnable_command(lang: &str, code: &str) -> Option<String> {
    let shell = matches!(lang, "sh" | "bash" | "zsh" | "fish" | "shell" | "powershell" | "pwsh");
    let command = code.trim();
    // Control and invisible format characters (`\p{Cc}\p{Cf}`) can make the shown command
    // differ from what the terminal receives.
    let hidden = |c: char| {
        c.is_control()
            || matches!(c, '\u{00ad}' | '\u{061c}' | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}')
            || matches!(c, '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}')
    };
    let body = code.strip_suffix('\n').unwrap_or(code);
    (shell && !command.is_empty() && !command.ends_with('\\') && !body.chars().any(hidden)).then(|| command.to_owned())
}

/// A short stable hash, to tell a message's code blocks apart.
fn fxhash(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| (hash ^ byte as u64).wrapping_mul(0x100000001b3))
}

/// `.chat-markdown` (`index.css:1866-2280`) and `MarkdownCodeBlock.tsx`.
/// The code block fill: `input/32` in dark mode, `secondary` in light.
fn code_background(t: &Tokens) -> Hsla {
    if t.dark { alpha(t.input, 0.32) } else { t.secondary }
}

pub(super) fn markdown_style(t: &Tokens, user: bool, wrap: bool) -> TextViewStyle {
    let code_background = code_background(t);
    let code_border = if t.dark { gpui_kit::transparent_black() } else { alpha(t.border, 0.7) };
    // The web's header row (6px + 24px toolbar) sits above the code's own 12.8px.
    let mut code_block = StyleRefinement::default()
        .pt(px(42.8))
        .pb(px(12.8))
        .px(px(14.4))
        .rounded(px(8.))
        .bg(code_background)
        .border_color(code_border)
        .text_size(px(t.fonts.code_size))
        .line_height(relative(1.375));
    if !wrap {
        code_block = code_block.whitespace_nowrap();
        code_block.overflow.x = Some(gpui_kit::Overflow::Scroll);
    }
    let mut style = TextViewStyle::default()
        .paragraph_gap(rems(0.65))
        .heading_font_size(move |level, _| {
            px(match level {
                _ if user => 14.,
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
fn plan_card(
    id: &SharedString,
    text: &SharedString,
    this: &WeakEntity<Workspace>,
    lane_width: f32,
    cx: &mut App,
    t: &Tokens,
) -> AnyElement {
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
        .child(markdown_with(
            this,
            id.clone(),
            text,
            MarkdownKind::Assistant { streaming: false },
            Some(lane_width - 42.),
            t,
            cx,
        ));
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

fn work_row(work: &WorkRow, this: &WeakEntity<Workspace>, t: &Tokens, cx: &mut App) -> AnyElement {
    let muted = t.muted_foreground;
    let row = div().w_full().min_w_0().rounded(px(6.)).px(px(2.)).py(px(2.));
    let content: AnyElement = match &work.kind {
        WorkRowKind::Single { entry, label, expanded } => single_entry(entry, label, *expanded, this, t, cx),
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
            .children(
                entries.iter().map(|(entry, expanded)| detail_entry(entry, *expanded, this, t, cx)).collect::<Vec<_>>(),
            )
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

fn expandable(entry: &Entry, label: &str) -> bool {
    if entry.is_reasoning() {
        entry.detail.is_some()
    } else {
        super::inspector::has_inspector(entry) || entry_body(entry, label).is_some()
    }
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

fn entry_details(
    entry: &Entry,
    label: &str,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &mut App,
) -> Option<AnyElement> {
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
                .child(markdown(this, thought_key(&entry.id), &text.into(), false, t, cx))
                .into_any_element(),
        );
    }
    if let Some(inspector) = super::inspector::inspector(entry, this, t, cx) {
        return Some(div().mt(px(2.)).mb(px(6.)).pb(px(8.)).child(inspector).into_any_element());
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
            .font_family(t.fonts.mono)
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
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &mut App,
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
    let failed = entry.failed() && entry.status != Some(Status::Declined);
    let color = if failed { failed_icon_color(t) } else { icon_color(t) };
    let label_text: SharedString = if expanded && entry.is_reasoning() {
        if entry.status == Some(Status::InProgress) { "Thinking".into() } else { "Thought".into() }
    } else {
        label.clone()
    };
    let label_color = if entry.is_tool_like() { muted } else { alpha(t.foreground, 0.8) };
    let details = if expanded { entry_details(entry, label, this, t, cx) } else { None };
    let expandable = expandable(entry, label);
    let line = work_line(entry.icon(), color, label_text, label_color, Some(chevron(expandable, expanded, t)));
    let row = row.id(ElementId::Name(entry.id.clone())).child(line).children(details);
    if expandable {
        row.cursor_pointer().on_click(toggle(this, timeline::entry_key(&entry.id))).into_any_element()
    } else {
        row.into_any_element()
    }
}

/// An entry inside an open group (`WorkEntryLogRow`): no vertical padding.
fn detail_entry(
    entry: &Rc<Entry>,
    expanded: bool,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &mut App,
) -> AnyElement {
    let label: SharedString = work_log::collapse_whitespace(&work_log::display_label(entry)).into();
    let failed = entry.failed() && entry.status != Some(Status::Declined);
    let color = if failed { failed_icon_color(t) } else { icon_color(t) };
    let label_color = if entry.is_tool_like() { t.muted_foreground } else { alpha(t.foreground, 0.8) };
    let expandable = expandable(entry, &label);
    let shown: SharedString = if expanded && entry.is_reasoning() { "Thought".into() } else { label.clone() };
    let details = if expanded { entry_details(entry, &label, this, t, cx) } else { None };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_single_visible_shell_commands_run() {
        assert_eq!(runnable_command("bash", "ls -la\n").as_deref(), Some("ls -la"));
        assert_eq!(runnable_command("pwsh", "Get-Item .").as_deref(), Some("Get-Item ."));
        assert_eq!(runnable_command("rust", "fn main() {}\n"), None);
        assert_eq!(runnable_command("sh", "echo a \\\n"), None);
        assert_eq!(runnable_command("sh", "  \n"), None);
        assert_eq!(runnable_command("sh", "rm -rf \u{202e}/ tmp\n"), None);
        // A newline or tab inside the block is a control character too, as on the web.
        assert_eq!(runnable_command("sh", "a\nb\n"), None);
    }

    #[test]
    fn thread_links_open_their_thread() {
        assert_eq!(thread_link("t3-thread://v1/abc").as_deref(), Some("abc"));
        assert_eq!(thread_link("t3-thread://v2/env/abc/").as_deref(), Some("abc"));
        assert_eq!(thread_link("https://example.com"), None);
        assert_eq!(thread_link("t3-thread://v3/x"), None);
    }

    #[test]
    fn typed_newlines_break_lines_except_in_code_and_between_paragraphs() {
        assert_eq!(hard_breaks("one\ntwo"), "one\\\ntwo");
        assert_eq!(hard_breaks("one\n\ntwo"), "one\n\ntwo");
        assert_eq!(hard_breaks("```\na\nb\n```\nafter"), "```\na\nb\n```\nafter");
    }
}
