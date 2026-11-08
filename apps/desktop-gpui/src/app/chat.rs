//! The chat timeline: the virtualized message list and its rows. Geometry and
//! colors follow `apps/web/src/components/chat/MessagesTimeline.tsx`, `WorkLog.tsx`
//! and `index.css` (`.chat-markdown`).

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use gpui_kit::component::button::Button;
use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::message_scroller::MessageScroller;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::{TextView, TextViewState, TextViewStyle};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, ElementId, Entity, Font, FontWeight, HighlightStyle, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    StyleRefinement, Styled as _, Subscription, TextRun, WeakEntity, Window, div, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, relative, rems, rgb,
};

use gpui_kit::assets::IconName;

use super::{Target, Workspace, message_actions};
use crate::theme::{self, Tokens};
use crate::timeline::{self, DetailEntry, Row, Steer, WorkRow, WorkRowKind};
use crate::work_log::{self, Activity, Entry, Status, WorkIcon};

/// Prose: `text-sm` at `leading-relaxed`.
const TEXT: f32 = 14.;
const LINE: f32 = 22.75;
/// `--guest`: fuchsia-500.
const GUEST: u32 = 0xe12afb;
/// A user message longer than this collapses (`shouldCollapseUserMessage`).
const COLLAPSE_CHARS: usize = 600;
const COLLAPSE_LINES: usize = 8;

/// `shouldCollapseUserMessage`; it stops counting at the limits.
pub(super) fn should_collapse(text: &str) -> bool {
    text.chars().nth(COLLAPSE_CHARS).is_some() || text.lines().nth(COLLAPSE_LINES).is_some()
}

impl Workspace {
    pub(super) fn render_body(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let (lane, _) = self.chat_layout(window);
        match (&self.target, &self.thread) {
            (None, _) => no_thread(&t),
            // The draft stage (`render_draft_stage`) draws drafts; nothing else does.
            (Some(Target::Draft { .. }), _) => div().into_any_element(),
            // `emptyPlaceholderInset`: the notes center in the space the composer leaves.
            (Some(Target::Thread { .. }), Some(thread)) if thread.state.projection.is_none() => v_flex()
                .size_full()
                .pb(px(self.composer_height))
                .child(centered_note("Loading thread…", &t))
                .into_any_element(),
            (Some(Target::Thread { .. }), _) if self.rows.is_empty() => v_flex()
                .size_full()
                .pb(px(self.composer_height))
                .child(centered_note("Send a message to start the conversation.", &t))
                .into_any_element(),
            (Some(Target::Thread { .. }), _) => {
                let rows = self.rows.clone();
                let this = cx.weak_entity();
                let minimap = self.render_minimap(lane.left, window, cx);
                let lane_width = lane.width;
                // The list keeps its top row's pixel offset across a width change, so reflowed text
                // would slide under it; anchor that row proportionally instead, as the browser keeps
                // what was in view.
                let previous = self.timeline_lane_width.replace(lane_width);
                if previous > 0. && (previous - lane_width).abs() >= 0.5 {
                    self.scroller.update(cx, gpui_kit::component::message_scroller::MessageScrollerState::remeasure);
                }
                let composer = self.composer_height;
                // `.chat-scroll-to-bottom` spans the lane, not the canvas, and clears the composer.
                let pill_offset = lane.left - lane.right;
                let mut jump_style = StyleRefinement::default().mb(px(composer - 6.));
                if pill_offset > 0. {
                    jump_style = jump_style.ml(px(pill_offset));
                } else if pill_offset < 0. {
                    jump_style = jump_style.mr(px(-pill_offset));
                }
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
                            StyleRefinement::default()
                                .pt(px(16.))
                                .pb(px(16. + composer))
                                .pl(px(lane.left))
                                .pr(px(lane.right)),
                        )
                        .with_row_style(StyleRefinement::default().pb(px(0.)))
                        .with_jump_button_label("Scroll to end")
                        .with_jump_button_style(jump_style)
                        .with_jump_button_renderer(move |button| scroll_to_end_button(button, &t)),
                    )
                    .children(minimap)
                    .children(self.render_chat_menu())
                    .into_any_element()
            }
        }
    }

    /// The parsed markdown for `key`, kept across frames; see [`MarkdownStates`].
    fn markdown_state(
        &mut self,
        key: SharedString,
        text: &SharedString,
        cx: &mut Context<Self>,
    ) -> Entity<TextViewState> {
        if let Some(state) = self.markdown.0.get(&key).filter(|state| state.text == *text) {
            return state.view.clone();
        }
        let source = self.with_skill_links(text);
        self.load_markdown_images(&key, &source, cx);
        if let Some(state) = self.markdown.0.get_mut(&key) {
            state.text = text.clone();
            state.view.update(cx, |view, cx| view.set_text(&source, cx));
            return state.view.clone();
        }
        let view = cx.new(|cx| TextViewState::markdown(&source, cx));
        let row_key = key.clone();
        let _changed = cx.observe(&view, move |this, _, cx| {
            // The streaming message is near the end.
            if let Some(index) = this.rows.iter().rposition(|row| shows_markdown(row, &row_key)) {
                this.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
            } else if let Some(side) = &this.side_chat.view
                && let Some(index) = side.rows.iter().position(|row| shows_markdown(row, &row_key))
            {
                side.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
            }
            this.paint_find(&row_key, cx);
        });
        self.markdown
            .0
            .insert(key.clone(), MarkdownState { view: view.clone(), text: text.clone(), nowrap: false, _changed });
        self.paint_find(&key, cx);
        view
    }

    /// The markdown view kept for `key` (a message's id), if it has rendered.
    pub(super) fn markdown_view(&self, key: &str) -> Option<Entity<TextViewState>> {
        self.markdown.0.get(key).map(|state| state.view.clone())
    }

    pub(super) fn markdown_keys(&self) -> Vec<SharedString> {
        self.markdown.0.keys().cloned().collect()
    }

    fn toggle_code_wrap(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(state) = self.markdown.0.get_mut(key) {
            state.nowrap = !state.nowrap;
            if let Some(index) = self.rows.iter().rposition(|row| shows_markdown(row, key)) {
                self.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
            }
            cx.notify();
        }
    }

    /// Opens or closes a work group, turn fold or entry.
    pub(super) fn toggle_expanded(&mut self, key: SharedString, cx: &mut Context<Self>) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        self.refresh_rows(cx);
        self.refresh_side_chat_rows(cx);
        cx.notify();
    }

    /// Keeps a once-a-second repaint alive while a "Working for …" row shows.
    pub(super) fn sync_working_tick(&mut self, rows: &[Row], cx: &mut Context<Self>) {
        let working = rows.iter().any(|row| match row {
            Row::Working { started_at: Some(_) } => true,
            Row::Now(line) => line.ticks(),
            Row::Workflow { row, .. } => row.phases.iter().flat_map(|p| &p.2).chain(&row.unphased).any(|c| c.live),
            Row::SubagentGroup { cards, .. } => cards.iter().any(|c| c.live),
            Row::WorktreeSetup { snapshot, .. } => snapshot.phase == "running",
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
/// The key an open entry's markdown and images load under, so its row remeasures when they land.
pub(super) fn thought_key(entry_id: &str) -> SharedString {
    format!("thought:{entry_id}").into()
}

pub(super) fn shows_markdown(row: &Row, key: &str) -> bool {
    let thought = key.strip_prefix("thought:");
    match row {
        Row::User { id, .. } | Row::Assistant { id, .. } | Row::Plan { id, .. } => id == key,
        Row::Workflow { row, .. } => key.strip_prefix("workflow-script:") == Some(row.opening.message_id.as_str()),
        Row::Work(WorkRow { kind: WorkRowKind::Single { entry, .. }, .. }) => thought == Some(&*entry.id),
        Row::Work(WorkRow { kind: WorkRowKind::Details { entries }, .. }) => {
            thought.is_some_and(|id| entries.iter().any(|detail| *detail.entry.id == *id))
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
pub(super) fn markdown(
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
    let key_for_view = key.clone();
    match this.update(cx, |workspace, cx| {
        let wrap = !workspace.markdown.0.get(&key).is_some_and(|state| state.nowrap);
        let math = workspace.client_settings.values.get("mathRenderingEnabled") == &serde_json::Value::Bool(true);
        let chips = (workspace.git_cwd(), workspace.entry_kinds.clone());
        (workspace.markdown_state(key, text, cx), wrap, workspace.markdown_images.clone(), math, chips)
    }) {
        Ok((state, wrap, images, math, chips)) => {
            markdown_view(&state, kind, key_for_view, wrap, width, this, chips, t)
                .image_source(images.resolver())
                .when(math, |view| view.plugin(super::markdown_ui::InlineMath).plugin(super::markdown_ui::DisplayMath))
                .into_any_element()
        }
        Err(_) => div().into_any_element(),
    }
}

fn scroll_to_end_button(
    button: gpui_kit::component::button::Button,
    t: &Tokens,
) -> gpui_kit::component::button::Button {
    use gpui_kit::component::IconName as ComponentIcon;
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

/// `DraftHeroProjectHeading`'s fit check: the one-line heading, measured at `text-3xl`, is
/// no wider than the lane.
fn heading_fits_inline(project: &str, width: f32, window: &Window) -> bool {
    let font = window.text_style().font();
    let size = rems(1.875).to_pixels(window.rem_size());
    let gap = f32::from(rems(0.5).to_pixels(window.rem_size()));
    let measure = |text: String, weight: FontWeight| {
        let run = TextRun {
            len: text.len(),
            font: Font { weight, ..font.clone() },
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        f32::from(window.text_system().shape_line(text.into(), size, &[run], None).width)
    };
    measure("What should we build in".into(), FontWeight::NORMAL)
        + gap
        + measure(format!("{project}?"), FontWeight::MEDIUM)
        <= width
}

/// `InlineButton` text: medium weight, underlined on hover.
fn inline_button(id: &'static str, label: SharedString, color: Hsla, hover: Hsla) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .min_w_0()
        .max_w_full()
        .truncate()
        .cursor_pointer()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .hover(move |s| s.text_color(hover).underline())
        .child(label)
}

fn headline_tooltip(text: SharedString) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView + 'static {
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
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
        let (lane, _) = self.chat_layout(window);
        let headline = self.render_draft_headline(project_id, lane.width, window, cx);
        // `[@media(max-height:34rem)]:items-end`: short windows pin the composer to the bottom.
        let short = f32::from(window.viewport_size().height) <= 544.;
        div()
            .size_full()
            .flex()
            .map(|el| if short { el.items_end() } else { el.items_center() })
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
                            .child(headline),
                    )
                    .child(self.render_composer(window, cx)),
            )
            .into_any_element()
    }

    /// `DraftHeroHeadline`: "What should we build in **project**?" on one line when the
    /// window is `lg` and it fits, else "What should we build?" over the project; then
    /// "or start without a project". A "No project" draft asks "What should we work on?".
    fn render_draft_headline(
        &self,
        project_id: &str,
        width: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let project = self.shell.projects.iter().find(|p| p.id == project_id);
        let is_scratch = self.draft_is_scratch();
        let scratch_env = self.scratch_environment();
        let viewport = f32::from(window.viewport_size().width);
        // Tailwind's `sm` and `lg` breakpoints, in CSS px.
        let title_size = if viewport >= 640. { rems(1.875) } else { rems(1.5) };
        let lg = viewport >= 1024.;
        // `[@media(max-height:28rem)]`: very short windows keep only the project name.
        let terse = f32::from(window.viewport_size().height) <= 448.;
        let open = self.draft_picker_open();
        let focus = &self.draft_picker.trigger;
        // `focus-visible:outline-2 outline-ring`, drawn outside the text as an outline is.
        let ring = focus.is_focused(window) && window.last_input_was_keyboard();
        let picker = |label: SharedString, tooltip: Option<SharedString>, cx: &mut Context<Self>| {
            // `InlineButton tone="picker"`: underlined while its popup is open.
            let trigger = inline_button("draft-project-picker", label, t.foreground, t.foreground)
                .track_focus(focus)
                .when(open, gpui_kit::Styled::underline)
                .when_some(tooltip, |el, text| el.tooltip(headline_tooltip(text)))
                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                    this.toggle_draft_picker(!event.is_keyboard(), window, cx);
                }))
                .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_str();
                    if matches!(key, "down" | "up") && !event.keystroke.modifiers.modified() {
                        cx.stop_propagation();
                        this.open_draft_picker_from_key(key == "down", window, cx);
                    }
                }));
            div()
                .relative()
                .min_w_0()
                .max_w_full()
                .child(trigger)
                .when(ring, |el| {
                    el.child(
                        div()
                            .absolute()
                            .top(px(-2.))
                            .bottom(px(-2.))
                            .left(px(-2.))
                            .right(px(-2.))
                            .border_2()
                            .border_color(t.ring),
                    )
                })
                .children(self.render_draft_picker(&t, window, cx))
        };
        let title = |el: gpui_kit::Div| {
            el.w_full().text_size(title_size).line_height(rems(2.25)).text_color(t.foreground).justify_center()
        };
        let heading = match project.filter(|_| !is_scratch) {
            Some(project) => {
                let name = project.title.clone();
                if terse {
                    let size = if lg && heading_fits_inline(&name, width, window) { title_size } else { rems(1.) };
                    title(h_flex()).text_size(size).child(picker(name.clone().into(), Some(name.into()), cx))
                } else if lg && heading_fits_inline(&name, width, window) {
                    title(h_flex().gap(rems(0.5))).child(div().flex_none().child("What should we build in")).child(
                        h_flex()
                            .min_w_0()
                            .child(picker(name.clone().into(), Some(name.into()), cx))
                            .child(div().flex_none().child("?")),
                    )
                } else {
                    title(v_flex().items_center().gap(rems(0.5)))
                        .child(div().text_center().child("What should we build?"))
                        .child(
                            h_flex()
                                .max_w_full()
                                .justify_center()
                                .text_size(rems(1.))
                                .line_height(rems(1.5))
                                .child(picker(name.clone().into(), Some(name.into()), cx)),
                        )
                }
            }
            None if is_scratch => title(h_flex()).child("What should we work on?"),
            None if !self.shell.projects.is_empty() => {
                title(h_flex()).child(picker("Choose a project".into(), None, cx)).child("\u{a0}to start")
            }
            None => title(h_flex()).child("Add a project to start"),
        };
        let can_start_without = !is_scratch && !self.shell.projects.is_empty();
        let subline = scratch_env.map(|scratch_env| {
            let action = if is_scratch {
                Some(picker("No project".into(), None, cx).into_any_element())
            } else {
                can_start_without.then(|| {
                    let shortcut = self.shortcut_label("chat.newWithoutProject");
                    inline_button(
                        "start-without-project",
                        "or start without a project".into(),
                        t.muted_foreground,
                        t.foreground,
                    )
                    .when_some(shortcut, |el, shortcut| el.tooltip(headline_tooltip(shortcut.into())))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_draft_without_project(scratch_env.clone(), window, cx);
                    }))
                    .into_any_element()
                })
            };
            h_flex()
                .mt(rems(0.5))
                .h(rems(1.5))
                .w_full()
                .justify_center()
                .text_size(rems(0.875))
                .line_height(rems(1.25))
                .children(action)
        });
        v_flex().w_full().items_center().child(heading).children(subline).into_any_element()
    }

    /// `startScratch`: an empty draft opens the "No project" draft; one with content moves there.
    fn start_draft_without_project(&mut self, env: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(&env) else { return };
        let origin = self.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.ensureScratch", serde_json::json!({})).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let project_id = match result {
                    Ok(value) => value["projectId"].as_str().map(str::to_owned),
                    Err(error) => {
                        window.push_notification(
                            Notification::error(error.to_string()).title("Could not start without a project"),
                            cx,
                        );
                        return;
                    }
                };
                // A draft that moved on meanwhile stays where it is.
                let Some(project_id) = project_id.filter(|_| this.target == origin) else { return };
                let empty = this.composer.read(cx).value().trim().is_empty() && this.attachments.is_empty();
                if empty {
                    this.start_draft_in(env, project_id, window, cx);
                } else {
                    this.move_draft_to_project(env, project_id, window, cx);
                }
            });
        })
        .detach();
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

pub(super) fn render_row(
    row: &Row,
    this: &WeakEntity<Workspace>,
    lane_width: f32,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
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
            attribution,
        } => {
            let expanded =
                this.upgrade().is_some_and(|workspace| workspace.read(cx).expanded.contains(&timeline::entry_key(id)));
            let fold = folded_message(attribution.as_ref(), text, this, cx);
            let actions = message_actions::user_actions(this, id, text, created_at, revert.as_ref(), &t, cx);
            let body = markdown_with(this, id.clone(), text, MarkdownKind::User, None, &t, cx);
            user_row(
                id,
                text,
                body,
                actions,
                steer.filter(|_| !*continues_steer),
                UserRowLook {
                    grouped_with_previous: *grouped_with_previous,
                    grouped_with_next: *grouped_with_next,
                    expanded,
                    attribution: attribution.as_ref(),
                    fold,
                },
                this,
                &t,
            )
        }
        Row::AssistantMeta { id, text, updated_at, status, fork } => {
            let meta =
                message_actions::assistant_meta(this, id, text, false, updated_at, status, fork.as_ref(), &t, cx)
                    .opacity(0.)
                    .group_hover("timeline-meta", |style| style.opacity(1.));
            lane(16., v_flex().group("timeline-meta").child(meta)).into_any_element()
        }
        Row::Assistant { id, text, streaming, terminal, updated_at, status, fork, changes, .. } => {
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
            let changes = changes.as_ref().map(|c| super::changed_files::card(c, this, lane_width - 8., &t, cx));
            lane(
                if *terminal { 16. } else { 8. },
                v_flex()
                    .group("timeline-row")
                    .child(body)
                    .children(changes.map(|card| div().px(px(4.)).child(card)))
                    .children(meta),
            )
            .into_any_element()
        }
        Row::Plan { id, plan } => plan_card(id, plan, this, lane_width, cx, &t),
        Row::Work(work) => work_row(work, this, &t, cx),
        Row::Working { started_at } => {
            let label = match started_at.as_deref().and_then(crate::timestamp::parse_ms) {
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
        Row::Subagent(card) => lane(0., subagent_card(card, this, &t, cx)).into_any_element(),
        Row::SubagentGroup { key, cards, expanded } => {
            lane(0., subagent_group(key, cards, *expanded, this, &t, cx)).into_any_element()
        }
        Row::Embed(embed) => lane(8., embed_card(embed, this, &t)).into_any_element(),
        Row::CreatedThread { id, title, thread_id, .. } => {
            lane(8., created_thread_card(id, title.as_ref(), thread_id.as_ref(), this, &t)).into_any_element()
        }
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
        Row::Now(line) => super::thread_now::now_row(line, this, lane_width, &t, window, cx),
        Row::Workflow { row, expanded } => {
            lane(16., super::workflow::workflow_row(row, *expanded, this, lane_width, &t, cx)).into_any_element()
        }
        Row::WorktreeSetup { snapshot, embedded } => {
            super::worktree_setup::setup_row(snapshot, *embedded, this, &t, cx)
        }
        Row::System(divider) => lane(8., system_divider(divider, this, &t)).into_any_element(),
        Row::Request(card) => super::request_cards::request_row(card, this, &t, window, cx),
        Row::ModelSwitch { switch, .. } => {
            let label = this.upgrade().and_then(|workspace| {
                crate::handoff::model_switch_label(&switch.0, &switch.1, &workspace.read(cx).server_info().providers)
            });
            match label {
                Some(label) => {
                    let center = div().px(px(8.)).py(px(4.)).font_weight(FontWeight::MEDIUM).child(label);
                    lane(8., divider_frame(center.into_any_element(), t.muted_foreground, &t)).into_any_element()
                }
                None => div().into_any_element(),
            }
        }
        Row::Handoff(handoff) => lane(8., handoff_divider(handoff, this, &t, cx)).into_any_element(),
        Row::Approval { request_id, .. } => this
            .update(cx, |workspace, cx| {
                let approvals = workspace.open_projection().map(|p| p.pending_approvals()).unwrap_or_default();
                let (request, item) = approvals.into_iter().find(|(request, _)| request.id == request_id.as_ref())?;
                Some(workspace.render_approval(request, item, &t, cx))
            })
            .ok()
            .flatten()
            .map_or_else(|| div().into_any_element(), |approval| lane(0., approval).into_any_element()),
    }
}

/// `TimelineSystemDivider`'s frame: `center` between hairlines, in `text-2xs`.
fn divider_frame(center: AnyElement, color: Hsla, t: &Tokens) -> gpui_kit::Div {
    let line = || div().flex_1().h(px(1.)).bg(alpha(t.border, 0.7));
    h_flex()
        .min_w_0()
        .gap(px(8.))
        .py(px(8.))
        .text_size(px(11.))
        .text_color(color)
        .child(line())
        .child(center)
        .child(line())
}

/// `providerTextColor`: the provider's brand color for its name, where it has one.
fn provider_text_color(driver: &str, t: &Tokens) -> Option<Hsla> {
    match driver {
        "codex" => Some(t.foreground),
        "antigravity" => Some(rgb(0x5b87bf).into()),
        _ => crate::assets::provider_icon(driver).and_then(|(_, tint)| tint),
    }
}

/// `HandoffRow`: "Context handoff", then where it came from and went, each provider's icon and
/// model; red when the handoff failed.
fn handoff_divider(handoff: &timeline::HandoffRow, this: &WeakEntity<Workspace>, t: &Tokens, cx: &App) -> AnyElement {
    let workspace = this.upgrade();
    let providers = workspace.as_ref().map_or(&[][..], |w| w.read(cx).server_info().providers.as_slice());
    let endpoint = |index: usize, endpoint: &crate::handoff::Endpoint| {
        let (label, display) = crate::handoff::endpoint_label(endpoint, providers);
        let driver = providers
            .iter()
            .find(|provider| provider.instance_id == endpoint.instance_id)
            .map_or(endpoint.instance_id.as_str(), |provider| provider.driver.as_str());
        let icon = match crate::assets::provider_icon(driver) {
            Some((path, tint)) => Icon::empty().path(path).size(px(14.)).text_color(tint.unwrap_or(t.foreground)),
            None => Icon::new(IconName::Bot).size(px(14.)),
        };
        let tooltip: SharedString = format!("{display} · {label}").into();
        h_flex()
            .id(ElementId::Name(format!("handoff:{}:{index}", handoff.id).into()))
            .min_w_0()
            .gap(px(6.))
            .child(icon.flex_none())
            .child(
                div()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .when_some(provider_text_color(driver, t), gpui_kit::Styled::text_color)
                    .child(label),
            )
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
    };
    let count = handoff.endpoints.from.len();
    let mut detail = h_flex().min_w_0().flex_wrap().justify_center().gap(px(6.)).opacity(0.7);
    for (index, from) in handoff.endpoints.from.iter().enumerate() {
        detail =
            detail.child(endpoint(index, from).when(index + 1 < count, |el| el.child(div().ml(px(-4.)).child(","))));
    }
    if count > 0 {
        detail = detail.child(Icon::new(IconName::ArrowRight).size(px(12.)).flex_none());
    }
    detail = detail.child(endpoint(count, &handoff.endpoints.to));
    let center = h_flex()
        .min_w_0()
        .flex_wrap()
        .justify_center()
        .gap(px(6.))
        .px(px(8.))
        .py(px(4.))
        .child(Icon::new(IconName::ArrowRightLeft).size(px(12.)).flex_none())
        .child(div().font_weight(FontWeight::MEDIUM).child(if handoff.pending {
            "Context handoff pending"
        } else {
            "Context handoff"
        }))
        .child(detail);
    let color = if handoff.failed { t.error } else { t.muted_foreground };
    divider_frame(center.into_any_element(), color, t).into_any_element()
}

/// `TimelineSystemDivider`: icon, label and detail between hairlines; a pill when it opens a thread.
fn system_divider(divider: &timeline::SystemDivider, this: &WeakEntity<Workspace>, t: &Tokens) -> AnyElement {
    let icon = match divider.icon {
        timeline::DividerIcon::Stop => IconName::CircleStop,
        timeline::DividerIcon::Fork => IconName::GitFork,
        timeline::DividerIcon::Eraser => IconName::Eraser,
    };
    let content = |el: gpui_kit::Stateful<gpui_kit::Div>| {
        el.flex()
            .min_w_0()
            .items_center()
            .gap(px(6.))
            .child(Icon::new(icon).size(px(12.)).flex_none())
            .child(div().font_weight(FontWeight::MEDIUM).child(divider.label.clone()))
            .children(
                divider
                    .detail
                    .clone()
                    .map(|d| div().min_w_0().max_w(px(320.)).opacity(0.7).child(format!("·\u{a0}{d}"))),
            )
    };
    let center = match &divider.action {
        Some((label, thread_id)) => {
            let (this, thread_id, label) = (this.clone(), thread_id.clone(), label.clone());
            content(div().id(ElementId::Name(format!("divider:{}", divider.id).into())))
                .px(px(10.))
                .py(px(4.))
                .rounded_full()
                .border_1()
                .border_color(alpha(t.border, 0.7))
                .bg(t.background)
                .cursor_pointer()
                .hover(|style| style.bg(t.accent).text_color(t.foreground))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .on_click(move |_, window, cx| {
                    let thread_id = thread_id.to_string();
                    let _ = this.update(cx, |w, cx| w.select_thread(thread_id, window, cx));
                })
                .into_any_element()
        }
        None => content(div().id(ElementId::Name(format!("divider:{}", divider.id).into())))
            .flex_wrap()
            .justify_center()
            .px(px(8.))
            .py(px(4.))
            .into_any_element(),
    };
    divider_frame(center, t.muted_foreground, t).into_any_element()
}

/// "Open chat" on a created thread, "Open subagent" on a subagent's notification (`WorkEntryLogRow`).
fn entry_open_link(entry: &Entry, this: &WeakEntity<Workspace>, t: &Tokens) -> Option<AnyElement> {
    let raw = &entry.item.0.raw;
    let text = |pointer: &str| raw.pointer(pointer).and_then(serde_json::Value::as_str).filter(|v| !v.is_empty());
    let (label, thread_id, color) = match entry.item_type.as_ref() {
        "thread_created" => ("Open chat", text("/targetThreadId")?, t.primary),
        "notification" if matches!(text("/source/kind"), Some("subagent" | "delegated_task")) => {
            ("Open subagent", text("/source/childThreadId")?, t.foreground)
        }
        _ => return None,
    };
    let (this, thread_id) = (this.clone(), thread_id.to_owned());
    Some(
        div()
            .id(ElementId::Name(format!("open-thread:{}", entry.id).into()))
            .flex_none()
            .font_weight(FontWeight::MEDIUM)
            .text_color(color)
            .cursor_pointer()
            .hover(gpui_kit::Styled::underline)
            .child(label)
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                let thread_id = thread_id.clone();
                let _ = this.update(cx, |w, cx| w.select_thread(thread_id, window, cx));
            })
            .into_any_element(),
    )
}

/// `SubagentTimelineLink`: avatar, title and detail, status, elapsed time, and a
/// chevron when the subagent's thread can open.
pub(super) fn subagent_card(
    card: &timeline::SubagentCard,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
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
    let elapsed = card.started_at.as_deref().and_then(crate::timestamp::parse_ms).and_then(|start| {
        let end = match card.completed_at.as_deref().and_then(crate::timestamp::parse_ms) {
            Some(end) => end,
            None if card.live => now_ms(),
            None => return None,
        };
        Some(timeline::agent_elapsed(end - start))
    });
    let title = div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(t.foreground);
    let text = v_flex()
        .flex_1()
        .min_w_0()
        .child(
            h_flex().min_w_0().gap(px(8.)).text_xs().child(title.child(card.title.clone())).children(
                card.remote
                    .as_ref()
                    .map(|remote| div().flex_none().text_color(muted).child(format!("on {}", remote.label))),
            ),
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
    // A task in a linked environment opens there, if this client is connected to it.
    let connected = |env: &str| this.upgrade().is_some_and(|workspace| workspace.read(cx).session(env).is_some());
    let target = card.target(connected).map(|target| match target {
        timeline::SubagentTarget::Local(child) => (None, child.to_string()),
        timeline::SubagentTarget::Remote(remote) => (Some(remote.environment_id.clone()), remote.thread_id.to_string()),
    });
    match target {
        Some((env, thread_id)) => {
            let this = this.clone();
            row.cursor_pointer()
                .hover(|style| style.bg(alpha(t.accent, 0.2)))
                .child(Icon::new(IconName::ChevronRight).size(px(14.)).text_color(alpha(muted, 0.6)))
                .on_click(move |_, window, cx| {
                    let (env, thread_id) = (env.clone(), thread_id.clone());
                    let _ = this.update(cx, |workspace, cx| match env {
                        Some(env) => workspace.select_thread_in(env, thread_id, window, cx),
                        None => workspace.select_thread(thread_id, window, cx),
                    });
                })
                .into_any_element()
        }
        None => row.into_any_element(),
    }
}

/// `V2SubagentGroup`: stacked avatars, "3 subagents" with their statuses, the batch's time,
/// and the members' cards when open.
fn subagent_group(
    key: &SharedString,
    cards: &[Rc<timeline::SubagentCard>],
    expanded: bool,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
    let muted = t.muted_foreground;
    let (summary, active, failed) = timeline::subagent_group_status(cards);
    let (start, end, live) = timeline::subagent_group_timing(cards);
    let elapsed = start.and_then(|start| {
        let end = end.or(live.then(now_ms))?;
        Some(timeline::agent_elapsed(end - start))
    });
    let avatar = |card: &timeline::SubagentCard| {
        div()
            .size(px(24.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(t.secondary)
            .border_2()
            .border_color(t.background)
            .child(match crate::assets::provider_icon(&card.driver) {
                Some((path, tint)) => Icon::empty().path(path).size(px(14.)).text_color(tint.unwrap_or(t.foreground)),
                None => Icon::new(IconName::Bot).size(px(14.)).text_color(muted),
            })
    };
    let more = cards.len().saturating_sub(3);
    let header = h_flex()
        .id(ElementId::Name(key.clone()))
        .w_full()
        .min_w_0()
        .gap(px(10.))
        .py(px(8.))
        .pr(px(13.))
        .cursor_pointer()
        .text_color(if expanded || active { t.foreground } else { muted })
        .when(!expanded && !active, |el| el.opacity(0.55))
        .hover(|style| style.opacity(1.))
        .child(
            h_flex()
                .flex_none()
                .children(
                    cards.iter().take(3).enumerate().map(|(i, card)| avatar(card).when(i > 0, |el| el.ml(px(-6.)))),
                )
                .when(more > 0, |el| {
                    el.child(
                        div()
                            .ml(px(-6.))
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(t.muted)
                            .border_2()
                            .border_color(t.background)
                            .text_size(px(9.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(muted)
                            .child(format!("+{more}")),
                    )
                }),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_xs().font_weight(FontWeight::SEMIBOLD).child(format!("{} subagents", cards.len())))
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(if active {
                            t.info
                        } else if failed {
                            t.error
                        } else {
                            muted
                        })
                        .child(summary),
                ),
        )
        .children(elapsed.map(|e| div().flex_none().text_xs().text_color(muted).child(e)))
        .child(
            Icon::new(if expanded { IconName::ChevronUp } else { IconName::ChevronDown })
                .size(px(14.))
                .text_color(muted),
        )
        .on_click(toggle(this, key.clone()));
    v_flex()
        .w_full()
        .pb(px(8.))
        .child(header)
        .when(expanded, |el| {
            el.child(
                v_flex()
                    .mt(px(4.))
                    .mb(px(4.))
                    .p(px(4.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(alpha(t.border, 0.6))
                    .bg(alpha(t.card, 0.3))
                    .children(cards.iter().map(|card| subagent_card(card, this, t, cx))),
            )
        })
        .into_any_element()
}

/// A rendered page or MCP app. The web embeds it in a frame; here it opens in the browser.
/// `V2LifecycleRow`'s `resourceSummary` card: a thread the turn created, with Open chat.
fn created_thread_card(
    id: &SharedString,
    title: Option<&SharedString>,
    thread_id: Option<&SharedString>,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let open = thread_id.cloned().map(|thread_id| {
        let this = this.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            let thread_id = thread_id.to_string();
            let _ = this.update(cx, |workspace, cx| workspace.select_thread(thread_id, window, cx));
        }
    });
    h_flex()
        .w_full()
        .min_w_0()
        .gap(px(12.))
        .p(px(12.))
        .rounded(px(8.))
        .border_1()
        .border_color(alpha(t.border, 0.6))
        .child(
            div()
                .size(px(36.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .bg(alpha(t.muted, 0.6))
                .child(Icon::new(IconName::MessageSquare).size(px(16.)).text_color(t.muted_foreground)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(title.cloned().unwrap_or_else(|| "Created thread".into())),
        )
        .child(
            Button::new(ElementId::Name(format!("open-created:{id}").into()))
                .outline()
                .xsmall()
                .label("Open chat")
                .when_some(open, |button, open| button.on_click(open)),
        )
        .into_any_element()
}

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

use super::sidebar::now_ms;

/// `formatWorkingTimer`: whole seconds, then `formatDuration`.
fn working_duration(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds < 60 { format!("{seconds}s") } else { work_log::format_duration(seconds * 1000) }
}

// ---------------------------------------------------------------------------
// Messages

/// How a user message bubble sits among its neighbours, and how much of it shows.
struct UserRowLook<'a> {
    grouped_with_previous: bool,
    grouped_with_next: bool,
    expanded: bool,
    attribution: Option<&'a timeline::Attribution>,
    fold: Option<FoldedMessage>,
}

/// An agent's or an automation's message folds to one line: who sent it, then a summary
/// (`FoldedMessageHeader`).
struct FoldedMessage {
    icon: IconName,
    label: SharedString,
    summary: SharedString,
    link: Option<FoldLink>,
    /// An agent's message keeps its actions while folded; an automation's show once it opens.
    actions_when_folded: bool,
}

/// "Open thread" on an agent's message, "Open automation" on a scheduled task's.
enum FoldLink {
    Thread(SharedString),
    Automation,
}

/// `UserTimelineRow`'s "agent" fold and `AutomationMessageRow`, for the attributions that fold.
fn folded_message(
    attribution: Option<&timeline::Attribution>,
    text: &str,
    this: &WeakEntity<Workspace>,
    cx: &App,
) -> Option<FoldedMessage> {
    let workspace = this.upgrade();
    let workspace = workspace.as_ref().map(|workspace| workspace.read(cx));
    match attribution? {
        timeline::Attribution::Agent { sender_thread } => {
            // `senderShell?.title || "Agent"`.
            let title = sender_thread.as_ref().and_then(|sender| {
                let title = workspace?.shell_thread(sender)?.title.trim().to_owned();
                (!title.is_empty()).then_some(title)
            });
            Some(FoldedMessage {
                icon: IconName::MessagesSquare,
                label: title.unwrap_or_else(|| "Agent".into()).into(),
                summary: text
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or_default()
                    .to_owned()
                    .into(),
                link: sender_thread.clone().map(FoldLink::Thread),
                actions_when_folded: true,
            })
        }
        timeline::Attribution::Automation { scheduled_task_id } => {
            // The task's title, else the prompt's first line (`useScheduledTaskTitle`).
            let title = scheduled_task_id
                .as_ref()
                .and_then(|task| workspace?.scheduled_task_title(task))
                .map(SharedString::from);
            Some(FoldedMessage {
                icon: IconName::CalendarClock,
                label: "Automation".into(),
                summary: title.unwrap_or_else(|| text.trim().lines().next().unwrap_or_default().to_owned().into()),
                link: scheduled_task_id.as_ref().map(|_| FoldLink::Automation),
                actions_when_folded: false,
            })
        }
        timeline::Attribution::T3Code | timeline::Attribution::Guest { .. } => None,
    }
}

fn user_row(
    id: &SharedString,
    text: &SharedString,
    body: AnyElement,
    actions: gpui_kit::Div,
    steer: Option<Steer>,
    look: UserRowLook,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let UserRowLook { grouped_with_previous, grouped_with_next, expanded, attribution, fold } = look;
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
    if let Some(fold) = fold {
        return folded_row(id, body, actions, steer_label, fold, expanded, this, t);
    }
    let guest = match attribution {
        Some(timeline::Attribution::Guest { name }) => Some(name.clone()),
        _ => None,
    };
    // `bg-guest-surface`: `color-mix(in srgb, var(--guest) 14%, var(--message-surface))`.
    let surface =
        if guest.is_some() { theme::mix_srgb(t.message_surface, rgb(GUEST).into(), 0.14) } else { t.message_surface };
    let collapsible = should_collapse(text);
    let collapsed = collapsible && !expanded;
    let mut bubble = div()
        .relative()
        .max_w(relative(0.8))
        .p(px(12.))
        .rounded(px(16.))
        .bg(surface)
        .text_color(t.foreground)
        .text_size(px(TEXT))
        .line_height(px(LINE))
        .when(grouped_with_previous, |this| this.rounded_tr(px(6.)))
        .when(grouped_with_next, |this| this.rounded_br(px(6.)))
        .child(div().when(collapsed, |this| this.max_h(px(176.)).overflow_hidden()).child(body));
    if collapsed {
        bubble = bubble.child(div().absolute().left_0().right_0().bottom(px(12.)).h(px(28.)).bg(linear_gradient(
            180.,
            linear_color_stop(alpha(surface, 0.), 0.),
            linear_color_stop(surface, 1.),
        )));
    }
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
    let attribution = match (attribution, guest) {
        (Some(timeline::Attribution::T3Code), _) => Some(
            div()
                .mr(px(4.))
                .text_size(rems(0.6875))
                .text_color(alpha(t.muted_foreground, 0.7))
                .child("Sent by T3 Code"),
        ),
        // `GuestMessageName`.
        (_, Some(name)) => {
            Some(div().mr(px(4.)).text_size(rems(0.6875)).text_color(super::sidebar::guest_foreground(t)).child(name))
        }
        _ => None,
    };
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

/// `UserTimelineRow` with `attribution === "agent"`, and `AutomationMessageRow`: a one-line header
/// (`FoldedMessageHeader`) that opens to the message, capped at `max-h-96`, plus "Open thread" when
/// a thread sent it or "Open automation" for a scheduled task.
fn folded_row(
    id: &SharedString,
    body: AnyElement,
    actions: gpui_kit::Div,
    steer_label: Option<gpui_kit::Div>,
    fold: FoldedMessage,
    expanded: bool,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let muted = t.muted_foreground;
    let header = h_flex()
        .id(ElementId::Name(format!("agent-fold:{id}").into()))
        .w_full()
        .min_w_0()
        .gap(px(6.))
        .px(px(12.))
        .py(px(8.))
        .rounded(px(16.))
        .line_height(px(20.))
        .cursor_pointer()
        .child(Icon::new(fold.icon).size(px(16.)).flex_none().text_color(muted))
        .child(div().flex_none().max_w(relative(0.5)).truncate().text_color(muted).child(fold.label))
        .child(div().flex_1().min_w_0().truncate().child(fold.summary))
        .child(
            Icon::new(if expanded { IconName::ChevronUp } else { IconName::ChevronDown })
                .size(px(14.))
                .flex_none()
                .text_color(muted),
        )
        .on_click(toggle(this, timeline::entry_key(id)));
    let bubble = v_flex()
        .min_w_0()
        .max_w(relative(0.8))
        .rounded(px(16.))
        .bg(t.message_surface)
        .text_color(t.foreground)
        .text_size(px(TEXT))
        .line_height(px(LINE))
        .when(expanded, gpui_kit::Styled::w_full)
        .child(header)
        .when(expanded, |el| {
            el.child(
                div()
                    .id(ElementId::Name(format!("agent-fold-body:{id}").into()))
                    .px(px(12.))
                    .pb(px(12.))
                    .max_h(px(384.))
                    .overflow_y_scroll()
                    .child(body),
            )
        });
    let link = fold.link.map(|link| {
        let this = this.clone();
        let label = match link {
            FoldLink::Thread(_) => "Open thread",
            FoldLink::Automation => "Open automation",
        };
        div()
            .id(ElementId::Name(format!("agent-open-thread:{id}").into()))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(muted)
            .cursor_pointer()
            .hover(|s| s.text_color(t.foreground).underline())
            .child(label)
            .on_click(move |_, window, cx| {
                let _ = this.update(cx, |workspace, cx| match &link {
                    FoldLink::Thread(sender) => workspace.select_thread(sender.to_string(), window, cx),
                    FoldLink::Automation => workspace.open_settings("scheduled-tasks", cx),
                });
            })
    });
    let actions = (fold.actions_when_folded || expanded).then(|| {
        h_flex()
            .w_full()
            .max_w(relative(0.8))
            .justify_end()
            .gap(px(8.))
            .pr(px(4.))
            .opacity(0.)
            .group_hover("timeline-row", |style| style.opacity(1.))
            .children(link)
            .child(actions)
    });
    let column =
        v_flex().group("timeline-row").items_end().gap(px(4.)).children(steer_label).child(bubble).children(actions);
    lane(16., column).into_any_element()
}

fn markdown_view(
    state: &Entity<TextViewState>,
    kind: MarkdownKind,
    key: SharedString,
    wrap: bool,
    width: Option<f32>,
    this: &WeakEntity<Workspace>,
    (cwd, kinds): (Option<String>, Arc<super::markdown_ui::EntryKinds>),
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
    let (header_this, link_this, table_this) = (this.clone(), this.clone(), this.clone());
    let table_key = key.clone();
    TextView::new(state)
        .style(markdown_style(&t, kind == MarkdownKind::User, wrap))
        .inline_code_style(inline_code)
        .selectable(true)
        .stream_fade(streaming)
        // The user's typed newlines break lines (`lineBreaks`, remark-breaks).
        .when(kind == MarkdownKind::User, TextView::markdown_line_breaks)
        .code_block_actions(move |block, _, cx| {
            code_block_header(&header_this, &key, block, wrap, streaming, width, &t, cx)
        })
        .table_actions(move |table, _, cx| super::markdown_ui::table_actions(table, &table_this, &table_key, &t, cx))
        .plugin(super::markdown_ui::SkillChips { workspace: this.clone() })
        .plugin(super::markdown_ui::ThreadChips { workspace: this.clone() })
        .plugin(super::markdown_ui::FileChips {
            workspace: this.clone(),
            dark: t.dark,
            folder_color: t.sidebar.icon,
            cwd,
            kinds,
            suffixes: Default::default(),
        })
        .on_link_click(move |url, event, window, cx| {
            // A right click opens the link menu (`externalLinkContextMenu`), and only for web links.
            let right = matches!(event, ClickEvent::Mouse(click) if click.up.button == gpui_kit::MouseButton::Right);
            // A link to a file opens in the editor, or its file menu (`MarkdownFileLink`).
            if crate::thread_links::parse(url).is_none() {
                let target = link_this
                    .upgrade()
                    .and_then(|w| super::markdown_ui::file_link_target(url, w.read(cx).git_cwd().as_deref()));
                if let Some(target) = target {
                    let position = event.position();
                    let secondary = event.modifiers().secondary(cx);
                    // A relative link may name a file by its basename alone (`findWorkspaceFileMatch`).
                    let authored = url.split(['?', '#']).next().unwrap_or_default().replace("%20", " ");
                    let _ = link_this.update(cx, |workspace, cx| {
                        workspace.with_workspace_match(
                            &authored,
                            target,
                            window,
                            cx,
                            move |workspace, target, window, cx| {
                                if right {
                                    workspace.open_file_menu(target, position, window, cx);
                                } else if workspace.opens_file_link_in_browser(&target, secondary) {
                                    let path = super::markdown_ui::without_position(&target).to_owned();
                                    workspace.open_host_file_in_browser(&path, window, cx);
                                } else {
                                    workspace.open_file_link(&target, window, cx);
                                }
                            },
                        );
                    });
                    return;
                }
            }
            if right {
                let _ =
                    link_this.update(cx, |workspace, cx| workspace.open_link_menu(url, event.position(), window, cx));
                return;
            }
            let _ = link_this.update(cx, |workspace, cx| match crate::thread_links::parse(url) {
                // A thread no longer here opens nothing, as its chip shows.
                Some(_) => {
                    let message_environment = workspace.active_environment().map(ToString::to_string);
                    let destination = super::markdown_ui::thread_link_destination(
                        url,
                        message_environment.as_deref(),
                        &workspace.shell.threads,
                    );
                    if let Some((env, thread_id)) = destination {
                        workspace.select_thread_in(env, thread_id, window, cx);
                    }
                }
                // A project's pull request opens in its panel (`pullRequestLinksOpenInApp`); Ctrl-click and
                // every other link follow `browserLinkTarget`.
                None if !event.modifiers().secondary(cx) && workspace.open_change_request_link(url, cx) => {}
                None => workspace.open_link(url, event, window, cx),
            });
        })
}

/// `MarkdownCodeBlock`'s header: the language on the left; wrap and copy on the right.
/// `TextView` pins the actions 8px into the block's top-right corner over its own fill,
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
    // Its place in the source keeps two identical blocks apart and skips hashing every frame.
    let at = block.span.as_ref().map_or_else(|| fnv1a(&code), |span| span.start as u64);
    let id: SharedString = format!("code:{key}:{at:x}").into();
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
                        window.dispatch_action(
                            Box::new(super::RunInTerminal { cwd, command, stop_on_settle: false }),
                            cx,
                        );
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

/// FNV-1a, to tell apart code blocks that carry no source span.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3))
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
    plan: &timeline::PlanText,
    this: &WeakEntity<Workspace>,
    lane_width: f32,
    cx: &mut App,
    t: &Tokens,
) -> AnyElement {
    let key = timeline::entry_key(id);
    let expanded = this.upgrade().is_some_and(|workspace| workspace.read(cx).expanded.contains(&key));
    let collapsible = plan.preview.is_some();
    let (body_key, body_text): (SharedString, &SharedString) = match &plan.preview {
        Some(preview) if !expanded => (format!("{id}:preview").into(), preview),
        _ => (id.clone(), &plan.displayed),
    };
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
            body_key,
            body_text,
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
                .justify_between()
                .child(
                    h_flex().min_w_0().gap(px(8.)).child(badge).child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(TEXT))
                            .font_weight(FontWeight::MEDIUM)
                            .child(plan.title.clone()),
                    ),
                )
                .child(super::plan_actions::plan_actions_button(this, id, &plan.markdown, cx)),
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
        WorkRowKind::Thinking { group, expanded, .. } => group.is_some() && *expanded,
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
            .children(entries.iter().map(|detail| detail_entry(detail, this, t, cx)).collect::<Vec<_>>())
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
                        .child(work_line(
                            entry.icon(),
                            color,
                            entry_label(entry, label.clone(), (*expanded, false), this, t, cx),
                            muted,
                            None,
                        ))
                        .on_click(toggle(this, group.clone())),
                )
                .into_any_element()
        }
        WorkRowKind::Thinking { group, preview, .. } => {
            let label = preview.clone().unwrap_or_else(|| "Thinking".into());
            let line = work_line(WorkIcon::Brain, icon_color(t), label, muted, None);
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

/// Whether [`entry_body`] has anything, without building it.
fn has_body(entry: &Entry, label: &str) -> bool {
    let label = label.trim();
    let shows = |text: &str| {
        let text = text.trim();
        !text.is_empty() && text != label
    };
    let slash = |c: char| if c == '\\' { '/' } else { c };
    let files_show = match entry.changed_files.as_slice() {
        [] => false,
        [file] => {
            let file = file.trim();
            !file.is_empty() && !file.chars().map(slash).eq(label.chars())
        }
        _ => true,
    };
    entry.command.as_deref().is_some_and(shows) || entry.detail.as_deref().is_some_and(shows) || files_show
}

fn expandable(entry: &Entry, label: &str, this: &WeakEntity<Workspace>, cx: &App) -> bool {
    if entry.question.is_some() || this.upgrade().is_some_and(|w| w.read(cx).viewed_image(entry).is_some()) {
        true
    } else if entry.is_reasoning() {
        entry.detail.is_some()
    } else {
        super::inspector::has_inspector(entry) || has_body(entry, label)
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

/// The image a tool viewed, once it loads (`WorkLogDetails kind="media"`).
fn viewed_image(entry: &Entry, this: &WeakEntity<Workspace>, cx: &App) -> Option<AnyElement> {
    use gpui_kit::StyledImage as _;
    let workspace = this.upgrade()?;
    let workspace = workspace.read(cx);
    let image = workspace.markdown_images.get(&workspace.viewed_image(entry)?)?;
    Some(
        div()
            .mt(px(4.))
            .child(gpui_kit::img(image).max_w_full().max_h(px(256.)).object_fit(gpui_kit::ObjectFit::Contain))
            .into_any_element(),
    )
}

fn entry_details(
    entry: &Entry,
    label: &str,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &mut App,
) -> Option<AnyElement> {
    let image = viewed_image(entry, this, cx);
    let details = entry_text_details(entry, label, this, t, cx);
    match (image, details) {
        (Some(image), Some(details)) => Some(v_flex().child(image).child(details).into_any_element()),
        (image, details) => image.or(details),
    }
}

fn entry_text_details(
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
                .child(markdown(this, thought_key(&entry.id), &text, false, t, cx))
                .into_any_element(),
        );
    }
    if let Some(question) = &entry.question {
        return Some(question_history(&entry.id, question, t));
    }
    // `WorkLogDetails`: a click or a text selection inside leaves the row open.
    if let Some(inspector) = super::inspector::inspector(entry, this, t, cx) {
        return Some(
            div()
                .id(ElementId::Name(format!("inspector:{}", entry.id).into()))
                .mt(px(2.))
                .mb(px(6.))
                .pb(px(8.))
                .cursor_default()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(inspector)
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
            .cursor_text()
            .on_click(|_, _, cx| cx.stop_propagation())
            .font_family(t.fonts.mono)
            .text_size(px(13.))
            .line_height(relative(1.625))
            .text_color(alpha(t.foreground, 0.85))
            .child(gpui_kit::base::SelectableText::new("text", body))
            .into_any_element(),
    )
}

/// A row's label: an answered request names its questions, then what was answered, which
/// reads muted once the answers show below; a thread read names the thread, linked to it.
fn entry_label(
    entry: &Entry,
    label: SharedString,
    (expanded, linked): (bool, bool),
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
    if let Some(read) = thread_read_label(entry, &label, linked, this, t, cx) {
        return read;
    }
    let Some(question) = &entry.question else { return label.into_any_element() };
    let heading = if question.heading.is_empty() { label } else { question.heading.clone() };
    let Some(preview) = &question.preview else { return heading.into_any_element() };
    h_flex()
        .min_w_0()
        .gap(px(6.))
        .child(div().min_w_0().truncate().child(heading))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_color(if expanded { t.muted_foreground } else { t.foreground })
                .child(preview.clone()),
        )
        .into_any_element()
}

/// `ThreadReadLabel`: `Read thread “Title”` for a thread of this environment that is still listed.
fn thread_read_label(
    entry: &Entry,
    label: &str,
    linked: bool,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &App,
) -> Option<AnyElement> {
    let thread_id = entry.thread_read_target()?;
    let prefix = work_log::thread_read_prefix(label)?;
    let workspace = this.upgrade()?;
    let workspace = workspace.read(cx);
    let environment = workspace.active_environment()?;
    let thread = workspace.shell.threads.iter().find(|t| t.id == thread_id && t.environment_id == *environment)?;
    let title = thread.title.trim();
    if title.is_empty() || thread.archived_at.is_some() || thread.deleted_at.is_some() {
        return None;
    }
    let title = div().min_w_0().truncate().child(SharedString::from(title.to_owned()));
    let title = if linked {
        let (this, env, id) = (this.clone(), thread.environment_id.clone(), thread.id.clone());
        title
            .id(ElementId::Name(format!("read-thread:{}", entry.id).into()))
            .text_color(t.foreground)
            .cursor_pointer()
            .hover(gpui_kit::Styled::underline)
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                let (env, id) = (env.clone(), id.clone());
                let _ = this.update(cx, |w, cx| w.select_thread_in(env, id, window, cx));
            })
            .into_any_element()
    } else {
        title.into_any_element()
    };
    Some(
        h_flex()
            .min_w_0()
            .child(div().flex_none().child(format!("{prefix} “")))
            .child(title)
            .child(div().flex_none().child("”"))
            .into_any_element(),
    )
}

/// `QuestionAnswerHistory`: each question, its answer, and the files sent with it.
fn question_history(id: &str, question: &work_log::QuestionAnswer, t: &Tokens) -> AnyElement {
    let text = || div().text_size(px(TEXT)).line_height(px(LINE)).text_color(t.muted_foreground);
    v_flex()
        .id(ElementId::Name(format!("answers:{id}").into()))
        .ml(px(28.))
        .mt(px(8.))
        .gap(px(8.))
        .cursor_default()
        .on_click(|_, _, cx| cx.stop_propagation())
        .children(question.questions.iter().map(|q| {
            v_flex()
                .gap(px(4.))
                .children(q.text.clone().map(|question| text().child(question)))
                .children(q.answer.clone().map(|answer| text().ml(px(12.)).child(answer)))
                .when(!q.attachments.is_empty(), |el| {
                    el.child(
                        h_flex().flex_wrap().gap(px(8.)).children(
                            q.attachments
                                .iter()
                                .map(|name| text().text_color(t.foreground).underline().child(name.clone())),
                        ),
                    )
                })
        }))
        .into_any_element()
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
    let label_text = entry_label(entry, label_text, (expanded, true), this, t, cx);
    let label_color = if entry.is_tool_like() { muted } else { alpha(t.foreground, 0.8) };
    let details = if expanded { entry_details(entry, label, this, t, cx) } else { None };
    let expandable = expandable(entry, label, this, cx);
    let trailing =
        h_flex().gap(px(6.)).children(entry_open_link(entry, this, t)).child(chevron(expandable, expanded, t));
    let line = work_line(entry.icon(), color, label_text, label_color, Some(trailing.into_any_element()));
    let row = row.id(ElementId::Name(entry.id.clone())).child(line).children(details);
    if expandable {
        row.cursor_pointer().on_click(toggle(this, timeline::entry_key(&entry.id))).into_any_element()
    } else {
        row.into_any_element()
    }
}

/// An entry inside an open group (`WorkEntryLogRow`): no vertical padding.
fn detail_entry(detail: &DetailEntry, this: &WeakEntity<Workspace>, t: &Tokens, cx: &mut App) -> AnyElement {
    let DetailEntry { entry, label, expanded } = detail;
    let expanded = *expanded;
    let failed = entry.failed() && entry.status != Some(Status::Declined);
    let color = if failed { failed_icon_color(t) } else { icon_color(t) };
    let label_color = if entry.is_tool_like() { t.muted_foreground } else { alpha(t.foreground, 0.8) };
    let expandable = expandable(entry, label, this, cx);
    let shown: SharedString = if expanded && entry.is_reasoning() { "Thought".into() } else { label.clone() };
    let shown = entry_label(entry, shown, (expanded, true), this, t, cx);
    let details = if expanded { entry_details(entry, label, this, t, cx) } else { None };
    div()
        .id(ElementId::Name(format!("group-entry:{}", entry.id).into()))
        .w_full()
        .min_w_0()
        .px(px(2.))
        .rounded(px(6.))
        .when(expandable, gpui_kit::Styled::cursor_pointer)
        .child(work_line(
            entry.icon(),
            color,
            shown,
            label_color,
            Some(
                h_flex()
                    .gap(px(6.))
                    .children(entry_open_link(entry, this, t))
                    .child(chevron(expandable, expanded, t))
                    .into_any_element(),
            ),
        ))
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
}
