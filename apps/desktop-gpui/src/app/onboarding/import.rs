//! The Projects band's logic (`projectImport.logic.ts`, `welcomeState.ts` and the parts of
//! `useProjectImport` that decide): which projects agent history found, which start picked,
//! how they group, and what a partial import reports.

use std::collections::{HashMap, HashSet};

use gpui_kit::SharedString;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::model::ProjectShell;
use crate::project_groups::normalize_path;

/// `AgentSessionSource` with `IMPORT_SOURCE_NAMES`, in the contract's order.
pub(super) const SOURCES: [(&str, &str); 3] = [("claudeAgent", "Claude Code"), ("codex", "Codex"), ("pi", "Pi")];
/// `IMPORT_SOURCES_LABEL`: the names as `Intl.ListFormat("en", { type: "conjunction" })` joins them.
pub(super) const SOURCES_LABEL: &str = "Claude Code, Codex, and Pi";
const RECENT_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const DEFAULT_SELECTION_MIN_THREADS: u64 = 3;

/// A scanned project's repository (`AgentSessionProjectGit`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Git {
    #[serde(default)]
    pub remote_key: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
}

/// `AgentSessionProjectCandidate`, with its computer and its key (`ImportCandidate`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Candidate {
    pub path: String,
    pub title: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub thread_count: u64,
    #[serde(default)]
    pub last_active_at: Option<String>,
    /// `None` from a server predating git scans, read as a repository; `Some(None)` is a folder.
    #[serde(default, deserialize_with = "present")]
    pub git: Option<Option<Git>>,
    #[serde(skip)]
    pub environment: SharedString,
    /// `onboardingProjectKey`: paths repeat across computers.
    #[serde(skip)]
    pub key: String,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Option<Git>>, D::Error> {
    Option::<Git>::deserialize(deserializer).map(Some)
}

/// `AgentSessionScanResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub(super) struct ScanResult {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub truncated: Option<bool>,
}

impl ScanResult {
    /// The scan of `environment`, each candidate keyed by its computer and path.
    pub(super) fn parse(environment: &SharedString, value: Value) -> Result<Self, String> {
        let mut result: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        for candidate in &mut result.candidates {
            candidate.environment = environment.clone();
            candidate.key = project_key(environment, &candidate.path);
        }
        Ok(result)
    }

    pub(super) fn truncated(&self) -> bool {
        self.truncated == Some(true)
    }
}

/// `onboardingProjectKey`.
pub(super) fn project_key(environment: &str, path: &str) -> String {
    serde_json::to_string(&[environment, path]).unwrap_or_default()
}

/// `partitionOnboardingProjects(...).recent`: a repository with a few threads in the last 30 days.
pub(super) fn is_recent(candidate: &Candidate, now_ms: i64) -> bool {
    if candidate.git == Some(None) || candidate.thread_count < DEFAULT_SELECTION_MIN_THREADS {
        return false;
    }
    let Some(at) = candidate.last_active_at.as_deref().and_then(crate::timestamp::parse_ms) else { return false };
    at >= now_ms - RECENT_WINDOW_MS && at <= now_ms
}

/// `OnboardingProjectGroup`: a repository's checkouts.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Group<'a> {
    pub key: String,
    pub label: String,
    pub candidates: Vec<&'a Candidate>,
    pub thread_count: u64,
    pub last_active_at: Option<&'a str>,
}

impl Group<'_> {
    /// The sources any checkout's history came from.
    pub(super) fn sources(&self) -> Vec<&str> {
        let mut sources: Vec<&str> = Vec::new();
        for source in self.candidates.iter().flat_map(|c| &c.sources) {
            if !sources.contains(&source.as_str()) {
                sources.push(source);
            }
        }
        sources
    }
}

/// `groupOnboardingProjects`: checkouts of one remote together, newest first; folders that
/// are not repositories apart.
pub(super) fn group_projects<'a>(candidates: &[&'a Candidate]) -> (Vec<Group<'a>>, Vec<&'a Candidate>) {
    let mut groups: Vec<Group<'a>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut other = Vec::new();
    for &candidate in candidates {
        let git = match &candidate.git {
            Some(None) => {
                other.push(candidate);
                continue;
            }
            Some(Some(git)) => git.clone(),
            None => Git::default(),
        };
        let key = match &git.remote_key {
            Some(remote) => format!("remote:{remote}"),
            None => format!("path:{}", candidate.path),
        };
        let at = candidate.last_active_at.as_deref();
        if let Some(&i) = index.get(&key) {
            let group = &mut groups[i];
            group.candidates.push(candidate);
            group.thread_count += candidate.thread_count;
            group.last_active_at = match (group.last_active_at, at) {
                (Some(left), Some(right)) => Some(if left > right { left } else { right }),
                (left, right) => left.or(right),
            };
            continue;
        }
        index.insert(key.clone(), groups.len());
        groups.push(Group {
            key,
            label: git.repository.unwrap_or_else(|| candidate.title.clone()),
            candidates: vec![candidate],
            thread_count: candidate.thread_count,
            last_active_at: at,
        });
    }
    groups.sort_by(|left, right| match (left.last_active_at, right.last_active_at) {
        (l, r) if l == r => locale_compare(&left.label, &right.label),
        (None, _) => std::cmp::Ordering::Greater,
        (_, None) => std::cmp::Ordering::Less,
        (Some(l), Some(r)) => r.cmp(l),
    });
    (groups, other)
}

/// `localeCompare`'s order for labels: letters before case.
fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase().cmp(&right.to_lowercase()).then_with(|| left.cmp(right))
}

/// `isNewOnboardingProject`: whether a candidate is not a project yet, reading the list once.
pub(super) struct KnownRoots(HashSet<String>);

impl KnownRoots {
    pub(super) fn new(projects: &[ProjectShell]) -> Self {
        Self(projects.iter().map(|p| format!("{}\0{}", p.environment_id, normalize_path(&p.workspace_root))).collect())
    }

    pub(super) fn is_new(&self, candidate: &Candidate) -> bool {
        if candidate.project_id.is_some() {
            return false;
        }
        let path = normalize_path(&candidate.path);
        path.is_empty() || !self.0.contains(&format!("{}\0{path}", candidate.environment))
    }
}

/// `resolveOnboardingProjectId`: the server's match first, then a project at the same root.
pub(super) fn resolve_project_id(projects: &[ProjectShell], candidate: &Candidate) -> Option<String> {
    if let Some(id) = &candidate.project_id {
        return Some(id.clone());
    }
    let path = normalize_path(&candidate.path);
    if path.is_empty() {
        return None;
    }
    projects
        .iter()
        .find(|p| p.environment_id == candidate.environment && normalize_path(&p.workspace_root) == path)
        .map(|p| p.id.clone())
}

/// `resolveOnboardingLandingProject`: the first pick that brought history, else the first one
/// imported in full.
pub(super) fn landing_project<T: Clone>(
    selection: &[String],
    with_history: &HashMap<String, T>,
    completed: &HashMap<String, T>,
) -> Option<T> {
    let first = |map: &HashMap<String, T>| selection.iter().find_map(|key| map.get(key).cloned());
    first(with_history).or_else(|| first(completed))
}

/// `splitOnboardingProjectPath`: the folder's name, and its parent with the trailing separator.
pub(super) fn split_path(path: &str) -> (String, String) {
    let separators = ['/', '\\'];
    let bytes = path.as_bytes();
    let drive_root = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && path[2..].chars().all(|c| separators.contains(&c));
    // Both separators are one byte, so the slice after them starts on a char boundary.
    let unc_root = bytes.len() > 2 && bytes[..2].iter().all(|b| matches!(b, b'/' | b'\\')) && {
        let parts: Vec<&str> = path[2..].trim_end_matches(separators).split(separators).collect();
        parts.len() == 2 && parts.iter().all(|part| !part.is_empty())
    };
    if drive_root || unc_root {
        return (path.to_owned(), String::new());
    }
    let trimmed = path.trim_end_matches(separators);
    if trimmed.is_empty() {
        return (path.to_owned(), String::new());
    }
    match trimmed.rfind(separators) {
        Some(at) => (trimmed[at + 1..].to_owned(), trimmed[..=at].to_owned()),
        None => (trimmed.to_owned(), String::new()),
    }
}

/// `WelcomeImportSelection`: a computer's picks, kept until setup finishes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Selection {
    /// Projects listed when the selection changed; projects found later get the defaults.
    pub known_keys: Vec<String>,
    pub keys: Vec<String>,
    /// Unchecking the main box also leaves out projects found later, until it is checked again.
    pub opted_out: bool,
}

impl Selection {
    /// `setSelectedKeys`: `keys` picked among everything listed now.
    pub(super) fn picked(candidates: &[&Candidate], keys: impl IntoIterator<Item = String>, opted_out: bool) -> Self {
        Self {
            known_keys: candidates.iter().map(|c| c.key.clone()).collect(),
            keys: keys.into_iter().collect(),
            opted_out,
        }
    }
}

/// The band's picks as `useProjectImport` and `ProjectsSection` read them.
#[derive(Debug)]
pub(super) struct Picks<'a> {
    pub selected: Vec<&'a Candidate>,
    /// What checking the main box picks: recent new projects, else any new ones, else all.
    pub default_keys: Vec<String>,
    /// The main box shows a dash: some, but neither none, all, nor the defaults.
    pub partial: bool,
    /// New projects among the picks, which the Start button counts.
    pub new_count: usize,
}

pub(super) fn picks<'a>(
    candidates: &[&'a Candidate],
    roots: &KnownRoots,
    selection: Option<&Selection>,
    now_ms: i64,
) -> Picks<'a> {
    let fresh: Vec<&Candidate> = candidates.iter().copied().filter(|c| roots.is_new(c)).collect();
    let recent: Vec<&Candidate> = fresh.iter().copied().filter(|c| is_recent(c, now_ms)).collect();
    let recent_keys: HashSet<&str> = recent.iter().map(|c| c.key.as_str()).collect();
    let selected: Vec<&Candidate> = match selection {
        Some(selection) if selection.opted_out => Vec::new(),
        _ => {
            let known: HashSet<&str> =
                selection.map(|s| s.known_keys.iter().map(String::as_str).collect()).unwrap_or_default();
            let picked: HashSet<&str> =
                selection.map(|s| s.keys.iter().map(String::as_str).collect()).unwrap_or_default();
            candidates
                .iter()
                .copied()
                .filter(|c| {
                    if known.contains(c.key.as_str()) {
                        picked.contains(c.key.as_str())
                    } else {
                        recent_keys.contains(c.key.as_str())
                    }
                })
                .collect()
        }
    };
    let defaults = if !recent.is_empty() {
        &recent
    } else if !fresh.is_empty() {
        &fresh
    } else {
        candidates
    };
    let default_keys: Vec<String> = defaults.iter().map(|c| c.key.clone()).collect();
    let selected_keys: HashSet<&str> = selected.iter().map(|c| c.key.as_str()).collect();
    let is_default =
        selected.len() == default_keys.len() && default_keys.iter().all(|key| selected_keys.contains(key.as_str()));
    let partial = !selected.is_empty() && selected.len() < candidates.len() && !is_default;
    let new_count = selected.iter().filter(|c| roots.is_new(c)).count();
    Picks { selected, default_keys, partial, new_count }
}

/// The Start button's label (`WelcomePage`).
pub(super) fn start_label(importing: bool, selected: usize, new_count: usize) -> String {
    match (importing, selected, new_count) {
        (true, 0, _) => "Starting…".into(),
        (true, _, _) => "Importing…".into(),
        (false, _, 1) => "Import 1 project and start".into(),
        (false, _, n) if n > 1 => format!("Import {n} projects and start"),
        (false, 0, _) => "Start".into(),
        (false, _, _) => "Import threads and start".into(),
    }
}

fn threads(count: usize) -> String {
    format!("{count} {}", if count == 1 { "thread" } else { "threads" })
}

/// `runImport`'s warning when some picks were not imported in full.
pub(super) fn import_warning(imported: usize, skipped: usize) -> String {
    match (imported, skipped) {
        (0, 0) => "Could not import thread history.".into(),
        (0, 1) => "1 thread could not be imported.".into(),
        (0, n) => format!("{n} threads could not be imported."),
        (i, 0) => format!("Imported {}. Some thread history could not be imported.", threads(i)),
        (i, s) => {
            format!("Imported {}. {s} {} could not be imported.", threads(i), if s == 1 { "thread" } else { "threads" })
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const NOW: &str = "2026-11-09T12:00:00.000Z";

    fn now() -> i64 {
        crate::timestamp::parse_ms(NOW).unwrap()
    }

    fn scan(environment: &str, candidates: Value) -> ScanResult {
        ScanResult::parse(&environment.into(), json!({ "candidates": candidates, "scannedAt": NOW })).unwrap()
    }

    fn candidate(path: &str, fields: Value) -> Value {
        let mut base = json!({
            "path": path, "title": path.rsplit('/').next().unwrap(), "sources": ["codex"],
            "threadCount": 5, "lastActiveAt": "2026-11-08T12:00:00.000Z", "alreadyImported": false,
            "git": {"remoteKey": null, "repository": null},
        });
        base.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        base
    }

    fn project(environment: &str, id: &str, root: &str) -> ProjectShell {
        let mut project: ProjectShell =
            serde_json::from_value(json!({"id": id, "title": id, "workspaceRoot": root})).unwrap();
        project.environment_id = environment.to_owned().into();
        project
    }

    fn keys(list: &[&Candidate]) -> Vec<String> {
        list.iter().map(|c| c.path.clone()).collect()
    }

    #[test]
    fn scans_key_each_candidate_by_computer_and_read_a_missing_git_as_a_repository() {
        let a = scan("env-a", json!([candidate("/w/app", json!({"git": null})), candidate("/w/old", json!({}))]));
        let mut old = candidate("/w/old", json!({}));
        old.as_object_mut().unwrap().remove("git");
        let b = scan("env-b", json!([old]));
        assert_eq!(a.candidates[0].key, r#"["env-a","/w/app"]"#);
        assert_ne!(a.candidates[1].key, b.candidates[0].key, "a path repeats across computers");
        assert_eq!(a.candidates[0].git, Some(None));
        assert_eq!(b.candidates[0].git, None);
        assert!(!a.truncated());
        assert!(ScanResult::parse(&"e".into(), json!({"candidates": [], "truncated": true})).unwrap().truncated());
        assert!(ScanResult::parse(&"e".into(), json!({"candidates": [{"title": "x"}]})).is_err());
    }

    #[test]
    fn recent_means_a_repository_with_three_threads_in_thirty_days() {
        let found = scan(
            "e",
            json!([
                candidate("/a", json!({})),
                candidate("/folder", json!({"git": null})),
                candidate("/few", json!({"threadCount": 2})),
                candidate("/stale", json!({"lastActiveAt": "2026-10-01T00:00:00.000Z"})),
                candidate("/future", json!({"lastActiveAt": "2026-11-10T00:00:00.000Z"})),
                candidate("/never", json!({"lastActiveAt": null})),
            ]),
        );
        let mut old = candidate("/old-server", json!({}));
        old.as_object_mut().unwrap().remove("git");
        let old = scan("e", json!([old]));
        let recent: Vec<&str> = found
            .candidates
            .iter()
            .chain(&old.candidates)
            .filter(|c| is_recent(c, now()))
            .map(|c| c.path.as_str())
            .collect();
        assert_eq!(recent, ["/a", "/old-server"]);
    }

    #[test]
    fn repositories_group_by_remote_newest_first_and_folders_stand_apart() {
        let found = scan(
            "e",
            json!([
                candidate(
                    "/w/one",
                    json!({"git": {"remoteKey": "gh/x", "repository": "x/repo"}, "lastActiveAt": "2026-11-01T00:00:00.000Z", "sources": ["codex"]})
                ),
                candidate("/w/plain", json!({"lastActiveAt": null})),
                candidate(
                    "/w/two",
                    json!({"git": {"remoteKey": "gh/x", "repository": "x/repo"}, "threadCount": 2, "lastActiveAt": "2026-11-05T00:00:00.000Z", "sources": ["claudeAgent", "codex"]})
                ),
                candidate("/w/tmp", json!({"git": null})),
                candidate("/w/beta", json!({"lastActiveAt": "2026-11-03T00:00:00.000Z"})),
            ]),
        );
        let all: Vec<&Candidate> = found.candidates.iter().collect();
        let (groups, other) = group_projects(&all);
        let summary: Vec<(&str, usize, u64, Option<&str>)> =
            groups.iter().map(|g| (g.label.as_str(), g.candidates.len(), g.thread_count, g.last_active_at)).collect();
        assert_eq!(
            summary,
            [
                ("x/repo", 2, 7, Some("2026-11-05T00:00:00.000Z")),
                ("beta", 1, 5, Some("2026-11-03T00:00:00.000Z")),
                ("plain", 1, 5, None),
            ]
        );
        assert_eq!(groups[0].sources(), ["codex", "claudeAgent"]);
        assert_eq!(keys(&other), ["/w/tmp"]);
    }

    #[test]
    fn a_candidate_is_new_unless_its_computer_already_has_the_folder() {
        let found = scan(
            "env-a",
            json!([
                candidate("/w/app/", json!({})),
                candidate("/w/other", json!({})),
                candidate("/w/known", json!({"projectId": "p-server"})),
            ]),
        );
        let projects = [project("env-a", "p-1", "/w/app"), project("env-b", "p-2", "/w/other")];
        let roots = KnownRoots::new(&projects);
        let fresh: Vec<&str> = found.candidates.iter().filter(|c| roots.is_new(c)).map(|c| c.path.as_str()).collect();
        assert_eq!(fresh, ["/w/other"], "another computer's project does not count");
        let ids: Vec<Option<String>> = found.candidates.iter().map(|c| resolve_project_id(&projects, c)).collect();
        assert_eq!(ids, [Some("p-1".into()), None, Some("p-server".into())]);
    }

    #[test]
    fn picks_default_to_recent_new_projects_until_the_user_chooses() {
        let found = scan(
            "e",
            json!([
                candidate("/recent", json!({})),
                candidate("/quiet", json!({"threadCount": 1})),
                candidate("/existing", json!({})),
            ]),
        );
        let all: Vec<&Candidate> = found.candidates.iter().collect();
        let roots = KnownRoots::new(&[project("e", "p", "/existing")]);
        let first = picks(&all, &roots, None, now());
        assert_eq!(keys(&first.selected), ["/recent"]);
        assert_eq!(first.default_keys, [all[0].key.clone()]);
        assert!(!first.partial, "the defaults are not a partial pick");
        assert_eq!(first.new_count, 1);

        // A project found after the choice gets the defaults; the known ones keep the choice.
        let chosen = Selection::picked(&all[1..], [all[2].key.clone()], false);
        let later = picks(&all, &roots, Some(&chosen), now());
        assert_eq!(keys(&later.selected), ["/recent", "/existing"]);
        assert!(later.partial);
        assert_eq!(later.new_count, 1);

        let out = Selection::picked(&all, [], true);
        assert!(picks(&all, &roots, Some(&out), now()).selected.is_empty());
        let every = Selection::picked(&all, all.iter().map(|c| c.key.clone()), false);
        assert!(!picks(&all, &roots, Some(&every), now()).partial, "all of them is not partial");

        // Without recent new projects the box picks any new ones, then everything.
        let none_recent = KnownRoots::new(&[project("e", "p", "/recent"), project("e", "q", "/existing")]);
        assert_eq!(picks(&all, &none_recent, None, now()).default_keys, [all[1].key.clone()]);
        let known = KnownRoots::new(&[
            project("e", "1", "/recent"),
            project("e", "2", "/quiet"),
            project("e", "3", "/existing"),
        ]);
        assert_eq!(picks(&all, &known, None, now()).default_keys.len(), 3);
    }

    #[test]
    fn the_landing_is_the_first_pick_that_brought_history() {
        let selection = ["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let history = HashMap::from([("c".to_owned(), 3), ("b".to_owned(), 2)]);
        let completed = HashMap::from([("a".to_owned(), 1)]);
        assert_eq!(landing_project(&selection, &history, &completed), Some(2));
        assert_eq!(landing_project(&selection, &HashMap::new(), &completed), Some(1));
        assert_eq!(landing_project::<i32>(&selection, &HashMap::new(), &HashMap::new()), None);
    }

    #[test]
    fn paths_split_into_name_and_parent_keeping_roots_whole() {
        let split = |path: &str| split_path(path);
        assert_eq!(split("/home/me/app"), ("app".into(), "/home/me/".into()));
        assert_eq!(split("/home/me/app//"), ("app".into(), "/home/me/".into()));
        assert_eq!(split(r"C:\Users\me\app"), ("app".into(), r"C:\Users\me\".into()));
        assert_eq!(split(r"C:\"), (r"C:\".into(), String::new()));
        assert_eq!(split(r"\\server\share\"), (r"\\server\share\".into(), String::new()));
        assert_eq!(split(r"\\server\share\app"), ("app".into(), r"\\server\share\".into()));
        assert_eq!(split("/"), ("/".into(), String::new()));
        assert_eq!(split("app"), ("app".into(), String::new()));
        assert_eq!(split("/home/mé/äpp"), ("äpp".into(), "/home/mé/".into()));
    }

    #[test]
    fn start_and_warnings_count_projects_and_threads() {
        assert_eq!(start_label(false, 0, 0), "Start");
        assert_eq!(start_label(false, 2, 1), "Import 1 project and start");
        assert_eq!(start_label(false, 3, 3), "Import 3 projects and start");
        assert_eq!(start_label(false, 2, 0), "Import threads and start");
        assert_eq!(start_label(true, 2, 2), "Importing…");
        assert_eq!(start_label(true, 0, 0), "Starting…");

        assert_eq!(import_warning(0, 0), "Could not import thread history.");
        assert_eq!(import_warning(0, 1), "1 thread could not be imported.");
        assert_eq!(import_warning(0, 4), "4 threads could not be imported.");
        assert_eq!(import_warning(1, 0), "Imported 1 thread. Some thread history could not be imported.");
        assert_eq!(import_warning(5, 1), "Imported 5 threads. 1 thread could not be imported.");
        assert_eq!(import_warning(2, 3), "Imported 2 threads. 3 threads could not be imported.");
    }

    #[test]
    fn a_selection_round_trips_in_the_web_shape() {
        let selection = Selection { known_keys: vec!["a".into()], keys: vec![], opted_out: true };
        let value = serde_json::to_value(&selection).unwrap();
        assert_eq!(value, json!({"knownKeys": ["a"], "keys": [], "optedOut": true}));
        assert_eq!(serde_json::from_value::<Selection>(value).unwrap(), selection);
    }
}
