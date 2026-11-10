//! Which RPCs need which grant on their destination (`clientRpcPermissions.ts`), and the grants
//! a socket checks them against (`RpcPermissionGuard` / `commandPermissions.ts`). The socket
//! refuses a guarded request its environment's grants lack; controls ask [`permits`] with the
//! same table. The server still authorizes every request.

use serde_json::{Value, json};

use crate::rpc::RpcError;

pub const ORCHESTRATION_READ: &str = "orchestration:read";
pub const ORCHESTRATION_OPERATE: &str = "orchestration:operate";
pub const SETTINGS_WRITE: &str = "settings:write";
pub const PROVIDERS_MANAGE: &str = "providers:manage";
pub const ENVIRONMENT_MAINTAIN: &str = "environment:maintain";
pub const PREVIEW_OPERATE: &str = "preview:operate";
pub const DIAGNOSTICS_READ: &str = "diagnostics:read";
pub const TERMINAL_READ: &str = "terminal:read";
pub const TERMINAL_OPERATE: &str = "terminal:operate";
pub const SOURCE_CONTROL_WRITE: &str = "source-control:write";
pub const FILESYSTEM_READ: &str = "filesystem:read";
pub const FILESYSTEM_WRITE: &str = "filesystem:write";
pub const ACCESS_WRITE: &str = "access:write";

/// `CLIENT_GUARDED_RPC_SCOPES`.
const GUARDED: &[(&str, &str)] = &[
    ("provider.auth.removeMethod", PROVIDERS_MANAGE),
    ("preview.close", PREVIEW_OPERATE),
    // Stopping a server signals its processes, as closing a terminal does.
    ("preview.stopLocalServer", TERMINAL_OPERATE),
    ("server.runStorageCleanup", SETTINGS_WRITE),
    ("server.reclaimDatabaseSpace", ENVIRONMENT_MAINTAIN),
    ("server.checkOtlpEndpoint", SETTINGS_WRITE),
    ("server.listTextGeneratorModels", SETTINGS_WRITE),
    ("server.refreshProviders", ORCHESTRATION_READ),
    ("pullRequests.runAction", SOURCE_CONTROL_WRITE),
    ("pullRequests.update", SOURCE_CONTROL_WRITE),
    ("pullRequests.comment", SOURCE_CONTROL_WRITE),
    ("pullRequests.updateComment", SOURCE_CONTROL_WRITE),
    ("pullRequests.submitReview", SOURCE_CONTROL_WRITE),
    ("pullRequests.replyToThread", SOURCE_CONTROL_WRITE),
    ("pullRequests.setThreadResolution", SOURCE_CONTROL_WRITE),
    ("pullRequests.setReaction", SOURCE_CONTROL_WRITE),
    ("pullRequests.setFilesViewed", SOURCE_CONTROL_WRITE),
    ("pullRequests.requestReviewers", SOURCE_CONTROL_WRITE),
    ("pullRequests.setLabels", SOURCE_CONTROL_WRITE),
    ("pullRequests.generateReviewGuide", ORCHESTRATION_OPERATE),
    ("projects.createFromThread", ORCHESTRATION_OPERATE),
    ("sourceControl.cloneRepository", SOURCE_CONTROL_WRITE),
    ("sourceControl.publishRepository", SOURCE_CONTROL_WRITE),
    ("sourceControl.star", SOURCE_CONTROL_WRITE),
    ("sourceControl.signInStart", SETTINGS_WRITE),
    ("sourceControl.signInWait", SETTINGS_WRITE),
    ("sourceControl.signInCancel", SETTINGS_WRITE),
    ("projectClone.start", SOURCE_CONTROL_WRITE),
    ("projectClone.cancel", SOURCE_CONTROL_WRITE),
    ("projectClone.retry", SOURCE_CONTROL_WRITE),
    ("vcs.pull", SOURCE_CONTROL_WRITE),
    ("git.runStackedAction", SOURCE_CONTROL_WRITE),
    ("git.preparePullRequestThread", SOURCE_CONTROL_WRITE),
    ("vcs.createWorktree", SOURCE_CONTROL_WRITE),
    ("vcs.removeWorktree", SOURCE_CONTROL_WRITE),
    ("vcs.createRef", SOURCE_CONTROL_WRITE),
    ("vcs.switchRef", SOURCE_CONTROL_WRITE),
    ("vcs.init", SOURCE_CONTROL_WRITE),
    // Starts an agent session on the environment that can read its logs and database.
    ("server.prepareTriage", ORCHESTRATION_OPERATE),
    ("scheduledTasks.upsert", ORCHESTRATION_OPERATE),
    ("scheduledTasks.setEnabled", ORCHESTRATION_OPERATE),
    ("scheduledTasks.delete", ORCHESTRATION_OPERATE),
    ("scheduledTasks.runNow", ORCHESTRATION_OPERATE),
    ("scheduledTasks.rotateWebhookToken", ORCHESTRATION_OPERATE),
    ("orchestration.stopWorkflow", ORCHESTRATION_OPERATE),
    ("orchestration.forceStopThread", ORCHESTRATION_OPERATE),
    ("guestShare.create", ACCESS_WRITE),
    ("peerLinks.link", ACCESS_WRITE),
    ("peerLinks.unlink", ACCESS_WRITE),
    ("peerLinks.answerRequest", ACCESS_WRITE),
];

/// `clientRpcRequiredScopes`: `payload` is the request's input, `Null` when there is none yet.
pub fn required_scopes(method: &str, payload: &Value) -> Vec<&'static str> {
    let has_thread = || payload.get("threadId").is_some_and(|id| !id.is_null());
    let action = payload.get("action").and_then(Value::as_str);
    match method {
        "git.preparePullRequestThread"
            if payload.get("mode").and_then(Value::as_str) == Some("worktree") && has_thread() =>
        {
            vec![SOURCE_CONTROL_WRITE, ORCHESTRATION_OPERATE]
        }
        "git.runStackedAction" if matches!(action, Some("create_pr" | "commit_push_pr")) && has_thread() => {
            vec![SOURCE_CONTROL_WRITE, ORCHESTRATION_READ]
        }
        _ => GUARDED.iter().filter(|(guarded, _)| *guarded == method).map(|&(_, scope)| scope).collect(),
    }
}

/// The first scope `method` needs that `holds` lacks.
pub fn missing(method: &str, payload: &Value, holds: impl Fn(&str) -> bool) -> Option<&'static str> {
    required_scopes(method, payload).into_iter().find(|scope| !holds(scope))
}

/// `permissionAtom`: whether a control that sends `method` may be used, given the grants.
pub fn permits(method: &str, payload: &Value, holds: impl Fn(&str) -> bool) -> bool {
    missing(method, payload, holds).is_none()
}

/// Older servers grant these through a broader parent scope (`legacyParents` in `auth.ts`).
pub fn legacy_parent(scope: &str) -> Option<&'static str> {
    Some(match scope {
        FILESYSTEM_READ | DIAGNOSTICS_READ => ORCHESTRATION_READ,
        SETTINGS_WRITE | PROVIDERS_MANAGE | ENVIRONMENT_MAINTAIN | PREVIEW_OPERATE | SOURCE_CONTROL_WRITE
        | FILESYSTEM_WRITE => ORCHESTRATION_OPERATE,
        TERMINAL_READ => TERMINAL_OPERATE,
        _ => return None,
    })
}

/// The `EnvironmentAuthorizationError` a refused request fails with, as the web's guard builds it
/// (`authScopeRequiredResponse`).
pub fn denied(scope: &str) -> RpcError {
    RpcError::Failure(json!({
        "_tag": "EnvironmentAuthorizationError",
        "message": format!("This connection requires {scope}."),
        "requiredScope": legacy_parent(scope).unwrap_or(scope),
        "requiredPermission": scope,
    }))
}

/// This client's grants on a socket's environment, as its session check last answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Grants {
    /// Not answered yet: a guarded request waits for the answer, then is refused without one.
    #[default]
    Pending,
    /// The guarded scopes held there.
    Ready(Vec<&'static str>),
    /// The check failed: guarded requests are refused.
    Failed,
}

impl Grants {
    /// The guarded scopes `holds` grants.
    pub fn ready(holds: impl Fn(&str) -> bool) -> Self {
        let mut held: Vec<&'static str> =
            GUARDED.iter().map(|&(_, scope)| scope).filter(|scope| holds(scope)).collect();
        held.sort_unstable();
        held.dedup();
        Self::Ready(held)
    }

    /// `RpcPermissionGuard.authorize`: `None` while the grants load, else the missing scope.
    pub fn authorize(&self, method: &str, payload: &Value) -> Option<Result<(), &'static str>> {
        let scopes = required_scopes(method, payload);
        let Some(&first) = scopes.first() else { return Some(Ok(())) };
        match self {
            Self::Pending => None,
            Self::Failed => Some(Err(first)),
            Self::Ready(held) => Some(match scopes.into_iter().find(|scope| !held.contains(scope)) {
                Some(scope) => Err(scope),
                None => Ok(()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holding(scopes: &[&'static str]) -> Grants {
        Grants::ready(|scope| scopes.contains(&scope))
    }

    fn allowed(grants: &Grants, method: &str, payload: &Value) -> bool {
        grants.authorize(method, payload) == Some(Ok(()))
    }

    #[test]
    fn single_scope_methods_need_exactly_their_grant() {
        for (method, scope) in [
            ("server.refreshProviders", ORCHESTRATION_READ),
            ("preview.close", PREVIEW_OPERATE),
            ("provider.auth.removeMethod", PROVIDERS_MANAGE),
            ("server.runStorageCleanup", SETTINGS_WRITE),
            ("server.reclaimDatabaseSpace", ENVIRONMENT_MAINTAIN),
            ("server.checkOtlpEndpoint", SETTINGS_WRITE),
            ("scheduledTasks.delete", ORCHESTRATION_OPERATE),
            ("orchestration.stopWorkflow", ORCHESTRATION_OPERATE),
            ("preview.stopLocalServer", TERMINAL_OPERATE),
            ("server.prepareTriage", ORCHESTRATION_OPERATE),
            ("scheduledTasks.rotateWebhookToken", ORCHESTRATION_OPERATE),
            ("sourceControl.star", SOURCE_CONTROL_WRITE),
            ("peerLinks.link", ACCESS_WRITE),
            ("peerLinks.unlink", ACCESS_WRITE),
            ("peerLinks.answerRequest", ACCESS_WRITE),
        ] {
            let others = if scope == ORCHESTRATION_OPERATE { ORCHESTRATION_READ } else { ORCHESTRATION_OPERATE };
            assert_eq!(holding(&[others]).authorize(method, &Value::Null), Some(Err(scope)), "{method}");
            assert!(allowed(&holding(&[scope]), method, &Value::Null), "{method}");
            assert!(permits(method, &Value::Null, |held| held == scope), "{method}");
            assert!(!permits(method, &Value::Null, |_| false), "{method}");
        }
    }

    #[test]
    fn unguarded_methods_need_nothing_even_before_the_grants_load() {
        for grants in [Grants::Pending, Grants::Failed, holding(&[])] {
            assert!(allowed(&grants, "subscribeServerConfig", &Value::Null));
            assert!(allowed(&grants, "pullRequests.detail", &json!({})));
        }
    }

    #[test]
    fn requests_wait_while_pending_and_are_refused_once_the_check_fails() {
        assert_eq!(Grants::Pending.authorize("scheduledTasks.delete", &Value::Null), None);
        assert_eq!(Grants::Failed.authorize("scheduledTasks.delete", &Value::Null), Some(Err(ORCHESTRATION_OPERATE)));
    }

    #[test]
    fn source_control_alone_for_git_and_both_grants_to_attach_a_worktree_to_a_thread() {
        let write = holding(&[SOURCE_CONTROL_WRITE]);
        assert!(allowed(&write, "vcs.init", &Value::Null));
        let input = json!({"cwd": "/repo", "reference": "123", "mode": "worktree", "threadId": "thread"});
        assert_eq!(write.authorize("git.preparePullRequestThread", &input), Some(Err(ORCHESTRATION_OPERATE)));
        let mut without_thread = input.clone();
        without_thread.as_object_mut().unwrap().remove("threadId");
        assert!(allowed(&write, "git.preparePullRequestThread", &without_thread));
        let mut local = input.clone();
        local["mode"] = json!("local");
        assert!(allowed(&write, "git.preparePullRequestThread", &local));
        assert!(allowed(
            &holding(&[SOURCE_CONTROL_WRITE, ORCHESTRATION_OPERATE]),
            "git.preparePullRequestThread",
            &input
        ));
    }

    #[test]
    fn stacked_actions_need_thread_read_only_when_generating_a_pr_from_a_thread() {
        let write = holding(&[SOURCE_CONTROL_WRITE]);
        let both = holding(&[SOURCE_CONTROL_WRITE, ORCHESTRATION_READ]);
        for action in ["commit", "push", "commit_push", "create_pr", "commit_push_pr"] {
            let input = json!({"actionId": "action", "cwd": "/repo", "action": action});
            let mut with_thread = input.clone();
            with_thread["threadId"] = json!("thread");
            assert!(allowed(&write, "git.runStackedAction", &input), "{action}");
            let reads_thread = matches!(action, "create_pr" | "commit_push_pr");
            assert_eq!(allowed(&write, "git.runStackedAction", &with_thread), !reads_thread, "{action}");
            if reads_thread {
                assert_eq!(write.authorize("git.runStackedAction", &with_thread), Some(Err(ORCHESTRATION_READ)));
            }
            assert!(allowed(&both, "git.runStackedAction", &with_thread), "{action}");
        }
    }

    #[test]
    fn a_refusal_carries_the_permission_and_its_legacy_scope() {
        let RpcError::Failure(error) = denied(SOURCE_CONTROL_WRITE) else { panic!("expected a failure") };
        assert_eq!(error["_tag"], "EnvironmentAuthorizationError");
        assert_eq!(error["requiredPermission"], SOURCE_CONTROL_WRITE);
        assert_eq!(error["requiredScope"], ORCHESTRATION_OPERATE);
        assert_eq!(denied(ACCESS_WRITE).message(), "This connection requires access:write.");
        let RpcError::Failure(error) = denied(ACCESS_WRITE) else { panic!("expected a failure") };
        assert_eq!(error["requiredScope"], ACCESS_WRITE);
    }

    #[test]
    fn grants_keep_only_guarded_scopes() {
        assert_eq!(holding(&[DIAGNOSTICS_READ, ORCHESTRATION_READ]), Grants::Ready(vec![ORCHESTRATION_READ]));
    }
}
