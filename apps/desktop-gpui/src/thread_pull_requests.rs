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
pub fn current(links: &[PullRequestLink]) -> Option<&PullRequestLink> {
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

/// The pull request the thread card leads with, under its branch (`ThreadDetailsPrRows`' current row).
#[derive(Debug, PartialEq)]
pub struct CardCurrent<'a> {
    /// The link it comes from; a branch's pull request has none.
    pub link: Option<&'a PullRequestLink>,
    pub host: Option<String>,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// The current link, else the thread's branch pull request, else the one the checkout's branch
/// has on its host (`branch`, as `(number, url)`).
pub fn card_current<'a>(
    links: &'a [PullRequestLink],
    linked_branch: Option<&LinkedPullRequest>,
    branch: Option<(u64, &str)>,
) -> Option<CardCurrent<'a>> {
    if let Some(link) = current(links) {
        let (host, repository) = (Some(link.host.clone()), link.repository.clone());
        return Some(CardCurrent { link: Some(link), host, repository, number: link.number, url: link.url.clone() });
    }
    if let Some(linked) = linked_branch {
        let (host, repository, number, url) =
            (url_host(&linked.url), linked.repository.clone(), linked.number, linked.url.clone());
        return Some(CardCurrent { link: None, host, repository, number, url });
    }
    let (number, url) = branch?;
    let parsed = crate::pull_request_detail::parse_change_request(url)?;
    Some(CardCurrent {
        link: None,
        host: Some(parsed.host),
        repository: parsed.repository,
        number,
        url: url.to_owned(),
    })
}

/// `ThreadDetailsPrRows`' "Show N more": the other links in list order, only beside a current link.
pub fn card_rest<'a>(links: &'a [PullRequestLink], current: &CardCurrent) -> Vec<&'a PullRequestLink> {
    let Some(current) = current.link else { return Vec::new() };
    list_lines(links).into_iter().map(|line| line.link).filter(|link| link.key() != current.key()).collect()
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
        // Origin's pages live on cursor.com whatever the remote names.
        "cursor-origin" => {
            let base = crate::pull_request_detail::CURSOR_ORIGIN_WEB_BASE_URL;
            return Some(format!("{base}/{repository}/pull/{number}"));
        }
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

/// `resolveLinkPullRequestInput`: a change request URL, or `123` / `#123` in the thread's own
/// repository. `None` is input that names nothing yet; an error says what is missing.
pub fn resolve_link_input(
    input: &str,
    identity: Option<&crate::model::RepositoryIdentity>,
) -> Option<Result<ResolvedLink, String>> {
    let trimmed = input.trim();
    let reference = match crate::pull_request_detail::parse_change_request(trimmed) {
        Some(_) => trimmed.to_owned(),
        None => crate::pull_request_checkout::parse_reference(trimmed)?,
    };
    if let Some(link) = crate::pull_request_detail::parse_change_request(&reference) {
        let (host, repository, number) = (link.host, link.repository, link.number);
        return Some(Ok(ResolvedLink { host, repository, number, url: reference }));
    }
    let number: u64 = reference.parse().ok().filter(|n| (1..=9_007_199_254_740_991).contains(n))?;
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

/// `PULL_REQUEST_STATE_PRESENTATION`'s keys: a draft is an open change request marked so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Draft,
    Open,
    Closed,
    Merged,
}

impl Tone {
    /// `resolvePullRequestState`.
    pub fn of(state: &str, is_draft: bool) -> Self {
        match state {
            "merged" => Self::Merged,
            "closed" => Self::Closed,
            _ if is_draft => Self::Draft,
            _ => Self::Open,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "Draft",
            Self::Open => "Open",
            Self::Closed => "Closed",
            Self::Merged => "Merged",
        }
    }
}

/// `ThreadPullRequestBadge`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    /// The links form one chain of this many layers.
    Stack { layers: usize, tone: Tone },
    /// The current change request, and how many more the thread links.
    PullRequest { others: usize, tone: Tone },
}

/// `resolveThreadPullRequestBadge`: every visible link a draft, else any open, else all merged,
/// else closed; several links in one chain are a stack.
pub fn badge(links: &[PullRequestLink]) -> Option<Badge> {
    fn state(link: &PullRequestLink) -> &str {
        link.snapshot.as_ref().map_or("open", |s| s.state.as_str())
    }
    let visible: Vec<&PullRequestLink> = visible(links).collect();
    if visible.is_empty() {
        return None;
    }
    let tone = if visible.iter().all(|l| l.snapshot.as_ref().is_some_and(|s| s.state == "open" && s.is_draft)) {
        Tone::Draft
    } else if visible.iter().any(|l| state(l) == "open") {
        Tone::Open
    } else if visible.iter().all(|l| state(l) == "merged") {
        Tone::Merged
    } else {
        Tone::Closed
    };
    if visible.len() > 1 && chains(links).len() == 1 {
        return Some(Badge::Stack { layers: visible.len(), tone });
    }
    Some(Badge::PullRequest { others: visible.len() - 1, tone })
}

fn provider_of(url: &str) -> &'static str {
    crate::pull_request_detail::parse_change_request(url).map_or("", |link| link.provider)
}

/// What a thread row shows of its change request (`LinkedThreadPullRequestStatus`).
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub tone: Tone,
    /// `SourceControlProviderKind`, which names it a PR or an MR.
    pub provider: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    #[serde(default)]
    provider: String,
    number: u64,
    url: String,
    title: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
}

impl Shown {
    /// `linkedPullRequestSnapshotStatus`: `None` until the link syncs with its host.
    pub fn of_link(link: &PullRequestLink) -> Option<Self> {
        let snapshot = link.snapshot.as_ref()?;
        Some(Self {
            number: link.number,
            url: link.url.clone(),
            title: snapshot.title.clone(),
            tone: Tone::of(&snapshot.state, snapshot.is_draft),
            provider: provider_of(&link.url).to_owned(),
        })
    }

    /// A `pullRequests.summary` answer.
    pub fn of_summary(value: &serde_json::Value) -> Option<Self> {
        let summary = Summary::deserialize(value).ok()?;
        let tone = Tone::of(&summary.state, summary.is_draft);
        Some(Self { number: summary.number, url: summary.url, title: summary.title, tone, provider: summary.provider })
    }

    /// `prStatusIndicator`'s tooltip lead: `PR #12 - Open`.
    pub fn lead(&self) -> String {
        let short = crate::pull_request_detail::terminology(&self.provider).1;
        format!("{short} #{} - {}", self.number, self.tone.label())
    }

    pub fn tooltip(&self) -> String {
        format!("{}: {}", self.lead(), self.title)
    }
}

/// Where `useLinkedThreadPullRequest` finds a thread row's change request.
#[derive(Debug, PartialEq)]
pub enum Source<'a> {
    /// The current link, shown from its snapshot once synced.
    Link(&'a PullRequestLink),
    /// No current link: the branch's pull request (before it, an older server's single link),
    /// read from its host.
    Read(&'a LinkedPullRequest),
}

/// `useLinkedThreadPullRequest`'s subject; `supports_links` is `useSupportsMultiplePullRequests`.
pub fn source(thread: &crate::model::ThreadShell, supports_links: bool) -> Option<Source<'_>> {
    if let Some(link) = current(&thread.pull_requests).filter(|_| supports_links) {
        return Some(Source::Link(link));
    }
    let legacy = thread.linked_pull_request.as_ref().filter(|_| !supports_links);
    legacy.or(thread.branch_pull_request.as_ref()).map(Source::Read)
}

/// `ThreadPullRequestBadgePresentation`'s icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Stack(Tone),
    State(Tone),
    /// The change request's state is not known yet.
    Pending,
}

/// `ThreadPullRequestBadgePresentation`, and whether it opens the linked list instead of the
/// change request.
#[derive(Debug, PartialEq)]
pub struct BadgeView {
    pub glyph: Glyph,
    /// Its accessible name and tooltip line.
    pub label: String,
    pub text: String,
    pub opens_list: bool,
}

/// `resolveThreadPullRequestBadgePresentation`: `None` without a stack or a known change request.
pub fn badge_view(
    badge: Option<Badge>,
    number: Option<u64>,
    url: Option<&str>,
    shown: Option<&Shown>,
) -> Option<BadgeView> {
    let (term, short) = crate::pull_request_detail::terminology(url.map_or("", provider_of));
    if let Some(Badge::Stack { layers, tone }) = badge {
        let label = format!("Stack of {layers} {term}s, {}", tone.label().to_lowercase());
        return Some(BadgeView { glyph: Glyph::Stack(tone), label, text: layers.to_string(), opens_list: true });
    }
    let (Some(number), Some(_)) = (number, url) else { return None };
    let tooltip = shown.map_or_else(|| format!("{short} #{number}, status pending"), Shown::tooltip);
    if let Some(Badge::PullRequest { others, tone }) = badge
        && others > 0
    {
        let label = format!("{tooltip}, and {others} more linked; overall {}", tone.label().to_lowercase());
        return Some(BadgeView {
            glyph: Glyph::State(tone),
            label,
            text: format!("+{}", others + 1),
            opens_list: true,
        });
    }
    let glyph = shown.map_or(Glyph::Pending, |shown| Glyph::State(shown.tone));
    Some(BadgeView { glyph, label: tooltip, text: number.to_string(), opens_list: false })
}

/// The sidebar row's badge (`ThreadPullRequestBadgeControl`): a stack, the read change request,
/// or the current link while its state is pending.
pub fn row_badge(thread: &crate::model::ThreadShell, supports_links: bool, shown: Option<&Shown>) -> Option<BadgeView> {
    let links = &thread.pull_requests;
    let badge = badge(links).filter(|_| supports_links);
    let current = current(links).filter(|_| supports_links);
    let number = shown.map(|s| s.number).or(current.map(|l| l.number));
    let url = shown.map(|s| s.url.as_str()).or(current.map(|l| l.url.as_str()));
    badge_view(badge, number, url, shown)
}

/// The query's words all appear among `terms` (`pullRequestMatchesSearchQuery`).
fn terms_match(terms: impl IntoIterator<Item = String>, query: &str) -> bool {
    let tokens = crate::search::fold_query(query);
    if tokens.is_empty() {
        return false;
    }
    let terms: Vec<String> = terms.into_iter().collect();
    let haystack: Vec<char> = crate::search::fold_query(&terms.join(" ")).join(&' ');
    tokens.iter().all(|token| haystack.windows(token.len()).any(|window| window == token.as_slice()))
}

fn entry_terms(entry: SearchPullRequest) -> impl Iterator<Item = String> {
    let [reference, url] = entry.references;
    [Some(format!("#{}", entry.number)), Some(reference), Some(url), entry.title].into_iter().flatten()
}

/// `pullRequestMatchesSearchQuery` over every pull request thread search matches the thread by.
pub fn matches_search(thread: &crate::model::ThreadShell, query: &str) -> bool {
    terms_match(search_entries(thread).into_iter().flat_map(entry_terms), query)
}

/// `ThreadSearchPullRequestNumber`'s badge, once the query names one of the thread's pull
/// requests. A stored reference gives the number before its read lands.
pub fn search_badge(
    thread: &crate::model::ThreadShell,
    query: &str,
    supports_links: bool,
    shown: Option<&Shown>,
) -> Option<BadgeView> {
    if !matches_search(thread, query) {
        return None;
    }
    let links = &thread.pull_requests;
    let badge = badge(links).filter(|_| supports_links);
    let stored = current(links).filter(|_| supports_links).map(|l| (l.number, l.url.as_str())).or_else(|| {
        let matches = |reference: &&LinkedPullRequest| {
            let entry = SearchPullRequest {
                number: reference.number,
                title: None,
                references: [format!("{}#{}", reference.repository, reference.number), reference.url.clone()],
            };
            terms_match(entry_terms(entry), query)
        };
        [thread.linked_pull_request.as_ref(), thread.branch_pull_request.as_ref()]
            .into_iter()
            .flatten()
            .find(matches)
            .map(|reference| (reference.number, reference.url.as_str()))
    });
    let number = shown.map(|s| s.number).or(stored.map(|s| s.0));
    let url = shown.map(|s| s.url.as_str()).or(stored.map(|s| s.1));
    badge_view(badge, number, url, shown)
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
        assert_eq!(resolved("GH  PR checkout #42", Some(&identity)), pull, "any case and spacing");
        assert_eq!(resolved("gh pr checkout", Some(&identity)), None);
        assert_eq!(resolved("ghpr checkout 42", Some(&identity)), None);
        let origin: crate::model::RepositoryIdentity = serde_json::from_value(json!({
            "canonicalKey": "origin.cursor.com/Acme/Web", "displayName": "Acme/Web", "provider": "cursor-origin",
            "webUrl": "https://origin.cursor.com/Acme/Web",
        }))
        .unwrap();
        let origin_pull = Some(Ok((
            "origin.cursor.com".into(),
            "acme/web".into(),
            7,
            "https://cursor.com/codebase/Acme/Web/pull/7".into(),
        )));
        assert_eq!(resolved("7", Some(&origin)), origin_pull, "a number opens on cursor.com, not the git host");
        assert_eq!(resolved("origin pr checkout 7", Some(&origin)), origin_pull);
        assert_eq!(
            resolved("origin pr checkout https://cursor.com/codebase/Acme/Web/pull/7", None),
            origin_pull,
            "a pasted Origin URL needs no project"
        );
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

    fn shell(links: &[PullRequestLink], branch: Option<serde_json::Value>) -> crate::model::ThreadShell {
        serde_json::from_value(json!({
            "id": "t", "projectId": "p", "title": "t", "status": "idle",
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z", "lineage": {"rootThreadId": "t"},
            "pullRequests": serde_json::to_value(links.iter().map(|l| json!({
                "host": l.host, "repository": l.repository, "number": l.number, "url": l.url, "source": l.source,
                "linkedAt": l.linked_at, "snapshot": l.snapshot.as_ref().map(|s| json!({"state": s.state, "title": s.title,
                    "headBranch": s.head_branch, "baseBranch": s.base_branch, "isDraft": s.is_draft, "updatedAt": s.updated_at})),
            })).collect::<Vec<_>>()).unwrap(),
            "branchPullRequest": branch,
        }))
        .unwrap()
    }

    #[test]
    fn the_badge_aggregates_states_and_tells_a_stack_from_several_links() {
        let state = |link: &mut PullRequestLink, state: &str, draft: bool| {
            let snapshot = link.snapshot.as_mut().unwrap();
            (snapshot.state, snapshot.is_draft) = (state.into(), draft);
        };
        let mut draft = link(1, "agent", "open", "2026-01-01T00:00:00Z");
        state(&mut draft, "open", true);
        assert_eq!(badge(std::slice::from_ref(&draft)), Some(Badge::PullRequest { others: 0, tone: Tone::Draft }));
        let mut merged = link(2, "agent", "merged", "2026-01-02T00:00:00Z");
        merged.snapshot.as_mut().unwrap().head_branch = "other".into();
        let mut closed = link(3, "agent", "closed", "2026-01-03T00:00:00Z");
        closed.snapshot.as_mut().unwrap().head_branch = "third".into();
        let tone = |links: &[PullRequestLink]| match badge(links) {
            Some(Badge::PullRequest { tone, .. } | Badge::Stack { tone, .. }) => Some(tone),
            None => None,
        };
        assert_eq!(tone(&[draft, merged.clone()]), Some(Tone::Open), "a draft is still open");
        assert_eq!(tone(&[merged.clone(), merged.clone()]), Some(Tone::Merged));
        assert_eq!(tone(&[merged.clone(), closed.clone()]), Some(Tone::Closed));
        assert_eq!(badge(&[]), None);
        assert_eq!(badge(&[link(4, "stack-dismissed", "open", "2026-01-01T00:00:00Z")]), None);

        // Two links on unrelated branches; then a chain by base branch.
        let pair = vec![merged.clone(), closed.clone()];
        assert_eq!(badge(&pair), Some(Badge::PullRequest { others: 1, tone: Tone::Closed }));
        let stacked = vec![
            chained(5, "a", "main", "2026-01-01T00:00:00Z", None),
            chained(6, "b", "a", "2026-01-02T00:00:00Z", None),
        ];
        assert_eq!(badge(&stacked), Some(Badge::Stack { layers: 2, tone: Tone::Open }));
        let view = row_badge(&shell(&stacked, None), true, None).unwrap();
        assert_eq!(
            (view.label.as_str(), view.text.as_str(), view.opens_list),
            ("Stack of 2 pull requests, open", "2", true)
        );
        assert_eq!(view.glyph, Glyph::Stack(Tone::Open));

        let several = row_badge(&shell(&pair, None), true, Shown::of_link(&pair[1]).as_ref()).unwrap();
        assert_eq!(several.label, "PR #3 - Closed: T, and 1 more linked; overall closed");
        assert_eq!((several.text.as_str(), several.opens_list), ("+2", true));
    }

    #[test]
    fn a_row_shows_its_current_link_or_reads_its_branch_pull_request() {
        let branch = json!({"projectId": "p", "repository": "o/r", "number": 9, "url": "https://gitlab.com/o/r/-/merge_requests/9"});
        let mut unsynced = link(2, "agent", "open", "2026-01-01T00:00:00Z");
        unsynced.snapshot = None;
        let thread = shell(std::slice::from_ref(&unsynced), Some(branch.clone()));
        assert!(matches!(source(&thread, true), Some(Source::Link(l)) if l.number == 2));
        let pending = row_badge(&thread, true, None).unwrap();
        assert_eq!(
            (pending.label.as_str(), pending.glyph, pending.opens_list),
            ("PR #2, status pending", Glyph::Pending, false)
        );

        // Without links, or on a server that predates them, the branch's pull request is read.
        let thread = shell(&[], Some(branch));
        assert!(matches!(source(&thread, true), Some(Source::Read(r)) if r.number == 9));
        assert_eq!(row_badge(&thread, true, None), None, "nothing to show until the read lands");
        let read = Shown::of_summary(&json!({"provider": "gitlab", "projectId": "p", "repository": "o/r", "number": 9,
            "title": "Fix", "url": "https://gitlab.com/o/r/-/merge_requests/9", "state": "open", "isDraft": true,
            "headBranch": "h", "baseBranch": "main", "updatedAt": "2026-01-01T00:00:00Z"}))
        .unwrap();
        assert_eq!(read.tooltip(), "MR #9 - Draft: Fix");
        let view = row_badge(&thread, true, Some(&read)).unwrap();
        assert_eq!((view.text.as_str(), view.glyph), ("9", Glyph::State(Tone::Draft)));
        assert_eq!(source(&shell(&[], None), true), None);
    }

    #[test]
    fn the_search_badge_shows_only_when_the_query_names_a_pull_request() {
        let links = vec![link(42, "agent", "open", "2026-01-01T00:00:00Z")];
        let thread = shell(&links, None);
        for query in ["#42", "o/r#42", "github.com/o/r/pull/42", "t"] {
            assert!(search_badge(&thread, query, true, None).is_some(), "{query}");
        }
        assert!(search_badge(&thread, "#43", true, None).is_none());
        assert!(search_badge(&thread, "", true, None).is_none());
        // A stored branch reference gives the number before the read lands.
        let branch =
            json!({"projectId": "p", "repository": "o/r", "number": 9, "url": "https://github.com/o/r/pull/9"});
        let thread = shell(&[], Some(branch));
        let view = search_badge(&thread, "#9", true, None).unwrap();
        assert_eq!((view.text.as_str(), view.glyph), ("9", Glyph::Pending));
    }

    #[test]
    fn the_thread_card_leads_with_the_current_link_and_folds_the_others() {
        let links = vec![
            chained(2, "b", "a", "2026-01-01T00:00:00Z", None),
            chained(1, "a", "main", "2026-01-02T00:00:00Z", None),
            link(9, "manual", "merged", "2026-01-03T00:00:00Z"),
            link(8, "stack-dismissed", "open", "2026-01-04T00:00:00Z"),
        ];
        let branch = LinkedPullRequest {
            project_id: "p".into(),
            repository: "o/r".into(),
            number: 5,
            url: "https://github.com/o/r/pull/5".into(),
        };
        let current = card_current(&links, Some(&branch), Some((6, "https://github.com/o/r/pull/6"))).unwrap();
        assert_eq!((current.number, current.host.as_deref()), (2, Some("github.com")), "the top of the open chain");
        let rest: Vec<u64> = card_rest(&links, &current).iter().map(|l| l.number).collect();
        assert_eq!(rest, vec![9, 1], "in list order, without the current or a dismissed one");

        // Without links: the thread's branch pull request, then the checkout branch's on its host.
        let from_branch = card_current(&[], Some(&branch), Some((6, "https://github.com/o/r/pull/6"))).unwrap();
        assert_eq!((from_branch.number, from_branch.link), (5, None));
        assert!(card_rest(&links, &from_branch).is_empty(), "nothing folds under a branch pull request");
        let from_status = card_current(&[], None, Some((6, "https://GitHub.com/O/R/pull/6"))).unwrap();
        assert_eq!(
            (from_status.host.as_deref(), from_status.repository.as_str(), from_status.number),
            (Some("github.com"), "o/r", 6)
        );
        assert_eq!(card_current(&[], None, Some((6, "not a link"))), None);
        assert_eq!(card_current(&[], None, None), None);
    }
}
