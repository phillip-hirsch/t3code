//! Port of `apps/web/src/components/settings/scheduledTasksSettings.logic.ts`, plus the
//! pure helpers of `ScheduledTasksSettings.tsx` (`scheduleLabel`, `relativeLabel`).
//! Times are local to `tz` so tests can pin a zone; callers pass `chrono::Local`.

use std::collections::BTreeSet;

use chrono::{DateTime, LocalResult, NaiveDateTime, Offset as _, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{ModelSelection, ProjectShell, ServerInfo};

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
        match value["type"].as_str() {
            Some("interval") => Schedule::Interval { every_ms: value["everyMs"].as_f64().unwrap_or(0.) },
            Some("fixed_time") => Schedule::FixedTime {
                time_of_day: text("timeOfDay"),
                weekdays: value["weekdays"]
                    .as_array()
                    .map(|days| days.iter().filter_map(|d| d.as_u64().map(|d| d as u8)).collect())
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
pub(super) fn default_project_id(projects: &[ProjectShell]) -> String {
    let mut open = projects.iter().filter(|p| p.archived_at.is_none());
    let first = open.clone().next();
    open.find(|p| p.pinned_at.is_some()).or(first).map(|p| p.id.clone()).unwrap_or_default()
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

/// `scheduleLabel`.
pub(in crate::app) fn schedule_label<Tz: TimeZone>(schedule: &Schedule, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match schedule {
        Schedule::Webhook => "On webhook".into(),
        Schedule::Once { run_at } => match parse_iso(run_at) {
            // `Intl.DateTimeFormat(undefined, {dateStyle: "medium", timeStyle: "short"})`, en-US.
            Some(t) => format!("Once, {}", t.with_timezone(tz).format("%b %-d, %Y, %-I:%M %p")),
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
        let minutes = -diff_ms / 60_000;
        return match minutes {
            0 => "just now".into(),
            1..60 => format!("{minutes}m ago"),
            60..1440 => format!("{}h ago", minutes / 60),
            _ => format!("{}d ago", minutes / 1440),
        };
    }
    let minutes = (diff_ms + 59_999) / 60_000;
    if minutes < 2 {
        return "in under a minute".into();
    }
    if minutes < 60 {
        return format!("in {minutes}m");
    }
    let hours = (minutes as f64 / 60.).round() as i64;
    if hours < 24 {
        return format!("in {hours}h");
    }
    format!("in {}d", (hours as f64 / 24.).round() as i64)
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
        let label = |schedule: Value| schedule_label(&Schedule::of(&schedule), &zone());
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
        assert_eq!(default_project_id(&[project("a", false, false), project("b", true, false)]), "b");
        assert_eq!(default_project_id(&[project("a", true, true), project("b", false, false)]), "b");
        assert_eq!(default_project_id(&[project("a", false, true)]), "");
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
