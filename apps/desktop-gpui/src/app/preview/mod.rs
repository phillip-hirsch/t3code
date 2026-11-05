//! The browser panel and its floating mini player (`apps/web/src/components/preview/**`).
//!
//! This client hosts a thread's desktop-runtime preview tabs in native WebViews
//! (WebView2 on Windows) and reports their navigation with `preview.reportStatus`,
//! as Electron's `<webview>` host does. Server-runtime tabs, which agents open, run in
//! the server's own Chromium; this client cannot draw them yet, so they are not listed.

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClickEvent, Context, Div, DragMoveEvent, Empty, Entity, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, anchored, deferred, div, point,
    px,
};
use gpui_webview::WebView;
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, WindowHandle};
use serde_json::{Value, json};

use super::titlebar::{RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT};
use super::{Target, Workspace};
use crate::preview::{self, Appearance, Applied, NavStatus, Session, ThreadPreviews};
use crate::theme::{self, Tokens};

#[cfg(windows)]
mod native;

gpui_kit::actions!(preview, [TogglePreview]);

/// `usePreviewPanelInlineSize`: default and minimum width, and the chat column it leaves.
const DEFAULT_WIDTH: f32 = 540.;
const MIN_WIDTH: f32 = 360.;
const SIBLING_MIN_WIDTH: f32 = 360.;
/// The tab row clears the panel toggles and, on Windows, the caption buttons drawn over it.
const TAB_ROW_RIGHT_INSET: f32 = if cfg!(target_os = "windows") { 138. + 13. + 92. } else { 13. + 92. };
/// `previewMiniPlayerLayout.ts`.
const MINI_SIZE: f32 = 320.;
const MINI_EDGE_GAP: f32 = 12.;
const MINI_HEADER: f32 = 28.;
/// The inline chrome row: `h-7` plus its `mb-3`.
const CHROME_ROW: f32 = 28. + 12.;

/// What a native WebView tells the panel, in the order it happened.
pub(super) enum PageEvent {
    Started(String),
    Finished(String),
    Title(String),
    /// Back/forward availability may have changed without a load.
    History,
    Failed {
        url: String,
        code: i64,
        description: String,
    },
    /// The page asked for a new window (`window.open`, `target=_blank`).
    OpenLink(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Presentation {
    Panel,
    Floating,
}

struct HostedTab {
    thread_id: String,
    view: Entity<WebView>,
    status: NavStatus,
    can_go_back: bool,
    can_go_forward: bool,
    /// The last state sent with `preview.reportStatus`.
    reported: Option<(NavStatus, bool, bool)>,
    /// The page zoom and colour scheme last applied to the native view.
    zoom: f64,
    appearance: Option<Appearance>,
    shown: bool,
    _events: Task<()>,
}

impl HostedTab {
    fn loading(&self) -> bool {
        matches!(self.status, NavStatus::Loading { .. })
    }
}

pub(super) struct PreviewState {
    threads: HashMap<String, ThreadPreviews>,
    /// Native views by tab id, created when a tab first shows a page.
    tabs: HashMap<String, HostedTab>,
    /// Tabs whose native view is being built, with the URL it should end up on.
    starting: HashMap<String, String>,
    shown: HashMap<String, Presentation>,
    width: Pixels,
    address: Entity<InputState>,
    /// The tab whose URL the address bar shows.
    address_tab: Option<String>,
    menu_open: bool,
    /// Threads whose session list must be (re)fetched.
    stale: HashSet<String>,
    _events: Option<Task<()>>,
    _subscription: Subscription,
}

impl PreviewState {
    /// The event stream ends with the socket; lists are refetched on reconnect.
    pub(super) fn disconnect(&mut self) {
        self._events = None;
    }
}

/// The GPUI window's native handle, owned so a view can be built after the borrow ends.
struct ParentWindow(RawWindowHandle);

impl HasWindowHandle for ParentWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: the handle is the live GPUI window that owns this workspace.
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}

/// Drag payload for the panel's resize rail.
struct PanelResize;

impl PreviewState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let address = cx.new(|cx| InputState::new(window, cx).placeholder("Search or enter URL"));
        let subscription = cx.subscribe_in(&address, window, |this, input, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let text = input.read(cx).value().to_string();
                this.submit_address(&text, window, cx);
            }
        });
        Self {
            threads: HashMap::new(),
            tabs: HashMap::new(),
            starting: HashMap::new(),
            shown: HashMap::new(),
            width: px(DEFAULT_WIDTH),
            address,
            address_tab: None,
            menu_open: false,
            stale: HashSet::new(),
            _events: None,
            _subscription: subscription,
        }
    }
}

fn tooltip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView {
    move |window, cx| Tooltip::new(text).build(window, cx)
}

/// A 24px ghost icon button (`Button variant=ghost size=icon-xs`).
fn icon_button(id: impl Into<gpui_kit::ElementId>, icon: IconName, enabled: bool, tokens: &Tokens) -> Stateful<Div> {
    let accent = tokens.accent;
    div()
        .id(id.into())
        .flex_none()
        .size(px(24.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(if enabled { tokens.muted_foreground } else { tokens.muted_foreground.opacity(0.5) })
        .when(enabled, |el| el.cursor_pointer().hover(move |style| style.bg(accent)))
        .child(Icon::new(icon).size(px(14.)))
}

impl Workspace {
    fn preview_thread_id(&self) -> Option<&str> {
        match self.target.as_ref()? {
            Target::Thread(id) => Some(id),
            Target::Draft { .. } => None,
        }
    }

    /// Tabs this client can draw: everything but the server's own browser.
    fn hosted_sessions<'a>(previews: &'a ThreadPreviews) -> impl Iterator<Item = &'a Session> {
        previews.sessions.iter().filter(|session| !session.server_hosted())
    }

    /// The thread's focused tab, or its last drawable one when an agent focused a server tab.
    fn active_session(&self, thread_id: &str) -> Option<&Session> {
        let previews = self.preview.threads.get(thread_id)?;
        previews.active().filter(|session| !session.server_hosted()).or_else(|| Self::hosted_sessions(previews).last())
    }

    fn presentation(&self) -> Option<(String, Presentation)> {
        let thread_id = self.preview_thread_id()?;
        Some((thread_id.to_owned(), *self.preview.shown.get(thread_id)?))
    }

    /// Whether the panel takes a column beside the chat.
    pub(super) fn preview_panel_open(&self) -> bool {
        self.page.is_none() && matches!(self.presentation(), Some((_, Presentation::Panel)))
    }

    /// The panel's width, for the chat canvas beside it.
    pub(super) fn preview_panel_width(&self) -> f32 {
        if self.preview_panel_open() { f32::from(self.preview.width) } else { 0. }
    }

    /// The tab whose native view is on screen. A native view covers GPUI overlays in its
    /// bounds on Windows, so it hides while Settings or the panel's menu is open.
    fn shown_preview_tab(&self) -> Option<String> {
        if self.page.is_some() || self.settings.is_some() || self.preview.menu_open {
            return None;
        }
        let (thread_id, _) = self.presentation()?;
        self.active_session(&thread_id).map(|session| session.tab_id.clone())
    }

    /// Subscribes to every thread's preview events for this connection.
    pub(super) fn subscribe_previews(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        // Lists fetched before a reconnect may have missed events.
        for previews in self.preview.threads.values_mut() {
            previews.list_loaded = false;
        }
        self.preview.stale.extend(self.preview.threads.keys().cloned());
        if let Some(thread_id) = self.preview_thread_id() {
            self.preview.stale.insert(thread_id.to_owned());
        }
        let stream = client.stream("subscribePreviewEvents", json!({}));
        self.preview._events = Some(Self::pump(
            stream,
            window,
            cx,
            |this, value| this.apply_preview_event(value),
            |this, window, cx| this.after_preview_change(window, cx),
        ));
        self.after_preview_change(window, cx);
    }

    fn apply_preview_event(&mut self, event: &Value) {
        let Some(thread_id) = event.get("threadId").and_then(Value::as_str) else { return };
        let previews = self.preview.threads.entry(thread_id.to_owned()).or_default();
        match previews.apply_event(event) {
            Ok(Applied::Relist) => {
                self.preview.stale.insert(thread_id.to_owned());
            }
            Ok(_) if !previews.list_loaded => {
                self.preview.stale.insert(thread_id.to_owned());
            }
            Ok(_) => {}
            Err(error) => log::warn!("bad preview event: {error}"),
        }
    }

    /// Opening a thread fetches its tabs once.
    pub(super) fn load_previews(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.preview.threads.get(thread_id).is_some_and(|previews| previews.list_loaded) {
            self.preview.stale.insert(thread_id.to_owned());
        }
        self.after_preview_change(window, cx);
    }

    /// Refetches stale lists, drops views whose tab closed and refreshes the address bar.
    fn after_preview_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(client) = self.client() {
            for thread_id in std::mem::take(&mut self.preview.stale) {
                let client = client.clone();
                cx.spawn_in(window, async move |this, cx| {
                    let result = client.request("preview.list", json!({"threadId": thread_id})).await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        match result {
                            Ok(list) => {
                                let previews = this.preview.threads.entry(thread_id).or_default();
                                if let Err(error) = previews.apply_list(&list) {
                                    log::warn!("bad preview list: {error}");
                                }
                            }
                            Err(error) => log::warn!("preview.list failed: {error}"),
                        }
                        this.after_preview_change(window, cx);
                    });
                })
                .detach();
            }
        }
        let threads = &self.preview.threads;
        self.preview.tabs.retain(|tab_id, tab| threads.get(&tab.thread_id).is_none_or(|p| p.get(tab_id).is_some()));
        self.preview.shown.retain(|thread_id, _| {
            threads.get(thread_id).is_none_or(|p| !p.list_loaded || Self::hosted_sessions(p).next().is_some())
        });
        self.sync_address(window, cx);
        cx.notify();
    }

    /// The URL the active tab shows, as its address bar reads when not being edited.
    fn active_url(&self) -> String {
        let Some(thread_id) = self.preview_thread_id() else { return String::new() };
        let Some(session) = self.active_session(thread_id) else { return String::new() };
        match self.preview.tabs.get(&session.tab_id) {
            Some(tab) => tab.status.url().unwrap_or_default().to_owned(),
            None => preview::display_url(session).to_owned(),
        }
    }

    /// Shows the active tab's URL, unless the user is editing the address of that same tab.
    fn sync_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let address = self.preview.address.clone();
        let tab = self.preview_thread_id().and_then(|id| self.active_session(id)).map(|s| s.tab_id.clone());
        let same_tab = std::mem::replace(&mut self.preview.address_tab, tab.clone()) == tab;
        if same_tab && address.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        let url = self.active_url();
        if address.read(cx).value().as_ref() != url {
            address.update(cx, |input, cx| input.set_value(url, window, cx));
        }
    }

    /// `preview.toggle` and the titlebar's right-panel toggle (`togglePreviewPanel`).
    pub(super) fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.preview_thread_id().map(str::to_owned) else { return };
        if self.preview_panel_open() {
            // Closing the panel floats its page over the chat, as the web does.
            if self.active_session(&thread_id).is_some() {
                self.preview.shown.insert(thread_id, Presentation::Floating);
            } else {
                self.preview.shown.remove(&thread_id);
            }
            self.preview.menu_open = false;
            cx.notify();
            return;
        }
        self.page = None;
        if self.active_session(&thread_id).is_some() {
            self.preview.shown.insert(thread_id, Presentation::Panel);
            self.sync_address(window, cx);
            cx.notify();
        } else {
            self.open_preview_tab(thread_id, None, window, cx);
        }
    }

    /// `openPreviewSession`: a new tab in the panel, optionally loading `url`. Project
    /// scripts with `autoOpenPreview` open their `previewUrl` through this.
    pub(super) fn open_preview_tab(
        &mut self,
        thread_id: String,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let mut input = json!({"threadId": thread_id});
        if let Some(url) = &url {
            input["url"] = url.clone().into();
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("preview.open", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result.map_err(|e| e.to_string()).and_then(|v| {
                    serde_json::from_value::<Session>(v).map_err(|e| format!("Unexpected preview.open result: {e}"))
                }) {
                    Ok(session) => {
                        this.preview.threads.entry(thread_id.clone()).or_default().apply_snapshot(session, true);
                        this.preview.shown.insert(thread_id, Presentation::Panel);
                        if url.is_none() {
                            this.preview.address.update(cx, |input, cx| input.focus(window, cx));
                        }
                    }
                    Err(error) => this.error = Some(format!("Unable to open browser: {error}").into()),
                }
                this.after_preview_change(window, cx);
            });
        })
        .detach();
    }

    fn select_preview_tab(&mut self, thread_id: &str, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(previews) = self.preview.threads.get_mut(thread_id) {
            previews.active_tab = Some(tab_id.to_owned());
        }
        self.sync_address(window, cx);
        cx.notify();
    }

    fn close_preview_tab(&mut self, thread_id: String, tab_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(previews) = self.preview.threads.get_mut(&thread_id) {
            previews.remove(&tab_id);
        }
        self.preview.tabs.remove(&tab_id);
        if let Some(client) = self.client() {
            cx.spawn(async move |this, cx| {
                if let Err(error) =
                    client.request("preview.close", json!({"threadId": thread_id, "tabId": tab_id})).await
                {
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some(format!("Unable to close the browser tab: {error}").into());
                        cx.notify();
                    });
                }
            })
            .detach();
        }
        self.after_preview_change(window, cx);
    }

    fn submit_address(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.preview_thread_id().map(str::to_owned) else { return };
        if text.trim().is_empty() {
            return;
        }
        let url = match preview::resolve_address(text, preview::DEFAULT_SEARCH_TEMPLATE) {
            Ok(url) => url,
            Err(message) => {
                self.error = Some(format!("Could not open this address. {message}").into());
                cx.notify();
                return;
            }
        };
        let Some(tab_id) = self.active_session(&thread_id).map(|s| s.tab_id.clone()) else {
            return self.open_preview_tab(thread_id, Some(url), window, cx);
        };
        match self.preview.tabs.get(&tab_id) {
            Some(tab) => tab.view.update(cx, |view, _| view.load_url(&url)),
            None => self.host_tab(&thread_id, &tab_id, &url, window, cx),
        }
        window.blur(cx);
        cx.notify();
    }

    /// Creates the native view for a tab and starts `url` in it. The view is built
    /// asynchronously: WebView2's blocking constructor pumps the window's messages, which
    /// would run GPUI tasks while the app is borrowed.
    fn host_tab(&mut self, thread_id: &str, tab_id: &str, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = self.preview.starting.get_mut(tab_id) {
            *pending = url.to_owned();
            return;
        }
        let parent = match window.window_handle() {
            Ok(handle) => ParentWindow(handle.as_raw()),
            Err(error) => {
                self.error = Some(format!("Unable to start the browser: {error}").into());
                return;
            }
        };
        let (sender, events) = async_channel::unbounded();
        let builder = wry::WebViewBuilder::new()
            .with_url(url)
            .with_devtools(true)
            .with_on_page_load_handler({
                let sender = sender.clone();
                move |event, url| {
                    let _ = sender.try_send(match event {
                        wry::PageLoadEvent::Started => PageEvent::Started(url),
                        wry::PageLoadEvent::Finished => PageEvent::Finished(url),
                    });
                }
            })
            .with_document_title_changed_handler({
                let sender = sender.clone();
                move |title| {
                    let _ = sender.try_send(PageEvent::Title(title));
                }
            })
            .with_new_window_req_handler({
                let sender = sender.clone();
                move |url, _| {
                    let _ = sender.try_send(PageEvent::OpenLink(url));
                    wry::NewWindowResponse::Deny
                }
            });
        self.preview.starting.insert(tab_id.to_owned(), url.to_owned());
        let (thread_id, tab_id, url) = (thread_id.to_owned(), tab_id.to_owned(), url.to_owned());
        cx.spawn_in(window, async move |this, cx| {
            let built = builder.build_as_child_async(&parent).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let latest = this.preview.starting.remove(&tab_id).unwrap_or_default();
                let webview = match built {
                    Ok(webview) => webview,
                    Err(error) => {
                        this.error = Some(format!("Unable to start the browser: {error}").into());
                        cx.notify();
                        return;
                    }
                };
                // The tab closed while its view was starting.
                if this.preview.threads.get(&thread_id).and_then(|p| p.get(&tab_id)).is_none() {
                    return;
                }
                if latest != url {
                    let _ = webview.load_url(&latest);
                }
                #[cfg(windows)]
                native::watch(&webview, sender);
                #[cfg(not(windows))]
                drop(sender);
                this.adopt_view(thread_id, tab_id, latest, webview, events, window, cx);
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn adopt_view(
        &mut self,
        thread_id: String,
        tab_id: String,
        url: String,
        webview: wry::WebView,
        events: async_channel::Receiver<PageEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let webview = WebView::new(webview, window, cx);
        // Hidden until a frame lays it out, so it never flashes at the window's origin.
        let _ = webview.set_visible(false);
        let view = cx.new(|_| webview);
        let tab = tab_id.clone();
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update_in(cx, |this, window, cx| this.on_page_event(&tab, event, window, cx)).is_err() {
                    break;
                }
            }
        });
        self.preview.tabs.insert(
            tab_id,
            HostedTab {
                thread_id,
                view,
                status: NavStatus::Loading { url, title: String::new() },
                can_go_back: false,
                can_go_forward: false,
                reported: None,
                zoom: 1.,
                appearance: None,
                shown: false,
                _events: pump,
            },
        );
        cx.notify();
    }

    fn on_page_event(&mut self, tab_id: &str, event: PageEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.preview.tabs.get_mut(tab_id) else { return };
        let title = tab.status.title().unwrap_or_default().to_owned();
        match event {
            PageEvent::Started(url) => tab.status = NavStatus::Loading { url, title },
            // A failure reported for this load stays; the error page's load is not a success.
            PageEvent::Finished(url) => {
                if !matches!(&tab.status, NavStatus::LoadFailed { url: failed, .. } if *failed == url) {
                    tab.status = NavStatus::Success { url, title };
                }
            }
            PageEvent::Title(new_title) => match &mut tab.status {
                NavStatus::Loading { title, .. }
                | NavStatus::Success { title, .. }
                | NavStatus::LoadFailed { title, .. } => *title = new_title,
                NavStatus::Idle => {}
            },
            PageEvent::History => {}
            PageEvent::Failed { url, code, description } => {
                tab.status = NavStatus::LoadFailed { url, title, code, description };
            }
            PageEvent::OpenLink(url) => {
                let thread_id = tab.thread_id.clone();
                self.open_preview_tab(thread_id, Some(url), window, cx);
                return;
            }
        }
        #[cfg(windows)]
        {
            (tab.can_go_back, tab.can_go_forward) = native::history(tab.view.read(cx).raw());
        }
        #[cfg(not(windows))]
        {
            // WKWebView and WebKitGTK history isn't reachable through wry; keep both usable.
            (tab.can_go_back, tab.can_go_forward) = (true, true);
        }
        self.report_status(tab_id, cx);
        self.sync_address(window, cx);
        cx.notify();
    }

    /// `preview.reportStatus`, once per change, so the server and agents see this tab's page.
    fn report_status(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(tab) = self.preview.tabs.get_mut(tab_id) else { return };
        let state = (tab.status.clone(), tab.can_go_back, tab.can_go_forward);
        if tab.reported.as_ref() == Some(&state) {
            return;
        }
        let input = json!({
            "threadId": tab.thread_id,
            "tabId": tab_id,
            "navStatus": state.0.to_json(),
            "canGoBack": state.1,
            "canGoForward": state.2,
        });
        tab.reported = Some(state);
        cx.spawn(async move |_, _| {
            if let Err(error) = client.request("preview.reportStatus", input).await {
                log::warn!("preview.reportStatus failed: {error}");
            }
        })
        .detach();
    }

    /// `preview.adjust` / `preview.resize`; the returned snapshot updates the tab.
    fn adjust_preview(&mut self, method: &'static str, change: Value, cx: &mut Context<Self>) {
        let (Some(client), Some(thread_id)) = (self.client(), self.preview_thread_id().map(str::to_owned)) else {
            return;
        };
        let Some(tab_id) = self.active_session(&thread_id).map(|s| s.tab_id.clone()) else { return };
        let mut input = json!({"threadId": thread_id, "tabId": tab_id});
        if let (Some(input), Value::Object(change)) = (input.as_object_mut(), change) {
            input.extend(change);
        }
        cx.spawn(async move |this, cx| {
            let result = client.request(method, input).await;
            let _ = this.update(cx, |this, cx| {
                match result.map_err(|e| e.to_string()).and_then(|v| {
                    serde_json::from_value::<Session>(v).map_err(|e| format!("Unexpected {method} result: {e}"))
                }) {
                    Ok(session) => this.preview.threads.entry(thread_id).or_default().apply_snapshot(session, false),
                    Err(error) => this.error = Some(format!("Unable to change the browser tab: {error}").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn with_active_view(&self, cx: &App, action: impl FnOnce(&wry::WebView)) {
        let Some(thread_id) = self.preview_thread_id() else { return };
        let Some(session) = self.active_session(thread_id) else { return };
        if let Some(tab) = self.preview.tabs.get(&session.tab_id) {
            action(tab.view.read(cx).raw());
        }
    }

    /// Applies each hosted tab's zoom, colour scheme and visibility to its native view.
    /// Runs every frame; it only calls into the native view when something changed.
    fn sync_preview_views(&mut self, fit_scale: f32, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown_preview_tab();
        // The shown tab's page appears once it has a URL to load.
        if let Some(tab_id) = shown.as_deref().filter(|tab| !self.preview.tabs.contains_key(*tab)) {
            let thread_id = self.preview_thread_id().unwrap_or_default().to_owned();
            if let Some(url) = self.active_session(&thread_id).and_then(|s| s.nav_status.url()).map(str::to_owned) {
                self.host_tab(&thread_id, tab_id, &url, window, cx);
            }
        }
        for (tab_id, tab) in &mut self.preview.tabs {
            let visible = shown.as_deref() == Some(tab_id.as_str());
            let session = self.preview.threads.get(&tab.thread_id).and_then(|p| p.get(tab_id));
            let view = tab.view.read(cx);
            if visible {
                let zoom = session.and_then(|s| s.zoom_factor).unwrap_or(1.) * f64::from(fit_scale);
                if (tab.zoom - zoom).abs() > 1e-6 {
                    tab.zoom = zoom;
                    let _ = view.set_zoom(zoom);
                }
                let appearance = session.and_then(|s| s.color_scheme).unwrap_or_default();
                if tab.appearance != Some(appearance) {
                    tab.appearance = Some(appearance);
                    let _ = view.set_theme(match appearance {
                        Appearance::System => wry::Theme::Auto,
                        Appearance::Light => wry::Theme::Light,
                        Appearance::Dark => wry::Theme::Dark,
                    });
                }
            }
            if tab.shown != visible {
                tab.shown = visible;
                let _ = view.set_visible(visible);
            }
        }
    }

    /// The panel column beside the chat, or `None` while it is closed.
    pub(super) fn render_preview_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.preview_panel_open() {
            self.sync_preview_views(1., window, cx);
            return None;
        }
        let tokens = *theme::tokens(cx);
        let thread_id = self.preview_thread_id()?.to_owned();
        let available = (
            f32::from(self.preview.width) - f32::from(RIGHT_RESIZE_INSET) - 1.,
            f32::from(window.viewport_size().height) - f32::from(TITLEBAR_HEIGHT) - CHROME_ROW,
        );
        let page = self.render_page_area(&thread_id, available, window, cx);
        let rail = div()
            .id("preview-rail")
            .group("preview-rail")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-8.))
            .w(px(16.))
            .cursor_col_resize()
            .on_drag(PanelResize, |_, _, _, cx| cx.new(|_| Empty))
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                if event.click_count() == 2 {
                    this.preview.width = px(DEFAULT_WIDTH);
                    cx.notify();
                }
            }))
            .child(
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(7.))
                    .w(px(2.))
                    .invisible()
                    .group_hover("preview-rail", |s| s.visible())
                    .bg(tokens.border),
            );
        Some(
            v_flex()
                .id("preview-panel")
                .relative()
                .flex_none()
                .w(self.preview.width)
                .h_full()
                .pr(RIGHT_RESIZE_INSET)
                .border_l_1()
                .border_color(tokens.border)
                .bg(tokens.background)
                .on_drag_move(cx.listener(|this, event: &DragMoveEvent<PanelResize>, window, cx| {
                    let right = event.bounds.right();
                    let max = (window.viewport_size().width - px(SIBLING_MIN_WIDTH)).max(px(MIN_WIDTH));
                    this.preview.width = (right - event.event.position.x).clamp(px(MIN_WIDTH), max);
                    cx.notify();
                }))
                .child(self.render_preview_tabs(&thread_id, &tokens, cx))
                .child(self.render_preview_chrome(&thread_id, &tokens, window, cx))
                .child(page)
                .child(deferred(rail).with_priority(1))
                .into_any_element(),
        )
    }

    /// The page, the empty state of a blank tab, or a fixed viewport centered in the panel.
    fn render_page_area(
        &mut self,
        thread_id: &str,
        available: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let session = self.active_session(thread_id).cloned();
        let fixed = session.as_ref().and_then(|s| s.viewport.size()).map(|size| preview::fit_viewport(size, available));
        self.sync_preview_views(fixed.map_or(1., |(_, scale)| scale), window, cx);
        let view = session.as_ref().and_then(|s| self.preview.tabs.get(&s.tab_id)).map(|tab| tab.view.clone());
        let Some(view) = view.filter(|_| !self.preview.menu_open) else {
            let blank = session.as_ref().is_none_or(|s| matches!(s.nav_status, NavStatus::Idle));
            return div().flex_1().min_h_0().when(blank, |el| el.child(render_empty_state(&tokens))).into_any_element();
        };
        match fixed {
            Some(((width, height), _)) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_start()
                .justify_center()
                .bg(tokens.muted)
                .child(div().w(px(width)).h(px(height)).child(view))
                .into_any_element(),
            None => div().flex_1().min_h_0().child(view).into_any_element(),
        }
    }

    /// `RightPanelTabs` for browser surfaces: one tab per page, then "+".
    fn render_preview_tabs(&self, thread_id: &str, tokens: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let previews = self.preview.threads.get(thread_id);
        let active = previews.and_then(|p| p.active_tab.clone());
        let mut row = h_flex().h(TITLEBAR_HEIGHT).flex_none().items_center().gap_1().pl_2().pr(px(TAB_ROW_RIGHT_INSET));
        for session in previews.into_iter().flat_map(Self::hosted_sessions) {
            let is_active = active.as_deref() == Some(session.tab_id.as_str());
            let hosted = self.preview.tabs.get(&session.tab_id);
            let title: SharedString = hosted
                .and_then(|tab| tab.status.title().map(str::to_owned))
                .or_else(|| session.nav_status.title().map(str::to_owned))
                .or_else(|| hosted.and_then(|tab| tab.status.url()).or(session.nav_status.url()).map(host_of))
                .unwrap_or_else(|| "New tab".to_owned())
                .into();
            let (select_thread, select_tab) = (thread_id.to_owned(), session.tab_id.clone());
            let (close_thread, close_tab) = (thread_id.to_owned(), session.tab_id.clone());
            let group: SharedString = format!("preview-tab-{}", session.tab_id).into();
            let (accent, foreground) = (tokens.accent, tokens.foreground);
            row = row.child(
                h_flex()
                    .id(SharedString::from(format!("preview-tab:{}", session.tab_id)))
                    .group(group.clone())
                    .flex_none()
                    .h(px(24.))
                    .max_w(px(144.))
                    .gap(px(2.))
                    .pl(px(6.))
                    .pr(px(4.))
                    .rounded(px(6.))
                    .text_xs()
                    .cursor_pointer()
                    .occlude()
                    .map(|el| {
                        if is_active {
                            el.bg(accent).text_color(foreground)
                        } else {
                            el.text_color(tokens.muted_foreground)
                                .hover(move |style| style.bg(accent.opacity(0.6)).text_color(foreground))
                        }
                    })
                    .child(Icon::new(IconName::Globe).size(px(12.)).flex_none())
                    .child(div().min_w_0().px(px(2.)).truncate().child(title))
                    .child(
                        div()
                            .id(SharedString::from(format!("preview-tab-close:{}", session.tab_id)))
                            .flex_none()
                            .size(px(16.))
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .invisible()
                            .when(is_active, |el| el.visible())
                            .group_hover(group, |style| style.visible())
                            .hover(|style| style.bg(tokens.muted))
                            .child(Icon::new(IconName::X).size(px(12.)))
                            .tooltip(tooltip("Close tab"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_preview_tab(close_thread.clone(), close_tab.clone(), window, cx);
                            })),
                    )
                    .on_mouse_down(MouseButton::Middle, {
                        let (thread, tab) = (thread_id.to_owned(), session.tab_id.clone());
                        cx.listener(move |this, _, window, cx| {
                            this.close_preview_tab(thread.clone(), tab.clone(), window, cx)
                        })
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_preview_tab(&select_thread, &select_tab, window, cx)
                    })),
            );
        }
        let new_tab_thread = thread_id.to_owned();
        row.child(
            icon_button("preview-new-tab", IconName::Plus, true, tokens)
                .occlude()
                .tooltip(tooltip("New browser tab"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_preview_tab(new_tab_thread.clone(), None, window, cx)
                })),
        )
    }

    /// `PreviewChromeRow` in the inline panel: back, forward, reload, the address bar and the menu.
    fn render_preview_chrome(
        &self,
        thread_id: &str,
        tokens: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let session = self.active_session(thread_id);
        let hosted = session.and_then(|s| self.preview.tabs.get(&s.tab_id));
        let (can_back, can_forward) = hosted.map_or((false, false), |tab| (tab.can_go_back, tab.can_go_forward));
        let loading = hosted.is_some_and(HostedTab::loading);
        let url = self.active_url();
        let has_page = hosted.is_some();
        let address_focused = self.preview.address.read(cx).focus_handle(cx).is_focused(window);
        // The menu's top-right corner: under the trigger at the chrome row's right end.
        let menu_corner = point(window.viewport_size().width - RIGHT_RESIZE_INSET - px(8.), TITLEBAR_HEIGHT + px(32.));
        h_flex()
            .relative()
            .h(px(28.))
            .mb_3()
            .flex_none()
            .items_center()
            .gap_1()
            .px_2()
            .child(
                h_flex()
                    .gap(px(2.))
                    .child(
                        icon_button("preview-back", IconName::ArrowLeft, can_back, tokens)
                            .tooltip(tooltip("Back"))
                            .when(can_back, |el| {
                                el.on_click(cx.listener(|this, _, _, cx| {
                                    this.with_active_view(cx, |view| {
                                        let _ = view.evaluate_script("history.back()");
                                    })
                                }))
                            }),
                    )
                    .child(
                        icon_button("preview-forward", IconName::ArrowRight, can_forward, tokens)
                            .tooltip(tooltip("Forward"))
                            .when(can_forward, |el| {
                                el.on_click(cx.listener(|this, _, _, cx| {
                                    this.with_active_view(cx, |view| {
                                        let _ = view.evaluate_script("history.forward()");
                                    })
                                }))
                            }),
                    )
                    .child(
                        icon_button("preview-reload", IconName::RotateCw, has_page, tokens)
                            .tooltip(tooltip(if loading { "Loading…" } else { "Refresh" }))
                            .when(has_page, |el| {
                                el.on_click(cx.listener(|this, _, _, cx| {
                                    this.with_active_view(cx, |view| {
                                        let _ = view.reload();
                                    })
                                }))
                            }),
                    ),
            )
            .child(
                h_flex()
                    .group("preview-address")
                    .flex_1()
                    .min_w_0()
                    .h(px(28.))
                    .rounded(px(8.))
                    .hover(|style| style.bg(tokens.accent.opacity(0.5)))
                    .child(
                        Input::new(&self.preview.address)
                            .appearance(false)
                            .flex_1()
                            .h(px(28.))
                            .px_2()
                            .text_size(px(13.)),
                    )
                    .when(!url.is_empty() && !address_focused, |el| {
                        el.child(
                            icon_button("preview-open-external", IconName::ExternalLink, true, tokens)
                                .invisible()
                                .group_hover("preview-address", |style| style.visible())
                                .tooltip(tooltip("Open in system browser"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let url = this.active_url();
                                    if !url.is_empty() {
                                        cx.open_url(&url);
                                    }
                                })),
                        )
                    }),
            )
            .child(self.render_preview_menu(session, has_page, menu_corner, tokens, cx))
            .when(loading, |el| {
                el.child(div().absolute().left_0().right_0().bottom(px(-1.)).h(px(2.)).bg(tokens.primary))
            })
    }

    /// `PreviewMoreMenu`.
    fn render_preview_menu(
        &self,
        session: Option<&Session>,
        has_page: bool,
        corner: Point<Pixels>,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = self.preview.menu_open;
        let trigger = icon_button("preview-more", IconName::Ellipsis, session.is_some(), tokens)
            .when(self.preview.menu_open, |el| el.bg(tokens.accent))
            .tooltip(tooltip("More"))
            // Acts on press: the menu's outside-press dismissal runs first, so a press that
            // already closed it must not reopen it.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if this.preview.menu_open == open {
                        this.preview.menu_open = !open;
                        cx.notify();
                    }
                }),
            );
        let Some(session) = session.filter(|_| self.preview.menu_open) else { return div().child(trigger) };
        let zoom = session.zoom_factor.unwrap_or(1.);
        let appearance = session.color_scheme.unwrap_or_default();
        let preset = session.viewport.preset_id().map(str::to_owned);
        let fill = session.viewport.size().is_none();

        let item = |id: SharedString, label: SharedString, checked: bool, enabled: bool| {
            let accent = tokens.accent;
            h_flex()
                .id(id)
                .h(px(28.))
                .px_2()
                .gap_2()
                .rounded(px(6.))
                .text_sm()
                .text_color(if enabled { tokens.foreground } else { tokens.muted_foreground })
                .when(enabled, |el| el.cursor_pointer().hover(move |style| style.bg(accent)))
                .child(div().flex_1().truncate().child(label))
                .when(checked, |el| el.child(Icon::new(IconName::Check).size(px(14.))))
        };
        let heading = |label: &'static str| {
            div()
                .px_2()
                .pt_2()
                .pb_1()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(tokens.muted_foreground)
                .child(label)
        };
        let separator = || div().my_1().h(px(1.)).bg(tokens.border);
        let close = |this: &mut Self, cx: &mut Context<Self>| {
            this.preview.menu_open = false;
            cx.notify();
        };

        let mut menu = v_flex()
            .id("preview-menu")
            .w(px(240.))
            .max_h(px(520.))
            .overflow_y_scroll()
            .p_1()
            .rounded(px(10.))
            .border_1()
            .border_color(tokens.border)
            .bg(tokens.popover)
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(move |this, _, _, cx| close(this, cx)))
            .child(item("menu-hard-reload".into(), "Hard reload".into(), false, has_page).when(has_page, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| {
                    this.with_active_view(cx, hard_reload);
                    close(this, cx);
                }))
            }))
            .child(item("menu-devtools".into(), "Open DevTools".into(), false, has_page).when(has_page, |el| {
                el.on_click(cx.listener(move |this, _, _, cx| {
                    this.with_active_view(cx, |view| view.open_devtools());
                    close(this, cx);
                }))
            }))
            .child(separator())
            .child(heading("Appearance"));
        for option in Appearance::ALL {
            menu = menu.child(
                item(
                    format!("menu-appearance-{}", option.as_str()).into(),
                    option.label().into(),
                    appearance == option,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.adjust_preview("preview.adjust", json!({"colorScheme": option.as_str()}), cx);
                    close(this, cx);
                })),
            );
        }
        let zoom_step = |id: &'static str, icon: IconName, direction: i32, label: &'static str| {
            icon_button(id, icon, true, tokens).tooltip(tooltip(label)).on_click(cx.listener(move |this, _, _, cx| {
                this.adjust_preview("preview.adjust", json!({"zoomFactor": preview::step_zoom(zoom, direction)}), cx);
            }))
        };
        menu = menu
            .child(separator())
            .child(
                h_flex()
                    .h(px(28.))
                    .px_2()
                    .gap_1()
                    .text_sm()
                    .child(div().flex_1().child("Zoom"))
                    .child(zoom_step("menu-zoom-out", IconName::Minus, -1, "Zoom out"))
                    .child(
                        div()
                            .id("menu-zoom-reset")
                            .w(px(44.))
                            .text_center()
                            .text_xs()
                            .cursor_pointer()
                            .text_color(tokens.muted_foreground)
                            .child(format!("{}%", (zoom * 100.).round()))
                            .tooltip(tooltip("Reset zoom"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.adjust_preview("preview.adjust", json!({"zoomFactor": 1.0}), cx);
                            })),
                    )
                    .child(zoom_step("menu-zoom-in", IconName::Plus, 1, "Zoom in")),
            )
            .child(separator())
            .child(heading("Viewport"))
            .child(item("menu-viewport-fill".into(), "Fill panel".into(), fill, true).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.adjust_preview("preview.resize", json!({"viewport": {"_tag": "fill"}}), cx);
                    close(this, cx);
                },
            )));
        for preset_option in &preview::PRESETS {
            let checked = preset.as_deref() == Some(preset_option.id);
            let label: SharedString =
                format!("{} · {} × {}", preset_option.label, preset_option.width, preset_option.height).into();
            menu =
                menu.child(item(format!("menu-viewport-{}", preset_option.id).into(), label, checked, true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.adjust_preview(
                            "preview.resize",
                            json!({"viewport": preview::Viewport::preset(preset_option)}),
                            cx,
                        );
                        close(this, cx);
                    }),
                ));
        }
        #[cfg(windows)]
        {
            menu = menu
                .child(separator())
                .child(item("menu-clear-cookies".into(), "Clear cookies".into(), false, has_page).when(
                    has_page,
                    |el| {
                        el.on_click(cx.listener(move |this, _, _, cx| {
                            this.with_active_view(cx, |view| {
                                native::devtools_command(view, "Network.clearBrowserCookies", "{}")
                            });
                            close(this, cx);
                        }))
                    },
                ))
                .child(item("menu-clear-cache".into(), "Clear cache".into(), false, has_page).when(has_page, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        this.with_active_view(cx, |view| {
                            native::devtools_command(view, "Network.clearBrowserCache", "{}")
                        });
                        close(this, cx);
                    }))
                }));
        }
        div()
            .child(trigger)
            .child(deferred(anchored().anchor(Anchor::TopRight).position(corner).child(menu)).with_priority(2))
    }

    /// `ThreadPreviewMiniPlayer`: the active tab floating over the chat's bottom-right corner.
    pub(super) fn render_preview_mini_player(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (thread_id, Presentation::Floating) = self.presentation()? else { return None };
        if self.page.is_some() {
            return None;
        }
        let tokens = *theme::tokens(cx);
        let session = self.active_session(&thread_id)?.clone();
        let body = (MINI_SIZE, MINI_SIZE - MINI_HEADER);
        let fixed = session.viewport.size().map(|size| preview::fit_viewport(size, body));
        self.sync_preview_views(fixed.map_or(1., |(_, scale)| scale), window, cx);
        let view = self.preview.tabs.get(&session.tab_id).map(|tab| tab.view.clone());
        let title: SharedString = self
            .preview
            .tabs
            .get(&session.tab_id)
            .and_then(|tab| tab.status.title().map(str::to_owned).or_else(|| tab.status.url().map(host_of)))
            .unwrap_or_else(|| "Browser".to_owned())
            .into();
        let expand_thread = thread_id.clone();
        let close_thread = thread_id.clone();
        Some(
            v_flex()
                .id("preview-mini-player")
                .absolute()
                .right(px(MINI_EDGE_GAP))
                .bottom(px(MINI_EDGE_GAP))
                .w(px(MINI_SIZE))
                .h(px(MINI_SIZE))
                .rounded(px(12.))
                .overflow_hidden()
                .border_1()
                .border_color(tokens.border.opacity(0.8))
                .bg(tokens.muted)
                .shadow_2xl()
                .occlude()
                .child(
                    h_flex()
                        .h(px(MINI_HEADER))
                        .flex_none()
                        .pl_2()
                        .pr_1()
                        .gap_1()
                        .bg(tokens.popover)
                        .text_xs()
                        .child(Icon::new(IconName::Globe).size(px(12.)).text_color(tokens.muted_foreground))
                        .child(div().flex_1().min_w_0().truncate().child(title))
                        .child(
                            icon_button("mini-expand", IconName::PanelRight, true, &tokens)
                                .tooltip(tooltip("Open in panel"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.preview.shown.insert(expand_thread.clone(), Presentation::Panel);
                                    this.sync_address(window, cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            icon_button("mini-close", IconName::X, true, &tokens)
                                .tooltip(tooltip("Close floating preview"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.preview.shown.remove(&close_thread);
                                    cx.notify();
                                })),
                        ),
                )
                .child(match (view, fixed) {
                    (Some(view), Some(((width, height), _))) => div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .justify_center()
                        .child(div().w(px(width)).h(px(height)).child(view))
                        .into_any_element(),
                    (Some(view), None) => div().flex_1().min_h_0().child(view).into_any_element(),
                    (None, _) => div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .text_color(tokens.muted_foreground)
                        .child("No preview yet")
                        .into_any_element(),
                })
                .into_any_element(),
        )
    }
}

/// `PreviewEmptyState` without recents or local servers.
fn render_empty_state(tokens: &Tokens) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap_2()
        .p_6()
        .text_center()
        .child(
            div()
                .size(px(36.))
                .mb_2()
                .rounded(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .bg(tokens.muted)
                .child(Icon::new(IconName::Globe).size(px(18.)).text_color(tokens.muted_foreground)),
        )
        .child(div().text_base().font_weight(FontWeight::MEDIUM).text_color(tokens.foreground).child("No preview yet"))
        .child(div().max_w(px(320.)).text_sm().text_color(tokens.muted_foreground).child(
            "Type a URL above, or run a dev script. Local servers this thread starts will show up here automatically.",
        ))
}

/// A tab's fallback title: the page's host.
fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_owned)).unwrap_or_else(|| url.to_owned())
}

/// Electron's `reloadIgnoringCache`.
fn hard_reload(view: &wry::WebView) {
    #[cfg(windows)]
    native::devtools_command(view, "Page.reload", r#"{"ignoreCache":true}"#);
    #[cfg(not(windows))]
    let _ = view.reload();
}
