//! Project groups as Settings shows them (`useSettingsProjectGroups`) and the panel's pure
//! rules (`ProjectSettingsPanel.logic.ts`, `planProjectRemoval`). The grouping itself is
//! `crate::project_groups`.

use serde_json::{Value, json};

use crate::model::{ProjectShell, ThreadShell};
pub(super) use crate::project_groups::{
    GroupingMode, GroupingSettings, Member, ProjectGroup, SCRATCH_PROJECT_TITLE, ScratchRoot,
};

const SCRATCH_ICON_NAME: &str = "message-square-dashed";
const SCRATCH_ICON_COLOR: &str = "gray";

/// `useSettingsProjectGroups`: grouped projects, the scratch project alone, sorted by name.
pub(super) fn build_groups(
    projects: &[ProjectShell],
    settings: &GroupingSettings,
    scratch_roots: &[ScratchRoot],
) -> Vec<ProjectGroup> {
    let mut groups = crate::project_groups::build_groups(projects, settings, scratch_roots, false);
    groups.sort_by(|a, b| {
        a.display_name
            .to_lowercase()
            .cmp(&b.display_name.to_lowercase())
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    groups
}

/// A picked project as the panel edits it (`scopedGroup`): the group narrowed to the picked
/// machine's checkouts, and every checkout, which pin and archive still write.
pub(super) struct ScopedGroup {
    pub(super) group: ProjectGroup,
    pub(super) whole_members: Vec<Member>,
    /// Checkouts on other machines are left out (`hasOtherMembers`).
    pub(super) has_other_members: bool,
}

/// `ProjectSettingsPanel`'s member filter: only `machine`'s checkouts, when one is picked.
pub(super) fn narrow(group: &ProjectGroup, machine: Option<&str>) -> ScopedGroup {
    let mut scoped = group.clone();
    scoped.members.retain(|m| machine.is_none_or(|id| m.project.environment_id == id));
    let has_other_members = scoped.members.len() < group.members.len();
    ScopedGroup { group: scoped, whole_members: group.members.clone(), has_other_members }
}

/// The group a former selection's checkouts joined after a grouping rule moved them.
pub(super) fn successor<'a>(groups: &'a [ProjectGroup], member_keys: &[String]) -> Option<&'a ProjectGroup> {
    groups.iter().find(|group| group.members.iter().any(|m| member_keys.contains(&m.physical_key)))
}

/// `projectGroupTitleNeedsUpdate`.
pub(super) fn title_needs_update(member_titles: &[&str], next: &str, was_edited: bool) -> bool {
    was_edited && member_titles.iter().any(|title| *title != next)
}

/// `defaultProjectIcon`, as the update fields it writes.
pub(super) fn default_icon_update(is_scratch: bool) -> Value {
    let icon = if is_scratch {
        json!({"kind": "lucide", "name": SCRATCH_ICON_NAME, "color": SCRATCH_ICON_COLOR})
    } else {
        Value::Null
    };
    json!({"faviconPath": null, "projectIcon": icon})
}

/// `hasDefaultProjectIcon`.
pub(super) fn has_default_icon(member: &Member) -> bool {
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

/// `planProjectRemoval`: whether the whole group goes, and the confirmation to show. `unloaded`
/// counts the members' threads the shell left out (older settled ones).
pub(super) fn plan_removal(
    members: &[&Member],
    group_member_count: usize,
    has_other_members: bool,
    display_name: &str,
    environment_label: Option<&str>,
    threads: &[ThreadShell],
    unloaded: usize,
) -> (bool, String) {
    let thread_count = threads
        .iter()
        .filter(|thread| thread.is_listed() && members.iter().any(|m| m.project.id == thread.project_id))
        .count()
        + unloaded;
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

    #[test]
    fn a_picked_machine_narrows_the_group_to_its_checkouts() {
        let member = |id: &str, environment: &str| {
            let mut project = project(id, &format!("/{id}"), Value::Null);
            project.environment_id = environment.into();
            Member { project, physical_key: id.into(), is_scratch: false }
        };
        let group = ProjectGroup {
            key: "repo".into(),
            display_name: "repo".into(),
            members: vec![member("a", "laptop"), member("b", "server"), member("c", "laptop")],
            project_ids: vec!["a".into(), "b".into(), "c".into()],
            project_refs: vec![
                ("laptop".into(), "a".into()),
                ("server".into(), "b".into()),
                ("laptop".into(), "c".into()),
            ],
            pinned: false,
            archived: false,
        };
        let ids = |members: &[Member]| members.iter().map(|m| m.project.id.clone()).collect::<Vec<_>>();
        let scoped = narrow(&group, Some("laptop"));
        assert_eq!(ids(&scoped.group.members), ["a", "c"]);
        assert_eq!(ids(&scoped.whole_members), ["a", "b", "c"]);
        assert!(scoped.has_other_members);
        let all = narrow(&group, None);
        assert_eq!(ids(&all.group.members), ["a", "b", "c"]);
        assert!(!all.has_other_members);
        assert!(narrow(&group, Some("gone")).group.members.is_empty());
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
        let (whole, message) = plan_removal(&[&member], 1, false, "api", Some("box"), &[thread], 0);
        assert!(whole);
        assert_eq!(
            message,
            "Remove project \"api\" and delete its 1 thread?\nPath: /src/api\nEnvironment: box\n\
             Those threads and any archived threads are deleted from T3 Code. Provider sessions are kept.\n\
             This removes only the project entries, not the files on disk.\nThis action cannot be undone."
        );
        let (whole, message) = plan_removal(&[&member], 2, false, "group", None, &[], 0);
        assert!(!whole);
        assert!(message.starts_with("Remove checkout \"api\"?\nPath: /src/api\nAny archived"));
        // Older settled threads the shell left out count too.
        let (_, message) = plan_removal(&[&member], 2, false, "group", None, &[], 2);
        assert!(
            message.starts_with("Remove checkout \"api\" and delete its 2 threads?\nPath: /src/api\nThose threads")
        );
        assert!(message.contains("Other entries in this grouped project are unaffected."));
    }
}
