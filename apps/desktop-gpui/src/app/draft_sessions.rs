//! Independent draft sessions (`composerDraftStore`'s `draftThreadsByThreadKey` and
//! `logicalProjectDraftThreadKeyByLogicalProjectKey`): every unsent draft has its own id, so a
//! project can hold several, and a draft keeps its identity when it moves to another project.

use std::collections::HashMap;
use std::fmt;

use gpui_kit::SharedString;

use super::Target;
use crate::model::ScopedId;

/// One draft's identity (`DraftId`), unique across runs like the web's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DraftId(SharedString);

impl DraftId {
    fn new() -> Self {
        Self(super::new_id().into())
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
    /// Creation order (`createdAt`): the sidebar lists drafts newest first.
    created: u64,
    /// It launched its thread (`promotedTo`), so no project opens it again.
    promoted: bool,
}

#[derive(Default)]
pub(super) struct DraftSessions {
    sessions: HashMap<DraftId, Session>,
    next: u64,
    /// The draft "New thread" opens in each project; a draft with content that lost its place
    /// stays a sidebar row.
    by_project: HashMap<ScopedId, DraftId>,
}

impl DraftSessions {
    /// A new draft in `project`, not yet its project's draft.
    pub(super) fn create(&mut self, project: ScopedId) -> DraftId {
        let id = DraftId::new();
        self.next += 1;
        self.sessions.insert(id.clone(), Session { project, created: self.next, promoted: false });
        id
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
        if let Some(session) = self.sessions.get_mut(draft) {
            session.project = project.clone();
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
    pub(super) fn created(&self, draft: &DraftId) -> u64 {
        self.sessions.get(draft).map_or(0, |session| session.created)
    }

    /// The draft launched its thread: its project's next draft starts fresh.
    pub(super) fn promote(&mut self, draft: &DraftId) {
        if let Some(session) = self.sessions.get_mut(draft) {
            session.promoted = true;
        }
        self.by_project.retain(|_, mapped| mapped != draft);
    }

    /// A removed project's drafts go with it.
    pub(super) fn forget_project(&mut self, project: &ScopedId) {
        self.sessions.retain(|_, session| session.project != *project);
        self.by_project.remove(project);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
