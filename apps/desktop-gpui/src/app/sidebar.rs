//! The left sidebar (`apps/web/src/components/Sidebar.tsx` and
//! `components/sidebar/*`): chrome row, search, the inbox list, and the footer.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, DragMoveEvent, EmptyView, Entity, Focusable as _,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, deferred, div,
    prelude::FluentBuilder as _, px, rgb, svg,
};
use serde_json::json;

use super::{Connection, Target, Workspace, new_id};
use crate::inbox::{self, Facts, Status};
use crate::model::ProjectShell;
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
    search: Entity<InputState>,
    /// The one-second repaint, alive only while a row shows a running duration.
    ticking: bool,
    _tick: Option<Task<()>>,
    _search_subscription: Subscription,
}

impl SidebarState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subscription = cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.sync_sidebar_tick(cx);
                cx.notify();
            }
        });
        Self {
            width: px(DEFAULT_WIDTH),
            working_expanded: false,
            snoozed_expanded: false,
            settled_expanded: false,
            settled_visible: SETTLED_INITIAL,
            search,
            ticking: false,
            _tick: None,
            _search_subscription: subscription,
        }
    }
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

fn now_ms() -> i64 {
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

    /// The rows to draw, in order: the web's `sidebarListItemsFresh`.
    fn sidebar_items(&self, now: i64, query: &str) -> Vec<Item<'_>> {
        let route = self.route_thread();
        let connected = matches!(self.connection, Connection::Connected(_) | Connection::Connecting);
        let titles: HashMap<&str, &str> =
            self.shell.projects.iter().map(|p| (p.id.as_str(), p.title.as_str())).collect();
        let matches = |t: &crate::model::ThreadShell| {
            query.is_empty()
                || t.title.to_lowercase().contains(query)
                || titles.get(t.project_id.as_str()).is_some_and(|p| p.to_lowercase().contains(query))
        };
        let sections = inbox::partition(&self.shell.threads, now, connected && self.shell.live, matches);
        if !query.is_empty() {
            let all = [sections.pinned, sections.active, sections.working, sections.snoozed, sections.settled];
            return all.into_iter().flatten().map(|f| Item::Slim(f, None)).collect();
        }
        let mut items = Vec::new();
        if let Some(Target::Draft { project_id }) = &self.target {
            items.push(Item::Draft(project_id));
            items.push(Item::DraftDivider);
        }
        items.extend(sections.pinned.into_iter().chain(sections.active).map(Item::Card));
        fn route_only<'a>(list: Vec<Facts<'a>>, expanded: bool, route: Option<&str>) -> Vec<Facts<'a>> {
            if expanded { list } else { list.into_iter().filter(|f| Some(f.thread.id.as_str()) == route).collect() }
        }
        let mut first = true;
        if !sections.working.is_empty() {
            let (count, expanded) = (sections.working.len(), self.sidebar.working_expanded);
            items.push(Item::Header { shelf: Shelf::Working, count, expanded, first });
            first = false;
            items.extend(route_only(sections.working, expanded, route).into_iter().map(Item::Card));
        }
        if !sections.snoozed.is_empty() {
            let (count, expanded) = (sections.snoozed.len(), self.sidebar.snoozed_expanded);
            items.push(Item::Header { shelf: Shelf::Snoozed, count, expanded, first });
            first = false;
            items.extend(
                route_only(sections.snoozed, expanded, route).into_iter().map(|f| Item::Slim(f, Some(Shelf::Snoozed))),
            );
        }
        let expanded = self.sidebar.settled_expanded;
        items.push(Item::Header { shelf: Shelf::Settled, count: sections.settled.len(), expanded, first });
        let total = sections.settled.len();
        let mut settled: Vec<Facts<'_>> = sections.settled;
        let hidden = total.saturating_sub(self.sidebar.settled_visible);
        if hidden > 0 {
            let route_row =
                settled[self.sidebar.settled_visible..].iter().position(|f| Some(f.thread.id.as_str()) == route);
            let route_row = route_row.map(|i| settled[self.sidebar.settled_visible + i]);
            settled.truncate(self.sidebar.settled_visible);
            settled.extend(route_row);
        }
        items.extend(route_only(settled, expanded, route).into_iter().map(|f| Item::Slim(f, Some(Shelf::Settled))));
        if expanded && hidden > 0 {
            items.push(Item::ShowMore(hidden.min(SETTLED_PAGE)));
        }
        items
    }

    fn shows_running_duration(&self, cx: &App) -> bool {
        let now = now_ms();
        self.sidebar_items(now, &self.search_query(cx)).iter().any(|item| {
            matches!(item, Item::Card(f) if matches!(f.status(), Status::Working | Status::Preparing | Status::Waiting))
        })
    }

    /// Starts the one-second repaint when a visible row shows a running duration.
    pub(super) fn sync_sidebar_tick(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.ticking || !self.shows_running_duration(cx) {
            return;
        }
        self.sidebar.ticking = true;
        self.sidebar._tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep = this.update(cx, |this, cx| {
                    let keep = this.shows_running_duration(cx);
                    this.sidebar.ticking = keep;
                    if keep {
                        cx.notify();
                    }
                    keep
                });
                if !matches!(keep, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn toggle_shelf(&mut self, shelf: Shelf, cx: &mut Context<Self>) {
        let expanded = match shelf {
            Shelf::Working => &mut self.sidebar.working_expanded,
            Shelf::Snoozed => &mut self.sidebar.snoozed_expanded,
            Shelf::Settled => &mut self.sidebar.settled_expanded,
        };
        *expanded = !*expanded;
        self.sync_sidebar_tick(cx);
        cx.notify();
    }

    fn open_from_sidebar(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.select_thread(thread_id, window, cx);
        self.sync_sidebar_tick(cx);
    }

    fn dispatch_thread_command(&mut self, kind: &str, thread_id: &str, cx: &mut Context<Self>) {
        let mut command = json!({"type": kind, "commandId": new_id(), "threadId": thread_id});
        if kind != "thread.settle" {
            command["reason"] = "user".into();
        }
        self.dispatch(command, cx);
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

    pub(super) fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *crate::theme::tokens(cx);
        let sb = tokens.sidebar;
        let now = now_ms();
        let query = self.search_query(cx);
        let items = self.sidebar_items(now, &query);
        let projects: HashMap<&str, &ProjectShell> = self.shell.projects.iter().map(|p| (p.id.as_str(), p)).collect();
        let route = self.route_thread();

        // Under a fixed header the web drops the list group's top padding.
        let mut list = v_flex().min_h_full().px(px(8.)).pb(px(8.)).gap(px(1.));
        if items.is_empty() || (query.is_empty() && self.shell.threads.is_empty()) {
            let label = if !query.is_empty() {
                "No threads found"
            } else if self.shell.projects.is_empty() {
                "No projects yet"
            } else {
                "No threads yet"
            };
            list = list.child(
                text_xs(div().px(px(8.)).py(px(24.)).text_center().text_color(tokens.muted_foreground.opacity(0.6)))
                    .child(label),
            );
        } else {
            for item in items {
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
                list = list.child(element);
            }
        }

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
            .child(self.render_chrome_row(&tokens))
            .child(self.render_search_row(window, &tokens, cx))
            .child(div().id("sidebar-scroll").flex_1().min_h_0().overflow_y_scroll().child(list))
            .child(self.render_footer(&tokens))
            .child(deferred(rail).with_priority(1))
    }

    fn render_chrome_row(&self, tokens: &Tokens) -> impl IntoElement {
        let sb = tokens.sidebar;
        let arrow = |id: &'static str, name: IconName, label: &'static str| {
            div()
                .id(id)
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .opacity(0.64)
                .tooltip(tooltip(label))
                .child(icon(name, 14., sb.muted_foreground))
        };
        h_flex()
            .h(px(40.))
            .flex_none()
            .pr(px(12.))
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
            .child(h_flex().ml(px(4.)).child(arrow("sidebar-back", IconName::ArrowLeft, "Go back")).child(arrow(
                "sidebar-forward",
                IconName::ArrowRight,
                "Go forward",
            )))
    }

    fn render_search_row(&self, window: &mut Window, tokens: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let sb = tokens.sidebar;
        let focused = self.sidebar.search.focus_handle(cx).contains_focused(window, cx);
        let searching = !self.search_query(cx).is_empty();
        let new_thread = self.new_thread_project();
        let header_button = |id: &'static str, name: IconName, label: &'static str| {
            div()
                .id(id)
                .size(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .tooltip(tooltip(label))
                .child(icon(name, 16., sb.icon))
        };
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
            .child(
                h_flex()
                    .flex_none()
                    .when(!self.shell.projects.is_empty(), |el| {
                        el.child(header_button("sidebar-filter", IconName::ListFilter, "Filter threads by project"))
                            .child(header_button("sidebar-add-project", IconName::FolderPlus, "Add project"))
                    })
                    .child(header_button("sidebar-new-thread", IconName::SquarePen, "New thread (Ctrl+N)").when_some(
                        new_thread,
                        |el, project_id| {
                            el.cursor_pointer().hover(|s| s.bg(sb.row_hover)).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.start_draft(project_id.clone(), window, cx)
                                }),
                            )
                        },
                    )),
            )
    }

    fn render_footer(&self, tokens: &Tokens) -> impl IntoElement {
        let sb = tokens.sidebar;
        let button = |id: &'static str, name: IconName, label: &'static str| {
            div()
                .id(id)
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
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
            .child(button("sidebar-settings", IconName::Settings, "Settings"))
            .child(button("sidebar-pull-requests", IconName::GitPullRequestArrow, "Pull requests"))
            .child(button("sidebar-usage", IconName::ChartNoAxesColumn, "Usage"))
            .children(status.map(|(label, color)| {
                text_xs(h_flex())
                    .ml_auto()
                    .min_w_0()
                    .gap(px(6.))
                    .child(div().size(px(6.)).flex_none().rounded_full().bg(color))
                    .child(div().min_w_0().truncate().text_color(sb.muted_foreground).child(label))
            }))
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
                cx.notify();
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
                                .children(project.map(|p| project_icon(p, tokens, false)))
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
        let thread_id = thread.id.clone();
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
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.dispatch_thread_command("thread.settle", &thread_id, cx);
            }))
            .child(Icon::new(IconName::Check).size(px(14.)))
            .child("Settle");
        let status_slot = text_xs(h_flex())
            .relative()
            .ml_auto()
            .h(px(20.))
            .min_w(px(32.))
            .flex_none()
            .justify_end()
            .child(h_flex().h_full().group_hover(ROW_GROUP, |s| s.absolute().right_0().invisible()).child(status_label))
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .invisible()
                    .group_hover(ROW_GROUP, |s| s.relative().visible())
                    .child(settle),
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
        let has_meta = meta.is_some();
        div()
            .py(px(2.))
            .child(
                div()
                    .id(SharedString::from(format!("thread-{}", thread.id)))
                    .group(ROW_GROUP)
                    .rounded(px(6.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .map(|el| if active { el.bg(sb.row_active) } else { el.hover(|s| s.bg(sb.row_hover)) })
                    .when(recede && matches!(status, Status::Working | Status::Preparing), |el| {
                        el.opacity(0.7).hover(|s| s.bg(sb.row_hover).opacity(1.))
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_from_sidebar(open_id.clone(), window, cx)),
                    )
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
                                    .children(project.map(|p| project_icon(p, tokens, false)))
                                    .when(thread.pinned_at.is_some(), |el| {
                                        el.child(icon(IconName::Pin, 12., tokens.muted_foreground.opacity(0.65)))
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
                                    .children(title_icons),
                            )
                            .children(meta),
                    ),
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
            Some(Shelf::Settled) => Some(("thread.unsettle", IconName::Undo2, 14., "Un-settle thread")),
            Some(Shelf::Snoozed) => Some(("thread.unsnooze", IconName::AlarmClockOff, 12., "Wake thread now")),
            _ => None,
        };
        let action = action.map(|(command, name, size, label)| {
            let thread_id = thread.id.clone();
            h_flex()
                .id(SharedString::from(format!("{command}-{}", thread.id)))
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
                    this.dispatch_thread_command(command, &thread_id, cx);
                }))
                .child(Icon::new(name).size(px(size)))
        });
        let muted_icon = !active || settled;
        let open_id = thread.id.clone();
        h_flex()
            .id(SharedString::from(format!("thread-{}", thread.id)))
            .group(ROW_GROUP)
            .h(px(36.))
            .flex_none()
            .px(px(10.))
            .gap(px(10.))
            .rounded(px(6.))
            .cursor_pointer()
            .map(|el| if active { el.bg(sb.row_active) } else { el.hover(|s| s.bg(sb.row_hover)) })
            .on_click(cx.listener(move |this, _, window, cx| this.open_from_sidebar(open_id.clone(), window, cx)))
            .children(project.map(|p| {
                div()
                    .flex_none()
                    .when(muted_icon, |el| el.opacity(0.4).group_hover(ROW_GROUP, |s| s.opacity(1.)))
                    .child(project_icon(p, tokens, muted_icon))
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

/// `ProjectFavicon` without the repo favicon: a custom monogram, else one
/// derived from the title (`ProjectMonogram.tsx`).
/// `grayscale` shows it in gray until the row is hovered (`grayscale` in CSS).
fn project_icon(project: &ProjectShell, tokens: &Tokens, grayscale: bool) -> AnyElement {
    let custom = project.project_icon.as_ref().filter(|icon| icon["kind"] == "monogram");
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
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(shown.opacity(0.14))
        .text_color(shown)
        .when(grayscale, |el| el.group_hover(ROW_GROUP, |s| s.bg(color.opacity(0.14)).text_color(color)))
        .font_family(FONT_MONO)
        .text_size(px(9.))
        .line_height(px(16.))
        .font_weight(FontWeight::BOLD)
        .child(
            h_flex()
                .w(px(if glyphs.len() > 1 { 12. } else { 6. }))
                .justify_between()
                .when(glyphs.len() < 2, |el| el.justify_center())
                .children(glyphs.into_iter().map(|glyph| SharedString::from(glyph.to_string()))),
        )
        .into_any_element()
}
