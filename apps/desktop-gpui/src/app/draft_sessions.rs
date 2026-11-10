//! Independent draft sessions (`composerDraftStore`'s `draftThreadsByThreadKey` and
//! `logicalProjectDraftThreadKeyByLogicalProjectKey`): every unsent draft has its own id, so a
//! project can hold several, and a draft keeps its identity when it moves to another project.

use std::collections::HashMap;
use std::fmt;

use gpui_kit::SharedString;
use serde::{Deserialize, Serialize};

use super::Target;
use crate::model::ScopedId;
use crate::new_thread::WorkspaceChoice;

/// One draft's identity (`DraftId`), unique across runs like the web's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DraftId(SharedString);

impl DraftId {
    fn new() -> Self {
        Self(super::new_id().into())
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    /// A draft saved by an earlier run.
    pub(super) fn from_saved(id: String) -> Self {
        Self(id.into())
    }

    #[cfg(test)]
    pub(super) fn for_test(id: &str) -> Self {
        Self(SharedString::from(id.to_owned()))
    }
}

impl fmt::Display for DraftId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

struct Session {
    project: ScopedId,
    /// When it was made, in milliseconds and strictly increasing (`createdAt`): the sidebar
    /// lists drafts newest first, across restarts too.
    created: i64,
    /// It launched its thread (`promotedTo`), so no project opens it again.
    promoted: bool,
    /// The id its thread launches with (`DraftSessionState.threadId`), assigned once by its first
    /// terminal or its send, so both share it; it goes when the draft launches or goes.
    thread_id: Option<String>,
    /// `branch`, `worktreePath`, `envMode` and `startFromOrigin`, for `project`.
    workspace: WorkspaceChoice,
}

/// `createDraftThreadState` on another project: the branch and worktree were the old one's.
fn moved(choice: WorkspaceChoice) -> WorkspaceChoice {
    WorkspaceChoice { branch: None, existing_worktree: None, ..choice }
}

#[derive(Default)]
pub(super) struct DraftSessions {
    sessions: HashMap<DraftId, Session>,
    last_created: i64,
    /// The draft "New thread" opens in each project; a draft with content that lost its place
    /// stays a sidebar row.
    by_project: HashMap<ScopedId, DraftId>,
}

impl DraftSessions {
    /// A new draft in `project`, not yet its project's draft.
    pub(super) fn create(&mut self, project: ScopedId) -> DraftId {
        let id = DraftId::new();
        self.last_created = crate::timestamp::now_ms().max(self.last_created + 1);
        let session = Session {
            project,
            created: self.last_created,
            promoted: false,
            thread_id: None,
            workspace: Default::default(),
        };
        self.sessions.insert(id.clone(), session);
        id
    }

    /// The id `draft`'s thread launches with, once assigned.
    pub(super) fn thread_id(&self, draft: &DraftId) -> Option<&str> {
        self.sessions.get(draft).filter(|session| !session.promoted)?.thread_id.as_deref()
    }

    /// The id `draft`'s thread launches with, assigned on first use, and whether this call
    /// assigned it. `None` for a draft that launched or is gone.
    pub(super) fn reserve_thread(&mut self, draft: &DraftId) -> Option<(String, bool)> {
        let session = self.sessions.get_mut(draft).filter(|session| !session.promoted)?;
        let assigned = session.thread_id.is_none();
        let id = session.thread_id.get_or_insert_with(super::new_id).clone();
        Some((id, assigned))
    }

    /// The unsent draft on `env` whose thread launches with `thread_id`; `None` for `env` is a
    /// legacy key that names no environment, matched by the thread id alone.
    pub(super) fn launching_with(&self, env: Option<&str>, thread_id: &str) -> Option<&DraftId> {
        let mut live = self.sessions.iter().filter(|(_, session)| !session.promoted);
        live.find(|(_, session)| {
            session.thread_id.as_deref() == Some(thread_id) && env.is_none_or(|env| session.project.env == env)
        })
        .map(|(draft, _)| draft)
    }

    /// The workspace picks `draft` made for its project.
    pub(super) fn workspace(&self, draft: &DraftId) -> WorkspaceChoice {
        self.sessions.get(draft).map(|session| session.workspace.clone()).unwrap_or_default()
    }

    /// Keeps the picks `draft` made while it was in `project`.
    pub(super) fn set_workspace(&mut self, draft: &DraftId, project: &ScopedId, choice: WorkspaceChoice) {
        let Some(session) = self.sessions.get_mut(draft) else { return };
        session.workspace = if session.project == *project { choice } else { moved(choice) };
    }

    /// `project`'s draft, unless it launched its thread.
    pub(super) fn of_project(&self, project: &ScopedId) -> Option<&DraftId> {
        self.by_project.get(project).filter(|id| self.is_live(id))
    }

    /// The empty draft "New thread" reopens in `project` (`createNewThreadHandler`): the project's
    /// draft, else the `open` draft when it is one of the project's.
    pub(super) fn reusable(
        &self,
        project: &ScopedId,
        open: Option<&DraftId>,
        empty: impl Fn(&DraftId) -> bool,
    ) -> Option<DraftId> {
        let open = open.filter(|draft| {
            self.sessions.get(draft).is_some_and(|session| !session.promoted && session.project == *project)
        });
        self.of_project(project).filter(|draft| empty(draft)).or(open.filter(|draft| empty(draft))).cloned()
    }

    /// `setLogicalProjectDraftThreadId`: `draft` moves to `project` and becomes its draft, leaving
    /// any other project it was the draft of. The project's previous draft keeps its own identity.
    pub(super) fn assign(&mut self, project: ScopedId, draft: &DraftId) {
        self.by_project.retain(|key, mapped| mapped != draft || *key == project);
        if let Some(session) = self.sessions.get_mut(draft)
            && session.project != project
        {
            session.project = project.clone();
            session.workspace = moved(std::mem::take(&mut session.workspace));
        }
        self.by_project.insert(project, draft.clone());
    }

    /// The draft still opens as itself: it exists and has not launched its thread.
    pub(super) fn is_live(&self, draft: &DraftId) -> bool {
        self.sessions.get(draft).is_some_and(|session| !session.promoted)
    }

    /// The draft as a target, on its project's environment.
    pub(super) fn target(&self, draft: &DraftId) -> Option<Target> {
        let project = &self.sessions.get(draft)?.project;
        Some(Target::draft(project.env.clone(), project.id.clone(), draft.clone()))
    }

    /// Where `draft` sorts among the sidebar's rows: newer drafts sort first.
    pub(super) fn created(&self, draft: &DraftId) -> i64 {
        self.sessions.get(draft).map_or(0, |session| session.created)
    }

    /// The draft launched its thread: its project's next draft starts fresh.
    pub(super) fn promote(&mut self, draft: &DraftId) {
        if let Some(session) = self.sessions.get_mut(draft) {
            session.promoted = true;
            session.thread_id = None;
        }
        self.by_project.retain(|_, mapped| mapped != draft);
    }

    /// A removed project's drafts go with it.
    pub(super) fn forget_project(&mut self, project: &ScopedId) {
        self.sessions.retain(|_, session| session.project != *project);
        self.by_project.remove(project);
    }

    /// A draft whose project is gone.
    pub(super) fn forget(&mut self, draft: &DraftId) {
        self.sessions.remove(draft);
        self.by_project.retain(|_, mapped| mapped != draft);
    }

    /// The drafts worth keeping across a restart (`partializeComposerDraftStoreState`): unsent, and
    /// either a project's draft or one `with_content`, oldest first.
    pub(super) fn saved(&self, with_content: impl Fn(&DraftId) -> bool) -> Vec<SavedSession> {
        let mapped = |id: &DraftId| self.by_project.values().any(|mapped| mapped == id);
        let mut saved: Vec<SavedSession> = (self.sessions.iter())
            .filter(|(id, session)| !session.promoted && (mapped(id) || with_content(id)))
            .map(|(id, session)| SavedSession {
                draft_id: id.to_string(),
                environment_id: session.project.env.to_string(),
                project_id: session.project.id.clone(),
                created_at: session.created,
                project_draft: mapped(id),
                thread_id: session.thread_id.clone(),
                workspace: session.workspace.clone(),
            })
            .collect();
        saved.sort_by_key(|session| session.created_at);
        saved
    }

    /// Brings back drafts an earlier run saved, beside any made since; a project keeps the draft it
    /// opens now. Hands back the drafts it added.
    pub(super) fn restore(&mut self, saved: Vec<SavedSession>) -> Vec<DraftId> {
        let mut restored = Vec::new();
        for saved in saved {
            let id = DraftId::from_saved(saved.draft_id);
            if self.sessions.contains_key(&id) {
                continue;
            }
            let project = ScopedId::new(saved.environment_id, saved.project_id);
            if saved.project_draft && self.of_project(&project).is_none() {
                self.by_project.insert(project.clone(), id.clone());
            }
            self.last_created = self.last_created.max(saved.created_at);
            let session = Session {
                project,
                created: saved.created_at,
                promoted: false,
                thread_id: saved.thread_id,
                workspace: saved.workspace,
            };
            self.sessions.insert(id.clone(), session);
            restored.push(id);
        }
        restored
    }
}

/// A draft's identity on disk (`draftThreadsByThreadKey` and its project mapping).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SavedSession {
    draft_id: String,
    environment_id: String,
    project_id: String,
    created_at: i64,
    /// The draft "New thread" opens in its project.
    #[serde(default)]
    project_draft: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    #[serde(default)]
    workspace: WorkspaceChoice,
}

impl SavedSession {
    pub(super) fn draft_id(&self) -> &str {
        &self.draft_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::new_thread::EnvMode;

    fn project(id: &str) -> ScopedId {
        ScopedId::new("local", id)
    }

    #[test]
    fn a_project_holds_several_drafts_but_opens_one() {
        let mut drafts = DraftSessions::default();
        let first = drafts.create(project("web"));
        drafts.assign(project("web"), &first);
        let second = drafts.create(project("web"));
        drafts.assign(project("web"), &second);
        assert_ne!(first, second);
        assert_eq!(drafts.of_project(&project("web")), Some(&second));
        assert!(drafts.is_live(&first), "the displaced draft keeps its identity");
        assert_eq!(drafts.target(&first), Some(Target::draft("local".into(), "web".into(), first.clone())));
        assert!(drafts.created(&second) > drafts.created(&first));
    }

    #[test]
    fn new_thread_reopens_only_an_empty_draft_of_its_project() {
        let mut drafts = DraftSessions::default();
        let mapped = drafts.create(project("web"));
        drafts.assign(project("web"), &mapped);
        let other = drafts.create(project("api"));
        drafts.assign(project("api"), &other);
        let web = project("web");
        assert_eq!(drafts.reusable(&web, None, |_| true), Some(mapped.clone()), "the project's empty draft");
        assert_eq!(drafts.reusable(&web, None, |_| false), None, "one with content stays a row");
        assert_eq!(drafts.reusable(&web, Some(&other), |d| *d != mapped), None, "another project's draft");
        // A populated draft lost its place; the open, empty draft of the project stays open.
        let unmapped = drafts.create(project("web"));
        assert_eq!(drafts.reusable(&web, Some(&unmapped), |d| *d == unmapped), Some(unmapped.clone()));
        drafts.promote(&unmapped);
        assert_eq!(drafts.reusable(&web, Some(&unmapped), |d| *d == unmapped), None, "a launched draft");
    }

    #[test]
    fn a_moved_draft_keeps_its_id_and_leaves_its_old_project() {
        let mut drafts = DraftSessions::default();
        let moving = drafts.create(project("web"));
        drafts.assign(project("web"), &moving);
        let resident = drafts.create(project("api"));
        drafts.assign(project("api"), &resident);
        let created = drafts.created(&moving);
        drafts.assign(project("api"), &moving);
        assert_eq!(drafts.of_project(&project("api")), Some(&moving));
        assert_eq!(drafts.of_project(&project("web")), None);
        assert_eq!(drafts.target(&moving), Some(Target::draft("local".into(), "api".into(), moving.clone())));
        assert_eq!(drafts.created(&moving), created, "moving is not creating");
        assert_eq!(drafts.target(&resident), Some(Target::draft("local".into(), "api".into(), resident.clone())));
    }

    #[test]
    fn a_launched_draft_never_opens_again() {
        let mut drafts = DraftSessions::default();
        let draft = drafts.create(project("web"));
        drafts.assign(project("web"), &draft);
        drafts.promote(&draft);
        assert_eq!(drafts.of_project(&project("web")), None);
        assert!(!drafts.is_live(&draft));
    }

    #[test]
    fn saved_drafts_come_back_older_than_new_ones_and_keep_their_project() {
        let mut drafts = DraftSessions::default();
        let mapped = drafts.create(project("web"));
        drafts.assign(project("web"), &mapped);
        let written = drafts.create(project("web"));
        let empty = drafts.create(project("api"));
        let launched = drafts.create(project("api"));
        drafts.assign(project("api"), &launched);
        drafts.promote(&launched);
        let saved = drafts.saved(|id| *id == written);
        let ids: Vec<String> = saved.iter().map(|s| s.draft_id.clone()).collect();
        assert_eq!(ids, [mapped.to_string(), written.to_string()], "not the empty or launched ones");
        assert!(!ids.contains(&empty.to_string()));

        let mut next = DraftSessions::default();
        let fresh = next.create(project("api"));
        next.assign(project("api"), &fresh);
        let restored = next.restore(saved.clone());
        assert_eq!(restored, [mapped.clone(), written.clone()]);
        assert_eq!(next.of_project(&project("web")), Some(&mapped));
        assert!(next.created(&written) > next.created(&mapped));
        assert_eq!(next.target(&written), Some(Target::draft("local".into(), "web".into(), written.clone())));
        assert!(next.restore(saved).is_empty(), "a draft comes back once");
        let newest = next.create(project("web"));
        assert!(next.created(&newest) > next.created(&written), "new drafts sort first");
        next.forget(&mapped);
        assert_eq!(next.of_project(&project("web")), None);
        assert!(!next.is_live(&mapped));
    }

    #[test]
    fn a_restored_draft_does_not_displace_the_draft_a_project_opens_now() {
        let mut earlier = DraftSessions::default();
        let old = earlier.create(project("web"));
        earlier.assign(project("web"), &old);
        let mut drafts = DraftSessions::default();
        let now = drafts.create(project("web"));
        drafts.assign(project("web"), &now);
        drafts.restore(earlier.saved(|_| false));
        assert_eq!(drafts.of_project(&project("web")), Some(&now));
        assert!(drafts.is_live(&old));
    }

    fn checkout() -> WorkspaceChoice {
        WorkspaceChoice {
            mode: Some(EnvMode::Worktree),
            branch: Some("feature".into()),
            existing_worktree: Some("/repo/wt".into()),
            start_from_origin: Some(true),
        }
    }

    #[test]
    fn each_draft_keeps_its_workspace_picks_and_a_moved_one_drops_its_checkout() {
        let mut drafts = DraftSessions::default();
        let first = drafts.create(project("web"));
        let second = drafts.create(project("web"));
        drafts.set_workspace(&first, &project("web"), checkout());
        assert_eq!(drafts.workspace(&first), checkout());
        assert_eq!(drafts.workspace(&second), WorkspaceChoice::default(), "another draft of the project");
        drafts.assign(project("api"), &first);
        let moved = WorkspaceChoice { branch: None, existing_worktree: None, ..checkout() };
        assert_eq!(drafts.workspace(&first), moved, "the mode stays, the checkout was the old project's");
        drafts.set_workspace(&first, &project("web"), checkout());
        assert_eq!(drafts.workspace(&first), moved, "picks made before the move");
    }

    #[test]
    fn a_saved_draft_comes_back_with_its_workspace_picks() {
        let mut drafts = DraftSessions::default();
        let draft = drafts.create(project("web"));
        drafts.assign(project("web"), &draft);
        drafts.set_workspace(&draft, &project("web"), checkout());
        let saved = serde_json::to_value(drafts.saved(|_| false)).unwrap();
        let mut next = DraftSessions::default();
        next.restore(serde_json::from_value(saved).unwrap());
        assert_eq!(next.workspace(&draft), checkout());
    }

    #[test]
    fn a_drafts_thread_id_is_assigned_once_and_kept_until_it_launches_or_goes() {
        let mut drafts = DraftSessions::default();
        let draft = drafts.create(project("web"));
        drafts.assign(project("web"), &draft);
        assert_eq!(drafts.thread_id(&draft), None, "none until a terminal or a send needs it");
        let (id, assigned) = drafts.reserve_thread(&draft).unwrap();
        assert!(assigned);
        assert_eq!(drafts.reserve_thread(&draft), Some((id.clone(), false)), "a send reuses the terminals' id");
        assert_eq!(drafts.launching_with(Some("local"), &id), Some(&draft));
        assert_eq!(drafts.launching_with(Some("remote"), &id), None, "another environment's thread with that id");
        assert_eq!(drafts.launching_with(None, &id), Some(&draft), "a legacy key names no environment");

        let mut next = DraftSessions::default();
        next.restore(drafts.saved(|_| false));
        assert_eq!(next.thread_id(&draft), Some(id.as_str()), "it outlives a restart with the draft");
        assert_eq!(next.launching_with(Some("local"), &id), Some(&draft));

        drafts.promote(&draft);
        assert_eq!(drafts.launching_with(Some("local"), &id), None, "the launched thread owns it now");
        assert_eq!(drafts.reserve_thread(&draft), None, "a launched draft takes no new thread");
        next.forget(&draft);
        assert_eq!(next.launching_with(Some("local"), &id), None);
        assert_eq!(drafts.reserve_thread(&DraftId::for_test("gone")), None);
    }

    #[test]
    fn a_removed_projects_drafts_go_with_it() {
        let mut drafts = DraftSessions::default();
        let gone = drafts.create(project("web"));
        drafts.assign(project("web"), &gone);
        let kept = drafts.create(project("api"));
        drafts.forget_project(&project("web"));
        assert_eq!(drafts.of_project(&project("web")), None);
        assert!(!drafts.is_live(&gone));
        assert!(drafts.is_live(&kept));
    }
}
