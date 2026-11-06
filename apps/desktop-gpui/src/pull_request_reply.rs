//! An action's reply (`pullRequests.runAction` with `replyWithSummary`) applied to what the
//! client holds, instead of reading it again (`packages/client-runtime/src/pullRequestReply.ts`
//! and the detail and list patches in `state/pullRequests.ts`).

use std::rc::Rc;

use serde_json::Value;

use crate::pull_request_detail::{Detail, Reference};
use crate::pull_requests::{Entry, State};
use crate::timestamp::parse_ms;

/// What staleness compares: the state, the host's update time and the server's read time.
struct Observed<'a> {
    state: State,
    updated_at: Option<&'a str>,
    observed_at: Option<f64>,
}

/// `isStalePullRequestReply`: older than what is shown by host update time, then server read
/// time, or a merge undone.
fn is_stale(shown: &Observed, reply: &Observed) -> bool {
    if shown.state == State::Merged && reply.state != State::Merged {
        return true;
    }
    if let (Some(shown), Some(reply)) = (shown.updated_at.and_then(parse_ms), reply.updated_at.and_then(parse_ms))
        && shown != reply
    {
        return reply < shown;
    }
    reply.observed_at.unwrap_or(f64::NEG_INFINITY) < shown.observed_at.unwrap_or(f64::NEG_INFINITY)
}

/// A `PullRequestSummary` reply: `None` when there is none or it cannot be read.
pub struct Reply<'a>(&'a Value, State);

impl<'a> Reply<'a> {
    pub fn read(value: &'a Value) -> Option<Self> {
        let state = serde_json::from_value(value.get("state")?.clone()).ok()?;
        Some(Self(value, state))
    }

    fn observed(&self) -> Observed<'a> {
        Observed { state: self.1, updated_at: self.0["updatedAt"].as_str(), observed_at: self.0["observedAt"].as_f64() }
    }

    fn text(&self, key: &str) -> Option<String> {
        self.0[key].as_str().map(str::to_owned)
    }

    fn count(&self, key: &str) -> Option<u64> {
        self.0[key].as_u64()
    }

    /// A present key, `null` included, decoded into `T`.
    fn present<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        serde_json::from_value(self.0.get(key)?.clone()).ok()
    }
}

/// The detail with the reply applied, `None` when the reply is older than it, and whether a read
/// must follow: reopening or a new head changes branch data and permissions the reply lacks.
pub fn patch_detail(detail: &Detail, reply: &Reply) -> (Option<Detail>, bool) {
    let shown = Observed { state: detail.state, updated_at: Some(&detail.updated_at), observed_at: detail.observed_at };
    let observed = reply.observed();
    if is_stale(&shown, &observed) {
        return (None, false);
    }
    let head = reply.text("headSha");
    let reread = (detail.state == State::Closed && observed.state == State::Open)
        || head.is_some_and(|head| detail.head_sha.as_ref() != Some(&head));
    let mut next = detail.clone();
    next.state = observed.state;
    let texts: [(&str, &mut String); 6] = [
        ("title", &mut next.title),
        ("url", &mut next.url),
        ("mergeability", &mut next.mergeability),
        ("headBranch", &mut next.head_branch),
        ("baseBranch", &mut next.base_branch),
        ("updatedAt", &mut next.updated_at),
    ];
    for (key, field) in texts {
        if let Some(value) = reply.text(key) {
            *field = value;
        }
    }
    for (key, field) in [
        ("additions", &mut next.additions),
        ("deletions", &mut next.deletions),
        ("changedFiles", &mut next.changed_files),
    ] {
        if let Some(value) = reply.count(key) {
            *field = value;
        }
    }
    if let Some(author) = reply.present("author") {
        next.author = author;
    }
    if let Some(draft) = reply.0["isDraft"].as_bool() {
        next.is_draft = draft;
    }
    if let Some(at) = reply.present("mergedAt") {
        next.merged_at = at;
    }
    if let Some(at) = reply.present("closedAt") {
        next.closed_at = at;
    }
    if let Some(enabled) = reply.0["autoMergeEnabled"].as_bool() {
        next.auto_merge_enabled = Some(enabled);
    }
    if let Some(method) = reply.text("autoMergeMethod") {
        next.auto_merge_method = Some(method);
    }
    if reply.0["autoMergeEnabled"].as_bool() == Some(false) {
        next.auto_merge_method = None;
    }
    (Some(next), reread)
}

/// `samePullRequest`: a missing host is not a wildcard.
fn same(entry: &Entry, reference: &Reference) -> bool {
    entry.project_id == reference.project_id
        && reference.host.as_ref().is_some_and(|host| host.eq_ignore_ascii_case(&entry.host))
        && entry.repository.eq_ignore_ascii_case(&reference.repository)
        && entry.number == reference.number
}

/// The listing a reply lands in: its state filter and `draft` filter (`listAccepts`).
pub struct Listing<'a> {
    pub state: &'a str,
    pub draft: Option<&'a str>,
}

impl Listing<'_> {
    fn accepts(&self, state: State, is_draft: bool) -> bool {
        (self.state == "all" || self.state == state.as_str())
            && (self.draft != Some("only") || is_draft)
            && (self.draft != Some("hide") || !is_draft)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ListPatch {
    Unchanged,
    Patched,
    /// The reply adds the pull request to the listing, which only a read can place.
    Refresh,
}

/// `patchPullRequestListEntries`: rows newest first, as the host lists them.
pub fn patch_entries(
    listing: &Listing,
    entries: &mut Vec<Rc<Entry>>,
    reference: &Reference,
    reply: &Reply,
) -> ListPatch {
    let observed = reply.observed();
    let Some(index) = entries.iter().position(|entry| same(entry, reference)) else {
        let draft = reply.0["isDraft"].as_bool().unwrap_or(false);
        return if listing.accepts(observed.state, draft) { ListPatch::Refresh } else { ListPatch::Unchanged };
    };
    let entry = &entries[index];
    let shown = Observed { state: entry.state, updated_at: Some(&entry.updated_at), observed_at: entry.observed_at };
    if is_stale(&shown, &observed) {
        return ListPatch::Unchanged;
    }
    let mut next = (**entry).clone();
    next.state = observed.state;
    let texts: [(&str, &mut String); 6] = [
        ("title", &mut next.title),
        ("url", &mut next.url),
        ("headBranch", &mut next.head_branch),
        ("baseBranch", &mut next.base_branch),
        ("mergeability", &mut next.mergeability),
        ("updatedAt", &mut next.updated_at),
    ];
    for (key, field) in texts {
        if let Some(value) = reply.text(key) {
            *field = value;
        }
    }
    if let Some(author) = reply.present("author") {
        next.author = author;
    }
    if let Some(draft) = reply.0["isDraft"].as_bool() {
        next.is_draft = draft;
    }
    next.updated_ms = parse_ms(&next.updated_at);
    if listing.accepts(next.state, next.is_draft) {
        entries[index] = Rc::new(next);
    } else {
        entries.remove(index);
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.updated_ms));
    ListPatch::Patched
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn entry(number: u64, state: &str, updated: &str) -> Rc<Entry> {
        let mut entry: Entry = serde_json::from_value(json!({
            "provider": "github", "host": "github.com", "projectId": "p", "repository": "o/r", "number": number,
            "title": "T", "url": "u", "author": null, "headBranch": "h", "baseBranch": "main", "state": state,
            "isDraft": false, "mergeability": "mergeable", "additions": 0, "deletions": 0,
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": updated, "viewerReviewRequested": false,
        }))
        .unwrap();
        entry.updated_ms = parse_ms(updated);
        Rc::new(entry)
    }

    fn reference(number: u64) -> Reference {
        Reference { project_id: "p".into(), host: Some("GitHub.com".into()), repository: "O/R".into(), number }
    }

    #[test]
    fn a_reply_is_stale_behind_what_is_shown() {
        let at = |state, updated_at, observed_at| Observed { state, updated_at, observed_at };
        let jan = Some("2026-01-01T00:00:00Z");
        let feb = Some("2026-02-01T00:00:00Z");
        assert!(is_stale(&at(State::Merged, feb, None), &at(State::Open, feb, Some(9.))), "a merge is never undone");
        assert!(is_stale(&at(State::Open, feb, None), &at(State::Closed, jan, Some(9.))), "the host's time wins");
        assert!(!is_stale(&at(State::Open, jan, Some(9.)), &at(State::Closed, feb, None)));
        assert!(is_stale(&at(State::Open, jan, Some(9.)), &at(State::Closed, jan, Some(1.))), "then the read time");
        assert!(!is_stale(&at(State::Open, jan, None), &at(State::Closed, jan, None)));
    }

    #[test]
    fn a_reply_patches_its_row_and_leaves_a_listing_it_no_longer_fits() {
        let mut entries = vec![entry(1, "open", "2026-01-02T00:00:00Z"), entry(2, "open", "2026-01-03T00:00:00Z")];
        let reply = json!({"state": "closed", "title": "Renamed", "updatedAt": "2026-01-04T00:00:00Z"});
        let reply = Reply::read(&reply).unwrap();
        let all = Listing { state: "all", draft: None };
        assert_eq!(patch_entries(&all, &mut entries, &reference(1), &reply), ListPatch::Patched);
        assert_eq!((entries[0].number, entries[0].title.as_str(), entries[0].state), (1, "Renamed", State::Closed));
        let open = Listing { state: "open", draft: None };
        assert_eq!(patch_entries(&open, &mut entries, &reference(1), &reply), ListPatch::Patched);
        assert_eq!(entries.iter().map(|e| e.number).collect::<Vec<_>>(), [2], "a closed row leaves Open");
        assert_eq!(
            patch_entries(&all, &mut entries, &reference(1), &reply),
            ListPatch::Refresh,
            "only a read places it"
        );
        assert_eq!(patch_entries(&open, &mut entries, &reference(1), &reply), ListPatch::Unchanged);
        let old = json!({"state": "merged", "updatedAt": "2026-01-01T00:00:00Z"});
        let old = Reply::read(&old).unwrap();
        assert_eq!(patch_entries(&all, &mut entries, &reference(2), &old), ListPatch::Unchanged, "a stale reply");
    }

    #[test]
    fn a_reply_patches_the_detail_and_asks_for_a_read_when_it_lacks_fields() {
        let detail: Detail = serde_json::from_value(json!({
            "provider": "github", "projectId": "p", "repository": "o/r", "number": 1, "title": "T", "url": "u",
            "state": "closed", "headBranch": "h", "baseBranch": "main", "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z", "headSha": "a1", "autoMergeEnabled": true, "autoMergeMethod": "squash",
        }))
        .unwrap();
        let reopened =
            json!({"state": "open", "updatedAt": "2026-01-02T00:00:00Z", "author": null, "autoMergeEnabled": false});
        let (patched, reread) = patch_detail(&detail, &Reply::read(&reopened).unwrap());
        let patched = patched.unwrap();
        assert!(reread, "reopening changes what the reply lacks");
        assert_eq!(
            (patched.state, patched.auto_merge_enabled, patched.auto_merge_method),
            (State::Open, Some(false), None)
        );
        let same_head =
            json!({"state": "closed", "updatedAt": "2026-01-02T00:00:00Z", "headSha": "a1", "title": "New"});
        let (patched, reread) = patch_detail(&detail, &Reply::read(&same_head).unwrap());
        assert!(!reread);
        assert_eq!(patched.unwrap().title, "New");
        let new_head = json!({"state": "closed", "updatedAt": "2026-01-02T00:00:00Z", "headSha": "b2"});
        assert!(patch_detail(&detail, &Reply::read(&new_head).unwrap()).1, "a new head reads again");
        let stale = json!({"state": "open", "updatedAt": "2025-12-01T00:00:00Z"});
        assert!(patch_detail(&detail, &Reply::read(&stale).unwrap()).0.is_none());
    }
}
