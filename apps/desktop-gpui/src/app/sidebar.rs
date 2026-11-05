//! The left sidebar (`apps/web/src/components/Sidebar.tsx` and
//! `components/sidebar/*`): chrome row, search, the inbox list, and the footer.

mod actions;
pub(super) use actions::{MenuOrigin, Renaming};
mod custom_snooze;
mod drafts;
mod filter;
mod provider_update;
mod search_results;
mod selection;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, AnyView, App, AppContext as _, ClickEvent, Context, DismissEvent, DragMoveEvent, EmptyView,
    Entity, FocusHandle, Focusable as _, FontWeight, Hsla, Image, ImageFormat, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Point, Render, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled as _, StyledImage as _, Subscription, Task, WeakEntity, Window, anchored, deferred, div, img,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use serde_json::json;

use super::titlebar::{self, TITLEBAR_HEIGHT};
use super::{Connection, Target, Workspace, project_name};
use crate::inbox::{self, Facts, FolderDot, Status};
use crate::model::{ProjectShell, ShellState, ThreadShell};
use crate::project_groups::{self, GroupingSettings, ProjectGroup, ScratchRoot, SortOrder};
use crate::theme::Tokens;

const DEFAULT_WIDTH: f32 = 256.;
const MIN_WIDTH: f32 = 208.;
/// The main column never gets narrower than this by dragging.
const MIN_MAIN_WIDTH: f32 = 640.;
const SETTLED_INITIAL: usize = 10;
const SETTLED_PAGE: usize = 25;
const ROW_GROUP: &str = "sidebar-row";
const FOLDER_GROUP: &str = "sidebar-folder";
/// `bg-success` (Tailwind's emerald-500), the done dot of a closed folder.
const SUCCESS: u32 = 0x00bc7d;

pub(super) struct SidebarState {
    width: Pixels,
    working_expanded: bool,
    snoozed_expanded: bool,
    settled_expanded: bool,
    settled_visible: usize,
    /// The card under the pointer. Its controls replace the status label, which
    /// changes layout, so it is state rather than a paint-time group hover.
    hovered: Option<String>,
    /// The row whose hover card shows, after the web's 150 ms delay.
    hover_card: Option<String>,
    _hover_card_delay: Option<Task<()>>,
    /// A menu opened by a click rather than a right-click (the snooze menu).
    menu: Option<OpenMenu>,
    /// By environment, workspace root, then favicon path, so a lookup needs no key built.
    favicons: HashMap<SharedString, HashMap<String, HashMap<Option<String>, Favicon>>>,
    renaming: Option<actions::Renaming>,
    /// The row the last single click opened, for double-click rename.
    clicked: Option<String>,
    undo: Option<actions::UndoNotice>,
    /// The project filter: project group keys, empty for all projects.
    scope: Vec<String>,
    groups: RefCell<GroupsCache>,
    search_ranking: RefCell<Option<search_results::Ranking>>,
    /// Boring sidebar folders the user closed, by group key; folders start open.
    collapsed_folders: HashSet<String>,
    /// Inbox (pinned, active, working) thread ids in sidebar order, as last drawn.
    inbox_order: Vec<String>,
    /// Every thread row in sidebar order, as last drawn, for Shift-click ranges.
    thread_order: Vec<String>,
    selection: selection::Selection,
    /// Draft rows by project, newest first.
    draft_order: Vec<String>,
    provider_pill: provider_update::PillState,
    /// Each local checkout's current branch, read when a hover card opens, by workspace root.
    checkout_branches: HashMap<String, String>,
    filter: Option<filter::FilterMenu>,
    filter_closed_at: Option<std::time::Instant>,
    /// The search result Up/Down or the pointer picked; `None` paints none.
    search_highlight: Option<String>,
    messages: super::palette::MessageSearch,
    /// Keyboard navigation of the rows (`sidebarTree.ts`).
    pub(super) list_focus: FocusHandle,
    /// The keyboard row (`thread:<id>`, `shelf:<name>`, `settled:more`), and the rows in order.
    cursor: Option<SharedString>,
    nav_rows: Vec<SharedString>,
    search: Entity<InputState>,
    returns: inbox::ReturnTracker,
    /// When `_clock` wakes the sidebar: its next visible label, snooze or hold deadline.
    clock_at: Option<i64>,
    _clock: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SidebarState {
    /// The footer offers to undo a thread action.
    pub(super) fn has_undo(&self) -> bool {
        self.undo.is_some()
    }

    /// The thread rows as last drawn, top to bottom (the web's `orderedThreadKeys`).
    pub(super) fn ordered_thread_ids(&self) -> impl Iterator<Item = &str> {
        self.nav_rows.iter().filter_map(|key| key.strip_prefix("thread:"))
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let list_focus = cx.focus_handle().tab_stop(true);
        // The sidebar is a cached view: focus moving in or out of the search box or
        // the list changes what it draws, so it re-renders on those too.
        let subscriptions = vec![
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => this.on_search_changed(cx),
                InputEvent::Focus | InputEvent::Blur => this.notify_sidebar(cx),
                InputEvent::PressEnter { .. } => {}
            }),
            cx.on_focus(&list_focus, window, |this, _, cx| this.notify_sidebar(cx)),
            cx.on_blur(&list_focus, window, |this, _, cx| this.notify_sidebar(cx)),
        ];
        Self {
            width: px(DEFAULT_WIDTH),
            working_expanded: false,
            snoozed_expanded: false,
            settled_expanded: false,
            settled_visible: SETTLED_INITIAL,
            hovered: None,
            hover_card: None,
            _hover_card_delay: None,
            menu: None,
            favicons: HashMap::new(),
            renaming: None,
            clicked: None,
            undo: None,
            scope: Vec::new(),
            groups: RefCell::default(),
            search_ranking: RefCell::default(),
            collapsed_folders: HashSet::new(),
            inbox_order: Vec::new(),
            thread_order: Vec::new(),
            selection: Default::default(),
            draft_order: Vec::new(),
            provider_pill: Default::default(),
            checkout_branches: HashMap::new(),
            filter: None,
            filter_closed_at: None,
            search_highlight: None,
            messages: Default::default(),
            list_focus,
            cursor: None,
            nav_rows: Vec::new(),
            search,
            returns: inbox::ReturnTracker::default(),
            clock_at: None,
            _clock: None,
            _subscriptions: subscriptions,
        }
    }
}

/// What the project groups are built from; they rebuild only when it changes.
#[derive(PartialEq)]
struct GroupsKey {
    revision: u64,
    order: SortOrder,
    grouping: GroupingSettings,
    scratch_roots: Vec<ScratchRoot>,
}

#[derive(Default)]
struct GroupsCache {
    key: Option<GroupsKey>,
    groups: Rc<[ProjectGroup]>,
}

/// The filter's groups that still exist and are open, in sidebar order; empty is all projects.
fn scoped_groups<'g>(groups: &'g [ProjectGroup], scope: &[String]) -> Vec<&'g ProjectGroup> {
    project_groups::pickable(groups).into_iter().filter(|g| scope.contains(&g.key)).collect()
}

/// `resolveSidebarStageBackdropVariant`: the stage art behind the sidebar header and on the
/// send button. This build has no Dev or Nightly stage of its own, so `artwork` follows a
/// Nightly server (`resolveServerBackedAppStageLabel`) and shows nothing otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StageArt {
    Nightly,
    Dev,
}

/// `NIGHTLY_SERVER_VERSION_PATTERN`: `<version>-nightly.<yyyymmdd>.<n>` (or `-preview.`).
fn is_nightly_version(version: &str) -> bool {
    let Some((base, pre)) = version.split_once('-') else { return false };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let mut parts = pre.split('.');
    !base.is_empty()
        && !base.contains('+')
        && matches!(parts.next(), Some("nightly" | "preview"))
        && parts.next().is_some_and(|date| date.len() == 8 && digits(date))
        && parts.next().is_some_and(digits)
        && parts.next().is_none()
}

fn stage_art(mode: &str, server_version: Option<&str>) -> Option<StageArt> {
    match mode {
        "nightly-artwork" => Some(StageArt::Nightly),
        "dev-artwork" => Some(StageArt::Dev),
        "artwork" if server_version.is_some_and(is_nightly_version) => Some(StageArt::Nightly),
        _ => None,
    }
}

const STAGE_NIGHTLY: &str = include_str!("../../assets/stage/nightly.svg");
const STAGE_DEV_LIGHT: &str = include_str!("../../assets/stage/dev-light.svg");
const STAGE_DEV_DARK: &str = include_str!("../../assets/stage/dev-dark.svg");
const STAGE_FRAME: &str = r#"width="2560" height="160" viewBox="0 0 1536 96""#;

/// The art, rasterized once per variant; `dark` picks the Dev blueprint's deeper paper.
/// `button` is the square the send button shows (`StageBackdropButtonArt`'s compact view box).
fn stage_image(art: StageArt, dark: bool, button: bool) -> Arc<Image> {
    fn load(svg: &str, button_left: Option<u32>) -> Arc<Image> {
        let svg = match button_left {
            Some(left) => svg.replacen(STAGE_FRAME, &format!(r#"width="64" height="64" viewBox="{left} 0 96 96""#), 1),
            None => svg.to_owned(),
        };
        Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))
    }
    static IMAGES: LazyLock<[Arc<Image>; 6]> = LazyLock::new(|| {
        [
            load(STAGE_NIGHTLY, None),
            load(STAGE_DEV_LIGHT, None),
            load(STAGE_DEV_DARK, None),
            load(STAGE_NIGHTLY, Some(96)),
            load(STAGE_DEV_LIGHT, Some(64)),
            load(STAGE_DEV_DARK, Some(64)),
        ]
    });
    let variant = match (art, dark) {
        (StageArt::Nightly, _) => 0,
        (StageArt::Dev, false) => 1,
        (StageArt::Dev, true) => 2,
    };
    IMAGES[variant + if button { 3 } else { 0 }].clone()
}

/// The art at the web's slice scale (96 art units to `height`), from its left edge.
fn stage_backdrop(art: StageArt, dark: bool, height: f32) -> AnyElement {
    let width = 1536. * height / 96.;
    img(stage_image(art, dark, false)).absolute().top_0().left_0().w(px(width)).h(px(height)).into_any_element()
}

/// The send button's round art.
pub(super) fn stage_button_art(art: StageArt, dark: bool) -> AnyElement {
    img(stage_image(art, dark, true)).absolute().top_0().left_0().size(px(32.)).rounded_full().into_any_element()
}

/// The sidebar as its own view, so GPUI can reuse its frame while the rest of
/// the window re-renders. It draws [`Workspace::render_sidebar`].
pub(super) struct SidebarView(pub(super) WeakEntity<Workspace>);

impl Render for SidebarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.0
            .update(cx, |workspace, cx| workspace.render_sidebar(window, cx).into_any_element())
            .unwrap_or_else(|_| div().into_any_element())
    }
}

/// A popup menu shown at a point until it dismisses itself.
struct OpenMenu {
    view: Entity<PopupMenu>,
    position: Point<Pixels>,
    anchor: Anchor,
    /// The row that opened it; its hover controls stay up while it is open.
    owner: Option<String>,
    _dismiss: Subscription,
}

/// Drag payload for the resize rail.
struct SidebarResize;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shelf {
    Working,
    Snoozed,
    Settled,
}

impl Shelf {
    fn label(self) -> &'static str {
        match self {
            Shelf::Working => "Working",
            Shelf::Snoozed => "Snoozed",
            Shelf::Settled => "Settled",
        }
    }
}

enum Item<'a> {
    Draft(drafts::DraftRow),
    DraftDivider,
    Card(Facts<'a>),
    Slim(Facts<'a>, Option<Shelf>),
    /// A boring sidebar project folder; its open rows follow as `FolderRow`s.
    Folder {
        group: &'a ProjectGroup,
        collapsed: bool,
        hidden: usize,
        dot: Option<FolderDot>,
    },
    FolderRow(Facts<'a>),
    Header {
        shelf: Shelf,
        count: usize,
        expanded: bool,
        first: bool,
    },
    ShowMore(usize),
}

impl Item<'_> {
    fn thread_id(&self) -> Option<String> {
        match self {
            Item::Card(facts) | Item::Slim(facts, _) | Item::FolderRow(facts) => Some(facts.thread.id.clone()),
            _ => None,
        }
    }

    /// The keyboard-navigation key of a focusable row.
    fn nav_key(&self) -> Option<SharedString> {
        match self {
            Item::Card(facts) | Item::Slim(facts, _) | Item::FolderRow(facts) => {
                Some(format!("thread:{}", facts.thread.id).into())
            }
            Item::Folder { group, .. } => Some(format!("folder:{}", group.key).into()),
            Item::Header { shelf, .. } => Some(format!("shelf:{}", shelf.label()).into()),
            Item::ShowMore(_) => Some("settled:more".into()),
            _ => None,
        }
    }
}

/// The 2 px inset focus ring of the keyboard row.
fn focus_ring(element: AnyElement, tokens: &Tokens) -> AnyElement {
    div()
        .relative()
        .child(element)
        .child(div().absolute().inset_0().rounded(px(6.)).border_2().border_color(tokens.ring))
        .into_any_element()
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn text_xs<E: gpui_kit::Styled>(el: E) -> E {
    el.text_xs().line_height(px(16.))
}

fn text_sm<E: gpui_kit::Styled>(el: E) -> E {
    el.text_sm().line_height(px(20.))
}

fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

fn tooltip(label: &'static str) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView + 'static {
    move |window, cx| Tooltip::new(label).build(window, cx)
}

impl SidebarState {
    /// The rows to draw, in order: the web's `sidebarListItemsFresh`. `boring` folds the
    /// active threads into project folders (`useSidebarThreads` `folderGroups`).
    fn items<'a>(
        &mut self,
        shell: &'a ShellState,
        groups: &'a [ProjectGroup],
        target: Option<&'a Target>,
        drafts: Vec<drafts::DraftRow>,
        working_shelf: bool,
        boring: bool,
        now: i64,
    ) -> Vec<Item<'a>> {
        fn route_only<'a>(list: Vec<Facts<'a>>, expanded: bool, route: Option<&str>) -> Vec<Facts<'a>> {
            if expanded { list } else { list.into_iter().filter(|f| Some(f.thread.id.as_str()) == route).collect() }
        }
        let route = match target {
            Some(Target::Thread { id, .. }) => Some(id.as_str()),
            _ => None,
        };
        let scoped = scoped_groups(groups, &self.scope);
        let scope_ids: Option<HashSet<&str>> = (!scoped.is_empty())
            .then(|| scoped.iter().flat_map(|g| g.project_ids.iter().map(String::as_str)).collect());
        let in_scope = |project_id: &str| scope_ids.as_ref().is_none_or(|ids| ids.contains(project_id));
        // Folders already separate the work, so boring mode has no Working shelf.
        let sections =
            inbox::partition(&shell.threads, now, working_shelf && !boring, boring, &mut self.returns, |t| {
                in_scope(&t.project_id)
            });
        let mut items: Vec<Item> =
            drafts.into_iter().filter(|row| in_scope(&row.project_id)).map(Item::Draft).collect();
        if !items.is_empty() {
            items.push(Item::DraftDivider);
        }
        items.extend(sections.pinned.into_iter().map(Item::Card));
        if boring {
            // An archived project keeps a folder for its active threads, below the rest.
            let folders: Vec<&ProjectGroup> = if scoped.is_empty() {
                project_groups::pickable(groups).into_iter().chain(groups.iter().filter(|g| g.archived)).collect()
            } else {
                scoped.clone()
            };
            let keep_empty = |g: &ProjectGroup| !scoped.is_empty() || (g.pinned && !g.archived);
            let folders =
                project_groups::folders(&folders, sections.active, |f| f.thread.project_id.as_str(), keep_empty);
            for (group, threads) in folders {
                let collapsed = self.collapsed_folders.contains(&group.key);
                // A closed folder still shows the open thread, and hints at the rest.
                let (shown, hidden): (Vec<Facts>, Vec<Facts>) = if collapsed {
                    threads.into_iter().partition(|f| Some(f.thread.id.as_str()) == route)
                } else {
                    (threads, Vec::new())
                };
                let dot = inbox::folder_dot(&hidden);
                items.push(Item::Folder { group, collapsed, hidden: hidden.len(), dot });
                items.extend(shown.into_iter().map(Item::FolderRow));
            }
        } else {
            items.extend(sections.active.into_iter().map(Item::Card));
        }
        let mut first = true;
        if !sections.working.is_empty() {
            let (count, expanded) = (sections.working.len(), self.working_expanded);
            items.push(Item::Header { shelf: Shelf::Working, count, expanded, first });
            first = false;
            items.extend(route_only(sections.working, expanded, route).into_iter().map(Item::Card));
        }
        if !sections.snoozed.is_empty() {
            let (count, expanded) = (sections.snoozed.len(), self.snoozed_expanded);
            items.push(Item::Header { shelf: Shelf::Snoozed, count, expanded, first });
            first = false;
            items.extend(
                route_only(sections.snoozed, expanded, route).into_iter().map(|f| Item::Slim(f, Some(Shelf::Snoozed))),
            );
        }
        let expanded = self.settled_expanded;
        items.push(Item::Header { shelf: Shelf::Settled, count: sections.settled.len(), expanded, first });
        let total = sections.settled.len();
        let mut settled: Vec<Facts<'_>> = sections.settled;
        let hidden = total.saturating_sub(self.settled_visible);
        if hidden > 0 {
            let route_row = settled[self.settled_visible..].iter().position(|f| Some(f.thread.id.as_str()) == route);
            let route_row = route_row.map(|i| settled[self.settled_visible + i]);
            settled.truncate(self.settled_visible);
            settled.extend(route_row);
        }
        items.extend(route_only(settled, expanded, route).into_iter().map(|f| Item::Slim(f, Some(Shelf::Settled))));
        if expanded && hidden > 0 {
            items.push(Item::ShowMore(hidden.min(SETTLED_PAGE)));
        }
        items
    }

    /// No rows and every shelf empty: the list shows its empty state.
    fn settled_count_is_zero(&self, items: &[Item]) -> bool {
        items.iter().all(|item| match item {
            Item::Header { count, .. } => *count == 0,
            Item::Card(_) | Item::Slim(..) | Item::Draft(_) | Item::FolderRow(_) | Item::Folder { .. } => false,
            _ => true,
        })
    }

    /// Milliseconds until something the sidebar shows changes with time alone:
    /// a duration or relative-time label, a wake label, a snooze deadline, or
    /// a Working hold running out.
    fn next_clock_change(&self, items: &[Item], threads: &[ThreadShell], now: i64) -> Option<i64> {
        let mut next = self.returns.next_deadline().map(|deadline| deadline - now);
        let mut at = |delay: i64| next = Some(next.map_or(delay, |n| n.min(delay)));
        for item in items {
            match item {
                Item::Card(facts) => match facts.status() {
                    Status::Working | Status::Preparing => {
                        if let Some(from) = facts.working_started_at() {
                            at(inbox::duration_changes_in(now - from));
                        }
                    }
                    Status::Waiting => {
                        if let Some(from) =
                            crate::timestamp::parse_ms(facts.latest_run_completed_at().unwrap_or_default())
                        {
                            at(inbox::duration_changes_in(now - from));
                        }
                    }
                    _ => {
                        if let Some(from) = facts.activity_timestamp() {
                            at(inbox::relative_changes_in(now - from));
                        }
                    }
                },
                // Folder rows show a status label without a duration, else the relative time.
                Item::FolderRow(facts) => {
                    if facts.status() == Status::Ready
                        && !facts.is_woke(now)
                        && !facts.is_unread()
                        && let Some(from) = facts.activity_timestamp()
                    {
                        at(inbox::relative_changes_in(now - from));
                    }
                }
                Item::Slim(facts, shelf) => {
                    let from = if *shelf == Some(Shelf::Settled) {
                        facts.settled_timestamp()
                    } else {
                        facts.activity_timestamp()
                    };
                    if let Some(from) = from {
                        at(inbox::relative_changes_in(now - from));
                    }
                }
                _ => {}
            }
        }
        // Waking moves a thread between shelves even while its shelf is collapsed.
        for thread in threads {
            if let Some(delay) = crate::timestamp::parse_ms(thread.snoozed_until.as_deref().unwrap_or_default())
                .and_then(|until| inbox::wake_changes_in(until - now))
            {
                at(delay);
            }
        }
        next
    }
}

impl Workspace {
    /// The project groups in sidebar order (`useSidebarProjectGroups`): grouped by
    /// `sidebarProjectGroupingMode` and its overrides, ordered by `sidebarProjectSortOrder`.
    /// They rebuild only when the shell or those settings change.
    pub(super) fn project_groups(&self) -> Rc<[ProjectGroup]> {
        let settings = &self.client_settings.values;
        let (order, grouping) = (SortOrder::from_settings(settings), GroupingSettings::from_settings(settings));
        let mut cache = self.sidebar.groups.borrow_mut();
        let fresh = cache.key.as_ref().is_some_and(|key| {
            key.revision == self.shell.revision
                && key.order == order
                && key.grouping == grouping
                && self.scratch_roots_match(&key.scratch_roots)
        });
        if !fresh {
            let key = GroupsKey { revision: self.shell.revision, order, grouping, scratch_roots: self.scratch_roots() };
            let mut groups =
                project_groups::build_groups(&self.shell.projects, &key.grouping, &key.scratch_roots, true);
            project_groups::sort_groups(&mut groups, &self.shell.threads, key.order);
            *cache = GroupsCache { key: Some(key), groups: Rc::from(groups) };
        }
        cache.groups.clone()
    }

    /// The projects the palette and "New thread in…" offer: one per open group, pinned first.
    pub(super) fn pickable_projects(&self) -> Vec<&ProjectShell> {
        let groups = self.project_groups();
        project_groups::pickable(&groups)
            .into_iter()
            .filter_map(|g| self.shell.projects.iter().find(|p| p.id == g.representative().id))
            .collect()
    }

    /// The project ids the filter keeps, or `None` for all projects.
    fn scoped_project_ids(&self) -> Option<HashSet<String>> {
        let groups = self.project_groups();
        let scoped = scoped_groups(&groups, &self.sidebar.scope);
        (!scoped.is_empty()).then(|| scoped.iter().flat_map(|g| g.project_ids.iter().cloned()).collect())
    }

    /// The boring sidebar (Labs), also on for users of the removed legacy sidebar.
    fn boring_sidebar(&self) -> bool {
        let values = &self.client_settings.values;
        values.bool("boringSidebarEnabled") || values.bool("legacySidebarEnabled")
    }

    /// The stage art the sidebar header and send button show (`environmentIdentificationMode`).
    pub(super) fn stage_art(&self) -> Option<StageArt> {
        let mode = self.client_settings.values.get("environmentIdentificationMode").as_str().unwrap_or("artwork");
        // The primary server's stage, as the web's `primaryServerConfigAtom`.
        let version = self.primary_info().config.pointer("/environment/serverVersion").and_then(|v| v.as_str());
        stage_art(mode, version)
    }

    /// The "Nightly" pill of `pill` mode, shown only on a Nightly server.
    fn stage_pill(&self) -> Option<&'static str> {
        let mode = self.client_settings.values.get("environmentIdentificationMode").as_str();
        let version = self.primary_info().config.pointer("/environment/serverVersion").and_then(|v| v.as_str());
        (mode == Some("pill") && version.is_some_and(is_nightly_version)).then_some("Nightly")
    }

    fn toggle_folder(&mut self, key: &str, cx: &mut Context<Self>) {
        let collapsed = &mut self.sidebar.collapsed_folders;
        if !collapsed.remove(key) {
            collapsed.insert(key.to_owned());
        }
        self.notify_sidebar(cx);
    }

    fn search_query(&self, cx: &App) -> String {
        self.sidebar.search.read(cx).value().trim().to_lowercase()
    }

    pub(super) fn route_thread(&self) -> Option<&str> {
        match &self.target {
            Some(Target::Thread { id, .. }) => Some(id),
            _ => None,
        }
    }

    /// Wakes the sidebar after `delay` ms, replacing a pending wake for another time.
    /// Idle sidebars wake at most once a minute; only a duration in its first
    /// minute ticks every second.
    fn arm_sidebar_clock(&mut self, delay: Option<i64>, now: i64, cx: &mut Context<Self>) {
        let at = delay.map(|delay| now + delay.max(0) + 5);
        if at == self.sidebar.clock_at {
            return;
        }
        self.sidebar.clock_at = at;
        self.sidebar._clock = at.map(|at| {
            cx.spawn(async move |this, cx| {
                let delay = u64::try_from(at - now_ms()).unwrap_or(0);
                cx.background_executor().timer(Duration::from_millis(delay)).await;
                let _ = this.update(cx, |this, cx| {
                    this.sidebar.clock_at = None;
                    this.notify_sidebar(cx);
                });
            })
        });
    }

    /// Opens `build`'s menu at `position` (window coordinates), focused, until it dismisses.
    fn open_menu(
        &mut self,
        position: Point<Pixels>,
        anchor: Anchor,
        window: &mut Window,
        cx: &mut Context<Self>,
        build: impl FnOnce(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu,
    ) {
        let view = PopupMenu::build(window, cx, build);
        // Closing hands focus back to where it was, so shortcuts keep reaching the workspace.
        let previous = window.focused(cx);
        view.focus_handle(cx).focus(window, cx);
        let dismiss = cx.subscribe_in(&view, window, move |this, _, _: &DismissEvent, window, cx| {
            this.sidebar.menu = None;
            if let Some(previous) = &previous {
                previous.focus(window, cx);
            }
            this.notify_sidebar(cx);
        });
        let owner = self.sidebar.hovered.clone();
        self.sidebar.menu = Some(OpenMenu { view, position, anchor, owner, _dismiss: dismiss });
        self.notify_sidebar(cx);
    }

    fn render_open_menu(&self) -> Option<AnyElement> {
        let menu = self.sidebar.menu.as_ref()?;
        Some(
            deferred(
                anchored()
                    .anchor(menu.anchor)
                    .position(menu.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(menu.view.clone()),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    /// A row click opens the thread; the second click of a double-click renames it.
    fn click_row(&mut self, thread_id: &str, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.modifiers();
        let toggle = if cfg!(target_os = "macos") { modifiers.platform } else { modifiers.control };
        if toggle {
            self.sidebar.selection.toggle(thread_id);
            self.notify_sidebar(cx);
            return;
        }
        if modifiers.shift {
            let order = std::mem::take(&mut self.sidebar.thread_order);
            self.sidebar.selection.range_to(thread_id, &order);
            self.sidebar.thread_order = order;
            self.notify_sidebar(cx);
            return;
        }
        if event.click_count() >= 2 {
            // Opening a thread can move the rows, so the second click may land on another one.
            let same_row = self.sidebar.clicked.as_deref() == Some(thread_id);
            if same_row && !event.modifiers().modified() && self.sidebar.renaming.is_none() {
                self.start_rename(thread_id, MenuOrigin::Sidebar, window, cx);
            }
            return;
        }
        self.sidebar.clicked = Some(thread_id.to_owned());
        self.sidebar.selection.clear();
        self.sidebar.selection.set_anchor(thread_id);
        self.select_thread(thread_id.to_owned(), window, cx);
        self.notify_sidebar(cx);
    }

    /// The right-click menu builder for a row.
    fn row_menu(
        &self,
        thread_id: &str,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.weak_entity();
        let thread_id = thread_id.to_owned();
        move |menu, window, cx| {
            let Some(workspace) = view.upgrade() else { return menu };
            if let Some(bulk) = workspace.read(cx).bulk_menu(&thread_id) {
                return bulk.build(&view, menu, window, cx);
            }
            let model = workspace.read(cx).thread_menu(&thread_id, MenuOrigin::Sidebar, cx);
            match model {
                Some(model) => model.build(&view, menu.min_w(px(200.)), window, cx),
                None => menu,
            }
        }
    }

    /// The pinned card's pin, a button that unpins (`Pin` swaps to `PinOff` on hover).
    fn render_unpin(&self, thread_id: &str, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let group = SharedString::from(format!("unpin-{thread_id}"));
        let id = thread_id.to_owned();
        let muted = tokens.muted_foreground.opacity(0.65);
        let fg = tokens.sidebar.foreground;
        div()
            .id(group.clone())
            .group(group.clone())
            .flex_none()
            .size(px(12.))
            .rounded(px(4.))
            .cursor_pointer()
            .tooltip(tooltip("Unpin thread"))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.unpin_thread(&id, window, cx);
            }))
            .child(div().absolute().group_hover(group.clone(), gpui_kit::Styled::invisible).child(icon(
                IconName::Pin,
                12.,
                muted,
            )))
            .child(div().absolute().invisible().group_hover(group, gpui_kit::Styled::visible).child(icon(
                IconName::PinOff,
                12.,
                fg,
            )))
            .into_any_element()
    }

    /// Covers a row's title with the rename input while it is being renamed
    /// (`ThreadRenameInput`); `left` is where the title starts in `el`.
    fn with_rename<E: gpui_kit::ParentElement + gpui_kit::Styled>(
        &self,
        el: E,
        thread_id: &str,
        left: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> E {
        let Some(renaming) = self.sidebar.renaming.as_ref().filter(|r| r.thread_id == thread_id) else { return el };
        el.relative().child(
            div()
                .id("sidebar-rename")
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(left))
                .right_0()
                .flex()
                .items_center()
                .bg(tokens.card)
                .on_click(|_, _, cx| cx.stop_propagation())
                .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                    cx.stop_propagation();
                    if event.keystroke.key == "escape" {
                        this.cancel_rename(MenuOrigin::Sidebar, cx);
                    }
                }))
                .child(
                    Input::new(&renaming.input)
                        .w_full()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .h(px(22.))
                        .px(px(4.))
                        .rounded(px(4.)),
                ),
        )
    }

    /// Tracks the row under the pointer and opens its hover card after 150 ms
    /// (`Sidebar.tsx` row tooltip delay).
    fn hover_row(&mut self, thread_id: &str, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.sidebar.hovered.as_deref() == Some(thread_id) {
                return;
            }
            self.sidebar.hovered = Some(thread_id.to_owned());
            self.sidebar.hover_card = None;
            let thread_id = thread_id.to_owned();
            self.sidebar._hover_card_delay = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.sidebar.hovered.as_ref() == Some(&thread_id) {
                        this.read_checkout_branch(&thread_id, cx);
                        this.sidebar.hover_card = Some(thread_id);
                        this.notify_sidebar(cx);
                    }
                });
            }));
        } else if self.sidebar.hovered.as_deref() == Some(thread_id) {
            self.sidebar.hovered = None;
            self.sidebar.hover_card = None;
            self.sidebar._hover_card_delay = None;
        } else {
            return;
        }
        self.notify_sidebar(cx);
    }

    /// A local thread's checkout may have moved to another branch since; the hover card warns
    /// then (`resolveLocalCheckoutBranchMismatch`). Read fresh each time a card opens.
    fn read_checkout_branch(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.shell.threads.iter().find(|t| t.id == thread_id) else { return };
        if thread.worktree_path.is_some() || thread.branch.is_none() {
            return;
        }
        let Some(cwd) =
            self.shell.projects.iter().find(|p| p.id == thread.project_id).map(|p| p.workspace_root.clone())
        else {
            return;
        };
        let Some(client) = self.thread_client(&thread.id) else { return };
        cx.spawn(async move |this, cx| {
            let Ok(status) = client.request("vcs.refreshStatus", json!({"cwd": cwd})).await else { return };
            let Some(branch) = status["refName"].as_str().map(str::to_owned) else { return };
            let _ = this.update(cx, |this, cx| {
                if this.sidebar.checkout_branches.get(&cwd) != Some(&branch) {
                    this.sidebar.checkout_branches.insert(cwd, branch);
                    this.notify_sidebar(cx);
                }
            });
        })
        .detach();
    }

    /// The row's hover card, drawn to the right of the row (`SidebarThreadTooltip`).
    fn render_hover_card(&self, facts: &Facts, project: Option<&ProjectShell>, tokens: &Tokens) -> Option<AnyElement> {
        let thread = facts.thread;
        if self.sidebar.hover_card.as_deref() != Some(thread.id.as_str())
            || self.sidebar.menu.is_some()
            || self.sidebar.renaming.is_some()
        {
            return None;
        }
        let line_text =
            |text: SharedString| div().min_w_0().truncate().text_color(tokens.foreground.opacity(0.75)).child(text);
        let line = |icon: AnyElement, text: SharedString| {
            h_flex().min_w_0().gap(px(8.)).child(div().flex_none().child(icon)).child(line_text(text))
        };
        let muted_icon = |name: IconName| icon(name, 12., tokens.muted_foreground).into_any_element();
        let instance = thread
            .provider_instance_id
            .as_deref()
            .filter(|_| facts.runtime_status().is_some())
            .or(thread.model_selection.as_ref().map(|m| m.instance_id.as_str()));
        // The hovered thread's machine, which need not be the open one.
        let info = self.env_info(&thread.environment_id);
        let provider = instance.and_then(|id| info?.provider(id));
        let model = thread.model_selection.as_ref().map(|selection| {
            let found = provider.and_then(|p| p.models.iter().find(|m| m.slug == selection.model).map(|m| (p, m)));
            match found {
                Some((p, m)) => crate::model_label::trigger_model_name(m, &p.driver),
                None => selection.model.clone(),
            }
        });
        let provider_line = provider.zip(model).and_then(|(provider, model)| {
            let (path, tint) = crate::assets::provider_icon(&provider.driver)?;
            let mark =
                svg().path(path).size(px(12.)).text_color(tint.unwrap_or(tokens.foreground).grayscale().opacity(0.6));
            Some(line(mark.into_any_element(), model.into()))
        });
        let machine = match info.map_or("", |info| info.machine_kind()) {
            "desktop" | "mac-studio" | "mac-mini" => IconName::Monitor,
            "laptop" => IconName::Laptop,
            "cloud" => IconName::Cloud,
            _ => IconName::Server,
        };
        let error =
            thread.last_error.as_ref().filter(|_| facts.runtime_status().is_some()).map(|_| {
                let limit = thread.last_error_class.as_deref() == Some("usage_limit");
                let color = if limit { tokens.warning } else { tokens.error };
                h_flex().min_w_0().gap(px(8.)).text_color(color).child(icon(IconName::CircleAlert, 12., color)).child(
                    div().min_w_0().truncate().child(if limit { "Usage limit reached" } else { "Error occurred" }),
                )
            });
        let mismatch = thread.worktree_path.is_none()
            && project
                .and_then(|p| self.sidebar.checkout_branches.get(&p.workspace_root))
                .is_some_and(|current| thread.branch.as_ref().is_some_and(|branch| branch != current));
        let mismatch = mismatch.then(|| {
            h_flex()
                .min_w_0()
                .items_start()
                .gap(px(8.))
                .text_color(tokens.warning)
                .child(div().mt(px(2.)).child(icon(IconName::CircleAlert, 12., tokens.warning)))
                .child(div().min_w_0().flex_1().child("You're currently checked out on another branch."))
        });
        let card = text_xs(v_flex())
            .max_w(px(320.))
            .px(px(12.))
            .py(px(12.))
            .gap(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(tokens.foreground.opacity(0.1))
            .bg(tokens.popover.opacity(0.96))
            .shadow_xl()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .line_height(px(15.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.foreground)
                    .child(thread.title.clone()),
            )
            .child(
                v_flex()
                    .gap(px(6.))
                    .pl(px(2.))
                    .text_color(tokens.muted_foreground)
                    .children(
                        project.map(|p| line(project_icon(p, tokens, false, 12., self.favicon(p)), project_name(p))),
                    )
                    .children(
                        info.and_then(|info| info.environment_label.clone())
                            .map(|label| line(muted_icon(machine), label.into())),
                    )
                    .children(thread.branch.clone().map(|branch| line(muted_icon(IconName::GitBranch), branch.into())))
                    .children(mismatch)
                    .children(provider_line)
                    .children(error),
            );
        Some(
            div()
                .absolute()
                .top_0()
                .left_full()
                .ml(px(4.))
                .child(deferred(anchored().snap_to_window_with_margin(px(8.)).child(card)).with_priority(2))
                .into_any_element(),
        )
    }

    /// The list's empty state (`Sidebar.tsx`), with "Add project" when there are no projects.
    fn render_empty_state(&self, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let sb = tokens.sidebar;
        let no_projects = self.shell.projects.is_empty();
        let label: SharedString = if no_projects {
            "No projects yet".into()
        } else {
            let groups = self.project_groups();
            match scoped_groups(&groups, &self.sidebar.scope).as_slice() {
                [] => "No threads yet".into(),
                [one] => format!("No threads in {} yet", one.display_name).into(),
                _ => "No threads in these projects yet".into(),
            }
        };
        text_xs(v_flex())
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(24.))
            .text_center()
            .text_color(tokens.muted_foreground.opacity(0.6))
            .child(label)
            .when(no_projects, |el| {
                el.child(
                    h_flex()
                        .id("sidebar-empty-add-project")
                        .gap(px(6.))
                        .px(px(10.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(sb.border)
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(sb.muted_foreground)
                        .cursor_pointer()
                        .hover(|s| s.bg(sb.row_hover).text_color(sb.foreground))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_palette(super::palette::View::AddProject, window, cx);
                        }))
                        .child(div().mx(px(-2.)).child(Icon::new(IconName::Plus).size(px(12.))))
                        .child("Add project"),
                )
            })
            .into_any_element()
    }

    /// Arrow keys, Home/End, Enter/Space, F2 and Left/Right on the focused list (`sidebarTree.ts`).
    fn list_key(&mut self, event: &gpui_kit::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let rows = &self.sidebar.nav_rows;
        let modifiers = event.keystroke.modifiers;
        if rows.is_empty() || modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        let last = rows.len() - 1;
        let at = self.sidebar.cursor.as_ref().and_then(|key| rows.iter().position(|r| r == key));
        let key = at.map(|i| rows[i].clone());
        let shelf_of = |key: &str| match key.strip_prefix("shelf:")? {
            "Working" => Some(Shelf::Working),
            "Snoozed" => Some(Shelf::Snoozed),
            "Settled" => Some(Shelf::Settled),
            _ => None,
        };
        let expanded = |this: &Self, shelf| match shelf {
            Shelf::Working => this.sidebar.working_expanded,
            Shelf::Snoozed => this.sidebar.snoozed_expanded,
            Shelf::Settled => this.sidebar.settled_expanded,
        };
        let move_to = match event.keystroke.key.as_str() {
            "down" => Some(at.map_or(0, |i| (i + 1).min(last))),
            "up" => Some(at.map_or(0, |i| i.saturating_sub(1))),
            "home" => Some(0),
            "end" => Some(last),
            _ => None,
        };
        if let Some(index) = move_to {
            self.sidebar.cursor = Some(rows[index].clone());
        } else if let Some(folder) = key.as_deref().and_then(|key| key.strip_prefix("folder:")) {
            let collapsed = self.sidebar.collapsed_folders.contains(folder);
            match event.keystroke.key.as_str() {
                "enter" | "space" => {}
                "right" if collapsed => {}
                "left" if !collapsed => {}
                _ => return,
            }
            let folder = folder.to_owned();
            self.toggle_folder(&folder, cx);
        } else {
            let Some(key) = key else { return };
            match (event.keystroke.key.as_str(), shelf_of(&key)) {
                ("enter" | "space", Some(shelf)) => self.toggle_shelf(shelf, cx),
                ("right", Some(shelf)) if !expanded(self, shelf) => self.toggle_shelf(shelf, cx),
                ("left", Some(shelf)) if expanded(self, shelf) => self.toggle_shelf(shelf, cx),
                ("enter" | "space", None) if key.as_ref() == "settled:more" => {
                    self.sidebar.settled_visible += SETTLED_PAGE;
                }
                ("enter" | "space", None) => {
                    if let Some(id) = key.strip_prefix("thread:") {
                        self.select_thread(id.to_owned(), window, cx);
                    }
                }
                ("f2", None) => {
                    if let Some(id) = key.strip_prefix("thread:") {
                        self.start_rename(id, MenuOrigin::Sidebar, window, cx);
                    }
                }
                _ => return,
            }
        }
        cx.stop_propagation();
        self.notify_sidebar(cx);
    }

    /// Sets the project filter; the settled tail starts over at its first page.
    pub(super) fn set_scope(&mut self, scope: Vec<String>, cx: &mut Context<Self>) {
        // A selection made under one scope can hold threads the next one hides.
        self.sidebar.selection.clear();
        self.sidebar.scope = scope;
        self.sidebar.settled_visible = SETTLED_INITIAL;
        self.notify_sidebar(cx);
    }

    fn toggle_shelf(&mut self, shelf: Shelf, cx: &mut Context<Self>) {
        let expanded = match shelf {
            Shelf::Working => &mut self.sidebar.working_expanded,
            Shelf::Snoozed => &mut self.sidebar.snoozed_expanded,
            Shelf::Settled => &mut self.sidebar.settled_expanded,
        };
        *expanded = !*expanded;
        self.notify_sidebar(cx);
    }

    /// New thread in the open thread's project, else the first project.
    pub(super) fn new_thread_project(&self) -> Option<String> {
        let current = match &self.target {
            Some(Target::Thread { id, .. }) => {
                self.shell.threads.iter().find(|t| &t.id == id).map(|t| t.project_id.clone())
            }
            Some(Target::Draft { project_id, .. }) => Some(project_id.clone()),
            None => None,
        };
        current.or_else(|| self.shell.projects.iter().find(|p| p.archived_at.is_none()).map(|p| p.id.clone()))
    }

    /// Re-renders the sidebar. It is a cached view: the workspace re-rendering
    /// (every timeline scroll frame, every streamed token) leaves it alone.
    pub(super) fn notify_sidebar(&self, cx: &mut Context<Self>) {
        self.sidebar_view.update(cx, |_, cx| cx.notify());
    }

    /// The sidebar view, reused from the last frame unless it was notified or resized.
    pub(super) fn render_cached_sidebar(&self) -> impl IntoElement {
        AnyView::from(self.sidebar_view.clone())
            .cached(StyleRefinement::default().flex_none().h_full().w(self.sidebar.width))
    }

    pub(super) fn sidebar_width(&self) -> Pixels {
        self.sidebar.width
    }

    /// The return tracker must see every shell change, like the web's. The
    /// render pass feeds it while the sidebar shows; this covers it hidden.
    pub(super) fn observe_inbox_while_hidden(&mut self) {
        if self.sidebar_open {
            return;
        }
        let requests = inbox::child_requests(&self.shell.threads);
        self.sidebar.returns.observe(&self.shell.threads, &requests, now_ms());
    }

    pub(super) fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load_favicons(cx);
        let tokens = *crate::theme::tokens(cx);
        let sb = tokens.sidebar;
        let now = now_ms();
        let query = self.search_query(cx);
        let connected = matches!(self.connection, Connection::Connected | Connection::Connecting);
        let working_shelf =
            connected && self.shell.live() && self.client_settings.values.bool("sidebarWorkingShelfEnabled");
        let boring = self.boring_sidebar();
        let multiline = self.client_settings.values.bool("boringSidebarMultilineRows");
        let groups = self.project_groups();
        let draft_rows = self.draft_rows();
        let items =
            self.sidebar.items(&self.shell, &groups, self.target.as_ref(), draft_rows, working_shelf, boring, now);
        let clock = self.sidebar.next_clock_change(&items, &self.shell.threads, now);
        let projects: HashMap<&str, &ProjectShell> = self.shell.projects.iter().map(|p| (p.id.as_str(), p)).collect();
        let route = self.route_thread();
        let label_of: HashMap<&str, &str> = groups
            .iter()
            .flat_map(|g| g.project_ids.iter().map(move |id| (id.as_str(), g.display_name.as_str())))
            .collect();
        let inbox_order: Vec<String> = items
            .iter()
            .filter_map(|item| matches!(item, Item::Card(_) | Item::FolderRow(_)).then(|| item.thread_id()).flatten())
            .collect();
        let nav_rows: Vec<SharedString> = items.iter().filter_map(Item::nav_key).collect();
        let thread_order: Vec<String> = items.iter().filter_map(Item::thread_id).collect();
        let list_focused = self.sidebar.list_focus.is_focused(window);
        let cursor = self.sidebar.cursor.clone().filter(|_| list_focused);
        let has_rows = items.iter().any(|item| {
            matches!(item, Item::Card(_) | Item::Slim(..) | Item::Draft(_) | Item::FolderRow(_) | Item::Folder { .. })
        });

        // Under a fixed header the web drops the list group's top padding.
        let mut list = v_flex().min_h_full().px(px(8.)).pb(px(8.)).gap(px(1.));
        if !query.is_empty() {
            list = list.child(self.render_search_results(&query, now, &tokens, cx));
        } else if !has_rows && self.sidebar.settled_count_is_zero(&items) {
            list = list.child(self.render_empty_state(&tokens, cx));
        } else {
            for (index, item) in items.into_iter().enumerate() {
                // Only a focused list draws the cursor ring, so only it needs the row keys.
                let ring = cursor.as_ref().is_some_and(|cursor| item.nav_key().as_ref() == Some(cursor));
                let element = match item {
                    Item::Draft(row) => {
                        self.render_draft(&row, projects.get(row.project_id.as_str()).copied(), &tokens, cx)
                    }
                    Item::DraftDivider => {
                        div().mx(px(10.)).my(px(6.)).h(px(1.)).bg(sb.border.opacity(0.6)).into_any_element()
                    }
                    Item::Card(facts) => {
                        let active = route == Some(facts.thread.id.as_str());
                        let project_id = facts.thread.project_id.as_str();
                        let label = label_of.get(project_id).map(|name| SharedString::from(name.to_string()));
                        self.render_card(facts, projects.get(project_id).copied(), label, active, now, &tokens, cx)
                    }
                    Item::Folder { group, collapsed, hidden, dot } => {
                        self.render_folder(group, collapsed, hidden, dot, index == 0, &tokens, cx)
                    }
                    Item::FolderRow(facts) => {
                        let active = route == Some(facts.thread.id.as_str());
                        let project = projects.get(facts.thread.project_id.as_str()).copied();
                        self.render_folder_row(facts, project, multiline, active, now, &tokens, cx)
                    }
                    Item::Slim(facts, shelf) => {
                        let active = route == Some(facts.thread.id.as_str());
                        self.render_slim(
                            facts,
                            projects.get(facts.thread.project_id.as_str()).copied(),
                            shelf,
                            active,
                            now,
                            &tokens,
                            cx,
                        )
                    }
                    Item::Header { shelf, count, expanded, first } => {
                        self.render_shelf_header(shelf, count, expanded, first, &tokens, cx)
                    }
                    Item::ShowMore(count) => self.render_show_more(count, &tokens, cx),
                };
                list = list.child(if ring { focus_ring(element, &tokens) } else { element });
            }
        }
        self.sidebar.inbox_order = inbox_order;
        self.sidebar.nav_rows = nav_rows;
        self.sidebar.thread_order = thread_order;
        self.arm_sidebar_clock(clock, now, cx);

        let rail = div()
            .id("sidebar-rail")
            .group("sidebar-rail")
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(-8.))
            .w(px(16.))
            .cursor_col_resize()
            .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| EmptyView))
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.sidebar.width = px(DEFAULT_WIDTH);
                    cx.notify();
                }
            }))
            .tooltip(tooltip("Drag to resize sidebar"))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(7.))
                    .w(px(2.))
                    .invisible()
                    .group_hover("sidebar-rail", gpui_kit::Styled::visible)
                    .bg(sb.border),
            );

        // `SidebarStageBackdrop`: 80 px of art behind the header, fading into the sidebar.
        let backdrop = self.stage_art().map(|art| {
            let fade = |from: Hsla, to: Hsla| {
                gpui_kit::linear_gradient(
                    180.,
                    gpui_kit::linear_color_stop(from, 0.),
                    gpui_kit::linear_color_stop(to, 1.),
                )
            };
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(80.))
                .overflow_hidden()
                .child(stage_backdrop(art, tokens.dark, 80.))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(22.))
                        .h(px(53.))
                        .bg(fade(sb.background.opacity(0.), sb.background)),
                )
                .child(div().absolute().left_0().right_0().top(px(74.)).bottom_0().bg(sb.background))
        });

        v_flex()
            .id("sidebar")
            .relative()
            .w(self.sidebar.width)
            .h_full()
            .flex_none()
            .bg(sb.background)
            .text_color(sb.foreground)
            .border_r_1()
            .border_color(sb.border)
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<SidebarResize>, window, cx| {
                let max = (window.viewport_size().width - px(MIN_MAIN_WIDTH)).max(px(MIN_WIDTH));
                this.sidebar.width = (event.event.position.x - event.bounds.left()).clamp(px(MIN_WIDTH), max);
                cx.notify();
            }))
            .children(backdrop)
            .child(self.render_chrome_row(&tokens, window, cx))
            .child(self.render_search_row(!query.is_empty(), window, &tokens, cx))
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_focus(&self.sidebar.list_focus)
                    .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                        this.list_key(event, window, cx);
                    }))
                    .child(list),
            )
            .children(
                self.render_provider_pill(&tokens, cx)
                    .map(|pill| div().flex_none().px(px(8.)).pt(px(4.)).pb(px(4.)).child(pill)),
            )
            .child(self.render_sidebar_footer(&tokens, cx))
            .child(deferred(rail).with_priority(1))
            .children(self.render_open_menu())
    }

    fn render_chrome_row(&self, tokens: &Tokens, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let sb = tokens.sidebar;
        // On the stage art the brand turns white and the arrows take `media-navigation`.
        let on_art = self.stage_art().is_some();
        let white = gpui_kit::white();
        let (brand, brand_muted, arrow_color, arrow_hover) = if on_art {
            (white, white.opacity(0.7), white.opacity(0.9), white.opacity(0.1))
        } else {
            (sb.foreground, sb.muted_foreground, sb.muted_foreground, sb.row_hover)
        };
        let arrow = |id: &'static str, name: IconName, label: &'static str, enabled: bool| {
            div()
                .id(id)
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .when(!enabled, |el| el.opacity(0.64))
                .when(enabled, |el| el.cursor_pointer().hover(|s| s.bg(arrow_hover)))
                // Inside the drag region a button must occlude, or Windows takes the press as a drag.
                .occlude()
                .tooltip(tooltip(label))
                .child(icon(name, 14., arrow_color))
        };
        let (back, forward) = (self.history.can_go_back(), self.history.can_go_forward());
        let chrome = h_flex().id("sidebar-chrome").h(TITLEBAR_HEIGHT).flex_none().pr(px(12.));
        titlebar::drag_region(chrome)
            .child(
                h_flex()
                    .ml(titlebar::collapsed_content_left(window))
                    .h(px(28.))
                    .gap(px(4.))
                    .child(svg().path("icons/app/t3-wordmark.svg").w(px(16.9)).h(px(10.2)).mt(px(1.)).text_color(brand))
                    .child(text_sm(div()).font_weight(FontWeight::MEDIUM).text_color(brand_muted).child("Code")),
            )
            .child(
                h_flex()
                    .ml(px(4.))
                    .child(
                        arrow("sidebar-back", IconName::ArrowLeft, "Go back", back)
                            .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
                    )
                    .child(
                        arrow("sidebar-forward", IconName::ArrowRight, "Go forward", forward)
                            .on_click(cx.listener(|this, _, window, cx| this.go_forward(window, cx))),
                    ),
            )
            .children(self.stage_pill().map(|label| {
                text_xs(div())
                    .ml(px(4.))
                    .px(px(6.))
                    .rounded(px(4.))
                    .font_weight(FontWeight::MEDIUM)
                    .bg(tokens.secondary)
                    .text_color(sb.foreground)
                    .child(label)
            }))
    }

    fn render_search_row(
        &self,
        searching: bool,
        window: &mut Window,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sb = tokens.sidebar;
        let focused = self.sidebar.search.focus_handle(cx).contains_focused(window, cx);
        h_flex()
            .flex_none()
            .p(px(8.))
            .gap(px(4.))
            .child(
                text_sm(h_flex())
                    .id("sidebar-search")
                    .flex_1()
                    .min_w_0()
                    .h(px(32.))
                    .px(px(8.))
                    .gap(px(8.))
                    .rounded(px(6.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(sb.muted_foreground)
                    .hover(|s| s.bg(sb.row_hover))
                    .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                        this.search_key(event, window, cx);
                    }))
                    .when(focused, |el| {
                        el.bg(sb.row_hover).border_1().border_color(tokens.ring.opacity(0.7)).px(px(7.))
                    })
                    .child(icon(IconName::Search, 16., sb.icon))
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.sidebar.search).appearance(false).px_0().py_0().h(px(20.)).text_sm(),
                        ),
                    )
                    .when(searching, |el| {
                        el.child(
                            div()
                                .id("sidebar-search-clear")
                                .size(px(20.))
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .hover(|s| s.bg(sb.row_hover))
                                .tooltip(tooltip("Clear thread search"))
                                .child(icon(IconName::X, 12., tokens.muted_foreground))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.sidebar.search.update(cx, |search, cx| {
                                        search.set_value("", window, cx);
                                        search.focus(window, cx);
                                    });
                                    this.on_search_changed(cx);
                                })),
                        )
                    }),
            )
            .child(self.render_header_buttons(tokens, cx))
    }

    fn render_sidebar_footer(&self, tokens: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let sb = tokens.sidebar;
        let button = |id: &'static str, name: IconName, label: &'static str| {
            div()
                .id(id)
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(sb.row_hover))
                .tooltip(tooltip(label))
                .child(icon(name, 16., sb.icon))
        };
        let status: Option<(SharedString, Hsla)> = match &self.connection {
            // A thread on a saved environment shows that environment's connection.
            _ if !self.target_on_primary() => self.saved_target_status(tokens, cx),
            Connection::Connected if self.shell.live() => None,
            Connection::Connected => Some(("Syncing…".into(), tokens.muted_foreground)),
            Connection::Starting => Some(("Starting server…".into(), tokens.muted_foreground)),
            Connection::Connecting => Some(("Connecting…".into(), tokens.muted_foreground)),
            Connection::Disconnected(reason) => {
                Some((format!("Reconnecting: {reason}").into(), tokens.warning_foreground))
            }
            Connection::Failed(reason) => Some((reason.clone(), tokens.error_foreground)),
        };
        h_flex()
            .flex_none()
            .px(px(8.))
            .pt(px(4.))
            .pb(px(8.))
            .gap(px(4.))
            // On a utility page the three shortcuts become one "Back" (`SidebarChrome.tsx`).
            .map(|el| match self.page {
                Some(_) => el.child(
                    text_sm(h_flex())
                        .id("sidebar-back-to-chat")
                        .flex_1()
                        .h(px(32.))
                        .px(px(8.))
                        .gap(px(8.))
                        .rounded(px(8.))
                        .cursor_pointer()
                        .text_color(sb.foreground)
                        .hover(|s| s.bg(sb.row_hover))
                        .on_click(cx.listener(|this, _, _, cx| this.close_page(cx)))
                        .child(icon(IconName::ArrowLeft, 16., sb.icon))
                        .child("Back"),
                ),
                None => el
                    .child(
                        button("sidebar-settings", IconName::Settings, "Settings")
                            .on_click(cx.listener(|this, _, _, cx| this.open_settings("general", cx))),
                    )
                    .child(
                        button("sidebar-pull-requests", IconName::GitPullRequestArrow, "Pull requests").on_click(
                            cx.listener(|this, _, _, cx| this.open_page(super::pages::Page::PullRequests, cx)),
                        ),
                    )
                    .child(
                        button("sidebar-usage", IconName::ChartNoAxesColumn, "Usage")
                            .on_click(cx.listener(|this, _, _, cx| this.open_page(super::pages::Page::Usage, cx))),
                    ),
            })
            .children(self.render_undo_notice(cx))
            .children(self.render_update_button(tokens, cx))
            .children(status.map(|(label, color)| {
                text_xs(h_flex())
                    .ml_auto()
                    .min_w_0()
                    .gap(px(6.))
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(color))
                    .child(div().min_w_0().truncate().text_color(sb.muted_foreground).child(label))
            }))
    }

    /// The footer's update control without a desktop updater: the web's "update
    /// everything" for providers, shown only while one can be updated in one click
    /// (`SidebarUpdatePill.tsx`, `isProviderUpdateCandidate`).
    fn render_update_button(&self, tokens: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let candidates: Vec<(String, String)> = self
            .server_info()
            .providers
            .iter()
            .filter(|p| inbox::is_provider_update_candidate(p))
            .map(|p| (p.driver.clone(), p.instance_id.clone()))
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let sb = tokens.sidebar;
        let label: SharedString = match candidates.len() {
            1 => "Update 1 provider".into(),
            n => format!("Update {n} providers").into(),
        };
        let surface: Hsla = rgb(if tokens.dark { 0x0a0a0a } else { 0xf4f4f5 }).into();
        let tip = label.clone();
        Some(
            div()
                .id("sidebar-update")
                .ml_auto()
                .size(px(32.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_pointer()
                .bg(surface)
                .hover(|s| s.bg(sb.row_hover))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.update_providers(label.clone(), candidates.clone(), window, cx);
                }))
                .child(icon(IconName::CircleArrowUp, 16., sb.foreground))
                .into_any_element(),
        )
    }

    /// Confirms (`UpdateEverythingDialog`), then `server.updateProvider` for each candidate.
    fn update_providers(
        &mut self,
        summary: SharedString,
        candidates: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let answer = window.prompt(
            gpui_kit::PromptLevel::Info,
            &format!("{summary}?"),
            Some("Each provider updates to its latest version."),
            &["Update", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            for (driver, instance_id) in candidates {
                let input = json!({"provider": driver, "instanceId": instance_id});
                if let Err(error) = client.request("server.updateProvider", input).await {
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some(format!("Provider update failed: {error}").into());
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn render_shelf_header(
        &self,
        shelf: Shelf,
        count: usize,
        expanded: bool,
        first: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let color = tokens.sidebar.muted_foreground;
        let chevron = icon(IconName::ChevronDown, 12., color)
            .when(!expanded, |i| i.rotate(gpui_kit::radians(-std::f32::consts::FRAC_PI_2)));
        text_xs(h_flex())
            .id(shelf.label())
            .when(first, gpui_kit::Styled::mt_auto)
            .mx(px(2.))
            .h(px(32.))
            .flex_none()
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(6.))
            .cursor_pointer()
            .font_weight(FontWeight::MEDIUM)
            .text_color(color)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_shelf(shelf, cx)))
            .child(shelf.label())
            .child(div().flex_1())
            .child(div().font_weight(FontWeight::NORMAL).child(count.to_string()))
            .child(chevron)
            .into_any_element()
    }

    fn render_show_more(&self, count: usize, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let sb = tokens.sidebar;
        text_sm(h_flex())
            .id("sidebar-settled-more")
            .h(px(36.))
            .flex_none()
            .px(px(10.))
            .gap(px(10.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_color(sb.muted_foreground)
            .hover(|s| s.bg(sb.row_hover).text_color(sb.foreground))
            .on_click(cx.listener(|this, _, _, cx| {
                this.sidebar.settled_visible += SETTLED_PAGE;
                this.notify_sidebar(cx);
            }))
            .child(icon(IconName::ChevronDown, 16., sb.muted_foreground))
            .child(format!("Show {count} more"))
            .into_any_element()
    }

    fn provider_mark(&self, facts: &Facts, foreground: Hsla) -> Option<AnyElement> {
        let thread = facts.thread;
        let instance = if facts.runtime_status().is_some() {
            thread.provider_instance_id.as_deref()
        } else {
            thread.model_selection.as_ref().map(|m| m.instance_id.as_str())
        }
        .or(thread.provider_instance_id.as_deref())?;
        let driver = &self.env_info(&thread.environment_id)?.provider(instance)?.driver;
        let (path, tint) = crate::assets::provider_icon(driver)?;
        let color = tint.unwrap_or(foreground);
        Some(svg().path(path).size(px(14.)).flex_none().text_color(color.opacity(0.6)).into_any_element())
    }

    fn render_card(
        &self,
        facts: Facts,
        project: Option<&ProjectShell>,
        label: Option<SharedString>,
        active: bool,
        now: i64,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sb = tokens.sidebar;
        let thread = facts.thread;
        let status = facts.status();
        let unread = facts.is_unread();
        let woke = facts.is_woke(now);
        let selected = !active && self.sidebar.selection.contains(&thread.id);
        let draft = !active && self.has_unsent_draft(&thread.id);
        let recede = inbox::recedes(status, unread, woke, active || selected);
        let top = top_status(status, woke, unread, thread.goal.as_ref().is_some_and(|g| g.status == "active"), tokens);
        let woke_status = top.is_some_and(|(_, label, _)| label == "Woke");
        let duration_from = match status {
            Status::Working | Status::Preparing => facts.working_started_at(),
            Status::Waiting => crate::timestamp::parse_ms(facts.latest_run_completed_at().unwrap_or_default()),
            _ => None,
        };
        let status_label: AnyElement = match top {
            Some((name, label, color)) => h_flex()
                .gap(px(4.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color)
                .child(icon(name, 16., color))
                .child(label)
                .children(duration_from.map(|from| inbox::format_duration(now - from)))
                .map(|el| if woke_status { self.woke_dismiss(el, &thread.id, cx) } else { el.into_any_element() }),
            None => div()
                .text_color(sb.secondary_label)
                .children(facts.activity_timestamp().map(|at| inbox::format_relative(now - at)))
                .into_any_element(),
        };
        let menu_open = self.sidebar.menu.as_ref().is_some_and(|m| m.owner.as_deref() == Some(thread.id.as_str()));
        let hovered = self.sidebar.hovered.as_deref() == Some(thread.id.as_str());
        let thread_id = thread.id.clone();
        let snooze = facts.can_snooze(now).then(|| {
            let thread_id = thread.id.clone();
            div()
                .id(SharedString::from(format!("snooze-{}", thread.id)))
                .flex()
                .h_full()
                .items_center()
                .px(px(6.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(tokens.muted_foreground)
                .hover(|s| s.text_color(sb.foreground))
                .tooltip(tooltip("Snooze thread"))
                .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_snooze_menu(&thread_id, event.position(), window, cx);
                }))
                .child(Icon::new(IconName::Clock).size(px(12.)))
        });
        let settle = div()
            .id(SharedString::from(format!("settle-{}", thread.id)))
            .flex()
            .items_center()
            .gap(px(4.))
            .px(px(6.))
            .mr(px(-4.))
            .rounded(px(6.))
            .cursor_pointer()
            .text_color(tokens.muted_foreground)
            .hover(|s| s.text_color(sb.foreground))
            .tooltip(tooltip("Settle thread"))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.settle_thread(&thread_id, true, window, cx);
            }))
            .child(Icon::new(IconName::Check).size(px(14.)))
            .child("Settle");
        let discard = draft.then(|| {
            let target = self.thread_target(&thread.id);
            div()
                .id(SharedString::from(format!("discard-{}", thread.id)))
                .flex()
                .h_full()
                .items_center()
                .px(px(6.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(tokens.muted_foreground)
                .hover(|s| s.text_color(sb.foreground))
                .tooltip(tooltip("Discard draft"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if let Some(target) = target.clone() {
                        this.discard_draft(target, window, cx);
                    }
                }))
                .child(Icon::new(IconName::X).size(px(14.)))
        });
        // A Woke label stays up beside the row controls: it is the button that dismisses it.
        let status_slot =
            text_xs(h_flex()).relative().ml_auto().h(px(20.)).min_w(px(32.)).flex_none().justify_end().child(
                if hovered || menu_open {
                    h_flex()
                        .h_full()
                        .when(woke_status, |el| el.child(status_label))
                        .children(discard)
                        .children(snooze)
                        .child(settle)
                } else {
                    h_flex().h_full().child(status_label)
                },
            );
        let meta = self.render_meta_line(&facts, tokens, cx);
        let title_icons = if meta.is_none() { self.provider_mark(&facts, sb.foreground) } else { None };
        let title_color = if recede {
            sb.foreground.opacity(0.75)
        } else if status == Status::Failed && !unread && !woke {
            sb.foreground.opacity(0.95)
        } else {
            sb.foreground
        };
        let regenerating = thread.title_regeneration.is_some();
        let open_id = thread.id.clone();
        let hover_id = thread.id.clone();
        let has_meta = meta.is_some();
        let receding_run = recede && matches!(status, Status::Working | Status::Preparing);
        let hover_card = self.render_hover_card(&facts, project, tokens);
        div()
            .relative()
            .py(px(2.))
            .children(hover_card)
            .child(
                div()
                    .id(SharedString::from(format!("thread-{}", thread.id)))
                    .group(ROW_GROUP)
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.hover_row(&hover_id, *hovered, cx)))
                    .rounded(px(6.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .when(active, |el| el.bg(sb.row_active))
                    .when(selected, |el| el.bg(row_selected(tokens)))
                    .when(receding_run, |el| el.opacity(0.7))
                    // GPUI allows one hover style per element.
                    .when(!active && !selected || receding_run, |el| {
                        let hover = if draft && !active { tokens.warning.opacity(0.08) } else { sb.row_hover };
                        el.when(draft && !selected, |el| el.bg(tokens.warning.opacity(0.04))).hover(move |s| {
                            let s = s.bg(hover);
                            if receding_run { s.opacity(1.) } else { s }
                        })
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.click_row(&open_id, event, window, cx);
                    }))
                    .child(
                        v_flex()
                            .when(has_meta, |el| el.h(px(78.)))
                            .px(px(10.))
                            .py(px(8.))
                            .child(
                                h_flex()
                                    .h(px(20.))
                                    .min_w_0()
                                    .gap(px(6.))
                                    .when(draft, |el| el.child(drafts::unsent_draft_marker(&thread.id, tokens)))
                                    .children(project.map(|p| project_icon(p, tokens, false, 16., self.favicon(p))))
                                    .when(thread.pinned_at.is_some(), |el| {
                                        el.child(self.render_unpin(&thread.id, tokens, cx))
                                    })
                                    .child(
                                        text_xs(div())
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .text_color(sb.secondary_label)
                                            .children(label.or_else(|| project.map(|p| p.title.clone().into()))),
                                    )
                                    .child(status_slot),
                            )
                            .child(
                                h_flex()
                                    .mt(px(4.))
                                    .min_w_0()
                                    .gap(px(6.))
                                    .child(
                                        text_sm(div())
                                            .min_w_0()
                                            .flex_1()
                                            .truncate()
                                            .font_weight(if recede { FontWeight::NORMAL } else { FontWeight::MEDIUM })
                                            .text_color(title_color)
                                            .when(recede, |el| {
                                                el.group_hover(ROW_GROUP, |s| s.text_color(sb.foreground))
                                            })
                                            .when(regenerating, |el| el.opacity(0.55))
                                            .child(thread.title.clone()),
                                    )
                                    .children(title_icons)
                                    .map(|el| self.with_rename(el, &thread.id, 0., tokens, cx)),
                            )
                            .children(meta),
                    )
                    .context_menu(self.row_menu(&thread.id, cx)),
            )
            .into_any_element()
    }

    /// The card's branch and terminal line (`metaLine`), or `None` when only its trailing
    /// icons would remain and they move to the title line.
    fn render_meta_line(&self, facts: &Facts, tokens: &Tokens, cx: &mut Context<Self>) -> Option<gpui_kit::Div> {
        let sb = tokens.sidebar;
        let thread = facts.thread;
        let terminals = self.render_running_terminals(&thread.id, cx);
        (thread.branch.is_some() || terminals.is_some()).then(|| {
            text_xs(h_flex())
                .mt(px(2.))
                .gap(px(6.))
                .min_w_0()
                .text_color(sb.secondary_label)
                .when(thread.worktree_path.is_some() && thread.branch.is_some(), |el| {
                    el.child(icon(IconName::FolderGit2, 14., sb.muted_foreground))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_middle()
                        .children(thread.branch.clone()),
                )
                .children(terminals)
                .children(self.provider_mark(facts, sb.foreground))
        })
    }

    /// A boring sidebar folder header (`SidebarProjectFolderHeader`): it reads like a shelf
    /// header, with the hidden count and a status dot only while it is closed.
    fn render_folder(
        &self,
        group: &ProjectGroup,
        collapsed: bool,
        hidden: usize,
        dot: Option<FolderDot>,
        first: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sb = tokens.sidebar;
        let representative = group.representative();
        let chevron = icon(IconName::ChevronDown, 12., sb.muted_foreground)
            .when(collapsed, |i| i.rotate(gpui_kit::radians(-std::f32::consts::FRAC_PI_2)));
        let dot = dot.map(|dot| {
            let color = match dot {
                FolderDot::Attention => tokens.warning,
                FolderDot::Failed => tokens.error,
                FolderDot::Unread => rgb(SUCCESS).into(),
                FolderDot::Working => tokens.info,
            };
            div().size(px(6.)).flex_none().rounded_full().bg(color)
        });
        let key = group.key.clone();
        let project_id = representative.id.clone();
        let new_thread_label: SharedString = format!("New thread in {}", group.display_name).into();
        let new_thread = div()
            .id(SharedString::from(format!("folder-new-{}", group.key)))
            .absolute()
            .top_0()
            .bottom_0()
            .right(px(24.))
            .flex()
            .items_center()
            .invisible()
            .group_hover(FOLDER_GROUP, gpui_kit::Styled::visible)
            .child(
                div()
                    .id(SharedString::from(format!("folder-new-button-{}", group.key)))
                    .size(px(24.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(tokens.muted_foreground)
                    .hover(|s| s.bg(tokens.accent).text_color(tokens.foreground))
                    .tooltip(move |window, cx| Tooltip::new(new_thread_label.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.start_draft(project_id.clone(), window, cx);
                    }))
                    .child(Icon::new(IconName::SquarePen).size(px(14.))),
            );
        div()
            .id(SharedString::from(format!("folder-{}", group.key)))
            .group(FOLDER_GROUP)
            .relative()
            .flex_none()
            .when(!first, |el| el.mt(px(8.)))
            .child(
                text_sm(h_flex())
                    .id(SharedString::from(format!("folder-row-{}", group.key)))
                    .h(px(32.))
                    .px(px(10.))
                    .gap(px(10.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(sb.row_hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_folder(&key, cx)))
                    .child(project_icon(representative, tokens, false, 16., self.favicon(representative)))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(sb.foreground.opacity(0.9))
                            .child(group.display_name.clone()),
                    )
                    .children(dot)
                    .when(collapsed && hidden > 0, |el| {
                        el.child(
                            text_xs(div())
                                .flex_none()
                                .text_color(sb.muted_foreground)
                                .group_hover(FOLDER_GROUP, gpui_kit::Styled::invisible)
                                .child(hidden.to_string()),
                        )
                    })
                    .child(chevron)
                    .context_menu(self.folder_menu(group, cx)),
            )
            .child(new_thread)
            .into_any_element()
    }

    /// The folder's right-click menu (`handleProjectFolderContextMenu`).
    fn folder_menu(
        &self,
        group: &ProjectGroup,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let workspace = cx.weak_entity();
        let project_id = group.representative().id.clone();
        let root = group.representative().workspace_root.clone();
        let member_ids: Vec<String> = group.members.iter().map(|m| m.project.id.clone()).collect();
        let pinned = group.pinned;
        move |menu, _, _| {
            let (new_in, pin) = (workspace.clone(), workspace.clone());
            let settings = workspace.clone();
            let (project_id, root, member_ids) = (project_id.clone(), root.clone(), member_ids.clone());
            menu.item(PopupMenuItem::new("New thread").on_click(move |_, window, cx| {
                let _ = new_in.update(cx, |this, cx| this.start_draft(project_id.clone(), window, cx));
            }))
            .item(PopupMenuItem::new("Copy path").on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(root.clone()));
            }))
            .item(
                PopupMenuItem::new(if pinned { "Unpin project" } else { "Pin project" })
                    .icon(Icon::new(if pinned { IconName::PinOff } else { IconName::Pin }))
                    .on_click(move |_, _, cx| {
                        let _ = pin.update(cx, |this, cx| this.set_projects_pinned(&member_ids, !pinned, cx));
                    }),
            )
            .item(PopupMenuItem::new("Project settings").icon(Icon::new(IconName::Settings)).on_click(
                move |_, _, cx| {
                    let _ = settings.update(cx, |this, cx| this.open_settings("projects", cx));
                },
            ))
        }
    }

    /// A one-line thread row in a folder (`variant="folder"`), or with the card's branch line
    /// under it (`folder-multiline`). Titles line up with the folder name, past its icon.
    fn render_folder_row(
        &self,
        facts: Facts,
        project: Option<&ProjectShell>,
        multiline: bool,
        active: bool,
        now: i64,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sb = tokens.sidebar;
        let thread = facts.thread;
        let status = facts.status();
        let unread = facts.is_unread();
        let woke = facts.is_woke(now);
        let selected = !active && self.sidebar.selection.contains(&thread.id);
        let draft = !active && self.has_unsent_draft(&thread.id);
        let recede = inbox::recedes(status, unread, woke, active || selected);
        let goal = thread.goal.as_ref().is_some_and(|g| g.status == "active");
        let top = top_status(status, woke, unread, goal, tokens);
        let woke_status = top.is_some_and(|(_, label, _)| label == "Woke");
        // Folder rows have no status line, so the status takes the time slot.
        let label: AnyElement = match top {
            Some((name, label, color)) => h_flex()
                .gap(px(4.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(color)
                .child(icon(name, 12., color))
                .child(label)
                .map(|el| if woke_status { self.woke_dismiss(el, &thread.id, cx) } else { el.into_any_element() }),
            None => div()
                .text_color(sb.secondary_label)
                .children(facts.activity_timestamp().map(|at| inbox::format_relative(now - at)))
                .into_any_element(),
        };
        let thread_id = thread.id.clone();
        let hovered = self.sidebar.hovered.as_deref() == Some(thread.id.as_str());
        // Beside a Woke label the action takes its own place (`isWoke && ...:static`).
        let beside = woke_status && hovered;
        let settle = h_flex()
            .id(SharedString::from(format!("folder-settle-{}", thread.id)))
            .when(!beside, |el| el.absolute().top_0().bottom_0().right_0().invisible())
            .h_full()
            .px(px(8.))
            .rounded(px(6.))
            .group_hover(ROW_GROUP, gpui_kit::Styled::visible)
            .text_color(tokens.muted_foreground)
            .hover(|s| s.text_color(sb.foreground))
            .tooltip(tooltip("Settle thread"))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.settle_thread(&thread_id, true, window, cx);
            }))
            .child(Icon::new(IconName::Check).size(px(12.)));
        let time_slot = text_xs(h_flex())
            .relative()
            .ml_auto()
            .h(px(24.))
            .min_w(px(32.))
            .flex_none()
            .justify_end()
            .child(div().when(!woke_status, |el| el.group_hover(ROW_GROUP, gpui_kit::Styled::invisible)).child(label))
            .child(settle);
        let title_color = if recede {
            sb.foreground.opacity(0.75)
        } else if status == Status::Failed && !unread && !woke {
            sb.foreground.opacity(0.95)
        } else {
            sb.foreground
        };
        let title = text_sm(div())
            .min_w_0()
            .flex_1()
            .truncate()
            .font_weight(if recede { FontWeight::NORMAL } else { FontWeight::MEDIUM })
            .text_color(title_color)
            .when(recede, |el| el.group_hover(ROW_GROUP, |s| s.text_color(sb.foreground)))
            .when(thread.title_regeneration.is_some(), |el| el.opacity(0.55))
            .child(thread.title.clone());
        let meta = if multiline { self.render_meta_line(&facts, tokens, cx) } else { None };
        let title_icons = if multiline && meta.is_none() { self.provider_mark(&facts, sb.foreground) } else { None };
        let line = h_flex()
            .when(multiline, |el| el.h(px(24.)))
            .when(!multiline, gpui_kit::Styled::size_full)
            .min_w_0()
            .gap(px(10.))
            .when(draft, |el| el.child(drafts::unsent_draft_marker(&thread.id, tokens)))
            .child(title)
            .map(|el| self.with_rename(el, &thread.id, 36., tokens, cx))
            .children(title_icons);
        let line = if multiline { line } else { line.children(self.render_running_terminals(&thread.id, cx)) };
        let line = line.child(time_slot);
        let receding_run = recede && matches!(status, Status::Working | Status::Preparing);
        let (open_id, hover_id) = (thread.id.clone(), thread.id.clone());
        v_flex()
            .id(SharedString::from(format!("thread-{}", thread.id)))
            .group(ROW_GROUP)
            .relative()
            .flex_none()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.hover_row(&hover_id, *hovered, cx)))
            .children(self.render_hover_card(&facts, project, tokens))
            .map(|el| if multiline { el.justify_center().py(px(6.)) } else { el.h(px(36.)) })
            .pl(px(36.))
            .pr(px(10.))
            .rounded(px(6.))
            .cursor_pointer()
            .when(receding_run, |el| el.opacity(0.7))
            .map(|el| {
                if active {
                    el.bg(sb.row_active)
                } else if selected {
                    el.bg(row_selected(tokens))
                } else {
                    let hover = if draft { tokens.warning.opacity(0.08) } else { sb.row_hover };
                    el.when(draft, |el| el.bg(tokens.warning.opacity(0.04))).hover(move |s| {
                        let s = s.bg(hover);
                        if receding_run { s.opacity(1.) } else { s }
                    })
                }
            })
            .on_click(
                cx.listener(move |this, event: &ClickEvent, window, cx| this.click_row(&open_id, event, window, cx)),
            )
            .child(line)
            .children(meta)
            .context_menu(self.row_menu(&thread.id, cx))
            .into_any_element()
    }

    fn render_slim(
        &self,
        facts: Facts,
        project: Option<&ProjectShell>,
        shelf: Option<Shelf>,
        active: bool,
        now: i64,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sb = tokens.sidebar;
        let thread = facts.thread;
        let status = facts.status();
        let unread = facts.is_unread();
        let woke = facts.is_woke(now);
        let selected = !active && self.sidebar.selection.contains(&thread.id);
        let draft = !active && self.has_unsent_draft(&thread.id);
        let recede = inbox::recedes(status, unread, woke, active || selected);
        let settled = shelf == Some(Shelf::Settled);
        let title_color = if (settled && !active) || recede {
            sb.secondary_label
        } else if active || woke || matches!(status, Status::Input | Status::Request) {
            sb.foreground
        } else if unread {
            sb.muted_foreground
        } else {
            sb.secondary_label.opacity(0.7)
        };
        let label: AnyElement = match (shelf, &thread.snoozed_until) {
            (Some(Shelf::Snoozed), Some(until)) => h_flex()
                .gap(px(4.))
                .text_color(tokens.info_foreground)
                .child(icon(IconName::AlarmClock, 12., tokens.info_foreground))
                .child(inbox::format_wake(crate::timestamp::parse_ms(until).unwrap_or(now) - now))
                .into_any_element(),
            _ if woke => {
                let color = tokens.warning_foreground;
                let label = h_flex()
                    .gap(px(4.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color)
                    .child(icon(IconName::AlarmClock, 12., color))
                    .child("Woke");
                self.woke_dismiss(label, &thread.id, cx)
            }
            _ => {
                let at = if settled { facts.settled_timestamp() } else { facts.activity_timestamp() };
                div()
                    .text_color(sb.secondary_label)
                    .children(at.map(|at| inbox::format_relative(now - at)))
                    .into_any_element()
            }
        };
        let action = match shelf {
            Some(Shelf::Settled) => Some((Shelf::Settled, IconName::Undo2, 14., "Un-settle thread")),
            Some(Shelf::Snoozed) => Some((Shelf::Snoozed, IconName::AlarmClockOff, 12., "Wake thread now")),
            _ => None,
        };
        let hovered = self.sidebar.hovered.as_deref() == Some(thread.id.as_str());
        let beside = woke && hovered;
        let action = action.map(|(action_shelf, name, size, label)| {
            let thread_id = thread.id.clone();
            h_flex()
                .id(SharedString::from(format!("{}-{}", action_shelf.label(), thread.id)))
                .when(!beside, |el| el.absolute().top_0().bottom_0().right(px(-4.)).invisible())
                .h_full()
                .px(px(6.))
                .rounded(px(6.))
                .group_hover(ROW_GROUP, gpui_kit::Styled::visible)
                .text_color(tokens.muted_foreground)
                .hover(|s| s.text_color(sb.foreground))
                .tooltip(tooltip(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.slim_action(action_shelf, &thread_id, cx);
                }))
                .child(Icon::new(name).size(px(size)))
        });
        let muted_icon = !active || settled;
        let hover_card = self.render_hover_card(&facts, project, tokens);
        let open_id = thread.id.clone();
        let hover_id = thread.id.clone();
        h_flex()
            .id(SharedString::from(format!("thread-{}", thread.id)))
            .group(ROW_GROUP)
            .relative()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.hover_row(&hover_id, *hovered, cx)))
            .children(hover_card)
            .h(px(36.))
            .flex_none()
            .px(px(10.))
            .gap(px(10.))
            .rounded(px(6.))
            .cursor_pointer()
            .map(|el| {
                if active {
                    el.bg(sb.row_active)
                } else if selected {
                    el.bg(row_selected(tokens))
                } else if draft {
                    drafts::draft_surface(el, tokens)
                } else {
                    el.hover(|s| s.bg(sb.row_hover))
                }
            })
            .on_click(
                cx.listener(move |this, event: &ClickEvent, window, cx| this.click_row(&open_id, event, window, cx)),
            )
            .children(project.map(|p| {
                div()
                    .flex_none()
                    .when(muted_icon, |el| el.opacity(0.4).group_hover(ROW_GROUP, |s| s.opacity(1.)))
                    .child(project_icon(p, tokens, muted_icon && !hovered, 16., self.favicon(p)))
            }))
            .when(draft, |el| el.child(drafts::unsent_draft_marker(&thread.id, tokens)))
            .child(
                text_sm(div())
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .font_weight(if recede { FontWeight::NORMAL } else { FontWeight::MEDIUM })
                    .text_color(title_color)
                    .group_hover(ROW_GROUP, |s| s.text_color(sb.foreground))
                    .child(thread.title.clone()),
            )
            .map(|el| self.with_rename(el, &thread.id, 36., tokens, cx))
            .children(self.render_running_terminals(&thread.id, cx))
            .child(
                text_xs(h_flex())
                    .relative()
                    .ml_auto()
                    .h(px(24.))
                    .min_w(px(32.))
                    .flex_none()
                    .justify_end()
                    .child(
                        div()
                            .when(action.is_some() && !woke, |el| {
                                el.group_hover(ROW_GROUP, gpui_kit::Styled::invisible)
                            })
                            .child(label),
                    )
                    .children(action),
            )
            .context_menu(self.row_menu(&thread.id, cx))
            .into_any_element()
    }
}

/// The card's top status: icon, label, and color (`SidebarThreadRow.tsx` `topStatus`).
fn top_status(
    status: Status,
    woke: bool,
    unread: bool,
    goal: bool,
    tokens: &Tokens,
) -> Option<(IconName, &'static str, Hsla)> {
    Some(match status {
        Status::Working => (IconName::CircleDashed, if goal { "Goal" } else { "Working" }, tokens.info_foreground),
        Status::Preparing => (IconName::CircleDashed, "Setting up", tokens.info_foreground),
        Status::Waiting => (IconName::CircleDotDashed, "Waiting", tokens.sidebar.muted_foreground),
        Status::Approval => (IconName::ShieldQuestionMark, "Approval", tokens.warning_foreground),
        Status::Input => (IconName::MessageCircleQuestionMark, "Input", tokens.indigo_foreground),
        Status::Request => (IconName::UserRound, "Request", guest_foreground(tokens)),
        Status::Limited => (IconName::CircleAlert, "Limited", tokens.warning_foreground),
        Status::Failed => (IconName::CircleAlert, "Failed", tokens.error_foreground),
        Status::Ready if woke => (IconName::AlarmClock, "Woke", tokens.warning_foreground),
        Status::Ready if unread => (IconName::CircleCheck, "Done", tokens.success_foreground),
        Status::Ready => return None,
    })
}

/// `--sidebar-row-selected`: white in light themes, a 7% tint in dark ones.
fn row_selected(tokens: &Tokens) -> Hsla {
    if tokens.dark { tokens.sidebar.foreground.opacity(0.07) } else { tokens.sidebar.row_active }
}

impl Workspace {
    /// Makes a Woke label the button that dismisses it (`handleAcknowledgeWokeClick`).
    fn woke_dismiss(&self, label: impl IntoElement, thread_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let id = thread_id.to_owned();
        div()
            .id(SharedString::from(format!("woke-{thread_id}")))
            .cursor_pointer()
            .hover(gpui_kit::Styled::underline)
            .tooltip(tooltip("Dismiss Woke notification"))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.acknowledge_woke(&id, cx);
            }))
            .child(label)
            .into_any_element()
    }
}

fn guest_foreground(tokens: &Tokens) -> Hsla {
    rgb(if tokens.dark { 0xf4a8ff } else { 0xc800de }).into()
}

/// A project's repo favicon, fetched once per workspace root and favicon path.
pub(super) enum Favicon {
    Loading,
    Missing,
    Ready(Arc<Image>),
}

impl Workspace {
    fn favicon_state(&self, project: &ProjectShell) -> Option<&Favicon> {
        self.sidebar.favicons.get(&project.environment_id)?.get(&project.workspace_root)?.get(&project.favicon_path)
    }

    pub(super) fn favicon(&self, project: &ProjectShell) -> Option<Arc<Image>> {
        match self.favicon_state(project) {
            Some(Favicon::Ready(image)) => Some(image.clone()),
            _ => None,
        }
    }

    /// Starts fetching the favicons not asked for yet: `assets.createUrl` with a
    /// `project-favicon` resource signs a URL, then the image is read over HTTP
    /// (`createProjectFaviconUrlAtomFamily` in `client-runtime/src/state/assets.ts`).
    fn load_favicons(&mut self, cx: &mut Context<Self>) {
        for project in &self.shell.projects {
            if self.favicon_state(project).is_some() {
                continue;
            }
            let Some(session) = self.session(&project.environment_id) else { continue };
            let (Some(client), Some(base)) =
                (session.client.clone(), session.endpoint.as_ref().map(|e| e.http_base.clone()))
            else {
                continue;
            };
            let (environment, root, path) =
                (project.environment_id.clone(), project.workspace_root.clone(), project.favicon_path.clone());
            self.sidebar
                .favicons
                .entry(environment.clone())
                .or_default()
                .entry(root.clone())
                .or_default()
                .insert(path.clone(), Favicon::Loading);
            let mut resource = json!({"_tag": "project-favicon", "cwd": project.workspace_root});
            if let Some(path) = &project.favicon_path {
                resource["path"] = path.as_str().into();
            }
            cx.spawn(async move |this, cx| {
                let image = async {
                    let signed = client.request("assets.createUrl", json!({"resource": resource})).await.ok()?;
                    let relative = signed["relativeUrl"].as_str()?.to_owned();
                    if relative.split(['?', '#']).next()?.ends_with("/project-favicon-missing") {
                        return None;
                    }
                    let url = format!("{base}{relative}");
                    let fetched = crate::runtime()
                        .spawn(async move {
                            let response = reqwest::get(url).await?.error_for_status()?;
                            let mime = response
                                .headers()
                                .get(reqwest::header::CONTENT_TYPE)
                                .and_then(|v| v.to_str().ok())
                                .map(|v| v.split(';').next().unwrap_or_default().trim().to_owned());
                            Ok::<_, reqwest::Error>((mime, response.bytes().await?.to_vec()))
                        })
                        .await
                        .ok()?
                        .ok()?;
                    let format = fetched.0.as_deref().and_then(ImageFormat::from_mime_type)?;
                    Some(Arc::new(Image::from_bytes(format, fetched.1)))
                }
                .await;
                let _ = this.update(cx, |this, cx| {
                    let state = image.map_or(Favicon::Missing, Favicon::Ready);
                    this.sidebar.favicons.entry(environment).or_default().entry(root).or_default().insert(path, state);
                    this.notify_sidebar(cx);
                });
            })
            .detach();
        }
    }
}

/// `ProjectFavicon`: a custom monogram, else the repo favicon, else a
/// monogram derived from the title (`ProjectMonogram.tsx`).
/// `grayscale` shows it in gray until the row is hovered (`grayscale` in CSS).
pub(in crate::app) fn project_icon(
    project: &ProjectShell,
    tokens: &Tokens,
    grayscale: bool,
    size: f32,
    favicon: Option<Arc<Image>>,
) -> AnyElement {
    let custom = project.project_icon.as_ref().filter(|icon| icon["kind"] == "monogram");
    if let Some(image) = favicon.filter(|_| custom.is_none()) {
        return img(image)
            .size(px(size))
            .flex_none()
            .rounded(px(size / 4.))
            .overflow_hidden()
            .grayscale(grayscale)
            .into_any_element();
    }
    let text =
        custom.and_then(|icon| icon["text"].as_str()).map_or_else(|| inbox::monogram(&project.title), str::to_owned);
    let index = custom
        .and_then(|icon| icon["color"].as_str())
        .and_then(|name| inbox::PROJECT_COLORS.iter().position(|(n, ..)| *n == name))
        .unwrap_or_else(|| inbox::color_index(&project.title));
    let (_, light, dark) = inbox::PROJECT_COLORS[index];
    let hex = if tokens.dark { dark } else { light };
    let color: Hsla = rgb(hex).into();
    // CSS `grayscale(1)`: Rec. 709 luma.
    let luma = |hex: u32| {
        let [_, r, g, b] = hex.to_be_bytes().map(f32::from);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "luminance of 0..=255 channels")]
        let l = (0.2126 * r + 0.7152 * g + 0.0722 * b).round() as u32;
        Hsla::from(rgb(l << 16 | l << 8 | l))
    };
    let shown = if grayscale { luma(hex) } else { color };
    // The web stretches the glyphs to `textLength` 12 (or 6 for one); GPUI has
    // no glyph stretch, so the glyphs spread across that width instead.
    let glyphs: Vec<char> = text.chars().collect();
    let scale = size / 16.;
    div()
        .size(px(size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size / 4.))
        .bg(shown.opacity(0.14))
        .text_color(shown)
        .when(grayscale, |el| el.group_hover(ROW_GROUP, |s| s.bg(color.opacity(0.14)).text_color(color)))
        .font_family(tokens.fonts.mono)
        .text_size(px(9. * scale))
        .line_height(px(size))
        .font_weight(FontWeight::BOLD)
        .child(
            h_flex()
                .w(px(if glyphs.len() > 1 { 12. } else { 6. } * scale))
                .justify_between()
                .when(glyphs.len() < 2, gpui_kit::Styled::justify_center)
                .children(glyphs.into_iter().map(|glyph| SharedString::from(glyph.to_string()))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_art_follows_the_mode_and_a_nightly_server() {
        assert_eq!(stage_art("dev-artwork", None), Some(StageArt::Dev));
        assert_eq!(stage_art("nightly-artwork", Some("0.0.20")), Some(StageArt::Nightly));
        // This build has no stage of its own: `artwork` shows art only for a Nightly server.
        assert_eq!(stage_art("artwork", Some("0.0.20")), None);
        assert_eq!(stage_art("artwork", Some("0.0.20-nightly.20261104.3")), Some(StageArt::Nightly));
        assert_eq!(stage_art("artwork", Some("0.0.20-preview.20261104.12")), Some(StageArt::Nightly));
        assert_eq!(stage_art("artwork", Some("0.0.20-nightly.2026114.3")), None);
        assert_eq!(stage_art("artwork", Some("0.0.20-beta.1")), None);
        assert_eq!(stage_art("pill", Some("0.0.20-nightly.20261104.3")), None);
        assert_eq!(stage_art("none", None), None);
    }
}
