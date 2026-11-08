//! A thread's pull requests as its shell carries them (`ThreadPullRequestLink`,
//! `ThreadLinkedPullRequest` in `packages/contracts/src/threadPullRequest.ts`), and which one the
//! right panel's Pull request surface opens (`threadPullRequestPanelTarget`), with the stacks and
//! base-branch chains they form (`packages/source-control-core/src/shared/threadPullRequests.ts`).

use std::collections::{HashMap, HashSet};

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
    pub head_branch: String,
    #[serde(default)]
    pub base_branch: String,
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
    #[serde(default)]
    pub stack: Option<LinkStack>,
}

/// `ThreadPullRequestStack`: the host's native stack the link belongs to.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkStack {
    pub id: String,
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
    fn from_top(&self) -> Vec<&'a PullRequestLink> {
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
            .map(|chain| chain.from_top().into_iter().filter(|l| l.is_open()).collect::<Vec<_>>())
            .filter(|layers| !layers.is_empty())
            .collect();
        let newest = |layers: &[&PullRequestLink]| {
            layers.iter().map(|l| crate::timestamp::parse_ms(&l.linked_at).unwrap_or(0)).max().unwrap_or(0)
        };
        open_chains.sort_by_key(|layers| std::cmp::Reverse(newest(layers)));
        return open_chains.into_iter().flatten().next();
    }
    if let [chain] = chains.as_slice() {
        return chain.from_top().first().copied();
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
pub fn list_lines<'a>(links: &'a [PullRequestLink]) -> Vec<ListLine<'a>> {
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
        assert!(links_url(&links, "https://GitHub.com/O/R/pull/2/files"), "the thread links #2");
        assert!(!links_url(&links, "https://github.com/o/r/pull/3"), "a dismissed link does not count");
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
        let branch = LinkedPullRequest { number: 9, ..linked };
        assert_eq!(panel_target("p", &[], None, Some(&branch)).unwrap().number, 9);
        assert!(panel_target("p", &[], None, None).is_none());
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
        assert_eq!(panel_target("p", &links, None, None).unwrap().number, 3, "the later sibling tops the chain");
        let lines: Vec<(u64, usize, Option<(bool, usize)>)> =
            list_lines(&links).iter().map(|l| (l.link.number, l.depth, l.stack)).collect();
        assert_eq!(lines, vec![(1, 0, Some((false, 3))), (2, 1, None), (3, 1, None)]);

        // A native stack orders its members by the host's layers, not by link time or branch.
        let stack = json!({"id": "s1", "layers": [{"number": 5}, {"number": 4}]});
        let links = vec![
            chained(4, "x", "main", "2026-01-03T00:00:00Z", Some(stack.clone())),
            chained(5, "y", "main", "2026-01-01T00:00:00Z", Some(stack)),
        ];
        assert_eq!(panel_target("p", &links, None, None).unwrap().number, 4);
        let chains = chains(&links);
        assert_eq!(chains.len(), 1);
        assert!(chains[0].native);
        assert_eq!(chains[0].layers.iter().map(|l| l.number).collect::<Vec<_>>(), vec![5, 4]);
    }
}
