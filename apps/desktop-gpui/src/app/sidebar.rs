//! The left sidebar (`apps/web/src/components/Sidebar.tsx` and
//! `components/sidebar/*`): chrome row, search, the inbox list, and the footer.

mod actions;
mod custom_snooze;
mod filter;
mod search_results;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, AnyView, App, AppContext as _, ClickEvent, Context, DismissEvent, DragMoveEvent, EmptyView,
    Entity, FocusHandle, Focusable as _, FontWeight, Hsla, Image, ImageFormat, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Point, Render, SharedString, StatefulInteractiveElement as _, StyleRefinement,
    Styled as _, StyledImage as _, Subscription, Task, WeakEntity, Window, WindowControlArea, anchored, deferred, div,
    img, prelude::FluentBuilder as _, px, rgb, svg,
};
use serde_json::json;

use super::{Connection, Target, Workspace, project_name};
use crate::inbox::{self, Facts, Status};
use crate::model::{ProjectShell, ShellState, ThreadShell};
use crate::theme::{FONT_MONO, Tokens};

const DEFAULT_WIDTH: f32 = 256.;
const MIN_WIDTH: f32 = 208.;
/// The main column never gets narrower than this by dragging.
const MIN_MAIN_WIDTH: f32 = 640.;
const SETTLED_INITIAL: usize = 10;
const SETTLED_PAGE: usize = 25;
const ROW_GROUP: &str = "sidebar-row";

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
    favicons: HashMap<String, Favicon>,
    renaming: Option<actions::Renaming>,
    /// The row the last single click opened, for double-click rename.
    clicked: Option<String>,
    undo: Option<actions::UndoNotice>,
    /// The project filter: project ids, empty for all projects.
    scope: Vec<String>,
    /// Inbox (pinned, active, working) thread ids in sidebar order, as last drawn.
    inbox_order: Vec<String>,
    filter: Option<filter::FilterMenu>,
    filter_closed_at: Option<std::time::Instant>,
    /// The search result Up/Down or the pointer picked; `None` paints none.
    search_highlight: Option<String>,
    messages: super::palette::MessageSearch,
    /// Keyboard navigation of the rows (`sidebarTree.ts`).
    list_focus: FocusHandle,
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
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let list_focus = cx.focus_handle().tab_stop(true);
        // The sidebar is a cached view: focus moving in or out of the search box or
        // the list changes what it draws, so it re-renders on those too.
        let subscriptions = vec![
            cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => this.on_search_changed(cx),
                InputEvent::Focus | InputEvent::Blur => this.notify_sidebar(cx),
                _ => {}
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
            inbox_order: Vec::new(),
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
    Draft(&'a str),
    DraftDivider,
    Card(Facts<'a>),
    Slim(Facts<'a>, Option<Shelf>),
    Header { shelf: Shelf, count: usize, expanded: bool, first: bool },
    ShowMore(usize),
}

impl Item<'_> {
    fn thread_id(&self) -> Option<String> {
        match self {
            Item::Card(facts) | Item::Slim(facts, _) => Some(facts.thread.id.clone()),
            _ => None,
        }
    }

    /// The keyboard-navigation key of a focusable row.
    fn nav_key(&self) -> Option<SharedString> {
        match self {
            Item::Card(facts) | Item::Slim(facts, _) => Some(format!("thread:{}", facts.thread.id).into()),
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
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn text_xs<E: gpui_kit::Styled>(el: E) -> E {
    el.text_size(px(12.)).line_height(px(16.))
}

fn text_sm<E: gpui_kit::Styled>(el: E) -> E {
    el.text_size(px(14.)).line_height(px(20.))
}

fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

fn tooltip(label: &'static str) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView + 'static {
    move |window, cx| Tooltip::new(label).build(window, cx)
}

impl SidebarState {
    /// The rows to draw, in order: the web's `sidebarListItemsFresh`.
    fn items<'a>(
        &mut self,
        shell: &'a ShellState,
        target: Option<&'a Target>,
        working_shelf: bool,
        now: i64,
    ) -> Vec<Item<'a>> {
        let route = match target {
            Some(Target::Thread(id)) => Some(id.as_str()),
            _ => None,
        };
        let scope = &self.scope;
        let in_scope = |project_id: &str| scope.is_empty() || scope.iter().any(|s| s == project_id);
        let sections =
            inbox::partition(&shell.threads, now, working_shelf, &mut self.returns, |t| in_scope(&t.project_id));
        let mut items = Vec::new();
        if let Some(Target::Draft { project_id }) =
            target.filter(|t| matches!(t, Target::Draft { project_id } if in_scope(project_id)))
        {
            items.push(Item::Draft(project_id));
            items.push(Item::DraftDivider);
        }
        items.extend(sections.pinned.into_iter().chain(sections.active).map(Item::Card));
        fn route_only<'a>(list: Vec<Facts<'a>>, expanded: bool, route: Option<&str>) -> Vec<Facts<'a>> {
            if expanded { list } else { list.into_iter().filter(|f| Some(f.thread.id.as_str()) == route).collect() }
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
            Item::Card(_) | Item::Slim(..) | Item::Draft(_) => false,
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
                        if let Some(from) = inbox::parse_ms(facts.latest_run_completed_at().unwrap_or_default()) {
                            at(inbox::duration_changes_in(now - from));
                        }
                    }
                    _ => {
                        if let Some(from) = facts.activity_timestamp() {
                            at(inbox::relative_changes_in(now - from));
                        }
                    }
                },
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
            if let Some(delay) = inbox::parse_ms(thread.snoozed_until.as_deref().unwrap_or_default())
                .and_then(|until| inbox::wake_changes_in(until - now))
            {
                at(delay);
            }
        }
        next
    }
}

impl Workspace {
    fn search_query(&self, cx: &App) -> String {
        self.sidebar.search.read(cx).value().trim().to_lowercase()
    }

    fn route_thread(&self) -> Option<&str> {
        match &self.target {
            Some(Target::Thread(id)) => Some(id),
            _ => None,
        }
    }

    /// Wakes the sidebar after `delay` ms, keeping a pending wake that still
    /// fits. Idle sidebars wake at most once a minute; only a duration in its
    /// first minute ticks every second.
    fn arm_sidebar_clock(&mut self, delay: Option<i64>, now: i64, cx: &mut Context<Self>) {
        let at = delay.map(|delay| now + delay.max(0) + 5);
        if at == self.sidebar.clock_at {
            return;
        }
        self.sidebar.clock_at = at;
        self.sidebar._clock = at.map(|at| {
            cx.spawn(async move |this, cx| {
                let delay = (at - now_ms()).max(0) as u64;
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
        if event.click_count() >= 2 {
            // Opening a thread can move the rows, so the second click may land on another one.
            let same_row = self.sidebar.clicked.as_deref() == Some(thread_id);
            if same_row && !event.modifiers().modified() && self.sidebar.renaming.is_none() {
                self.start_rename(thread_id, window, cx);
            }
            return;
        }
        self.sidebar.clicked = Some(thread_id.to_owned());
        self.select_thread(thread_id.to_owned(), window, cx);
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
            let model = view.upgrade().and_then(|workspace| workspace.read(cx).thread_menu(&thread_id, cx));
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
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.unpin_thread(&id, cx);
            }))
            .child(div().absolute().group_hover(group.clone(), |s| s.invisible()).child(icon(
                IconName::Pin,
                12.,
                muted,
            )))
            .child(div().absolute().invisible().group_hover(group, |s| s.visible()).child(icon(
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
                        this.cancel_rename(cx);
                    }
                }))
                .child(
                    Input::new(&renaming.input)
                        .w_full()
                        .text_size(px(14.))
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
            self.sidebar.hovered = Some(thread_id.to_owned());
            self.sidebar.hover_card = None;
            let thread_id = thread_id.to_owned();
            self.sidebar._hover_card_delay = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(150)).await;
                let _ = this.update(cx, |this, cx| {
                    if this.sidebar.hovered.as_ref() == Some(&thread_id) {
                        this.sidebar.hover_card = Some(thread_id);
                        this.notify_sidebar(cx);
                    }
                });
            }));
        } else if self.sidebar.hovered.as_deref() == Some(thread_id) {
            self.sidebar.hovered = None;
            self.sidebar.hover_card = None;
            self.sidebar._hover_card_delay = None;
        }
        self.notify_sidebar(cx);
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
        let provider = instance.and_then(|id| self.server_info.provider(id));
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
        let machine = match self.server_info.machine_kind() {
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
                        self.server_info.environment_label.clone().map(|label| line(muted_icon(machine), label.into())),
                    )
                    .children(thread.branch.clone().map(|branch| line(muted_icon(IconName::GitBranch), branch.into())))
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
            let scoped: Vec<&ProjectShell> =
                self.shell.projects.iter().filter(|p| self.sidebar.scope.contains(&p.id)).collect();
            match scoped.as_slice() {
                [] => "No threads yet".into(),
                [one] => format!("No threads in {} yet", project_name(one)).into(),
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
                            this.open_palette(super::palette::View::AddProject, window, cx)
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
        } else {
            let Some(key) = key else { return };
            match (event.keystroke.key.as_str(), shelf_of(&key)) {
                ("enter" | "space", Some(shelf)) => self.toggle_shelf(shelf, cx),
                ("right", Some(shelf)) if !expanded(self, shelf) => self.toggle_shelf(shelf, cx),
                ("left", Some(shelf)) if expanded(self, shelf) => self.toggle_shelf(shelf, cx),
                ("enter" | "space", None) if key.as_ref() == "settled:more" => {
                    self.sidebar.settled_visible += SETTLED_PAGE
                }
                ("enter" | "space", None) => {
                    if let Some(id) = key.strip_prefix("thread:") {
                        self.select_thread(id.to_owned(), window, cx);
                    }
                }
                ("f2", None) => {
                    if let Some(id) = key.strip_prefix("thread:") {
                        self.start_rename(id, window, cx);
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
    fn new_thread_project(&self) -> Option<String> {
        let current = match &self.target {
            Some(Target::Thread(id)) => self.shell.threads.iter().find(|t| &t.id == id).map(|t| t.project_id.clone()),
            Some(Target::Draft { project_id }) => Some(project_id.clone()),
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
        let connected = matches!(self.connection, Connection::Connected(_) | Connection::Connecting);
        let items = self.sidebar.items(&self.shell, self.target.as_ref(), connected && self.shell.live, now);
        let clock = self.sidebar.next_clock_change(&items, &self.shell.threads, now);
        let projects: HashMap<&str, &ProjectShell> = self.shell.projects.iter().map(|p| (p.id.as_str(), p)).collect();
        let route = self.route_thread();
        let inbox_order: Vec<String> =
            items.iter().filter_map(|item| matches!(item, Item::Card(_)).then(|| item.thread_id()).flatten()).collect();
        let nav_rows: Vec<SharedString> = items.iter().filter_map(Item::nav_key).collect();
        let list_focused = self.sidebar.list_focus.is_focused(window);
        let cursor = self.sidebar.cursor.clone().filter(|_| list_focused);
        let has_rows = items.iter().any(|item| matches!(item, Item::Card(_) | Item::Slim(..) | Item::Draft(_)));

        // Under a fixed header the web drops the list group's top padding.
        let mut list = v_flex().min_h_full().px(px(8.)).pb(px(8.)).gap(px(1.));
        if !query.is_empty() {
            list = list.child(self.render_search_results(&query, now, &tokens, cx));
        } else if !has_rows && self.sidebar.settled_count_is_zero(&items) {
            list = list.child(self.render_empty_state(&tokens, cx));
        } else {
            for item in items {
                let key = item.nav_key();
                let element = match item {
                    Item::Draft(project_id) => self.render_draft(projects.get(project_id).copied(), &tokens, cx),
                    Item::DraftDivider => {
                        div().mx(px(10.)).my(px(6.)).h(px(1.)).bg(sb.border.opacity(0.6)).into_any_element()
                    }
                    Item::Card(facts) => {
                        let active = route == Some(facts.thread.id.as_str());
                        self.render_card(
                            facts,
                            projects.get(facts.thread.project_id.as_str()).copied(),
                            active,
                            now,
                            &tokens,
                            cx,
                        )
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
                let ring = key.is_some() && key == cursor;
                list = list.child(if ring { focus_ring(element, &tokens) } else { element });
            }
        }
        self.sidebar.inbox_order = inbox_order;
        self.sidebar.nav_rows = nav_rows;
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
                    .group_hover("sidebar-rail", |s| s.visible())
                    .bg(sb.border),
            );

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
            .child(self.render_chrome_row(&tokens, cx))
            .child(self.render_search_row(!query.is_empty(), window, &tokens, cx))
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_focus(&self.sidebar.list_focus)
                    .on_key_down(
                        cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                            this.list_key(event, window, cx)
                        }),
                    )
                    .child(list),
            )
            .child(self.render_sidebar_footer(&tokens, cx))
            .child(deferred(rail).with_priority(1))
            .children(self.render_open_menu())
    }

    fn render_chrome_row(&self, tokens: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let sb = tokens.sidebar;
        let arrow = |id: &'static str, name: IconName, label: &'static str, enabled: bool| {
            div()
                .id(id)
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .when(!enabled, |el| el.opacity(0.64))
                .when(enabled, |el| el.cursor_pointer().hover(|s| s.bg(sb.row_hover)))
                // Inside the drag region a button must occlude, or Windows takes the press as a drag.
                .occlude()
                .tooltip(tooltip(label))
                .child(icon(name, 14., sb.muted_foreground))
        };
        let (back, forward) = (self.history.can_go_back(), self.history.can_go_forward());
        h_flex()
            .id("sidebar-chrome")
            .h(px(40.))
            .flex_none()
            .pr(px(12.))
            .window_control_area(WindowControlArea::Drag)
            .child(
                h_flex()
                    .ml(px(52.))
                    .h(px(28.))
                    .gap(px(4.))
                    .child(
                        svg()
                            .path("icons/app/t3-wordmark.svg")
                            .w(px(16.9))
                            .h(px(10.2))
                            .mt(px(1.))
                            .text_color(sb.foreground),
                    )
                    .child(
                        text_sm(div()).font_weight(FontWeight::MEDIUM).text_color(sb.muted_foreground).child("Code"),
                    ),
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
                        this.search_key(event, window, cx)
                    }))
                    .when(focused, |el| {
                        el.bg(sb.row_hover).border_1().border_color(tokens.ring.opacity(0.7)).px(px(7.))
                    })
                    .child(icon(IconName::Search, 16., sb.icon))
                    .child(div().flex_1().min_w_0().child(
                        Input::new(&self.sidebar.search).appearance(false).px_0().py_0().h(px(20.)).text_size(px(14.)),
                    ))
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
                                .child(icon(IconName::X, 12., tokens.muted_foreground))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.sidebar.search.update(cx, |search, cx| search.set_value("", window, cx));
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
            Connection::Connected(_) if self.shell.live => None,
            Connection::Connected(_) => Some(("Syncing…".into(), tokens.muted_foreground)),
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
            .server_info
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
                    this.update_providers(label.clone(), candidates.clone(), window, cx)
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
            .when(first, |el| el.mt_auto())
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

    fn render_draft(&self, project: Option<&ProjectShell>, tokens: &Tokens, _cx: &mut Context<Self>) -> AnyElement {
        let sb = tokens.sidebar;
        div()
            .py(px(2.))
            .child(
                div().rounded(px(6.)).bg(sb.row_active).child(
                    v_flex()
                        .h(px(78.))
                        .px(px(10.))
                        .py(px(8.))
                        .child(
                            h_flex()
                                .h(px(20.))
                                .gap(px(6.))
                                .child(icon(IconName::SquarePen, 12., tokens.warning_foreground))
                                .children(project.map(|p| project_icon(p, tokens, false, 16., self.favicon(p))))
                                .child(
                                    text_xs(div())
                                        .min_w_0()
                                        .flex_1()
                                        .truncate()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(sb.secondary_label)
                                        .children(project.map(|p| p.title.clone())),
                                ),
                        )
                        .child(
                            text_sm(div())
                                .mt(px(2.))
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(sb.foreground.opacity(0.9))
                                .child("New thread"),
                        ),
                ),
            )
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
        let driver = &self.server_info.provider(instance)?.driver;
        let (path, tint) = crate::assets::provider_icon(driver)?;
        let color = tint.unwrap_or(foreground);
        Some(svg().path(path).size(px(14.)).flex_none().text_color(color.opacity(0.6)).into_any_element())
    }

    fn render_card(
        &self,
        facts: Facts,
        project: Option<&ProjectShell>,
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
        let recede = inbox::recedes(status, unread, woke, active);
        let top = top_status(status, woke, unread, thread.goal.as_ref().is_some_and(|g| g.status == "active"), tokens);
        let duration_from = match status {
            Status::Working | Status::Preparing => facts.working_started_at(),
            Status::Waiting => inbox::parse_ms(facts.latest_run_completed_at().unwrap_or_default()),
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
                .into_any_element(),
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
                this.settle_thread(&thread_id, window, cx);
            }))
            .child(Icon::new(IconName::Check).size(px(14.)))
            .child("Settle");
        let status_slot =
            text_xs(h_flex()).relative().ml_auto().h(px(20.)).min_w(px(32.)).flex_none().justify_end().child(
                if hovered || menu_open {
                    h_flex().h_full().children(snooze).child(settle)
                } else {
                    h_flex().h_full().child(status_label)
                },
            );
        let provider = self.provider_mark(&facts, sb.foreground);
        let meta = thread.branch.as_ref().map(|branch| {
            text_xs(h_flex())
                .mt(px(2.))
                .gap(px(6.))
                .min_w_0()
                .text_color(sb.secondary_label)
                .when(thread.worktree_path.is_some(), |el| {
                    el.child(icon(IconName::FolderGit2, 14., sb.muted_foreground))
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis_middle()
                        .child(branch.clone()),
                )
                .children(provider)
        });
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
                    .map(|el| if active { el.bg(sb.row_active) } else { el.hover(|s| s.bg(sb.row_hover)) })
                    .when(recede && matches!(status, Status::Working | Status::Preparing), |el| {
                        el.opacity(0.7).hover(|s| s.bg(sb.row_hover).opacity(1.))
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.click_row(&open_id, event, window, cx)
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
                                            .children(project.map(|p| p.title.clone())),
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

    #[allow(clippy::too_many_arguments)]
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
        let recede = inbox::recedes(status, unread, woke, active);
        let settled = shelf == Some(Shelf::Settled);
        let title_color = if settled && !active {
            sb.secondary_label
        } else if recede {
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
                .child(inbox::format_wake(inbox::parse_ms(until).unwrap_or(now) - now))
                .into_any_element(),
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
        let action = action.map(|(action_shelf, name, size, label)| {
            let thread_id = thread.id.clone();
            h_flex()
                .id(SharedString::from(format!("{}-{}", action_shelf.label(), thread.id)))
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(-4.))
                .px(px(6.))
                .rounded(px(6.))
                .invisible()
                .group_hover(ROW_GROUP, |s| s.visible())
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
        let hovered = self.sidebar.hovered.as_deref() == Some(thread.id.as_str());
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
            .map(|el| if active { el.bg(sb.row_active) } else { el.hover(|s| s.bg(sb.row_hover)) })
            .on_click(
                cx.listener(move |this, event: &ClickEvent, window, cx| this.click_row(&open_id, event, window, cx)),
            )
            .children(project.map(|p| {
                div()
                    .flex_none()
                    .when(muted_icon, |el| el.opacity(0.4).group_hover(ROW_GROUP, |s| s.opacity(1.)))
                    .child(project_icon(p, tokens, muted_icon && !hovered, 16., self.favicon(p)))
            }))
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
            .child(
                text_xs(h_flex())
                    .relative()
                    .ml_auto()
                    .h(px(24.))
                    .min_w(px(32.))
                    .flex_none()
                    .justify_end()
                    .child(div().when(action.is_some(), |el| el.group_hover(ROW_GROUP, |s| s.invisible())).child(label))
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

fn guest_foreground(tokens: &Tokens) -> Hsla {
    rgb(if tokens.dark { 0xf4a8ff } else { 0xc800de }).into()
}

/// A project's repo favicon, fetched once per workspace root and favicon path.
pub(super) enum Favicon {
    Loading,
    Missing,
    Ready(Arc<Image>),
}

fn favicon_key(project: &ProjectShell) -> String {
    format!("{}\0{}", project.workspace_root, project.favicon_path.as_deref().unwrap_or_default())
}

impl Workspace {
    pub(super) fn favicon(&self, project: &ProjectShell) -> Option<Arc<Image>> {
        match self.sidebar.favicons.get(&favicon_key(project)) {
            Some(Favicon::Ready(image)) => Some(image.clone()),
            _ => None,
        }
    }

    /// Starts fetching the favicons not asked for yet: `assets.createUrl` with a
    /// `project-favicon` resource signs a URL, then the image is read over HTTP
    /// (`createProjectFaviconUrlAtomFamily` in `client-runtime/src/state/assets.ts`).
    fn load_favicons(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(base)) = (self.client(), self.http_base.clone()) else { return };
        for project in &self.shell.projects {
            let key = favicon_key(project);
            if self.sidebar.favicons.contains_key(&key) {
                continue;
            }
            self.sidebar.favicons.insert(key.clone(), Favicon::Loading);
            let mut resource = json!({"_tag": "project-favicon", "cwd": project.workspace_root});
            if let Some(path) = &project.favicon_path {
                resource["path"] = path.as_str().into();
            }
            let (client, base) = (client.clone(), base.clone());
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
                    this.sidebar.favicons.insert(key, image.map_or(Favicon::Missing, Favicon::Ready));
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
pub(super) fn project_icon(
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
    let text = custom
        .and_then(|icon| icon["text"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| inbox::monogram(&project.title));
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
        .font_family(FONT_MONO)
        .text_size(px(9. * scale))
        .line_height(px(size))
        .font_weight(FontWeight::BOLD)
        .child(
            h_flex()
                .w(px(if glyphs.len() > 1 { 12. } else { 6. } * scale))
                .justify_between()
                .when(glyphs.len() < 2, |el| el.justify_center())
                .children(glyphs.into_iter().map(|glyph| SharedString::from(glyph.to_string()))),
        )
        .into_any_element()
}
