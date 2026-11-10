//! Reopen closed view (`closedViewStore.ts`, `reopenClosedView.ts`,
//! `ReopenClosedViewShortcut.tsx`): closed panel and browser tabs, newest first, each with the
//! thread or draft that owned it (or the Pull requests page's panel), kept in `closed-views.json`.
//! `view.reopenClosed` (Mod+Shift+T) restores the newest one its owner can take and opens that owner.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, Context, Window};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::Surface;
use super::stored::StoredTarget;
use crate::app::{Target, Workspace, pages};
use crate::preview;
use crate::pull_request_detail::Reference;

/// `closedViewStore`'s cap.
const MAX_ENTRIES: usize = 20;

/// `ClosedView`: a panel tab (never a terminal or a browser tab), a browser tab as its session
/// last stood, to open again as a new session, or a tab of the Pull requests page's panel
/// (`PULL_REQUESTS_PANEL_REF`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub(super) enum ClosedView {
    PanelTab {
        owner: StoredTarget,
        surface: Surface,
    },
    Browser {
        owner: StoredTarget,
        tab_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        /// The session's `PreviewViewportSetting`, as the wire carries it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        viewport: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile_id: Option<String>,
    },
    PullRequestTab {
        reference: Reference,
    },
}

impl ClosedView {
    /// The thread or draft that owned the tab; the Pull requests page has none.
    fn owner(&self) -> Option<&StoredTarget> {
        match self {
            Self::PanelTab { owner, .. } | Self::Browser { owner, .. } => Some(owner),
            Self::PullRequestTab { .. } => None,
        }
    }

    /// `sameTarget`: the same tab of the same owner.
    fn same_target(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::PanelTab { owner, surface }, Self::PanelTab { owner: other_owner, surface: other_surface }) => {
                owner == other_owner && surface == other_surface
            }
            (Self::Browser { owner, tab_id, .. }, Self::Browser { owner: other_owner, tab_id: other_tab, .. }) => {
                owner == other_owner && tab_id == other_tab
            }
            (Self::PullRequestTab { reference }, Self::PullRequestTab { reference: other }) => reference == other,
            _ => false,
        }
    }

    /// `isPersistentView`: an incognito tab is forgotten with the app.
    fn persistent(&self) -> bool {
        !matches!(self, Self::Browser { profile_id: Some(profile), .. } if profile == preview::INCOGNITO_PROFILE_ID)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Entry {
    pub id: u64,
    pub view: ClosedView,
}

/// The history, newest first; ids are this launch's own.
#[derive(Default)]
pub(super) struct History {
    entries: Vec<Entry>,
    next_id: u64,
}

impl History {
    fn from_document(document: &Value) -> Self {
        let views = document.get("entries").and_then(Value::as_array).into_iter().flatten();
        let mut history = Self::default();
        for view in views.filter_map(|view| serde_json::from_value::<ClosedView>(view.clone()).ok()) {
            if view.persistent() && history.entries.len() < MAX_ENTRIES {
                history.push_back(view);
            }
        }
        history
    }

    fn document(&self) -> Value {
        let entries: Vec<&ClosedView> =
            self.entries.iter().map(|entry| &entry.view).filter(|view| view.persistent()).collect();
        json!({ "version": 1, "entries": entries })
    }

    fn push_back(&mut self, view: ClosedView) {
        self.next_id += 1;
        self.entries.push(Entry { id: self.next_id, view });
    }

    pub(super) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// `remember`: the view goes first, replacing an older entry for the same tab.
    pub(super) fn remember(&mut self, view: ClosedView) -> u64 {
        self.next_id += 1;
        self.entries.retain(|entry| !entry.view.same_target(&view));
        self.entries.insert(0, Entry { id: self.next_id, view });
        self.entries.truncate(MAX_ENTRIES);
        self.next_id
    }

    /// `defer`: an entry that could not reopen now moves behind the others.
    pub(super) fn defer(&mut self, id: u64) {
        if let Some(index) = self.entries.iter().position(|entry| entry.id == id) {
            let entry = self.entries.remove(index);
            self.entries.push(entry);
        }
    }

    pub(super) fn remove(&mut self, id: u64) {
        self.entries.retain(|entry| entry.id != id);
    }

    /// A sent draft's tabs belong to the thread it launched, as the web's draft and thread share a
    /// ref; a newer close of the same tab replaces an older one, as in `remember`.
    pub(super) fn retarget(&mut self, from: &StoredTarget, to: &StoredTarget) {
        for entry in &mut self.entries {
            if let ClosedView::PanelTab { owner, .. } | ClosedView::Browser { owner, .. } = &mut entry.view
                && owner == from
            {
                *owner = to.clone();
            }
        }
        let mut kept: Vec<Entry> = Vec::with_capacity(self.entries.len());
        for entry in self.entries.drain(..) {
            if !kept.iter().any(|newer| newer.view.same_target(&entry.view)) {
                kept.push(entry);
            }
        }
        self.entries = kept;
    }
}

/// The history and the file that keeps it.
pub(super) struct ClosedViews {
    history: History,
    writer: super::writer::Writer,
    persisted: Value,
    /// A browser tab's reopen is waiting on the server; presses meanwhile queue behind it.
    reopening: bool,
    queued: usize,
}

impl ClosedViews {
    pub(super) fn restored() -> Self {
        let writer = super::writer::Writer::new("closed views", "closed-views.json");
        let history = writer.read().map(|document| History::from_document(&document)).unwrap_or_default();
        Self { persisted: history.document(), history, writer, reopening: false, queued: 0 }
    }

    fn update(&mut self, change: impl FnOnce(&mut History)) {
        change(&mut self.history);
        let document = self.history.document();
        if document != self.persisted {
            self.persisted = document.clone();
            self.writer.save(document);
        }
    }
}

/// What `planNextReopen` needs to know about an entry's owner.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct OwnerState {
    /// The owner's environment is one this client lists.
    pub environment_known: bool,
    /// The environment list has loaded, so an unknown environment is gone for good.
    pub catalog_ready: bool,
    /// The owner's thread (or a draft's project) is listed.
    pub owner_exists: bool,
    /// False only for a side chat tab whose thread is gone.
    pub side_chat_exists: bool,
    /// The owner's environment has sent its threads, so a missing owner is gone for good.
    pub shell_live: bool,
    /// The owner's panel already shows the tab.
    pub already_open: bool,
}

/// `planNextReopen`: entries to forget, and the first one to restore. An entry whose owner may
/// still show up stops the walk, so a later entry never jumps ahead of it.
pub(super) fn plan_next_reopen(
    entries: &[Entry],
    owner: impl Fn(&ClosedView) -> OwnerState,
) -> (Vec<u64>, Option<u64>) {
    let mut drop = Vec::new();
    for entry in entries {
        let state = owner(&entry.view);
        if !state.environment_known {
            if !state.catalog_ready {
                break;
            }
            drop.push(entry.id);
            continue;
        }
        if !state.owner_exists || !state.side_chat_exists {
            if state.shell_live {
                continue;
            }
            break;
        }
        if state.already_open {
            drop.push(entry.id);
            continue;
        }
        return (drop, Some(entry.id));
    }
    (drop, None)
}

/// The order a close records its tabs in: the one that was active last, so it reopens first.
pub(super) fn closed_in_order<T: PartialEq>(mut closed: Vec<T>, active: Option<&T>) -> Vec<T> {
    if let Some(index) = active.and_then(|active| closed.iter().position(|s| s == active)) {
        let active = closed.remove(index);
        closed.push(active);
    }
    closed
}

impl Workspace {
    /// Remembers a tab the user closed on the open target's panel; terminals and browser tabs
    /// are remembered their own way, or not at all.
    pub(super) fn remember_closed_surface(&mut self, surface: &Surface) {
        let Some(target) = &self.target else { return };
        if matches!(surface, Surface::Terminal(_) | Surface::Browser(_)) {
            return;
        }
        let view = ClosedView::PanelTab { owner: target.into(), surface: surface.clone() };
        self.right_panel.closed.update(|history| {
            history.remember(view);
        });
    }

    /// `closePreviewSession`'s record: a browser tab the server closed, as it last stood.
    pub(in crate::app) fn remember_closed_browser(&mut self, thread: &preview::ThreadRef, session: &preview::Session) {
        let target = crate::app::preview::panel_target(thread);
        let view = ClosedView::Browser {
            owner: (&target).into(),
            tab_id: session.tab_id.clone(),
            url: session.nav_status.url().map(str::to_owned),
            viewport: Some(session.viewport.to_json()),
            profile_id: session.profile_id.clone(),
        };
        self.right_panel.closed.update(|history| {
            history.remember(view);
        });
    }

    /// `draft` launched `thread`, or moved to another project on its environment: its closed tabs
    /// reopen there. The web keys them by environment and thread id, so a move off it leaves them.
    pub(in crate::app) fn retarget_closed_views(&mut self, draft: &Target, thread: &Target) {
        if draft.environment() != thread.environment() {
            return;
        }
        let (from, to) = (StoredTarget::from(draft), StoredTarget::from(thread));
        self.right_panel.closed.update(|history| history.retarget(&from, &to));
    }

    /// Forgets a deleted thread's panel layout (`removeThread`).
    pub(in crate::app) fn forget_thread_panel(&mut self, environment_id: &str, thread_id: &str) {
        let target = Target::Thread { env: environment_id.to_owned().into(), id: thread_id.to_owned() };
        self.right_panel.panels_mut().remove(&target);
    }

    /// `close_pr_tab`'s record: the page's tabs, the one it showed last.
    pub(in crate::app) fn remember_closed_pull_request_tabs(
        &mut self,
        closed: Vec<Reference>,
        shown: Option<&Reference>,
    ) {
        self.right_panel.closed.update(|history| {
            for reference in closed_in_order(closed, shown) {
                history.remember(ClosedView::PullRequestTab { reference });
            }
        });
    }

    /// `environmentCatalog.isReady`: the saved environments are read, and the primary's id is
    /// known unless the local environment is off and there is no primary.
    fn environment_catalog_ready(&self, cx: &App) -> bool {
        !self.saved_environments.read(cx).loading()
            && (matches!(self.connection, crate::app::Connection::Off) || self.primary_environment().is_some())
    }

    fn reopen_owner_state(&self, view: &ClosedView, catalog_ready: bool) -> OwnerState {
        let Some(owner) = view.owner() else {
            // The Pull requests page is always there to reopen on.
            let ClosedView::PullRequestTab { reference } = view else { return OwnerState::default() };
            return OwnerState {
                environment_known: true,
                catalog_ready: true,
                owner_exists: true,
                side_chat_exists: true,
                shell_live: true,
                already_open: self.pull_request_tab_shown(reference),
            };
        };
        let target = owner.target();
        let environment = target.environment();
        let owner_exists = match &target {
            Target::Thread { env, id } => self.shell.threads.iter().any(|t| &t.id == id && &t.environment_id == env),
            // `getDraftThreadByRef`: the draft itself, unsent, not its project's next one.
            Target::Draft { env, project_id, draft } => {
                self.drafts.is_live(draft)
                    && self.shell.projects.iter().any(|p| &p.id == project_id && &p.environment_id == env)
            }
        };
        let side_chat_exists = match view {
            ClosedView::PanelTab { surface: Surface::SideChat(thread_id), .. } => {
                self.shell.threads.iter().any(|t| &t.id == thread_id && &t.environment_id == environment)
            }
            _ => true,
        };
        let panel = self.right_panel.panels.get(&target);
        let already_open = panel.is_some_and(|panel| match view {
            ClosedView::PanelTab { surface, .. } => panel.open && panel.surfaces.contains(surface),
            ClosedView::Browser { tab_id, .. } => panel.surfaces.contains(&Surface::Browser(tab_id.clone())),
            ClosedView::PullRequestTab { .. } => false,
        });
        OwnerState {
            environment_known: self.session(environment).is_some(),
            catalog_ready,
            owner_exists,
            side_chat_exists,
            shell_live: self.shell.has_snapshot(environment),
            already_open,
        }
    }

    /// Whether `target` has a project workspace for Files and file tabs (`workspaceAvailable`).
    fn owner_has_workspace(&self, target: &Target) -> bool {
        let (env, project_id) = match target {
            Target::Thread { env, id } => {
                let Some(thread) = self.shell.threads.iter().find(|t| &t.id == id && &t.environment_id == env) else {
                    return false;
                };
                (env, &thread.project_id)
            }
            Target::Draft { env, project_id, .. } => (env, project_id),
        };
        self.shell.projects.iter().any(|p| &p.id == project_id && &p.environment_id == env)
    }

    fn open_owner(&mut self, target: &Target, window: &mut Window, cx: &mut Context<Self>) {
        match target.clone() {
            Target::Thread { env, id } => self.select_thread_in(env, id, window, cx),
            draft @ Target::Draft { .. } => self.open_draft_entry(draft, window, cx),
        }
    }

    /// Whether Reopen Closed Tab has anything to reopen (`hasHistory`).
    pub(in crate::app) fn has_closed_views(&self) -> bool {
        !self.right_panel.closed.history.entries().is_empty()
    }

    /// `view.reopenClosed`: `false` with nothing to reopen, so the key goes on; a held key is
    /// claimed without reopening more.
    pub(in crate::app) fn reopen_closed_view(
        &mut self,
        held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let closed = &mut self.right_panel.closed;
        if closed.history.entries().is_empty() || held {
            return !closed.history.entries().is_empty();
        }
        if closed.reopening {
            closed.queued += 1;
            return true;
        }
        let catalog_ready = self.environment_catalog_ready(cx);
        let (drop, restore) = plan_next_reopen(self.right_panel.closed.history.entries(), |view| {
            self.reopen_owner_state(view, catalog_ready)
        });
        let restore = restore
            .and_then(|id| self.right_panel.closed.history.entries().iter().find(|entry| entry.id == id).cloned());
        self.right_panel.closed.update(|history| {
            for id in drop {
                history.remove(id);
            }
        });
        let Some(Entry { id, view }) = restore else { return true };
        match view {
            ClosedView::PanelTab { owner, surface } => {
                let target = owner.target();
                let needs_workspace = matches!(surface, Surface::Files | Surface::File(_));
                if needs_workspace && !self.owner_has_workspace(&target) {
                    self.right_panel.closed.update(|history| history.defer(id));
                    return true;
                }
                self.right_panel.closed.update(|history| history.remove(id));
                self.open_owner(&target, window, cx);
                match surface {
                    Surface::File(path) => self.open_file_surface(path, window, cx),
                    Surface::Attachment(file) => self.open_attachment(file, window, cx),
                    surface => self.open_surface(surface, window, cx),
                }
            }
            ClosedView::PullRequestTab { reference } => {
                self.right_panel.closed.update(|history| history.remove(id));
                self.open_page(pages::Page::PullRequests, cx);
                self.open_pull_request_panel(reference, None, cx);
            }
            ClosedView::Browser { owner, url, viewport, profile_id, .. } => {
                let target = owner.target();
                let Target::Thread { env, id: thread_id } = &target else {
                    self.right_panel.closed.update(|history| history.remove(id));
                    return true;
                };
                let profile = profile_id.unwrap_or_else(|| preview::default_profile_id(&self.client_settings.values));
                let mut input = json!({"threadId": thread_id, "profileId": profile});
                if let Some(url) = &url {
                    input["url"] = url.clone().into();
                }
                match viewport {
                    Some(viewport) => input["viewport"] = viewport,
                    None => {
                        let viewport = self.client_settings.values.get("browserDefaultViewport");
                        if viewport.is_object() {
                            input["viewport"] = viewport.clone();
                        }
                    }
                }
                self.right_panel.closed.reopening = true;
                let thread = preview::ThreadRef::new(env.clone(), thread_id.clone());
                self.request_preview_open(thread, input, window, cx, move |this, opened, window, cx| {
                    this.right_panel.closed.reopening = false;
                    match opened {
                        Ok(tab) => {
                            this.right_panel.closed.update(|history| history.remove(id));
                            this.open_owner(&target, window, cx);
                            this.open_surface(Surface::Browser(tab), window, cx);
                        }
                        Err(error) => {
                            this.right_panel.closed.update(|history| history.defer(id));
                            window.push_notification(Notification::error(error).title("Could not reopen view"), cx);
                        }
                    }
                    if this.right_panel.closed.queued > 0 {
                        this.right_panel.closed.queued -= 1;
                        this.reopen_closed_view(false, window, cx);
                    }
                });
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(thread: &str) -> StoredTarget {
        StoredTarget::Thread { environment_id: "env".into(), thread_id: thread.into() }
    }

    fn tab(thread: &str, surface: Surface) -> ClosedView {
        ClosedView::PanelTab { owner: owner(thread), surface }
    }

    fn browser(thread: &str, tab_id: &str, profile: Option<&str>) -> ClosedView {
        ClosedView::Browser {
            owner: owner(thread),
            tab_id: tab_id.into(),
            url: Some("http://localhost:3000/".into()),
            viewport: Some(json!({"_tag": "fill"})),
            profile_id: profile.map(str::to_owned),
        }
    }

    fn views(history: &History) -> Vec<&ClosedView> {
        history.entries().iter().map(|entry| &entry.view).collect()
    }

    #[test]
    fn the_newest_close_comes_first_once_per_tab() {
        let mut history = History::default();
        history.remember(tab("a", Surface::Diff));
        history.remember(tab("a", Surface::Files));
        history.remember(tab("b", Surface::Diff));
        history.remember(tab("a", Surface::Diff));
        assert_eq!(views(&history), [&tab("a", Surface::Diff), &tab("b", Surface::Diff), &tab("a", Surface::Files)]);
        for i in 0..MAX_ENTRIES {
            history.remember(tab("c", Surface::File(format!("f{i}"))));
        }
        assert_eq!(history.entries().len(), MAX_ENTRIES);
        assert_eq!(history.entries()[0].view, tab("c", Surface::File(format!("f{}", MAX_ENTRIES - 1))));
    }

    #[test]
    fn a_deferred_entry_waits_behind_the_others() {
        let mut history = History::default();
        let older = history.remember(tab("a", Surface::Diff));
        let newer = history.remember(tab("a", Surface::Files));
        history.defer(newer);
        assert_eq!(history.entries().iter().map(|e| e.id).collect::<Vec<_>>(), [older, newer]);
        history.remove(older);
        assert_eq!(history.entries().iter().map(|e| e.id).collect::<Vec<_>>(), [newer]);
    }

    #[test]
    fn a_sent_drafts_tabs_move_to_the_thread_it_launched() {
        let draft =
            |id: &str| StoredTarget::Draft { environment_id: "b".into(), project_id: "p".into(), draft_id: id.into() };
        let thread = StoredTarget::Thread { environment_id: "b".into(), thread_id: "t".into() };
        let on = |owner: &StoredTarget, surface| ClosedView::PanelTab { owner: owner.clone(), surface };
        let mut history = History::default();
        history.remember(on(&thread, Surface::Diff));
        history.remember(on(&draft("other"), Surface::Files));
        history.remember(on(&draft("sent"), Surface::Diff));
        history.remember(on(&draft("sent"), Surface::File("x".into())));
        history.retarget(&draft("sent"), &thread);
        // The newer close of the same tab wins, as `remember` keeps it.
        assert_eq!(
            views(&history),
            [
                &on(&thread, Surface::File("x".into())),
                &on(&thread, Surface::Diff),
                &on(&draft("other"), Surface::Files)
            ]
        );
    }

    #[test]
    fn the_history_is_stored_without_incognito_tabs() {
        let mut history = History::default();
        let reference = Reference {
            environment_id: "e".into(),
            project_id: "p".into(),
            host: Some("github.com".into()),
            repository: "o/r".into(),
            number: 3,
        };
        history.remember(browser("a", "private", Some(preview::INCOGNITO_PROFILE_ID)));
        history.remember(browser("a", "public", Some("default")));
        history.remember(ClosedView::PullRequestTab { reference: reference.clone() });
        history.remember(tab("a", Surface::PullRequest(reference.clone())));
        history.remember(ClosedView::PullRequestTab { reference });
        assert_eq!(history.entries().len(), 4, "incognito tabs still reopen in this launch");
        let text = serde_json::to_string(&history.document()).unwrap();
        let mut stored: Value = serde_json::from_str(&text).unwrap();
        stored["entries"].as_array_mut().unwrap().push(json!({"kind": "panel-tab", "owner": {"kind": "nowhere"}}));
        let restored = History::from_document(&stored);
        assert_eq!(views(&restored), views(&history)[..3].to_vec());
        assert_eq!(restored.document(), history.document());
    }

    #[test]
    fn closing_several_tabs_leaves_the_active_one_newest() {
        let closed = vec![Surface::Diff, Surface::Files, Surface::PullRequests];
        let order = closed_in_order(closed.clone(), Some(&Surface::Files));
        assert_eq!(order, [Surface::Diff, Surface::PullRequests, Surface::Files]);
        assert_eq!(closed_in_order(closed.clone(), Some(&Surface::SideChat("kept".into()))), closed);
        let mut history = History::default();
        for surface in order {
            history.remember(tab("a", surface));
        }
        assert_eq!(history.entries()[0].view, tab("a", Surface::Files));
    }

    #[test]
    fn reopening_skips_or_waits_on_owners_as_the_web_does() {
        let mut history = History::default();
        let ids: Vec<u64> = ["gone-env", "missing", "open", "next", "later"]
            .into_iter()
            .rev()
            .map(|thread| history.remember(tab(thread, Surface::Diff)))
            .collect();
        let [later, next, open, _, gone_env] = ids[..] else { unreachable!() };
        let state = |live: bool, catalog: bool| {
            move |view: &ClosedView| {
                let Some(StoredTarget::Thread { thread_id, .. }) = view.owner() else { unreachable!() };
                OwnerState {
                    environment_known: thread_id != "gone-env",
                    catalog_ready: catalog,
                    owner_exists: thread_id != "missing",
                    side_chat_exists: true,
                    shell_live: live,
                    already_open: thread_id == "open",
                }
            }
        };
        assert_eq!(plan_next_reopen(history.entries(), state(true, true)), (vec![gone_env, open], Some(next)));
        assert_eq!(
            plan_next_reopen(history.entries(), state(true, false)),
            (vec![], None),
            "the catalog may still list it"
        );
        assert_eq!(
            plan_next_reopen(&history.entries()[1..], state(false, true)),
            (vec![], None),
            "the owner may still arrive"
        );
        assert_eq!(plan_next_reopen(&history.entries()[4..], state(true, true)), (vec![], Some(later)));
    }
}
