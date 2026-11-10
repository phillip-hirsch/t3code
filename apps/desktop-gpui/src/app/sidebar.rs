//! The left sidebar (`apps/web/src/components/Sidebar.tsx` and
//! `components/sidebar/*`): chrome row, search, the inbox list, and the footer.

mod actions;
pub(super) use actions::{MenuOrigin, Renaming};
mod custom_snooze;
mod drafts;
mod filter;
mod older_settled;
mod provider_update;
mod scope_store;
mod search_results;
mod selection;
mod update_plan;

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
    Entity, FocusHandle, Focusable as _, Font, FontWeight, Hsla, Image, ImageFormat, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, Point, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _, StyledImage as _, Subscription, Task, TextRun,
    WeakEntity, Window, anchored, deferred, div, img, prelude::FluentBuilder as _, px, rems, rgb, svg,
};
use serde_json::json;

use super::pages::settings::code::projects::icon as project_icons;
use super::titlebar::{self, TITLEBAR_HEIGHT};
use super::{Connection, Target, Workspace};
use crate::inbox::{self, Facts, FolderDot, Status};
use crate::model::{ProjectShell, ScopedId, ShellState, ThreadRef, ThreadShell};
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
    /// The width the user chose; [`Workspace::sidebar_width`] keeps it clear of the brand row.
    width: Pixels,
    /// The brand row's minimum width and what it was measured with (font family, rem size, fullscreen).
    brand_fit: Option<((SharedString, Pixels, bool), Pixels)>,
    /// The window's width as last drawn, which caps the sidebar's.
    viewport: Pixels,
    working_expanded: bool,
    snoozed_expanded: bool,
    settled_expanded: bool,
    settled_visible: usize,
    /// Older settled threads the shells left out, read in pages for the Settled shelf.
    older: older_settled::Shelf,
    /// The card under the pointer. Its controls replace the status label, which
    /// changes layout, so it is state rather than a paint-time group hover.
    hovered: Option<ScopedId>,
    /// The row whose hover card shows, after the web's 150 ms delay.
    hover_card: Option<ScopedId>,
    _hover_card_delay: Option<Task<()>>,
    /// Where the rows were drawn last frame; `hovered` and `hover_card` belong to those rows.
    rows_surface: super::sidebar_sheet::RowsSurface,
    /// A menu opened by a click rather than a right-click (the snooze menu).
    menu: Option<OpenMenu>,
    /// The row whose right-click menu is open. Electron's menu is native and leaves the page's
    /// hover on the row, so its Snooze and Settle controls stay up until the menu closes.
    context_menu: Option<ScopedId>,
    /// By environment, workspace root, then favicon path, so a lookup needs no key built.
    favicons: HashMap<SharedString, HashMap<String, HashMap<Option<String>, Favicon>>>,
    renaming: Option<actions::Renaming>,
    /// The row the last single click opened, for double-click rename.
    clicked: Option<ScopedId>,
    undo: Option<actions::UndoNotice>,
    /// The project filter: project group keys, empty for all projects.
    scope: Vec<String>,
    /// The thread route last seen, and the one whose row still waits to scroll into view
    /// (`SidebarRouteReveal`).
    route_seen: Option<ScopedId>,
    reveal: Option<ScopedId>,
    scroll: ScrollHandle,
    groups: RefCell<GroupsCache>,
    search_ranking: RefCell<Option<search_results::Ranking>>,
    /// Boring sidebar folders the user closed, by group key; folders start open.
    collapsed_folders: HashSet<String>,
    /// Inbox (pinned, active, working) threads in sidebar order, as last drawn.
    inbox_order: Vec<ScopedId>,
    /// Every thread row in sidebar order, as last drawn, for Shift-click ranges.
    thread_order: Vec<ScopedId>,
    selection: selection::Selection,
    /// The open draft's row as it was when opened; `None` when it had no content then
    /// (`SidebarDraftBlock`'s `frozenActive`).
    open_draft_preview: Option<SharedString>,
    provider_pill: provider_update::PillState,
    /// Each local checkout's current branch, read when a hover card opens, by environment and
    /// workspace root.
    checkout_branches: HashMap<SharedString, HashMap<String, String>>,
    filter: Option<filter::FilterMenu>,
    filter_closed_at: Option<std::time::Instant>,
    /// The search result Up/Down or the pointer picked; `None` paints none.
    search_highlight: Option<ScopedId>,
    messages: super::palette::MessageSearch,
    /// Keyboard navigation of the rows (`sidebarTree.ts`).
    pub(super) list_focus: FocusHandle,
    /// The header's filter, Add project and New thread buttons, tab stops after the search box.
    header_focus: [FocusHandle; 3],
    /// The sheet's "Toggle main sidebar" button, its first tab stop.
    pub(super) sheet_toggle: FocusHandle,
    /// The chrome row's brand ("T3 Code home"), Go back and Go forward.
    chrome_focus: [FocusHandle; 3],
    /// The keyboard row, and the rows in order.
    cursor: Option<NavRow>,
    nav_rows: Vec<NavRow>,
    search: Entity<InputState>,
    returns: inbox::ReturnTracker,
    /// When `_clock` wakes the sidebar: its next visible label, snooze or hold deadline.
    clock_at: Option<i64>,
    _clock: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SidebarState {
    /// Records the window's width, and measures the brand row's minimum width again when its
    /// font, text size or the traffic lights' room changed.
    pub(super) fn fit_window(&mut self, window: &mut Window) {
        self.viewport = window.viewport_size().width;
        let font = Font { weight: FontWeight::MEDIUM, ..window.text_style().font() };
        let key = (font.family.clone(), window.rem_size(), window.is_fullscreen());
        if self.brand_fit.as_ref().is_some_and(|(fitted, _)| *fitted == key) {
            return;
        }
        let size = rems(0.875).to_pixels(window.rem_size());
        let text_system = window.text_system();
        let cap_height = text_system.cap_height(text_system.resolve_font(&font), size);
        let run = TextRun {
            len: "Code".len(),
            font,
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let code = text_system.shape_line("Code".into(), size, &[run], None).width;
        let probe = brand_probe_width(titlebar::collapsed_content_left(window), cap_height, code, size);
        self.brand_fit = Some((key, min_width(probe)));
    }

    fn min_width(&self) -> Pixels {
        self.brand_fit.as_ref().map_or(px(MIN_WIDTH), |(_, min)| *min)
    }

    /// The footer offers to undo a thread action.
    pub(super) fn has_undo(&self) -> bool {
        self.undo.is_some()
    }

    /// The thread rows as last drawn, top to bottom (the web's `orderedThreadKeys`).
    pub(super) fn ordered_threads(&self) -> impl Iterator<Item = &ScopedId> {
        self.nav_rows.iter().filter_map(|row| match row {
            NavRow::Thread(key) => Some(key),
            _ => None,
        })
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let list_focus = cx.focus_handle().tab_stop(true);
        let header_focus: [FocusHandle; 3] = std::array::from_fn(|_| cx.focus_handle().tab_stop(true));
        let sheet_toggle = cx.focus_handle().tab_stop(true);
        let chrome_focus: [FocusHandle; 3] = std::array::from_fn(|_| cx.focus_handle().tab_stop(true));
        // The sidebar is a cached view: focus moving in or out of the search box, the
        // header buttons or the list changes what it draws, so it re-renders on those too.
        let mut subscriptions = vec![cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.on_search_changed(cx),
            InputEvent::Focus | InputEvent::Blur => this.notify_sidebar(cx),
            InputEvent::PressEnter { .. } => {}
        })];
        for focus in header_focus.iter().chain(&chrome_focus).chain([&list_focus, &sheet_toggle]) {
            subscriptions.push(cx.on_focus(focus, window, |this, _, cx| this.notify_sidebar(cx)));
            subscriptions.push(cx.on_blur(focus, window, |this, _, cx| this.notify_sidebar(cx)));
        }
        Self {
            width: px(DEFAULT_WIDTH),
            brand_fit: None,
            viewport: window.viewport_size().width,
            working_expanded: false,
            snoozed_expanded: false,
            settled_expanded: false,
            settled_visible: SETTLED_INITIAL,
            older: older_settled::Shelf::default(),
            hovered: None,
            hover_card: None,
            _hover_card_delay: None,
            rows_surface: super::sidebar_sheet::RowsSurface::Docked,
            menu: None,
            context_menu: None,
            favicons: HashMap::new(),
            renaming: None,
            clicked: None,
            undo: None,
            scope: scope_store::load(),
            route_seen: None,
            reveal: None,
            scroll: ScrollHandle::new(),
            groups: RefCell::default(),
            search_ranking: RefCell::default(),
            collapsed_folders: HashSet::new(),
            inbox_order: Vec::new(),
            thread_order: Vec::new(),
            selection: Default::default(),
            open_draft_preview: None,
            provider_pill: Default::default(),
            checkout_branches: HashMap::new(),
            filter: None,
            filter_closed_at: None,
            search_highlight: None,
            messages: Default::default(),
            list_focus,
            header_focus,
            sheet_toggle,
            chrome_focus,
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

/// `resolveSidebarRouteReveal`'s `widensScope`: the filter hides a listed thread.
fn reveal_widens_scope(thread: &ThreadShell, groups: &[ProjectGroup], scope: &[String]) -> bool {
    let scoped = scoped_groups(groups, scope);
    !scoped.is_empty()
        && thread.is_listed()
        && thread.archived_at.is_none()
        && thread.deleted_at.is_none()
        && !scoped.iter().any(|g| g.contains_ref(thread.project_ref()))
}

/// The filter's groups that still exist and are open, in sidebar order; empty is all projects.
fn scoped_groups<'g>(groups: &'g [ProjectGroup], scope: &[String]) -> Vec<&'g ProjectGroup> {
    project_groups::pickable(groups).into_iter().filter(|g| scope.contains(&g.key)).collect()
}

/// The open thread of a target, on its environment.
fn target_thread_ref(target: &Target) -> Option<ThreadRef<'_>> {
    match target {
        Target::Thread { env, id } => Some((env, id)),
        Target::Draft { .. } => None,
    }
}

/// `resolveSidebarStageBackdropVariant`: the stage art behind the sidebar header and on the
/// send button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StageArt {
    Nightly,
    Dev,
}

/// The version this build ships as: release packaging sets `T3CODE_APP_VERSION` (a Nightly
/// build is `<version>-nightly.<yyyymmdd>.<n>`), as Electron's version comes from its release.
const APP_VERSION: &str = match option_env!("T3CODE_APP_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

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

/// `resolveDesktopAppStageLabel`: this build's own stage (`APP_STAGE_LABEL`).
fn build_stage(development: bool, app_version: &str) -> &'static str {
    if development {
        "Dev"
    } else if is_nightly_version(app_version) {
        "Nightly"
    } else {
        "Beta"
    }
}

/// `resolveServerBackedAppStageLabel`: a Nightly server wins, else the build's own stage.
fn stage_label(server_version: Option<&str>, build: &'static str) -> &'static str {
    if server_version.is_some_and(is_nightly_version) { "Nightly" } else { build }
}

fn stage_art(mode: &str, stage_label: &str) -> Option<StageArt> {
    match (mode, stage_label) {
        ("nightly-artwork", _) | ("artwork", "Nightly") => Some(StageArt::Nightly),
        ("dev-artwork", _) | ("artwork", "Dev") => Some(StageArt::Dev),
        _ => None,
    }
}

/// `resolveEnvironmentIdentificationPillLabel`, shown in `pill` mode.
fn stage_pill(mode: &str, stage_label: &'static str) -> Option<&'static str> {
    (mode == "pill" && matches!(stage_label, "Dev" | "Nightly")).then_some(stage_label)
}

/// `environmentIdentificationOptions`: `artwork` is named after the art this stage shows.
pub(super) fn identification_options(stage_label: &str) -> &'static [(&'static str, &'static str)] {
    match stage_art("artwork", stage_label) {
        Some(StageArt::Dev) => &[
            ("artwork", "Dev artwork"),
            ("nightly-artwork", "Nightly artwork"),
            ("pill", "Version pill"),
            ("none", "None"),
        ],
        Some(StageArt::Nightly) => &[
            ("artwork", "Nightly artwork"),
            ("dev-artwork", "Dev artwork"),
            ("pill", "Version pill"),
            ("none", "None"),
        ],
        None => &[("artwork", "None"), ("nightly-artwork", "Nightly artwork"), ("dev-artwork", "Dev artwork")],
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
    img(stage_image(art, dark, true)).absolute().top_0().left_0().size_full().rounded_full().into_any_element()
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
    owner: Option<ScopedId>,
    _dismiss: Subscription,
}

/// Drag payload for the resize rail.
struct SidebarResize;

/// A row the list's keyboard cursor rests on (`sidebarTree.ts`).
#[derive(Clone, PartialEq, Eq)]
enum NavRow {
    Thread(ScopedId),
    /// A boring sidebar folder, by group key.
    Folder(String),
    Shelf(Shelf),
    SettledMore,
}

/// A row element's id: thread ids repeat across environments, so it names both.
fn row_id(prefix: &str, env: &str, id: &str) -> SharedString {
    format!("{prefix}:{env}:{id}").into()
}

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
    /// "Show N more" under the Settled shelf, or its Loading and Retry states.
    ShowMore {
        count: usize,
        loading: bool,
        failed: bool,
    },
}

impl Item<'_> {
    fn thread(&self) -> Option<&ThreadShell> {
        match self {
            Item::Card(facts) | Item::Slim(facts, _) | Item::FolderRow(facts) => Some(facts.thread),
            _ => None,
        }
    }

    fn thread_key(&self) -> Option<ScopedId> {
        self.thread().map(ScopedId::of_thread)
    }

    /// The keyboard-navigation key of a focusable row.
    fn nav_key(&self) -> Option<NavRow> {
        match self {
            Item::Card(facts) | Item::Slim(facts, _) | Item::FolderRow(facts) => {
                Some(NavRow::Thread(ScopedId::of_thread(facts.thread)))
            }
            Item::Folder { group, .. } => Some(NavRow::Folder(group.key.clone())),
            Item::Header { shelf, .. } => Some(NavRow::Shelf(*shelf)),
            Item::ShowMore { .. } => Some(NavRow::SettledMore),
            _ => None,
        }
    }
}

/// A card row's list item padding (`py-0.5`).
const CARD_ITEM_INSET: f32 = 2.;

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

/// `SidebarBrandWidthProbe`: the chrome row's brand mark at `content_left`, room for the two
/// history arrows, `pr-3` and a 1px border. The wordmark is `h-[1cap] w-auto` on its 94.39 x 56.96
/// viewBox, and "Code" is `tracking-tight` (-0.025em after each of its 4 letters).
fn brand_probe_width(content_left: Pixels, cap_height: Pixels, code: Pixels, size: Pixels) -> Pixels {
    let wordmark = cap_height * (94.3941 / 56.96);
    let code = code - size * (0.025 * 4.);
    content_left + wordmark + px(4.) + code + px(4.) + px(56.) + px(12.) + px(1.)
}

/// `resolveThreadSidebarMinimumWidth`: the brand row never gets cut off. Past macOS's traffic
/// lights it is wider than the 256px default.
fn min_width(brand_probe: Pixels) -> Pixels {
    px(MIN_WIDTH.max(f32::from(brand_probe).ceil()))
}

/// `clampThreadSidebarWidth`: the chosen width, at least `min` and leaving the main column its
/// [`MIN_MAIN_WIDTH`] in a `viewport`-wide window.
fn clamp_width(width: Pixels, min: Pixels, viewport: Pixels) -> Pixels {
    let max = (viewport.floor() - px(MIN_MAIN_WIDTH)).max(min);
    width.max(min).min(max)
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

/// A `<button>`: Tab reaches it, Enter or Space clicks it, and keyboard focus shows the ring.
pub(super) fn keyboard_button(el: gpui_kit::Stateful<gpui_kit::Div>, ring: Hsla) -> gpui_kit::Stateful<gpui_kit::Div> {
    el.focusable().tab_stop(true).focus_visible(move |s| s.border_2().border_color(ring))
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
        fn route_only<'a>(list: Vec<Facts<'a>>, expanded: bool, route: Option<ThreadRef>) -> Vec<Facts<'a>> {
            if expanded { list } else { list.into_iter().filter(|f| Some(f.thread.thread_ref()) == route).collect() }
        }
        let route = target.and_then(target_thread_ref);
        let scoped = scoped_groups(groups, &self.scope);
        let scope_ids: Option<HashSet<ThreadRef>> = (!scoped.is_empty()).then(|| {
            scoped.iter().flat_map(|g| g.project_refs.iter().map(|(env, id)| (env.as_ref(), id.as_str()))).collect()
        });
        let in_scope = |project: ThreadRef| scope_ids.as_ref().is_none_or(|ids| ids.contains(&project));
        // Folders already separate the work, so boring mode has no Working shelf.
        let sections =
            inbox::partition(&shell.threads, now, working_shelf && !boring, boring, &mut self.returns, |t| {
                in_scope(t.project_ref())
            });
        let mut items: Vec<Item> =
            drafts.into_iter().filter(|row| in_scope(row.project.pair())).map(Item::Draft).collect();
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
            let folders = project_groups::folders(&folders, sections.active, |f| f.thread.project_ref(), keep_empty);
            for (group, threads) in folders {
                let collapsed = self.collapsed_folders.contains(&group.key);
                // A closed folder still shows the open thread, and hints at the rest.
                let (shown, hidden): (Vec<Facts>, Vec<Facts>) = if collapsed {
                    threads.into_iter().partition(|f| Some(f.thread.thread_ref()) == route)
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
        // Older settled threads load in pages; the shelf lists them only as far as they are loaded.
        let older_scopes = older_settled::scopes(shell, scope_ids.as_ref());
        self.older.follow(shell, &older_scopes);
        let frontier = self.older.frontier();
        let listed: Vec<Facts<'_>> = (sections.settled.iter().copied())
            .filter(|f| crate::older_settled::is_listed(f.thread, frontier))
            .collect();
        let unloaded: usize =
            older_scopes.iter().map(|(env, projects)| shell.unloaded_older_settled(env, projects.as_deref())).sum();
        let total = sections.settled.len() + unloaded;
        if expanded {
            self.older.grow(listed.len(), self.settled_visible);
        }
        items.push(Item::Header { shelf: Shelf::Settled, count: total, expanded, first });
        let mut settled = listed;
        settled.truncate(self.settled_visible);
        if let Some(route_row) = (sections.settled.iter())
            .find(|f| Some(f.thread.thread_ref()) == route)
            .filter(|f| !settled.iter().any(|s| s.thread.thread_ref() == f.thread.thread_ref()))
        {
            settled.push(*route_row);
        }
        let hidden = total.saturating_sub(settled.len());
        items.extend(route_only(settled, expanded, route).into_iter().map(|f| Item::Slim(f, Some(Shelf::Settled))));
        if expanded && hidden > 0 {
            let (loading, failed) = (self.older.loading(), self.older.failed());
            items.push(Item::ShowMore { count: hidden.min(SETTLED_PAGE), loading, failed });
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

    /// The projects the filter keeps, on their environments, or `None` for all projects.
    fn scoped_projects(&self) -> Option<HashSet<ScopedId>> {
        let groups = self.project_groups();
        let scoped = scoped_groups(&groups, &self.sidebar.scope);
        (!scoped.is_empty()).then(|| {
            scoped.iter().flat_map(|g| g.project_refs.iter().map(|(env, id)| ScopedId::new(env.clone(), id))).collect()
        })
    }

    /// The boring sidebar (Labs).
    fn boring_sidebar(&self) -> bool {
        self.client_settings.values.bool("boringSidebarEnabled")
    }

    /// Whether times read on a 12-hour clock (`timestampFormat`, "System default" by the OS).
    pub(super) fn hour12(&self) -> bool {
        crate::timestamp::hour12(self.client_settings.values.get("timestampFormat").as_str().unwrap_or_default())
    }

    fn identification_mode(&self) -> &str {
        self.client_settings.values.get("environmentIdentificationMode").as_str().unwrap_or("artwork")
    }

    /// `useEnvironmentStageLabel`: the primary server's stage (`primaryServerConfigAtom`), else the build's.
    pub(super) fn stage_label(&self) -> &'static str {
        let version = self.primary_info().config.pointer("/environment/serverVersion").and_then(|v| v.as_str());
        stage_label(version, build_stage(cfg!(debug_assertions), APP_VERSION))
    }

    /// The stage art the sidebar header and send button show (`environmentIdentificationMode`).
    pub(super) fn stage_art(&self) -> Option<StageArt> {
        stage_art(self.identification_mode(), self.stage_label())
    }

    /// The stage pill of `pill` mode.
    fn stage_pill(&self) -> Option<&'static str> {
        stage_pill(self.identification_mode(), self.stage_label())
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

    /// The open thread on its environment.
    pub(super) fn route_key(&self) -> Option<ThreadRef<'_>> {
        self.target.as_ref().and_then(target_thread_ref)
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
    fn click_row(&mut self, key: &ScopedId, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.modifiers();
        let toggle = if cfg!(target_os = "macos") { modifiers.platform } else { modifiers.control };
        if toggle {
            self.sidebar.selection.toggle(key);
            self.notify_sidebar(cx);
            return;
        }
        if modifiers.shift {
            let order = std::mem::take(&mut self.sidebar.thread_order);
            self.sidebar.selection.range_to(key, &order);
            self.sidebar.thread_order = order;
            self.notify_sidebar(cx);
            return;
        }
        if event.click_count() >= 2 {
            // Opening a thread can move the rows, so the second click may land on another one.
            let same_row = self.sidebar.clicked.as_ref() == Some(key);
            if same_row && !event.modifiers().modified() && self.sidebar.renaming.is_none() {
                self.start_rename(key, MenuOrigin::Sidebar, window, cx);
            }
            return;
        }
        self.sidebar.clicked = Some(key.clone());
        self.sidebar.selection.clear();
        self.sidebar.selection.set_anchor(key);
        self.select_thread_in(key.env.clone(), key.id.clone(), window, cx);
        self.notify_sidebar(cx);
    }

    /// The right-click menu builder for a row.
    fn row_menu(
        &self,
        thread: &ThreadShell,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let view = cx.weak_entity();
        let thread_id = ScopedId::of_thread(thread);
        move |menu, window, cx| {
            let Some(workspace) = view.upgrade() else { return menu };
            workspace.update(cx, |this, cx| {
                this.sidebar.context_menu = Some(thread_id.clone());
                cx.notify();
            });
            let (closed_view, closed_id) = (view.clone(), thread_id.clone());
            cx.subscribe_self(move |_, _: &gpui_kit::DismissEvent, cx| {
                let _ = closed_view.update(cx, |this, cx| {
                    if this.sidebar.context_menu.as_ref() == Some(&closed_id) {
                        this.sidebar.context_menu = None;
                        cx.notify();
                    }
                });
            })
            .detach();
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

    /// The pinned card's pin, a button that unpins (`Pin` swaps to `PinOff` on hover). A
    /// connection that can't unpin it, or a machine whose config hasn't loaded, shows the pin.
    fn render_unpin(&self, thread: &ThreadShell, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let muted = tokens.muted_foreground.opacity(0.65);
        let loaded = self.env_info(&thread.environment_id).is_some_and(|info| info.environment_id.is_some());
        if !loaded || !self.can_operate(&thread.environment_id) {
            return div().flex_none().child(icon(IconName::Pin, 12., muted)).into_any_element();
        }
        let group = row_id("unpin", &thread.environment_id, &thread.id);
        let id = ScopedId::of_thread(thread);
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
        thread: &ThreadShell,
        left: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> E {
        let Some(renaming) = self.sidebar.renaming.as_ref().filter(|r| r.thread.is_thread(thread)) else { return el };
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
    fn hover_row(&mut self, key: &ScopedId, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.sidebar.hovered.as_ref() == Some(key) {
                return;
            }
            self.sidebar.hovered = Some(key.clone());
            self.sidebar.hover_card = None;
            let key = key.clone();
            self.sidebar._hover_card_delay = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.sidebar.hovered.as_ref() == Some(&key) {
                        this.read_checkout_branch(&key, cx);
                        this.sidebar.hover_card = Some(key);
                        this.notify_sidebar(cx);
                    }
                });
            }));
        } else if self.sidebar.hovered.as_ref() == Some(key) {
            self.sidebar.hovered = None;
            self.sidebar.hover_card = None;
            self.sidebar._hover_card_delay = None;
        } else {
            return;
        }
        self.notify_sidebar(cx);
    }

    /// Forgets the pointer once the rows move to another surface (a resize hid the docked
    /// sidebar, the sheet opened or closed): the web's row tooltips unmount with their rows.
    pub(super) fn follow_rows_surface(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.rows_surface.follow(self.sidebar_open.surface()) {
            self.sidebar._hover_card_delay = None;
            let hovered = self.sidebar.hovered.take().is_some();
            if self.sidebar.hover_card.take().is_some() || hovered {
                self.notify_sidebar(cx);
            }
        }
    }

    /// A row's menu covers its hover card, as the native menu does; it shows again on the next hover.
    fn hide_hover_card(&mut self, cx: &mut Context<Self>) {
        self.sidebar._hover_card_delay = None;
        if self.sidebar.hover_card.take().is_some() {
            self.notify_sidebar(cx);
        }
    }

    /// A local thread's checkout may have moved to another branch since; the hover card warns
    /// then (`resolveLocalCheckoutBranchMismatch`). Read fresh each time a card opens.
    fn read_checkout_branch(&mut self, key: &ScopedId, cx: &mut Context<Self>) {
        let Some(thread) = self.shell.thread(key) else { return };
        if thread.worktree_path.is_some() || thread.branch.is_none() {
            return;
        }
        let Some(cwd) = self.shell.project(&ScopedId::thread_project(thread)).map(|p| p.workspace_root.clone()) else {
            return;
        };
        let Some(client) = self.env_client(&key.env) else { return };
        let env = key.env.clone();
        cx.spawn(async move |this, cx| {
            let Ok(status) = client.request("vcs.refreshStatus", json!({"cwd": cwd})).await else { return };
            let Some(branch) = status["refName"].as_str().map(str::to_owned) else { return };
            let _ = this.update(cx, |this, cx| {
                let branches = this.sidebar.checkout_branches.entry(env).or_default();
                if branches.get(&cwd) != Some(&branch) {
                    branches.insert(cwd, branch);
                    this.notify_sidebar(cx);
                }
            });
        })
        .detach();
    }

    /// The row's hover card, drawn to the right of the row (`SidebarThreadTooltip`).
    fn render_hover_card(&self, facts: &Facts, project: Option<&ProjectShell>, tokens: &Tokens) -> Option<AnyElement> {
        let thread = facts.thread;
        if !self.sidebar.hover_card.as_ref().is_some_and(|key| key.is_thread(thread))
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
                .and_then(|p| self.sidebar.checkout_branches.get(&p.environment_id)?.get(&p.workspace_root))
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
            .bg(tokens.popover)
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
                        project.map(|p| line(project_icon(p, tokens, false, 12., self.favicon(p)), self.group_name(p))),
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
        } else if let Some(NavRow::Folder(folder)) = &key {
            let collapsed = self.sidebar.collapsed_folders.contains(folder);
            match event.keystroke.key.as_str() {
                "enter" | "space" => {}
                "right" if collapsed => {}
                "left" if !collapsed => {}
                _ => return,
            }
            self.toggle_folder(folder, cx);
        } else {
            let Some(key) = key else { return };
            match (event.keystroke.key.as_str(), key) {
                ("enter" | "space", NavRow::Shelf(shelf)) => self.toggle_shelf(shelf, cx),
                ("right", NavRow::Shelf(shelf)) if !expanded(self, shelf) => self.toggle_shelf(shelf, cx),
                ("left", NavRow::Shelf(shelf)) if expanded(self, shelf) => self.toggle_shelf(shelf, cx),
                ("enter" | "space", NavRow::SettledMore) => self.show_more_settled(cx),
                ("enter" | "space", NavRow::Thread(thread)) => self.select_thread_in(thread.env, thread.id, window, cx),
                // `startRename` refuses a read-only connection.
                ("f2", NavRow::Thread(thread)) => self.start_rename(&thread, MenuOrigin::Sidebar, window, cx),
                _ => return,
            }
        }
        cx.stop_propagation();
        self.notify_sidebar(cx);
    }

    /// `/projects/$projectKey`: Settings › Projects with the group holding `project` picked,
    /// on every environment (`getSidebarProjectSettingsKey`, resolved as Settings groups them).
    pub(in crate::app) fn open_project_settings(&mut self, project: &ScopedId, cx: &mut Context<Self>) {
        let project =
            self.settings_project_groups().into_iter().find(|g| g.contains_ref(project.pair())).map(|g| g.key);
        self.settings_ui.scope = crate::settings::scope::ScopeSearch { project, ..Default::default() };
        self.open_settings("projects", cx);
    }

    /// A new thread route that the filter hides widens it to every project, like the web's
    /// route reveal, so history, links and the palette never open a thread with no row.
    fn reveal_route(&mut self, groups: &[ProjectGroup], searching: bool) {
        let Some(route) = self.route_key() else { return };
        if self.sidebar.route_seen.as_ref().is_none_or(|seen| seen.pair() != route) {
            let route = ScopedId::new(route.0.to_owned(), route.1);
            self.sidebar.route_seen = Some(route.clone());
            self.sidebar.reveal = Some(route);
        }
        // Search results replace the thread list; the reveal waits for it to return.
        let Some(pending) = self.sidebar.reveal.as_ref().filter(|_| !searching) else { return };
        let Some(thread) = self.shell.thread(pending) else { return };
        if reveal_widens_scope(thread, groups, &self.sidebar.scope) {
            self.sidebar.selection.clear();
            self.sidebar.scope.clear();
            scope_store::save(&[]);
            self.sidebar.settled_visible = SETTLED_INITIAL;
        }
    }

    /// Sets the project filter; the settled tail starts over at its first page.
    pub(super) fn set_scope(&mut self, scope: Vec<String>, cx: &mut Context<Self>) {
        // A selection made under one scope can hold threads the next one hides.
        self.sidebar.selection.clear();
        if self.sidebar.scope != scope {
            scope_store::save(&scope);
        }
        self.sidebar.scope = scope;
        self.sidebar.settled_visible = SETTLED_INITIAL;
        // The server's older matches are scoped to the filter's projects.
        self.refresh_sidebar_search(cx);
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

    /// New thread in the open thread's project, else the first project, on its environment.
    pub(super) fn new_thread_project(&self) -> Option<ScopedId> {
        let current = match &self.target {
            Some(Target::Thread { env, id }) => {
                self.shell.thread(&ScopedId::new(env.clone(), id.as_str())).map(ScopedId::thread_project)
            }
            Some(Target::Draft { env, project_id, .. }) => Some(ScopedId::new(env.clone(), project_id.as_str())),
            None => None,
        };
        current.or_else(|| self.shell.projects.iter().find(|p| p.archived_at.is_none()).map(ScopedId::of_project))
    }

    /// Re-renders the sidebar. It is a cached view: the workspace re-rendering
    /// (every timeline scroll frame, every streamed token) leaves it alone.
    pub(super) fn notify_sidebar(&self, cx: &mut Context<Self>) {
        self.sidebar_view.update(cx, |_, cx| cx.notify());
    }

    /// The sidebar view, reused from the last frame unless it was notified or resized.
    pub(super) fn render_cached_sidebar(&self, cx: &App) -> AnyElement {
        let style = StyleRefinement::default().flex_none().h_full().w(self.sidebar_width());
        let view = AnyView::from(self.sidebar_view.clone());
        // A cached view replays its last paint, which would skip the theme inspector's probes.
        if super::theme_inspect::active(cx) {
            return div().flex_none().h_full().w(self.sidebar_width()).child(view).into_any_element();
        }
        view.cached(style).into_any_element()
    }

    pub(super) fn sidebar_width(&self) -> Pixels {
        clamp_width(self.sidebar.width, self.sidebar.min_width(), self.sidebar.viewport)
    }

    /// The return tracker must see every shell change, like the web's. The
    /// render pass feeds it while the sidebar shows; this covers it hidden.
    pub(super) fn observe_inbox_while_hidden(&mut self) {
        if self.sidebar_open.shown() {
            return;
        }
        let requests = inbox::child_requests(&self.shell.threads);
        self.sidebar.returns.observe(&self.shell.threads, &requests, now_ms());
    }

    pub(super) fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load_favicons(cx);
        self.drop_unoperable_renames();
        let tokens = *crate::theme::tokens(cx);
        let sb = tokens.sidebar;
        let now = now_ms();
        let query = self.search_query(cx);
        // Working threads keep their shelf while the environment is disconnected.
        let working_shelf = self.client_settings.values.bool("sidebarWorkingShelfEnabled");
        let boring = self.boring_sidebar();
        let multiline = self.client_settings.values.bool("boringSidebarMultilineRows");
        let groups = self.project_groups();
        self.reveal_route(&groups, !query.is_empty());
        let draft_rows = self.draft_rows();
        let items =
            self.sidebar.items(&self.shell, &groups, self.target.as_ref(), draft_rows, working_shelf, boring, now);
        let revealed = (self.sidebar.reveal.as_ref())
            .filter(|_| query.is_empty())
            .and_then(|key| items.iter().position(|item| item.thread().is_some_and(|t| key.is_thread(t))));
        if let Some(index) = revealed {
            self.sidebar.reveal = None;
            self.sidebar.scroll.scroll_to_item(index);
        }
        let clock = self.sidebar.next_clock_change(&items, &self.shell.threads, now);
        let projects: HashMap<ThreadRef, &ProjectShell> =
            self.shell.projects.iter().map(|p| (p.project_ref(), p)).collect();
        let route = self.route_key();
        let label_of: HashMap<ThreadRef, &str> = groups
            .iter()
            .flat_map(|g| {
                g.project_refs.iter().map(move |(env, id)| ((env.as_ref(), id.as_str()), g.display_name.as_str()))
            })
            .collect();
        let inbox_order: Vec<ScopedId> = items
            .iter()
            .filter_map(|item| matches!(item, Item::Card(_) | Item::FolderRow(_)).then(|| item.thread_key()).flatten())
            .collect();
        let nav_rows: Vec<NavRow> = items.iter().filter_map(Item::nav_key).collect();
        let thread_order: Vec<ScopedId> = items.iter().filter_map(Item::thread_key).collect();
        let list_focused = self.sidebar.list_focus.is_focused(window);
        let cursor = self.sidebar.cursor.clone().filter(|_| list_focused);
        let has_rows = items.iter().any(|item| {
            matches!(item, Item::Card(_) | Item::Slim(..) | Item::Draft(_) | Item::FolderRow(_) | Item::Folder { .. })
        });

        // Under a fixed header the web drops the list group's top padding.
        // The rows are the scroll container's own children, so a route reveal can scroll to one.
        let mut list: Vec<AnyElement> = Vec::new();
        if !query.is_empty() {
            list.push(self.render_search_results(&query, now, &tokens, cx));
        } else if !has_rows && self.sidebar.settled_count_is_zero(&items) {
            list.push(self.render_empty_state(&tokens, cx));
        } else {
            for (index, item) in items.into_iter().enumerate() {
                // Only a focused list draws the cursor ring, so only it needs the row keys.
                let ring = cursor.as_ref().is_some_and(|cursor| item.nav_key().as_ref() == Some(cursor));
                let element = match item {
                    Item::Draft(row) => {
                        self.render_draft(&row, projects.get(&row.project.pair()).copied(), &tokens, cx)
                    }
                    Item::DraftDivider => {
                        div().mx(px(10.)).my(px(6.)).h(px(1.)).bg(sb.border.opacity(0.6)).into_any_element()
                    }
                    Item::Card(facts) => {
                        let active = route == Some(facts.thread.thread_ref());
                        let project = facts.thread.project_ref();
                        let label = label_of.get(&project).map(|name| SharedString::from(name.to_string()));
                        self.render_card(facts, projects.get(&project).copied(), label, active, now, &tokens, cx)
                    }
                    Item::Folder { group, collapsed, hidden, dot } => {
                        self.render_folder(group, collapsed, hidden, dot, index == 0, &tokens, cx)
                    }
                    Item::FolderRow(facts) => {
                        let active = route == Some(facts.thread.thread_ref());
                        let project = projects.get(&facts.thread.project_ref()).copied();
                        self.render_folder_row(facts, project, multiline, active, now, &tokens, cx)
                    }
                    Item::Slim(facts, shelf) => {
                        let active = route == Some(facts.thread.thread_ref());
                        self.render_slim(
                            facts,
                            projects.get(&facts.thread.project_ref()).copied(),
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
                    Item::ShowMore { count, loading, failed } => {
                        self.render_show_more(count, loading, failed, &tokens, cx)
                    }
                };
                list.push(if ring { focus_ring(element, &tokens) } else { element });
            }
        }
        self.sidebar.inbox_order = inbox_order;
        self.sidebar.nav_rows = nav_rows;
        self.sidebar.thread_order = thread_order;
        self.arm_sidebar_clock(clock, now, cx);
        self.read_older_settled_pages(cx);

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

        let sheet = self.sidebar_open.sheet();
        let surface = if sheet { sb.sheet } else { sb.background };
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
                    div().absolute().left_0().right_0().top(px(22.)).h(px(53.)).bg(fade(surface.opacity(0.), surface)),
                )
                .child(div().absolute().left_0().right_0().top(px(74.)).bottom_0().bg(surface))
        });

        v_flex()
            .id("sidebar")
            .children(super::theme_inspect::probe(&["sidebar", "sidebarForeground", "sidebarBorder"], cx))
            .relative()
            .h_full()
            .when(!sheet, |el| el.w(self.sidebar_width()))
            .flex_none()
            .bg(surface)
            .text_color(sb.foreground)
            .border_r_1()
            .border_color(sb.border)
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<SidebarResize>, window, cx| {
                let chosen = event.event.position.x - event.bounds.left();
                this.sidebar.width = clamp_width(chosen, this.sidebar.min_width(), window.viewport_size().width);
                cx.notify();
            }))
            .children(backdrop)
            .child(self.render_chrome_row(&tokens, window, cx))
            .child(self.render_search_row(!query.is_empty(), window, &tokens, cx))
            .child(
                v_flex()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .px(px(8.))
                    .pb(px(8.))
                    .gap(px(1.))
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar.scroll)
                    .track_focus(&self.sidebar.list_focus)
                    .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                        this.list_key(event, window, cx);
                    }))
                    .children(list),
            )
            .children(
                self.render_provider_pill(&tokens, cx)
                    .map(|pill| div().flex_none().px(px(8.)).pt(px(4.)).pb(px(4.)).child(pill)),
            )
            .child(self.render_sidebar_footer(&tokens, cx))
            // The sheet doesn't resize (`resolveSidebarResizable`).
            .when(!sheet, |el| el.child(deferred(rail).with_priority(1)))
            .children(self.render_open_menu())
    }

    fn render_chrome_row(&self, tokens: &Tokens, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let sb = tokens.sidebar;
        // On the stage art the brand turns white and the arrows take `media-navigation`.
        let on_art = self.stage_art().is_some();
        let white = gpui_kit::white();
        let (brand, brand_muted, arrow_color, arrow_hover) = if on_art {
            (white, white.opacity(0.7), white.opacity(0.9), white.opacity(0.1))
        } else {
            (sb.foreground, sb.muted_foreground, sb.muted_foreground, sb.row_hover)
        };
        let ring = tokens.ring;
        let [brand_focus, back_focus, forward_focus] = &self.sidebar.chrome_focus;
        let arrow = |id: &'static str, name: IconName, label: &'static str, enabled: bool, focus: &FocusHandle| {
            div()
                .id(id)
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .when(!enabled, |el| el.opacity(0.64))
                // A disabled button is no tab stop.
                .when(enabled, |el| {
                    titlebar::keyboard_control(el.cursor_pointer().hover(|s| s.bg(arrow_hover)), focus, ring)
                })
                // Inside the drag region a button must occlude, or Windows takes the press as a drag.
                .occlude()
                .tooltip(tooltip(label))
                .child(icon(name, 14., arrow_color))
        };
        let (back, forward) = (self.history.can_go_back(), self.history.can_go_forward());
        let chrome = h_flex().id("sidebar-chrome").h(TITLEBAR_HEIGHT).flex_none().pr(px(12.));
        // In the sheet the header carries the toggle and hides the brand and arrows (`md:hidden`).
        if self.sidebar_open.sheet() {
            return titlebar::drag_region(chrome).child(self.render_sidebar_toggle(window, cx)).into_any_element();
        }
        titlebar::drag_region(chrome)
            .child(
                // `SidebarBrand`: a link to `/`, labelled "T3 Code home".
                titlebar::keyboard_control(
                    h_flex()
                        .id("sidebar-brand")
                        .ml(titlebar::collapsed_content_left(window))
                        .h(px(28.))
                        .gap(px(4.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .occlude(),
                    brand_focus,
                    ring,
                )
                .on_click(cx.listener(|this, _, window, cx| this.go_home(window, cx)))
                .child(svg().path("icons/app/t3-wordmark.svg").w(px(16.9)).h(px(10.2)).mt(px(1.)).text_color(brand))
                .child(text_sm(div()).font_weight(FontWeight::MEDIUM).text_color(brand_muted).child("Code")),
            )
            .child(
                h_flex()
                    .ml(px(4.))
                    .child(
                        arrow("sidebar-back", IconName::ArrowLeft, "Go back", back, back_focus)
                            .on_click(cx.listener(|this, _, window, cx| this.go_back(window, cx))),
                    )
                    .child(
                        arrow("sidebar-forward", IconName::ArrowRight, "Go forward", forward, forward_focus)
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
            .into_any_element()
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
                            keyboard_button(div().id("sidebar-search-clear"), tokens.ring)
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
        // Settings covers these buttons; a tooltip left from the click would sit over it.
        let covered = self.settings.is_some();
        let button = |id: &'static str, name: IconName, label: &'static str| {
            keyboard_button(div().id(id), tokens.ring)
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(sb.row_hover))
                .when(!covered, |el| el.tooltip(tooltip(label)))
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
                    keyboard_button(text_sm(h_flex()).id("sidebar-back-to-chat"), tokens.ring)
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
            .child(self.render_update_button(tokens, cx))
            .children(status.map(|(label, color)| {
                text_xs(h_flex())
                    .ml_auto()
                    .min_w_0()
                    .gap(px(6.))
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(color))
                    .child(div().min_w_0().truncate().text_color(sb.muted_foreground).child(label))
            }))
    }

    /// The footer's update control (`SidebarUpdatePill.tsx`): "update everything" while a
    /// provider on a connected machine can be updated in one click (`useProviderUpdates`), else the desktop
    /// app's "Check for updates", which this build always shows as unavailable since it has
    /// no update feed (`getDesktopUpdateIdleTooltip`).
    fn render_update_button(&self, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let plan = self.provider_update_plan();
        let sb = tokens.sidebar;
        if plan.is_empty() {
            let detail: SharedString = self.about_update_description().into();
            return div()
                .id("sidebar-check-updates")
                .ml_auto()
                .size(px(32.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .cursor_not_allowed()
                .opacity(0.6)
                .tooltip(move |window, cx| {
                    let detail = detail.clone();
                    Tooltip::element(move |_, cx| {
                        let t = crate::theme::tokens(cx);
                        v_flex()
                            .child(div().font_weight(FontWeight::MEDIUM).child("Updates unavailable"))
                            .child(div().text_color(t.muted_foreground).child(detail.clone()))
                    })
                    .build(window, cx)
                })
                .child(icon(IconName::RefreshCw, 16., sb.icon))
                .into_any_element();
        }
        let surface = sb.control_surface;
        let tip = Self::provider_update_summary(&plan);
        keyboard_button(div().id("sidebar-update"), tokens.ring)
            .children(super::theme_inspect::probe(&["sidebarControlSurface", "sidebarForeground"], cx))
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
                this.confirm_provider_updates(plan.clone(), window, cx);
            }))
            .child(icon(IconName::CircleArrowUp, 16., sb.foreground))
            .into_any_element()
    }

    /// Confirms (`UpdateEverythingDialog`), then `server.updateProvider` for each candidate.
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
        keyboard_button(text_xs(h_flex()).id(shelf.label()), tokens.ring)
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

    fn render_show_more(
        &self,
        count: usize,
        loading: bool,
        failed: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sb = tokens.sidebar;
        let label: SharedString = if loading {
            "Loading".into()
        } else if failed {
            "Could not load older threads. Retry".into()
        } else {
            format!("Show {count} more").into()
        };
        keyboard_button(text_sm(h_flex()).id("sidebar-settled-more"), tokens.ring)
            .h(px(36.))
            .flex_none()
            .px(px(10.))
            .gap(px(10.))
            .rounded(px(6.))
            .text_color(sb.muted_foreground)
            // Disabled while a page loads, like the web's `disabled` button.
            .when(!loading, |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(sb.row_hover).text_color(sb.foreground))
                    .on_click(cx.listener(|this, _, _, cx| this.show_more_settled(cx)))
            })
            .child(icon(IconName::ChevronDown, 16., sb.muted_foreground))
            .child(label)
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
        let selected = !active && self.sidebar.selection.has(thread);
        let draft = !active && self.has_unsent_draft(thread);
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
                .map(|el| if woke_status { self.woke_dismiss(el, thread, cx) } else { el.into_any_element() }),
            None => div()
                .text_color(sb.secondary_label)
                .children(facts.activity_timestamp().map(|at| inbox::format_relative(now - at)))
                .into_any_element(),
        };
        let is_row = |key: &Option<ScopedId>| key.as_ref().is_some_and(|key| key.is_thread(thread));
        let menu_open =
            self.sidebar.menu.as_ref().is_some_and(|m| is_row(&m.owner)) || is_row(&self.sidebar.context_menu);
        let hovered = is_row(&self.sidebar.hovered);
        let thread_id = ScopedId::of_thread(thread);
        // `showSnoozeButton`: only where this connection may change the thread.
        let snooze = (facts.can_snooze(now) && self.can_operate(&thread.environment_id)).then(|| {
            let thread_id = thread_id.clone();
            div()
                .id(row_id("snooze", &thread.environment_id, &thread.id))
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
            .id(row_id("settle", &thread.environment_id, &thread.id))
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
            let target = Target::Thread { env: thread.environment_id.clone(), id: thread.id.clone() };
            div()
                .id(row_id("discard", &thread.environment_id, &thread.id))
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
                    this.discard_draft(target.clone(), window, cx);
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
        let open_id = ScopedId::of_thread(thread);
        let hover_id = open_id.clone();
        let has_meta = meta.is_some();
        let receding_run = recede && matches!(status, Status::Working | Status::Preparing);
        // `align="start"` against the row, inside the list item's `py-0.5`.
        let hover_card = self
            .render_hover_card(&facts, project, tokens)
            .map(|card| div().absolute().top(px(CARD_ITEM_INSET)).left_0().right_0().child(card));
        let fill_probe = row_probe(active, selected, cx);
        div()
            .relative()
            .py(px(CARD_ITEM_INSET))
            .children(hover_card)
            .child(
                div()
                    .id(row_id("thread", &thread.environment_id, &thread.id))
                    .children(fill_probe)
                    .group(ROW_GROUP)
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.hover_row(&hover_id, *hovered, cx)))
                    .rounded(px(6.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .when(active, |el| el.bg(sb.row_active))
                    .when(selected, |el| el.bg(tokens.sidebar.row_selected))
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
                                    .when(draft, |el| el.child(drafts::unsent_draft_marker(thread, tokens)))
                                    .children(project.map(|p| project_icon(p, tokens, false, 16., self.favicon(p))))
                                    .when(thread.pinned_at.is_some(), |el| {
                                        el.child(self.render_unpin(thread, tokens, cx))
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
                                    .map(|el| self.with_rename(el, thread, 0., tokens, cx)),
                            )
                            .children(meta),
                    )
                    .on_mouse_down(gpui_kit::MouseButton::Right, cx.listener(|this, _, _, cx| this.hide_hover_card(cx)))
                    .context_menu(self.row_menu(thread, cx)),
            )
            .into_any_element()
    }

    /// The card's branch and terminal line (`metaLine`), or `None` when only its trailing
    /// icons would remain and they move to the title line.
    fn render_meta_line(&self, facts: &Facts, tokens: &Tokens, cx: &mut Context<Self>) -> Option<gpui_kit::Div> {
        let sb = tokens.sidebar;
        let thread = facts.thread;
        let pull_request = self.render_row_pull_request_badge(thread, cx);
        let terminals = self.render_running_terminals(&thread.id, cx);
        (thread.branch.is_some() || terminals.is_some() || pull_request.is_some()).then(|| {
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
                .children(pull_request)
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
        let project = ScopedId::of_project(representative);
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
                        this.start_draft_in(project.env.clone(), project.id.clone(), window, cx);
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

    /// The folder's right-click menu (`handleProjectFolderContextMenu`). Pinning shows only
    /// where this connection may change every member (`canPinProjectGroup`), read on open.
    fn folder_menu(
        &self,
        group: &ProjectGroup,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let workspace = cx.weak_entity();
        let project = ScopedId::of_project(group.representative());
        let root = group.representative().workspace_root.clone();
        let members = group.member_refs();
        let pinned = group.pinned;
        move |mut menu, _, cx| {
            let (new_in, settings) = (workspace.clone(), workspace.clone());
            let (project, settings_project, root) = (project.clone(), project.clone(), root.clone());
            let can_pin = workspace.upgrade().is_some_and(|w| w.read(cx).can_pin_projects(&members));
            menu = menu
                .item(PopupMenuItem::new("New thread").on_click(move |_, window, cx| {
                    let _ = new_in.update(cx, |this, cx| {
                        this.start_draft_in(project.env.clone(), project.id.clone(), window, cx);
                    });
                }))
                .item(PopupMenuItem::new("Copy path").on_click(move |_, _, cx| {
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(root.clone()));
                }));
            if can_pin {
                let (pin, members) = (workspace.clone(), members.clone());
                menu = menu.item(
                    PopupMenuItem::new(if pinned { "Unpin project" } else { "Pin project" })
                        .icon(Icon::new(if pinned { IconName::PinOff } else { IconName::Pin }))
                        .on_click(move |_, _, cx| {
                            let _ = pin.update(cx, |this, cx| this.set_projects_pinned(&members, !pinned, cx));
                        }),
                );
            }
            menu.item(PopupMenuItem::new("Project settings").icon(Icon::new(IconName::Settings)).on_click(
                move |_, _, cx| {
                    let _ = settings.update(cx, |this, cx| this.open_project_settings(&settings_project, cx));
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
        let selected = !active && self.sidebar.selection.has(thread);
        let draft = !active && self.has_unsent_draft(thread);
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
                .map(|el| if woke_status { self.woke_dismiss(el, thread, cx) } else { el.into_any_element() }),
            None => div()
                .text_color(sb.secondary_label)
                .children(facts.activity_timestamp().map(|at| inbox::format_relative(now - at)))
                .into_any_element(),
        };
        let thread_id = ScopedId::of_thread(thread);
        let is_row = |key: &Option<ScopedId>| key.as_ref().is_some_and(|key| key.is_thread(thread));
        let hovered = is_row(&self.sidebar.hovered);
        // Beside a Woke label the action takes its own place (`isWoke && ...:static`).
        let beside = woke_status && hovered;
        let menu_open = is_row(&self.sidebar.context_menu);
        let settle = h_flex()
            .id(row_id("folder-settle", &thread.environment_id, &thread.id))
            .when(!beside, |el| el.absolute().top_0().bottom_0().right_0().invisible())
            .h_full()
            .px(px(8.))
            .rounded(px(6.))
            .group_hover(ROW_GROUP, gpui_kit::Styled::visible)
            .when(menu_open, gpui_kit::Styled::visible)
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
            .child(
                div()
                    .when(!woke_status, |el| el.group_hover(ROW_GROUP, gpui_kit::Styled::invisible))
                    .when(!woke_status && menu_open, gpui_kit::Styled::invisible)
                    .child(label),
            )
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
            .when(draft, |el| el.child(drafts::unsent_draft_marker(thread, tokens)))
            .child(title)
            .map(|el| self.with_rename(el, thread, 36., tokens, cx))
            .children(title_icons);
        let line = if multiline { line } else { line.children(self.render_running_terminals(&thread.id, cx)) };
        let line = line.child(time_slot);
        let receding_run = recede && matches!(status, Status::Working | Status::Preparing);
        let open_id = ScopedId::of_thread(thread);
        let hover_id = open_id.clone();
        v_flex()
            .id(row_id("thread", &thread.environment_id, &thread.id))
            .children(row_probe(active, selected, cx))
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
                    el.bg(tokens.sidebar.row_selected)
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
            .on_mouse_down(gpui_kit::MouseButton::Right, cx.listener(|this, _, _, cx| this.hide_hover_card(cx)))
            .context_menu(self.row_menu(thread, cx))
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
        let selected = !active && self.sidebar.selection.has(thread);
        let draft = !active && self.has_unsent_draft(thread);
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
                self.woke_dismiss(label, thread, cx)
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
        let is_row = |key: &Option<ScopedId>| key.as_ref().is_some_and(|key| key.is_thread(thread));
        let hovered = is_row(&self.sidebar.hovered);
        let beside = woke && hovered;
        let menu_open = is_row(&self.sidebar.context_menu);
        let action = action.map(|(action_shelf, name, size, label)| {
            let thread_id = ScopedId::of_thread(thread);
            h_flex()
                .id(row_id(action_shelf.label(), &thread.environment_id, &thread.id))
                .when(!beside, |el| el.absolute().top_0().bottom_0().right(px(-4.)).invisible())
                .h_full()
                .px(px(6.))
                .rounded(px(6.))
                .group_hover(ROW_GROUP, gpui_kit::Styled::visible)
                .when(menu_open, gpui_kit::Styled::visible)
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
        let open_id = ScopedId::of_thread(thread);
        let hover_id = open_id.clone();
        h_flex()
            .id(row_id("thread", &thread.environment_id, &thread.id))
            .children(row_probe(active, selected, cx))
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
                    el.bg(tokens.sidebar.row_selected)
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
            .when(draft, |el| el.child(drafts::unsent_draft_marker(thread, tokens)))
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
            .map(|el| self.with_rename(el, thread, 36., tokens, cx))
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
                                    .when(menu_open, gpui_kit::Styled::invisible)
                            })
                            .child(label),
                    )
                    .children(action),
            )
            .on_mouse_down(gpui_kit::MouseButton::Right, cx.listener(|this, _, _, cx| this.hide_hover_card(cx)))
            .context_menu(self.row_menu(thread, cx))
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

impl Workspace {
    /// Makes a Woke label the button that dismisses it (`handleAcknowledgeWokeClick`).
    fn woke_dismiss(&self, label: impl IntoElement, thread: &ThreadShell, cx: &mut Context<Self>) -> AnyElement {
        let id = ScopedId::of_thread(thread);
        div()
            .id(row_id("woke", &thread.environment_id, &thread.id))
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

/// `--guest-foreground`: fuchsia-600, fuchsia-300 in the dark.
pub(super) fn guest_foreground(tokens: &Tokens) -> Hsla {
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
            let (Some(client), Some(endpoint)) = (session.client.clone(), session.endpoint.clone()) else {
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
            let resource = project_icons::favicon_resource(&project.workspace_root, project.favicon_path.as_deref());
            cx.spawn(async move |this, cx| {
                // Signed with the session token, as the web's image requests carry its cookie.
                let image = project_icons::fetch(&client, endpoint, resource).await.ok();
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

/// `ProjectFavicon`: a custom icon, else the repo favicon, else a
/// monogram derived from the title (`ProjectMonogram.tsx`).
/// `grayscale` shows it in gray until the row is hovered (`grayscale` in CSS).
pub(in crate::app) fn project_icon(
    project: &ProjectShell,
    tokens: &Tokens,
    grayscale: bool,
    size: f32,
    favicon: Option<Arc<Image>>,
) -> AnyElement {
    let icon = project.project_icon.as_ref().filter(|icon| !icon.is_null());
    // An emoji or Lucide override, such as No project's dashed square, draws as the web's.
    if icon.is_some_and(|icon| icon["kind"] == "emoji" || icon["kind"] == "lucide") {
        return project_icons::project_icon(icon, &project.title, None, size, tokens);
    }
    let custom = icon.filter(|icon| icon["kind"] == "monogram");
    if let Some(image) = favicon.filter(|_| icon.is_none()) {
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

/// A thread row's theme roles for the inspector: its fill when active or selected.
fn row_probe(active: bool, selected: bool, cx: &App) -> Option<AnyElement> {
    let roles: &'static [&'static str] = match (selected, active) {
        (true, _) => &["sidebarRowSelected"],
        (false, true) => &["sidebarRowActive"],
        (false, false) => return None,
    };
    super::theme_inspect::probe(roles, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brand_row_widens_the_sidebar_past_macos_traffic_lights() {
        let size = px(14.);
        // SF Pro at 14px, as CoreText measures it: Electron's macOS sidebar is 257px, its border
        // in the 257th column.
        let sf = |content_left| min_width(brand_probe_width(px(content_left), px(9.864), px(34.909), size));
        assert_eq!(sf(130.), px(257.));
        // In fullscreen the traffic lights hide, and so do Windows' and Linux's (on the right).
        assert_eq!(sf(52.), px(MIN_WIDTH));
        assert_eq!(min_width(brand_probe_width(px(52.), px(10.18), px(36.), size)), px(MIN_WIDTH));
    }

    #[test]
    fn a_route_the_filter_hides_widens_it_unless_the_thread_has_no_row() {
        let group = |key: &str, archived: bool| ProjectGroup {
            key: key.into(),
            display_name: key.into(),
            members: Vec::new(),
            project_ids: vec![key.into()],
            project_refs: vec![("env".into(), key.into())],
            pinned: false,
            archived,
        };
        let thread = |project: &str, extra: serde_json::Value| -> ThreadShell {
            let mut value = json!({
                "id": "t", "projectId": project, "title": "t", "status": "idle",
                "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z",
                "lineage": {"rootThreadId": "t"},
            });
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let mut thread: ThreadShell = serde_json::from_value(value).unwrap();
            thread.environment_id = "env".into();
            thread
        };
        let groups = [group("a", false), group("b", false), group("gone", true)];
        let scope = ["a".to_owned()];
        assert!(reveal_widens_scope(&thread("b", json!({})), &groups, &scope));
        assert!(!reveal_widens_scope(&thread("a", json!({})), &groups, &scope), "already shown");
        assert!(!reveal_widens_scope(&thread("b", json!({})), &groups, &[]), "no filter");
        assert!(!reveal_widens_scope(&thread("b", json!({})), &groups, &["gone".into()]), "an archived scope is all");
        let archived = thread("b", json!({"archivedAt": "2026-11-02T00:00:00.000Z"}));
        assert!(!reveal_widens_scope(&archived, &groups, &scope));
        let sub = thread("b", json!({"lineage": {"rootThreadId": "p", "relationshipToParent": "subagent"}}));
        assert!(!reveal_widens_scope(&sub, &groups, &scope), "listed under its parent");
        // Project `a` on another machine is not in the filter.
        let elsewhere = ThreadShell { environment_id: "copy".into(), ..thread("a", json!({})) };
        assert!(reveal_widens_scope(&elsewhere, &groups, &scope));
    }

    #[test]
    fn the_sidebar_leaves_the_main_column_its_640px() {
        let min = px(MIN_WIDTH);
        let width = |chosen: f32, viewport: f32| clamp_width(px(chosen), min, px(viewport));
        assert_eq!(width(DEFAULT_WIDTH, 800.), min, "an 800px window keeps only the minimum");
        assert_eq!(width(DEFAULT_WIDTH, 870.5), px(230.));
        assert_eq!(width(DEFAULT_WIDTH, 1280.), px(DEFAULT_WIDTH));
        assert_eq!(width(500., 1000.), px(360.));
        assert_eq!(width(100., 1280.), min);
    }

    #[test]
    fn stage_art_follows_the_mode_a_nightly_server_and_the_build() {
        // `resolveDesktopAppStageLabel`: a dev build is Dev, a nightly or preview version Nightly.
        assert_eq!(build_stage(true, "0.1.0"), "Dev");
        assert_eq!(build_stage(false, "0.1.8-nightly.20261106.4288"), "Nightly");
        assert_eq!(build_stage(false, "0.1.8-preview.20261106.12"), "Nightly");
        assert_eq!(build_stage(false, "0.1.0"), "Beta");
        assert_eq!(build_stage(false, "0.1.8-nightly.2026116.3"), "Beta");
        // A Nightly server wins over the build's stage; otherwise the build's stage shows.
        assert_eq!(stage_label(Some("0.0.20-nightly.20261104.3"), "Beta"), "Nightly");
        assert_eq!(stage_label(Some("0.0.20-nightly.20261104.3"), "Dev"), "Nightly");
        assert_eq!(stage_label(Some("0.1.7"), "Dev"), "Dev");
        assert_eq!(stage_label(Some("0.0.20-beta.1"), "Beta"), "Beta");
        assert_eq!(stage_label(None, "Nightly"), "Nightly");

        assert_eq!(stage_art("artwork", "Nightly"), Some(StageArt::Nightly));
        assert_eq!(stage_art("artwork", "Dev"), Some(StageArt::Dev));
        assert_eq!(stage_art("artwork", "Beta"), None);
        assert_eq!(stage_art("dev-artwork", "Beta"), Some(StageArt::Dev));
        assert_eq!(stage_art("nightly-artwork", "Dev"), Some(StageArt::Nightly));
        assert_eq!(stage_art("pill", "Nightly"), None);
        assert_eq!(stage_art("none", "Dev"), None);

        assert_eq!(stage_pill("pill", "Dev"), Some("Dev"));
        assert_eq!(stage_pill("pill", "Beta"), None);
        assert_eq!(stage_pill("artwork", "Nightly"), None);
        assert_eq!(identification_options("Dev")[0], ("artwork", "Dev artwork"));
        assert_eq!(identification_options("Beta")[0], ("artwork", "None"));
    }
}
