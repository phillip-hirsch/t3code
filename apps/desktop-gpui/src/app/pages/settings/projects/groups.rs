//! Project groups as Settings shows them (`buildProjectGroups` in
//! `packages/client-runtime/src/state/projectGrouping.ts`, `useSettingsProjectGroups`),
//! the grouping rule they use, and the panel's pure rules (`ProjectSettingsPanel.logic.ts`,
//! `planProjectRemoval`). This client talks to one environment, so every member shares it.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Map, Value, json};

use crate::model::{ProjectShell, RepositoryIdentity, ThreadShell};

pub(in crate::app) const SCRATCH_PROJECT_TITLE: &str = "No project";
const SCRATCH_ICON_NAME: &str = "message-square-dashed";
const SCRATCH_ICON_COLOR: &str = "gray";

/// `SidebarProjectGroupingMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum GroupingMode {
    Repository,
    RepositoryPath,
    Separate,
}

impl GroupingMode {
    pub(in crate::app) const ALL: [Self; 3] = [Self::Repository, Self::RepositoryPath, Self::Separate];

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
    pub(in crate::app) fn label(self) -> &'static str {
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
pub(in crate::app) struct GroupingSettings {
    pub mode: GroupingMode,
    pub overrides: BTreeMap<String, GroupingMode>,
}

impl Default for GroupingSettings {
    fn default() -> Self {
        Self { mode: GroupingMode::Repository, overrides: BTreeMap::new() }
    }
}

impl GroupingSettings {
    pub(in crate::app) fn from_json(settings: &Value) -> Self {
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
    pub(in crate::app) fn set_override(&mut self, physical_key: &str, mode: Option<GroupingMode>) {
        match mode {
            Some(mode) => self.overrides.insert(physical_key.to_owned(), mode),
            None => self.overrides.remove(physical_key),
        };
    }

    /// The `sidebarProjectGroupingOverrides` value these settings store.
    pub(in crate::app) fn overrides_json(&self) -> Value {
        self.overrides.iter().map(|(key, mode)| (key.clone(), Value::from(mode.as_str()))).collect::<Map<_, _>>().into()
    }
}

/// `normalizeProjectPathForComparison`: no trailing separators; Windows paths compare
/// case-insensitively with backslashes.
pub(in crate::app) fn normalize_path(path: &str) -> String {
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
pub(in crate::app) fn physical_key(environment_id: &str, workspace_root: &str) -> String {
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

/// `deriveLogicalProjectKey`, without scratch grouping (Settings lists the scratch project alone).
fn logical_key(project: &ProjectShell, mode: GroupingMode, physical: &str) -> String {
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
pub(in crate::app) struct Member {
    pub project: ProjectShell,
    pub physical_key: String,
    pub is_scratch: bool,
}

/// A project as Settings lists it (`SidebarProjectSnapshot`).
#[derive(Clone, Debug)]
pub(in crate::app) struct ProjectGroup {
    pub key: String,
    pub display_name: String,
    /// Grouped checkouts in first-seen order; the first is the representative.
    pub members: Vec<Member>,
    /// Clients write every member together; any pinned member pins the group.
    pub pinned: bool,
    pub archived: bool,
}

impl ProjectGroup {
    pub(in crate::app) fn representative(&self) -> &ProjectShell {
        &self.members[0].project
    }
}

/// `useSettingsProjectGroups`: grouped projects, sorted by name.
pub(in crate::app) fn build_groups(
    projects: &[ProjectShell],
    settings: &GroupingSettings,
    environment_id: &str,
    scratch_root: Option<&str>,
) -> Vec<ProjectGroup> {
    let scratch_root = scratch_root.map(normalize_path);
    // Several entries can share a folder; the freshest one stands for it.
    let mut physical: Vec<(String, Vec<&ProjectShell>)> = Vec::new();
    for project in projects {
        let key = physical_key(environment_id, &project.workspace_root);
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
        let key = logical_key(identity_source, settings.mode_for(&physical_key), &physical_key);
        let is_scratch = scratch_root.as_deref() == Some(normalize_path(&winner.workspace_root).as_str());
        let member = Member { project: winner.clone(), physical_key, is_scratch };
        match index_by_key.get(&key) {
            Some(&index) => groups[index].members.push(member),
            None => {
                index_by_key.insert(key.clone(), groups.len());
                groups.push(ProjectGroup {
                    key,
                    display_name: String::new(),
                    members: vec![member],
                    pinned: false,
                    archived: false,
                });
            }
        }
    }
    for group in &mut groups {
        group.display_name =
            if group.members.len() > 1 { group_label(&group.members) } else { group.members[0].project.title.clone() };
        group.pinned = group.members.iter().any(|m| m.project.pinned_at.is_some());
        group.archived = group.members.iter().all(|m| m.project.archived_at.is_some());
    }
    groups.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    groups
}

/// The group a former selection's checkouts joined after a grouping rule moved them.
pub(in crate::app) fn successor<'a>(groups: &'a [ProjectGroup], member_keys: &[String]) -> Option<&'a ProjectGroup> {
    groups.iter().find(|group| group.members.iter().any(|m| member_keys.contains(&m.physical_key)))
}

/// `projectGroupTitleNeedsUpdate`.
pub(in crate::app) fn title_needs_update(member_titles: &[&str], next: &str, was_edited: bool) -> bool {
    was_edited && member_titles.iter().any(|title| *title != next)
}

/// `defaultProjectIcon`, as the update fields it writes.
pub(in crate::app) fn default_icon_update(is_scratch: bool) -> Value {
    let icon = if is_scratch {
        json!({"kind": "lucide", "name": SCRATCH_ICON_NAME, "color": SCRATCH_ICON_COLOR})
    } else {
        Value::Null
    };
    json!({"faviconPath": null, "projectIcon": icon})
}

/// `hasDefaultProjectIcon`.
pub(in crate::app) fn has_default_icon(member: &Member) -> bool {
    if member.project.favicon_path.is_some() {
        return false;
    }
    let icon = member.project.project_icon.as_ref().filter(|icon| !icon.is_null());
    match (member.is_scratch, icon) {
        (false, icon) => icon.is_none(),
        (true, Some(icon)) => {
            icon["kind"] == "lucide" && icon["name"] == SCRATCH_ICON_NAME && icon["color"] == SCRATCH_ICON_COLOR
        }
        (true, None) => false,
    }
}

/// `planProjectRemoval`: whether the whole group goes, and the confirmation to show.
pub(in crate::app) fn plan_removal(
    members: &[&Member],
    group_member_count: usize,
    has_other_members: bool,
    display_name: &str,
    environment_label: Option<&str>,
    threads: &[ThreadShell],
) -> (bool, String) {
    let thread_count = threads
        .iter()
        .filter(|thread| thread.is_listed() && members.iter().any(|m| m.project.id == thread.project_id))
        .count();
    let whole_group = members.len() == group_member_count;
    let kind = if has_other_members || !whole_group { "checkout" } else { "project" };
    let single = if let [member] = members { Some(*member) } else { None };
    let label = single.map_or(display_name, |member| &member.project.title);
    let mut lines = vec![match thread_count {
        0 => format!("Remove {kind} \"{label}\"?"),
        1 => format!("Remove {kind} \"{label}\" and delete its 1 thread?"),
        count => format!("Remove {kind} \"{label}\" and delete its {count} threads?"),
    }];
    match single {
        Some(member) => {
            lines.push(format!("Path: {}", member.project.workspace_root));
            lines.extend(environment_label.map(|label| format!("Environment: {label}")));
        }
        None => lines.push(format!("This removes {} grouped project entries.", members.len())),
    }
    lines.push(
        if thread_count > 0 {
            "Those threads and any archived threads are deleted from T3 Code. Provider sessions are kept."
        } else {
            "Any archived threads are deleted from T3 Code. Provider sessions are kept."
        }
        .into(),
    );
    lines.push(
        if whole_group && !has_other_members {
            "This removes only the project entries, not the files on disk."
        } else {
            "Other entries in this grouped project are unaffected."
        }
        .into(),
    );
    lines.push("This action cannot be undone.".into());
    (whole_group, lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(id: &str, root: &str, identity: Value) -> ProjectShell {
        serde_json::from_value(json!({
            "id": id,
            "title": id,
            "workspaceRoot": root,
            "repositoryIdentity": identity,
            "updatedAt": "2026-11-01T00:00:00.000Z",
        }))
        .unwrap()
    }

    fn repo(key: &str, root: &str) -> Value {
        json!({"canonicalKey": key, "rootPath": root, "name": "t3code", "displayName": "pingdotgg/t3code"})
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
        let groups = build_groups(&projects, &settings, "env", None);
        let keys: Vec<_> = groups.iter().map(|g| (g.key.as_str(), g.members.len())).collect();
        assert_eq!(keys, [("github.com/pingdotgg/t3code", 2), ("env:/src/solo", 1)]);
        // Two titles that match no repository name: the shared repository name labels the group.
        assert_eq!(groups[0].display_name, "pingdotgg/t3code");

        settings.mode = GroupingMode::RepositoryPath;
        let groups = build_groups(&projects, &settings, "env", None);
        let mut keys: Vec<_> = groups.iter().map(|g| g.key.as_str()).collect();
        keys.sort();
        assert_eq!(keys, ["env:/src/solo", "github.com/pingdotgg/t3code", "github.com/pingdotgg/t3code::apps/web"]);

        settings.mode = GroupingMode::Repository;
        settings.set_override("env:/src/t3code/apps/web", Some(GroupingMode::Separate));
        let groups = build_groups(&projects, &settings, "env", None);
        assert!(groups.iter().any(|g| g.key == "env:/src/t3code/apps/web"));
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
        let groups = build_groups(&[older, newer], &GroupingSettings::default(), "env", Some("/src/app"));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.len(), 1);
        assert_eq!(groups[0].representative().id, "b");
        assert!(groups[0].pinned && groups[0].archived && groups[0].members[0].is_scratch);
    }

    #[test]
    fn group_labels_prefer_a_shared_custom_title() {
        let mut a = project("a", "/x/one", repo("k", "/x/one"));
        let mut b = project("b", "/y/two", repo("k", "/y/two"));
        a.title = "Rex".into();
        b.title = "Rex".into();
        let groups = build_groups(&[a, b], &GroupingSettings::default(), "env", None);
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
        assert_eq!(settings.overrides_json(), json!({"env:/a": "repository"}));
    }

    #[test]
    fn titles_update_only_after_an_edit_that_changes_one() {
        assert!(!title_needs_update(&["Rex"], "Rex", true));
        assert!(!title_needs_update(&["Rex"], "Other", false));
        assert!(title_needs_update(&["Rex", "rex"], "Rex", true));
    }

    #[test]
    fn default_icons_depend_on_the_scratch_project() {
        let mut member =
            Member { project: project("a", "/a", Value::Null), physical_key: "k".into(), is_scratch: false };
        assert!(has_default_icon(&member));
        member.project.project_icon = Some(json!({"kind": "emoji", "emoji": "🚀"}));
        assert!(!has_default_icon(&member));
        member.is_scratch = true;
        member.project.project_icon = Some(default_icon_update(true)["projectIcon"].clone());
        assert!(has_default_icon(&member));
        member.project.favicon_path = Some("logo.png".into());
        assert!(!has_default_icon(&member));
    }

    #[test]
    fn removal_confirmations_match_the_web() {
        let member =
            Member { project: project("api", "/src/api", Value::Null), physical_key: "k".into(), is_scratch: false };
        let thread: ThreadShell = serde_json::from_value(json!({
            "id": "t", "projectId": "api", "title": "t", "status": "idle", "lineage": {},
            "updatedAt": "2026-11-01T00:00:00.000Z", "createdAt": "2026-11-01T00:00:00.000Z",
        }))
        .unwrap();
        let (whole, message) = plan_removal(&[&member], 1, false, "api", Some("box"), &[thread]);
        assert!(whole);
        assert_eq!(
            message,
            "Remove project \"api\" and delete its 1 thread?\nPath: /src/api\nEnvironment: box\n\
             Those threads and any archived threads are deleted from T3 Code. Provider sessions are kept.\n\
             This removes only the project entries, not the files on disk.\nThis action cannot be undone."
        );
        let (whole, message) = plan_removal(&[&member], 2, false, "group", None, &[]);
        assert!(!whole);
        assert!(message.starts_with("Remove checkout \"api\"?\nPath: /src/api\nAny archived"));
        assert!(message.contains("Other entries in this grouped project are unaffected."));
    }
}
