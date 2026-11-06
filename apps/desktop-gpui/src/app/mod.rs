//! The main window: sidebar of projects and threads, the open thread's
//! timeline, pending approvals, and the composer.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, TextareaMode, TextareaState};
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div,
};
use serde_json::{Value, json};

use crate::backend;
use crate::model::{ModelSelection, ProjectShell, ShellState, ThreadState};
use crate::requests::QuestionDraft;
use crate::rpc::{self, ConnectOptions, RpcClient, RpcError, RpcStream, StreamEvent};
use crate::runtime;
use crate::timeline::{self, Row};

mod alerts;
mod attachment_preview;
mod attachments;
mod banners;
mod chat;
mod commands;
mod composer;
mod composer_menu;
mod composer_meter;
mod composer_plan;
mod context_strip;
mod desktop;
mod details;
pub(crate) mod environments;
mod files;
mod find;
mod git;
mod header;
mod host_picker;
mod inspector;
mod keep_alive;
mod keymap;
mod layout;
pub mod main_window;
mod markdown_images;
mod markdown_ui;
pub mod menu;
mod message_actions;
mod minimap;
mod model_picker;
mod pages;
mod palette;
mod palette_actions;
mod plan_actions;
mod preview;
mod prompt_stash;
mod queue;
mod quit;
mod request_cards;
mod right_panel;
mod send;
mod sessions;
mod side_chat;
mod sidebar;
mod snap_shot;
mod subagents;
mod terminal;
mod theme_editor;
mod thread_menu;
mod thread_menu_actions;
mod thread_now;
mod titlebar;
mod visits;
mod workflow;
mod worktree_setup;

gpui_kit::actions!(
    workspace,
    [ToggleSidebar, ToggleThreadDetails, ToggleRightPanel, TogglePalette, NewThread, NewThreadInProject]
);

/// Runs a shell command in the thread's terminal, as a project script does.
#[derive(Clone, PartialEq, Eq, serde::Deserialize, gpui_kit::Action)]
#[action(namespace = workspace, no_json)]
#[expect(clippy::unsafe_derive_deserialize, reason = "the unsafe is in `gpui_kit::Action`'s derive, not this type")]
pub struct RunInTerminal {
    pub cwd: String,
    pub command: String,
}
pub use terminal::key_bindings as terminal_key_bindings;
pub use titlebar::TRAFFIC_LIGHTS;

const RECONNECT_DELAY: Duration = Duration::from_secs(1);

enum Connection {
    Starting,
    Connecting,
    Connected,
    Disconnected(SharedString),
    Failed(SharedString),
}

/// Where the composer sends, on the environment the thread or project lives on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Target {
    Thread {
        env: SharedString,
        id: String,
    },
    /// A new thread in this project, created by the first message.
    Draft {
        env: SharedString,
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
    /// The primary environment's socket; see [`sessions`] for every environment's.
    connection: Connection,
    /// Watches this app's own server for why it stops; `None` while the primary is elsewhere.
    local_server: Option<Task<()>>,
    /// One per connected environment, the primary first.
    sessions: Vec<sessions::EnvironmentSession>,
    /// Every environment's projects and threads.
    shell: ShellState,
    target: Option<Target>,
    thread: Option<OpenThread>,
    rows: Rc<[Row]>,
    /// Open work groups, turn folds and entries in the timeline (`timeline::*_key`).
    expanded: HashSet<SharedString>,
    entry_cache: timeline::EntryCache,
    markdown: chat::MarkdownStates,
    /// Output of the open tool entries.
    details: inspector::Details,
    copy_feedback: message_actions::CopyFeedback,
    /// The minimap tick under the pointer.
    minimap_hover: Option<usize>,
    /// Repaints the "Working for …" timer once a second while a turn runs.
    _working_tick: Option<Task<()>>,
    scroller: Entity<MessageScrollerState>,
    composer: Entity<TextareaState>,
    sending: bool,
    /// Threads whose rewind is still rolling back on the server; they take no sends.
    rewinding: HashSet<String>,
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
    /// Plan mode per target and the dismissed fallback notice.
    composer_ui: composer_plan::ComposerUi,
    attachments: Vec<attachments::Attachment>,
    /// Prompts set aside with `composer.stash`.
    prompt_stash: prompt_stash::PromptStash,
    /// Where Up/Down prompt history stands.
    prompt_history: Option<crate::composer_text::HistoryPosition>,
    /// The editor's chip ids after the last edit, to notice deleted attachment chips.
    chip_ids: Vec<String>,
    /// The composer's Markdown styling, made the first time `composerRichTextEnabled` paints.
    composer_marks: Option<gpui_kit::component::input::TextDecorationCollection<TextareaMode>>,
    /// Ctrl (Cmd) is down, which turns steer into queue.
    ctrl_held: bool,
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
    /// The git status of the open target's workspace.
    vcs: git::VcsWatch,
    /// Local servers, while the details card's Servers section can show.
    servers: details::servers::ServersWatch,
    /// Scheduled tasks, while the details card's Automations section can show.
    automations: details::automations::AutomationsWatch,
    /// The editor "Open in" launches, once picked.
    preferred_editor: Option<String>,
    right_panel: right_panel::RightPanel,
    /// The thread title's menu and inline rename.
    title: thread_menu::TitleState,
    /// The composer's "Run on" picker and each draft's machine choice.
    host: host_picker::HostUi,
    _connection_task: Task<()>,
    /// The launch landing ran (once, after the first shell snapshot).
    landed: bool,
    /// A page covering the chat in the main column.
    page: Option<pages::Page>,
    pull_requests: pages::PullRequestsState,
    usage: pages::UsageState,
    /// The Settings modal's open section.
    settings: Option<SharedString>,
    settings_ui: pages::settings::SettingsUi,
    /// This device's settings; the server's are in `server_info.settings`.
    client_settings: crate::settings::ClientSettings,
    history: pages::History,
    devices: crate::device::Devices,
    /// The server's keybindings `keymap` was merged from; see [`keymap`].
    installed_keybindings: Rc<[crate::keybindings::Rule]>,
    /// The server's rules merged with the defaults, which every keystroke resolves against.
    keymap: Rc<[crate::keybindings::Rule]>,
    /// The keystroke whose command last ran, until a key is released: a repeat of it is held.
    held_key: Option<gpui_kit::Keystroke>,
    reload_toasts: keymap::ReloadToasts,
    /// The keybindings reload the config batch being applied reported, toasted after it.
    pending_reload: Option<keymap::Reload>,
    /// Settings → Keybindings, created the first time it opens.
    keybindings_panel: Option<gpui_kit::AnyView>,
    /// The floating theme editor (`ThemeEditorHost`), over every surface.
    theme_editor: Option<Entity<theme_editor::ThemeEditor>>,
    /// The command palette, while open.
    palette: Option<palette::Palette>,
    /// The thread find bar, while open.
    find: Option<find::ThreadFind>,
    /// Go to file, Search project contents, and the Files and file panel surfaces.
    files: files::FilesState,
    /// Holds focus when nothing inside does, so window shortcuts still reach the workspace.
    focus: gpui_kit::FocusHandle,
    /// Settings > Scheduled tasks, kept while the window lives; it unsubscribes while hidden.
    scheduled_tasks_view: Option<gpui_kit::AnyView>,
    preview: preview::PreviewState,
    /// Each checkout's `t3.json`, keyed by workspace root, read on demand for project actions.
    t3_project_files: HashMap<String, crate::model::T3ProjectFile>,
    terminals: terminal::Terminals,
    side_chat: side_chat::SideChats,
    /// Thread notifications: toasts, OS notifications and sounds.
    alerts: alerts::AlertState,
    /// The SnapShot capture shortcut and Settings' shortcut recorder.
    snap_shot: snap_shot::SnapShotUi,
    quit: quit::QuitState,
    queue: queue::QueueUi,
    now: thread_now::NowUi,
    markdown_images: markdown_images::ImageCache,
    workflow: workflow::WorkflowUi,
    setup: worktree_setup::SetupWatch,
    chat_menu: Option<markdown_ui::ChatMenu>,
    requests: request_cards::RequestUi,
    subagents: subagents::SubagentUi,
    banners: banners::BannerUi,
    /// Window bounds persistence and other desktop shell state.
    desktop: desktop::DesktopShell,
    /// Running threads synced in the background.
    keep_alive: keep_alive::KeepAlive,
    /// Paired remote environments and their background connections.
    saved_environments: Entity<environments::SavedEnvironments>,
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
        let saved_environments = cx.new(environments::SavedEnvironments::new);
        let subscriptions = vec![
            cx.observe_in(&saved_environments, window, |this, _, window, cx| this.sync_saved_sessions(window, cx)),
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::Change = event {
                    this.on_composer_change(cx);
                }
            }),
            // Caret moves re-read the completion trigger.
            cx.observe(&composer, |this, _, cx| this.sync_trigger(cx)),
            // Ahead of GPUI's bindings, the terminal and text fields: the composer's menus, then the keymap.
            cx.intercept_keystrokes({
                let this = weak;
                move |event, window, cx| {
                    let handled = this.update(cx, |this, cx| this.on_keystroke(event, window, cx));
                    if handled.unwrap_or(false) {
                        cx.stop_propagation();
                    }
                }
            }),
            cx.observe(&scroller, |_, _, cx| cx.notify()),
            cx.observe_window_appearance(window, |this, window, cx| {
                crate::theme::sync(window, cx);
                this.sync_composer_marks(cx);
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.clear_system_alerts(window, cx);
                }
            }),
        ];
        let (desktop, desktop_subscriptions) = desktop::DesktopShell::new(window, cx);
        let mut subscriptions = subscriptions;
        subscriptions.extend(desktop_subscriptions);
        let client_settings = crate::settings::ClientSettings::load();
        cx.set_global(crate::theme::Appearance::from_settings(&client_settings.values));
        crate::theme::set_preference(client_settings.theme_choice(), window, cx);
        let mut workspace = Self {
            connection: Connection::Starting,
            local_server: None,
            sessions: vec![sessions::EnvironmentSession::default()],
            shell: ShellState::default(),
            target: None,
            thread: None,
            rows: Rc::default(),
            expanded: HashSet::new(),
            entry_cache: timeline::EntryCache::default(),
            markdown: chat::MarkdownStates::default(),
            details: inspector::Details::default(),
            copy_feedback: message_actions::CopyFeedback::default(),
            minimap_hover: None,
            _working_tick: None,
            scroller,
            composer,
            sending: false,
            rewinding: HashSet::new(),
            model_override: None,
            runtime_mode_override: None,
            composer_empty: true,
            question: None,
            composer_drafts: HashMap::new(),
            responding: None,
            menu: Default::default(),
            picker: Default::default(),
            draft_workspace: Default::default(),
            composer_ui: Default::default(),
            attachments: Vec::new(),
            prompt_stash: Default::default(),
            prompt_history: None,
            chip_ids: Vec::new(),
            composer_marks: None,
            ctrl_held: false,
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
            vcs: git::VcsWatch::default(),
            servers: Default::default(),
            automations: Default::default(),
            preferred_editor: None,
            right_panel: right_panel::RightPanel::new(cx),
            title: thread_menu::TitleState::default(),
            host: Default::default(),
            _connection_task: Self::connect(window, cx),
            landed: false,
            page: None,
            pull_requests: pages::PullRequestsState::new(window, cx),
            usage: pages::UsageState::default(),
            settings: None,
            settings_ui: pages::settings::SettingsUi::new(window, cx),
            client_settings,
            history: pages::History::default(),
            devices: Default::default(),
            installed_keybindings: Rc::default(),
            keymap: Rc::from(crate::keybindings::merge_with_defaults(&[])),
            held_key: None,
            reload_toasts: Default::default(),
            pending_reload: None,
            keybindings_panel: None,
            theme_editor: None,
            palette: None,
            find: None,
            files: files::FilesState::default(),
            focus: cx.focus_handle(),
            scheduled_tasks_view: None,
            preview: preview::PreviewState::new(window, cx),
            t3_project_files: HashMap::new(),
            terminals: terminal::Terminals::default(),
            side_chat: Default::default(),
            alerts: Default::default(),
            snap_shot: Default::default(),
            quit: Default::default(),
            queue: Default::default(),
            now: Default::default(),
            markdown_images: Default::default(),
            workflow: Default::default(),
            setup: Default::default(),
            chat_menu: None,
            requests: Default::default(),
            subagents: Default::default(),
            banners: Default::default(),
            desktop,
            keep_alive: Default::default(),
            saved_environments,
            _subscriptions: subscriptions,
        };
        workspace.sync_snap_shot(window, cx);
        workspace.sync_saved_sessions(window, cx);
        workspace
    }

    /// Waits for the app's backend ([`crate::environments::PrimaryBackend`]), then keeps one
    /// socket open, reconnecting after drops.
    fn connect(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        let backend = cx.global::<crate::environments::PrimaryBackend>().clone();
        cx.spawn_in(window, async move |this, cx| {
            let endpoint = match backend.ready().await {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    log::error!("could not reach the server: {error}");
                    let _ = this.update(cx, |this, cx| this.set_connection(Connection::Failed(error.into()), cx));
                    return;
                }
            };
            if endpoint.renewable() {
                let _ = this.update(cx, |this, cx| this.watch_server(backend, cx));
            }
            let mut endpoint = endpoint;
            // Set by a renewal, so a session the server rejects again waits a beat before the next one.
            let mut renewed = false;
            loop {
                if this.update(cx, |this, cx| this.set_connection(Connection::Connecting, cx)).is_err() {
                    return;
                }
                // The environment's id keys its threads, so it is read before the socket opens.
                let known = this.update(cx, |this, _| this.primary_environment().is_some()).unwrap_or(true);
                if !known {
                    let http_base = endpoint.http_base.clone();
                    let descriptor =
                        runtime().spawn(async move { crate::environments::fetch_descriptor(&http_base).await }).await;
                    let failure: Option<SharedString> = match descriptor {
                        Ok(Ok(descriptor)) => {
                            let id = SharedString::from(descriptor.environment_id);
                            if this.update(cx, |this, cx| this.set_primary_environment(id, cx)).is_err() {
                                return;
                            }
                            None
                        }
                        Ok(Err(failure)) => Some(failure.detail.into()),
                        Err(error) => Some(error.to_string().into()),
                    };
                    if let Some(failure) = failure {
                        let disconnected = Connection::Disconnected(failure);
                        if this.update(cx, |this, cx| this.set_connection(disconnected, cx)).is_err() {
                            return;
                        }
                        cx.background_executor().timer(RECONNECT_DELAY).await;
                        continue;
                    }
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
                        let current = endpoint.clone();
                        let remote = !current.renewable();
                        if this
                            .update_in(cx, |this, window, cx| {
                                this.environment_remote = remote;
                                this.on_connected(client.clone(), current, window, cx);
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

    fn watch_server(&mut self, backend: crate::environments::PrimaryBackend, cx: &mut Context<Self>) {
        self.local_server = Some(cx.spawn(async move |this, cx| {
            let reason = backend.stopped().await;
            let _ = this.update(cx, |this, cx| this.set_connection(Connection::Failed(reason.into()), cx));
        }));
    }

    fn set_connection(&mut self, connection: Connection, cx: &mut Context<Self>) {
        // A stopped server is final; a late socket close must not hide why.
        if matches!(self.connection, Connection::Failed(_)) && !matches!(connection, Connection::Connected) {
            return;
        }
        if !matches!(connection, Connection::Connected) {
            self.set_primary_connection(None, None);
            if self.target_on_primary() {
                self.end_target_streams();
            }
        }
        self.connection = connection;
        self.sync_connection_banner(cx);
        self.notify_sidebar(cx);
        cx.notify();
    }

    fn on_connected(
        &mut self,
        client: RpcClient,
        endpoint: backend::Endpoint,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connection = Connection::Connected;
        self.set_primary_connection(Some(client), Some(endpoint));
        // Read per connection: the files may have changed, or never loaded while down.
        self.t3_project_files.clear();
        // A page opened while the socket was down reads now.
        self.page_opened(cx);
        if let Some(primary) = self.primary_environment().cloned() {
            self.start_session(primary, window, cx);
        }
        // Rows that come back unchanged would not resync these.
        self.sync_worktree_setup(cx);
        self.sync_details(cx);
        cx.notify();
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
                                if let Err(error) = result
                                    && !matches!(error, RpcError::Disconnected(_))
                                {
                                    this.error = Some(error.to_string().into());
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
                this.refresh_thread_find(cx);
                this.sync_question(window, cx);
                this.sync_queue_edit(window, cx);
                this.observe_proactive_panels(cx);
            },
        ));
    }

    /// Electron always opens the index route, which starts a draft in the most
    /// recently active project (`IndexDraftLanding`); this is that landing.
    fn land(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let primary_sequence = self.primary_environment().map_or(0, |id| self.shell.sequence(id));
        if self.landed || primary_sequence == 0 {
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
        let mut new_rows = match self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) {
            Some(projection) => timeline::rows(projection, &self.expanded, &mut self.entry_cache),
            None => Vec::new(),
        };
        self.apply_workflow_opening(&mut new_rows, cx);
        self.sync_worktree_setup(cx);
        self.insert_worktree_setup(&mut new_rows);
        new_rows.extend(self.now_rows(cx));
        new_rows.extend(self.queue_rows());
        let was_empty = self.rows.is_empty();
        self.sync_working_tick(&new_rows, cx);
        self.sync_subagent_tick(cx);
        self.sync_child_approvals(cx);
        let Some((range, count)) = timeline::changed_range(&self.rows, &new_rows) else { return };
        let total = new_rows.len();
        self.rows = Rc::from(new_rows);
        self.sync_details(cx);
        self.prune_requests();
        self.scroller.update(cx, |scroller, cx| {
            if was_empty {
                scroller.reset(total, cx);
            } else {
                scroller.splice(range, count, cx);
            }
        });
    }

    /// Opens a listed thread on the environment it lives on.
    fn select_thread(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = self.thread_environment(&thread_id) else { return };
        self.select_thread_in(env, thread_id, window, cx);
    }

    fn select_thread_in(&mut self, env: SharedString, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let target = Target::Thread { env, id: thread_id.clone() };
        if self.target.as_ref() == Some(&target) {
            if self.page.take().is_some() {
                self.record_location();
                cx.notify();
            }
            return;
        }
        self.page = None;
        self.stash_composer(cx);
        let leaving = self.target.replace(target);
        if let (Some(old), Some(leaving)) = (self.thread.take(), leaving) {
            self.retire_thread(leaving.environment(), old.id, old.state, cx);
        }
        let state = self.take_warm_thread(&thread_id).unwrap_or_default();
        let resumed = state.projection.is_some();
        self.thread = Some(OpenThread {
            id: thread_id.clone(),
            state,
            snapshot_failed: false,
            retried: false,
            _subscription: None,
        });
        self.model_override = None;
        self.restore_composer(window, cx);
        self.reset_timeline(cx);
        self.subscribe_thread(window, cx);
        if resumed {
            self.refresh_rows(cx);
        }
        self.load_previews(&thread_id, window, cx);
        self.open_visit(&thread_id, cx);
        self.focus_composer(window, cx);
        self.notify_sidebar(cx);
        cx.notify();
        self.record_location();
    }

    fn start_draft(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = self.project_environment(&project_id) else { return };
        self.start_draft_in(env, project_id, window, cx);
    }

    /// A project's draft on `env`, for a project the shell may not list yet.
    fn start_draft_in(&mut self, env: SharedString, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.page = None;
        self.stash_composer(cx);
        let leaving = self.target.replace(Target::Draft { env, project_id });
        if let (Some(old), Some(leaving)) = (self.thread.take(), leaving) {
            self.retire_thread(leaving.environment(), old.id, old.state, cx);
        }
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
        self.find = None;
        self.expanded.clear();
        self.entry_cache = timeline::EntryCache::default();
        self.markdown = chat::MarkdownStates::default();
        self.markdown_images = markdown_images::ImageCache::default();
        self.details = inspector::Details::default();
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
            Some(Target::Thread { .. }) => {
                self.thread.as_ref()?.state.projection.as_ref()?.thread.model_selection.clone()
            }
            Some(Target::Draft { project_id, .. }) => {
                let default =
                    self.server_info().new_thread_model(self.shell.projects.iter().find(|p| &p.id == project_id))?;
                Some(self.draft_usage_fallback(&default).map_or(default, |fallback| fallback.selection))
            }
            None => None,
        }
    }

    /// The composer model's provider, borrowing the selection where one is stored.
    fn composer_provider(&self) -> Option<&crate::model::Provider> {
        let stored = match (&self.model_override, &self.target) {
            (Some(selection), _) => Some(selection),
            (None, Some(Target::Thread { .. })) => {
                self.thread.as_ref()?.state.projection.as_ref()?.thread.model_selection.as_ref()
            }
            _ => None,
        };
        match stored {
            Some(selection) => self.server_info().provider(&selection.instance_id),
            None => self.server_info().provider(&self.composer_model()?.instance_id),
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else { return };
        let thread_id = projection.thread.id.clone();
        let command = if self.server_info().capability("threadStop") {
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
        self.dispatch_in_order(vec![command], cx);
    }

    /// Dispatches dependent commands one after another, stopping at the first failure.
    fn dispatch_in_order(&mut self, commands: Vec<Value>, cx: &mut Context<Self>) {
        let Some(client) = commands.first().and_then(|command| self.command_client(command)) else { return };
        cx.spawn(async move |this, cx| {
            for command in commands {
                if let Err(error) = client.request("orchestration.dispatchCommand", command).await {
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some(error.to_string().into());
                        cx.notify();
                    });
                    return;
                }
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
        // A modal page covers the composer, so the editor behind it must not keep the keys.
        if window.focused(cx).is_none()
            || (self.page.is_some_and(pages::Page::is_modal) && self.composer.focus_handle(cx).is_focused(window))
        {
            self.focus.focus(window, cx);
        }
        self.sync_vcs(window, cx);
        #[cfg(windows)]
        if !self.preview.warm_requested && self.right_panel_open() {
            self.preview.warm_requested = true;
            preview::PreviewState::warm_up(window, cx);
        }
        self.sync_servers(window, cx);
        self.sync_load_balancing(cx);
        self.sync_automations(window, cx);
        let right_panel = self.render_right_panel(window, cx);
        let mini_player = self.render_preview_mini_player(cx);
        self.sync_preview_views(window, cx);
        self.sync_document_views(window, cx);
        let terminal_drawer = self.render_terminal_drawer(window, cx);
        let theme = cx.theme();
        div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .bg(theme.background)
            .text_color(theme.foreground)
            .text_sm()
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleThreadDetails, window, cx| this.toggle_details(window, cx)))
            .on_action(
                cx.listener(|this, _: &TogglePalette, window, cx| this.open_palette(palette::View::Root, window, cx)),
            )
            .on_action(cx.listener(|this, _: &NewThread, window, cx| this.new_thread(false, window, cx)))
            .on_action(cx.listener(|this, _: &NewThreadInProject, window, cx| this.new_thread(true, window, cx)))
            .on_action(cx.listener(|this, _: &preview::TogglePreview, window, cx| this.toggle_preview(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleRightPanel, window, cx| this.toggle_right_panel(window, cx)))
            .on_action(cx.listener(Self::on_steer_queued))
            .on_action(cx.listener(Self::on_edit_queued))
            .map(|el| self.on_terminal_actions(el, cx))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.quit_key_down(event, cx) {
                    cx.stop_propagation();
                    return;
                }
                // Escape closes the topmost layer: Settings, then the details popover.
                if this.settings.is_some() && event.keystroke.key == "escape" {
                    if this.settings_escape(window, cx) {
                        cx.stop_propagation();
                    }
                    return;
                }
                if this.page.is_some_and(pages::Page::is_modal) && event.keystroke.key == "escape" {
                    this.close_page(cx);
                    cx.stop_propagation();
                    return;
                }
                this.escape_details_popover(event, window, cx);
            }))
            // Escape that nothing else took closes find (`useChatShortcuts` `dismissFind`).
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.find.is_some() && this.palette.is_none() {
                    this.close_thread_find(window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_key_up(cx.listener(|this, event: &gpui_kit::KeyUpEvent, _, cx| {
                this.held_key = None;
                this.quit_key_up(event, cx);
            }))
            .on_modifiers_changed(cx.listener(|this, event: &gpui_kit::ModifiersChangedEvent, _, cx| {
                this.snap_shot_record_modifiers(event, cx);
                this.quit_modifiers_changed(event, cx);
            }))
            .child(
                h_flex()
                    .size_full()
                    .when(self.sidebar_open, |el| el.child(self.render_cached_sidebar()))
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .when(right_panel.is_none(), |el| el.pr(titlebar::RIGHT_RESIZE_INSET))
                            .child(self.render_header(window, cx))
                            .child(
                                div()
                                    .id("chat-canvas")
                                    .group("chat-canvas")
                                    .relative()
                                    .flex_1()
                                    .min_h_0()
                                    .map(|el| {
                                        if let Some(page) = self.render_page(window, cx) {
                                            return el.child(page);
                                        }
                                        match self.target.clone() {
                                            Some(Target::Draft { project_id, .. }) => el
                                                .child(self.render_draft_stage(&project_id, window, cx))
                                                .children(self.render_details_card(window, cx)),
                                            _ => el
                                                .child(
                                                    v_flex()
                                                        .size_full()
                                                        .child(
                                                            // A click on the timeline leaves the editor, as in a
                                                            // browser, so a question card's 1-9 keys reach it.
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
                                                .children(self.render_find_bar(cx))
                                                .children(self.render_details_card(window, cx)),
                                        }
                                    })
                                    .children(mini_player)
                                    .children(self.render_banner_lane(cx))
                                    .on_drop(cx.listener(|this, paths: &gpui_kit::ExternalPaths, window, cx| {
                                        this.drop_paths(paths, window, cx);
                                    }))
                                    .on_drag_move(cx.listener(Self::on_file_drag_move))
                                    .when(self.dragging_files && cx.has_active_drag(), |el| {
                                        el.child(self.render_drop_overlay(crate::theme::tokens(cx)))
                                    }),
                            )
                            .children(terminal_drawer),
                    )
                    .children(
                        right_panel
                            .map(|panel| div().h_full().flex_none().mr(titlebar::RIGHT_RESIZE_INSET).child(panel)),
                    ),
            )
            .child(self.render_titlebar_overlay(window, cx))
            .children(self.render_page_modal(window, cx))
            .children(self.render_settings(window, cx))
            .children(self.theme_editor.clone())
            .children(self.render_palette(window, cx))
            .children(self.render_file_overlay(window, cx))
            .children(self.render_quit_hint(cx))
            .children(titlebar::render_resize_edges(window, cx))
    }
}
