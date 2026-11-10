//! The thread card's pull request rows under its branch (`ThreadDetailsPrRows`,
//! `ThreadDetailsPrRow`): the current pull request with its title, checks and a rich tooltip, the
//! other linked ones behind "Show N more", one Resolve, Ready, Fix or Merge action each, and the
//! watch with its way to stop. Each shown row reads its pull request's detail and checks while
//! the card shows it.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, ClickEvent, Context, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
    rgb,
};
use serde_json::{Value, json};

use super::super::details::{CardColors, ROW_HEIGHT, keyboard_button, row};
use super::super::git::SOURCE_CONTROL_WRITE;
use super::super::{Target, Workspace};
use super::pull_request_detail::breakdown::{self, Breakdown};
use super::pull_request_detail::{check_status_icon, checks_body, toast_error, toast_success};
use super::pull_requests::{checks_glyph, tone};
use crate::pull_request_detail::{
    self as logic, Activity, CardAction, CheckoutMode, CheckoutRequest, ChecksRead, Detail, HandoffSubject, Reference,
};
use crate::pull_requests::State;
use crate::rpc::{RpcClient, RpcError};
use crate::theme::{self, Tokens};
use crate::thread_pull_requests::{self as links, PullRequestLink};

/// An open pull request's detail is read again this often (`useLiveRefresh`, ten minutes).
const DETAIL_INTERVAL: Duration = Duration::from_secs(600);
/// A read the card put off (hidden, disconnected, the app behind) is looked at again after this.
const RETRY_AFTER: Duration = Duration::from_secs(15);
const NO_WRITE: &str = "This connection cannot change source control.";
const NO_OPERATE: &str = "This connection cannot change threads.";
const WATCHING_TIP: &str =
    "Watching: the agent wakes when checks finish, someone comments, or the branch conflicts. Click to stop.";

/// The card's pull request rows across threads.
#[derive(Default)]
pub(in crate::app) struct CardPullRequests {
    /// Each read pull request by its reference key; one leaving the card keeps its answer.
    reads: HashMap<String, CardRead>,
    /// The keys the card shows now, which keep their reads going.
    wanted: Vec<String>,
    /// The thread (its details key) whose other pull requests are unfolded.
    expanded: Option<String>,
    /// Rows whose `pullRequests.runAction` is in flight (`actionPending`).
    acting: HashSet<String>,
    /// The checkout a Resolve or Fix prepares; one at a time, as they all open a thread.
    handoff: Option<(String, CardAction)>,
    /// The row whose Merge confirmation is open.
    confirming: Option<String>,
    /// The row whose checks popover shows every check.
    checks_all: Option<String>,
}

/// One pull request's reads (`pullRequestEnvironment.detail` and `.checks`).
struct CardRead {
    reference: Reference,
    detail: Option<Rc<Detail>>,
    checks: Option<ChecksRead>,
    /// The checks read landed after the detail read.
    checks_newer: bool,
    /// The detail under the checks read when that is not older (`resolvePullRequestDetailChecks`).
    shown: Option<Rc<Detail>>,
    detail_at: Option<Instant>,
    checks_at: Option<Instant>,
    /// The host answered no checks, which polling cannot find (`isSettledWithoutChecks`).
    no_checks: bool,
    _task: Option<Task<()>>,
}

/// What a row's reads do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    Detail,
    Checks,
    Wait(Duration),
}

impl CardRead {
    fn new(reference: Reference) -> Self {
        Self {
            reference,
            detail: None,
            checks: None,
            checks_newer: false,
            shown: None,
            detail_at: None,
            checks_at: None,
            no_checks: false,
            _task: None,
        }
    }

    /// The detail at once; while it is open, the detail every ten minutes and the checks
    /// (`usePullRequestChecksRefresh`) every 45 seconds while any run, else every minute.
    fn next(&self, supports_checks: bool, now: Instant) -> Next {
        let Some(detail_at) = self.detail_at else { return Next::Detail };
        if self.shown.as_ref().is_some_and(|d| d.state != State::Open) {
            return Next::Wait(DETAIL_INTERVAL);
        }
        let checks = self.shown.as_ref().filter(|_| supports_checks && !self.no_checks).map(|shown| {
            (self.checks_at.map_or(detail_at, |at| at + logic::checks_refresh_interval(&shown.checks)), Next::Checks)
        });
        let (due, read) = std::iter::once((detail_at + DETAIL_INTERVAL, Next::Detail))
            .chain(checks)
            .min_by_key(|(due, _)| *due)
            .unwrap_or((detail_at + DETAIL_INTERVAL, Next::Detail));
        if due <= now { read } else { Next::Wait(due.saturating_duration_since(now)) }
    }

    fn land(&mut self, kind: Next, answer: Result<Value, RpcError>, now: Instant) {
        match kind {
            Next::Detail => {
                self.detail_at = Some(now);
                if let Ok(detail) = answer.map(serde_json::from_value::<Detail>) {
                    match detail {
                        Ok(detail) => {
                            self.detail = Some(Rc::new(detail));
                            self.checks_newer = false;
                        }
                        Err(error) => log::warn!("unreadable pull request detail: {error}"),
                    }
                }
            }
            Next::Checks => {
                self.checks_at = Some(now);
                match answer {
                    Ok(Value::Null) => self.no_checks = true,
                    Ok(value) => {
                        if let Ok(read) = serde_json::from_value::<ChecksRead>(value) {
                            self.checks = Some(read);
                            self.checks_newer = true;
                        }
                    }
                    Err(_) => {}
                }
            }
            Next::Wait(_) => return,
        }
        self.shown = self.detail.as_ref().map(|detail| {
            match self.checks.as_ref().and_then(|read| logic::with_checks(detail, read, self.checks_newer)) {
                Some(merged) => Rc::new(merged),
                None => detail.clone(),
            }
        });
    }
}

/// One row as the card draws it.
struct RowSubject {
    /// Unique among the card's rows.
    id: String,
    /// Where its detail is read; `None` outside a project of the thread's environment.
    reference: Option<Reference>,
    number: u64,
    url: String,
    /// `#12: Title`, or `#12` until a title is known.
    label: String,
    /// The link's last synced state and draft flag.
    synced: Option<(State, bool)>,
    /// The link is watched, and the thread to stop it on.
    watch: Option<(String, Reference)>,
}

/// The rows the open target's card shows: the current one and, when unfolded, the others.
struct Subjects {
    environment_id: String,
    /// The card's details key, which the unfolded list belongs to.
    thread_key: String,
    current: RowSubject,
    rest: Vec<RowSubject>,
    expanded: bool,
}

fn link_state(link: &PullRequestLink) -> Option<(State, bool)> {
    let snapshot = link.snapshot.as_ref()?;
    let state = match snapshot.state.as_str() {
        "open" => State::Open,
        "closed" => State::Closed,
        "merged" => State::Merged,
        _ => State::Unknown,
    };
    Some((state, snapshot.is_draft))
}

/// `resolvePullRequestState`'s label.
fn state_label(state: State, is_draft: bool) -> &'static str {
    match (state, is_draft) {
        (State::Merged, _) => "Merged",
        (State::Closed, _) => "Closed",
        (_, true) => "Draft",
        _ => "Open",
    }
}

/// `resolvePullRequestState`'s icon and tone.
fn state_glyph(state: State, is_draft: bool, t: &Tokens) -> (Lucide, Hsla) {
    match (state, is_draft) {
        (State::Merged, _) => (Lucide::GitMerge, tone(t, 0x7f22fe, 0xc4b4ff, 0.9)),
        (State::Closed, _) => (Lucide::GitPullRequestClosed, tone(t, 0xe7000b, 0xffa2a2, 0.9)),
        (_, true) => (Lucide::GitPullRequestDraft, tone(t, 0x71717b, 0x9f9fa9, 0.8)),
        _ => (Lucide::GitPullRequestArrow, tone(t, 0x009966, 0x5ee9b5, 0.9)),
    }
}

fn provider_of(url: &str) -> &'static str {
    logic::parse_change_request(url).map_or("", |link| link.provider)
}

impl Workspace {
    /// The open target's rows, with the references their reads use.
    fn card_subjects(&self) -> Option<Subjects> {
        let (environment_id, thread_id, project_id, thread_key) = match self.target.as_ref()? {
            Target::Thread { env, id } => {
                let shell = self.shell.threads.iter().find(|t| &t.id == id && &t.environment_id == env)?;
                (env.clone(), Some(id.clone()), shell.project_id.clone(), format!("{env}:{id}"))
            }
            Target::Draft { env, project_id, draft } => {
                (env.clone(), None, project_id.clone(), format!("{env}:draft:{draft}"))
            }
        };
        let shell = thread_id
            .as_ref()
            .and_then(|id| self.shell.threads.iter().find(|t| &t.id == id && t.environment_id == environment_id));
        let links: &[PullRequestLink] = shell.map_or(&[], |s| s.pull_requests.as_slice());
        let status_pr = {
            let (branch, worktree_base, _) = self.branch_trigger();
            let status = self.vcs.status.as_ref().filter(|s| !worktree_base && s.local.ref_name.as_deref() == branch);
            status.and_then(|s| s.remote.pr.as_ref()).map(|pr| (pr.number, pr.url.as_str()))
        };
        let current = links::card_current(links, shell.and_then(|s| s.branch_pull_request.as_ref()), status_pr)?;
        // `supportsPullRequests`: the environment reports its server config.
        let readable = self.env_info(&environment_id).is_some();
        let projects: Vec<(&str, Option<&str>)> = self
            .shell
            .projects
            .iter()
            .filter(|p| p.environment_id == environment_id)
            .map(|p| (p.id.as_str(), p.repository_identity.as_ref().and_then(|i| i.canonical_key.as_deref())))
            .collect();
        let own_project = projects.iter().any(|(id, _)| *id == project_id).then_some(project_id.as_str());
        let reference = |project: Option<&str>, host: Option<String>, repository: &str, number: u64| {
            project.filter(|_| readable).map(|project| Reference {
                environment_id: environment_id.to_string(),
                project_id: project.to_owned(),
                host,
                repository: repository.to_owned(),
                number,
            })
        };
        let watch = |link: &PullRequestLink| {
            let thread = thread_id.clone().filter(|_| link.watch.is_some())?;
            let target = Reference {
                environment_id: environment_id.to_string(),
                project_id: project_id.clone(),
                host: Some(link.host.clone()),
                repository: link.repository.clone(),
                number: link.number,
            };
            Some((thread, target))
        };
        let titled = |number: u64, title: Option<&str>| match title.filter(|t| !t.trim().is_empty()) {
            Some(title) => format!("#{number}: {title}"),
            None => format!("#{number}"),
        };
        let current_reference = reference(own_project, current.host.clone(), &current.repository, current.number);
        let snapshot = current.link.and_then(|l| l.snapshot.as_ref());
        // Until a link syncs, a branch's pull request takes its title from its read.
        let read_title = current_reference
            .as_ref()
            .and_then(|r| self.pull_requests.card.reads.get(&r.key())?.shown.as_ref().map(|d| d.title.clone()));
        let current_row = RowSubject {
            id: current_reference.as_ref().map_or_else(|| current.url.clone(), Reference::key),
            reference: current_reference,
            number: current.number,
            url: current.url.clone(),
            label: titled(current.number, snapshot.map(|s| s.title.as_str()).or(read_title.as_deref())),
            synced: current.link.and_then(link_state),
            watch: current.link.and_then(watch),
        };
        let rest = links::card_rest(links, &current)
            .into_iter()
            .map(|link| {
                // `findProjectOnChangeRequestHost` among the environment's projects.
                let project = logic::project_for_link(projects.iter().copied(), &link.host, &link.repository);
                let reference = reference(project, Some(link.host.clone()), &link.repository, link.number);
                RowSubject {
                    id: reference.as_ref().map_or_else(|| link.url.clone(), Reference::key),
                    reference,
                    number: link.number,
                    url: link.url.clone(),
                    label: titled(link.number, link.snapshot.as_ref().map(|s| s.title.as_str())),
                    synced: link_state(link),
                    watch: watch(link),
                }
            })
            .collect();
        let expanded = self.pull_requests.card.expanded.as_deref() == Some(thread_key.as_str());
        Some(Subjects { environment_id: environment_id.to_string(), thread_key, current: current_row, rest, expanded })
    }

    /// Keeps the shown rows' reads going and stops the others, after this render.
    pub(in crate::app) fn follow_card_pull_requests(&self, shown: bool, window: &Window, cx: &mut Context<Self>) {
        let wanted: Vec<Reference> = match (shown && self.branch_row_shown()).then(|| self.card_subjects()).flatten() {
            Some(subjects) => {
                let rest = subjects.rest.into_iter().filter(|_| subjects.expanded);
                std::iter::once(subjects.current).chain(rest).filter_map(|row| row.reference).collect()
            }
            None => Vec::new(),
        };
        let same = wanted.len() == self.pull_requests.card.wanted.len()
            && wanted.iter().zip(&self.pull_requests.card.wanted).all(|(r, key)| r.key() == *key);
        if !same {
            cx.defer_in(window, move |this, window, cx| this.sync_card_pull_requests(wanted, window, cx));
        }
    }

    fn sync_card_pull_requests(&mut self, wanted: Vec<Reference>, window: &mut Window, cx: &mut Context<Self>) {
        let keys: Vec<String> = wanted.iter().map(Reference::key).collect();
        let card = &mut self.pull_requests.card;
        for (key, read) in &mut card.reads {
            if !keys.contains(key) {
                read._task = None;
            }
        }
        for reference in wanted {
            let key = reference.key();
            let read = card.reads.entry(key.clone()).or_insert_with(|| CardRead::new(reference));
            if read._task.is_none() {
                read._task = Some(card_read_task(key, window, cx));
            }
        }
        card.wanted = keys;
        self.settle_card_confirmation(window, cx);
    }

    /// What `key`'s reads do next; `None` once the card no longer shows it.
    fn card_read_step(&self, key: &str, window: &Window) -> Option<Step> {
        let card = &self.pull_requests.card;
        if !card.wanted.iter().any(|k| k == key) {
            return None;
        }
        let read = card.reads.get(key)?;
        let environment = &read.reference.environment_id;
        let supports_checks = self.env_info(environment).is_some_and(|info| info.capability("pullRequestChecks"));
        let kind = match read.next(supports_checks, Instant::now()) {
            Next::Wait(wait) => return Some(Step::Wait(wait)),
            kind => kind,
        };
        let on_screen = self.page.is_none() && self.details_shown(window);
        // A refresh waits for the app to be in front; the first read does not.
        let in_front = read.detail_at.is_none() || window.is_window_active();
        let Some(client) = self.env_client(environment).filter(|_| on_screen && in_front) else {
            return Some(Step::Wait(RETRY_AFTER));
        };
        let mut input = read.reference.to_json();
        if kind == Next::Detail {
            input["allowStale"] = json!(false);
        }
        Some(Step::Read(kind, client, input))
    }

    fn land_card_read(
        &mut self,
        key: &str,
        kind: Next,
        answer: Result<Value, RpcError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(read) = self.pull_requests.card.reads.get_mut(key) {
            read.land(kind, answer, Instant::now());
            cx.notify();
        }
        self.settle_card_confirmation(window, cx);
    }

    /// Reads `key` again from its detail, as after an action on it.
    fn reread_card_pull_request(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.pull_requests.card.wanted.iter().any(|k| k == key);
        if let Some(read) = self.pull_requests.card.reads.get_mut(key) {
            read.detail_at = None;
            read._task = wanted.then(|| card_read_task(key.to_owned(), window, cx));
        }
    }

    /// The row's action and whether this connection may take it.
    fn card_action_of(&self, key: &str) -> Option<(CardAction, Option<&'static str>)> {
        let read = self.pull_requests.card.reads.get(key)?;
        let action = logic::card_action(read.shown.as_deref()?)?;
        let environment = &read.reference.environment_id;
        // `permissionAtom` for what the action sends: a worktree checkout for a new thread, else `runAction`.
        let permitted = match action {
            CardAction::Resolve | CardAction::Fix => self.permits_card_handoff(environment),
            _ => self.env_permits(environment, "pullRequests.runAction", &read.reference.to_json()),
        };
        let denied = match permitted {
            true => None,
            false if !self.env_grants(environment, SOURCE_CONTROL_WRITE) => Some(NO_WRITE),
            false => Some(NO_OPERATE),
        };
        Some((action, denied))
    }

    /// `permissionAtom` for Resolve and Fix's worktree checkout, made for a new draft's thread.
    fn permits_card_handoff(&self, environment: &str) -> bool {
        let input = json!({"mode": "worktree", "threadId": "draft"});
        self.env_permits(environment, "git.preparePullRequestThread", &input)
    }

    /// The Merge confirmation closes once the row can no longer merge (`confirmingMerge`).
    fn settle_card_confirmation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.pull_requests.card.confirming.clone() else { return };
        let shown = self.pull_requests.card.wanted.contains(&key);
        if shown && self.card_action_of(&key) == Some((CardAction::Merge, None)) {
            return;
        }
        self.pull_requests.card.confirming = None;
        window.close_dialog(cx);
    }

    /// The method a row's Merge uses: the project's default, else the last one picked.
    fn card_merge_method(&self, reference: &Reference, detail: &Detail) -> &'static str {
        logic::resolve_merge_method(
            &logic::allowed_merge_methods(detail),
            None,
            self.pr_project_merge_method(reference).as_deref(),
            self.pull_requests.last_merge_method,
        )
    }

    fn press_card_action(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let card = &self.pull_requests.card;
        if card.acting.contains(key) || card.handoff.is_some() {
            return;
        }
        match self.card_action_of(key) {
            Some((CardAction::Ready, None)) => self.run_card_action(key, "ready", None, window, cx),
            Some((CardAction::Merge, None)) => self.confirm_card_merge(key, window, cx),
            Some((action @ (CardAction::Resolve | CardAction::Fix), None)) => {
                self.start_card_handoff(key, action, window, cx);
            }
            _ => {}
        }
    }

    /// "Merge pull request?": merges with the method it names unless the row stops being able to.
    fn confirm_card_merge(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(read) = self.pull_requests.card.reads.get(key) else { return };
        let Some(detail) = read.shown.clone() else { return };
        let method = self.card_merge_method(&read.reference, &detail);
        let term = logic::terminology(&detail.provider).0;
        let title: SharedString = format!("Merge {term}?").into();
        let ask = crate::app::confirm::Ask {
            title: Rc::new(move |_, _| title.clone().into_any_element()),
            description: Some(format!("This merges #{} using {method}.", detail.number).into()),
            confirm_label: "Merge".into(),
            destructive: false,
        };
        self.pull_requests.card.confirming = Some(key.to_owned());
        let answer = crate::app::confirm::ask_confirmation(ask, window, cx);
        let key = key.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let confirmed = answer.await.unwrap_or(false);
            let _ = this.update_in(cx, |this, window, cx| {
                let card = &mut this.pull_requests.card;
                let asked = card.confirming.as_deref() == Some(key.as_str());
                if asked {
                    card.confirming = None;
                }
                if confirmed && asked && this.card_action_of(&key) == Some((CardAction::Merge, None)) {
                    this.run_card_action(&key, "merge", Some(method), window, cx);
                }
            });
        })
        .detach();
    }

    /// `perform`: the action on the row's pull request, with a toast either way.
    fn run_card_action(
        &mut self,
        key: &str,
        action: &'static str,
        method: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(read) = self.pull_requests.card.reads.get(key) else { return };
        let reference = read.reference.clone();
        let term = logic::terminology(read.shown.as_ref().map_or("", |d| d.provider.as_str())).0;
        let mut input = reference.to_json();
        input["action"] = json!(action);
        input["replyWithSummary"] = json!(true);
        if let Some(method) = method {
            input["mergeMethod"] = json!(method);
        }
        if self.pull_requests.card.acting.contains(key)
            || !self.env_permits(&reference.environment_id, "pullRequests.runAction", &input)
        {
            return;
        }
        let Some(client) = self.env_client(&reference.environment_id) else {
            toast_error(window, cx, logic::action_failure(action, term).0, "This environment is not connected.");
            return;
        };
        let key = key.to_owned();
        self.pull_requests.card.acting.insert(key.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("pullRequests.runAction", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pull_requests.card.acting.remove(&key);
                match result {
                    Err(error) => {
                        let (title, hint) = logic::action_failure(action, term);
                        let message = crate::pull_requests::error_message(&error);
                        toast_error(window, cx, title, logic::readable_failure(&message, &hint));
                    }
                    Ok(reply) => {
                        toast_success(window, cx, logic::action_success(action, term));
                        this.apply_pr_action_reply(&reference.key(), &reference, &reply, cx);
                        this.reread_card_pull_request(&key, window, cx);
                        // `onActed`: the branch's status follows the pull request.
                        this.refresh_vcs(cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Resolve or Fix: a worktree checkout of the pull request with a thread on it and the task
    /// in its composer (`usePullRequestHandoffs`).
    fn start_card_handoff(&mut self, key: &str, action: CardAction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(read) = self.pull_requests.card.reads.get(key) else { return };
        let Some(detail) = read.shown.clone() else { return };
        let environment_id = read.reference.environment_id.clone();
        if self.pull_requests.card.handoff.is_some() || !self.permits_card_handoff(&environment_id) {
            return;
        }
        let subject = HandoffSubject::of(&detail);
        let task = match action {
            CardAction::Resolve => logic::resolve_conflicts_prompt(&subject).into(),
            _ => logic::fix_findings_handoff(&subject, &Activity::default(), &detail.checks),
        };
        let request = CheckoutRequest {
            environment_id,
            project_id: detail.project_id.clone(),
            url: detail.url.clone(),
            term: logic::terminology(&detail.provider).0,
            task: Some(task),
            mode: CheckoutMode::Worktree,
        };
        self.pull_requests.card.handoff = Some((key.to_owned(), action));
        self.prepare_pr_checkout(request, |this| this.pull_requests.card.handoff = None, window, cx);
    }

    /// `useOpenPrLink`: the Pull request surface for a pull request the environment reads, else the link.
    fn open_card_pull_request(
        &mut self,
        url: &str,
        reference: Option<Reference>,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match reference {
            Some(reference) => self.open_linked_pull_request(url, reference, event, window, cx),
            None => self.open_link(url, event, window, cx),
        }
    }

    /// `onStopWatching`: `thread.pull-request.watch` with `watching: false`.
    fn stop_watching_card_pull_request(
        &mut self,
        thread_id: &str,
        reference: &Reference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.can_operate(&reference.environment_id) {
            self.watch_linked_pull_request(thread_id, reference, false, window, cx);
        }
    }

    fn toggle_card_pull_requests(&mut self, thread_key: &str, cx: &mut Context<Self>) {
        let card = &mut self.pull_requests.card;
        card.expanded = if card.expanded.as_deref() == Some(thread_key) { None } else { Some(thread_key.to_owned()) };
        cx.notify();
    }

    /// The rows under the branch select, and their height for the card's density estimate.
    pub(in crate::app) fn card_pull_request_rows(
        &self,
        colors: &CardColors,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<(AnyElement, f32)> {
        let Subjects { environment_id, thread_key, current, rest, expanded } = self.card_subjects()?;
        let view = cx.entity();
        let mut drawn = vec![self.card_pull_request_row(&current, colors, t, &view, cx)];
        if !rest.is_empty() {
            if expanded {
                for subject in &rest {
                    let row = self.card_pull_request_row(subject, colors, t, &view, cx);
                    drawn.push(row);
                }
            }
            let label = if expanded { "Show less".to_owned() } else { format!("Show {} more", rest.len()) };
            let (hover, hover_text) = (colors.hover, t.foreground.opacity(0.8));
            let toggle = keyboard_button(
                row(colors)
                    .id(ElementId::Name(format!("card-pr-more-{environment_id}").into()))
                    .cursor_pointer()
                    .text_color(t.muted_foreground)
                    .hover(move |style| style.bg(hover).text_color(hover_text)),
                colors,
            )
            // Chevrons, not plus and minus: the plus already means "add" in this card.
            .child(Icon::new(if expanded { Lucide::ChevronUp } else { Lucide::ChevronDown }).with_size(px(16.)))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_card_pull_requests(&thread_key, cx)));
            drawn.push((toggle.into_any_element(), ROW_HEIGHT));
        }
        let height = drawn.iter().map(|(_, height)| height).sum();
        Some((v_flex().w_full().children(drawn.into_iter().map(|(row, _)| row)).into_any_element(), height))
    }

    /// `ThreadDetailsPrRow`: once read, the title with its checks, watch and action under it; a
    /// watched one beside its stop control; else a row opening it.
    fn card_pull_request_row(
        &self,
        subject: &RowSubject,
        colors: &CardColors,
        t: &Tokens,
        view: &Entity<Workspace>,
        cx: &App,
    ) -> (AnyElement, f32) {
        let card = &self.pull_requests.card;
        let read = subject.reference.as_ref().and_then(|r| card.reads.get(&r.key()));
        let detail = read.and_then(|r| r.shown.clone());
        let provider = detail.as_ref().map_or_else(|| provider_of(&subject.url), |d| d.provider.as_str());
        let term = logic::terminology(provider).0;
        let state = detail.as_ref().map(|d| (d.state, d.is_draft)).or(subject.synced);
        let watching = subject.watch.as_ref().filter(|_| state.is_none_or(|(s, _)| s == State::Open));
        let tooltip = card_tooltip(subject, detail.clone(), provider, view);
        let parts = RowParts { subject, colors, t, view, state, tooltip, detail: detail.clone() };

        let Some((detail, reference)) = detail.zip(subject.reference.clone()) else {
            return parts.plain(self, watching);
        };
        let key = reference.key();
        let action = self.card_action_of(&key);
        let rollup = logic::checks_state(&detail.checks);
        let show_checks = !detail.is_draft && !logic::is_conflicting(&detail) && rollup.is_some();
        if !(show_checks || action.is_some() || watching.is_some()) {
            return parts.plain(self, watching);
        }
        let checks = rollup.filter(|_| show_checks).map(|rollup| parts.checks_popover(&key, &detail, rollup, cx));
        let pending = card.acting.contains(&key);
        let busy = pending || card.handoff.is_some();
        let trailing = action.map(|(action, denied)| {
            let preparing = card.handoff.as_ref().is_some_and(|(k, a)| *k == key && *a == action);
            let (label, pending_label, destructive, tip) = match action {
                CardAction::Resolve => (
                    "Resolve",
                    "Preparing...",
                    true,
                    "Check the branch out and resolve the conflicts in a new thread".to_owned(),
                ),
                CardAction::Ready => ("Ready", "Marking...", false, format!("Mark this {term} as ready for review")),
                CardAction::Fix => ("Fix", "Preparing...", true, "Fix the failing checks in a new thread".to_owned()),
                CardAction::Merge => {
                    let method = self.card_merge_method(&reference, &detail);
                    ("Merge", "Merging...", false, format!("Merge this {term} ({method})"))
                }
            };
            let handoff = matches!(action, CardAction::Resolve | CardAction::Fix);
            let shows_pending = if handoff { preparing } else { pending };
            let tip: SharedString = denied.map_or(tip, str::to_owned).into();
            let (fill, text, hover) = if destructive {
                (t.error.opacity(0.1), t.error, t.error.opacity(0.16))
            } else {
                (t.primary, t.primary_foreground, t.primary.opacity(0.85))
            };
            let pill = div()
                .id(parts.id("action"))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .h(px(20.))
                .px(px(6.))
                .rounded(px(6.))
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .bg(fill)
                .text_color(text)
                .child(if shows_pending { pending_label } else { label })
                .when(handoff, |el| el.child(Icon::new(Lucide::ArrowUpRight).with_size(px(12.))))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx));
            if busy || denied.is_some() {
                return pill.opacity(0.64).into_any_element();
            }
            // `focus-visible:ring-1`, drawn in the canvas color so it shows against the fill.
            let ring = t.background;
            let pressed = key.clone();
            pill.cursor_pointer()
                .tab_index(0)
                .focus_visible(move |style| style.border_color(ring))
                .hover(move |style| style.bg(hover))
                .on_click(window_listener(view, move |this, _, window, cx| {
                    this.press_card_action(&pressed, window, cx);
                }))
                .into_any_element()
        });
        let meta = h_flex()
            .mt(px(6.))
            .h(px(20.))
            .min_w_0()
            .gap(px(2.))
            .pl(px(36.))
            .pr(px(10.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(div().mr(px(4.)).flex_none().child(format!("#{}", subject.number)))
            .children(checks)
            .children(watching.map(|watch| parts.watch_control(self, watch, false)))
            .child(div().flex_1())
            .children(trailing);
        let element = v_flex().w_full().pb(px(6.)).child(parts.open_row(detail.title.clone(), true, false)).child(meta);
        // Up to two title lines, the 6px gap, the 20px meta line and the 6px under it.
        (element.into_any_element(), 42. + 6. + 20. + 6.)
    }
}

/// What a row's controls share.
struct RowParts<'a> {
    subject: &'a RowSubject,
    colors: &'a CardColors,
    t: &'a Tokens,
    view: &'a Entity<Workspace>,
    state: Option<(State, bool)>,
    tooltip: TooltipBuilder,
    detail: Option<Rc<Detail>>,
}

impl RowParts<'_> {
    fn id(&self, part: &str) -> ElementId {
        ElementId::Name(format!("card-pr-{}-{part}", self.subject.id).into())
    }

    /// The row's state glyph: the read state, else the synced one, else a plain pull request.
    fn glyph(&self) -> Icon {
        let (icon, color) = match self.state {
            Some((state, draft)) => state_glyph(state, draft, self.t),
            None => (Lucide::GitPullRequest, self.t.muted_foreground),
        };
        Icon::new(icon).with_size(px(16.)).text_color(color)
    }

    /// The button that opens the pull request; `wrap` gives the title two lines, `half` is the
    /// split's primary half.
    fn open_row(&self, text: String, wrap: bool, half: bool) -> gpui_kit::Stateful<gpui_kit::Div> {
        let (url, reference) = (self.subject.url.clone(), self.subject.reference.clone());
        let hover = self.colors.hover;
        let split = self.detail.as_ref().zip(reference.as_ref()).filter(|(d, _)| d.additions + d.deletions > 0).map(
            |(d, reference)| breakdown::Subject {
                environment_id: reference.environment_id.clone(),
                reference: reference.to_json(),
                additions: d.additions,
                deletions: d.deletions,
            },
        );
        let (hover_view, tooltip) = (self.view.clone(), self.tooltip.clone());
        keyboard_button(
            row(self.colors)
                .id(self.id(if half { "primary" } else { "open" }))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .when(half, |el| el.flex_1().min_w_0().rounded_r(px(0.)))
                .when(wrap, |el| el.h_auto().min_h(px(ROW_HEIGHT)).py(px(3.)).items_start()),
            self.colors,
        )
        .child(div().flex_none().when(wrap, |el| el.mt(px(1.))).child(self.glyph()))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .map(|el| if wrap { el.line_clamp(2).line_height(px(18.)) } else { el.truncate() })
                .child(text),
        )
        // The tooltip's split is read on the row's first hover.
        .on_hover(move |hovered, _, cx| {
            if let (true, Some(split)) = (*hovered, split.as_ref()) {
                hover_view.update(cx, |this, cx| this.read_pr_breakdown(split, cx));
            }
        })
        .tooltip(move |window, cx| tooltip(window, cx))
        .on_click(window_listener(self.view, move |this, event, window, cx| {
            this.open_card_pull_request(&url, reference.clone(), event, window, cx);
        }))
    }

    /// The eye that stops the watch, crossed out on hover; `half` is the split's secondary half.
    fn watch_control(&self, workspace: &Workspace, watch: &(String, Reference), half: bool) -> AnyElement {
        let (thread_id, reference) = watch.clone();
        let allowed = workspace.can_operate(&reference.environment_id);
        let group: SharedString = format!("card-pr-{}-watch", self.subject.id).into();
        let (hover, size) = (self.colors.hover, if half { 16. } else { 14. });
        let control = div()
            .id(self.id("watch"))
            .group(group.clone())
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .text_color(self.t.muted_foreground)
            .map(|el| if half { el.w(px(32.)).h_full().rounded_r(px(8.)) } else { el.size(px(24.)).rounded(px(6.)) })
            .child(
                div()
                    .group_hover(group.clone(), gpui_kit::Styled::invisible)
                    .child(Icon::new(Lucide::Eye).with_size(px(size))),
            )
            .child(
                div()
                    .absolute()
                    .invisible()
                    .group_hover(group, gpui_kit::Styled::visible)
                    .child(Icon::new(Lucide::EyeOff).with_size(px(size))),
            )
            .tooltip(move |window, cx| Tooltip::new(if allowed { WATCHING_TIP } else { NO_OPERATE }).build(window, cx));
        if !allowed {
            return control.opacity(0.64).into_any_element();
        }
        keyboard_button(control.cursor_pointer().hover(move |style| style.bg(hover)), self.colors)
            .px_0()
            .on_click(window_listener(self.view, move |this, _, window, cx| {
                this.stop_watching_card_pull_request(&thread_id, &reference, window, cx);
            }))
            .into_any_element()
    }

    /// A row with nothing under its title: a watched one keeps its stop control beside it.
    fn plain(&self, workspace: &Workspace, watching: Option<&(String, Reference)>) -> (AnyElement, f32) {
        let label = self.subject.label.clone();
        let Some(watch) = watching else { return (self.open_row(label, false, false).into_any_element(), ROW_HEIGHT) };
        let group: SharedString = format!("card-pr-{}-split", self.subject.id).into();
        let (hover, line) = (self.colors.hover, self.colors.label.opacity(0.2));
        let split = h_flex()
            .id(ElementId::Name(group.clone()))
            .group(group.clone())
            .w_full()
            .h(px(ROW_HEIGHT))
            .rounded(px(8.))
            .hover(move |style| style.bg(hover))
            .child(self.open_row(label, false, true))
            .child(div().w(px(1.)).h(px(16.)).flex_none().group_hover(group, move |style| style.bg(line)))
            .child(self.watch_control(workspace, watch, true));
        (split.into_any_element(), ROW_HEIGHT)
    }
}

impl RowParts<'_> {
    /// `PullRequestChecksPopover variant="count"`: the checks' state and what is left, opening the list.
    fn checks_popover(&self, key: &str, detail: &Detail, rollup: &'static str, cx: &App) -> AnyElement {
        let (id, colors, t, view) = (&self.subject.id, self.colors, self.t, self.view);
        let Some((icon, color, label)) = checks_glyph(rollup, t) else { return div().into_any_element() };
        let checks = &detail.checks;
        let incomplete = checks.iter().filter(|c| matches!(c.status.as_str(), "pending" | "action-required")).count();
        let failed = checks.iter().filter(|c| matches!(c.status.as_str(), "failure" | "cancelled")).count();
        let count = if incomplete > 0 {
            Some(format!("{}/{} checks", checks.len() - incomplete, checks.len()))
        } else {
            (failed > 0).then(|| format!("{failed} failed"))
        };
        let trigger = Button::new(ElementId::Name(format!("card-pr-{id}-checks").into()))
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(t.background.opacity(0.))
                    .foreground(t.muted_foreground)
                    .hover(colors.hover)
                    .active(colors.hover),
            )
            .h(px(24.))
            .px(px(6.))
            .gap(px(4.))
            .rounded(px(6.))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .tooltip(format!("Open checks: {}", logic::summarize_checks(checks)))
            .child(Icon::new(icon).with_size(px(14.)).text_color(color))
            .children(count);
        let (key, view) = (key.to_owned(), view.clone());
        Popover::new(ElementId::Name(format!("card-pr-{id}-checks-popover").into()))
            .anchor(gpui_kit::Anchor::TopLeft)
            .trigger(trigger)
            .w(px(360.))
            .p(px(12.))
            .content(move |_, _, cx| {
                let t = *theme::tokens(cx);
                let card = &view.read(cx).pull_requests.card;
                let checks = card.reads.get(&key).and_then(|r| r.shown.as_ref()).map(|d| d.checks.clone());
                let show_all = card.checks_all.as_deref() == Some(key.as_str());
                let (view, toggled) = (view.clone(), key.clone());
                v_flex()
                    .child(div().mb(px(8.)).text_sm().font_weight(FontWeight::MEDIUM).child(label))
                    .children(checks.as_deref().map(|checks| {
                        div().mb(px(8.)).text_xs().text_color(t.muted_foreground).child(logic::summarize_checks(checks))
                    }))
                    .child(checks_body(
                        checks.as_deref().unwrap_or_default(),
                        show_all,
                        move |cx| {
                            view.update(cx, |this, cx| {
                                let card = &mut this.pull_requests.card;
                                card.checks_all = if card.checks_all.as_ref() == Some(&toggled) {
                                    None
                                } else {
                                    Some(toggled.clone())
                                };
                                cx.notify();
                            });
                        },
                        &t,
                    ))
            })
            .into_any_element()
    }
}

/// Builds a row's tooltip each time it shows.
type TooltipBuilder = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;

/// A click handler that updates the workspace.
fn window_listener(
    view: &Entity<Workspace>,
    f: impl Fn(&mut Workspace, &ClickEvent, &mut Window, &mut Context<Workspace>) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |event, window, cx| view.update(cx, |this, cx| f(this, event, window, cx))
}

/// The row's tooltip: the pull request's title, state, branches, checks, conflicts and diff once
/// read, else its state line (`prStatusIndicator`'s tooltip) or its number.
fn card_tooltip(
    subject: &RowSubject,
    detail: Option<Rc<Detail>>,
    provider: &str,
    view: &Entity<Workspace>,
) -> TooltipBuilder {
    let (term, short) = logic::terminology(provider);
    let Some(detail) = detail else {
        let text: SharedString = match subject.synced {
            Some((state, draft)) => {
                let title = subject.label.split_once(": ").map(|(_, title)| title.to_owned());
                let lead = format!("{short} #{} - {}", subject.number, state_label(state, draft));
                title.map_or(lead.clone(), |title| format!("{lead}: {title}")).into()
            }
            None => format!("{} #{}", logic::capitalize(term), subject.number).into(),
        };
        return Rc::new(move |window, cx| Tooltip::new(text.clone()).build(window, cx));
    };
    let breakdown_key =
        subject.reference.as_ref().filter(|_| detail.additions + detail.deletions > 0).map(|reference| {
            breakdown::Subject {
                environment_id: reference.environment_id.clone(),
                reference: reference.to_json(),
                additions: detail.additions,
                deletions: detail.deletions,
            }
            .key()
        });
    let view = view.clone();
    Rc::new(move |window, cx| {
        let (detail, breakdown_key, view) = (detail.clone(), breakdown_key.clone(), view.clone());
        Tooltip::element(move |_, cx| {
            let t = *theme::tokens(cx);
            let split = breakdown_key.as_ref().and_then(|key| match view.read(cx).pull_requests.breakdowns.get(key) {
                Some(Breakdown::Ready(Some(split))) => Some(split.clone()),
                _ => None,
            });
            rich_tooltip(&detail, split.as_deref(), &t)
        })
        .build(window, cx)
    })
}

fn rich_tooltip(
    detail: &Detail,
    split: Option<&crate::pull_request_detail::ChangeBreakdown>,
    t: &Tokens,
) -> AnyElement {
    let (state_icon, state_color) = state_glyph(detail.state, detail.is_draft, t);
    let state = state_label(detail.state, detail.is_draft);
    let soft = t.foreground.opacity(0.75);
    let line = |icon: AnyElement, text: AnyElement| {
        h_flex().min_w_0().gap(px(8.)).child(icon).child(div().min_w_0().truncate().text_color(soft).child(text))
    };
    let small = |icon: Lucide, color: Hsla| Icon::new(icon).with_size(px(12.)).text_color(color).into_any_element();
    let checks = logic::classify_checks(&detail.checks);
    let checks_line = (detail.state == State::Open && checks != "none").then(|| {
        let status = match checks {
            "failing" => "failure",
            "pending" => "pending",
            _ => "success",
        };
        let (icon, color) = check_status_icon(status, t);
        line(small(icon, color), logic::describe_checks(&detail.checks).into_any_element())
    });
    let conflict = (detail.is_draft && logic::is_conflicting(detail)).then(|| {
        h_flex()
            .min_w_0()
            .items_start()
            .gap(px(8.))
            .text_color(t.error)
            .child(div().mt(px(2.)).child(small(Lucide::TriangleAlert, t.error)))
            .child(div().flex_1().min_w_0().child(format!("Merge conflicts with {}", detail.base_branch)))
    });
    let addition: Hsla = if t.dark { rgb(0x00d492).into() } else { rgb(0x007a55).into() };
    let files = if detail.changed_files == 1 { "file" } else { "files" };
    let diff = h_flex()
        .items_baseline()
        .gap(px(4.))
        .child(format!("{} {files}", crate::app::git::group_thousands(detail.changed_files)))
        .child(div().font_family(t.fonts.mono).text_color(addition).child(format!("+{}", detail.additions)))
        .child(div().font_family(t.fonts.mono).text_color(t.error).child(format!("-{}", detail.deletions)));
    v_flex()
        .min_w_0()
        .max_w(px(320.))
        .gap(px(8.))
        .px(px(4.))
        .py(px(8.))
        .text_xs()
        .child(
            h_flex()
                .min_w_0()
                .items_baseline()
                .gap(px(6.))
                .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(detail.title.clone()))
                .child(div().flex_none().text_color(t.muted_foreground).child(format!("#{}", detail.number))),
        )
        .child(
            v_flex()
                .gap(px(6.))
                .pl(px(2.))
                .text_color(t.muted_foreground)
                .child(line(small(state_icon, state_color), state.into_any_element()))
                .child(line(
                    small(Lucide::GitBranch, t.muted_foreground),
                    format!("{} ← {}", detail.base_branch, detail.head_branch).into_any_element(),
                ))
                .children(checks_line)
                .children(conflict)
                .child(line(small(Lucide::FileDiff, t.muted_foreground), diff.into_any_element()))
                .children(split.map(|split| div().pl(px(20.)).text_color(soft).child(breakdown::grid(split, None, t)))),
        )
        .into_any_element()
}

/// What a row's read task does next.
enum Step {
    Read(Next, RpcClient, Value),
    Wait(Duration),
}

/// Reads `key`'s pull request while the card shows it: the detail, then its checks and refreshes.
fn card_read_task(key: String, window: &mut Window, cx: &mut Context<Workspace>) -> Task<()> {
    cx.spawn_in(window, async move |this, cx| {
        loop {
            let Ok(Some(step)) = this.update_in(cx, |this, window, _| this.card_read_step(&key, window)) else { break };
            match step {
                Step::Wait(wait) => cx.background_executor().timer(wait).await,
                Step::Read(kind, client, input) => {
                    let method = if kind == Next::Detail { "pullRequests.detail" } else { "pullRequests.checks" };
                    let answer = client.request(method, input).await;
                    if this
                        .update_in(cx, |this, window, cx| this.land_card_read(&key, kind, answer, window, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read() -> CardRead {
        CardRead::new(Reference {
            environment_id: "e".into(),
            project_id: "p".into(),
            host: Some("github.com".into()),
            repository: "o/r".into(),
            number: 7,
        })
    }

    #[test]
    fn a_row_reads_its_detail_then_its_checks_and_refreshes_only_while_open() {
        let start = Instant::now();
        let mut read = read();
        assert_eq!(read.next(true, start), Next::Detail, "the first read is at once");
        let running = json!([{"name": "ci", "status": "pending"}]);
        read.land(Next::Detail, Ok(Value::Null), start);
        assert!(read.shown.is_none(), "an unreadable answer shows nothing");
        assert_eq!(read.next(true, start), Next::Wait(DETAIL_INTERVAL), "and is read again later");

        read.detail_at = None;
        read.land(Next::Detail, Ok(detail_json("open", running.clone())), start);
        assert_eq!(read.next(true, start), Next::Checks, "the checks follow the detail");
        assert_eq!(read.next(false, start), Next::Wait(DETAIL_INTERVAL), "unless the server has no checks read");

        let passed = json!({"state": "open", "checks": [{"name": "ci", "status": "success"}]});
        read.land(Next::Checks, Ok(passed), start);
        assert_eq!(read.shown.as_ref().map(|d| d.checks[0].status.as_str()), Some("success"), "the newer checks show");
        assert_eq!(read.next(true, start), Next::Wait(Duration::from_secs(60)), "settled checks poll every minute");
        assert_eq!(read.next(true, start + Duration::from_secs(60)), Next::Checks);
        assert_eq!(read.next(true, start + DETAIL_INTERVAL), Next::Checks, "the earlier read due goes first");

        // A detail read after the checks keeps them only where they are not older.
        read.land(Next::Detail, Ok(detail_json("open", running)), start + Duration::from_secs(1));
        assert_eq!(read.shown.as_ref().map(|d| d.checks[0].status.as_str()), Some("pending"));

        read.land(Next::Checks, Ok(Value::Null), start + Duration::from_secs(2));
        assert!(read.no_checks);
        assert_eq!(read.next(true, start + Duration::from_secs(2)), Next::Wait(Duration::from_secs(599)));

        let mut merged = self::read();
        merged.land(Next::Detail, Ok(detail_json("merged", json!([]))), start);
        assert_eq!(merged.next(true, start + DETAIL_INTERVAL * 2), Next::Wait(DETAIL_INTERVAL), "a merged one rests");
    }

    fn detail_json(state: &str, checks: Value) -> Value {
        json!({
            "provider": "github", "projectId": "p", "repository": "o/r", "number": 7, "title": "T",
            "url": "https://github.com/o/r/pull/7", "state": state, "headBranch": "h", "baseBranch": "main",
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z", "checks": checks,
        })
    }
}
