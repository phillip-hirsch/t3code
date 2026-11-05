//! The panel as a living surface: several pull requests as tabs (`RightPanelTabs mode="inline"`),
//! a width the user drags, the header condensing once its fold scrolls away, and reads that
//! follow the host (`useLiveRefresh` and `pullRequests.subscribeRefreshes`).

use gpui_kit::{DragMoveEvent, Empty, MouseButton, ScrollEvent, deferred};

use super::*;
use crate::rpc::StreamEvent;

/// `LIVE_REFRESH_INTERVAL_MS`: an open pull request is read again this often while the app is in front.
const LIVE_REFRESH: Duration = Duration::from_secs(5 * 60);
/// A refresh stream the server ended is asked for again after this long, never in a hot loop.
const RESUBSCRIBE_AFTER: Duration = Duration::from_secs(60);
const MIN_WIDTH: f32 = 360.;
/// The list keeps at least this much beside the panel.
const LIST_MIN_WIDTH: f32 = 320.;
/// Scrolled past the header's fold by this much, the header condenses (`foldHeight + 32`).
const CONDENSE_AT: f32 = 140.;

/// Drag payload for the panel's left-edge resize handle.
struct PanelResize;

/// `clamp(width)` between the panel's minimum and what leaves the list its own.
pub(super) fn clamp_panel_width(width: f32, available: f32) -> f32 {
    width.min(available - LIST_MIN_WIDTH).max(MIN_WIDTH)
}

impl Workspace {
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
        cx.notify();
    }

    /// Reads the shown pull request again in place, as a refresh from the host's side does;
    /// a refresh from one environment's host touches only a pull request on that environment.
    fn refresh_pr_in_place(&mut self, environment: Option<&str>, cx: &mut Context<Self>) {
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

    /// Starts following the hosts once the page opens: turn refreshes as they happen, and a slow
    /// read while the app is in front. Neither repaints anything until an answer lands.
    pub(in crate::app) fn follow_pull_requests(&mut self, cx: &mut Context<Self>) {
        if self.pull_requests._live_interval.is_none() {
            self.pull_requests._live_interval = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(LIVE_REFRESH).await;
                    let alive = this.update(cx, |this, cx| {
                        if cx.active_window().is_some() && this.page == Some(super::super::Page::PullRequests) {
                            this.refresh_pr_in_place(None, cx);
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
                        let _ = this.update(cx, |this, cx| this.refresh_pr_in_place(Some(&environment), cx));
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
    pub(super) fn pr_panel_width(&self, window: &Window) -> f32 {
        let viewport = f32::from(window.viewport_size().width);
        let sidebar = if self.sidebar_open { f32::from(self.sidebar_width()) } else { 0. };
        clamp_panel_width(self.pull_requests.panel_width.unwrap_or(viewport / 2.), viewport - sidebar)
    }

    /// The panel beside the list: its tabs, the shown pull request, and the resize handle.
    pub(in crate::app) fn render_pr_panel_column(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let viewport = f32::from(window.viewport_size().width);
        let sidebar = if self.sidebar_open { f32::from(self.sidebar_width()) } else { 0. };
        let available = viewport - sidebar;
        let width = clamp_panel_width(self.pull_requests.panel_width.unwrap_or(viewport / 2.), available);
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
                    .group_hover("pr-panel-resize", |s| s.visible())
                    .bg(t.border),
            );
        v_flex()
            .id("pr-panel")
            .relative()
            .flex_none()
            .w(px(width))
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
            .child(self.render_pr_tab_strip(&t, cx))
            .child(div().flex_1().min_h_0().child(self.render_pull_request_panel(window, cx)))
            .child(deferred(handle).with_priority(1))
            .into_any_element()
    }

    /// The open pull requests, each a tab whose glyph turns into its close button on hover.
    fn render_pr_tab_strip(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
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
                let title = detail.as_ref().map(|d| d.title.clone()).or_else(|| seed.as_ref().map(|s| s.title.clone()));
                let (state, draft) = detail
                    .as_ref()
                    .map(|d| (d.state, d.is_draft))
                    .or_else(|| seed.as_ref().map(|s| (s.state, s.is_draft)))
                    .unwrap_or((list_logic::State::Open, false));
                let (icon, color, _) = state_glyph(state, draft, t);
                let label = match title {
                    Some(title) => format!("#{} {title}", reference.number),
                    None => format!("#{}", reference.number),
                };
                let group: SharedString = format!("pr-tab:{key}").into();
                let (close, middle, activate) = (reference.clone(), reference.clone(), reference.clone());
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
                            .child(
                                div()
                                    .group_hover(group.clone(), |s| s.invisible())
                                    .child(Icon::new(icon).with_size(px(12.)).text_color(color)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .invisible()
                                    .group_hover(group.clone(), |s| s.visible())
                                    .child(Icon::new(Lucide::X).with_size(px(12.))),
                            )
                            .tooltip(|window, cx| Tooltip::new("Close").build(window, cx))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_pr_tab(&close, cx);
                            })),
                    )
                    .child(div().min_w_0().truncate().child(label))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, _, cx| this.close_pr_tab(&middle, cx)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let seed = this.pull_requests.entry_for(&activate);
                        this.open_pull_request_panel(activate.clone(), seed, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        h_flex()
            .id("pr-tabs")
            .h(px(40.))
            .flex_none()
            .min_w_0()
            .px(px(8.))
            .gap(px(4.))
            .overflow_x_scroll()
            .border_b_1()
            .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
            .children(tabs)
            .into_any_element()
    }

    /// The condensed header row: author and time, branches, and the diff size (`condensedRowRef`).
    pub(super) fn render_pr_condensed_row(&self, detail: &Detail, t: &Tokens) -> AnyElement {
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
                    .children(diff_stat(detail.additions, detail.deletions, 11., t)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_keeps_its_minimum_and_leaves_the_list_its_own() {
        assert_eq!(clamp_panel_width(640., 1600.), 640.);
        assert_eq!(clamp_panel_width(1500., 1600.), 1280., "the list keeps 320px");
        assert_eq!(clamp_panel_width(100., 1600.), 360.);
        assert_eq!(clamp_panel_width(500., 600.), 360., "a narrow window keeps the panel usable");
    }
}
