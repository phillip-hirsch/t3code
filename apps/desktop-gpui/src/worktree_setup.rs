//! A new worktree's setup as the server streams it (`subscribeWorktreeSetup`), and where
//! its card sits in the timeline (`deriveMessagesTimelineRows`, `resolveVisibleWorktreeSetup`).

use std::rc::Rc;

use serde::Deserialize;

use crate::timeline::Row;

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub thread_id: String,
    pub phase: String,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub setup_script: Option<SetupScript>,
    #[serde(default)]
    pub stages: Vec<Stage>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub sequence: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SetupScript {
    pub name: String,
    pub command: String,
    pub terminal_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Stage {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub percent: Option<u8>,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub tail: Vec<String>,
}

impl Snapshot {
    /// `worktreeSetupAgentStarted`.
    pub fn agent_started(&self) -> bool {
        self.stages.iter().any(|s| s.id == "agent" && s.status == "done")
    }

    fn stage_failed(&self) -> bool {
        self.stages.iter().any(|s| s.status == "failed")
    }

    /// `headerLabel`.
    pub fn header_label(&self) -> &'static str {
        match self.phase.as_str() {
            "running" => "Setting up worktree…",
            "done" if self.stage_failed() => "Worktree ready, setup script failed",
            "done" => "Worktree ready",
            "failed" => "Worktree setup failed",
            _ => "Worktree setup cancelled",
        }
    }

    /// The collapsed summary's status (`CollapsedSummaryRow`).
    pub fn summary_failed(&self) -> bool {
        matches!(self.phase.as_str(), "failed" | "cancelled")
            || self.stages.iter().any(|s| s.id == "setup-script" && s.status == "failed")
    }
}

/// `worktreeSetupStageLabel`, or the setup script's own name.
pub fn stage_label<'a>(stage: &Stage, script: Option<&'a SetupScript>) -> std::borrow::Cow<'a, str> {
    match stage.id.as_str() {
        "setup-script" if script.is_some() => script.map(|s| s.name.as_str()).unwrap_or_default().into(),
        "fetch" => "Fetch base branch".into(),
        "checkout" => "Check out files".into(),
        "submodules" => "Init submodules".into(),
        "setup-script" => "Run setup script".into(),
        _ => "Start agent".into(),
    }
}

/// What trails a stage's label: its detail, "skipped", or the checkout's progress.
pub fn stage_trailing(stage: &Stage) -> Option<String> {
    match stage.status.as_str() {
        "pending" => None,
        "skipped" => Some(stage.detail.clone().unwrap_or_else(|| "skipped".into())),
        "running" if stage.id == "checkout" && stage.percent.is_some() => stage.percent.map(|p| format!("{p}%")),
        _ => stage.detail.clone(),
    }
}

/// `resolveVisibleWorktreeSetup` without a recorded snapshot: a finished setup gives way to the
/// turn it started unless a stage failed.
pub fn visible(snapshot: Option<&Snapshot>, turn_started: bool) -> Option<&Snapshot> {
    let snapshot = snapshot?;
    match snapshot.phase.as_str() {
        "running" => Some(snapshot),
        "done" if turn_started && !snapshot.stage_failed() => None,
        _ => Some(snapshot),
    }
}

/// Puts the card in the timeline: after the working row when setup owns it, else after the
/// first user message. Once the agent took over and its run started, the card folds to a
/// summary row (`embedded`), and a running one hides.
pub fn insert_row(rows: &mut Vec<Row>, snapshot: &Snapshot, run_started: bool) {
    let handed_off = snapshot.agent_started() && run_started;
    if handed_off && snapshot.phase == "running" {
        return;
    }
    let owns_working = !handed_off && matches!(snapshot.phase.as_str(), "running" | "done");
    let row = Row::WorktreeSetup { snapshot: Rc::new(snapshot.clone()), embedded: handed_off };
    let working = owns_working.then(|| rows.iter().position(|r| matches!(r, Row::Working { .. }))).flatten();
    match working {
        Some(index) => rows.insert(index + 1, row),
        None => {
            let first_user = rows.iter().position(|r| matches!(r, Row::User { .. }));
            let at = first_user.map_or(rows.len(), |index| index + 1);
            if owns_working {
                rows.splice(at..at, [Row::Working { started_at: Some(snapshot.started_at.clone().into()) }, row]);
            } else {
                rows.insert(at, row);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn snapshot(phase: &str, stages: serde_json::Value) -> Snapshot {
        serde_json::from_value(json!({"threadId": "t", "phase": phase, "startedAt": "2026-01-01T00:00:00Z",
            "stages": stages, "sequence": 1}))
        .unwrap()
    }

    fn stage(id: &str, status: &str) -> serde_json::Value {
        json!({"id": id, "status": status, "startedAt": null, "endedAt": null, "percent": null, "detail": null, "tail": []})
    }

    #[test]
    fn finished_setups_give_way_to_the_turn_unless_a_stage_failed() {
        let done = snapshot("done", json!([stage("setup-script", "done")]));
        assert!(visible(Some(&done), false).is_some());
        assert!(visible(Some(&done), true).is_none());
        let script_failed = snapshot("done", json!([stage("setup-script", "failed")]));
        assert_eq!(script_failed.header_label(), "Worktree ready, setup script failed");
        assert!(visible(Some(&script_failed), true).is_some());
        assert!(visible(Some(&snapshot("cancelled", json!([]))), true).is_some());
    }

    #[test]
    fn the_card_follows_the_first_message_and_owns_the_working_row() {
        let user = Row::User {
            id: "u".into(),
            text: "hi".into(),
            steer: None,
            grouped_with_previous: false,
            grouped_with_next: false,
            continues_steer: false,
            created_at: "".into(),
            revert: None,
            sent_by_t3code: false,
        };
        let running = snapshot("running", json!([stage("fetch", "running")]));
        let mut rows = vec![user.clone()];
        insert_row(&mut rows, &running, false);
        assert!(matches!(
            rows.as_slice(),
            [Row::User { .. }, Row::Working { .. }, Row::WorktreeSetup { embedded: false, .. }]
        ));
        let handed = snapshot("done", json!([stage("agent", "done")]));
        let mut rows = vec![user.clone(), Row::Working { started_at: None }];
        insert_row(&mut rows, &handed, true);
        assert!(matches!(
            rows.as_slice(),
            [Row::User { .. }, Row::WorktreeSetup { embedded: true, .. }, Row::Working { .. }]
        ));
        let mut rows = vec![user];
        insert_row(&mut rows, &snapshot("running", json!([stage("agent", "done")])), true);
        assert_eq!(rows.len(), 1);
    }
}
