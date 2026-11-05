//! Writing from the panel: the comment and review composer (`PullRequestComposer`,
//! `PullRequestCommentForm`, `PullRequestReviewForm`), line comments drafted in the Code tab
//! (`DiffCommentAnnotation`, `pullRequestReviewStore`), replies, resolving, edits
//! (`PullRequestMarkdownEditor`, `PullRequestTitleEditor`), reactions (`PullRequestReactions`)
//! and the reviewer and label pickers (`PullRequestCandidatePicker`).

use std::cell::Cell;

use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::popover::Popover;

use super::*;
use crate::pull_request_detail::Reaction;
use crate::pull_request_diff::Position;

/// `PULL_REQUEST_REACTION_ORDER`, with each reaction's emoji and name.
const REACTIONS: [(&str, &str, &str); 8] = [
    ("thumbs-up", "👍", "thumbs up"),
    ("thumbs-down", "👎", "thumbs down"),
    ("laugh", "😄", "laugh"),
    ("hooray", "🎉", "hooray"),
    ("confused", "😕", "confused"),
    ("heart", "❤️", "heart"),
    ("rocket", "🚀", "rocket"),
    ("eyes", "👀", "eyes"),
];

/// `VERDICTS`: the value, its label and the toast once sent.
const VERDICTS: [(&str, &str); 3] =
    [("comment", "Comment"), ("approve", "Approve"), ("request-changes", "Request changes")];

/// A line comment waiting for the review (`PendingReviewComment`).
#[derive(Clone, Debug)]
pub(in crate::app) struct PendingComment {
    id: u64,
    path: String,
    old_path: Option<String>,
    position: Position,
    start: Option<Position>,
    body: String,
}

impl PendingComment {
    /// `PullRequestReviewCommentDraft`.
    fn to_json(&self) -> Value {
        let mut value = json!({"path": self.path, "position": self.position.to_json(), "body": self.body});
        if let Some(old) = &self.old_path {
            value["oldPath"] = json!(old);
        }
        if let Some(start) = self.start {
            value["startPosition"] = start.to_json();
        }
        value
    }

    pub(super) fn anchor(&self) -> (crate::pull_request_diff::Side, u64) {
        self.position.anchor()
    }

    pub(super) fn path(&self) -> &str {
        &self.path
    }
}

/// An unsent review, kept per pull request across panels (`usePullRequestReviewStore`).
#[derive(Default)]
pub(in crate::app) struct ReviewDraft {
    comments: Vec<PendingComment>,
    summary: Option<Entity<TextareaState>>,
    next_id: u64,
}

/// A line comment being written under its line in the Code tab.
pub(super) struct LineDraft {
    pub(super) path: String,
    old_path: Option<String>,
    position: Position,
    start: Option<Position>,
    input: Entity<TextareaState>,
}

impl LineDraft {
    pub(super) fn anchor(&self) -> (crate::pull_request_diff::Side, u64) {
        self.position.anchor()
    }
}

/// An edit in progress: a comment (`kind` is its endpoint) or the description.
struct Editing {
    subject: String,
    kind: &'static str,
    input: Entity<TextareaState>,
    preview: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerKind {
    Reviewers,
    Labels,
}

struct Picker {
    kind: PickerKind,
    search: Entity<InputState>,
    candidates: Option<Result<Value, String>>,
    pending: Option<String>,
}

#[derive(Default)]
pub(super) struct WriteState {
    composer_open: bool,
    review_mode: bool,
    verdict: Option<&'static str>,
    comment: Option<Entity<TextareaState>>,
    submitting: Option<&'static str>,
    reply: Option<(String, Entity<TextareaState>)>,
    editing: Option<Editing>,
    saving: bool,
    thread_pending: bool,
    /// Reactions pressed and not yet read back, by subject and content.
    pub(super) reactions: HashMap<(String, String), bool>,
    reaction_picker: Option<String>,
    pub(super) draft: Option<LineDraft>,
    picker: Option<Picker>,
    title: Option<Entity<InputState>>,
    _watch: Vec<Subscription>,
}

/// `applyPendingPullRequestReactions`: pressed reactions shown before the host confirms them.
pub(super) fn apply_pending(reactions: &[Reaction], pending: &[(String, bool)]) -> Vec<Reaction> {
    let mut shown: Vec<Reaction> = reactions.to_vec();
    for (content, reacted) in pending {
        match shown.iter().position(|r| &r.content == content) {
            None if *reacted => shown.push(Reaction { content: content.clone(), count: 1, viewer_has_reacted: true }),
            None => {}
            Some(index) if shown[index].viewer_has_reacted != *reacted => {
                let count = shown[index].count as i64 + if *reacted { 1 } else { -1 };
                if count <= 0 {
                    shown.remove(index);
                } else {
                    shown[index].count = count as u64;
                    shown[index].viewer_has_reacted = *reacted;
                }
            }
            Some(_) => {}
        }
    }
    shown.sort_by_key(|r| REACTIONS.iter().position(|(c, _, _)| *c == r.content).unwrap_or(usize::MAX));
    shown
}

/// `canSubmit`: a review needs a summary or line comments, except an approval.
pub(super) fn can_submit_review(verdict: &str, summary: &str, comments: usize, summary_required: bool) -> bool {
    if verdict == "request-changes" && summary_required {
        !summary.trim().is_empty()
    } else {
        verdict == "approve" || !summary.trim().is_empty() || comments > 0
    }
}

/// Forgejo needs a summary to request changes (`reviewSummaryRequired`).
fn summary_required(provider: &str) -> bool {
    provider == "forgejo"
}

fn text_of(input: &Entity<TextareaState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

/// The group an edit pencil appears on hover in.
pub(super) const EDIT_GROUP: &str = "pr-edit";

/// Shown while the pointer is over its `EDIT_GROUP` (`PullRequestEditButton`).
pub(super) fn hover_only(child: impl IntoElement) -> AnyElement {
    div().flex_none().opacity(0.).group_hover(EDIT_GROUP, |s| s.opacity(1.)).child(child).into_any_element()
}

/// Notifies the workspace only when an input turns empty or stops being so, which is all the
/// buttons beside it read; Ctrl+Enter runs `submit`.
fn watch(
    input: &Entity<TextareaState>,
    submit: Option<fn(&mut Workspace, &mut Window, &mut Context<Workspace>)>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Subscription {
    let empty = Rc::new(Cell::new(true));
    cx.subscribe_in(input, window, move |this, input, event: &InputEvent, window, cx| match event {
        InputEvent::Change => {
            let now = input.read(cx).value().trim().is_empty();
            if empty.replace(now) != now {
                cx.notify();
            }
        }
        InputEvent::PressEnter { secondary: true, .. } => {
            if let Some(submit) = submit {
                submit(this, window, cx);
            }
        }
        _ => {}
    })
}

impl Workspace {
    fn pr_write(&mut self) -> Option<&mut WriteState> {
        self.pull_requests.panel.as_mut().map(|p| &mut p.write)
    }

    /// The write state of the pull request `key` names, shown or parked.
    fn pr_write_of(&mut self, key: &str) -> Option<&mut WriteState> {
        self.pr_panel(key).map(|p| &mut p.write)
    }

    fn pr_review(&mut self) -> Option<&mut ReviewDraft> {
        let key = self.pull_requests.panel.as_ref()?.reference.key();
        Some(self.pull_requests.reviews.entry(key).or_default())
    }

    pub(super) fn pr_pending_comments(&self) -> Vec<PendingComment> {
        self.pull_requests
            .panel
            .as_ref()
            .and_then(|p| self.pull_requests.reviews.get(&p.reference.key()))
            .map(|r| r.comments.clone())
            .unwrap_or_default()
    }

    /// Runs one write RPC for the shown pull request; success reads it again, failure toasts
    /// `failure`. `done` gets that pull request's key, which stays right if another tab shows meanwhile.
    fn run_pr_write(
        &mut self,
        method: &'static str,
        input: Value,
        failure: String,
        hint: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Workspace, &str, bool, &mut Window, &mut Context<Workspace>) + 'static,
    ) {
        let (Some(client), Some(key)) = (self.pull_request_client(), self.shown_pr_key()) else { return };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(method, input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match &result {
                    Ok(_) => this.read_pr_detail_of(&key, cx),
                    Err(error) => {
                        let message = match hint {
                            Some(hint) => logic::readable_failure(&list_logic::error_message(error), hint),
                            None => list_logic::error_message(error),
                        };
                        toast_error(window, cx, failure, message);
                    }
                }
                done(this, &key, result.is_ok(), window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn pr_ref_json(&self) -> Option<Value> {
        self.pull_requests.panel.as_ref().map(|p| p.reference.to_json())
    }

    // The composer.

    fn toggle_pr_composer(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        let started = self.pr_review().is_some_and(|r| {
            !r.comments.is_empty() || r.summary.as_ref().is_some_and(|s| !text_of(s, cx).trim().is_empty())
        });
        if open {
            self.ensure_pr_composer_inputs(window, cx);
        }
        if let Some(write) = self.pr_write() {
            if open {
                write.review_mode = started;
            }
            write.composer_open = open;
        }
        if open {
            self.sync_pr_review_placeholder(window, cx);
        }
        cx.notify();
    }

    /// "Finish review": the composer opens on its review.
    pub(super) fn open_pr_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_pr_composer(true, window, cx);
        if let Some(write) = self.pr_write() {
            write.review_mode = true;
        }
        cx.notify();
    }

    fn ensure_pr_composer_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pr_write().is_some_and(|w| w.comment.is_none()) {
            let input = cx.new(|cx| TextareaState::new(window, cx).rows(3).placeholder("Leave a comment"));
            let sub = watch(&input, Some(|this, window, cx| this.post_pr_comment("comment", window, cx)), window, cx);
            if let Some(write) = self.pr_write() {
                write.comment = Some(input);
                write._watch.push(sub);
            }
        }
        if self.pr_review().is_some_and(|r| r.summary.is_none()) {
            let input = cx.new(|cx| TextareaState::new(window, cx).rows(3));
            let sub = watch(&input, None, window, cx);
            if let Some(review) = self.pr_review() {
                review.summary = Some(input);
            }
            if let Some(write) = self.pr_write() {
                write._watch.push(sub);
            }
        }
    }

    /// The summary's placeholder says whether this verdict needs it.
    fn sync_pr_review_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let Some(detail) = panel.detail.clone() else { return };
        let verdict = panel.write.verdict;
        let pending = self.pr_pending_comments().len();
        let Some(summary) = self.pr_review().and_then(|r| r.summary.clone()) else { return };
        let offered: Vec<&str> = VERDICTS
            .iter()
            .map(|v| v.0)
            .filter(|v| detail.capabilities.review.verdicts.iter().any(|c| c == v))
            .filter(|v| detail.viewer_permissions.verdicts.iter().any(|c| c == v))
            .collect();
        let selected = offered.iter().copied().find(|v| Some(*v) == verdict).or_else(|| offered.first().copied());
        let required = summary_required(&detail.provider);
        let placeholder = match selected {
            None => "Summarize your review (optional)",
            Some(v) if can_submit_review(v, "", pending, required) => "Summarize your review (optional)",
            Some("request-changes") if required => "Summarize your review (required to request changes)",
            _ => "Summarize your review or add line comments",
        };
        summary.update(cx, |input, cx| input.set_placeholder(placeholder, window, cx));
    }

    /// `PullRequestCommentForm.submit`: a comment, optionally followed by closing or reopening.
    fn post_pr_comment(&mut self, action: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let Some(input) = panel.write.comment.clone() else { return };
        let body = text_of(&input, cx).trim().to_owned();
        if body.is_empty() || panel.write.submitting.is_some() || panel.pending.is_some() {
            return;
        }
        let Some(mut payload) = self.pr_ref_json() else { return };
        payload["body"] = json!(body);
        if let Some(write) = self.pr_write() {
            write.submitting = Some(action);
        }
        self.run_pr_write(
            "pullRequests.comment",
            payload,
            "Could not post the comment".into(),
            None,
            window,
            cx,
            move |this, key, ok, window, cx| {
                let Some(panel) = this.pr_panel(key) else { return };
                panel.write.submitting = None;
                if !ok {
                    return;
                }
                panel.write.composer_open = false;
                if let Some(input) = panel.write.comment.clone() {
                    input.update(cx, |input, cx| input.set_value("", window, cx));
                }
                if action != "comment" {
                    this.perform_pr_action_on(key, action, None, None, window, cx);
                }
            },
        );
        cx.notify();
    }

    /// `PullRequestReviewForm.submit`.
    fn submit_pr_review(&mut self, verdict: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        if panel.write.submitting.is_some() {
            return;
        }
        let term = logic::terminology(panel.provider()).0;
        let key = panel.reference.key();
        let Some(mut payload) = self.pr_ref_json() else { return };
        let Some(review) = self.pr_review() else { return };
        let summary = review.summary.as_ref().map(|s| text_of(s, cx)).unwrap_or_default();
        let sent: Vec<u64> = review.comments.iter().map(|c| c.id).collect();
        payload["verdict"] = json!(verdict);
        payload["body"] = json!(summary);
        payload["comments"] = Value::Array(review.comments.iter().map(PendingComment::to_json).collect());
        if let Some(write) = self.pr_write() {
            write.submitting = Some("review");
        }
        let toast = match verdict {
            "approve" => format!("{} approved", logic::capitalize(term)),
            "request-changes" => "Changes requested".into(),
            _ => "Review submitted".into(),
        };
        self.run_pr_write(
            "pullRequests.submitReview",
            payload,
            "The review could not be submitted".into(),
            Some("The host refused the review. Your draft is still here to try again."),
            window,
            cx,
            move |this, _, ok, window, cx| {
                if let Some(panel) = this.pr_panel(&key) {
                    panel.write.submitting = None;
                    if ok {
                        panel.write.composer_open = false;
                    }
                }
                if !ok {
                    return;
                }
                // Comments and summary edits made while it was sending stay.
                if let Some(review) = this.pull_requests.reviews.get_mut(&key) {
                    review.comments.retain(|c| !sent.contains(&c.id));
                    if let Some(input) = review.summary.clone()
                        && text_of(&input, cx) == summary
                    {
                        input.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                }
                toast_success(window, cx, toast);
            },
        );
        cx.notify();
    }

    /// The floating composer button and its popover, bottom right of the panel.
    pub(super) fn render_pr_composer(
        &mut self,
        detail: &Detail,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let term = logic::terminology(&detail.provider).0;
        let can_comment = detail.capabilities.comment && detail.viewer_permissions.comment;
        let verdicts: Vec<(&'static str, &'static str)> = VERDICTS
            .iter()
            .copied()
            .filter(|(v, _)| detail.capabilities.review.verdicts.iter().any(|c| c == v))
            .filter(|(v, _)| detail.viewer_permissions.verdicts.iter().any(|c| c == v))
            .collect();
        if !can_comment && verdicts.is_empty() {
            return None;
        }
        let pending = self.pr_pending_comments();
        let panel = self.pull_requests.panel.as_ref()?;
        let write = &panel.write;
        let open = write.composer_open;
        let review_mode = if can_comment { !verdicts.is_empty() && write.review_mode } else { true };
        let view = cx.entity();
        let badge = (!pending.is_empty()).then(|| {
            div()
                .absolute()
                .top(px(-4.))
                .right(px(-4.))
                .h(px(14.))
                .min_w(px(14.))
                .px(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(t.info)
                .text_size(px(9.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(gpui_kit::white())
                .child(pending.len().to_string())
        });
        let tip: SharedString = if !pending.is_empty() {
            format!("Review {term}, {} pending", pending.len())
        } else if !can_comment {
            format!("Review {term}")
        } else {
            format!("Comment on {term}")
        }
        .into();
        let trigger = Button::new("pr-composer")
            .outline()
            .bg(t.background)
            .size(px(36.))
            .p_0()
            .rounded(px(8.))
            .tooltip(tip)
            .child(Icon::new(Lucide::MessageSquare).with_size(px(16.)))
            .children(badge);
        let body =
            open.then(|| self.render_pr_composer_body(detail, can_comment, &verdicts, review_mode, &pending, t, cx));
        Some(
            Popover::new("pr-composer-popover")
                .anchor(gpui_kit::Anchor::BottomRight)
                .open(open)
                .on_open_change(move |open, window, cx| {
                    let open = *open;
                    view.update(cx, |this, cx| this.toggle_pr_composer(open, window, cx));
                })
                .trigger(trigger)
                .w(px(448.))
                .p(px(16.))
                .children(body)
                .into_any_element(),
        )
    }

    fn render_pr_composer_body(
        &self,
        detail: &Detail,
        can_comment: bool,
        verdicts: &[(&'static str, &'static str)],
        review_mode: bool,
        pending: &[PendingComment],
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let term = logic::terminology(&detail.provider).0;
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let write = &panel.write;
        let submitting = write.submitting;
        let review_label = if pending.is_empty() { "Review".to_owned() } else { format!("Review ({})", pending.len()) };
        let heading = if can_comment && !verdicts.is_empty() {
            let pick = |mode: bool, label: String, cx: &mut Context<Self>| {
                let pressed = review_mode == mode;
                div()
                    .id(if mode { "pr-composer-review" } else { "pr-composer-comment" })
                    .h(px(24.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .text_color(if pressed { t.foreground } else { t.muted_foreground })
                    .when(pressed, |el| el.bg(Hsla { a: t.input.a * 0.72, ..t.input }))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(write) = this.pr_write() {
                            write.review_mode = mode;
                        }
                        cx.notify();
                    }))
            };
            h_flex()
                .p(px(2.))
                .gap(px(2.))
                .rounded(px(8.))
                .bg(Hsla { a: t.input.a * 0.4, ..t.input })
                .child(pick(false, "Comment".into(), cx))
                .child(pick(true, review_label, cx))
                .into_any_element()
        } else {
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(if review_mode { format!("Review {term}") } else { format!("Comment on {term}") })
                .into_any_element()
        };
        let discard = (review_mode && !pending.is_empty()).then(|| {
            ghost_button("pr-composer-discard", t)
                .tooltip("Discard pending line comments")
                .disabled(submitting.is_some())
                .child(Icon::new(Lucide::Trash).with_size(px(14.)))
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(review) = this.pr_review() {
                        review.comments.clear();
                    }
                    cx.notify();
                }))
        });
        let close = ghost_button("pr-composer-close", t)
            .tooltip("Close composer")
            .child(Icon::new(Lucide::X).with_size(px(14.)))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_pr_composer(false, window, cx)));
        let header = h_flex()
            .mb(px(12.))
            .justify_between()
            .gap(px(8.))
            .child(heading)
            .child(h_flex().gap(px(4.)).children(discard).child(close));
        let form = if review_mode {
            self.render_pr_review_form(detail, verdicts, pending.len(), t, cx)
        } else {
            self.render_pr_comment_form(detail, t, cx)
        };
        v_flex().child(header).child(form).into_any_element()
    }

    fn render_pr_comment_form(&self, detail: &Detail, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let write = &panel.write;
        let Some(input) = write.comment.clone() else { return div().into_any_element() };
        let busy = write.submitting.is_some() || panel.pending.is_some();
        let empty = text_of(&input, cx).trim().is_empty();
        let follow_up = if detail.state == list_logic::State::Open && logic::can(detail, "close") {
            Some("close")
        } else if detail.state == list_logic::State::Closed && logic::can(detail, "reopen") {
            Some("reopen")
        } else {
            None
        };
        let follow = follow_up.map(|action| {
            let label = match (action, write.submitting, panel.armed) {
                ("close", Some("close"), _) => "Closing...",
                ("close", _, Some("comment-close")) => "Confirm close?",
                ("close", _, _) => "Close with comment",
                (_, Some("reopen"), _) => "Reopening...",
                _ => "Reopen with comment",
            };
            let button = xs_outline("pr-comment-follow-up", t)
                .disabled(empty || busy)
                .child(
                    Icon::new(if action == "close" {
                        Lucide::GitPullRequestClosed
                    } else {
                        Lucide::GitPullRequestArrow
                    })
                    .with_size(px(14.)),
                )
                .child(label);
            let button = if action == "close" {
                button.text_color(t.error).border_color(Hsla { a: 0.4, ..t.error })
            } else {
                button
            };
            button.on_click(cx.listener(move |this, _, window, cx| {
                if action == "reopen" {
                    return this.post_pr_comment("reopen", window, cx);
                }
                // The inline confirm: the first press arms, a second within three seconds acts.
                let armed = this.pull_requests.panel.as_ref().is_some_and(|p| p.armed == Some("comment-close"));
                if armed {
                    if let Some(panel) = this.pull_requests.panel.as_mut() {
                        panel.armed = None;
                    }
                    this.post_pr_comment("close", window, cx);
                } else {
                    this.arm_pr_confirm("comment-close", cx);
                }
            }))
        });
        v_flex()
            .gap(px(8.))
            .child(Textarea::new(&input).disabled(busy))
            .child(
                h_flex().flex_wrap().justify_end().gap(px(8.)).children(follow).child(
                    xs_outline("pr-comment-send", t)
                        .disabled(empty || busy)
                        .child(Icon::new(Lucide::Send).with_size(px(14.)))
                        .child(if write.submitting == Some("comment") { "Posting..." } else { "Comment" })
                        .on_click(cx.listener(|this, _, window, cx| this.post_pr_comment("comment", window, cx))),
                ),
            )
            .into_any_element()
    }

    fn render_pr_review_form(
        &self,
        detail: &Detail,
        verdicts: &[(&'static str, &'static str)],
        pending: usize,
        _t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let Some(summary) = self.pull_requests.reviews.get(&panel.reference.key()).and_then(|r| r.summary.clone())
        else {
            return div().into_any_element();
        };
        let required = summary_required(&detail.provider);
        let selected = verdicts
            .iter()
            .copied()
            .find(|(v, _)| Some(*v) == panel.write.verdict)
            .or_else(|| verdicts.first().copied());
        let text = text_of(&summary, cx);
        let busy = panel.write.submitting.is_some();
        let view = cx.entity();
        let verdict_icon = |v: &str| match v {
            "approve" => Lucide::Check,
            "request-changes" => Lucide::CircleX,
            _ => Lucide::MessageSquare,
        };
        let picker = (verdicts.len() > 1).then(|| {
            let options = verdicts.to_vec();
            let (value, label) = selected.unwrap_or(("comment", "Comment"));
            Button::new("pr-review-verdict")
                .outline()
                .xsmall()
                .mr_auto()
                .disabled(busy)
                .child(Icon::new(verdict_icon(value)).with_size(px(12.)))
                .child(label)
                .child(Icon::new(Lucide::ChevronDown).with_size(px(12.)))
                .dropdown_menu(move |mut menu: PopupMenu, window, _| {
                    for (value, label) in &options {
                        let value: &'static str = value;
                        menu = menu.item(PopupMenuItem::new(*label).icon(verdict_icon(value)).on_click(
                            window.listener_for(&view, move |this, _, window, cx| {
                                if let Some(write) = this.pr_write() {
                                    write.verdict = Some(value);
                                }
                                this.sync_pr_review_placeholder(window, cx);
                                cx.notify();
                            }),
                        ));
                    }
                    menu
                })
        });
        let can = selected.is_some_and(|(v, _)| can_submit_review(v, &text, pending, required));
        v_flex()
            .child(Textarea::new(&summary))
            .child(
                h_flex().mt(px(8.)).justify_end().gap(px(8.)).children(picker).child(
                    Button::new("pr-review-submit")
                        .primary()
                        .xsmall()
                        .disabled(busy || !can)
                        .child(if busy { "Submitting..." } else { "Submit review" })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Some((verdict, _)) = selected {
                                this.submit_pr_review(verdict, window, cx);
                            }
                        })),
                ),
            )
            .into_any_element()
    }

    /// Arms an inline confirm for three seconds (`useInlineActionConfirm`).
    fn arm_pr_confirm(&mut self, action: &'static str, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        panel.armed = Some(action);
        let key = panel.reference.key();
        panel._disarm = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CONFIRM_WINDOW).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(panel) = this.pr_panel(&key) {
                    panel.armed = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    // Line comments in the Code tab.

    /// `beginComment`: a line or selection gets a draft under its last line.
    pub(super) fn begin_pr_line_comment(
        &mut self,
        path: String,
        start: (crate::pull_request_diff::Side, u64),
        end: (crate::pull_request_diff::Side, u64),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let Some(detail) = panel.detail.clone() else { return };
        let Some(code) = panel.code.as_ref() else { return };
        if code.commit_selected() || panel.write.draft.is_some() {
            return;
        }
        if !(detail.capabilities.review.inline_comment && detail.viewer_permissions.comment) {
            return;
        }
        let ranged = detail.capabilities.review.range_comment == Some(true);
        let Some(file) = code.patch(&path) else { return };
        let old_path = file.old_path.clone().filter(|old| *old != path);
        let Some((position, start)) = crate::pull_request_diff::review_anchor(file, start, end, ranged) else { return };
        let input = cx.new(|cx| TextareaState::new(window, cx).rows(3).placeholder("Leave a comment"));
        input.update(cx, |input, cx| input.focus(window, cx));
        let sub = watch(&input, Some(|this, _, cx| this.add_pr_line_comment(cx)), window, cx);
        if let Some(write) = self.pr_write() {
            write.draft = Some(LineDraft { path, old_path, position, start, input });
            write._watch.push(sub);
        }
        cx.notify();
    }

    fn add_pr_line_comment(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.pr_write().and_then(|w| w.draft.take()) else { return };
        let body = text_of(&draft.input, cx);
        if body.trim().is_empty() {
            if let Some(write) = self.pr_write() {
                write.draft = Some(draft);
            }
            return;
        }
        if let Some(review) = self.pr_review() {
            review.next_id += 1;
            review.comments.push(PendingComment {
                id: review.next_id,
                path: draft.path,
                old_path: draft.old_path,
                position: draft.position,
                start: draft.start,
                body: body.trim().to_owned(),
            });
        }
        cx.notify();
    }

    /// "Add to agent": the drafted lines and request go to a new thread instead of the review.
    fn send_pr_draft_to_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.pr_write().and_then(|w| w.draft.take()) else { return };
        let request = text_of(&draft.input, cx);
        let lines = match (draft.start.map(Position::anchor), draft.position.anchor()) {
            (Some((_, from)), (_, to)) if from != to => format!("L{from}-L{to}"),
            (_, (side, line)) => {
                format!("L{line}{}", if side == crate::pull_request_diff::Side::Left { " (before)" } else { "" })
            }
        };
        self.add_pr_selection_to_agent(draft.path, lines, request, window, cx);
    }

    /// `DiffCommentAnnotation kind="draft"`.
    pub(super) fn render_pr_line_draft(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let draft = self.pull_requests.panel.as_ref()?.write.draft.as_ref()?;
        let (_, line) = draft.position.anchor();
        let empty = text_of(&draft.input, cx).trim().is_empty();
        Some(
            v_flex()
                .mx(px(12.))
                .my(px(8.))
                .p(px(12.))
                .gap(px(8.))
                .rounded(px(12.))
                .border_1()
                .border_color(Hsla { a: t.border.a * 0.7, ..t.border })
                .bg(t.background)
                .shadow_sm()
                .child(
                    div()
                        .text_xs()
                        .font_family(FONT_MONO)
                        .text_color(t.muted_foreground)
                        .child(format!("{}:{line}", draft.path)),
                )
                .child(Textarea::new(&draft.input))
                .child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(ghost_button("pr-draft-cancel", t).child("Cancel").on_click(cx.listener(
                            |this, _, _, cx| {
                                if let Some(write) = this.pr_write() {
                                    write.draft = None;
                                }
                                cx.notify();
                            },
                        )))
                        .child(
                            xs_outline("pr-draft-agent", t)
                                .disabled(empty)
                                .child("Add to agent")
                                .on_click(cx.listener(|this, _, window, cx| this.send_pr_draft_to_agent(window, cx))),
                        )
                        .child(
                            Button::new("pr-draft-add")
                                .primary()
                                .xsmall()
                                .disabled(empty)
                                .child("Add to review")
                                .on_click(cx.listener(|this, _, _, cx| this.add_pr_line_comment(cx))),
                        ),
                )
                .into_any_element(),
        )
    }

    /// `PendingReviewCommentCard`.
    pub(super) fn render_pr_pending_comment(
        &self,
        comment: &PendingComment,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = comment.id;
        v_flex()
            .mx(px(12.))
            .my(px(8.))
            .p(px(12.))
            .rounded(px(12.))
            .border_1()
            .border_dashed()
            .border_color(Hsla { a: t.border.a * 0.7, ..t.border })
            .bg(t.background)
            .text_sm()
            .child(
                h_flex()
                    .gap(px(8.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(Icon::new(Lucide::MessageSquare).with_size(px(14.)))
                    .child("Pending. Sent when you submit the review")
                    .child(
                        ghost_button(ElementId::Name(format!("pr-pending-discard:{id}").into()), t)
                            .ml_auto()
                            .tooltip("Discard this comment")
                            .child(Icon::new(Lucide::Trash).with_size(px(14.)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(review) = this.pr_review() {
                                    review.comments.retain(|c| c.id != id);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .child(div().mt(px(8.)).text_color(t.foreground).child(comment.body.clone()))
            .into_any_element()
    }

    // Threads: reply and resolve.

    pub(super) fn render_pr_thread_actions(
        &self,
        thread: &crate::pull_request_detail::ReviewThread,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel = self.pull_requests.panel.as_ref()?;
        let detail = panel.detail.as_ref()?;
        let resolve = detail.capabilities.review.resolve && logic::can_resolve_thread(detail, thread);
        resolve.then(|| {
            let (id, resolved) = (thread.id.clone(), thread.is_resolved);
            ghost_button(ElementId::Name(format!("pr-thread-resolve:{id}").into()), t)
                .disabled(panel.write.thread_pending)
                .child(if resolved { "Unresolve" } else { "Resolve" })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_pr_thread_resolved(id.clone(), !resolved, window, cx);
                }))
                .into_any_element()
        })
    }

    /// "Fix in a thread" on a review conversation (`onFixFinding`).
    pub(super) fn render_pr_fix_thread(
        &self,
        thread: &crate::pull_request_detail::ReviewThread,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel = self.pull_requests.panel.as_ref()?;
        let detail = panel.detail.as_ref()?;
        self.shell.projects.iter().find(|p| p.id == detail.project_id)?;
        let key = format!("finding:thread:{}", thread.id);
        let preparing = panel.handoff.as_deref() == Some(key.as_str());
        let thread = thread.clone();
        Some(
            ghost_button(ElementId::Name(format!("pr-thread-fix:{}", thread.id).into()), t)
                .ml_auto()
                .disabled(panel.write.thread_pending || panel.handoff.is_some())
                .child(Icon::new(Lucide::Hammer).with_size(px(12.)))
                .child(if preparing { "Preparing..." } else { "Fix in a thread" })
                .on_click(cx.listener(move |this, _, window, cx| this.fix_pr_thread(&thread, window, cx)))
                .into_any_element(),
        )
    }

    fn set_pr_thread_resolved(
        &mut self,
        thread_id: String,
        resolved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut input) = self.pr_ref_json() else { return };
        let Some(write) = self.pr_write().filter(|w| !w.thread_pending) else { return };
        write.thread_pending = true;
        input["threadId"] = json!(thread_id);
        input["resolved"] = json!(resolved);
        self.run_pr_write(
            "pullRequests.setThreadResolution",
            input,
            "The conversation could not be updated".into(),
            None,
            window,
            cx,
            |this, key, _, _, _| {
                if let Some(write) = this.pr_write_of(key) {
                    write.thread_pending = false;
                }
            },
        );
    }

    /// The Reply button, or the open reply box, under a thread.
    pub(super) fn render_pr_thread_reply(
        &self,
        thread: &crate::pull_request_detail::ReviewThread,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel = self.pull_requests.panel.as_ref()?;
        let detail = panel.detail.as_ref()?;
        if !(detail.capabilities.review.reply && detail.viewer_permissions.comment) {
            return None;
        }
        let id = thread.id.clone();
        match &panel.write.reply {
            Some((open, input)) if *open == thread.id => {
                let empty = text_of(input, cx).trim().is_empty();
                let pending = panel.write.thread_pending;
                Some(
                    v_flex()
                        .mt(px(8.))
                        .gap(px(8.))
                        .child(Textarea::new(input))
                        .child(
                            h_flex()
                                .justify_end()
                                .gap(px(8.))
                                .child(ghost_button("pr-reply-cancel", t).child("Cancel").on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if let Some(write) = this.pr_write() {
                                            write.reply = None;
                                        }
                                        cx.notify();
                                    },
                                )))
                                .child(
                                    Button::new("pr-reply-send")
                                        .primary()
                                        .xsmall()
                                        .disabled(pending || empty)
                                        .child("Reply")
                                        .on_click(cx.listener(|this, _, window, cx| this.send_pr_reply(window, cx))),
                                ),
                        )
                        .into_any_element(),
                )
            }
            _ => Some(
                ghost_button(ElementId::Name(format!("pr-thread-reply:{id}").into()), t)
                    .mt(px(8.))
                    .child("Reply")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let input = cx.new(|cx| TextareaState::new(window, cx).rows(2).placeholder("Reply"));
                        input.update(cx, |input, cx| input.focus(window, cx));
                        let sub = watch(&input, Some(crate::app::Workspace::send_pr_reply), window, cx);
                        if let Some(write) = this.pr_write() {
                            write.reply = Some((id.clone(), input));
                            write._watch.push(sub);
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            ),
        }
    }

    fn send_pr_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut payload) = self.pr_ref_json() else { return };
        let Some(write) = self.pr_write().filter(|w| !w.thread_pending) else { return };
        let Some((thread_id, input)) = write.reply.clone() else { return };
        let body = text_of(&input, cx).trim().to_owned();
        if body.is_empty() {
            return;
        }
        if let Some(write) = self.pr_write() {
            write.thread_pending = true;
        }
        payload["threadId"] = json!(thread_id);
        payload["body"] = json!(body);
        self.run_pr_write(
            "pullRequests.replyToThread",
            payload,
            "Reply could not be posted".into(),
            None,
            window,
            cx,
            |this, key, ok, _, _| {
                if let Some(write) = this.pr_write_of(key) {
                    write.thread_pending = false;
                    if ok {
                        write.reply = None;
                    }
                }
            },
        );
    }

    // Edits: comments and the description.

    /// The pencil that opens an edit (`PullRequestEditButton`).
    pub(super) fn pr_edit_button(
        &self,
        subject: String,
        kind: &'static str,
        body: String,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if kind == "description" { "Edit description" } else { "Edit comment" };
        let button = ghost_button(ElementId::Name(format!("pr-edit:{subject}").into()), t)
            .size(px(20.))
            .p_0()
            .tooltip(label)
            .child(Icon::new(Lucide::Pencil).with_size(px(12.)))
            .on_click(cx.listener(move |this, _, window, cx| {
                let input = cx.new(|cx| TextareaState::new(window, cx).rows(6).default_value(body.clone()));
                input.update(cx, |input, cx| input.focus(window, cx));
                let sub = watch(&input, Some(crate::app::Workspace::save_pr_edit), window, cx);
                if let Some(write) = this.pr_write() {
                    write.editing = Some(Editing { subject: subject.clone(), kind, input, preview: false });
                    write._watch.push(sub);
                }
                cx.notify();
            }));
        hover_only(button)
    }

    pub(super) fn pr_editing(&self, subject: &str) -> bool {
        self.pull_requests.panel.as_ref().and_then(|p| p.write.editing.as_ref()).is_some_and(|e| e.subject == subject)
    }

    fn save_pr_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut payload) = self.pr_ref_json() else { return };
        let Some(write) = self.pr_write().filter(|w| !w.saving) else { return };
        let Some(editing) = write.editing.as_ref() else { return };
        let body = text_of(&editing.input, cx);
        let description = editing.kind == "description";
        if body.trim().is_empty() && !description {
            return;
        }
        let (method, failure) = if description {
            payload["body"] = json!(body);
            ("pullRequests.update", "Could not save the description")
        } else {
            payload["commentId"] = json!(editing.subject);
            payload["kind"] = json!(editing.kind);
            payload["body"] = json!(body);
            ("pullRequests.updateComment", "Could not save the comment")
        };
        write.saving = true;
        self.run_pr_write(method, payload, failure.into(), None, window, cx, |this, key, ok, _, _| {
            if let Some(write) = this.pr_write_of(key) {
                write.saving = false;
                if ok {
                    write.editing = None;
                }
            }
        });
    }

    /// `PullRequestMarkdownEditor`: Write and Preview, then Cancel and Save.
    pub(super) fn render_pr_editor(&mut self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editing = self.pull_requests.panel.as_ref()?.write.editing.as_ref()?;
        let (input, preview, description, saving) = (
            editing.input.clone(),
            editing.preview,
            editing.kind == "description",
            self.pull_requests.panel.as_ref()?.write.saving,
        );
        let text = text_of(&input, cx);
        let empty = text.trim().is_empty();
        let mode = |value: bool, label: &'static str, cx: &mut Context<Self>| {
            let pressed = preview == value;
            div()
                .id(if value { "pr-editor-preview" } else { "pr-editor-write" })
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .text_color(if pressed { t.foreground } else { t.muted_foreground })
                .when(pressed, |el| el.bg(Hsla { a: t.input.a * 0.72, ..t.input }))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(editing) = this.pr_write().and_then(|w| w.editing.as_mut()) {
                        editing.preview = value;
                    }
                    cx.notify();
                }))
        };
        let body = if preview {
            let rendered = (!empty).then(|| self.pr_markdown("editor-preview", &text, cx)).flatten();
            div()
                .rounded(px(8.))
                .border_1()
                .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                .px(px(12.))
                .py(px(8.))
                .child(match rendered {
                    Some(state) => pr_markdown_view(&state, t, cx.entity()).into_any_element(),
                    None => {
                        div().text_xs().text_color(t.muted_foreground).child("Nothing to preview.").into_any_element()
                    }
                })
                .into_any_element()
        } else {
            Textarea::new(&input).disabled(saving).into_any_element()
        };
        Some(
            v_flex()
                .w_full()
                .gap(px(8.))
                .child(
                    h_flex()
                        .p(px(2.))
                        .gap(px(2.))
                        .rounded(px(8.))
                        .bg(Hsla { a: t.input.a * 0.4, ..t.input })
                        .child(mode(false, "Write", cx))
                        .child(mode(true, "Preview", cx)),
                )
                .child(body)
                .child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(ghost_button("pr-editor-cancel", t).disabled(saving).child("Cancel").on_click(
                            cx.listener(|this, _, _, cx| {
                                if let Some(write) = this.pr_write() {
                                    write.editing = None;
                                }
                                cx.notify();
                            }),
                        ))
                        .child(
                            xs_outline("pr-editor-save", t)
                                .disabled(saving || (empty && !description))
                                .child(if saving { "Saving..." } else { "Save" })
                                .on_click(cx.listener(|this, _, window, cx| this.save_pr_edit(window, cx))),
                        ),
                )
                .into_any_element(),
        )
    }

    // The title (`PullRequestTitleEditor`).

    pub(super) fn begin_pr_title_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.as_ref()).map(|d| d.title.clone())
        else {
            return;
        };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        input.update(cx, |input, cx| input.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } => this.save_pr_title(window, cx),
            InputEvent::Blur => {
                if let Some(write) = this.pr_write().filter(|w| !w.saving) {
                    write.title = None;
                    cx.notify();
                }
            }
            _ => {}
        });
        if let Some(write) = self.pr_write() {
            write.title = Some(input);
            write._watch.push(sub);
        }
        cx.notify();
    }

    fn save_pr_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.pull_requests.panel.as_ref().and_then(|p| p.detail.as_ref()).map(|d| d.title.clone());
        let Some(mut payload) = self.pr_ref_json() else { return };
        let Some(write) = self.pr_write().filter(|w| !w.saving) else { return };
        let Some(input) = write.title.clone() else { return };
        let title = input.read(cx).value().trim().to_owned();
        if title.is_empty() || Some(&title) == current.as_ref() {
            write.title = None;
            cx.notify();
            return;
        }
        write.saving = true;
        payload["title"] = json!(title);
        self.run_pr_write(
            "pullRequests.update",
            payload,
            "Could not save the title".into(),
            None,
            window,
            cx,
            |this, key, _, _, _| {
                if let Some(write) = this.pr_write_of(key) {
                    write.saving = false;
                    write.title = None;
                }
            },
        );
    }

    pub(super) fn render_pr_title_input(&self, t: &Tokens) -> Option<AnyElement> {
        let input = self.pull_requests.panel.as_ref()?.write.title.as_ref()?;
        Some(
            div()
                .min_w_0()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.foreground)
                .child(Input::new(input).small())
                .into_any_element(),
        )
    }

    // Reactions.

    /// `PullRequestReactionBar` for a comment (`subject`) or the description (`None`).
    pub(super) fn render_pr_reactions(
        &self,
        subject: Option<&str>,
        reactions: &[Reaction],
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel = self.pull_requests.panel.as_ref()?;
        let can_react = panel.detail.as_ref().is_some_and(|d| d.capabilities.reactions == Some(true));
        let key = subject.unwrap_or("").to_owned();
        let pending: Vec<(String, bool)> =
            panel.write.reactions.iter().filter(|((s, _), _)| *s == key).map(|((_, c), r)| (c.clone(), *r)).collect();
        let shown = apply_pending(reactions, &pending);
        if shown.is_empty() && !can_react {
            return None;
        }
        let pill = |id: String| {
            h_flex()
                .id(ElementId::Name(id.into()))
                .h(px(24.))
                .flex_none()
                .gap(px(4.))
                .px(px(8.))
                .rounded_full()
                .border_1()
                .text_xs()
        };
        let mut bar = h_flex().min_w_0().flex_wrap().gap(px(4.));
        for reaction in &shown {
            let (emoji, name) = REACTIONS.iter().find(|r| r.0 == reaction.content).map_or(("", ""), |r| (r.1, r.2));
            let content = reaction.content.clone();
            let next = !reaction.viewer_has_reacted;
            let subject = subject.map(str::to_owned);
            let tip: SharedString = format!("{} reacted with {name} emoji", reaction.count).into();
            bar = bar.child(
                pill(format!("pr-reaction:{key}:{content}"))
                    .map(|el| {
                        if reaction.viewer_has_reacted {
                            el.border_color(Hsla { a: 0.6, ..t.primary })
                                .bg(Hsla { a: 0.1, ..t.primary })
                                .text_color(t.foreground)
                        } else {
                            el.border_color(Hsla { a: t.border.a * 0.7, ..t.border })
                                .bg(Hsla { a: t.muted.a * 0.4, ..t.muted })
                                .text_color(t.muted_foreground)
                        }
                    })
                    .when(can_react, |el| el.cursor_pointer().hover(|s| s.border_color(Hsla { a: 0.6, ..t.primary })))
                    .child(emoji)
                    .child(reaction.count.to_string())
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .when(can_react, |el| {
                        el.on_click(cx.listener(move |this, _, window, cx| {
                            this.set_pr_reaction(subject.clone(), content.clone(), next, window, cx);
                        }))
                    }),
            );
        }
        if can_react {
            let open = panel.write.reaction_picker.as_deref() == Some(key.as_str());
            let view = cx.entity();
            let picker_key = key.clone();
            let reacted: Vec<(String, bool)> =
                shown.iter().map(|r| (r.content.clone(), r.viewer_has_reacted)).collect();
            let subject = subject.map(str::to_owned);
            let trigger = Button::new(ElementId::Name(format!("pr-react:{key}").into()))
                .ghost()
                .xsmall()
                .h(px(24.))
                .px(px(6.))
                .rounded_full()
                .border_1()
                .border_color(Hsla { a: t.border.a * 0.7, ..t.border })
                .tooltip("Add a reaction")
                .child(Icon::new(Lucide::FaceSlightlySmilingPlus).with_size(px(14.)).text_color(t.muted_foreground));
            let body = open.then(|| {
                h_flex()
                    .gap(px(2.))
                    .children(REACTIONS.iter().map(|(content, emoji, _)| {
                        let pressed = reacted.iter().any(|(c, r)| c == content && *r);
                        let (subject, content) = (subject.clone(), (*content).to_owned());
                        div()
                            .id(ElementId::Name(format!("pr-react-pick:{content}").into()))
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .text_base()
                            .cursor_pointer()
                            .hover(|s| s.bg(t.accent))
                            .when(pressed, |el| el.bg(Hsla { a: 0.1, ..t.primary }))
                            .child(*emoji)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(write) = this.pr_write() {
                                    write.reaction_picker = None;
                                }
                                this.set_pr_reaction(subject.clone(), content.clone(), !pressed, window, cx);
                            }))
                    }))
                    .into_any_element()
            });
            bar = bar.child(
                Popover::new(ElementId::Name(format!("pr-react-popover:{key}").into()))
                    .anchor(gpui_kit::Anchor::BottomLeft)
                    .open(open)
                    .on_open_change(move |open, _, cx| {
                        let (open, key) = (*open, picker_key.clone());
                        view.update(cx, |this, cx| {
                            if let Some(write) = this.pr_write() {
                                write.reaction_picker = open.then_some(key);
                            }
                            cx.notify();
                        });
                    })
                    .trigger(trigger)
                    .p(px(4.))
                    .children(body),
            );
        }
        Some(bar.into_any_element())
    }

    fn set_pr_reaction(
        &mut self,
        subject: Option<String>,
        content: String,
        reacted: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(mut payload) = self.pr_ref_json() else { return };
        if let Some(subject) = &subject {
            payload["subjectId"] = json!(subject);
        }
        payload["content"] = json!(content);
        payload["reacted"] = json!(reacted);
        let key = (subject.unwrap_or_default(), content);
        if let Some(write) = self.pr_write() {
            write.reactions.insert(key.clone(), reacted);
        }
        self.run_pr_write(
            "pullRequests.setReaction",
            payload,
            "The reaction could not be saved".into(),
            None,
            window,
            cx,
            move |this, pr, ok, _, _| {
                if !ok && let Some(write) = this.pr_write_of(pr) {
                    write.reactions.remove(&key);
                }
            },
        );
    }

    // Reviewers and labels (`PullRequestReviewerPicker`, `PullRequestLabelPicker`).

    fn open_pr_picker(&mut self, kind: PickerKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.pull_request_client() else { return };
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let (key, input) = (panel.reference.key(), panel.reference.to_json());
        let placeholder = if kind == PickerKind::Reviewers { "Search people with access" } else { "Search labels" };
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        search.update(cx, |input, cx| input.focus(window, cx));
        let sub = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        });
        if let Some(write) = self.pr_write() {
            write.picker = Some(Picker { kind, search, candidates: None, pending: None });
            write._watch.push(sub);
        }
        let method = if kind == PickerKind::Reviewers {
            "pullRequests.reviewerCandidates"
        } else {
            "pullRequests.labelCandidates"
        };
        cx.spawn(async move |this, cx| {
            let result = client.request(method, input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(picker) = this.pr_panel(&key).and_then(|p| p.write.picker.as_mut()).filter(|p| p.kind == kind)
                else {
                    return;
                };
                picker.candidates = Some(result.map_err(|error| list_logic::error_message(&error)));
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn pick_pr_candidate(&mut self, kind: PickerKind, candidate: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut payload) = self.pr_ref_json() else { return };
        let Some(picker) = self.pr_write().and_then(|w| w.picker.as_mut()).filter(|p| p.pending.is_none()) else {
            return;
        };
        let (method, failure, hint, id) = if kind == PickerKind::Reviewers {
            let login = candidate["login"].as_str().unwrap_or_default().to_owned();
            let requested = candidate["isRequested"].as_bool() == Some(true);
            payload["reviewers"] = json!([{"id": candidate["id"], "kind": candidate["kind"]}]);
            payload["requested"] = json!(!requested);
            let failure = if requested {
                format!("Could not take back the review request to {login}")
            } else {
                format!("Could not ask {login} for a review")
            };
            (
                "pullRequests.requestReviewers",
                failure,
                "The host refused it. Check that you have write access on this repository, and that they still have access to it.",
                candidate["id"].as_str().unwrap_or_default().to_owned(),
            )
        } else {
            let name = candidate["name"].as_str().unwrap_or_default().to_owned();
            let applied = candidate["isApplied"].as_bool() == Some(true);
            payload["labels"] = json!([name]);
            payload["applied"] = json!(!applied);
            let failure =
                if applied { format!("Could not take {name} off") } else { format!("Could not put {name} on") };
            (
                "pullRequests.setLabels",
                failure,
                "The host refused it. Check that you have triage access on this repository.",
                name,
            )
        };
        picker.pending = Some(id);
        self.run_pr_write(method, payload, failure, Some(hint), window, cx, move |this, key, ok, window, cx| {
            if let Some(picker) = this.pr_write_of(key).and_then(|w| w.picker.as_mut()) {
                picker.pending = None;
            }
            // `open_pr_picker` reads for the shown pull request; a parked one's ticks read again when reopened.
            if ok && this.shown_pr_key().as_deref() == Some(key) {
                // The list reads again so the ticks follow the host.
                this.open_pr_picker(kind, window, cx);
            }
        });
    }

    /// The icon button beside Reviewers or Labels and its searchable list.
    pub(super) fn render_pr_picker(
        &self,
        reviewers: bool,
        allowed: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = if reviewers { PickerKind::Reviewers } else { PickerKind::Labels };
        let (icon, label, refused) = if reviewers {
            (Lucide::UserPlus, "Request a review", "Asking someone to review needs write access on this repository")
        } else {
            (Lucide::Tag, "Change labels", "Changing labels needs triage access on this repository")
        };
        let id = if reviewers { "pr-pick-reviewers" } else { "pr-pick-labels" };
        let trigger = ghost_button(id, t)
            .size(px(20.))
            .p_0()
            .tooltip(if allowed { label } else { refused })
            .child(Icon::new(icon).with_size(px(14.)));
        if !allowed {
            return trigger.disabled(true).into_any_element();
        }
        let picker = self.pull_requests.panel.as_ref().and_then(|p| p.write.picker.as_ref()).filter(|p| p.kind == kind);
        let open = picker.is_some();
        let body = picker.map(|picker| self.render_pr_picker_list(picker, t, cx));
        let view = cx.entity();
        Popover::new(ElementId::Name(format!("{id}-popover").into()))
            .anchor(gpui_kit::Anchor::TopLeft)
            .open(open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                view.update(cx, |this, cx| {
                    if open {
                        this.open_pr_picker(kind, window, cx);
                    } else if let Some(write) = this.pr_write() {
                        write.picker = None;
                        cx.notify();
                    }
                });
            })
            .trigger(trigger)
            .w(px(288.))
            .p_0()
            .children(body)
            .into_any_element()
    }

    fn render_pr_picker_list(&self, picker: &Picker, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let reviewers = picker.kind == PickerKind::Reviewers;
        let query = picker.search.read(cx).value().to_lowercase();
        let note =
            |text: String| div().p(px(8.)).text_xs().text_color(t.muted_foreground).child(text).into_any_element();
        let rows: Vec<AnyElement> = match &picker.candidates {
            None => vec![note("Loading...".into())],
            Some(Err(_)) => vec![note(
                if reviewers { "The people with access could not be read." } else { "The labels could not be read." }
                    .into(),
            )],
            Some(Ok(value)) => {
                let all = value["candidates"].as_array().cloned().unwrap_or_default();
                let text = |c: &Value, key: &str| c[key].as_str().unwrap_or_default().to_lowercase();
                let matching: Vec<Value> = all
                    .iter()
                    .filter(|c| {
                        query.is_empty()
                            || if reviewers {
                                text(c, "login").contains(&query) || text(c, "name").contains(&query)
                            } else {
                                text(c, "name").contains(&query) || text(c, "description").contains(&query)
                            }
                    })
                    .cloned()
                    .collect();
                let mut rows: Vec<AnyElement> = Vec::new();
                if all.is_empty() {
                    rows.push(note(
                        if reviewers {
                            "Nobody else has access to this repository."
                        } else {
                            "This repository has no labels."
                        }
                        .into(),
                    ));
                } else if matching.is_empty() {
                    rows.push(note(
                        if reviewers { "Nobody with access matches that." } else { "No label matches that." }.into(),
                    ));
                }
                for candidate in matching {
                    let key = if reviewers {
                        format!(
                            "{}:{}",
                            candidate["kind"].as_str().unwrap_or_default(),
                            candidate["id"].as_str().unwrap_or_default()
                        )
                    } else {
                        candidate["name"].as_str().unwrap_or_default().to_owned()
                    };
                    let checked =
                        candidate[if reviewers { "isRequested" } else { "isApplied" }].as_bool() == Some(true);
                    let content: AnyElement = if reviewers {
                        let actor: Actor = serde_json::from_value(candidate.clone()).unwrap_or_default();
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.))
                            .child(actor_label(Some(&actor), t))
                            .when(candidate["kind"] == "team", |el| {
                                el.child(div().text_color(t.muted_foreground).child("team"))
                            })
                            .into_any_element()
                    } else {
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .child(label_chip(
                                candidate["name"].as_str().unwrap_or_default(),
                                candidate["color"].as_str(),
                                0,
                                t,
                            ))
                            .into_any_element()
                    };
                    let busy = picker.pending.is_some();
                    let kind = picker.kind;
                    rows.push(
                        h_flex()
                            .id(ElementId::Name(format!("pr-candidate:{key}").into()))
                            .gap(px(8.))
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(4.))
                            .text_xs()
                            .when(!busy, |el| el.cursor_pointer().hover(|s| s.bg(t.accent)))
                            .child(content)
                            .when(checked, |el| el.child(Icon::new(Lucide::Check).with_size(px(14.))))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_pr_candidate(kind, candidate.clone(), window, cx);
                            }))
                            .into_any_element(),
                    );
                }
                if value["truncated"].as_bool() == Some(true) {
                    rows.push(note(
                        if reviewers {
                            "This repository has more people with access than are listed here. Ask for the rest on the host."
                        } else {
                            "This repository has more labels than are listed here. Apply the rest on the host."
                        }
                        .into(),
                    ));
                }
                rows
            }
        };
        v_flex()
            .child(div().p(px(8.)).border_b_1().border_color(t.border).child(Input::new(&picker.search).small()))
            .child(v_flex().id("pr-candidates").max_h(px(288.)).overflow_y_scroll().p(px(4.)).children(rows))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reaction(content: &str, count: u64, mine: bool) -> Reaction {
        Reaction { content: content.into(), count, viewer_has_reacted: mine }
    }

    #[test]
    fn pressed_reactions_show_before_the_host_confirms_them() {
        let held = [reaction("heart", 2, false), reaction("thumbs-up", 1, true)];
        let shown = apply_pending(&held, &[("heart".into(), true), ("thumbs-up".into(), false), ("eyes".into(), true)]);
        let shape: Vec<(&str, u64, bool)> =
            shown.iter().map(|r| (r.content.as_str(), r.count, r.viewer_has_reacted)).collect();
        assert_eq!(
            shape,
            [("heart", 3, true), ("eyes", 1, true)],
            "a last reaction taken back disappears; order is the host's"
        );
        assert_eq!(
            apply_pending(&held, &[("eyes".into(), false)]).len(),
            2,
            "taking back one never given changes nothing"
        );
    }

    #[test]
    fn a_review_needs_a_summary_or_comments_unless_it_approves() {
        assert!(can_submit_review("approve", "", 0, false));
        assert!(!can_submit_review("comment", "  ", 0, false));
        assert!(can_submit_review("comment", "", 2, false));
        assert!(can_submit_review("request-changes", "", 1, false));
        assert!(!can_submit_review("request-changes", "", 1, true), "Forgejo wants a summary to request changes");
    }

    #[test]
    fn a_pending_comment_sends_its_paths_and_range() {
        let comment = PendingComment {
            id: 1,
            path: "b.rs".into(),
            old_path: Some("a.rs".into()),
            position: Position::Added { new: 4 },
            start: Some(Position::Added { new: 2 }),
            body: "hm".into(),
        };
        assert_eq!(
            comment.to_json(),
            json!({
                "path": "b.rs", "oldPath": "a.rs", "body": "hm",
                "position": {"kind": "added", "newLine": 4}, "startPosition": {"kind": "added", "newLine": 2},
            })
        );
    }
}
