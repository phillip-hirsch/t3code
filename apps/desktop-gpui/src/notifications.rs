//! Thread alerts: which shell transitions notify (`ThreadNotificationCoordinator.tsx`
//! and its `.logic.ts` gate). Presenting them is in `app/alerts.rs`.

use std::collections::HashMap;

use serde_json::Value;

use crate::inbox::{self, Facts, Status};
use crate::model::ThreadShell;

/// The `notificationMode` client setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Off,
    Notifications,
    Sound,
    NotificationsAndSound,
}

impl Mode {
    pub fn parse(value: &Value) -> Self {
        match value.as_str() {
            Some("notifications") => Self::Notifications,
            Some("sound") => Self::Sound,
            Some("notifications-and-sound") => Self::NotificationsAndSound,
            _ => Self::Off,
        }
    }

    pub fn sound(self) -> bool {
        matches!(self, Self::Sound | Self::NotificationsAndSound)
    }

    pub fn desktop(self) -> bool {
        matches!(self, Self::Notifications | Self::NotificationsAndSound)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Input,
    Completion,
}

/// One thread's notification-relevant state (`ThreadNotificationObservation`).
#[derive(Clone, Debug, PartialEq)]
struct Observation {
    /// Run and status while the thread needs attention.
    attention: Option<(Option<String>, Status)>,
    /// Idle, and its latest run completed.
    settled: bool,
    completed_at: Option<i64>,
}

impl Observation {
    fn of(facts: Facts) -> (Self, Status) {
        let thread = facts.thread;
        let mut status = facts.status();
        if status == Status::Ready && facts.latest_run_status() == Some("failed") {
            status = Status::Failed;
        }
        let attention =
            matches!(status, Status::Input | Status::Request | Status::Approval | Status::Failed | Status::Limited)
                .then(|| (thread.latest_run_id.clone(), status));
        let settled = status == Status::Ready && facts.latest_run_status() == Some("completed");
        let completed_at = facts.latest_run_completed_at().filter(|_| settled).and_then(inbox::parse_ms);
        (Self { attention, settled, completed_at }, status)
    }
}

/// `createThreadNotificationGate`: decides which transitions notify, at most once per completion.
#[derive(Default)]
struct Gate {
    /// The newest completion already notified, or deliberately skipped.
    handled: HashMap<String, i64>,
}

impl Gate {
    fn skip(&mut self, key: &str, at: Option<i64>) {
        let Some(at) = at else { return };
        match self.handled.get_mut(key) {
            Some(handled) => *handled = (*handled).max(at),
            None => {
                self.handled.insert(key.to_owned(), at);
            }
        }
    }

    fn observe(
        &mut self,
        key: &str,
        prior: Option<&Observation>,
        next: &Observation,
        archived: bool,
        quiet: bool,
    ) -> Option<Kind> {
        // The first sighting, archived threads, and stopped runs never notify completion.
        if prior.is_none() || archived || quiet {
            self.skip(key, next.completed_at);
        }
        let prior = prior.filter(|_| !archived)?;
        if next.attention.is_some() && next.attention != prior.attention {
            return Some(Kind::Input);
        }
        let at = next.completed_at.filter(|_| next.settled)?;
        if self.handled.get(key).is_some_and(|handled| at <= *handled) {
            return None;
        }
        self.skip(key, Some(at));
        Some(Kind::Completion)
    }
}

/// A transition to present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub thread_id: String,
    pub kind: Kind,
    pub status: Status,
}

impl Alert {
    pub fn title(&self) -> &'static str {
        match (self.kind, self.status) {
            (Kind::Completion, _) => "Thread completed",
            (_, Status::Approval) => "Approval needed",
            (_, Status::Limited) => "Usage limit reached",
            (_, Status::Failed) => "Thread failed",
            (_, Status::Request) => "Guest request",
            _ => "Input needed",
        }
    }
}

/// Watches one environment's shell for alerts while notifications are on.
#[derive(Default)]
pub struct ThreadAlerts {
    previous: HashMap<String, Observation>,
    gate: Gate,
}

impl ThreadAlerts {
    /// The alerts since the last call. Threads shown under a parent never alert.
    pub fn observe(&mut self, threads: &[ThreadShell]) -> Vec<Alert> {
        let requests = inbox::child_requests(threads);
        let mut alerts = Vec::new();
        let mut seen = 0;
        for thread in threads.iter().filter(|t| t.is_listed()) {
            seen += 1;
            let (next, status) = Observation::of(Facts::new(thread, &requests));
            let prior = self.previous.get(&thread.id);
            let quiet = thread.latest_run_stop_requested == Some(true);
            let kind = self.gate.observe(&thread.id, prior, &next, thread.archived_at.is_some(), quiet);
            if let Some(kind) = kind {
                alerts.push(Alert { thread_id: thread.id.clone(), kind, status });
            }
            match self.previous.get_mut(&thread.id) {
                Some(prior) => *prior = next,
                None => {
                    self.previous.insert(thread.id.clone(), next);
                }
            }
        }
        // Every listed thread is in `previous` now, so a longer map holds threads that left.
        if seen < self.previous.len() {
            let listed: std::collections::HashSet<&str> =
                threads.iter().filter(|t| t.is_listed()).map(|t| t.id.as_str()).collect();
            self.previous.retain(|id, _| listed.contains(id.as_str()));
            self.gate.handled.retain(|id, _| listed.contains(id.as_str()));
        }
        alerts
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn thread(id: &str, fields: Value) -> ThreadShell {
        let mut value = json!({
            "id": id,
            "projectId": "p",
            "title": id,
            "status": "completed",
            "updatedAt": "2026-01-01T00:00:00.000Z",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "lineage": {"rootThreadId": id},
        });
        value.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn running(id: &str) -> ThreadShell {
        thread(id, json!({"status": "running", "latestRunId": "r1", "activeRunId": "r1", "latestRunCompletedAt": null}))
    }

    fn completed(id: &str, run: &str, at: &str) -> ThreadShell {
        thread(id, json!({"latestRunId": run, "latestRunCompletedAt": at}))
    }

    fn kinds(alerts: &[Alert]) -> Vec<(&str, Kind, &str)> {
        alerts.iter().map(|a| (a.thread_id.as_str(), a.kind, a.title())).collect()
    }

    #[test]
    fn a_finished_run_notifies_once_and_the_first_sighting_never_does() {
        let mut alerts = ThreadAlerts::default();
        // Already completed when first seen.
        assert!(alerts.observe(&[completed("a", "r0", "2026-01-01T00:00:01.000Z")]).is_empty());
        assert!(alerts.observe(&[running("a")]).is_empty());
        let done = [completed("a", "r1", "2026-01-01T00:00:05.000Z")];
        assert_eq!(kinds(&alerts.observe(&done)), [("a", Kind::Completion, "Thread completed")]);
        assert!(alerts.observe(&done).is_empty());
    }

    #[test]
    fn attention_notifies_on_each_new_run_and_status() {
        let mut alerts = ThreadAlerts::default();
        alerts.observe(&[running("a")]);
        let approval = |run: &str| {
            thread(
                "a",
                json!({"status": "running", "latestRunId": run, "activeRunId": run, "pendingRuntimeRequest": {"kind": "command"}}),
            )
        };
        assert_eq!(kinds(&alerts.observe(&[approval("r1")])), [("a", Kind::Input, "Approval needed")]);
        assert!(alerts.observe(&[approval("r1")]).is_empty());
        let failed = thread("a", json!({"status": "failed", "latestRunId": "r1"}));
        assert_eq!(kinds(&alerts.observe(&[failed])), [("a", Kind::Input, "Thread failed")]);
        let limited = thread("a", json!({"status": "failed", "latestRunId": "r2", "lastErrorClass": "usage_limit"}));
        assert_eq!(kinds(&alerts.observe(&[limited])), [("a", Kind::Input, "Usage limit reached")]);
    }

    #[test]
    fn stopped_archived_and_child_threads_stay_quiet() {
        let mut alerts = ThreadAlerts::default();
        let child = |fields: Value| {
            let mut t = thread("child", fields);
            t.lineage.parent_thread_id = Some("a".into());
            t.lineage.relationship_to_parent = Some("subagent".into());
            t
        };
        alerts.observe(&[running("a"), running("b"), child(json!({"status": "running", "latestRunId": "r1"}))]);
        let stopped = thread(
            "a",
            json!({"latestRunId": "r1", "latestRunCompletedAt": "2026-01-01T00:00:05.000Z", "latestRunStopRequested": true}),
        );
        let archived = thread(
            "b",
            json!({"latestRunId": "r1", "latestRunCompletedAt": "2026-01-01T00:00:05.000Z", "archivedAt": "2026-01-01T00:00:06.000Z"}),
        );
        let child_done = child(json!({"latestRunId": "r1", "latestRunCompletedAt": "2026-01-01T00:00:05.000Z"}));
        assert!(alerts.observe(&[stopped.clone(), archived, child_done]).is_empty());
        // The stopped completion stays handled after the flag clears.
        let mut cleared = stopped;
        cleared.latest_run_stop_requested = None;
        assert!(alerts.observe(&[cleared]).is_empty());
    }

    #[test]
    fn a_thread_that_leaves_and_returns_is_a_first_sighting() {
        let mut alerts = ThreadAlerts::default();
        alerts.observe(&[running("a")]);
        alerts.observe(&[]);
        assert!(alerts.observe(&[completed("a", "r1", "2026-01-01T00:00:05.000Z")]).is_empty());
    }

    #[test]
    fn modes_split_sound_from_desktop_alerts() {
        let parse = |mode: &str| Mode::parse(&json!(mode));
        assert_eq!([parse("off").sound(), parse("off").desktop()], [false, false]);
        assert_eq!([parse("sound").sound(), parse("sound").desktop()], [true, false]);
        assert_eq!([parse("notifications").sound(), parse("notifications").desktop()], [false, true]);
        let both = parse("notifications-and-sound");
        assert_eq!([both.sound(), both.desktop()], [true, true]);
        assert_eq!(Mode::parse(&Value::Null), Mode::Off);
    }
}
