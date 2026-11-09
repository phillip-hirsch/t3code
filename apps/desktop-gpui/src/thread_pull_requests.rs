//! A thread's pull requests as its shell carries them (`ThreadPullRequestLink`,
//! `ThreadLinkedPullRequest` in `packages/contracts/src/threadPullRequest.ts`), and which one the
//! right panel's Pull request surface opens (`threadPullRequestPanelTarget`), with the stacks and
//! base-branch chains they form (`packages/source-control-core/src/shared/threadPullRequests.ts`).

use std::collections::{HashMap, HashSet};

use serde::Deserialize;

use crate::pull_request_detail::Reference;
use crate::pull_requests::{Actor, Entry, StackMembership, State};

/// `ThreadLinkedPullRequest`: the thread's own change request, or its branch's.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkedPullRequest {
    pub project_id: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// `ThreadPullRequestSnapshot`: the host's state of a link at its last sync. Fields older
/// servers leave out read as unknown, so a link is never dropped for them.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub state: String,
    pub title: String,
    #[serde(default)]
    pub head_branch: String,
    #[serde(default)]
    pub base_branch: String,
    #[serde(default)]
    pub is_draft: bool,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub synced_at: Option<String>,
    #[serde(default)]
    pub author: Option<Actor>,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    /// `approved`, `changes-requested` or `review-required`.
    #[serde(default)]
    pub review_decision: Option<String>,
    /// `passing`, `failing` or `pending`.
    #[serde(default)]
    pub checks_state: Option<String>,
    /// `mergeable`, `conflicting` or `unknown`.
    #[serde(default)]
    pub mergeability: Option<String>,
}

impl Snapshot {
    /// `resolvePullRequestConflict`: an open, ready change request that conflicts with its base.
    pub fn conflicts(&self) -> bool {
        self.state == "open" && !self.is_draft && self.mergeability.as_deref() == Some("conflicting")
    }
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
    #[serde(default)]
    pub stack: Option<LinkStack>,
    /// `ThreadPullRequestWatch`: present while the agent wakes on the change request's checks,
    /// comments and conflicts. Only its presence is read.
    #[serde(default)]
    pub watch: Option<serde::de::IgnoredAny>,
}

/// `ThreadPullRequestStack`: the host's native stack the link belongs to.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkStack {
    pub id: String,
    /// The stack's own change request number.
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub layers: Vec<StackLayer>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct StackLayer {
    pub number: u64,
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

    /// `actionEntry`: what Shift's quick actions act on, a synced and unmerged link read on
    /// `environment_id` in `project_id`.
    pub fn action_entry(&self, environment_id: &str, project_id: &str) -> Option<Entry> {
        let snapshot = self.snapshot.as_ref().filter(|s| s.state != "merged")?;
        let state = match snapshot.state.as_str() {
            "open" => State::Open,
            "closed" => State::Closed,
            _ => State::Unknown,
        };
        let provider = crate::pull_request_detail::parse_change_request(&self.url).map_or("", |l| l.provider);
        let stack = self.stack.as_ref().map(|stack| StackMembership {
            number: stack.number,
            position: stack.layers.iter().position(|l| l.number == self.number).map_or(0, |i| i as u64 + 1),
            size: stack.layers.len() as u64,
        });
        let updated_at = snapshot.updated_at.clone().unwrap_or_default();
        Some(Entry {
            stack,
            environment_id: environment_id.to_owned(),
            provider: provider.to_owned(),
            host: self.host.clone(),
            project_id: project_id.to_owned(),
            repository: self.repository.clone(),
            number: self.number,
            title: snapshot.title.clone(),
            url: self.url.clone(),
            author: snapshot.author.clone(),
            head_branch: snapshot.head_branch.clone(),
            base_branch: snapshot.base_branch.clone(),
            state,
            is_draft: snapshot.is_draft,
            mergeability: snapshot.mergeability.clone().unwrap_or_else(|| "unknown".to_owned()),
            additions: snapshot.additions,
            deletions: snapshot.deletions,
            created_at: self.linked_at.clone(),
            updated_ms: crate::timestamp::parse_ms(&updated_at),
            updated_at,
            viewer_review_requested: false,
            labels: Vec::new(),
            review_decision: snapshot.review_decision.clone(),
            checks_state: snapshot.checks_state.clone(),
            observed_at: None,
            created_ms: crate::timestamp::parse_ms(&self.linked_at),
        })
    }
}

/// `latestSyncedAt`: when the newest of the links last synced with its host.
pub fn latest_synced_ms(links: &[PullRequestLink]) -> Option<i64> {
    visible(links).filter_map(|l| crate::timestamp::parse_ms(l.snapshot.as_ref()?.synced_at.as_deref()?)).max()
}

/// `visibleThreadPullRequests`: a dismissed stack member is hidden.
pub fn visible(links: &[PullRequestLink]) -> impl Iterator<Item = &PullRequestLink> {
    links.iter().filter(|link| link.source != "stack-dismissed")
}

/// `usePullRequestLinking().isLinked`: a visible link names the change request at `url`.
pub fn links_url(links: &[PullRequestLink], url: &str) -> bool {
    crate::pull_request_detail::parse_change_request_url(url).is_some_and(|key| visible(links).any(|l| l.key() == key))
}

/// `ThreadPullRequestChain`: a host's native stack, or links chained by base branch.
#[derive(Debug)]
pub struct Chain<'a> {
    pub native: bool,
    pub layers: Vec<&'a PullRequestLink>,
    /// One per layer: 0 for the base, parent's depth + 1 above it. Siblings share a depth.
    pub depths: Vec<usize>,
}

impl<'a> Chain<'a> {
    /// `layersFromTop`: deepest first, the later sibling first.
    fn top_down(&self) -> Vec<&'a PullRequestLink> {
        let mut order: Vec<usize> = (0..self.layers.len()).collect();
        order.sort_by(|&l, &r| self.depths[r].cmp(&self.depths[l]).then(r.cmp(&l)));
        order.into_iter().map(|i| self.layers[i]).collect()
    }
}

type BranchKey = (String, String, String);

/// `resolveThreadPullRequestChains`.
pub fn chains<'a>(links: &'a [PullRequestLink]) -> Vec<Chain<'a>> {
    let visible: Vec<&PullRequestLink> = visible(links).collect();
    let mut chains = Vec::new();
    let mut placed = HashSet::new();

    let mut stacks: Vec<(BranchKey, Vec<&PullRequestLink>)> = Vec::new();
    for link in visible.iter().copied() {
        let Some(stack) = &link.stack else { continue };
        let (host, repository, _) = link.key();
        let key = (host, repository, stack.id.clone());
        match stacks.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(link),
            None => stacks.push((key, vec![link])),
        }
    }
    for (_, mut members) in stacks {
        // Saved snapshots can lag behind newer members of the same stack.
        let numbers: HashSet<u64> = members.iter().map(|m| m.number).collect();
        let coverage = |layers: &[StackLayer]| layers.iter().filter(|l| numbers.contains(&l.number)).count();
        let layers_of = |link: &'a PullRequestLink| -> &'a [StackLayer] {
            link.stack.as_ref().map_or(&[], |s| s.layers.as_slice())
        };
        let mut layers = layers_of(members[0]);
        for member in members[1..].iter().copied() {
            let candidate = layers_of(member);
            let (have, best) = (coverage(candidate), coverage(layers));
            if have > best || (have == best && candidate.len() > layers.len()) {
                layers = candidate;
            }
        }
        let order: HashMap<u64, usize> = layers.iter().enumerate().map(|(i, l)| (l.number, i)).collect();
        let end = layers.len();
        members.sort_by_key(|m| order.get(&m.number).copied().unwrap_or(end));
        placed.extend(members.iter().map(|m| m.key()));
        let depths = (0..members.len()).collect();
        chains.push(Chain { native: true, layers: members, depths });
    }

    let remaining: Vec<&PullRequestLink> = visible.into_iter().filter(|l| !placed.contains(&l.key())).collect();
    let branch_key = |link: &PullRequestLink, branch: &str| -> BranchKey {
        let (host, repository, _) = link.key();
        (host, repository, branch.to_owned())
    };
    let mut by_head: HashMap<BranchKey, Option<&PullRequestLink>> = HashMap::new();
    for link in remaining.iter().copied() {
        let Some(snapshot) = &link.snapshot else { continue };
        by_head.entry(branch_key(link, &snapshot.head_branch)).and_modify(|slot| *slot = None).or_insert(Some(link));
    }
    let parent_of = |link: &PullRequestLink| -> Option<&'a PullRequestLink> {
        let snapshot = link.snapshot.as_ref()?;
        let parent = (*by_head.get(&branch_key(link, &snapshot.base_branch))?)?;
        (!std::ptr::eq(parent, link)).then_some(parent)
    };
    let mut children: HashMap<(String, String, u64), Vec<&PullRequestLink>> = HashMap::new();
    for link in remaining.iter().copied() {
        if let Some(parent) = parent_of(link) {
            children.entry(parent.key()).or_default().push(link);
        }
    }
    for base in remaining.iter().copied() {
        if parent_of(base).is_some() {
            continue;
        }
        let (mut layers, mut depths) = (Vec::new(), Vec::new());
        let mut pending = vec![(base, 0)];
        while let Some((link, depth)) = pending.pop() {
            // Two links with one identity would otherwise share a child list and expand forever.
            if !placed.insert(link.key()) {
                continue;
            }
            layers.push(link);
            depths.push(depth);
            for child in children.get(&link.key()).into_iter().flatten().rev() {
                pending.push((child, depth + 1));
            }
        }
        if !layers.is_empty() {
            chains.push(Chain { native: false, layers, depths });
        }
    }
    for link in remaining {
        if placed.insert(link.key()) {
            chains.push(Chain { native: false, layers: vec![link], depths: vec![0] });
        }
    }
    chains
}

/// `resolveThreadCurrentPullRequestLink`: the only open link; else the top open layer of the
/// newest open chain; else the top of a lone chain; else the most recently updated link.
fn current(links: &[PullRequestLink]) -> Option<&PullRequestLink> {
    let visible: Vec<&PullRequestLink> = visible(links).collect();
    let open: Vec<&PullRequestLink> = visible.iter().copied().filter(|l| l.is_open()).collect();
    if let [only] = open.as_slice() {
        return Some(only);
    }
    let chains = chains(links);
    if open.len() > 1 {
        let mut open_chains: Vec<Vec<&PullRequestLink>> = chains
            .iter()
            .map(|chain| chain.top_down().into_iter().filter(|l| l.is_open()).collect::<Vec<_>>())
            .filter(|layers| !layers.is_empty())
            .collect();
        let newest = |layers: &[&PullRequestLink]| {
            layers.iter().map(|l| crate::timestamp::parse_ms(&l.linked_at).unwrap_or(0)).max().unwrap_or(0)
        };
        open_chains.sort_by_key(|layers| std::cmp::Reverse(newest(layers)));
        return open_chains.into_iter().flatten().next();
    }
    if let [chain] = chains.as_slice() {
        return chain.top_down().first().copied();
    }
    visible.into_iter().reduce(|latest, link| if link.updated_ms() > latest.updated_ms() { link } else { latest })
}

/// `PullRequestListLine`: a row of the linked list, indented by its depth in its chain; the
/// chain's base row carries whether it is a native stack and its size.
pub struct ListLine<'a> {
    pub link: &'a PullRequestLink,
    pub depth: usize,
    pub stack: Option<(bool, usize)>,
}

/// `pullRequestListLines`: chains by their latest update, newest first.
pub fn list_lines(links: &[PullRequestLink]) -> Vec<ListLine<'_>> {
    let mut chains = chains(links);
    let latest = |chain: &Chain| chain.layers.iter().map(|l| l.updated_ms()).max().unwrap_or(0);
    chains.sort_by_key(|chain| std::cmp::Reverse(latest(chain)));
    chains
        .iter()
        .flat_map(|chain| {
            let size = chain.layers.len();
            chain.layers.iter().zip(&chain.depths).enumerate().map(move |(index, (&link, &depth))| ListLine {
                link,
                depth,
                stack: (index == 0 && size > 1).then_some((chain.native, size)),
            })
        })
        .collect()
}

/// A change request a thread can link (`resolveLinkPullRequestInput`'s `link`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// The browser URL of a project's change request (`pullRequestLinkProject(identity).webUrl`), for
/// the hosts whose links name the repository.
fn project_change_request_url(identity: &crate::model::RepositoryIdentity, number: u64) -> Option<String> {
    let canonical = identity.canonical_key.as_deref()?;
    let (host, path) = canonical.split_once('/').unwrap_or((canonical, ""));
    let repository = identity
        .display_name
        .clone()
        .or_else(|| Some(format!("{}/{}", identity.owner.as_ref()?, identity.name.as_ref()?)))?;
    let (route, repository) = match identity.provider.as_deref()? {
        "github" => ("pull", repository),
        "gitlab" => ("-/merge_requests", repository),
        "forgejo" | "gitcafe" => ("pulls", repository),
        "bitbucket" => ("pull-requests", repository),
        "azure-devops" => ("pullrequest", path.to_owned()),
        _ => return None,
    };
    Some(match identity.web_url.as_deref() {
        Some(web) => format!("{}/{route}/{number}", web.trim_end_matches('/')),
        None => format!("https://{}/{repository}/{route}/{number}", host.to_lowercase()),
    })
}

/// `parsePullRequestReference`'s checkout commands: `gh pr checkout 12`, `glab mr checkout 12`.
fn checkout_argument(input: &str) -> Option<&str> {
    let words: Vec<&str> = input.split_whitespace().collect();
    match words.as_slice() {
        ["gh", "pr", "checkout", argument] | ["glab", "mr", "checkout", argument] => Some(argument),
        _ => None,
    }
}

/// `resolveLinkPullRequestInput`: a change request URL, or `123` / `#123` in the thread's own
/// repository. `None` is input that names nothing yet; an error says what is missing.
pub fn resolve_link_input(
    input: &str,
    identity: Option<&crate::model::RepositoryIdentity>,
) -> Option<Result<ResolvedLink, String>> {
    let trimmed = input.trim();
    let reference = checkout_argument(trimmed).unwrap_or(trimmed);
    if let Some(link) = crate::pull_request_detail::parse_change_request(reference) {
        let (host, repository, number) = (link.host, link.repository, link.number);
        return Some(Ok(ResolvedLink { host, repository, number, url: reference.to_owned() }));
    }
    let digits = reference.strip_prefix('#').unwrap_or(reference);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number: u64 = digits.parse().ok().filter(|n| (1..=9_007_199_254_740_991).contains(n))?;
    let Some(identity) = identity.filter(|i| i.display_name.is_some() || (i.owner.is_some() && i.name.is_some()))
    else {
        return Some(Err(
            "Paste the full pull request URL; this thread has no repository to resolve the number against.".into(),
        ));
    };
    let link = project_change_request_url(identity, number)
        .and_then(|url| Some((crate::pull_request_detail::parse_change_request(&url)?, url)));
    let Some((link, url)) = link else {
        let term = crate::pull_request_detail::terminology(identity.provider.as_deref().unwrap_or_default()).0;
        return Some(Err(format!("Paste a full URL; this project's host has no known {term} URL.")));
    };
    Some(Ok(ResolvedLink { host: link.host, repository: link.repository, number: link.number, url }))
}

/// The host of a change request URL (`https://github.com/o/r/pull/1` → `github.com`).
fn url_host(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(str::to_lowercase)
}

/// `threadPullRequestPanelTarget`: the pull request the Pull request surface opens, read on
/// the thread's environment.
pub fn panel_target(
    environment_id: &str,
    project_id: &str,
    links: &[PullRequestLink],
    linked: Option<&LinkedPullRequest>,
    branch: Option<&LinkedPullRequest>,
) -> Option<Reference> {
    let from_linked = |linked: &LinkedPullRequest| Reference {
        environment_id: environment_id.to_owned(),
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
            environment_id: environment_id.to_owned(),
            project_id: project_id.to_owned(),
            host: Some(current.host.clone()),
            repository: current.repository.clone(),
            number: current.number,
        });
    }
    linked.or(branch).map(from_linked)
}

/// What `thread.copyReference` copies (`ThreadReferenceCopyTarget`'s kind and value).
#[derive(Debug, PartialEq)]
pub enum ReferenceCopy<'a> {
    /// "Copy PR link": this URL.
    PullRequest(&'a str),
    /// "Copy thread": a link that opens the thread.
    Thread,
}

/// `resolveThreadReferenceCopyTarget`: the pull request open in the panel (`None` without one,
/// `Some(None)` while its URL loads, when there is nothing to copy yet), else the thread's current
/// pull request, else its linked or branch one, else the thread.
pub fn reference_copy<'a>(
    open_panel: Option<Option<&'a str>>,
    links: &'a [PullRequestLink],
    linked: Option<&'a LinkedPullRequest>,
    branch: Option<&'a LinkedPullRequest>,
) -> Option<ReferenceCopy<'a>> {
    let url = match open_panel {
        Some(url) => Some(url?),
        None => current(links).map(|link| link.url.as_str()).or(linked.or(branch).map(|l| l.url.as_str())),
    };
    Some(url.map_or(ReferenceCopy::Thread, ReferenceCopy::PullRequest))
}

/// `ThreadSearchPullRequest`: what thread search matches a pull request by.
pub struct SearchPullRequest {
    pub number: u64,
    pub title: Option<String>,
    /// `owner/repo#N` and the link.
    pub references: [String; 2],
}

/// `SEARCHED_PULL_REQUEST_TITLES`: a triage thread linked to dozens would match almost any word.
const SEARCHED_TITLES: usize = 3;

/// `threadSearchPullRequests`: every pull request by number and link, the newest few by title.
pub fn search_entries(thread: &crate::model::ThreadShell) -> Vec<SearchPullRequest> {
    let (links, linked, branch) =
        (&thread.pull_requests, thread.linked_pull_request.as_ref(), thread.branch_pull_request.as_ref());
    let entry = |repository: &str, number: u64, url: &str, title: Option<String>| SearchPullRequest {
        number,
        title,
        references: [format!("{repository}#{number}"), url.to_owned()],
    };
    let mut entries: Vec<SearchPullRequest> =
        branch.map(|b| entry(&b.repository, b.number, &b.url, None)).into_iter().collect();
    if links.is_empty() {
        entries.extend(linked.map(|l| entry(&l.repository, l.number, &l.url, None)));
        return entries;
    }
    let mut newest_first: Vec<&PullRequestLink> = visible(links).collect();
    newest_first.sort_by_key(|link| std::cmp::Reverse(link.updated_ms()));
    entries.extend(newest_first.into_iter().enumerate().map(|(index, link)| {
        let title = link.snapshot.as_ref().filter(|_| index < SEARCHED_TITLES).map(|s| s.title.clone());
        entry(&link.repository, link.number, &link.url, title)
    }));
    entries
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
        let target = panel_target("e", "p", &links, None, None).unwrap();
        assert_eq!((target.number, target.host.as_deref(), target.project_id.as_str()), (2, Some("github.com"), "p"));
        assert_eq!(visible(&links).count(), 2);
        assert!(links_url(&links, "https://GitHub.com/O/R/pull/2/files"), "the thread links #2");
        assert!(!links_url(&links, "https://github.com/o/r/pull/3"), "a dismissed link does not count");
        let closed = vec![
            link(1, "manual", "closed", "2026-01-03T00:00:00Z"),
            link(4, "manual", "merged", "2026-01-04T00:00:00Z"),
        ];
        assert_eq!(panel_target("e", "p", &closed, None, None).unwrap().number, 4, "else the latest update");
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
        assert_eq!(panel_target("e", "p", &links, Some(&linked), None).unwrap().project_id, "other");
        let branch = LinkedPullRequest { number: 9, ..linked };
        assert_eq!(panel_target("e", "p", &[], None, Some(&branch)).unwrap().number, 9);
        assert!(panel_target("e", "p", &[], None, None).is_none());
    }

    #[test]
    fn copy_reference_prefers_the_open_panel_then_the_current_pull_request_then_the_thread() {
        let links = vec![link(2, "agent", "open", "2026-01-01T00:00:00Z")];
        let branch = LinkedPullRequest {
            project_id: "p".into(),
            repository: "o/r".into(),
            number: 9,
            url: "https://github.com/o/r/pull/9".into(),
        };
        let panel = Some(Some("https://github.com/o/r/pull/5"));
        assert_eq!(
            reference_copy(panel, &links, None, Some(&branch)),
            Some(ReferenceCopy::PullRequest("https://github.com/o/r/pull/5"))
        );
        assert_eq!(reference_copy(Some(None), &links, None, None), None, "the panel's URL is loading");
        assert_eq!(
            reference_copy(None, &links, None, Some(&branch)),
            Some(ReferenceCopy::PullRequest("https://github.com/o/r/pull/2"))
        );
        assert_eq!(
            reference_copy(None, &[], None, Some(&branch)),
            Some(ReferenceCopy::PullRequest("https://github.com/o/r/pull/9"))
        );
        assert_eq!(reference_copy(None, &[], None, None), Some(ReferenceCopy::Thread));
    }

    #[test]
    fn a_link_comes_from_a_url_or_a_number_in_the_threads_repository() {
        let identity: crate::model::RepositoryIdentity = serde_json::from_value(
            json!({"canonicalKey": "github.com/acme/app", "displayName": "acme/app", "provider": "github"}),
        )
        .unwrap();
        let resolved = |input: &str, identity| {
            resolve_link_input(input, identity).map(|r| r.map(|l| (l.host, l.repository, l.number, l.url)))
        };
        assert_eq!(
            resolved("https://codeberg.org/Forge/App/pulls/3", None),
            Some(Ok(("codeberg.org".into(), "forge/app".into(), 3, "https://codeberg.org/Forge/App/pulls/3".into())))
        );
        let pull = Some(Ok(("github.com".into(), "acme/app".into(), 42, "https://github.com/acme/app/pull/42".into())));
        assert_eq!(resolved(" #42 ", Some(&identity)), pull);
        assert_eq!(resolved("gh pr checkout 42", Some(&identity)), pull);
        assert!(matches!(resolved("42", None), Some(Err(e)) if e.starts_with("Paste the full pull request URL")));
        let unknown: crate::model::RepositoryIdentity =
            serde_json::from_value(json!({"canonicalKey": "git.example.com/a/b", "displayName": "a/b"})).unwrap();
        assert!(matches!(resolved("42", Some(&unknown)), Some(Err(e)) if e.contains("no known pull request URL")));
        assert_eq!(resolved("abc", Some(&identity)), None);
        assert_eq!(resolved("#0", Some(&identity)), None);
    }

    #[test]
    fn a_link_decodes_its_full_snapshot_and_watch_and_older_servers_still_decode() {
        let full: PullRequestLink = serde_json::from_value(json!({
            "host": "gitlab.com", "repository": "g/p", "number": 7, "url": "https://gitlab.com/g/p/-/merge_requests/7",
            "source": "agent", "linkedAt": "2026-01-01T00:00:00Z",
            "snapshot": {"state": "open", "title": "T", "headBranch": "feat", "baseBranch": "main", "isDraft": false,
                "updatedAt": "2026-01-02T00:00:00Z", "syncedAt": "2026-01-03T00:00:00Z",
                "author": {"login": "ada", "name": "Ada", "avatarUrl": "https://x/a.png"},
                "additions": 12, "deletions": 3, "changedFiles": 2, "reviewDecision": "approved",
                "checksState": "failing", "mergeability": "conflicting", "closedAt": null},
            "stack": {"kind": "native", "id": "s", "number": 9, "url": "https://gitlab.com/s", "base": "main",
                "layers": [{"number": 6, "headBranch": "a", "state": "open"}, {"number": 7, "headBranch": "feat", "state": "open"}]},
            "watch": {"startedAt": "2026-01-01T00:00:00Z", "headSha": null, "failedChecks": [], "passed": false,
                "remarksThrough": "2026-01-01T00:00:00Z", "remarkIds": [], "conflicting": false, "wakes": 0},
        }))
        .unwrap();
        let snapshot = full.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.author.as_ref().map(|a| a.login.as_str()), Some("ada"));
        assert_eq!((snapshot.additions, snapshot.deletions), (12, 3));
        assert_eq!(snapshot.review_decision.as_deref(), Some("approved"));
        assert_eq!(snapshot.checks_state.as_deref(), Some("failing"));
        assert!(snapshot.conflicts(), "an open, ready change request that conflicts");
        assert!(full.watch.is_some());
        assert_eq!(latest_synced_ms(std::slice::from_ref(&full)), crate::timestamp::parse_ms("2026-01-03T00:00:00Z"));

        let entry = full.action_entry("e", "p").unwrap();
        assert_eq!(
            (entry.environment_id.as_str(), entry.project_id.as_str(), entry.provider.as_str()),
            ("e", "p", "gitlab")
        );
        assert_eq!(entry.stack.as_ref().map(|s| (s.number, s.position, s.size)), Some((9, 2, 2)));
        let actions = crate::pull_requests::kind_actions(&entry.provider);
        assert_eq!(crate::pull_requests::quick_actions(&entry, actions), vec!["close", "merge"]);

        // A server that predates these fields, and a link not watched.
        let old = link(1, "manual", "open", "2026-01-01T00:00:00Z");
        let snapshot = old.snapshot.as_ref().unwrap();
        assert!(old.watch.is_none() && snapshot.author.is_none() && !snapshot.conflicts());
        assert_eq!((snapshot.additions, snapshot.mergeability.as_deref()), (0, None));
        let mut merged = old.clone();
        merged.snapshot.as_mut().unwrap().state = "merged".into();
        assert!(merged.action_entry("e", "p").is_none(), "a merged link has no quick actions");
        let unsynced = PullRequestLink { snapshot: None, ..old };
        assert!(unsynced.action_entry("e", "p").is_none(), "nor one waiting for its host");
    }

    #[test]
    fn a_hosts_kind_offers_its_own_quick_actions() {
        let draft = |url: &str| {
            let mut link = link(1, "manual", "open", "2026-01-01T00:00:00Z");
            link.url = url.into();
            link.snapshot.as_mut().unwrap().is_draft = true;
            let entry = link.action_entry("e", "p").unwrap();
            crate::pull_requests::quick_actions(&entry, crate::pull_requests::kind_actions(&entry.provider))
        };
        assert_eq!(draft("https://github.com/o/r/pull/1"), vec!["close", "ready"]);
        assert_eq!(draft("https://codeberg.org/o/r/pulls/1"), vec!["close"], "Forgejo cannot mark ready");
        assert!(draft("https://example.com/o/r/1").is_empty(), "an unknown host offers none");
    }

    fn chained(
        number: u64,
        head: &str,
        base: &str,
        linked_at: &str,
        stack: Option<serde_json::Value>,
    ) -> PullRequestLink {
        let mut link = link(number, "agent", "open", linked_at);
        let snapshot = link.snapshot.as_mut().unwrap();
        (snapshot.head_branch, snapshot.base_branch) = (head.into(), base.into());
        link.stack = stack.map(|s| serde_json::from_value(s).unwrap());
        link
    }

    #[test]
    fn the_top_of_a_chain_is_current_and_lines_follow_the_hierarchy() {
        // #2 sits on #1 by base branch; #1 was linked last, yet the stack's top is current.
        let links = vec![
            chained(2, "b", "a", "2026-01-01T00:00:00Z", None),
            chained(1, "a", "main", "2026-01-02T00:00:00Z", None),
            chained(3, "c", "a", "2026-01-01T00:00:00Z", None),
        ];
        assert_eq!(panel_target("e", "p", &links, None, None).unwrap().number, 3, "the later sibling tops the chain");
        let lines: Vec<_> = list_lines(&links).iter().map(|l| (l.link.number, l.depth, l.stack)).collect();
        assert_eq!(lines, vec![(1, 0, Some((false, 3))), (2, 1, None), (3, 1, None)]);

        // A native stack orders its members by the host's layers, not by link time or branch.
        let stack = json!({"id": "s1", "layers": [{"number": 5}, {"number": 4}]});
        let links = vec![
            chained(4, "x", "main", "2026-01-03T00:00:00Z", Some(stack.clone())),
            chained(5, "y", "main", "2026-01-01T00:00:00Z", Some(stack)),
        ];
        assert_eq!(panel_target("e", "p", &links, None, None).unwrap().number, 4);
        let chains = chains(&links);
        assert_eq!(chains.len(), 1);
        assert!(chains[0].native);
        assert_eq!(chains[0].layers.iter().map(|l| l.number).collect::<Vec<_>>(), vec![5, 4]);
    }
}
