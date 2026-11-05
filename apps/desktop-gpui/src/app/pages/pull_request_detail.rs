//! The pull request detail panel beside the Pull requests list (`PullRequestDetailPanel`,
//! `PullRequestDetailLayout`, `PullRequestDetailCrumbs`, `PullRequestDetailActions`,
//! `PullRequestActionsMenu`, `PullRequestSummaryTab`, `PullRequestTimelineTab`, `PullRequestGuideTab`).
//! It reads `pullRequests.detail` and `pullRequests.activity`, and acts through `pullRequests.runAction`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogClose, DialogEntrance, DialogFooter};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, ElementId, Entity, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement as _, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, list, px, rgb,
};
use serde_json::{Value, json};

use super::super::Workspace;
use super::super::sidebar::now_ms;
use super::pull_requests::{label_chip, outline_button, relative_time, tone};
use crate::inbox::parse_ms;
use crate::pull_request_detail::{
    self as logic, Activity, Actor, Check, Comment, Conversation, Detail, Event, EventKind, Primary, Reference,
    ReviewOutcome, TimelineRow,
};
use crate::pull_requests::{self as list_logic, Entry};
use crate::rpc::RpcError;
use crate::theme::{self, FONT_MONO, Tokens};

mod avatars;
mod code;
mod guide;
mod handoff;
mod header;
mod live;
pub(in crate::app) mod write;

const CONVERSATION_PAGE: usize = 10;
/// How long an inline "Confirm …?" stays armed (`useInlineActionConfirm`).
const CONFIRM_WINDOW: Duration = Duration::from_secs(3);
const COPIED_WINDOW: Duration = Duration::from_millis(1600);
/// A guide run takes minutes; its answer is read again at this pace while it runs.
const GUIDE_POLL: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tab {
    Summary,
    Guide,
    Timeline,
    Code,
}

/// One open pull request.
pub(in crate::app) struct Panel {
    reference: Reference,
    /// The list row it was opened from, shown while the detail loads.
    seed: Option<Rc<Entry>>,
    detail: Option<Rc<Detail>>,
    activity: Option<Rc<Activity>>,
    /// Derived from `activity` once when it lands (`set_activity`), so render only borrows.
    conversations: Rc<Vec<Conversation>>,
    review_outcomes: Rc<Vec<(String, logic::ReviewOutcome)>>,
    newest_commit: Option<i64>,
    /// The detail and activity reads in flight; a newer read drops (cancels) the older one.
    _detail_read: Option<Task<()>>,
    _activity_read: Option<Task<()>>,
    detail_error: Option<(SharedString, bool)>,
    activity_error: Option<SharedString>,
    detail_loading: bool,
    activity_loading: bool,
    refreshing: bool,
    tab: Tab,
    timeline_newest: bool,
    comments_newest: bool,
    show_bots: bool,
    show_resolved: bool,
    shown_conversations: usize,
    /// Description, Checks, Comments.
    sections: [bool; 3],
    /// Disclosures flipped from their default: folded comments, timeline groups and threads.
    toggled: Rc<HashSet<String>>,
    pending: Option<&'static str>,
    armed: Option<&'static str>,
    _disarm: Option<Task<()>>,
    merge_method: Option<&'static str>,
    copied: Option<&'static str>,
    _copied: Option<Task<()>>,
    markdown: HashMap<String, (String, Entity<TextViewState>, Subscription)>,
    summary_scroll: ScrollHandle,
    timeline: ListState,
    timeline_rows: Rc<Vec<TimelineRow>>,
    guide: Option<Rc<Value>>,
    /// `guide["guide"]` decoded once when it lands (`set_guide`); `None` while there is none.
    guide_tour: Option<Result<Rc<logic::Guide>, SharedString>>,
    guide_loading: bool,
    guide_error: Option<SharedString>,
    guide_starting: bool,
    _guide_poll: Option<Task<()>>,
    code: Option<code::CodeTab>,
    write: write::WriteState,
    stack: Option<Rc<logic::Stack>>,
    stack_failed: bool,
    linked: Option<Vec<header::LinkedThread>>,
    checks_all: bool,
    maintainer_saving: bool,
    /// A maintainer-edits choice saved and not yet read back.
    maintainer_choice: Option<bool>,
    _picker_search: Option<Subscription>,
    /// The handoff being prepared, as the web keys it ("ask", "checkout:worktree", ...).
    handoff: Option<String>,
    /// The header's fold scrolled away.
    condensed: bool,
    guide_head: Option<String>,
    guide_position: usize,
    guide_seen: HashSet<usize>,
    guide_rail_open: bool,
    guide_files_open: HashSet<String>,
}

impl Panel {
    fn new(reference: Reference, seed: Option<Rc<Entry>>) -> Self {
        Self {
            reference,
            seed,
            detail: None,
            activity: None,
            conversations: Rc::default(),
            review_outcomes: Rc::default(),
            newest_commit: None,
            _detail_read: None,
            _activity_read: None,
            detail_error: None,
            activity_error: None,
            detail_loading: false,
            activity_loading: false,
            refreshing: false,
            tab: Tab::Summary,
            timeline_newest: true,
            comments_newest: true,
            show_bots: false,
            show_resolved: false,
            shown_conversations: CONVERSATION_PAGE,
            sections: [true, false, true],
            toggled: Rc::default(),
            pending: None,
            armed: None,
            _disarm: None,
            merge_method: None,
            copied: None,
            _copied: None,
            markdown: HashMap::new(),
            summary_scroll: ScrollHandle::new(),
            timeline: ListState::new(0, ListAlignment::Top, px(600.)),
            timeline_rows: Rc::default(),
            guide: None,
            guide_tour: None,
            guide_loading: false,
            guide_error: None,
            guide_starting: false,
            _guide_poll: None,
            code: None,
            write: write::WriteState::default(),
            stack: None,
            stack_failed: false,
            linked: None,
            checks_all: false,
            maintainer_saving: false,
            maintainer_choice: None,
            _picker_search: None,
            handoff: None,
            condensed: false,
            guide_head: None,
            guide_position: 0,
            guide_seen: HashSet::new(),
            guide_rail_open: false,
            guide_files_open: HashSet::new(),
        }
    }

    pub(super) fn reference(&self) -> &Reference {
        &self.reference
    }

    fn set_activity(&mut self, activity: Activity) {
        self.conversations = Rc::new(logic::conversations(&activity.comments, &activity.review_threads));
        self.review_outcomes = Rc::new(logic::latest_review_outcomes(&activity.comments, &activity.commits));
        self.newest_commit = logic::newest_commit_ms(&activity.commits);
        self.activity = Some(Rc::new(activity));
    }

    fn set_guide(&mut self, value: Value) {
        use serde::Deserialize as _;
        let guide = &value["guide"];
        self.guide_tour = (!guide.is_null()).then(|| {
            logic::Guide::deserialize(guide)
                .map(Rc::new)
                .map_err(|error| format!("Unreadable review guide: {error}").into())
        });
        self.guide = Some(Rc::new(value));
    }

    fn rebuild_timeline(&mut self) {
        let (Some(detail), Some(activity)) = (&self.detail, &self.activity) else {
            self.timeline_rows = Rc::default();
            self.timeline.reset(0);
            return;
        };
        let mut events = logic::build_timeline(detail, activity, logic::terminology(&detail.provider).0);
        if !self.timeline_newest {
            events.reverse();
        }
        let rows = logic::group_timeline(events);
        self.timeline.reset(rows.len());
        self.timeline_rows = Rc::new(rows);
    }

    fn provider(&self) -> &str {
        self.detail
            .as_ref()
            .map(|d| d.provider.as_str())
            .or(self.seed.as_ref().map(|s| s.provider.as_str()))
            .unwrap_or("github")
    }

    fn url(&self) -> Option<&str> {
        self.detail.as_ref().map(|d| d.url.as_str()).or(self.seed.as_ref().map(|s| s.url.as_str()))
    }

    /// Checks the list row knows newer than the detail, and workflows the detail says await approval.
    fn checks_state(&self, detail: &Detail) -> (Option<&'static str>, bool) {
        let from_detail = logic::checks_state(&detail.checks);
        let latest = match self.seed.as_ref().filter(|s| s.updated_at > detail.updated_at) {
            Some(seed) => seed
                .checks_state
                .as_deref()
                .and_then(|s| ["passing", "failing", "pending"].into_iter().find(|v| *v == s)),
            None => from_detail,
        };
        let state = if latest != Some("failing") && detail.checks.iter().any(|c| c.status == "action-required") {
            Some("pending")
        } else {
            latest
        };
        (state, state != from_detail)
    }
}

fn is_not_found(error: &RpcError) -> bool {
    matches!(error, RpcError::Failure(value) if value["_tag"] == "PullRequestOperationError" && value["reason"] == "not-found")
}

fn toast_error(window: &mut Window, cx: &mut App, title: impl Into<SharedString>, message: impl Into<SharedString>) {
    window.push_notification(Notification::error(message).title(title), cx);
}

fn toast_success(window: &mut Window, cx: &mut App, title: impl Into<SharedString>) {
    window.push_notification(Notification::success("").title(title), cx);
}

/// The repository's page: the pull request URL without its `/pull/N` tail.
fn repository_url(url: &str) -> Option<String> {
    ["/pull/", "/-/merge_requests/"].iter().find_map(|tail| url.find(tail).map(|at| url[..at].to_owned()))
}

impl Workspace {
    /// Opens a pull request in the panel, or shows the one already open.
    pub(super) fn open_pull_request_panel(
        &mut self,
        reference: Reference,
        seed: Option<Rc<Entry>>,
        cx: &mut Context<Self>,
    ) {
        if self.pull_requests.panel.as_ref().is_some_and(|p| p.reference == reference) {
            return;
        }
        if self.show_pr_tab(reference, seed, cx) {
            return;
        }
        self.read_pr_detail(cx);
        if self.server_info.settings.bool("reviewGuidesEnabled") {
            self.read_pr_guide(cx);
        }
        cx.notify();
    }

    /// "Collapse pull request panel": every tab closes with it.
    pub(super) fn close_pull_request_panel(&mut self, cx: &mut Context<Self>) {
        self.pull_requests.panel = None;
        self.pull_requests.tabs.clear();
        self.pull_requests.parked.clear();
        cx.notify();
    }

    /// `useOpenChangeRequestLink`: with "Open pull requests in T3 Code" on, a link to a project's
    /// change request opens in the Pull requests page; otherwise the caller opens the browser.
    pub(in crate::app) fn open_change_request_link(&mut self, url: &str, cx: &mut Context<Self>) -> bool {
        if !self.client_settings.values.bool("pullRequestLinksOpenInApp") {
            return false;
        }
        let Some((host, repository, number)) = logic::parse_change_request_url(url) else { return false };
        let projects = self
            .shell
            .projects
            .iter()
            .map(|p| (p.id.as_str(), p.repository_identity.as_ref().and_then(|i| i.canonical_key.as_deref())));
        let Some(project_id) = logic::project_for_link(projects, &host, &repository).map(str::to_owned) else {
            return false;
        };
        let reference = Reference { project_id, host: Some(host), repository, number };
        if self.page != Some(super::Page::PullRequests) {
            self.open_page(super::Page::PullRequests, cx);
        }
        let seed = self.pull_requests.entry_for(&reference);
        self.open_pull_request_panel(reference, seed, cx);
        true
    }

    /// The open pull request `key` names, shown or parked in its tab, for an answer to land in.
    fn pr_panel(&mut self, key: &str) -> Option<&mut Panel> {
        if self.pull_requests.panel.as_ref().is_some_and(|p| p.reference.key() == key) {
            return self.pull_requests.panel.as_mut();
        }
        self.pull_requests.parked.get_mut(key)
    }

    fn pr_panel_ref(&self, key: &str) -> Option<&Panel> {
        match &self.pull_requests.panel {
            Some(panel) if panel.reference.key() == key => Some(panel),
            _ => self.pull_requests.parked.get(key),
        }
    }

    /// Reads the shown pull request's detail and activity.
    fn read_pr_detail(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.pull_requests.panel.as_ref().map(|p| p.reference.key()) {
            self.read_pr_detail_of(&key, cx);
        }
    }

    /// Reads `key`'s detail and its activity; each lands on its own, in that pull request's tab.
    fn read_pr_detail_of(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(panel) = self.pr_panel_ref(key) else { return };
        let reference = panel.reference.to_json();
        let mut detail_input = reference.clone();
        detail_input["allowStale"] = json!(false);
        let (detail_client, detail_key, key) = (client.clone(), key.to_owned(), key.to_owned());
        let detail_read = cx.spawn(async move |this, cx| {
            let result = detail_client.request("pullRequests.detail", detail_input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(panel) = this.pr_panel(&detail_key) else { return };
                panel.detail_loading = false;
                let (mut first, mut landed) = (false, false);
                match result.map(serde_json::from_value::<Detail>) {
                    Ok(Ok(detail)) => {
                        first = panel.detail.is_none();
                        landed = true;
                        panel.detail = Some(Rc::new(detail));
                        panel.detail_error = None;
                        panel.maintainer_choice = None;
                    }
                    Ok(Err(error)) => {
                        panel.detail_error = Some((format!("Unreadable pull request: {error}").into(), false))
                    }
                    Err(error) => {
                        panel.detail_error = Some((list_logic::error_message(&error).into(), is_not_found(&error)))
                    }
                }
                panel.rebuild_timeline();
                if first {
                    this.read_pr_extras_of(&detail_key, cx);
                }
                if landed {
                    this.read_pr_avatars(&detail_key, cx);
                }
                cx.notify();
            });
        });
        let activity_key = key.clone();
        let activity_read = cx.spawn(async move |this, cx| {
            let result = client.request("pullRequests.activity", reference).await;
            let _ = this.update(cx, |this, cx| {
                let Some(panel) = this.pr_panel(&activity_key) else { return };
                panel.activity_loading = false;
                let mut landed = false;
                match result.map(serde_json::from_value::<Activity>) {
                    Ok(Ok(activity)) => {
                        landed = true;
                        panel.set_activity(activity);
                        panel.activity_error = None;
                        panel.write.reactions.clear();
                    }
                    Ok(Err(error)) => panel.activity_error = Some(format!("Unreadable activity: {error}").into()),
                    Err(error) => panel.activity_error = Some(list_logic::error_message(&error).into()),
                }
                panel.rebuild_timeline();
                if landed {
                    this.read_pr_avatars(&activity_key, cx);
                }
                cx.notify();
            });
        });
        let Some(panel) = self.pr_panel(&key) else { return };
        panel.detail_loading = true;
        panel.activity_loading = true;
        panel._detail_read = Some(detail_read);
        panel._activity_read = Some(activity_read);
        cx.notify();
    }

    /// Refresh: the host is asked again rather than the server's cache.
    fn refresh_pr_from_host(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        panel.refreshing = true;
        let key = panel.reference.key();
        let input = json!({"reference": panel.reference.to_json()});
        cx.spawn(async move |this, cx| {
            let _ = client.request("pullRequests.invalidate", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(panel) = this.pr_panel(&key) else { return };
                panel.refreshing = false;
                this.read_pr_detail_of(&key, cx);
                if this.pull_requests.panel.as_ref().is_some_and(|p| p.reference.key() == key) {
                    this.refresh_pr_code(cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// `perform`: runs a write action, then reads the pull request and the list again.
    fn perform_pr_action(
        &mut self,
        action: &'static str,
        method: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.perform_pr_action_with(action, method, None, window, cx);
    }

    /// `perform` with the update method `update-branch` takes.
    fn perform_pr_action_with(
        &mut self,
        action: &'static str,
        method: Option<&'static str>,
        update_method: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        if panel.pending.is_some() {
            return;
        }
        panel.pending = Some(action);
        panel.armed = None;
        let key = panel.reference.key();
        let term = logic::terminology(panel.provider()).0;
        let mut input = panel.reference.to_json();
        input["action"] = json!(action);
        if let Some(method) = method {
            input["mergeMethod"] = json!(method);
        }
        if let Some(update) = update_method {
            input["updateMethod"] = json!(update);
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("pullRequests.runAction", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(panel) = this.pr_panel(&key) {
                    panel.pending = None;
                }
                match result {
                    Err(error) => {
                        let (title, mut hint) = logic::action_failure(action, term);
                        if update_method == Some("rebase") {
                            hint = "The host refused it. A rebase stops at the first commit that does not apply cleanly; updating with a merge commit may still work.".into();
                        }
                        toast_error(
                            window,
                            cx,
                            title,
                            logic::readable_failure(&list_logic::error_message(&error), &hint),
                        );
                    }
                    Ok(_) => {
                        match action {
                            "revert" => toast_success(window, cx, format!("Revert {term} opened")),
                            "delete-branch" => toast_success(window, cx, "Branch deleted"),
                            _ => {}
                        }
                        if logic::needs_host_refresh(action) {
                            this.refresh_pr_from_host(cx);
                        } else {
                            this.read_pr_detail(cx);
                        }
                        this.read_pull_requests(false, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The header's inline confirm: the first press arms "Confirm …?", a second within three seconds acts.
    fn press_pr_action(
        &mut self,
        action: &'static str,
        method: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        if panel.armed == Some(action) {
            self.perform_pr_action(action, method, window, cx);
            return;
        }
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

    /// A menu item's confirmation: the menu closes on a click, so it asks in a dialog instead.
    fn confirm_pr_action(
        &mut self,
        title: String,
        body: String,
        confirm: &'static str,
        action: &'static str,
        method: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        let destructive = matches!(action, "close" | "revert" | "delete-branch");
        window.open_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let button = Button::new("pr-confirm").label(confirm).on_click(move |_, window, cx| {
                window.close_dialog(cx);
                let _ = view.update(cx, |this, cx| this.perform_pr_action(action, method, window, cx));
            });
            dialog.entrance(DialogEntrance::None).title(title.clone()).child(v_flex().child(body.clone())).footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("pr-confirm-cancel").label("Cancel").outline()))
                    .child(if destructive { button.danger() } else { button.primary() }),
            )
        });
    }

    fn copy_pr_text(&mut self, slot: &'static str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        panel.copied = Some(slot);
        let key = panel.reference.key();
        panel._copied = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_WINDOW).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(panel) = this.pr_panel(&key) {
                    panel.copied = None;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    /// `pullRequests.reviewGuide`, read again while a run is writing one.
    fn read_pr_guide(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        panel.guide_loading = true;
        let key = panel.reference.key();
        let input = panel.reference.to_json();
        cx.spawn(async move |this, cx| {
            let result = client.request("pullRequests.reviewGuide", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(panel) = this.pr_panel(&key) else { return };
                panel.guide_loading = false;
                match result {
                    Ok(value) => {
                        panel.guide_error = None;
                        panel.set_guide(value);
                    }
                    Err(error) => panel.guide_error = Some(list_logic::error_message(&error).into()),
                }
                this.poll_pr_guide(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn poll_pr_guide(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        let running = panel.guide.as_ref().is_some_and(|g| g["run"]["_tag"] == "Running");
        if !running {
            panel._guide_poll = None;
            return;
        }
        let key = panel.reference.key();
        panel._guide_poll = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GUIDE_POLL).await;
            let _ = this.update(cx, |this, cx| {
                if this.pr_panel(&key).is_some() {
                    this.read_pr_guide(cx);
                }
            });
        }));
    }

    /// `useReviewGuideWriter.write`: starts an agent run on the server (`pullRequests.generateReviewGuide`).
    fn write_pr_guide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        if panel.guide_starting || panel.guide.as_ref().is_some_and(|g| g["run"]["_tag"] == "Running") {
            return;
        }
        panel.guide_starting = true;
        if panel.guide.as_ref().is_none_or(|g| g["guide"].is_null()) {
            panel.tab = Tab::Guide;
        }
        let key = panel.reference.key();
        let input = panel.reference.to_json();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("pullRequests.generateReviewGuide", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(panel) = this.pr_panel(&key) else { return };
                panel.guide_starting = false;
                match result {
                    Ok(value) => panel.set_guide(value),
                    Err(error) => toast_error(
                        window,
                        cx,
                        "Could not start a review guide",
                        logic::readable_failure(
                            &list_logic::error_message(&error),
                            "Check the review guide model in Settings → Source control.",
                        ),
                    ),
                }
                this.poll_pr_guide(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn rebuild_pr_timeline(&mut self) {
        if let Some(panel) = self.pull_requests.panel.as_mut() {
            panel.rebuild_timeline();
        }
    }

    fn toggle_pr_disclosure(&mut self, key: String, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        let toggled = Rc::make_mut(&mut panel.toggled);
        if !toggled.remove(&key) {
            toggled.insert(key);
        }
        panel.timeline.remeasure();
        cx.notify();
    }

    /// One markdown body, parsed once and kept while the panel shows this pull request.
    fn pr_markdown(&mut self, key: &str, text: &str, cx: &mut Context<Self>) -> Option<Entity<TextViewState>> {
        let panel = self.pull_requests.panel.as_mut()?;
        if let Some((held, view, _)) = panel.markdown.get_mut(key) {
            if held != text {
                let source = logic::strip_html_comments(text);
                view.update(cx, |view, cx| view.set_text(&source, cx));
                text.clone_into(held);
            }
            return Some(view.clone());
        }
        let source = logic::strip_html_comments(text);
        let view = cx.new(|cx| TextViewState::markdown(&source, cx));
        let changed = cx.observe(&view, |this, _, cx| {
            if let Some(panel) = this.pull_requests.panel.as_mut() {
                panel.timeline.remeasure();
            }
            cx.notify();
        });
        panel.markdown.insert(key.to_owned(), (text.to_owned(), view.clone(), changed));
        Some(view)
    }

    /// The merge method the header and menu use (`resolvePullRequestMergeMethod`).
    fn pr_merge_method(&self, panel: &Panel, detail: &Detail) -> &'static str {
        let project = self.shell.projects.iter().find(|p| p.id == detail.project_id);
        let project_default = self.server_info.project_setting(project, "pullRequestMergeMethod");
        logic::resolve_merge_method(
            &logic::allowed_merge_methods(detail),
            panel.merge_method.or(detail.auto_merge_method.as_deref().and_then(known_method)),
            project_default.as_str(),
            self.pull_requests.last_merge_method,
        )
    }

    pub(super) fn render_pull_request_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(panel) = self.pull_requests.panel.as_ref() else { return div().into_any_element() };
        let frame = v_flex().size_full().min_h_0().bg(t.background);
        let Some(detail) = panel.detail.clone() else {
            let body = match &panel.detail_error {
                Some((error, not_found)) if !panel.detail_loading => {
                    self.render_pr_unavailable(error.clone(), *not_found, &t, cx)
                }
                _ => detail_ghost(&t),
            };
            return frame.child(self.render_pr_seed_header(&t, cx)).child(body).into_any_element();
        };
        let guides = self.server_info.settings.bool("reviewGuidesEnabled");
        let panel = self.pull_requests.panel.as_ref().expect("checked above");
        let tab = match panel.tab {
            Tab::Guide if !guides => Tab::Summary,
            Tab::Code if !detail.capabilities.diff => Tab::Summary,
            tab => tab,
        };
        let header = self.render_pr_header(&detail, tab, guides, &t, window, cx);
        let body = match tab {
            Tab::Summary => self.render_pr_summary(&detail, &t, window, cx),
            Tab::Timeline => self.render_pr_timeline(&detail, &t, cx),
            Tab::Guide => {
                let width = self.pr_panel_width(window);
                self.render_pr_guide(&t, width, cx)
            }
            Tab::Code => self.render_pr_code(&detail, &t, window, cx),
        };
        let composer = self.render_pr_composer(&detail, &t, cx);
        frame
            .child(header)
            .child(
                self.pr_scroll_watch(body, cx)
                    .children(composer.map(|c| div().absolute().right(px(16.)).bottom(px(12.)).child(c))),
            )
            .into_any_element()
    }

    /// The crumbs and collapse button while the detail is not in yet (`PullRequestDetailGhost`).
    fn render_pr_seed_header(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let number = panel.reference.number;
        let title = panel.seed.as_ref().map(|s| s.title.clone());
        v_flex()
            .flex_none()
            .border_b_1()
            .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
            .child(
                h_flex()
                    .h(px(28.))
                    .pl(px(16.))
                    .pr(px(16.))
                    .gap(px(4.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(panel.reference.repository.clone()),
                    )
                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("#{number}")))
                    .child(collapse_button(t, cx)),
            )
            .children(title.map(|title| {
                div()
                    .px(px(16.))
                    .pt(px(4.))
                    .pb(px(16.))
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .truncate()
                    .child(title)
            }))
            .into_any_element()
    }

    fn render_pr_unavailable(
        &self,
        error: SharedString,
        not_found: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let term = logic::terminology(panel.provider()).0;
        let (title, description): (String, SharedString) = if not_found {
            (
                format!("{} #{} not found", logic::capitalize(term), panel.reference.number),
                format!("It may be an issue rather than a {term}, or this account can't see it.").into(),
            )
        } else {
            (format!("Could not load this {term}"), error)
        };
        let host = panel.url().map(|url| (url.to_owned(), list_logic::open_on_host_label(panel.provider())));
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(16.))
            .p(px(32.))
            .text_center()
            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(title))
            .child(div().max_w(px(384.)).text_sm().text_color(t.muted_foreground).child(description))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        outline_button("pr-detail-retry", t)
                            .disabled(panel.detail_loading)
                            .child(Icon::new(Lucide::RefreshCw).with_size(px(14.)).text_color(t.muted_foreground))
                            .child("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.read_pr_detail(cx))),
                    )
                    .children(host.map(|(url, label)| {
                        outline_button("pr-detail-open-host", t)
                            .child(label)
                            .on_click(move |_, _, cx| cx.open_url(&url))
                    })),
            )
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_pr_header(
        &mut self,
        detail: &Rc<Detail>,
        tab: Tab,
        guides: bool,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let term = logic::terminology(&detail.provider).0;
        let (checks_state, checks_stale) = panel.checks_state(detail);
        let primary = logic::primary_control(detail, checks_state);
        let method = self.pr_merge_method(panel, detail);
        let (_, state_color, state_label) = state_glyph(&detail.state, detail.is_draft, t);
        let hairline = Hsla { a: t.border.a * 0.6, ..t.border };
        let now = now_ms();
        let view = cx.entity();

        let repo_url = repository_url(&detail.url);
        let repository = div()
            .id("pr-crumb-repository")
            .min_w_0()
            .truncate()
            .font_weight(FontWeight::MEDIUM)
            .child(detail.repository.clone())
            .when_some(repo_url, |el, url| {
                let tip: SharedString = format!("Open {} repository", detail.repository).into();
                el.cursor_pointer()
                    .hover(|s| s.text_color(t.foreground))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(move |_, _, cx| cx.open_url(&url))
            });
        let open_label = list_logic::open_on_host_label(&detail.provider);
        let number = number_link(&detail.url, detail.number, state_color, open_label);
        let condensed = panel.condensed;
        let condensed_row = self.render_pr_condensed_row(detail, t);
        let crumbs = h_flex()
            .flex_1()
            .min_w_0()
            .h(px(28.))
            .pl(px(16.))
            .gap(px(4.))
            .text_xs()
            .text_color(t.muted_foreground)
            .when(!condensed, |el| el.child(repository))
            .child(number)
            .when(condensed, |el| {
                el.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(detail.title.clone()),
                )
            });

        let actions = h_flex()
            .flex_none()
            .h(px(28.))
            .mr(px(16.))
            .gap(px(4.))
            .children(self.render_pr_thread_count(t, cx))
            .children(self.render_pr_stack(detail, method, t, cx))
            .children(self.render_pr_primary(detail, primary, method, state_label, state_color, t, cx))
            .child(self.render_pr_checkout(detail, t, cx))
            .child(self.render_pr_menu(detail, primary, method, guides, &view, t))
            .child(collapse_button(t, cx));

        let author = actor_label(detail.author.as_ref(), t);
        let checkout = logic::checkout_command(&detail.provider, detail.number).map(|command| {
            copyable_code("pr-copy-checkout", command, panel.copied == Some("checkout"), "Copy checkout command", t)
                .ml_auto()
                .on_click(cx.listener(move |this, _, _, cx| {
                    let command = this
                        .pull_requests
                        .panel
                        .as_ref()
                        .and_then(|p| p.detail.as_ref())
                        .and_then(|d| logic::checkout_command(&d.provider, d.number));
                    if let Some(command) = command {
                        this.copy_pr_text("checkout", command, cx);
                    }
                }))
        });
        let updated = parse_ms(&detail.updated_at).map(|at| format!("updated {}", relative_time(now - at)));
        let meta = h_flex()
            .mt(px(8.))
            .min_h(px(20.))
            .min_w_0()
            .gap(px(8.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(meta_line([Some(author.into_any_element()), updated.map(|u| div().child(u).into_any_element())], t))
            .children(checkout);

        let head = detail.head_branch.clone();
        let branches = h_flex()
            .mt(px(16.))
            .min_h(px(20.))
            .min_w_0()
            .gap(px(8.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .font_family(FONT_MONO)
                    .text_color(Hsla { a: 0.7, ..t.muted_foreground })
                    .child(self.render_pr_base_branch(detail, t, cx))
                    .child(
                        Icon::new(Lucide::ArrowLeft)
                            .with_size(px(14.))
                            .text_color(Hsla { a: 0.6, ..t.muted_foreground }),
                    )
                    .child(
                        copyable_code(
                            "pr-copy-branch",
                            head,
                            panel.copied == Some("branch"),
                            &format!("Copy {term} branch"),
                            t,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            let branch = this
                                .pull_requests
                                .panel
                                .as_ref()
                                .and_then(|p| p.detail.as_ref())
                                .map(|d| d.head_branch.clone());
                            if let Some(branch) = branch {
                                this.copy_pr_text("branch", branch, cx);
                            }
                        })),
                    ),
            )
            .child(
                h_flex()
                    .ml_auto()
                    .flex_none()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .min_w(px(64.))
                            .justify_end()
                            .gap(px(6.))
                            .child(Icon::new(Lucide::FileDiff).with_size(px(14.)).text_color(t.muted_foreground))
                            .child(format!(
                                "{} {}",
                                detail.changed_files,
                                if detail.changed_files == 1 { "file" } else { "files" }
                            )),
                    )
                    .children(diff_stat(detail.additions, detail.deletions, 12., t)),
            );

        let title = match self.render_pr_title_input(t) {
            Some(input) => input,
            None => h_flex()
                .min_h(px(24.))
                .min_w_0()
                .gap(px(4.))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_base()
                        .line_height(px(22.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(t.foreground)
                        .child(detail.title.clone()),
                )
                .group(write::EDIT_GROUP)
                .when(logic::can_edit_change_request(detail), |el| {
                    el.child(write::hover_only(
                        ghost_button("pr-edit-title", t)
                            .size(px(20.))
                            .p_0()
                            .tooltip("Edit title")
                            .child(Icon::new(Lucide::Pencil).with_size(px(12.)))
                            .on_click(cx.listener(|this, _, window, cx| this.begin_pr_title_edit(window, cx))),
                    ))
                })
                .into_any_element(),
        };

        let mut tabs = vec![(Tab::Summary, "Summary")];
        if guides {
            tabs.push((Tab::Guide, "Guide"));
        }
        tabs.push((Tab::Timeline, "Timeline"));
        if detail.capabilities.diff {
            tabs.push((Tab::Code, "Code"));
        }
        let toggle = super::usage::segmented("pr-tab", tabs.into_iter(), tab, t, cx, |this, tab, cx| {
            let Some(panel) = this.pull_requests.panel.as_mut() else { return };
            panel.tab = tab;
            if tab == Tab::Guide && panel.guide.is_none() && !panel.guide_loading {
                this.read_pr_guide(cx);
            }
            // The guide's excerpts come from the change's diff.
            if matches!(tab, Tab::Code | Tab::Guide) {
                this.open_pr_code(cx);
            }
            cx.notify();
        });
        let status = match tab {
            Tab::Summary => Some(self.render_pr_checks_status(detail, checks_state, checks_stale, t, window, cx)),
            Tab::Timeline => Some(self.render_pr_timeline_status(t, cx)),
            Tab::Guide | Tab::Code => None,
        };
        let tab_bar = h_flex()
            .flex_wrap()
            .min_w_0()
            .gap(px(8.))
            .px(px(16.))
            .py(px(8.))
            .border_t_1()
            .border_color(hairline)
            .child(toggle)
            .children(status);

        v_flex()
            .flex_none()
            .min_w_0()
            .border_b_1()
            .border_color(hairline)
            .child(h_flex().items_start().gap(px(8.)).child(crumbs).child(actions))
            .map(|el| {
                if condensed {
                    el.child(condensed_row)
                } else {
                    el.child(
                        v_flex().mt(px(4.)).px(px(16.)).pb(px(16.)).min_w_0().child(title).child(meta).child(branches),
                    )
                }
            })
            .child(tab_bar)
            .into_any_element()
    }

    /// `PullRequestPrimaryAction`.
    #[allow(clippy::too_many_arguments)]
    fn render_pr_primary(
        &self,
        detail: &Detail,
        primary: Option<Primary>,
        method: &'static str,
        state_label: &'static str,
        state_color: Hsla,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let pending = panel.pending;
        let armed = panel.armed;
        let auto_label = |m: &str| format!("Auto-merge ({})", logic::merge_method_label(m).to_lowercase());
        let armed_label = detail.auto_merge_method.as_deref().map_or("Auto-merge".to_owned(), auto_label);
        let armed_badge =
            || badge("pr-auto-merge-armed", Lucide::GitMerge, armed_label.clone(), t.info_foreground, t.info, t);
        let mut out = Vec::new();
        if detail.state == "open" && detail.auto_merge_enabled == Some(true) && primary != Some(Primary::AutoMergeArmed)
        {
            out.push(armed_badge());
        }
        let button = |id: &'static str, icon: Lucide, label: String| {
            Button::new(id)
                .primary()
                .xsmall()
                .h(px(24.))
                .px(px(8.))
                .gap(px(4.))
                .text_xs()
                .disabled(pending.is_some())
                .child(Icon::new(icon).with_size(px(14.)))
                .child(label)
        };
        match primary {
            Some(Primary::Ready) => out.push(
                button("pr-ready", Lucide::GitPullRequestArrow, "Ready for review".into())
                    .on_click(cx.listener(|this, _, window, cx| this.perform_pr_action("ready", None, window, cx)))
                    .into_any_element(),
            ),
            Some(primary @ (Primary::Merge | Primary::EnableAutoMerge)) => {
                let (action, busy, confirm, idle) = if primary == Primary::Merge {
                    ("merge", "Merging...", "Confirm merge?", logic::merge_method_label(method).to_owned())
                } else {
                    ("enable-auto-merge", "Enabling...", "Confirm auto-merge?", auto_label(method))
                };
                let label = if pending == Some(action) {
                    busy.to_owned()
                } else if armed == Some(action) {
                    confirm.to_owned()
                } else {
                    idle
                };
                let icon = if armed == Some(action) { Lucide::Check } else { Lucide::GitMerge };
                out.push(
                    button(if primary == Primary::Merge { "pr-merge" } else { "pr-enable-auto-merge" }, icon, label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.press_pr_action(action, Some(method), window, cx)
                        }))
                        .into_any_element(),
                );
            }
            Some(Primary::AutoMergeArmed) => out.push(armed_badge()),
            Some(Primary::Merged | Primary::Closed) => {
                let icon = if detail.state == "merged" { Lucide::GitMerge } else { Lucide::GitPullRequestClosed };
                out.push(
                    h_flex()
                        .h(px(24.))
                        .px(px(8.))
                        .gap(px(4.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(t.border)
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(state_color)
                        .child(Icon::new(icon).with_size(px(14.)).text_color(state_color))
                        .child(state_label)
                        .into_any_element(),
                );
            }
            Some(Primary::Resolve) => out.push(self.render_pr_resolve_conflicts(detail, t, cx)),
            None => {}
        }
        out
    }

    /// `PullRequestActionsMenu`.
    fn render_pr_menu(
        &self,
        detail: &Rc<Detail>,
        primary: Option<Primary>,
        method: &'static str,
        guides: bool,
        view: &Entity<Workspace>,
        t: &Tokens,
    ) -> impl IntoElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let (term, short) = logic::terminology(&detail.provider);
        let actions = logic::menu_actions(detail, primary);
        let allowed = logic::allowed_merge_methods(detail);
        let busy = panel.pending.is_some();
        let deleting = panel.pending == Some("delete-branch");
        let refreshing = panel.refreshing || panel.detail_loading;
        let guide_running = panel.guide_starting || panel.guide.as_ref().is_some_and(|g| g["run"]["_tag"] == "Running");
        let guide_written = panel.guide.as_ref().is_some_and(|g| !g["guide"].is_null());
        let maintainer = logic::can_edit_maintainer_edits(detail)
            .then(|| (panel.maintainer_choice.or(detail.maintainer_can_modify) == Some(true), panel.maintainer_saving));
        let can_link = logic::parse_change_request_url(&detail.url).is_some();
        let handoff = panel.handoff.clone();
        let can_fix = self.shell.projects.iter().any(|p| p.id == detail.project_id);
        let detail = detail.clone();
        let view = view.clone();
        let tip: SharedString =
            if refreshing { format!("Refreshing {term}") } else { format!("More {term} actions") }.into();
        Button::new("pr-actions")
            .ghost()
            .xsmall()
            .size(px(24.))
            .p_0()
            .rounded(px(6.))
            .tooltip(tip)
            .child(
                Icon::new(if refreshing { Lucide::RefreshCw } else { Lucide::Ellipsis })
                    .with_size(px(16.))
                    .text_color(t.muted_foreground),
            )
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu: PopupMenu, window, _| {
                let on = |f: fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
                    window.listener_for(&view, move |this, _, window, cx| f(this, window, cx))
                };
                if can_link {
                    menu = menu.item(
                        PopupMenuItem::new("Link to thread")
                            .icon(Lucide::Link)
                            .on_click(on(|this, window, cx| this.open_pr_thread_picker(window, cx))),
                    );
                }
                menu = menu.item(
                    PopupMenuItem::new("Refresh")
                        .icon(Lucide::RefreshCw)
                        .disabled(refreshing)
                        .on_click(on(|this, _, cx| this.refresh_pr_from_host(cx))),
                );
                let two_line = |label: String, note: String, icon: Lucide| {
                    PopupMenuItem::element(move |_, cx| {
                        let muted = theme::tokens(cx).muted_foreground;
                        h_flex()
                            .items_start()
                            .gap(px(8.))
                            .child(div().mt(px(3.)).child(Icon::new(icon).with_size(px(14.))))
                            .child(v_flex().child(label.clone()).child(div().text_xs().text_color(muted).child(note.clone())))
                    })
                };
                let busy_handoff = handoff.is_some();
                menu = menu
                    .item(
                        two_line(
                            if handoff.as_deref() == Some("ask") { "Opening..." } else { "Ask a question" }.into(),
                            format!("Opens a thread that knows which {term} you mean."),
                            Lucide::MessageCircleQuestionMark,
                        )
                        .disabled(busy_handoff)
                        .on_click(on(|this, window, cx| this.ask_about_pr(window, cx))),
                    )
                    .item(
                        two_line(
                            if handoff.as_deref() == Some("explain") { "Opening...".into() } else { format!("Explain this {short}") },
                            "A walk through the diff and what to read closely.".into(),
                            Lucide::BookOpen,
                        )
                        .disabled(busy_handoff)
                        .on_click(on(|this, window, cx| this.explain_pr(window, cx))),
                    )
                    .item(
                        PopupMenuItem::new(if handoff.as_deref() == Some("findings") {
                            "Preparing..."
                        } else {
                            "Fix findings in a thread"
                        })
                        .icon(Lucide::Hammer)
                        .disabled(busy_handoff || !can_fix)
                        .on_click(on(|this, window, cx| this.fix_pr_findings(window, cx))),
                    );
                if guides {
                    let label = if guide_running {
                        "Writing guide..."
                    } else if guide_written {
                        "Rewrite guide"
                    } else {
                        "Write guide"
                    };
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .icon(Lucide::Sparkles)
                            .disabled(guide_running)
                            .on_click(on(|this, window, cx| this.write_pr_guide(window, cx))),
                    );
                }
                menu = menu.separator();
                if let Some((allowed, saving)) = maintainer {
                    let label = if saving { "Allow edits from maintainers (saving…)" } else { "Allow edits from maintainers" };
                    menu = menu
                        .item(PopupMenuItem::new(label).checked(allowed).disabled(saving).on_click(window.listener_for(
                            &view,
                            move |this, _, window, cx| this.save_pr_maintainer_edits(!allowed, window, cx),
                        )))
                        .label("Maintainers can push to this branch. If your fork has workflows, this can also give access to its secrets.")
                        .separator();
                }
                if detail.state == "open" {
                    if actions.draft_toggle {
                        let (label, action, icon) = if detail.is_draft {
                            ("Ready for review", "ready", Lucide::GitPullRequestArrow)
                        } else {
                            ("Convert to draft", "draft", Lucide::GitPullRequestDraft)
                        };
                        menu = menu.item(PopupMenuItem::new(label).icon(icon).disabled(busy).on_click(
                            window.listener_for(&view, move |this, _, window, cx| {
                                this.perform_pr_action(action, None, window, cx)
                            }),
                        ));
                    }
                    if actions.merge_now {
                        let body = format!(
                            "This merges the {term} now with {}.",
                            logic::merge_method_label(method).to_lowercase()
                        );
                        menu =
                            menu.item(PopupMenuItem::new("Merge now").icon(Lucide::GitMerge).disabled(busy).on_click(
                                window.listener_for(&view, move |this, _, window, cx| {
                                    this.confirm_pr_action(
                                        "Confirm merge?".into(),
                                        body.clone(),
                                        "Merge",
                                        "merge",
                                        Some(method),
                                        window,
                                        cx,
                                    )
                                }),
                            ));
                    }
                    if actions.disable_auto_merge {
                        menu = menu.item(
                            PopupMenuItem::new("Disable auto-merge").icon(Lucide::GitMerge).disabled(busy).on_click(
                                window.listener_for(&view, |this, _, window, cx| {
                                    this.perform_pr_action("disable-auto-merge", None, window, cx)
                                }),
                            ),
                        );
                    } else if actions.auto_merge {
                        let body = format!(
                            "The host merges this {term} with {} once its requirements are met.",
                            logic::merge_method_label(method).to_lowercase()
                        );
                        menu = menu.item(
                            PopupMenuItem::new("Enable auto-merge").icon(Lucide::GitMerge).disabled(busy).on_click(
                                window.listener_for(&view, move |this, _, window, cx| {
                                    this.confirm_pr_action(
                                        "Confirm auto-merge?".into(),
                                        body.clone(),
                                        "Enable auto-merge",
                                        "enable-auto-merge",
                                        Some(method),
                                        window,
                                        cx,
                                    )
                                }),
                            ),
                        );
                    }
                    if actions.merge_methods {
                        if actions.draft_toggle || actions.merge_now || actions.auto_merge {
                            menu = menu.separator();
                        }
                        for choice in &allowed {
                            let choice: &'static str = choice;
                            menu = menu.item(
                                PopupMenuItem::new(logic::merge_method_label(choice))
                                    .checked(choice == method)
                                    .disabled(busy)
                                    .on_click(window.listener_for(&view, move |this, _, _, cx| {
                                        this.pull_requests.last_merge_method = choice;
                                        if let Some(panel) = this.pull_requests.panel.as_mut() {
                                            panel.merge_method = Some(choice);
                                        }
                                        cx.notify();
                                    })),
                            );
                        }
                    }
                    if actions.draft_toggle || actions.auto_merge || actions.merge_now || actions.merge_methods {
                        menu = menu.separator();
                    }
                }
                let (open_url, copy_url) = (detail.url.clone(), detail.url.clone());
                menu = menu
                    .item(
                        PopupMenuItem::new(list_logic::open_on_host_label(&detail.provider))
                            .icon(Lucide::ArrowUpRight)
                            .on_click(move |_, _, cx| cx.open_url(&open_url)),
                    )
                    .item(
                        PopupMenuItem::new("Copy link").icon(Lucide::Link).on_click(
                            window.listener_for(&view, move |this, _, _, cx| {
                                this.copy_pr_text("link", copy_url.clone(), cx)
                            }),
                        ),
                    )
                    .item(PopupMenuItem::new(format!("Copy {short} number")).icon(Lucide::Copy).on_click({
                        let number = format!("#{}", detail.number);
                        window
                            .listener_for(&view, move |this, _, _, cx| this.copy_pr_text("number", number.clone(), cx))
                    }));
                if actions.close {
                    let body = format!("This closes the {term} on the host. It can be reopened later.");
                    menu = menu.separator().item(
                        PopupMenuItem::new(format!("Close {term}"))
                            .icon(Lucide::GitPullRequestClosed)
                            .disabled(busy)
                            .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                this.confirm_pr_action(
                                    "Confirm close?".into(),
                                    body.clone(),
                                    "Close",
                                    "close",
                                    None,
                                    window,
                                    cx,
                                )
                            })),
                    );
                } else if actions.reopen {
                    menu = menu.separator().item(
                        PopupMenuItem::new(format!("Reopen {term}"))
                            .icon(Lucide::GitPullRequestArrow)
                            .disabled(busy)
                            .on_click(window.listener_for(&view, |this, _, window, cx| {
                                this.perform_pr_action("reopen", None, window, cx)
                            })),
                    );
                } else if actions.revert {
                    let body = format!("This opens a new {term} that reverts these changes.");
                    menu = menu.separator().item(
                        PopupMenuItem::new("Revert changes").icon(Lucide::RotateCcw).disabled(busy).on_click(
                            window.listener_for(&view, move |this, _, window, cx| {
                                this.confirm_pr_action(
                                    "Confirm revert?".into(),
                                    body.clone(),
                                    "Revert",
                                    "revert",
                                    None,
                                    window,
                                    cx,
                                )
                            }),
                        ),
                    );
                }
                if actions.delete_branch {
                    let branch = detail.head_branch.clone();
                    let owner =
                        detail.head_repository_name_with_owner.clone().unwrap_or_else(|| detail.repository.clone());
                    let label = if deleting { "Deleting branch..." } else { "Delete branch" };
                    menu =
                        menu.separator().item(PopupMenuItem::new(label).icon(Lucide::Trash).disabled(busy).on_click(
                            window.listener_for(&view, move |this, _, window, cx| {
                                this.confirm_pr_action(
                                format!("Delete branch {branch}?"),
                                format!(
                                    "This deletes {branch} from {owner}. Local branches and worktrees are not changed."
                                ),
                                "Delete branch",
                                "delete-branch",
                                None,
                                window,
                                cx,
                            )
                            }),
                        ));
                }
                menu
            })
    }

    /// The Summary tab bar's checks line, or "Approve workflows to run" when runs wait on the viewer.
    fn render_pr_checks_status(
        &self,
        detail: &Detail,
        state: Option<&'static str>,
        stale: bool,
        t: &Tokens,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let approvals = if detail.state == "open" { detail.workflow_approvals_required.unwrap_or(0) } else { 0 };
        if approvals > 0 && !stale && logic::can(detail, "approve-workflows") {
            let label = match (panel.pending, panel.armed) {
                (Some("approve-workflows"), _) => "Approving...",
                (_, Some("approve-workflows")) => "Confirm approve?",
                _ => "Approve workflows to run",
            };
            let warning = t.warning_foreground;
            return Button::new("pr-approve-workflows")
                .outline()
                .xsmall()
                .ml_auto()
                .h(px(24.))
                .px(px(8.))
                .gap(px(4.))
                .text_xs()
                .border_color(Hsla { a: 0.4, ..t.warning })
                .text_color(warning)
                .disabled(panel.pending.is_some())
                .child(Icon::new(Lucide::Play).with_size(px(14.)).text_color(warning))
                .child(label)
                .on_click(
                    cx.listener(|this, _, window, cx| this.press_pr_action("approve-workflows", None, window, cx)),
                )
                .into_any_element();
        }
        let label = if stale {
            state.map_or("No checks reported".to_owned(), |s| logic::checks_state_label(s).to_owned())
        } else {
            logic::summarize_checks(&detail.checks)
        };
        let icon = match state.filter(|s| checks_glyph(s, t).is_some()) {
            Some(state) => self.render_pr_checks_popover(detail, state, stale, t, cx),
            None => Icon::new(Lucide::CircleDot).with_size(px(14.)).text_color(t.muted_foreground).into_any_element(),
        };
        h_flex()
            .ml_auto()
            .flex_1()
            .min_w_0()
            .justify_end()
            .gap(px(6.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(icon)
            .child(div().whitespace_nowrap().child(label))
            .into_any_element()
    }

    fn render_pr_timeline_status(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let pending = panel.activity.is_none() && panel.activity_loading;
        let failed = panel.activity.is_none() && panel.activity_error.is_some();
        let count = |n: Option<usize>| match (failed, pending, n) {
            (true, _, _) => "-".to_owned(),
            (_, true, _) | (_, _, None) => "…".to_owned(),
            (_, _, Some(n)) => n.to_string(),
        };
        let activity = panel.activity.as_ref();
        let approvals = activity
            .filter(|a| !a.comments_truncated)
            .map(|_| panel.review_outcomes.iter().filter(|(_, e)| e.outcome == "approved" && !e.stale).count())
            .unwrap_or(0);
        let approved = outcome_color("approved", t);
        let item =
            |icon: Lucide, text: String| h_flex().gap(px(4.)).child(Icon::new(icon).with_size(px(12.))).child(text);
        let newest = panel.timeline_newest;
        h_flex()
            .ml_auto()
            .flex_none()
            .gap(px(8.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(
                h_flex()
                    .gap(px(6.))
                    .text_size(px(11.))
                    .when(pending || failed, |el| el.opacity(0.35))
                    .child(item(Lucide::MessageSquare, count(activity.map(|a| a.comment_total()))))
                    .child(div().text_color(Hsla { a: 0.5, ..t.muted_foreground }).child("·"))
                    .child(item(Lucide::GitCommitHorizontal, count(activity.map(|a| a.commits.len()))))
                    .when(approvals > 0, |el| {
                        el.child(div().text_color(Hsla { a: 0.5, ..t.muted_foreground }).child("·"))
                            .child(item(Lucide::CircleCheck, approvals.to_string()).text_color(approved))
                    }),
            )
            .child(
                ghost_button("pr-timeline-order", t)
                    .child(Icon::new(Lucide::ArrowDownUp).with_size(px(12.)))
                    .child(if newest { "Newest first" } else { "Oldest first" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(panel) = this.pull_requests.panel.as_mut() {
                            panel.timeline_newest = !panel.timeline_newest;
                        }
                        this.rebuild_pr_timeline();
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_pr_summary(
        &mut self,
        detail: &Rc<Detail>,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let activity = panel.activity.clone();
        let (_, stale) = panel.checks_state(detail);
        let [description_open, checks_open, comments_open] = panel.sections;
        let scroll = panel.summary_scroll.clone();
        let reviewers = match &activity {
            Some(a) => a.reviewers.as_deref().unwrap_or(&detail.reviewers),
            None => detail.reviewers.as_slice(),
        };
        let reviewers = logic::reviewer_entries(reviewers, &panel.review_outcomes);

        let reviewer_picker = (detail.capabilities.reviewers.request && detail.capabilities.reviewers.list_candidates)
            .then(|| self.render_pr_picker(true, detail.viewer_permissions.request_reviewers, t, cx));
        let reviewer_row = meta_row(
            Lucide::Users,
            "Reviewers",
            h_flex()
                .gap(px(6.))
                .child(if reviewers.is_empty() {
                    div().text_color(t.muted_foreground).child("None").into_any_element()
                } else {
                    h_flex()
                        .children(
                            reviewers
                                .iter()
                                .enumerate()
                                .map(|(index, (actor, outcome))| reviewer_avatar(index, actor, outcome.as_ref(), t)),
                        )
                        .into_any_element()
                })
                .children(reviewer_picker)
                .into_any_element(),
            t,
        );
        let label_picker = (detail.capabilities.labels == Some(true))
            .then(|| self.render_pr_picker(false, detail.viewer_permissions.labels != Some(false), t, cx));
        let label_row = (!detail.labels.is_empty() || detail.capabilities.labels == Some(true)).then(|| {
            let value = h_flex()
                .flex_wrap()
                .gap(px(4.))
                .when(detail.labels.is_empty(), |el| el.child(div().text_color(t.muted_foreground).child("None")))
                .children(detail.labels.iter().map(|l| label_chip(&l.name, l.color.as_deref(), 0, t)))
                .children(label_picker)
                .into_any_element();
            meta_row(Lucide::Tag, "Labels", value, t)
        });

        let description = description_open.then(|| {
            if self.pr_editing("description") {
                return self.render_pr_editor(t, cx);
            }
            let text = if detail.body.trim().is_empty() { "_No description provided._" } else { detail.body.as_str() };
            let markdown =
                self.pr_markdown("description", text, cx).map(|state| pr_markdown_view(&state, t, cx.entity()));
            let edit = logic::can_edit_change_request(detail)
                .then(|| self.pr_edit_button("description".into(), "description", detail.body.clone(), t, cx));
            let reactions = activity.as_ref().and_then(|a| self.render_pr_reactions(None, &a.reactions, t, cx));
            Some(
                v_flex()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .group(write::EDIT_GROUP)
                            .items_start()
                            .gap(px(4.))
                            .child(div().flex_1().min_w_0().children(markdown))
                            .children(edit),
                    )
                    .children(reactions)
                    .into_any_element(),
            )
        });
        let checks = checks_open.then(|| render_checks(&detail.checks, stale, t, cx));
        let comment_total = activity.as_ref().map_or(0, |a| a.comment_total());
        let comments_body = comments_open.then(|| self.render_pr_comments(detail, activity.as_ref(), t, window, cx));
        let comments_actions = self.render_pr_comment_actions(activity.as_ref(), t, cx);

        v_flex()
            .id("pr-summary")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&scroll)
            .child(v_flex().px(px(16.)).pt(px(10.)).pb(px(4.)).gap(px(8.)).child(reviewer_row).children(label_row))
            .child(section(0, "Description", description_open, None, t, cx).children(description.flatten()))
            .child(section(1, "Checks", checks_open, None, t, cx).children(checks))
            .child(
                section(2, &format!("Comments ({comment_total})"), comments_open, Some(comments_actions), t, cx)
                    .children(comments_body),
            )
            .child(div().h(px(16.)))
            .into_any_element()
    }

    fn render_pr_comment_actions(
        &self,
        activity: Option<&Rc<Activity>>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let (bots, resolved, newest) = (panel.show_bots, panel.show_resolved, panel.comments_newest);
        let hidden = activity.map_or(0, |_| {
            let items = &panel.conversations;
            let all: usize = items.iter().map(|i| i.comments().count()).sum();
            let kept: usize =
                logic::filter_conversations(items, bots, resolved).iter().map(|i| i.comments().count()).sum();
            all - kept
        });
        let view = cx.entity();
        h_flex()
            .flex_none()
            .gap(px(4.))
            .child(
                ghost_button("pr-comment-filter", t)
                    .child(Icon::new(Lucide::ListFilter).with_size(px(12.)))
                    .child(if hidden > 0 { format!("{hidden} hidden") } else { "Filter".into() })
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu: PopupMenu, window, _| {
                        menu.label("Show")
                            .item(PopupMenuItem::new("Bot comments").checked(bots).on_click(window.listener_for(
                                &view,
                                |this, _, _, cx| {
                                    if let Some(panel) = this.pull_requests.panel.as_mut() {
                                        panel.show_bots = !panel.show_bots;
                                    }
                                    cx.notify();
                                },
                            )))
                            .item(PopupMenuItem::new("Resolved or dismissed").checked(resolved).on_click(
                                window.listener_for(&view, |this, _, _, cx| {
                                    if let Some(panel) = this.pull_requests.panel.as_mut() {
                                        panel.show_resolved = !panel.show_resolved;
                                    }
                                    cx.notify();
                                }),
                            ))
                    }),
            )
            .child(
                ghost_button("pr-comment-order", t)
                    .child(Icon::new(Lucide::ArrowDownUp).with_size(px(12.)))
                    .child(if newest { "Newest first" } else { "Oldest first" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(panel) = this.pull_requests.panel.as_mut() {
                            panel.comments_newest = !panel.comments_newest;
                        }
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn render_pr_comments(
        &mut self,
        detail: &Detail,
        activity: Option<&Rc<Activity>>,
        t: &Tokens,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let term = logic::terminology(&detail.provider).0;
        let Some(activity) = activity.cloned() else {
            return match &panel.activity_error {
                Some(error) if !panel.activity_loading => {
                    activity_unavailable(error.clone(), term, t, cx).into_any_element()
                }
                _ => conversation_ghost(t).into_any_element(),
            };
        };
        let (bots, resolved, newest, shown) =
            (panel.show_bots, panel.show_resolved, panel.comments_newest, panel.shown_conversations);
        let toggled = panel.toggled.clone();
        let items = panel.conversations.clone();
        let loaded: usize = items.iter().map(|i| i.comments().count()).sum();
        let filtered = logic::filter_conversations(&items, bots, resolved);
        let hidden_by_filter = loaded - filtered.iter().map(|i| i.comments().count()).sum::<usize>();
        let start = filtered.len().saturating_sub(shown);
        let older = start;
        let mut visible = filtered[start..].to_vec();
        if newest {
            visible.reverse();
        }
        let mut column = v_flex().gap(px(12.));
        if activity.comments_truncated {
            column = column.child(
                div()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(Hsla { a: 0.3, ..t.warning })
                    .bg(Hsla { a: 0.08, ..t.warning })
                    .px(px(8.))
                    .py(px(6.))
                    .text_xs()
                    .child(format!(
                        "This conversation is only partially loaded. {loaded} comment{} loaded; open it on the host to read the rest.",
                        if loaded == 1 { " is" } else { "s are" }
                    )),
            );
        }
        if items.is_empty() {
            return column.child(muted_line("No comments yet.", t)).into_any_element();
        }
        if filtered.is_empty() {
            let text = if hidden_by_filter == 1 {
                "The only comment is hidden by the filter.".to_owned()
            } else {
                format!("All {hidden_by_filter} comments are hidden by the filter.")
            };
            return column.child(muted_line(&text, t)).into_any_element();
        }
        let older_button = (older > 0).then(|| {
            let next = older.min(CONVERSATION_PAGE);
            outline_button("pr-older-conversations", t)
                .w_full()
                .child(format!("Show {next} older conversation{} ({older} hidden)", if older == 1 { "" } else { "s" }))
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(panel) = this.pull_requests.panel.as_mut() {
                        panel.shown_conversations += CONVERSATION_PAGE;
                    }
                    cx.notify();
                }))
        });
        let mut cards = Vec::with_capacity(visible.len());
        for item in visible {
            cards.push(self.render_conversation(item, &toggled, t, cx));
        }
        let (top, bottom) = if newest { (None, older_button) } else { (older_button, None) };
        column
            .children(top)
            .children(cards)
            .children(bottom)
            .when(shown > CONVERSATION_PAGE, |el| {
                el.child(
                    ghost_button("pr-recent-conversations", t)
                        .w_full()
                        .justify_center()
                        .child(format!("Show only {CONVERSATION_PAGE} recent conversations"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(panel) = this.pull_requests.panel.as_mut() {
                                panel.shown_conversations = CONVERSATION_PAGE;
                            }
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    fn render_conversation(
        &mut self,
        item: &Conversation,
        toggled: &HashSet<String>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let root = item.comments().next().expect("a conversation has a first comment");
        let thread = item.thread();
        let (replies, more, has_more): (&[Comment], usize, bool) = match item {
            Conversation::Thread { replies, more_replies, has_more, .. } => (replies, *more_replies, *has_more),
            Conversation::Comment(_) => (&[], 0, false),
        };
        let more_url = item.comments().find_map(|c| c.url.clone());
        let more_replies = has_more.then(|| more_replies_line(more, more_url, t));
        let mut reply_cards = Vec::with_capacity(replies.len());
        for reply in replies {
            reply_cards.push(self.render_comment_card(reply, None, true, t, cx));
        }
        let replies_column = (!reply_cards.is_empty()).then(|| {
            v_flex()
                .ml(px(16.))
                .pl(px(12.))
                .gap(px(8.))
                .border_l_2()
                .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                .children(reply_cards)
        });
        if !item.finished() {
            let card = self.render_comment_card(root, thread, false, t, cx);
            return v_flex().gap(px(8.)).child(card).children(replies_column).children(more_replies).into_any_element();
        }
        // A resolved thread or dismissed review folds to one line (`CollapsedComment`).
        let key = format!("comment:{}", item.id());
        let open = toggled.contains(&key);
        let label = if thread.is_some_and(|t| t.is_resolved) { "Resolved" } else { "Review dismissed" };
        let reply_count = replies.len() + more;
        let body = logic::visible_body(&root.body);
        let preview = (!open).then(|| body.as_deref().map(preview_line)).flatten();
        let opened_body =
            if open { body.as_ref().and_then(|b| self.pr_markdown(&format!("c:{}", root.id), b, cx)) } else { None };
        let toggle_key = key.clone();
        v_flex()
            .rounded(px(8.))
            .border_1()
            .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
            .child(
                v_flex()
                    .p(px(12.))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap(px(8.))
                            .child(comment_identity(root, t))
                            .when(reply_count > 0, |el| {
                                el.child(div().text_xs().text_color(t.muted_foreground).child(format!(
                                    "{reply_count} {}",
                                    if reply_count == 1 { "reply" } else { "replies" }
                                )))
                            })
                            .child(
                                h_flex()
                                    .id(ElementId::Name(format!("pr-fold-{}", item.id()).into()))
                                    .gap(px(6.))
                                    .px(px(8.))
                                    .py(px(4.))
                                    .rounded(px(6.))
                                    .text_xs()
                                    .text_color(t.muted_foreground)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(t.muted).text_color(t.foreground))
                                    .child(label)
                                    .child(
                                        Icon::new(if open { Lucide::ChevronUp } else { Lucide::ChevronDown })
                                            .with_size(px(14.)),
                                    )
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_pr_disclosure(toggle_key.clone(), cx)
                                    })),
                            ),
                    )
                    .children(comment_location(root, thread, t))
                    .children(
                        preview.map(|line| {
                            div().mt(px(8.)).truncate().text_xs().text_color(t.muted_foreground).child(line)
                        }),
                    ),
            )
            .when(open, |el| {
                el.child(
                    v_flex()
                        .px(px(12.))
                        .pb(px(12.))
                        .gap(px(12.))
                        .children(opened_body.map(|state| pr_markdown_view(&state, t, cx.entity())))
                        .children(replies_column),
                )
            })
            .children(more_replies.map(|line| div().px(px(12.)).pb(px(12.)).child(line)))
            .into_any_element()
    }

    fn render_comment_card(
        &mut self,
        comment: &Comment,
        thread: Option<&logic::ReviewThread>,
        reply: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = logic::visible_body(&comment.body);
        let markdown = body.as_ref().and_then(|b| self.pr_markdown(&format!("c:{}", comment.id), b, cx));
        let can_edit = self
            .pull_requests
            .panel
            .as_ref()
            .and_then(|p| p.detail.as_ref())
            .is_some_and(|d| logic::can_edit_comment(d, &comment.kind, comment.author.as_ref(), comment.can_edit));
        let editor = if self.pr_editing(&comment.id) { self.render_pr_editor(t, cx) } else { None };
        let edit_kind: &'static str = if comment.kind == "review-comment" { "review-comment" } else { "issue-comment" };
        let edit = (can_edit && editor.is_none())
            .then(|| self.pr_edit_button(comment.id.clone(), edit_kind, comment.body.clone(), t, cx));
        let reactions = self.render_pr_reactions(Some(&comment.id), &comment.reactions, t, cx);
        let outcome = logic::review_outcome(comment.review_state.as_deref());
        let state = match (outcome, comment.review_state.as_deref()) {
            (Some(outcome), _) => Some(outcome_badge(outcome, t)),
            (None, Some(state)) if !state.is_empty() => {
                Some(div().text_xs().text_color(t.muted_foreground).child(friendly_state(state)).into_any_element())
            }
            _ => None,
        };
        v_flex()
            .rounded(px(8.))
            .border_1()
            .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
            .bg(t.background)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .rounded_t(px(8.))
                    .bg(Hsla { a: t.muted.a * 0.25, ..t.muted })
                    .px(px(12.))
                    .py(px(10.))
                    .child(comment_identity(comment, t))
                    .children(state)
                    .children(reactions.map(|r| div().ml_auto().child(r))),
            )
            .when(!reply, |el| {
                el.children(comment_location(comment, thread, t).map(|row| div().px(px(12.)).child(row)))
            })
            .map(|el| match editor {
                Some(editor) => el.child(div().px(px(12.)).py(px(12.)).child(editor)),
                None if markdown.is_some() || edit.is_some() => el.child(
                    h_flex()
                        .group(write::EDIT_GROUP)
                        .items_start()
                        .gap(px(4.))
                        .px(px(12.))
                        .py(px(12.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .children(markdown.map(|state| pr_markdown_view(&state, t, cx.entity()))),
                        )
                        .children(edit),
                ),
                None => el,
            })
            .into_any_element()
    }

    fn render_pr_timeline(&mut self, detail: &Rc<Detail>, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let term = logic::terminology(&detail.provider).0;
        if panel.activity.is_none() {
            return match &panel.activity_error {
                Some(error) if !panel.activity_loading => {
                    div().p(px(16.)).child(activity_unavailable(error.clone(), term, t, cx)).into_any_element()
                }
                _ => div().p(px(16.)).child(conversation_ghost(t)).into_any_element(),
            };
        }
        let rows = panel.timeline_rows.clone();
        if rows.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .text_xs()
                .text_color(t.muted_foreground)
                .child(Icon::new(Lucide::GitPullRequestArrow).with_size(px(20.)))
                .child("No activity yet.")
                .into_any_element();
        }
        let toggled = panel.toggled.clone();
        let newest_commit = panel.newest_commit;
        let view = cx.entity();
        let t = *t;
        let now = now_ms();
        let term: &'static str = term;
        let count = rows.len();
        list(panel.timeline.clone(), move |index, window, cx| {
            let row = match &rows[index] {
                TimelineRow::Comments(events) => conversation_group(events, &toggled, &view, now, &t, window, cx),
                TimelineRow::Event(event) => match (&event.thread, event.kind) {
                    (Some(_), _) => thread_event(event, &toggled, &view, now, &t, window, cx),
                    (None, EventKind::Commit) => commit_event(event, now, &t),
                    (None, EventKind::Opened | EventKind::Merged | EventKind::Closed) => {
                        lifecycle_event(event, term, now, &t)
                    }
                    (None, _) => match logic::review_outcome(event.review_state.as_deref()) {
                        Some(outcome) => verdict_event(
                            event,
                            outcome,
                            logic::verdict_stale(&event.at, newest_commit),
                            &view,
                            now,
                            &t,
                            cx,
                        ),
                        None => thread_event(event, &toggled, &view, now, &t, window, cx),
                    },
                },
            };
            // The connector line runs behind the markers, from the first row to the last.
            div()
                .relative()
                .w_full()
                .max_w(px(768.))
                .mx_auto()
                .px(px(16.))
                .when(index == 0, |el| el.pt(px(20.)))
                .when(index + 1 == count, |el| el.pb(px(20.)))
                .child(
                    div()
                        .absolute()
                        .left(px(31.))
                        .top(px(if index == 0 { 44. } else { 0. }))
                        .bottom(px(if index + 1 == count { 40. } else { 0. }))
                        .w(px(1.))
                        .bg(Hsla { a: t.border.a * 0.45, ..t.border }),
                )
                .child(row)
                .into_any_element()
        })
        .size_full()
        .into_any_element()
    }

    fn render_pr_guide(&mut self, t: &Tokens, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let panel = self.pull_requests.panel.as_ref().expect("a panel is open");
        let result = panel.guide.clone().unwrap_or_default();
        let tour = panel.guide_tour.clone();
        let running = panel.guide_starting || result["run"]["_tag"] == "Running";
        let current = result["currentHeadSha"].as_str();
        let failure = (result["run"]["_tag"] == "Failed" && result["run"]["headSha"].as_str() == current)
            .then(|| result["run"]["detail"].as_str().unwrap_or_default().to_owned());
        const HINT: &str = "Check the review guide model in Settings → Source control.";
        let write_button = |label: &str, cx: &mut Context<Self>| {
            outline_button("pr-write-guide", t)
                .disabled(running)
                .child(Icon::new(if running { Lucide::LoaderCircle } else { Lucide::Sparkles }).with_size(px(14.)))
                .child(if running { "Reading the change...".to_owned() } else { label.to_owned() })
                .on_click(cx.listener(|this, _, window, cx| this.write_pr_guide(window, cx)))
        };
        let guide = &result["guide"];
        if guide.is_null() {
            if panel.guide.is_none() && panel.guide_loading {
                return v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(Lucide::LoaderCircle).with_size(px(16.)).text_color(t.muted_foreground))
                    .into_any_element();
            }
            let (title, description) = if running {
                (
                    "Writing a review guide",
                    "An agent is reading the change. A large one takes a few minutes. You can leave; you will be told when it is ready.".to_owned(),
                )
            } else if failure.is_some() {
                ("Could not write a review guide", HINT.to_owned())
            } else {
                (
                    "No review guide yet",
                    panel.guide_error.as_ref().map(ToString::to_string).unwrap_or_else(|| {
                        "A guide is a tour of the change: stops through the code in reading order, with the calls only a maintainer can make.".into()
                    }),
                )
            };
            return v_flex()
                .id("pr-guide")
                .size_full()
                .overflow_y_scroll()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .px(px(16.))
                .py(px(40.))
                .text_center()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(title))
                .child(div().max_w(px(448.)).text_xs().text_color(t.muted_foreground).child(description))
                .child(write_button(if failure.is_some() { "Try again" } else { "Write guide" }, cx))
                .children(failure.filter(|_| !running).map(|detail| failure_details(detail, t)))
                .into_any_element();
        }
        let stale = current.is_some_and(|sha| guide["headSha"].as_str() != Some(sha));
        let banner = (stale || failure.is_some() || running).then(|| {
            let text = if running {
                "Rewriting this guide. You will be told when the new one is ready.".to_owned()
            } else if failure.is_some() {
                format!("Could not rewrite this guide. {HINT}")
            } else {
                "Written before the latest push.".to_owned()
            };
            let tint = if running { t.info } else { t.warning };
            h_flex()
                .gap(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(Hsla { a: 0.3, ..tint })
                .bg(Hsla { a: 0.08, ..tint })
                .px(px(12.))
                .py(px(8.))
                .text_xs()
                .child(Icon::new(if running { Lucide::LoaderCircle } else { Lucide::TriangleAlert }).with_size(px(14.)))
                .child(div().flex_1().min_w_0().child(text))
                .when(!running, |el| el.child(write_button("Rewrite", cx)))
        });
        match tour {
            Some(Ok(guide)) => {
                self.render_pr_guide_tour(&guide, banner.map(IntoElement::into_any_element), width, t, cx)
            }
            Some(Err(error)) => div().p(px(16.)).text_xs().text_color(t.error).child(error).into_any_element(),
            None => div().into_any_element(),
        }
    }
}

fn known_method(method: &str) -> Option<&'static str> {
    logic::MERGE_METHODS.iter().map(|m| m.0).find(|m| *m == method)
}

/// `PullRequestMarkdown`: the chat's markdown, with change request links routed like the web's.
fn pr_markdown_view(state: &Entity<TextViewState>, t: &Tokens, view: Entity<Workspace>) -> TextView {
    let view = view.downgrade();
    TextView::new(state).style(super::super::chat::markdown_style(t, false, true)).selectable(true).on_link_click(
        move |url, event, _, cx| {
            let external = event.modifiers().secondary(cx);
            let opened =
                !external && view.update(cx, |this, cx| this.open_change_request_link(url, cx)).unwrap_or(false);
            if !opened {
                cx.open_url(url);
            }
        },
    )
}

fn collapse_button(t: &Tokens, cx: &mut Context<Workspace>) -> Button {
    Button::new("pr-collapse")
        .ghost()
        .xsmall()
        .size(px(24.))
        .p_0()
        .rounded(px(6.))
        .tooltip("Collapse pull request panel")
        .child(Icon::new(Lucide::PanelRight).with_size(px(14.)).text_color(t.muted_foreground))
        .on_click(cx.listener(|this, _, _, cx| this.close_pull_request_panel(cx)))
}

/// `Button size="xs" variant="outline"`.
fn xs_outline(id: impl Into<ElementId>, t: &Tokens) -> Button {
    Button::new(id)
        .outline()
        .xsmall()
        .h(px(24.))
        .px(px(8.))
        .gap(px(4.))
        .rounded(px(6.))
        .border_color(t.input)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
}

/// `Button size="xs" variant="ghost-muted"`.
fn ghost_button(id: impl Into<ElementId>, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .h(px(24.))
        .px(px(8.))
        .gap(px(4.))
        .rounded(px(6.))
        .text_xs()
        .text_color(t.muted_foreground)
}

fn number_link(url: &str, number: u64, color: Hsla, open_label: &'static str) -> impl IntoElement {
    let (open, copy, menu_open) = (url.to_owned(), url.to_owned(), url.to_owned());
    h_flex()
        .id("pr-crumb-number")
        .flex_none()
        .gap(px(2.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .cursor_pointer()
        .child(format!("#{number}"))
        .child(Icon::new(Lucide::ExternalLink).with_size(px(10.)).text_color(color))
        .tooltip(move |window, cx| Tooltip::new(open_label).build(window, cx))
        .on_click(move |_, _, cx| cx.open_url(&open))
        .context_menu(move |menu, _, _| {
            let (copy, open) = (copy.clone(), menu_open.clone());
            menu.item(
                PopupMenuItem::new("Copy link")
                    .icon(Lucide::Copy)
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))),
            )
            .item(PopupMenuItem::new(open_label).on_click(move |_, _, cx| cx.open_url(&open)))
        })
}

/// `PullRequestCopyableCode`: shows "Copied" in place for a moment after a click.
fn copyable_code(
    id: &'static str,
    value: String,
    copied: bool,
    label: &str,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let tip: SharedString = format!("{}: {value}", if copied { "Copied" } else { label }).into();
    div()
        .id(id)
        .min_w_0()
        .max_w_full()
        .px(px(4.))
        .py(px(2.))
        .rounded(px(4.))
        .font_family(FONT_MONO)
        .cursor_pointer()
        .hover(|s| s.bg(Hsla { a: t.accent.a * 0.45, ..t.accent }).text_color(t.foreground))
        .child(div().truncate().child(if copied { "Copied".to_owned() } else { value }))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
}

/// `PullRequestMetaLine`: segments joined by a faint middle dot.
fn meta_line<const N: usize>(segments: [Option<AnyElement>; N], t: &Tokens) -> gpui_kit::Div {
    let mut line = h_flex().min_w_0().gap(px(6.)).whitespace_nowrap();
    for (index, segment) in segments.into_iter().flatten().enumerate() {
        if index > 0 {
            line = line.child(div().flex_none().text_color(Hsla { a: 0.5, ..t.muted_foreground }).child("·"));
        }
        line = line.child(segment);
    }
    line
}

/// `PullRequestActorAvatar`: the picture once read, else the login's initial.
fn avatar(actor: Option<&Actor>, size: f32, t: &Tokens) -> gpui_kit::Div {
    let login = actor.map_or("ghost", |a| a.login.as_str());
    let initial = div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(t.muted)
        .text_size(px((size * 0.6).min(11.)))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.muted_foreground)
        .child(login.chars().next().unwrap_or('?').to_uppercase().to_string());
    avatars::avatar_or(actor, size, initial)
}

/// `PullRequestActorLabel`: avatar and login, with the display name on hover.
fn actor_label(actor: Option<&Actor>, t: &Tokens) -> impl IntoElement {
    let login = actor.map_or("ghost", |a| a.login.as_str()).to_owned();
    let tip: SharedString = Actor::named(actor).into();
    h_flex()
        .id(ElementId::Name(format!("pr-actor-{login}").into()))
        .min_w_0()
        .gap(px(6.))
        .child(avatar(actor, 16., t))
        .child(div().truncate().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(login))
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
}

fn diff_stat(additions: u64, deletions: u64, size: f32, t: &Tokens) -> Option<impl IntoElement> {
    (additions + deletions > 0).then(|| {
        let addition: Hsla = if t.dark { rgb(0x00d492).into() } else { rgb(0x007a55).into() };
        h_flex()
            .flex_none()
            .gap(px(4.))
            .font_family(FONT_MONO)
            .text_size(px(size))
            .child(div().text_color(addition).child(format!("+{additions}")))
            .child(div().text_color(t.error).child(format!("-{deletions}")))
    })
}

fn state_glyph(state: &str, draft: bool, t: &Tokens) -> (Lucide, Hsla, &'static str) {
    match (state, draft) {
        ("merged", _) => (Lucide::GitMerge, tone(t, 0x7f22fe, 0xc4b4ff, 0.9), "Merged"),
        ("closed", _) => (Lucide::GitPullRequestClosed, tone(t, 0xe7000b, 0xffa2a2, 0.9), "Closed"),
        (_, true) => (Lucide::GitPullRequestDraft, tone(t, 0x71717b, 0x9f9fa9, 0.8), "Draft"),
        _ => (Lucide::GitPullRequestArrow, tone(t, 0x009966, 0x5ee9b5, 0.9), "Open"),
    }
}

fn checks_glyph(state: &str, t: &Tokens) -> Option<(Lucide, Hsla, &'static str)> {
    Some(match state {
        "passing" => (Lucide::CircleCheck, tone(t, 0x009966, 0x5ee9b5, 0.9), logic::checks_state_label("passing")),
        "failing" => (Lucide::CircleX, t.error, logic::checks_state_label("failing")),
        "pending" => (Lucide::CircleDot, tone(t, 0xe17100, 0xffb900, 0.9), logic::checks_state_label("pending")),
        _ => return None,
    })
}

/// `CHECK_STATUS_PRESENTATION`. A running check shows a still spinner glyph, not an animation.
fn check_status_icon(status: &str, t: &Tokens) -> (Lucide, Hsla) {
    let muted = Hsla { a: 0.7, ..t.muted_foreground };
    match status {
        "pending" => (Lucide::LoaderCircle, tone(t, 0xfe9a00, 0xfe9a00, 1.)),
        "action-required" => (Lucide::CircleDot, tone(t, 0xe17100, 0xffb900, 0.9)),
        "success" => (Lucide::CircleCheck, tone(t, 0x009966, 0x5ee9b5, 0.9)),
        "failure" | "cancelled" => (Lucide::CircleX, t.error),
        "skipped" => (Lucide::CircleSlash, muted),
        _ => (Lucide::CircleDashed, muted),
    }
}

fn outcome_color(outcome: &str, t: &Tokens) -> Hsla {
    match outcome {
        "approved" => tone(t, 0x009966, 0x5ee9b5, 0.9),
        "changes-requested" => t.error,
        _ => t.muted_foreground,
    }
}

fn outcome_icon(outcome: &str) -> Lucide {
    match outcome {
        "approved" => Lucide::CircleCheck,
        "changes-requested" => Lucide::CircleX,
        _ => Lucide::CircleDashed,
    }
}

/// `Badge size="control"`: an icon and label in a tinted pill, with the label as its tooltip.
fn badge(id: &'static str, icon: Lucide, label: String, fg: Hsla, tint: Hsla, _: &Tokens) -> AnyElement {
    let tip: SharedString =
        format!("{label}: the host will merge this on its own once its requirements are met").into();
    h_flex()
        .id(id)
        .h(px(24.))
        .px(px(8.))
        .gap(px(4.))
        .rounded(px(6.))
        .bg(Hsla { a: 0.12, ..tint })
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(fg)
        .child(Icon::new(icon).with_size(px(14.)).text_color(fg))
        .child(label)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .into_any_element()
}

fn outcome_badge(outcome: &str, t: &Tokens) -> AnyElement {
    let color = outcome_color(outcome, t);
    h_flex()
        .h(px(18.))
        .px(px(6.))
        .gap(px(4.))
        .rounded(px(4.))
        .bg(Hsla { a: 0.12, ..color })
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(Icon::new(outcome_icon(outcome)).with_size(px(12.)).text_color(color))
        .child(logic::outcome_label(outcome))
        .into_any_element()
}

/// `friendlyReviewState`: `CHANGES_REQUESTED` reads "Changes requested".
fn friendly_state(state: &str) -> String {
    logic::capitalize(&state.to_lowercase().replace(['_', '-'], " "))
}

fn reviewer_avatar(index: usize, actor: &Actor, outcome: Option<&ReviewOutcome>, t: &Tokens) -> AnyElement {
    let named = Actor::named(Some(actor));
    let tip: SharedString = match outcome {
        Some(entry) if entry.stale => format!("{named}: {} earlier changes", logic::outcome_label(entry.outcome)),
        Some(entry) => format!("{named}: {}", logic::outcome_label(entry.outcome)),
        None => named,
    }
    .into();
    let ring = outcome.map(|entry| {
        let color = outcome_color(entry.outcome, t);
        if entry.stale { list_mix(color, t.background, 0.35) } else { color }
    });
    div()
        .id(ElementId::Name(format!("pr-reviewer-{}", actor.login).into()))
        .relative()
        .when(index > 0, |el| el.ml(px(-4.)))
        .rounded_full()
        .when_some(ring, |el, ring| el.border_2().border_color(ring))
        .child(avatar(Some(actor), 16., t))
        .when_some(outcome, |el, entry| {
            let color = outcome_color(entry.outcome, t);
            el.child(
                div()
                    .absolute()
                    .right(px(-4.))
                    .bottom(px(-4.))
                    .rounded_full()
                    .bg(t.background)
                    .when(entry.stale, |el| el.opacity(0.5))
                    .child(Icon::new(outcome_icon(entry.outcome)).with_size(px(12.)).text_color(color)),
            )
        })
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .into_any_element()
}

fn list_mix(a: Hsla, b: Hsla, share: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let lerp = |x: f32, y: f32| x * share + y * (1. - share);
    gpui_kit::Rgba { r: lerp(a.r, b.r), g: lerp(a.g, b.g), b: lerp(a.b, b.b), a: 1. }.into()
}

fn meta_row(icon: Lucide, label: &'static str, value: AnyElement, t: &Tokens) -> impl IntoElement {
    h_flex()
        .min_h(px(24.))
        .min_w_0()
        .gap(px(8.))
        .text_xs()
        .child(
            h_flex()
                .w(px(96.))
                .flex_none()
                .gap(px(6.))
                .text_color(t.muted_foreground)
                .child(Icon::new(icon).with_size(px(14.)))
                .child(label),
        )
        .child(div().flex_1().min_w_0().text_color(t.foreground).child(value))
}

/// `Section`: a collapsible block of the Summary tab with its heading.
fn section(
    index: usize,
    title: &str,
    open: bool,
    actions: Option<AnyElement>,
    t: &Tokens,
    cx: &mut Context<Workspace>,
) -> gpui_kit::Div {
    let heading = h_flex()
        .w_full()
        .pr(px(16.))
        .child(
            h_flex()
                .id(ElementId::Name(format!("pr-section-{index}").into()))
                .flex_1()
                .min_w_0()
                .gap(px(6.))
                .px(px(16.))
                .py(px(12.))
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.muted_foreground)
                .cursor_pointer()
                .hover(|s| s.text_color(t.foreground))
                .child(title.to_owned())
                .child(
                    Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight })
                        .with_size(px(14.))
                        .text_color(Hsla { a: 0.6, ..t.muted_foreground }),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(panel) = this.pull_requests.panel.as_mut() {
                        panel.sections[index] = !panel.sections[index];
                    }
                    cx.notify();
                })),
        )
        .children(actions);
    v_flex().w_full().child(heading).when(open, |el| el.px(px(16.)).pb(px(16.)))
}

fn render_checks(checks: &[Check], stale: bool, t: &Tokens, cx: &mut Context<Workspace>) -> AnyElement {
    if stale {
        return h_flex()
            .gap(px(8.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child("Check details are out of date.")
            .child(
                ghost_button("pr-checks-refresh", t)
                    .child("Refresh")
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_pr_from_host(cx))),
            )
            .into_any_element();
    }
    if checks.is_empty() {
        return muted_line("No checks reported.", t).into_any_element();
    }
    v_flex()
        .children(checks.iter().enumerate().map(|(index, check)| {
            let (icon, color) = check_status_icon(&check.status, t);
            let url = check.url.clone();
            h_flex()
                .id(ElementId::Name(format!("pr-check-{index}-{}", check.name).into()))
                .items_start()
                .gap(px(8.))
                .px(px(8.))
                .py(px(8.))
                .rounded(px(6.))
                .text_xs()
                .line_height(px(20.))
                .when(url.is_some(), |el| el.cursor_pointer())
                .hover(|s| s.bg(Hsla { a: t.accent.a * 0.6, ..t.accent }))
                .child(div().mt(px(3.)).child(Icon::new(icon).with_size(px(14.)).text_color(color)))
                .child(div().flex_1().min_w_0().text_color(t.foreground).child(check.name.clone()))
                .child(div().flex_none().text_color(t.muted_foreground).child(logic::check_status_label(check)))
                .when_some(url, |el, url| el.on_click(move |_, _, cx| cx.open_url(&url)))
        }))
        .into_any_element()
}

fn muted_line(text: &str, t: &Tokens) -> impl IntoElement {
    div().py(px(8.)).text_xs().text_color(t.muted_foreground).child(text.to_owned())
}

fn comment_identity(comment: &Comment, t: &Tokens) -> impl IntoElement {
    let now = now_ms();
    let time = parse_ms(&comment.created_at).map(|at| relative_time(now - at)).unwrap_or_default();
    let url = comment.url.clone();
    let tip: SharedString =
        format!("{}{}", comment.created_at, if url.is_some() { " · Open comment on host" } else { "" }).into();
    h_flex().flex_1().min_w_0().flex_wrap().gap(px(8.)).text_xs().child(actor_label(comment.author.as_ref(), t)).child(
        div()
            .id(ElementId::Name(format!("pr-comment-time-{}", comment.id).into()))
            .text_color(t.muted_foreground)
            .when(url.is_some(), |el| el.cursor_pointer().hover(|s| s.text_color(t.foreground)))
            .child(time)
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .when_some(url, |el, url| el.on_click(move |_, _, cx| cx.open_url(&url))),
    )
}

fn comment_location(comment: &Comment, thread: Option<&logic::ReviewThread>, t: &Tokens) -> Option<impl IntoElement> {
    let path = thread.map(|t| t.path.clone()).or_else(|| comment.path.clone())?;
    let label = match thread.and_then(|t| t.line) {
        Some(line) => format!("{path}:{line}"),
        None => path,
    };
    Some(
        h_flex()
            .mt(px(8.))
            .min_w_0()
            .gap(px(8.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(div().min_w_0().truncate().font_family(FONT_MONO).child(label))
            .when(thread.is_some_and(|t| t.is_outdated), |el| el.child(div().flex_none().child("Outdated"))),
    )
}

/// A folded comment's one-line preview, with markdown syntax stripped.
fn preview_line(body: &str) -> String {
    let stripped = logic::strip_html_comments(body);
    let mut out = String::new();
    for line in stripped.lines() {
        let line = line.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '>' | '#' | '*' | '-'));
        if line.starts_with("[!") && line.ends_with(']') {
            continue;
        }
        out.push_str(&line.replace(['*', '`'], ""));
        out.push(' ');
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn more_replies_line(more: usize, url: Option<String>, t: &Tokens) -> AnyElement {
    let text = if more > 0 {
        format!("{more} more {}", if more == 1 { "reply" } else { "replies" })
    } else {
        "More replies".into()
    };
    h_flex()
        .flex_wrap()
        .gap(px(8.))
        .py(px(4.))
        .text_xs()
        .text_color(t.muted_foreground)
        .child(text)
        .children(url.map(|url| {
            div()
                .id("pr-more-replies")
                .cursor_pointer()
                .hover(|s| s.text_color(t.foreground))
                .child("View on host")
                .on_click(move |_, _, cx| cx.open_url(&url))
        }))
        .into_any_element()
}

/// `PullRequestActivityUnavailableState` (compact).
fn activity_unavailable(error: SharedString, term: &str, t: &Tokens, cx: &mut Context<Workspace>) -> impl IntoElement {
    v_flex()
        .gap(px(8.))
        .py(px(8.))
        .text_xs()
        .child(
            div()
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.foreground)
                .child(format!("Could not load this {term}'s activity")),
        )
        .child(div().text_color(t.muted_foreground).child(error))
        .child(
            outline_button("pr-activity-retry", t)
                .child(Icon::new(Lucide::RefreshCw).with_size(px(14.)).text_color(t.muted_foreground))
                .child("Retry")
                .on_click(cx.listener(|this, _, _, cx| this.read_pr_detail(cx))),
        )
}

fn ghost_bar(width: gpui_kit::DefiniteLength, height: f32, t: &Tokens) -> gpui_kit::Div {
    div().w(width).h(px(height)).rounded(px(4.)).bg(Hsla { a: 0.15, ..t.muted_foreground })
}

/// `PullRequestConversationGhost`: still placeholder cards while activity loads.
fn conversation_ghost(t: &Tokens) -> impl IntoElement {
    v_flex().gap(px(12.)).children((0..3).map(|index| {
        v_flex()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
            .child(h_flex().gap(px(8.)).child(ghost_bar(px(16.).into(), 16., t).rounded_full()).child(ghost_bar(
                px(96.).into(),
                12.,
                t,
            )))
            .child(ghost_bar(gpui_kit::relative([0.9, 0.7, 0.8][index]), 12., t))
            .child(ghost_bar(gpui_kit::relative([0.6, 0.5, 0.4][index]), 12., t))
    }))
}

/// `PullRequestDetailGhost`'s body.
fn detail_ghost(t: &Tokens) -> AnyElement {
    v_flex()
        .p(px(16.))
        .gap(px(12.))
        .child(ghost_bar(gpui_kit::relative(0.4), 12., t))
        .child(ghost_bar(gpui_kit::relative(0.6), 12., t))
        .child(div().h(px(8.)))
        .child(conversation_ghost(t))
        .into_any_element()
}

fn failure_details(detail: String, t: &Tokens) -> impl IntoElement {
    v_flex()
        .w_full()
        .max_w(px(512.))
        .gap(px(8.))
        .text_xs()
        .text_color(t.muted_foreground)
        .child("Technical details")
        .child(
            div()
                .max_h(px(160.))
                .overflow_hidden()
                .rounded(px(8.))
                .border_1()
                .border_color(t.border)
                .bg(Hsla { a: t.muted.a * 0.4, ..t.muted })
                .p(px(12.))
                .text_left()
                .font_family(FONT_MONO)
                .child(detail),
        )
}

// Timeline rows (`PullRequestTimelineTab`). Each sits 48px right of the connector line.

fn marker(child: impl IntoElement, top: Option<f32>, t: &Tokens) -> impl IntoElement {
    div()
        .absolute()
        .left_0()
        .map(|el| match top {
            Some(top) => el.top(px(top)),
            None => el.top(px(4.)),
        })
        .size(px(32.))
        .flex()
        .items_center()
        .justify_center()
        .bg(t.background)
        .child(child)
}

fn icon_marker(icon: Lucide, top: Option<f32>, t: &Tokens) -> impl IntoElement {
    marker(Icon::new(icon).with_size(px(14.)).text_color(t.muted_foreground), top, t)
}

fn actor_marker(actor: Option<&Actor>, fallback: Lucide, muted: bool, top: Option<f32>, t: &Tokens) -> AnyElement {
    match actor {
        Some(actor) => {
            marker(avatar(Some(actor), 28., t).when(muted, |el| el.opacity(0.45)), top, t).into_any_element()
        }
        None => icon_marker(fallback, top, t).into_any_element(),
    }
}

fn time_label(at: &str, now: i64) -> String {
    parse_ms(at).map(|at| relative_time(now - at)).unwrap_or_default()
}

fn actor_name(actor: Option<&Actor>, t: &Tokens) -> impl IntoElement {
    div()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.foreground)
        .child(actor.map_or("ghost", |a| a.login.as_str()).to_owned())
}

fn open_on_host_button(id: String, url: Option<String>, t: &Tokens) -> Option<impl IntoElement> {
    url.map(|url| {
        div()
            .id(ElementId::Name(id.into()))
            .size(px(24.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .text_color(t.muted_foreground)
            .hover(|s| s.bg(t.accent).text_color(t.foreground))
            .child(Icon::new(Lucide::ExternalLink).with_size(px(12.)))
            .tooltip(|window, cx| Tooltip::new("Open activity on host").build(window, cx))
            .on_click(move |_, _, cx| cx.open_url(&url))
    })
}

fn timeline_body(event: &Event, view: &Entity<Workspace>, t: &Tokens, cx: &mut App) -> Option<AnyElement> {
    let body = event.body.as_ref()?;
    let state = view.update(cx, |this, cx| this.pr_markdown(&format!("c:{}", event.id), body, cx))?;
    Some(div().mt(px(12.)).child(pr_markdown_view(&state, t, view.clone())).into_any_element())
}

/// `ConversationCard`.
fn conversation_card(
    event: &Event,
    show_path: bool,
    view: &Entity<Workspace>,
    now: i64,
    t: &Tokens,
    cx: &mut App,
) -> AnyElement {
    let body = timeline_body(event, view, t, cx);
    v_flex()
        .py(px(8.))
        .child(
            h_flex()
                .px(px(8.))
                .gap(px(8.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap(px(6.))
                                .text_xs()
                                .child(actor_name(event.actor.as_ref(), t))
                                .child(div().text_color(t.muted_foreground).child(event.title.clone()))
                                .children(event.review_state.as_deref().map(|state| {
                                    div()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(t.muted_foreground)
                                        .child(friendly_state(state))
                                })),
                        )
                        .child(
                            meta_line(
                                [
                                    Some(div().child(time_label(&event.at, now)).into_any_element()),
                                    event.path.clone().filter(|_| show_path).map(|path| {
                                        h_flex()
                                            .min_w_0()
                                            .gap(px(4.))
                                            .child(Icon::new(Lucide::FileCode).with_size(px(12.)))
                                            .child(div().truncate().child(path))
                                            .into_any_element()
                                    }),
                                ],
                                t,
                            )
                            .mt(px(4.))
                            .text_size(px(11.))
                            .text_color(t.muted_foreground),
                        ),
                )
                .children(open_on_host_button(format!("pr-open-{}", event.id), event.url.clone(), t)),
        )
        .children(body.map(|body| div().px(px(8.)).pb(px(8.)).child(body)))
        .into_any_element()
}

fn conversation_group(
    events: &[Event],
    toggled: &HashSet<String>,
    view: &Entity<Workspace>,
    now: i64,
    t: &Tokens,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let first = &events[0];
    let key = format!("group:{}", first.id);
    let open = toggled.contains(&key);
    let mut actors: Vec<&Actor> = Vec::new();
    for actor in events.iter().filter_map(|e| e.actor.as_ref()) {
        if !actors.iter().any(|a| a.login == actor.login) {
            actors.push(actor);
        }
    }
    let count = events.len();
    let authors = actors.len();
    let cards: Vec<AnyElement> =
        if open { events.iter().map(|e| conversation_card(e, true, view, now, t, cx)).collect() } else { Vec::new() };
    div()
        .relative()
        .mb(px(20.))
        .pl(px(48.))
        .child(actor_marker(actors.first().copied(), Lucide::MessageSquare, !open, Some(8.), t))
        .child(
            h_flex()
                .id(ElementId::Name(key.clone().into()))
                .w_full()
                .min_w_0()
                .gap(px(12.))
                .py(px(8.))
                .cursor_pointer()
                .text_color(if open { t.foreground } else { t.muted_foreground })
                .when(!open, |el| el.opacity(0.55))
                .hover(|s| s.opacity(1.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("{count} {}", if count == 1 { "comment" } else { "comments" })),
                        )
                        .child(div().truncate().text_size(px(10.)).text_color(t.muted_foreground).child(format!(
                            "{authors} {} · {}",
                            if authors == 1 { "author" } else { "authors" },
                            time_label(&first.at, now)
                        ))),
                )
                .child(
                    Icon::new(if open { Lucide::ChevronUp } else { Lucide::ChevronDown })
                        .with_size(px(14.))
                        .text_color(t.muted_foreground),
                )
                .on_click(window.listener_for(view, move |this, _, _, cx| this.toggle_pr_disclosure(key.clone(), cx))),
        )
        .when(open, |el| el.child(v_flex().mt(px(4.)).gap(px(4.)).children(cards)))
        .into_any_element()
}

/// `ReviewThreadEvent`: a file-and-line heading that opens the finding and its replies.
fn thread_event(
    event: &Event,
    toggled: &HashSet<String>,
    view: &Entity<Workspace>,
    now: i64,
    t: &Tokens,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let Some(thread) = &event.thread else {
        return div()
            .relative()
            .mb(px(20.))
            .pl(px(48.))
            .child(conversation_card(event, true, view, now, t, cx))
            .into_any_element();
    };
    let dismissed = logic::review_outcome(event.review_state.as_deref()) == Some("dismissed");
    let key = format!("thread:{}", event.id);
    let expanded = (!thread.is_resolved && !dismissed) != toggled.contains(&key);
    let replies = thread.replies.len() + thread.more_replies;
    let location = match thread.line {
        Some(line) => format!("{}:{line}", thread.path),
        None => thread.path.clone(),
    };
    let success = t.success_foreground;
    let mut body = Vec::new();
    if expanded {
        body.push(conversation_card(event, false, view, now, t, cx));
        if !thread.replies.is_empty() {
            let cards: Vec<AnyElement> =
                thread.replies.iter().map(|r| conversation_card(r, false, view, now, t, cx)).collect();
            body.push(
                v_flex()
                    .ml(px(8.))
                    .pl(px(12.))
                    .gap(px(4.))
                    .border_l_1()
                    .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                    .children(cards)
                    .into_any_element(),
            );
        }
    }
    div()
        .relative()
        .mb(px(20.))
        .pl(px(48.))
        .child(actor_marker(event.actor.as_ref(), Lucide::MessageSquare, !expanded, Some(0.), t))
        .child(
            h_flex()
                .id(ElementId::Name(key.clone().into()))
                .w_full()
                .min_w_0()
                .gap(px(8.))
                .px(px(8.))
                .py(px(8.))
                .text_size(px(11.))
                .text_color(t.muted_foreground)
                .cursor_pointer()
                .hover(|s| s.text_color(t.foreground))
                .child(Icon::new(Lucide::FileCode).with_size(px(12.)))
                .child(div().min_w_0().truncate().font_family(FONT_MONO).child(location))
                .when(thread.is_resolved, |el| {
                    el.child(
                        h_flex()
                            .flex_none()
                            .gap(px(4.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(success)
                            .child(Icon::new(Lucide::CircleCheck).with_size(px(12.)))
                            .child("Resolved"),
                    )
                })
                .when(dismissed, |el| {
                    el.child(div().flex_none().font_weight(FontWeight::MEDIUM).child("Review dismissed"))
                })
                .when(thread.is_outdated, |el| {
                    el.child(div().flex_none().font_weight(FontWeight::MEDIUM).child("Outdated"))
                })
                .when(!expanded, |el| {
                    let mut text = format!("· {}", event.actor.as_ref().map_or("ghost", |a| a.login.as_str()));
                    if replies > 0 {
                        text.push_str(&format!(" · {replies} {}", if replies == 1 { "reply" } else { "replies" }));
                    }
                    el.child(div().flex_none().child(text))
                })
                .child(div().ml_auto().child(
                    Icon::new(if expanded { Lucide::ChevronUp } else { Lucide::ChevronDown }).with_size(px(14.)),
                ))
                .on_click(window.listener_for(view, move |this, _, _, cx| this.toggle_pr_disclosure(key.clone(), cx))),
        )
        .children(body)
        .when(thread.has_more, |el| {
            el.child(div().px(px(8.)).child(more_replies_line(thread.more_replies, thread.url.clone(), t)))
        })
        .into_any_element()
}

fn commit_event(event: &Event, now: i64, t: &Tokens) -> AnyElement {
    div()
        .relative()
        .mb(px(20.))
        .pl(px(48.))
        .child(actor_marker(event.commit_authors.first(), Lucide::GitCommitHorizontal, false, None, t))
        .child(
            h_flex()
                .min_w_0()
                .gap(px(10.))
                .py(px(6.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(t.foreground)
                                .child(event.body.clone().unwrap_or_else(|| "Untitled commit".into())),
                        )
                        .child(
                            h_flex()
                                .mt(px(4.))
                                .gap(px(8.))
                                .text_size(px(10.))
                                .text_color(t.muted_foreground)
                                .child(div().font_family(FONT_MONO).child(logic::short_oid(&event.id).to_owned()))
                                .child(time_label(&event.at, now)),
                        ),
                )
                .children(event.stat.and_then(|(a, d)| diff_stat(a, d, 10., t))),
        )
        .into_any_element()
}

fn lifecycle_event(event: &Event, term: &str, now: i64, t: &Tokens) -> AnyElement {
    let title = logic::capitalize(term);
    let (icon, label) = match event.kind {
        EventKind::Opened => (Lucide::GitPullRequestArrow, format!("{title} opened")),
        EventKind::Merged => (Lucide::GitMerge, format!("{title} merged")),
        _ => (Lucide::GitPullRequestClosed, format!("{title} closed")),
    };
    div()
        .relative()
        .mb(px(20.))
        .pl(px(48.))
        .child(icon_marker(icon, None, t))
        .child(
            v_flex()
                .py(px(6.))
                .text_xs()
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .when_some(event.actor.as_ref(), |el, actor| el.child(actor_name(Some(actor), t)))
                        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(label)),
                )
                .child(
                    div()
                        .mt(px(2.))
                        .text_size(px(11.))
                        .text_color(t.muted_foreground)
                        .child(time_label(&event.at, now)),
                ),
        )
        .into_any_element()
}

fn verdict_event(
    event: &Event,
    outcome: &'static str,
    stale: bool,
    view: &Entity<Workspace>,
    now: i64,
    t: &Tokens,
    cx: &mut App,
) -> AnyElement {
    let color = if stale { Hsla { a: 0.7, ..t.muted_foreground } } else { outcome_color(outcome, t) };
    let tip: SharedString = format!("{} earlier changes", logic::outcome_label(outcome)).into();
    let body = timeline_body(event, view, t, cx);
    div()
        .relative()
        .mb(px(20.))
        .pl(px(48.))
        .child(actor_marker(event.actor.as_ref(), outcome_icon(outcome), false, Some(8.), t))
        .child(
            h_flex()
                .min_w_0()
                .gap(px(8.))
                .py(px(6.))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .flex_wrap()
                                .gap(px(6.))
                                .text_xs()
                                .child(actor_name(event.actor.as_ref(), t))
                                .child(
                                    div()
                                        .id(ElementId::Name(format!("pr-verdict-{}", event.id).into()))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(color)
                                        .child(logic::outcome_label(outcome).to_lowercase())
                                        .when(stale, |el| {
                                            el.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                                        }),
                                ),
                        )
                        .child(
                            meta_line(
                                [
                                    Some(div().child(time_label(&event.at, now)).into_any_element()),
                                    event.path.clone().map(|path| {
                                        h_flex()
                                            .min_w_0()
                                            .gap(px(4.))
                                            .child(Icon::new(Lucide::FileCode).with_size(px(12.)))
                                            .child(div().truncate().child(path))
                                            .into_any_element()
                                    }),
                                ],
                                t,
                            )
                            .mt(px(2.))
                            .text_size(px(11.))
                            .text_color(t.muted_foreground),
                        ),
                )
                .children(open_on_host_button(format!("pr-open-{}", event.id), event.url.clone(), t)),
        )
        .children(body)
        .into_any_element()
}
