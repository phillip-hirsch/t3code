//! `firstRun.logic.ts`: whether a launch opens the Welcome page, the app, or waits for the
//! evidence that tells a fresh workspace from one that is already in use.

use serde::Deserialize;

use crate::model::{ProjectShell, ThreadShell};
use crate::project_groups::normalize_path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum Decision {
    Pending,
    App,
    Wizard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) struct GateState {
    pub decision: Decision,
    pub stalled: bool,
}

impl GateState {
    /// `useState` in `FirstRunGate`: a finished setup opens the app at once.
    pub fn initial(completed: bool) -> Self {
        Self { decision: if completed { Decision::App } else { Decision::Pending }, stalled: false }
    }

    /// `transitionFirstRunGateState` with new evidence: pending evidence never moves a decision.
    pub fn evidence(self, decision: Decision) -> Self {
        if decision == Decision::Pending || self.decision == decision {
            return self;
        }
        Self { decision, stalled: false }
    }

    /// `transitionFirstRunGateState` on the timeout: a decision still pending shows recovery.
    pub fn timeout(self) -> Self {
        if self.decision == Decision::Pending { Self { stalled: true, ..self } } else { self }
    }
}

/// The server's `welcome` lifecycle event (`ServerLifecycleWelcomePayload`), as far as the gate reads it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(in crate::app) struct Welcome {
    #[serde(default)]
    pub bootstrap_status: Option<String>,
    #[serde(default)]
    pub bootstrap_project_id: Option<String>,
    #[serde(default)]
    pub bootstrap_thread_id: Option<String>,
    #[serde(default)]
    pub bootstrap_project_created: Option<bool>,
    #[serde(default)]
    pub bootstrap_thread_created: Option<bool>,
}

impl Welcome {
    /// `isFirstRunWorkspaceProvenanceAuthoritative`: an older server's welcome has no status.
    pub fn provenance_authoritative(welcome: Option<&Self>) -> bool {
        welcome.is_some_and(|welcome| welcome.bootstrap_status.as_deref() != Some("pending"))
    }
}

/// `isFreshFirstRunWorkspace`: nothing, or only the unused project and thread the server
/// created from its cwd at startup, which `npx t3` users still count as fresh.
pub(in crate::app) fn is_fresh_workspace(
    primary_environment_id: Option<&str>,
    server_cwd: Option<&str>,
    welcome: Option<&Welcome>,
    projects: &[ProjectShell],
    threads: &[ThreadShell],
) -> bool {
    if projects.len() > 1 || threads.len() > 1 {
        return false;
    }
    let welcome = welcome.cloned().unwrap_or_default();
    let project = projects.first();
    if let Some(project) = project {
        let created_here = welcome.bootstrap_project_created == Some(true)
            && welcome.bootstrap_project_id.as_deref() == Some(project.id.as_str())
            && primary_environment_id == Some(&*project.environment_id)
            && server_cwd.is_some_and(|cwd| normalize_path(&project.workspace_root) == normalize_path(cwd));
        if !created_here {
            return false;
        }
    }
    let Some(thread) = threads.first() else { return true };
    project.is_some_and(|project| {
        welcome.bootstrap_thread_created == Some(true)
            && welcome.bootstrap_thread_id.as_deref() == Some(thread.id.as_str())
            && primary_environment_id == Some(&*thread.environment_id)
            && thread.project_id == project.id
            && unused(thread)
    })
}

/// No run, no user message and no runtime (`latestRun`, `latestUserMessageAt`, `runtime`).
fn unused(thread: &ThreadShell) -> bool {
    thread.latest_run_id.is_none()
        && thread.active_provider_thread_id.is_none()
        && thread.latest_user_message_at.is_none()
        && thread.pending_background_tasks.is_empty()
}

/// `FirstRunDecisionInput`.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::app) struct Evidence {
    pub enabled: bool,
    pub hydrated: bool,
    pub completed: bool,
    /// Every connected environment's shell snapshot arrived.
    pub bootstrapped: bool,
    /// The primary environment's shell is live.
    pub authoritative: bool,
    /// Every environment that lists a project or thread is live.
    pub workspace_authoritative: bool,
    pub workspace_provenance_authoritative: bool,
    pub catalog_ready: bool,
    pub server_config_available: bool,
    pub workspace_fresh: bool,
    pub project_count: usize,
    pub thread_count: usize,
}

/// `resolveFirstRunDecision`: the decision, and whether to record setup as finished.
pub(in crate::app) fn resolve(input: &Evidence) -> (Decision, bool) {
    if !input.enabled || (input.hydrated && input.completed) {
        return (Decision::App, false);
    }
    if !input.hydrated {
        return (Decision::Pending, false);
    }
    if input.project_count > 1 || input.thread_count > 1 {
        let confirmed = input.bootstrapped
            && input.authoritative
            && input.workspace_authoritative
            && input.catalog_ready
            && input.server_config_available;
        return (Decision::App, confirmed);
    }
    if !input.bootstrapped
        || !input.authoritative
        || !input.workspace_provenance_authoritative
        || !input.catalog_ready
        || !input.server_config_available
    {
        return (Decision::Pending, false);
    }
    if input.workspace_fresh { (Decision::Wizard, false) } else { (Decision::App, input.workspace_authoritative) }
}

/// The gate's decision for this launch, or `None` while it waits, without the timeout, for the
/// local server. With the local environment off there is no primary to wait for, so the web's
/// hosted gate applies (`resolveHostedFirstRunDecision`).
pub(in crate::app) fn decide(local_off: bool, input: &Evidence) -> Option<(Decision, bool)> {
    if local_off {
        return Some((if input.hydrated { Decision::App } else { Decision::Pending }, false));
    }
    if !input.enabled && !input.completed {
        return None;
    }
    Some(resolve(input))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fresh() -> Evidence {
        Evidence {
            enabled: true,
            hydrated: true,
            completed: false,
            bootstrapped: true,
            authoritative: true,
            workspace_authoritative: true,
            workspace_provenance_authoritative: true,
            catalog_ready: true,
            server_config_available: true,
            workspace_fresh: true,
            project_count: 1,
            thread_count: 1,
        }
    }

    #[test]
    fn a_confirmed_fresh_workspace_opens_the_welcome_page() {
        assert_eq!(resolve(&fresh()), (Decision::Wizard, false));
    }

    #[test]
    fn cached_projects_open_the_app_without_recording_setup_until_the_server_confirms_them() {
        let cached = Evidence { authoritative: false, project_count: 3, workspace_fresh: false, ..fresh() };
        assert_eq!(resolve(&cached), (Decision::App, false));
        let confirmed = Evidence { project_count: 3, workspace_fresh: false, ..fresh() };
        assert_eq!(resolve(&confirmed), (Decision::App, true));
    }

    #[test]
    fn a_pending_welcome_waits_and_a_finished_one_decides() {
        let pending = Welcome { bootstrap_status: Some("pending".into()), ..Welcome::default() };
        let waiting = Evidence {
            workspace_provenance_authoritative: Welcome::provenance_authoritative(Some(&pending)),
            ..fresh()
        };
        assert_eq!(resolve(&waiting), (Decision::Pending, false));
        let complete = Welcome { bootstrap_status: Some("complete".into()), ..Welcome::default() };
        let decided = Evidence {
            workspace_provenance_authoritative: Welcome::provenance_authoritative(Some(&complete)),
            ..fresh()
        };
        assert_eq!(resolve(&decided), (Decision::Wizard, false));
        // An older server's welcome has no status; no welcome at all proves nothing.
        assert!(Welcome::provenance_authoritative(Some(&Welcome::default())));
        assert!(!Welcome::provenance_authoritative(None));
    }

    #[test]
    fn finished_setup_opens_the_app_without_waiting_for_the_server() {
        let input = Evidence {
            authoritative: false,
            bootstrapped: false,
            completed: true,
            server_config_available: false,
            ..fresh()
        };
        assert_eq!(resolve(&input), (Decision::App, false));
        assert_eq!(GateState::initial(true).decision, Decision::App);
        assert_eq!(GateState::initial(false).decision, Decision::Pending);
    }

    #[test]
    fn with_the_local_environment_off_the_app_opens_without_a_primary() {
        let no_primary = Evidence { hydrated: true, ..Evidence::default() };
        // `resolveHostedFirstRunDecision`: nothing to wait for, and setup is not recorded.
        assert_eq!(decide(true, &no_primary), Some((Decision::App, false)));
        assert_eq!(decide(true, &Evidence { completed: true, ..no_primary }), Some((Decision::App, false)));
        assert_eq!(decide(true, &Evidence::default()), Some((Decision::Pending, false)));
        // An enabled local server that has not answered yet is waited on.
        assert_eq!(decide(false, &no_primary), None);
        assert_eq!(decide(false, &Evidence { completed: true, ..no_primary }), Some((Decision::App, false)));
        assert_eq!(decide(false, &fresh()), Some((Decision::Wizard, false)));
    }

    #[test]
    fn a_used_workspace_records_setup_only_once_every_environment_is_live() {
        let used = Evidence { workspace_fresh: false, project_count: 1, thread_count: 0, ..fresh() };
        assert_eq!(resolve(&used), (Decision::App, true));
        let stale = Evidence { workspace_authoritative: false, ..used };
        assert_eq!(resolve(&stale), (Decision::App, false));
    }

    #[test]
    fn the_gate_stalls_only_while_pending_and_follows_changed_evidence() {
        let pending = GateState::initial(false);
        assert_eq!(pending.timeout(), GateState { decision: Decision::Pending, stalled: true });
        let app = GateState { decision: Decision::App, stalled: false };
        assert_eq!(app.timeout(), app);
        // A stalled gate recovers as soon as evidence decides.
        assert_eq!(
            pending.timeout().evidence(Decision::Wizard),
            GateState { decision: Decision::Wizard, stalled: false }
        );
        let wizard = resolve(&fresh()).0;
        assert_eq!(app.evidence(wizard).decision, Decision::Wizard);
        let used = resolve(&Evidence { workspace_fresh: false, ..fresh() }).0;
        assert_eq!(app.evidence(wizard).evidence(used).decision, Decision::App);
        for decision in [Decision::Pending, Decision::App, Decision::Wizard] {
            let state = GateState { decision, stalled: false };
            assert_eq!(state.evidence(decision), state);
            assert_eq!(state.evidence(Decision::Pending), state, "pending evidence never moves {decision:?}");
        }
    }

    const PRIMARY: &str = "primary-environment";

    fn project(root: &str) -> ProjectShell {
        serde_json::from_value(json!({
            "id": "bootstrap-project", "title": "current", "workspaceRoot": root,
        }))
        .map(|mut project: ProjectShell| {
            project.environment_id = PRIMARY.into();
            project
        })
        .expect("project shell")
    }

    fn thread(extra: serde_json::Value) -> ThreadShell {
        let mut value = json!({
            "id": "bootstrap-thread", "projectId": "bootstrap-project", "title": "New thread",
            "status": "idle", "updatedAt": "2026-08-23T12:00:00.000Z", "createdAt": "2026-08-23T12:00:00.000Z",
        });
        if let (Some(base), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            base.extend(extra.clone());
        }
        let mut thread: ThreadShell = serde_json::from_value(value).expect("thread shell");
        thread.environment_id = PRIMARY.into();
        thread
    }

    fn created() -> Welcome {
        Welcome {
            bootstrap_status: None,
            bootstrap_project_id: Some("bootstrap-project".into()),
            bootstrap_thread_id: Some("bootstrap-thread".into()),
            bootstrap_project_created: Some(true),
            bootstrap_thread_created: Some(true),
        }
    }

    fn is_fresh(cwd: &str, welcome: &Welcome, projects: &[ProjectShell], threads: &[ThreadShell]) -> bool {
        is_fresh_workspace(Some(PRIMARY), Some(cwd), Some(welcome), projects, threads)
    }

    #[test]
    fn only_the_unused_project_and_thread_created_from_the_server_cwd_are_fresh() {
        let projects = [project("/projects/current")];
        let threads = [thread(json!({}))];
        assert!(is_fresh("/projects/current/", &created(), &projects, &threads));
        assert!(is_fresh_workspace(Some(PRIMARY), None, None, &[], &[]), "an empty workspace is fresh");
        // Startup reused a project and thread that were already there.
        let reused =
            Welcome { bootstrap_project_created: Some(false), bootstrap_thread_created: Some(false), ..created() };
        assert!(!is_fresh("/projects/current", &reused, &projects, &threads));
        // An older server omits whether it created them.
        let older = Welcome { bootstrap_project_created: None, bootstrap_thread_created: None, ..created() };
        assert!(!is_fresh("/projects/current", &older, &projects, &threads));
        // Without a welcome nothing proves the project came from startup.
        assert!(!is_fresh_workspace(Some(PRIMARY), Some("/projects/current"), None, &projects, &[]));
        // Another environment's project of the same path.
        assert!(!is_fresh_workspace(Some("other"), Some("/projects/current"), Some(&created()), &projects, &[]));
    }

    #[test]
    fn windows_paths_compare_normalized() {
        let projects = [project("c:/projects/current")];
        assert!(is_fresh("C:\\Projects\\Current\\", &created(), &projects, &[thread(json!({}))]));
    }

    #[test]
    fn a_bootstrap_thread_someone_used_is_not_fresh() {
        let projects = [project("/projects/current")];
        for used in [
            json!({"latestUserMessageAt": "2026-08-23T12:00:00.000Z"}),
            json!({"latestRunId": "run-1"}),
            json!({"activeProviderThreadId": "provider-thread"}),
        ] {
            assert!(!is_fresh("/projects/current", &created(), &projects, &[thread(used.clone())]), "{used}");
        }
        // A thread with no project is never the bootstrap thread.
        assert!(!is_fresh("/projects/current", &created(), &[], &[thread(json!({}))]));
    }
}
