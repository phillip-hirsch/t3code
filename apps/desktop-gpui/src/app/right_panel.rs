//! The right panel (`RightPanelTabs.tsx` inside `PreviewPanelShell`, state as `rightPanelStore.ts`)
//! with its Diff (`DiffPanel.tsx`) and Browser (`preview/`) surfaces. The panel owns the titlebar
//! row above it, keeps per-thread open state, tabs and width across launches (`stored`) with the
//! tabs it closed (`closed`), and resizes from its left edge.
//! Other surfaces render in their own modules (files, terminal, pull requests, side chats); the
//! device surface is not native, so the launcher leaves it out.

mod closed;
mod commit_link;
mod diff_base;
mod diff_comments;
mod diff_load;
mod diff_open;
mod diff_tool_call;
pub(super) mod diff_tree;
mod sheet;
mod stored;
mod writer;

pub(in crate::app) use diff_load::contents_loader;

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::diff::{
    Diff, DiffChangeIndicator, DiffEvent, DiffFile, DiffHunkSeparator, DiffMode, DiffState,
};
use gpui_kit::component::menu::{DropdownMenu as _, MenuMetrics, PopupMenu, PopupMenuItem};
use gpui_kit::component::theme::Theme;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, DismissEvent, DragMoveEvent, Element, ElementId, Empty, Entity,
    FocusHandle, Focusable as _, GlobalElementId, Hsla, InspectorElementId, InteractiveElement as _, IntoElement,
    KeyDownEvent, LayoutId, MouseButton, ParentElement as _, Pixels, Point, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, deferred, div, px,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::git::{self, DiffColorScheme};
use super::titlebar::{CAPTION_BUTTONS_WIDTH, RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT, icon};
use super::{Target, Workspace};
use crate::model::{Checkpoint, ThreadProjection};
use crate::preview::ThreadRef;
use crate::rpc::RpcClient;
use crate::theme::{self, Tokens};

const DEFAULT_WIDTH: f32 = 540.;
/// A diff read this recently counts as fresh when the panel shows (`LIVE_REFRESH_MIN_INTERVAL_MS`).
const REVALIDATE_FLOOR: std::time::Duration = std::time::Duration::from_secs(10);
const MIN_WIDTH: f32 = 360.;
const MAX_WIDTH_FRACTION: f32 = 0.7;
/// The chat column keeps at least this much beside the panel.
const SIBLING_MIN_WIDTH: f32 = 360.;

/// The row the panel and the chat column share: the window less the docked sidebar and the
/// right resize inset `#root` keeps free (`getPreviewPanelMaxWidth`'s `containerWidth`).
fn panel_row(viewport: f32, sidebar: f32) -> f32 {
    viewport - sidebar - f32::from(RIGHT_RESIZE_INSET)
}

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
    /// A tool call's recorded file change, by the thread and item it was recorded in.
    ToolCall {
        thread_id: String,
        item_id: String,
    },
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

/// One tab of the panel (`RightPanelSurface`): the Diff, one browser tab by its preview tab id,
/// the Files tree, or a file (or folder) by its workspace path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub(super) enum Surface {
    Diff,
    Browser(String),
    Files,
    File(String),
    /// A file sent with a message (`attachment:<id>`), which needs no workspace.
    Attachment(super::files::AttachmentFile),
    /// A side chat's tab, by its thread id.
    SideChat(String),
    /// A pull request in the detail panel.
    PullRequest(crate::pull_request_detail::Reference),
    /// The thread's linked pull requests.
    PullRequests,
    /// A group of the thread's terminals, by its surface id.
    Terminal(String),
}

/// A target's panel (`ThreadRightPanelState`).
#[derive(Clone, Default)]
struct PanelState {
    open: bool,
    surfaces: Vec<Surface>,
    active: Option<Surface>,
    scope: DiffScope,
    /// The file a chat link asked the turn diff to show (`filePath`): it stays visible while
    /// test files hide, until another selection is picked.
    turn_file: Option<SharedString>,
    /// Bumped by every panel change the user makes (`userActionRevisionByThreadKey`).
    user_revision: u64,
    /// The turn whose diff was last offered on its own (`proactiveDiffTurnId`).
    proactive_diff_run: Option<String>,
    /// The panel fills the row and the chat column steps aside (`maximized`), kept per thread.
    maximized: bool,
    /// The width the user dragged it to (`t3code:preview-panel-width:<thread>`); none is the default.
    width: Option<f32>,
}

impl PanelState {
    /// `openProactive` for a turn's diff: offered once per turn, and refused after a panel
    /// choice made since `revision` or while the diff already shows.
    fn open_proactive_diff(&mut self, run: &str, revision: u64) -> bool {
        if self.proactive_diff_run.as_deref() == Some(run) {
            return false;
        }
        self.proactive_diff_run = Some(run.to_owned());
        let shown = self.active.as_ref().filter(|_| self.open);
        let pull_request = matches!(shown, Some(Surface::PullRequest(_) | Surface::PullRequests));
        if self.user_revision != revision || shown == Some(&Surface::Diff) || pull_request {
            return false;
        }
        self.upsert(Surface::Diff);
        self.scope = DiffScope::Branch;
        true
    }

    /// `openProactive` for a pull request surface: refused after a panel choice made since
    /// `revision`, and an existing tab stays where the user left it unless `activate_existing`.
    fn open_proactive(&mut self, surface: Surface, revision: u64, activate_existing: bool) -> bool {
        if self.user_revision != revision || (!activate_existing && self.surfaces.contains(&surface)) {
            return false;
        }
        self.upsert(surface);
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

    /// `moveSurface`: a dragged tab takes `to`'s place, the tabs between shifting over.
    fn move_surface(&mut self, surface: &Surface, to: &Surface) {
        let (Some(from), Some(to)) =
            (self.surfaces.iter().position(|s| s == surface), self.surfaces.iter().position(|s| s == to))
        else {
            return;
        };
        let moved = self.surfaces.remove(from);
        self.surfaces.insert(to, moved);
    }

    /// `openFile`: a file's tab replaces the standalone Files tree and keeps its place when open.
    fn open_file(&mut self, path: String) {
        self.surfaces.retain(|s| *s != Surface::Files);
        let surface = Surface::File(path);
        if !self.surfaces.contains(&surface) {
            self.surfaces.push(surface.clone());
        }
        self.open = true;
        self.active = Some(surface);
    }

    /// `openAttachment`: like a file, an attachment's tab replaces the standalone Files tree.
    fn open_attachment(&mut self, file: super::files::AttachmentFile) {
        self.surfaces.retain(|s| *s != Surface::Files);
        self.upsert(Surface::Attachment(file));
    }

    /// `navigateFolder`: `source` turns into the folder (the root is the Files tree), or with
    /// `keep_source` the folder opens beside it. A folder already open is focused instead.
    fn navigate_folder(&mut self, source: &Surface, folder: &str, keep_source: bool) {
        let Some(index) = self.surfaces.iter().position(|s| s == source) else { return };
        let target = if folder.is_empty() { Surface::Files } else { Surface::File(folder.to_owned()) };
        let existing = self.surfaces.iter().enumerate().any(|(i, s)| i != index && *s == target);
        let mut surfaces = Vec::new();
        for (i, surface) in self.surfaces.drain(..).enumerate() {
            if i == index {
                if keep_source {
                    surfaces.push(surface);
                }
                if !existing {
                    surfaces.push(target.clone());
                }
            } else if existing || !(matches!(target, Surface::File(_)) && surface == Surface::Files) {
                surfaces.push(surface);
            }
        }
        self.surfaces = surfaces;
        self.open = true;
        self.active = Some(target);
    }

    /// `reconcileBrowserSurfaces`: a tab per preview session, dropping closed ones. Closing the
    /// last session hides the panel rather than leave it on the launcher. Returns whether
    /// anything changed.
    fn reconcile_browsers(&mut self, tab_ids: &[&str]) -> bool {
        let (count, open) = (self.surfaces.len(), self.open);
        let had_browser = self.surfaces.iter().any(|s| matches!(s, Surface::Browser(_)));
        self.surfaces.retain(|s| !matches!(s, Surface::Browser(tab) if !tab_ids.contains(&tab.as_str())));
        let mut changed = self.surfaces.len() != count;
        for tab in tab_ids {
            if !self.surfaces.iter().any(|s| matches!(s, Surface::Browser(id) if id == tab)) {
                self.surfaces.push(Surface::Browser((*tab).to_owned()));
                changed = true;
            }
        }
        // Nothing active, as after a restart, also falls back to a browser tab.
        if self.active.as_ref().is_none_or(|active| !self.surfaces.contains(active)) {
            let browser = self.surfaces.iter().find(|s| matches!(s, Surface::Browser(_)));
            let active = browser.or(self.surfaces.first()).cloned();
            changed |= active != self.active;
            self.active = active;
        }
        if self.surfaces.is_empty() && had_browser {
            self.open = false;
        }
        changed || open != self.open
    }
}

impl DiffKey {
    /// The same files asked for again: only the reads' revisions differ.
    fn same_selection(&self, other: &Self) -> bool {
        (&self.target, &self.cwd, &self.scope, &self.base_ref, self.ignore_whitespace)
            == (&other.target, &other.cwd, &other.scope, &other.base_ref, other.ignore_whitespace)
    }
}

#[derive(Clone, PartialEq)]
struct DiffKey {
    target: Target,
    cwd: Option<String>,
    scope: DiffScope,
    base_ref: Option<String>,
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
    Ready {
        additions: usize,
        deletions: usize,
        head: Option<String>,
        base: Option<String>,
        truncated: bool,
        /// Patch reads asked for that have not settled.
        pending: usize,
        /// A tool call's recorded files and how many its patch leaves out.
        omitted: Option<(usize, usize)>,
    },
    /// A tool call's patch the viewer can't read, as text with why.
    Raw {
        reason: &'static str,
        text: SharedString,
        truncated: bool,
    },
    /// File contents a tool call recorded without a diff, each under its label.
    Recorded {
        content: std::rc::Rc<[(&'static str, SharedString)]>,
        truncated: bool,
    },
    Empty(&'static str),
    Failed(SharedString),
}

#[derive(Clone)]
struct Commit {
    sha: String,
    subject: String,
    committed_at: Option<String>,
}

/// How [`Workspace::show_diff_files`] puts files in the patch.
#[derive(Clone, Copy)]
enum Install {
    /// A new read: every file starts fresh.
    Replace,
    /// More of the shown selection's files settled: the files already shown keep their
    /// selection, expanded context, scroll and prepared highlighting.
    Update,
}

/// A selection's files whose patches load one by one (`useReviewFilePatches`): each metadata
/// row's files once settled, in path order, and what reads one again. Dropping it cancels its
/// retries.
struct FilePatches {
    selection: (Target, DiffScope),
    client: RpcClient,
    request: diff_load::PatchRequest,
    files: Vec<diff_load::FileStat>,
    /// `files` by path, for the headers' counts.
    stats: std::rc::Rc<HashMap<SharedString, diff_load::FileStat>>,
    /// Each file's index in `files`, by path.
    index: HashMap<SharedString, usize>,
    /// The header-only row each file shows until its patch settles (the `:pending` rows).
    placeholders: Vec<Vec<DiffFile>>,
    rows: Vec<Option<(Vec<DiffFile>, diff_load::FileState)>>,
    demand: diff_load::PatchDemand,
    _reads: Vec<Task<()>>,
}

impl FilePatches {
    fn new(
        selection: (Target, DiffScope),
        client: RpcClient,
        request: diff_load::PatchRequest,
        files: Vec<diff_load::FileStat>,
        previous: Option<&Self>,
    ) -> Self {
        let stats = files.iter().map(|file| (file.path.clone().into(), file.clone())).collect();
        let index = files.iter().enumerate().map(|(index, file)| (file.path.clone().into(), index)).collect();
        // A refresh of the same selection keeps the patches it read, each read again behind them.
        let rows = previous.map_or_else(
            || vec![None; files.len()],
            |previous| diff_load::carried_rows(&previous.files, &previous.rows, &files),
        );
        let mut demand = diff_load::PatchDemand::new(files.len());
        demand.request(rows.iter().enumerate().filter_map(|(index, row)| row.as_ref().map(|_| index)));
        Self {
            selection,
            client,
            request,
            stats: std::rc::Rc::new(stats),
            index,
            placeholders: files.iter().map(diff_load::placeholder).collect(),
            rows,
            demand,
            files,
            _reads: Vec::new(),
        }
    }
}

pub(super) struct RightPanel {
    panels: HashMap<Target, PanelState>,
    /// Saves `panels` across launches; see [`RightPanel::panels_mut`].
    layout: stored::LayoutStore,
    /// Closed tabs for Reopen Closed Tab.
    closed: closed::ClosedViews,
    /// The "+" menu is open; a native browser view must not cover it. It also opens the menu,
    /// as `rightPanel.new` does.
    pub(super) add_menu_open: bool,
    /// The launcher's "Open browser in a profile" menu is open.
    pub(super) profiles_menu_open: bool,
    /// The launcher (`RightPanelEmptyState`), one Tab stop, and the row its arrows reached
    /// among the available surfaces.
    launcher_focus: FocusHandle,
    launcher_highlight: Option<usize>,
    /// A tab's right-click menu, at its window position.
    tab_menu: Option<(Entity<PopupMenu>, Point<Pixels>, Subscription)>,
    diff: Entity<DiffState>,
    loaded: Option<DiffKey>,
    load: DiffLoad,
    _task: Option<Task<()>>,
    /// The branch's commits for the scope menu, read beside the diff (`branchCommitsQuery`).
    _commits_task: Option<Task<()>>,
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
    /// When the last diff read landed, and whether one is on its way (`dataUpdatedAt`, `isPending`).
    diff_read_at: Option<std::time::Instant>,
    diff_reading: bool,
    /// The git diff showed at the last render, to revalidate it when it shows again.
    git_diff_shown: bool,
    commits: Vec<Commit>,
    /// The tab strip, and the tab it last scrolled into view.
    tabs_scroll: ScrollHandle,
    /// The diff header's refs area, measured for its narrow form (`@max-[20rem]/diff-refs`).
    diff_refs: ScrollHandle,
    /// A frame was already spent waiting for `diff_refs` to be measured; zero after that is a real width.
    diff_refs_waited: std::cell::Cell<bool>,
    revealed_tab: Option<(Target, Surface, usize)>,
    observation: Option<Observation>,
    /// The pull request each thread showed at the last read (`observedThreadPullRequests`).
    observed_pull_requests: HashMap<String, Option<crate::pull_request_detail::Reference>>,
    /// The branch diff's picked comparison target per thread; none is Automatic.
    base_refs: HashMap<Target, String>,
    base_picker: diff_base::BasePicker,
    /// Every file of the loaded selection; the patch shows them less hidden test files.
    all_diff_files: Vec<DiffFile>,
    /// How many of `all_diff_files` are tests or test support.
    diff_test_files: usize,
    /// The loaded selection's files whose patches load one by one, and the files among them
    /// that did not load in full, for their headers.
    patches: Option<FilePatches>,
    patch_states: std::rc::Rc<HashMap<SharedString, diff_load::FileState>>,
    hide_test_files: bool,
    tree_open: bool,
    tree: diff_tree::DiffTree,
    /// The file `open_turn_diff` asked for, scrolled to once the shown selection holds it.
    pending_reveal: Option<((Target, DiffScope), SharedString)>,
    toolbar_focus: DiffToolbarFocus,
    /// The line comment being written (`AnnotatableCodeView`'s draft).
    comment: Option<diff_comments::DiffComment>,
    /// The window width at the last render, for work that runs outside one.
    viewport_width: f32,
    /// The narrow-window sheet's focus: what had it when the sheet opened, the sheet's trap, and
    /// an opening whose focus move waits for the sheet's first frame.
    sheet_focus: super::sidebar_sheet::SheetFocus<FocusHandle>,
    sheet_trap: FocusHandle,
    sheet_entering: bool,
    _diff_events: Subscription,
}

impl RightPanel {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        let diff = cx.new(|cx| DiffState::new(Vec::new(), cx));
        let diff_events = cx.subscribe(&diff, |this, _, event: &DiffEvent, cx| match event {
            // The kit's chevron on a file still loading: unfold it and read it now.
            DiffEvent::FileExpanded(path) if this.pending_patch(path).is_some() => {
                this.fold_diff_file(path, false, cx);
            }
            DiffEvent::FileExpanded(_) | DiffEvent::FileCollapsed(_) => this.remember_collapsed(cx),
            _ => {}
        });
        let (layout, panels) = stored::LayoutStore::restored();
        Self {
            panels,
            layout,
            closed: closed::ClosedViews::restored(),
            add_menu_open: false,
            profiles_menu_open: false,
            launcher_focus: cx.focus_handle().tab_stop(true),
            launcher_highlight: None,
            tab_menu: None,
            diff,
            loaded: None,
            load: DiffLoad::Idle,
            _task: None,
            _commits_task: None,
            wrap: None,
            ignore_whitespace: None,
            diff_mount: None,
            collapsed: HashMap::new(),
            shown: None,
            refresh: 0,
            diff_read_at: None,
            diff_reading: false,
            git_diff_shown: false,
            commits: Vec::new(),
            tabs_scroll: ScrollHandle::new(),
            diff_refs: ScrollHandle::new(),
            diff_refs_waited: std::cell::Cell::new(false),
            revealed_tab: None,
            observation: None,
            observed_pull_requests: HashMap::new(),
            base_refs: HashMap::new(),
            base_picker: Default::default(),
            all_diff_files: Vec::new(),
            diff_test_files: 0,
            patches: None,
            patch_states: std::rc::Rc::default(),
            hide_test_files: false,
            tree_open: false,
            tree: diff_tree::DiffTree::new(cx),
            pending_reveal: None,
            toolbar_focus: DiffToolbarFocus::new(cx),
            comment: None,
            viewport_width: f32::INFINITY,
            sheet_focus: Default::default(),
            sheet_trap: cx.focus_handle(),
            sheet_entering: false,
            _diff_events: diff_events,
        }
    }

    /// The panels for a change, which the layout then saves.
    fn panels_mut(&mut self) -> &mut HashMap<Target, PanelState> {
        self.layout.changed = true;
        &mut self.panels
    }
}

/// The diff toolbar's Tab stops (`Button` and `Toggle`); the layout segments are one stop
/// between them, like the web's `ToggleGroup`.
struct DiffToolbarFocus {
    refresh: FocusHandle,
    collapse: FocusHandle,
    stacked: FocusHandle,
    split: FocusHandle,
    wrap: FocusHandle,
    whitespace: FocusHandle,
    tests: FocusHandle,
    tree: FocusHandle,
}

impl DiffToolbarFocus {
    fn new(cx: &mut App) -> Self {
        let stop = || cx.focus_handle().tab_stop(true);
        Self {
            refresh: stop(),
            collapse: stop(),
            stacked: stop(),
            split: stop(),
            wrap: stop(),
            whitespace: stop(),
            tests: stop(),
            tree: stop(),
        }
    }
}

/// Drag payload for the left-edge resize handle.
struct RightPanelResize;

/// A tab dragged along the strip (`SortableTabList`).
struct DraggedTab(Surface);

/// The pill that follows the pointer while a tab drags.
struct TabDragPreview {
    title: SharedString,
    colors: (Hsla, Hsla),
}

impl gpui_kit::Render for TabDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (background, foreground) = self.colors;
        div()
            .h(px(24.))
            .max_w(px(144.))
            .px_2()
            .flex()
            .items_center()
            .rounded(px(6.))
            .bg(background)
            .text_color(foreground)
            .text_xs()
            .child(div().min_w_0().truncate().child(self.title.clone()))
    }
}

impl Workspace {
    fn panel_state(&self) -> Option<&PanelState> {
        self.right_panel.panels.get(self.target.as_ref()?)
    }

    fn panel_state_mut(&mut self) -> Option<&mut PanelState> {
        let target = self.target.clone()?;
        Some(self.right_panel.panels_mut().entry(target).or_default())
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

    /// The open target's panel has `surface` among its tabs.
    pub(super) fn panel_has_surface(&self, surface: &Surface) -> bool {
        self.panel_state().is_some_and(|panel| panel.surfaces.contains(surface))
    }

    /// The surface on screen (`selectActiveRightPanelSurface`).
    pub(super) fn active_surface(&self) -> Option<&Surface> {
        self.panel_state().filter(|panel| panel.open && self.page.is_none())?.active.as_ref()
    }

    /// The panel's laid-out width while it is open; maximized, the whole row.
    pub(super) fn right_panel_width(&self, window: &Window) -> Option<f32> {
        if !self.right_panel_open() {
            return None;
        }
        let viewport = f32::from(window.viewport_size().width);
        if self.right_panel_sheet(window) {
            return Some(sheet::sheet_width(viewport));
        }
        let sidebar = if self.sidebar_open.docked() { f32::from(self.sidebar_width()) } else { 0. };
        if self.right_panel_maximized() {
            return Some(panel_row(viewport, sidebar));
        }
        let width = self.panel_state().and_then(|panel| panel.width).unwrap_or(DEFAULT_WIDTH);
        Some(clamp_width(width, viewport, panel_row(viewport, sidebar)))
    }

    /// The width the open panel takes from the row beside the chat; a sheet takes none.
    pub(super) fn right_panel_docked_width(&self, window: &Window) -> Option<f32> {
        self.right_panel_width(window).filter(|_| !self.right_panel_sheet(window))
    }

    /// `rightPanelMaximized`: only an open panel counts as maximized.
    pub(super) fn right_panel_maximized(&self) -> bool {
        self.right_panel_open() && self.panel_state().is_some_and(|panel| panel.maximized)
    }

    /// `rightPanel.toggleMaximized` and the titlebar's maximize toggle; not a panel choice.
    pub(super) fn toggle_right_panel_maximized(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.right_panel_open() {
            return false;
        }
        if let Some(panel) = self.panel_state_mut() {
            panel.maximized = !panel.maximized;
            cx.notify();
        }
        true
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
        if let (Some(thread), Some(Surface::Browser(tab))) = (self.preview_thread(), self.active_surface().cloned()) {
            self.float_browser(thread, tab, cx);
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

    /// [`open_surface`](Self::open_surface) on `target`'s panel, which may not be the one showing.
    pub(super) fn open_surface_on(
        &mut self,
        target: &Target,
        surface: Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.target.as_ref() == Some(target) {
            return self.open_surface(surface, window, cx);
        }
        let panel = self.right_panel.panels_mut().entry(target.clone()).or_default();
        panel.user_revision += 1;
        panel.upsert(surface);
    }

    fn activate_surface(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = self.user_panel_state().filter(|panel| panel.surfaces.contains(&surface)) {
            panel.open = true;
            panel.active = Some(surface);
        }
        self.sync_active_browser(window, cx);
        cx.notify();
    }

    /// Removes a tab without the side effects of closing it (its terminals already closed).
    pub(super) fn drop_surface(&mut self, target: &Target, surface: &Surface) {
        if let Some(panel) = self.right_panel.panels_mut().get_mut(target) {
            panel.close(surface);
        }
    }

    /// Closes a tab; a browser tab's session closes with it, a terminal tab's after confirming.
    pub(super) fn close_surface(&mut self, surface: Surface, window: &mut Window, cx: &mut Context<Self>) {
        if let Surface::Terminal(id) = surface {
            return self.request_close_terminal_surface(id, window, cx);
        }
        let closed = self.user_panel_state().is_some_and(|panel| {
            let shown = panel.surfaces.contains(&surface);
            panel.close(&surface);
            shown
        });
        if closed {
            self.remember_closed_surface(&surface);
        }
        if let Surface::SideChat(thread_id) = &surface {
            self.release_side_chat(thread_id, cx);
        }
        if let (Surface::Browser(tab), Some(thread)) = (&surface, self.preview_thread()) {
            self.close_preview_tab(thread.tab(tab), window, cx);
        }
        self.sync_active_browser(window, cx);
        cx.notify();
    }

    /// `rightPanel.close`: closes the tab on screen; `false` when the panel shows none.
    pub(super) fn close_active_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(surface) = self.active_surface().cloned() else { return false };
        self.close_surface(surface, window, cx);
        true
    }

    /// `diff.toggle` (`rightPanelStore.toggle(…, "diff")`): hides the panel while it shows the
    /// Diff, else opens the Diff tab.
    pub(super) fn toggle_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.target, Some(Target::Thread { .. })) {
            return;
        }
        if self.active_surface() == Some(&Surface::Diff) {
            self.close_right_panel(cx);
        } else if self.diff_available() {
            self.open_surface(Surface::Diff, window, cx);
        }
    }

    /// The tab menu's Close others / Close to the right / Close all (`closeOtherSurfaces` and
    /// friends): each tab closes as its own close would, then `keep` stays active.
    fn close_surfaces(
        &mut self,
        close: Vec<Surface>,
        keep: Option<Surface>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let active = self.panel_state().and_then(|panel| panel.active.clone());
        for surface in closed::closed_in_order(close, active.as_ref()) {
            self.close_surface(surface, window, cx);
        }
        if let Some(panel) = self.panel_state_mut().filter(|panel| !panel.surfaces.is_empty()) {
            panel.open = true;
            panel.active = active.filter(|active| panel.surfaces.contains(active)).or(keep);
        }
        self.sync_active_browser(window, cx);
        cx.notify();
    }

    /// Opens a tab's menu at `position`; the browser view hides while it is open.
    fn open_tab_menu(
        &mut self,
        surface: &Surface,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let build = self.tab_menu(surface, cx);
        let view = PopupMenu::build(window, cx, move |menu, window, cx| build(menu.min_w(px(180.)), window, cx));
        let previous = window.focused(cx);
        view.focus_handle(cx).focus(window, cx);
        let dismiss = cx.subscribe_in(&view, window, move |this, _, _: &DismissEvent, window, cx| {
            this.right_panel.tab_menu = None;
            if let Some(previous) = &previous {
                previous.focus(window, cx);
            }
            cx.notify();
        });
        self.right_panel.tab_menu = Some((view, position, dismiss));
        cx.notify();
    }

    pub(super) fn tab_menu_open(&self) -> bool {
        self.right_panel.tab_menu.is_some()
    }

    /// The right-click menu on a tab (`handleTabContextMenu`).
    fn tab_menu(
        &self,
        surface: &Surface,
        cx: &mut Context<Self>,
    ) -> impl FnOnce(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.entity();
        let surfaces = self.panel_state().map(|panel| panel.surfaces.clone()).unwrap_or_default();
        let index = surfaces.iter().position(|s| s == surface).unwrap_or(0);
        let surface = surface.clone();
        let copy_path = match &surface {
            Surface::File(path) => {
                let host = self.git_cwd().or_else(|| self.target_project().map(|p| p.workspace_root.clone()));
                let windows =
                    host.as_deref().is_some_and(|h| h.as_bytes().get(1) == Some(&b':') || h.starts_with("\\\\"));
                Some(if windows { path.replace('/', "\\") } else { path.clone() })
            }
            _ => None,
        };
        move |mut menu, window, _| {
            if let Some(path) = copy_path.clone() {
                menu = menu.item(PopupMenuItem::new("Copy path").on_click(move |_, window, cx| {
                    use gpui_kit::component::WindowExt as _;
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(path.clone()));
                    window.push_notification(
                        gpui_kit::component::notification::Notification::success(path.clone()).title("Path copied"),
                        cx,
                    );
                }));
            }
            let others: Vec<Surface> = surfaces.iter().filter(|s| **s != surface).cloned().collect();
            let right: Vec<Surface> = surfaces.iter().skip(index + 1).cloned().collect();
            let all = surfaces.clone();
            let (close, keep, keep_right) = (surface.clone(), surface.clone(), surface.clone());
            menu.item(PopupMenuItem::new("Close").on_click(
                window.listener_for(&view, move |this, _, window, cx| this.close_surface(close.clone(), window, cx)),
            ))
            .item(PopupMenuItem::new("Close others").disabled(surfaces.len() <= 1).on_click(window.listener_for(
                &view,
                move |this, _, window, cx| {
                    this.close_surfaces(others.clone(), Some(keep.clone()), window, cx);
                },
            )))
            .item(PopupMenuItem::new("Close to the right").disabled(index + 1 >= surfaces.len()).on_click(
                window.listener_for(&view, move |this, _, window, cx| {
                    this.close_surfaces(right.clone(), Some(keep_right.clone()), window, cx);
                }),
            ))
            .item(PopupMenuItem::new("Close all").disabled(surfaces.is_empty()).on_click(
                window.listener_for(&view, move |this, _, window, cx| {
                    this.close_surfaces(all.clone(), None, window, cx);
                }),
            ))
        }
    }

    /// Keeps the thread's focused preview tab on the browser surface the panel shows.
    fn sync_active_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let (Some(thread), Some(Surface::Browser(tab))) = (self.preview_thread(), self.active_surface().cloned()) {
            self.focus_preview_tab(&thread, &tab, window, cx);
        }
    }

    /// Follows a thread's preview sessions: a panel tab per drawable session.
    pub(super) fn reconcile_browser_surfaces(&mut self, thread: &ThreadRef, tab_ids: &[&str]) {
        let target = super::preview::panel_target(thread);
        // Preview events reconcile often; only a real change is saved.
        if self.right_panel.panels.entry(target).or_default().reconcile_browsers(tab_ids) {
            self.right_panel.layout.changed = true;
        }
    }

    /// Whether the thread's panel has a tab for `tab_id`; `focus` shows it (`revealSurface`).
    pub(super) fn reveal_browser_surface(&mut self, thread: &ThreadRef, tab_id: &str, focus: bool) -> bool {
        let Some(panel) = self.right_panel.panels.get_mut(&super::preview::panel_target(thread)) else { return false };
        let surface = Surface::Browser(tab_id.to_owned());
        if !panel.surfaces.contains(&surface) {
            return false;
        }
        if focus {
            panel.open = true;
            panel.active = Some(surface);
            self.right_panel.layout.changed = true;
        }
        true
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

    /// `onOpenTurnDiff` (`selectTurn` then the Diff tab): the run's turn diff, at `file` if given.
    pub(super) fn open_turn_diff(
        &mut self,
        run: SharedString,
        file: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target @ Target::Thread { .. }) = self.target.clone() else { return };
        let selection = (target, DiffScope::Turn(run.to_string()));
        if let Some(panel) = self.panel_state_mut() {
            panel.scope = selection.1.clone();
            panel.turn_file = file.clone();
        }
        // The shown patch may hide the requested file as a test file.
        if self.right_panel.hide_test_files && self.right_panel.shown.as_ref() == Some(&selection) {
            self.apply_diff_files(cx);
        }
        self.right_panel.pending_reveal = file.map(|file| (selection, file));
        self.open_surface(Surface::Diff, window, cx);
    }

    /// `onOpenToolDiff` (`selectToolCall` then the Diff tab): the file change a tool call
    /// recorded, by the thread and item it was recorded in. It opens without a git repository.
    pub(super) fn open_tool_call_diff(
        &mut self,
        thread_id: String,
        item_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(self.target, Some(Target::Thread { .. })) {
            return;
        }
        if let Some(panel) = self.panel_state_mut() {
            panel.scope = DiffScope::ToolCall { thread_id, item_id };
            panel.turn_file = None;
        }
        self.right_panel.pending_reveal = None;
        self.open_surface(Surface::Diff, window, cx);
    }

    /// `openFile` for the open target's panel.
    pub(super) fn open_file_surface(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.page = None;
        if let Some(panel) = self.user_panel_state() {
            panel.open_file(path);
        }
        self.sync_active_browser(window, cx);
        cx.notify();
    }

    /// `openAttachment`: an uploaded attachment's tab on the open target's panel.
    pub(super) fn open_attachment(
        &mut self,
        file: super::files::AttachmentFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.page = None;
        if let Some(panel) = self.user_panel_state() {
            panel.open_attachment(file);
        }
        self.sync_active_browser(window, cx);
        cx.notify();
    }

    /// `navigateFolder` from the tab showing `source`.
    pub(super) fn navigate_file_folder(
        &mut self,
        source: Surface,
        folder: &str,
        keep_source: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(panel) = self.user_panel_state() {
            panel.navigate_folder(&source, folder, keep_source);
        }
        cx.notify();
    }

    /// The workspace mutation the open thread last saw, so file surfaces refetch after agent edits.
    pub(super) fn current_workspace_mutation(&self) -> (Option<String>, Option<String>) {
        let Some(projection) = self.open_projection() else { return (None, None) };
        // The newest turn as `turn_summaries` orders them, without sorting every frame.
        let latest = projection
            .checkpoints
            .iter()
            .filter(|c| c.run_id.is_some() && c.app_run_ordinal.is_some_and(|n| u32::try_from(n).is_ok()))
            .max_by(|a, b| a.app_run_ordinal.cmp(&b.app_run_ordinal).then_with(|| a.captured_at.cmp(&b.captured_at)));
        workspace_mutation_id(projection, latest.map(|c| c.captured_at.as_str()))
    }

    /// `diffAvailable`: a server thread in a git repository.
    fn diff_available(&self) -> bool {
        matches!(self.target, Some(Target::Thread { .. })) && self.vcs.is_repo()
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
        // Rows without lines are folded for them, not by the user: one that did not load drops
        // out, and one still loading keeps the fold it had.
        let previous = self.current_folds();
        let state = self.right_panel.diff.read(cx);
        let failed =
            |path: &SharedString| self.right_panel.patch_states.get(path) == Some(&diff_load::FileState::Failed);
        let folded = state.files().iter().map(DiffFile::path).filter(|path| {
            if self.pending_patch(path).is_some() {
                previous.contains(*path)
            } else {
                state.is_file_collapsed(path) && !failed(path)
            }
        });
        self.right_panel.collapsed.insert(shown, folded.cloned().collect());
    }

    /// The shown selection's folded files: as the user left them, else `diffFilesCollapsed`.
    fn current_folds(&self) -> HashSet<SharedString> {
        let Some(shown) = &self.right_panel.shown else { return HashSet::new() };
        let paths: Vec<SharedString> = self.right_panel.all_diff_files.iter().map(|f| f.path().clone()).collect();
        collapsed_files(
            self.right_panel.collapsed.get(shown),
            self.client_settings.values.bool("diffFilesCollapsed"),
            &paths,
        )
    }

    /// The index of `path` among the shown selection's lazily read files while its patch is
    /// still on its way.
    fn pending_patch(&self, path: &str) -> Option<usize> {
        let patches = self.right_panel.patches.as_ref()?;
        if self.right_panel.shown.as_ref() != Some(&patches.selection) {
            return None;
        }
        patches.index.get(path).copied().filter(|index| patches.rows[*index].is_none())
    }

    /// Folds the shown files to `folded`, remembered for the selection; rows without lines stay
    /// folded whatever the user chose.
    fn set_diff_folds(&mut self, folded: HashSet<SharedString>, cx: &mut Context<Self>) {
        let Some(shown) = self.right_panel.shown.clone() else { return };
        let paths: Vec<SharedString> =
            self.right_panel.diff.read(cx).files().iter().map(|file| file.path().clone()).collect();
        let lineless: HashSet<SharedString> = paths
            .iter()
            .filter(|path| {
                self.pending_patch(path).is_some()
                    || self.right_panel.patch_states.get(*path) == Some(&diff_load::FileState::Failed)
            })
            .cloned()
            .collect();
        self.right_panel.diff.update(cx, |state, cx| {
            for path in &paths {
                state.set_file_collapsed(path, folded.contains(path) || lineless.contains(path), cx);
            }
        });
        let failed =
            |path: &SharedString| self.right_panel.patch_states.get(path) == Some(&diff_load::FileState::Failed);
        let folded = folded.iter().filter(|path| !failed(path)).cloned().collect();
        self.right_panel.collapsed.insert(shown, folded);
        cx.notify();
    }

    /// The header's fold toggle (`toggleDiffFileCollapsed`): a file still loading unfolds and is
    /// read at once.
    pub(super) fn fold_diff_file(&mut self, path: &str, collapsed: bool, cx: &mut Context<Self>) {
        let mut folded = self.current_folds();
        if collapsed {
            folded.insert(path.to_owned().into());
        } else {
            folded.remove(path);
        }
        self.set_diff_folds(folded, cx);
        if let Some(index) = self.pending_patch(path).filter(|_| !collapsed) {
            self.request_file_patches([index], cx);
        }
    }

    /// `revealDiffFile`: unfolds the file, asks for its patch if it is still loading, and
    /// scrolls to it.
    fn reveal_diff_file(&mut self, path: &str, cx: &mut Context<Self>) {
        self.fold_diff_file(path, false, cx);
        self.right_panel.diff.update(cx, |state, cx| state.scroll_to_file(path, cx));
    }

    /// `useProactivePanels` for turn diffs: with proactive panels on, a turn that just completed
    /// with enough changes opens the branch diff, unless the user chose a panel since it started.
    /// A thread's pull requests take precedence (`observe_proactive_pull_requests`).
    pub(super) fn observe_proactive_panels(&mut self, cx: &mut Context<Self>) {
        let Some(target @ Target::Thread { .. }) = self.target.clone() else {
            self.right_panel.observation = None;
            return;
        };
        let Target::Thread { id: thread_id, .. } = &target else { return };
        let thread_id = thread_id.clone();
        let revision = self.right_panel.panels.get(&target).map_or(0, |panel| panel.user_revision);
        let projection = self.open_projection();
        let running = projection.and_then(ThreadProjection::interruptible_run).map(|run| run.id.clone());
        let mut observation = observe(self.right_panel.observation.as_ref(), &thread_id, running.as_deref(), revision);
        let pull_requests = self.observe_proactive_pull_requests(&thread_id, &mut observation, cx);
        let projection = self.open_projection();
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
            // A thread with pull requests offers those instead of its turn diffs.
            Some(run) if self.proactive_panels_enabled() && !pull_requests => {
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
        if self.right_panel.panels_mut().entry(target).or_default().open_proactive_diff(&run, revision) {
            cx.notify();
        }
    }

    /// The pull request half of `useProactivePanels`: a Pull request tab follows the thread's
    /// pull request when it changes, and with proactive panels on, newly linked pull requests
    /// open (the list when there are several). `true` while the thread has any.
    fn observe_proactive_pull_requests(
        &mut self,
        thread_id: &str,
        observation: &mut Observation,
        cx: &mut Context<Self>,
    ) -> bool {
        let linked = self.thread_pull_request_target();
        let previous = self.right_panel.observed_pull_requests.insert(thread_id.to_owned(), linked.clone()).flatten();
        let shown = self.active_surface().cloned();
        // `shouldRetargetThreadPullRequestPanel`.
        let follow = match (&previous, &linked, &shown) {
            (Some(previous), Some(linked), Some(Surface::PullRequest(open))) => previous != linked && open == previous,
            _ => false,
        };
        let visible = self.thread_pull_request_keys();
        let key = match visible.len() {
            0 => linked.as_ref().map(|r| format!("{}:{}#{}", r.project_id, r.repository.to_lowercase(), r.number)),
            _ => Some(visible.join("\n")),
        };
        let eligible =
            self.proactive_panels_enabled() && key.is_some() && observation.target_key.as_ref() != Some(&key);
        observation.target_key = Some(key.clone());
        let revision = observation.revision;
        let Some(target) = self.thread_target(thread_id) else { return false };
        // This runs on every thread update; only a tab it opens is saved.
        let panel = self.right_panel.panels.entry(target).or_default();
        let mut changed = false;
        if let (true, Some(linked)) = (follow, &linked) {
            changed |= panel.open_proactive(Surface::PullRequest(linked.clone()), revision, true);
        }
        if eligible && !follow {
            if !visible.is_empty() && (visible.len() > 1 || linked.is_none()) {
                changed |= panel.open_proactive(Surface::PullRequests, revision, false);
            } else if let Some(linked) = linked {
                changed |= panel.open_proactive(Surface::PullRequest(linked), revision, false);
            }
        }
        if changed {
            self.right_panel.layout.changed = true;
            cx.notify();
        }
        key.is_some()
    }

    fn set_diff_scope(&mut self, scope: DiffScope, cx: &mut Context<Self>) {
        let Some(panel) = self.panel_state_mut() else { return };
        panel.scope = scope;
        if panel.turn_file.take().is_some() && self.right_panel.hide_test_files {
            self.apply_diff_files(cx);
        }
        cx.notify();
    }

    /// The file a chat link asked `selection`'s turn diff to show.
    fn requested_diff_file(&self, selection: &(Target, DiffScope)) -> Option<&str> {
        let panel = self.right_panel.panels.get(&selection.0)?;
        panel.turn_file.as_deref().filter(|_| panel.scope == selection.1)
    }

    /// Starts a fetch when the shown selection differs from the loaded one, else reveals a pending file.
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
            DiffScope::Turn(run) => {
                (turns.iter().find(|t| &t.run_id == run).map_or(0, |t| u64::from(t.turn_count)), 0, 0)
            }
            DiffScope::ToolCall { .. } => (0, 0, 0),
            _ => self.vcs.status.as_ref().map_or((0, 0, 0), |s| {
                let (ins, del, _) = git::changes_totals(s);
                (ins, del, s.local.working_tree.insertions + s.local.working_tree.deletions)
            }),
        };
        let mutation = match &scope {
            DiffScope::Turn(_) => (None, None),
            // `resolveToolCallDiffTarget`: the item is read again once it changes.
            DiffScope::ToolCall { thread_id, item_id } => (self.tool_call_revision(thread_id, item_id), None),
            _ => self
                .open_projection()
                .map(|p| workspace_mutation_id(p, turns.first().map(|t| t.completed_at.as_str())))
                .unwrap_or_default(),
        };
        let ignore_whitespace = self.diff_ignore_whitespace();
        let remembered_base = self.diff_base_ref().map(str::to_owned);
        let base_ref = remembered_base.clone().filter(|_| matches!(scope, DiffScope::Branch | DiffScope::Commit(_)));
        let key = DiffKey {
            target: target.clone(),
            cwd: cwd.clone(),
            scope: scope.clone(),
            base_ref: base_ref.clone(),
            ignore_whitespace,
            version,
            mutation: mutation.clone(),
            refresh: self.right_panel.refresh,
        };
        if self.right_panel.loaded.as_ref() == Some(&key) {
            self.reveal_pending_file(cx);
            return;
        }
        let selection = (target.clone(), scope.clone());
        // A refresh of the shown selection keeps it readable until the new read lands.
        let retain = self.right_panel.loaded.as_ref().is_some_and(|loaded| loaded.same_selection(&key))
            && self.right_panel.shown.as_ref() == Some(&selection)
            && matches!(self.right_panel.load, DiffLoad::Ready { .. } | DiffLoad::Empty(_));
        let install = if retain { Install::Update } else { Install::Replace };
        self.right_panel.loaded = Some(key);
        let (Some(client), Target::Thread { env, id: thread_id }) = (self.client(), target) else { return };
        if let DiffScope::ToolCall { thread_id, item_id } = &scope {
            let mut input = json!({"threadId": thread_id, "itemId": item_id});
            if let Some(revision) = mutation.0 {
                input["revision"] = revision.into();
            }
            if let Some(cwd) = cwd {
                self.read_branch_commits(client.clone(), cwd, remembered_base, cx);
            }
            return self.read_tool_call_diff(selection, client, input, ignore_whitespace, install, window, cx);
        }
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
                // `useCheckpointDiff`: a range from the start is the full thread diff.
                let from = turn.turn_count.saturating_sub(1);
                let mut input = json!({"threadId": thread_id, "toTurnCount": turn.turn_count, "ignoreWhitespace": ignore_whitespace});
                if from == 0 {
                    ("orchestration.getFullThreadDiff", input)
                } else {
                    input["fromTurnCount"] = from.into();
                    ("orchestration.getTurnDiff", input)
                }
            }
            scope => {
                let Some(cwd) = cwd.clone() else { return };
                // Stats only; the patches follow one file at a time, as the web reads them.
                let mut input = json!({"cwd": cwd, "ignoreWhitespace": ignore_whitespace, "metadataOnly": true});
                if let Some(base) = &base_ref {
                    input["baseRef"] = base.clone().into();
                }
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
            DiffScope::Turn(_) | DiffScope::ToolCall { .. } => None,
        };
        if let Some(cwd) = cwd.clone() {
            self.read_branch_commits(client.clone(), cwd, remembered_base.clone(), cx);
        }
        let commit_sha = match &scope {
            DiffScope::Commit(sha) => Some(sha.clone()),
            _ => None,
        };
        if !retain {
            self.right_panel.load = DiffLoad::Loading;
            (self.right_panel.patches, self.right_panel.patch_states) = (None, std::rc::Rc::default());
        }
        let diff = self.right_panel.diff.clone();
        self.right_panel.diff_reading = true;
        self.right_panel._task = Some(cx.spawn_in(window, async move |this, cx| {
            let (method, input) = request;
            let result = client.request(method, input).await;
            let read = result.is_ok();
            let parsed =
                cx.background_executor()
                    .spawn(async move {
                        result.map_err(|error| error.to_string()).map(|value| parse_diff(&value, source_kind))
                    })
                    .await;
            // `useReviewFilePatches`: each file's patch on its own, as the viewport asks for them.
            let source_base = match &parsed {
                Ok(Some(parsed)) => parsed.base.clone(),
                _ => None,
            };
            // `createGitDiffFileContentsLoader`: a git comparison's files read in full, from its refs.
            let contents_source = match (&parsed, source_kind, &cwd) {
                (Ok(Some(parsed)), Some(kind), Some(cwd)) => {
                    Some(json!({"cwd": cwd, "sourceKind": kind, "baseRef": parsed.base, "headRef": parsed.head}))
                }
                _ => None,
            };
            let base_ref = if commit_sha.is_some() { remembered_base } else { source_base.or(remembered_base) };
            let patch_request = source_kind.zip(cwd).map(|(source_kind, cwd)| diff_load::PatchRequest {
                cwd,
                base_ref,
                commit_sha,
                source_kind,
                ignore_whitespace,
            });
            let _ = this.update(cx, |this, cx| {
                this.right_panel.diff_reading = false;
                if read {
                    this.right_panel.diff_read_at = Some(std::time::Instant::now());
                }
                let previous = this.right_panel.patches.take().filter(|_| retain);
                let loader = contents_source.map(|source| {
                    contents_loader(
                        cx.weak_entity(),
                        move |this| this.env_client(&env),
                        "review.getDiffFileContents",
                        source,
                    )
                });
                diff.update(cx, |state, cx| state.set_contents_loader(loader, cx));
                this.right_panel.load = match parsed {
                    Err(error) => DiffLoad::Failed(error.into()),
                    Ok(None) => {
                        this.store_diff_files(Vec::new());
                        diff.update(cx, |state, cx| state.set_files(Vec::new(), cx));
                        DiffLoad::Empty("No patch available for this selection.")
                    }
                    Ok(Some(parsed)) if parsed.files.is_empty() && parsed.deferred.is_empty() => {
                        this.store_diff_files(Vec::new());
                        diff.update(cx, |state, cx| state.set_files(Vec::new(), cx));
                        DiffLoad::Empty("No net changes in this selection.")
                    }
                    Ok(Some(parsed)) => {
                        let (additions, deletions) = parsed.totals.unwrap_or_else(|| {
                            let sum = |count: fn(&DiffFile) -> usize| parsed.files.iter().map(count).sum::<usize>();
                            (sum(DiffFile::additions), sum(DiffFile::deletions))
                        });
                        let pending = match patch_request.filter(|_| !parsed.deferred.is_empty()) {
                            Some(request) => {
                                let patches = FilePatches::new(
                                    selection.clone(),
                                    client.clone(),
                                    request,
                                    parsed.deferred,
                                    previous.as_ref(),
                                );
                                let outstanding = patches.demand.outstanding();
                                this.right_panel.patches = Some(patches);
                                this.show_file_patches(install, cx);
                                this.start_patch_reads(cx);
                                outstanding
                            }
                            None => {
                                this.show_diff_files(&selection, parsed.files, install, cx);
                                0
                            }
                        };
                        DiffLoad::Ready {
                            additions,
                            deletions,
                            head: parsed.head,
                            base: parsed.base,
                            truncated: parsed.truncated,
                            pending,
                            omitted: None,
                        }
                    }
                };
                this.rebuild_tree_rows(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// `branchCommitsQuery`: the branch's commits for the scope menu.
    fn read_branch_commits(&mut self, client: RpcClient, cwd: String, base: Option<String>, cx: &mut Context<Self>) {
        let mut input = json!({"cwd": cwd});
        if let Some(base) = base {
            input["baseRef"] = base.into();
        }
        self.right_panel._commits_task = Some(cx.spawn(async move |this, cx| {
            let Ok(reply) = client.request("review.getBranchCommits", input).await else { return };
            let _ = this.update(cx, |this, cx| {
                this.right_panel.commits = parse_commits(&reply);
                cx.notify();
            });
        }));
    }

    /// The revision of a tool call's item in the thread or side chat on screen
    /// (`turnItemDetailRevision`), while it is still a file change there.
    fn tool_call_revision(&self, thread_id: &str, item_id: &str) -> Option<String> {
        let projections = self.open_projection().into_iter().chain(self.side_chat_projection());
        let mut rows = projections.flat_map(|projection| &projection.visible_turn_items);
        let row = rows.find(|row| row.source_thread_id == thread_id && row.source_item_id == item_id)?;
        (row.item.item_type == "file_change").then(|| super::inspector::revision(&row.item).to_string())
    }

    /// `useTurnItemDetail` for a tool call selection: reads the item and shows its recorded diff.
    fn read_tool_call_diff(
        &mut self,
        selection: (Target, DiffScope),
        client: RpcClient,
        input: Value,
        ignore_whitespace: bool,
        install: Install,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(install, Install::Replace) {
            self.right_panel.load = DiffLoad::Loading;
        }
        (self.right_panel.patches, self.right_panel.patch_states) = (None, std::rc::Rc::default());
        let diff = self.right_panel.diff.clone();
        self.right_panel.diff_reading = true;
        self.right_panel._task = Some(cx.spawn_in(window, async move |this, cx| {
            let reply = client.request("orchestration.getTurnItem", input).await;
            let view = cx
                .background_executor()
                .spawn(async move {
                    reply
                        .map(|reply| diff_tool_call::view(&reply, ignore_whitespace))
                        .map_err(|error| error.to_string())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.right_panel.diff_reading = false;
                diff.update(cx, |state, cx| state.set_contents_loader(None, cx));
                let view = match view {
                    Ok(view) => view,
                    Err(error) => {
                        this.right_panel.load = DiffLoad::Failed(error.into());
                        return cx.notify();
                    }
                };
                let truncated = view.truncated;
                if !matches!(view.body, diff_tool_call::Body::Files { .. }) {
                    this.clear_diff(cx);
                }
                this.right_panel.load = match view.body {
                    diff_tool_call::Body::Files { files, omitted } => {
                        let additions = files.iter().map(DiffFile::additions).sum();
                        let deletions = files.iter().map(DiffFile::deletions).sum();
                        this.show_diff_files(&selection, files, install, cx);
                        this.rebuild_tree_rows(cx);
                        DiffLoad::Ready { additions, deletions, head: None, base: None, truncated, pending: 0, omitted }
                    }
                    diff_tool_call::Body::NoPatch => DiffLoad::Empty("No patch available for this selection."),
                    diff_tool_call::Body::NoNetChanges => DiffLoad::Empty("No net changes in this selection."),
                    diff_tool_call::Body::Raw { reason, text } => {
                        DiffLoad::Raw { reason, text: text.into(), truncated }
                    }
                    diff_tool_call::Body::Recorded(content) => DiffLoad::Recorded {
                        content: content.into_iter().map(|(label, text)| (label, text.into())).collect(),
                        truncated,
                    },
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Starts the patch reads the demand lets run now; the patch shows it is loading while any
    /// asked-for read is outstanding.
    fn start_patch_reads(&mut self, cx: &mut Context<Self>) {
        let Some(patches) = &mut self.right_panel.patches else { return };
        let outstanding = patches.demand.outstanding();
        if let DiffLoad::Ready { pending, .. } = &mut self.right_panel.load
            && *pending != outstanding
        {
            *pending = outstanding;
            cx.notify();
        }
        for index in patches.demand.start() {
            let (client, file) = (patches.client.clone(), patches.files[index].clone());
            let (input, source_kind) = (patches.request.input(&file), patches.request.source_kind);
            patches._reads.push(cx.spawn(async move |this, cx| {
                let reply = client.request("review.getDiffPreview", input).await;
                if let Err(error) = &reply {
                    log::warn!("diff file patch failed: {error}");
                }
                let settled = cx
                    .background_executor()
                    .spawn(async move { diff_load::settle_file(&file, reply.as_ref().ok(), source_kind) })
                    .await;
                let _ = this.update(cx, |this, cx| this.settle_file_patch(index, settled, cx));
            }));
        }
    }

    /// Asks for files' patches by their index among the lazily read files.
    fn request_file_patches(&mut self, indices: impl IntoIterator<Item = usize>, cx: &mut Context<Self>) {
        let Some(patches) = &mut self.right_panel.patches else { return };
        patches.demand.request(indices);
        self.start_patch_reads(cx);
    }

    /// Puts a settled read in its file's place and starts the next one waiting.
    fn settle_file_patch(
        &mut self,
        index: usize,
        settled: (Vec<DiffFile>, diff_load::FileState),
        cx: &mut Context<Self>,
    ) {
        let Some(patches) = &mut self.right_panel.patches else { return };
        patches.rows[index] = Some(settled);
        patches.demand.settle(index);
        self.start_patch_reads(cx);
        self.show_file_patches(Install::Update, cx);
    }

    /// Shows every lazily read file: its patch once settled, else its placeholder header.
    fn show_file_patches(&mut self, install: Install, cx: &mut Context<Self>) {
        let Some(patches) = &self.right_panel.patches else { return };
        let (mut files, mut states, mut lineless) = (Vec::new(), HashMap::new(), Vec::new());
        for (row, placeholder) in patches.rows.iter().zip(&patches.placeholders) {
            match row {
                Some((rows, state)) => {
                    if *state != diff_load::FileState::Loaded {
                        states.extend(rows.iter().map(|file| (file.path().clone(), *state)));
                    }
                    if *state == diff_load::FileState::Failed {
                        lineless.extend(rows.iter().map(|file| file.path().clone()));
                    }
                    files.extend(rows.iter().cloned());
                }
                None => {
                    lineless.extend(placeholder.iter().map(|file| file.path().clone()));
                    files.extend(placeholder.iter().cloned());
                }
            }
        }
        let selection = patches.selection.clone();
        self.right_panel.patch_states = std::rc::Rc::new(states);
        self.show_diff_files(&selection, files, install, cx);
        // A row with no lines yet, or one that did not load, has nothing to unfold.
        self.right_panel.diff.update(cx, |state, cx| {
            for path in &lineless {
                state.set_file_collapsed(path, true, cx);
            }
        });
        self.rebuild_tree_rows(cx);
        cx.notify();
    }

    /// `DiffFileLoadingBoundary` and each pending header's `load`: reads the files whose headers
    /// come within the demand margin below the viewport unless the user folded them, and the
    /// next batch once the last file comes near.
    fn request_visible_patches(&mut self, cx: &mut Context<Self>) {
        let Some(patches) = &self.right_panel.patches else { return };
        if self.right_panel.shown.as_ref() != Some(&patches.selection) || !patches.rows.iter().any(Option::is_none) {
            return;
        }
        let state = self.right_panel.diff.read(cx);
        let near = state.files_near_viewport(px(diff_load::DEMAND_MARGIN));
        if near.is_empty() {
            return;
        }
        let at_end = near.end >= state.files().len();
        let asked: Vec<(usize, SharedString)> = state.files()[near]
            .iter()
            .filter_map(|file| Some((*patches.index.get(file.path())?, file.path().clone())))
            .filter(|(index, _)| patches.rows[*index].is_none() && !patches.demand.is_requested(*index))
            .collect();
        let first_unsettled = patches.rows.iter().position(Option::is_none);
        let indices: Vec<usize> = if asked.is_empty() {
            Vec::new()
        } else {
            let folded = self.current_folds();
            asked.into_iter().filter(|(_, path)| !folded.contains(path)).map(|(index, _)| index).collect()
        };
        let Some(patches) = &mut self.right_panel.patches else { return };
        patches.demand.request(indices);
        if let Some(first) = first_unsettled.filter(|_| at_end) {
            patches.demand.load_next(first);
        }
        self.start_patch_reads(cx);
    }

    /// `retry`: reads a file whose patch did not load again.
    fn retry_file_patch(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(patches) = &mut self.right_panel.patches else { return };
        let Some(&index) = patches.index.get(path) else { return };
        patches.demand.retry(index);
        self.start_patch_reads(cx);
    }

    /// Puts a selection's loaded files in the patch, folded as the user left them.
    fn show_diff_files(
        &mut self,
        selection: &(Target, DiffScope),
        files: Vec<DiffFile>,
        install: Install,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<SharedString> = files.iter().map(|file| file.path().clone()).collect();
        let folded = collapsed_files(
            self.right_panel.collapsed.get(selection),
            self.client_settings.values.bool("diffFilesCollapsed"),
            &paths,
        );
        self.store_diff_files(files);
        let shown = self.shown_diff_files(Some(selection));
        self.right_panel.diff.update(cx, |state, cx| {
            match install {
                Install::Replace => state.set_files(shown, cx),
                Install::Update => state.update_files(shown, cx),
            }
            for path in &paths {
                state.set_file_collapsed(path, folded.contains(path), cx);
            }
        });
        self.right_panel.shown = Some(selection.clone());
        self.reveal_pending_file(cx);
    }

    /// `revealDiffFile` for the file `open_turn_diff` asked for: done once the shown selection's
    /// patch holds it, whether that selection just loaded or was already loaded. The file unfolds
    /// as if the user had unfolded it, then scrolls into view.
    fn reveal_pending_file(&mut self, cx: &mut Context<Self>) {
        let Some((selection, file)) = &self.right_panel.pending_reveal else { return };
        if self.right_panel.shown.as_ref() != Some(selection) {
            return;
        }
        let Some(path) = diff_tree::find_requested(self.right_panel.diff.read(cx).files(), file).cloned() else {
            return;
        };
        self.right_panel.pending_reveal = None;
        self.reveal_diff_file(&path, cx);
    }

    fn clear_diff(&mut self, cx: &mut Context<Self>) {
        (self.right_panel.patches, self.right_panel.patch_states) = (None, std::rc::Rc::default());
        self.store_diff_files(Vec::new());
        self.right_panel.diff.update(cx, |state, cx| state.set_files(Vec::new(), cx));
        self.rebuild_tree_rows(cx);
    }

    /// The inline panel beside the main column, or `None` while closed.
    /// The panel shows a git diff (not a turn's), which edits outside T3 Code can change.
    fn git_diff_shown(&self) -> bool {
        self.right_panel_open()
            && self.active_surface() == Some(&Surface::Diff)
            && self.diff_available()
            && self.panel_state().is_some_and(|panel| {
                matches!(panel.scope, DiffScope::Branch | DiffScope::Unstaged | DiffScope::Commit(_))
            })
    }

    /// `useLiveRefresh(…, { refreshOnShow })` for the git diff: showing it, or the window
    /// regaining focus while it shows, reads it again unless a read landed within the floor.
    /// Edits made outside T3 Code bump no mutation id, so nothing else would. Returns whether
    /// a read was asked for.
    pub(super) fn revalidate_diff(&mut self) -> bool {
        let stale =
            should_revalidate(self.right_panel.diff_read_at, self.right_panel.diff_reading, std::time::Instant::now());
        if !stale || !self.git_diff_shown() {
            return false;
        }
        self.right_panel.refresh += 1;
        true
    }

    pub(super) fn render_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.persist_panel_layout(cx);
        self.right_panel.viewport_width = f32::from(window.viewport_size().width);
        self.sync_right_panel_sheet_focus(window, cx);
        let git_diff_shown = self.git_diff_shown();
        if git_diff_shown && !self.right_panel.git_diff_shown {
            self.revalidate_diff();
        }
        self.right_panel.git_diff_shown = git_diff_shown;
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
        self.sync_discovered_servers(window, cx);
        // A hidden panel closes its "+" menu, so showing the panel again leaves it closed.
        let Some(width) = self.right_panel_width(window) else {
            self.right_panel.add_menu_open = false;
            return None;
        };
        self.sync_diff(window, cx);
        if self.active_surface() == Some(&Surface::Diff) {
            self.request_visible_patches(cx);
        }
        let tokens = *theme::tokens(cx);
        let sheet = self.right_panel_sheet(window);
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
            (Some(Surface::Diff), _) => self.render_diff_surface(&panel.scope, &tokens, window, cx),
            (Some(Surface::Browser(tab)), Some(Target::Thread { env, id })) => {
                self.render_browser_surface(&ThreadRef::new(env, id), tab, width, window, cx)
            }
            (Some(Surface::Files), _) => self.render_files_surface(None, width, window, cx),
            (Some(Surface::File(path)), _) => self.render_files_surface(Some(path.clone()), width, window, cx),
            (Some(Surface::Attachment(file)), _) => self.render_attachment_surface(file, width, window, cx),
            (Some(Surface::SideChat(thread_id)), _) => self.render_side_chat_surface(thread_id, width, window, cx),
            (Some(Surface::PullRequest(reference)), _) => self.render_pull_request_surface(reference, window, cx),
            (Some(Surface::PullRequests), _) => self.render_linked_pull_requests(window, cx),
            (Some(Surface::Terminal(id)), _) => self.render_terminal_surface(&id.clone(), window, cx),
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
                if event.click_count() == 2
                    && let Some(panel) = this.panel_state_mut()
                {
                    panel.width = None;
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
                    .group_hover("right-panel-resize", gpui_kit::Styled::visible)
                    .bg(tokens.border),
            );
        Some(
            v_flex()
                .id("right-panel")
                .track_focus(&self.areas.panel)
                .relative()
                .w(px(width))
                .h_full()
                .flex_none()
                .bg(tokens.background)
                .children(super::theme_inspect::probe(&["canvas", "border"], cx))
                .border_l_1()
                .border_color(tokens.border)
                .on_drag_move(cx.listener(|this, event: &DragMoveEvent<RightPanelResize>, window, cx| {
                    let viewport = f32::from(window.viewport_size().width);
                    let right = f32::from(event.bounds.right());
                    let width = (right - f32::from(event.event.position.x)).max(MIN_WIDTH);
                    let sidebar = if this.sidebar_open.docked() { f32::from(this.sidebar_width()) } else { 0. };
                    let width = clamp_width(width, viewport, panel_row(viewport, sidebar));
                    if let Some(panel) = this.panel_state_mut() {
                        panel.width = Some(width);
                    }
                    cx.notify();
                }))
                .child({
                    // `COLLAPSED_SIDEBAR_TITLEBAR_INSET_CLASS`: maximized past a hidden sidebar, clear its toggle.
                    let inset = (panel.maximized && !self.sidebar_open.docked())
                        .then(|| super::titlebar::collapsed_content_left(window));
                    self.render_tab_bar(&panel, inset, sheet, &tokens, window, cx)
                })
                .child(div().flex_1().min_h_0().flex().flex_col().child(content))
                // The sheet keeps its fixed width (`mode="sheet"` has no resize handle).
                .when(!sheet, |el| el.child(deferred(handle).with_priority(1)))
                .children(self.right_panel.tab_menu.as_ref().map(|(menu, position, _)| {
                    deferred(
                        gpui_kit::anchored().position(*position).snap_to_window_with_margin(px(8.)).child(menu.clone()),
                    )
                    .with_priority(3)
                }))
                .into_any_element(),
        )
    }

    /// `handleAddSurfaceMenuKeyDown`: an available surface's letter closes the "+" menu and opens it.
    fn add_surface_for_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(shortcut) = SurfaceShortcut::for_keystroke(&event.keystroke) else { return };
        if !self.surface_available(shortcut) {
            return;
        }
        cx.stop_propagation();
        window.dispatch_action(Box::new(gpui_kit::base::actions::Cancel), cx);
        self.open_surface_shortcut(shortcut, window, cx);
    }

    /// Whether the "+" menu and the launcher offer `shortcut`'s surface and it can open here.
    fn surface_available(&self, shortcut: SurfaceShortcut) -> bool {
        match shortcut {
            SurfaceShortcut::Browser => self.browser_available(),
            SurfaceShortcut::Terminal => self.terminal_available(),
            SurfaceShortcut::Files => self.project_target().is_some(),
            SurfaceShortcut::Diff => self.diff_available(),
            SurfaceShortcut::PullRequest => self.thread_pull_request_target().is_some(),
            SurfaceShortcut::LinkedPullRequests => self.thread_links_pull_requests(),
            SurfaceShortcut::SideChat => self.side_chats_offered() && self.side_chat_disabled_reason().is_none(),
        }
    }

    fn open_surface_shortcut(&mut self, shortcut: SurfaceShortcut, window: &mut Window, cx: &mut Context<Self>) {
        match shortcut {
            SurfaceShortcut::Browser => self.add_browser_surface(window, cx),
            SurfaceShortcut::Terminal => self.add_terminal_surface(window, cx),
            SurfaceShortcut::Files => self.open_surface(Surface::Files, window, cx),
            SurfaceShortcut::Diff => self.open_changes(window, cx),
            SurfaceShortcut::PullRequest => {
                if let Some(reference) = self.thread_pull_request_target() {
                    self.open_surface(Surface::PullRequest(reference), window, cx);
                }
            }
            SurfaceShortcut::LinkedPullRequests => self.open_surface(Surface::PullRequests, window, cx),
            SurfaceShortcut::SideChat => self.add_side_chat_surface(window, cx),
        }
    }

    /// The launcher's available surfaces in row order (`availableActions`).
    fn launcher_actions(&self) -> Vec<SurfaceShortcut> {
        SurfaceShortcut::ALL.into_iter().filter(|shortcut| self.surface_available(*shortcut)).collect()
    }

    /// The panel shows the launcher in place of a surface.
    fn launcher_shown(&self) -> bool {
        self.right_panel_open()
            && match self.active_surface() {
                None => true,
                Some(Surface::Browser(_)) => !matches!(self.target, Some(Target::Thread { .. })),
                Some(_) => false,
            }
    }

    /// `rightPanel.new`: focuses the "+" menu open, unless another menu or dialog is up. A held
    /// key is still claimed without reopening it.
    pub(super) fn open_add_surface_menu(&mut self, layer_open: bool, held: bool, cx: &mut Context<Self>) -> bool {
        if !self.right_panel_open() || (layer_open && !self.right_panel.add_menu_open) {
            return false;
        }
        if !held && !self.right_panel.add_menu_open {
            self.right_panel.add_menu_open = true;
            cx.notify();
        }
        true
    }

    /// The launcher's window-wide letters (`handleShortcut`): an available surface's letter
    /// opens it while the launcher shows, unless focus is typing or a menu or dialog is up.
    pub(super) fn launcher_letter_key(
        &mut self,
        keystroke: &gpui_kit::Keystroke,
        typing_or_layer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if typing_or_layer || self.right_panel.add_menu_open || self.right_panel.profiles_menu_open {
            return false;
        }
        if self.page.is_some() || self.settings.is_some() || self.tab_menu_open() || !self.launcher_shown() {
            return false;
        }
        let Some(shortcut) = SurfaceShortcut::for_keystroke(keystroke) else { return false };
        if !self.surface_available(shortcut) {
            return false;
        }
        self.open_surface_shortcut(shortcut, window, cx);
        true
    }

    /// A launcher row's hover highlights it, and leaving it clears its highlight.
    fn launcher_hover(
        &self,
        shortcut: SurfaceShortcut,
        cx: &mut Context<Self>,
    ) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        cx.listener(move |this, hovered: &bool, _, cx| {
            let index = this.launcher_actions().iter().position(|action| *action == shortcut);
            if *hovered {
                this.right_panel.launcher_highlight = index;
            } else if this.right_panel.launcher_highlight == index {
                this.right_panel.launcher_highlight = None;
            }
            cx.notify();
        })
    }

    /// The focused launcher's arrows and Enter (`handleKeyDown`).
    fn launcher_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let actions = self.launcher_actions();
        let highlight = self.right_panel.launcher_highlight.filter(|_| !actions.is_empty());
        let highlight = highlight.map(|index| index.min(actions.len().saturating_sub(1)));
        match launcher_step(&event.keystroke, highlight, actions.len()) {
            Some(LauncherStep::Highlight(index)) => {
                self.right_panel.launcher_highlight = Some(index);
                cx.notify();
            }
            Some(LauncherStep::Activate) => {
                let Some(shortcut) = highlight.and_then(|index| actions.get(index).copied()) else { return };
                self.open_surface_shortcut(shortcut, window, cx);
            }
            None => return,
        }
        cx.stop_propagation();
    }

    /// The 40px tab strip, a drag region clear of the panel toggles and caption buttons.
    fn render_tab_bar(
        &self,
        panel: &PanelState,
        left_inset: Option<Pixels>,
        sheet: bool,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // `wco:pr-(--workspace-native-controls-inset)` plus the `w-24` spacer for the toggles.
        let reserve = CAPTION_BUTTONS_WIDTH + px(12.) - RIGHT_RESIZE_INSET + px(96.);
        let view = cx.entity();
        let (diff, browser, files) = (self.diff_available(), self.browser_available(), self.project_target().is_some());
        let side_chat = self.side_chats_offered().then(|| self.side_chat_disabled_reason().is_none());
        let (pull_request, linked) = (self.thread_pull_request_target(), self.thread_links_pull_requests());
        let profiles = self.browser_profiles();
        let terminal = self.terminal_available();
        let add_menu_open = self.right_panel.add_menu_open;
        let tabs: Vec<AnyElement> = panel
            .surfaces
            .iter()
            .map(|surface| self.render_surface_tab(surface, panel.active.as_ref() == Some(surface), tokens, window, cx))
            .collect();
        let tab_bar = h_flex()
            .id("right-panel-tabbar")
            .h(TITLEBAR_HEIGHT)
            .flex_none()
            .min_w_0()
            .pl(left_inset.unwrap_or(px(8.)))
            .pr(if sheet { px(12.) } else { reserve })
            .gap_1();
        // Only the inline panel owns the titlebar row (`ownsDesktopTitleBar`); the sheet carries
        // the layout controls at its right end (`layoutControls`).
        let (tab_bar, layout_controls) = if sheet {
            (tab_bar, Some(self.render_sheet_layout_controls(window, cx)))
        } else {
            (super::titlebar::drag_region(tab_bar), None)
        };
        // The open "+" menu has focus; its letters bubble up here (gpui-kit menus bind only arrows,
        // Enter and Escape).
        tab_bar
            .when(add_menu_open, |el| el.on_key_down(cx.listener(Self::add_surface_for_key)))
            .child(
                super::history_swipe::horizontal_scroller(
                    h_flex().id("right-panel-tabs"),
                    &self.right_panel.tabs_scroll,
                )
                .min_w_0()
                .h_full()
                .gap_1()
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
                    // `ghost-muted`: the open menu keeps its trigger's accent fill (`data-pressed`).
                    .selected(add_menu_open)
                    .dropdown_menu(move |menu: PopupMenu, window, cx| {
                        // `MenuItem`'s `sm:min-h-7 sm:text-sm`.
                        let rem = window.rem_size();
                        let menu = menu.min_w(px(200.)).metrics(MenuMetrics {
                            font_family: None,
                            text_size: rem * 0.875,
                            row_height: rem * 1.75,
                            // `MenuPopup`'s `p-1`, `MenuItem`'s `px-2` and `MenuSeparator`'s `my-1 h-px`.
                            padding_y: px(4.),
                            item_padding_x: px(8.),
                            separator_height: px(9.),
                            background: None,
                            separator: None,
                        });
                        // An available Browser lists the profiles to open it in (`MenuSub`).
                        let menu = if browser {
                            let (view, profiles) = (view.clone(), profiles.clone());
                            menu.submenu_with_icon(
                                Some(menu_icon(Icon::new(IconName::Earth))),
                                "Browser",
                                window,
                                cx,
                                move |menu, window, _| profile_items(menu.max_w(px(224.)), &profiles, &view, window),
                            )
                        } else {
                            menu.item(surface_item(Icon::new(IconName::Earth), "Browser", "B").disabled(true))
                        };
                        let menu = menu
                            .item(
                                surface_item(icon("icons/square-terminal.svg"), "Terminal", "T")
                                    .disabled(!terminal)
                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                        this.add_terminal_surface(window, cx);
                                    })),
                            )
                            .item(surface_item(Icon::new(IconName::Files), "Files", "F").disabled(!files).on_click(
                                window.listener_for(&view, |this, _, window, cx| {
                                    this.open_surface(Surface::Files, window, cx);
                                }),
                            ))
                            .item(surface_item(icon("icons/file-diff.svg"), "Diff", "D").disabled(!diff).on_click(
                                window.listener_for(&view, |this, _, window, cx| this.open_changes(window, cx)),
                            ))
                            .item(
                                surface_item(Icon::new(IconName::GitPullRequest), "Pull request", "P")
                                    .disabled(pull_request.is_none())
                                    .on_click(window.listener_for(&view, move |this, _, window, cx| {
                                        if let Some(reference) = this.thread_pull_request_target() {
                                            this.open_surface(Surface::PullRequest(reference), window, cx);
                                        }
                                    })),
                            )
                            .item(
                                surface_item(Icon::new(IconName::Link2), "Linked pull requests", "L")
                                    .disabled(!linked)
                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                        this.open_surface(Surface::PullRequests, window, cx);
                                    })),
                            );
                        match side_chat {
                            Some(available) => menu.item(
                                surface_item(Icon::new(IconName::MessagesSquare), "Side chat", "S")
                                    .disabled(!available)
                                    .on_click(window.listener_for(&view, |this, _, window, cx| {
                                        this.add_side_chat_surface(window, cx);
                                    })),
                            ),
                            None => menu,
                        }
                    })
                    .open(add_menu_open)
                    .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                        this.right_panel.add_menu_open = *open;
                        cx.notify();
                    })),
            )
            .when_some(layout_controls, |el, controls| el.child(div().flex_1()).child(controls))
    }

    /// A tab (`h-6 rounded-md pl-1.5 pr-2 text-xs`), its icon turning into a close button on hover
    /// or keyboard focus. The close button and the title are separate tab stops, as the web's
    /// two `<button>`s are.
    fn render_surface_tab(
        &self,
        surface: &Surface,
        active: bool,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (key, title, glyph): (SharedString, SharedString, AnyElement) = match surface {
            Surface::Diff => {
                ("diff".into(), "Diff".into(), icon("icons/file-diff.svg").with_size(px(12.)).into_any_element())
            }
            Surface::Browser(tab) => (
                format!("browser:{tab}").into(),
                self.browser_tab_title(tab),
                Icon::new(IconName::Earth).size(px(12.)).into_any_element(),
            ),
            Surface::Files => {
                ("files".into(), "Files".into(), Icon::new(IconName::Files).size(px(12.)).into_any_element())
            }
            Surface::File(path) => {
                (format!("file:{path}").into(), path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned().into(), {
                    let (asset, color) = crate::file_icons::file_icon(path, self.dark_theme(cx));
                    Icon::empty().path(asset).size(px(12.)).text_color(color).into_any_element()
                })
            }
            Surface::Attachment(file) => (
                format!("attachment:{}", file.id).into(),
                { file.name.rsplit(['/', '\\']).next().unwrap_or(&file.name).to_owned().into() },
                {
                    let (asset, color) = crate::file_icons::file_icon(&file.name, self.dark_theme(cx));
                    Icon::empty().path(asset).size(px(12.)).text_color(color).into_any_element()
                },
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
            Surface::PullRequest(reference) => (
                format!("pull-request:{}:{}#{}", reference.project_id, reference.repository, reference.number).into(),
                format!("#{}", reference.number).into(),
                Icon::new(IconName::GitPullRequest).size(px(12.)).into_any_element(),
            ),
            Surface::PullRequests => (
                "pull-requests".into(),
                "Pull requests".into(),
                Icon::new(IconName::Link2).size(px(12.)).into_any_element(),
            ),
            Surface::Terminal(id) => (
                format!("terminal:{id}").into(),
                self.terminal_surface_title(id),
                icon("icons/square-terminal.svg").with_size(px(12.)).into_any_element(),
            ),
        };
        let group: SharedString = format!("right-panel-tab:{key}").into();
        let tooltip: SharedString = format!("Close {title}").into();
        let (accent, foreground, muted) = (tokens.accent, tokens.foreground, tokens.muted);
        let (close, activate, middle, menu_surface) =
            (surface.clone(), surface.clone(), surface.clone(), surface.clone());
        let title_activate = surface.clone();
        // `group-focus-visible/close`: a keyboard-focused close button shows its X.
        let close_focus = window
            .use_keyed_state(ElementId::Name(format!("{group}:close-focus").into()), cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        let close_focused = close_focus.is_focused(window) && window.last_input_was_keyboard();
        // The `outline-ring/50` focus outline, drawn 2px inside each button.
        let ring = tokens.ring.opacity(0.5);
        let (dragged, drop_target, drag_title) = (surface.clone(), surface.clone(), title.clone());
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
                    .track_focus(&close_focus)
                    .border_2()
                    .border_color(gpui_kit::transparent_black())
                    .focus_visible(move |style| style.border_color(ring))
                    .hover(move |style| style.bg(muted))
                    .child(
                        div()
                            .group_hover(group.clone(), gpui_kit::Styled::invisible)
                            .when(close_focused, gpui_kit::Styled::invisible)
                            .child(glyph),
                    )
                    .child(
                        div()
                            .absolute()
                            .invisible()
                            .group_hover(group.clone(), gpui_kit::Styled::visible)
                            .when(close_focused, gpui_kit::Styled::visible)
                            .child(icon("icons/x.svg").with_size(px(12.))),
                    )
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_surface(close.clone(), window, cx);
                    })),
            )
            .child(
                div()
                    .id(ElementId::Name(format!("{group}:title").into()))
                    .tab_index(0)
                    .min_w_0()
                    .truncate()
                    .mx(px(-2.))
                    .rounded(px(4.))
                    .border_2()
                    .border_color(gpui_kit::transparent_black())
                    .focus_visible(move |style| style.border_color(ring))
                    .child(title)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.activate_surface(title_activate.clone(), window, cx);
                    })),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| this.close_surface(middle.clone(), window, cx)),
            )
            .on_click(cx.listener(move |this, _, window, cx| this.activate_surface(activate.clone(), window, cx)))
            .on_drag(DraggedTab(dragged), move |_, _, _, cx| {
                let title = drag_title.clone();
                cx.new(|_| TabDragPreview { title, colors: (accent, foreground) })
            })
            .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(accent.opacity(0.6)))
            .on_drop(cx.listener(move |this, DraggedTab(surface): &DraggedTab, _, cx| {
                if let Some(panel) = this.panel_state_mut() {
                    panel.move_surface(surface, &drop_target);
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_tab_menu(&menu_surface, event.position, window, cx);
                }),
            )
            .into_any_element()
    }

    /// `RightPanelEmptyState`: "Open a surface" with the surfaces this client has.
    fn render_launcher(&self, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let hover = tokens.accent.opacity(0.6);
        let actions = self.launcher_actions();
        let highlighted = self
            .right_panel
            .launcher_highlight
            .filter(|_| !actions.is_empty())
            .and_then(|index| actions.get(index.min(actions.len() - 1)).copied());
        let row = |id: &'static str,
                   glyph: AnyElement,
                   label: &'static str,
                   key: &'static str,
                   available: bool,
                   hint: &'static str| {
            let shortcut = SurfaceShortcut::ALL.into_iter().find(|s| s.letter() == key);
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
                        el.cursor_pointer()
                            .hover(move |style| style.bg(hover))
                            .when(shortcut.is_some() && shortcut == highlighted, |el| el.bg(hover))
                    } else {
                        el.opacity(0.5).tooltip(move |window, cx| Tooltip::new(hint).build(window, cx))
                    }
                })
        };
        let browser_available = self.browser_available();
        let browser = row(
            "right-panel-open-browser",
            Icon::new(IconName::Earth).size(px(16.)).into_any_element(),
            "Browser",
            "B",
            browser_available,
            "Only available in a thread.",
        )
        .when(browser_available, |el| el.on_hover(self.launcher_hover(SurfaceShortcut::Browser, cx)))
        .when(browser_available, |el| {
            el.on_click(cx.listener(|this, _, window, cx| this.add_browser_surface(window, cx)))
        });
        // "Open browser in a profile", beside the shortcut while there is more than one.
        let profiles = self.browser_profiles();
        let browser = if browser_available && profiles.len() > 1 {
            let view = cx.entity();
            div().relative().child(browser).child(
                div().absolute().top(px(4.)).right(px(32.)).child(
                    Button::new("right-panel-browser-profiles")
                        .custom(
                            ButtonCustomVariant::new(cx)
                                .foreground(tokens.muted_foreground)
                                .hover(tokens.accent)
                                .active(tokens.accent),
                        )
                        .xsmall()
                        .size(px(24.))
                        .rounded(px(6.))
                        .icon(icon("icons/chevron-down.svg"))
                        .tooltip("Open browser in a profile")
                        .dropdown_menu(move |menu: PopupMenu, window, _| {
                            profile_items(menu.max_w(px(224.)), &profiles, &view, window)
                        })
                        .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                            this.right_panel.profiles_menu_open = *open;
                            cx.notify();
                        })),
                ),
            )
        } else {
            div().child(browser)
        };
        let terminal_available = self.terminal_available();
        let terminal = row(
            "right-panel-open-terminal",
            icon("icons/square-terminal.svg").with_size(px(16.)).into_any_element(),
            "Terminal",
            "T",
            terminal_available,
            "Available when a project is open.",
        )
        .when(terminal_available, |el| el.on_hover(self.launcher_hover(SurfaceShortcut::Terminal, cx)))
        .when(terminal_available, |el| {
            el.on_click(cx.listener(|this, _, window, cx| this.add_terminal_surface(window, cx)))
        });
        let files_available = self.project_target().is_some();
        let files = row(
            "right-panel-open-files",
            Icon::new(IconName::Files).size(px(16.)).into_any_element(),
            "Files",
            "F",
            files_available,
            "Available when a project is open.",
        )
        .when(files_available, |el| el.on_hover(self.launcher_hover(SurfaceShortcut::Files, cx)))
        .when(files_available, |el| {
            el.on_click(cx.listener(|this, _, window, cx| this.open_surface(Surface::Files, window, cx)))
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
        .when(diff_available, |el| el.on_hover(self.launcher_hover(SurfaceShortcut::Diff, cx)))
        .when(diff_available, |el| el.on_click(cx.listener(|this, _, window, cx| this.open_changes(window, cx))));
        let target = self.thread_pull_request_target();
        let pull_request = row(
            "right-panel-open-pull-request",
            Icon::new(IconName::GitPullRequest).size(px(16.)).into_any_element(),
            "Pull request",
            "P",
            target.is_some(),
            "No pull request on this branch yet.",
        )
        .when(target.is_some(), |el| el.on_hover(self.launcher_hover(SurfaceShortcut::PullRequest, cx)))
        .when_some(target, |el, reference| {
            el.on_click(cx.listener(move |this, _, window, cx| {
                this.open_surface(Surface::PullRequest(reference.clone()), window, cx);
            }))
        });
        let linked = self.thread_links_pull_requests();
        let linked_pull_requests = row(
            "right-panel-open-pull-requests",
            Icon::new(IconName::Link2).size(px(16.)).into_any_element(),
            "Linked pull requests",
            "L",
            linked,
            "No linked pull requests available.",
        )
        .when(linked, |el| el.on_hover(self.launcher_hover(SurfaceShortcut::LinkedPullRequests, cx)))
        .when(linked, |el| {
            el.on_click(cx.listener(|this, _, window, cx| this.open_surface(Surface::PullRequests, window, cx)))
        });
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
                el.on_hover(self.launcher_hover(SurfaceShortcut::SideChat, cx))
                    .on_click(cx.listener(|this, _, window, cx| this.add_side_chat_surface(window, cx)))
            })
        });
        // `tabIndex={0}`: one Tab stop whose arrows and Enter drive the rows.
        div()
            .id("right-panel-launcher")
            .track_focus(&self.right_panel.launcher_focus)
            .on_key_down(cx.listener(Self::launcher_key))
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
                    .child(
                        v_flex()
                            .gap(px(2.))
                            .child(browser)
                            .child(terminal)
                            .child(files)
                            .child(diff)
                            .child(pull_request)
                            .child(linked_pull_requests)
                            .children(side_chat),
                    ),
            )
            .into_any_element()
    }

    /// `DiffPanel` embedded in the inline panel: the scope menu and view toggles, then the patch.
    fn render_diff_surface(
        &self,
        scope: &DiffScope,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
                .map_or_else(|| format!("Commit {}", &sha[..sha.len().min(7)]), |c| c.subject.clone())
                .into(),
            DiffScope::ToolCall { .. } => "Tool call".into(),
        };
        let section = diff_comments::section(&scope, &turns, &scope_label);
        let view = cx.entity();
        let menu_scope = scope.clone();
        let menu_turns = turns;
        let menu_latest = latest;
        let identity = self.target_project().and_then(|project| project.repository_identity.clone());
        let menu_identity = identity.clone();
        // `headerCommitSha`: the picked commit's short SHA beside the menu, linked to its host.
        let header_sha = match &scope {
            DiffScope::Commit(sha) => {
                let url = identity.as_ref().and_then(|identity| commit_link::commit_url(identity, sha));
                Some(commit_sha_link("diff-commit-sha", sha, url, &view, None, tokens))
            }
            _ => None,
        };
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
                            .on_click(window.listener_for(
                                &turn_view,
                                move |this, _, _, cx| {
                                    this.set_diff_scope(value.clone(), cx);
                                },
                            )),
                        );
                    }
                    menu
                });
                let (commits, scope, commit_view) = (commits.clone(), menu_scope.clone(), view.clone());
                let identity = menu_identity.clone();
                menu.submenu("Commits", window, cx, move |mut menu, window, cx| {
                    if commits.is_empty() {
                        return menu.item(PopupMenuItem::new("No commits on this branch.").disabled(true));
                    }
                    let now = crate::timestamp::now_ms();
                    let popup = cx.entity().downgrade();
                    for commit in &commits {
                        let value = DiffScope::Commit(commit.sha.clone());
                        let checked = scope == value;
                        let url = identity.as_ref().and_then(|identity| commit_link::commit_url(identity, &commit.sha));
                        let time: SharedString = commit
                            .committed_at
                            .as_deref()
                            .and_then(crate::timestamp::parse_ms)
                            .map(|at| crate::timestamp::relative_label(now - at))
                            .unwrap_or_default()
                            .into();
                        let row = CommitRow {
                            subject: commit.subject.clone().into(),
                            time,
                            sha: commit.sha.clone(),
                            url,
                            view: commit_view.clone(),
                            menu: popup.clone(),
                        };
                        menu =
                            menu.item(PopupMenuItem::element(move |_, cx| row.render(cx)).checked(checked).on_click(
                                window.listener_for(&commit_view, move |this, _, _, cx| {
                                    this.set_diff_scope(value.clone(), cx);
                                }),
                            ));
                    }
                    menu.max_h(px(320.)).max_w(px(384.)).scrollable(true)
                })
            });
        let scheme = self.diff_color_scheme();
        let (stats, truncated, pending, omitted) = match &self.right_panel.load {
            DiffLoad::Ready { additions, deletions, truncated, pending, omitted, .. } => {
                (Some((*additions, *deletions)), *truncated, *pending, *omitted)
            }
            DiffLoad::Raw { truncated, .. } | DiffLoad::Recorded { truncated, .. } => (None, *truncated, 0, None),
            _ => (None, false, 0, None),
        };
        // `comparisonHeadRef` / `comparisonBaseRef`, with the base as its picker.
        let refs = matches!(scope, DiffScope::Branch).then(|| {
            let (head, base) = match &self.right_panel.load {
                DiffLoad::Ready { head, base, .. } => (head.clone(), base.clone()),
                _ => (None, None),
            };
            let head = head
                .or_else(|| self.vcs.status.as_ref().and_then(|s| s.local.ref_name.clone()))
                .unwrap_or_else(|| "HEAD".into());
            let base = base.or_else(|| self.diff_base_ref().map(str::to_owned)).unwrap_or_else(|| "Automatic".into());
            // Under 20rem the head drops out and the base reads "vs …"; unmeasured, wait one frame,
            // since a narrow panel can lay the area out at zero for good.
            let refs_width = self.right_panel.diff_refs.bounds().size.width;
            if wait_for_measure(refs_width, &self.right_panel.diff_refs_waited) {
                window.request_animation_frame();
            }
            let narrow = refs_width < window.rem_size() * 20.;
            self.render_base_ref_picker(&head, &base, narrow, tokens, window, cx)
        });
        let can_refresh = matches!(scope, DiffScope::Branch | DiffScope::Unstaged | DiffScope::Commit(_));
        let (shows_files, all_collapsed) = {
            let state = self.right_panel.diff.read(cx);
            let shows = matches!(self.right_panel.load, DiffLoad::Ready { .. }) && !state.files().is_empty();
            (shows, shows && state.files().iter().all(|file| state.is_file_collapsed(file.path())))
        };
        let (wrap, ignore_whitespace) = (self.diff_wrap(), self.diff_ignore_whitespace());
        let (test_files, hide_tests, tree_open) =
            (self.right_panel.diff_test_files, self.right_panel.hide_test_files, self.right_panel.tree_open);
        let has_files = !self.right_panel.all_diff_files.is_empty();
        let focus = &self.right_panel.toolbar_focus;
        let header = h_flex()
            .h(px(28.))
            .mb_3()
            .px_2()
            .gap_2()
            .justify_between()
            .child(
                h_flex()
                    .id("diff-refs-area")
                    .track_scroll(&self.right_panel.diff_refs)
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .child(scope_menu)
                    .children(header_sha)
                    .children(refs),
            )
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
                            icon_toggle("diff-refresh", "icons/refresh-cw.svg", false, tokens, &focus.refresh)
                                .tooltip(|window, cx| Tooltip::new("Refresh diff").build(window, cx))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.right_panel.refresh += 1;
                                    cx.notify();
                                })),
                        )
                    })
                    .when(shows_files, |el| {
                        let label = if all_collapsed { "Expand all files" } else { "Collapse all files" };
                        let path =
                            if all_collapsed { "icons/chevrons-up-down.svg" } else { "icons/chevrons-down-up.svg" };
                        el.child(
                            icon_toggle("diff-collapse", path, false, tokens, &focus.collapse)
                                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let shown: Vec<SharedString> = this
                                        .right_panel
                                        .diff
                                        .read(cx)
                                        .files()
                                        .iter()
                                        .map(|file| file.path().clone())
                                        .collect();
                                    let mut folded = this.current_folds();
                                    if all_collapsed {
                                        folded.retain(|path| !shown.contains(path));
                                    } else {
                                        folded.extend(shown);
                                    }
                                    this.set_diff_folds(folded, cx);
                                })),
                        )
                    })
                    .child(self.render_layout_toggle(tokens, cx))
                    .child({
                        let label = if wrap { "Disable line wrapping" } else { "Enable line wrapping" };
                        icon_toggle("diff-wrap", "icons/text-wrap.svg", wrap, tokens, &focus.wrap)
                            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.right_panel.wrap = Some(!wrap);
                                cx.notify();
                            }))
                    })
                    .child({
                        let label =
                            if ignore_whitespace { "Show whitespace changes" } else { "Hide whitespace changes" };
                        icon_toggle(
                            "diff-whitespace",
                            "icons/pilcrow.svg",
                            ignore_whitespace,
                            tokens,
                            &focus.whitespace,
                        )
                        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.right_panel.ignore_whitespace = Some(!ignore_whitespace);
                            cx.notify();
                        }))
                    })
                    .when(test_files > 0, |el| {
                        let label: SharedString = match (hide_tests, test_files) {
                            (true, 1) => "Show 1 hidden test file".into(),
                            (true, count) => format!("Show {count} hidden test files").into(),
                            (false, _) => "Hide test files".into(),
                        };
                        el.child(
                            icon_toggle(
                                "diff-hide-tests",
                                "icons/flask-conical-off.svg",
                                hide_tests,
                                tokens,
                                &focus.tests,
                            )
                            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.right_panel.hide_test_files = !hide_tests;
                                this.apply_diff_files(cx);
                            })),
                        )
                    })
                    .when(has_files, |el| {
                        let label = if tree_open { "Hide file tree" } else { "Show file tree" };
                        el.child(
                            icon_toggle("diff-file-tree", "icons/folder-tree.svg", tree_open, tokens, &focus.tree)
                                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.right_panel.tree_open = !tree_open;
                                    cx.notify();
                                })),
                        )
                    }),
            );
        // `bg-muted/40` notices above the patch.
        let notice = |text: SharedString| {
            div()
                .px_3()
                .py(px(6.))
                .border_b_1()
                .border_color(tokens.border.opacity(0.7))
                .bg(tokens.muted.opacity(0.4))
                .text_size(px(11.))
                .text_color(tokens.muted_foreground)
                .child(text)
        };
        let truncated_notice =
            truncated.then(|| notice("This preview exceeds the size limit. Changes shown are incomplete.".into()));
        let omitted_notice = omitted.map(|(recorded, omitted)| {
            notice(
                format!("This tool call changed {recorded} files. The recorded patch omits {omitted} of them.").into(),
            )
        });
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
                    DiffScope::ToolCall { .. } => "Loading tool call diff...",
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
            DiffLoad::Raw { reason, text, .. } => v_flex()
                .flex_1()
                .min_h_0()
                .children(truncated_notice)
                .child(raw_patch(reason, text, wrap, tokens))
                .into_any_element(),
            DiffLoad::Recorded { content, .. } => v_flex()
                .flex_1()
                .min_h_0()
                .children(truncated_notice)
                .child(recorded_contents(content, wrap, tokens))
                .into_any_element(),
            DiffLoad::Ready { .. } => v_flex()
                .flex_1()
                .min_h_0()
                .children(truncated_notice)
                .children(omitted_notice)
                .child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .items_start()
                        .relative()
                        .children(super::theme_inspect::probe(&["codeBackground", "codeForeground"], cx))
                        .child({
                            // `--diffs-font-size`, the Code font size.
                            let stats = self.right_panel.patches.as_ref().map(|patches| patches.stats.clone());
                            let (states, state, view, header_tokens) = (
                                self.right_panel.patch_states.clone(),
                                self.right_panel.diff.clone(),
                                cx.entity(),
                                *tokens,
                            );
                            // A custom header replaces the kit's, suffix included, so the file's load status goes in it.
                            let diff = Diff::new(&self.right_panel.diff)
                                .render_header(move |file, _, cx| {
                                    let status =
                                        states.get(file.path()).map(|load| diff_file_status(file.path(), *load, &view));
                                    let stat = stats.as_ref().and_then(|stats| stats.get(file.path()));
                                    diff_open::header(
                                        file,
                                        stat,
                                        state.clone(),
                                        view.clone(),
                                        &header_tokens,
                                        scheme,
                                        status,
                                        cx,
                                    )
                                })
                                .hunk_separator(DiffHunkSeparator::LineInfo)
                                .change_indicator(DiffChangeIndicator::Bars)
                                .soft_wrap(wrap)
                                .flex_1()
                                .min_h_0()
                                .text_size(px(tokens.fonts.code_size));
                            let diff = self.with_diff_comments(diff, section, tokens, cx);
                            match scheme {
                                DiffColorScheme::RedGreen => diff.into_any_element(),
                                DiffColorScheme::BlueOrange => DiffTint {
                                    child: diff.into_any_element(),
                                    colors: (git::addition_color(tokens, scheme), git::deletion_color(tokens, scheme)),
                                }
                                .into_any_element(),
                            }
                        })
                        .when(tree_open, |el| {
                            let row = self.right_panel_width(window).unwrap_or_default();
                            el.child(self.render_diff_tree(
                                diff_tree::TreeOwner::Changes,
                                row,
                                None,
                                tokens,
                                window,
                                cx,
                            ))
                        }),
                )
                // `DiffFileLoadingBoundary` while file patches are still on their way.
                .when(pending > 0, |el| {
                    el.child(
                        div()
                            .px_3()
                            .py(px(6.))
                            .border_t_1()
                            .border_color(tokens.border.opacity(0.4))
                            .text_size(px(11.))
                            .text_color(tokens.muted_foreground)
                            .child("Loading diff…"),
                    )
                })
                .into_any_element(),
        };
        v_flex().flex_1().min_h_0().child(header).child(body).into_any_element()
    }

    /// The stacked / split segmented toggle (`ToggleGroup variant="segmented"`), saved as `diffLayout`.
    fn render_layout_toggle(&self, tokens: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let split = self.diff_mode() == DiffMode::Split;
        let focus = &self.right_panel.toolbar_focus;
        let ring = tokens.ring;
        let segment = |id: &'static str, path: &'static str, pressed: bool, value: bool, cx: &mut Context<Self>| {
            let pressed_bg = tokens.input.opacity(0.72);
            let hover = tokens.input.opacity(0.32);
            // One Tab stop on the pressed segment; the arrows, Home and End move between them.
            let own = if value { &focus.split } else { &focus.stacked };
            let (stacked, split_focus) = (focus.stacked.clone(), focus.split.clone());
            div()
                .id(id)
                .track_focus(&own.clone().tab_stop(pressed))
                .focus_visible(move |s| s.border_2().border_color(ring))
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    let to_split = match event.keystroke.key.as_str() {
                        _ if event.keystroke.modifiers.modified() => return,
                        "left" | "right" | "up" | "down" => !value,
                        "home" => false,
                        "end" => true,
                        _ => return,
                    };
                    window.focus(if to_split { &split_focus } else { &stacked }, cx);
                    cx.stop_propagation();
                })
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
                    this.client_settings.apply(&serde_json::Map::from_iter([("diffLayout".into(), layout.into())]));
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

/// A tool call's patch the viewer can't read: why, then its text (`renderablePatch.kind === "raw"`).
fn raw_patch(reason: &'static str, text: &SharedString, wrap: bool, tokens: &Tokens) -> impl IntoElement {
    let block = div()
        .id("diff-raw-patch")
        .rounded(px(6.))
        .border_1()
        .border_color(tokens.border.opacity(0.7))
        .bg(tokens.background.opacity(0.7))
        .p_3()
        .font_family(tokens.fonts.mono)
        .text_size(px(11.))
        .line_height(gpui_kit::relative(1.625))
        .text_color(tokens.muted_foreground.opacity(0.9))
        .when(!wrap, gpui_kit::Styled::whitespace_nowrap)
        .overflow_x_scroll()
        .child(gpui_kit::base::SelectableText::new("diff-raw-patch-text", text.clone()));
    div().id("diff-raw").flex_1().min_h_0().overflow_y_scroll().p_2().child(
        v_flex()
            .gap_2()
            .child(div().text_size(px(11.)).text_color(tokens.muted_foreground.opacity(0.75)).child(reason))
            .child(block),
    )
}

/// File contents a tool call recorded without a diff, each under its label.
fn recorded_contents(content: &[(&'static str, SharedString)], wrap: bool, tokens: &Tokens) -> impl IntoElement {
    let entries = content.iter().enumerate().map(|(index, (label, text))| {
        let text = if text.is_empty() { SharedString::from("Empty content.") } else { text.clone() };
        v_flex().gap_1().child(div().text_color(tokens.muted_foreground).child(*label)).child(
            div()
                .id(ElementId::NamedInteger("diff-recorded".into(), index as u64))
                .font_family(tokens.fonts.mono)
                .text_size(px(11.))
                .when(!wrap, gpui_kit::Styled::whitespace_nowrap)
                .overflow_x_scroll()
                .child(gpui_kit::base::SelectableText::new(
                    ElementId::NamedInteger("diff-recorded-text".into(), index as u64),
                    text,
                )),
        )
    });
    v_flex()
        .id("diff-recorded-contents")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .p_3()
        .gap_3()
        .text_xs()
        .child(div().text_color(tokens.muted_foreground).child("The provider recorded file content without a diff."))
        .children(entries)
}

/// A menu row's 16px icon (`sm:size-4`, `-mx-0.5`): the menu lays icons out in a 12px box, so
/// the glyph is scaled up and nudged left to clear the label.
fn menu_icon(icon: Icon) -> Icon {
    icon.transform(
        gpui_kit::Transformation::scale(gpui_kit::size(4. / 3., 4. / 3.))
            .with_translation(gpui_kit::point(px(-2.), px(0.))),
    )
}

/// A "+" menu row (`SurfaceMenuItem`): icon, label, and its shortcut letter at the right.
pub(super) fn surface_item(icon: Icon, label: &'static str, shortcut: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, cx| {
        h_flex().w_full().gap_2().child(label).child(
            div()
                .ml_auto()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(theme::tokens(cx).muted_foreground)
                .child(shortcut),
        )
    })
    .icon(menu_icon(icon))
}

/// One item per browser profile, each opening a new tab in it.
fn profile_items(
    mut menu: PopupMenu,
    profiles: &[crate::preview::Profile],
    view: &Entity<Workspace>,
    window: &mut Window,
) -> PopupMenu {
    for profile in profiles {
        let id = profile.id.clone();
        menu = menu.item(PopupMenuItem::new(profile.name.clone()).on_click(window.listener_for(
            view,
            move |this, _, window, cx| {
                this.add_browser_surface_in(Some(id.clone()), window, cx);
            },
        )));
    }
    menu
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
    /// The pull requests the thread linked at the last read; `None` before the first.
    target_key: Option<Option<String>>,
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
        target_key: same.and_then(|previous| previous.target_key.clone()),
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
pub(super) struct DiffTint {
    pub(super) child: AnyElement,
    pub(super) colors: (Hsla, Hsla),
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
        (): &mut (),
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
        (): &mut (),
        (): &mut (),
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
    /// Files whose patches load one by one (`lazySource`); `files` is empty then.
    deferred: Vec<diff_load::FileStat>,
    /// `sourceLineStat`: the totals of every file, when the source lists them.
    totals: Option<(usize, usize)>,
    head: Option<String>,
    base: Option<String>,
    truncated: bool,
}

/// `review.getBranchCommits`' commits, newest first as the server lists them.
fn parse_commits(reply: &Value) -> Vec<Commit> {
    reply["commits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some(Commit {
                sha: c["sha"].as_str()?.into(),
                subject: c["subject"].as_str()?.into(),
                committed_at: c["committedAt"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

fn parse_diff(value: &Value, source_kind: Option<&str>) -> Option<ParsedDiff> {
    let Some(kind) = source_kind else {
        let files = diff_load::parse_patch(value["diff"].as_str()?, false);
        return Some(ParsedDiff {
            files,
            deferred: Vec::new(),
            totals: None,
            head: None,
            base: None,
            truncated: false,
        });
    };
    let source = value["sources"].as_array()?.iter().find(|s| s["kind"].as_str() == Some(kind))?;
    let text = |key: &str| source[key].as_str().map(str::to_owned);
    let truncated = source["truncated"].as_bool() == Some(true);
    let deferred = diff_load::deferred_files(source);
    let files =
        if deferred.is_empty() { diff_load::parse_patch(source["diff"].as_str()?, truncated) } else { Vec::new() };
    let totals = diff_load::file_stats(source)
        .map(|stats| stats.iter().fold((0, 0), |(a, d), file| (a + file.additions, d + file.deletions)));
    // A lazily loaded source shows every file, so the size notice no longer applies.
    let truncated = truncated && deferred.is_empty();
    Some(ParsedDiff { files, deferred, totals, head: text("headRef"), base: text("baseRef"), truncated })
}

/// Whether showing the diff reads it again: only a diff already read, with no read on its way,
/// whose last read is older than [`REVALIDATE_FLOOR`].
fn should_revalidate(read_at: Option<std::time::Instant>, reading: bool, now: std::time::Instant) -> bool {
    !reading && read_at.is_some_and(|at| now.saturating_duration_since(at) >= REVALIDATE_FLOOR)
}

/// `HH:MM` from an ISO timestamp, as the turn menu labels completion.
fn short_time(iso: &str) -> SharedString {
    iso.get(11..16).unwrap_or(iso).to_owned().into()
}

/// `DiffFileStatus`: a retry for a file whose patch did not load, or a note on a partial one.
fn diff_file_status(path: &SharedString, state: diff_load::FileState, view: &Entity<Workspace>) -> AnyElement {
    let failed = state == diff_load::FileState::Failed;
    let (icon_path, tooltip) = if failed {
        ("icons/rotate-cw.svg", "Retry loading diff")
    } else {
        ("icons/info.svg", "This file is too large to show in full. Counts include all changes.")
    };
    let (view, path) = (view.clone(), path.clone());
    Button::new(ElementId::Name(format!("diff-file-status:{path}").into()))
        .ghost()
        .xsmall()
        .icon(icon(icon_path).with_size(px(12.)))
        .tooltip(tooltip)
        .on_click(move |_, _, cx| {
            // Inside the file header, whose click folds the file.
            cx.stop_propagation();
            if failed {
                view.update(cx, |this, cx| this.retry_file_patch(&path, cx));
            }
        })
        .into_any_element()
}

/// Whether to spend a frame waiting for an area to be measured: once at most, so an area laid
/// out at zero width doesn't repaint every frame.
fn wait_for_measure(width: Pixels, waited: &std::cell::Cell<bool>) -> bool {
    if width > px(0.) {
        waited.set(false);
        return false;
    }
    !waited.replace(true)
}

/// A 28px ghost icon toggle (`Toggle variant="ghost" size="sm"` / `Button size="icon-sm"`).
/// A commit's short SHA (`font-mono text-xs text-muted-foreground`), underlined as a link to
/// its host's page when it has one; the link opens without picking the commit, and closes
/// `menu` when it sits in one.
fn commit_sha_link(
    id: impl Into<ElementId>,
    sha: &str,
    url: Option<String>,
    view: &Entity<Workspace>,
    menu: Option<gpui_kit::WeakEntity<PopupMenu>>,
    tokens: &Tokens,
) -> AnyElement {
    let short: SharedString = sha.chars().take(7).collect::<String>().into();
    let base =
        div().id(id.into()).flex_none().font_family(tokens.fonts.mono).text_xs().text_color(tokens.muted_foreground);
    let Some(url) = url else { return base.child(short).into_any_element() };
    let (view, tip) = (view.clone(), SharedString::from(url.clone()));
    base.underline()
        .cursor_pointer()
        .child(short)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            view.update(cx, |this, cx| this.open_link(&url, event, window, cx));
            if let Some(menu) = menu.as_ref().and_then(gpui_kit::WeakEntity::upgrade) {
                cx.defer(move |cx| menu.update(cx, |_, cx| cx.emit(DismissEvent)));
            }
        })
        .into_any_element()
}

/// A Commits menu row: the subject (with its tooltip), how long ago, and the SHA link.
#[derive(Clone)]
struct CommitRow {
    subject: SharedString,
    time: SharedString,
    sha: String,
    url: Option<String>,
    view: Entity<Workspace>,
    menu: gpui_kit::WeakEntity<PopupMenu>,
}

impl CommitRow {
    fn render(&self, cx: &App) -> AnyElement {
        let tokens = theme::tokens(cx);
        let subject = self.subject.clone();
        let link = commit_sha_link(
            ElementId::Name(format!("diff-commit-link:{}", self.sha).into()),
            &self.sha,
            self.url.clone(),
            &self.view,
            Some(self.menu.clone()),
            tokens,
        );
        h_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .child(
                h_flex()
                    .id(ElementId::Name(format!("diff-commit-row:{}", self.sha).into()))
                    .flex_1()
                    .min_w_0()
                    .gap_3()
                    .child(div().flex_1().min_w_0().truncate().child(self.subject.clone()))
                    .child(div().flex_none().text_xs().text_color(tokens.muted_foreground).child(self.time.clone()))
                    .tooltip(move |window, cx| Tooltip::new(subject.clone()).build(window, cx)),
            )
            .child(div().flex_none().px_2().child(link))
            .into_any_element()
    }
}

/// A toolbar button or toggle: a Tab stop that Enter and Space press, ringed while focused by key.
fn icon_toggle(
    id: &'static str,
    path: &'static str,
    pressed: bool,
    tokens: &Tokens,
    focus: &FocusHandle,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let (accent, ring) = (tokens.accent, tokens.ring);
    div()
        .id(id)
        .track_focus(focus)
        .focus_visible(move |s| s.border_2().border_color(ring))
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

/// A "+" menu surface by its shortcut letter (`SurfaceMenuItem`'s `shortcut`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SurfaceShortcut {
    Browser,
    Terminal,
    Files,
    Diff,
    PullRequest,
    LinkedPullRequests,
    SideChat,
}

impl SurfaceShortcut {
    /// The launcher's and the "+" menu's order.
    const ALL: [Self; 7] = [
        Self::Browser,
        Self::Terminal,
        Self::Files,
        Self::Diff,
        Self::PullRequest,
        Self::LinkedPullRequests,
        Self::SideChat,
    ];

    /// The letter its rows show.
    fn letter(self) -> &'static str {
        match self {
            Self::Browser => "B",
            Self::Terminal => "T",
            Self::Files => "F",
            Self::Diff => "D",
            Self::PullRequest => "P",
            Self::LinkedPullRequests => "L",
            Self::SideChat => "S",
        }
    }

    /// `surfaceShortcutActionForKey`: Ctrl, Alt and Cmd chords are commands, not letters.
    fn for_keystroke(keystroke: &gpui_kit::Keystroke) -> Option<Self> {
        let modifiers = &keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return None;
        }
        Some(match keystroke.key.as_str() {
            "b" => Self::Browser,
            "t" => Self::Terminal,
            "f" => Self::Files,
            "d" => Self::Diff,
            "p" => Self::PullRequest,
            "l" => Self::LinkedPullRequests,
            "s" => Self::SideChat,
            _ => return None,
        })
    }
}

/// What a key does in the focused launcher.
#[derive(Debug, PartialEq, Eq)]
enum LauncherStep {
    Highlight(usize),
    Activate,
}

/// `RightPanelEmptyState`'s `handleKeyDown` over `len` available rows: Down and Right move the
/// highlight on and Up and Left back, wrapping; Enter opens the highlighted row.
fn launcher_step(keystroke: &gpui_kit::Keystroke, highlight: Option<usize>, len: usize) -> Option<LauncherStep> {
    let modifiers = &keystroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform || len == 0 {
        return None;
    }
    Some(match keystroke.key.as_str() {
        "down" | "right" => LauncherStep::Highlight(highlight.map_or(0, |index| (index + 1) % len)),
        "up" | "left" => LauncherStep::Highlight(highlight.map_or(len - 1, |index| (index + len - 1) % len)),
        "enter" => LauncherStep::Activate,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ThreadState;

    #[test]
    fn showing_the_diff_reads_it_again_only_after_the_floor() {
        let now = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let read = |ago: u64| now.checked_sub(std::time::Duration::from_secs(ago));
        assert!(should_revalidate(read(11), false, now));
        assert!(should_revalidate(read(10), false, now));
        assert!(!should_revalidate(read(3), false, now), "a mutation's refetch counts as the revalidation");
        assert!(!should_revalidate(read(60), true, now), "a read on its way is about to land");
        assert!(!should_revalidate(None, false, now), "the first read is the mount's own");
    }

    #[test]
    fn a_dragged_tab_takes_the_place_of_the_tab_it_drops_on() {
        let mut panel = PanelState {
            surfaces: vec![Surface::Diff, Surface::Files, Surface::PullRequests, Surface::Terminal("t".into())],
            ..PanelState::default()
        };
        let order = |panel: &PanelState| panel.surfaces.clone();
        panel.move_surface(&Surface::Diff, &Surface::PullRequests);
        assert_eq!(
            order(&panel),
            [Surface::Files, Surface::PullRequests, Surface::Diff, Surface::Terminal("t".into())]
        );
        panel.move_surface(&Surface::Terminal("t".into()), &Surface::Files);
        assert_eq!(
            order(&panel),
            [Surface::Terminal("t".into()), Surface::Files, Surface::PullRequests, Surface::Diff]
        );
        let before = order(&panel);
        panel.move_surface(&Surface::Files, &Surface::Files);
        panel.move_surface(&Surface::SideChat("gone".into()), &Surface::Files);
        assert_eq!(order(&panel), before);
    }

    #[test]
    fn launcher_arrows_wrap_through_the_available_rows() {
        let step = |key: &'static str, highlight: Option<usize>, len: usize| {
            launcher_step(&gpui_kit::Keystroke::parse(key).unwrap(), highlight, len)
        };
        assert_eq!(step("down", None, 3), Some(LauncherStep::Highlight(0)));
        assert_eq!(step("right", Some(2), 3), Some(LauncherStep::Highlight(0)));
        assert_eq!(step("up", None, 3), Some(LauncherStep::Highlight(2)));
        assert_eq!(step("left", Some(0), 3), Some(LauncherStep::Highlight(2)));
        assert_eq!(step("up", Some(2), 3), Some(LauncherStep::Highlight(1)));
        assert_eq!(step("enter", Some(1), 3), Some(LauncherStep::Activate));
        assert_eq!(step("ctrl-down", None, 3), None);
        assert_eq!(step("down", None, 0), None);
        assert_eq!(step("t", None, 3), None);
    }

    #[test]
    fn add_menu_letters_open_surfaces_only_without_command_modifiers() {
        let shortcut = |key: &'static str| SurfaceShortcut::for_keystroke(&gpui_kit::Keystroke::parse(key).unwrap());
        assert_eq!(shortcut("f"), Some(SurfaceShortcut::Files));
        assert_eq!(shortcut("shift-f"), Some(SurfaceShortcut::Files));
        assert_eq!(shortcut("s"), Some(SurfaceShortcut::SideChat));
        assert_eq!(shortcut("ctrl-f"), None);
        assert_eq!(shortcut("alt-t"), None);
        assert_eq!(shortcut("cmd-d"), None);
        assert_eq!(shortcut("m"), None);
        assert_eq!(shortcut("enter"), None);
    }

    #[test]
    fn width_clamps_to_the_viewport_and_the_chat_column() {
        assert_eq!(clamp_width(540., 1600., 1300.), 540.);
        // 70% of a 1000px window is 700, but the chat keeps 360 of a 800px row.
        assert_eq!(clamp_width(900., 1000., 800.), 440.);
        assert_eq!(clamp_width(100., 1600., 1300.), MIN_WIDTH);
        // Too narrow for both: the panel keeps its minimum.
        assert_eq!(clamp_width(540., 600., 500.), MIN_WIDTH);
        // The web's 1138px window beside a 256px sidebar leaves 516 once the resize inset is out.
        let expected = if cfg!(windows) { 516. } else { 522. };
        assert_eq!(clamp_width(540., 1138., panel_row(1138., 256.)), expected);
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
    fn files_open_in_place_of_the_tree_and_folders_navigate_in_place() {
        let file = |path: &str| Surface::File(path.into());
        let mut panel = PanelState::default();
        panel.upsert(Surface::Diff);
        panel.upsert(Surface::Files);
        panel.open_file("src/a.rs".into());
        assert_eq!(panel.surfaces, [Surface::Diff, file("src/a.rs")], "a file replaces the standalone tree");
        panel.open_file("src/a.rs".into());
        assert_eq!(panel.surfaces.len(), 2, "reopening a file focuses its tab");
        panel.navigate_folder(&file("src/a.rs"), "src", true);
        assert_eq!(panel.surfaces, [Surface::Diff, file("src/a.rs"), file("src")]);
        assert_eq!(panel.active, Some(file("src")));
        panel.navigate_folder(&file("src"), "", false);
        assert_eq!(panel.surfaces, [Surface::Diff, file("src/a.rs"), Surface::Files], "the root is the Files tree");
        panel.navigate_folder(&Surface::Files, "src/a.rs", false);
        assert_eq!(panel.surfaces, [Surface::Diff, file("src/a.rs")], "an open target is focused, not duplicated");
        assert_eq!(panel.active, Some(file("src/a.rs")));
    }

    #[test]
    fn attachments_open_as_their_own_tabs_without_a_workspace() {
        use crate::app::files::AttachmentFile;
        let file = || AttachmentFile { id: "a1".into(), name: "notes.txt".into(), mime: "text/plain".into(), size: 3 };
        let mut panel = PanelState::default();
        panel.upsert(Surface::Files);
        panel.open_file("notes.txt".into());
        panel.upsert(Surface::Files);
        panel.open_attachment(file());
        assert_eq!(
            panel.surfaces,
            [Surface::File("notes.txt".into()), Surface::Attachment(file())],
            "an attachment is not the workspace file of its name, and replaces the standalone tree"
        );
        panel.upsert(Surface::Diff);
        panel.open_attachment(file());
        assert_eq!(panel.surfaces.len(), 3, "reopening an attachment focuses its tab");
        assert_eq!(panel.active, Some(Surface::Attachment(file())));
        panel.close(&Surface::Attachment(file()));
        assert_eq!(panel.surfaces, [Surface::File("notes.txt".into()), Surface::Diff]);
    }

    #[test]
    fn browser_tabs_follow_the_preview_sessions() {
        let browser = |tab: &str| Surface::Browser(tab.into());
        let mut panel = PanelState::default();
        panel.upsert(Surface::Diff);
        panel.upsert(browser("gone"));
        assert!(panel.reconcile_browsers(&["kept", "new"]));
        assert_eq!(panel.surfaces, [Surface::Diff, browser("kept"), browser("new")]);
        assert_eq!(panel.active, Some(browser("kept")), "a closed active tab falls back to a browser");
        assert!(panel.open);
        assert!(!panel.reconcile_browsers(&["kept", "new"]), "the same sessions change nothing to save");
        let mut only_browser = PanelState::default();
        only_browser.upsert(browser("a"));
        assert!(only_browser.reconcile_browsers(&[]));
        assert!(!only_browser.open, "losing the last session hides the panel");
        let mut restarted = PanelState::default();
        restarted.reconcile_browsers(&["left"]);
        assert_eq!(restarted.active, Some(browser("left")), "a tab listed after a restart is selected");
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
    fn an_area_measured_at_zero_waits_one_frame_not_every_frame() {
        let waited = std::cell::Cell::new(false);
        assert!(wait_for_measure(px(0.), &waited), "the first unmeasured render waits a frame");
        assert!(!wait_for_measure(px(0.), &waited), "zero after that is the real width");
        assert!(!wait_for_measure(px(0.), &waited));
        assert!(!wait_for_measure(px(120.), &waited));
        assert!(wait_for_measure(px(0.), &waited), "a later collapse to zero gets one frame again");
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
    fn proactive_pull_requests_defer_to_user_choices_and_keep_existing_tabs() {
        let mut panel = PanelState::default();
        assert!(panel.open_proactive(Surface::PullRequests, 0, false));
        // Already open: left where it is, unless following the thread's pull request.
        panel.active = Some(Surface::Diff);
        assert!(!panel.open_proactive(Surface::PullRequests, 0, false));
        assert!(panel.open_proactive(Surface::PullRequests, 0, true));
        assert_eq!(panel.active, Some(Surface::PullRequests));
        panel.user_revision = 1;
        assert!(!panel.open_proactive(Surface::Files, 0, false));
        // A pull request on screen keeps a turn diff from replacing it.
        assert!(!panel.open_proactive_diff("run-1", 1));
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

    #[test]
    fn a_patch_cut_at_the_size_limit_keeps_its_files() {
        // The server stops at its byte cap mid-hunk and appends its marker.
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,10 +1,10 @@\n-old\n+new\n context\n\n\n[truncated]";
        let value = json!({"sources": [
            {"kind": "branch-range", "diff": patch, "headRef": "main", "baseRef": "origin/main", "truncated": true},
        ]});
        let parsed = parse_diff(&value, Some("branch-range")).unwrap();
        assert_eq!(parsed.files.len(), 1);
        assert_eq!((parsed.files[0].additions(), parsed.files[0].deletions()), (1, 1));
    }
}
