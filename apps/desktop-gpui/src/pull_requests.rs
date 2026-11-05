//! The Pull requests page's list logic: the `pullRequests.list` read model, the search
//! qualifiers, and how rows are narrowed, grouped and sorted
//! (`apps/web/src/components/pullRequest/pullRequestList.logic.ts`,
//! `pullRequestListGroups.logic.ts`, `packages/client-runtime/src/pullRequestQuery.ts`).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::inbox::parse_ms;
use crate::rpc::RpcError;

/// One page from the host and no more: providers ask for one row beyond the page as their
/// "is there more" probe, and GitHub serves a hundred per request.
pub const PAGE_SIZE: u32 = 99;
pub const MAX_PAGE_SIZE: u32 = 500;
const MATCHED_ELSEWHERE_SCORE: u32 = 10;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Label {
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

/// `PullRequestListEntry`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub provider: String,
    pub host: String,
    pub project_id: String,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<Actor>,
    pub head_branch: String,
    pub base_branch: String,
    /// `open`, `closed` or `merged`.
    pub state: String,
    pub is_draft: bool,
    /// `mergeable`, `conflicting` or `unknown`.
    pub mergeability: String,
    pub additions: u64,
    pub deletions: u64,
    pub created_at: String,
    pub updated_at: String,
    pub viewer_review_requested: bool,
    #[serde(default)]
    pub labels: Vec<Label>,
    #[serde(default)]
    pub review_decision: Option<String>,
    #[serde(default)]
    pub checks_state: Option<String>,
    #[serde(skip)]
    pub updated_ms: Option<i64>,
    #[serde(skip)]
    pub created_ms: Option<i64>,
}

impl Entry {
    /// `pullRequestEntryKey`: unique across hosts and repositories.
    pub fn key(&self) -> String {
        format!("{}:{}#{}", self.host, self.repository, self.number)
    }

    /// `pullRequestDiffStatKey`, which `pullRequests.listStats` answers by.
    pub fn stat_key(&self) -> String {
        stat_key(&self.project_id, &self.repository, self.number)
    }

    pub fn reference(&self) -> Value {
        json!({"projectId": self.project_id, "host": self.host, "repository": self.repository, "number": self.number})
    }
}

pub fn stat_key(project_id: &str, repository: &str, number: u64) -> String {
    format!("{project_id}\u{0}{}#{number}", repository.to_lowercase())
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub host: String,
    pub kind: String,
    #[serde(default)]
    pub searches_on_host: bool,
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectError {
    pub project_id: String,
}

/// `PullRequestListResult`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    #[serde(default)]
    pub viewers: HashMap<String, String>,
    #[serde(default)]
    pub providers: Vec<ProviderSummary>,
    #[serde(default, deserialize_with = "entries")]
    pub entries: Vec<Rc<Entry>>,
    #[serde(default)]
    pub errors: Vec<ProjectError>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub next_cursors: Map<String, Value>,
}

/// Rows that fail to decode are dropped rather than failing the page, as the web's forward-compatible arrays do.
fn entries<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<Rc<Entry>>, D::Error> {
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|value| serde_json::from_value::<Entry>(value).ok())
        .map(|mut entry| {
            entry.updated_ms = parse_ms(&entry.updated_at);
            entry.created_ms = parse_ms(&entry.created_at);
            Rc::new(entry)
        })
        .collect())
}

/// The listing's lead-in states (`PullRequestInvolvement`, `PullRequestListState`) and sorts.
pub const INVOLVEMENTS: &[(&str, &str)] = &[("all", "All"), ("reviewing", "Reviewing"), ("authored", "Authored")];
pub const STATES: &[(&str, &str)] = &[("all", "All"), ("open", "Open"), ("closed", "Closed"), ("merged", "Merged")];
pub const SORTS: &[(&str, &str)] = &[
    ("ready", "Merge readiness"),
    ("blocked", "Blocked on me"),
    ("updated", "Recently updated"),
    ("newest", "Newest shown"),
    ("oldest", "Oldest shown"),
    ("largest", "Largest shown"),
    ("smallest", "Smallest shown"),
];
pub const DRAFTS: &[(&str, &str)] = &[("all", "All"), ("only", "Drafts only"), ("hide", "Hide drafts")];
pub const REVIEWS: &[(&str, &str)] = &[
    ("all", "All"),
    ("approved", "Approved"),
    ("changes-requested", "Changes requested"),
    ("review-required", "Review required"),
    ("none", "No reviews"),
];
pub const CHECKS: &[(&str, &str)] = &[("all", "All"), ("passing", "Passing"), ("failing", "Failing")];

/// `PullRequestListFilters`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filters {
    pub draft: Option<String>,
    pub review: Option<String>,
    pub checks: Option<String>,
    pub author: Option<String>,
    /// Every group must match; any name within a group suffices.
    pub labels: Vec<Vec<String>>,
    pub excluded_labels: Vec<String>,
}

impl Filters {
    pub fn is_empty(&self) -> bool {
        *self == Filters::default()
    }

    /// `{ ...self, ...over }`: a qualifier the reader typed outranks the menu.
    pub fn merged(&self, over: &Filters) -> Filters {
        Filters {
            draft: over.draft.clone().or_else(|| self.draft.clone()),
            review: over.review.clone().or_else(|| self.review.clone()),
            checks: over.checks.clone().or_else(|| self.checks.clone()),
            author: over.author.clone().or_else(|| self.author.clone()),
            labels: if over.labels.is_empty() { self.labels.clone() } else { over.labels.clone() },
            excluded_labels: if over.excluded_labels.is_empty() {
                self.excluded_labels.clone()
            } else {
                over.excluded_labels.clone()
            },
        }
    }

    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        for (key, value) in
            [("draft", &self.draft), ("review", &self.review), ("checks", &self.checks), ("author", &self.author)]
        {
            if let Some(value) = value {
                out.insert(key.into(), json!(value));
            }
        }
        if !self.labels.is_empty() {
            out.insert("labels".into(), json!(self.labels));
        }
        if !self.excluded_labels.is_empty() {
            out.insert("excludedLabels".into(), json!(self.excluded_labels));
        }
        Value::Object(out)
    }
}

const MAX_QUALIFIER_VALUES: usize = 10;
const MAX_QUALIFIER_LENGTH: usize = 200;

/// Splits on `separator` outside double quotes, like `/(?:[^\s"]|"[^"]*")+/g`: an unclosed
/// quote matches nothing and ends the run it interrupts.
fn quoted_runs(raw: &str, separator: impl Fn(char) -> bool) -> Vec<&str> {
    let mut runs = Vec::new();
    let mut start: Option<usize> = None;
    let mut index = 0;
    while index < raw.len() {
        let c = raw[index..].chars().next().unwrap_or_default();
        if c == '"' {
            if let Some(close) = raw[index + 1..].find('"') {
                start.get_or_insert(index);
                index += close + 2;
                continue;
            }
        }
        if c == '"' || separator(c) {
            if let Some(from) = start.take() {
                runs.push(&raw[from..index]);
            }
        } else {
            start.get_or_insert(index);
        }
        index += c.len_utf8();
    }
    if let Some(from) = start {
        runs.push(&raw[from..]);
    }
    runs
}

fn qualifier_value(raw: &str) -> String {
    raw.replace('"', "").trim().to_owned()
}

fn bounded_names(names: impl Iterator<Item = String>) -> Vec<String> {
    names
        .take(MAX_QUALIFIER_VALUES)
        .map(|name| name.chars().take(MAX_QUALIFIER_LENGTH).collect::<String>().trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

fn split_list(raw: &str) -> impl Iterator<Item = String> + '_ {
    quoted_runs(raw, |c| c == ',').into_iter().map(qualifier_value).filter(|part| !part.is_empty())
}

/// `-?name:rest`, where a name starts with a letter and then takes letters, digits, `_` or `-`.
fn qualifier(token: &str) -> Option<(bool, &str, &str)> {
    let (negated, body) = match token.strip_prefix('-') {
        Some(body) => (true, body),
        None => (false, token),
    };
    let (name, rest) = body.split_once(':')?;
    let mut chars = name.chars();
    let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    valid.then_some((negated, name, rest))
}

/// `parsePullRequestQuery`: the free text and the filters its qualifiers name.
pub fn parse_query(raw: &str) -> (String, Filters) {
    let mut text = Vec::new();
    let mut filters = Filters::default();
    for token in quoted_runs(raw, char::is_whitespace) {
        if let Some((negated, name, rest)) = qualifier(token) {
            let value = qualifier_value(rest);
            let lower = if value.is_empty() { String::new() } else { name.to_lowercase() };
            let value_lower = value.to_lowercase();
            match lower.as_str() {
                "" => {}
                "label" => {
                    let names = bounded_names(split_list(rest));
                    if !names.is_empty() {
                        if negated {
                            filters.excluded_labels.extend(names);
                        } else {
                            filters.labels.push(names);
                        }
                        continue;
                    }
                }
                "author" if !negated => {
                    filters.author =
                        Some(value.chars().take(MAX_QUALIFIER_LENGTH).collect::<String>().trim().to_owned());
                    continue;
                }
                "author" => {}
                "draft" if !negated && (value_lower == "true" || value_lower == "false") => {
                    filters.draft = Some(if value_lower == "true" { "only" } else { "hide" }.into());
                    continue;
                }
                "draft" => {}
                "review" => {
                    let decision = match value_lower.as_str() {
                        "approved" => Some("approved"),
                        "changes_requested" | "changes-requested" => Some("changes-requested"),
                        "required" | "review-required" => Some("review-required"),
                        "none" => Some("none"),
                        _ => None,
                    };
                    if let (false, Some(decision)) = (negated, decision) {
                        filters.review = Some(decision.into());
                        continue;
                    }
                }
                "status" | "checks" => {
                    let state = match value_lower.as_str() {
                        "success" | "passing" => Some("passing"),
                        "failure" | "failing" => Some("failing"),
                        _ => None,
                    };
                    if let (false, Some(state)) = (negated, state) {
                        filters.checks = Some(state.into());
                        continue;
                    }
                }
                // Any other qualifier names a scoped label, e.g. `area:ui`.
                _ if !value.starts_with('/') => {
                    let names = bounded_names(
                        split_list(rest).map(|part| if part.contains(':') { part } else { format!("{name}:{part}") }),
                    );
                    if !names.is_empty() {
                        if negated {
                            filters.excluded_labels.extend(names);
                        } else {
                            filters.labels.push(names);
                        }
                        continue;
                    }
                }
                _ => {}
            }
        }
        text.push(token);
    }
    filters.labels.truncate(MAX_QUALIFIER_VALUES);
    filters.excluded_labels.truncate(MAX_QUALIFIER_VALUES);
    (text.join(" "), filters)
}

/// `applyPullRequestDraftFilterChange`: picking a Draft value takes a typed `draft:` qualifier
/// out of the query, which would otherwise keep outranking the menu.
pub fn drop_draft_qualifier(query: &str) -> String {
    quoted_runs(query, char::is_whitespace)
        .into_iter()
        .filter(|token| parse_query(token).1.draft.is_none())
        .collect::<Vec<_>>()
        .join(" ")
}

fn normalize(value: &str) -> Option<String> {
    let trimmed = value.trim().to_lowercase();
    (!trimmed.is_empty()).then_some(trimmed)
}

pub fn viewer_for<'a>(entry: &Entry, viewers: &'a HashMap<String, String>) -> Option<&'a str> {
    viewers.get(&entry.host).map(String::as_str).filter(|viewer| !viewer.trim().is_empty())
}

fn authored_by_viewer(entry: &Entry, viewers: &HashMap<String, String>) -> bool {
    let viewer = viewer_for(entry, viewers).and_then(normalize);
    viewer.is_some() && entry.author.as_ref().and_then(|a| normalize(&a.login)) == viewer
}

/// `resolvePullRequestAuthorFilter`: `author:me` (or `@me`) is whoever is signed in to the host.
fn resolve_author<'a>(author: &'a str, viewer: Option<&'a str>) -> &'a str {
    let trimmed = author.trim();
    let me = trimmed.trim_start_matches('@').eq_ignore_ascii_case("me") && trimmed.len() <= 3;
    match (me, viewer) {
        (true, Some(viewer)) if !viewer.trim().is_empty() => viewer,
        _ => trimmed,
    }
}

pub fn matches_filters(entry: &Entry, filters: &Filters, viewer: Option<&str>) -> bool {
    let labels: Vec<String> = entry.labels.iter().map(|l| l.name.trim().to_lowercase()).collect();
    let holds = |label: &String| labels.contains(&label.trim().to_lowercase());
    filters.draft.as_ref().is_none_or(|draft| entry.is_draft == (draft == "only"))
        && filters.review.as_ref().is_none_or(|review| match review.as_str() {
            "none" => entry.review_decision.is_none(),
            review => entry.review_decision.as_deref() == Some(review),
        })
        && filters.labels.iter().all(|group| group.iter().any(holds))
        && !filters.excluded_labels.iter().any(holds)
        && filters.author.as_ref().is_none_or(|author| {
            entry.author.as_ref().is_some_and(|a| a.login.eq_ignore_ascii_case(resolve_author(author, viewer)))
        })
}

pub fn matches_query(entry: &Entry, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let author = entry.author.as_ref().map_or("", |a| a.login.as_str());
    format!("#{} {} {} {} {}", entry.number, entry.title, entry.repository, entry.head_branch, author)
        .to_lowercase()
        .contains(&query)
}

/// `scorePullRequestMatch`: host search also matches bodies and commits a row does not show.
pub fn score_match(entry: &Entry, query: &str) -> u32 {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return 0;
    }
    let number = needle.strip_prefix('#').unwrap_or(&needle);
    if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
        return if entry.number.to_string() == number { 100 } else { 0 };
    }
    let title = entry.title.to_lowercase();
    let terms: Vec<&str> = needle.split_whitespace().collect();
    if title == needle {
        90
    } else if title.contains(&needle) {
        80
    } else if terms.len() > 1 && terms.iter().all(|term| title.contains(term)) {
        70
    } else if entry.head_branch.to_lowercase().contains(&needle) {
        60
    } else if entry.author.as_ref().is_some_and(|a| a.login.to_lowercase().contains(&needle)) {
        50
    } else if entry.repository.to_lowercase().contains(&needle) {
        40
    } else if terms.iter().any(|term| title.contains(term)) {
        30
    } else {
        10
    }
}

/// A row the host matched by its description rather than anything it shows.
pub fn matched_elsewhere(entry: &Entry, query: &str) -> bool {
    !query.is_empty() && score_match(entry, query) <= MATCHED_ELSEWHERE_SCORE
}

/// `comparePullRequestsByRecency`: newest update first, undated last.
pub fn by_recency(left: &Entry, right: &Entry) -> Ordering {
    match (left.updated_ms, right.updated_ms) {
        (Some(l), Some(r)) => r.cmp(&l),
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

pub fn rank_matches(entries: &mut [Rc<Entry>], query: &str) {
    if query.trim().is_empty() {
        return;
    }
    entries.sort_by(|l, r| score_match(r, query).cmp(&score_match(l, query)).then_with(|| by_recency(l, r)));
}

/// A group of rows under one heading; an unlabeled group has no header.
#[derive(Clone, Debug)]
pub struct Group {
    /// `authored`, `reviewRequested` or `others`.
    pub key: &'static str,
    pub label: &'static str,
    pub entries: Vec<Rc<Entry>>,
}

/// `groupPullRequestsByInvolvement`.
pub fn group_by_involvement(entries: Vec<Rc<Entry>>, viewers: &HashMap<String, String>) -> Vec<Group> {
    let mut groups = [
        Group { key: "authored", label: "Authored", entries: Vec::new() },
        Group { key: "reviewRequested", label: "Review requested", entries: Vec::new() },
        Group { key: "others", label: "Others", entries: Vec::new() },
    ];
    for entry in entries {
        let slot = if authored_by_viewer(&entry, viewers) {
            0
        } else if entry.viewer_review_requested {
            1
        } else {
            2
        };
        groups[slot].entries.push(entry);
    }
    groups.into_iter().filter(|group| !group.entries.is_empty()).collect()
}

/// Diff sizes `pullRequests.listStats` measured for rows the listing left at zero.
pub type Stats = HashMap<String, (u64, u64)>;

pub fn diff_stat(entry: &Entry, stats: &Stats) -> Option<(u64, u64)> {
    if entry.additions + entry.deletions > 0 {
        return Some((entry.additions, entry.deletions));
    }
    stats.get(&entry.stat_key()).copied()
}

fn size_of(entry: &Entry, stats: &Stats) -> (bool, u64) {
    diff_stat(entry, stats).map_or((false, 0), |(a, d)| (true, a + d))
}

fn readiness_tier(entry: &Entry) -> u8 {
    if entry.mergeability == "conflicting" {
        4
    } else if entry.state != "open" {
        3
    } else if entry.is_draft {
        2
    } else if entry.checks_state.as_deref() == Some("passing") && entry.review_decision.as_deref() == Some("approved") {
        0
    } else if entry.checks_state.as_deref() == Some("passing") {
        1
    } else {
        2
    }
}

fn blocked_on_author_tier(entry: &Entry) -> u8 {
    if entry.state != "open" {
        6
    } else if entry.mergeability == "conflicting" {
        0
    } else if entry.review_decision.as_deref() == Some("changes-requested") {
        1
    } else if entry.checks_state.as_deref() == Some("failing") {
        2
    } else if entry.is_draft {
        3
    } else if entry.checks_state.as_deref() == Some("passing") && entry.review_decision.as_deref() == Some("approved") {
        5
    } else {
        4
    }
}

/// `sortPullRequestGroups`. Sorting by readiness or blockers yields to search ranking while text is typed.
pub fn sort_groups(groups: &mut [Group], sort: &str, text: &str, involvement: &str, stats: &Stats) {
    let searching = !text.trim().is_empty();
    let measured_then = |l: &Entry, r: &Entry, sized: Ordering| {
        let (lm, ls) = size_of(l, stats);
        let (rm, rs) = size_of(r, stats);
        rm.cmp(&lm).then(match sized {
            Ordering::Less => ls.cmp(&rs),
            _ => rs.cmp(&ls),
        })
    };
    let timestamp = |e: &Entry| e.updated_ms.or(e.created_ms).unwrap_or(0);
    for group in groups.iter_mut() {
        let entries = &mut group.entries;
        match sort {
            "ready" if !searching => entries.sort_by(|l, r| {
                readiness_tier(l)
                    .cmp(&readiness_tier(r))
                    .then_with(|| measured_then(l, r, Ordering::Less))
                    .then_with(|| by_recency(l, r))
            }),
            "blocked" if !searching => {
                let role = match group.key {
                    "others" => involvement,
                    "authored" => "authored",
                    _ => "reviewing",
                };
                match role {
                    "authored" => entries.sort_by(|l, r| {
                        blocked_on_author_tier(l).cmp(&blocked_on_author_tier(r)).then_with(|| by_recency(l, r))
                    }),
                    "reviewing" => entries
                        .sort_by(|l, r| (l.state != "open").cmp(&(r.state != "open")).then_with(|| by_recency(l, r))),
                    _ => {}
                }
            }
            "newest" | "oldest" => entries.sort_by(|l, r| {
                r.created_ms
                    .is_some()
                    .cmp(&l.created_ms.is_some())
                    .then_with(|| {
                        let dated = l.created_ms.unwrap_or(0).cmp(&r.created_ms.unwrap_or(0));
                        if sort == "newest" { dated.reverse() } else { dated }
                    })
                    .then_with(|| timestamp(r).cmp(&timestamp(l)))
            }),
            "largest" | "smallest" => entries.sort_by(|l, r| {
                let order = if sort == "largest" { Ordering::Greater } else { Ordering::Less };
                measured_then(l, r, order).then_with(|| timestamp(r).cmp(&timestamp(l)))
            }),
            _ => {}
        }
    }
}

/// What the page asked for and how it reads the answer.
pub struct View<'a> {
    /// The listing's state, project and host: rows held from an earlier answer are narrowed to
    /// them while the new read travels (`narrowPullRequestsToFilters`).
    pub state: &'a str,
    pub project: Option<&'a str>,
    pub host: Option<&'a str>,
    pub involvement: &'a str,
    pub sort: &'a str,
    /// Menu filters plus the qualifiers typed so far.
    pub local_filters: &'a Filters,
    /// The typed search text (qualifiers removed).
    pub text: &'a str,
    /// The host has answered the typed text: rows from hosts that search on the host stay even
    /// when nothing they show matches.
    pub answered_locally: bool,
}

/// `pullRequestGroups` + `usePullRequestDisplayGroups`: the rows narrowed to what the reader
/// asked for, grouped, and sorted.
pub fn display_groups(list: &ListResult, view: &View, stats: &Stats) -> Vec<Group> {
    let searching_hosts: Vec<&str> =
        list.providers.iter().filter(|p| p.searches_on_host).map(|p| p.host.as_str()).collect();
    let mut entries: Vec<Rc<Entry>> = list
        .entries
        .iter()
        .filter(|entry| {
            (view.state == "all" || entry.state == view.state)
                && view.project.is_none_or(|project| entry.project_id == project)
                && view.host.is_none_or(|host| entry.host == host)
        })
        .filter(|entry| match view.involvement {
            "reviewing" => entry.viewer_review_requested,
            "authored" => authored_by_viewer(entry, &list.viewers),
            _ => true,
        })
        .filter(|entry| {
            view.local_filters.is_empty()
                || matches_filters(entry, view.local_filters, viewer_for(entry, &list.viewers))
        })
        .filter(|entry| {
            view.text.is_empty()
                || (view.answered_locally && searching_hosts.contains(&entry.host.as_str()))
                || matches_query(entry, view.text)
        })
        .cloned()
        .collect();
    if view.text.is_empty() {
        entries.sort_by(|l, r| by_recency(l, r));
    }
    let mut groups = if view.involvement == "all" {
        group_by_involvement(entries, &list.viewers)
    } else {
        vec![Group { key: "others", label: "", entries }]
    };
    sort_groups(&mut groups, view.sort, view.text, view.involvement, stats);
    groups
}

pub struct AuthorFacet {
    pub login: String,
    pub count: usize,
}

pub struct LabelFacet {
    pub name: String,
    pub count: usize,
}

/// `collectPullRequestListFacets`: who wrote the loaded rows and which labels they carry.
pub fn facets(entries: &[Rc<Entry>], state: &str) -> (Vec<AuthorFacet>, Vec<LabelFacet>) {
    let mut authors: Vec<(String, AuthorFacet, usize)> = Vec::new();
    let mut labels: Vec<(String, LabelFacet)> = Vec::new();
    for entry in entries {
        let in_state = state == "all" || entry.state == state;
        if let Some(author) = &entry.author
            && let Some(key) = normalize(&author.login)
        {
            match authors.iter_mut().find(|(k, ..)| *k == key) {
                Some((_, facet, merged)) => {
                    facet.count += usize::from(in_state);
                    *merged += usize::from(entry.state == "merged");
                }
                None => authors.push((
                    key,
                    AuthorFacet { login: author.login.clone(), count: usize::from(in_state) },
                    usize::from(entry.state == "merged"),
                )),
            }
        }
        if !in_state {
            continue;
        }
        for label in &entry.labels {
            let Some(key) = normalize(&label.name) else { continue };
            match labels.iter_mut().find(|(k, _)| *k == key) {
                Some((_, facet)) => facet.count += 1,
                None => labels.push((key, LabelFacet { name: label.name.clone(), count: 1 })),
            }
        }
    }
    authors.retain(|(_, facet, _)| facet.count > 0);
    authors
        .sort_by(|(_, l, lm), (_, r, rm)| rm.cmp(lm).then(r.count.cmp(&l.count)).then_with(|| l.login.cmp(&r.login)));
    labels.sort_by(|(_, l), (_, r)| r.count.cmp(&l.count).then_with(|| l.name.cmp(&r.name)));
    (authors.into_iter().map(|(_, f, _)| f).collect(), labels.into_iter().map(|(_, f)| f).collect())
}

/// `pullRequestLabelColor`: a six-digit hex color, else none.
pub fn label_color(color: Option<&str>) -> Option<u32> {
    let hex = color?.trim().trim_start_matches('#');
    (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten()
}

/// Appends a continuation's rows the list does not hold yet, newest first and ranked by the
/// search, after the rows already shown (a continuation never reorders what was read).
pub fn append_page(held: &mut ListResult, mut next: ListResult, query: &str) {
    let mut arrived: Vec<Rc<Entry>> =
        next.entries.drain(..).filter(|entry| !held.entries.iter().any(|h| h.key() == entry.key())).collect();
    arrived.sort_by(|l, r| by_recency(l, r));
    rank_matches(&mut arrived, query);
    held.entries.extend(arrived);
    held.truncated = next.truncated;
    held.next_cursors = next.next_cursors;
    held.errors = next.errors;
    held.providers = next.providers;
    held.viewers.extend(next.viewers);
}

/// The error text the web shows for a failed read (`formatEnvironmentQueryError`, which reads
/// each tagged error's `message` getter).
pub fn error_message(error: &RpcError) -> String {
    let RpcError::Failure(value) = error else { return "The environment request failed.".into() };
    let field = |key: &str| value.get(key).and_then(Value::as_str);
    match field("_tag") {
        Some("PullRequestOperationError") => format!(
            "Pull request operation {} failed: {}",
            field("operation").unwrap_or_default(),
            field("detail").unwrap_or_default()
        ),
        Some("PullRequestUnavailableError") => match field("reason") {
            Some("cli-missing") => {
                field("requirement").unwrap_or("The tool this host is read through is not installed or set up.").into()
            }
            Some("cli-unauthenticated") => {
                field("requirement").unwrap_or("This host has no working credentials.").into()
            }
            _ => "Change requests cannot be browsed for this project's host yet.".into(),
        },
        _ => field("message").or(field("detail")).unwrap_or("The environment request failed.").into(),
    }
}

/// "Open on GitHub", from the provider kind (`openOnHostLabel`).
pub fn open_on_host_label(provider: &str) -> &'static str {
    match provider {
        "github" => "Open on GitHub",
        "gitlab" => "Open on GitLab",
        "bitbucket" => "Open on Bitbucket",
        "azure-devops" => "Open on Azure DevOps",
        _ => "Open on host",
    }
}

pub fn provider_name(kind: &str) -> &'static str {
    match kind {
        "github" => "GitHub",
        "gitlab" => "GitLab",
        "bitbucket" => "Bitbucket",
        "azure-devops" => "Azure DevOps",
        _ => "Source control",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(number: u64, patch: Value) -> Rc<Entry> {
        let mut value = json!({
            "provider": "github", "host": "github.com", "projectId": "p1", "repository": "acme/app",
            "number": number, "title": format!("PR {number}"), "url": format!("https://github.com/acme/app/pull/{number}"),
            "author": {"login": "octo", "name": null, "avatarUrl": null}, "headBranch": "feature", "baseBranch": "main",
            "state": "open", "isDraft": false, "mergeability": "mergeable", "additions": 0, "deletions": 0,
            "createdAt": "2026-10-01T00:00:00.000Z", "updatedAt": format!("2026-10-{:02}T00:00:00.000Z", number),
            "viewerReviewRequested": false, "labels": [],
        });
        if let (Value::Object(target), Value::Object(fields)) = (&mut value, patch) {
            target.extend(fields);
        }
        let list: ListResult = serde_json::from_value(json!({"entries": [value]})).unwrap();
        list.entries[0].clone()
    }

    fn numbers(group: &Group) -> Vec<u64> {
        group.entries.iter().map(|e| e.number).collect()
    }

    #[test]
    fn parses_qualifiers_out_of_the_search_text() {
        let (text, filters) = parse_query(
            r#"fix crash label:bug,"needs review" -label:wip author:@me draft:true review:changes_requested checks:failure area:ui"#,
        );
        assert_eq!(text, "fix crash");
        assert_eq!(filters.labels, vec![vec!["bug".to_string(), "needs review".into()], vec!["area:ui".into()]]);
        assert_eq!(filters.excluded_labels, vec!["wip".to_string()]);
        assert_eq!(filters.author.as_deref(), Some("@me"));
        assert_eq!(filters.draft.as_deref(), Some("only"));
        assert_eq!(filters.review.as_deref(), Some("changes-requested"));
        assert_eq!(filters.checks.as_deref(), Some("failing"));
        // Unknown values, negations that mean nothing, empty values and paths stay text.
        let (text, filters) = parse_query("draft:maybe -author:me label: http:/x");
        assert_eq!(text, "draft:maybe -author:me label: http:/x");
        assert!(filters.is_empty());
    }

    #[test]
    fn picking_a_draft_value_drops_the_typed_qualifier() {
        assert_eq!(drop_draft_qualifier("crash draft:true label:bug"), "crash label:bug");
    }

    #[test]
    fn filters_match_labels_authors_and_reviews() {
        let row = entry(1, json!({"labels": [{"name": "Bug", "color": "d73a4a"}], "reviewDecision": "approved"}));
        let parse = |q: &str| parse_query(q).1;
        assert!(matches_filters(&row, &parse("label:bug,docs review:approved author:me"), Some("Octo")));
        assert!(!matches_filters(&row, &parse("-label:bug"), None));
        assert!(!matches_filters(&row, &parse("review:none"), None));
        assert!(!matches_filters(&row, &parse("author:me"), Some("someone")));
        assert!(matches_filters(&row, &parse("draft:false"), None));
    }

    #[test]
    fn groups_by_involvement_and_sorts_by_merge_readiness() {
        let viewers = HashMap::from([("github.com".to_string(), "octo".to_string())]);
        let list = ListResult {
            viewers,
            entries: vec![
                entry(1, json!({"author": {"login": "other"}})),
                entry(2, json!({"author": {"login": "other"}, "viewerReviewRequested": true})),
                entry(3, json!({"checksState": "passing", "reviewDecision": "approved"})),
                entry(4, json!({"mergeability": "conflicting"})),
                entry(5, json!({"checksState": "passing"})),
            ],
            ..ListResult::default()
        };
        let filters = Filters::default();
        let view = View {
            state: "open",
            project: None,
            host: None,
            involvement: "all",
            sort: "ready",
            local_filters: &filters,
            text: "",
            answered_locally: false,
        };
        let groups = display_groups(&list, &view, &Stats::new());
        let keys: Vec<&str> = groups.iter().map(|g| g.key).collect();
        assert_eq!(keys, ["authored", "reviewRequested", "others"]);
        assert_eq!(numbers(&groups[0]), [3, 5, 4], "approved and passing, then passing, conflicts last");

        let view = View { involvement: "reviewing", ..view };
        let groups = display_groups(&list, &view, &Stats::new());
        assert_eq!((groups.len(), groups[0].label, numbers(&groups[0])), (1, "", vec![2]));

        // Rows held from an open listing leave as soon as the state filter moves to merged.
        let view = View { involvement: "all", state: "merged", ..view };
        assert!(display_groups(&list, &view, &Stats::new()).is_empty());
    }

    #[test]
    fn sorts_by_measured_size_and_creation() {
        let mut stats = Stats::new();
        let small = entry(1, json!({"additions": 3, "deletions": 1}));
        let measured_later = entry(2, json!({}));
        stats.insert(measured_later.stat_key(), (100, 50));
        let unmeasured = entry(3, json!({}));
        let mut groups = vec![Group { key: "others", label: "", entries: vec![small, measured_later, unmeasured] }];
        sort_groups(&mut groups, "largest", "", "all", &stats);
        assert_eq!(numbers(&groups[0]), [2, 1, 3], "unmeasured rows trail");
        sort_groups(&mut groups, "smallest", "", "all", &stats);
        assert_eq!(numbers(&groups[0]), [1, 2, 3]);

        let older = entry(4, json!({"createdAt": "2026-01-01T00:00:00Z"}));
        let newer = entry(5, json!({"createdAt": "2026-06-01T00:00:00Z"}));
        let mut groups = vec![Group { key: "others", label: "", entries: vec![older, newer] }];
        sort_groups(&mut groups, "newest", "", "all", &stats);
        assert_eq!(numbers(&groups[0]), [5, 4]);
        sort_groups(&mut groups, "oldest", "", "all", &stats);
        assert_eq!(numbers(&groups[0]), [4, 5]);
    }

    #[test]
    fn search_ranks_and_flags_description_matches() {
        let crash = entry(1, json!({"title": "Fix crash on start"}));
        let branch = entry(2, json!({"title": "Something", "headBranch": "crash-fix"}));
        let body = entry(3, json!({"title": "Unrelated"}));
        let mut rows = vec![body.clone(), branch, crash];
        rank_matches(&mut rows, "crash");
        assert_eq!(rows.iter().map(|e| e.number).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(matched_elsewhere(&body, "crash") && !matched_elsewhere(&rows[0], "crash"));
        assert_eq!(score_match(&rows[0], "#1"), 100);
        assert!(matches_query(&rows[1], "CRASH-fix") && !matches_query(&body, "crash"));
    }

    #[test]
    fn a_continuation_appends_only_new_rows() {
        let mut held = ListResult { entries: vec![entry(1, json!({})), entry(2, json!({}))], ..ListResult::default() };
        let next = ListResult { entries: vec![entry(2, json!({})), entry(3, json!({}))], ..ListResult::default() };
        append_page(&mut held, next, "");
        assert_eq!(held.entries.iter().map(|e| e.number).collect::<Vec<_>>(), [1, 2, 3]);
    }

    #[test]
    fn facets_count_authors_and_labels_in_state() {
        let rows = vec![
            entry(1, json!({"labels": [{"name": "bug", "color": null}]})),
            entry(2, json!({"labels": [{"name": "Bug", "color": null}, {"name": "ui", "color": null}]})),
            entry(
                3,
                json!({"state": "merged", "author": {"login": "merger"}, "labels": [{"name": "ui", "color": null}]}),
            ),
        ];
        let (authors, labels) = facets(&rows, "open");
        assert_eq!(authors.iter().map(|a| (a.login.as_str(), a.count)).collect::<Vec<_>>(), [("octo", 2)]);
        assert_eq!(labels.iter().map(|l| (l.name.as_str(), l.count)).collect::<Vec<_>>(), [("bug", 2), ("ui", 1)]);
    }

    #[test]
    fn reads_tagged_errors_like_their_message_getters() {
        let failure = RpcError::Failure(
            json!({"_tag": "PullRequestOperationError", "operation": "list", "detail": "gh timed out"}),
        );
        assert_eq!(error_message(&failure), "Pull request operation list failed: gh timed out");
        let unavailable =
            RpcError::Failure(json!({"_tag": "PullRequestUnavailableError", "reason": "cli-unauthenticated"}));
        assert_eq!(error_message(&unavailable), "This host has no working credentials.");
        assert_eq!(label_color(Some("#D73A4A")), Some(0xd73a4a));
        assert_eq!(label_color(Some("red")), None);
    }
}
