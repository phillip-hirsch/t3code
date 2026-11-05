//! A thread's pull requests as its shell carries them (`ThreadPullRequestLink`,
//! `ThreadLinkedPullRequest` in `packages/contracts/src/threadPullRequest.ts`), and which one the
//! right panel's Pull request surface opens (`threadPullRequestPanelTarget`).

use serde::Deserialize;

use crate::pull_request_detail::Reference;

/// `ThreadLinkedPullRequest`: the thread's own change request, or its branch's.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkedPullRequest {
    pub project_id: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub state: String,
    pub title: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// `ThreadPullRequestLink`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
    pub source: String,
    pub linked_at: String,
    #[serde(default)]
    pub snapshot: Option<Snapshot>,
}

impl PullRequestLink {
    fn key(&self) -> (String, String, u64) {
        (self.host.to_lowercase(), self.repository.to_lowercase(), self.number)
    }

    fn is_open(&self) -> bool {
        self.snapshot.as_ref().is_none_or(|s| s.state == "open")
    }

    fn updated_ms(&self) -> i64 {
        let at = self.snapshot.as_ref().and_then(|s| s.updated_at.as_deref()).unwrap_or(&self.linked_at);
        crate::timestamp::parse_ms(at).unwrap_or(0)
    }
}

/// `visibleThreadPullRequests`: a dismissed stack member is hidden.
pub fn visible(links: &[PullRequestLink]) -> impl Iterator<Item = &PullRequestLink> {
    links.iter().filter(|link| link.source != "stack-dismissed")
}

/// `resolveThreadCurrentPullRequestLink` without native stacks: the newest open link, else the
/// most recently updated one.
fn current(links: &[PullRequestLink]) -> Option<&PullRequestLink> {
    let newest_open = visible(links).filter(|l| l.is_open()).max_by_key(|l| crate::timestamp::parse_ms(&l.linked_at));
    newest_open.or_else(|| visible(links).max_by_key(|l| l.updated_ms()))
}

/// The host of a change request URL (`https://github.com/o/r/pull/1` → `github.com`).
fn url_host(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_lowercase)
}

/// `threadPullRequestPanelTarget`: the pull request the Pull request surface opens.
pub fn panel_target(
    project_id: &str,
    links: &[PullRequestLink],
    linked: Option<&LinkedPullRequest>,
    branch: Option<&LinkedPullRequest>,
) -> Option<Reference> {
    let from_linked = |linked: &LinkedPullRequest| Reference {
        project_id: linked.project_id.clone(),
        host: url_host(&linked.url),
        repository: linked.repository.clone(),
        number: linked.number,
    };
    let current = current(links);
    if let (Some(current), Some(linked)) = (current, linked) {
        let legacy = (url_host(&linked.url).unwrap_or_default(), linked.repository.to_lowercase(), linked.number);
        if current.key() == legacy {
            return Some(from_linked(linked));
        }
    }
    if let Some(current) = current {
        return Some(Reference {
            project_id: project_id.to_owned(),
            host: Some(current.host.clone()),
            repository: current.repository.clone(),
            number: current.number,
        });
    }
    linked.or(branch).map(from_linked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link(number: u64, source: &str, state: &str, linked_at: &str) -> PullRequestLink {
        serde_json::from_value(json!({
            "host": "github.com", "repository": "o/r", "number": number, "url": format!("https://github.com/o/r/pull/{number}"),
            "source": source, "linkedAt": linked_at,
            "snapshot": {"state": state, "title": "T", "headBranch": "h", "baseBranch": "main", "isDraft": false,
                "updatedAt": linked_at, "syncedAt": linked_at},
        }))
        .unwrap()
    }

    #[test]
    fn the_newest_open_link_wins_over_closed_and_dismissed_ones() {
        let links = vec![
            link(1, "manual", "closed", "2026-01-03T00:00:00Z"),
            link(2, "agent", "open", "2026-01-01T00:00:00Z"),
            link(3, "stack-dismissed", "open", "2026-01-05T00:00:00Z"),
        ];
        let target = panel_target("p", &links, None, None).unwrap();
        assert_eq!((target.number, target.host.as_deref(), target.project_id.as_str()), (2, Some("github.com"), "p"));
        assert_eq!(visible(&links).count(), 2);
        let closed = vec![
            link(1, "manual", "closed", "2026-01-03T00:00:00Z"),
            link(4, "manual", "merged", "2026-01-04T00:00:00Z"),
        ];
        assert_eq!(panel_target("p", &closed, None, None).unwrap().number, 4, "else the latest update");
    }

    #[test]
    fn the_linked_pull_request_keeps_its_project_and_the_branch_is_the_fallback() {
        let linked = LinkedPullRequest {
            project_id: "other".into(),
            repository: "O/R".into(),
            number: 2,
            url: "https://github.com/O/R/pull/2".into(),
        };
        let links = vec![link(2, "agent", "open", "2026-01-01T00:00:00Z")];
        assert_eq!(panel_target("p", &links, Some(&linked), None).unwrap().project_id, "other");
        let branch = LinkedPullRequest { number: 9, ..linked.clone() };
        assert_eq!(panel_target("p", &[], None, Some(&branch)).unwrap().number, 9);
        assert!(panel_target("p", &[], None, None).is_none());
    }
}
