//! Marks the open thread read. Mirrors `createThreadVisitTracker` in
//! `packages/client-runtime/src/state/threadVisits.ts`: opening a thread sends
//! one visit with `opened`, then each shell update advances the read
//! watermark, throttled while a run streams.

use std::time::Duration;

use gpui_kit::{Context, Task};
use serde_json::json;

use super::{Workspace, new_id};
use crate::inbox::{self, Facts};
use crate::model::ThreadShell;

const THROTTLE_MS: i64 = 10_000;

#[derive(Default)]
pub(super) struct VisitTracker {
    thread_id: Option<String>,
    state: VisitState,
    _timer: Option<Task<()>>,
}

#[derive(Default)]
struct VisitState {
    open_pending: bool,
    /// The watermark last sent (or seen echoed), in ms.
    last_dispatched: Option<i64>,
    last_dispatch_at: Option<i64>,
}

#[derive(Debug, PartialEq)]
enum Visit {
    Send { visited_at: String, opened: bool },
    Later(i64),
    Skip,
}

/// The tracker's `onShell` decision for one shell update. A snooze wake marks
/// the thread with no shell event to follow, so a snoozed thread also checks
/// again at its wake.
fn decide(state: &mut VisitState, shell: &ThreadShell, woke_at: Option<&str>, now: i64) -> Visit {
    let visit = decide_now(state, shell, woke_at, now);
    let wake =
        shell.snoozed_until.as_deref().and_then(inbox::parse_ms).map(|until| until - now).filter(|in_ms| *in_ms > 0);
    match (visit, wake) {
        (Visit::Skip, Some(wake)) => Visit::Later(wake),
        (Visit::Later(delay), Some(wake)) => Visit::Later(delay.min(wake)),
        (visit, _) => visit,
    }
}

fn decide_now(state: &mut VisitState, shell: &ThreadShell, woke_at: Option<&str>, now: i64) -> Visit {
    // A server without visit tracking has no watermark to move (the web keeps a local one instead).
    if !shell.tracks_visits() {
        return Visit::Skip;
    }
    let Some(updated) = inbox::parse_ms(&shell.updated_at) else { return Visit::Skip };
    let woke = woke_at.and_then(inbox::parse_ms).filter(|woke| *woke > updated);
    let (visited_at, updated) = match (woke, woke_at) {
        (Some(woke), Some(at)) => (at, woke),
        _ => (shell.updated_at.as_str(), updated),
    };
    let send = |state: &mut VisitState, opened: bool| {
        state.last_dispatched = Some(updated);
        state.last_dispatch_at = Some(now);
        Visit::Send { visited_at: visited_at.to_owned(), opened }
    };
    if state.open_pending {
        state.open_pending = false;
        return send(state, true);
    }
    let last_visited = shell.last_visited().and_then(inbox::parse_ms);
    if last_visited.is_some_and(|visited| visited >= updated) {
        // Already seen, so a later mark-unread stays sticky.
        if state.last_dispatched.is_none_or(|sent| sent < updated) {
            state.last_dispatched = Some(updated);
        }
        return Visit::Skip;
    }
    if state.last_dispatched.is_some_and(|sent| sent >= updated) {
        return Visit::Skip;
    }
    let completed = shell.latest_run_completed_at.clone().flatten().as_deref().and_then(inbox::parse_ms);
    let unseen_completion = completed.is_some_and(|completed| last_visited.is_none_or(|visited| completed > visited));
    let elapsed = state.last_dispatch_at.map_or(THROTTLE_MS, |at| now - at);
    if unseen_completion || woke.is_some() || elapsed >= THROTTLE_MS {
        return send(state, false);
    }
    Visit::Later(THROTTLE_MS - elapsed)
}

impl Workspace {
    /// Starts tracking `thread_id` as the thread on screen.
    pub(super) fn open_visit(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.visit = VisitTracker {
            thread_id: Some(thread_id.to_owned()),
            state: VisitState { open_pending: true, ..VisitState::default() },
            _timer: None,
        };
        self.track_visit(cx);
    }

    /// Runs after every shell update, and when a throttled visit comes due.
    pub(super) fn track_visit(&mut self, cx: &mut Context<Self>) {
        self.visit._timer = None;
        let Some(thread_id) = self.visit.thread_id.clone() else { return };
        if self.open_thread_id() != Some(&*thread_id) {
            return;
        }
        let Some(shell) = self.shell.threads.iter().find(|t| t.id == thread_id) else { return };
        // Decide only once it can be sent: deciding records the visit as sent.
        let Some(client) = self.client() else { return };
        let now = super::sidebar::now_ms();
        let woke_at = Facts::new(shell, &Default::default()).woke_at(now);
        match decide(&mut self.visit.state, shell, woke_at, now) {
            Visit::Skip => {}
            Visit::Later(delay) => {
                self.visit._timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(delay.max(0) as u64)).await;
                    let _ = this.update(cx, |this, cx| this.track_visit(cx));
                }));
            }
            Visit::Send { visited_at, opened } => {
                let mut command = json!({"type": "thread.visit", "commandId": new_id(), "threadId": thread_id, "visitedAt": visited_at});
                if opened {
                    command["opened"] = true.into();
                }
                // Like the web, a failed visit is not worth an error banner.
                cx.spawn(async move |_, _| {
                    let _ = client.request("orchestration.dispatchCommand", command).await;
                })
                .detach();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(updated_at: &str, last_visited_at: Option<&str>, completed_at: Option<&str>) -> ThreadShell {
        serde_json::from_value(json!({
            "id": "t", "projectId": "p", "title": "t", "status": "idle", "updatedAt": updated_at,
            "lastVisitedAt": last_visited_at, "latestRunCompletedAt": completed_at,
        }))
        .unwrap()
    }

    const T0: &str = "2026-11-04T10:00:00.000Z";
    const T1: &str = "2026-11-04T10:00:01.000Z";
    const T2: &str = "2026-11-04T10:00:02.000Z";

    #[test]
    fn opening_sends_one_opened_visit_then_skips_until_the_thread_changes() {
        let mut state = VisitState { open_pending: true, ..VisitState::default() };
        let now = inbox::parse_ms(T2).unwrap();
        assert_eq!(
            decide(&mut state, &shell(T0, None, None), None, now),
            Visit::Send { visited_at: T0.into(), opened: true }
        );
        assert_eq!(decide(&mut state, &shell(T0, None, None), None, now), Visit::Skip);
    }

    #[test]
    fn streaming_updates_are_throttled_but_a_completion_goes_out_at_once() {
        let now = inbox::parse_ms(T2).unwrap();
        let mut state =
            VisitState { last_dispatched: Some(0), last_dispatch_at: Some(now - 1_000), ..VisitState::default() };
        assert_eq!(decide(&mut state, &shell(T1, Some(T0), None), None, now), Visit::Later(9_000));
        assert_eq!(
            decide(&mut state, &shell(T1, Some(T0), Some(T1)), None, now),
            Visit::Send { visited_at: T1.into(), opened: false }
        );
    }

    #[test]
    fn a_snoozed_open_thread_is_marked_read_when_it_wakes() {
        let mut snoozed = shell(T0, Some(T0), None);
        snoozed.snoozed_until = Some(T2.into());
        let mut state = VisitState::default();
        let before = inbox::parse_ms(T1).unwrap();
        assert_eq!(decide(&mut state, &snoozed, None, before), Visit::Later(1_000));
        let at = inbox::parse_ms(T2).unwrap();
        let woke_at = Facts::new(&snoozed, &Default::default()).woke_at(at);
        assert_eq!(decide(&mut state, &snoozed, woke_at, at), Visit::Send { visited_at: T2.into(), opened: false });
        assert_eq!(decide(&mut state, &snoozed, woke_at, at), Visit::Skip);
    }

    #[test]
    fn an_already_seen_update_is_not_resent() {
        let mut state = VisitState::default();
        let now = inbox::parse_ms(T2).unwrap();
        assert_eq!(decide(&mut state, &shell(T1, Some(T1), None), None, now), Visit::Skip);
        assert_eq!(state.last_dispatched, inbox::parse_ms(T1));
    }

    #[test]
    fn servers_without_visit_tracking_get_no_visits() {
        let mut state = VisitState { open_pending: true, ..VisitState::default() };
        let mut thread = shell(T0, None, None);
        thread.last_visited_at = None;
        let now = inbox::parse_ms(T2).unwrap();
        assert_eq!(decide(&mut state, &thread, None, now), Visit::Skip);
        assert!(state.open_pending, "the open waits for a server that tracks visits");
    }
}
