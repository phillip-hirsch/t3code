//! The main window: sidebar of projects and threads, the open thread's
//! timeline, pending approvals, and the composer.

use std::collections::{HashMap, HashSet};
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::future::{self, Either};
use gpui_kit::component::input::{InputEvent, TextareaMode, TextareaState};
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, AsyncWindowContext, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, WeakEntity, Window, div,
};
use serde_json::{Value, json};

use crate::backend;
use crate::environments::{BackendStatus, PrimaryBackend};
use crate::model::{ModelSelection, ProjectShell, ScopedId, ServerInfo, ShellState, ThreadState};
use crate::rpc::{self, ConnectOptions, RpcClient, RpcError, RpcStream, StreamEvent};
use crate::runtime;
use crate::timeline::{self, Row};

mod add_to_machine;
mod alerts;
mod app_activation;
mod attachment_preview;
mod attachments;
mod banners;
mod changed_files;
mod chat;
mod citations;
mod commands;
mod compact_send;
mod composer;
mod composer_fit;
mod composer_menu;
mod composer_plan;
mod composer_rich;
mod composer_suggestion;
mod confirm;
mod context_records;
mod context_strip;
mod deleted_thread;
mod desktop;
mod details;
mod draft_holder;
mod draft_picker;
mod draft_sessions;
mod draft_store;
mod embed;
pub(crate) mod environments;
mod external_links;
mod fanout;
mod files;
mod find;
mod fresh_session;
mod git;
mod guest_share;
mod header;
mod history_swipe;
mod host_picker;
mod html_page;
mod image_viewer;
mod inspector;
mod keep_alive;
mod keymap;
mod layout;
pub mod main_window;
mod markdown_images;
mod markdown_media;
mod markdown_ui;
mod math;
mod mcp_app;
mod mcp_setup;
pub mod menu;
mod mermaid;
mod message_actions;
mod minimap;
mod model_picker;
mod onboarding;
mod pages;
mod palette;
mod palette_actions;
mod plan_actions;
mod positions;
mod preview;
mod project_clone;
mod prompt_stash;
mod queue;
mod quit;
mod recovery;
mod remote_open;
mod request_cards;
mod review_comments;
mod right_panel;
mod send;
mod sessions;
mod side_chat;
mod sidebar;
mod sidebar_sheet;
mod snap_shot;
mod source_control;
mod ssh_prompt;
mod subagents;
mod terminal;
mod theme_editor;
mod theme_feed;
mod theme_inspect;
mod thread_menu;
mod thread_menu_actions;
mod thread_now;
mod timeline_anchor;
mod titlebar;
mod user_message;
mod visits;
mod workflow;
mod workspace_lookup;
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
    /// The script's `stopOnSettle`.
    pub stop_on_settle: bool,
    /// The script's `previewUrl` when it has `autoOpenPreview`, opened once the command is written.
    pub preview_url: Option<String>,
}

impl RunInTerminal {
    /// A project script run in `cwd`.
    fn script(cwd: String, script: &crate::model::ProjectScript) -> Self {
        let preview_url =
            script.preview_url.clone().filter(|url| script.auto_open_preview == Some(true) && !url.trim().is_empty());
        Self { cwd, command: script.command.clone(), stop_on_settle: script.stop_on_settle == Some(true), preview_url }
    }
}
pub use terminal::key_bindings as terminal_key_bindings;

const RECONNECT_DELAY: Duration = Duration::from_secs(1);

enum Connection {
    Starting,
    Connecting,
    Connected,
    Disconnected(SharedString),
    Failed(SharedString),
    /// The local environment is off, so there is no primary session; saved environments carry
    /// every thread.
    Off,
}

/// Where the composer sends, on the environment the thread or project lives on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Target {
    Thread {
        env: SharedString,
        id: String,
    },
    /// An unsent draft in this project, whose first message creates the thread. Build one with
    /// [`Target::draft`]; `draft` is its identity, which stays when it moves between projects.
    Draft {
        env: SharedString,
        project_id: String,
        draft: draft_sessions::DraftId,
    },
}

impl Target {
    fn draft(env: SharedString, project_id: String, draft: draft_sessions::DraftId) -> Self {
        Self::Draft { env, project_id, draft }
    }

    fn draft_id(&self) -> Option<&draft_sessions::DraftId> {
        match self {
            Self::Draft { draft, .. } => Some(draft),
            Self::Thread { .. } => None,
        }
    }
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
    /// One per connected environment, the primary first.
    sessions: Vec<sessions::EnvironmentSession>,
    /// What Settings reads while its scope selects no environment, and what an environment that
    /// is gone reads as (the contract defaults); see `settings_info`, `server_info`.
    unscoped_server_info: ServerInfo,
    /// Every environment's projects and threads.
    shell: ShellState,
    target: Option<Target>,
    thread: Option<OpenThread>,
    rows: Rc<[Row]>,
    /// Open work groups, turn folds and entries in the timeline (`timeline::*_key`).
    expanded: HashSet<SharedString>,
    entry_cache: timeline::EntryCache,
    /// The run whose limit rows the last rebuild left to the recovery notice; it is read from the shell.
    rows_notice_run: Option<String>,
    markdown: chat::MarkdownStates,
    /// Output of the open tool entries.
    details: inspector::Details,
    /// Where each timeline was left, and where the open one scrolls once its rows arrive.
    positions: positions::Positions,
    pending_position: Option<positions::Remembered>,
    copy_feedback: message_actions::CopyFeedback,
    /// The minimap tick under the pointer.
    minimap_hover: Option<usize>,
    /// Repaints the "Working for …" timer once a second while a turn runs.
    _working_tick: Option<Task<()>>,
    scroller: Entity<MessageScrollerState>,
    composer: Entity<TextareaState>,
    /// Targets whose send is in flight: each thread's or draft's own (`draftSendState`).
    sending: HashSet<Target>,
    /// Threads whose rewind is still rolling back on the server; they take no sends.
    rewinding: HashSet<ScopedId>,
    /// Model picked in the composer; for an open thread it applies to the next message.
    model_override: Option<ModelSelection>,
    /// `model_override` is the user's pick, not the model a new draft inherited
    /// (`modelSelectionExplicit`).
    model_explicit: bool,
    /// The model the user last picked in the composer (`stickyModelSelectionByProvider`), which
    /// new drafts start on. Kept for the session.
    sticky_model: Option<ModelSelection>,
    /// Runtime mode a new thread launches with; open threads keep theirs on the server.
    runtime_mode_override: Option<String>,
    /// Tracked so typing only re-renders when the send button's state flips.
    composer_empty: bool,
    /// The composer links the triage playbook (`isTriagePrompt`), which the draft hero follows.
    composer_triage: bool,
    /// Where the prompt says "ultrathink", which can own the model's effort level.
    composer_ultrathink: crate::model_label::Ultrathink,
    /// The floating composer's height last frame: the transcript reserves it at its end, and the
    /// "Scroll to end" pill sits above it (`composerTimelineInset`).
    composer_height: f32,
    /// The transcript lane's width last frame; a change reflows its rows (see `render_body`).
    timeline_lane_width: std::cell::Cell<f32>,
    /// A newly sent turn held near the top (see `timeline_anchor`); placed while rendering.
    timeline_anchor: std::cell::RefCell<timeline_anchor::Anchor>,
    /// The question card's drafts, for the first pending question.
    question: Option<attachments::QuestionDraft>,
    /// Requests that arrived mid-typing, kept out of sight for now.
    typing_guard: crate::composer_guard::TypingGuard,
    _typing_idle: Option<Task<()>>,
    /// Tab stops of the composer's attach, stop, primary (send, resume, submit) and compact controls.
    composer_action_focus: [gpui_kit::FocusHandle; 4],
    /// Composer text and question drafts of the targets not on screen.
    composer_drafts: HashMap<Target, composer::ComposerDraft>,
    /// Every draft's identity and the draft each project opens.
    drafts: draft_sessions::DraftSessions,
    /// Drafts kept across restarts.
    draft_store: draft_store::DraftStore,
    /// The model and permissions picks of the targets not on screen (`composerDraftStore`).
    composer_selections: HashMap<Target, composer::ComposerSelection>,
    /// Responses in flight, by environment, thread and request: request ids repeat across them.
    responding: HashSet<(SharedString, String, String)>,
    /// The `@` / `$` / `/` menu.
    menu: composer_menu::MenuState,
    picker: model_picker::PickerState,
    /// A draft's workspace mode, branch and permissions controls.
    draft_workspace: context_strip::DraftWorkspaceState,
    /// Plan mode per target and the dismissed fallback notice.
    composer_ui: composer_plan::ComposerUi,
    changed_files: changed_files::ChangedFilesUi,
    attachments: Vec<attachments::Attachment>,
    /// Prompts set aside with `composer.stash`.
    prompt_stash: prompt_stash::PromptStash,
    /// Where Up/Down prompt history stands.
    prompt_history: Option<crate::composer_text::HistoryPosition>,
    /// The editor's chip ids after the last edit, to notice a chip an undo brings back.
    chip_ids: Vec<String>,
    /// The context links the text held after the last edit, to notice deleted attachment references.
    context_refs: Vec<String>,
    /// A chip catch-up the composer refused during an IME composition.
    chip_sync_pending: Option<composer::ChipSync>,
    /// The provider instance the composer's and messages' skill chips last resolved against.
    chip_provider: Option<String>,
    /// The composer's Markdown styling, made the first time `composerRichTextEnabled` paints.
    composer_marks: Option<gpui_kit::component::input::TextDecorationCollection<TextareaMode>>,
    /// Ctrl (Cmd) is down, which turns steer into queue.
    ctrl_held: bool,
    /// Connected through `T3_SERVER_URL` rather than to this app's own server.
    environment_remote: bool,
    /// Files from the OS are being dragged over the chat canvas.
    dragging_files: bool,
    /// Targets whose dropped or picked files are still being read, one entry per read.
    reading_attachments: Vec<attachments::Origin>,
    /// Attachments whose reference was deleted, kept until the editor's undo history ends.
    retained_attachments: Vec<attachments::Attachment>,
    /// The open draft's context records, behind its `t3-context://` chips.
    context: context_records::ContextState,
    /// Sent messages their thread doesn't show yet.
    pending_sends: Vec<send::PendingSend>,
    error: Option<SharedString>,
    sidebar: sidebar::SidebarState,
    sidebar_view: Entity<sidebar::SidebarView>,
    visit: visits::VisitTracker,
    /// Collapsed, the sidebar column is not rendered at all.
    sidebar_open: sidebar_sheet::SidebarOpen,
    /// Where focus goes back to when the sheet hides.
    sheet_focus: sidebar_sheet::SheetFocus<gpui_kit::FocusHandle>,
    /// The sheet's focus trap: Tab cycles inside it while it shows.
    sheet_trap: gpui_kit::FocusHandle,
    titlebar_focus: titlebar::TitlebarFocus,
    /// Each thread's details card, inline or as the popover while the canvas is too narrow.
    details_visibility: details::DetailsVisibility,
    /// The thread whose "Previous agents" group is expanded in the details card.
    lineage_expanded: Option<String>,
    lineage: details::lineage::LineageUi,
    /// The git status of the open target's workspace.
    vcs: git::VcsWatch,
    /// Local servers, while the details card's Servers section can show.
    servers: details::servers::ServersWatch,
    /// Scheduled tasks, while the details card's Automations section can show.
    automations: details::automations::AutomationsWatch,
    /// The editor "Open in" launches, once picked.
    preferred_editor: Option<String>,
    /// This machine's editors that open remote paths, for remote environments' Open in.
    remote_editors: remote_open::RemoteEditors,
    right_panel: right_panel::RightPanel,
    /// The thread title's menu and inline rename.
    title: thread_menu::TitleState,
    share_header: guest_share::ShareHeader,
    /// The composer's "Run on" picker and each draft's machine choice.
    host: host_picker::HostUi,
    _connection_task: Task<()>,
    /// The launch landing ran (once, after the first shell snapshot and the first-run decision).
    landed: bool,
    onboarding: onboarding::Onboarding,
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
    /// Trackpad history swipes and their edge indicator.
    swipe: history_swipe::SwipeState,
    /// `t3 app` requests waiting for the primary environment.
    activation: app_activation::ActivationQueue,
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
    /// The draft hero's project picker.
    draft_picker: draft_picker::DraftPicker,
    /// The thread find bar, while open.
    find: Option<find::ThreadFind>,
    /// Go to file, Search project contents, and the Files and file panel surfaces.
    files: files::FilesState,
    /// The workspace search for the file a chat reference names, while it runs.
    file_lookup: Option<gpui_kit::Task<()>>,
    /// Holds focus when nothing inside does, so window shortcuts still reach the workspace.
    focus: gpui_kit::FocusHandle,
    /// The F6 stops that are regions rather than controls.
    areas: commands::WorkspaceAreas,
    /// Settings > Scheduled tasks, kept while the window lives; it unsubscribes while hidden.
    scheduled_tasks_view: Option<gpui_kit::AnyView>,
    preview: preview::PreviewState,
    /// Each checkout's `t3.json`, keyed by environment and workspace root, read on demand for
    /// project actions.
    t3_project_files: HashMap<(SharedString, String), crate::model::T3ProjectFile>,
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
    link_hover: external_links::LinkHoverState,
    image_viewer: image_viewer::ViewerUi,
    /// Native pages laid into the layout: rendered HTML pages and chat media.
    embeds: embed::Embeds,
    html_pages: html_page::HtmlPages,
    mcp_apps: mcp_app::McpApps,
    chat_media: markdown_media::ChatMediaUi,
    diagrams: mermaid::Diagrams,
    formulas: math::Formulas,
    entry_kinds: Arc<markdown_ui::EntryKinds>,
    thread_link_lookups: markdown_ui::ThreadLinkLookups,
    workflow: workflow::WorkflowUi,
    setup: worktree_setup::SetupWatch,
    chat_menu: Option<markdown_ui::ChatMenu>,
    citations: citations::CitationUi,
    requests: request_cards::RequestUi,
    subagents: subagents::SubagentUi,
    banners: banners::BannerUi,
    fresh_sessions: fresh_session::FreshSessions,
    prompt_suggestions: composer_suggestion::PromptSuggestions,
    /// Window bounds persistence and other desktop shell state.
    desktop: desktop::DesktopShell,
    /// Each environment's project clones and their toasts.
    clones: project_clone::ProjectClones,
    /// Running threads synced in the background.
    keep_alive: keep_alive::KeepAlive,
    /// Paired remote environments and their background connections.
    saved_environments: Entity<environments::SavedEnvironments>,
    /// The theme environment's published palettes and default theme.
    theme_feed: theme_feed::ThemeFeed,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let view = cx.weak_entity();
        let composer = cx.new(|cx| {
            // The composer paints its own contextual placeholder.
            let mut state =
                TextareaState::new(window, cx).auto_grow(2, composer::EDITOR_MAX_ROWS).submit_on_enter(true);
            state.set_scrollbar_renderer(Some(composer::editor_scrollbar()));
            state.set_copy_metadata(context_records::copy_metadata(view));
            state
        });
        let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
        let weak = cx.weak_entity();
        let saved_environments = cx.new(environments::SavedEnvironments::new);
        let subscriptions = vec![
            cx.observe_in(&saved_environments, window, |this, _, window, cx| this.sync_saved_sessions(window, cx)),
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.on_composer_change(window, cx),
                InputEvent::Focus => this.typing_guard.focus(),
                InputEvent::Blur => this.release_typing_hold(true, window, cx),
                InputEvent::PressEnter { .. } => {}
            }),
            // Caret moves re-read the completion trigger and what the rich editor shows as source.
            cx.observe(&composer, |this, _, cx| {
                this.sync_trigger(cx);
                this.sync_composer_look(false, cx);
            }),
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
            cx.observe_window_bounds(window, |_, window, cx| {
                if crate::theme::tokens(cx).breakpoint != crate::theme::Breakpoint::of(window.viewport_size().width) {
                    crate::theme::sync(window, cx);
                    // Cached views read the tokens too.
                    window.refresh();
                }
            }),
            cx.on_focus_lost(window, Self::recover_request_focus),
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.clear_system_alerts(window, cx);
                    if this.revalidate_diff() {
                        cx.notify();
                    }
                }
            }),
        ];
        let (desktop, desktop_subscriptions) = desktop::DesktopShell::new(window, cx);
        let mut subscriptions = subscriptions;
        subscriptions.extend(desktop_subscriptions);
        // A window macOS closes takes its workspace with it; the next one reads these drafts.
        subscriptions.push(cx.on_release(|this, cx| this.flush_drafts(cx)));
        subscriptions.push(cx.on_app_quit(|this, cx| {
            this.flush_drafts(cx);
            async {}
        }));
        let client_settings = crate::settings::ClientSettings::load();
        cx.set_global(crate::theme::Appearance::from_settings(&client_settings.values));
        crate::theme::set_preference(client_settings.theme_choice(), window, cx);
        let mut workspace = Self {
            connection: Connection::Starting,
            sessions: vec![sessions::EnvironmentSession::default()],
            unscoped_server_info: ServerInfo::default(),
            shell: ShellState::default(),
            target: None,
            thread: None,
            rows: Rc::default(),
            expanded: HashSet::new(),
            entry_cache: timeline::EntryCache::default(),
            rows_notice_run: None,
            markdown: chat::MarkdownStates::default(),
            details: inspector::Details::default(),
            positions: positions::Positions::default(),
            pending_position: None,
            copy_feedback: message_actions::CopyFeedback::default(),
            minimap_hover: None,
            _working_tick: None,
            scroller,
            composer,
            sending: HashSet::new(),
            rewinding: HashSet::new(),
            model_override: None,
            model_explicit: false,
            sticky_model: None,
            runtime_mode_override: None,
            composer_empty: true,
            composer_triage: false,
            composer_ultrathink: Default::default(),
            composer_height: 0.,
            timeline_lane_width: std::cell::Cell::new(0.),
            timeline_anchor: Default::default(),
            question: None,
            typing_guard: Default::default(),
            _typing_idle: None,
            composer_action_focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            composer_drafts: HashMap::new(),
            drafts: Default::default(),
            draft_store: Default::default(),
            composer_selections: HashMap::new(),
            responding: HashSet::new(),
            menu: Default::default(),
            picker: Default::default(),
            draft_workspace: Default::default(),
            composer_ui: Default::default(),
            changed_files: Default::default(),
            attachments: Vec::new(),
            prompt_stash: Default::default(),
            prompt_history: None,
            chip_ids: Vec::new(),
            context_refs: Vec::new(),
            chip_sync_pending: None,
            chip_provider: None,
            composer_marks: None,
            ctrl_held: false,
            environment_remote: false,
            dragging_files: false,
            reading_attachments: Vec::new(),
            retained_attachments: Vec::new(),
            context: Default::default(),
            pending_sends: Vec::new(),
            error: None,
            sidebar: sidebar::SidebarState::new(window, cx),
            visit: visits::VisitTracker::default(),
            sidebar_view: {
                let workspace = cx.weak_entity();
                cx.new(|_| sidebar::SidebarView(workspace))
            },
            sidebar_open: sidebar_sheet::SidebarOpen::restored(
                sidebar_sheet::docked_path().and_then(|path| sidebar_sheet::read_docked(&path)),
            ),
            sheet_focus: sidebar_sheet::SheetFocus::default(),
            sheet_trap: cx.focus_handle(),
            titlebar_focus: titlebar::TitlebarFocus::new(cx),
            details_visibility: details::DetailsVisibility::from_stored(
                client_settings.values.get(details::DETAILS_VISIBILITY_KEY),
            ),
            lineage_expanded: None,
            lineage: Default::default(),
            vcs: git::VcsWatch::default(),
            servers: Default::default(),
            automations: Default::default(),
            preferred_editor: None,
            remote_editors: Default::default(),
            right_panel: right_panel::RightPanel::new(cx),
            title: thread_menu::TitleState::new(cx),
            share_header: Default::default(),
            host: host_picker::HostUi::new(cx),
            _connection_task: Self::connect(window, cx),
            landed: false,
            onboarding: onboarding::Onboarding::new(&client_settings),
            page: None,
            pull_requests: pages::PullRequestsState::new(window, cx),
            usage: pages::UsageState::new(&client_settings.values),
            settings: None,
            settings_ui: pages::settings::SettingsUi::new(window, cx),
            client_settings,
            history: pages::History::default(),
            swipe: Default::default(),
            activation: Default::default(),
            devices: Default::default(),
            installed_keybindings: Rc::default(),
            keymap: Rc::from(crate::keybindings::merge_with_defaults(&[])),
            held_key: None,
            reload_toasts: Default::default(),
            pending_reload: None,
            keybindings_panel: None,
            theme_editor: None,
            palette: None,
            draft_picker: draft_picker::DraftPicker::new(cx),
            find: None,
            files: files::FilesState::default(),
            file_lookup: None,
            focus: cx.focus_handle(),
            areas: commands::WorkspaceAreas::new(cx),
            scheduled_tasks_view: None,
            preview: preview::PreviewState::new(window, cx),
            t3_project_files: HashMap::new(),
            terminals: terminal::Terminals::restored(),
            side_chat: Default::default(),
            alerts: Default::default(),
            snap_shot: Default::default(),
            quit: Default::default(),
            queue: Default::default(),
            now: Default::default(),
            markdown_images: Default::default(),
            link_hover: Default::default(),
            image_viewer: Default::default(),
            embeds: Default::default(),
            html_pages: Default::default(),
            mcp_apps: Default::default(),
            chat_media: Default::default(),
            diagrams: Default::default(),
            formulas: Default::default(),
            entry_kinds: Default::default(),
            thread_link_lookups: Default::default(),
            workflow: Default::default(),
            setup: Default::default(),
            chat_menu: None,
            citations: Default::default(),
            requests: Default::default(),
            subagents: Default::default(),
            banners: Default::default(),
            fresh_sessions: Default::default(),
            prompt_suggestions: Default::default(),
            desktop,
            clones: project_clone::ProjectClones::default(),
            keep_alive: Default::default(),
            saved_environments,
            theme_feed: Default::default(),
            _subscriptions: subscriptions,
        };
        workspace.restore_terminal_surfaces();
        workspace.sync_snap_shot(window, cx);
        workspace.resume_snap_shot_setup(window, cx);
        workspace.sync_saved_sessions(window, cx);
        workspace.start_theme_feed(window, cx);
        workspace.load_drafts(window, cx);
        ssh_prompt::listen(window, cx);
        workspace
    }

    /// Follows the app's backend ([`PrimaryBackend`]): while it is ready, keeps one socket open
    /// to it, reconnecting after drops. A restarted server gets a new socket.
    fn connect(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        let mut backend = cx.global::<PrimaryBackend>().clone();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let status = backend.status();
                let shown = this.update_in(cx, |this, window, cx| match &status {
                    BackendStatus::Starting => {
                        this.connection = Connection::Starting;
                        this.set_connection(Connection::Starting, cx);
                    }
                    // A new start replaces why the last one failed.
                    BackendStatus::Ready(_) if matches!(this.connection, Connection::Failed(_)) => {
                        this.connection = Connection::Starting;
                    }
                    BackendStatus::Ready(_) => {}
                    BackendStatus::Restarting(reason) => {
                        this.set_connection(Connection::Disconnected(reason.clone().into()), cx);
                    }
                    BackendStatus::Failed(reason) => {
                        this.set_connection(Connection::Failed(reason.clone().into()), cx);
                    }
                    BackendStatus::Off => {
                        this.set_connection(Connection::Off, cx);
                        this.land(window, cx);
                    }
                });
                if shown.is_err() {
                    return;
                }
                let BackendStatus::Ready(endpoint) = status else {
                    backend.changed().await;
                    continue;
                };
                let session = pin!(Self::primary_session(&this, endpoint, cx));
                // A session ends only for good (a rejected token); then wait for the backend.
                if let Either::Left(((), changed)) = future::select(session, pin!(backend.changed())).await {
                    changed.await;
                }
            }
        })
    }

    /// Keeps one socket open to `endpoint`, reconnecting after drops.
    async fn primary_session(this: &WeakEntity<Self>, mut endpoint: backend::Endpoint, cx: &mut AsyncWindowContext) {
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
                    if this.update(cx, |this, cx| this.set_connection(Connection::Disconnected(message), cx)).is_err() {
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
                    if this.update(cx, |this, cx| this.set_connection(Connection::Disconnected(message), cx)).is_err() {
                        return;
                    }
                }
            }
            cx.background_executor().timer(RECONNECT_DELAY).await;
        }
    }

    fn set_connection(&mut self, connection: Connection, cx: &mut Context<Self>) {
        // A stopped server is final; a late socket close must not hide why.
        if matches!(self.connection, Connection::Failed(_)) && !matches!(connection, Connection::Connected) {
            return;
        }
        if !matches!(connection, Connection::Connected) {
            self.set_primary_connection(None, None, cx);
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
        self.set_primary_connection(Some(client), Some(endpoint), cx);
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
        Self::pump_until(stream, window, cx, apply, after_batch, |_, _, _| {})
    }

    /// `pump`, also telling `on_end` how the stream ended, after its last batch.
    fn pump_until(
        stream: RpcStream,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut Self, &Value) + 'static,
        after_batch: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        on_end: impl Fn(&mut Self, &Result<(), RpcError>, &mut Context<Self>) + 'static,
    ) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(first) = stream.next().await {
                let mut events = vec![first];
                events.extend(std::iter::from_fn(|| stream.try_next()));
                let ended = this.update_in(cx, |this, window, cx| {
                    let mut ended = None;
                    let mut applied = false;
                    for event in events {
                        match event {
                            StreamEvent::Item(value) => {
                                apply(this, &value);
                                applied = true;
                            }
                            StreamEvent::End(result) => {
                                if let Err(error) = &result
                                    && !matches!(error, RpcError::Disconnected(_))
                                {
                                    this.error = Some(error.to_string().into());
                                }
                                ended = Some(result);
                            }
                        }
                    }
                    if applied {
                        after_batch(this, window, cx);
                    }
                    if let Some(result) = &ended {
                        on_end(this, result, cx);
                    }
                    cx.notify();
                    ended.is_some()
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
        let end_thread_id = thread_id.clone();
        thread._subscription = Some(Self::pump_until(
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
                    this.settle_pending_citation(true, cx);
                } else if thread.state.projection.is_some() {
                    thread.retried = false;
                }
                let Some(thread) = this.thread.as_ref() else { return };
                if thread.state.deleted {
                    this.reveal_pending_citation(cx);
                    this.close_thread(window, cx);
                    return;
                }
                this.refresh_rows(cx);
                // Another client may have moved the thread to a provider with other skills.
                this.sync_chip_provider(cx);
                this.refresh_thread_find(cx);
                this.sync_typing_guard(window, cx);
                this.sync_question(side_chat::ChatPane::Main, window, cx);
                this.sync_queue_edit(side_chat::ChatPane::Main, window, cx);
                this.sync_resume_compaction(cx);
                this.observe_proactive_panels(cx);
            },
            // A stream that ended short of a snapshot sends none; a drop resubscribes on reconnect.
            move |this, result, cx| {
                if this.thread.as_ref().is_some_and(|t| t.id == end_thread_id)
                    && !matches!(result, Err(RpcError::Disconnected(_)))
                {
                    this.settle_pending_citation(true, cx);
                }
            },
        ));
    }

    /// Electron always opens the index route, which starts a draft in the most
    /// recently active project (`IndexDraftLanding`); this is that landing, once the first-run
    /// gate decided. It waits for the saved drafts, so it opens the project's own. A fresh
    /// workspace gets the Welcome page instead.
    fn land(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_first_run(cx);
        if !self.landed {
            let shells = match self.primary_environment() {
                Some(id) => self.shell.has_snapshot(id),
                // `IndexDraftLanding` waits for every environment's shell.
                None => matches!(self.connection, Connection::Off) && self.shells_bootstrapped(cx),
            };
            let ready = self.draft_store.restored() && shells;
            if self.onboarding.pending() || !ready {
                return;
            }
            self.landed = true;
            if self.target.is_none()
                && !self.welcome_owns_home()
                && let Some((env, project_id)) =
                    crate::inbox::most_recent_project(&self.shell.projects, &self.shell.threads)
                        .map(|p| (p.environment_id.clone(), p.id.clone()))
            {
                self.start_draft_in(env, project_id, window, cx);
            }
            // With no environment to send a snapshot, nothing else opens the links that waited.
            self.open_pending_links(window, cx);
        }
        if self.welcome_owns_home() {
            self.show_welcome(cx);
        }
    }

    /// Leaves a thread that was deleted, as the web ends its view.
    fn close_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Its draft has nowhere to go: drop it rather than leave it in the editor.
        if let Some(target) = self.target.clone() {
            self.drop_deleted_draft(&target, cx);
            self.target = None;
        }
        self.attachments.clear();
        self.set_composer_text("", window, cx);
        self.thread = None;
        self.model_override = None;
        self.model_explicit = false;
        self.question = None;
        self.reset_timeline(cx);
        self.notify_sidebar(cx);
    }

    /// This device's Settings › Labs › Tool calls in chat.
    fn tool_call_visibility(&self) -> timeline::ToolCallVisibility {
        let value = self.client_settings.values.get("toolCallVisibility");
        timeline::ToolCallVisibility::from_setting(value.as_str().unwrap_or_default())
    }

    /// Rebuilds the timeline and tells the scroller which rows changed.
    fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        self.rows_notice_run = self.usage_limit_notice_run();
        let mut new_rows = match self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) {
            Some(projection) => {
                let visibility = self.tool_call_visibility();
                let visibility = if self.find.is_some() { visibility.while_finding() } else { visibility };
                let options = timeline::RowOptions { visibility, usage_limit_run: self.rows_notice_run.clone() };
                timeline::rows(projection, &self.expanded, &options, &mut self.entry_cache)
            }
            None => Vec::new(),
        };
        let env = self.target.as_ref().map(|target| target.environment().to_owned()).unwrap_or_default();
        let (sent, queued) = match self.thread.as_ref().and_then(|t| Some((&t.id, t.state.projection.as_ref()?))) {
            Some((id, projection)) => send::pending_rows(&mut self.pending_sends, &env, id, projection),
            None => Default::default(),
        };
        new_rows.extend(sent);
        self.apply_workflow_opening(&mut new_rows, cx);
        self.sync_worktree_setup(cx);
        self.insert_worktree_setup(&mut new_rows);
        // The footer: a pending handoff, what the thread is doing now, then the queue.
        new_rows.extend(self.pending_handoff_row());
        new_rows.extend(self.now_rows(side_chat::ChatPane::Main, cx));
        let mut queue = self.queue_rows(side_chat::ChatPane::Main);
        self.load_queue_images(side_chat::ChatPane::Main, cx);
        if !queued.is_empty() {
            let at = queue.iter().position(|row| matches!(row, Row::QueueToggle { .. })).unwrap_or(queue.len());
            if let Some(Row::Queued { last, .. }) = at.checked_sub(1).and_then(|i| queue.get_mut(i)) {
                *last = false;
            }
            queue.splice(at..at, queued);
        }
        new_rows.extend(queue);
        let answer_images: Vec<String> = new_rows.iter().flat_map(Row::open_answer_images).map(str::to_owned).collect();
        self.load_attachment_images(side_chat::ChatPane::Main, answer_images.iter().map(String::as_str), cx);
        let was_empty = self.rows.is_empty();
        self.sync_working_tick(&new_rows, cx);
        self.sync_subagent_tick(cx);
        self.sync_child_approvals(cx);
        self.sync_lineage_titles(cx);
        self.sync_timeline_anchor(cx);
        let Some((range, count)) = timeline::changed_range(&self.rows, &new_rows) else {
            self.reveal_pending_citation(cx);
            return;
        };
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
        if self.open_projection().is_some() {
            self.restore_position(cx);
        }
        self.reveal_pending_citation(cx);
    }

    /// Opens a thread the open timeline names (a subagent, a created thread, a sender): it
    /// lives on the open target's environment, as both chat panes' threads do.
    fn select_thread_beside(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = self.target.as_ref().map(Target::environment).cloned() else { return };
        self.select_thread_in(env, thread_id, window, cx);
    }

    /// Opens a thread on `env`; ids repeat across environments, so callers name the one they show.
    fn select_thread_in(&mut self, env: SharedString, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.mount_thread_family(&env, &thread_id, cx);
        let target = Target::Thread { env: env.clone(), id: thread_id.clone() };
        if self.target.as_ref() == Some(&target) {
            // `navigateToThread` closes the sheet even when the thread is already open.
            self.page = None;
            self.record_location();
            cx.notify();
            return;
        }
        self.page = None;
        self.stash_composer(cx);
        self.remember_position(cx);
        let leaving = self.target.replace(target);
        if let (Some(old), Some(leaving)) = (self.thread.take(), leaving) {
            self.retire_thread(leaving.environment(), old.id, old.state, cx);
        }
        let state = self.take_warm_thread(&ScopedId::new(env, thread_id.as_str())).unwrap_or_default();
        let resumed = state.projection.is_some();
        self.thread = Some(OpenThread {
            id: thread_id.clone(),
            state,
            snapshot_failed: false,
            retried: false,
            _subscription: None,
        });
        let saved = self.target.as_ref().and_then(|target| self.composer_selections.remove(target));
        self.model_override = saved.as_ref().and_then(|saved| saved.model.clone());
        self.model_explicit = saved.is_some_and(|saved| saved.explicit);
        self.restore_composer(window, cx);
        self.reset_timeline(cx);
        self.subscribe_thread(window, cx);
        if resumed {
            self.refresh_rows(cx);
        }
        if let Some(thread) = self.preview_thread() {
            self.load_previews(thread, window, cx);
        }
        if let Some(thread) = self.target.as_ref().and_then(Target::thread_key) {
            self.open_visit(thread, cx);
        }
        self.focus_composer(window, cx);
        self.notify_sidebar(cx);
        cx.notify();
        self.record_location();
    }

    /// "New thread" in a project on `env`, which the shell may not list yet (`createNewThreadHandler`).
    fn start_draft_in(&mut self, env: SharedString, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.project_draft(env, project_id, cx);
        self.open_draft(target, window, cx);
    }

    /// The draft "New thread" opens in a project, made that project's draft: an empty one it can
    /// reopen, else a new one. A draft with content keeps its own identity and stays a sidebar row.
    fn project_draft(&mut self, env: SharedString, project_id: String, cx: &App) -> Target {
        let project = ScopedId::new(env.clone(), project_id.as_str());
        let open = self.target.as_ref().and_then(Target::draft_id);
        let reused = self.drafts.reusable(&project, open, |draft| {
            let target = Target::draft(env.clone(), project_id.clone(), draft.clone());
            if self.target.as_ref() == Some(&target) {
                !self.composer_has_user_content(cx)
            } else {
                !self.composer_drafts.get(&target).is_some_and(composer::ComposerDraft::has_content)
            }
        });
        let draft = reused.unwrap_or_else(|| self.drafts.create(project.clone()));
        self.drafts.assign(project, &draft);
        Target::draft(env, project_id, draft)
    }

    /// A history entry's draft: that draft while it is unsent, still in this project and the
    /// project listed, else its project's draft on the entry's environment.
    fn open_draft_entry(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let Target::Draft { env, project_id, draft } = &target else { return };
        if !self.shell.projects.iter().any(|p| p.id == *project_id && p.environment_id == *env) {
            return;
        }
        if self.drafts.is_live(draft) && self.drafts.target(draft).as_ref() == Some(&target) {
            self.open_draft(target, window, cx);
        } else {
            self.start_draft_in(env.clone(), project_id.clone(), window, cx);
        }
    }

    /// Opens this exact draft, as its sidebar row does.
    fn open_draft(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.page = None;
        self.stash_composer(cx);
        let opened = self.target.as_ref() != Some(&target);
        if opened && !self.composer_drafts.contains_key(&target) {
            self.forget_draft_routing(&target);
        }
        let carried = self.carried_model();
        if opened {
            self.remember_position(cx);
        }
        let leaving = self.target.replace(target);
        if let (Some(old), Some(leaving)) = (self.thread.take(), leaving) {
            self.retire_thread(leaving.environment(), old.id, old.state, cx);
        }
        let saved = self.target.as_ref().and_then(|target| self.composer_selections.remove(target));
        // The draft already open keeps its picks (`useHandleNewThread`'s `isDraftAlreadyOpen`).
        match saved.filter(|_| opened) {
            Some(saved) => {
                self.model_override = saved.model;
                self.model_explicit = saved.explicit;
                self.runtime_mode_override = saved.runtime_mode;
            }
            None if opened => {
                let project = self.target_project();
                self.model_override = crate::new_thread::new_draft_model(
                    self.server_info(),
                    project,
                    carried,
                    self.sticky_model.as_ref(),
                );
                self.model_explicit = false;
                self.runtime_mode_override = None;
            }
            None => {}
        }
        self.restore_composer(window, cx);
        if opened {
            self.freeze_open_draft(cx);
        }
        self.reset_timeline(cx);
        self.focus_composer(window, cx);
        self.notify_sidebar(cx);
        cx.notify();
        self.record_location();
        self.sync_clone_toasts(window, cx);
    }

    fn reset_timeline(&mut self, cx: &mut Context<Self>) {
        self.rows = Rc::default();
        self.find = None;
        self.expanded.clear();
        self.entry_cache = timeline::EntryCache::default();
        self.markdown = chat::MarkdownStates::default();
        self.markdown_images = markdown_images::ImageCache::default();
        self.diagrams.forget_sources(side_chat::ChatPane::Main);
        // Leaving the thread drops its open image, as the web's thread overlay retention does.
        self.image_viewer.dismiss();
        self.entry_kinds = Arc::default();
        self.details = inspector::Details::default();
        self._working_tick = None;
        *self.timeline_anchor.get_mut() = Default::default();
        self.scroller.update(cx, |scroller, cx| scroller.reset(0, cx));
        // The cards went with the rows; their typed answers go too, as the web's unmounted forms.
        self.prune_requests();
        self.recall_position();
    }

    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |composer, cx| composer.focus(window, cx));
    }

    /// The model the next message runs on.
    fn composer_model(&self) -> Option<ModelSelection> {
        let selection = self.stored_composer_model()?;
        let descriptors = self.composer_descriptors(&selection);
        let selection = crate::model_label::with_legacy_ultracode(&descriptors, selection);
        Some(crate::model_label::without_plan_agent_selection(&descriptors, selection))
    }

    /// The model an existing thread's message carries: the composer's pick, else the thread's own
    /// when it still names the retired Plan agent (the web dispatches it normalized).
    pub(super) fn dispatch_model(&self) -> Option<ModelSelection> {
        let stored = self.stored_composer_model()?;
        (self.model_override.is_some() || crate::model_label::selects_plan_agent(&stored))
            .then(|| self.composer_model())
            .flatten()
    }

    /// The options the composer offers for `selection`'s model: never the `opencode` Plan agent.
    fn composer_descriptors(
        &self,
        selection: &ModelSelection,
    ) -> std::borrow::Cow<'_, [crate::model::OptionDescriptor]> {
        let model = self.server_info().model(selection).and_then(|m| m.capabilities.as_ref());
        crate::model_label::without_plan_agent(model.map(|c| c.option_descriptors.as_slice()).unwrap_or_default())
    }

    fn stored_composer_model(&self) -> Option<ModelSelection> {
        if let Some(selection) = &self.model_override {
            return Some(selection.clone());
        }
        match &self.target {
            Some(Target::Thread { .. }) => {
                self.thread.as_ref()?.state.projection.as_ref()?.thread.model_selection.clone()
            }
            Some(Target::Draft { env, project_id, .. }) => {
                let project = self.shell.project(&ScopedId::new(env.clone(), project_id.as_str()));
                let default = self.server_info().new_thread_model(project)?;
                Some(self.draft_usage_fallback(&default).map_or(default, |fallback| fallback.selection))
            }
            None => None,
        }
    }

    /// `resolvePendingHandoff` for the composer's pick, when its provider can take a turn.
    fn pending_handoff_row(&self) -> Option<timeline::Row> {
        let projection = self.thread.as_ref()?.state.projection.as_ref()?;
        let selection = self.composer_model()?;
        let providers = &self.server_info().providers;
        let usable = providers.iter().any(|provider| {
            provider.instance_id == selection.instance_id
                && provider.enabled
                && provider.availability.as_deref() != Some("unavailable")
        });
        let endpoints = crate::handoff::pending(projection, usable.then_some(&selection), providers)?;
        Some(timeline::Row::Handoff(Rc::new(timeline::HandoffRow {
            id: "pending-handoff".into(),
            endpoints,
            failed: false,
            pending: true,
        })))
    }

    /// The model a new draft carries from where it was started: the composer's pick, else the open
    /// thread's model or the draft's project default (`carryModelSelection`).
    fn carried_model(&self) -> Option<ModelSelection> {
        if let Some(selection) = &self.model_override {
            return Some(selection.clone());
        }
        match self.target.as_ref()? {
            Target::Thread { env, id } => {
                self.shell.thread(&ScopedId::new(env.clone(), id.as_str()))?.model_selection.clone()
            }
            Target::Draft { env, project_id, .. } => {
                let project = self.shell.project(&ScopedId::new(env.clone(), project_id.as_str()));
                crate::new_thread::draft_carried_model(self.server_info(), project, crate::timestamp::now_ms())
            }
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
        let Some((client, _, thread_id)) = self.pane_client(side_chat::ChatPane::Main, cx) else { return };
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else { return };
        let command = if self.server_info().capability("threadStop") {
            json!({"type": "thread.stop", "commandId": new_id(), "threadId": thread_id})
        } else {
            let Some(run) = projection.interruptible_run() else { return };
            json!({"type": "run.interrupt", "commandId": new_id(), "threadId": thread_id, "runId": run.id, "holdQueue": true})
        };
        self.dispatch_in_order(client, vec![command], cx);
    }

    /// Answers a runtime request in the pane's thread; `response` holds `decision`, `feedback` or `answers`.
    fn respond(&mut self, pane: side_chat::ChatPane, request_id: String, response: Value, cx: &mut Context<Self>) {
        self.dispatch_response(
            pane,
            request_id,
            |thread_id, request_id| respond_value(thread_id, request_id, response),
            cx,
        );
    }

    /// Sends `command(thread_id, request_id)` for a request in the pane's thread, holding the
    /// request's controls (`respondingRequestIds`) until it lands.
    fn dispatch_response(
        &mut self,
        pane: side_chat::ChatPane,
        request_id: String,
        command: impl FnOnce(&str, &str) -> Value,
        cx: &mut Context<Self>,
    ) {
        let Some((client, env, thread_id)) = self.pane_client(pane, cx) else { return };
        let command = command(&thread_id, &request_id);
        let key = (env.clone(), thread_id.clone(), request_id);
        if !self.responding.insert(key.clone()) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update(cx, |this, cx| {
                this.responding.remove(&key);
                if let Err(error) = result
                    && this.pane_shows(pane, &env, &thread_id)
                {
                    this.error = Some(error.to_string().into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Dispatches `command` on `env`, the environment its thread or project lives on.
    fn dispatch(&mut self, env: &str, command: Value, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(env) else { return };
        self.dispatch_in_order(client, vec![command], cx);
    }

    /// Dispatches dependent commands one after another on `client`, the socket of the environment
    /// the commands' thread lives on (a bare thread id can name a thread on two environments),
    /// stopping at the first failure.
    fn dispatch_in_order(&mut self, client: RpcClient, commands: Vec<Value>, cx: &mut Context<Self>) {
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

/// `runtime-request.respond` in `thread_id`, with `response`'s fields.
fn respond_value(thread_id: &str, request_id: &str, response: Value) -> Value {
    let mut command = json!({
        "type": "runtime-request.respond",
        "commandId": new_id(),
        "threadId": thread_id,
        "requestId": request_id,
    });
    if let (Some(command), Value::Object(fields)) = (command.as_object_mut(), response) {
        command.extend(fields);
    }
    command
}

fn draft_title(text: &str) -> String {
    // `threadTitleTextFromPrompt`: model references as their slugs, then citations as plain text.
    let text = crate::assistant_citations::to_plain_text(&crate::composer_text::model_mentions_as_slugs(text));
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
        if self.client_settings.read_failed {
            return self.render_settings_recovery(window, cx);
        }
        if self.onboarding.stalled() {
            return self.render_connection_recovery(window, cx);
        }
        if self.sidebar_open.fit(window.viewport_size().width) {
            self.notify_sidebar(cx);
        }
        self.follow_rows_surface(cx);
        self.sync_error_scope();
        self.sync_prompt_suggestion(cx);
        self.sync_saved_connection_banner(cx);
        self.sync_sheet_focus(window, cx);
        self.sync_welcome(window, cx);
        self.sidebar.fit_window(window);
        // Widening to the inline layout closes the popover, as the web does.
        if self.details_inline(window) {
            self.set_details_popover(false);
        }
        // A modal page covers the composer, so the editor behind it must not keep the keys.
        if window.focused(cx).is_none()
            || (self.page.is_some_and(pages::Page::is_modal) && self.composer.focus_handle(cx).is_focused(window))
        {
            self.focus.focus(window, cx);
        }
        self.sync_vcs(window, cx);
        theme_inspect::begin_frame(self.theme_editor.as_ref(), cx);
        #[cfg(windows)]
        if !self.preview.warm_requested && self.right_panel_open() {
            self.preview.warm_requested = true;
            preview::PreviewState::warm_up(window, cx);
        }
        self.sync_servers(window, cx);
        self.sync_load_balancing(cx);
        self.sync_automations(window, cx);
        let right_panel = self.render_right_panel(window, cx);
        let (right_panel, right_panel_sheet) =
            if self.right_panel_sheet(window) { (None, right_panel) } else { (right_panel, None) };
        let mini_player = self.render_preview_mini_player(window, cx);
        self.sync_preview_views(window, cx);
        self.sync_document_views(window, cx);
        self.begin_embed_frame(window, cx);
        let terminal_drawer = self.render_terminal_drawer(window, cx);
        let theme = cx.theme();
        let canvas = theme.background;
        let view = div()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .children(theme_inspect::probe(&["chrome"], cx))
            // `html, body`: the app chrome, behind the panes that paint the canvas.
            .bg(crate::theme::tokens(cx).chrome)
            .text_color(theme.foreground)
            .text_sm()
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
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
            .map(|el| self.on_menu_actions(el, cx))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.theme_inspect_escape(event, cx) {
                    cx.stop_propagation();
                    return;
                }
                if this.quit_key_down(event, cx) {
                    cx.stop_propagation();
                    return;
                }
                // The image viewer sits over every other layer and takes its own Escape.
                if this.image_viewer_open() {
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
                    if !this.usage_model_escape(window, cx) && !this.usage_card_escape(cx) {
                        this.close_page(cx);
                    }
                    cx.stop_propagation();
                    return;
                }
                this.escape_details_popover(event, window, cx);
            }))
            // Escape that nothing else took closes find (`useChatShortcuts` `dismissFind`).
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" && this.sidebar_open.sheet() {
                    this.sidebar_open.close_sheet();
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "escape" && this.palette.is_none() && this.right_panel_sheet(window) {
                    this.close_right_panel(cx);
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "escape" && this.find.is_some() && this.palette.is_none() {
                    this.close_thread_find(window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_key_up(cx.listener(|this, event: &gpui_kit::KeyUpEvent, _, cx| {
                this.held_key = None;
                this.quit_key_up(event, cx);
            }))
            .on_modifiers_changed(cx.listener(|this, event: &gpui_kit::ModifiersChangedEvent, window, cx| {
                this.snap_shot_record_modifiers(event, cx);
                this.quit_modifiers_changed(event, cx);
                this.pull_request_modifiers_changed(event, window, cx);
            }))
            .when(!cfg!(target_os = "macos"), |el| el.child(menu::wheel_zoom_listener(self.image_viewer_open())))
            .child(
                h_flex()
                    .size_full()
                    // Setup starts with an empty workspace, so the Welcome page has no thread list.
                    .when(self.sidebar_open.docked() && !self.welcome_open(), |el| {
                        el.child(self.render_cached_sidebar(cx))
                    })
                    // The main content (`sidebar-inset`), which the history swipe indicator edges.
                    .child(
                        h_flex()
                            .relative()
                            .flex_1()
                            .h_full()
                            .min_w_0()
                            .child(self.render_history_swipe_probe(cx))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .h_full()
                                    .min_w_0()
                                    // `SidebarInset`'s `bg-background`.
                                    .bg(canvas)
                                    .relative()
                                    .children(theme_inspect::probe(&["canvas", "text"], cx))
                                    .when(right_panel.is_none(), |el| el.pr(titlebar::RIGHT_RESIZE_INSET))
                                    // A maximized panel takes the row; the chat column keeps its state at zero width.
                                    .when(self.right_panel_maximized(), |el| el.w_0().flex_none().overflow_hidden())
                                    // The Pull requests page heads its list column; its panel takes the top row.
                                    .when(self.page != Some(pages::Page::PullRequests), |el| {
                                        el.child(self.render_header(window, cx))
                                    })
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
                                                    Some(Target::Draft { env, project_id, .. }) => el
                                                        .child(self.render_draft_stage(
                                                            &ScopedId::new(env, project_id),
                                                            window,
                                                            cx,
                                                        ))
                                                        .children(self.render_citation_comment(cx))
                                                        .children(self.render_details_card(window, cx)),
                                                    _ => el
                                                        .child(
                                                            div()
                                                                .relative()
                                                                .size_full()
                                                                .child(
                                                                    // A click on the timeline leaves the editor, as in a
                                                                    // browser, so a question card's 1-9 keys reach it.
                                                                    v_flex()
                                                                        .size_full()
                                                                        .on_mouse_down(
                                                                            gpui_kit::MouseButton::Left,
                                                                            |_, window, cx| window.blur(cx),
                                                                        )
                                                                        // The Cite button follows selections the text
                                                                        // views make, so it listens ahead of them.
                                                                        .capture_any_mouse_down(cx.listener(
                                                                            |this, _: &gpui_kit::MouseDownEvent, _, cx| {
                                                                                this.chat_mouse_down(cx);
                                                                            },
                                                                        ))
                                                                        .capture_any_mouse_up(cx.listener(
                                                                            |this, event: &gpui_kit::MouseUpEvent, window, cx| {
                                                                                if event.button == gpui_kit::MouseButton::Left {
                                                                                    this.chat_mouse_up(side_chat::ChatPane::Main, event.position, window, cx);
                                                                                }
                                                                            },
                                                                        ))
                                                                        .capture_action(cx.listener(
                                                                            |this, _: &gpui_kit::component::input::SelectAll, window, cx| {
                                                                                this.chat_select_all(side_chat::ChatPane::Main, window, cx);
                                                                            },
                                                                        ))
                                                                        .child(self.render_body(window, cx)),
                                                                )
                                                                .child(self.render_floating_composer(window, cx)),
                                                        )
                                                        .children(self.render_find_bar(cx))
                                                        .children(self.render_cite_toolbar(cx))
                                                        .children(self.render_citation_comment(cx))
                                                        .children(self.render_details_card(window, cx)),
                                                }
                                            })
                                            .children(mini_player)
                                            .children(self.render_link_hover(cx))
                                            .children(self.render_banner_lane(cx))
                                            .on_drop(cx.listener(
                                                |this, paths: &gpui_kit::ExternalPaths, window, cx| {
                                                    this.drop_paths(paths, window, cx);
                                                },
                                            ))
                                            .on_drag_move(cx.listener(Self::on_file_drag_move))
                                            .when(self.dragging_files && cx.has_active_drag(), |el| {
                                                el.child(self.render_drop_overlay(crate::theme::tokens(cx)))
                                            }),
                                    )
                                    .children(terminal_drawer),
                            )
                            .children(
                                right_panel.map(|panel| {
                                    div().h_full().flex_none().mr(titlebar::RIGHT_RESIZE_INSET).child(panel)
                                }),
                            ),
                    ),
            )
            .children(right_panel_sheet.map(|panel| self.render_right_panel_sheet(panel, window, cx)))
            // The panel toggles stay above the sheet, as the web's `z-50` titlebar controls do.
            .children(self.render_sidebar_sheet(window, cx))
            .child(self.render_titlebar_overlay(window, cx))
            .children(self.render_mcp_app_fullscreen(cx))
            .children(self.render_page_modal(window, cx))
            .children(self.render_settings(window, cx))
            .children(theme_inspect::render_overlay(self.theme_editor.as_ref(), cx))
            .children(self.theme_editor.clone())
            .children(self.render_palette(window, cx))
            .children(self.render_file_overlay(window, cx))
            .children(self.render_html_dialog(window, cx))
            .children(self.render_image_viewer(window, cx))
            .child(self.embeds.sweep())
            .children(self.render_quit_hint(cx))
            .children(self.render_history_swipe(cx))
            .child(self.render_caption_overlay(window, cx))
            .children(titlebar::render_resize_edges(window, cx))
            .into_any_element();
        // Only the built view knows whether the card rendered Lineage.
        self.sync_lineage_tick(cx);
        view
    }
}
