//! A pull request's detail panel logic: the `pullRequests.detail` and `pullRequests.activity`
//! read models and the rules the panel derives from them
//! (`apps/web/src/components/pullRequest/pullRequestDetail.logic.ts`, `pullRequestPresentation.tsx`).

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::inbox::parse_ms;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_bot: bool,
}

impl Actor {
    /// "Name (@login)" when the host knows a different display name.
    pub fn named(actor: Option<&Actor>) -> String {
        let login = actor.map_or("ghost", |a| a.login.as_str());
        match actor.and_then(|a| a.name.as_deref()) {
            Some(name) if name != login => format!("{name} (@{login})"),
            _ => login.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Label {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Check {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCapabilities {
    #[serde(default)]
    pub inline_comment: bool,
    #[serde(default)]
    pub range_comment: Option<bool>,
    #[serde(default)]
    pub reply: bool,
    #[serde(default)]
    pub resolve: bool,
    #[serde(default)]
    pub verdicts: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerCapabilities {
    #[serde(default)]
    pub request: bool,
    #[serde(default)]
    pub list_candidates: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditCapabilities {
    #[serde(default)]
    pub change_request: bool,
    #[serde(default)]
    pub comment: bool,
    #[serde(default)]
    pub maintainer_edits: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub diff: bool,
    #[serde(default)]
    pub comment: bool,
    #[serde(default)]
    pub reactions: Option<bool>,
    #[serde(default)]
    pub viewed_files: Option<String>,
    #[serde(default)]
    pub review: ReviewCapabilities,
    #[serde(default)]
    pub reviewers: ReviewerCapabilities,
    #[serde(default)]
    pub edit: Option<EditCapabilities>,
    #[serde(default)]
    pub update_methods: Vec<String>,
    #[serde(default)]
    pub stacks: Option<bool>,
    #[serde(default)]
    pub stack_actions: Option<bool>,
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default)]
    pub merge_methods: Vec<String>,
    #[serde(default)]
    pub delete_branch: Option<bool>,
    #[serde(default)]
    pub labels: Option<bool>,
}

/// Unreported host permissions are granted.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    #[serde(default)]
    pub actions: Vec<String>,
    #[serde(default = "granted")]
    pub comment: bool,
    #[serde(default = "granted")]
    pub resolve: bool,
    #[serde(default)]
    pub verdicts: Vec<String>,
    #[serde(default = "granted")]
    pub request_reviewers: bool,
    #[serde(default)]
    pub labels: Option<bool>,
    #[serde(default)]
    pub edit_change_request: Option<bool>,
    #[serde(default)]
    pub update_methods: Vec<String>,
    #[serde(default)]
    pub maintainer_edits: Option<bool>,
    #[serde(default)]
    pub stack_rebase: Option<bool>,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            actions: Vec::new(),
            comment: true,
            resolve: true,
            verdicts: Vec::new(),
            request_reviewers: true,
            labels: None,
            edit_change_request: None,
            update_methods: Vec::new(),
            maintainer_edits: None,
            stack_rebase: None,
        }
    }
}

fn granted() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct MergeCapabilities {
    #[serde(default)]
    pub merge: bool,
    #[serde(default)]
    pub squash: bool,
    #[serde(default)]
    pub rebase: bool,
}

/// `PullRequestDetail`, as far as the panel reads it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    pub provider: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub viewer: Option<String>,
    #[serde(default)]
    pub base_comparison: Option<String>,
    #[serde(default)]
    pub behind_by: Option<u64>,
    #[serde(default)]
    pub maintainer_can_modify: Option<bool>,
    #[serde(default)]
    pub viewer_permissions: Permissions,
    pub project_id: String,
    pub repository: String,
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub url: String,
    #[serde(default)]
    pub author: Option<Actor>,
    pub state: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub mergeability: String,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub changed_files: u64,
    pub head_branch: String,
    #[serde(default)]
    pub head_repository_name_with_owner: Option<String>,
    pub base_branch: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub merged_at: Option<String>,
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default)]
    pub reviewers: Vec<Actor>,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub merge_capabilities: MergeCapabilities,
    /// Absent means the host does not report auto-merge, not that it is off.
    #[serde(default)]
    pub auto_merge_enabled: Option<bool>,
    #[serde(default)]
    pub auto_merge_method: Option<String>,
    #[serde(default)]
    pub workflow_approvals_required: Option<u64>,
}

/// `PullRequestReaction`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reaction {
    pub content: String,
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub viewer_has_reacted: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub author: Option<Actor>,
    #[serde(default)]
    pub body: String,
    pub created_at: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub review_state: Option<String>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub can_edit: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadComment {
    pub id: String,
    #[serde(default)]
    pub author: Option<Actor>,
    #[serde(default)]
    pub body: String,
    pub created_at: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub can_edit: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub line: Option<u64>,
    #[serde(default = "right_side")]
    pub side: String,
    #[serde(default)]
    pub can_resolve: Option<bool>,
    #[serde(default)]
    pub is_resolved: bool,
    #[serde(default)]
    pub is_outdated: bool,
    #[serde(default)]
    pub comments: Vec<ThreadComment>,
    #[serde(default)]
    pub comment_count: Option<usize>,
    #[serde(default)]
    pub next_comments_cursor: Option<String>,
}

fn right_side() -> String {
    "right".into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub oid: String,
    #[serde(default)]
    pub message_headline: String,
    pub committed_date: String,
    #[serde(default)]
    pub additions: Option<u64>,
    #[serde(default)]
    pub deletions: Option<u64>,
    #[serde(default)]
    pub authors: Vec<Actor>,
}

/// `PullRequestActivity`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    #[serde(default)]
    pub author: Option<Option<Actor>>,
    #[serde(default)]
    pub reviewers: Option<Vec<Actor>>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub comment_count: usize,
    #[serde(default)]
    pub comments_truncated: bool,
    #[serde(default)]
    pub review_threads: Vec<ReviewThread>,
    #[serde(default)]
    pub commits: Vec<Commit>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
}

impl Activity {
    /// `commentCount`, never below the comments actually loaded.
    pub fn comment_total(&self) -> usize {
        let ids: HashSet<&str> = self
            .comments
            .iter()
            .map(|c| c.id.as_str())
            .chain(self.review_threads.iter().flat_map(|t| t.comments.iter().map(|c| c.id.as_str())))
            .collect();
        self.comment_count.max(ids.len())
    }
}

/// `PullRequestRef`: where a pull request is read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub project_id: String,
    pub host: Option<String>,
    pub repository: String,
    pub number: u64,
}

impl Reference {
    pub fn to_json(&self) -> Value {
        let mut value = json!({"projectId": self.project_id, "repository": self.repository, "number": self.number});
        if let Some(host) = &self.host {
            value["host"] = json!(host);
        }
        value
    }

    /// `pullRequestRefKey`.
    pub fn key(&self) -> String {
        format!("{}:{}:{}#{}", self.project_id, self.host.as_deref().unwrap_or(""), self.repository, self.number)
    }
}

pub const MERGE_METHODS: [(&str, &str); 3] =
    [("merge", "Merge"), ("squash", "Squash and merge"), ("rebase", "Rebase and merge")];

pub fn merge_method_label(method: &str) -> &'static str {
    MERGE_METHODS.iter().find(|m| m.0 == method).map_or("Merge", |m| m.1)
}

/// `allowedPullRequestMergeMethods`: what the host offers and the repository allows.
pub fn allowed_merge_methods(detail: &Detail) -> Vec<&'static str> {
    let caps = detail.merge_capabilities;
    MERGE_METHODS
        .iter()
        .map(|m| m.0)
        .filter(|m| detail.capabilities.merge_methods.iter().any(|c| c == m))
        .filter(|m| match *m {
            "merge" => caps.merge,
            "squash" => caps.squash,
            _ => caps.rebase,
        })
        .collect()
}

/// `resolvePullRequestMergeMethod`: the panel's pick, then the project's default, then the last one used.
pub fn resolve_merge_method(
    allowed: &[&'static str],
    current: Option<&str>,
    project_default: Option<&str>,
    last: &str,
) -> &'static str {
    [current, project_default, Some(last)]
        .into_iter()
        .flatten()
        .find_map(|method| allowed.iter().find(|a| **a == method).copied())
        .or_else(|| allowed.first().copied())
        .unwrap_or("merge")
}

/// `can(action)`: the host offers it and the viewer may do it.
pub fn can(detail: &Detail, action: &str) -> bool {
    detail.capabilities.actions.iter().any(|a| a == action)
        && detail.viewer_permissions.actions.iter().any(|a| a == action)
}

fn same_login(a: Option<&str>, b: Option<&str>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b))
}

/// `canEditPullRequestChangeRequest`: the host's answer, else the author or someone who may merge.
pub fn can_edit_change_request(detail: &Detail) -> bool {
    if !detail.capabilities.edit.is_some_and(|e| e.change_request) {
        return false;
    }
    detail.viewer_permissions.edit_change_request.unwrap_or_else(|| {
        same_login(detail.viewer.as_deref(), detail.author.as_ref().map(|a| a.login.as_str()))
            || detail.viewer_permissions.actions.iter().any(|a| a == "merge")
    })
}

/// `canEditPullRequestComment`: a remark, not a review's summary, by the reader or that the host allows.
pub fn can_edit_comment(detail: &Detail, kind: &str, author: Option<&Actor>, can_edit: Option<bool>) -> bool {
    if !detail.capabilities.edit.is_some_and(|e| e.comment) || !matches!(kind, "issue-comment" | "review-comment") {
        return false;
    }
    can_edit.unwrap_or_else(|| same_login(detail.viewer.as_deref(), author.map(|a| a.login.as_str())))
}

/// `canResolvePullRequestThread`: the thread's answer can only narrow the reader's permission.
pub fn can_resolve_thread(detail: &Detail, thread: &ReviewThread) -> bool {
    detail.viewer_permissions.resolve && thread.can_resolve != Some(false)
}

/// `canEditPullRequestMaintainerEdits`.
pub fn can_edit_maintainer_edits(detail: &Detail) -> bool {
    detail.state == "open"
        && detail.capabilities.edit.and_then(|e| e.maintainer_edits) == Some(true)
        && detail.viewer_permissions.maintainer_edits == Some(true)
        && detail.maintainer_can_modify.is_some()
}

/// `resolveBaseFreshness`: how far an open, mergeable branch is behind its base, and the ways
/// this reader may bring it up to date.
pub fn base_freshness(detail: &Detail) -> Option<(Option<u64>, Vec<String>)> {
    if detail.state != "open"
        || detail.base_comparison.as_deref() != Some("behind")
        || detail.mergeability != "mergeable"
    {
        return None;
    }
    let methods = detail
        .capabilities
        .update_methods
        .iter()
        .filter(|m| detail.viewer_permissions.update_methods.contains(m))
        .cloned()
        .collect();
    Some((detail.behind_by, methods))
}

/// `groupPullRequestChecks`: what needs attention, what is running, and what finished.
pub fn group_checks(checks: &[Check]) -> (Vec<&Check>, Vec<&Check>, Vec<&Check>) {
    let pick = |statuses: &[&str]| checks.iter().filter(|c| statuses.contains(&c.status.as_str())).collect::<Vec<_>>();
    (pick(&["failure", "cancelled", "action-required"]), pick(&["pending"]), pick(&["success", "skipped", "neutral"]))
}

/// One layer of a native stack (`PullRequestStack.layers`).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackLayer {
    pub number: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub head_sha: Option<String>,
    pub head_branch: String,
    pub state: String,
}

/// `PullRequestStack`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stack {
    pub number: u64,
    pub base: String,
    #[serde(default)]
    pub layers: Vec<StackLayer>,
}

/// What the stack menu offers for the layer shown (`PullRequestStackMenu`).
#[derive(Debug, PartialEq, Eq)]
pub struct StackPlan {
    /// One-based place of the shown layer.
    pub position: usize,
    /// Unmerged layers up to the shown one, which "Merge stack" merges.
    pub merge: Vec<u64>,
    pub merge_disabled: bool,
    /// Every unmerged layer, which "Rebase stack" rebases.
    pub rebase: Vec<u64>,
    pub rebase_disabled: bool,
    /// A layer being merged is closed or still a draft.
    pub merge_blocked: bool,
    pub selected_open: bool,
}

pub fn stack_plan(stack: &Stack, number: u64) -> StackPlan {
    let position = stack.layers.iter().position(|l| l.number == number).map_or(0, |i| i + 1);
    let unmerged: Vec<&StackLayer> = stack.layers.iter().filter(|l| l.state != "merged").collect();
    let merge: Vec<&StackLayer> = stack.layers[..position].iter().filter(|l| l.state != "merged").collect();
    let selected_open = position > 0 && stack.layers[position - 1].state == "open";
    let merge_blocked = merge.iter().any(|l| l.state != "open" || l.is_draft);
    StackPlan {
        position,
        merge_disabled: !selected_open
            || merge.is_empty()
            || merge.iter().any(|l| l.head_sha.is_none())
            || merge_blocked,
        merge: merge.iter().map(|l| l.number).collect(),
        rebase_disabled: unmerged.is_empty()
            || unmerged.iter().any(|l| l.head_sha.is_none())
            || unmerged.iter().any(|l| l.state != "open"),
        rebase: unmerged.iter().map(|l| l.number).collect(),
        merge_blocked,
        selected_open,
    }
}

/// `expectedStackHeads` for the layers an action covers.
pub fn stack_heads(stack: &Stack, numbers: &[u64]) -> Value {
    Value::Array(
        stack
            .layers
            .iter()
            .filter(|l| numbers.contains(&l.number))
            .filter_map(|l| Some(json!({"number": l.number, "headSha": l.head_sha.as_ref()?})))
            .collect(),
    )
}

/// The header's one prominent control (`resolvePullRequestPrimaryControl`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primary {
    Resolve,
    Ready,
    Merge,
    EnableAutoMerge,
    AutoMergeArmed,
    Merged,
    Closed,
}

pub fn primary_control(detail: &Detail, checks_state: Option<&str>) -> Option<Primary> {
    let has_method = !allowed_merge_methods(detail).is_empty();
    match detail.state.as_str() {
        "merged" => return Some(Primary::Merged),
        "closed" => return Some(Primary::Closed),
        _ => {}
    }
    if detail.mergeability == "conflicting" {
        return Some(Primary::Resolve);
    }
    if detail.is_draft {
        return can(detail, "ready").then_some(Primary::Ready);
    }
    if detail.auto_merge_enabled == Some(true) {
        return Some(Primary::AutoMergeArmed);
    }
    if !has_method {
        return None;
    }
    if detail.auto_merge_enabled == Some(false)
        && checks_state.is_some_and(|s| s != "passing")
        && can(detail, "enable-auto-merge")
    {
        return Some(Primary::EnableAutoMerge);
    }
    can(detail, "merge").then_some(Primary::Merge)
}

/// The overflow menu's state actions (`showsDraftToggle`, `showsMergeNow`, ...).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MenuActions {
    pub draft_toggle: bool,
    pub merge_now: bool,
    pub auto_merge: bool,
    pub disable_auto_merge: bool,
    pub merge_methods: bool,
    pub close: bool,
    pub reopen: bool,
    pub revert: bool,
    pub delete_branch: bool,
}

pub fn menu_actions(detail: &Detail, primary: Option<Primary>) -> MenuActions {
    let open = detail.state == "open";
    let conflicting = open && detail.mergeability == "conflicting";
    let methods = allowed_merge_methods(detail).len();
    let armed = open && detail.auto_merge_enabled == Some(true);
    let mergeable = open && can(detail, "merge") && !detail.is_draft && !conflicting && methods > 0;
    MenuActions {
        draft_toggle: open
            && can(detail, if detail.is_draft { "ready" } else { "draft" })
            && !(detail.is_draft && primary == Some(Primary::Ready)),
        merge_now: mergeable && matches!(primary, Some(Primary::EnableAutoMerge | Primary::AutoMergeArmed)),
        disable_auto_merge: armed && can(detail, "disable-auto-merge"),
        auto_merge: open
            && !armed
            && primary != Some(Primary::EnableAutoMerge)
            && !detail.is_draft
            && !conflicting
            && can(detail, "enable-auto-merge")
            && methods > 0,
        merge_methods: mergeable && methods > 1,
        close: open && can(detail, "close"),
        reopen: detail.state == "closed" && can(detail, "reopen"),
        revert: detail.state == "merged" && can(detail, "revert"),
        delete_branch: detail.state != "open"
            && detail.capabilities.delete_branch == Some(true)
            && can(detail, "delete-branch"),
    }
}

/// `pullRequestChecksState`.
pub fn checks_state(checks: &[Check]) -> Option<&'static str> {
    let has = |status: &str| checks.iter().any(|c| c.status == status);
    if checks.is_empty() {
        None
    } else if has("failure") || has("cancelled") {
        Some("failing")
    } else if has("pending") || has("action-required") {
        Some("pending")
    } else {
        has("success").then_some("passing")
    }
}

fn is_workflow_approval(check: &Check) -> bool {
    check.status == "action-required"
        && check.url.as_deref().is_some_and(|url| {
            url.split_once("/actions/runs/").is_some_and(|(_, rest)| {
                let digits = rest.chars().take_while(char::is_ascii_digit).count();
                digits > 0 && matches!(rest[digits..].chars().next(), None | Some('/'))
            })
        })
}

/// `pullRequestCheckStatusLabel`.
pub fn check_status_label(check: &Check) -> &'static str {
    if is_workflow_approval(check) {
        return "Awaiting approval";
    }
    match check.status.as_str() {
        "pending" => "Running",
        "action-required" => "Awaiting action",
        "success" => "Passed",
        "failure" => "Failed",
        "cancelled" => "Cancelled",
        "skipped" => "Skipped",
        _ => "Neutral",
    }
}

/// `summarizePullRequestChecks`.
pub fn summarize_checks(checks: &[Check]) -> String {
    let total = checks.len();
    if total == 0 {
        return "No checks reported".into();
    }
    let count = |f: &dyn Fn(&Check) -> bool| checks.iter().filter(|c| f(c)).count();
    let approvals = count(&|c| is_workflow_approval(c));
    let other_action = count(&|c| c.status == "action-required") - approvals;
    let failed = count(&|c| c.status == "failure" || c.status == "cancelled");
    let pending = count(&|c| c.status == "pending");
    let passed = count(&|c| c.status == "success");
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if failed > 0 {
        format!("{failed} of {total} failing")
    } else if approvals > 0 && other_action > 0 {
        format!(
            "{} and {} awaiting action",
            plural(approvals, "workflow", "workflows"),
            plural(other_action, "check", "checks")
        )
    } else if approvals > 0 {
        format!("{} awaiting approval", plural(approvals, "workflow", "workflows"))
    } else if other_action > 0 {
        format!("{} awaiting action", plural(other_action, "check", "checks"))
    } else if pending > 0 {
        format!("{pending} of {total} running")
    } else if passed == total {
        "All checks passed".into()
    } else {
        format!("{passed} of {total} passing")
    }
}

pub fn checks_state_label(state: &str) -> &'static str {
    match state {
        "passing" => "All checks have passed",
        "failing" => "Some checks were not successful",
        _ => "Some checks haven't completed yet",
    }
}

/// `pullRequestReviewOutcome`: `approved`, `changes-requested` or `dismissed`.
pub fn review_outcome(review_state: Option<&str>) -> Option<&'static str> {
    match review_state?.trim().to_lowercase().replace('_', "-").as_str() {
        "approved" => Some("approved"),
        "changes-requested" => Some("changes-requested"),
        "dismissed" => Some("dismissed"),
        _ => None,
    }
}

pub fn outcome_label(outcome: &str) -> &'static str {
    match outcome {
        "approved" => "Approved",
        "changes-requested" => "Changes requested",
        _ => "Review dismissed",
    }
}

fn instant(iso: &str) -> Option<i64> {
    parse_ms(iso)
}

fn newest_commit_ms(commits: &[Commit]) -> Option<i64> {
    commits.iter().filter_map(|c| instant(&c.committed_date)).max()
}

/// `isPullRequestVerdictStale`: the verdict came before the newest commit.
pub fn verdict_stale(at: &str, commits: &[Commit]) -> bool {
    matches!((instant(at), newest_commit_ms(commits)), (Some(at), Some(newest)) if at < newest)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReviewOutcome {
    pub actor: Option<Actor>,
    pub outcome: &'static str,
    pub stale: bool,
}

/// `latestPullRequestReviewOutcomes`: each reviewer's newest verdict, dismissals dropped.
pub fn latest_review_outcomes(comments: &[Comment], commits: &[Commit]) -> Vec<(String, ReviewOutcome)> {
    let mut order: Vec<String> = Vec::new();
    let mut latest: HashMap<String, (i64, ReviewOutcome)> = HashMap::new();
    for comment in comments {
        let Some(outcome) = review_outcome(comment.review_state.as_deref()) else { continue };
        let key = comment.author.as_ref().map_or_else(|| format!("ghost:{}", comment.id), |a| a.login.clone());
        let at = instant(&comment.created_at).unwrap_or(i64::MIN);
        if latest.get(&key).is_some_and(|(held, _)| *held > at) {
            continue;
        }
        if !latest.contains_key(&key) {
            order.push(key.clone());
        }
        let stale = verdict_stale(&comment.created_at, commits);
        latest.insert(key, (at, ReviewOutcome { actor: comment.author.clone(), outcome, stale }));
    }
    order
        .into_iter()
        .filter_map(|key| latest.remove(&key).map(|(_, entry)| (key, entry)))
        .filter(|(_, entry)| entry.outcome != "dismissed")
        .collect()
}

/// The Reviewers row: requested reviewers with their verdicts, then reviewers the host no
/// longer lists as requested but who left one. Logins compare case-insensitively.
pub fn reviewer_entries(
    reviewers: &[Actor],
    outcomes: &[(String, ReviewOutcome)],
) -> Vec<(Actor, Option<ReviewOutcome>)> {
    let find = |login: &str| {
        outcomes.iter().find(|(_, e)| e.actor.as_ref().is_some_and(|a| a.login.eq_ignore_ascii_case(login)))
    };
    let mut entries: Vec<(Actor, Option<ReviewOutcome>)> =
        reviewers.iter().map(|actor| (actor.clone(), find(&actor.login).map(|(_, e)| e.clone()))).collect();
    for (key, entry) in outcomes {
        let listed =
            entry.actor.as_ref().is_some_and(|a| reviewers.iter().any(|r| r.login.eq_ignore_ascii_case(&a.login)));
        if !listed {
            let actor = entry.actor.clone().unwrap_or(Actor { login: key.clone(), ..Actor::default() });
            entries.push((actor, Some(entry.clone())));
        }
    }
    entries
}

/// `visibleBody`: a body of nothing but HTML comments is empty.
pub fn visible_body(body: &str) -> Option<String> {
    let trimmed = body.trim();
    (!strip_html_comments(trimmed).trim().is_empty()).then(|| trimmed.to_owned())
}

pub fn strip_html_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// One conversation: a loose comment, or a review thread and its replies.
#[derive(Clone, Debug)]
pub enum Conversation {
    Comment(Comment),
    Thread { thread: ReviewThread, root: Comment, replies: Vec<Comment>, more_replies: usize, has_more: bool },
}

impl Conversation {
    pub fn id(&self) -> &str {
        match self {
            Conversation::Comment(c) => &c.id,
            Conversation::Thread { thread, .. } => &thread.id,
        }
    }

    pub fn comments(&self) -> impl Iterator<Item = &Comment> {
        let (first, rest): (&Comment, &[Comment]) = match self {
            Conversation::Comment(c) => (c, &[]),
            Conversation::Thread { root, replies, .. } => (root, replies),
        };
        std::iter::once(first).chain(rest)
    }

    pub fn thread(&self) -> Option<&ReviewThread> {
        match self {
            Conversation::Thread { thread, .. } => Some(thread),
            Conversation::Comment(_) => None,
        }
    }

    /// `isPullRequestCommentFinished`: resolved, or its review was dismissed.
    pub fn finished(&self) -> bool {
        let root = self.comments().next().expect("a conversation has a first comment");
        self.thread().is_some_and(|t| t.is_resolved)
            || review_outcome(root.review_state.as_deref()) == Some("dismissed")
    }
}

/// `pullRequestConversationItems`: threads gather their comments and sit at their first
/// comment's time among the loose comments, whose order is kept.
pub fn conversations(comments: &[Comment], threads: &[ReviewThread]) -> Vec<Conversation> {
    let by_id: HashMap<&str, &Comment> = comments.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut claimed: HashSet<String> = HashSet::new();
    let mut gathered: Vec<(i64, Conversation)> = Vec::new();
    for thread in threads {
        let mut ordered: Vec<Comment> = thread
            .comments
            .iter()
            .map(|c| {
                by_id.get(c.id.as_str()).map(|c| (*c).clone()).unwrap_or_else(|| Comment {
                    id: c.id.clone(),
                    kind: "review-comment".into(),
                    author: c.author.clone(),
                    body: c.body.clone(),
                    created_at: c.created_at.clone(),
                    url: c.url.clone(),
                    path: Some(thread.path.clone()),
                    review_state: None,
                    reactions: c.reactions.clone(),
                    can_edit: c.can_edit,
                })
            })
            .collect();
        ordered.sort_by_key(|c| instant(&c.created_at).unwrap_or(0));
        if ordered.is_empty() {
            continue;
        }
        claimed.extend(ordered.iter().map(|c| c.id.clone()));
        let more_replies = thread.comment_count.unwrap_or(ordered.len()).saturating_sub(ordered.len());
        let at = instant(&ordered[0].created_at).unwrap_or(0);
        let root = ordered.remove(0);
        gathered.push((
            at,
            Conversation::Thread {
                has_more: more_replies > 0 || thread.next_comments_cursor.is_some(),
                thread: thread.clone(),
                root,
                replies: ordered,
                more_replies,
            },
        ));
    }
    gathered.sort_by_key(|(at, _)| *at);
    let mut threads = gathered.into_iter().peekable();
    let mut items = Vec::new();
    for comment in comments.iter().filter(|c| !claimed.contains(&c.id)) {
        let at = instant(&comment.created_at).unwrap_or(0);
        while threads.peek().is_some_and(|(thread_at, _)| *thread_at < at) {
            items.push(threads.next().expect("peeked").1);
        }
        items.push(Conversation::Comment(comment.clone()));
    }
    items.extend(threads.map(|(_, c)| c));
    items
}

/// `filterPullRequestConversationItems`: bot chatter and finished conversations are hidden
/// unless asked for; a bot's verdict is not chatter.
pub fn filter_conversations(items: &[Conversation], bots: bool, resolved: bool) -> Vec<Conversation> {
    items
        .iter()
        .filter(|item| {
            item.comments().any(|comment| {
                let verdict =
                    matches!(review_outcome(comment.review_state.as_deref()), Some("approved" | "changes-requested"));
                let finished = item.thread().is_some_and(|t| t.is_resolved)
                    || review_outcome(comment.review_state.as_deref()) == Some("dismissed");
                (bots || !comment.author.as_ref().is_some_and(|a| a.is_bot) || verdict) && (resolved || !finished)
            })
        })
        .cloned()
        .collect()
}

/// One Timeline event (`PullRequestTimelineEvent`).
#[derive(Clone, Debug)]
pub struct Event {
    pub id: String,
    pub at: String,
    pub kind: EventKind,
    pub title: String,
    pub body: Option<String>,
    pub url: Option<String>,
    pub actor: Option<Actor>,
    pub commit_authors: Vec<Actor>,
    pub stat: Option<(u64, u64)>,
    pub path: Option<String>,
    pub review_state: Option<String>,
    pub thread: Option<TimelineThread>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Opened,
    Commit,
    Comment,
    Review,
    Merged,
    Closed,
}

#[derive(Clone, Debug)]
pub struct TimelineThread {
    pub path: String,
    pub line: Option<u64>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub replies: Vec<Event>,
    pub more_replies: usize,
    pub has_more: bool,
    pub url: Option<String>,
}

fn lifecycle(id: &str, at: &str, kind: EventKind, title: String, actor: Option<Actor>) -> Event {
    Event {
        id: id.into(),
        at: at.into(),
        kind,
        title,
        body: None,
        url: None,
        actor,
        commit_authors: Vec::new(),
        stat: None,
        path: None,
        review_state: None,
        thread: None,
    }
}

fn comment_event(comment: &Comment) -> Event {
    let review = comment.kind == "review";
    Event {
        id: comment.id.clone(),
        at: comment.created_at.clone(),
        kind: if review { EventKind::Review } else { EventKind::Comment },
        title: if review { "reviewed" } else { "commented" }.into(),
        body: visible_body(&comment.body),
        url: comment.url.clone(),
        actor: comment.author.clone(),
        commit_authors: Vec::new(),
        stat: None,
        path: comment.path.clone(),
        review_state: comment.review_state.clone(),
        thread: None,
    }
}

/// `buildPullRequestTimeline`, newest first. Merged wins over closed: GitHub sets both on a merge.
pub fn build_timeline(detail: &Detail, activity: &Activity, term: &str) -> Vec<Event> {
    let title = capitalize(term);
    let mut events = vec![lifecycle(
        "created",
        &detail.created_at,
        EventKind::Opened,
        format!("opened this {term}"),
        activity.author.clone().unwrap_or_else(|| detail.author.clone()),
    )];
    events.extend(activity.commits.iter().map(|commit| Event {
        title: format!("Commit {}", short_oid(&commit.oid)),
        body: (!commit.message_headline.is_empty()).then(|| commit.message_headline.clone()),
        actor: commit.authors.first().cloned(),
        commit_authors: commit.authors.clone(),
        stat: commit.additions.zip(commit.deletions),
        ..lifecycle(&commit.oid, &commit.committed_date, EventKind::Commit, String::new(), None)
    }));
    for item in conversations(&activity.comments, &activity.review_threads) {
        match item {
            Conversation::Comment(comment) => events.push(comment_event(&comment)),
            Conversation::Thread { thread, root, replies, more_replies, has_more } => events.push(Event {
                thread: Some(TimelineThread {
                    url: std::iter::once(&root).chain(&replies).find_map(|c| c.url.clone()),
                    path: thread.path,
                    line: thread.line,
                    is_resolved: thread.is_resolved,
                    is_outdated: thread.is_outdated,
                    replies: replies.iter().map(comment_event).collect(),
                    more_replies,
                    has_more,
                }),
                ..comment_event(&root)
            }),
        }
    }
    match (&detail.merged_at, &detail.closed_at) {
        (Some(at), _) => events.push(lifecycle("merged", at, EventKind::Merged, format!("{title} merged"), None)),
        (None, Some(at)) => events.push(lifecycle("closed", at, EventKind::Closed, format!("{title} closed"), None)),
        _ => {}
    }
    events.sort_by_key(|e| std::cmp::Reverse(instant(&e.at).unwrap_or(0)));
    events
}

/// A Timeline row: one event, or a run of plain comments folded together.
#[derive(Clone, Debug)]
pub enum TimelineRow {
    Event(Event),
    Comments(Vec<Event>),
}

/// `groupPullRequestTimelineConversations`.
pub fn group_timeline(events: Vec<Event>) -> Vec<TimelineRow> {
    let mut rows: Vec<TimelineRow> = Vec::new();
    for event in events {
        let plain = matches!(event.kind, EventKind::Comment | EventKind::Review)
            && event.thread.is_none()
            && review_outcome(event.review_state.as_deref()).is_none();
        match (plain, rows.last_mut()) {
            (true, Some(TimelineRow::Comments(run))) => run.push(event),
            (true, _) => rows.push(TimelineRow::Comments(vec![event])),
            (false, _) => rows.push(TimelineRow::Event(event)),
        }
    }
    rows
}

pub fn short_oid(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
}

pub fn capitalize(term: &str) -> String {
    let mut chars = term.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// The host's word for a change request: "pull request", or GitLab's "merge request".
pub fn terminology(provider: &str) -> (&'static str, &'static str) {
    match provider {
        "gitlab" => ("merge request", "MR"),
        _ => ("pull request", "PR"),
    }
}

/// `checkoutCommand` for the hosts whose command needs only the number.
pub fn checkout_command(provider: &str, number: u64) -> Option<String> {
    match provider {
        "github" => Some(format!("gh pr checkout {number}")),
        "gitlab" => Some(format!("glab mr checkout {number}")),
        "azure-devops" => Some(format!("az repos pr checkout --id {number}")),
        _ => None,
    }
}

/// `readableFailure`: the host's own words, or the hint when they say nothing useful.
pub fn readable_failure(message: &str, hint: &str) -> String {
    let detail = match message.find(" failed:") {
        Some(index) if message.starts_with("Pull request operation ") => message[index + 8..].trim(),
        _ => message.trim(),
    };
    let lower = detail.trim_end_matches('.').to_lowercase();
    let noise = lower.is_empty()
        || lower == "unknown error"
        || lower.ends_with("command failed")
        || lower.ends_with("cli failed")
        || lower.ends_with("api failed")
        || lower.starts_with("exited with")
        || lower.starts_with("exit with");
    if noise {
        return hint.into();
    }
    if detail.chars().count() <= 320 {
        detail.into()
    } else {
        format!("{}…", detail.chars().take(319).collect::<String>())
    }
}

/// The failure toast's title and hint for an action (`actionFailureLabels`, `actionFailureHints`).
pub fn action_failure(action: &str, term: &str) -> (String, String) {
    let write = "The host refused it. Check that you have write access to this repository.";
    match action {
        "merge" => (
            format!("Could not merge this {term}"),
            "The host refused the merge. Check that you have write access, that the checks it requires have passed, and that the branch is not conflicting.".into(),
        ),
        "ready" => ("Could not mark this ready for review".into(), write.into()),
        "draft" => ("Could not convert this to a draft".into(), write.into()),
        "close" => (
            format!("Could not close this {term}"),
            "The host refused it. Check that you have write access, or that you opened it.".into(),
        ),
        "reopen" => (
            format!("Could not reopen this {term}"),
            "The host refused it. Check that you have write access, and that the branch still exists.".into(),
        ),
        "enable-auto-merge" => (
            "Could not turn on auto-merge".into(),
            "The host refused it. Check that this repository allows auto-merge, that you have write access, and that there is something left for it to wait on.".into(),
        ),
        "disable-auto-merge" => (
            "Could not turn off auto-merge".into(),
            "The host refused it. Check that you have write access, and that the merge has not already happened.".into(),
        ),
        "revert" => (
            format!("Could not open a revert {term}"),
            format!("The host refused it. Check that you have write access and that this {term} was merged on the host."),
        ),
        "update-branch" => (
            "Could not update this branch".into(),
            "The host refused it. Check that you have write access to the branch (a fork branch also needs its author to allow edits from maintainers), and that it does not conflict with the base.".into(),
        ),
        "delete-branch" => (
            "Could not delete this branch".into(),
            "The host refused it. Check that you have write access to the source repository and that the branch has not moved since this page loaded.".into(),
        ),
        _ => (
            "Could not approve workflows".into(),
            "The host refused it. Check that you have Actions write access and that these workflow runs are still awaiting approval.".into(),
        ),
    }
}

/// `pullRequestActionNeedsHostRefresh`.
pub fn needs_host_refresh(action: &str) -> bool {
    matches!(action, "update-branch" | "approve-workflows")
}

/// A change request link's host, repository and number (`parseChangeRequestUrl`), for the
/// hosts whose links name the repository in the path: GitHub's `/pull/` and GitLab's `/-/merge_requests/`.
pub fn parse_change_request_url(url: &str) -> Option<(String, String, u64)> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let path = path.split(['?', '#']).next()?;
    let (repository, tail) = path.split_once("/-/merge_requests/").or_else(|| {
        let mut parts = path.splitn(3, '/');
        let (owner, name, tail) = (parts.next()?, parts.next()?, parts.next()?);
        let tail = tail.strip_prefix("pull/")?;
        Some((&path[..owner.len() + 1 + name.len()], tail))
    })?;
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    let after = &tail[digits.len()..];
    if repository.split('/').count() < 2 || !(after.is_empty() || after.starts_with('/')) {
        return None;
    }
    Some((host.to_lowercase(), repository.to_owned(), digits.parse().ok().filter(|n| *n > 0)?))
}

/// The project whose repository holds the change request: its repository key is `host/repository`.
pub fn project_for_link<'a>(
    projects: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    host: &str,
    repository: &str,
) -> Option<&'a str> {
    let wanted = format!("{host}/{repository}").to_lowercase();
    projects.into_iter().find(|(_, key)| key.is_some_and(|k| k.to_lowercase() == wanted)).map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail(patch: Value) -> Detail {
        let mut value = json!({
            "provider": "github", "projectId": "p1", "repository": "acme/app", "number": 7, "title": "Add",
            "url": "https://github.com/acme/app/pull/7", "state": "open", "isDraft": false, "mergeability": "mergeable",
            "headBranch": "feature", "baseBranch": "main", "createdAt": "2026-10-01T00:00:00.000Z",
            "updatedAt": "2026-10-02T00:00:00.000Z", "mergedAt": null, "closedAt": null,
            "capabilities": {"diff": true, "actions": ["merge", "ready", "draft", "close", "reopen", "enable-auto-merge", "disable-auto-merge", "revert", "delete-branch"], "mergeMethods": ["merge", "squash", "rebase"], "deleteBranch": true},
            "viewerPermissions": {"actions": ["merge", "ready", "draft", "close", "reopen", "enable-auto-merge", "disable-auto-merge", "revert", "delete-branch"]},
            "mergeCapabilities": {"merge": true, "squash": true, "rebase": false},
        });
        for (key, v) in patch.as_object().unwrap() {
            value[key] = v.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    fn comment(id: &str, login: &str, at: &str, patch: Value) -> Comment {
        let mut value =
            json!({"id": id, "kind": "issue-comment", "author": {"login": login}, "body": "hi", "createdAt": at});
        for (key, v) in patch.as_object().unwrap() {
            value[key] = v.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn merge_method_prefers_the_panel_pick_then_the_project_default_then_the_last_one() {
        let allowed = allowed_merge_methods(&detail(json!({})));
        assert_eq!(allowed, ["merge", "squash"], "the repository disallows rebase");
        assert_eq!(resolve_merge_method(&allowed, Some("squash"), Some("merge"), "merge"), "squash");
        assert_eq!(resolve_merge_method(&allowed, None, Some("squash"), "merge"), "squash");
        assert_eq!(resolve_merge_method(&allowed, Some("rebase"), Some("rebase"), "squash"), "squash");
        assert_eq!(resolve_merge_method(&allowed, None, None, "rebase"), "merge", "falls back to the first allowed");
        assert_eq!(resolve_merge_method(&[], None, None, "squash"), "merge");
    }

    #[test]
    fn the_primary_control_follows_where_the_pull_request_stands() {
        let pending = Some("pending");
        assert_eq!(primary_control(&detail(json!({"state": "merged"})), None), Some(Primary::Merged));
        assert_eq!(primary_control(&detail(json!({"state": "closed"})), None), Some(Primary::Closed));
        assert_eq!(primary_control(&detail(json!({"mergeability": "conflicting"})), None), Some(Primary::Resolve));
        assert_eq!(primary_control(&detail(json!({"isDraft": true})), None), Some(Primary::Ready));
        assert_eq!(primary_control(&detail(json!({"autoMergeEnabled": true})), None), Some(Primary::AutoMergeArmed));
        assert_eq!(
            primary_control(&detail(json!({"autoMergeEnabled": false})), pending),
            Some(Primary::EnableAutoMerge)
        );
        assert_eq!(primary_control(&detail(json!({})), pending), Some(Primary::Merge), "unreported auto-merge merges");
        let viewer_cannot = detail(json!({"viewerPermissions": {"actions": []}}));
        assert_eq!(primary_control(&viewer_cannot, None), None);
        let no_method = detail(json!({"mergeCapabilities": {"merge": false, "squash": false, "rebase": false}}));
        assert_eq!(primary_control(&no_method, None), None);
    }

    #[test]
    fn the_menu_offers_what_the_state_allows() {
        let open = detail(json!({}));
        let actions = menu_actions(&open, primary_control(&open, None));
        assert!(actions.draft_toggle && actions.auto_merge && actions.merge_methods && actions.close);
        assert!(!actions.merge_now && !actions.reopen && !actions.revert && !actions.delete_branch);
        let armed = detail(json!({"autoMergeEnabled": true}));
        let actions = menu_actions(&armed, primary_control(&armed, None));
        assert!(actions.merge_now && actions.disable_auto_merge && !actions.auto_merge);
        let draft = detail(json!({"isDraft": true}));
        assert!(!menu_actions(&draft, primary_control(&draft, None)).draft_toggle, "the header already offers ready");
        let closed = detail(json!({"state": "closed"}));
        let actions = menu_actions(&closed, primary_control(&closed, None));
        assert!(actions.reopen && actions.delete_branch && !actions.close);
        let merged = detail(json!({"state": "merged"}));
        assert!(menu_actions(&merged, None).revert);
    }

    #[test]
    fn editing_follows_the_hosts_answer_then_authorship() {
        let caps = json!({"actions": [], "mergeMethods": [], "edit": {"changeRequest": true, "comment": true}});
        let mine = detail(json!({"capabilities": caps.clone(), "viewer": "OctoCat", "author": {"login": "octocat"}}));
        assert!(can_edit_change_request(&mine), "the author, in any case");
        let theirs = detail(json!({"capabilities": caps.clone(), "viewer": "me", "author": {"login": "you"}}));
        assert!(can_edit_change_request(&theirs), "someone who may merge");
        let refused = detail(json!({
            "capabilities": caps.clone(), "viewer": "octocat", "author": {"login": "octocat"},
            "viewerPermissions": {"actions": ["merge"], "editChangeRequest": false},
        }));
        assert!(!can_edit_change_request(&refused), "the host's own answer wins");
        assert!(!can_edit_change_request(&detail(json!({}))), "a host that cannot rewrite one");
        let me = Actor { login: "octocat".into(), ..Actor::default() };
        assert!(can_edit_comment(&mine, "review-comment", Some(&me), None));
        assert!(!can_edit_comment(&mine, "review", Some(&me), None), "a review's summary");
        assert!(!can_edit_comment(&mine, "issue-comment", Some(&me), Some(false)));
        let thread: ReviewThread =
            serde_json::from_value(json!({"id": "t", "path": "a", "canResolve": false})).unwrap();
        assert!(!can_resolve_thread(&mine, &thread));
        let open: ReviewThread = serde_json::from_value(json!({"id": "t", "path": "a"})).unwrap();
        assert!(can_resolve_thread(&mine, &open) && open.side == "right");
    }

    #[test]
    fn a_branch_behind_its_base_offers_the_updates_both_sides_allow() {
        let behind = detail(json!({
            "baseComparison": "behind", "behindBy": 3,
            "capabilities": {"actions": [], "mergeMethods": [], "updateMethods": ["merge", "rebase"]},
            "viewerPermissions": {"actions": [], "updateMethods": ["merge"]},
        }));
        assert_eq!(base_freshness(&behind), Some((Some(3), vec!["merge".to_owned()])));
        let conflicting = detail(json!({"baseComparison": "behind", "mergeability": "conflicting"}));
        assert_eq!(base_freshness(&conflicting), None, "a conflicting branch shows Resolve instead");
        assert_eq!(base_freshness(&detail(json!({"baseComparison": "up-to-date"}))), None);
    }

    #[test]
    fn a_stack_merges_up_to_the_shown_layer_and_rebases_every_unmerged_one() {
        let stack: Stack = serde_json::from_value(json!({
            "number": 9, "base": "main",
            "layers": [
                {"number": 1, "headBranch": "a", "state": "merged", "headSha": "a1"},
                {"number": 2, "headBranch": "b", "state": "open", "headSha": "b1"},
                {"number": 3, "headBranch": "c", "state": "open", "headSha": "c1"},
                {"number": 4, "headBranch": "d", "state": "open", "isDraft": true},
            ],
        }))
        .unwrap();
        let plan = stack_plan(&stack, 3);
        assert_eq!((plan.position, plan.merge.as_slice(), plan.merge_disabled), (3, &[2, 3][..], false));
        assert_eq!(plan.rebase, [2, 3, 4]);
        assert!(plan.rebase_disabled, "a layer without a known head cannot be rebased safely");
        assert!(stack_plan(&stack, 4).merge_disabled, "a draft layer blocks the merge");
        assert_eq!(
            stack_heads(&stack, &[2, 3]),
            json!([{"number": 2, "headSha": "b1"}, {"number": 3, "headSha": "c1"}])
        );
    }

    #[test]
    fn checks_group_by_what_they_need() {
        let checks: Vec<Check> = ["failure", "pending", "success", "action-required", "skipped"]
            .iter()
            .map(|status| Check { name: (*status).into(), status: (*status).into(), url: None })
            .collect();
        let (attention, running, completed) = group_checks(&checks);
        let names = |list: Vec<&Check>| list.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(attention), ["failure", "action-required"]);
        assert_eq!(names(running), ["pending"]);
        assert_eq!(names(completed), ["success", "skipped"]);
    }

    #[test]
    fn checks_summarize_by_their_worst_status() {
        let checks = |statuses: &[(&str, Option<&str>)]| -> Vec<Check> {
            statuses
                .iter()
                .map(|(status, url)| Check { name: "ci".into(), status: (*status).into(), url: url.map(Into::into) })
                .collect()
        };
        assert_eq!(checks_state(&[]), None);
        assert_eq!(checks_state(&checks(&[("success", None), ("failure", None)])), Some("failing"));
        assert_eq!(checks_state(&checks(&[("success", None), ("action-required", None)])), Some("pending"));
        assert_eq!(checks_state(&checks(&[("skipped", None)])), None);
        assert_eq!(summarize_checks(&checks(&[("success", None), ("success", None)])), "All checks passed");
        assert_eq!(summarize_checks(&checks(&[("success", None), ("pending", None)])), "1 of 2 running");
        assert_eq!(summarize_checks(&checks(&[("failure", None), ("pending", None)])), "1 of 2 failing");
        let run = Some("https://github.com/acme/app/actions/runs/42");
        assert_eq!(summarize_checks(&checks(&[("action-required", run)])), "1 workflow awaiting approval");
        assert_eq!(
            summarize_checks(&checks(&[("action-required", run), ("action-required", None)])),
            "1 workflow and 1 check awaiting action"
        );
        assert_eq!(check_status_label(&checks(&[("action-required", run)])[0]), "Awaiting approval");
        assert_eq!(summarize_checks(&checks(&[("success", None), ("skipped", None)])), "1 of 2 passing");
    }

    #[test]
    fn each_reviewer_keeps_only_their_newest_verdict() {
        let review = |id: &str, login: &str, at: &str, state: &str| {
            comment(id, login, at, json!({"kind": "review", "reviewState": state}))
        };
        let comments = [
            review("1", "ana", "2026-10-01T10:00:00Z", "CHANGES_REQUESTED"),
            review("2", "ana", "2026-10-03T10:00:00Z", "APPROVED"),
            review("3", "bo", "2026-10-01T09:00:00Z", "APPROVED"),
            review("4", "cy", "2026-10-01T09:00:00Z", "APPROVED"),
            review("5", "cy", "2026-10-02T09:00:00Z", "DISMISSED"),
            review("6", "di", "2026-10-02T09:00:00Z", "COMMENTED"),
        ];
        let commits: Vec<Commit> =
            vec![serde_json::from_value(json!({"oid": "abc", "committedDate": "2026-10-02T00:00:00Z"})).unwrap()];
        let outcomes = latest_review_outcomes(&comments, &commits);
        let summary: Vec<(&str, &str, bool)> = outcomes.iter().map(|(k, e)| (k.as_str(), e.outcome, e.stale)).collect();
        assert_eq!(summary, [("ana", "approved", false), ("bo", "approved", true)]);
        let reviewers =
            [Actor { login: "BO".into(), ..Actor::default() }, Actor { login: "ed".into(), ..Actor::default() }];
        let entries = reviewer_entries(&reviewers, &outcomes);
        let logins: Vec<(&str, Option<&str>)> =
            entries.iter().map(|(a, e)| (a.login.as_str(), e.as_ref().map(|e| e.outcome))).collect();
        assert_eq!(logins, [("BO", Some("approved")), ("ed", None), ("ana", Some("approved"))]);
    }

    #[test]
    fn threads_sit_among_loose_comments_at_their_first_comment() {
        let comments = [
            comment("a", "ana", "2026-10-01T00:00:00Z", json!({})),
            comment("t1", "bo", "2026-10-02T00:00:00Z", json!({"kind": "review-comment"})),
            comment("b", "ana", "2026-10-03T00:00:00Z", json!({})),
        ];
        let threads: Vec<ReviewThread> = serde_json::from_value(json!([{
            "id": "th", "path": "src/a.rs", "line": 3, "isResolved": true, "isOutdated": false, "commentCount": 4,
            "comments": [
                {"id": "t2", "author": {"login": "cy"}, "body": "reply", "createdAt": "2026-10-02T05:00:00Z"},
                {"id": "t1", "author": {"login": "bo"}, "body": "root", "createdAt": "2026-10-02T00:00:00Z"},
            ],
        }]))
        .unwrap();
        let items = conversations(&comments, &threads);
        let ids: Vec<&str> = items.iter().map(Conversation::id).collect();
        assert_eq!(ids, ["a", "th", "b"]);
        let Conversation::Thread { root, replies, more_replies, has_more, .. } = &items[1] else { panic!() };
        assert_eq!((root.id.as_str(), replies[0].id.as_str(), *more_replies, *has_more), ("t1", "t2", 2, true));
        assert_eq!(replies[0].kind, "review-comment", "a thread-only comment is normalized");
        assert!(items[1].finished());
        assert_eq!(filter_conversations(&items, false, false).len(), 2, "resolved threads hide by default");
        assert_eq!(filter_conversations(&items, false, true).len(), 3);
    }

    #[test]
    fn bot_chatter_hides_but_a_bot_verdict_stays() {
        let bot = json!({"login": "ci[bot]", "isBot": true});
        let items = conversations(
            &[
                comment("1", "x", "2026-10-01T00:00:00Z", json!({"author": bot})),
                comment(
                    "2",
                    "x",
                    "2026-10-02T00:00:00Z",
                    json!({"author": bot, "kind": "review", "reviewState": "APPROVED"}),
                ),
            ],
            &[],
        );
        let shown: Vec<String> = filter_conversations(&items, false, false).iter().map(|c| c.id().to_owned()).collect();
        assert_eq!(shown, ["2"]);
        assert_eq!(filter_conversations(&items, true, false).len(), 2);
    }

    #[test]
    fn the_timeline_runs_newest_first_and_folds_plain_comments() {
        let activity: Activity = serde_json::from_value(json!({
            "comments": [
                {"id": "c1", "kind": "issue-comment", "author": {"login": "ana"}, "body": "one", "createdAt": "2026-10-03T00:00:00Z"},
                {"id": "c2", "kind": "issue-comment", "author": {"login": "bo"}, "body": "<!-- marker -->", "createdAt": "2026-10-04T00:00:00Z"},
                {"id": "r1", "kind": "review", "author": {"login": "cy"}, "body": "", "createdAt": "2026-10-05T00:00:00Z", "reviewState": "APPROVED"},
            ],
            "commentCount": 3, "commentsTruncated": false, "reviewThreads": [],
            "commits": [{"oid": "abcdef1234", "messageHeadline": "Fix", "committedDate": "2026-10-02T00:00:00Z", "additions": 3, "deletions": 1}],
        }))
        .unwrap();
        let both =
            detail(json!({"state": "merged", "mergedAt": "2026-10-06T00:00:00Z", "closedAt": "2026-10-06T00:00:00Z"}));
        let events = build_timeline(&both, &activity, "pull request");
        let ids: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["merged", "r1", "c2", "c1", "abcdef1234", "created"], "merged wins over closed");
        assert_eq!(events[0].title, "Pull request merged");
        assert_eq!(events[2].body, None, "a marker-only body is empty");
        assert_eq!((events[4].title.as_str(), events[4].stat), ("Commit abcdef1", Some((3, 1))));
        let rows = group_timeline(events);
        let shape: Vec<String> = rows
            .iter()
            .map(|row| match row {
                TimelineRow::Event(e) => e.id.clone(),
                TimelineRow::Comments(run) => format!("{}x", run.len()),
            })
            .collect();
        assert_eq!(shape, ["merged", "r1", "2x", "abcdef1234", "created"], "a verdict stands alone");
    }

    #[test]
    fn failures_keep_the_hosts_words_unless_they_are_noise() {
        let hint = "Check access.";
        assert_eq!(
            readable_failure("Pull request operation merge failed: Base branch was modified", hint),
            "Base branch was modified"
        );
        assert_eq!(readable_failure("Pull request operation merge failed: GitHub CLI command failed.", hint), hint);
        assert_eq!(readable_failure("exited with code 1", hint), hint);
        assert_eq!(readable_failure("", hint), hint);
        assert_eq!(readable_failure(&"x".repeat(400), hint).chars().count(), 320);
    }

    #[test]
    fn change_request_links_name_their_host_repository_and_number() {
        let parse = parse_change_request_url;
        assert_eq!(
            parse("https://github.com/Acme/App/pull/12/files"),
            Some(("github.com".into(), "Acme/App".into(), 12))
        );
        assert_eq!(
            parse("https://GitHub.com/acme/app/pull/12#top"),
            Some(("github.com".into(), "acme/app".into(), 12))
        );
        assert_eq!(
            parse("https://gitlab.com/group/sub/proj/-/merge_requests/5"),
            Some(("gitlab.com".into(), "group/sub/proj".into(), 5))
        );
        assert_eq!(parse("https://github.com/acme/app/issues/12"), None);
        assert_eq!(parse("https://github.com/acme/app/pull/12x"), None);
        assert_eq!(parse("https://github.com/acme/app/pull/0"), None);
        let projects = [("p1", Some("github.com/acme/web")), ("p2", Some("github.com/acme/app")), ("p3", None)];
        assert_eq!(project_for_link(projects, "github.com", "Acme/App"), Some("p2"));
        assert_eq!(project_for_link(projects, "gitlab.com", "acme/app"), None);
    }
}
