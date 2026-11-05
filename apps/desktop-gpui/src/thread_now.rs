//! What the open thread is doing now, for the end of the timeline (`ThreadNow.tsx`,
//! `ThreadActivityBanners.tsx`, `ThreadStatusLine.tsx`): what needs you, its work, its
//! goal, then its snooze or settle status. Presenters follow
//! `packages/client-runtime/src/state/threadExecution.ts`.

use gpui_kit::SharedString;
use serde_json::Value;

use crate::model::{PendingBackgroundTask, Run, ThreadGoal, ThreadShell, TurnItem};
use crate::work_log;

#[derive(Clone, Debug, PartialEq)]
pub enum NowLine {
    /// Subagents or side chats waiting on the user (`childInputNowItem`).
    ChildInput {
        title: SharedString,
        approvals: bool,
        children: Vec<(SharedString, SharedString)>,
    },
    RunStop(RunStop),
    Background(BackgroundWork),
    Goal(Goal),
    Status(StatusLine),
}

impl NowLine {
    /// Shows a clock that moves every second.
    pub fn ticks(&self) -> bool {
        match self {
            NowLine::RunStop(_) => true,
            NowLine::Background(work) => work.started_at_ms.is_some(),
            _ => false,
        }
    }
}

/// `RunStopPresentation`.
#[derive(Clone, Debug, PartialEq)]
pub struct RunStop {
    /// The provider did not confirm the stop in time, so Force stop is offered.
    pub unresponsive: bool,
    pub run_id: SharedString,
    pub requested_at_ms: i64,
    /// The run already ended; the Stop targeted its background work.
    pub background: bool,
}

/// `presentRunStop`: a Stop the provider has not finished yet.
pub fn run_stop(runs: &[Run], turn_items: &[std::rc::Rc<TurnItem>]) -> Option<RunStop> {
    let interruptible = |status: &str| matches!(status, "preparing" | "starting" | "running");
    let latest_by = |keep: &dyn Fn(&Run) -> bool| runs.iter().filter(|r| keep(r)).max_by_key(|r| r.ordinal);
    let active = latest_by(&|r: &Run| interruptible(&r.status));
    let run = active.or_else(|| latest_by(&|r: &Run| r.status != "queued"))?;
    let mut request: Option<&TurnItem> = None;
    for item in turn_items.iter().filter(|item| item.run_id.as_deref() == Some(run.id.as_str())) {
        match item.item_type.as_str() {
            "run_interrupt_result" => return None,
            "run_interrupt_request" => request = Some(item),
            _ => {}
        }
    }
    let request = request?;
    let unresponsive = request.raw.get("unresponsiveAt").is_some_and(|v| !v.is_null());
    if active.is_none() && !unresponsive {
        return None;
    }
    let requested_at = request.str("startedAt").or_else(|| request.str("updatedAt"))?;
    Some(RunStop {
        unresponsive,
        run_id: run.id.clone().into(),
        requested_at_ms: crate::timestamp::parse_ms(requested_at)?,
        background: active.is_none(),
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundWork {
    pub title: SharedString,
    pub description: Option<SharedString>,
    /// Every task is a command, monitor or background task, not a subagent.
    pub commands_only: bool,
    /// One task, with its item's start time.
    pub started_at_ms: Option<i64>,
    /// One subagent, which opens.
    pub open_thread: Option<SharedString>,
}

fn kind_order(kind: &str) -> (u8, &'static str, &'static str) {
    match kind {
        "subagent" => (0, "subagent", "subagents"),
        "command" => (1, "command", "commands"),
        "monitor" => (2, "monitor", "monitors"),
        _ => (3, "background task", "background tasks"),
    }
}

/// `presentPendingBackgroundWork` joined to the thread's items (`backgroundWorkNowItem`).
pub fn background_work(
    tasks: &[PendingBackgroundTask],
    turn_items: &[std::rc::Rc<TurnItem>],
) -> Option<BackgroundWork> {
    if tasks.is_empty() {
        return None;
    }
    // `backgroundWorkHoldsCompletion`: commands and monitors hold only when awaited.
    let waiting =
        tasks.iter().any(|t| t.awaited == Some(true) || matches!(t.kind.as_str(), "subagent" | "background_task"));
    struct Task<'a> {
        task: &'a PendingBackgroundTask,
        label: String,
        item: Option<&'a TurnItem>,
    }
    let mut rows: Vec<Task> = tasks
        .iter()
        .map(|task| {
            let singular = kind_order(&task.kind).1;
            let label = task.description.as_deref().map(str::trim).filter(|d| !d.is_empty()).map(|d| {
                match task.kind.as_str() {
                    "subagent" => crate::timeline::subagent_title(d).trim().to_owned(),
                    "command" | "monitor" => work_log::command_display_text(d),
                    _ => d.to_owned(),
                }
            });
            let item = turn_items
                .iter()
                .rev()
                .find(|item| {
                    let native =
                        item.raw.pointer("/nativeItemRef/nativeId").and_then(Value::as_str).filter(|id| !id.is_empty());
                    native.unwrap_or(&item.id) == task.task_id
                })
                .map(AsRef::as_ref);
            Task { task, label: label.filter(|l| !l.is_empty()).unwrap_or_else(|| singular.to_owned()), item }
        })
        .collect();
    rows.sort_by_key(|row| kind_order(&row.task.kind).0);
    fn subagent<'a>(row: &Task<'a>) -> Option<&'a TurnItem> {
        row.item.filter(|item| item.item_type == "subagent")
    }
    let activity = |row: &Task| {
        subagent(row)
            .and_then(|item| item.str("progress"))
            .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|p| !p.is_empty())
    };
    let commands_only = rows.iter().all(|row| row.task.kind != "subagent");
    if let [only] = rows.as_slice() {
        let noun = kind_order(&only.task.kind).1;
        let title = if only.label != noun {
            format!("{} · {}", if waiting { "Waiting" } else { "Running" }, only.label)
        } else if waiting {
            format!("Waiting on a {noun}")
        } else {
            format!("Running a {noun}")
        };
        return Some(BackgroundWork {
            title: title.into(),
            description: activity(only).map(Into::into),
            commands_only,
            started_at_ms: only.item.and_then(|item| item.str("startedAt")).and_then(crate::timestamp::parse_ms),
            open_thread: only
                .task
                .child_thread_id
                .clone()
                .or_else(|| subagent(only).and_then(|item| item.str("childThreadId")).map(str::to_owned))
                .map(Into::into),
        });
    }
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for row in &rows {
        match counts.iter_mut().find(|(kind, _)| kind_order(kind).0 == kind_order(&row.task.kind).0) {
            Some((_, count)) => *count += 1,
            None => counts.push((&row.task.kind, 1)),
        }
    }
    let groups: Vec<String> = counts
        .iter()
        .map(|(kind, count)| {
            let (_, singular, plural) = kind_order(kind);
            format!("{count} {}", if *count == 1 { singular } else { plural })
        })
        .collect();
    let joined = match groups.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => groups.join(""),
    };
    let updated = |row: &Task| {
        subagent(row).and_then(|item| item.str("updatedAt")).and_then(crate::timestamp::parse_ms).unwrap_or(0)
    };
    let newest = rows.iter().filter(|row| activity(row).is_some()).max_by_key(|row| updated(row));
    let description = match newest {
        Some(row) => format!("{}: {}", row.label, activity(row).unwrap_or_default()),
        None => rows.iter().map(|row| row.label.as_str()).collect::<Vec<_>>().join(", "),
    };
    Some(BackgroundWork {
        title: format!("{} {joined}", if waiting { "Waiting on" } else { "Running" }).into(),
        description: Some(description.into()),
        commands_only,
        started_at_ms: None,
        open_thread: None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Goal {
    pub title: SharedString,
    pub objective: SharedString,
    pub can_resume: bool,
}

fn goal_tokens(tokens: u64) -> String {
    match tokens {
        0..1000 => tokens.to_string(),
        _ if (tokens as f64 / 1000.).round() < 1000. => format!("{}k", (tokens as f64 / 1000.).round()),
        _ => format!("{:.1}m", tokens as f64 / 1_000_000.).replace(".0m", "m"),
    }
}

/// `presentProviderGoal`, with the usage joined to the title like `goalNowItem`.
pub fn goal(goal: &ThreadGoal, working: bool) -> Goal {
    let mut usage = Vec::new();
    if let Some(used) = goal.tokens_used.filter(|used| *used > 0) {
        usage.push(match goal.token_budget {
            Some(budget) => format!("{} / {} tokens", goal_tokens(used), goal_tokens(budget)),
            None => format!("{} tokens", goal_tokens(used)),
        });
    }
    let used_ms = goal.time_used_seconds.filter(|s| *s >= 60).and_then(|s| i64::try_from(s).ok()?.checked_mul(1000));
    if let Some(ms) = used_ms {
        usage.push(work_log::format_duration(ms));
    }
    if let Some(checks) = goal.checks.filter(|c| *c > 0) {
        usage.push(format!("{checks} {}", if checks == 1 { "check" } else { "checks" }));
    }
    let title = match goal.status.as_str() {
        "active" if !working => "Goal set",
        "active" => "Pursuing goal",
        "paused" => "Goal paused",
        "blocked" => "Goal blocked",
        "usage_limited" => "Goal hit a usage limit",
        "budget_limited" => "Goal reached its token budget",
        _ => "Goal complete",
    };
    let title = if usage.is_empty() { title.to_owned() } else { format!("{title} · {}", usage.join(" · ")) };
    Goal {
        title: title.into(),
        objective: goal.objective.clone().into(),
        can_resume: !matches!(goal.status.as_str(), "active" | "complete"),
    }
}

/// `ThreadStatusLine`: the thread's snooze or settle state.
#[derive(Clone, Debug, PartialEq)]
pub enum StatusLine {
    Snoozed { until_ms: i64 },
    Settled { at_ms: Option<i64> },
    Woke { woke_at: SharedString },
}

impl StatusLine {
    /// The label at `now`, and when it next changes.
    pub fn label(&self, now: i64) -> (String, Option<i64>) {
        match self {
            StatusLine::Snoozed { until_ms } => {
                let remaining = until_ms - now;
                let label = if remaining <= 0 {
                    "Snoozed".to_owned()
                } else {
                    format!("Snoozed, {} left", crate::inbox::format_wake(remaining))
                };
                (label, crate::inbox::wake_changes_in(remaining))
            }
            StatusLine::Settled { at_ms: Some(at) } => {
                let elapsed = now - at;
                (format!("Settled {}", relative_label(elapsed)), Some(crate::inbox::relative_changes_in(elapsed)))
            }
            StatusLine::Settled { at_ms: None } => ("Settled".into(), None),
            StatusLine::Woke { .. } => ("Woke from snooze".into(), None),
        }
    }
}

/// `formatRelativeTimeLabel`.
fn relative_label(elapsed_ms: i64) -> String {
    if elapsed_ms < 60_000 { "just now".into() } else { format!("{} ago", crate::inbox::format_relative(elapsed_ms)) }
}

/// `childThreadInputsTitle`.
pub fn child_inputs_title(children: &[&ThreadShell]) -> String {
    let approvals = children.iter().filter(|c| has_pending_approval(c)).count();
    let need = match approvals {
        0 => "input",
        n if n == children.len() => "approval",
        _ => "you",
    };
    let side_chats = children.iter().filter(|c| c.lineage.side_chat == Some(true)).count();
    let noun = match side_chats {
        n if n == children.len() => "side chat",
        0 => "subagent",
        _ => "thread",
    };
    match children.len() {
        1 => format!("{} needs {need}", if noun == "side chat" { "Side chat" } else { "Subagent" }),
        n => format!("{n} {noun}s need {need}"),
    }
}

pub fn has_pending_approval(thread: &ThreadShell) -> bool {
    thread.pending_runtime_request.as_ref().is_some_and(|r| !matches!(r.kind.as_str(), "user_input" | "auth_refresh"))
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use serde_json::json;

    use super::*;

    fn run(id: &str, status: &str, ordinal: i64) -> Run {
        serde_json::from_value(json!({"id": id, "status": status, "ordinal": ordinal})).unwrap()
    }

    fn item(value: Value) -> Rc<TurnItem> {
        Rc::new(serde_json::from_value(value).unwrap())
    }

    #[test]
    fn a_stop_shows_until_the_provider_answers_it() {
        let runs = [run("r1", "running", 1)];
        let request = item(
            json!({"id": "i1", "type": "run_interrupt_request", "runId": "r1", "startedAt": "2026-01-01T00:00:00Z"}),
        );
        let stop = run_stop(&runs, std::slice::from_ref(&request)).unwrap();
        assert!(!stop.unresponsive && !stop.background);
        assert_eq!(stop.run_id, "r1");
        let answered = item(json!({"id": "i2", "type": "run_interrupt_result", "runId": "r1"}));
        assert_eq!(run_stop(&runs, &[request.clone(), answered]), None);
        // An ended run keeps only an unanswered Stop of its background work.
        let ended = [run("r1", "completed", 1)];
        assert_eq!(run_stop(&ended, &[request]), None);
        let stuck = item(json!({"id": "i1", "type": "run_interrupt_request", "runId": "r1",
            "startedAt": "2026-01-01T00:00:00Z", "unresponsiveAt": "2026-01-01T00:00:30Z"}));
        let stop = run_stop(&ended, &[stuck]).unwrap();
        assert!(stop.unresponsive && stop.background);
    }

    fn task(kind: &str, id: &str, description: Option<&str>) -> PendingBackgroundTask {
        serde_json::from_value(json!({"kind": kind, "taskId": id, "description": description})).unwrap()
    }

    #[test]
    fn background_work_reads_like_the_web() {
        let one = background_work(&[task("command", "t1", Some("npm run dev"))], &[]).unwrap();
        assert_eq!(one.title, "Running · npm run dev");
        assert!(one.commands_only);
        let unnamed = background_work(&[task("subagent", "t1", None)], &[]).unwrap();
        assert_eq!(unnamed.title, "Waiting on a subagent");
        let many = background_work(
            &[task("command", "a", Some("a")), task("subagent", "b", Some("Review")), task("command", "c", Some("c"))],
            &[],
        )
        .unwrap();
        assert_eq!(many.title, "Waiting on 1 subagent and 2 commands");
        assert_eq!(many.description.as_deref(), Some("Review, a, c"));
        let progress = item(json!({"id": "b", "type": "subagent", "progress": "Reading  files",
            "updatedAt": "2026-01-01T00:00:00Z", "childThreadId": "child"}));
        let busy = background_work(&[task("subagent", "b", Some("Review"))], &[progress]).unwrap();
        assert_eq!(busy.description.as_deref(), Some("Reading files"));
        assert_eq!(busy.open_thread.as_deref(), Some("child"));
    }

    #[test]
    fn goals_name_their_state_and_usage() {
        let goal_of = |value: Value| -> ThreadGoal { serde_json::from_value(value).unwrap() };
        let active = goal_of(json!({"status": "active", "objective": "Ship", "tokensUsed": 12_400, "checks": 1}));
        assert_eq!(goal(&active, true).title, "Pursuing goal · 12k tokens · 1 check");
        assert_eq!(goal(&active, false).title, "Goal set · 12k tokens · 1 check");
        let paused = goal_of(
            json!({"status": "paused", "objective": "Ship", "tokensUsed": 2_500_000, "tokenBudget": 3_000_000}),
        );
        let paused = goal(&paused, false);
        assert_eq!(paused.title, "Goal paused · 2.5m / 3m tokens");
        assert!(paused.can_resume);
    }

    #[test]
    fn status_lines_count_down_and_age() {
        let (label, next) = StatusLine::Snoozed { until_ms: 3 * 3_600_000 }.label(0);
        assert_eq!(label, "Snoozed, 3h left");
        assert_eq!(next, Some(3_600_000));
        assert_eq!(StatusLine::Snoozed { until_ms: 0 }.label(10).0, "Snoozed");
        assert_eq!(StatusLine::Settled { at_ms: Some(0) }.label(30_000).0, "Settled just now");
        assert_eq!(StatusLine::Settled { at_ms: Some(0) }.label(5 * 60_000).0, "Settled 5m ago");
    }
}
