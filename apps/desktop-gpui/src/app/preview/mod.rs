//! The right panel's Browser surface and its floating mini player (`apps/web/src/components/preview/**`).
//!
//! This client hosts a thread's desktop-runtime preview tabs in native WebViews
//! (WebView2 on Windows) and reports their navigation with `preview.reportStatus`,
//! as Electron's `<webview>` host does. Server-runtime tabs, which agents open, run in
//! the server's own Chromium and stream to the panel (`server.rs`).

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, anchored, deferred, div, point, px,
};
use gpui_webview::WebView;
use serde_json::{Value, json};

use super::right_panel::Surface;
use super::titlebar::{RIGHT_RESIZE_INSET, TITLEBAR_HEIGHT};
use super::{Target, Workspace};
use crate::preview::{self, Appearance, Applied, NavStatus, Session, ThreadPreviews};
use crate::theme::{self, Tokens};

mod empty;
mod profiles;
mod server;

gpui_kit::actions!(preview, [TogglePreview]);

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
    /// The page's history moved, possibly without a load: whether it can go back and forward.
    History {
        back: bool,
        forward: bool,
    },
    Failed {
        url: String,
        code: i64,
        description: String,
    },
    /// The page asked for a new window (`window.open`, `target=_blank`).
    OpenLink(String),
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
    /// `browserDefaultZoomFactor` / `browserDefaultAppearance` when the tab was created; they
    /// apply until the tab is adjusted (`browserDefaultTabState`).
    defaults: (f64, Appearance),
    shown: bool,
    /// The tab's own page events, which a load's failure probe reports on.
    page_events: async_channel::Sender<PageEvent>,
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
    /// The tab each thread's mini player floats (`previewMiniPlayerStore`).
    floating: HashMap<String, String>,
    /// The tabs drawn this frame, by the panel and the mini player, with their page scale.
    panel_view: Option<(String, f32)>,
    mini_view: Option<(String, f32)>,
    address: Entity<InputState>,
    /// The tab whose URL the address bar shows.
    address_tab: Option<String>,
    pub(super) menu_open: bool,
    /// Threads whose session list must be (re)fetched.
    stale: HashSet<String>,
    /// What this connection last sent with `preview.reportProfiles`.
    reported_profiles: Option<Value>,
    /// Agent tabs' streams.
    server: server::ServerStreams,
    /// Pages opened per project, for a blank tab's "Recently used".
    history: crate::browser_history::BrowserHistory,
    discovery: empty::Discovery,
    /// Failed tabs showing the unreachable page's details.
    unreachable_details: HashSet<String>,
    _events: Option<Task<()>>,
    _subscription: Subscription,
}

impl PreviewState {
    /// The event stream ends with the socket; lists are refetched on reconnect.
    pub(super) fn disconnect(&mut self) {
        self._events = None;
        self.reported_profiles = None;
    }
}

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
            floating: HashMap::new(),
            panel_view: None,
            mini_view: None,
            address,
            address_tab: None,
            menu_open: false,
            stale: HashSet::new(),
            reported_profiles: None,
            server: Default::default(),
            history: crate::browser_history::BrowserHistory::load(),
            discovery: Default::default(),
            unreachable_details: HashSet::new(),
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
    pub(in crate::app) fn preview_thread_id(&self) -> Option<&str> {
        match self.target.as_ref()? {
            Target::Thread(id) => Some(id),
            Target::Draft { .. } => None,
        }
    }

    /// Tabs this client draws: its own pages and the agents' streamed ones.
    fn hosted_sessions<'a>(previews: &'a ThreadPreviews) -> impl Iterator<Item = &'a Session> {
        previews.sessions.iter()
    }

    /// The tab the panel shows, else the thread's focused tab, or its last drawable one when an
    /// agent focused a server tab. Another client may move the focus without changing the panel.
    fn active_session(&self, thread_id: &str) -> Option<&Session> {
        let previews = self.preview.threads.get(thread_id)?;
        let shown = match self.active_surface() {
            Some(Surface::Browser(tab)) => previews.get(tab),
            _ => None,
        };
        shown.or_else(|| previews.active()).or_else(|| Self::hosted_sessions(previews).last())
    }

    /// A tab's zoom and page colour scheme: its own, else the defaults it was created with.
    fn tab_look(&self, session: &Session) -> (f64, Appearance) {
        let defaults =
            self.preview.tabs.get(&session.tab_id).map_or_else(|| self.browser_defaults(), |tab| tab.defaults);
        (session.zoom_factor.unwrap_or(defaults.0), session.color_scheme.unwrap_or(defaults.1))
    }

    /// `browserDefaultZoomFactor` and `browserDefaultAppearance`.
    fn browser_defaults(&self) -> (f64, Appearance) {
        let settings = &self.client_settings.values;
        let zoom = settings.get("browserDefaultZoomFactor").as_f64().unwrap_or(1.);
        let appearance = serde_json::from_value(settings.get("browserDefaultAppearance").clone()).unwrap_or_default();
        (zoom, appearance)
    }

    /// `useOpenLink`: a web link opens in a new browser tab when "Open links in" is the app,
    /// else (and when that fails) in the system browser.
    pub(super) fn open_link(&mut self, url: &str, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.modifiers();
        let middle = matches!(event, ClickEvent::Mouse(click) if click.down.button == MouseButton::Middle);
        let modified = modifiers.control || modifiers.platform || middle;
        let in_app = preview::link_opens_in_app(url, modified, &self.client_settings.values, self.browser_available());
        match self.preview_thread_id().map(str::to_owned).filter(|_| in_app) {
            Some(thread_id) => self.request_preview_tab(thread_id, Some(url.to_owned()), true, window, cx),
            None => cx.open_url(url),
        }
    }

    /// `browserAvailable`: a thread to host tabs for.
    pub(super) fn browser_available(&self) -> bool {
        self.preview_thread_id().is_some()
    }

    /// A browser tab's label (`surfaceTitle`): the page title, else its host, else "Browser".
    pub(super) fn browser_tab_title(&self, tab_id: &str) -> SharedString {
        let session = self.preview_thread_id().and_then(|id| self.preview.threads.get(id)).and_then(|p| p.get(tab_id));
        let hosted = self.preview.tabs.get(tab_id);
        let title = hosted.and_then(|tab| tab.status.title()).or_else(|| session.and_then(|s| s.nav_status.title()));
        let url = hosted.and_then(|tab| tab.status.url()).or_else(|| session.and_then(|s| s.nav_status.url()));
        match (title.filter(|title| !title.trim().is_empty()), url) {
            (Some(title), _) => title.to_owned().into(),
            (None, Some(url)) => host_of(url).into(),
            (None, None) => "Browser".into(),
        }
    }

    /// Whether GPUI draws over the browser's area: a native view would cover it on Windows.
    fn browser_covered(&self, window: &mut Window, cx: &mut App) -> bool {
        self.page.is_some()
            || self.settings.is_some()
            || self.palette.is_some()
            || self.preview.menu_open
            || self.right_panel.add_menu_open
            || self.tab_menu_open()
            || window.has_active_dialog(cx)
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
        self.preview.floating.retain(|thread_id, tab| threads.get(thread_id).is_none_or(|p| p.get(tab).is_some()));
        let loaded: Vec<(String, Vec<String>)> = threads
            .iter()
            .filter(|(_, previews)| previews.list_loaded)
            .map(|(id, previews)| (id.clone(), Self::hosted_sessions(previews).map(|s| s.tab_id.clone()).collect()))
            .collect();
        for (thread_id, tabs) in &loaded {
            self.reconcile_browser_surfaces(thread_id, &tabs.iter().map(String::as_str).collect::<Vec<_>>());
        }
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

    /// `togglePreviewPanel` (`preview.toggle`): hides a panel showing the browser, else shows the
    /// thread's focused tab, else opens a new one.
    /// `preview.refresh` / `focusUrl` / `zoomIn` / `zoomOut` / `resetZoom` on the panel's browser tab.
    pub(super) fn preview_command(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.preview_thread_id().and_then(|thread_id| self.active_session(thread_id)) else {
            return;
        };
        let (zoom, _) = self.tab_look(session);
        let zoom_to = |direction| json!({"zoomFactor": preview::step_zoom(zoom, direction)});
        match command {
            "preview.refresh" => self.with_active_view(cx, |view| {
                let _ = view.reload();
            }),
            "preview.focusUrl" => self.preview.address.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            }),
            "preview.zoomIn" => self.adjust_preview("preview.adjust", zoom_to(1), cx),
            "preview.zoomOut" => self.adjust_preview("preview.adjust", zoom_to(-1), cx),
            "preview.resetZoom" => self.adjust_preview("preview.adjust", zoom_to(0), cx),
            _ => {}
        }
    }

    pub(super) fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.preview_thread_id().map(str::to_owned) else { return };
        if matches!(self.active_surface(), Some(Surface::Browser(_))) {
            return self.close_right_panel(cx);
        }
        match self.active_session(&thread_id).map(|s| s.tab_id.clone()) {
            Some(tab) => self.open_surface(Surface::Browser(tab), window, cx),
            None => self.open_preview_tab(thread_id, None, window, cx),
        }
    }

    /// `createBrowserSurface`: the launcher's and the "+" menu's Browser.
    pub(super) fn add_browser_surface(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_browser_surface_in(None, window, cx);
    }

    /// `onAddBrowserInProfile`: a new tab in `profile`, else the default profile.
    pub(super) fn add_browser_surface_in(
        &mut self,
        profile: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(thread_id) = self.preview_thread_id().map(str::to_owned) {
            self.open_browser_tab(thread_id, None, false, profile, window, cx);
        }
    }

    /// The profiles a new tab can open in (`useBrowserDefaults().profiles`).
    pub(super) fn browser_profiles(&self) -> Vec<preview::Profile> {
        preview::resolve_profiles(&preview::user_profiles(&self.client_settings.values))
    }

    /// Hiding the panel floats the tab it showed over the chat.
    pub(super) fn float_browser(&mut self, thread_id: String, tab_id: String, cx: &mut Context<Self>) {
        self.preview.floating.insert(thread_id, tab_id);
        cx.notify();
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
        self.request_preview_tab(thread_id, url, false, window, cx);
    }

    /// `preview.open` with the default viewport; `external_fallback` opens `url` in the system
    /// browser when the tab can't be opened, as `useOpenLink` does.
    pub(in crate::app) fn request_preview_tab(
        &mut self,
        thread_id: String,
        url: Option<String>,
        external_fallback: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_browser_tab(thread_id, url, external_fallback, None, window, cx);
    }

    fn open_browser_tab(
        &mut self,
        thread_id: String,
        url: Option<String>,
        external_fallback: bool,
        profile: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        let profile = profile.unwrap_or_else(|| preview::default_profile_id(&self.client_settings.values));
        let mut input = json!({"threadId": thread_id, "profileId": profile});
        if let Some(url) = &url {
            input["url"] = url.clone().into();
        }
        let viewport = self.client_settings.values.get("browserDefaultViewport");
        if viewport.is_object() {
            input["viewport"] = viewport.clone();
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("preview.open", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result.map_err(|e| e.to_string()).and_then(|v| {
                    serde_json::from_value::<Session>(v).map_err(|e| format!("Unexpected preview.open result: {e}"))
                }) {
                    Ok(session) => {
                        let tab = session.tab_id.clone();
                        if let Some(url) = &url {
                            this.record_visit(&thread_id, url);
                        }
                        this.preview.threads.entry(thread_id.clone()).or_default().apply_snapshot(session, true);
                        if this.preview_thread_id() == Some(thread_id.as_str()) {
                            this.open_surface(Surface::Browser(tab), window, cx);
                        }
                        if url.is_none() {
                            this.preview.address.update(cx, |input, cx| input.focus(window, cx));
                        }
                    }
                    Err(error) => match url.as_deref().filter(|_| external_fallback) {
                        Some(url) => {
                            log::warn!("preview.open failed, opening the system browser: {error}");
                            cx.open_url(url);
                        }
                        None => this.error = Some(format!("Unable to open browser: {error}").into()),
                    },
                }
                this.after_preview_change(window, cx);
            });
        })
        .detach();
    }

    /// `setActivePreviewTab`: the tab the address bar and the chrome act on.
    pub(super) fn focus_preview_tab(
        &mut self,
        thread_id: &str,
        tab_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(previews) =
            self.preview.threads.get_mut(thread_id).filter(|p| p.active_tab.as_deref() != Some(tab_id))
        {
            previews.active_tab = Some(tab_id.to_owned());
            self.sync_address(window, cx);
        }
    }

    /// `closePreviewSession`.
    pub(super) fn close_preview_tab(
        &mut self,
        thread_id: String,
        tab_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        let url = match preview::resolve_address(text, &preview::search_template(&self.client_settings.values)) {
            Ok(url) => url,
            Err(message) => {
                self.error = Some(format!("Could not open this address. {message}").into());
                cx.notify();
                return;
            }
        };
        self.record_visit(&thread_id, &url);
        let Some((tab_id, server)) = self.active_session(&thread_id).map(|s| (s.tab_id.clone(), s.server_hosted()))
        else {
            return self.open_preview_tab(thread_id, Some(url), window, cx);
        };
        if server {
            self.send_server_input(&tab_id, json!({"type": "navigate", "url": url}));
            window.blur(cx);
            return;
        }
        match self.preview.tabs.get(&tab_id) {
            Some(tab) => tab.view.update(cx, |view, _| view.load_url(&url)),
            None => self.host_tab(&thread_id, &tab_id, &url, window, cx),
        }
        window.blur(cx);
        cx.notify();
    }

    /// Creates the native view for a tab and starts `url` in it, from a task: see [`profiles::build`].
    fn host_tab(&mut self, thread_id: &str, tab_id: &str, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = self.preview.starting.get_mut(tab_id) {
            *pending = url.to_owned();
            return;
        }
        let session = self.preview.threads.get(thread_id).and_then(|p| p.get(tab_id));
        let partition = profiles::Storage::of(session.and_then(|s| s.profile_id.as_deref()));
        self.preview.starting.insert(tab_id.to_owned(), url.to_owned());
        let (thread_id, tab_id) = (thread_id.to_owned(), tab_id.to_owned());
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                let url = this.preview.starting.remove(&tab_id).unwrap_or_default();
                // The tab closed while its view was waiting to start.
                if this.preview.threads.get(&thread_id).and_then(|p| p.get(&tab_id)).is_none() {
                    return;
                }
                let (sender, events) = async_channel::unbounded();
                let mut context = partition.context();
                let builder = page_builder(partition.builder(context.as_mut()), &url, sender.clone());
                match profiles::build(builder, window) {
                    Ok(webview) => this.adopt_view(thread_id, tab_id, url, webview, (sender, events), window, cx),
                    Err(error) => {
                        this.error = Some(format!("Unable to start the browser: {error}").into());
                        cx.notify();
                    }
                }
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
        (page_events, events): (async_channel::Sender<PageEvent>, async_channel::Receiver<PageEvent>),
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
                // Until the page reports its history; WebKit without the Navigation API never does.
                can_go_back: !cfg!(windows),
                can_go_forward: !cfg!(windows),
                reported: None,
                zoom: 1.,
                appearance: None,
                defaults: self.browser_defaults(),
                shown: false,
                page_events,
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
                    probe_failure(tab.view.read(cx).raw(), &url, &tab.page_events);
                    tab.status = NavStatus::Success { url, title };
                }
            }
            PageEvent::Title(new_title) => match &mut tab.status {
                NavStatus::Loading { title, .. }
                | NavStatus::Success { title, .. }
                | NavStatus::LoadFailed { title, .. } => *title = new_title,
                NavStatus::Idle => {}
            },
            // Same-document navigation (pushState, fragments) has no load events: read the URL.
            PageEvent::History { back, forward } => {
                (tab.can_go_back, tab.can_go_forward) = (back, forward);
                if let Ok(current) = tab.view.read(cx).raw().url()
                    && let NavStatus::Loading { url, .. } | NavStatus::Success { url, .. } = &mut tab.status
                {
                    *url = current;
                }
            }
            PageEvent::Failed { url, code, description } => {
                tab.status = NavStatus::LoadFailed { url, title, code, description };
            }
            PageEvent::OpenLink(url) => {
                let thread_id = tab.thread_id.clone();
                self.open_preview_tab(thread_id, Some(url), window, cx);
                return;
            }
        }
        let (thread_id, status) = (tab.thread_id.clone(), tab.status.clone());
        self.record_title(&thread_id, &status);
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

    /// Applies each hosted tab's zoom, colour scheme and visibility to its native view, after the
    /// panel and the mini player recorded what they draw. It only calls into a view on a change.
    pub(super) fn sync_preview_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.retain_server_streams(cx);
        let covered = self.browser_covered(window, cx);
        let panel = self.preview.panel_view.take().filter(|_| !covered);
        let mini = self.preview.mini_view.take().filter(|_| !covered);
        let shown =
            |tab: &str| [&panel, &mini].into_iter().flatten().find(|(id, _)| id == tab).map(|(_, scale)| *scale);
        // A shown tab's page appears once it has a URL to load.
        if let Some(thread_id) = self.preview_thread_id().map(str::to_owned) {
            for (tab_id, _) in [&panel, &mini].into_iter().flatten() {
                if self.preview.tabs.contains_key(tab_id) {
                    continue;
                }
                let url = (self.preview.threads.get(&thread_id).and_then(|p| p.get(tab_id)))
                    .filter(|s| !s.server_hosted())
                    .and_then(|s| s.nav_status.url());
                if let Some(url) = url.map(str::to_owned) {
                    self.host_tab(&thread_id, tab_id, &url, window, cx);
                }
            }
        }
        for (tab_id, tab) in &mut self.preview.tabs {
            let scale = shown(tab_id);
            let session = self.preview.threads.get(&tab.thread_id).and_then(|p| p.get(tab_id));
            let view = tab.view.read(cx);
            if let Some(scale) = scale {
                let zoom = session.and_then(|s| s.zoom_factor).unwrap_or(tab.defaults.0) * f64::from(scale);
                if (tab.zoom - zoom).abs() > 1e-6 {
                    tab.zoom = zoom;
                    let _ = view.set_zoom(zoom);
                }
                let appearance = session.and_then(|s| s.color_scheme).unwrap_or(tab.defaults.1);
                if tab.appearance != Some(appearance) {
                    tab.appearance = Some(appearance);
                    let _ = view.set_theme(match appearance {
                        Appearance::System => wry::Theme::Auto,
                        Appearance::Light => wry::Theme::Light,
                        Appearance::Dark => wry::Theme::Dark,
                    });
                }
            }
            if tab.shown != scale.is_some() {
                tab.shown = scale.is_some();
                let _ = view.set_visible(tab.shown);
            }
        }
    }

    /// The Browser surface: the chrome row over the tab's page.
    pub(super) fn render_browser_surface(
        &mut self,
        thread_id: &str,
        tab_id: &str,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let available =
            (width - 1., f32::from(window.viewport_size().height) - f32::from(TITLEBAR_HEIGHT) - CHROME_ROW);
        v_flex()
            .key_context(super::keymap::PREVIEW_CONTEXT)
            .flex_1()
            .min_h_0()
            .child(self.render_preview_chrome(thread_id, &tokens, window, cx))
            .child(self.render_page_area(thread_id, tab_id, available, window, cx))
            .into_any_element()
    }

    /// The page, the empty state of a blank tab, or a fixed viewport centered in the panel.
    fn render_page_area(
        &mut self,
        thread_id: &str,
        tab_id: &str,
        available: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let session = self.preview.threads.get(thread_id).and_then(|p| p.get(tab_id)).cloned();
        // A failed load shows `PreviewUnreachable` in place of the page (and its native view).
        let status = match self.preview.tabs.get(tab_id) {
            Some(tab) => Some(tab.status.clone()),
            None => session.as_ref().filter(|s| s.server_hosted()).map(|s| s.nav_status.clone()),
        };
        if let Some(page) = status.and_then(|status| self.render_unreachable(tab_id, &status, &tokens, cx)) {
            return div().flex_1().min_h_0().child(page).into_any_element();
        }
        if let Some(session) = session.as_ref().filter(|s| s.server_hosted()) {
            let follow_size = session.viewport.size().is_none();
            return self.render_server_page(thread_id, tab_id, follow_size, &tokens, window, cx);
        }
        let fixed = session.as_ref().and_then(|s| s.viewport.size()).map(|size| preview::fit_viewport(size, available));
        self.preview.panel_view = Some((tab_id.to_owned(), fixed.map_or(1., |(_, scale)| scale)));
        let view = self.preview.tabs.get(tab_id).map(|tab| tab.view.clone());
        let Some(view) = view.filter(|_| !self.preview.menu_open) else {
            let blank = session.as_ref().is_none_or(|s| matches!(s.nav_status, NavStatus::Idle));
            return div()
                .flex_1()
                .min_h_0()
                .when(blank, |el| el.child(self.render_preview_empty_state(thread_id, &tokens, cx)))
                .into_any_element();
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

    /// `PreviewChromeRow`: back, forward, reload, the address bar and the menu.
    fn render_preview_chrome(
        &self,
        thread_id: &str,
        tokens: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let session = self.active_session(thread_id);
        let hosted = session.and_then(|s| self.preview.tabs.get(&s.tab_id));
        // An agent tab navigates over its stream, from the server's own history.
        let server_tab = session.filter(|s| s.server_hosted());
        let (can_back, can_forward) = match server_tab {
            Some(s) => (s.can_go_back, s.can_go_forward),
            None => hosted.map_or((false, false), |tab| (tab.can_go_back, tab.can_go_forward)),
        };
        let loading = match server_tab {
            Some(s) => matches!(s.nav_status, NavStatus::Loading { .. }),
            None => hosted.is_some_and(HostedTab::loading),
        };
        let url = self.active_url();
        let has_page = hosted.is_some() || server_tab.is_some_and(|s| !matches!(s.nav_status, NavStatus::Idle));
        let server_tab = server_tab.map(|s| s.tab_id.clone());
        let (back_tab, forward_tab, reload_tab) = (server_tab.clone(), server_tab.clone(), server_tab);
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
                                el.on_click(cx.listener(move |this, _, _, cx| match &back_tab {
                                    Some(tab) => {
                                        this.send_server_input(tab, json!({"type": "history", "delta": -1}));
                                    }
                                    None => this.with_active_view(cx, |view| {
                                        let _ = view.evaluate_script("history.back()");
                                    }),
                                }))
                            }),
                    )
                    .child(
                        icon_button("preview-forward", IconName::ArrowRight, can_forward, tokens)
                            .tooltip(tooltip("Forward"))
                            .when(can_forward, |el| {
                                el.on_click(cx.listener(move |this, _, _, cx| match &forward_tab {
                                    Some(tab) => {
                                        this.send_server_input(tab, json!({"type": "history", "delta": 1}));
                                    }
                                    None => this.with_active_view(cx, |view| {
                                        let _ = view.evaluate_script("history.forward()");
                                    }),
                                }))
                            }),
                    )
                    .child(
                        icon_button("preview-reload", IconName::RotateCw, has_page, tokens)
                            .tooltip(tooltip(if loading { "Loading…" } else { "Refresh" }))
                            .when(has_page, |el| {
                                el.on_click(cx.listener(move |this, _, _, cx| match &reload_tab {
                                    Some(tab) => {
                                        this.send_server_input(tab, json!({"type": "reload"}));
                                    }
                                    None => this.with_active_view(cx, |view| {
                                        let _ = view.reload();
                                    }),
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
        let server = session.server_hosted();
        let move_label = self.move_tab_label(session);
        let moving = session.clone();
        let (zoom, appearance) = self.tab_look(session);
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
                    if server {
                        this.adjust_preview("preview.adjust", json!({"hardReload": true}), cx);
                    } else {
                        this.with_active_view(cx, hard_reload);
                    }
                    close(this, cx);
                }))
            }))
            .when(!server, |menu| {
                menu.child(item("menu-devtools".into(), "Open DevTools".into(), false, has_page).when(has_page, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        this.with_active_view(cx, |view| view.open_devtools());
                        close(this, cx);
                    }))
                }))
            })
            .children(move_label.map(|label| {
                item("menu-move-tab".into(), label, false, true).on_click(cx.listener(move |this, _, window, cx| {
                    close(this, cx);
                    this.move_preview_tab(moving.clone(), window, cx);
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
            menu = menu.child(separator()).child(
                item("menu-clear-cookies".into(), "Clear cookies".into(), false, has_page).when(has_page, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        this.with_active_view(cx, clear_cookies);
                        close(this, cx);
                    }))
                }),
            );
        }
        div()
            .child(trigger)
            .child(deferred(anchored().anchor(Anchor::TopRight).position(corner).child(menu)).with_priority(2))
    }

    /// `ThreadPreviewMiniPlayer`: the floated tab over the chat's bottom-right corner, unless the
    /// panel shows that same tab (`shouldRenderPreviewMiniPlayer`).
    pub(super) fn render_preview_mini_player(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let thread_id = self.preview_thread_id()?.to_owned();
        let tab_id = self.preview.floating.get(&thread_id)?.clone();
        if self.page.is_some() || self.active_surface() == Some(&Surface::Browser(tab_id.clone())) {
            return None;
        }
        let tokens = *theme::tokens(cx);
        let session = self.preview.threads.get(&thread_id)?.get(&tab_id)?.clone();
        let body = (MINI_SIZE, MINI_SIZE - MINI_HEADER);
        let fixed = session.viewport.size().map(|size| preview::fit_viewport(size, body));
        self.preview.mini_view = Some((tab_id.clone(), fixed.map_or(1., |(_, scale)| scale)));
        let view = self.preview.tabs.get(&tab_id).map(|tab| tab.view.clone());
        let title = self.browser_tab_title(&tab_id);
        let (expand_thread, expand_tab) = (thread_id.clone(), tab_id);
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
                                    this.preview.floating.remove(&expand_thread);
                                    this.open_surface(Surface::Browser(expand_tab.clone()), window, cx);
                                })),
                        )
                        .child(
                            icon_button("mini-close", IconName::X, true, &tokens)
                                .tooltip(tooltip("Close floating preview"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.preview.floating.remove(&close_thread);
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

/// A tab's fallback title: the page's host.
fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_owned)).unwrap_or_else(|| url.to_owned())
}

/// Electron's `reloadIgnoringCache`, as close as wry gets: a plain reload, which revalidates the page.
fn hard_reload(view: &wry::WebView) {
    if let Err(error) = view.reload() {
        log::warn!("could not reload the page: {error}");
    }
}

/// `Network.clearBrowserCookies`: every cookie in the tab's profile.
#[cfg(windows)]
fn clear_cookies(view: &wry::WebView) {
    let cleared = view.cookies().and_then(|cookies| cookies.iter().try_for_each(|cookie| view.delete_cookie(cookie)));
    if let Err(error) = cleared {
        log::warn!("could not clear the cookies: {error}");
    }
}

/// wry reports a failed navigation as a finished one: asks the page whether it is WebView2's
/// error page, and if so reports the load as failed with the error it names.
fn probe_failure(view: &wry::WebView, url: &str, events: &async_channel::Sender<PageEvent>) {
    let (url, events) = (url.to_owned(), events.clone());
    let probed = view.evaluate_script_with_callback(preview::FAILURE_PROBE, move |result| {
        if let Some((code, description)) = preview::failed_load(&result) {
            let _ = events.try_send(PageEvent::Failed { url: url.clone(), code, description: description.into() });
        }
    });
    if let Err(error) = probed {
        log::warn!("could not check the page for a failed load: {error}");
    }
}

/// A tab's view: its page events, and the script that reports its history.
fn page_builder<'a>(
    builder: wry::WebViewBuilder<'a>,
    url: &str,
    sender: async_channel::Sender<PageEvent>,
) -> wry::WebViewBuilder<'a> {
    builder
        .with_url(url)
        .with_devtools(true)
        .with_initialization_script(preview::HISTORY_SCRIPT)
        .with_ipc_handler({
            let sender = sender.clone();
            move |request| {
                if let Some((back, forward)) = preview::history_message(request.body()) {
                    let _ = sender.try_send(PageEvent::History { back, forward });
                }
            }
        })
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
        .with_new_window_req_handler(move |url, _| {
            let _ = sender.try_send(PageEvent::OpenLink(url));
            wry::NewWindowResponse::Deny
        })
}
