//! The sidebar's inbox organization and row labels.
//!
//! Ports `partitionSidebarThreads` (`apps/web/src/components/Sidebar.tsx`),
//! `client-runtime/src/state/{threadInbox,threadSort,threadSettled,models}.ts`,
//! `Sidebar.logic.ts` and `projectIdentity.ts`. One environment only, so the
//! web's environment-id tiebreaks and cross-environment grouping drop out.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};

use crate::model::{ProjectShell, ThreadShell};
use crate::timestamp::parse_ms;

fn ms(value: Option<&str>) -> Option<i64> {
    value.and_then(parse_ms)
}

/// `compactSidebarTimeLabel(formatRelativeTimeLabel(...))`.
pub fn format_relative(elapsed_ms: i64) -> String {
    let minutes = elapsed_ms.max(0) / 60_000;
    match minutes {
        0 => "now".into(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        _ => format!("{}d", minutes / 1440),
    }
}

/// The project the web's index route opens a draft in at launch
/// (`IndexDraftLanding`): `sortScopedProjectsForSidebar(.., "updated_at")[0]`
/// over unarchived projects. A project's time is its newest unarchived
/// thread's latest user message (else updated, else created), or its own
/// updated/created time without threads; ties go by title, then id.
pub fn most_recent_project<'a>(projects: &'a [ProjectShell], threads: &[ThreadShell]) -> Option<&'a ProjectShell> {
    projects_by_recency(projects, threads).into_iter().next()
}

/// Unarchived projects, newest first (`sortScopedProjectsForSidebar(.., "updated_at")`).
pub fn projects_by_recency<'a>(projects: &'a [ProjectShell], threads: &[ThreadShell]) -> Vec<&'a ProjectShell> {
    let thread_time = |t: &ThreadShell| {
        ms(t.latest_user_message_at.as_deref()).or_else(|| parse_ms(&t.updated_at)).or_else(|| parse_ms(&t.created_at))
    };
    let mut latest: HashMap<&str, i64> = HashMap::new();
    for thread in threads.iter().filter(|t| t.archived_at.is_none()) {
        let time = thread_time(thread).unwrap_or(i64::MIN);
        latest.entry(thread.project_id.as_str()).and_modify(|l| *l = (*l).max(time)).or_insert(time);
    }
    let project_time = |p: &ProjectShell| {
        latest
            .get(p.id.as_str())
            .copied()
            .or_else(|| ms(p.updated_at.as_deref().or(p.created_at.as_deref())))
            .unwrap_or(i64::MIN)
    };
    let mut sorted: Vec<&ProjectShell> = projects.iter().filter(|p| p.archived_at.is_none()).collect();
    sorted.sort_by_cached_key(|p| (Reverse(project_time(p)), p.title.clone(), p.id.clone()));
    sorted
}

/// Milliseconds until [`format_relative`] shows a different label.
pub fn relative_changes_in(elapsed_ms: i64) -> i64 {
    let elapsed = elapsed_ms.max(0);
    let unit = match elapsed / 60_000 {
        0..60 => 60_000,
        60..1440 => 3_600_000,
        _ => 86_400_000,
    };
    unit - elapsed % unit
}

/// Milliseconds until [`format_duration`] shows a different label: every
/// second for the first minute, then every minute.
pub fn duration_changes_in(elapsed_ms: i64) -> i64 {
    let elapsed = elapsed_ms.max(0);
    let unit = if elapsed < 60_000 { 1000 } else { 60_000 };
    unit - elapsed % unit
}

/// Milliseconds until [`format_wake`] changes, or the wake itself; `None` once woken.
pub fn wake_changes_in(remaining_ms: i64) -> Option<i64> {
    const HOUR: i64 = 3_600_000;
    let unit = match remaining_ms {
        ..=0 => return None,
        1..HOUR => 60_000,
        HOUR..86_400_000 => HOUR,
        _ => 86_400_000,
    };
    Some(match remaining_ms % unit {
        0 => unit,
        rest => rest,
    })
}

/// `formatWorkingDurationLabel` in `client-runtime/src/state/models.ts`.
pub fn format_duration(elapsed_ms: i64) -> String {
    let seconds = elapsed_ms.max(0) / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// `snoozeWakeLabel`: time left until a snoozed thread wakes.
pub fn format_wake(remaining_ms: i64) -> String {
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;
    let ceil = |unit: i64| (remaining_ms + unit - 1) / unit;
    match remaining_ms {
        ..=0 => "now".into(),
        1..HOUR => format!("{}m", ceil(60_000).max(1)),
        HOUR..DAY => format!("{}h", ceil(HOUR)),
        _ => format!("{}d", ceil(DAY)),
    }
}

fn is_active_run(status: &str) -> bool {
    matches!(status, "preparing" | "queued" | "starting" | "running" | "waiting")
}

fn is_terminal_run(status: &str) -> bool {
    matches!(status, "completed" | "interrupted" | "failed" | "cancelled" | "rolled_back")
}

/// `isChildRunLive`: older servers omit the activity run, so fall back to `status`.
fn is_child_run_live(thread: &ThreadShell) -> bool {
    match &thread.activity_run_status {
        Some(status) => status.is_some(),
        None => matches!(thread.status.as_str(), "preparing" | "starting" | "running" | "waiting"),
    }
}

/// What a subagent or side chat under a thread asks of the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildRequest {
    Approval,
    Input,
}

/// `pendingSubagentRequestsByThread` in `client-runtime/src/state/threadShell.ts`.
pub fn child_requests(threads: &[ThreadShell]) -> HashMap<&str, ChildRequest> {
    let mut requests: HashMap<&str, ChildRequest> = HashMap::new();
    let by_id: HashMap<&str, &ThreadShell> = threads.iter().map(|t| (t.id.as_str(), t)).collect();
    for child in threads {
        let Some(kind) = child.pending_runtime_request.as_ref().map(|r| r.kind.as_str()) else { continue };
        let Some(parent_id) = child.lineage.parent_thread_id.as_deref() else { continue };
        if kind == "auth_refresh" {
            continue;
        }
        if child.lineage.side_chat == Some(true) {
            let request = if kind == "user_input" { ChildRequest::Input } else { ChildRequest::Approval };
            if requests.get(parent_id) != Some(&ChildRequest::Approval) {
                requests.insert(parent_id, request);
            }
            continue;
        }
        // The coordinator answers its subagents' questions; only approvals climb.
        if child.lineage.relationship_to_parent.as_deref() != Some("subagent") || kind == "user_input" {
            continue;
        }
        // `subagentRequest`: provider-native subagents ask on their parent, and a
        // question outlives its run, but the parent stops waiting once the run ends.
        if child.creation_source.as_deref() == Some("provider") || !is_child_run_live(child) {
            continue;
        }
        let mut seen = vec![child.id.as_str()];
        let mut next = Some(parent_id);
        while let Some(id) = next.filter(|id| !seen.contains(id)) {
            seen.push(id);
            requests.insert(id, ChildRequest::Approval);
            next = by_id.get(id).and_then(|parent| {
                let chained = parent.lineage.relationship_to_parent.as_deref() == Some("subagent")
                    || parent.lineage.side_chat == Some(true);
                chained.then_some(parent.lineage.parent_thread_id.as_deref()).flatten()
            });
        }
    }
    requests
}

/// `childThreadInputsAtom`: the subagents and side chats under `parent`, at any depth,
/// that ask the user something (`subagentRequest`, `sideChatRequest`).
pub fn child_thread_inputs<'a>(threads: &'a [ThreadShell], parent: &str) -> Vec<&'a ThreadShell> {
    let is_child = |t: &ThreadShell| {
        t.lineage.relationship_to_parent.as_deref() == Some("subagent") || t.lineage.side_chat == Some(true)
    };
    let asks = |t: &ThreadShell| {
        let Some(kind) = t.pending_runtime_request.as_ref().map(|r| r.kind.as_str()) else { return false };
        kind != "auth_refresh"
            && (t.lineage.side_chat == Some(true)
                || (t.creation_source.as_deref() != Some("provider") && is_child_run_live(t)))
    };
    let mut seen = vec![parent];
    let mut pending = vec![parent];
    let mut inputs = Vec::new();
    while let Some(id) = pending.pop() {
        for child in threads.iter().filter(|t| t.lineage.parent_thread_id.as_deref() == Some(id) && is_child(t)) {
            if seen.contains(&child.id.as_str()) {
                continue;
            }
            seen.push(&child.id);
            pending.insert(0, &child.id);
            if asks(child) {
                inputs.push(child);
            }
        }
    }
    inputs
}

/// The facts the sidebar derives from one shell (`presentThreadShell`).
#[derive(Clone, Copy)]
pub struct Facts<'a> {
    pub thread: &'a ThreadShell,
    pub child_request: Option<ChildRequest>,
}

impl<'a> Facts<'a> {
    pub fn new(thread: &'a ThreadShell, requests: &HashMap<&str, ChildRequest>) -> Self {
        Self { thread, child_request: requests.get(thread.id.as_str()).copied() }
    }

    /// `shellRuntime(...)?.status`; `None` before the thread ever ran.
    pub fn runtime_status(&self) -> Option<&'a str> {
        let t = self.thread;
        let parked = t.status != "failed"
            && t.pending_background_tasks
                .iter()
                .any(|task| task.awaited == Some(true) || matches!(task.kind.as_str(), "subagent" | "background_task"));
        if t.latest_run_id.is_none() && t.active_provider_thread_id.is_none() && !parked {
            return None;
        }
        Some(if parked {
            "idle"
        } else {
            t.activity_run_status.as_ref().and_then(Option::as_deref).unwrap_or(&t.status)
        })
    }

    pub fn latest_run_status(&self) -> Option<&'a str> {
        self.thread.latest_run_id.as_ref()?;
        Some(if self.thread.status == "idle" { "completed" } else { &self.thread.status })
    }

    fn latest_run_requested_at(&self) -> Option<&'a str> {
        self.thread.latest_run_id.as_ref()?;
        self.thread.latest_run_requested_at.as_deref()
    }

    fn latest_run_started_at(&self) -> Option<&'a str> {
        self.thread.latest_run_id.as_ref()?;
        self.thread.latest_run_started_at.as_deref()
    }

    pub fn latest_run_completed_at(&self) -> Option<&'a str> {
        let t = self.thread;
        t.latest_run_id.as_ref()?;
        match &t.latest_run_completed_at {
            Some(value) => value.as_deref(),
            None if t.status == "idle" || is_terminal_run(&t.status) => Some(&t.updated_at),
            None => None,
        }
    }

    fn has_pending_approvals(&self) -> bool {
        self.thread
            .pending_runtime_request
            .as_ref()
            .is_some_and(|r| !matches!(r.kind.as_str(), "user_input" | "auth_refresh"))
    }

    fn has_pending_user_input(&self) -> bool {
        self.thread.pending_runtime_request.as_ref().is_some_and(|r| r.kind == "user_input")
    }

    /// Wants the user's attention, so a [`ReturnTracker`] grace ends at once.
    fn is_actionable(&self) -> bool {
        let t = self.thread;
        self.has_pending_approvals()
            || self.has_pending_user_input()
            || self.child_request.is_some()
            || t.pending_guest_message_count > 0
            || (t.interaction_mode.as_deref() == Some("plan") && t.has_actionable_proposed_plan)
            || (self.runtime_status().is_some() && t.last_error.is_some())
            || self.latest_run_status() == Some("failed")
    }

    /// `isThreadWorking` in `threadInbox.ts`.
    pub fn is_working(&self) -> bool {
        if self.has_pending_approvals() || self.has_pending_user_input() || self.child_request.is_some() {
            return false;
        }
        match self.runtime_status() {
            Some(status) if is_active_run(status) || status == "idle" => {}
            _ => return false,
        }
        let t = self.thread;
        let run_settled =
            self.latest_run_status().is_some_and(|status| !is_active_run(status)) && t.active_run_id != t.latest_run_id;
        !(t.interaction_mode.as_deref() == Some("plan") && t.has_actionable_proposed_plan && run_settled)
    }

    fn failure_edge(&self) -> Option<Option<&'a str>> {
        if !matches!(self.runtime_status(), Some("error" | "failed")) {
            return None;
        }
        if self.latest_run_status() == Some("failed") {
            return Some(self.latest_run_completed_at());
        }
        Some(Some(&self.thread.updated_at))
    }

    /// `threadRaisedHandWhileSnoozed` in `threadSettled.ts`.
    fn raised_hand_while_snoozed(&self) -> bool {
        if self.thread.pending_runtime_request.is_some()
            || self.child_request.is_some()
            || self.thread.pending_guest_message_count > 0
        {
            return true;
        }
        match self.failure_edge() {
            None => false,
            Some(failed_at) => match ms(self.thread.snoozed_at.as_deref()) {
                None => true,
                Some(snoozed_at) => ms(failed_at).is_some_and(|failed| failed > snoozed_at),
            },
        }
    }

    /// `effectiveSnoozed`.
    pub fn is_snoozed(&self, now: i64) -> bool {
        ms(self.thread.snoozed_until.as_deref()).is_some_and(|wake| wake > now) && !self.raised_hand_while_snoozed()
    }

    /// `threadWokeAt`.
    pub fn woke_at(&self, now: i64) -> Option<&'a str> {
        let t = self.thread;
        let wake = ms(t.snoozed_until.as_deref())?;
        if self.raised_hand_while_snoozed() {
            if let Some(Some(failed_at)) = self.failure_edge() {
                let newer = match ms(t.snoozed_at.as_deref()) {
                    None => true,
                    Some(snoozed_at) => ms(Some(failed_at)).is_some_and(|f| f > snoozed_at),
                };
                if newer {
                    return Some(failed_at);
                }
            }
            return if self.runtime_status().is_some() { Some(&t.updated_at) } else { t.snoozed_at.as_deref() };
        }
        (wake <= now).then_some(t.snoozed_until.as_deref()).flatten()
    }

    /// Woke and not visited since, the "Woke" top status.
    pub fn is_woke(&self, now: i64) -> bool {
        let Some(woke_at) = ms(self.woke_at(now)) else { return false };
        let visited_before = match self.thread.last_visited() {
            None => true,
            Some(visited) => parse_ms(visited).is_none_or(|v| v < woke_at),
        };
        visited_before && self.thread.settled_override.as_deref() != Some("settled")
    }

    /// `hasUnseenCompletion`.
    pub fn is_unread(&self) -> bool {
        let Some(completed) = ms(self.latest_run_completed_at()) else { return false };
        let Some(visited) = self.thread.last_visited() else { return false };
        parse_ms(visited).is_none_or(|visited| completed > visited)
    }

    /// `resolveSidebarThreadStatus`.
    pub fn status(&self) -> Status {
        if self.has_pending_approvals() || self.child_request == Some(ChildRequest::Approval) {
            return Status::Approval;
        }
        if self.has_pending_user_input() || self.child_request == Some(ChildRequest::Input) {
            return Status::Input;
        }
        if self.thread.pending_guest_message_count > 0 {
            return Status::Request;
        }
        let runtime = self.runtime_status();
        match runtime {
            Some("running") if self.thread.blocked_on_thread_wait == Some(true) => Status::Waiting,
            Some("preparing") => Status::Preparing,
            Some("queued" | "starting" | "running" | "waiting") => Status::Working,
            Some("idle") => Status::Waiting,
            Some("failed") if self.thread.last_error_class.as_deref() == Some("usage_limit") => Status::Limited,
            Some("failed") => Status::Failed,
            _ => Status::Ready,
        }
    }

    /// `resolveThreadWorkingStartedAt`: when the running duration counts from.
    pub fn working_started_at(&self) -> Option<i64> {
        let t = self.thread;
        self.runtime_status()?;
        if let Some(started) = &t.activity_run_started_at {
            return ms(started.as_deref());
        }
        if self.latest_run_completed_at().is_none() && t.latest_run_id.is_some() && t.latest_run_id == t.active_run_id {
            return ms(self.latest_run_started_at()).or(ms(self.latest_run_requested_at()));
        }
        None
    }

    /// `resolveSettledThreadTimestamp`, in ms.
    pub fn settled_timestamp(&self) -> Option<i64> {
        if let Some(settled) = ms(self.thread.settled_at.as_deref()) {
            return Some(settled);
        }
        [
            self.thread.latest_user_message_at.as_deref(),
            self.latest_run_requested_at(),
            self.latest_run_started_at(),
            self.latest_run_completed_at(),
        ]
        .into_iter()
        .filter_map(ms)
        .max()
        .or_else(|| parse_ms(&self.thread.updated_at))
    }

    /// The compact time a card shows: last user message, else last update.
    pub fn activity_timestamp(&self) -> Option<i64> {
        parse_ms(self.thread.latest_user_message_at.as_deref().unwrap_or(&self.thread.updated_at))
    }

    fn return_timestamp(&self) -> i64 {
        [
            Some(self.thread.created_at.as_str()),
            self.thread.unsettled_at.as_deref(),
            self.latest_run_requested_at(),
            self.latest_run_completed_at(),
        ]
        .into_iter()
        .filter_map(ms)
        .fold(0, i64::max)
    }

    /// `canSnooze` in `threadSettled.ts`: nothing waits on the user and no turn is queued.
    pub fn can_snooze(&self, now: i64) -> bool {
        let t = self.thread;
        if t.pending_runtime_request.is_some() || self.child_request.is_some() || t.pending_guest_message_count > 0 {
            return false;
        }
        !self.has_queued_turn_start(now)
    }

    /// `hasQueuedTurnStart`, bounded by the 2-minute grace on both sides.
    fn has_queued_turn_start(&self, now: i64) -> bool {
        if matches!(self.runtime_status(), Some("preparing" | "queued" | "starting")) {
            return true;
        }
        let Some(message_at) = ms(self.thread.latest_user_message_at.as_deref()) else { return false };
        if self.runtime_status() == Some("error") || (now - message_at).abs() > 2 * 60_000 {
            return false;
        }
        if self.thread.latest_run_id.is_none() {
            return true;
        }
        [self.latest_run_requested_at(), self.latest_run_started_at(), self.latest_run_completed_at()]
            .into_iter()
            .all(|at| ms(at).is_none_or(|at| at < message_at))
    }

    fn send_timestamp(&self) -> i64 {
        let sent = match &self.thread.latest_user_authored_message_at {
            Some(authored) => authored.as_deref(),
            None => self.latest_run_requested_at(),
        };
        ms(Some(&self.thread.created_at)).unwrap_or(0).max(ms(sent).unwrap_or(0))
    }
}

/// `SidebarThreadStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Approval,
    Input,
    Request,
    Preparing,
    Working,
    Waiting,
    Failed,
    Limited,
    Ready,
}

/// `shouldRecedeSidebarThread`.
pub fn recedes(status: Status, unread: bool, woke: bool, active: bool) -> bool {
    match status {
        _ if active => false,
        Status::Input | Status::Request => false,
        Status::Preparing | Status::Working | Status::Waiting => true,
        Status::Ready | Status::Approval => !unread && !woke,
        Status::Failed | Status::Limited => false,
    }
}

/// The dot on a closed boring-sidebar folder (`resolveSidebarFolderDot`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderDot {
    Attention,
    Failed,
    Unread,
    Working,
}

/// The most urgent state among a closed folder's hidden threads. Waiting threads stay quiet,
/// like their rows, and only a ready thread reads as done.
pub fn folder_dot(hidden: &[Facts]) -> Option<FolderDot> {
    let has = |wanted: &[Status]| hidden.iter().any(|f| wanted.contains(&f.status()));
    if has(&[Status::Input, Status::Approval, Status::Request, Status::Limited]) {
        Some(FolderDot::Attention)
    } else if has(&[Status::Failed]) {
        Some(FolderDot::Failed)
    } else if hidden.iter().any(|f| f.status() == Status::Ready && f.is_unread()) {
        Some(FolderDot::Unread)
    } else if has(&[Status::Working, Status::Preparing]) {
        Some(FolderDot::Working)
    } else {
        None
    }
}

/// How long a thread that stopped working stays on the Working shelf: an
/// orchestrator run can end moments before its continuation starts.
const WORKING_EXIT_GRACE_MS: i64 = 2_000;

/// `createInboxReturnTracker` in `threadInbox.ts`: holds a thread that stopped
/// working on the Working shelf for a short grace, and records when it returned
/// to the inbox so the Active list sorts it by that time.
#[derive(Default)]
pub struct ReturnTracker {
    /// Threads that read as working, keyed by id. Only these carry state.
    working: HashMap<String, Hold>,
    returns: HashMap<String, i64>,
}

struct Hold {
    /// Set while the thread has stopped but is still held.
    deadline: Option<i64>,
    placement: u64,
}

/// The fields whose change means the user moved the thread, which ends a grace.
fn placement(t: &ThreadShell) -> u64 {
    let mut hasher = DefaultHasher::new();
    (&t.archived_at, &t.deleted_at, &t.pinned_at, &t.settled_override, &t.settled_at).hash(&mut hasher);
    (&t.snoozed_at, &t.snoozed_until, &t.active_order_key).hash(&mut hasher);
    hasher.finish()
}

impl ReturnTracker {
    /// Updates the holds for the current shells at `now`.
    pub fn observe(&mut self, threads: &[ThreadShell], requests: &HashMap<&str, ChildRequest>, now: i64) {
        let (mut working, mut returns) = (0, 0);
        for thread in threads {
            self.observe_one(Facts::new(thread, requests), now);
            working += usize::from(self.working.contains_key(thread.id.as_str()));
            returns += usize::from(self.returns.contains_key(thread.id.as_str()));
        }
        // Forget threads that left the shell.
        if working < self.working.len() || returns < self.returns.len() {
            let present: HashSet<&str> = threads.iter().map(|t| t.id.as_str()).collect();
            self.working.retain(|id, _| present.contains(id.as_str()));
            self.returns.retain(|id, _| present.contains(id.as_str()));
        }
    }

    fn observe_one(&mut self, facts: Facts, now: i64) {
        let thread = facts.thread;
        let actual = facts.is_working();
        let Some(hold) = self.working.get_mut(thread.id.as_str()) else {
            if actual {
                self.working.insert(thread.id.clone(), Hold { deadline: None, placement: placement(thread) });
            }
            return;
        };
        let placement = placement(thread);
        let moved = std::mem::replace(&mut hold.placement, placement) != placement;
        if actual {
            hold.deadline = None;
            return;
        }
        // Only shelf placement is held: execution state and attention requests stay authoritative.
        let deadline = hold.deadline.unwrap_or(now + WORKING_EXIT_GRACE_MS);
        let excluded = thread.archived_at.is_some()
            || thread.deleted_at.is_some()
            || thread.pinned_at.is_some()
            || thread.settled_override.as_deref() == Some("settled")
            || facts.is_snoozed(now);
        if deadline > now && !moved && !facts.is_actionable() && !excluded {
            hold.deadline = Some(deadline);
            return;
        }
        // A grace that ran out returned the thread at its deadline, as the web's timer does.
        let returned = hold.deadline.filter(|d| *d <= now).unwrap_or(now);
        self.working.remove(thread.id.as_str());
        self.returns.insert(thread.id.clone(), returned);
    }

    /// Working, or held on the Working shelf, as of the last [`Self::observe`].
    pub fn is_working(&self, thread_id: &str) -> bool {
        self.working.contains_key(thread_id)
    }

    pub fn returned_at(&self, thread_id: &str) -> Option<i64> {
        self.returns.get(thread_id).copied()
    }

    /// When the next hold expires; the caller observes again then.
    pub fn next_deadline(&self) -> Option<i64> {
        self.working.values().filter_map(|hold| hold.deadline).min()
    }
}

#[derive(Default)]
pub struct Sections<'a> {
    pub pinned: Vec<Facts<'a>>,
    pub active: Vec<Facts<'a>>,
    pub working: Vec<Facts<'a>>,
    pub snoozed: Vec<Facts<'a>>,
    pub settled: Vec<Facts<'a>>,
}

/// `partitionSidebarThreads`. `working_shelf` is false while the environment is disconnected
/// or the shelf is off; `sort_active_by_send` sorts Active as the Working shelf does, for the
/// boring sidebar's folders.
pub fn partition<'a>(
    threads: &'a [ThreadShell],
    now: i64,
    working_shelf: bool,
    sort_active_by_send: bool,
    returns: &mut ReturnTracker,
    include: impl Fn(&ThreadShell) -> bool,
) -> Sections<'a> {
    let requests = child_requests(threads);
    returns.observe(threads, &requests, now);
    let mut sections = Sections::default();
    let visible =
        threads.iter().filter(|t| t.is_listed() && t.archived_at.is_none() && t.deleted_at.is_none() && include(t));
    for thread in visible {
        let facts = Facts::new(thread, &requests);
        let list = if facts.is_snoozed(now) {
            &mut sections.snoozed
        } else if thread.settled_override.as_deref() == Some("settled") {
            &mut sections.settled
        } else if thread.pinned_at.is_some() {
            &mut sections.pinned
        } else if working_shelf && returns.is_working(&thread.id) {
            &mut sections.working
        } else {
            &mut sections.active
        };
        list.push(facts);
    }
    let id = |f: &Facts<'a>| f.thread.id.as_str();
    // `sortPinnedThreadsByOrderKey`: keyed rows by key, then keyless newest first.
    sections.pinned.sort_by_cached_key(|f| {
        let key = f.thread.pin_order_key.as_deref();
        let created = if key.is_none() { ms(Some(&f.thread.created_at)).unwrap_or(0) } else { 0 };
        (key.is_none(), key, Reverse(created), id(f))
    });
    if sort_active_by_send {
        sections.active.sort_by_cached_key(|f| (Reverse(f.send_timestamp()), id(f)));
    } else {
        sections.active.sort_by_cached_key(|f| {
            let returned = returns.returned_at(id(f)).unwrap_or(0);
            (Reverse(f.return_timestamp().max(returned)), id(f))
        });
    }
    sections.working.sort_by_cached_key(|f| (Reverse(f.send_timestamp()), id(f)));
    sections.snoozed.sort_by_cached_key(|f| ms(f.thread.snoozed_until.as_deref()).unwrap_or(i64::MAX));
    sections.settled.sort_by_cached_key(|f| (Reverse(f.settled_timestamp().unwrap_or(0)), id(f)));
    sections
}

fn words(title: &str) -> Vec<Vec<char>> {
    let mut words = Vec::new();
    let mut current = Vec::new();
    for c in title.trim().chars() {
        if c.is_alphanumeric() {
            current.push(c);
        } else if !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// `projectMonogram` in `apps/web/src/projectIdentity.ts`.
pub fn monogram(title: &str) -> String {
    let words = words(title);
    let Some(first_word) = words.first() else { return "PR".into() };
    let first = first_word[0];
    let second = first_word[1..]
        .iter()
        .find(|c| c.is_numeric())
        .or_else(|| if words.len() > 1 { words.last()?.first() } else { first_word.last() })
        .copied()
        .unwrap_or(first);
    format!("{first}{second}").to_uppercase().chars().take(2).collect()
}

/// `projectColor`: an index into [`PROJECT_COLORS`].
pub fn color_index(title: &str) -> usize {
    let seed = title.trim().to_lowercase();
    let seed = if seed.is_empty() { "project".to_owned() } else { seed };
    seed.chars().fold(0, |index, c| (index * 31 + c as usize) % PROJECT_COLORS.len())
}

/// `projectIconColors.ts`, as (name, light -600, dark -400).
pub const PROJECT_COLORS: [(&str, u32, u32); 18] = [
    ("gray", 0x4a5565, 0x99a1af),
    ("red", 0xe7000b, 0xff6467),
    ("orange", 0xf54900, 0xff8904),
    ("amber", 0xe17100, 0xffb900),
    ("yellow", 0xd08700, 0xfdc700),
    ("lime", 0x5ea500, 0x9ae600),
    ("green", 0x00a63e, 0x05df72),
    ("emerald", 0x009966, 0x00d492),
    ("teal", 0x009689, 0x00d5be),
    ("cyan", 0x0092b8, 0x00d3f2),
    ("sky", 0x0084d1, 0x00bcff),
    ("blue", 0x155dfc, 0x51a2ff),
    ("indigo", 0x4f39f6, 0x7c86ff),
    ("violet", 0x7f22fe, 0xa684ff),
    ("purple", 0x9810fa, 0xc27aff),
    ("fuchsia", 0xc800de, 0xed6aff),
    ("pink", 0xe60076, 0xfb64b6),
    ("rose", 0xec003f, 0xff637e),
];

/// `isProviderUpdateCandidate` + `canOneClickUpdateProviderCandidate`: enabled,
/// behind its latest version, not known broken, one-click updatable, not updating.
pub fn is_provider_update_candidate(provider: &crate::model::Provider) -> bool {
    let advisory = provider.version_advisory.as_ref();
    let field = |key: &str| advisory.map(|a| &a[key]);
    let latest_status = provider.compatibility_advisory.as_ref().and_then(|c| c["latestVersionStatus"].as_str());
    let updating = provider.update_state.as_ref().and_then(|u| u["status"].as_str());
    provider.enabled
        && !matches!(latest_status, Some("broken" | "unsupported"))
        && field("status").and_then(|s| s.as_str()) == Some("behind_latest")
        && field("latestVersion").is_some_and(serde_json::Value::is_string)
        && field("canUpdate").and_then(serde_json::Value::as_bool) == Some(true)
        && !matches!(updating, Some("queued" | "running"))
}

/// One `snoozePresets` entry (`packages/contracts/src/settings.ts`).
#[derive(Clone, Debug, PartialEq)]
pub enum SnoozeSetting {
    After {
        minutes: i64,
    },
    /// `day` is `today`, `tomorrow`, or a lowercase weekday name.
    At {
        day: String,
        hour: u32,
        minute: u32,
    },
}

impl SnoozeSetting {
    fn id(&self) -> String {
        match self {
            SnoozeSetting::After { minutes } => format!("after:{minutes}"),
            SnoozeSetting::At { day, hour, minute } => format!("at:{day}:{hour:02}:{minute:02}"),
        }
    }
}

/// `MAX_SNOOZE_PRESET_MINUTES`: a duration preset is 1 minute to 30 days.
pub const MAX_SNOOZE_PRESET_MINUTES: i64 = 30 * 24 * 60;

/// The server's `snoozePresets`, else `DEFAULT_SNOOZE_PRESETS`. Entries the
/// contract rejects are skipped, keeping the rest.
pub fn snooze_settings(value: &serde_json::Value) -> Vec<SnoozeSetting> {
    let Some(entries) = value.as_array() else {
        let at = |day: &str, hour| SnoozeSetting::At { day: day.into(), hour, minute: 0 };
        return vec![
            SnoozeSetting::After { minutes: 60 },
            SnoozeSetting::After { minutes: 180 },
            at("today", 18),
            at("tomorrow", 9),
            at("monday", 9),
        ];
    };
    entries
        .iter()
        .filter_map(|entry| match entry["kind"].as_str()? {
            "after" => entry["minutes"]
                .as_i64()
                .filter(|minutes| (1..=MAX_SNOOZE_PRESET_MINUTES).contains(minutes))
                .map(|minutes| SnoozeSetting::After { minutes }),
            "at" => {
                let (hour, minute) = entry["time"].as_str()?.split_once(':')?;
                let day = entry["day"].as_str()?.to_owned();
                Some(SnoozeSetting::At { day, hour: hour.parse().ok()?, minute: minute.parse().ok()? })
            }
            _ => None,
        })
        .collect()
}

/// A snooze menu row (`SnoozePreset`).
#[derive(Clone, Debug, PartialEq)]
pub struct SnoozePreset {
    pub id: String,
    pub label: String,
    pub when_label: String,
    pub wake_ms: i64,
}

const WEEKDAYS: [&str; 7] = ["sunday", "monday", "tuesday", "wednesday", "thursday", "friday", "saturday"];

fn plural(count: i64, unit: &str) -> String {
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// "3:05 PM", the en-US `formatShortTimestamp`.
pub fn clock_label<Tz: chrono::TimeZone>(at: &chrono::DateTime<Tz>) -> String {
    use chrono::Timelike as _;
    let suffix = if at.hour() < 12 { "AM" } else { "PM" };
    format!("{}:{:02} {suffix}", (at.hour() + 11) % 12 + 1, at.minute())
}

/// `resolveSnoozePreset` in the zone of `now`; `None` once a set time has passed.
pub fn resolve_snooze<Tz: chrono::TimeZone>(
    setting: &SnoozeSetting,
    now: &chrono::DateTime<Tz>,
) -> Option<SnoozePreset> {
    use chrono::Datelike as _;
    let now_ms = now.timestamp_millis();
    let (wake, label, names_day) = match setting {
        SnoozeSetting::After { minutes } => {
            let duration = if minutes % 1440 == 0 {
                plural(minutes / 1440, "day")
            } else if minutes % 60 == 0 {
                plural(minutes / 60, "hour")
            } else {
                plural(*minutes, "minute")
            };
            let wake = now.clone().checked_add_signed(chrono::TimeDelta::try_minutes(*minutes)?)?;
            (wake, format!("In {duration}"), false)
        }
        SnoozeSetting::At { day, hour, minute } => {
            let today = i64::from(now.weekday().num_days_from_sunday());
            let days_ahead = match day.as_str() {
                "today" => 0,
                "tomorrow" => 1,
                weekday => match (WEEKDAYS.iter().position(|d| *d == weekday)? as i64 - today + 7) % 7 {
                    0 => 7,
                    n => n,
                },
            };
            // A calendar-day advance, so DST days keep their local time.
            let date = now.date_naive().checked_add_days(chrono::Days::new(days_ahead as u64))?;
            let local = date.and_hms_opt(*hour, *minute, 0)?;
            let zone = now.timezone();
            let wake = zone
                .from_local_datetime(&local)
                .earliest()
                .or_else(|| zone.from_local_datetime(&(local + chrono::Duration::hours(1))).earliest())?;
            if wake.timestamp_millis() <= now_ms {
                return None;
            }
            let label = match days_ahead {
                0 if *hour < 12 => "This morning".to_owned(),
                0 if *hour < 17 => "This afternoon".to_owned(),
                0 => "This evening".to_owned(),
                1 => "Tomorrow".to_owned(),
                _ if day == "monday" => "Next week".to_owned(),
                7 => format!("Next {}", capitalize(day)),
                _ => capitalize(day),
            };
            let names_day = label != "Next week";
            (wake, label, names_day)
        }
    };
    let time = clock_label(&wake);
    let when_label = if names_day || wake.date_naive() == now.date_naive() {
        time
    } else {
        format!("{} {time}", capitalize(&WEEKDAYS[wake.weekday().num_days_from_sunday() as usize][..3]))
    };
    Some(SnoozePreset { id: setting.id(), label, when_label, wake_ms: wake.timestamp_millis() })
}

/// `resolveSnoozePresets`: soonest first, one row per wake minute.
pub fn snooze_presets<Tz: chrono::TimeZone>(
    settings: &[SnoozeSetting],
    now: &chrono::DateTime<Tz>,
) -> Vec<SnoozePreset> {
    let mut presets: Vec<SnoozePreset> = settings.iter().filter_map(|s| resolve_snooze(s, now)).collect();
    presets.sort_by_key(|p| p.wake_ms.div_euclid(60_000));
    presets.dedup_by_key(|p| p.wake_ms.div_euclid(60_000));
    presets
}

/// `pickSnoozePreset`: a duration counts from the pick; a set time keeps the
/// shown wake while it is still ahead.
pub fn pick_snooze<Tz: chrono::TimeZone>(
    shown: &[SnoozePreset],
    id: &str,
    settings: &[SnoozeSetting],
    now: &chrono::DateTime<Tz>,
) -> Option<i64> {
    let after = settings.iter().find(|s| matches!(s, SnoozeSetting::After { .. }) && s.id() == id);
    if let Some(preset) = after.and_then(|s| resolve_snooze(s, now)) {
        return Some(preset.wake_ms);
    }
    shown.iter().find(|p| p.id == id).map(|p| p.wake_ms).filter(|wake| *wake > now.timestamp_millis())
}

const PIN_DIGITS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";

fn pin_midpoint(a: &[u8], b: &[u8]) -> Vec<u8> {
    if !b.is_empty() {
        let n = (0..b.len()).take_while(|&n| a.get(n).copied().unwrap_or(b'a') == b[n]).count();
        if n > 0 {
            let mut key = b[..n].to_vec();
            key.extend(pin_midpoint(a.get(n..).unwrap_or_default(), &b[n..]));
            return key;
        }
    }
    let digit_a = a.first().map_or(0, |c| (c - b'a') as usize);
    let digit_b = b.first().map_or(PIN_DIGITS.len(), |c| (c - b'a') as usize);
    if digit_b - digit_a > 1 {
        return vec![PIN_DIGITS[((digit_a + digit_b) as f32 / 2.).round() as usize]];
    }
    if b.len() > 1 {
        return vec![b[0]];
    }
    let mut key = vec![PIN_DIGITS[digit_a]];
    key.extend(pin_midpoint(a.get(1..).unwrap_or_default(), b""));
    key
}

/// `pinOrderKeyBetween`: a key sorting strictly between two neighbors, `None`
/// for corrupt or out-of-order keys (the server then picks one).
pub fn pin_key_between(before: Option<&str>, after: Option<&str>) -> Option<String> {
    let valid = |key: &str| key.bytes().all(|c| c.is_ascii_lowercase()) && !key.is_empty() && !key.ends_with('a');
    let (a, b) = (before.unwrap_or_default(), after.unwrap_or_default());
    if (!a.is_empty() && !valid(a)) || (!b.is_empty() && !valid(b)) || (!b.is_empty() && a >= b) {
        return None;
    }
    String::from_utf8(pin_midpoint(a.as_bytes(), b.as_bytes())).ok()
}

/// The key that pins a thread above every other pin (`pinThread`).
pub fn top_pin_key(threads: &[ThreadShell]) -> Option<String> {
    let lowest = threads.iter().filter(|t| t.pinned_at.is_some()).filter_map(|t| t.pin_order_key.as_deref()).min();
    pin_key_between(None, lowest)
}

/// The custom snooze dialog's unit (`CustomSnoozeDialog.tsx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnoozeUnit {
    Minutes,
    Hours,
    Days,
}

impl SnoozeUnit {
    pub fn label(self) -> &'static str {
        match self {
            SnoozeUnit::Minutes => "Minutes",
            SnoozeUnit::Hours => "Hours",
            SnoozeUnit::Days => "Days",
        }
    }
}

pub enum CustomSnooze {
    Date { date: String, time: String },
    Duration { amount: String, unit: SnoozeUnit },
}

/// `resolveCustomSnooze`: the wake time in ms, or the dialog's error text.
pub fn resolve_custom_snooze<Tz: chrono::TimeZone>(
    input: &CustomSnooze,
    now: &chrono::DateTime<Tz>,
) -> Result<i64, &'static str> {
    let now_ms = now.timestamp_millis();
    match input {
        CustomSnooze::Duration { amount, unit } => {
            const ERROR: &str = "Enter a positive duration.";
            let amount: f64 = amount.trim().parse().map_err(|_| ERROR)?;
            let unit_ms = match unit {
                SnoozeUnit::Minutes => 60_000.,
                SnoozeUnit::Hours => 3_600_000.,
                SnoozeUnit::Days => 86_400_000.,
            };
            if !amount.is_finite() || amount <= 0. {
                return Err(ERROR);
            }
            // `as` saturates a huge amount; anything past chrono's range is no date at all.
            let wake = now_ms.checked_add((amount * unit_ms).round() as i64).ok_or(ERROR)?;
            chrono::DateTime::from_timestamp_millis(wake).ok_or(ERROR)?;
            Ok(wake)
        }
        CustomSnooze::Date { date, time } => {
            const ERROR: &str = "Choose a valid date and time in the future.";
            let date = chrono::NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").map_err(|_| ERROR)?;
            let time = chrono::NaiveTime::parse_from_str(time.trim(), "%H:%M").map_err(|_| ERROR)?;
            // A nonexistent local time (a DST gap) has no single reading: refuse it.
            let wake = now.timezone().from_local_datetime(&date.and_time(time)).single().ok_or(ERROR)?;
            let wake = wake.timestamp_millis();
            if wake <= now_ms { Err(ERROR) } else { Ok(wake) }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn labels_report_when_they_next_change() {
        assert_eq!(relative_changes_in(0), 60_000);
        assert_eq!(relative_changes_in(61_000), 59_000);
        assert_eq!(relative_changes_in(2 * 3_600_000 + 5), 3_600_000 - 5);
        assert_eq!(duration_changes_in(1_500), 500);
        assert_eq!(duration_changes_in(90_000), 30_000);
        assert_eq!(wake_changes_in(0), None);
        assert_eq!(wake_changes_in(90_000), Some(30_000));
        assert_eq!(wake_changes_in(120_000), Some(60_000));
    }

    use super::*;

    #[test]
    fn launch_opens_a_draft_in_the_most_recently_active_project() {
        let project = |id: &str, title: &str, updated: Option<&str>, archived: bool| -> ProjectShell {
            serde_json::from_value(serde_json::json!({
                "id": id, "title": title, "workspaceRoot": id, "updatedAt": updated,
                "archivedAt": if archived { Some("2026-01-01T00:00:00.000Z") } else { None },
            }))
            .unwrap()
        };
        let thread = |project: &str, at: &str| -> ThreadShell {
            serde_json::from_value(serde_json::json!({
                "id": format!("t-{project}-{at}"), "projectId": project, "title": "t", "status": "idle",
                "updatedAt": "2026-01-01T00:00:00.000Z", "createdAt": "2026-01-01T00:00:00.000Z", "latestUserMessageAt": at,
            }))
            .unwrap()
        };
        let projects = [
            project("a", "Alpha", Some("2026-11-04T09:00:00.000Z"), false),
            project("b", "Beta", None, false),
            project("z", "Zulu", Some("2026-12-01T00:00:00.000Z"), true),
        ];
        // Thread activity wins over a project's own time; archived projects never land.
        let threads = [thread("b", "2026-11-04T10:00:00.000Z")];
        assert_eq!(most_recent_project(&projects, &threads).map(|p| p.id.as_str()), Some("b"));
        assert_eq!(most_recent_project(&projects, &[]).map(|p| p.id.as_str()), Some("a"));
        // Equal times fall back to the title.
        let tied = [project("y", "Yak", Some("2026-11-04T09:00:00.000Z"), false), projects[0].clone()];
        assert_eq!(most_recent_project(&tied, &[]).map(|p| p.id.as_str()), Some("a"));
    }
    use serde::Deserialize as _;
    use serde_json::{Value, json};

    fn thread(id: &str, extra: Value) -> ThreadShell {
        let mut value = json!({
            "id": id, "projectId": "p", "title": id, "status": "idle",
            "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z",
            "lineage": {"rootThreadId": id},
        });
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        ThreadShell::deserialize(&value).unwrap()
    }

    fn ids(list: &[Facts]) -> Vec<String> {
        list.iter().map(|f| f.thread.id.clone()).collect()
    }

    const NOW: &str = "2026-11-04T12:00:00.000Z";

    fn now() -> i64 {
        parse_ms(NOW).unwrap()
    }

    #[test]
    fn formats_relative_times_and_durations() {
        let cases = [
            (0, "now"),
            (59_999, "now"),
            (60_000, "1m"),
            (3_599_999, "59m"),
            (3_600_000, "1h"),
            (86_399_999, "23h"),
            (86_400_000, "1d"),
            (40 * 86_400_000, "40d"),
        ];
        for (elapsed, label) in cases {
            assert_eq!(format_relative(elapsed), label, "{elapsed}");
        }
        let cases = [
            (-5, "0s"),
            (999, "0s"),
            (35_000, "35s"),
            (60_000, "1m"),
            (35 * 60_000, "35m"),
            (3_600_000, "1h 0m"),
            (3_725_000, "1h 2m"),
        ];
        for (elapsed, label) in cases {
            assert_eq!(format_duration(elapsed), label, "{elapsed}");
        }
        let cases = [
            (0, "now"),
            (1, "1m"),
            (60_001, "2m"),
            (3_600_000, "1h"),
            (3_600_001, "2h"),
            (86_400_000, "1d"),
            (86_400_001, "2d"),
        ];
        for (remaining, label) in cases {
            assert_eq!(format_wake(remaining), label, "{remaining}");
        }
    }

    #[test]
    fn monograms_and_colors_match_the_web() {
        assert_eq!(monogram("japan-trip-december-2026"), "J2");
        assert_eq!(monogram("examples"), "ES");
        assert_eq!(monogram(".claude"), "CE");
        assert_eq!(monogram("tr3x"), "T3");
        assert_eq!(monogram("t3code"), "T3");
        assert_eq!(monogram("My Project"), "MP");
        assert_eq!(monogram("x"), "XX");
        assert_eq!(monogram("--"), "PR");
        assert_eq!(PROJECT_COLORS[color_index("japan-trip-december-2026")].0, "blue");
        assert_eq!(PROJECT_COLORS[color_index("examples")].0, "blue");
        assert_eq!(PROJECT_COLORS[color_index(".claude")].0, "purple");
    }

    #[test]
    fn sections_follow_snoozed_settled_pinned_working_active_precedence() {
        let threads = vec![
            thread("snoozed", json!({"snoozedUntil": "2026-11-05T00:00:00.000Z", "settledOverride": "settled"})),
            thread(
                "woke-early",
                json!({"snoozedUntil": "2026-11-05T00:00:00.000Z", "pendingRuntimeRequest": {"kind": "command"}}),
            ),
            thread("snooze-over", json!({"snoozedUntil": "2026-11-03T00:00:00.000Z"})),
            thread("settled", json!({"settledOverride": "settled", "pinnedAt": NOW})),
            thread("pinned", json!({"pinnedAt": NOW, "latestRunId": "r", "status": "running"})),
            thread("working", json!({"latestRunId": "r", "activeRunId": "r", "status": "running"})),
            thread(
                "asking",
                json!({"latestRunId": "r", "status": "running", "pendingRuntimeRequest": {"kind": "user_input"}}),
            ),
            thread(
                "sub",
                json!({"lineage": {"rootThreadId": "working", "parentThreadId": "working", "relationshipToParent": "subagent"}}),
            ),
            thread("archived", json!({"archivedAt": NOW})),
        ];
        let sections = partition(&threads, now(), true, false, &mut ReturnTracker::default(), |_| true);
        assert_eq!(ids(&sections.snoozed), ["snoozed"]);
        assert_eq!(ids(&sections.settled), ["settled"]);
        assert_eq!(ids(&sections.pinned), ["pinned"]);
        assert_eq!(ids(&sections.working), ["working"]);
        let mut active = ids(&sections.active);
        active.sort();
        assert_eq!(active, ["asking", "snooze-over", "woke-early"]);

        let offline = partition(&threads, now(), false, false, &mut ReturnTracker::default(), |_| true);
        assert!(offline.working.is_empty(), "a disconnected environment has no Working shelf");
    }

    #[test]
    fn subagent_approvals_pull_the_parent_out_of_working() {
        let child = |extra: Value| {
            let mut value = json!({"pendingRuntimeRequest": {"kind": "command"}, "lineage": {"rootThreadId": "parent", "parentThreadId": "parent", "relationshipToParent": "subagent"}});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            thread("child", value)
        };
        let parent = thread("parent", json!({"latestRunId": "r", "status": "running"}));
        let flagged = |child: ThreadShell| child_requests(&[parent.clone(), child]).get("parent").copied();
        // Older servers omit the activity run, so the run status decides.
        assert_eq!(flagged(child(json!({"status": "running"}))), Some(ChildRequest::Approval));
        assert_eq!(flagged(child(json!({"status": "idle"}))), None);
        // An explicit null activity run means the child's run ended.
        assert_eq!(flagged(child(json!({"status": "running", "activityRunStatus": null}))), None);
        assert_eq!(
            flagged(child(json!({"status": "idle", "activityRunStatus": "waiting"}))),
            Some(ChildRequest::Approval)
        );
        // Provider-native subagents ask on their parent.
        assert_eq!(flagged(child(json!({"status": "running", "creationSource": "provider"}))), None);

        let threads = vec![parent.clone(), child(json!({"status": "running"}))];
        let sections = partition(&threads, now(), true, false, &mut ReturnTracker::default(), |_| true);
        assert_eq!(ids(&sections.active), ["parent"]);
        assert_eq!(sections.active[0].status(), Status::Approval);
    }

    fn running(id: &str, working: bool, extra: Value) -> ThreadShell {
        let mut value = if working { json!({"latestRunId": "r", "status": "running"}) } else { json!({}) };
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        thread(id, value)
    }

    /// Observes `threads` at `at` ms past the epoch, as `threadInbox.test.ts` does from 10s.
    fn observe(tracker: &mut ReturnTracker, threads: &[ThreadShell], at: i64) {
        tracker.observe(threads, &child_requests(threads), at);
    }

    #[test]
    fn a_thread_that_stops_working_is_held_then_stamped_at_its_deadline() {
        let mut tracker = ReturnTracker::default();
        observe(&mut tracker, &[running("a", true, json!({})), running("b", false, json!({}))], 10_000);
        assert_eq!(tracker.returned_at("a"), None, "never stamped on the first observation");
        observe(&mut tracker, &[running("a", false, json!({})), running("b", false, json!({}))], 10_000);
        assert!(tracker.is_working("a") && !tracker.is_working("b"));
        assert_eq!(tracker.next_deadline(), Some(12_000));
        observe(&mut tracker, &[running("a", false, json!({}))], 11_999);
        assert!(tracker.is_working("a"));
        // The clock wakes a little late; the return still reads as the deadline.
        observe(&mut tracker, &[running("a", false, json!({}))], 12_005);
        assert!(!tracker.is_working("a"));
        assert_eq!(tracker.returned_at("a"), Some(12_000));
        assert_eq!(tracker.returned_at("b"), None);
        assert_eq!(tracker.next_deadline(), None);
    }

    #[test]
    fn resuming_cancels_a_hold_and_the_next_finish_gets_a_fresh_grace() {
        let mut tracker = ReturnTracker::default();
        let (idle, busy) = (running("a", false, json!({})), running("a", true, json!({})));
        observe(&mut tracker, std::slice::from_ref(&busy), 10_000);
        observe(&mut tracker, std::slice::from_ref(&idle), 10_000);
        observe(&mut tracker, std::slice::from_ref(&busy), 11_500);
        observe(&mut tracker, &[busy], 12_500);
        assert_eq!(tracker.returned_at("a"), None);
        observe(&mut tracker, std::slice::from_ref(&idle), 12_500);
        observe(&mut tracker, &[idle], 14_500);
        assert_eq!(tracker.returned_at("a"), Some(14_500));
    }

    #[test]
    fn attention_or_manual_placement_ends_a_hold_at_once() {
        let changes = [
            json!({"pendingRuntimeRequest": {"kind": "command"}}),
            json!({"pendingRuntimeRequest": {"kind": "user_input"}}),
            json!({"pendingGuestMessageCount": 1}),
            json!({"interactionMode": "plan", "hasActionableProposedPlan": true}),
            json!({"latestRunId": "r", "status": "failed", "lastError": "failed"}),
            json!({"pinnedAt": "2026-06-01T00:00:01Z"}),
            json!({"settledOverride": "settled"}),
            json!({"snoozedUntil": "2099-06-02T00:00:00Z"}),
            json!({"activeOrderKey": "manual-order"}),
        ];
        for change in changes {
            let mut tracker = ReturnTracker::default();
            observe(&mut tracker, &[running("a", true, json!({}))], 10_000);
            observe(&mut tracker, &[running("a", false, json!({}))], 10_000);
            observe(&mut tracker, &[running("a", false, change.clone())], 10_000);
            assert!(!tracker.is_working("a"), "{change}");
            assert_eq!(tracker.returned_at("a"), Some(10_000), "{change}");
            assert_eq!(tracker.next_deadline(), None, "{change}");
        }
    }

    #[test]
    fn stable_returns_sort_the_active_list_and_removed_threads_are_forgotten() {
        let mut tracker = ReturnTracker::default();
        let created = json!({"createdAt": "1970-01-01T00:00:00Z"});
        let (a, b) = (running("a", false, created.clone()), running("b", false, created));
        observe(&mut tracker, &[running("a", true, json!({})), running("b", true, json!({}))], 10_000);
        observe(&mut tracker, &[a.clone(), running("b", true, json!({}))], 10_000);
        observe(&mut tracker, &[a.clone(), b.clone()], 10_500);
        observe(&mut tracker, &[a.clone(), b.clone()], 12_000);
        assert_eq!((tracker.returned_at("a"), tracker.returned_at("b")), (Some(12_000), None));
        let threads = [a, b];
        let sections = partition(&threads, 12_500, true, false, &mut tracker, |_| true);
        assert_eq!(ids(&sections.active), ["b", "a"]);
        // The b hold ended at this partition, so it no longer reads as working.
        assert!(sections.working.is_empty());

        observe(&mut tracker, &[running("c", true, json!({}))], 13_000);
        observe(&mut tracker, &[], 13_000);
        assert_eq!((tracker.returned_at("a"), tracker.next_deadline()), (None, None));
    }

    #[test]
    fn sort_orders_match_the_web() {
        let threads = vec![
            // Active: newest of createdAt, unsettledAt, run requested/completed.
            thread("a-old", json!({})),
            thread("a-unsettled", json!({"unsettledAt": "2026-11-03T00:00:00.000Z"})),
            thread(
                "a-run",
                json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-11-02T00:00:00.000Z"}),
            ),
            // Pinned: keyed by key, then keyless newest first.
            thread("p-b", json!({"pinnedAt": NOW, "pinOrderKey": "b"})),
            thread("p-a", json!({"pinnedAt": NOW, "pinOrderKey": "a"})),
            thread("p-new", json!({"pinnedAt": NOW, "createdAt": "2026-11-03T00:00:00.000Z"})),
            thread("p-old", json!({"pinnedAt": NOW})),
            // Settled: settledAt, else latest activity, descending.
            thread("s-at", json!({"settledOverride": "settled", "settledAt": "2026-11-02T00:00:00.000Z"})),
            thread("s-msg", json!({"settledOverride": "settled", "latestUserMessageAt": "2026-11-03T00:00:00.000Z"})),
            // Snoozed: soonest wake first.
            thread("z-late", json!({"snoozedUntil": "2026-11-09T00:00:00.000Z"})),
            thread("z-soon", json!({"snoozedUntil": "2026-11-05T00:00:00.000Z"})),
            // Working: newest send first; an explicit null authored time beats the run time.
            thread(
                "w-run",
                json!({"latestRunId": "r", "status": "running", "latestRunRequestedAt": "2026-11-03T00:00:00.000Z"}),
            ),
            thread(
                "w-msg",
                json!({"latestRunId": "r", "status": "running", "latestUserAuthoredMessageAt": "2026-11-02T00:00:00.000Z", "latestRunRequestedAt": "2026-11-04T00:00:00.000Z"}),
            ),
        ];
        let sections = partition(&threads, now(), true, false, &mut ReturnTracker::default(), |_| true);
        assert_eq!(ids(&sections.active), ["a-unsettled", "a-run", "a-old"]);
        assert_eq!(ids(&sections.pinned), ["p-a", "p-b", "p-new", "p-old"]);
        assert_eq!(ids(&sections.settled), ["s-msg", "s-at"]);
        assert_eq!(ids(&sections.snoozed), ["z-soon", "z-late"]);
        assert_eq!(ids(&sections.working), ["w-run", "w-msg"]);
        // Boring sidebar folders: no Working shelf, and Active sorts by send.
        let folders = partition(&threads, now(), false, true, &mut ReturnTracker::default(), |_| true);
        assert_eq!(ids(&folders.active), ["w-run", "w-msg", "a-old", "a-run", "a-unsettled"]);
    }

    #[test]
    fn status_unread_and_recede() {
        let requests = HashMap::new();
        let read = thread(
            "read",
            json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-11-02T00:00:00.000Z", "lastVisitedAt": "2026-11-03T00:00:00.000Z"}),
        );
        let unread = thread(
            "unread",
            json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-11-03T00:00:00.000Z", "lastVisitedAt": "2026-11-02T00:00:00.000Z"}),
        );
        let never = thread(
            "never",
            json!({"latestRunId": "r", "status": "completed", "latestRunCompletedAt": "2026-11-03T00:00:00.000Z"}),
        );
        let waiting = thread("waiting", json!({"latestRunId": "r", "status": "running", "blockedOnThreadWait": true}));
        let limited =
            thread("limited", json!({"latestRunId": "r", "status": "failed", "lastErrorClass": "usage_limit"}));
        let woke = thread(
            "woke",
            json!({"snoozedUntil": "2026-11-04T11:00:00.000Z", "lastVisitedAt": "2026-11-04T10:00:00.000Z"}),
        );
        let [read, unread, never, waiting, limited, woke] =
            [&read, &unread, &never, &waiting, &limited, &woke].map(|t| Facts::new(t, &requests));
        assert_eq!(read.status(), Status::Ready);
        assert!(!read.is_unread() && unread.is_unread() && !never.is_unread());
        assert_eq!(waiting.status(), Status::Waiting);
        assert_eq!(limited.status(), Status::Limited);
        assert!(woke.is_woke(now()));
        assert!(recedes(Status::Ready, false, false, false));
        assert!(!recedes(Status::Ready, true, false, false));
        assert!(!recedes(Status::Ready, false, false, true));
        assert!(recedes(Status::Working, false, false, false));
        assert!(!recedes(Status::Failed, false, false, false));
        // A closed folder's dot: attention over failure over done; waiting stays quiet.
        assert_eq!(folder_dot(&[read, waiting]), None);
        assert_eq!(folder_dot(&[read, unread, waiting]), Some(FolderDot::Unread));
        assert_eq!(folder_dot(&[unread, limited]), Some(FolderDot::Attention));
        let running = thread("running", json!({"latestRunId": "r", "status": "running"}));
        assert_eq!(folder_dot(&[read, Facts::new(&running, &requests)]), Some(FolderDot::Working));
    }

    #[test]
    fn working_duration_starts_at_the_activity_run() {
        let requests = HashMap::new();
        let activity = thread(
            "a",
            json!({"latestRunId": "r", "status": "running", "activityRunStartedAt": "2026-11-04T11:25:00.000Z"}),
        );
        let run = thread(
            "b",
            json!({"latestRunId": "r", "activeRunId": "r", "status": "running", "latestRunCompletedAt": null, "latestRunStartedAt": "2026-11-04T11:00:00.000Z"}),
        );
        assert_eq!(Facts::new(&activity, &requests).working_started_at(), parse_ms("2026-11-04T11:25:00.000Z"));
        assert_eq!(Facts::new(&run, &requests).working_started_at(), parse_ms("2026-11-04T11:00:00.000Z"));
    }

    #[test]
    fn snooze_presets_follow_the_web_labels_and_order() {
        use chrono::TimeZone as _;
        // Tuesday 2026-11-03 14:05 local.
        let zone = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        let now = zone.with_ymd_and_hms(2026, 11, 3, 14, 5, 0).unwrap();
        let settings = snooze_settings(&serde_json::Value::Null);
        let presets = snooze_presets(&settings, &now);
        let rows: Vec<(&str, &str)> = presets.iter().map(|p| (p.label.as_str(), p.when_label.as_str())).collect();
        assert_eq!(
            rows,
            [
                ("In 1 hour", "3:05 PM"),
                ("In 3 hours", "5:05 PM"),
                ("This evening", "6:00 PM"),
                ("Tomorrow", "9:00 AM"),
                ("Next week", "Mon 9:00 AM"),
            ]
        );
        // After 6 PM "This evening" is gone; on a Sunday Monday 9:00 folds into Tomorrow.
        let sunday = zone.with_ymd_and_hms(2026, 11, 8, 19, 0, 0).unwrap();
        let labels: Vec<String> = snooze_presets(&settings, &sunday).into_iter().map(|p| p.label).collect();
        assert_eq!(labels, ["In 1 hour", "In 3 hours", "Tomorrow"]);
        let late = zone.with_ymd_and_hms(2026, 11, 3, 23, 30, 0).unwrap();
        assert_eq!(resolve_snooze(&SnoozeSetting::After { minutes: 90 }, &late).unwrap().when_label, "Wed 1:00 AM");
        assert_eq!(resolve_snooze(&SnoozeSetting::After { minutes: 2880 }, &late).unwrap().label, "In 2 days");
        assert_eq!(resolve_snooze(&SnoozeSetting::After { minutes: i64::MAX }, &late), None);
        assert_eq!(resolve_snooze(&SnoozeSetting::After { minutes: i64::MAX / 60_000 }, &late), None);
        let later = now + chrono::Duration::minutes(10);
        assert_eq!(pick_snooze(&presets, "after:60", &settings, &later), Some(later.timestamp_millis() + 3_600_000));
        assert_eq!(pick_snooze(&presets, "at:tomorrow:09:00", &settings, &later), Some(presets[3].wake_ms));
    }

    #[test]
    fn pin_keys_sort_between_their_neighbors() {
        assert_eq!(pin_key_between(None, None).as_deref(), Some("n"));
        assert_eq!(pin_key_between(None, Some("n")).as_deref(), Some("h"));
        assert_eq!(pin_key_between(Some("n"), None).as_deref(), Some("u"));
        assert_eq!(pin_key_between(Some("n"), Some("na")), None, "a trailing `a` is corrupt");
        assert_eq!(pin_key_between(Some("u"), Some("n")), None);
        for after in ["b", "ab", "aab"] {
            let key = pin_key_between(None, Some(after)).unwrap();
            assert!(key.as_str() < after && !key.ends_with('a'), "{key} < {after}");
        }
    }

    #[test]
    fn custom_snooze_needs_a_future_time_or_positive_duration() {
        use chrono::TimeZone as _;
        let zone = chrono::FixedOffset::east_opt(0).unwrap();
        let now = zone.with_ymd_and_hms(2026, 11, 3, 14, 5, 0).unwrap();
        let at = |date: &str, time: &str| CustomSnooze::Date { date: date.into(), time: time.into() };
        let after = |amount: &str| CustomSnooze::Duration { amount: amount.into(), unit: SnoozeUnit::Hours };
        assert_eq!(resolve_custom_snooze(&at("2026-11-03", "15:00"), &now), Ok(now.timestamp_millis() + 55 * 60_000));
        assert!(resolve_custom_snooze(&at("2026-11-03", "14:00"), &now).is_err(), "in the past");
        assert!(resolve_custom_snooze(&at("2026-02-30", "09:00"), &now).is_err(), "no such day");
        assert_eq!(resolve_custom_snooze(&after("1.5"), &now), Ok(now.timestamp_millis() + 5_400_000));
        assert_eq!(resolve_custom_snooze(&after("0"), &now), Err("Enter a positive duration."));
        for huge in ["1e20", "1e300", "9223372036854775807"] {
            assert_eq!(resolve_custom_snooze(&after(huge), &now), Err("Enter a positive duration."), "{huge}");
        }
    }

    #[test]
    fn snooze_settings_skip_presets_the_contract_rejects_and_keep_the_rest() {
        let value = serde_json::json!([
            {"kind": "after", "minutes": 30},
            {"kind": "after", "minutes": i64::MAX},
            {"kind": "after", "minutes": 0},
            {"kind": "after", "minutes": 1e30},
            {"kind": "at", "day": "today", "time": "18:00"},
        ]);
        let at = SnoozeSetting::At { day: "today".into(), hour: 18, minute: 0 };
        assert_eq!(snooze_settings(&value), [SnoozeSetting::After { minutes: 30 }, at]);
    }

    #[test]
    fn provider_updates_need_a_one_click_update_to_a_newer_version() {
        let provider = |advisory: serde_json::Value, extra: serde_json::Value| {
            let mut value = serde_json::json!({"instanceId": "codex", "driver": "codex", "enabled": true, "versionAdvisory": advisory});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            serde_json::from_value::<crate::model::Provider>(value).unwrap()
        };
        let behind = serde_json::json!({"status": "behind_latest", "latestVersion": "1.2.0", "canUpdate": true});
        assert!(is_provider_update_candidate(&provider(behind.clone(), serde_json::json!({}))));
        let current = serde_json::json!({"status": "current", "latestVersion": "1.2.0", "canUpdate": true});
        assert!(!is_provider_update_candidate(&provider(current, serde_json::json!({}))));
        let manual = serde_json::json!({"status": "behind_latest", "latestVersion": "1.2.0", "canUpdate": false});
        assert!(!is_provider_update_candidate(&provider(manual, serde_json::json!({}))));
        let broken = serde_json::json!({"compatibilityAdvisory": {"latestVersionStatus": "broken"}});
        assert!(!is_provider_update_candidate(&provider(behind.clone(), broken)));
        let running = serde_json::json!({"updateState": {"status": "running"}});
        assert!(!is_provider_update_candidate(&provider(behind, running)));
    }
}
