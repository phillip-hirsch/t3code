//! The right panel's Browser surface and its floating mini player (`apps/web/src/components/preview/**`).
//!
//! This client hosts a thread's desktop-runtime preview tabs in native WebViews
//! (WebView2 on Windows) and reports their navigation with `preview.reportStatus`,
//! as Electron's `<webview>` host does. Server-runtime tabs, which agents open, run in
//! the server's own Chromium and stream to the panel (`server.rs`).

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::base::actions::{Confirm, SelectDown, SelectUp};
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{
    Disableable as _, Icon, Selectable as _, Side, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use gpui_webview::WebView;
use serde_json::{Value, json};

use super::right_panel::Surface;
use super::titlebar::TITLEBAR_HEIGHT;
use super::{Target, Workspace};
use crate::preview::{self, Appearance, NavStatus, PreviewThreads, Session, TabRef, ThreadPreviews, ThreadRef};
use crate::rpc::RpcClient;
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

/// How long after the right panel first opens [`PreviewState::warm_up`] starts WebView2, so the
/// panel draws before the blocking build.
#[cfg(windows)]
const WARM_UP_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

/// What a native WebView tells the panel, in the order it happened.
pub(super) enum PageEvent {
    Started(String),
    Finished(String),
    Title(String),
    /// The page's history moved, possibly without a load.
    History(preview::PageHistory),
    Failed {
        url: String,
        code: i64,
        description: String,
    },
    /// The page asked for a new window (`window.open`, `target=_blank`).
    OpenLink(String),
}

struct HostedTab {
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
    _events: Task<()>,
}

impl HostedTab {
    fn loading(&self) -> bool {
        matches!(self.status, NavStatus::Loading { .. })
    }
}

pub(super) struct PreviewState {
    threads: PreviewThreads,
    /// Native views, created when a tab first shows a page.
    tabs: HashMap<TabRef, HostedTab>,
    /// Tabs whose native view is being built, with the URL it should end up on.
    starting: HashMap<TabRef, String>,
    /// The tab each thread's mini player floats (`previewMiniPlayerStore`).
    floating: HashMap<ThreadRef, String>,
    /// Agent reveals not yet shown, and whether each is forced.
    reveals: Vec<(TabRef, bool)>,
    /// The tabs drawn this frame, by the panel and the mini player, with their page scale.
    panel_view: Option<(TabRef, f32)>,
    mini_view: Option<(TabRef, f32)>,
    address: Entity<InputState>,
    /// The tab whose URL the address bar shows.
    address_tab: Option<TabRef>,
    pub(super) menu_open: bool,
    /// The address bar's "Open in system browser" button, shown while it has focus.
    external_focus: FocusHandle,
    /// The More menu's Zoom row, which keeps Tab among its buttons.
    zoom_focus: FocusHandle,
    /// Threads whose session list must be (re)fetched.
    stale: HashSet<ThreadRef>,
    reported_profiles: profiles::ReportedProfiles,
    /// Agent tabs' streams.
    server: server::ServerStreams,
    /// Pages opened per project, for a blank tab's "Recently used".
    history: crate::browser_history::BrowserHistory,
    discovery: empty::Discovery,
    /// Failed tabs showing the unreachable page's details.
    unreachable_details: HashSet<TabRef>,
    /// A hidden view that keeps WebView2's browser process running, so tabs and file previews
    /// start without its startup cost.
    #[cfg(windows)]
    _warm: Option<WebView>,
    /// The warm-up has started; it runs once, the first time the right panel opens.
    #[cfg(windows)]
    pub(super) warm_requested: bool,
    _subscription: Subscription,
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
            threads: PreviewThreads::default(),
            tabs: HashMap::new(),
            starting: HashMap::new(),
            floating: HashMap::new(),
            reveals: Vec::new(),
            panel_view: None,
            mini_view: None,
            address,
            address_tab: None,
            menu_open: false,
            external_focus: cx.focus_handle().tab_stop(true),
            zoom_focus: cx.focus_handle(),
            stale: HashSet::new(),
            reported_profiles: profiles::ReportedProfiles::default(),
            server: Default::default(),
            history: crate::browser_history::BrowserHistory::load(),
            discovery: Default::default(),
            unreachable_details: HashSet::new(),
            #[cfg(windows)]
            _warm: None,
            #[cfg(windows)]
            warm_requested: false,
            _subscription: subscription,
        }
    }

    /// Starts WebView2 the first time the right panel opens (Files, file previews and the browser
    /// live there), in a hidden view kept for the session: the view build blocks the UI thread,
    /// and the first one also starts WebView2's browser process (several times longer). Dropping
    /// the view would end that process. Sessions that never open the panel pay nothing.
    #[cfg(windows)]
    pub(super) fn warm_up(window: &mut Window, cx: &mut Context<Workspace>) {
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(WARM_UP_DELAY).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let builder = wry::WebViewBuilder::new()
                    .with_visible(false)
                    // A focused view would take the keyboard focus from the window.
                    .with_focused(false)
                    .with_url("about:blank");
                match build_view(builder, window, cx) {
                    Ok(view) => this.preview._warm = Some(view),
                    Err(error) => log::warn!("could not start WebView2 early: {error}"),
                }
            });
        })
        .detach();
    }
}

/// A 24px `Button variant=ghost size=icon-xs`: a Tab stop that Enter and Space press, ringed while
/// focused. A disabled one leaves the Tab order, like a disabled `<button>`.
fn icon_button(id: impl Into<gpui_kit::ElementId>, icon: IconName, enabled: bool, tokens: &Tokens, cx: &App) -> Button {
    Button::new(id)
        .custom(
            ButtonCustomVariant::new(cx).foreground(tokens.muted_foreground).hover(tokens.accent).active(tokens.accent),
        )
        .xsmall()
        .size(px(24.))
        .rounded(px(6.))
        .icon(Icon::new(icon).size(px(14.)))
        .disabled(!enabled)
}

/// The menu's Zoom row (`closeOnClick={false}`): its buttons step the active tab's zoom, read
/// live so the label follows, without closing the menu. Tab moves from the menu into them and
/// cycles there; Enter and Space press them, and the arrow keys go back to the menu's items.
fn zoom_row(view: &Entity<Workspace>, menu: &FocusHandle, window: &mut Window, cx: &mut App) -> AnyElement {
    let tokens = *theme::tokens(cx);
    let workspace = view.read(cx);
    let zoom = workspace.active_zoom();
    let trap = workspace.preview.zoom_focus.clone();
    let apply = |this: &mut Workspace, zoom: fn(f64) -> f64, cx: &mut Context<Workspace>| {
        let zoom = zoom(this.active_zoom());
        this.adjust_preview("preview.adjust", json!({"zoomFactor": zoom}), cx);
    };
    let wire = |button: Button, label: &'static str, zoom: fn(f64) -> f64| {
        button
            .tooltip(label)
            .on_click(window.listener_for(view, move |this, _: &ClickEvent, _, cx| apply(this, zoom, cx)))
            // The menu binds Enter to picking its highlighted item.
            .on_action(window.listener_for(view, move |this, _: &Confirm, _, cx| apply(this, zoom, cx)))
    };
    let step = |id: &'static str, icon: IconName| {
        Button::new(id).outline().xsmall().size(px(24.)).rounded(px(6.)).icon(Icon::new(icon).size(px(14.)))
    };
    let zoom_out = wire(step("menu-zoom-out", IconName::Minus), "Zoom out", |zoom| preview::step_zoom(zoom, -1));
    let zoom_in = wire(step("menu-zoom-in", IconName::Plus), "Zoom in", |zoom| preview::step_zoom(zoom, 1));
    let reset = wire(icon_button("menu-zoom-reset", IconName::RotateCcw, true, &tokens, cx), "Reset zoom", |_| 1.);
    let (up_menu, down_menu) = (menu.clone(), menu.clone());
    h_flex()
        .id("menu-zoom")
        .flex_1()
        .justify_between()
        .on_click(|_, _, cx| cx.stop_propagation())
        .on_action(move |_: &SelectUp, window, cx| {
            window.focus(&up_menu, cx);
            cx.propagate();
        })
        .on_action(move |_: &SelectDown, window, cx| {
            window.focus(&down_menu, cx);
            cx.propagate();
        })
        .child(div().text_color(tokens.foreground).child("Zoom"))
        .child(
            h_flex()
                .gap_1()
                .child(zoom_out)
                .child(
                    div()
                        .min_w(px(48.))
                        .text_center()
                        .text_xs()
                        .text_color(tokens.muted_foreground)
                        .child(format!("{}%", (zoom * 100.).round())),
                )
                .child(zoom_in)
                .child(reset),
        )
        .focus_trap("menu-zoom-trap", &trap)
        .into_any_element()
}

impl Workspace {
    pub(in crate::app) fn preview_thread_id(&self) -> Option<&str> {
        match self.target.as_ref()? {
            Target::Thread { id, .. } => Some(id),
            Target::Draft { .. } => None,
        }
    }

    /// The open thread on its environment: the one the panel's browser and the address bar act on.
    pub(in crate::app) fn preview_thread(&self) -> Option<ThreadRef> {
        match self.target.as_ref()? {
            Target::Thread { env, id } => Some(ThreadRef::new(env.clone(), id.clone())),
            Target::Draft { .. } => None,
        }
    }

    /// `thread_id` on its environment: the open thread's, else the one the sidebar lists it on.
    pub(in crate::app) fn preview_thread_of(&self, thread_id: &str) -> Option<ThreadRef> {
        self.preview_thread()
            .filter(|thread| thread.thread_id == thread_id)
            .or_else(|| Some(ThreadRef::new(self.thread_environment(thread_id)?, thread_id)))
    }

    /// The open thread's tab `tab_id`.
    fn preview_tab(&self, tab_id: &str) -> Option<TabRef> {
        Some(self.preview_thread()?.tab(tab_id))
    }

    /// The tab the chrome, the menu and the shortcuts act on.
    fn active_tab(&self) -> Option<TabRef> {
        let thread = self.preview_thread()?;
        let tab_id = self.active_session(&thread)?.tab_id.clone();
        Some(thread.tab(tab_id))
    }

    /// Tabs this client draws: its own pages and the agents' streamed ones.
    fn hosted_sessions(previews: &ThreadPreviews) -> impl Iterator<Item = &Session> {
        previews.sessions.iter()
    }

    /// The tab the panel shows, else the thread's focused tab, or its last drawable one when an
    /// agent focused a server tab. Another client may move the focus without changing the panel.
    fn active_session(&self, thread: &ThreadRef) -> Option<&Session> {
        let previews = self.preview.threads.get(thread)?;
        let shown = match self.active_surface() {
            Some(Surface::Browser(tab)) => previews.get(tab),
            _ => None,
        };
        shown.or_else(|| previews.active()).or_else(|| Self::hosted_sessions(previews).last())
    }

    /// A tab's zoom and page colour scheme: its own, else the defaults it was created with.
    fn tab_look(&self, tab: &TabRef, session: &Session) -> (f64, Appearance) {
        let defaults = self.preview.tabs.get(tab).map_or_else(|| self.browser_defaults(), |tab| tab.defaults);
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
        match self.preview_thread().filter(|_| in_app) {
            Some(thread) => self.request_preview_tab(thread, Some(url.to_owned()), true, window, cx),
            None => cx.open_url(url),
        }
    }

    /// `browserAvailable`: a thread to host tabs for.
    pub(super) fn browser_available(&self) -> bool {
        self.preview_thread_id().is_some()
    }

    /// A browser tab's label (`surfaceTitle`): the page title, else its host, else "Browser".
    pub(super) fn browser_tab_title(&self, tab_id: &str) -> SharedString {
        let tab = self.preview_tab(tab_id);
        let session = tab.as_ref().and_then(|tab| self.preview.threads.session(tab));
        let hosted = tab.as_ref().and_then(|tab| self.preview.tabs.get(tab));
        let title = hosted.and_then(|tab| tab.status.title()).or_else(|| session.and_then(|s| s.nav_status.title()));
        let url = hosted.and_then(|tab| tab.status.url()).or_else(|| session.and_then(|s| s.nav_status.url()));
        match (title.filter(|title| !title.trim().is_empty()), url) {
            (Some(title), _) => title.to_owned().into(),
            (None, Some(url)) => host_of(url).into(),
            (None, None) => "Browser".into(),
        }
    }

    /// Whether GPUI draws over the browser's area: a native view would cover it on Windows.
    pub(super) fn browser_covered(&self, window: &mut Window, cx: &mut App) -> bool {
        self.page.is_some()
            || self.settings.is_some()
            || self.palette.is_some()
            || self.preview.menu_open
            || self.right_panel.add_menu_open
            || self.right_panel.profiles_menu_open
            || self.tab_menu_open()
            || self.files.menu_open
            || window.has_active_dialog(cx)
    }

    /// Subscribes to the preview events of every thread on one environment.
    pub(super) fn subscribe_previews(
        &mut self,
        environment: &SharedString,
        client: &RpcClient,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        // Lists fetched before a reconnect may have missed events.
        let mut stale: Vec<ThreadRef> = self.preview.threads.on_environment(environment).cloned().collect();
        stale.extend(self.preview_thread().filter(|thread| thread.environment == *environment));
        for thread in stale {
            if let Some(previews) = self.preview.threads.get_mut(&thread) {
                previews.list_loaded = false;
            }
            self.preview.stale.insert(thread);
        }
        self.preview.discovery.reconnected(environment);
        self.preview.reported_profiles.forget(environment);
        // Tabs whose report failed or waited for this connection send it now.
        let unreported: Vec<TabRef> = self
            .preview
            .tabs
            .iter()
            .filter(|(key, tab)| tab.reported.is_none() && key.thread.environment == *environment)
            .map(|(key, _)| key.clone())
            .collect();
        for tab in unreported {
            self.report_status(&tab, cx);
        }
        let stream = client.stream("subscribePreviewEvents", json!({}));
        let environment = environment.clone();
        let task = Self::pump(
            stream,
            window,
            cx,
            move |this, event| this.apply_preview_event(&environment, event),
            super::Workspace::after_preview_change,
        );
        self.after_preview_change(window, cx);
        task
    }

    /// An item of `environment`'s event stream: it changes that environment's thread only.
    fn apply_preview_event(&mut self, environment: &SharedString, event: &Value) {
        let Some(applied) = self.preview.threads.apply_event(environment, event) else { return };
        let thread = applied.thread;
        self.preview.reveals.extend(applied.reveals.into_iter().map(|(tab, force)| (thread.tab(tab), force)));
        if applied.needs_list {
            self.preview.stale.insert(thread);
        }
    }

    /// Opening a thread fetches its tabs once.
    pub(super) fn load_previews(&mut self, thread: ThreadRef, window: &mut Window, cx: &mut Context<Self>) {
        if !self.preview.threads.get(&thread).is_some_and(|previews| previews.list_loaded) {
            self.preview.stale.insert(thread);
        }
        self.after_preview_change(window, cx);
    }

    /// Refetches stale lists, drops views whose tab closed and refreshes the address bar.
    fn after_preview_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for thread in std::mem::take(&mut self.preview.stale) {
            // A thread whose environment is offline refetches once it reconnects.
            let Some(client) = self.env_client(&thread.environment) else {
                self.preview.stale.insert(thread);
                continue;
            };
            cx.spawn_in(window, async move |this, cx| {
                let result = client.request("preview.list", json!({"threadId": thread.thread_id})).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    match result {
                        Ok(list) => {
                            let fresh = this.preview.threads.apply_list(&thread, &list);
                            this.preview.reveals.extend(fresh.into_iter().map(|(tab, force)| (thread.tab(tab), force)));
                        }
                        Err(error) => log::warn!("preview.list failed: {error}"),
                    }
                    this.after_preview_change(window, cx);
                });
            })
            .detach();
        }
        self.show_agent_reveals();
        let threads = &self.preview.threads;
        let open = |tab: &TabRef| threads.get(&tab.thread).is_none_or(|p| p.get(&tab.tab_id).is_some());
        self.preview.tabs.retain(|tab, _| open(tab));
        self.preview.floating.retain(|thread, tab| threads.get(thread).is_none_or(|p| p.get(tab).is_some()));
        let loaded: Vec<(ThreadRef, Vec<String>)> = threads
            .iter()
            .filter(|(_, previews)| previews.list_loaded)
            .map(|(thread, previews)| {
                (thread.clone(), Self::hosted_sessions(previews).map(|s| s.tab_id.clone()).collect())
            })
            .collect();
        for (thread, tabs) in &loaded {
            self.reconcile_browser_surfaces(thread, &tabs.iter().map(String::as_str).collect::<Vec<_>>());
        }
        self.sync_address(window, cx);
        cx.notify();
    }

    /// `revealServerTabs`: a revealed tab floats over the chat while the
    /// `browserAutoShowFloatingPreview` setting is on; a forced reveal also focuses its panel tab.
    /// Runs before the panel gains tabs for new sessions, so a new tab floats first.
    fn show_agent_reveals(&mut self) {
        let auto_show = self.client_settings.values.bool("browserAutoShowFloatingPreview");
        for (tab, force) in std::mem::take(&mut self.preview.reveals) {
            if !auto_show && !force {
                continue;
            }
            let supported = self
                .session(&tab.thread.environment)
                .is_some_and(|session| session.server_info.capability("serverBrowser"));
            if !supported {
                continue;
            }
            if !self.reveal_browser_surface(&tab.thread, &tab.tab_id, force) {
                self.preview.floating.insert(tab.thread, tab.tab_id);
            }
        }
    }

    /// The URL the active tab shows, as its address bar reads when not being edited.
    fn active_url(&self) -> String {
        let Some(tab) = self.active_tab() else { return String::new() };
        match (self.preview.tabs.get(&tab), self.preview.threads.session(&tab)) {
            (Some(hosted), _) => hosted.status.url().unwrap_or_default().to_owned(),
            (None, Some(session)) => preview::display_url(session).to_owned(),
            (None, None) => String::new(),
        }
    }

    /// Shows the active tab's URL, unless the user is editing the address of that same tab.
    fn sync_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let address = self.preview.address.clone();
        let tab = self.active_tab();
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
        let Some(tab) = self.active_tab() else { return };
        let Some(session) = self.preview.threads.session(&tab) else { return };
        let (zoom, _) = self.tab_look(&tab, session);
        let zoom_to = |direction| json!({"zoomFactor": preview::step_zoom(zoom, direction)});
        match command {
            "preview.refresh" => self.refresh_preview_tab(&tab, cx),
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
        let Some(thread) = self.preview_thread() else { return };
        if matches!(self.active_surface(), Some(Surface::Browser(_))) {
            return self.close_right_panel(cx);
        }
        match self.active_session(&thread).map(|s| s.tab_id.clone()) {
            Some(tab) => self.open_surface(Surface::Browser(tab), window, cx),
            None => self.open_preview_tab(thread, None, window, cx),
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
        if let Some(thread) = self.preview_thread() {
            self.open_browser_tab(thread, None, false, profile, window, cx);
        }
    }

    /// The profiles a new tab can open in (`useBrowserDefaults().profiles`).
    pub(super) fn browser_profiles(&self) -> Vec<preview::Profile> {
        preview::resolve_profiles(&preview::user_profiles(&self.client_settings.values))
    }

    /// Hiding the panel floats the tab it showed over the chat.
    pub(super) fn float_browser(&mut self, thread: ThreadRef, tab_id: String, cx: &mut Context<Self>) {
        self.preview.floating.insert(thread, tab_id);
        cx.notify();
    }

    /// `openPreviewSession`: a new tab in the panel, optionally loading `url`. Project
    /// scripts with `autoOpenPreview` open their `previewUrl` through this.
    pub(super) fn open_preview_tab(
        &mut self,
        thread: ThreadRef,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.request_preview_tab(thread, url, false, window, cx);
    }

    /// `preview.open` with the default viewport; `external_fallback` opens `url` in the system
    /// browser when the tab can't be opened, as `useOpenLink` does.
    pub(in crate::app) fn request_preview_tab(
        &mut self,
        thread: ThreadRef,
        url: Option<String>,
        external_fallback: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_browser_tab(thread, url, external_fallback, None, window, cx);
    }

    fn open_browser_tab(
        &mut self,
        thread: ThreadRef,
        url: Option<String>,
        external_fallback: bool,
        profile: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.env_client(&thread.environment) else { return };
        let profile = profile.unwrap_or_else(|| preview::default_profile_id(&self.client_settings.values));
        let mut input = json!({"threadId": thread.thread_id, "profileId": profile});
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
                            this.record_visit(&thread, url);
                        }
                        this.preview.threads.entry(&thread).apply_snapshot(session, true);
                        // A tab for a thread the user has since left joins that thread's panel.
                        if this.preview_thread().as_ref() == Some(&thread) {
                            this.open_surface(Surface::Browser(tab), window, cx);
                            if url.is_none() {
                                this.preview.address.update(cx, |input, cx| input.focus(window, cx));
                            }
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
        thread: &ThreadRef,
        tab_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(previews) = self.preview.threads.get_mut(thread).filter(|p| p.active_tab.as_deref() != Some(tab_id))
        {
            previews.active_tab = Some(tab_id.to_owned());
            self.sync_address(window, cx);
        }
    }

    /// `closePreviewSession`.
    pub(super) fn close_preview_tab(&mut self, tab: TabRef, window: &mut Window, cx: &mut Context<Self>) {
        let closing = self.preview.threads.entry(&tab.thread).begin_close(&tab.tab_id);
        // The page stays alive, hidden, until the server confirms the close.
        let mut hosted = self.preview.tabs.remove(&tab);
        if let Some(hosted) = hosted.as_mut().filter(|hosted| hosted.shown) {
            hosted.shown = false;
            let _ = hosted.view.read(cx).set_visible(false);
        }
        if let Some(client) = self.env_client(&tab.thread.environment) {
            let input = json!({"threadId": tab.thread.thread_id, "tabId": tab.tab_id});
            cx.spawn_in(window, async move |this, cx| {
                let Err(error) = client.request("preview.close", input).await else { return };
                let _ = this.update_in(cx, |this, window, cx| {
                    if let Some(previews) = this.preview.threads.get_mut(&tab.thread) {
                        previews.cancel_close(closing);
                        if previews.get(&tab.tab_id).is_some()
                            && let Some(hosted) = hosted
                        {
                            this.preview.tabs.insert(tab, hosted);
                        }
                    }
                    this.error = Some(format!("Unable to close the browser tab: {error}").into());
                    this.after_preview_change(window, cx);
                });
            })
            .detach();
        }
        self.after_preview_change(window, cx);
    }

    fn submit_address(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.preview_thread() else { return };
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
        self.record_visit(&thread, &url);
        self.navigate_preview(thread, url, window, cx);
    }

    /// `navigateToResolvedUrl`: the active tab loads `url`, or a new tab opens on it.
    fn navigate_preview(&mut self, thread: ThreadRef, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((tab, server)) = self.active_session(&thread).map(|s| (thread.tab(&s.tab_id), s.server_hosted()))
        else {
            return self.open_preview_tab(thread, Some(url), window, cx);
        };
        if server {
            self.send_server_input(&tab, json!({"type": "navigate", "url": url}));
            window.blur(cx);
            return;
        }
        match self.preview.tabs.get(&tab) {
            Some(hosted) => hosted.view.update(cx, |view, _| view.load_url(&url)),
            None => self.host_tab(&tab, &url, window, cx),
        }
        window.blur(cx);
        cx.notify();
    }

    /// Creates the native view for a tab and starts `url` in it, from a task: see [`build_view`].
    fn host_tab(&mut self, tab: &TabRef, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pending) = self.preview.starting.get_mut(tab) {
            *pending = url.to_owned();
            return;
        }
        let session = self.preview.threads.session(tab);
        let partition = profiles::Storage::of(session.and_then(|s| s.profile_id.as_deref()));
        self.preview.starting.insert(tab.clone(), url.to_owned());
        let tab = tab.clone();
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                let url = this.preview.starting.remove(&tab).unwrap_or_default();
                // The tab closed while its view was waiting to start.
                if this.preview.threads.session(&tab).is_none() {
                    return;
                }
                let (sender, events) = async_channel::unbounded();
                let mut context = partition.context();
                let builder = page_builder(partition.builder(context.as_mut()), &url, sender);
                match build_view(builder, window, cx) {
                    Ok(webview) => this.adopt_view(tab, url, webview, events, window, cx),
                    Err(error) => {
                        this.error = Some(format!("Unable to start the browser: {error}").into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn adopt_view(
        &mut self,
        tab: TabRef,
        url: String,
        webview: WebView,
        events: async_channel::Receiver<PageEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Hidden until a frame lays it out, so it never flashes at the window's origin.
        let _ = webview.set_visible(false);
        let view = cx.new(|_| webview);
        let events_tab = tab.clone();
        let pump = cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update_in(cx, |this, window, cx| this.on_page_event(&events_tab, event, window, cx)).is_err() {
                    break;
                }
            }
        });
        self.preview.tabs.insert(
            tab,
            HostedTab {
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
                _events: pump,
            },
        );
        cx.notify();
    }

    fn on_page_event(&mut self, key: &TabRef, event: PageEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.preview.tabs.get_mut(key) else { return };
        let title = tab.status.title().unwrap_or_default().to_owned();
        match event {
            PageEvent::Started(url) => tab.status = NavStatus::Loading { url, title },
            // A failed load also finishes (WebView2, WebKitGTK); only a new load clears the failure.
            PageEvent::Finished(url) => {
                if !matches!(tab.status, NavStatus::LoadFailed { .. }) {
                    tab.status = NavStatus::Success { url, title };
                }
            }
            PageEvent::Title(new_title) => match &mut tab.status {
                NavStatus::Loading { title, .. }
                | NavStatus::Success { title, .. }
                | NavStatus::LoadFailed { title, .. } => *title = new_title,
                NavStatus::Idle => {}
            },
            // Same-document navigation (pushState, fragments) has no load events: take the page's
            // URL, which can't leave the origin the view is on.
            PageEvent::History(history) => {
                (tab.can_go_back, tab.can_go_forward) = (history.back, history.forward);
                let origin = |url: &str| url::Url::parse(url).ok().map(|url| url.origin());
                let current = tab.view.read(cx).raw().url().ok();
                if current.is_some_and(|current| origin(&current) == origin(&history.url))
                    && let NavStatus::Loading { url, .. } | NavStatus::Success { url, .. } = &mut tab.status
                {
                    *url = history.url;
                }
            }
            PageEvent::Failed { url, code, description } => {
                // A first load that fails can leave WKWebView without the URL it tried.
                let url = if url.is_empty() { tab.status.url().unwrap_or_default().to_owned() } else { url };
                tab.status = NavStatus::LoadFailed { url, title, code, description, download: None };
            }
            PageEvent::OpenLink(url) => {
                self.open_preview_tab(key.thread.clone(), Some(url), window, cx);
                return;
            }
        }
        let status = tab.status.clone();
        self.record_title(&key.thread, &status);
        self.report_status(key, cx);
        self.sync_address(window, cx);
        cx.notify();
    }

    /// `preview.reportStatus`, once per change, so the server and agents see this tab's page.
    /// It goes to the tab's own thread's environment: a hidden tab's need not be the open one's.
    /// A failed report is sent again with the tab's next change or its environment's reconnect.
    fn report_status(&mut self, key: &TabRef, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(&key.thread.environment) else { return };
        let Some(tab) = self.preview.tabs.get_mut(key) else { return };
        let state = (tab.status.clone(), tab.can_go_back, tab.can_go_forward);
        if tab.reported.as_ref() == Some(&state) {
            return;
        }
        let input = json!({
            "threadId": key.thread.thread_id,
            "tabId": key.tab_id,
            "navStatus": state.0.to_json(),
            "canGoBack": state.1,
            "canGoForward": state.2,
        });
        tab.reported = Some(state.clone());
        let key = key.clone();
        cx.spawn(async move |this, cx| {
            let Err(error) = client.request("preview.reportStatus", input).await else { return };
            log::warn!("preview.reportStatus failed: {error}");
            let _ = this.update(cx, |this, _| {
                if let Some(tab) = this.preview.tabs.get_mut(&key).filter(|tab| tab.reported == Some(state)) {
                    tab.reported = None;
                }
            });
        })
        .detach();
    }

    /// `preview.adjust` / `preview.resize`; the returned snapshot updates the tab.
    fn adjust_preview(&mut self, method: &'static str, change: Value, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else { return };
        let Some(client) = self.env_client(&tab.thread.environment) else { return };
        let mut input = json!({"threadId": tab.thread.thread_id, "tabId": tab.tab_id});
        if let (Some(input), Value::Object(change)) = (input.as_object_mut(), change) {
            input.extend(change);
        }
        cx.spawn(async move |this, cx| {
            let result = client.request(method, input).await;
            let _ = this.update(cx, |this, cx| {
                match result.map_err(|e| e.to_string()).and_then(|v| {
                    serde_json::from_value::<Session>(v).map_err(|e| format!("Unexpected {method} result: {e}"))
                }) {
                    Ok(session) => this.preview.threads.entry(&tab.thread).apply_snapshot(session, false),
                    Err(error) => this.error = Some(format!("Unable to change the browser tab: {error}").into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `handleRefresh`: an agent tab reloads over its stream, this client's own tab in its view.
    pub(super) fn refresh_preview_tab(&self, tab: &TabRef, cx: &App) {
        if self.preview.threads.session(tab).is_some_and(Session::server_hosted) {
            self.send_server_input(tab, json!({"type": "reload"}));
        } else if let Some(hosted) = self.preview.tabs.get(tab) {
            // WebKit keeps no entry for a page that failed to load, so a reload would load the one before.
            let _ = match &hosted.status {
                NavStatus::LoadFailed { url, .. } if !url.is_empty() => hosted.view.read(cx).raw().load_url(url),
                _ => hosted.view.read(cx).raw().reload(),
            };
        }
    }

    fn with_active_view(&self, cx: &App, action: impl FnOnce(&wry::WebView)) {
        if let Some(hosted) = self.active_tab().and_then(|tab| self.preview.tabs.get(&tab)) {
            action(hosted.view.read(cx).raw());
        }
    }

    /// Applies each hosted tab's zoom, colour scheme and visibility to its native view, after the
    /// panel and the mini player recorded what they draw. It only calls into a view on a change.
    pub(super) fn sync_preview_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let covered = self.browser_covered(window, cx);
        self.retain_server_streams(covered, window, cx);
        let panel = self.preview.panel_view.take().filter(|_| !covered);
        let mini = self.preview.mini_view.take().filter(|_| !covered);
        let shown =
            |tab: &TabRef| [&panel, &mini].into_iter().flatten().find(|(id, _)| id == tab).map(|(_, scale)| *scale);
        // A shown tab's page appears once it has a URL to load.
        for (tab, _) in [&panel, &mini].into_iter().flatten() {
            if self.preview.tabs.contains_key(tab) {
                continue;
            }
            let url = self.preview.threads.session(tab).filter(|s| !s.server_hosted()).and_then(|s| s.nav_status.url());
            if let Some(url) = url.map(str::to_owned) {
                self.host_tab(tab, &url, window, cx);
            }
        }
        for (key, tab) in &mut self.preview.tabs {
            let scale = shown(key);
            let session = self.preview.threads.session(key);
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
        thread: &ThreadRef,
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
            .child(self.render_preview_chrome(thread, &tokens, window, cx))
            .child(self.render_page_area(&thread.tab(tab_id), available, window, cx))
            .into_any_element()
    }

    /// The page, the empty state of a blank tab, or a fixed viewport centered in the panel.
    fn render_page_area(
        &mut self,
        tab: &TabRef,
        available: (f32, f32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let session = self.preview.threads.session(tab).cloned();
        let server = session.as_ref().filter(|s| s.server_hosted());
        // An agent tab's stream stays connected under the empty and unreachable states, so a URL
        // picked there and a Reload reach the page.
        if server.is_some() {
            self.keep_server_stream(tab, window, cx);
        }
        // A failed load shows `PreviewUnreachable` in place of the page (and its native view).
        let status = match self.preview.tabs.get(tab) {
            Some(hosted) => Some(hosted.status.clone()),
            None => server.map(|s| s.nav_status.clone()),
        };
        if let Some(page) = status.and_then(|status| self.render_unreachable(tab, &status, &tokens, cx)) {
            return div().flex_1().min_h_0().child(page).into_any_element();
        }
        if let Some(session) = server {
            if matches!(session.nav_status, NavStatus::Idle) {
                let empty = self.render_preview_empty_state(&tab.thread, &tokens, cx);
                return div().flex_1().min_h_0().child(empty).into_any_element();
            }
            let follow_size = session.viewport.size().is_none();
            return self.render_server_page(tab, follow_size, false, &tokens, window, cx);
        }
        let fixed = session.as_ref().and_then(|s| s.viewport.size()).map(|size| preview::fit_viewport(size, available));
        self.preview.panel_view = Some((tab.clone(), fixed.map_or(1., |(_, scale)| scale)));
        let view = self.preview.tabs.get(tab).map(|hosted| hosted.view.clone());
        let Some(view) = view.filter(|_| !self.preview.menu_open) else {
            let blank = session.as_ref().is_none_or(|s| matches!(s.nav_status, NavStatus::Idle));
            return div()
                .flex_1()
                .min_h_0()
                .when(blank, |el| el.child(self.render_preview_empty_state(&tab.thread, &tokens, cx)))
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
        thread: &ThreadRef,
        tokens: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let session = self.active_session(thread);
        let tab = session.map(|s| thread.tab(&s.tab_id));
        let hosted = tab.as_ref().and_then(|tab| self.preview.tabs.get(tab));
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
        let reload_tab = tab.clone();
        let unreachable = match server_tab {
            Some(s) => matches!(s.nav_status, NavStatus::LoadFailed { .. }),
            None => hosted.is_some_and(|tab| matches!(tab.status, NavStatus::LoadFailed { .. })),
        };
        // `onPictureInPicture`: float the tab over the chat, or stop floating it.
        let float = session.map(|s| {
            let floating = self.preview.floating.get(thread) == Some(&s.tab_id);
            (s.tab_id.clone(), floating, !unreachable && (server_tab.is_some() || hosted.is_some()))
        });
        let server_tab = server_tab.map(|s| thread.tab(&s.tab_id));
        let (back_tab, forward_tab) = (server_tab.clone(), server_tab);
        let address_focused = self.preview.address.read(cx).focus_handle(cx).is_focused(window);
        // `opacity-0` until the address bar is hovered or the button has focus, but still a Tab stop.
        let external_focused = self.preview.external_focus.is_focused(window);
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
                    .child(icon_button("preview-back", IconName::ArrowLeft, can_back, tokens, cx).tooltip("Back").when(
                        can_back,
                        |el| {
                            el.on_click(cx.listener(move |this, _, _, cx| match &back_tab {
                                Some(tab) => {
                                    this.send_server_input(tab, json!({"type": "history", "delta": -1}));
                                }
                                None => this.with_active_view(cx, |view| {
                                    let _ = view.evaluate_script("history.back()");
                                }),
                            }))
                        },
                    ))
                    .child(
                        icon_button("preview-forward", IconName::ArrowRight, can_forward, tokens, cx)
                            .tooltip("Forward")
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
                        icon_button("preview-reload", IconName::RotateCw, has_page, tokens, cx)
                            .tooltip(if loading { "Loading…" } else { "Refresh" })
                            .when(has_page, |el| {
                                el.on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(tab) = &reload_tab {
                                        this.refresh_preview_tab(tab, cx);
                                    }
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
                            div()
                                .flex_none()
                                .when(!external_focused, |el| {
                                    el.opacity(0.).group_hover("preview-address", |style| style.opacity(1.))
                                })
                                .child(
                                    icon_button("preview-open-external", IconName::ExternalLink, true, tokens, cx)
                                        .track_focus(&self.preview.external_focus)
                                        .tooltip("Open in system browser")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let url = this.active_url();
                                            if !url.is_empty() {
                                                cx.open_url(&url);
                                            }
                                        })),
                                ),
                        )
                    }),
            )
            .children(float.map(|(tab, floating, enabled)| {
                let label = if floating { "Close floating preview" } else { "Float preview over chat" };
                let thread = thread.clone();
                icon_button("preview-float", IconName::PictureInPicture2, enabled, tokens, cx)
                    // `variant="secondary"` with a primary icon while floating.
                    .when(floating, |el| {
                        el.custom(
                            ButtonCustomVariant::new(cx)
                                .color(tokens.secondary)
                                .foreground(tokens.primary)
                                .hover(tokens.secondary)
                                .active(tokens.secondary),
                        )
                    })
                    .tooltip(label)
                    .when(enabled, |el| {
                        el.on_click(cx.listener(move |this, _, _, cx| {
                            if floating {
                                this.preview.floating.remove(&thread);
                                cx.notify();
                            } else {
                                this.preview.floating.insert(thread.clone(), tab.clone());
                                this.close_right_panel(cx);
                            }
                        }))
                    })
            }))
            .child(self.render_preview_menu(thread, session, has_page, tokens, cx))
            .when(loading, |el| {
                el.child(div().absolute().left_0().right_0().bottom(px(-1.)).h(px(2.)).bg(tokens.primary))
            })
    }

    /// `PreviewMoreMenu`: a `Menu` that arrow keys move through, Enter picks from and Escape
    /// closes, handing focus back to where it was.
    fn render_preview_menu(
        &self,
        thread: &ThreadRef,
        session: Option<&Session>,
        has_page: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let trigger = icon_button("preview-more", IconName::Ellipsis, session.is_some(), tokens, cx)
            .tooltip("More")
            .selected(self.preview.menu_open);
        let Some(session) = session else { return trigger.into_any_element() };
        let server = session.server_hosted();
        let move_label = self.move_tab_label(thread, session);
        let moving = (thread.clone(), session.clone());
        let (_, appearance) = self.tab_look(&thread.tab(&session.tab_id), session);
        let preset = session.viewport.preset_id().map(str::to_owned);
        let fill = session.viewport.size().is_none();
        let view = cx.entity();
        trigger
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, window, cx| {
                let adjust = |method: &'static str, change: Value| {
                    window.listener_for(&view, move |this: &mut Self, _: &ClickEvent, _, cx| {
                        this.adjust_preview(method, change.clone(), cx);
                    })
                };
                let mut menu = menu.min_w(px(240.)).max_h(px(520.)).scrollable(true).check_side(Side::Right).item(
                    PopupMenuItem::new("Hard reload").disabled(!has_page).on_click(window.listener_for(
                        &view,
                        move |this, _: &ClickEvent, _, cx| {
                            if server {
                                this.adjust_preview("preview.adjust", json!({"hardReload": true}), cx);
                            } else {
                                this.with_active_view(cx, hard_reload);
                            }
                        },
                    )),
                );
                if !server {
                    menu = menu.item(PopupMenuItem::new("Open DevTools").disabled(!has_page).on_click(
                        window.listener_for(&view, |this, _: &ClickEvent, _, cx| {
                            this.with_active_view(cx, wry::WebView::open_devtools);
                        }),
                    ));
                }
                if let Some(label) = move_label.clone() {
                    let moving = moving.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(window.listener_for(
                        &view,
                        move |this, _: &ClickEvent, window, cx| {
                            let (thread, session) = moving.clone();
                            this.move_preview_tab(thread, session, window, cx);
                        },
                    )));
                }
                menu = menu.separator().label("Appearance");
                for option in Appearance::ALL {
                    menu = menu.item(
                        PopupMenuItem::new(option.label())
                            .checked(appearance == option)
                            .on_click(adjust("preview.adjust", json!({"colorScheme": option.as_str()}))),
                    );
                }
                let zoom_view = view.clone();
                let menu_focus = menu.focus_handle(cx);
                menu = menu
                    .separator()
                    // Not an item the arrow keys stop on: Enter there would close the menu.
                    .item(
                        PopupMenuItem::element(move |window, cx| zoom_row(&zoom_view, &menu_focus, window, cx))
                            .disabled(true),
                    )
                    .separator()
                    .label("Viewport")
                    .item(
                        PopupMenuItem::new("Fill panel")
                            .checked(fill)
                            .on_click(adjust("preview.resize", json!({"viewport": {"_tag": "fill"}}))),
                    );
                for option in &preview::PRESETS {
                    let label = format!("{} · {} × {}", option.label, option.width, option.height);
                    menu =
                        menu.item(PopupMenuItem::new(label).checked(preset.as_deref() == Some(option.id)).on_click(
                            adjust("preview.resize", json!({"viewport": preview::Viewport::preset(option)})),
                        ));
                }
                // The server's browser clears the tab's profile itself. wry clears a profile's cache
                // only with all its other data, so this computer's tabs offer cookies alone.
                if server {
                    menu = menu
                        .separator()
                        .item(
                            PopupMenuItem::new("Clear cookies")
                                .on_click(adjust("preview.adjust", json!({"clear": "cookies"}))),
                        )
                        .item(
                            PopupMenuItem::new("Clear cache")
                                .on_click(adjust("preview.adjust", json!({"clear": "cache"}))),
                        );
                }
                #[cfg(windows)]
                if !server {
                    menu = menu.separator().item(PopupMenuItem::new("Clear cookies").disabled(!has_page).on_click(
                        window.listener_for(&view, |_, _: &ClickEvent, window, cx| {
                            // Reading the cookies runs WebView2's message loop: a task, as `build_view` needs.
                            cx.spawn_in(window, async move |this, cx| {
                                let _ = this.update(cx, |this, cx| this.with_active_view(cx, clear_cookies));
                            })
                            .detach();
                        }),
                    ));
                }
                menu
            })
            .open(self.preview.menu_open)
            .on_open_change(cx.listener(|this, open: &bool, _, cx| {
                this.preview.menu_open = *open;
                cx.notify();
            }))
            .into_any_element()
    }

    /// The active tab's zoom factor.
    fn active_zoom(&self) -> f64 {
        let Some(tab) = self.active_tab() else { return 1. };
        self.preview.threads.session(&tab).map_or(1., |session| self.tab_look(&tab, session).0)
    }

    /// `ThreadPreviewMiniPlayer`: the floated tab over the chat's bottom-right corner, unless the
    /// panel shows that same tab (`shouldRenderPreviewMiniPlayer`).
    pub(super) fn render_preview_mini_player(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let thread = self.preview_thread()?;
        let tab = thread.tab(self.preview.floating.get(&thread)?);
        if self.page.is_some() || self.active_surface() == Some(&Surface::Browser(tab.tab_id.clone())) {
            return None;
        }
        let tokens = *theme::tokens(cx);
        let session = self.preview.threads.session(&tab)?.clone();
        let body = (MINI_SIZE, MINI_SIZE - MINI_HEADER);
        let fixed = session.viewport.size().map(|size| preview::fit_viewport(size, body));
        // An agent's tab streams here as in the panel, with its control bar below the page.
        let streamed = session.server_hosted().then(|| self.render_server_page(&tab, false, true, &tokens, window, cx));
        if streamed.is_none() {
            self.preview.mini_view = Some((tab.clone(), fixed.map_or(1., |(_, scale)| scale)));
        }
        let view = self.preview.tabs.get(&tab).map(|hosted| hosted.view.clone());
        let title = self.browser_tab_title(&tab.tab_id);
        let (expand_thread, expand_tab) = (thread.clone(), tab.tab_id);
        let close_thread = thread;
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
                            icon_button("mini-expand", IconName::PanelRight, true, &tokens, cx)
                                .tooltip("Open in panel")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.preview.floating.remove(&expand_thread);
                                    this.open_surface(Surface::Browser(expand_tab.clone()), window, cx);
                                })),
                        )
                        .child(
                            icon_button("mini-close", IconName::X, true, &tokens, cx)
                                .tooltip("Close floating preview")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.preview.floating.remove(&close_thread);
                                    cx.notify();
                                })),
                        ),
                )
                .child(match (streamed, view, fixed) {
                    (Some(page), _, _) => page,
                    (None, Some(view), Some(((width, height), _))) => div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .justify_center()
                        .child(div().w(px(width)).h(px(height)).child(view))
                        .into_any_element(),
                    (None, Some(view), None) => div().flex_1().min_h_0().child(view).into_any_element(),
                    (None, None, _) => div()
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

/// The target whose right panel holds `thread`'s browser tabs.
pub(super) fn panel_target(thread: &ThreadRef) -> Target {
    Target::Thread { env: thread.environment.clone(), id: thread.thread_id.clone() }
}

/// A tab's fallback title: the page's host.
fn host_of(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_owned)).unwrap_or_else(|| url.to_owned())
}

/// Electron's `reloadIgnoringCache`. wry has no cache-bypassing reload, so the page refetches its
/// document and static resources past the HTTP cache, then reloads onto the fresh copies. The
/// refetch is bounded: a stalled server must not stop the reload. Fetches and XHRs are not replayed.
const HARD_RELOAD_SCRIPT: &str = "(()=>{const abort=new AbortController();\
const timer=setTimeout(()=>abort.abort(),5000);\
const kinds=new Set(['script','link','css','img','image','video','audio','track']);\
const urls=new Set([location.href.split('#')[0]]);\
for(const e of performance.getEntriesByType('resource')){if(kinds.has(e.initiatorType))urls.add(e.name);}\
const refetch=(u)=>{try{if(!/^https?:/.test(u))return null;const same=new URL(u).origin===location.origin;\
return fetch(u,{cache:'reload',mode:same?'same-origin':'no-cors',credentials:'include',signal:abort.signal})\
.then((r)=>r.blob());}catch{return null;}};\
Promise.allSettled([...urls].map(refetch)).finally(()=>{clearTimeout(timer);location.reload();});})()";

fn hard_reload(view: &wry::WebView) {
    if let Err(error) = view.evaluate_script(HARD_RELOAD_SCRIPT) {
        log::warn!("could not hard reload the page: {error}");
        let _ = view.reload();
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

/// Builds a native view in `window` (gpui-webview's safe constructor). Call it from inside a task's
/// update, never from an event handler or `defer`: WebView2's constructor runs the window's message
/// loop, and only while a task runs does GPUI post no other task to that loop, so no task re-enters
/// the borrowed app. The UI still pauses for the build, and window events in that time are dropped.
pub(super) fn build_view(builder: wry::WebViewBuilder, window: &mut Window, cx: &mut App) -> wry::Result<WebView> {
    WebView::build(builder, window, cx)
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
                if let Some(history) = preview::history_message(request.body()) {
                    let _ = sender.try_send(PageEvent::History(history));
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
        // Electron's `did-fail-load`, from each platform's own navigation failure callback.
        .with_on_page_load_error_handler({
            let sender = sender.clone();
            move |error| {
                if let Some((code, description)) = preview::net_error(&error) {
                    let _ =
                        sender.try_send(PageEvent::Failed { url: error.url, code, description: description.into() });
                }
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
