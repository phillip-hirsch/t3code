//! The thread's git status (`subscribeVcsStatus`) and the git actions the
//! details card offers: Initialize Git, the quick action, Commit/Push/PR, Pull.
//! The decision logic mirrors `apps/web/src/components/GitActionsControl.logic.ts`.

use std::collections::HashMap;

use gpui_kit::{Context, Hsla, SharedString, Task, Window, rgb};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Target, Workspace, new_id};
use crate::model::ScopedId;
use crate::rpc::{RpcClient, RpcError, StreamEvent};
use crate::theme::Tokens;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LineTotals {
    #[serde(default)]
    pub insertions: u64,
    #[serde(default)]
    pub deletions: u64,
}

/// One `workingTree.files` entry; `path` is relative to the worktree root.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkingTreeFile {
    pub path: String,
    #[serde(default)]
    pub insertions: u64,
    #[serde(default)]
    pub deletions: u64,
}

/// `workingTree`: the uncommitted changes and their totals.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkingTree {
    /// The absolute worktree root every `files[].path` is relative to; absent when the server
    /// could not resolve it.
    #[serde(default)]
    pub root_path: Option<String>,
    #[serde(default)]
    pub files: Vec<WorkingTreeFile>,
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
    /// The host's display name ("GitHub Self-Hosted"), when the server names it.
    #[serde(default)]
    pub name: Option<String>,
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
    pub working_tree: WorkingTree,
    #[serde(default)]
    pub branch_changes: Option<BranchChanges>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeRequest {
    pub number: u64,
    pub url: String,
    pub state: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub is_draft: bool,
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
        let local =
            || VcsLocal::deserialize(&event["local"]).inspect_err(|error| log::warn!("bad git status: {error}")).ok();
        let remote = || VcsRemote::deserialize(&event["remote"]).unwrap_or_default();
        match event.get("_tag").and_then(Value::as_str)? {
            "snapshot" => Some(Self { local: local()?, remote: remote() }),
            "localUpdated" => Some(Self { local: local()?, remote: current.map(|c| c.remote).unwrap_or_default() }),
            "remoteUpdated" => {
                let local = current.map_or(VcsLocal { is_repo: true, ..VcsLocal::default() }, |c| c.local);
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
            Some("github" | "cursor-origin" | "gitcafe" | "forgejo" | "bitbucket" | "azure-devops") => "PR",
            _ => "change request",
        }
    }

    /// `changeRequestTerminologyFor(...).singular`.
    pub fn pr_singular(&self) -> &'static str {
        match self.pr_label() {
            "MR" => "merge request",
            "PR" => "pull request",
            _ => "change request",
        }
    }
}

/// `requiresDefaultBranchConfirmation`: what a push from the default ref asks about first.
pub fn requires_default_ref_confirmation(action: &str, is_default_ref: bool) -> bool {
    is_default_ref && matches!(action, "push" | "create_pr" | "commit_push" | "commit_push_pr")
}

/// The default-ref dialog's title, description and continue label.
#[derive(Debug, PartialEq)]
pub struct DefaultRefCopy {
    pub title: String,
    pub description: String,
    pub continue_label: String,
}

/// `resolveDefaultBranchActionDialogCopy` (`GitActionsControl.logic.ts`).
pub fn default_ref_copy(action: &str, branch: &str, includes_commit: bool, status: &VcsStatus) -> DefaultRefCopy {
    let suffix = format!(
        " on \"{branch}\". You can continue on this ref or create a feature ref and run the same action there."
    );
    let (pr, singular) = (status.pr_label(), status.pr_singular());
    let (title, description, continue_label) = match (action, includes_commit) {
        ("push" | "commit_push", true) => (
            "Commit & push to default ref?".to_owned(),
            format!("This action will commit and push changes{suffix}"),
            format!("Commit & push to {branch}"),
        ),
        ("push" | "commit_push", false) => (
            "Push to default ref?".to_owned(),
            format!("This action will push local commits{suffix}"),
            format!("Push to {branch}"),
        ),
        (_, true) => (
            format!("Commit, push & create {pr} from default ref?"),
            format!("This action will commit, push, and create a {singular}{suffix}"),
            format!("Commit, push & create {pr}"),
        ),
        (_, false) => (
            format!("Push & create {pr} from default ref?"),
            format!("This action will push local commits and create a {singular}{suffix}"),
            format!("Push & create {pr}"),
        ),
    };
    DefaultRefCopy { title, description, continue_label }
}

/// `resolveFeatureBranchUnavailableReason`: the server names a feature branch from the
/// uncommitted changes, so a clean tree can't get one.
pub fn feature_branch_unavailable(branch: &str, has_working_tree_changes: bool) -> Option<String> {
    (!has_working_tree_changes).then(|| {
        format!(
            "There are no changes to commit, so a feature branch can't be named. Create a branch yourself or continue on {branch}."
        )
    })
}

/// `resolveThreadBranchUpdate` on a `git.runStackedAction` event: the branch an
/// `action_finished` result says the action created.
pub fn created_branch(event: &Value) -> Option<String> {
    if event["kind"].as_str() != Some("action_finished") {
        return None;
    }
    let branch = &event["result"]["branch"];
    (branch["status"].as_str() == Some("created")).then(|| branch["name"].as_str()).flatten().map(str::to_owned)
}

/// `persistThreadBranchSync` for a server thread: the `thread.metadata.update` fields that record
/// `branch`, guarded by the branch the thread had (`expectedBranch`); `None` when it already has it.
pub fn branch_metadata_patch(branch: &str, thread_branch: Option<&str>) -> Option<Value> {
    (thread_branch != Some(branch)).then(|| json!({"branch": branch, "expectedBranch": thread_branch}))
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
        let singular = status.pr_singular();
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

/// `--diff-addition`: `--success` (emerald 500), or the blue-orange scheme's blue.
pub fn addition_base_color(tokens: &Tokens, scheme: DiffColorScheme) -> Hsla {
    match scheme {
        DiffColorScheme::RedGreen => rgb(0x00bc7d).into(),
        DiffColorScheme::BlueOrange => addition_color(tokens, scheme),
    }
}

/// `--diff-deletion`: `--destructive`, or the blue-orange scheme's orange.
pub fn deletion_base_color(tokens: &Tokens, scheme: DiffColorScheme) -> Hsla {
    match scheme {
        DiffColorScheme::RedGreen => tokens.error,
        DiffColorScheme::BlueOrange => deletion_color(tokens, scheme),
    }
}

/// `Intl.NumberFormat("en-US")` for a count.
pub fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `formatCompactDiffCount`: 999, 1.2k, 12k, 1.5m, 2b.
pub fn compact_diff_count(value: u64) -> String {
    let short = |scaled: f64, unit: &str| {
        if scaled < 10. {
            // `toFixed(1)` rounds an exact half up; `{:.1}` would round it to even.
            let tenths = format!("{:.1}", (scaled * 10.).round() / 10.);
            format!("{}{unit}", tenths.strip_suffix(".0").unwrap_or(&tenths))
        } else {
            format!("{}{unit}", scaled.round())
        }
    };
    #[expect(clippy::cast_precision_loss, reason = "a display count")]
    let value_f = value as f64;
    match value {
        0..1_000 => value.to_string(),
        1_000..999_500 => short(value_f / 1e3, "k"),
        999_500..999_500_000 => short(value_f / 1e6, "m"),
        _ => short(value_f / 1e9, "b"),
    }
}

/// `DiffStatLabel` (`layout="aligned"`): `+n` and `-n` in mono at `size`px, in right-aligned
/// 4ch columns (a mono `ch` is 0.6em).
pub fn diff_stat_label(
    additions: u64,
    deletions: u64,
    size: f32,
    tokens: &Tokens,
    scheme: DiffColorScheme,
) -> gpui_kit::Div {
    use gpui_kit::{ParentElement as _, Styled as _, div, px};
    let column = |text: String, color: Hsla| div().w(px(size * 2.4)).flex().justify_end().text_color(color).child(text);
    gpui_kit::component::h_flex()
        .flex_none()
        .gap(px(8.))
        .font_family(tokens.fonts.mono)
        .text_size(px(size))
        .child(column(format!("+{}", compact_diff_count(additions)), addition_color(tokens, scheme)))
        .child(column(format!("-{}", compact_diff_count(deletions)), deletion_color(tokens, scheme)))
}

/// `AuthSourceControlWriteScope`: commit, push, PR, init, pull and publish need it.
pub(super) const SOURCE_CONTROL_WRITE: &str = "source-control:write";

/// One `git.runStackedAction` request (`RunGitActionWithToastInput`).
#[derive(Clone, Debug)]
pub(super) struct GitRequest {
    pub action: &'static str,
    /// Blank lets the server write one.
    pub commit_message: Option<String>,
    /// `featureBranch`: check out a new branch named from the changes, then run the action there.
    pub feature_branch: bool,
    /// `filePaths`: commit only these; `None` commits every change.
    pub file_paths: Option<Vec<String>>,
}

impl GitRequest {
    pub(super) fn new(action: &'static str) -> Self {
        Self { action, commit_message: None, feature_branch: false, file_paths: None }
    }

    /// The `git.runStackedAction` input for `cwd`, without the thread and project it ran for.
    fn input(&self, action_id: String, cwd: &str) -> Value {
        let mut input = json!({"actionId": action_id, "cwd": cwd, "action": self.action});
        if let Some(message) = &self.commit_message {
            input["commitMessage"] = message.clone().into();
        }
        if self.feature_branch {
            input["featureBranch"] = true.into();
        }
        if let Some(paths) = &self.file_paths {
            input["filePaths"] = paths.clone().into();
        }
        input
    }
}

/// The thread (with the branch it had when the action started) or draft a git action ran for.
enum BranchOwner {
    Thread { id: String, env: SharedString, branch: Option<String> },
    Draft(Target),
}

/// The open target's status subscription; it follows the git cwd and its environment.
#[derive(Default)]
pub(super) struct VcsWatch {
    /// The environment `cwd` is on: the same path on two machines is two checkouts.
    pub env: Option<SharedString>,
    pub cwd: Option<String>,
    pub status: Option<VcsStatus>,
    /// The label of each checkout's git action in flight, as its progress reports it; the web
    /// keys action state by environment and cwd, so another checkout's operation never shows here.
    running: HashMap<Checkout, SharedString>,
    task: Option<Task<()>>,
    /// `rememberCheckoutIsRepo`: what each checkout reported this session, read while its status loads.
    known_repo: HashMap<Checkout, bool>,
}

/// A checkout: its environment and cwd.
pub(super) type Checkout = (Option<SharedString>, String);

impl VcsWatch {
    /// Whether a reply for `cwd` on `env` is about the checkout watched now.
    fn watches(&self, env: Option<&str>, cwd: &str) -> bool {
        self.env.as_deref() == env && self.cwd.as_deref() == Some(cwd)
    }

    pub(super) fn checkout(&self) -> Option<Checkout> {
        Some((self.env.clone(), self.cwd.clone()?))
    }

    /// Marks an action started on `checkout`, watched or not; false while one already runs there.
    pub(super) fn start_on(&mut self, checkout: Checkout, label: impl Into<SharedString>) -> bool {
        if self.running.contains_key(&checkout) {
            return false;
        }
        self.running.insert(checkout, label.into());
        true
    }

    /// Whether an action runs on `checkout`.
    pub(super) fn is_running(&self, checkout: &Checkout) -> bool {
        self.running.contains_key(checkout)
    }

    /// The watched checkout's git action in flight.
    pub(super) fn running(&self) -> Option<&SharedString> {
        self.running.get(&self.checkout()?)
    }

    /// Marks an action started on the watched checkout; `None` while one already runs there.
    pub(super) fn start(&mut self, label: impl Into<SharedString>) -> Option<Checkout> {
        let checkout = self.checkout()?;
        self.start_on(checkout.clone(), label).then_some(checkout)
    }

    fn progress(&mut self, checkout: &Checkout, label: SharedString) {
        if let Some(running) = self.running.get_mut(checkout) {
            *running = label;
        }
    }

    pub(super) fn finish(&mut self, checkout: &Checkout) {
        self.running.remove(checkout);
    }

    /// `liveIsGitRepo ?? recallCheckoutIsRepo(...) ?? true`.
    pub(super) fn is_repo(&self) -> bool {
        self.status.as_ref().map_or_else(
            || {
                let recalled = self.cwd.as_ref().and_then(|cwd| self.known_repo.get(&(self.env.clone(), cwd.clone())));
                recalled.copied().unwrap_or(true)
            },
            |status| status.local.is_repo,
        )
    }
}

impl Workspace {
    /// `gitCwd`: the thread's worktree (a draft's picked existing worktree), else its project root.
    pub(super) fn git_cwd(&self) -> Option<String> {
        let worktree = match self.target.as_ref()? {
            Target::Thread { env, id } => self
                .shell
                .thread(&ScopedId::new(env.clone(), id.as_str()))
                .and_then(|thread| thread.worktree_path.clone())
                .or_else(|| self.open_projection().and_then(|p| p.thread.worktree_path.clone())),
            Target::Draft { .. } => self.draft_workspace.existing_worktree(self.target.as_ref()).map(str::to_owned),
        };
        worktree.or_else(|| self.target_project().map(|project| project.workspace_root.clone()))
    }

    /// Keeps one `subscribeVcsStatus` stream on the current cwd; an ended stream resubscribes after a backoff.
    pub(super) fn sync_vcs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.active_environment().cloned();
        let cwd = self.git_cwd();
        let same = cwd == self.vcs.cwd && env == self.vcs.env;
        if same && (self.vcs.task.is_some() || cwd.is_none()) {
            return;
        }
        if !same {
            self.vcs.status = None;
        }
        self.vcs.cwd = cwd.clone();
        self.vcs.env = env.clone();
        self.vcs.task = None;
        let (Some(cwd), Some(client)) = (cwd, self.vcs_client()) else { return };
        let stream = client.stream("subscribeVcsStatus", json!({"cwd": cwd}));
        self.vcs.task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = stream.next().await {
                let mut events = vec![first];
                events.extend(std::iter::from_fn(|| stream.try_next()));
                let ended = this.update(cx, |this, cx| {
                    let mut ended = false;
                    for event in events {
                        match event {
                            StreamEvent::Item(value) if this.vcs.watches(env.as_deref(), &cwd) => {
                                this.vcs.status = VcsStatus::apply(this.vcs.status.take(), &value);
                                if let Some(status) = &this.vcs.status {
                                    this.vcs.known_repo.insert((env.clone(), cwd.clone()), status.local.is_repo);
                                }
                            }
                            StreamEvent::Item(_) => {}
                            StreamEvent::End(result) => {
                                ended = true;
                                if let Err(error) = result
                                    && !matches!(error, RpcError::Disconnected(_))
                                {
                                    log::warn!("git status stream ended: {error}");
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
            // Waits before the next render resubscribes, so a cwd the server rejects can't loop.
            super::details::resubscribe_after_backoff(&this, cx, |this| this.vcs.task = None).await;
        }));
    }

    /// The socket of the environment the watched checkout is on.
    pub(super) fn vcs_client(&self) -> Option<RpcClient> {
        self.env_client(self.vcs.env.as_deref()?)
    }

    /// `useEnvironmentScope(…, "source-control:write")` on the watched checkout's environment.
    pub(super) fn can_write_source_control(&self) -> bool {
        self.vcs.env.as_deref().is_some_and(|env| self.env_grants(env, SOURCE_CONTROL_WRITE))
    }

    /// `permissionAtom` for the `git.runStackedAction` `request` would send from the open target.
    pub(super) fn permits_git_action(&self, request: &GitRequest) -> bool {
        let (Some(env), Some(cwd)) = (self.vcs.env.as_deref(), self.vcs.cwd.as_deref()) else { return false };
        self.env_permits(env, "git.runStackedAction", &self.git_action_input(request, String::new(), cwd))
    }

    /// `permissionAtom` for a one-shot git RPC on the watched checkout.
    pub(super) fn permits_vcs_request(&self, method: &str) -> bool {
        let (Some(env), Some(cwd)) = (self.vcs.env.as_deref(), self.vcs.cwd.as_deref()) else { return false };
        self.env_permits(env, method, &json!({"cwd": cwd}))
    }

    /// `permissionAtom` for `sourceControl.publishRepository` on the watched checkout.
    pub(super) fn permits_publish(&self) -> bool {
        self.permits_vcs_request("sourceControl.publishRepository")
    }

    /// `permissionAtom` for what the quick action sends; a hint sends nothing but still needs write.
    pub(super) fn permits_quick_action(&self, kind: &QuickKind) -> bool {
        match kind {
            QuickKind::Run(action) => self.permits_git_action(&GitRequest::new(action)),
            QuickKind::Pull => self.permits_vcs_request("vcs.pull"),
            QuickKind::Publish => self.permits_publish(),
            QuickKind::Hint(_) => self.can_write_source_control(),
        }
    }

    /// The `git.runStackedAction` input for `request` on `cwd`, for the open thread and project.
    fn git_action_input(&self, request: &GitRequest, action_id: String, cwd: &str) -> Value {
        let mut input = request.input(action_id, cwd);
        if let Some(Target::Thread { id, .. }) = &self.target {
            input["threadId"] = id.clone().into();
        }
        if let Some(project) = self.target_project() {
            input["projectId"] = project.id.clone().into();
        }
        input
    }

    /// `canChangeThreadBranch`: a feature branch also moves a server thread to it.
    pub(super) fn can_change_thread_branch(&self) -> bool {
        self.can_write_source_control()
            && match &self.target {
                Some(Target::Thread { env, .. }) => self.can_operate(env),
                _ => true,
            }
    }

    /// `runGitActionWithToast`: a push or PR from the default ref asks first.
    pub(super) fn request_git_action(&mut self, request: GitRequest, window: &mut Window, cx: &mut Context<Self>) {
        if !self.permits_git_action(&request) {
            return;
        }
        let status = self.vcs.status.as_ref();
        let is_default_ref = !request.feature_branch && status.is_some_and(|s| s.local.is_default_ref);
        let branch = status.and_then(|s| s.local.ref_name.clone());
        match branch.filter(|_| requires_default_ref_confirmation(request.action, is_default_ref)) {
            Some(branch) => self.confirm_default_ref(request, branch, window, cx),
            None => self.run_git_action(request, cx),
        }
    }

    /// `git.runStackedAction` on the watched checkout, past the default-ref prompt. The grants are
    /// checked again, since they can change while a dialog is open.
    pub(super) fn run_git_action(&mut self, request: GitRequest, cx: &mut Context<Self>) {
        if !self.permits_git_action(&request) || (request.feature_branch && !self.can_change_thread_branch()) {
            return;
        }
        let Some(client) = self.vcs_client() else { return };
        let Some(checkout) = self.vcs.start("Starting source control action...") else { return };
        let input = self.git_action_input(&request, new_id(), &checkout.1);
        let owner = self.branch_owner();
        let origin = self.target.clone();
        let stream = client.stream("git.runStackedAction", input);
        cx.spawn(async move |this, cx| {
            let mut outcome: Result<(), String> = Ok(());
            let mut created = None;
            while let Some(event) = stream.next().await {
                match event {
                    StreamEvent::Item(value) => match value.get("kind").and_then(Value::as_str) {
                        Some("phase_started") => {
                            let label = value["label"].as_str().unwrap_or_default().to_owned();
                            let _ = this.update(cx, |this, cx| {
                                this.vcs.progress(&checkout, label.into());
                                cx.notify();
                            });
                        }
                        Some("action_finished") => created = created_branch(&value),
                        Some("action_failed") => {
                            outcome = Err(value["message"].as_str().unwrap_or("Git action failed.").to_owned());
                        }
                        _ => {}
                    },
                    StreamEvent::End(Err(error)) => outcome = Err(error.message()),
                    StreamEvent::End(Ok(())) => break,
                }
            }
            let _ = this.update(cx, |this, cx| {
                if let (Some(branch), Some(owner), Ok(())) = (created, owner, &outcome) {
                    this.record_created_branch(owner, branch, cx);
                }
                this.finish_vcs(client, checkout, origin, outcome.err(), cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// The thread or draft a git action starts from, which its created branch belongs to.
    fn branch_owner(&self) -> Option<BranchOwner> {
        match self.target.as_ref()? {
            Target::Thread { id, env } => {
                let shell = self.shell.thread(&ScopedId::new(env.clone(), id.as_str()));
                let branch = match shell {
                    Some(thread) => thread.branch.clone(),
                    None => self.open_projection().and_then(|p| p.thread.branch.clone()),
                };
                Some(BranchOwner::Thread { id: id.clone(), env: env.clone(), branch })
            }
            draft @ Target::Draft { .. } => Some(BranchOwner::Draft(draft.clone())),
        }
    }

    /// `syncThreadBranchAfterGitAction`: a server thread records the branch, guarded by the one
    /// it had; a draft picks it.
    fn record_created_branch(&mut self, owner: BranchOwner, branch: String, cx: &mut Context<Self>) {
        match owner {
            BranchOwner::Thread { id, env, branch: had } => {
                let fields = branch_metadata_patch(&branch, had.as_deref());
                if let Some(fields) = fields.filter(|_| self.can_operate(&env)) {
                    self.thread_command("thread.metadata.update", &crate::model::ScopedId::new(env, id), fields, cx);
                }
            }
            BranchOwner::Draft(draft) => self.set_draft_branch(&draft, branch, cx),
        }
    }

    /// A one-shot git RPC (`vcs.init`, `vcs.pull`) on the current cwd.
    pub(super) fn run_vcs_request(&mut self, method: &'static str, label: &'static str, cx: &mut Context<Self>) {
        if !self.permits_vcs_request(method) {
            return;
        }
        let Some(client) = self.vcs_client() else { return };
        let Some(checkout) = self.vcs.start(label) else { return };
        let origin = self.target.clone();
        cx.spawn(async move |this, cx| {
            let result = client.request(method, json!({"cwd": checkout.1})).await;
            let error = result.err().map(|e| e.to_string());
            let _ = this.update(cx, |this, cx| this.finish_vcs(client, checkout, origin, error, cx));
        })
        .detach();
        cx.notify();
    }

    /// Ends `checkout`'s git action: reports its error on `origin`, the target it started from,
    /// and refreshes that checkout's status, which lands only if it is still the one watched.
    pub(super) fn finish_vcs(
        &mut self,
        client: RpcClient,
        checkout: Checkout,
        origin: Option<Target>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.vcs.finish(&checkout);
        if let Some(error) = error {
            self.set_error_from(origin.as_ref(), error.into());
        }
        self.refresh_checkout(client, checkout, cx);
        cx.notify();
    }

    /// `vcs.refreshStatus` after a git operation, so the card does not wait on the watcher.
    pub(super) fn refresh_vcs(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(checkout)) = (self.vcs_client(), self.vcs.checkout()) else { return };
        self.refresh_checkout(client, checkout, cx);
    }

    fn refresh_checkout(&mut self, client: RpcClient, (env, cwd): Checkout, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let value = match client.request("vcs.refreshStatus", json!({"cwd": cwd})).await {
                Ok(value) => value,
                Err(error) => {
                    log::warn!("vcs.refreshStatus failed: {error}");
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                if !this.vcs.watches(env.as_deref(), &cwd) {
                    return;
                }
                match VcsLocal::deserialize(&value) {
                    Ok(local) => {
                        let remote = VcsRemote::deserialize(&value).unwrap_or_default();
                        this.vcs.status = Some(VcsStatus { local, remote });
                        cx.notify();
                    }
                    Err(error) => log::warn!("bad vcs.refreshStatus reply: {error}"),
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_git_action_belongs_to_the_checkout_it_started_on() {
        let mut vcs = VcsWatch { env: Some("env".into()), cwd: Some("/repo".into()), ..VcsWatch::default() };
        let pull = vcs.start("Pulling...").unwrap();
        assert!(vcs.start("Pushing...").is_none(), "one action per checkout at a time");
        // Another checkout is free while the first one's action runs.
        vcs.cwd = Some("/repo/wt".into());
        assert_eq!(vcs.running(), None);
        let push = vcs.start("Pushing...").unwrap();
        vcs.env = Some("other".into());
        vcs.cwd = Some("/repo".into());
        assert_eq!(vcs.running(), None, "the same path on another machine is another checkout");
        vcs.env = Some("env".into());
        vcs.progress(&pull, "Fetching".into());
        assert_eq!(vcs.running().map(SharedString::as_ref), Some("Fetching"));
        vcs.finish(&pull);
        assert_eq!(vcs.running(), None);
        vcs.cwd = Some("/repo/wt".into());
        assert_eq!(vcs.running().map(SharedString::as_ref), Some("Pushing..."));
        vcs.finish(&push);
        assert_eq!(vcs.running(), None);
    }

    #[test]
    fn diff_counts_compact_like_the_web() {
        let cases = [
            (0, "0"),
            (999, "999"),
            (1_000, "1k"),
            (1_250, "1.3k"),
            (9_940, "9.9k"),
            (12_345, "12k"),
            (999_499, "999k"),
            (999_500, "1m"),
            (1_500_000, "1.5m"),
            (999_500_000, "1b"),
            (23_000_000_000, "23b"),
        ];
        for (value, shown) in cases {
            assert_eq!(compact_diff_count(value), shown, "{value}");
        }
    }

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
    fn pushes_from_the_default_ref_ask_first_with_the_web_copy() {
        assert!(requires_default_ref_confirmation("commit_push_pr", true));
        assert!(!requires_default_ref_confirmation("commit", true), "a plain commit never asks");
        assert!(!requires_default_ref_confirmation("push", false));
        let github = status(local(true, json!({"isDefaultRef": true, "refName": "main"})), Value::Null);
        let copy = default_ref_copy("commit_push_pr", "main", true, &github);
        assert_eq!(copy.title, "Commit, push & create PR from default ref?");
        assert_eq!(
            copy.description,
            "This action will commit, push, and create a pull request on \"main\". You can continue on this ref or create a feature ref and run the same action there."
        );
        assert_eq!(copy.continue_label, "Commit, push & create PR");
        let push = default_ref_copy("commit_push", "main", false, &github);
        assert_eq!((push.title.as_str(), push.continue_label.as_str()), ("Push to default ref?", "Push to main"));
        let gitlab =
            status(local(false, json!({"sourceControlProvider": {"kind": "gitlab"}})), json!({"hasUpstream": true}));
        let create = default_ref_copy("create_pr", "main", false, &gitlab);
        assert_eq!(create.title, "Push & create MR from default ref?");
        assert!(create.description.starts_with("This action will push local commits and create a merge request"));
        assert_eq!(feature_branch_unavailable("main", true), None);
        assert_eq!(
            feature_branch_unavailable("main", false).as_deref(),
            Some(
                "There are no changes to commit, so a feature branch can't be named. Create a branch yourself or continue on main."
            )
        );
    }

    #[test]
    fn a_created_branch_is_recorded_once_and_guarded_by_the_old_one() {
        let finished =
            |branch: Value| json!({"kind": "action_finished", "actionId": "a", "result": {"branch": branch}});
        assert_eq!(
            created_branch(&finished(json!({"status": "created", "name": "t3/fix-login"}))).as_deref(),
            Some("t3/fix-login")
        );
        assert_eq!(created_branch(&finished(json!({"status": "skipped_not_requested"}))), None);
        assert_eq!(created_branch(&json!({"kind": "phase_started", "label": "Committing"})), None);
        assert_eq!(
            branch_metadata_patch("t3/fix-login", Some("main")),
            Some(json!({"branch": "t3/fix-login", "expectedBranch": "main"}))
        );
        assert_eq!(
            branch_metadata_patch("t3/fix-login", None),
            Some(json!({"branch": "t3/fix-login", "expectedBranch": null}))
        );
        assert_eq!(branch_metadata_patch("main", Some("main")), None);
    }

    #[test]
    fn a_commit_request_sends_only_what_it_was_given() {
        assert_eq!(
            GitRequest::new("commit").input("a".into(), "/repo"),
            json!({"actionId": "a", "cwd": "/repo", "action": "commit"})
        );
        let picked = GitRequest {
            commit_message: Some("Fix login".into()),
            feature_branch: true,
            file_paths: Some(vec!["src/a.rs".into(), "README.md".into()]),
            ..GitRequest::new("commit")
        };
        assert_eq!(
            picked.input("b".into(), "/repo"),
            json!({
                "actionId": "b", "cwd": "/repo", "action": "commit", "commitMessage": "Fix login",
                "featureBranch": true, "filePaths": ["src/a.rs", "README.md"],
            })
        );
    }

    #[test]
    fn the_working_tree_carries_its_files_and_root() {
        let tree = json!({
            "rootPath": "/repo",
            "files": [{"path": "src/a.rs", "insertions": 3, "deletions": 1}, {"path": "b.md", "insertions": 0, "deletions": 2}],
            "insertions": 3, "deletions": 3,
        });
        let parsed = status(local(true, json!({"workingTree": tree})), Value::Null).local.working_tree;
        assert_eq!(parsed.root_path.as_deref(), Some("/repo"));
        assert_eq!(
            parsed.files,
            [
                WorkingTreeFile { path: "src/a.rs".into(), insertions: 3, deletions: 1 },
                WorkingTreeFile { path: "b.md".into(), insertions: 0, deletions: 2 },
            ]
        );
        assert_eq!((parsed.insertions, parsed.deletions), (3, 3));
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(7), "7");
        assert_eq!(group_thousands(11_314), "11,314");
        assert_eq!(group_thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn a_status_reply_lands_only_on_the_checkout_it_was_asked_on() {
        let watch = VcsWatch { env: Some("b".into()), cwd: Some("/repo".into()), ..VcsWatch::default() };
        assert!(watch.watches(Some("b"), "/repo"));
        assert!(!watch.watches(Some("a"), "/repo"), "the same path on another machine is another checkout");
        assert!(!watch.watches(Some("b"), "/other"));
    }
}
