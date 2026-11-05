//! One session per connected environment (the web's per-environment runtime): its socket,
//! server config and shell subscription. The workspace's own server is the primary session;
//! each enabled saved environment adds one on the socket `SavedEnvironments` keeps open.
//! Every environment's projects and threads land in the one merged `Workspace::shell`.
//!
//! `client()`, `server_info()` and `endpoint()` resolve the open target's environment, so code
//! about the open thread or draft needs no environment of its own; code acting on a listed
//! thread or project resolves that item's environment (`thread_client`, `project_client`).

use std::mem;

use gpui_kit::{App, Context, Hsla, SharedString, Task, Window};
use serde_json::{Value, json};

use super::{Target, Workspace};
use crate::backend::Endpoint;
use crate::device::DeviceState;
use crate::environments::Presented;
use crate::model::ServerInfo;
use crate::project_groups::ScratchRoot;
use crate::rpc::RpcClient;
use crate::theme::Tokens;

/// The primary session's index in `Workspace::sessions`; it is always there.
const PRIMARY: usize = 0;

#[derive(Default)]
pub(super) struct EnvironmentSession {
    /// The environment's id; `None` only for the primary until its descriptor is read.
    pub id: Option<SharedString>,
    pub client: Option<RpcClient>,
    pub endpoint: Option<Endpoint>,
    pub server_info: ServerInfo,
    /// The `SavedEnvironments` connection the streams run on; `None` for the primary.
    generation: Option<u64>,
    /// `subscribeDeviceState`; `None` until this connection's first state arrives.
    pub device_state: Option<DeviceState>,
    /// See `OpenThread::snapshot_failed` / `OpenThread::retried`.
    shell_snapshot_failed: bool,
    shell_retried: bool,
    /// Config, terminal, preview and device streams.
    _streams: Vec<Task<()>>,
    _shell: Option<Task<()>>,
}

impl EnvironmentSession {
    fn saved(id: SharedString) -> Self {
        Self { id: Some(id), ..Self::default() }
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    fn is(&self, environment_id: &str) -> bool {
        self.id.as_deref() == Some(environment_id)
    }

    pub(super) fn disconnect(&mut self) {
        self.client = None;
        self._streams.clear();
        self._shell = None;
    }
}

impl Target {
    pub(super) fn environment(&self) -> &SharedString {
        match self {
            Target::Thread { env, .. } | Target::Draft { env, .. } => env,
        }
    }
}

impl Workspace {
    /// Every environment's session, the primary first.
    pub(super) fn sessions(&self) -> &[EnvironmentSession] {
        &self.sessions
    }

    /// The workspace's own server (or `T3_SERVER_URL`'s), once its descriptor is read.
    pub(super) fn primary_environment(&self) -> Option<&SharedString> {
        self.sessions[PRIMARY].id.as_ref()
    }

    /// The open target's environment, else the primary one.
    pub(super) fn active_environment(&self) -> Option<&SharedString> {
        self.target.as_ref().map(Target::environment).or_else(|| self.primary_environment())
    }

    fn session_index(&self, environment_id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.is(environment_id))
    }

    pub(super) fn session(&self, environment_id: &str) -> Option<&EnvironmentSession> {
        self.sessions.iter().find(|s| s.is(environment_id))
    }

    fn session_mut(&mut self, environment_id: &str) -> Option<&mut EnvironmentSession> {
        self.sessions.iter_mut().find(|s| s.is(environment_id))
    }

    /// The open target lives on the primary environment (or nothing is open).
    pub(super) fn target_on_primary(&self) -> bool {
        self.active_index() == PRIMARY
    }

    /// The footer status of the saved environment the open target lives on, read as the
    /// primary's is: nothing once synced, else what the connection is doing.
    pub(super) fn saved_target_status(&self, tokens: &Tokens, cx: &App) -> Option<(SharedString, Hsla)> {
        let environment = self.active_environment()?;
        let saved = self.saved_environments.read(cx);
        let state = &saved.session(environment)?.state;
        let muted = |text: &str| Some((SharedString::from(text.to_owned()), tokens.muted_foreground));
        match state.presented() {
            Presented::Connected if self.shell.sequence(environment) > 0 => None,
            Presented::Connected => muted("Syncing…"),
            Presented::Available => None,
            Presented::Connecting => muted("Connecting…"),
            Presented::Reconnecting => Some((state.status_text().into(), tokens.warning_foreground)),
            Presented::Unsupported | Presented::Error => Some((state.status_text().into(), tokens.error_foreground)),
        }
    }

    /// The open target's session's index, so callers can borrow it beside other fields.
    pub(super) fn active_index(&self) -> usize {
        self.target.as_ref().and_then(|t| self.session_index(t.environment())).unwrap_or(PRIMARY)
    }

    fn active_env_session(&self) -> &EnvironmentSession {
        &self.sessions[self.active_index()]
    }

    /// The open target's environment's server config.
    pub(super) fn server_info(&self) -> &ServerInfo {
        &self.active_env_session().server_info
    }

    pub(super) fn server_info_mut(&mut self) -> &mut ServerInfo {
        let index = self.active_index();
        &mut self.sessions[index].server_info
    }

    /// One environment's config, to apply what it saved.
    pub(super) fn env_info_mut(&mut self, environment_id: &str) -> Option<&mut ServerInfo> {
        Some(&mut self.session_mut(environment_id)?.server_info)
    }

    /// The device state of the environment Settings reads.
    pub(super) fn settings_device_state(&self) -> Option<&DeviceState> {
        self.sessions[self.settings_index()].device_state.as_ref()
    }

    pub(super) fn set_device_state(&mut self, environment_id: &str, state: DeviceState) {
        if let Some(session) = self.session_mut(environment_id) {
            session.device_state = Some(state);
        }
    }

    /// The primary environment's config: client-wide reads such as keybindings.
    pub(super) fn primary_info(&self) -> &ServerInfo {
        &self.sessions[PRIMARY].server_info
    }

    /// The open target's environment's socket.
    pub(super) fn client(&self) -> Option<RpcClient> {
        self.active_env_session().client.clone()
    }

    pub(super) fn env_client(&self, environment_id: &str) -> Option<RpcClient> {
        self.session(environment_id)?.client.clone()
    }

    /// The environment of a listed thread, else the open target's.
    pub(super) fn thread_environment(&self, thread_id: &str) -> Option<SharedString> {
        self.shell.thread_environment(thread_id).or_else(|| self.active_environment()).cloned()
    }

    /// The environment of a listed project, else the open target's.
    pub(super) fn project_environment(&self, project_id: &str) -> Option<SharedString> {
        self.shell.project_environment(project_id).or_else(|| self.active_environment()).cloned()
    }

    /// The socket of the environment a listed thread lives on.
    pub(super) fn thread_client(&self, thread_id: &str) -> Option<RpcClient> {
        self.env_client(&self.thread_environment(thread_id)?)
    }

    /// The socket of the environment a listed project lives on.
    pub(super) fn project_client(&self, project_id: &str) -> Option<RpcClient> {
        self.env_client(&self.project_environment(project_id)?)
    }

    /// The socket for an orchestration command: its thread's environment, else its project's.
    pub(super) fn command_client(&self, command: &Value) -> Option<RpcClient> {
        if let Some(thread_id) = command.get("threadId").and_then(Value::as_str) {
            return self.thread_client(thread_id);
        }
        match command.get("projectId").and_then(Value::as_str) {
            Some(project_id) => self.project_client(project_id),
            None => self.client(),
        }
    }

    /// The open target's environment's address, for its HTTP routes (`/api/auth/*`).
    pub(super) fn endpoint(&self) -> Option<&Endpoint> {
        self.active_env_session().endpoint.as_ref()
    }

    /// The connected server's HTTP origin, for its routes and signed asset URLs.
    pub(super) fn http_base(&self) -> Option<&str> {
        self.endpoint().map(|e| e.http_base.as_str())
    }

    /// Starts an environment's config and shell subscriptions on its socket.
    pub(super) fn start_session(&mut self, environment_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let primary = self.primary_environment() == Some(&environment_id);
        let Some(session) = self.session_mut(&environment_id) else { return };
        let Some(client) = session.client.clone() else { return };
        session.shell_retried = false;
        let config = client.stream("subscribeServerConfig", json!({"workspaceSnapshotsOnDemand": true}));
        let id = environment_id.clone();
        let config_task = Self::pump(
            config,
            window,
            cx,
            move |this, value| {
                let Some(session) = this.session_mut(&id) else { return };
                if let Err(error) = session.server_info.apply(value) {
                    log::warn!("bad server config event: {error}");
                }
                // Keybindings are this device's, read from the primary environment.
                if primary && let Some(reload) = this.reload_toasts.handle(value, std::time::Instant::now()) {
                    this.pending_reload = Some(reload);
                }
            },
            move |this, window, cx| {
                if primary {
                    this.sync_keymap(cx);
                    this.show_reload_toast(window, cx);
                    this.check_chatgpt_welcome(window, cx);
                    this.report_browser_profiles(cx);
                }
                this.notify_sidebar(cx);
                // Skills arrive with provider updates.
                this.refresh_menu_items(cx);
            },
        );
        let streams = vec![
            config_task,
            self.subscribe_terminal_metadata(environment_id.clone(), &client, window, cx),
            self.subscribe_previews(&environment_id, &client, window, cx),
            self.subscribe_device_state(environment_id.clone(), &client, window, cx),
        ];
        let Some(session) = self.session_mut(&environment_id) else { return };
        session._streams = streams;
        session.device_state = None;
        // The open thread on this environment resumes on the new socket.
        if self.active_environment() == Some(&environment_id) {
            self.subscribe_thread(window, cx);
        }
        self.subscribe_shell(environment_id, window, cx);
    }

    fn subscribe_shell(&mut self, environment_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(&environment_id) else { return };
        let mut input = json!({"requestCompletionMarker": true});
        let sequence = self.shell.sequence(&environment_id);
        if sequence > 0 {
            input["afterSequence"] = sequence.into();
        }
        let stream = client.stream("orchestration.subscribeShell", input);
        let apply_id = environment_id.clone();
        let batch_id = environment_id.clone();
        let task = Self::pump(
            stream,
            window,
            cx,
            move |this, value| {
                if let Err(error) = this.shell.apply(&apply_id, value) {
                    log::warn!("bad shell event: {error}");
                    if let Some(session) = this.session_mut(&apply_id) {
                        session.shell_snapshot_failed = true;
                    }
                }
            },
            move |this, window, cx| {
                let sequence = this.shell.sequence(&batch_id);
                let Some(session) = this.session_mut(&batch_id) else { return };
                // The reducer reset its cursor, so one resubscribe fetches a fresh snapshot.
                if mem::take(&mut session.shell_snapshot_failed) {
                    if !mem::replace(&mut session.shell_retried, true) {
                        this.subscribe_shell(batch_id.clone(), window, cx);
                        return;
                    }
                    this.error =
                        Some("Couldn't read the thread list from the server. Restart T3 Code to retry.".into());
                } else if sequence > 0 {
                    session.shell_retried = false;
                }
                this.land(window, cx);
                this.open_pending_links(window, cx);
                this.keep_running_threads(cx);
                this.observe_alerts(window, cx);
                this.observe_inbox_while_hidden();
                this.notify_sidebar(cx);
                this.track_visit(cx);
                this.sync_now(cx);
                this.sync_child_approvals(cx);
            },
        );
        if let Some(session) = self.session_mut(&environment_id) {
            session._shell = Some(task);
        }
    }

    /// Follows `SavedEnvironments`: a new socket gets a session and its subscriptions, a closed
    /// one keeps the last state on screen, a removed or switched-off environment leaves the lists.
    pub(super) fn sync_saved_sessions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved = self.saved_environments.read(cx);
        let mut listed: Vec<SharedString> = Vec::new();
        let mut connections = Vec::new();
        for environment in saved.environments() {
            if !environment.enabled || saved.is_primary(&environment.environment_id) {
                continue;
            }
            let id: SharedString = environment.environment_id.clone().into();
            let socket =
                saved.session(&environment.environment_id).and_then(|s| Some((s.client.clone()?, s.generation)));
            listed.push(id.clone());
            connections.push((id, socket, environment.endpoint()));
        }
        let mut started = Vec::new();
        for (id, socket, endpoint) in connections {
            let index = match self.session_index(&id) {
                Some(index) => index,
                None => {
                    self.sessions.push(EnvironmentSession::saved(id.clone()));
                    self.sessions.len() - 1
                }
            };
            let session = &mut self.sessions[index];
            match socket {
                Some((client, generation)) if session.generation != Some(generation) => {
                    session.client = Some(client);
                    session.endpoint = endpoint;
                    session.generation = Some(generation);
                    started.push(id);
                }
                Some(_) => {}
                None => session.disconnect(),
            }
        }
        let removed: Vec<SharedString> = self.sessions[PRIMARY + 1..]
            .iter()
            .filter_map(|s| s.id.clone())
            .filter(|id| !listed.contains(id))
            .collect();
        for id in &removed {
            self.sessions.retain(|s| !s.is(id));
            self.shell.remove_environment(id);
        }
        for id in started {
            self.start_session(id, window, cx);
        }
        if !removed.is_empty() {
            self.notify_sidebar(cx);
        }
        cx.notify();
    }

    /// The primary environment's id is known: an equal saved environment is this session.
    pub(super) fn set_primary_environment(&mut self, environment_id: SharedString, cx: &mut Context<Self>) {
        if self.primary_environment() == Some(&environment_id) {
            return;
        }
        // A saved session to the same environment closes; the primary carries its threads now.
        if let Some(index) = self.session_index(&environment_id) {
            self.sessions.remove(index);
            self.shell.remove_environment(&environment_id);
        }
        self.saved_environments.update(cx, |saved, cx| saved.set_primary(&environment_id, cx));
        self.sessions[PRIMARY].id = Some(environment_id);
    }

    /// The primary session's socket opened or closed.
    pub(super) fn set_primary_connection(&mut self, client: Option<RpcClient>, endpoint: Option<Endpoint>) {
        let session = &mut self.sessions[PRIMARY];
        match client {
            Some(client) => {
                session.client = Some(client);
                session.endpoint = endpoint;
            }
            None => session.disconnect(),
        }
    }

    /// Each connected environment's "No project" workspace, for project grouping.
    pub(super) fn scratch_roots(&self) -> Vec<ScratchRoot> {
        self.sessions
            .iter()
            .filter_map(|session| {
                let root = session.server_info.scratch_workspace_root.clone()?;
                Some(ScratchRoot { environment_id: session.id.clone()?, root })
            })
            .collect()
    }

    /// [`Self::scratch_roots`] equals `cached`, compared without collecting them.
    pub(super) fn scratch_roots_match(&self, cached: &[ScratchRoot]) -> bool {
        let current = self
            .sessions
            .iter()
            .filter_map(|session| Some((session.id.as_ref()?, session.server_info.scratch_workspace_root.as_ref()?)));
        current.eq(cached.iter().map(|scratch| (&scratch.environment_id, &scratch.root)))
    }

    /// The open thread's id, when a thread is the target.
    pub(super) fn open_thread_id(&self) -> Option<&str> {
        match &self.target {
            Some(Target::Thread { id, .. }) => Some(id),
            _ => None,
        }
    }

    /// A project's draft as a target, on the project's environment.
    pub(super) fn draft_target(&self, project_id: &str) -> Option<Target> {
        Some(Target::Draft { env: self.project_environment(project_id)?, project_id: project_id.to_owned() })
    }

    /// A listed thread as a target, on its environment.
    pub(super) fn thread_target(&self, thread_id: &str) -> Option<Target> {
        Some(Target::Thread { env: self.thread_environment(thread_id)?, id: thread_id.to_owned() })
    }

    /// The session a settings page reads and its environment-wide controls act on: the
    /// scope's environment (`useSettingsScope().environment`), the primary on device pages.
    fn settings_index(&self) -> usize {
        let device = self.settings.as_deref().is_some_and(super::pages::settings::device_only);
        self.settings_ui
            .scope
            .machine
            .as_deref()
            .filter(|_| !device)
            .and_then(|id| self.session_index(id))
            .filter(|&index| self.sessions[index].is_connected())
            .unwrap_or(PRIMARY)
    }

    pub(super) fn settings_info(&self) -> &ServerInfo {
        &self.sessions[self.settings_index()].server_info
    }

    pub(super) fn settings_info_mut(&mut self) -> &mut ServerInfo {
        let index = self.settings_index();
        &mut self.sessions[index].server_info
    }

    pub(super) fn settings_endpoint(&self) -> Option<&Endpoint> {
        self.sessions[self.settings_index()].endpoint.as_ref()
    }

    pub(super) fn settings_client(&self) -> Option<RpcClient> {
        self.sessions[self.settings_index()].client.clone()
    }
}
