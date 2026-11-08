//! The panel as a living surface: several pull requests as tabs (`RightPanelTabs mode="inline"`),
//! a width the user drags, the header condensing once its fold scrolls away, and reads that
//! follow the host (`useLiveRefresh` and `pullRequests.subscribeRefreshes`).

use gpui_kit::{DismissEvent, DragMoveEvent, Empty, Focusable as _, MouseButton, Pixels, Point, ScrollEvent, deferred};

use gpui_kit::component::button::ButtonCustomVariant;

use super::*;
use crate::app::right_panel::surface_item;
use crate::app::titlebar::{self, CAPTION_BUTTONS_WIDTH, RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT};
use crate::rpc::StreamEvent;

/// `LIVE_REFRESH_INTERVAL_MS`: an open pull request is read again this often while the app is in front.
const LIVE_REFRESH: Duration = Duration::from_mins(5);
/// A refresh stream the server ended is asked for again after this long, never in a hot loop.
const RESUBSCRIBE_AFTER: Duration = Duration::from_secs(60);
const MIN_WIDTH: f32 = 360.;
/// The list keeps at least this much beside the panel.
const LIST_MIN_WIDTH: f32 = 320.;
/// Scrolled past the header's fold by this much, the header condenses (`foldHeight + 32`).
const CONDENSE_AT: f32 = 140.;

/// Drag payload for the panel's left-edge resize handle.
struct PanelResize;

/// Drag payload for a tab moved along the strip (`SortableTabList`).
struct TabDrag(Reference);

/// The tab menu's bulk closes (`closeOtherSurfaces`, `closeSurfacesToRight`, `closeAllSurfaces`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloseTabs {
    Others,
    ToTheRight,
    All,
}

/// The tabs `which` closes, counted from the tab at `index`.
fn tabs_to_close<T: Clone>(tabs: &[T], index: usize, which: CloseTabs) -> Vec<T> {
    match which {
        CloseTabs::Others => tabs.iter().enumerate().filter(|(i, _)| *i != index).map(|(_, t)| t.clone()).collect(),
        CloseTabs::ToTheRight => tabs.iter().skip(index + 1).cloned().collect(),
        CloseTabs::All => tabs.to_vec(),
    }
}

/// `moveSurface`: the tab at `from` lands at `to`, the others keeping their order.
fn move_tab<T>(tabs: &mut Vec<T>, from: usize, to: usize) {
    if from < tabs.len() && to < tabs.len() && from != to {
        let tab = tabs.remove(from);
        tabs.insert(to, tab);
    }
}

/// Whether the page shows its panel (`rightPanelState.isOpen`) and lets it take the list's room
/// (`rightPanelMaximized`). Hiding the panel keeps every tab, as the web's store keeps its surfaces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::app) struct PanelLayout {
    hidden: bool,
    maximized: bool,
}

impl PanelLayout {
    fn shown(self, has_panel: bool) -> bool {
        has_panel && !self.hidden
    }

    /// Only a shown panel counts as maximized.
    fn maximized(self, has_panel: bool) -> bool {
        self.shown(has_panel) && self.maximized
    }

    /// `toggleRightPanel`; refused while no pull request is open to show.
    fn toggle(&mut self, has_panel: bool) -> bool {
        if self.shown(has_panel) {
            *self = Self { hidden: true, maximized: false };
        } else if has_panel {
            self.hidden = false;
        } else {
            return false;
        }
        true
    }

    /// `toggleRightPanelMaximized`; refused while the panel is hidden.
    fn toggle_maximized(&mut self, has_panel: bool) -> bool {
        if !self.shown(has_panel) {
            return false;
        }
        self.maximized = !self.maximized;
        true
    }

    /// Opening a pull request shows the panel; returns whether it was hidden.
    pub(super) fn reveal(&mut self) -> bool {
        std::mem::take(&mut self.hidden)
    }
}

/// The "+" of the page's panel (`RightPanelTabs`'s add menu): the page offers no surface to add,
/// so every row shows unavailable.
fn render_add_surface_button(t: &Tokens, cx: &mut Context<Workspace>) -> impl IntoElement {
    Button::new("pr-panel-add")
        .custom(ButtonCustomVariant::new(cx).foreground(t.muted_foreground).hover(t.accent).active(t.accent))
        .xsmall()
        .icon(titlebar::icon("icons/plus.svg"))
        .flex_none()
        .size(px(24.))
        .rounded(px(8.))
        .dropdown_menu(|menu: PopupMenu, _, _| {
            let rows = [
                (Icon::new(Lucide::Globe), "Browser", "B"),
                (titlebar::icon("icons/square-terminal.svg"), "Terminal", "T"),
                (Icon::new(Lucide::Files), "Files", "F"),
                (titlebar::icon("icons/file-diff.svg"), "Diff", "D"),
                (Icon::new(Lucide::GitPullRequest), "Pull request", "P"),
                (Icon::new(Lucide::Link), "Linked pull requests", "L"),
                (Icon::new(Lucide::Smartphone), "Device", "M"),
            ];
            rows.into_iter().fold(menu.min_w(px(200.)), |menu, (icon, label, key)| {
                menu.item(surface_item(icon, label, key).disabled(true))
            })
        })
}

/// `clamp(width)` between the panel's minimum and what leaves the list its own.
pub(super) fn clamp_panel_width(width: f32, available: f32) -> f32 {
    width.min(available - LIST_MIN_WIDTH).max(MIN_WIDTH)
}

impl Workspace {
    /// The page shows the panel beside (or over) the list.
    pub(in crate::app) fn pull_request_panel_shown(&self) -> bool {
        self.pull_requests.layout.shown(self.pull_requests.panel.is_some())
    }

    /// A pull request is open on the page, shown or hidden (`selectedPullRequestSurface`).
    pub(in crate::app) fn pull_request_panel_open(&self) -> bool {
        self.pull_requests.panel.is_some()
    }

    pub(in crate::app) fn pull_request_panel_maximized(&self) -> bool {
        self.pull_requests.layout.maximized(self.pull_requests.panel.is_some())
    }

    /// The page's "Toggle right panel" (`rightPanel.toggle`): hides the panel with its tabs kept,
    /// or shows it again. Returns whether there was a panel to toggle.
    pub(in crate::app) fn toggle_pull_request_panel(&mut self, cx: &mut Context<Self>) -> bool {
        let toggled = self.pull_requests.layout.toggle(self.pull_requests.panel.is_some());
        cx.notify();
        toggled
    }

    /// The page's maximize toggle (`rightPanel.toggleMaximized`).
    pub(in crate::app) fn toggle_pull_request_panel_maximized(&mut self, cx: &mut Context<Self>) -> bool {
        let toggled = self.pull_requests.layout.toggle_maximized(self.pull_requests.panel.is_some());
        cx.notify();
        toggled
    }

    /// Opens a pull request as a tab, or shows its tab; the shown one's state is parked, not lost.
    pub(super) fn show_pr_tab(
        &mut self,
        reference: Reference,
        seed: Option<Rc<Entry>>,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = reference.key();
        if !self.pull_requests.tabs.contains(&reference) {
            self.pull_requests.tabs.push(reference.clone());
        }
        if let Some(current) = self.pull_requests.panel.take() {
            self.pull_requests.parked.insert(current.reference.key(), current);
        }
        match self.pull_requests.parked.remove(&key) {
            Some(panel) => {
                self.pull_requests.panel = Some(panel);
                self.rebuild_pr_timeline();
                self.read_pr_avatars(&key, cx);
                cx.notify();
                true
            }
            None => {
                self.pull_requests.panel = Some(Panel::new(reference, seed));
                false
            }
        }
    }

    /// Closes one tab; the next one to its right, else to its left, shows.
    pub(super) fn close_pr_tab(&mut self, reference: &Reference, cx: &mut Context<Self>) {
        let Some(index) = self.pull_requests.tabs.iter().position(|r| r == reference) else { return };
        self.pull_requests.tabs.remove(index);
        self.pull_requests.parked.remove(&reference.key());
        if self.pull_requests.panel.as_ref().is_some_and(|p| &p.reference == reference) {
            self.pull_requests.panel = None;
            let next = self.pull_requests.tabs.get(index).or_else(|| self.pull_requests.tabs.last()).cloned();
            if let Some(next) = next {
                self.show_pr_tab(next, None, cx);
            }
        }
        // The last tab closed closes the panel; the next one opens unmaximized.
        if self.pull_requests.panel.is_none() {
            self.pull_requests.layout = PanelLayout::default();
        }
        cx.notify();
    }

    /// The Pull requests page's panel shortcuts (`PullRequestsPage`'s keydown listener). Returns
    /// whether the key was claimed; a held key is claimed without acting.
    pub(in crate::app) fn pull_requests_page_command(
        &mut self,
        command: &str,
        held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let shown = self.pull_request_panel_shown();
        let panel = self.pull_requests.panel.as_ref();
        let has_url = panel.and_then(Panel::url).is_some();
        let Some(shortcut) = page_panel_shortcut(command, panel.is_some(), shown, has_url) else { return false };
        if held {
            return true;
        }
        match shortcut {
            PageShortcut::Show | PageShortcut::Hide => {
                self.pull_requests.layout.toggle(true);
            }
            PageShortcut::ToggleMaximized => {
                self.pull_requests.layout.toggle_maximized(true);
            }
            PageShortcut::CloseTab => {
                if let Some(reference) = panel.map(|p| p.reference.clone()) {
                    self.close_pr_tab(&reference, cx);
                }
            }
            PageShortcut::CopyLink => {
                if let Some(panel) = panel {
                    let url = panel.url().unwrap_or_default().to_owned();
                    let short = logic::terminology(panel.provider()).1;
                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                    window.push_notification(Notification::success(url).title(format!("{short} link copied")), cx);
                }
            }
        }
        cx.notify();
        true
    }

    /// `pullRequest.copyNumber` while a pull request's detail is on screen, on the Pull requests
    /// page or in the thread's panel (`PullRequestDetailPanel`'s `shortcutsEnabled`).
    pub(in crate::app) fn copy_pull_request_number(
        &mut self,
        held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return false };
        let on_page = self.page == Some(super::super::Page::PullRequests) && self.pull_request_panel_shown();
        let in_thread_panel = matches!(
            self.active_surface(),
            Some(super::super::super::right_panel::Surface::PullRequest(reference)) if *reference == panel.reference
        );
        if !on_page && !in_thread_panel {
            return false;
        }
        if !held {
            let short = logic::terminology(panel.provider()).1;
            cx.write_to_clipboard(ClipboardItem::new_string(format!("#{}", panel.reference.number)));
            toast_success(window, cx, format!("{short} number copied"));
        }
        true
    }

    /// Closes several tabs at once; when the shown one goes, `keep` shows instead.
    fn close_pr_tabs(&mut self, close: &[Reference], keep: Option<Reference>, cx: &mut Context<Self>) {
        self.pull_requests.tabs.retain(|tab| !close.contains(tab));
        for reference in close {
            self.pull_requests.parked.remove(&reference.key());
        }
        if self.pull_requests.panel.as_ref().is_some_and(|p| close.contains(&p.reference)) {
            self.pull_requests.panel = None;
            if let Some(keep) = keep.filter(|keep| self.pull_requests.tabs.contains(keep)) {
                self.show_pr_tab(keep, None, cx);
            }
        }
        if self.pull_requests.panel.is_none() {
            self.pull_requests.layout = PanelLayout::default();
        }
        cx.notify();
    }

    /// The tab menu's Close others, Close to the right and Close all.
    fn close_pr_tabs_from(&mut self, reference: &Reference, which: CloseTabs, cx: &mut Context<Self>) {
        let Some(index) = self.pull_requests.tabs.iter().position(|r| r == reference) else { return };
        let close = tabs_to_close(&self.pull_requests.tabs, index, which);
        let keep = (which != CloseTabs::All).then(|| reference.clone());
        self.close_pr_tabs(&close, keep, cx);
    }

    /// A tab dropped on another takes its place.
    fn move_pr_tab(&mut self, dragged: &Reference, onto: &Reference, cx: &mut Context<Self>) {
        let tabs = &mut self.pull_requests.tabs;
        let (Some(from), Some(to)) = (tabs.iter().position(|r| r == dragged), tabs.iter().position(|r| r == onto))
        else {
            return;
        };
        move_tab(tabs, from, to);
        cx.notify();
    }

    /// The right-click menu on a tab (`handleTabContextMenu`).
    fn open_pr_tab_menu(
        &mut self,
        reference: Reference,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.pull_requests.tabs.iter().position(|r| *r == reference) else { return };
        let count = self.pull_requests.tabs.len();
        let view = cx.entity();
        let menu = PopupMenu::build(window, cx, move |menu, window, _| {
            let item = |label: &'static str, disabled: bool, which: Option<CloseTabs>| {
                let reference = reference.clone();
                PopupMenuItem::new(label).disabled(disabled).on_click(window.listener_for(
                    &view,
                    move |this: &mut Workspace, _, _, cx| match which {
                        Some(which) => this.close_pr_tabs_from(&reference, which, cx),
                        None => this.close_pr_tab(&reference, cx),
                    },
                ))
            };
            menu.min_w(px(180.))
                .item(item("Close", false, None))
                .item(item("Close others", count <= 1, Some(CloseTabs::Others)))
                .item(item("Close to the right", index + 1 >= count, Some(CloseTabs::ToTheRight)))
                .item(item("Close all", false, Some(CloseTabs::All)))
        });
        let previous = window.focused(cx);
        menu.focus_handle(cx).focus(window, cx);
        let dismiss = cx.subscribe_in(&menu, window, move |this, _, _: &DismissEvent, window, cx| {
            this.pull_requests.tab_menu = None;
            if let Some(previous) = &previous {
                previous.focus(window, cx);
            }
            cx.notify();
        });
        self.pull_requests.tab_menu = Some((menu, position, dismiss));
        cx.notify();
    }

    /// The panel is on screen: beside the Pull requests list, or as a thread's Pull request surface.
    fn pr_panel_on_screen(&self) -> bool {
        self.page == Some(super::super::Page::PullRequests)
            || matches!(self.active_surface(), Some(super::super::super::right_panel::Surface::PullRequest(_)))
    }

    /// Reads the shown pull request again in place, as a refresh from the host's side does;
    /// a refresh from one environment's host touches only a pull request on that environment.
    fn refresh_pr_in_place(&mut self, environment: Option<&str>, cx: &mut Context<Self>) {
        if !self.pr_panel_on_screen() {
            return;
        }
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        if panel.detail.is_none() || panel.detail_loading {
            return;
        }
        if let Some(environment) = environment
            && self.project_environment(&panel.reference.project_id).as_deref() != Some(environment)
        {
            return;
        }
        self.read_pr_detail(cx);
        self.refresh_pr_code(cx);
    }

    /// Starts following the hosts once the page or a thread's Pull request surface opens: turn
    /// refreshes as they happen, and a slow read of what is on screen while the app is in front.
    /// Neither repaints anything until an answer lands.
    pub(in crate::app) fn follow_pull_requests(&mut self, cx: &mut Context<Self>) {
        if self.pull_requests._live_interval.is_none() {
            self.pull_requests._live_interval = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(LIVE_REFRESH).await;
                    let alive = this.update(cx, |this, cx| {
                        if cx.active_window().is_some() {
                            this.refresh_pr_in_place(None, cx);
                            if this.page == Some(super::super::Page::PullRequests) {
                                this.live_refresh_pull_requests(cx);
                            }
                        }
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            }));
        }
        self.sync_pull_request_refreshes(cx);
    }

    /// Once followed, keeps one refresh stream per connected environment: a new socket (the
    /// primary's or a saved environment's reconnect) restarts its environment's stream.
    pub(in crate::app) fn sync_pull_request_refreshes(&mut self, cx: &mut Context<Self>) {
        if self.pull_requests._live_interval.is_none() {
            return;
        }
        let connected: Vec<(SharedString, RpcClient)> =
            self.sessions().iter().filter_map(|s| Some((s.id.clone()?, s.client.clone()?))).collect();
        let start =
            crate::pull_requests::resync_follows(&mut self.pull_requests.refreshes, &connected, RpcClient::same_socket);
        for index in start {
            let (environment, client) = connected[index].clone();
            let task = self.follow_pr_refreshes(environment.clone(), client.clone(), cx);
            self.pull_requests.refreshes.insert(environment, (client, task));
        }
    }

    fn follow_pr_refreshes(
        &mut self,
        environment: SharedString,
        client: RpcClient,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        let stream = client.stream("pullRequests.subscribeRefreshes", json!({}));
        cx.spawn(async move |this, cx| {
            let mut first = true;
            while let Some(event) = stream.next().await {
                match event {
                    // The first value is the current count, not a refresh.
                    StreamEvent::Item(_) if first => first = false,
                    StreamEvent::Item(_) => {
                        let _ = this.update(cx, |this, cx| {
                            this.refresh_pr_in_place(Some(&environment), cx);
                            this.refresh_pull_requests_after_turn(cx);
                        });
                    }
                    // A dropped socket restarts when its environment reconnects (`start_session`);
                    // anything else the server ended is asked for again after a pause.
                    StreamEvent::End(result) => {
                        let disconnected = matches!(result, Err(crate::rpc::RpcError::Disconnected(_)));
                        if !disconnected {
                            log::warn!("pull request refreshes ended: {result:?}");
                            cx.background_executor().timer(RESUBSCRIBE_AFTER).await;
                        }
                        let _ = this.update(cx, |this, cx| {
                            let refreshes = &mut this.pull_requests.refreshes;
                            if refreshes.get(&environment).is_some_and(|(socket, _)| socket.same_socket(&client)) {
                                refreshes.remove(&environment);
                            }
                            if !disconnected {
                                this.sync_pull_request_refreshes(cx);
                            }
                        });
                        break;
                    }
                }
            }
        })
    }

    /// `usePullRequestChecksRefresh`: an open pull request's checks are read through
    /// `pullRequests.checks` every 45 seconds while any run, else every minute, while it is on
    /// screen, connected and the app is in front. A server without the read, or a host that
    /// answers none, ends the poll.
    pub(super) fn follow_pr_checks(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(panel) = self.pr_panel_ref(key) else { return };
        if panel._checks_poll.is_some()
            || !self.pull_request_info(&panel.reference.project_id).is_some_and(|i| i.capability("pullRequestChecks"))
        {
            return;
        }
        let key = key.to_owned();
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                loop {
                    let wait = this.update(cx, |this, _| {
                        let detail = this.pr_panel_ref(&key)?.detail.clone()?;
                        Some(logic::checks_refresh_interval(&detail.checks))
                    });
                    let Ok(Some(wait)) = wait else { break };
                    cx.background_executor().timer(wait).await;
                    let read = this.update(cx, |this, cx| {
                        let shown = this.shown_pr_key().as_deref() == Some(key.as_str()) && this.pr_panel_on_screen();
                        let panel = this.pr_panel_ref(&key)?;
                        let detail = panel.detail.as_ref()?;
                        let idle = detail.state == list_logic::State::Open && !panel.detail_loading;
                        // A disconnected environment skips its turn; the poll resumes on reconnect.
                        let client = this.pull_request_client_of(&key);
                        Some(
                            client
                                .filter(|_| shown && idle && cx.active_window().is_some())
                                .map(|client| (client, panel.reference.to_json())),
                        )
                    });
                    let Ok(Some(read)) = read else { break };
                    let Some((client, reference)) = read else { continue };
                    let answer = client.request("pullRequests.checks", reference).await;
                    let keep = this.update(cx, |this, cx| {
                        let Ok(value) = answer else { return true };
                        if value.is_null() {
                            return false;
                        }
                        let Ok(read) = serde_json::from_value::<logic::ChecksRead>(value) else { return true };
                        let Some(panel) = this.pr_panel(&key) else { return false };
                        // Never under a detail read in flight, so a checks answer always lands after the detail.
                        if let Some(merged) = panel.detail.as_deref().and_then(|d| logic::with_checks(d, &read, true)) {
                            panel.detail = Some(Rc::new(merged));
                            cx.notify();
                        }
                        panel.checks_read = Some(read);
                        true
                    });
                    if !matches!(keep, Ok(true)) {
                        break;
                    }
                }
            }
        });
        if let Some(panel) = self.pr_panel(&key) {
            panel._checks_poll = Some(task);
        }
    }

    /// Condenses the header once the shown tab has scrolled past its fold, and back near the top.
    fn track_pr_scroll(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_mut() else { return };
        let top = match panel.tab {
            Tab::Summary => -f32::from(panel.summary_scroll.offset().y),
            Tab::Timeline => {
                let top = panel.timeline.logical_scroll_top();
                if top.item_ix > 0 { CONDENSE_AT + 1. } else { f32::from(top.offset_in_item) }
            }
            Tab::Guide | Tab::Code => return,
        };
        let condensed = if panel.condensed { top >= 4. } else { top > CONDENSE_AT };
        if condensed != panel.condensed {
            panel.condensed = condensed;
            cx.notify();
        }
    }

    /// The tab body, watching its scroll for the header.
    pub(super) fn pr_scroll_watch(
        &self,
        body: AnyElement,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        div()
            .id("pr-body")
            .flex_1()
            .min_h_0()
            .relative()
            .on_scroll(cx.listener(|this, _: &ScrollEvent, _, cx| this.track_pr_scroll(cx)))
            .child(body)
    }

    /// The panel's width in this window.
    pub(in crate::app) fn pr_panel_width(&self, window: &Window) -> f32 {
        let viewport = f32::from(window.viewport_size().width);
        let sidebar = if self.sidebar_open.docked() { f32::from(self.sidebar_width()) } else { 0. };
        clamp_panel_width(self.pull_requests.panel_width.unwrap_or(viewport / 2.), viewport - sidebar)
    }

    /// The panel beside the list: its tabs, the shown pull request, and the resize handle.
    pub(in crate::app) fn render_pr_panel_column(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let viewport = f32::from(window.viewport_size().width);
        let sidebar = if self.sidebar_open.docked() { f32::from(self.sidebar_width()) } else { 0. };
        let available = viewport - sidebar;
        let width = clamp_panel_width(self.pull_requests.panel_width.unwrap_or(viewport / 2.), available);
        let maximized = self.pull_request_panel_maximized();
        let handle = div()
            .id("pr-panel-resize")
            .group("pr-panel-resize")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-4.))
            .w(px(8.))
            .cursor_col_resize()
            .on_drag(PanelResize, |_, _, _, cx| cx.new(|_| Empty))
            .on_click(cx.listener(|this, event: &gpui_kit::ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.pull_requests.panel_width = None;
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
                    .group_hover("pr-panel-resize", gpui_kit::Styled::visible)
                    .bg(t.border),
            );
        v_flex()
            .id("pr-panel")
            .track_focus(&self.areas.panel)
            .relative()
            .map(|el| if maximized { el.flex_1().min_w_0() } else { el.flex_none().w(px(width)) })
            .h_full()
            .min_h_0()
            .border_l_1()
            .border_color(t.border)
            .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<PanelResize>, _, cx| {
                let right = f32::from(event.bounds.right());
                let width = right - f32::from(event.event.position.x);
                this.pull_requests.panel_width = Some(clamp_panel_width(width, available));
                cx.notify();
            }))
            .child(self.render_pr_tab_strip(&t, window, cx))
            .child(div().flex_1().min_h_0().child(self.render_pull_request_panel(None, window, cx)))
            .when(!maximized, |el| el.child(deferred(handle).with_priority(1)))
            .children(self.pull_requests.tab_menu.as_ref().map(|(menu, position, _)| {
                deferred(
                    gpui_kit::anchored().position(*position).snap_to_window_with_margin(px(8.)).child(menu.clone()),
                )
                .with_priority(3)
            }))
            .into_any_element()
    }

    /// The open pull requests, each a tab whose glyph turns into its close button on hover or
    /// keyboard focus.
    fn render_pr_tab_strip(&mut self, t: &Tokens, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let keys: HashSet<String> = self.pull_requests.tabs.iter().map(Reference::key).collect();
        let focus = &mut self.pull_requests.tab_focus;
        focus.retain(|key, _| keys.contains(key));
        for key in keys {
            focus.entry(key).or_insert_with(|| [cx.focus_handle().tab_stop(true), cx.focus_handle().tab_stop(true)]);
        }
        let keyboard = window.last_input_was_keyboard();
        // `focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring`
        let ring = |focus: &gpui_kit::FocusHandle| {
            (keyboard && focus.is_focused(window))
                .then(|| vec![gpui_kit::BoxShadow::new(px(0.), px(0.), t.ring).spread_radius(px(1.)).inset()])
        };
        let pressed = |event: &gpui_kit::KeyDownEvent| {
            matches!(event.keystroke.key.as_str(), "enter" | "space") && !event.keystroke.modifiers.modified()
        };
        let active = self.pull_requests.panel.as_ref().map(|p| p.reference.clone());
        let tabs: Vec<AnyElement> = self
            .pull_requests
            .tabs
            .iter()
            .map(|reference| {
                let key = reference.key();
                let shown = active.as_ref() == Some(reference);
                let panel = if shown { self.pull_requests.panel.as_ref() } else { self.pull_requests.parked.get(&key) };
                let detail = panel.and_then(|p| p.detail.clone());
                let seed = panel.and_then(|p| p.seed.clone()).or_else(|| self.pull_requests.entry_for(reference));
                let (state, draft) = detail
                    .as_ref()
                    .map(|d| (d.state, d.is_draft))
                    .or_else(|| seed.as_ref().map(|s| (s.state, s.is_draft)))
                    .unwrap_or((list_logic::State::Open, false));
                let (icon, color, _) = state_glyph(state, draft, t);
                // `surfaceTitle`, also the title's tooltip.
                let label: SharedString = format!("#{}", reference.number).into();
                let tip = label.clone();
                let group: SharedString = format!("pr-tab:{key}").into();
                let (close, middle, activate) = (reference.clone(), reference.clone(), reference.clone());
                let (menu, dragged, onto) = (reference.clone(), reference.clone(), reference.clone());
                let (close_key, activate_key) = (reference.clone(), reference.clone());
                let [close_focus, title_focus] = self.pull_requests.tab_focus[&key].clone();
                let close_visible = keyboard && close_focus.is_focused(window);
                let (accent, foreground, muted) = (t.accent, t.foreground, t.muted);
                h_flex()
                    .id(ElementId::Name(group.clone()))
                    .group(group.clone())
                    .flex_none()
                    .h(px(24.))
                    .max_w(px(200.))
                    .gap(px(2.))
                    .pl(px(6.))
                    .pr(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_xs()
                    .map(|el| {
                        if shown {
                            el.bg(accent).text_color(foreground)
                        } else {
                            el.text_color(t.muted_foreground)
                                .hover(move |s| s.bg(Hsla { a: accent.a * 0.6, ..accent }).text_color(foreground))
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
                            .hover(move |s| s.bg(muted))
                            .track_focus(&close_focus)
                            .when_some(ring(&close_focus), gpui_kit::Styled::shadow)
                            .on_key_down(cx.listener(move |this, event: &gpui_kit::KeyDownEvent, _, cx| {
                                if pressed(event) {
                                    cx.stop_propagation();
                                    this.close_pr_tab(&close_key, cx);
                                }
                            }))
                            .child(
                                div()
                                    .when(close_visible, gpui_kit::Styled::invisible)
                                    .group_hover(group.clone(), gpui_kit::Styled::invisible)
                                    .child(Icon::new(icon).with_size(px(12.)).text_color(color)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .when(!close_visible, gpui_kit::Styled::invisible)
                                    .group_hover(group, gpui_kit::Styled::visible)
                                    .child(Icon::new(Lucide::X).with_size(px(12.))),
                            )
                            .tooltip(|window, cx| Tooltip::new("Close").build(window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_pr_tab(&close, cx);
                            })),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(format!("pr-tab-title:{key}").into()))
                            .min_w_0()
                            .truncate()
                            .rounded(px(2.))
                            .track_focus(&title_focus)
                            .when_some(ring(&title_focus), gpui_kit::Styled::shadow)
                            .on_key_down(cx.listener(move |this, event: &gpui_kit::KeyDownEvent, _, cx| {
                                if pressed(event) {
                                    cx.stop_propagation();
                                    let seed = this.pull_requests.entry_for(&activate_key);
                                    this.open_pull_request_panel(activate_key.clone(), seed, cx);
                                }
                            }))
                            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                            .child(label),
                    )
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, _, cx| this.close_pr_tab(&middle, cx)),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_pr_tab_menu(menu.clone(), event.position, window, cx);
                        }),
                    )
                    .on_drag(TabDrag(dragged), |_, _, _, cx| cx.new(|_| Empty))
                    .drag_over::<TabDrag>(move |style, _, _, _| style.bg(Hsla { a: accent.a * 0.6, ..accent }))
                    .on_drop(cx.listener(move |this, drag: &TabDrag, _, cx| this.move_pr_tab(&drag.0, &onto, cx)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let seed = this.pull_requests.entry_for(&activate);
                        this.open_pull_request_panel(activate.clone(), seed, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        // `RightPanelTabs mode="inline"` owning the titlebar row: a drag region clear of the panel
        // toggles and the caption buttons, past a hidden sidebar's toggle while maximized.
        let reserve = CAPTION_BUTTONS_WIDTH + px(12.) - RIGHT_RESIZE_INSET + px(96.);
        let inset = (self.pull_request_panel_maximized() && !self.sidebar_open.docked())
            .then(|| titlebar::collapsed_content_left(window));
        let strip = h_flex()
            .id("pr-tabbar")
            .h(TITLEBAR_HEIGHT)
            .flex_none()
            .min_w_0()
            .pl(inset.unwrap_or(px(8.)))
            .pr(reserve)
            .gap_1();
        titlebar::drag_region(strip)
            .child(h_flex().id("pr-tabs").min_w_0().h_full().gap_1().overflow_x_scroll().children(tabs))
            .child(render_add_surface_button(t, cx))
            .into_any_element()
    }

    /// The condensed header row: author and time, branches, and the diff size (`condensedRowRef`).
    pub(super) fn render_pr_condensed_row(&self, detail: &Detail, stat: Option<AnyElement>, t: &Tokens) -> AnyElement {
        let now = now_ms();
        let updated = parse_ms(&detail.updated_at).map(|at| relative_time(now - at)).unwrap_or_default();
        h_flex()
            .min_w_0()
            .gap(px(8.))
            .px(px(16.))
            .pt(px(4.))
            .pb(px(8.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(h_flex().flex_none().gap(px(6.)).child(avatar(detail.author.as_ref(), 16., t)).child(updated))
            .child(div().h(px(12.)).w(px(1.)).flex_none().bg(Hsla { a: t.border.a * 0.7, ..t.border }))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .font_family(FONT_MONO)
                    .text_size(px(11.))
                    .text_color(Hsla { a: 0.65, ..t.muted_foreground })
                    .child(div().flex_none().child(detail.base_branch.clone()))
                    .child(Icon::new(Lucide::ArrowLeft).with_size(px(12.)))
                    .child(div().min_w_0().truncate().child(detail.head_branch.clone())),
            )
            .child(
                h_flex()
                    .ml_auto()
                    .flex_none()
                    .gap(px(8.))
                    .text_size(px(11.))
                    .child(format!(
                        "{} {}",
                        detail.changed_files,
                        if detail.changed_files == 1 { "file" } else { "files" }
                    ))
                    .children(stat),
            )
            .into_any_element()
    }
}

/// What a panel shortcut does on the Pull requests page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageShortcut {
    Show,
    Hide,
    ToggleMaximized,
    CloseTab,
    CopyLink,
}

/// The page's panel shortcut for `command`, given whether a pull request is open in the panel,
/// whether the panel shows, and whether its link is known; `None` leaves the key alone.
fn page_panel_shortcut(command: &str, open: bool, shown: bool, has_url: bool) -> Option<PageShortcut> {
    match command {
        "rightPanel.toggle" if shown => Some(PageShortcut::Hide),
        "rightPanel.toggle" if open => Some(PageShortcut::Show),
        "rightPanel.toggleMaximized" if shown => Some(PageShortcut::ToggleMaximized),
        "rightPanel.close" if shown => Some(PageShortcut::CloseTab),
        "thread.copyReference" if shown && has_url => Some(PageShortcut::CopyLink),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_panel_shortcuts_act_only_on_a_panel_they_can_reach() {
        use PageShortcut::*;
        let cases = [
            ("rightPanel.toggle", (true, true, true), Some(Hide)),
            ("rightPanel.toggle", (true, false, true), Some(Show)),
            ("rightPanel.toggle", (false, false, false), None),
            ("rightPanel.toggleMaximized", (true, true, true), Some(ToggleMaximized)),
            ("rightPanel.toggleMaximized", (true, false, true), None),
            ("rightPanel.close", (true, true, true), Some(CloseTab)),
            ("rightPanel.close", (true, false, true), None),
            ("thread.copyReference", (true, true, true), Some(CopyLink)),
            ("thread.copyReference", (true, true, false), None),
            ("thread.copyReference", (true, false, true), None),
            ("chat.new", (true, true, true), None),
        ];
        for (command, (open, shown, has_url), expected) in cases {
            assert_eq!(page_panel_shortcut(command, open, shown, has_url), expected, "{command} {open} {shown}");
        }
    }

    #[test]
    fn the_tab_menu_closes_the_tabs_it_names() {
        let tabs = ["a", "b", "c", "d"];
        assert_eq!(tabs_to_close(&tabs, 1, CloseTabs::Others), ["a", "c", "d"]);
        assert_eq!(tabs_to_close(&tabs, 1, CloseTabs::ToTheRight), ["c", "d"]);
        assert!(tabs_to_close(&tabs, 3, CloseTabs::ToTheRight).is_empty());
        assert_eq!(tabs_to_close(&tabs, 0, CloseTabs::All), tabs);
    }

    #[test]
    fn a_dragged_tab_takes_the_place_it_lands_on() {
        let mut tabs = vec!["a", "b", "c", "d"];
        move_tab(&mut tabs, 0, 2);
        assert_eq!(tabs, ["b", "c", "a", "d"]);
        move_tab(&mut tabs, 3, 0);
        assert_eq!(tabs, ["d", "b", "c", "a"]);
        move_tab(&mut tabs, 1, 9);
        assert_eq!(tabs, ["d", "b", "c", "a"], "a place past the end changes nothing");
    }

    #[test]
    fn hiding_the_panel_keeps_it_to_show_again_and_ends_maximizing() {
        let mut layout = PanelLayout::default();
        assert!(!layout.toggle(false), "nothing to show before a pull request opens");
        assert!(layout.toggle_maximized(true));
        assert!(layout.maximized(true));
        assert!(layout.toggle(true));
        assert!(!layout.shown(true), "hidden, with its pull requests still open");
        assert!(!layout.toggle_maximized(true), "a hidden panel does not maximize");
        assert!(layout.toggle(true));
        assert!(layout.shown(true));
        assert!(!layout.maximized(true), "shown again at its own width");
        layout.toggle(true);
        assert!(layout.reveal(), "opening a pull request shows the panel");
        assert!(layout.shown(true));
    }

    #[test]
    fn the_panel_keeps_its_minimum_and_leaves_the_list_its_own() {
        assert_eq!(clamp_panel_width(640., 1600.), 640.);
        assert_eq!(clamp_panel_width(1500., 1600.), 1280., "the list keeps 320px");
        assert_eq!(clamp_panel_width(100., 1600.), 360.);
        assert_eq!(clamp_panel_width(500., 600.), 360., "a narrow window keeps the panel usable");
    }
}
