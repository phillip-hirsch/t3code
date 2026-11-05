//! The main window: sidebar of projects and threads, the open thread's
//! timeline, pending approvals, and the composer.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, Styled as _, Subscription, Task, Window, div,
};
use serde_json::{Value, json};

use crate::backend::{self, ServerHandle};
use crate::model::{ModelSelection, ProjectShell, ServerInfo, ShellState, ThreadState};
use crate::requests::QuestionDraft;
use crate::rpc::{self, ConnectOptions, RpcClient, RpcError, RpcStream, StreamEvent};
use crate::runtime;
use crate::timeline::{self, Row};

mod attachments;
mod chat;
mod composer;
mod composer_menu;
mod context_strip;
mod details;
mod header;
mod layout;
mod model_picker;
mod pages;
mod send;
mod sidebar;
mod titlebar;
mod visits;

gpui_kit::actions!(workspace, [ToggleSidebar, ToggleThreadDetails]);

const RECONNECT_DELAY: Duration = Duration::from_secs(1);

enum Connection {
    Starting,
    Connecting,
    Connected(RpcClient),
    Disconnected(SharedString),
    Failed(SharedString),
}

/// Where the composer sends.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Target {
    Thread(String),
    /// A new thread in this project, created by the first message.
    Draft {
        project_id: String,
    },
}

struct OpenThread {
    id: String,
    state: ThreadState,
    /// A snapshot failed to decode in the batch just applied.
    snapshot_failed: bool,
    /// One automatic resubscribe per open; a second failure is shown.
    retried: bool,
    _subscription: Option<Task<()>>,
}

pub struct Workspace {
    connection: Connection,
    server: Option<ServerHandle>,
    server_info: ServerInfo,
    shell: ShellState,
    target: Option<Target>,
    thread: Option<OpenThread>,
    rows: Rc<Vec<Row>>,
    /// Open work groups, turn folds and entries in the timeline (`timeline::*_key`).
    expanded: HashSet<SharedString>,
    entry_cache: timeline::EntryCache,
    /// Repaints the "Working for …" timer once a second while a turn runs.
    _working_tick: Option<Task<()>>,
    scroller: Entity<MessageScrollerState>,
    composer: Entity<TextareaState>,
    sending: bool,
    /// Model picked in the composer; for an open thread it applies to the next message.
    model_override: Option<ModelSelection>,
    /// Runtime mode a new thread launches with; open threads keep theirs on the server.
    runtime_mode_override: Option<String>,
    /// Tracked so typing only re-renders when the send button's state flips.
    composer_empty: bool,
    /// The question card's drafts, for the first pending question.
    question: Option<QuestionDraft>,
    /// Composer text and question drafts of the targets not on screen.
    composer_drafts: HashMap<Target, composer::ComposerDraft>,
    /// The request whose response is in flight.
    responding: Option<String>,
    /// The `@` / `$` / `/` menu.
    menu: composer_menu::MenuState,
    picker: model_picker::PickerState,
    /// A draft's workspace mode, branch and permissions controls.
    draft_workspace: context_strip::DraftWorkspaceState,
    attachments: Vec<attachments::Attachment>,
    /// Where Up/Down prompt history stands.
    prompt_history: Option<crate::composer_text::HistoryPosition>,
    /// The editor's chip ids after the last edit, to notice deleted attachment chips.
    chip_ids: Vec<String>,
    /// Ctrl (Cmd) is down, which turns steer into queue.
    ctrl_held: bool,
    /// The server's HTTP origin, for attachment uploads.
    http_base: Option<String>,
    /// Connected through `T3_SERVER_URL` rather than to this app's own server.
    environment_remote: bool,
    /// Files from the OS are being dragged over the chat canvas.
    dragging_files: bool,
    error: Option<SharedString>,
    sidebar: sidebar::SidebarState,
    sidebar_view: Entity<sidebar::SidebarView>,
    visit: visits::VisitTracker,
    /// Collapsed, the sidebar column is not rendered at all.
    sidebar_open: bool,
    details_open: bool,
    /// The details card as a popover, while the canvas is too narrow for it inline.
    details_popover: bool,
    /// The thread whose "Previous agents" group is expanded in the details card.
    lineage_expanded: Option<String>,
    _connection_task: Task<()>,
    _stream_tasks: Vec<Task<()>>,
    _shell_task: Option<Task<()>>,
    /// Shell twin of [`OpenThread::snapshot_failed`] / [`OpenThread::retried`].
    shell_snapshot_failed: bool,
    shell_retried: bool,
    /// The launch landing ran (once, after the first shell snapshot).
    landed: bool,
    /// A page covering the chat in the main column.
    page: Option<pages::Page>,
    /// The Settings modal's open section.
    settings: Option<SharedString>,
    history: pages::History,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            // The composer paints its own contextual placeholder.
            let mut state =
                TextareaState::new(window, cx).auto_grow(2, composer::EDITOR_MAX_ROWS).submit_on_enter(true);
            state.set_scrollbar_renderer(Some(composer::editor_scrollbar()));
            state
        });
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let weak = cx.weak_entity();
        let subscriptions = vec![
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    this.on_composer_change(cx);
                }
            }),
            // Caret moves re-read the completion trigger.
            cx.observe(&composer, |this, _, cx| this.sync_trigger(cx)),
            // Ahead of key bindings, so the composer's menus and shortcuts see arrows, Tab and Enter.
            cx.intercept_keystrokes({
                let this = weak;
                move |event, window, cx| {
                    let handled = this.update(cx, |this, cx| this.intercept_key(&event.keystroke, window, cx));
                    if handled.unwrap_or(false) {
                        cx.stop_propagation();
                    }
                }
            }),
            cx.observe(&scroller, |_, _, cx| cx.notify()),
            cx.observe_window_appearance(window, |_, window, cx| crate::theme::sync(window, cx)),
        ];
        Self {
            connection: Connection::Starting,
            server: None,
            server_info: ServerInfo::default(),
            shell: ShellState::default(),
            target: None,
            thread: None,
            rows: Rc::default(),
            expanded: HashSet::new(),
            entry_cache: timeline::EntryCache::default(),
            _working_tick: None,
            scroller,
            composer,
            sending: false,
            model_override: None,
            runtime_mode_override: None,
            composer_empty: true,
            question: None,
            composer_drafts: HashMap::new(),
            responding: None,
            menu: Default::default(),
            picker: Default::default(),
            draft_workspace: Default::default(),
            attachments: Vec::new(),
            prompt_history: None,
            chip_ids: Vec::new(),
            ctrl_held: false,
            http_base: None,
            environment_remote: false,
            dragging_files: false,
            error: None,
            sidebar: sidebar::SidebarState::new(window, cx),
            visit: visits::VisitTracker::default(),
            sidebar_view: {
                let workspace = cx.weak_entity();
                cx.new(|_| sidebar::SidebarView(workspace))
            },
            sidebar_open: true,
            details_open: true,
            details_popover: false,
            lineage_expanded: None,
            _connection_task: Self::connect(window, cx),
            _stream_tasks: Vec::new(),
            _shell_task: None,
            shell_snapshot_failed: false,
            shell_retried: false,
            landed: false,
            page: None,
            settings: None,
            history: pages::History::default(),
            _subscriptions: subscriptions,
        }
    }

    /// Starts or finds the server, then keeps one socket open, reconnecting after drops.
    fn connect(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            let resolved = runtime().spawn(backend::resolve_endpoint()).await;
            let endpoint = match resolved {
                Ok(Ok((endpoint, server))) => {
                    log::info!("server ready at {}", endpoint.http_base);
                    let handle = server.map(|server| server.supervise(runtime().handle()));
                    let _ = this.update(cx, |this, cx| {
                        if let Some(handle) = handle {
                            this.watch_server(handle, cx);
                        }
                    });
                    endpoint
                }
                Ok(Err(error)) => {
                    log::error!("could not reach the server: {error:#}");
                    let _ = this.update(cx, |this, cx| {
                        this.set_connection(Connection::Failed(format!("{error:#}").into()), cx)
                    });
                    return;
                }
                Err(error) => {
                    let _ = this
                        .update(cx, |this, cx| this.set_connection(Connection::Failed(error.to_string().into()), cx));
                    return;
                }
            };
            let mut endpoint = endpoint;
            // Set by a renewal, so a session the server rejects again waits a beat before the next one.
            let mut renewed = false;
            loop {
                if this.update(cx, |this, cx| this.set_connection(Connection::Connecting, cx)).is_err() {
                    return;
                }
                let options = ConnectOptions { ws_url: endpoint.ws_url(), bearer_token: endpoint.bearer_token.clone() };
                match RpcClient::connect(runtime().handle(), options).await {
                    Err(error) if rpc::is_unauthorized(&error) && !endpoint.renewable() => {
                        let message = "The server rejected T3_SERVER_TOKEN. Set a valid token and restart.";
                        let _ = this.update(cx, |this, cx| this.set_connection(Connection::Failed(message.into()), cx));
                        return;
                    }
                    Err(error) if rpc::is_unauthorized(&error) && !renewed => {
                        // The session expired or was revoked: exchange this window's bootstrap token again.
                        log::info!("session rejected; renewing it");
                        let message: SharedString = match runtime().spawn(endpoint.clone().renew()).await {
                            Ok(Ok(fresh)) => {
                                endpoint = fresh;
                                renewed = true;
                                continue;
                            }
                            Ok(Err(error)) => format!("{error:#}").into(),
                            Err(error) => error.to_string().into(),
                        };
                        if this
                            .update(cx, |this, cx| this.set_connection(Connection::Disconnected(message), cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(client) => {
                        renewed = false;
                        log::info!("connected");
                        let (http_base, remote) = (endpoint.http_base.clone(), !endpoint.renewable());
                        if this
                            .update_in(cx, |this, window, cx| {
                                this.http_base = Some(http_base);
                                this.environment_remote = remote;
                                this.on_connected(client.clone(), window, cx)
                            })
                            .is_err()
                        {
                            return;
                        }
                        let reason = client.closed().await;
                        if this
                            .update(cx, |this, cx| this.set_connection(Connection::Disconnected(reason.into()), cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(error) => {
                        renewed = false;
                        let message: SharedString = format!("{error:#}").into();
                        if this
                            .update(cx, |this, cx| this.set_connection(Connection::Disconnected(message), cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                cx.background_executor().timer(RECONNECT_DELAY).await;
            }
        })
    }

    fn watch_server(&mut self, mut handle: ServerHandle, cx: &mut Context<Self>) {
        let exited = std::mem::replace(&mut handle.exited, futures::channel::oneshot::channel().1);
        self.server = Some(handle);
        cx.spawn(async move |this, cx| {
            if let Ok(reason) = exited.await {
                log::warn!("{reason}");
                let _ = this.update(cx, |this, cx| this.set_connection(Connection::Failed(reason.into()), cx));
            }
        })
        .detach();
    }

    fn set_connection(&mut self, connection: Connection, cx: &mut Context<Self>) {
        // A stopped server is final; a late socket close must not hide why.
        if matches!(self.connection, Connection::Failed(_)) && !matches!(connection, Connection::Connected(_)) {
            return;
        }
        if !matches!(connection, Connection::Connected(_)) {
            self._stream_tasks.clear();
            self._shell_task = None;
            if let Some(thread) = &mut self.thread {
                thread._subscription = None;
            }
        }
        self.connection = connection;
        self.notify_sidebar(cx);
        cx.notify();
    }

    fn client(&self) -> Option<RpcClient> {
        match &self.connection {
            Connection::Connected(client) => Some(client.clone()),
            _ => None,
        }
    }

    fn on_connected(&mut self, client: RpcClient, window: &mut Window, cx: &mut Context<Self>) {
        self.connection = Connection::Connected(client.clone());
        let config = client.stream("subscribeServerConfig", json!({"workspaceSnapshotsOnDemand": true}));
        self._stream_tasks = vec![Self::pump(
            config,
            window,
            cx,
            |this, value| {
                if let Err(error) = this.server_info.apply(value) {
                    log::warn!("bad server config event: {error}");
                }
            },
            |this, _, cx| {
                this.notify_sidebar(cx);
                // Skills arrive with provider updates.
                this.refresh_menu_items(cx);
            },
        )];
        self.shell_retried = false;
        self.subscribe_shell(window, cx);
        self.subscribe_thread(window, cx);
        cx.notify();
    }

    fn subscribe_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let mut input = json!({"requestCompletionMarker": true});
        if self.shell.sequence > 0 {
            input["afterSequence"] = self.shell.sequence.into();
        }
        let stream = client.stream("orchestration.subscribeShell", input);
        self._shell_task = Some(Self::pump(
            stream,
            window,
            cx,
            |this, value| {
                if let Err(error) = this.shell.apply(value) {
                    log::warn!("bad shell event: {error}");
                    this.shell_snapshot_failed = true;
                }
            },
            |this, window, cx| {
                // The reducer reset its cursor, so one resubscribe fetches a fresh snapshot.
                if std::mem::take(&mut this.shell_snapshot_failed) {
                    if !std::mem::replace(&mut this.shell_retried, true) {
                        this.subscribe_shell(window, cx);
                        return;
                    }
                    this.error =
                        Some("Couldn't read the thread list from the server. Restart T3 Code to retry.".into());
                } else if this.shell.sequence > 0 {
                    this.shell_retried = false;
                }
                this.land(window, cx);
                this.observe_inbox_while_hidden();
                this.notify_sidebar(cx);
                this.track_visit(cx);
            },
        ));
    }

    /// Applies every event already queued on `stream`, then runs `after_batch`
    /// once and re-renders.
    fn pump(
        stream: RpcStream,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Self, &Value) + 'static,
        after_batch: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = stream.next().await {
                let mut events = vec![first];
                events.extend(std::iter::from_fn(|| stream.try_next()));
                let ended = this.update_in(cx, |this, window, cx| {
                    let mut ended = false;
                    let mut applied = false;
                    for event in events {
                        match event {
                            StreamEvent::Item(value) => {
                                apply(this, &value);
                                applied = true;
                            }
                            StreamEvent::End(result) => {
                                ended = true;
                                if let Err(error) = result {
                                    if !matches!(error, RpcError::Disconnected(_)) {
                                        this.error = Some(error.to_string().into());
                                    }
                                }
                            }
                        }
                    }
                    if applied {
                        after_batch(this, window, cx);
                    }
                    cx.notify();
                    ended
                });
                if ended.unwrap_or(true) {
                    break;
                }
            }
        })
    }

    fn subscribe_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(thread) = &mut self.thread else { return };
        let mut input = json!({"threadId": thread.id, "requestCompletionMarker": true});
        if thread.state.sequence > 0 {
            input["afterSequence"] = thread.state.sequence.into();
        }
        let stream = client.stream("orchestration.subscribeThread", input);
        let thread_id = thread.id.clone();
        let batch_thread_id = thread_id.clone();
        thread._subscription = Some(Self::pump(
            stream,
            window,
            cx,
            move |this, value| {
                let Some(thread) = this.thread.as_mut().filter(|t| t.id == thread_id) else { return };
                if let Err(error) = thread.state.apply(value) {
                    log::warn!("bad thread event: {error}");
                    thread.snapshot_failed = true;
                }
            },
            move |this, window, cx| {
                let Some(thread) = this.thread.as_mut().filter(|t| t.id == batch_thread_id) else { return };
                // The reducer reset its cursor, so one resubscribe fetches a fresh snapshot.
                if std::mem::take(&mut thread.snapshot_failed) {
                    if !std::mem::replace(&mut thread.retried, true) {
                        this.subscribe_thread(window, cx);
                        return;
                    }
                    this.error = Some(
                        "Couldn't read this thread from the server. Open another thread and come back to retry.".into(),
                    );
                } else if thread.state.projection.is_some() {
                    thread.retried = false;
                }
                let Some(thread) = this.thread.as_ref() else { return };
                if thread.state.deleted {
                    this.close_thread(window, cx);
                    return;
                }
                this.refresh_rows(cx);
                this.sync_question(window, cx);
            },
        ));
    }

    /// Electron always opens the index route, which starts a draft in the most
    /// recently active project (`IndexDraftLanding`); this is that landing.
    fn land(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.landed || self.shell.sequence == 0 {
            return;
        }
        self.landed = true;
        if self.target.is_some() {
            return;
        }
        if let Some(project_id) =
            crate::inbox::most_recent_project(&self.shell.projects, &self.shell.threads).map(|p| p.id.clone())
        {
            self.start_draft(project_id, window, cx);
        }
    }

    /// Leaves a thread that was deleted, as the web ends its view.
    fn close_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Its draft has nowhere to go: drop it rather than leave it in the editor.
        if let Some(target) = self.target.take() {
            self.composer_drafts.remove(&target);
        }
        self.attachments.clear();
        self.set_composer_text("", window, cx);
        self.thread = None;
        self.model_override = None;
        self.question = None;
        self.reset_timeline(cx);
        self.notify_sidebar(cx);
    }

    /// Rebuilds the timeline and tells the scroller which rows changed.
    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let new_rows = match self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) {
            Some(projection) => timeline::rows(projection, &self.expanded, &mut self.entry_cache),
            None => Vec::new(),
        };
        let was_empty = self.rows.is_empty();
        self.sync_working_tick(&new_rows, cx);
        let Some((range, count)) = timeline::changed_range(&self.rows, &new_rows) else { return };
        let total = new_rows.len();
        self.rows = Rc::new(new_rows);
        self.scroller.update(cx, |scroller, cx| {
            if was_empty {
                scroller.reset(total, cx);
            } else {
                scroller.splice(range, count, cx);
            }
        });
    }

    fn select_thread(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_ref() == Some(&Target::Thread(thread_id.clone())) {
            if self.page.take().is_some() {
                self.record_location();
                cx.notify();
            }
            return;
        }
        self.page = None;
        self.stash_composer(cx);
        self.target = Some(Target::Thread(thread_id.clone()));
        self.thread = Some(OpenThread {
            id: thread_id.clone(),
            state: ThreadState::default(),
            snapshot_failed: false,
            retried: false,
            _subscription: None,
        });
        self.model_override = None;
        self.restore_composer(window, cx);
        self.reset_timeline(cx);
        self.subscribe_thread(window, cx);
        self.open_visit(&thread_id, cx);
        self.focus_composer(window, cx);
        self.notify_sidebar(cx);
        cx.notify();
        self.record_location();
    }

    fn start_draft(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.page = None;
        self.stash_composer(cx);
        self.target = Some(Target::Draft { project_id });
        self.thread = None;
        self.model_override = None;
        self.runtime_mode_override = None;
        self.restore_composer(window, cx);
        self.reset_timeline(cx);
        self.focus_composer(window, cx);
        self.notify_sidebar(cx);
        cx.notify();
        self.record_location();
    }

    fn reset_timeline(&mut self, cx: &mut Context<Self>) {
        self.rows = Rc::default();
        self.expanded.clear();
        self.entry_cache = timeline::EntryCache::default();
        self._working_tick = None;
        self.scroller.update(cx, |scroller, cx| scroller.reset(0, cx));
    }

    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |composer, cx| composer.focus(window, cx));
    }

    /// The model the next message runs on.
    fn composer_model(&self) -> Option<ModelSelection> {
        if let Some(selection) = &self.model_override {
            return Some(selection.clone());
        }
        match &self.target {
            Some(Target::Thread(_)) => self.thread.as_ref()?.state.projection.as_ref()?.thread.model_selection.clone(),
            Some(Target::Draft { project_id }) => {
                self.server_info.new_thread_model(self.shell.projects.iter().find(|p| &p.id == project_id))
            }
            None => None,
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else { return };
        let thread_id = projection.thread.id.clone();
        let command = if self.server_info.thread_stop {
            json!({"type": "thread.stop", "commandId": new_id(), "threadId": thread_id})
        } else {
            let Some(run) = projection.interruptible_run() else { return };
            json!({"type": "run.interrupt", "commandId": new_id(), "threadId": thread_id, "runId": run.id, "holdQueue": true})
        };
        self.dispatch(command, cx);
    }

    /// `runtime-request.respond` for the open thread; `response` holds `decision`, `feedback` or `answers`.
    fn respond_command(&self, request_id: &str, response: Value) -> Option<Value> {
        let thread = self.thread.as_ref()?;
        let mut command = json!({
            "type": "runtime-request.respond",
            "commandId": new_id(),
            "threadId": thread.id,
            "requestId": request_id,
        });
        if let (Some(command), Value::Object(fields)) = (command.as_object_mut(), response) {
            command.extend(fields);
        }
        Some(command)
    }

    /// Answers a runtime request; `response` holds `decision`, `feedback` or `answers`.
    fn respond(&mut self, request_id: String, response: Value, cx: &mut Context<Self>) {
        let (Some(command), Some(client)) = (self.respond_command(&request_id, response), self.client()) else {
            return;
        };
        self.responding = Some(request_id.clone());
        cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update(cx, |this, cx| {
                if this.responding.as_ref() == Some(&request_id) {
                    this.responding = None;
                }
                if let Err(error) = result {
                    this.error = Some(error.to_string().into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn dispatch(&mut self, command: Value, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.request("orchestration.dispatchCommand", command).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error.to_string().into());
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn draft_title(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(60) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

fn project_name(project: &ProjectShell) -> SharedString {
    project
        .repository_identity
        .as_ref()
        .and_then(|identity| identity.display_name.clone())
        .unwrap_or_else(|| project.title.clone())
        .into()
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Widening to the inline layout closes the popover, as the web does.
        if self.details_inline(window) {
            self.details_popover = false;
        }
        let theme = cx.theme();
        div()
            .size_full()
            .relative()
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_sm()
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleThreadDetails, window, cx| this.toggle_details(window, cx)))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                // Escape closes the topmost layer: Settings, then the details popover.
                if this.settings.is_some() && event.keystroke.key == "escape" {
                    this.close_settings(cx);
                    cx.stop_propagation();
                    return;
                }
                this.escape_details_popover(event, window, cx);
            }))
            .child(
                h_flex().size_full().when(self.sidebar_open, |el| el.child(self.render_cached_sidebar())).child(
                    v_flex()
                        .flex_1()
                        .h_full()
                        .min_w_0()
                        .pr(titlebar::RIGHT_RESIZE_INSET)
                        .child(self.render_header(cx))
                        .children(self.render_error(cx))
                        .child(
                            div()
                                .id("chat-canvas")
                                .group("chat-canvas")
                                .relative()
                                .flex_1()
                                .min_h_0()
                                .map(|el| {
                                    match (self.page, self.target.clone()) {
                                        (Some(page), _) => el.child(self.render_page(page, window, cx)),
                                        (None, Some(Target::Draft { project_id })) => el
                                            .child(self.render_draft_stage(&project_id, window, cx))
                                            .children(self.render_details_card(window, cx)),
                                        (None, _) => el
                                            .child(
                                                v_flex()
                                                    .size_full()
                                                    .child(
                                                        // A click on the timeline leaves the editor, as in a browser,
                                                        // so a question card's 1-9 keys reach it.
                                                        v_flex()
                                                            .flex_1()
                                                            .min_h_0()
                                                            .on_mouse_down(
                                                                gpui_kit::MouseButton::Left,
                                                                |_, window, cx| window.blur(cx),
                                                            )
                                                            .child(self.render_body(window, cx)),
                                                    )
                                                    .child(self.render_composer(window, cx)),
                                            )
                                            .children(self.render_details_card(window, cx)),
                                    }
                                })
                                .on_drop(cx.listener(|this, paths: &gpui_kit::ExternalPaths, window, cx| {
                                    this.drop_paths(paths, window, cx)
                                }))
                                .on_drag_move(cx.listener(Self::on_file_drag_move))
                                .when(self.dragging_files && cx.has_active_drag(), |el| {
                                    el.child(self.render_drop_overlay(crate::theme::tokens(cx)))
                                }),
                        ),
                ),
            )
            .child(self.render_titlebar_overlay(window, cx))
            .children(self.render_settings(window, cx))
    }
}
