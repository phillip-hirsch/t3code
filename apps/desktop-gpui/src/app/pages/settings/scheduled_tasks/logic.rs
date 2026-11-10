//! Port of `apps/web/src/components/settings/scheduledTasksSettings.logic.ts`, plus the
//! pure helpers of `ScheduledTasksSettings.tsx` (`scheduleLabel`, `relativeLabel`).
//! Times are local to `tz` so tests can pin a zone; callers pass `chrono::Local`.

use std::collections::BTreeSet;

use chrono::{DateTime, LocalResult, NaiveDateTime, Offset as _, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{ModelSelection, ProjectShell, ServerInfo, ThreadShell};
use crate::settings::scope::{Scope, ScopeKind};

/// `ScheduledTask` (`packages/contracts/src/scheduledTask.ts`). The schedule, workspace
/// strategy and model selection stay raw so a save sends back what it did not edit.
#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::app) struct ScheduledTask {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: Value,
    pub project_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    /// `ScheduledTaskThreadMode`; servers before thread modes omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_mode: Option<String>,
    pub workspace_strategy: Value,
    pub model_selection: Value,
    pub runtime_mode: String,
    pub interaction_mode: String,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run_status: String,
    #[serde(default)]
    pub last_run_error: Option<String>,
    /// `ScheduledTaskWebhookEndpoint`, on webhook tasks.
    #[serde(default)]
    pub webhook: Option<Value>,
    #[serde(default)]
    pub blocked_by_policy: Option<String>,
}

impl ScheduledTask {
    /// The task as the server sent it, for the webhook and worktree fields.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    pub fn schedule(&self) -> Schedule {
        Schedule::of(&self.schedule)
    }

    /// `scheduledTaskThreadMode`: older servers' bound tasks continue the conversation.
    pub fn thread_mode(&self) -> ThreadMode {
        match self.thread_mode.as_deref() {
            Some("new_thread") => ThreadMode::NewThread,
            Some("continue") => ThreadMode::Continue,
            Some("fresh_session") => ThreadMode::FreshSession,
            _ if self.thread_id.is_none() => ThreadMode::NewThread,
            _ => ThreadMode::Continue,
        }
    }
}

/// `ScheduledTaskThreadMode`: where runs go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum ThreadMode {
    NewThread,
    Continue,
    FreshSession,
}

impl ThreadMode {
    pub const ALL: [ThreadMode; 3] = [ThreadMode::NewThread, ThreadMode::Continue, ThreadMode::FreshSession];

    pub fn wire(self) -> &'static str {
        match self {
            ThreadMode::NewThread => "new_thread",
            ThreadMode::Continue => "continue",
            ThreadMode::FreshSession => "fresh_session",
        }
    }

    /// `THREAD_MODE_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            ThreadMode::NewThread => "New thread each run",
            ThreadMode::Continue => "One thread, continuing the conversation",
            ThreadMode::FreshSession => "One thread, fresh session each run",
        }
    }

    /// The Destination field's explanation; `bound` when the task already has its thread.
    pub fn detail(self, bound: bool) -> &'static str {
        match (self, bound) {
            (ThreadMode::NewThread, _) => "Each run starts a new thread with the workspace and model below.",
            (ThreadMode::Continue, false) => {
                "The first run starts a thread with the workspace and model below. Later runs continue that conversation."
            }
            (ThreadMode::FreshSession, false) => {
                "The first run starts a thread with the workspace and model below. Later runs post there, each in a fresh session without earlier runs' context."
            }
            (ThreadMode::Continue, true) => {
                "Each run continues this conversation, in its workspace and with its current model."
            }
            (ThreadMode::FreshSession, true) => {
                "Each run starts a fresh session in this thread, without earlier runs' context."
            }
        }
    }
}

/// `scheduledTasks.subscribe` elements: the whole list each time. Like the TS
/// `ForwardCompatibleArray`, a task that fails to decode is skipped, not the frame.
pub(in crate::app) fn decode_tasks(value: &Value) -> Vec<ScheduledTask> {
    value["tasks"]
        .as_array()
        .map(|tasks| tasks.iter().filter_map(|task| ScheduledTask::deserialize(task).ok()).collect())
        .unwrap_or_default()
}

/// `ScheduledTaskSchedule`, as far as the client reads it.
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) enum Schedule {
    Interval { every_ms: f64 },
    FixedTime { time_of_day: String, weekdays: Vec<u8> },
    Once { run_at: String },
    Webhook,
    Unknown,
}

impl Schedule {
    pub fn of(value: &Value) -> Self {
        let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
        // `0..=6`, Sunday first; anything else is dropped rather than wrapped.
        let weekday = |day: &Value| day.as_u64().and_then(|day| u8::try_from(day).ok()).filter(|day| *day < 7);
        match value["type"].as_str() {
            Some("interval") => Schedule::Interval { every_ms: value["everyMs"].as_f64().unwrap_or(0.) },
            Some("fixed_time") => Schedule::FixedTime {
                time_of_day: text("timeOfDay"),
                weekdays: value["weekdays"]
                    .as_array()
                    .map(|days| days.iter().filter_map(weekday).collect())
                    .unwrap_or_default(),
            },
            Some("once") => Schedule::Once { run_at: text("runAt") },
            Some("webhook") => Schedule::Webhook,
            _ => Schedule::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ScheduleMode {
    Fixed,
    Once,
    Interval,
    Webhook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceMode {
    Root,
    Worktree,
    ExistingWorktree,
}

impl WorkspaceMode {
    pub const ALL: [WorkspaceMode; 3] = [WorkspaceMode::Worktree, WorkspaceMode::Root, WorkspaceMode::ExistingWorktree];

    /// `WORKSPACE_MODE_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            WorkspaceMode::Worktree => "Create a new worktree",
            WorkspaceMode::Root => "Use the project checkout",
            WorkspaceMode::ExistingWorktree => "Use a specific checkout",
        }
    }
}

pub(super) const WEEKDAY_ORDER: [u8; 7] = [1, 2, 3, 4, 5, 6, 0];
pub(super) const WEEKDAY_LABELS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
pub(super) const WEEKDAY_SHORT: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
pub(super) const MIN_INTERVAL_MS: f64 = 60_000.;
const DATETIME_LOCAL: &str = "%Y-%m-%dT%H:%M";

#[derive(Clone, Debug, PartialEq)]
pub(super) struct DraftError {
    pub title: &'static str,
    pub description: String,
}

impl DraftError {
    pub fn new(title: &'static str, description: impl Into<String>) -> Self {
        Self { title, description: description.into() }
    }
}

/// The editor's form state (`DraftState`). Text fields mirror their inputs.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DraftState {
    pub editing_id: Option<String>,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule_mode: ScheduleMode,
    pub interval_minutes: String,
    pub time_of_day: String,
    /// Local date and time as a `datetime-local` value, such as 2026-11-02T15:00.
    pub run_at: String,
    /// The stored ISO run time, sent back unchanged when the field was not edited.
    pub saved_run_at: String,
    pub weekdays: BTreeSet<u8>,
    pub project_id: String,
    pub thread_id: String,
    pub thread_mode: ThreadMode,
    pub workspace_mode: WorkspaceMode,
    pub base_ref: String,
    pub start_from_origin: bool,
    pub existing_worktree_path: String,
    /// `instanceId:model`, empty for the default model.
    pub model_key: String,
    pub runtime_mode: String,
    pub interaction_mode: String,
    /// Keeps the source selection's provider options while the picker leaves instanceId:model unchanged.
    pub base_model_selection: Option<Value>,
}

impl Default for DraftState {
    /// `EMPTY_DRAFT`.
    fn default() -> Self {
        Self {
            editing_id: None,
            title: String::new(),
            prompt: String::new(),
            enabled: true,
            schedule_mode: ScheduleMode::Fixed,
            interval_minutes: "15".into(),
            time_of_day: "09:00".into(),
            run_at: String::new(),
            saved_run_at: String::new(),
            weekdays: BTreeSet::from([1, 2, 3, 4, 5]),
            project_id: String::new(),
            thread_id: String::new(),
            thread_mode: ThreadMode::NewThread,
            workspace_mode: WorkspaceMode::Worktree,
            base_ref: "main".into(),
            start_from_origin: true,
            existing_worktree_path: String::new(),
            model_key: String::new(),
            runtime_mode: "full-access".into(),
            interaction_mode: "default".into(),
            base_model_selection: None,
        }
    }
}

impl DraftState {
    /// `moveDraftToThread`: runs post to `thread_id`, continuing it unless a one-thread mode was picked.
    pub fn move_to_thread(&mut self, thread_id: &str, project_id: &str) {
        thread_id.clone_into(&mut self.thread_id);
        project_id.clone_into(&mut self.project_id);
        if self.thread_mode == ThreadMode::NewThread {
            self.thread_mode = ThreadMode::Continue;
        }
    }

    /// `unbindDraftFromThread`: leaves the thread but keeps the mode. Like MCP, new threads stay
    /// out of the shared checkout, so a project-checkout task gets a worktree from the remote.
    pub fn unbind_from_thread(&mut self) {
        if self.thread_id.is_empty() {
            return;
        }
        self.thread_id.clear();
        if self.workspace_mode == WorkspaceMode::Root {
            self.workspace_mode = WorkspaceMode::Worktree;
            self.start_from_origin = true;
        }
    }

    /// `changeDraftProject`: a thread belongs to one project, so another project unbinds it.
    pub fn change_project(&mut self, project_id: &str) {
        if project_id != self.project_id {
            self.unbind_from_thread();
            project_id.clone_into(&mut self.project_id);
        }
    }

    /// The Destination select: "New thread each run" leaves the bound thread.
    pub fn set_thread_mode(&mut self, mode: ThreadMode) {
        if mode == ThreadMode::NewThread {
            self.unbind_from_thread();
        }
        self.thread_mode = mode;
    }
}

/// One row of the destination thread picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DestinationChoice {
    /// A thread id, or [`NEW_THREAD_DESTINATION`].
    pub value: String,
    pub label: String,
    /// What a search matches: the thread's own title, which may be empty.
    search_text: Option<String>,
}

/// Thread ids are trimmed, so the leading space keeps this from matching one.
pub(super) const NEW_THREAD_DESTINATION: &str = " new-thread";

impl DestinationChoice {
    /// "New thread each run" shows only without a search; threads match on their title.
    pub fn matches(&self, query: &str) -> bool {
        let search = query.trim().to_lowercase();
        match &self.search_text {
            None => search.is_empty(),
            Some(text) => text.to_lowercase().contains(&search),
        }
    }
}

/// The destination picker's trigger label (`currentLabel`) and selected value.
pub(super) fn destination_current(
    threads: &[ThreadShell],
    environment: &str,
    draft: &DraftState,
) -> (String, Option<String>) {
    if !draft.thread_id.is_empty() {
        let title = threads
            .iter()
            .find(|t| t.environment_id == environment && t.id == draft.thread_id)
            .map(|t| t.title.trim())
            .filter(|title| !title.is_empty());
        let label = title.map_or_else(|| format!("Thread {}", draft.thread_id), str::to_owned);
        return (label, Some(draft.thread_id.clone()));
    }
    if draft.thread_mode == ThreadMode::NewThread {
        return (ThreadMode::NewThread.label().to_owned(), Some(NEW_THREAD_DESTINATION.to_owned()));
    }
    ("Started by the first run".to_owned(), None)
}

/// The picker's rows (`items`): "New thread each run", the current thread, then the threads a
/// task can move to (`scheduledTaskDestinationThreads`): the project's listed, unarchived
/// threads on its environment, most recently updated first.
pub(super) fn destination_choices(
    threads: &[ThreadShell],
    environment: &str,
    project_id: &str,
    draft: &DraftState,
) -> Vec<DestinationChoice> {
    let (current_label, _) = destination_current(threads, environment, draft);
    let mut others: Vec<&ThreadShell> = threads
        .iter()
        .filter(|t| {
            t.environment_id == environment
                && t.project_id == project_id
                && t.id != draft.thread_id
                && t.archived_at.is_none()
                && t.deleted_at.is_none()
                && t.is_listed()
        })
        .collect();
    others.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let new_thread = DestinationChoice {
        value: NEW_THREAD_DESTINATION.to_owned(),
        label: ThreadMode::NewThread.label().to_owned(),
        search_text: None,
    };
    let current = (!draft.thread_id.is_empty()).then(|| DestinationChoice {
        value: draft.thread_id.clone(),
        label: current_label.clone(),
        search_text: Some(current_label),
    });
    let others = others.into_iter().map(|t| DestinationChoice {
        value: t.id.clone(),
        label: if t.title.is_empty() { "Untitled thread".to_owned() } else { t.title.clone() },
        search_text: Some(t.title.clone()),
    });
    std::iter::once(new_thread).chain(current).chain(others).collect()
}

/// Whether the destination picker must show `choices` in place of its `shown` rows. While its
/// menu is open only a new, removed or renamed row counts, since replacing the items drops the
/// search being typed; a reorder waits for the menu to close, so it reopens newest first.
pub(super) fn destination_rows_stale(shown: &[DestinationChoice], choices: &[DestinationChoice], open: bool) -> bool {
    fn by_value(rows: &[DestinationChoice]) -> Vec<&DestinationChoice> {
        let mut rows: Vec<&DestinationChoice> = rows.iter().collect();
        rows.sort_by(|a, b| a.value.cmp(&b.value));
        rows
    }
    if open { by_value(shown) != by_value(choices) } else { shown != choices }
}

/// Formats an instant as a `datetime-local` value in `tz`.
pub(super) fn to_datetime_local_value<Tz: TimeZone>(instant: &DateTime<Tz>) -> String {
    instant.naive_local().format(DATETIME_LOCAL).to_string()
}

pub(super) fn parse_datetime_local(value: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value, DATETIME_LOCAL).ok()
}

fn parse_iso(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).ok().map(|instant| instant.with_timezone(&Utc))
}

/// `Date.prototype.toISOString`.
pub(super) fn to_iso(instant: DateTime<Utc>) -> String {
    instant.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub(super) fn schedule_draft_errors<Tz: TimeZone>(draft: &DraftState, tz: &Tz) -> Vec<DraftError> {
    match draft.schedule_mode {
        ScheduleMode::Once => {
            let Some(naive) = parse_datetime_local(&draft.run_at) else {
                return vec![DraftError::new("Invalid run time", "Pick a date and time.")];
            };
            // A time skipped by a daylight saving change has no instant.
            if matches!(tz.from_local_datetime(&naive), LocalResult::None) {
                return vec![DraftError::new("Invalid run time", "That time does not exist in your time zone.")];
            }
            Vec::new()
        }
        // `webhooks::WebhookFields::errors` checks the webhook fields.
        ScheduleMode::Webhook => Vec::new(),
        _ => Vec::new(),
    }
}

/// JS `Number(text)` for the interval field: blank is 0, junk is NaN.
fn js_number(text: &str) -> f64 {
    let text = text.trim();
    if text.is_empty() { 0. } else { text.parse().unwrap_or(f64::NAN) }
}

/// `scheduleFromDraft` for every mode but webhook, which `webhooks::WebhookFields::schedule` builds.
/// The interval may come out invalid (null for junk, below a minute); the editor checks it, as the web's submit does.
pub(super) fn schedule_from_draft<Tz: TimeZone>(draft: &DraftState, tz: &Tz) -> Option<Value> {
    if !schedule_draft_errors(draft, tz).is_empty() {
        return None;
    }
    match draft.schedule_mode {
        ScheduleMode::Once => {
            let saved = parse_iso(&draft.saved_run_at);
            if let Some(saved) = saved
                && to_datetime_local_value(&saved.with_timezone(tz)) == draft.run_at
            {
                return Some(json!({"type": "once", "runAt": draft.saved_run_at}));
            }
            let naive = parse_datetime_local(&draft.run_at)?;
            let instant = match tz.from_local_datetime(&naive) {
                LocalResult::Single(instant) => instant,
                // In an hour that repeats when clocks go back, keep the saved time's offset;
                // otherwise take the first, as `new Date(local)` does.
                LocalResult::Ambiguous(first, second) => {
                    let saved_offset = saved.map(|s| s.with_timezone(tz).offset().fix());
                    if saved_offset == Some(second.offset().fix()) { second } else { first }
                }
                LocalResult::None => return None,
            };
            Some(json!({"type": "once", "runAt": to_iso(instant.with_timezone(&Utc))}))
        }
        ScheduleMode::Interval => {
            let every_ms = (js_number(&draft.interval_minutes) * 60_000.).round();
            #[expect(clippy::cast_possible_truncation, reason = "finite, and `as` saturates out-of-range values")]
            let every_ms = if every_ms.is_finite() { json!(every_ms as i64) } else { Value::Null };
            Some(json!({"type": "interval", "everyMs": every_ms}))
        }
        ScheduleMode::Fixed => {
            let time_of_day = if draft.time_of_day.is_empty() { "09:00" } else { &draft.time_of_day };
            let mut schedule = json!({"type": "fixed_time", "timeOfDay": time_of_day});
            if !draft.weekdays.is_empty() && draft.weekdays.len() != 7 {
                schedule["weekdays"] = json!(draft.weekdays.iter().collect::<Vec<_>>());
            }
            Some(schedule)
        }
        ScheduleMode::Webhook => None,
    }
}

/// JS `String(number)` for whole and fractional minutes.
fn js_number_text(value: f64) -> String {
    format!("{value}")
}

pub(super) fn task_to_draft<Tz: TimeZone>(task: &ScheduledTask, tz: &Tz) -> DraftState {
    let schedule = task.schedule();
    let strategy = &task.workspace_strategy;
    let workspace_mode = match strategy["type"].as_str() {
        Some("root") => WorkspaceMode::Root,
        Some("existing_worktree") => WorkspaceMode::ExistingWorktree,
        _ => WorkspaceMode::Worktree,
    };
    let worktree = workspace_mode == WorkspaceMode::Worktree;
    let selection = &task.model_selection;
    DraftState {
        editing_id: Some(task.id.clone()),
        title: task.title.clone(),
        prompt: task.prompt.clone(),
        enabled: task.enabled,
        schedule_mode: match schedule {
            Schedule::Once { .. } => ScheduleMode::Once,
            Schedule::Interval { .. } => ScheduleMode::Interval,
            Schedule::Webhook => ScheduleMode::Webhook,
            Schedule::FixedTime { .. } | Schedule::Unknown => ScheduleMode::Fixed,
        },
        interval_minutes: match schedule {
            Schedule::Interval { every_ms } => js_number_text((every_ms / 60_000.).max(1.)),
            _ => "15".into(),
        },
        time_of_day: match &schedule {
            Schedule::FixedTime { time_of_day, .. } => time_of_day.clone(),
            _ => "09:00".into(),
        },
        run_at: match &schedule {
            Schedule::Once { run_at } => {
                parse_iso(run_at).map(|t| to_datetime_local_value(&t.with_timezone(tz))).unwrap_or_default()
            }
            _ => String::new(),
        },
        saved_run_at: match &schedule {
            Schedule::Once { run_at } => run_at.clone(),
            _ => String::new(),
        },
        weekdays: match &schedule {
            Schedule::FixedTime { weekdays, .. } if !weekdays.is_empty() => weekdays.iter().copied().collect(),
            _ => (0..7).collect(),
        },
        project_id: task.project_id.clone(),
        thread_id: task.thread_id.clone().unwrap_or_default(),
        thread_mode: task.thread_mode(),
        workspace_mode,
        base_ref: if worktree { strategy["baseRef"].as_str().unwrap_or("main").to_owned() } else { "main".into() },
        start_from_origin: if worktree { strategy["startFromOrigin"].as_bool().unwrap_or(false) } else { true },
        existing_worktree_path: if workspace_mode == WorkspaceMode::ExistingWorktree {
            strategy["worktreePath"].as_str().unwrap_or_default().to_owned()
        } else {
            String::new()
        },
        model_key: format!(
            "{}:{}",
            selection["instanceId"].as_str().unwrap_or_default(),
            selection["model"].as_str().unwrap_or_default()
        ),
        runtime_mode: task.runtime_mode.clone(),
        interaction_mode: task.interaction_mode.clone(),
        base_model_selection: Some(selection.clone()),
    }
}

/// `splitModelKey`.
pub(super) fn split_model_key(value: &str) -> Option<ModelSelection> {
    let (instance_id, model) = value.split_once(':')?;
    (!instance_id.is_empty() && !model.is_empty()).then(|| ModelSelection {
        instance_id: instance_id.to_owned(),
        model: model.to_owned(),
        options: None,
    })
}

/// `scheduledTaskDefaultModel`: the project's default, then the environment's, while its
/// provider is usable and the model not legacy; else a usable provider's default model.
pub(super) fn default_model(info: &ServerInfo, project: Option<&ProjectShell>) -> Option<ModelSelection> {
    let available: Vec<_> = info
        .providers
        .iter()
        .filter(|p| {
            p.enabled
                && p.installed
                && p.availability.as_deref() != Some("unavailable")
                && p.auth.status != "unauthenticated"
        })
        .collect();
    // `resolveProjectSettings`: the stored override wins over the legacy project field until folded.
    let configured = project.and_then(|project| {
        let stored = info.settings.get("projectSettingsOverrides")[project.id.as_str()].get("defaultModelSelection");
        match stored {
            Some(value) => ModelSelection::deserialize(value).ok(),
            None if info.settings.bool("projectSettingsFolded") => None,
            None => project.default_model_selection.clone(),
        }
    });
    for selection in [configured.as_ref(), info.default_model_selection.as_ref()].into_iter().flatten() {
        let usable = available.iter().any(|p| {
            p.instance_id == selection.instance_id
                && !p.models.iter().find(|m| m.slug == selection.model).is_some_and(|m| m.is_legacy)
        });
        if usable {
            return Some(selection.clone());
        }
    }
    let models: Vec<_> =
        available.iter().flat_map(|p| p.models.iter().filter(|m| !m.is_legacy).map(move |m| (p, m))).collect();
    let (provider, model) = models.iter().find(|(_, m)| m.is_default == Some(true)).or(models.first())?;
    Some(ModelSelection { instance_id: provider.instance_id.clone(), model: model.slug.clone(), options: None })
}

/// `defaultScheduledTaskProjectId` for the unscoped (global) selection: a pinned open project, then any open one.
/// `defaultScheduledTaskProjectId`: a pinned open project, then any open one; an archived one
/// only when Settings is scoped to it, so that scope can still save.
pub(super) fn default_project_id(projects: &[ProjectShell], scoped_to_project: bool) -> String {
    let mut open = projects.iter().filter(|p| p.archived_at.is_none());
    let first = open.clone().next();
    let fallback = projects.first().filter(|_| scoped_to_project);
    open.find(|p| p.pinned_at.is_some()).or(first).or(fallback).map(|p| p.id.clone()).unwrap_or_default()
}

/// `matchesScheduledTaskScope`: a task shows, and a project can take one, when its environment
/// is selected and, under a project scope, it belongs to one of the picked checkouts.
pub(super) fn matches_scope(scope: &Scope, environment_id: &str, project_id: &str) -> bool {
    if !scope.environment_ids.iter().any(|id| id == environment_id) {
        return false;
    }
    match &scope.kind {
        ScopeKind::Unavailable { .. } => false,
        ScopeKind::Project { members } => {
            members.iter().any(|m| m.environment_id == environment_id && m.id == project_id)
        }
        ScopeKind::All | ScopeKind::Environment => true,
    }
}

const RUNTIME_MODES: [&str; 4] = ["approval-required", "auto-accept-edits", "auto", "full-access"];

/// `clampManagedRuntimeMode`: tasks have no mode picker, so a mode above the
/// administrator's ceiling would fail every run.
pub(super) fn clamp_runtime_mode(policy: &Value, mode: &str) -> String {
    let rank = |mode: &str| RUNTIME_MODES.iter().position(|m| *m == mode);
    match (policy.pointer("/limits/maxRuntimeMode").and_then(Value::as_str), rank(mode)) {
        (Some(ceiling), Some(index)) if rank(ceiling).is_some_and(|c| index > c) => ceiling.to_owned(),
        _ => mode.to_owned(),
    }
}

/// `managedCapabilityDenial(policy, "webhookTasks")`.
pub(super) fn webhook_denial(policy: &Value) -> Option<String> {
    let organization = policy["organization"].as_str()?;
    (policy.pointer("/limits/webhookTasks").and_then(Value::as_bool) == Some(false))
        .then(|| format!("{organization}'s policy does not allow webhook tasks."))
}

/// `scheduleLabel`, on the system's clock.
pub(in crate::app) fn schedule_label<Tz: TimeZone>(schedule: &Schedule, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    schedule_label_on(schedule, tz, crate::timestamp::hour12("locale"))
}

fn schedule_label_on<Tz: TimeZone>(schedule: &Schedule, tz: &Tz, hour12: bool) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match schedule {
        Schedule::Webhook => "On webhook".into(),
        Schedule::Once { run_at } => match parse_iso(run_at) {
            // `Intl.DateTimeFormat(undefined, {dateStyle: "medium", timeStyle: "short"})`, en-US.
            Some(t) => {
                let t = t.with_timezone(tz);
                format!("Once, {}, {}", t.format("%b %-d, %Y"), crate::timestamp::clock(&t, hour12))
            }
            None => "Once".into(),
        },
        Schedule::Interval { every_ms } => {
            let minutes = every_ms / 60_000.;
            if minutes.fract() == 0. {
                format!("Every {minutes} min")
            } else {
                format!("Every {} sec", (every_ms / 1000.).round())
            }
        }
        Schedule::FixedTime { time_of_day, weekdays } => {
            let days = if weekdays.is_empty() {
                "Daily".to_owned()
            } else if weekdays.len() == 5 && weekdays.iter().all(|d| (1..=5).contains(d)) {
                "Weekdays".to_owned()
            } else {
                weekdays.iter().filter_map(|d| WEEKDAY_LABELS.get(*d as usize)).copied().collect::<Vec<_>>().join(", ")
            };
            format!("{days} at {time_of_day}")
        }
        Schedule::Unknown => String::new(),
    }
}

/// `relativeLabel`: past times as `formatRelativeTime`, upcoming ones as "in 5m".
pub(in crate::app) fn relative_label(value: Option<&str>, now_ms: i64) -> String {
    let Some(at) = value.and_then(parse_iso) else {
        return "Not scheduled".into();
    };
    let diff_ms = at.timestamp_millis() - now_ms;
    if diff_ms <= 0 {
        return crate::app::pages::settings::ago_label(-diff_ms);
    }
    let minutes = (diff_ms + 59_999) / 60_000;
    if minutes < 2 {
        return "in under a minute".into();
    }
    if minutes < 60 {
        return format!("in {minutes}m");
    }
    #[expect(clippy::cast_possible_truncation, reason = "rounded first, and `as` saturates out-of-range values")]
    let hours = (minutes as f64 / 60.).round() as i64;
    if hours < 24 {
        return format!("in {hours}h");
    }
    #[expect(clippy::cast_possible_truncation, reason = "rounded first, and `as` saturates out-of-range values")]
    let days = (hours as f64 / 24.).round() as i64;
    format!("in {days}d")
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;

    fn task(schedule: Value) -> ScheduledTask {
        ScheduledTask::deserialize(json!({
            "id": "task-1", "title": "Check", "prompt": "Say hi", "enabled": true,
            "schedule": schedule, "projectId": "p1", "threadId": null,
            "workspaceStrategy": {"type": "worktree", "baseRef": "dev", "startFromOrigin": true},
            "modelSelection": {"instanceId": "codex", "model": "gpt", "options": [{"id": "effort", "value": "high"}]},
            "runtimeMode": "auto", "interactionMode": "default", "lastRunStatus": "never",
        }))
        .unwrap()
    }

    fn zone() -> FixedOffset {
        FixedOffset::west_opt(5 * 3600).unwrap()
    }

    #[test]
    fn labels_each_schedule() {
        let label = |schedule: Value| schedule_label_on(&Schedule::of(&schedule), &zone(), true);
        assert_eq!(label(json!({"type": "fixed_time", "timeOfDay": "09:00"})), "Daily at 09:00");
        assert_eq!(
            label(json!({"type": "fixed_time", "timeOfDay": "09:00", "weekdays": [1, 2, 3, 4, 5]})),
            "Weekdays at 09:00"
        );
        assert_eq!(label(json!({"type": "fixed_time", "timeOfDay": "18:30", "weekdays": [0, 6]})), "Sun, Sat at 18:30");
        assert_eq!(label(json!({"type": "interval", "everyMs": 900_000})), "Every 15 min");
        assert_eq!(label(json!({"type": "interval", "everyMs": 30_000})), "Every 30 sec");
        assert_eq!(label(json!({"type": "once", "runAt": "2026-11-02T20:00:00.000Z"})), "Once, Nov 2, 2026, 3:00 PM");
        assert_eq!(label(json!({"type": "webhook", "signature": null})), "On webhook");
    }

    #[test]
    fn relative_labels_cover_past_and_future() {
        let now = parse_iso("2026-11-05T12:00:00Z").unwrap().timestamp_millis();
        let at = |iso: &str| relative_label(Some(iso), now);
        assert_eq!(relative_label(None, now), "Not scheduled");
        assert_eq!(at("2026-11-05T12:00:30Z"), "in under a minute");
        assert_eq!(at("2026-11-05T12:05:00Z"), "in 5m");
        assert_eq!(at("2026-11-05T14:40:00Z"), "in 3h");
        assert_eq!(at("2026-11-08T12:00:00Z"), "in 3d");
        assert_eq!(at("2026-11-05T11:59:30Z"), "just now");
        assert_eq!(at("2026-11-05T11:45:00Z"), "15m ago");
        assert_eq!(at("2026-11-05T09:00:00Z"), "3h ago");
        assert_eq!(at("2026-11-01T12:00:00Z"), "4d ago");
    }

    #[test]
    fn fixed_schedule_round_trips_and_drops_every_day() {
        let draft =
            task_to_draft(&task(json!({"type": "fixed_time", "timeOfDay": "07:15", "weekdays": [5, 1]})), &zone());
        assert_eq!(draft.schedule_mode, ScheduleMode::Fixed);
        assert_eq!(draft.weekdays, BTreeSet::from([1, 5]));
        let wrapped = Schedule::of(&json!({"type": "fixed_time", "timeOfDay": "07:15", "weekdays": [256, 263, 3]}));
        assert_eq!(wrapped, Schedule::FixedTime { time_of_day: "07:15".into(), weekdays: vec![3] });
        assert_eq!(
            schedule_from_draft(&draft, &zone()),
            Some(json!({"type": "fixed_time", "timeOfDay": "07:15", "weekdays": [1, 5]}))
        );
        let daily = task_to_draft(&task(json!({"type": "fixed_time", "timeOfDay": "07:15"})), &zone());
        assert_eq!(daily.weekdays.len(), 7);
        assert_eq!(schedule_from_draft(&daily, &zone()), Some(json!({"type": "fixed_time", "timeOfDay": "07:15"})));
        let blank = DraftState { time_of_day: String::new(), ..DraftState::default() };
        assert_eq!(schedule_from_draft(&blank, &zone()).unwrap()["timeOfDay"], "09:00");
    }

    #[test]
    fn interval_minutes_round_trip_and_parse_like_js() {
        let draft = task_to_draft(&task(json!({"type": "interval", "everyMs": 5_400_000})), &zone());
        assert_eq!(draft.interval_minutes, "90");
        assert_eq!(schedule_from_draft(&draft, &zone()), Some(json!({"type": "interval", "everyMs": 5_400_000})));
        let legacy = task_to_draft(&task(json!({"type": "interval", "everyMs": 30_000})), &zone());
        assert_eq!(legacy.interval_minutes, "1");
        let half = DraftState {
            schedule_mode: ScheduleMode::Interval,
            interval_minutes: " 1.5 ".into(),
            ..DraftState::default()
        };
        assert_eq!(schedule_from_draft(&half, &zone()).unwrap()["everyMs"], 90_000);
        let junk = DraftState { interval_minutes: "abc".into(), ..half };
        assert!(schedule_from_draft(&junk, &zone()).unwrap()["everyMs"].is_null());
    }

    #[test]
    fn once_keeps_an_unedited_run_time_and_converts_an_edited_one() {
        let draft = task_to_draft(&task(json!({"type": "once", "runAt": "2026-11-02T20:00:00Z"})), &zone());
        assert_eq!(draft.run_at, "2026-11-02T15:00");
        assert_eq!(
            schedule_from_draft(&draft, &zone()),
            Some(json!({"type": "once", "runAt": "2026-11-02T20:00:00Z"}))
        );
        let edited = DraftState { run_at: "2026-11-03T08:30".into(), ..draft.clone() };
        assert_eq!(
            schedule_from_draft(&edited, &zone()),
            Some(json!({"type": "once", "runAt": "2026-11-03T13:30:00.000Z"}))
        );
        let blank = DraftState { run_at: String::new(), ..draft };
        assert_eq!(
            schedule_draft_errors(&blank, &zone()),
            vec![DraftError::new("Invalid run time", "Pick a date and time.")]
        );
        assert_eq!(schedule_from_draft(&blank, &zone()), None);
    }

    #[test]
    fn task_to_draft_keeps_workspace_and_model() {
        let draft = task_to_draft(&task(json!({"type": "interval", "everyMs": 60_000})), &zone());
        assert_eq!(draft.workspace_mode, WorkspaceMode::Worktree);
        assert_eq!((draft.base_ref.as_str(), draft.start_from_origin), ("dev", true));
        assert_eq!(draft.model_key, "codex:gpt");
        assert_eq!(draft.base_model_selection.unwrap()["options"][0]["value"], "high");
        assert_eq!(draft.editing_id.as_deref(), Some("task-1"));
        let mut root = task(json!({"type": "interval", "everyMs": 60_000}));
        root.workspace_strategy = json!({"type": "root"});
        let root = task_to_draft(&root, &zone());
        assert_eq!(
            (root.workspace_mode, root.base_ref.as_str(), root.start_from_origin),
            (WorkspaceMode::Root, "main", true)
        );
        assert_eq!(split_model_key("codex:gpt-5:mini").map(|s| s.model), Some("gpt-5:mini".into()));
        assert_eq!(split_model_key(":gpt"), None);
    }

    #[test]
    fn destination_changes_unbind_the_thread_they_leave() {
        let mut bound = task(json!({"type": "interval", "everyMs": 60_000}));
        bound.thread_id = Some("thread:bound".into());
        bound.workspace_strategy = json!({"type": "root"});
        let bound = task_to_draft(&bound, &zone());

        // Unbinding leaves the shared checkout for a new worktree from the remote.
        let mut draft = bound.clone();
        draft.set_thread_mode(ThreadMode::NewThread);
        assert_eq!(
            (draft.thread_id.as_str(), draft.thread_mode, draft.workspace_mode, draft.start_from_origin),
            ("", ThreadMode::NewThread, WorkspaceMode::Worktree, true)
        );
        let mut draft = bound.clone();
        draft.set_thread_mode(ThreadMode::FreshSession);
        assert_eq!((draft.thread_id.as_str(), draft.workspace_mode), ("thread:bound", WorkspaceMode::Root));

        let mut draft = bound.clone();
        draft.change_project("p1");
        assert_eq!(draft.thread_id, "thread:bound", "the same project keeps the thread");
        draft.change_project("p2");
        assert_eq!((draft.thread_id.as_str(), draft.project_id.as_str()), ("", "p2"));

        let mut unbound = DraftState { workspace_mode: WorkspaceMode::Root, start_from_origin: false, ..bound.clone() };
        unbound.thread_id.clear();
        unbound.unbind_from_thread();
        assert_eq!((unbound.workspace_mode, unbound.start_from_origin), (WorkspaceMode::Root, false));

        let mut draft = DraftState::default();
        draft.move_to_thread("t2", "p1");
        assert_eq!(
            (draft.thread_id.as_str(), draft.project_id.as_str(), draft.thread_mode),
            ("t2", "p1", ThreadMode::Continue)
        );
        let mut draft = DraftState { thread_mode: ThreadMode::FreshSession, ..bound };
        draft.move_to_thread("t2", "p1");
        assert_eq!(draft.thread_mode, ThreadMode::FreshSession);
    }

    #[test]
    fn the_destination_picker_lists_the_projects_threads_newest_first() {
        let thread = |env: &str, id: &str, title: &str, updated: &str, extra: Value| {
            let mut value =
                json!({"id": id, "projectId": "p1", "title": title, "status": "idle", "updatedAt": updated});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let mut thread: ThreadShell = serde_json::from_value(value).unwrap();
            thread.environment_id = env.to_owned().into();
            thread
        };
        let none = json!({});
        let threads = vec![
            thread("laptop", "old", "Older settled run", "2026-01-01T00:00:00Z", none.clone()),
            thread("laptop", "new", "Fresh run", "2026-03-01T00:00:00Z", none.clone()),
            thread("laptop", "blank", "", "2026-02-01T00:00:00Z", none.clone()),
            thread("laptop", "bound", "  Nightly  ", "2026-04-01T00:00:00Z", none.clone()),
            thread(
                "laptop",
                "archived",
                "Archived",
                "2026-05-01T00:00:00Z",
                json!({"archivedAt": "2026-05-02T00:00:00Z"}),
            ),
            thread(
                "laptop",
                "child",
                "Subagent",
                "2026-05-01T00:00:00Z",
                json!({"lineage": {"relationshipToParent": "subagent"}}),
            ),
            thread("laptop", "elsewhere", "Other project", "2026-05-01T00:00:00Z", json!({"projectId": "p2"})),
            thread("desktop", "remote", "Other environment", "2026-05-01T00:00:00Z", none),
        ];
        let draft =
            DraftState { thread_id: "bound".into(), thread_mode: ThreadMode::Continue, ..DraftState::default() };
        let choices = destination_choices(&threads, "laptop", "p1", &draft);
        let rows: Vec<(&str, &str)> = choices.iter().map(|c| (c.value.as_str(), c.label.as_str())).collect();
        assert_eq!(
            rows,
            [
                (NEW_THREAD_DESTINATION, "New thread each run"),
                ("bound", "Nightly"),
                ("new", "Fresh run"),
                ("blank", "Untitled thread"),
                ("old", "Older settled run"),
            ]
        );
        let matching = |query: &str| -> Vec<&str> {
            choices.iter().filter(|c| c.matches(query)).map(|c| c.value.as_str()).collect()
        };
        assert_eq!(matching(" RUN "), ["new", "old"], "a search hides New thread each run");
        assert_eq!(matching("nightly"), ["bound"]);
        assert!(matching("untitled").is_empty(), "an untitled thread matches on its own title");

        let mut reordered = choices.clone();
        reordered.swap(2, 3);
        assert!(!destination_rows_stale(&choices, &reordered, true), "a reorder waits while the menu is open");
        assert!(destination_rows_stale(&choices, &reordered, false), "a closed menu reopens newest first");
        assert!(destination_rows_stale(&choices, &choices[..4], true), "a removed row shows at once");
        assert!(!destination_rows_stale(&choices, &choices, false));

        assert_eq!(destination_current(&threads, "laptop", &draft), ("Nightly".into(), Some("bound".into())));
        let gone = DraftState { thread_id: "gone".into(), ..draft.clone() };
        assert_eq!(destination_current(&threads, "laptop", &gone).0, "Thread gone");
        let first_run = DraftState { thread_id: String::new(), ..draft };
        assert_eq!(destination_current(&threads, "laptop", &first_run), ("Started by the first run".into(), None));
        let each_run = DraftState::default();
        assert_eq!(
            destination_current(&threads, "laptop", &each_run),
            ("New thread each run".into(), Some(NEW_THREAD_DESTINATION.into()))
        );
    }

    #[test]
    fn thread_mode_falls_back_to_the_binding_on_older_servers() {
        let mut task = task(json!({"type": "interval", "everyMs": 60_000}));
        assert_eq!(task.thread_mode(), ThreadMode::NewThread);
        task.thread_id = Some("thread-1".into());
        assert_eq!(task_to_draft(&task, &zone()).thread_mode, ThreadMode::Continue);
        task.thread_mode = Some("fresh_session".into());
        assert_eq!(task.thread_mode(), ThreadMode::FreshSession);
        assert_eq!(task.to_json()["threadMode"], "fresh_session");
    }

    #[test]
    fn a_bad_task_is_skipped_not_the_list() {
        let list = json!({"tasks": [{"id": "broken"}, task_json()]});
        assert_eq!(decode_tasks(&list).iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["task-1"]);
    }

    fn task_json() -> Value {
        json!({
            "id": "task-1", "title": "Check", "prompt": "Say hi", "enabled": true,
            "schedule": {"type": "interval", "everyMs": 60_000}, "projectId": "p1",
            "workspaceStrategy": {"type": "root"}, "modelSelection": {"instanceId": "codex", "model": "gpt"},
            "runtimeMode": "auto", "interactionMode": "default",
        })
    }

    fn project(id: &str, pinned: bool, archived: bool) -> ProjectShell {
        ProjectShell::deserialize(json!({
            "id": id, "title": id, "workspaceRoot": "/r",
            "pinnedAt": pinned.then_some("2026-01-01T00:00:00Z"),
            "archivedAt": archived.then_some("2026-01-01T00:00:00Z"),
        }))
        .unwrap()
    }

    #[test]
    fn default_project_prefers_pinned_open_projects() {
        assert_eq!(default_project_id(&[project("a", false, false), project("b", true, false)], false), "b");
        assert_eq!(default_project_id(&[project("a", true, true), project("b", false, false)], false), "b");
        assert_eq!(default_project_id(&[project("a", false, true)], false), "");
        assert_eq!(default_project_id(&[project("a", false, true)], true), "a");
    }

    #[test]
    fn tasks_match_the_selected_environments_and_picked_checkouts() {
        use crate::settings::scope::Project;
        let scope = |kind, environments: &[&str]| Scope {
            kind,
            label: String::new(),
            environment_ids: environments.iter().map(|id| (*id).to_owned()).collect(),
        };
        let checkout = |id: &str, environment: &str| Project {
            id: id.into(),
            environment_id: environment.into(),
            name: "repo".into(),
            workspace_root: "/repo".into(),
            legacy_model_selection: None,
            legacy_thread_env_mode: None,
        };
        let all = scope(ScopeKind::All, &["laptop", "server"]);
        assert!(matches_scope(&all, "server", "p9"));
        assert!(!matches_scope(&all, "gone", "p9"));
        let laptop = scope(ScopeKind::Environment, &["laptop"]);
        assert!(matches_scope(&laptop, "laptop", "p9"));
        assert!(!matches_scope(&laptop, "server", "p9"));
        let project = scope(ScopeKind::Project { members: vec![checkout("p1", "laptop")] }, &["laptop"]);
        assert!(matches_scope(&project, "laptop", "p1"));
        assert!(!matches_scope(&project, "laptop", "p2"));
        let unavailable = scope(ScopeKind::Unavailable { message: "gone" }, &["laptop"]);
        assert!(!matches_scope(&unavailable, "laptop", "p1"));
    }

    fn info(providers: Value, settings: Value) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.apply(&json!({"type": "snapshot", "config": {"providers": providers, "settings": settings}})).unwrap();
        info
    }

    #[test]
    fn default_model_skips_unusable_configured_models() {
        let providers = json!([
            {"instanceId": "off", "enabled": false, "installed": true, "auth": {"status": "authenticated"},
             "models": [{"slug": "x", "isDefault": true}]},
            {"instanceId": "codex", "enabled": true, "installed": true, "auth": {"status": "authenticated"},
             "models": [{"slug": "old", "isLegacy": true}, {"slug": "a"}, {"slug": "b", "isDefault": true}]},
        ]);
        let pick = |settings: Value| default_model(&info(providers.clone(), settings), None).map(|s| s.model);
        assert_eq!(pick(json!({"defaultModelSelection": {"instanceId": "codex", "model": "a"}})), Some("a".into()));
        assert_eq!(pick(json!({"defaultModelSelection": {"instanceId": "codex", "model": "old"}})), Some("b".into()));
        assert_eq!(pick(json!({"defaultModelSelection": {"instanceId": "off", "model": "x"}})), Some("b".into()));
        let override_info = info(
            providers.clone(),
            json!({"projectSettingsOverrides": {"p": {"defaultModelSelection": {"instanceId": "codex", "model": "a"}}}}),
        );
        assert_eq!(default_model(&override_info, Some(&project("p", false, false))).map(|s| s.model), Some("a".into()));
    }

    #[test]
    fn managed_policy_clamps_modes_and_denies_webhooks() {
        let policy = json!({"organization": "Acme", "lockedSettings": [], "limits": {"maxRuntimeMode": "auto-accept-edits", "webhookTasks": false}});
        assert_eq!(clamp_runtime_mode(&policy, "full-access"), "auto-accept-edits");
        assert_eq!(clamp_runtime_mode(&policy, "approval-required"), "approval-required");
        assert_eq!(clamp_runtime_mode(&Value::Null, "full-access"), "full-access");
        assert_eq!(webhook_denial(&policy).as_deref(), Some("Acme's policy does not allow webhook tasks."));
        assert_eq!(webhook_denial(&Value::Null), None);
    }
}
