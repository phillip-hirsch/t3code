//! The right panel (`RightPanelTabs.tsx` inside `PreviewPanelShell`, state as `rightPanelStore.ts`)
//! with its Diff (`DiffPanel.tsx`) and Browser (`preview/`) surfaces. The panel owns the titlebar
//! row above it, keeps per-thread open state and tabs, and resizes from its left edge.
//! The web's other surfaces (terminal, files, pull requests, devices, side chats) are not
//! native yet, so the launcher offers Browser and Diff.

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::diff::{Diff, DiffEvent, DiffFile, DiffMode, DiffState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, DragMoveEvent, Element, ElementId, Empty, Entity,
    GlobalElementId, Hsla, InspectorElementId, InteractiveElement as _, IntoElement, LayoutId, MouseButton,
    ParentElement as _, Pixels, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, Window, WindowControlArea, deferred, div, px,
};
use serde_json::{Value, json};

use super::git::{self, DiffColorScheme};
use super::titlebar::{CAPTION_BUTTONS_WIDTH, RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT, icon};
use super::{Target, Workspace};
use crate::model::{Checkpoint, ThreadProjection};
use crate::theme::{self, Tokens};

const DEFAULT_WIDTH: f32 = 540.;
const MIN_WIDTH: f32 = 360.;
const MAX_WIDTH_FRACTION: f32 = 0.7;
/// The chat column keeps at least this much beside the panel.
const SIBLING_MIN_WIDTH: f32 = 360.;

/// `getPreviewPanelMaxWidth` + the clamp in `useResizableWidth`.
fn clamp_width(width: f32, viewport: f32, row: f32) -> f32 {
    let max = (viewport * MAX_WIDTH_FRACTION).floor().min((row - SIBLING_MIN_WIDTH).floor()).max(MIN_WIDTH);
    width.clamp(MIN_WIDTH, max)
}

/// What the Diff surface shows (`DiffPanelSelection`); the web's default is the branch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(super) enum DiffScope {
    #[default]
    Branch,
    Unstaged,
    Turn(String),
    Commit(String),
}

/// One completed turn the Diff menu offers (`ThreadCheckpointSummary`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TurnSummary {
    pub run_id: String,
    pub turn_count: u32,
    pub completed_at: String,
}

/// `deriveThreadCheckpointSummaries`, newest turn first, as `DiffPanel` orders them.
pub(super) fn turn_summaries(projection: &ThreadProjection) -> Vec<TurnSummary> {
    let mut turns: Vec<TurnSummary> = projection
        .checkpoints
        .iter()
        .filter_map(|checkpoint| {
            Some(TurnSummary {
                run_id: checkpoint.run_id.clone()?,
                turn_count: u32::try_from(checkpoint.app_run_ordinal?).ok()?,
                completed_at: checkpoint.captured_at.clone(),
            })
        })
        .collect();
    turns.sort_by(|a, b| b.turn_count.cmp(&a.turn_count).then_with(|| b.completed_at.cmp(&a.completed_at)));
    turns
}

/// `revertThreadCheckpoint`: roll the thread back to the checkpoint that closes
/// `turn_count` (0 is the scope's base snapshot). `None` when it is not ready.
pub(super) fn rollback_command(projection: &ThreadProjection, turn_count: u32, restore_files: bool) -> Option<Value> {
    let checkpoint = projection.checkpoints.iter().rev().find(|c| {
        if turn_count == 0 {
            c.ordinal_within_scope == 0 && c.app_run_ordinal.is_none()
        } else {
            c.app_run_ordinal == Some(i64::from(turn_count))
        }
    })?;
    (checkpoint.status == "ready").then(|| {
        json!({
            "type": "checkpoint.rollback",
            "restoreFiles": restore_files,
            "commandId": super::new_id(),
            "threadId": projection.thread.id,
            "scopeId": checkpoint.scope_id,
            "checkpointId": checkpoint.id,
        })
    })
}

/// One tab of the panel (`RightPanelSurface`): the Diff, or one browser tab by its preview tab id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Surface {
    Diff,
    Browser(String),
    /// A side chat's tab, by its thread id.
    SideChat(String),
}

/// A target's panel (`ThreadRightPanelState`).
#[derive(Clone, Default)]
struct PanelState {
    open: bool,
    surfaces: Vec<Surface>,
    active: Option<Surface>,
    scope: DiffScope,
    /// Bumped by every panel change the user makes (`userActionRevisionByThreadKey`).
    user_revision: u64,
    /// The turn whose diff was last offered on its own (`proactiveDiffTurnId`).
    proactive_diff_run: Option<String>,
}

impl PanelState {
    /// `openProactive` for a turn's diff: offered once per turn, and refused after a panel
    /// choice made since `revision` or while the diff already shows.
    fn open_proactive_diff(&mut self, run: &str, revision: u64) -> bool {
        if self.proactive_diff_run.as_deref() == Some(run) {
            return false;
        }
        self.proactive_diff_run = Some(run.to_owned());
        if self.user_revision != revision || (self.open && self.active == Some(Surface::Diff)) {
            return false;
        }
        self.upsert(Surface::Diff);
        self.scope = DiffScope::Branch;
        true
    }

    /// `upsertSurface`: adds the surface if missing, shows the panel and activates it.
    fn upsert(&mut self, surface: Surface) {
        if !self.surfaces.contains(&surface) {
            self.surfaces.push(surface.clone());
        }
        self.open = true;
        self.active = Some(surface);
    }

    /// `closeSurface`: the panel hides with its last surface; the next one over takes focus.
    fn close(&mut self, surface: &Surface) {
        let Some(index) = self.surfaces.iter().position(|s| s == surface) else { return };
        self.surfaces.remove(index);
        self.open &= !self.surfaces.is_empty();
        if self.active.as_ref() == Some(surface) {
            self.active = self.surfaces.get(index.min(self.surfaces.len().saturating_sub(1))).cloned();
        }
    }

    /// `reconcileBrowserSurfaces`: a tab per preview session, dropping closed ones. Closing the
    /// last session hides the panel rather than leave it on the launcher.
    fn reconcile_browsers(&mut self, tab_ids: &[&str]) {
        let had_browser = self.surfaces.iter().any(|s| matches!(s, Surface::Browser(_)));
        self.surfaces.retain(|s| !matches!(s, Surface::Browser(tab) if !tab_ids.contains(&tab.as_str())));
        for tab in tab_ids {
            let surface = Surface::Browser((*tab).to_owned());
            if !self.surfaces.contains(&surface) {
                self.surfaces.push(surface);
            }
        }
        if self.active.as_ref().is_some_and(|active| !self.surfaces.contains(active)) {
            let browser = self.surfaces.iter().find(|s| matches!(s, Surface::Browser(_)));
            self.active = browser.or(self.surfaces.first()).cloned();
        }
        if self.surfaces.is_empty() && had_browser {
            self.open = false;
        }
    }
}

#[derive(Clone, PartialEq)]
struct DiffKey {
    target: Target,
    cwd: Option<String>,
    scope: DiffScope,
    ignore_whitespace: bool,
    /// The turn count, or the git status totals, so new work refetches.
    version: (u64, u64, u64),
    /// `deriveWorkspaceMutationId` for git selections: an edit that keeps the totals refetches too.
    mutation: (Option<String>, Option<String>),
    refresh: u64,
}

enum DiffLoad {
    Idle,
    Loading,
    Ready { additions: usize, deletions: usize, head: Option<String>, base: Option<String>, truncated: bool },
    Empty(&'static str),
    Failed(SharedString),
}

#[derive(Clone)]
struct Commit {
    sha: String,
    subject: String,
}

pub(super) struct RightPanel {
    panels: HashMap<Target, PanelState>,
    width: f32,
    /// The "+" menu is open; a native browser view must not cover it.
    pub(super) add_menu_open: bool,
    diff: Entity<DiffState>,
    loaded: Option<DiffKey>,
    load: DiffLoad,
    _task: Option<Task<()>>,
    /// The wrap and whitespace toggles; unset, they follow `wordWrap` and `diffIgnoreWhitespace`.
    wrap: Option<bool>,
    ignore_whitespace: Option<bool>,
    /// The target whose Diff surface shows; the toggles reset when it changes, as `DiffPanel` remounts.
    diff_mount: Option<Target>,
    /// Folded files of each selection the user folded or unfolded (`collapsedFileKeysByScopeKey`).
    collapsed: HashMap<(Target, DiffScope), HashSet<SharedString>>,
    /// The selection whose files `diff` holds.
    shown: Option<(Target, DiffScope)>,
    refresh: u64,
    commits: Vec<Commit>,
    /// The tab strip, and the tab it last scrolled into view.
    tabs_scroll: ScrollHandle,
    revealed_tab: Option<(Target, Surface, usize)>,
    observation: Option<Observation>,
    _diff_events: Subscription,
}

impl RightPanel {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        let diff = cx.new(|cx| DiffState::new(Vec::new(), cx));
        let diff_events = cx.subscribe(&diff, |this, _, event: &DiffEvent, cx| {
            if matches!(event, DiffEvent::FileExpanded(_) | DiffEvent::FileCollapsed(_)) {
                this.remember_collapsed(cx);
            }
        });
        Self {
            panels: HashMap::new(),
            width: DEFAULT_WIDTH,
            add_menu_open: false,
            diff,
            loaded: None,
            load: DiffLoad::Idle,
            _task: None,
            wrap: None,
            ignore_whitespace: None,
            diff_mount: None,
            collapsed: HashMap::new(),
            shown: None,
            refresh: 0,
            commits: Vec::new(),
            tabs_scroll: ScrollHandle::new(),
            revealed_tab: None,
            observation: None,
            _diff_events: diff_events,
        }
    }
}

/// Drag payload for the left-edge resize handle.
struct RightPanelResize;

impl Workspace {
    fn panel_state(&self) -> Option<&PanelState> {
        self.right_panel.panels.get(self.target.as_ref()?)
    }

    fn panel_state_mut(&mut self) -> Option<&mut PanelState> {
        let target = self.target.clone()?;
        Some(self.right_panel.panels.entry(target).or_default())
    }

    /// The panel state for a change the user made, which proactive opens then defer to.
    fn user_panel_state(&mut self) -> Option<&mut PanelState> {
        let panel = self.panel_state_mut()?;
        panel.user_revision += 1;
        Some(panel)
    }

    pub(super) fn right_panel_open(&self) -> bool {
        self.page.is_none() && self.panel_state().is_some_and(|panel| panel.open)
    }

    /// The surface on screen (`selectActiveRightPanelSurface`).
    pub(super) fn active_surface(&self) -> Option<&Surface> {
        self.panel_state().filter(|panel| panel.open && self.page.is_none())?.active.as_ref()
    }

    /// The panel's laid-out width while it is open.
    pub(super) fn right_panel_width(&self, window: &Window) -> Option<f32> {
        if !self.right_panel_open() {
            return None;
        }
        let viewport = f32::from(window.viewport_size().width);
        let sidebar = if self.sidebar_open { f32::from(self.sidebar_width()) } else { 0. };
        Some(clamp_width(self.right_panel.width, viewport, viewport - sidebar))
    }

    /// `toggleRightPanel` (Ctrl+Alt+B and the titlebar toggle).
    pub(super) fn toggle_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.right_panel_open() {
            self.close_right_panel(cx);
        } else if let Some(panel) = self.user_panel_state() {
            panel.open = true;
            self.sync_active_browser(window, cx);
            cx.notify();
        }
    }

    /// `closePreviewPanel`: hiding the panel floats the browser it showed.
    pub(super) fn close_right_panel(&mut self, cx: &mut Context<Self>) {
        if let (Some(Target::Thread(thread_id)), Some(Surface::Browser(tab))) =
            (self.target.clone(), self.active_surface().cloned())
        {
            self.float_browser(thread_id, tab, cx);
        }
        if let Some(panel) = self.user_panel_state() {
            panel.open = false;
        }
        self.preview.menu_open = false;
        cx.notify();
    }

    /// Adds (or focuses) a surface and shows the panel.
    pub(super) fn open_surface(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        self.page = None;
        if let Some(panel) = self.user_panel_state() {
            panel.upsert(surface);
        }
        self.sync_active_browser(window, cx);
        cx.notify();
    }

    fn activate_surface(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.user_panel_state().filter(|panel| panel.surfaces.contains(&surface)) {
            panel.open = true;
            panel.active = Some(surface);
        }
        self.sync_active_browser(window, cx);
        cx.notify();
    }

    /// Closes a tab; a browser tab's session closes with it.
    pub(super) fn close_surface(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.user_panel_state() {
            panel.close(&surface);
        }
        if let Surface::SideChat(thread_id) = &surface {
            self.release_side_chat(thread_id, cx);
        }
        if let (Surface::Browser(tab), Some(Target::Thread(thread_id))) = (&surface, self.target.clone()) {
            self.close_preview_tab(thread_id, tab.clone(), window, cx);
        }
        self.sync_active_browser(window, cx);
        cx.notify();
    }

    /// Keeps the thread's focused preview tab on the browser surface the panel shows.
    fn sync_active_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let (Some(Target::Thread(thread_id)), Some(Surface::Browser(tab))) =
            (self.target.clone(), self.active_surface().cloned())
        {
            self.focus_preview_tab(&thread_id, &tab, window, cx);
        }
    }

    /// Follows a thread's preview sessions: a panel tab per drawable session.
    pub(super) fn reconcile_browser_surfaces(&mut self, thread_id: &str, tab_ids: &[&str]) {
        let panel = self.right_panel.panels.entry(Target::Thread(thread_id.to_owned())).or_default();
        panel.reconcile_browsers(tab_ids);
    }

    /// `addDiffSurface`: the details card's Changes row and the launcher's Diff open the branch's changes.
    pub(super) fn open_changes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.diff_available() {
            return;
        }
        if let Some(panel) = self.panel_state_mut() {
            panel.scope = DiffScope::Branch;
        }
        self.open_surface(Surface::Diff, window, cx);
    }

    /// `diffAvailable`: a server thread in a git repository.
    fn diff_available(&self) -> bool {
        matches!(self.target, Some(Target::Thread(_))) && self.vcs.status.as_ref().is_none_or(|s| s.local.is_repo)
    }

    /// `diffLayout`, which the layout toggle also writes.
    fn diff_mode(&self) -> DiffMode {
        match self.client_settings.values.get("diffLayout").as_str() {
            Some("split") => DiffMode::Split,
            _ => DiffMode::Unified,
        }
    }

    fn diff_wrap(&self) -> bool {
        self.right_panel.wrap.unwrap_or_else(|| self.client_settings.values.bool("wordWrap"))
    }

    fn diff_ignore_whitespace(&self) -> bool {
        self.right_panel.ignore_whitespace.unwrap_or_else(|| self.client_settings.values.bool("diffIgnoreWhitespace"))
    }

    fn diff_color_scheme(&self) -> DiffColorScheme {
        DiffColorScheme::from_settings(&self.client_settings.values)
    }

    /// Records the shown selection's folds once the user changes one.
    fn remember_collapsed(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = self.right_panel.shown.clone() else { return };
        let state = self.right_panel.diff.read(cx);
        let folded = state.files().iter().map(DiffFile::path).filter(|path| state.is_file_collapsed(path)).cloned();
        self.right_panel.collapsed.insert(shown, folded.collect());
    }

    /// `useProactivePanels` for turn diffs: with proactive panels on, a turn that just completed
    /// with enough changes opens the branch diff, unless the user chose a panel since it started.
    /// Change-request panels are not native, so their half of the hook is not ported.
    pub(super) fn observe_proactive_panels(&mut self, cx: &mut Context<Self>) {
        let Some(Target::Thread(thread_id)) = self.target.clone() else {
            self.right_panel.observation = None;
            return;
        };
        let target = Target::Thread(thread_id.clone());
        let revision = self.right_panel.panels.get(&target).map_or(0, |panel| panel.user_revision);
        let projection = self.open_projection();
        let running = projection.and_then(ThreadProjection::interruptible_run).map(|run| run.id.clone());
        let mut observation = observe(self.right_panel.observation.as_ref(), &thread_id, running.as_deref(), revision);
        let Some(projection) = projection else {
            self.right_panel.observation = Some(observation);
            return;
        };
        let latest = projection.runs.iter().rev().find(|run| run.status != "queued");
        let settled = latest
            .filter(|run| {
                !matches!(run.status.as_str(), "preparing" | "queued" | "starting" | "running" | "waiting")
                    && running.as_ref() != Some(&run.id)
            })
            .map(|run| run.id.clone());
        let completed = latest.is_some_and(|run| run.status == "completed");
        let newly = settled.filter(|run| {
            turn_just_completed(observation.running.as_ref().map(Option::as_deref), running.as_deref(), run, completed)
        });
        let offer = match &newly {
            Some(run) if self.client_settings.values.bool("proactivePanelsEnabled") => {
                let checkpoint = projection.checkpoints.iter().rev().find(|c| c.run_id.as_ref() == Some(run));
                turn_diff_offer(checkpoint, self.vcs.status.as_ref().map(|status| status.local.is_repo))
            }
            _ => DiffOffer::Ignore,
        };
        if offer != DiffOffer::Defer {
            observation.running = Some(running);
        }
        let revision = observation.revision;
        self.right_panel.observation = Some(observation);
        let (DiffOffer::Open, Some(run)) = (offer, newly) else { return };
        if self.right_panel.panels.entry(target).or_default().open_proactive_diff(&run, revision) {
            cx.notify();
        }
    }

    fn set_diff_scope(&mut self, scope: DiffScope, cx: &mut Context<Self>) {
        if let Some(panel) = self.panel_state_mut() {
            panel.scope = scope;
            cx.notify();
        }
    }

    /// Starts a fetch when the shown selection differs from the loaded one.
    fn sync_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_surface() != Some(&Surface::Diff) {
            return;
        }
        let mode = self.diff_mode();
        if self.right_panel.diff.read(cx).mode() != mode {
            self.right_panel.diff.update(cx, |state, cx| state.set_mode(mode, cx));
        }
        let Some(panel) = self.panel_state().cloned() else { return };
        let Some(target) = self.target.clone() else { return };
        let turns = self.open_projection().map(turn_summaries).unwrap_or_default();
        let scope = match &panel.scope {
            DiffScope::Turn(run) if !turns.iter().any(|t| &t.run_id == run) => {
                turns.first().map_or(DiffScope::Branch, |t| DiffScope::Turn(t.run_id.clone()))
            }
            scope => scope.clone(),
        };
        let cwd = self.git_cwd();
        let version = match &scope {
            DiffScope::Turn(run) => (turns.iter().find(|t| &t.run_id == run).map_or(0, |t| t.turn_count as u64), 0, 0),
            _ => self.vcs.status.as_ref().map_or((0, 0, 0), |s| {
                let (ins, del, _) = git::changes_totals(s);
                (ins, del, s.local.working_tree.insertions + s.local.working_tree.deletions)
            }),
        };
        let mutation = match &scope {
            DiffScope::Turn(_) => (None, None),
            _ => self
                .open_projection()
                .map(|p| workspace_mutation_id(p, turns.first().map(|t| t.completed_at.as_str())))
                .unwrap_or_default(),
        };
        let ignore_whitespace = self.diff_ignore_whitespace();
        let key = DiffKey {
            target: target.clone(),
            cwd: cwd.clone(),
            scope: scope.clone(),
            ignore_whitespace,
            version,
            mutation,
            refresh: self.right_panel.refresh,
        };
        if self.right_panel.loaded.as_ref() == Some(&key) {
            return;
        }
        self.right_panel.loaded = Some(key);
        let selection = (target.clone(), scope.clone());
        let (Some(client), Target::Thread(thread_id)) = (self.client(), target) else { return };
        if self.vcs.status.as_ref().is_some_and(|s| !s.local.is_repo) {
            self.right_panel.load =
                DiffLoad::Empty("Turn diffs are unavailable because this project is not a git repository.");
            self.clear_diff(cx);
            return;
        }
        let request = match &scope {
            DiffScope::Turn(run) => {
                let Some(turn) = turns.iter().find(|t| &t.run_id == run) else {
                    self.right_panel.load = DiffLoad::Empty("No completed turns yet.");
                    self.clear_diff(cx);
                    return;
                };
                (
                    "orchestration.getTurnDiff",
                    json!({
                        "threadId": thread_id,
                        "fromTurnCount": turn.turn_count.saturating_sub(1),
                        "toTurnCount": turn.turn_count,
                        "ignoreWhitespace": ignore_whitespace,
                    }),
                )
            }
            scope => {
                let Some(cwd) = cwd.clone() else { return };
                let mut input = json!({"cwd": cwd, "ignoreWhitespace": ignore_whitespace});
                match scope {
                    DiffScope::Unstaged => input["sourceKind"] = "working-tree".into(),
                    DiffScope::Commit(sha) => input["commitSha"] = sha.clone().into(),
                    _ => input["sourceKind"] = "branch-range".into(),
                }
                ("review.getDiffPreview", input)
            }
        };
        let source_kind = match &scope {
            DiffScope::Unstaged => Some("working-tree"),
            DiffScope::Commit(_) => Some("commit"),
            DiffScope::Branch => Some("branch-range"),
            DiffScope::Turn(_) => None,
        };
        let commits_request = cwd.filter(|_| source_kind.is_some()).map(|cwd| json!({"cwd": cwd}));
        self.right_panel.load = DiffLoad::Loading;
        let diff = self.right_panel.diff.clone();
        self.right_panel._task = Some(cx.spawn_in(window, async move |this, cx| {
            let (method, input) = request;
            let result = client.request(method, input).await;
            let commits = match commits_request {
                Some(input) => client.request("review.getBranchCommits", input).await.ok(),
                None => None,
            };
            let parsed = result.map_err(|error| error.to_string()).map(|value| parse_diff(&value, source_kind));
            let _ = this.update(cx, |this, cx| {
                if let Some(commits) = commits {
                    this.right_panel.commits = commits["commits"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| {
                            Some(Commit { sha: c["sha"].as_str()?.into(), subject: c["subject"].as_str()?.into() })
                        })
                        .collect();
                }
                this.right_panel.load = match parsed {
                    Err(error) => DiffLoad::Failed(error.into()),
                    Ok(None) => {
                        diff.update(cx, |state, cx| state.set_files(Vec::new(), cx));
                        DiffLoad::Empty("No patch available for this selection.")
                    }
                    Ok(Some(parsed)) if parsed.files.is_empty() => {
                        diff.update(cx, |state, cx| state.set_files(Vec::new(), cx));
                        DiffLoad::Empty("No net changes in this selection.")
                    }
                    Ok(Some(parsed)) => {
                        let additions = parsed.files.iter().map(DiffFile::additions).sum();
                        let deletions = parsed.files.iter().map(DiffFile::deletions).sum();
                        let paths: Vec<SharedString> = parsed.files.iter().map(|file| file.path().clone()).collect();
                        let folded = collapsed_files(
                            this.right_panel.collapsed.get(&selection),
                            this.client_settings.values.bool("diffFilesCollapsed"),
                            &paths,
                        );
                        diff.update(cx, |state, cx| {
                            state.set_files(parsed.files, cx);
                            for path in &paths {
                                state.set_file_collapsed(path, folded.contains(path), cx);
                            }
                        });
                        this.right_panel.shown = Some(selection);
                        DiffLoad::Ready {
                            additions,
                            deletions,
                            head: parsed.head,
                            base: parsed.base,
                            truncated: parsed.truncated,
                        }
                    }
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// "Edit from here?" (`RevertConfirmHost`): rewind the thread to before a user message,
    /// optionally restoring files, and put the message back in the composer. The timeline's
    /// per-message revert button calls this with the message's turn count and text.
    #[allow(dead_code)]
    pub(super) fn confirm_revert(
        &mut self,
        turn_count: u32,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_kit::component::WindowExt as _;
        use gpui_kit::component::dialog::{DialogClose, DialogFooter};
        if self.open_projection().is_some_and(ThreadProjection::is_running) {
            self.error = Some("Interrupt the current turn before reverting checkpoints.".into());
            cx.notify();
            return;
        }
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let choice = |id: &'static str, label: &'static str, restore_files: bool| {
                let (view, prompt) = (view.clone(), prompt.clone());
                let button = Button::new(id).label(label);
                let button = if restore_files { button.outline().text_color(gpui_kit::red()) } else { button.primary() };
                button.on_click(move |_, window, cx| {
                    window.close_dialog(cx);
                    let prompt = prompt.clone();
                    view.update(cx, |this, cx| this.revert_to_turn(turn_count, restore_files, prompt, window, cx));
                })
            };
            alert
                .title("Edit from here?")
                .description(
                    "Rewind chat to before this message. Your prompt and attachments return to the composer.                      “Revert files too” also undoes your own edits.",
                )
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.outline().label("Cancel")))
                        .child(choice("revert-files", "Revert files too", true))
                        .child(choice("revert-keep", "Revert and keep changes", false)),
                )
        });
    }

    fn revert_to_turn(
        &mut self,
        turn_count: u32,
        restore_files: bool,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(command) = self.open_projection().and_then(|p| rollback_command(p, turn_count, restore_files)) else {
            self.error = Some(format!("Checkpoint for turn {turn_count} is unavailable.").into());
            cx.notify();
            return;
        };
        self.dispatch(command, cx);
        self.set_composer_text(prompt, window, cx);
    }

    fn clear_diff(&mut self, cx: &mut Context<Self>) {
        self.right_panel.diff.update(cx, |state, cx| state.set_files(Vec::new(), cx));
    }

    /// The inline panel beside the main column, or `None` while closed.
    pub(super) fn render_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let mount = self.target.clone().filter(|_| self.active_surface() == Some(&Surface::Diff));
        if self.right_panel.diff_mount != mount {
            self.right_panel.diff_mount = mount;
            (self.right_panel.wrap, self.right_panel.ignore_whitespace) = (None, None);
        }
        let side_chat = match self.active_surface() {
            Some(Surface::SideChat(thread_id)) => Some(thread_id.clone()),
            _ => None,
        };
        self.sync_side_chat(side_chat.as_deref(), window, cx);
        let width = self.right_panel_width(window)?;
        self.sync_diff(window, cx);
        let tokens = *theme::tokens(cx);
        let panel = self.panel_state().cloned().unwrap_or_default();
        // A newly active tab, or a new tab, can sit outside the scrolled strip.
        let active_index = panel.active.as_ref().and_then(|active| panel.surfaces.iter().position(|s| s == active));
        if let (Some(index), Some(target)) = (active_index, self.target.clone()) {
            let key = (target, panel.surfaces[index].clone(), panel.surfaces.len());
            if self.right_panel.revealed_tab.as_ref() != Some(&key) {
                self.right_panel.tabs_scroll.scroll_to_item(index);
                // A strip shown for the first time has no viewport to scroll within until it lays out.
                if self.right_panel.tabs_scroll.bounds().size.width > px(0.) {
                    self.right_panel.revealed_tab = Some(key);
                } else {
                    window.request_animation_frame();
                }
            }
        }
        let content = match (&panel.active, self.target.clone()) {
            (Some(Surface::Diff), _) => self.render_diff_surface(&panel.scope, &tokens, cx),
            (Some(Surface::Browser(tab)), Some(Target::Thread(thread_id))) => {
                self.render_browser_surface(&thread_id, tab, width, window, cx)
            }
            (Some(Surface::SideChat(thread_id)), _) => self.render_side_chat_surface(thread_id, width, window, cx),
            _ => self.render_launcher(&tokens, cx),
        };
        let handle = div()
            .id("right-panel-resize")
            .group("right-panel-resize")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-4.))
            .w(px(8.))
            .cursor_col_resize()
            .on_drag(RightPanelResize, |_, _, _, cx| cx.new(|_| Empty))
            .on_click(cx.listener(|this, event: &gpui_kit::ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.right_panel.width = DEFAULT_WIDTH;
                    cx.notify();
                }
            }))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(3.))
                    .w(px(2.))
                    .invisible()
                    .group_hover("right-panel-resize", |s| s.visible())
                    .bg(tokens.border),
            );
        Some(
            v_flex()
                .id("right-panel")
                .relative()
                .w(px(width))
                .h_full()
                .flex_none()
                .bg(tokens.background)
                .border_l_1()
                .border_color(tokens.border)
                .on_drag_move(cx.listener(|this, event: &DragMoveEvent<RightPanelResize>, window, cx| {
                    let viewport = f32::from(window.viewport_size().width);
                    let right = f32::from(event.bounds.right());
                    this.right_panel.width = (right - f32::from(event.event.position.x)).max(MIN_WIDTH);
                    let sidebar = if this.sidebar_open { f32::from(this.sidebar_width()) } else { 0. };
                    this.right_panel.width = clamp_width(this.right_panel.width, viewport, viewport - sidebar);
                    cx.notify();
                }))
                .child(self.render_tab_bar(&panel, &tokens, cx))
                .child(div().flex_1().min_h_0().flex().flex_col().child(content))
                .child(deferred(handle).with_priority(1))
                .into_any_element(),
        )
    }

    /// The 40px tab strip, a drag region clear of the panel toggles and caption buttons.
    fn render_tab_bar(&self, panel: &PanelState, tokens: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        // `wco:pr-(--workspace-native-controls-inset)` plus the `w-24` spacer for the toggles.
        let reserve = CAPTION_BUTTONS_WIDTH + px(12.) - RIGHT_RESIZE_INSET + px(96.);
        let view = cx.entity();
        let (diff, browser) = (self.diff_available(), self.browser_available());
        let side_chat = self.side_chats_offered().then(|| self.side_chat_disabled_reason().is_none());
        let tabs: Vec<AnyElement> = panel
            .surfaces
            .iter()
            .map(|surface| self.render_surface_tab(surface, panel.active.as_ref() == Some(surface), tokens, cx))
            .collect();
        h_flex()
            .id("right-panel-tabbar")
            .h(TITLEBAR_HEIGHT)
            .flex_none()
            .min_w_0()
            .pl_2()
            .pr(reserve)
            .gap_1()
            .window_control_area(WindowControlArea::Drag)
            .child(
                h_flex()
                    .id("right-panel-tabs")
                    .min_w_0()
                    .h_full()
                    .gap_1()
                    .overflow_x_scroll()
                    .track_scroll(&self.right_panel.tabs_scroll)
                    .children(tabs),
            )
            .child(
                Button::new("right-panel-add")
                    .custom(
                        ButtonCustomVariant::new(cx)
                            .foreground(tokens.muted_foreground)
                            .hover(tokens.accent)
                            .active(tokens.accent),
                    )
                    .xsmall()
                    .icon(icon("icons/plus.svg"))
                    .flex_none()
                    .size(px(24.))
                    .rounded(px(8.))
                    .dropdown_menu(move |menu: PopupMenu, window, _| {
                        let menu = menu
                            .min_w(px(200.))
                            .item(
                                PopupMenuItem::new("Browser")
                                    .icon(Icon::new(IconName::Globe))
                                    .disabled(!browser)
                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                        this.add_browser_surface(window, cx)
                                    })),
                            )
                            .item(
                                PopupMenuItem::new("Diff").icon(icon("icons/file-diff.svg")).disabled(!diff).on_click(
                                    window.listener_for(&view, |this, _, window, cx| this.open_changes(window, cx)),
                                ),
                            );
                        match side_chat {
                            Some(available) => menu.item(
                                PopupMenuItem::new("Side chat")
                                    .icon(Icon::new(IconName::MessagesSquare))
                                    .disabled(!available)
                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                        this.add_side_chat_surface(window, cx)
                                    })),
                            ),
                            None => menu,
                        }
                    })
                    .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                        this.right_panel.add_menu_open = *open;
                        cx.notify();
                    })),
            )
    }

    /// A tab (`h-6 rounded-md pl-1.5 pr-2 text-xs`), its icon turning into a close button on hover.
    fn render_surface_tab(
        &self,
        surface: &Surface,
        active: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (key, title, glyph): (SharedString, SharedString, AnyElement) = match surface {
            Surface::Diff => {
                ("diff".into(), "Diff".into(), icon("icons/file-diff.svg").with_size(px(12.)).into_any_element())
            }
            Surface::Browser(tab) => (
                format!("browser:{tab}").into(),
                self.browser_tab_title(tab),
                Icon::new(IconName::Globe).size(px(12.)).into_any_element(),
            ),
            Surface::SideChat(thread_id) => {
                let (title, state) = self.side_chat_tab(thread_id);
                // Static dots: a side chat tab can stay open for a whole run.
                let dot = state.map(|state| {
                    div().absolute().top(px(-2.)).right(px(-2.)).size(px(6.)).rounded_full().bg(if state == "running" {
                        tokens.muted_foreground
                    } else {
                        tokens.info
                    })
                });
                let glyph = div()
                    .relative()
                    .child(Icon::new(IconName::MessagesSquare).size(px(12.)))
                    .children(dot)
                    .into_any_element();
                (format!("side-chat:{thread_id}").into(), title, glyph)
            }
        };
        let group: SharedString = format!("right-panel-tab:{key}").into();
        let tooltip: SharedString = format!("Close {title}").into();
        let (accent, foreground, muted) = (tokens.accent, tokens.foreground, tokens.muted);
        let (close, activate, middle) = (surface.clone(), surface.clone(), surface.clone());
        h_flex()
            .id(ElementId::Name(group.clone()))
            .group(group.clone())
            .flex_none()
            .h(px(24.))
            .max_w(px(144.))
            .gap(px(2.))
            .pl(px(6.))
            .pr_2()
            .rounded(px(6.))
            .occlude()
            .cursor_pointer()
            .text_xs()
            .map(|el| {
                if active {
                    el.bg(accent).text_color(foreground)
                } else {
                    el.text_color(tokens.muted_foreground)
                        .hover(move |style| style.bg(accent.opacity(0.6)).text_color(foreground))
                }
            })
            .child(
                div()
                    .id(ElementId::Name(format!("{group}:close").into()))
                    .relative()
                    .size(px(16.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .hover(move |style| style.bg(muted))
                    .child(div().group_hover(group.clone(), |style| style.invisible()).child(glyph))
                    .child(
                        div()
                            .absolute()
                            .invisible()
                            .group_hover(group.clone(), |style| style.visible())
                            .child(icon("icons/x.svg").with_size(px(12.))),
                    )
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_surface(close.clone(), window, cx);
                    })),
            )
            .child(div().min_w_0().truncate().child(title))
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| this.close_surface(middle.clone(), window, cx)),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.activate_surface(activate.clone(), window, cx)))
            .into_any_element()
    }

    /// `RightPanelEmptyState`: "Open a surface" with the surfaces this client has.
    fn render_launcher(&self, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let hover = tokens.accent.opacity(0.6);
        let row = |id: &'static str,
                   glyph: AnyElement,
                   label: &'static str,
                   key: &'static str,
                   available: bool,
                   hint: &'static str| {
            h_flex()
                .id(id)
                .h(px(32.))
                .w_full()
                .gap(px(10.))
                .px(px(10.))
                .rounded(px(8.))
                .text_sm()
                .child(glyph)
                .child(div().flex_1().min_w_0().truncate().child(label))
                .child(kbd(key, tokens))
                .map(|el| {
                    if available {
                        el.cursor_pointer().hover(move |style| style.bg(hover))
                    } else {
                        el.opacity(0.5).tooltip(move |window, cx| Tooltip::new(hint).build(window, cx))
                    }
                })
        };
        let browser_available = self.browser_available();
        let browser = row(
            "right-panel-open-browser",
            Icon::new(IconName::Globe).size(px(16.)).into_any_element(),
            "Browser",
            "B",
            browser_available,
            "Only available in a thread.",
        )
        .when(browser_available, |el| {
            el.on_click(cx.listener(|this, _, window, cx| this.add_browser_surface(window, cx)))
        });
        let diff_available = self.diff_available();
        let diff = row(
            "right-panel-open-diff",
            icon("icons/file-diff.svg").with_size(px(16.)).into_any_element(),
            "Diff",
            "D",
            diff_available,
            "Available for Git repositories.",
        )
        .when(diff_available, |el| el.on_click(cx.listener(|this, _, window, cx| this.open_changes(window, cx))));
        let side_chat = self.side_chats_offered().then(|| {
            let reason = self.side_chat_disabled_reason();
            row(
                "right-panel-open-side-chat",
                Icon::new(IconName::MessagesSquare).size(px(16.)).into_any_element(),
                "Side chat",
                "S",
                reason.is_none(),
                reason.unwrap_or("Available after the first message."),
            )
            .when(reason.is_none(), |el| {
                el.on_click(cx.listener(|this, _, window, cx| this.add_side_chat_surface(window, cx)))
            })
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .px_6()
            .pb(TITLEBAR_HEIGHT)
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(320.))
                    .py_6()
                    .child(
                        div()
                            .mb_3()
                            .text_center()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(tokens.foreground)
                            .child("Open a surface"),
                    )
                    .child(v_flex().gap(px(2.)).child(browser).child(diff).children(side_chat)),
            )
            .into_any_element()
    }

    /// `DiffPanel` embedded in the inline panel: the scope menu and view toggles, then the patch.
    fn render_diff_surface(&self, scope: &DiffScope, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let turns = self.open_projection().map(turn_summaries).unwrap_or_default();
        let latest = turns.first().map(|t| t.run_id.clone());
        let scope = match scope {
            DiffScope::Turn(run) if !turns.iter().any(|t| &t.run_id == run) => {
                latest.clone().map_or(DiffScope::Branch, DiffScope::Turn)
            }
            scope => scope.clone(),
        };
        let commits = self.right_panel.commits.clone();
        let scope_label: SharedString = match &scope {
            DiffScope::Branch => "Changes".into(),
            DiffScope::Unstaged => "Uncommitted".into(),
            DiffScope::Turn(run) if Some(run) == latest.as_ref() => "Latest turn".into(),
            DiffScope::Turn(run) => {
                format!("Turn {}", turns.iter().find(|t| &t.run_id == run).map_or(0, |t| t.turn_count)).into()
            }
            DiffScope::Commit(sha) => commits
                .iter()
                .find(|c| &c.sha == sha)
                .map(|c| c.subject.clone())
                .unwrap_or_else(|| format!("Commit {}", &sha[..sha.len().min(7)]))
                .into(),
        };
        let view = cx.entity();
        let menu_scope = scope.clone();
        let menu_turns = turns.clone();
        let menu_latest = latest.clone();
        let scope_menu = Button::new("diff-scope")
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(tokens.secondary)
                    .foreground(tokens.foreground)
                    .hover(tokens.secondary.opacity(0.9))
                    .active(tokens.secondary.opacity(0.8)),
            )
            .xsmall()
            .h(px(24.))
            .px(px(7.))
            .gap_1()
            .rounded(px(8.))
            .text_xs()
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .child(div().max_w(px(200.)).truncate().child(scope_label))
            .child(icon("icons/chevron-down.svg").with_size(px(14.)).opacity(0.7))
            .dropdown_menu(move |menu: PopupMenu, window, cx| {
                let pick = |label: &str, value: DiffScope, checked: bool, window: &mut Window| {
                    PopupMenuItem::new(label.to_owned()).checked(checked).on_click(
                        window.listener_for(&view, move |this, _, _, cx| this.set_diff_scope(value.clone(), cx)),
                    )
                };
                let latest_checked = matches!(&menu_scope, DiffScope::Turn(run) if Some(run) == menu_latest.as_ref());
                let mut menu = menu
                    .min_w(px(180.))
                    .item(pick("Changes", DiffScope::Branch, menu_scope == DiffScope::Branch, window))
                    .item(pick("Uncommitted", DiffScope::Unstaged, menu_scope == DiffScope::Unstaged, window));
                if let Some(latest) = &menu_latest {
                    menu = menu.item(pick("Latest turn", DiffScope::Turn(latest.clone()), latest_checked, window));
                }
                let (turns, scope, turn_view) = (menu_turns.clone(), menu_scope.clone(), view.clone());
                menu = menu.submenu("Turn", window, cx, move |mut menu, window, _| {
                    if turns.is_empty() {
                        return menu.item(PopupMenuItem::new("No completed turns yet.").disabled(true));
                    }
                    for turn in &turns {
                        let value = DiffScope::Turn(turn.run_id.clone());
                        let checked = scope == value;
                        let time = short_time(&turn.completed_at);
                        let label: SharedString = format!("Turn {}", turn.turn_count).into();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, cx| {
                                h_flex().w_full().gap_2().child(label.clone()).child(
                                    div()
                                        .ml_auto()
                                        .text_xs()
                                        .text_color(theme::tokens(cx).muted_foreground)
                                        .child(time.clone()),
                                )
                            })
                            .checked(checked)
                            .on_click(
                                window.listener_for(&turn_view, move |this, _, _, cx| {
                                    this.set_diff_scope(value.clone(), cx)
                                }),
                            ),
                        );
                    }
                    menu
                });
                let (commits, scope, commit_view) = (commits.clone(), menu_scope.clone(), view.clone());
                menu.submenu("Commits", window, cx, move |mut menu, window, _| {
                    if commits.is_empty() {
                        return menu.item(PopupMenuItem::new("No commits on this branch.").disabled(true));
                    }
                    for commit in &commits {
                        let value = DiffScope::Commit(commit.sha.clone());
                        let checked = scope == value;
                        menu = menu.item(PopupMenuItem::new(commit.subject.clone()).checked(checked).on_click(
                            window.listener_for(&commit_view, move |this, _, _, cx| {
                                this.set_diff_scope(value.clone(), cx)
                            }),
                        ));
                    }
                    menu.max_h(px(320.)).scrollable(true)
                })
            });
        let scheme = self.diff_color_scheme();
        let (refs, stats, truncated) = match &self.right_panel.load {
            DiffLoad::Ready { additions, deletions, head, base, truncated } => {
                let refs = matches!(scope, DiffScope::Branch).then(|| {
                    format!("{} → {}", head.as_deref().unwrap_or("HEAD"), base.as_deref().unwrap_or("Automatic"))
                });
                (refs, Some((*additions, *deletions)), *truncated)
            }
            _ => (None, None, false),
        };
        let can_refresh = !matches!(scope, DiffScope::Turn(_));
        let files: Vec<SharedString> = match self.right_panel.load {
            DiffLoad::Ready { .. } => {
                self.right_panel.diff.read(cx).files().iter().map(|file| file.path().clone()).collect()
            }
            _ => Vec::new(),
        };
        let all_collapsed =
            !files.is_empty() && files.iter().all(|path| self.right_panel.diff.read(cx).is_file_collapsed(path));
        let (wrap, ignore_whitespace) = (self.diff_wrap(), self.diff_ignore_whitespace());
        let header = h_flex()
            .h(px(28.))
            .mb_3()
            .px_2()
            .gap_2()
            .justify_between()
            .child(h_flex().flex_1().min_w_0().gap_3().child(scope_menu).children(
                refs.map(|refs| div().min_w_0().truncate().text_xs().text_color(tokens.muted_foreground).child(refs)),
            ))
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .children(stats.map(|(additions, deletions)| {
                        h_flex()
                            .mr_1()
                            .gap_1()
                            .font_family(tokens.fonts.mono)
                            .text_size(px(11.))
                            .child(div().text_color(git::addition_color(tokens, scheme)).child(format!("+{additions}")))
                            .child(div().text_color(git::deletion_color(tokens, scheme)).child(format!("-{deletions}")))
                    }))
                    .when(can_refresh, |el| {
                        el.child(
                            icon_toggle("diff-refresh", "icons/refresh-cw.svg", false, tokens, cx)
                                .tooltip(|window, cx| Tooltip::new("Refresh diff").build(window, cx))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.right_panel.refresh += 1;
                                    cx.notify();
                                })),
                        )
                    })
                    .when(!files.is_empty(), |el| {
                        let label = if all_collapsed { "Expand all files" } else { "Collapse all files" };
                        let path =
                            if all_collapsed { "icons/chevrons-up-down.svg" } else { "icons/chevrons-down-up.svg" };
                        el.child(
                            icon_toggle("diff-collapse", path, false, tokens, cx)
                                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let collapse = !all_collapsed;
                                    let files = files.clone();
                                    this.right_panel.diff.update(cx, |state, cx| {
                                        for path in &files {
                                            state.set_file_collapsed(path, collapse, cx);
                                        }
                                    });
                                    this.remember_collapsed(cx);
                                    cx.notify();
                                })),
                        )
                    })
                    .child(self.render_layout_toggle(tokens, cx))
                    .child({
                        let label = if wrap { "Disable line wrapping" } else { "Enable line wrapping" };
                        icon_toggle("diff-wrap", "icons/text-wrap.svg", wrap, tokens, cx)
                            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.right_panel.wrap = Some(!wrap);
                                cx.notify();
                            }))
                    })
                    .child({
                        let label =
                            if ignore_whitespace { "Show whitespace changes" } else { "Hide whitespace changes" };
                        icon_toggle("diff-whitespace", "icons/pilcrow.svg", ignore_whitespace, tokens, cx)
                            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.right_panel.ignore_whitespace = Some(!ignore_whitespace);
                                cx.notify();
                            }))
                    }),
            );
        let quiet = |text: SharedString| {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px_5()
                .text_center()
                .text_xs()
                .text_color(tokens.muted_foreground.opacity(0.7))
                .child(text)
        };
        let body = match &self.right_panel.load {
            DiffLoad::Idle | DiffLoad::Loading => quiet(
                match &scope {
                    DiffScope::Turn(_) => "Loading checkpoint diff...",
                    DiffScope::Unstaged => "Loading uncommitted changes...",
                    DiffScope::Commit(_) => "Loading commit...",
                    DiffScope::Branch => "Loading changes...",
                }
                .into(),
            )
            .into_any_element(),
            DiffLoad::Empty(text) => quiet((*text).into()).into_any_element(),
            DiffLoad::Failed(error) => div()
                .px_3()
                .text_size(px(11.))
                .text_color(tokens.error.opacity(0.8))
                .child(error.clone())
                .into_any_element(),
            DiffLoad::Ready { .. } => v_flex()
                .flex_1()
                .min_h_0()
                .when(truncated, |el| {
                    el.child(
                        div()
                            .px_3()
                            .py(px(6.))
                            .border_b_1()
                            .border_color(tokens.border)
                            .bg(tokens.muted)
                            .text_size(px(11.))
                            .text_color(tokens.muted_foreground)
                            .child("This preview exceeds the size limit. Changes shown are incomplete."),
                    )
                })
                .child({
                    // `--diffs-font-size`, the Code font size.
                    let diff = Diff::new(&self.right_panel.diff)
                        .soft_wrap(wrap)
                        .flex_1()
                        .min_h_0()
                        .text_size(px(tokens.fonts.code_size));
                    match scheme {
                        DiffColorScheme::RedGreen => diff.into_any_element(),
                        DiffColorScheme::BlueOrange => DiffTint {
                            child: diff.into_any_element(),
                            colors: (git::addition_color(tokens, scheme), git::deletion_color(tokens, scheme)),
                        }
                        .into_any_element(),
                    }
                })
                .into_any_element(),
        };
        v_flex().flex_1().min_h_0().child(header).child(body).into_any_element()
    }

    /// The stacked / split segmented toggle (`ToggleGroup variant="segmented"`), saved as `diffLayout`.
    fn render_layout_toggle(&self, tokens: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let split = self.diff_mode() == DiffMode::Split;
        let segment = |id: &'static str, path: &'static str, pressed: bool, value: bool, cx: &mut Context<Self>| {
            let pressed_bg = tokens.input.opacity(0.72);
            let hover = tokens.input.opacity(0.32);
            div()
                .id(id)
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(if pressed { tokens.foreground } else { tokens.muted_foreground })
                .when(pressed, |el| el.bg(pressed_bg))
                .when(!pressed, |el| el.hover(move |style| style.bg(hover)))
                .child(icon(path).with_size(px(14.)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let layout = if value { "split" } else { "stacked" };
                    this.client_settings.apply(json!({ "diffLayout": layout }).as_object().expect("an object"));
                    let mode = if value { DiffMode::Split } else { DiffMode::Unified };
                    this.right_panel.diff.update(cx, |state, cx| state.set_mode(mode, cx));
                    cx.notify();
                }))
        };
        h_flex()
            .p(px(2.))
            .gap(px(2.))
            .rounded(px(8.))
            .bg(tokens.input.opacity(0.4))
            .child(segment("diff-stacked", "icons/rows-3.svg", !split, false, cx))
            .child(segment("diff-split", "icons/columns-2.svg", split, true, cx))
    }
}

/// The files a selection shows folded: the user's folds once they changed any, else every
/// file or none by `diffFilesCollapsed`.
fn collapsed_files(
    remembered: Option<&HashSet<SharedString>>,
    collapse_by_default: bool,
    paths: &[SharedString],
) -> HashSet<SharedString> {
    match remembered {
        Some(folded) => folded.clone(),
        None if collapse_by_default => paths.iter().cloned().collect(),
        None => HashSet::new(),
    }
}

/// `deriveWorkspaceMutationId`: the last settled item that may have written files, and the
/// newest turn's completion.
fn workspace_mutation_id(
    projection: &ThreadProjection,
    latest_turn_completed_at: Option<&str>,
) -> (Option<String>, Option<String>) {
    let item = projection.visible_turn_items.iter().rev().map(|row| &row.item).find(|item| {
        matches!(item.item_type.as_str(), "command_execution" | "file_change" | "dynamic_tool" | "subagent")
            && !matches!(item.status.as_str(), "pending" | "running" | "waiting")
    });
    (item.map(|item| item.id.clone()), latest_turn_completed_at.map(str::to_owned))
}

/// What the proactive panel last saw of the open thread (`ProactivePanelObservation`).
#[derive(Clone, Debug, PartialEq)]
struct Observation {
    thread_id: String,
    /// The running turn at the last read; `None` before the first, when a just-completed turn still opens.
    running: Option<Option<String>>,
    /// The turn the user's last panel choice belongs to.
    user_turn: Option<String>,
    /// The panel revision a proactive open must still find.
    revision: u64,
}

/// `observeProactivePanelUserChoice`: a panel choice made since the thread opened or its
/// current turn started outranks a proactive open.
fn observe(previous: Option<&Observation>, thread_id: &str, running: Option<&str>, revision: u64) -> Observation {
    let same = previous.filter(|previous| previous.thread_id == thread_id);
    let new_turn = same.is_some_and(|previous| running.is_some() && running != previous.user_turn.as_deref());
    Observation {
        thread_id: thread_id.to_owned(),
        running: same.and_then(|previous| previous.running.clone()),
        user_turn: running.map(str::to_owned).or_else(|| same.and_then(|previous| previous.user_turn.clone())),
        revision: match same {
            Some(previous) if !new_turn => previous.revision,
            _ => revision,
        },
    }
}

/// `shouldOpenProactiveTurnDiff`: the settled turn is the one that was running at the last read.
fn turn_just_completed(previous: Option<Option<&str>>, running: Option<&str>, settled: &str, completed: bool) -> bool {
    running.is_none() && completed && previous.is_none_or(|previous| previous == Some(settled))
}

#[derive(Debug, PartialEq)]
enum DiffOffer {
    /// Its checkpoint or the repository status has not loaded yet.
    Defer,
    Ignore,
    Open,
}

/// `resolveProactiveTurnDiffAction`: only a turn with enough changes is worth opening.
fn turn_diff_offer(checkpoint: Option<&Checkpoint>, is_repo: Option<bool>) -> DiffOffer {
    let (Some(checkpoint), Some(is_repo)) = (checkpoint.filter(|c| c.status != "missing"), is_repo) else {
        return DiffOffer::Defer;
    };
    if !is_repo || checkpoint.status != "ready" || checkpoint.files.is_empty() {
        return DiffOffer::Ignore;
    }
    let totals = checkpoint.file_totals();
    if totals.count >= 3 || totals.additions + totals.deletions >= 50 { DiffOffer::Open } else { DiffOffer::Ignore }
}

/// The blue-orange scheme's line colors while the diff lays out and paints: the component
/// reads added and removed lines from the global theme's success and danger colors.
struct DiffTint {
    child: AnyElement,
    colors: (Hsla, Hsla),
}

impl DiffTint {
    fn tinted<R>(colors: (Hsla, Hsla), cx: &mut App, f: impl FnOnce(&mut App) -> R) -> R {
        let theme = Theme::global_mut(cx);
        let saved = (theme.success, theme.danger);
        (theme.success, theme.danger) = colors;
        let result = f(cx);
        let theme = Theme::global_mut(cx);
        (theme.success, theme.danger) = saved;
        result
    }
}

impl IntoElement for DiffTint {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for DiffTint {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = &mut self.child;
        (Self::tinted(self.colors, cx, |cx| child.request_layout(window, cx)), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        Self::tinted(self.colors, cx, |cx| child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        Self::tinted(self.colors, cx, |cx| child.paint(window, cx));
    }
}

/// `{sources, ...}` from `review.getDiffPreview` or `{diff}` from `getTurnDiff`, parsed.
struct ParsedDiff {
    files: Vec<DiffFile>,
    head: Option<String>,
    base: Option<String>,
    truncated: bool,
}

fn parse_diff(value: &Value, source_kind: Option<&str>) -> Option<ParsedDiff> {
    let (patch, head, base, truncated) = match source_kind {
        Some(kind) => {
            let source = value["sources"].as_array()?.iter().find(|s| s["kind"].as_str() == Some(kind))?;
            let text = |key: &str| source[key].as_str().map(str::to_owned);
            (source["diff"].as_str()?, text("headRef"), text("baseRef"), source["truncated"].as_bool() == Some(true))
        }
        None => (value["diff"].as_str()?, None, None, false),
    };
    let files = DiffFile::parse(patch).unwrap_or_else(|error| {
        log::warn!("unparseable diff: {error}");
        Vec::new()
    });
    Some(ParsedDiff { files, head, base, truncated })
}

/// `HH:MM` from an ISO timestamp, as the turn menu labels completion.
fn short_time(iso: &str) -> SharedString {
    iso.get(11..16).unwrap_or(iso).to_owned().into()
}

/// A 28px ghost icon toggle (`Toggle variant="ghost" size="sm"` / `Button size="icon-sm"`).
fn icon_toggle(
    id: &'static str,
    path: &'static str,
    pressed: bool,
    tokens: &Tokens,
    _: &mut Context<Workspace>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let accent = tokens.accent;
    div()
        .id(id)
        .size(px(28.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .text_color(tokens.foreground)
        .when(pressed, |el| el.bg(accent))
        .hover(move |style| style.bg(accent))
        .child(icon(path).with_size(px(14.)))
}

/// `Kbd`: 20px tall, 4px radius, `bg-foreground/7%`, medium muted 12px.
fn kbd(text: &'static str, tokens: &Tokens) -> impl IntoElement {
    div()
        .h(px(20.))
        .min_w(px(20.))
        .px_1()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(tokens.foreground.opacity(0.07))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(tokens.muted_foreground)
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ThreadState;

    #[test]
    fn width_clamps_to_the_viewport_and_the_chat_column() {
        assert_eq!(clamp_width(540., 1600., 1300.), 540.);
        // 70% of a 1000px window is 700, but the chat keeps 360 of a 800px row.
        assert_eq!(clamp_width(900., 1000., 800.), 440.);
        assert_eq!(clamp_width(100., 1600., 1300.), MIN_WIDTH);
        // Too narrow for both: the panel keeps its minimum.
        assert_eq!(clamp_width(540., 600., 500.), MIN_WIDTH);
    }

    #[test]
    fn closing_a_tab_focuses_its_neighbour_and_the_last_one_hides_the_panel() {
        let browser = |tab: &str| Surface::Browser(tab.into());
        let mut panel = PanelState::default();
        panel.upsert(Surface::Diff);
        panel.upsert(browser("a"));
        panel.upsert(browser("b"));
        panel.upsert(browser("a"));
        assert_eq!(panel.surfaces, [Surface::Diff, browser("a"), browser("b")]);
        panel.close(&browser("a"));
        assert_eq!(panel.active, Some(browser("b")));
        panel.close(&Surface::Diff);
        panel.close(&browser("b"));
        assert!(!panel.open && panel.active.is_none());
    }

    #[test]
    fn browser_tabs_follow_the_preview_sessions() {
        let browser = |tab: &str| Surface::Browser(tab.into());
        let mut panel = PanelState::default();
        panel.upsert(Surface::Diff);
        panel.upsert(browser("gone"));
        panel.reconcile_browsers(&["kept", "new"]);
        assert_eq!(panel.surfaces, [Surface::Diff, browser("kept"), browser("new")]);
        assert_eq!(panel.active, Some(browser("kept")), "a closed active tab falls back to a browser");
        assert!(panel.open);
        let mut only_browser = PanelState::default();
        only_browser.upsert(browser("a"));
        only_browser.reconcile_browsers(&[]);
        assert!(!only_browser.open, "losing the last session hides the panel");
    }

    #[test]
    fn turns_come_newest_first_and_skip_base_snapshots() {
        let mut state = ThreadState::default();
        let checkpoint = |id: &str, run: Option<&str>, ordinal: Option<u32>, at: &str| {
            json!({"id": id, "threadId": "t", "scopeId": "s", "runId": run, "nodeId": "n", "parentCheckpointId": null,
                "ordinalWithinScope": 0, "appRunOrdinal": ordinal, "ref": "r", "status": "ready", "files": [], "capturedAt": at})
        };
        state
            .apply(&json!({"kind": "snapshot", "snapshotSequence": 1, "projection": {
                "thread": {"id": "t", "title": "T"},
                "checkpoints": [
                    checkpoint("c0", None, None, "2026-01-01T00:00:00Z"),
                    checkpoint("c1", Some("r1"), Some(1), "2026-01-01T00:01:00Z"),
                    checkpoint("c2", Some("r2"), Some(2), "2026-01-01T00:02:00Z"),
                ],
            }}))
            .unwrap();
        let turns = turn_summaries(state.projection.as_ref().unwrap());
        assert_eq!(turns.iter().map(|t| t.turn_count).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(short_time(&turns[0].completed_at), "00:02");
    }

    #[test]
    fn rollback_targets_the_ready_checkpoint_closing_the_turn() {
        let mut state = ThreadState::default();
        let checkpoint = |id: &str, ordinal: u32, run: Option<u32>, status: &str| {
            json!({"id": id, "threadId": "t", "scopeId": "s", "runId": run.map(|r| format!("r{r}")), "nodeId": "n",
                "parentCheckpointId": null, "ordinalWithinScope": ordinal, "appRunOrdinal": run, "ref": "r",
                "status": status, "files": [], "capturedAt": "2026-01-01T00:00:00Z"})
        };
        state
            .apply(&json!({"kind": "snapshot", "snapshotSequence": 1, "projection": {
                "thread": {"id": "t", "title": "T"},
                "checkpoints": [checkpoint("c0", 0, None, "ready"), checkpoint("c1", 1, Some(1), "ready"), checkpoint("c2", 2, Some(2), "missing")],
            }}))
            .unwrap();
        let projection = state.projection.as_ref().unwrap();
        let command = rollback_command(projection, 1, true).unwrap();
        assert_eq!((command["checkpointId"].as_str(), command["scopeId"].as_str()), (Some("c1"), Some("s")));
        assert_eq!(command["restoreFiles"], true);
        assert_eq!(rollback_command(projection, 0, false).unwrap()["checkpointId"], "c0");
        assert!(rollback_command(projection, 2, false).is_none(), "a missing checkpoint cannot be restored");
    }

    #[test]
    fn folds_follow_the_default_until_the_user_folds() {
        let paths: Vec<SharedString> = vec!["a.rs".into(), "b.rs".into()];
        assert_eq!(collapsed_files(None, true, &paths), paths.iter().cloned().collect());
        assert!(collapsed_files(None, false, &paths).is_empty());
        let folded: HashSet<SharedString> = ["a.rs".into()].into();
        assert_eq!(collapsed_files(Some(&folded), true, &paths), folded, "a new file in a folded selection opens");
    }

    #[test]
    fn workspace_mutations_skip_unsettled_and_read_only_items() {
        let item = |id: &str, kind: &str, status: &str| {
            let item = json!({"id": id, "type": kind, "threadId": "t", "ordinal": 1, "status": status});
            json!({"visibility": "visible", "sourceThreadId": "t", "sourceItemId": id, "item": item})
        };
        let projection: ThreadProjection = serde_json::from_value(json!({
            "thread": {"id": "t", "title": "T"},
            "visibleTurnItems": [
                item("edit", "file_change", "completed"),
                item("cmd", "command_execution", "running"),
                item("msg", "assistant_message", "completed"),
            ],
        }))
        .unwrap();
        assert_eq!(workspace_mutation_id(&projection, Some("12:00")), (Some("edit".into()), Some("12:00".into())));
    }

    #[test]
    fn a_completed_turn_opens_its_diff_once_unless_the_user_chose_a_panel() {
        // The turn runs, then settles: the diff opens for a big enough checkpoint.
        let running = observe(None, "t", Some("r1"), 0);
        assert_eq!((running.revision, running.user_turn.as_deref()), (0, Some("r1")));
        let previous = Some(Some("r1"));
        assert!(turn_just_completed(previous, None, "r1", true));
        assert!(!turn_just_completed(previous, None, "r1", false), "failed or interrupted");
        assert!(!turn_just_completed(Some(None), None, "r1", true), "it had settled before");
        assert!(turn_just_completed(None, None, "r1", true), "first read of the thread");

        let checkpoint = |status: &str, files: usize, lines: u64| {
            let file = json!({"path": "a", "kind": "modified", "additions": lines, "deletions": 0});
            serde_json::from_value::<Checkpoint>(json!({"id": "c", "scopeId": "s", "status": status,
                "files": vec![file; files]}))
            .unwrap()
        };
        assert_eq!(turn_diff_offer(Some(&checkpoint("ready", 3, 1)), Some(true)), DiffOffer::Open);
        assert_eq!(turn_diff_offer(Some(&checkpoint("ready", 1, 50)), Some(true)), DiffOffer::Open);
        assert_eq!(turn_diff_offer(Some(&checkpoint("ready", 2, 10)), Some(true)), DiffOffer::Ignore);
        assert_eq!(turn_diff_offer(Some(&checkpoint("ready", 3, 1)), Some(false)), DiffOffer::Ignore);
        assert_eq!(turn_diff_offer(Some(&checkpoint("missing", 3, 1)), Some(true)), DiffOffer::Defer);
        assert_eq!(turn_diff_offer(Some(&checkpoint("ready", 3, 1)), None), DiffOffer::Defer);

        let mut panel = PanelState::default();
        assert!(panel.open_proactive_diff("r1", 0));
        assert!(panel.open && panel.active == Some(Surface::Diff));
        panel.open = false;
        assert!(!panel.open_proactive_diff("r1", 0), "each turn is offered once");
        panel.user_revision = 1;
        assert!(!panel.open_proactive_diff("r2", 0), "the user chose a panel during the turn");
        assert!(!panel.open && panel.proactive_diff_run.as_deref() == Some("r2"));

        // A choice before the next turn starts is superseded once it runs.
        let next = observe(Some(&running), "t", Some("r3"), 1);
        assert_eq!(next.revision, 1);
        assert_eq!(observe(Some(&next), "t", None, 2).revision, 1, "choices during the turn still count");
    }

    #[test]
    fn preview_sources_pick_the_requested_kind() {
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let value = json!({"cwd": "c", "sources": [
            {"kind": "working-tree", "diff": "", "headRef": null, "baseRef": null, "truncated": false},
            {"kind": "branch-range", "diff": patch, "headRef": "feat", "baseRef": "origin/main", "truncated": true},
        ]});
        let parsed = parse_diff(&value, Some("branch-range")).unwrap();
        assert_eq!(parsed.files.len(), 1);
        assert_eq!((parsed.files[0].additions(), parsed.files[0].deletions()), (1, 1));
        assert_eq!(parsed.base.as_deref(), Some("origin/main"));
        assert!(parsed.truncated);
        assert!(parse_diff(&value, Some("working-tree")).unwrap().files.is_empty());
        assert!(parse_diff(&value, Some("commit")).is_none());
    }
}
