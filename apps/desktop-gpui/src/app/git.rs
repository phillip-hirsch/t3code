//! The thread's git status (`subscribeVcsStatus`) and the git actions the
//! details card offers: Initialize Git, the quick action, Commit/Push/PR, Pull.
//! The decision logic mirrors `apps/web/src/components/GitActionsControl.logic.ts`.

use gpui_kit::{Context, Hsla, SharedString, Task, Window, rgb};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Target, Workspace, new_id};
use crate::rpc::{RpcError, StreamEvent};
use crate::theme::Tokens;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LineTotals {
    #[serde(default)]
    pub insertions: u64,
    #[serde(default)]
    pub deletions: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BranchChanges {
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(flatten)]
    pub totals: LineTotals,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProvider {
    #[serde(default)]
    pub kind: String,
}

/// `VcsStatusLocalResult`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VcsLocal {
    pub is_repo: bool,
    #[serde(default)]
    pub source_control_provider: Option<SourceControlProvider>,
    #[serde(default)]
    pub has_primary_remote: bool,
    #[serde(default)]
    pub is_default_ref: bool,
    #[serde(default)]
    pub ref_name: Option<String>,
    #[serde(default)]
    pub has_working_tree_changes: bool,
    #[serde(default)]
    pub working_tree: LineTotals,
    #[serde(default)]
    pub branch_changes: Option<BranchChanges>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRequest {
    pub number: u64,
    pub url: String,
    pub state: String,
}

/// `VcsStatusRemoteResult`; a missing remote part reads as all zeros (`mergeGitStatusParts`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VcsRemote {
    #[serde(default)]
    pub has_upstream: bool,
    #[serde(default)]
    pub ahead_count: u64,
    #[serde(default)]
    pub behind_count: u64,
    #[serde(default)]
    pub ahead_of_default_count: Option<u64>,
    #[serde(default)]
    pub pr: Option<ChangeRequest>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VcsStatus {
    pub local: VcsLocal,
    pub remote: VcsRemote,
}

impl VcsStatus {
    /// Folds one `VcsStatusStreamEvent` into the current status (`applyVcsStatusStreamEvent`).
    pub fn apply(current: Option<Self>, event: &Value) -> Option<Self> {
        let local = || VcsLocal::deserialize(&event["local"]).ok();
        let remote = || VcsRemote::deserialize(&event["remote"]).unwrap_or_default();
        match event.get("_tag").and_then(Value::as_str)? {
            "snapshot" => Some(Self { local: local()?, remote: remote() }),
            "localUpdated" => Some(Self { local: local()?, remote: current.map(|c| c.remote).unwrap_or_default() }),
            "remoteUpdated" => {
                let local = current.map(|c| c.local).unwrap_or(VcsLocal { is_repo: true, ..VcsLocal::default() });
                Some(Self { local, remote: remote() })
            }
            _ => current,
        }
    }

    fn has_open_pr(&self) -> bool {
        self.remote.pr.as_ref().is_some_and(|pr| pr.state == "open")
    }

    /// `changeRequestTerminologyFor(...).shortLabel`.
    pub fn pr_label(&self) -> &'static str {
        match self.local.source_control_provider.as_ref().map(|p| p.kind.as_str()) {
            Some("gitlab") => "MR",
            Some("github" | "gitcafe" | "forgejo" | "bitbucket" | "azure-devops") => "PR",
            _ => "change request",
        }
    }
}

/// The Changes row's counts and the ref they compare against (`resolveChangesTotals`):
/// the branch total, unless it is empty and the working tree is not.
pub fn changes_totals(status: &VcsStatus) -> (u64, u64, Option<&str>) {
    let tree = &status.local.working_tree;
    let tree_dirty = tree.insertions > 0 || tree.deletions > 0;
    match &status.local.branch_changes {
        Some(branch) if branch.totals.insertions > 0 || branch.totals.deletions > 0 || !tree_dirty => {
            (branch.totals.insertions, branch.totals.deletions, branch.base_ref.as_deref())
        }
        _ => (tree.insertions, tree.deletions, None),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum QuickKind {
    Run(&'static str),
    Pull,
    Publish,
    Hint(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuickAction {
    pub label: String,
    pub kind: QuickKind,
}

impl QuickAction {
    pub fn disabled(&self) -> bool {
        matches!(self.kind, QuickKind::Hint(_))
    }

    /// `GitQuickActionIcon`: the PR glyph for anything that opens one.
    pub fn icon(&self) -> &'static str {
        match &self.kind {
            QuickKind::Run("commit_push_pr" | "create_pr") => "icons/app/github.svg",
            QuickKind::Run("commit") => "icons/git-commit-horizontal.svg",
            QuickKind::Run(_) | QuickKind::Publish => "icons/cloud-upload.svg",
            QuickKind::Pull => "icons/cloud-download.svg",
            QuickKind::Hint(_) if self.label == "Commit" => "icons/git-commit-horizontal.svg",
            QuickKind::Hint(_) => "icons/info.svg",
        }
    }
}

fn hint(label: &str, text: &str) -> QuickAction {
    QuickAction { label: label.into(), kind: QuickKind::Hint(text.into()) }
}

fn run(label: String, action: &'static str) -> QuickAction {
    QuickAction { label, kind: QuickKind::Run(action) }
}

/// `resolveQuickAction`.
pub fn quick_action(status: Option<&VcsStatus>, busy: bool) -> QuickAction {
    if busy {
        return hint("Commit", "Git action in progress.");
    }
    let Some(status) = status else { return hint("Commit", "Git status is unavailable.") };
    let (local, remote) = (&status.local, &status.remote);
    let pr = status.pr_label();
    let has_open_pr = status.has_open_pr();
    let ahead = remote.ahead_count > 0;
    let behind = remote.behind_count > 0;
    let default_delta = remote.ahead_of_default_count.unwrap_or(remote.ahead_count) > 0;
    let up_to_date = "Branch is up to date. No action needed.";
    if local.ref_name.is_none() {
        let singular = if pr == "change request" { "change request".to_owned() } else { "pull request".to_owned() };
        let singular = if pr == "MR" { "merge request".to_owned() } else { singular };
        return hint("Commit", &format!("Create and checkout a ref before pushing or opening a {singular}."));
    }
    if local.has_working_tree_changes {
        if behind || (!remote.has_upstream && !local.has_primary_remote) {
            return run("Commit".into(), "commit");
        }
        if has_open_pr || local.is_default_ref {
            return run("Commit & push".into(), "commit_push");
        }
        return run(format!("Commit, push & {pr}"), "commit_push_pr");
    }
    if !remote.has_upstream {
        if !local.has_primary_remote {
            return QuickAction { label: "Publish repository".into(), kind: QuickKind::Publish };
        }
        if !ahead {
            return if has_open_pr { hint("Commit", up_to_date) } else { hint("Push", "No local commits to push.") };
        }
        if has_open_pr || local.is_default_ref {
            return run("Push".into(), if local.is_default_ref { "commit_push" } else { "push" });
        }
        return run(format!("Push & create {pr}"), "create_pr");
    }
    if ahead && behind {
        return hint("Sync ref", "Branch has diverged from upstream. Rebase/merge first.");
    }
    if behind {
        return QuickAction { label: "Pull".into(), kind: QuickKind::Pull };
    }
    if ahead {
        if has_open_pr || local.is_default_ref {
            return run("Push".into(), if local.is_default_ref { "commit_push" } else { "push" });
        }
        return run(format!("Push & create {pr}"), "create_pr");
    }
    if has_open_pr {
        return hint("Commit", up_to_date);
    }
    if default_delta && !local.is_default_ref {
        return run(format!("Create {pr}"), "create_pr");
    }
    hint("Commit", up_to_date)
}

/// One entry of the quick action's menu (`buildMenuItems`): label, icon, enabled, action.
pub struct MenuAction {
    pub label: String,
    pub icon: &'static str,
    pub enabled: bool,
    pub action: &'static str,
}

/// `buildMenuItems`.
pub fn menu_actions(status: &VcsStatus, busy: bool) -> Vec<MenuAction> {
    let (local, remote) = (&status.local, &status.remote);
    let has_branch = local.ref_name.is_some();
    let behind = remote.behind_count > 0;
    let has_open_pr = status.has_open_pr();
    let default_delta = remote.ahead_of_default_count.unwrap_or(remote.ahead_count) > 0;
    let can_reach_remote = remote.has_upstream || local.has_primary_remote;
    let commit = MenuAction {
        label: "Commit".into(),
        icon: "icons/git-commit-horizontal.svg",
        enabled: !busy && local.has_working_tree_changes,
        action: "commit",
    };
    if !local.has_primary_remote {
        return vec![commit];
    }
    let push = MenuAction {
        label: "Push".into(),
        icon: "icons/cloud-upload.svg",
        enabled: !busy && has_branch && !behind && remote.ahead_count > 0 && can_reach_remote,
        action: "push",
    };
    if has_open_pr {
        return vec![commit, push];
    }
    let create_pr = MenuAction {
        label: format!("Create {}", status.pr_label()),
        icon: "icons/app/github.svg",
        enabled: !busy && has_branch && default_delta && !behind && can_reach_remote,
        action: "create_pr",
    };
    vec![commit, push, create_pr]
}

/// `diffColorScheme`, which `index.css` applies through `data-diff-color-scheme`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiffColorScheme {
    #[default]
    RedGreen,
    BlueOrange,
}

impl DiffColorScheme {
    pub fn from_settings(settings: &crate::settings::Settings) -> Self {
        match settings.get("diffColorScheme").as_str() {
            Some("blue-orange") => Self::BlueOrange,
            _ => Self::RedGreen,
        }
    }
}

/// `--diff-addition-foreground`: emerald 400 dark, 700 light; blue 400 / 600 when blue-orange.
pub fn addition_color(tokens: &Tokens, scheme: DiffColorScheme) -> Hsla {
    rgb(match (scheme, tokens.dark) {
        (DiffColorScheme::RedGreen, true) => 0x00d492,
        (DiffColorScheme::RedGreen, false) => 0x007a55,
        (DiffColorScheme::BlueOrange, true) => 0x51a2ff,
        (DiffColorScheme::BlueOrange, false) => 0x155dfc,
    })
    .into()
}

/// `--diff-deletion-foreground`: red 400 dark, 700 light; orange 400 / 600 when blue-orange.
pub fn deletion_color(tokens: &Tokens, scheme: DiffColorScheme) -> Hsla {
    rgb(match (scheme, tokens.dark) {
        (DiffColorScheme::RedGreen, true) => 0xff6467,
        (DiffColorScheme::RedGreen, false) => 0xc10007,
        (DiffColorScheme::BlueOrange, true) => 0xff8904,
        (DiffColorScheme::BlueOrange, false) => 0xf54900,
    })
    .into()
}

/// `Intl.NumberFormat("en-US")` for a count.
pub fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// The open target's status subscription; it follows the git cwd.
#[derive(Default)]
pub(super) struct VcsWatch {
    pub cwd: Option<String>,
    pub status: Option<VcsStatus>,
    /// The label of the git action in flight, as its progress reports it.
    pub running: Option<SharedString>,
    task: Option<Task<()>>,
}

impl Workspace {
    /// `gitCwd`: the thread's worktree, else its project root.
    pub(super) fn git_cwd(&self) -> Option<String> {
        let worktree = match self.target.as_ref()? {
            Target::Thread(id) => self
                .shell
                .threads
                .iter()
                .find(|thread| &thread.id == id)
                .and_then(|thread| thread.worktree_path.clone())
                .or_else(|| self.open_projection().and_then(|p| p.thread.worktree_path.clone())),
            Target::Draft { .. } => None,
        };
        worktree.or_else(|| self.target_project().map(|project| project.workspace_root.clone()))
    }

    /// Keeps one `subscribeVcsStatus` stream on the current cwd; reconnects resubscribe.
    pub(super) fn sync_vcs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.git_cwd();
        if cwd == self.vcs.cwd && (self.vcs.task.is_some() || cwd.is_none()) {
            return;
        }
        if cwd != self.vcs.cwd {
            self.vcs.status = None;
        }
        self.vcs.cwd = cwd.clone();
        self.vcs.task = None;
        let (Some(cwd), Some(client)) = (cwd, self.client()) else { return };
        let stream = client.stream("subscribeVcsStatus", json!({"cwd": cwd}));
        self.vcs.task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = stream.next().await {
                let mut events = vec![first];
                events.extend(std::iter::from_fn(|| stream.try_next()));
                let ended = this.update(cx, |this, cx| {
                    let mut ended = false;
                    for event in events {
                        match event {
                            StreamEvent::Item(value) if this.vcs.cwd.as_deref() == Some(cwd.as_str()) => {
                                this.vcs.status = VcsStatus::apply(this.vcs.status.take(), &value);
                            }
                            StreamEvent::Item(_) => {}
                            StreamEvent::End(result) => {
                                ended = true;
                                if let Err(error) = result {
                                    if !matches!(error, RpcError::Disconnected(_)) {
                                        log::warn!("git status stream ended: {error}");
                                    }
                                }
                            }
                        }
                    }
                    this.observe_proactive_panels(cx);
                    cx.notify();
                    ended
                });
                if ended.unwrap_or(true) {
                    break;
                }
            }
            // Dropped, so the next render resubscribes once the socket is back.
            let _ = this.update(cx, |this, _| this.vcs.task = None);
        }));
    }

    /// `git.runStackedAction` with the server writing the commit message, as the quick action does.
    pub(super) fn run_git_action(
        &mut self,
        action: &'static str,
        commit_message: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let (Some(client), Some(cwd)) = (self.client(), self.vcs.cwd.clone()) else { return };
        if self.vcs.running.is_some() {
            return;
        }
        let mut input = json!({"actionId": new_id(), "cwd": cwd, "action": action});
        if let Some(message) = commit_message {
            input["commitMessage"] = message.into();
        }
        if let Some(Target::Thread(id)) = &self.target {
            input["threadId"] = id.clone().into();
        }
        if let Some(project) = self.target_project() {
            input["projectId"] = project.id.clone().into();
        }
        self.vcs.running = Some("Starting source control action...".into());
        let stream = client.stream("git.runStackedAction", input);
        cx.spawn(async move |this, cx| {
            let mut outcome: Result<Option<String>, String> = Ok(None);
            while let Some(event) = stream.next().await {
                match event {
                    StreamEvent::Item(value) => match value.get("kind").and_then(Value::as_str) {
                        Some("phase_started") => {
                            let label = value["label"].as_str().unwrap_or_default().to_owned();
                            let _ = this.update(cx, |this, cx| {
                                this.vcs.running = Some(label.into());
                                cx.notify();
                            });
                        }
                        Some("action_failed") => {
                            outcome = Err(value["message"].as_str().unwrap_or("Git action failed.").to_owned())
                        }
                        Some("action_finished") => {
                            let toast = &value["result"]["toast"];
                            outcome = Ok(toast["title"].as_str().map(str::to_owned));
                        }
                        _ => {}
                    },
                    StreamEvent::End(Err(error)) => outcome = Err(error.to_string()),
                    StreamEvent::End(Ok(())) => break,
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.vcs.running = None;
                if let Err(error) = outcome {
                    this.error = Some(error.into());
                }
                this.refresh_vcs(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// A one-shot git RPC (`vcs.init`, `vcs.pull`) on the current cwd.
    pub(super) fn run_vcs_request(&mut self, method: &'static str, label: &'static str, cx: &mut Context<Self>) {
        let (Some(client), Some(cwd)) = (self.client(), self.vcs.cwd.clone()) else { return };
        if self.vcs.running.is_some() {
            return;
        }
        self.vcs.running = Some(label.into());
        cx.spawn(async move |this, cx| {
            let result = client.request(method, json!({"cwd": cwd})).await;
            let _ = this.update(cx, |this, cx| {
                this.vcs.running = None;
                if let Err(error) = result {
                    this.error = Some(error.to_string().into());
                }
                this.refresh_vcs(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// `vcs.refreshStatus` after a git operation, so the card does not wait on the watcher.
    pub(super) fn refresh_vcs(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(cwd)) = (self.client(), self.vcs.cwd.clone()) else { return };
        cx.spawn(async move |this, cx| {
            let Ok(value) = client.request("vcs.refreshStatus", json!({"cwd": cwd})).await else { return };
            let _ = this.update(cx, |this, cx| {
                if this.vcs.cwd.as_deref() != Some(cwd.as_str()) {
                    return;
                }
                if let Ok(local) = VcsLocal::deserialize(&value) {
                    let remote = VcsRemote::deserialize(&value).unwrap_or_default();
                    this.vcs.status = Some(VcsStatus { local, remote });
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(local: Value, remote: Value) -> VcsStatus {
        VcsStatus::apply(None, &json!({"_tag": "snapshot", "local": local, "remote": remote})).unwrap()
    }

    fn local(changes: bool, extra: Value) -> Value {
        let mut local = json!({
            "isRepo": true, "hasPrimaryRemote": true, "isDefaultRef": false, "refName": "feature",
            "hasWorkingTreeChanges": changes,
            "workingTree": {"files": [], "insertions": 0, "deletions": 0},
            "sourceControlProvider": {"kind": "github", "name": "GitHub", "baseUrl": "https://github.com"},
        });
        local.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        local
    }

    #[test]
    fn quick_action_follows_the_web() {
        let label = |s: &VcsStatus| quick_action(Some(s), false).label;
        assert_eq!(label(&status(local(true, json!({})), Value::Null)), "Commit, push & PR");
        assert_eq!(
            label(&status(local(true, json!({"isDefaultRef": true})), json!({"hasUpstream": true}))),
            "Commit & push"
        );
        assert_eq!(label(&status(local(false, json!({})), json!({"hasUpstream": false}))), "Push");
        assert!(quick_action(Some(&status(local(false, json!({})), Value::Null)), false).disabled());
        assert_eq!(
            label(&status(local(false, json!({})), json!({"hasUpstream": true, "aheadCount": 2}))),
            "Push & create PR"
        );
        assert_eq!(
            quick_action(Some(&status(local(false, json!({})), json!({"hasUpstream": true, "behindCount": 1}))), false)
                .kind,
            QuickKind::Pull
        );
        assert_eq!(
            label(&status(local(false, json!({"hasPrimaryRemote": false})), json!({"hasUpstream": false}))),
            "Publish repository"
        );
        assert_eq!(
            label(&status(
                local(false, json!({"sourceControlProvider": {"kind": "gitlab"}})),
                json!({"hasUpstream": true, "aheadOfDefaultCount": 3})
            )),
            "Create MR"
        );
        assert!(quick_action(None, false).disabled());
    }

    #[test]
    fn menu_hides_pr_when_one_is_open_and_push_needs_commits() {
        let open = status(
            local(true, json!({})),
            json!({"hasUpstream": true, "pr": {"number": 1, "title": "t", "url": "u", "baseRef": "main", "headRef": "f", "state": "open"}}),
        );
        let items = menu_actions(&open, false);
        assert_eq!(items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>(), ["Commit", "Push"]);
        assert!(items[0].enabled && !items[1].enabled);
        let no_remote = status(local(false, json!({"hasPrimaryRemote": false})), Value::Null);
        assert_eq!(menu_actions(&no_remote, false).len(), 1);
    }

    #[test]
    fn changes_prefer_the_branch_unless_only_the_tree_is_dirty() {
        let tree = json!({"files": [], "insertions": 4, "deletions": 1});
        let clean_branch = status(
            local(
                true,
                json!({"workingTree": tree, "branchChanges": {"baseRef": "origin/main", "insertions": 0, "deletions": 0}}),
            ),
            Value::Null,
        );
        assert_eq!(changes_totals(&clean_branch), (4, 1, None));
        let branch = status(
            local(
                true,
                json!({"workingTree": tree, "branchChanges": {"baseRef": "origin/main", "insertions": 9, "deletions": 2}}),
            ),
            Value::Null,
        );
        assert_eq!(changes_totals(&branch), (9, 2, Some("origin/main")));
    }

    #[test]
    fn stream_updates_keep_the_other_half() {
        let first = status(local(false, json!({})), json!({"hasUpstream": true, "aheadCount": 2}));
        let next =
            VcsStatus::apply(Some(first), &json!({"_tag": "localUpdated", "local": local(true, json!({}))})).unwrap();
        assert!(next.local.has_working_tree_changes);
        assert_eq!(next.remote.ahead_count, 2);
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(7), "7");
        assert_eq!(group_thousands(11_314), "11,314");
        assert_eq!(group_thousands(1_000_000), "1,000,000");
    }
}
