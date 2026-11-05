//! Project groups (`buildProjectGroups` in `packages/client-runtime/src/state/projectGrouping.ts`,
//! `buildSidebarProjectSnapshots`) and the order the sidebar, its project filter and the palette
//! list them in (`sortLogicalProjectsForSidebar`). This client talks to one environment, so every
//! member shares it.

use std::collections::{BTreeMap, HashMap};

use gpui_kit::SharedString;
use serde_json::{Map, Value};

use crate::model::{ProjectShell, RepositoryIdentity, ThreadShell};
use crate::settings::Settings;

pub const SCRATCH_PROJECT_TITLE: &str = "No project";
/// A NUL keeps the scratch key out of the repository-key namespace.
const SCRATCH_PROJECT_SCOPE_KEY: &str = "\0no-project";

/// `SidebarProjectGroupingMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupingMode {
    Repository,
    RepositoryPath,
    Separate,
}

impl GroupingMode {
    pub const ALL: [Self; 3] = [Self::Repository, Self::RepositoryPath, Self::Separate];

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "repository" => Self::Repository,
            "repository_path" => Self::RepositoryPath,
            "separate" => Self::Separate,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Repository => "repository",
            Self::RepositoryPath => "repository_path",
            Self::Separate => "separate",
        }
    }

    /// `PROJECT_GROUPING_MODE_LABELS`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Repository => "Group by repository",
            Self::RepositoryPath => "Group by repository path",
            Self::Separate => "Keep separate",
        }
    }
}

/// The client settings that group projects: `sidebarProjectGroupingMode` and
/// `sidebarProjectGroupingOverrides` (keyed by physical project key).
#[derive(Clone, Debug, PartialEq)]
pub struct GroupingSettings {
    pub mode: GroupingMode,
    pub overrides: BTreeMap<String, GroupingMode>,
}

impl Default for GroupingSettings {
    fn default() -> Self {
        Self { mode: GroupingMode::Repository, overrides: BTreeMap::new() }
    }
}

impl GroupingSettings {
    pub fn from_settings(settings: &Settings) -> Self {
        Self::from_json(settings)
    }

    fn from_json(settings: &Value) -> Self {
        let mode = settings["sidebarProjectGroupingMode"].as_str().and_then(GroupingMode::parse);
        let overrides = settings["sidebarProjectGroupingOverrides"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, mode)| Some((key.clone(), GroupingMode::parse(mode.as_str()?)?)))
            .collect();
        Self { mode: mode.unwrap_or(GroupingMode::Repository), overrides }
    }

    fn mode_for(&self, physical_key: &str) -> GroupingMode {
        self.overrides.get(physical_key).copied().unwrap_or(self.mode)
    }

    /// `withProjectGroupingOverride`: `None` follows the global default again.
    pub fn set_override(&mut self, physical_key: &str, mode: Option<GroupingMode>) {
        match mode {
            Some(mode) => self.overrides.insert(physical_key.to_owned(), mode),
            None => self.overrides.remove(physical_key),
        };
    }

    /// The `sidebarProjectGroupingOverrides` client patch.
    pub fn overrides_patch(&self) -> Map<String, Value> {
        let overrides: Map<String, Value> =
            self.overrides.iter().map(|(key, mode)| (key.clone(), mode.as_str().into())).collect();
        Map::from_iter([("sidebarProjectGroupingOverrides".to_owned(), overrides.into())])
    }
}

/// `SidebarProjectSortOrder`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortOrder {
    UpdatedAt,
    CreatedAt,
    Manual,
}

impl SortOrder {
    pub fn from_settings(settings: &Settings) -> Self {
        match settings.get("sidebarProjectSortOrder").as_str() {
            Some("created_at") => Self::CreatedAt,
            Some("manual") => Self::Manual,
            _ => Self::UpdatedAt,
        }
    }
}

/// `normalizeProjectPathForComparison`: no trailing separators; Windows paths compare
/// case-insensitively with backslashes.
pub fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    let mut normalized = trimmed.trim_end_matches(['/', '\\']);
    if normalized.is_empty() && !trimmed.is_empty() {
        normalized = &trimmed[..1];
    }
    let bytes = normalized.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let unc = normalized.starts_with("\\\\") || normalized.starts_with("//");
    if drive || unc { normalized.replace('/', "\\").to_lowercase() } else { normalized.to_owned() }
}

/// `derivePhysicalProjectKey`.
pub fn physical_key(environment_id: &str, workspace_root: &str) -> String {
    format!("{environment_id}:{}", normalize_path(workspace_root))
}

fn repository_key(identity: &RepositoryIdentity) -> Option<&str> {
    identity.origin.as_ref().map(|origin| origin.canonical_key.as_str()).or(identity.canonical_key.as_deref())
}

/// `repositoryGroupingDisplayNameOf`.
fn repository_display_name(identity: &RepositoryIdentity) -> Option<&str> {
    match &identity.origin {
        Some(origin) => Some(origin.display_name.as_deref().unwrap_or(&origin.canonical_key)),
        None => identity.display_name.as_deref(),
    }
}

/// `deriveRepositoryRelativeProjectPath`.
fn repository_relative_path(project: &ProjectShell) -> Option<String> {
    let root = project.repository_identity.as_ref()?.root_path.as_deref()?.trim();
    let (path, root) = (normalize_path(&project.workspace_root), normalize_path(root));
    if path.is_empty() || root.is_empty() {
        return None;
    }
    if path == root {
        return Some(String::new());
    }
    let separator = if root.contains('\\') { '\\' } else { '/' };
    Some(path.strip_prefix(&format!("{root}{separator}"))?.replace('\\', "/"))
}

/// `deriveLogicalProjectKey`.
fn logical_key(project: &ProjectShell, mode: GroupingMode, physical: &str, scratch: bool) -> String {
    if scratch {
        return SCRATCH_PROJECT_SCOPE_KEY.to_owned();
    }
    let scoped = project.repository_identity.as_ref().and_then(repository_key).filter(|key| !key.contains('\0'));
    match (mode, scoped) {
        (GroupingMode::Separate, _) | (_, None) => physical.to_owned(),
        (GroupingMode::Repository, Some(key)) => key.to_owned(),
        (GroupingMode::RepositoryPath, Some(key)) => match repository_relative_path(project) {
            Some(relative) if !relative.is_empty() => format!("{key}::{relative}"),
            _ => key.to_owned(),
        },
    }
}

fn unique_non_empty<'a>(values: impl Iterator<Item = Option<&'a str>>) -> Vec<&'a str> {
    let mut unique = Vec::new();
    for value in values.flatten().map(str::trim).filter(|value| !value.is_empty()) {
        if !unique.contains(&value) {
            unique.push(value);
        }
    }
    unique
}

/// `deriveProjectGroupLabel`.
fn group_label(members: &[Member]) -> String {
    let projects = || members.iter().map(|member| &member.project);
    let titles = unique_non_empty(projects().map(|p| Some(p.title.as_str())));
    let identities = || projects().filter_map(|p| p.repository_identity.as_ref());
    let display_names = unique_non_empty(identities().map(repository_display_name));
    let names = unique_non_empty(identities().map(|identity| identity.name.as_deref()));
    match (titles.as_slice(), display_names.as_slice(), names.as_slice()) {
        ([title], ..) if !display_names.contains(title) && !names.contains(title) => (*title).to_owned(),
        (_, [display_name], _) => (*display_name).to_owned(),
        (.., [name]) => (*name).to_owned(),
        _ => members[0].project.title.clone(),
    }
}

/// `projectFreshnessTime` order: ISO timestamps in one format compare as text.
fn freshness(project: &ProjectShell) -> &str {
    project.updated_at.as_deref().or(project.created_at.as_deref()).unwrap_or_default()
}

fn fresher(candidate: &ProjectShell, existing: &ProjectShell) -> bool {
    (freshness(candidate), candidate.id.as_str()) > (freshness(existing), existing.id.as_str())
}

/// One checkout of a project group (`SidebarProjectGroupMember`).
#[derive(Clone, Debug)]
pub struct Member {
    pub project: ProjectShell,
    pub physical_key: String,
    pub is_scratch: bool,
}

/// A project group (`SidebarProjectSnapshot`).
#[derive(Clone, Debug)]
pub struct ProjectGroup {
    pub key: String,
    pub display_name: String,
    /// Grouped checkouts in first-seen order; the first is the representative.
    pub members: Vec<Member>,
    /// Every project record in the group (`memberProjectRefs`), including entries that share
    /// a folder with a member.
    pub project_ids: Vec<String>,
    /// Clients write every member together; any pinned member pins the group.
    pub pinned: bool,
    pub archived: bool,
}

impl ProjectGroup {
    pub fn representative(&self) -> &ProjectShell {
        &self.members[0].project
    }

    pub fn contains(&self, project_id: &str) -> bool {
        self.project_ids.iter().any(|id| id == project_id)
    }
}

/// An environment's "No project" workspace (`scratchWorkspaceRoot`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScratchRoot {
    pub environment_id: SharedString,
    pub root: String,
}

/// `buildProjectGroups` over every environment's projects, in first-seen order. A folder is
/// one checkout per environment; checkouts on different environments group by repository.
/// `group_scratch` folds every scratch project into one "No project" group, as the sidebar does.
pub fn build_groups(
    projects: &[ProjectShell],
    settings: &GroupingSettings,
    scratch_roots: &[ScratchRoot],
    group_scratch: bool,
) -> Vec<ProjectGroup> {
    let scratch_roots: Vec<(&SharedString, String)> =
        scratch_roots.iter().map(|scratch| (&scratch.environment_id, normalize_path(&scratch.root))).collect();
    // Several entries can share a folder; the freshest one stands for it.
    let mut physical: Vec<(String, Vec<&ProjectShell>)> = Vec::new();
    for project in projects {
        let key = physical_key(&project.environment_id, &project.workspace_root);
        match physical.iter_mut().find(|(existing, _)| *existing == key) {
            Some((_, entries)) => entries.push(project),
            None => physical.push((key, vec![project])),
        }
    }
    let mut groups: Vec<ProjectGroup> = Vec::new();
    let mut index_by_key: HashMap<String, usize> = HashMap::new();
    for (physical_key, entries) in physical {
        let winner = entries.iter().copied().reduce(|best, p| if fresher(p, best) { p } else { best }).unwrap();
        let identity_source = if winner.repository_identity.is_some() {
            winner
        } else {
            entries
                .iter()
                .copied()
                .filter(|p| p.repository_identity.is_some())
                .reduce(|best, p| if fresher(p, best) { p } else { best })
                .unwrap_or(winner)
        };
        let root = normalize_path(&winner.workspace_root);
        let is_scratch = scratch_roots
            .iter()
            .any(|(environment, scratch)| **environment == winner.environment_id && *scratch == root);
        let mode = settings.mode_for(&physical_key);
        let key = logical_key(identity_source, mode, &physical_key, group_scratch && is_scratch);
        let project_ids = entries.iter().map(|p| p.id.clone());
        let member = Member { project: winner.clone(), physical_key, is_scratch };
        match index_by_key.get(&key) {
            Some(&index) => {
                groups[index].members.push(member);
                groups[index].project_ids.extend(project_ids);
            }
            None => {
                index_by_key.insert(key.clone(), groups.len());
                groups.push(ProjectGroup {
                    key,
                    display_name: String::new(),
                    members: vec![member],
                    project_ids: project_ids.collect(),
                    pinned: false,
                    archived: false,
                });
            }
        }
    }
    for group in &mut groups {
        group.display_name = if group_scratch && group.members[0].is_scratch {
            SCRATCH_PROJECT_TITLE.to_owned()
        } else if group.members.len() > 1 {
            group_label(&group.members)
        } else {
            group.members[0].project.title.clone()
        };
        group.pinned = group.members.iter().any(|m| m.project.pinned_at.is_some());
        group.archived = group.members.iter().all(|m| m.project.archived_at.is_some());
    }
    groups
}

fn parse_ms(value: Option<&str>) -> Option<i64> {
    value.and_then(crate::inbox::parse_ms)
}

/// `sortLogicalProjectsForSidebar` over the sidebar's listed threads: newest activity first
/// (a thread's last user message, or its creation for `created_at`), else the project's own
/// time; ties by title, then key. `Manual` keeps the given order.
pub fn sort_groups(groups: &mut Vec<ProjectGroup>, threads: &[ThreadShell], order: SortOrder) {
    if order == SortOrder::Manual {
        return;
    }
    let group_of: HashMap<&str, usize> = groups
        .iter()
        .enumerate()
        .flat_map(|(index, group)| group.project_ids.iter().map(move |id| (id.as_str(), index)))
        .collect();
    let thread_time = |t: &ThreadShell| match order {
        SortOrder::CreatedAt => parse_ms(Some(&t.created_at)).or_else(|| parse_ms(Some(&t.updated_at))),
        _ => parse_ms(t.latest_user_message_at.as_deref())
            .or_else(|| parse_ms(Some(&t.updated_at)))
            .or_else(|| parse_ms(Some(&t.created_at))),
    };
    let mut latest: Vec<Option<i64>> = vec![None; groups.len()];
    let listed = threads.iter().filter(|t| t.is_listed() && t.archived_at.is_none() && t.deleted_at.is_none());
    for thread in listed {
        if let Some(&index) = group_of.get(thread.project_id.as_str()) {
            let time = thread_time(thread).unwrap_or(i64::MIN);
            latest[index] = Some(latest[index].map_or(time, |l| l.max(time)));
        }
    }
    let times: Vec<i64> = groups
        .iter()
        .zip(latest)
        .map(|(group, latest)| {
            let project = group.representative();
            let own = match order {
                SortOrder::CreatedAt => parse_ms(project.created_at.as_deref()),
                _ => parse_ms(project.updated_at.as_deref().or(project.created_at.as_deref())),
            };
            latest.or(own).unwrap_or(i64::MIN)
        })
        .collect();
    let mut keyed: Vec<(i64, ProjectGroup)> = times.into_iter().zip(std::mem::take(groups)).collect();
    keyed.sort_by(|(a_time, a), (b_time, b)| {
        let (a_title, b_title) = (&a.representative().title, &b.representative().title);
        b_time
            .cmp(a_time)
            .then_with(|| a_title.to_lowercase().cmp(&b_title.to_lowercase()))
            .then_with(|| a_title.cmp(b_title))
            .then_with(|| a.key.cmp(&b.key))
    });
    *groups = keyed.into_iter().map(|(_, group)| group).collect();
}

/// `pickableProjectGroups`: archived groups left out, pinned ones first.
pub fn pickable(groups: &[ProjectGroup]) -> Vec<&ProjectGroup> {
    let open = || groups.iter().filter(|g| !g.archived);
    open().filter(|g| g.pinned).chain(open().filter(|g| !g.pinned)).collect()
}

/// `groupSidebarThreadsByProject`: boring sidebar folders in `groups` order, each with its
/// threads in input order. A group without threads keeps a folder only when `keep_empty`
/// says so; a thread in no group has no folder and is left out.
pub fn folders<'g, T>(
    groups: &[&'g ProjectGroup],
    threads: Vec<T>,
    project_of: impl Fn(&T) -> &str,
    keep_empty: impl Fn(&ProjectGroup) -> bool,
) -> Vec<(&'g ProjectGroup, Vec<T>)> {
    let folder_of: HashMap<&str, usize> = groups
        .iter()
        .enumerate()
        .flat_map(|(index, group)| group.project_ids.iter().map(move |id| (id.as_str(), index)))
        .collect();
    let mut folders: Vec<(&ProjectGroup, Vec<T>)> = groups.iter().map(|group| (*group, Vec::new())).collect();
    for thread in threads {
        if let Some(&index) = folder_of.get(project_of(&thread)) {
            folders[index].1.push(thread);
        }
    }
    folders.retain(|(group, threads)| !threads.is_empty() || keep_empty(group));
    folders
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn project(id: &str, root: &str, identity: Value) -> ProjectShell {
        let mut project: ProjectShell = serde_json::from_value(json!({
            "id": id,
            "title": id,
            "workspaceRoot": root,
            "repositoryIdentity": identity,
            "updatedAt": "2026-11-01T00:00:00.000Z",
        }))
        .unwrap();
        project.environment_id = "env".into();
        project
    }

    fn scratch(root: &str) -> [ScratchRoot; 1] {
        [ScratchRoot { environment_id: "env".into(), root: root.into() }]
    }

    #[test]
    fn the_same_folder_on_two_environments_is_two_checkouts_of_one_repository() {
        let local = project("a", "/src/app", repo("k", "/src/app"));
        let mut remote = project("b", "/src/app", repo("k", "/src/app"));
        remote.environment_id = "remote".into();
        let groups = build_groups(&[local, remote], &GroupingSettings::default(), &scratch("/src/app"), false);
        assert_eq!(groups.len(), 1);
        let members: Vec<(&str, bool)> =
            groups[0].members.iter().map(|m| (m.physical_key.as_str(), m.is_scratch)).collect();
        assert_eq!(members, [("env:/src/app", true), ("remote:/src/app", false)], "scratch roots are per environment");
    }

    fn repo(key: &str, root: &str) -> Value {
        json!({"canonicalKey": key, "rootPath": root, "name": "t3code", "displayName": "pingdotgg/t3code"})
    }

    fn thread(id: &str, project_id: &str, created: &str, message: Option<&str>) -> ThreadShell {
        serde_json::from_value(json!({
            "id": id, "projectId": project_id, "title": id, "status": "idle", "lineage": {},
            "updatedAt": created, "createdAt": created, "latestUserMessageAt": message,
        }))
        .unwrap()
    }

    fn keys(groups: &[ProjectGroup]) -> Vec<&str> {
        groups.iter().map(|g| g.key.as_str()).collect()
    }

    #[test]
    fn paths_normalize_like_the_web() {
        assert_eq!(normalize_path("C:/Dev/T3/"), "c:\\dev\\t3");
        assert_eq!(normalize_path("/home/me/app//"), "/home/me/app");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(physical_key("env", "H:\\Dev\\App"), "env:h:\\dev\\app");
    }

    #[test]
    fn grouping_follows_the_mode_and_per_checkout_overrides() {
        let projects = vec![
            project("main", "/src/t3code", repo("github.com/pingdotgg/t3code", "/src/t3code")),
            project("web", "/src/t3code/apps/web", repo("github.com/pingdotgg/t3code", "/src/t3code")),
            project("solo", "/src/solo", Value::Null),
        ];
        let mut settings = GroupingSettings::default();
        let groups = build_groups(&projects, &settings, &[], false);
        let counts: Vec<_> = groups.iter().map(|g| (g.key.as_str(), g.members.len())).collect();
        assert_eq!(counts, [("github.com/pingdotgg/t3code", 2), ("env:/src/solo", 1)]);
        assert_eq!(groups[0].project_ids, ["main", "web"]);
        // Two titles that match no repository name: the shared repository name labels the group.
        assert_eq!(groups[0].display_name, "pingdotgg/t3code");

        settings.mode = GroupingMode::RepositoryPath;
        let groups = build_groups(&projects, &settings, &[], false);
        assert_eq!(
            keys(&groups),
            ["github.com/pingdotgg/t3code", "github.com/pingdotgg/t3code::apps/web", "env:/src/solo"]
        );

        settings.mode = GroupingMode::Repository;
        settings.set_override("env:/src/t3code/apps/web", Some(GroupingMode::Separate));
        let groups = build_groups(&projects, &settings, &[], false);
        assert!(groups.iter().any(|g| g.key == "env:/src/t3code/apps/web"));
        assert_eq!(
            settings.overrides_patch()["sidebarProjectGroupingOverrides"],
            json!({"env:/src/t3code/apps/web": "separate"})
        );
        settings.set_override("env:/src/t3code/apps/web", None);
        assert!(settings.overrides.is_empty(), "inherit drops the override");
    }

    #[test]
    fn one_folder_keeps_its_freshest_entry_and_groups_pin_and_archive_together() {
        let mut older = project("a", "/src/app", Value::Null);
        older.updated_at = Some("2026-01-01T00:00:00.000Z".into());
        let mut newer = project("b", "/src/app/", Value::Null);
        newer.pinned_at = Some("2026-11-02T00:00:00.000Z".into());
        newer.archived_at = Some("2026-11-02T00:00:00.000Z".into());
        let groups = build_groups(&[older, newer], &GroupingSettings::default(), &scratch("/src/app"), false);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.len(), 1);
        assert_eq!(groups[0].project_ids, ["a", "b"], "both entries' threads belong to the group");
        assert_eq!(groups[0].representative().id, "b");
        assert!(groups[0].pinned && groups[0].archived && groups[0].members[0].is_scratch);
    }

    #[test]
    fn the_sidebar_folds_scratch_projects_into_no_project() {
        let projects =
            [project("scratch", "/home/me/.t3/scratch", Value::Null), project("app", "/src/app", Value::Null)];
        let groups = build_groups(&projects, &GroupingSettings::default(), &scratch("/home/me/.t3/scratch"), true);
        assert_eq!(keys(&groups), [SCRATCH_PROJECT_SCOPE_KEY, "env:/src/app"]);
        assert_eq!(groups[0].display_name, SCRATCH_PROJECT_TITLE);
        let unfolded = build_groups(&projects, &GroupingSettings::default(), &scratch("/home/me/.t3/scratch"), false);
        assert_eq!(unfolded[0].display_name, "scratch");
    }

    #[test]
    fn group_labels_prefer_a_shared_custom_title() {
        let mut a = project("a", "/x/one", repo("k", "/x/one"));
        let mut b = project("b", "/y/two", repo("k", "/y/two"));
        a.title = "Rex".into();
        b.title = "Rex".into();
        let groups = build_groups(&[a, b], &GroupingSettings::default(), &[], false);
        assert_eq!(groups[0].display_name, "Rex");
    }

    #[test]
    fn grouping_settings_read_the_client_settings_shape() {
        let settings = GroupingSettings::from_json(&json!({
            "sidebarProjectGroupingMode": "separate",
            "sidebarProjectGroupingOverrides": {"env:/a": "repository", "env:/b": "bogus"},
        }));
        assert_eq!(settings.mode, GroupingMode::Separate);
        assert_eq!(settings.overrides.len(), 1);
        assert_eq!(GroupingSettings::from_json(&Value::Null), GroupingSettings::default());
    }

    #[test]
    fn groups_sort_by_their_newest_thread_then_their_own_time() {
        let mut quiet = project("quiet", "/q", Value::Null);
        quiet.created_at = Some("2026-10-01T00:00:00.000Z".into());
        quiet.updated_at = Some("2026-10-20T00:00:00.000Z".into());
        let mut busy = project("busy", "/b", Value::Null);
        busy.created_at = Some("2026-09-01T00:00:00.000Z".into());
        let mut early = project("early", "/e", Value::Null);
        early.created_at = Some("2026-01-01T00:00:00.000Z".into());
        let projects = [early, quiet, busy];
        let threads = [
            // Created long ago, but messaged most recently.
            thread("t1", "early", "2026-02-01T00:00:00.000Z", Some("2026-11-04T00:00:00.000Z")),
            thread("t2", "busy", "2026-10-25T00:00:00.000Z", None),
        ];
        let base = build_groups(&projects, &GroupingSettings::default(), &[], true);
        let sorted = |order| {
            let mut groups = base.clone();
            sort_groups(&mut groups, &threads, order);
            keys(&groups).into_iter().map(str::to_owned).collect::<Vec<_>>()
        };
        assert_eq!(sorted(SortOrder::UpdatedAt), ["env:/e", "env:/b", "env:/q"]);
        assert_eq!(sorted(SortOrder::CreatedAt), ["env:/b", "env:/q", "env:/e"]);
        assert_eq!(sorted(SortOrder::Manual), ["env:/e", "env:/q", "env:/b"], "manual keeps the server's order");
    }

    #[test]
    fn folders_follow_group_order_and_keep_empty_ones_on_request() {
        let projects =
            [project("a", "/a", Value::Null), project("b", "/b", Value::Null), project("c", "/c", Value::Null)];
        let groups = build_groups(&projects, &GroupingSettings::default(), &[], true);
        let order: Vec<&ProjectGroup> = vec![&groups[2], &groups[0], &groups[1]];
        let threads = vec![("t1", "a"), ("t2", "x"), ("t3", "c"), ("t4", "a")];
        let folders = folders(&order, threads, |t| t.1, |g| g.key == "env:/b");
        let shape: Vec<(&str, Vec<&str>)> =
            folders.iter().map(|(g, threads)| (g.key.as_str(), threads.iter().map(|t| t.0).collect())).collect();
        assert_eq!(shape, [("env:/c", vec!["t3"]), ("env:/a", vec!["t1", "t4"]), ("env:/b", vec![])]);
    }

    #[test]
    fn pickable_groups_drop_archived_and_lead_with_pinned() {
        let mut pinned = project("p", "/p", Value::Null);
        pinned.pinned_at = Some("2026-11-01T00:00:00.000Z".into());
        let mut archived = project("x", "/x", Value::Null);
        archived.archived_at = Some("2026-11-01T00:00:00.000Z".into());
        let groups =
            build_groups(&[project("a", "/a", Value::Null), archived, pinned], &GroupingSettings::default(), &[], true);
        let picked: Vec<&str> = pickable(&groups).iter().map(|g| g.key.as_str()).collect();
        assert_eq!(picked, ["env:/p", "env:/a"]);
    }
}
