//! One session per connected environment (the web's per-environment runtime): its socket,
//! server config and shell subscription. The workspace's own server is the primary session
//! (none while the local environment is off); each enabled saved environment adds one on the
//! socket `SavedEnvironments` keeps open.
//! Every environment's projects and threads land in the one merged `Workspace::shell`.
//!
//! `client()`, `server_info()` and `endpoint()` resolve the open target's environment, so code
//! about the open thread or draft needs no environment of its own; code acting on a listed
//! thread or project passes that item's environment (`env_client`): ids repeat across
//! environments, so there is no lookup by a bare thread or project id.

use std::mem;

use gpui_kit::{App, Context, Hsla, SharedString, Task, Window};
use serde_json::{Value, json};

use super::pages::settings::{SessionLoad, check_session, operates_terminals};
use super::{Target, Workspace};
use crate::backend::Endpoint;
use crate::device::DeviceState;
use crate::environments::{ConnectionState, Phase, Presented};
use crate::model::{ScopedId, ServerInfo};
use crate::project_groups::ScratchRoot;
use crate::rpc::RpcClient;
use crate::theme::Tokens;

/// The primary session's index in `Workspace::sessions`; it is always there, without an id
/// while the local environment is off.
const PRIMARY: usize = 0;
/// How long a first connection's shell waits for the config before subscribing without it.
const SHELL_CONFIG_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// This client's grants there (`/api/auth/session`), checked on each connection.
    grants: SessionLoad,
    /// See `OpenThread::snapshot_failed` / `OpenThread::retried`.
    shell_snapshot_failed: bool,
    shell_retried: bool,
    /// The shell subscription waits for this connection's first config, which says whether it
    /// may leave older settled threads out.
    shell_waits_for_config: bool,
    /// Config, terminal, preview and device streams, and the session's grants.
    _streams: Vec<Task<()>>,
    _shell: Option<Task<()>>,
}

impl EnvironmentSession {
    fn saved(id: SharedString) -> Self {
        Self { id: Some(id), ..Self::default() }
    }

    /// Only a session that answered without `terminal:operate` keeps its terminals to output;
    /// the server still checks every write.
    pub fn operates_terminals(&self) -> bool {
        operates_terminals(&self.grants)
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

    /// The open thread's id on its environment.
    pub(super) fn thread_key(&self) -> Option<ScopedId> {
        match self {
            Target::Thread { env, id } => Some(ScopedId::new(env.clone(), id.clone())),
            Target::Draft { .. } => None,
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

    /// `readEnvironmentScope`: whether this client holds `scope` on the environment. A pending
    /// or failed check grants nothing.
    pub(super) fn env_grants(&self, environment_id: &str, scope: &str) -> bool {
        self.session(environment_id).is_some_and(|s| s.grants.grants(scope))
    }

    /// This client's session check on the environment, rechecked on each connection.
    pub(super) fn env_auth(&self, environment_id: &str) -> Option<&SessionLoad> {
        Some(&self.session(environment_id)?.grants)
    }

    /// `permissionAtom`: whether a control that sends `method` with `payload` to the environment
    /// may be used. Its socket refuses the request otherwise.
    pub(super) fn env_permits(&self, environment_id: &str, method: &str, payload: &Value) -> bool {
        self.session(environment_id).is_some_and(|s| s.grants.permits(method, payload))
    }

    /// `env_grants`, or `None` while the session check is pending or failing.
    pub(super) fn env_grant(&self, environment_id: &str, scope: &str) -> Option<bool> {
        Some(self.session(environment_id)?.grants.session()?.grants(scope))
    }

    /// Whether this client may change the environment's threads.
    pub(super) fn can_operate(&self, environment_id: &str) -> bool {
        self.env_grants(environment_id, "orchestration:operate")
    }

    /// The open target lives on the primary environment (or nothing is open).
    pub(super) fn target_on_primary(&self) -> bool {
        self.active_index() == Some(PRIMARY)
    }

    /// The footer status of the saved environment the open target lives on, read as the
    /// primary's is: nothing once synced, else what the connection is doing.
    pub(super) fn saved_target_status(&self, tokens: &Tokens, cx: &App) -> Option<(SharedString, Hsla)> {
        let environment = self.active_environment()?;
        let saved = self.saved_environments.read(cx);
        let state = &saved.session(environment)?.state;
        let muted = |text: &str| Some((SharedString::from(text.to_owned()), tokens.muted_foreground));
        match state.presented() {
            Presented::Connected if self.shell.has_snapshot(environment) => None,
            Presented::Connected => muted("Syncing…"),
            Presented::Available => None,
            Presented::Connecting => muted("Connecting…"),
            Presented::Reconnecting => Some((state.status_text().into(), tokens.warning_foreground)),
            Presented::Unsupported | Presented::Error => Some((state.status_text().into(), tokens.error_foreground)),
        }
    }

    /// The open target's session's index, so callers can borrow it beside other fields; `None`
    /// once the target's environment is gone. See [`target_session_index`].
    pub(super) fn active_index(&self) -> Option<usize> {
        target_session_index(&self.sessions, self.target.as_ref().map(|t| t.environment().as_ref()))
    }

    fn active_env_session(&self) -> Option<&EnvironmentSession> {
        Some(&self.sessions[self.active_index()?])
    }

    /// The open target's environment's server config, or an empty one once that environment is gone.
    pub(super) fn server_info(&self) -> &ServerInfo {
        self.active_env_session().map_or(&self.unscoped_server_info, |session| &session.server_info)
    }

    /// One environment's config, to apply what it saved.
    pub(super) fn env_info_mut(&mut self, environment_id: &str) -> Option<&mut ServerInfo> {
        Some(&mut self.session_mut(environment_id)?.server_info)
    }

    /// One environment's config, for a listed item that lives there.
    pub(super) fn env_info(&self, environment_id: &str) -> Option<&ServerInfo> {
        Some(&self.session(environment_id)?.server_info)
    }

    /// The device state of the environment Settings reads.
    pub(super) fn settings_device_state(&self) -> Option<&DeviceState> {
        self.settings_session()?.device_state.as_ref()
    }

    /// One environment's device state, `None` until its stream's first state arrives.
    pub(super) fn env_device_state(&self, environment_id: &str) -> Option<&DeviceState> {
        self.session(environment_id)?.device_state.as_ref()
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
        self.active_env_session()?.client.clone()
    }

    pub(super) fn env_client(&self, environment_id: &str) -> Option<RpcClient> {
        self.session(environment_id)?.client.clone()
    }

    /// One environment's address, for its HTTP routes.
    pub(super) fn env_endpoint(&self, environment_id: &str) -> Option<&Endpoint> {
        self.session(environment_id)?.endpoint.as_ref()
    }

    /// The open target's environment's address, for its HTTP routes (`/api/auth/*`).
    pub(super) fn endpoint(&self) -> Option<&Endpoint> {
        self.active_env_session()?.endpoint.as_ref()
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
        // A new socket starts from the last answer, as the web keeps a refreshing grant.
        client.set_grants(session.grants.rpc_grants());
        session.shell_retried = false;
        // A reconnect keeps the last config; only a first connection waits for one.
        let config_known = !session.server_info.config.is_null();
        session.shell_waits_for_config = !config_known;
        self.resume_file_saves(&environment_id, cx);
        self.retry_failed_embeds(&environment_id, window, cx);
        let config = client
            .stream("subscribeServerConfig", json!({"workspaceSnapshotsOnDemand": true, "environmentThemes": true}));
        let id = environment_id.clone();
        let config_id = environment_id.clone();
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
                // Whether it answers checks may have changed.
                this.publish_pull_request_routing(cx);
                if let Some(session) = this.session_mut(&config_id)
                    && mem::take(&mut session.shell_waits_for_config)
                {
                    this.subscribe_shell(config_id.clone(), window, cx);
                }
                if primary {
                    this.sync_keymap(cx);
                    this.show_reload_toast(window, cx);
                    this.check_chatgpt_welcome(window, cx);
                    // The first-run gate waits on the primary's config too.
                    this.land(window, cx);
                }
                // Its thread tag filter may have changed.
                this.retract_hidden_alerts(window, cx);
                // Each environment's browser gets this client's profiles.
                this.report_browser_profiles(cx);
                this.notify_sidebar(cx);
                // Skills arrive with provider updates.
                this.refresh_menu_items(cx);
                this.sync_theme_feed(window, cx);
                this.chip_lists_changed(cx);
            },
        );
        let mut streams = vec![
            config_task,
            self.subscribe_terminal_metadata(environment_id.clone(), &client, window, cx),
            self.subscribe_previews(&environment_id, &client, window, cx),
            self.subscribe_device_state(environment_id.clone(), &client, window, cx),
            self.subscribe_project_clones(environment_id.clone(), &client, window, cx),
        ];
        if !config_known {
            streams.push(self.subscribe_shell_without_config(environment_id.clone(), window, cx));
        }
        if primary {
            streams.push(self.subscribe_lifecycle(&client, window, cx));
        }
        if let Some(endpoint) = self.session(&environment_id).and_then(|s| s.endpoint.clone()) {
            let id = environment_id.clone();
            streams.push(check_session(endpoint, cx, move |this, grants, cx| {
                let before = this.env_grant(&id, super::git::SOURCE_CONTROL_WRITE);
                if let Some(session) = this.session_mut(&id) {
                    if let Some(client) = &session.client {
                        client.set_grants(grants.rpc_grants());
                    }
                    session.grants = grants;
                }
                this.apply_pr_write_grant(&id, before, cx);
                this.drop_unoperable_renames();
                this.notify_sidebar(cx);
                cx.notify();
            }));
        }
        let Some(session) = self.session_mut(&environment_id) else { return };
        session._streams = streams;
        session.device_state = None;
        // The open target's streams on this environment resume on the new socket.
        if self.active_environment() == Some(&environment_id) {
            self.end_target_streams();
            self.subscribe_thread(window, cx);
            self.sync_worktree_setup(cx);
            self.sync_details(cx);
        }
        if config_known {
            self.subscribe_shell(environment_id, window, cx);
        }
        self.sync_pull_request_refreshes(cx);
    }

    /// A config that never comes must not keep the thread list away: the shell subscribes
    /// without it after a while, keeping every settled thread.
    fn subscribe_shell_without_config(
        &mut self,
        environment_id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SHELL_CONFIG_WAIT).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(session) = this.session_mut(&environment_id)
                    && mem::take(&mut session.shell_waits_for_config)
                {
                    this.subscribe_shell(environment_id, window, cx);
                }
            });
        })
    }

    /// Ends the open target's own streams (thread, worktree setup, tool output, side chat)
    /// with the socket they ran on; each guard then lets the next connection start them again.
    pub(super) fn end_target_streams(&mut self) {
        if let Some(thread) = &mut self.thread {
            thread._subscription = None;
        }
        self.setup.disconnect();
        self.details = super::inspector::Details::default();
        self.side_chat.disconnect();
        self.prompt_suggestions.disconnect();
    }

    fn subscribe_shell(&mut self, environment_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(&environment_id) else { return };
        let mut input = json!({"requestCompletionMarker": true});
        // Older settled threads load on demand (the Settled shelf's pages, search, families).
        let omit_older_settled = self
            .session(&environment_id)
            .is_some_and(|s| s.server_info.config.get("shellOlderSettled").and_then(Value::as_bool) == Some(true));
        if omit_older_settled {
            input["omitOlderSettled"] = true.into();
        }
        let sequence = self.shell.sequence(&environment_id);
        // Rows held under other omissions would resume incomplete or stale, so they reload.
        if sequence > 0 && self.shell.older_settled(&environment_id).is_some() == omit_older_settled {
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
                this.verify_restored_drafts(cx);
                this.land(window, cx);
                this.open_pending_links(window, cx);
                this.keep_running_threads(cx);
                this.observe_alerts(window, cx);
                this.observe_inbox_while_hidden();
                this.notify_sidebar(cx);
                this.track_visit(cx);
                this.sync_now(cx);
                this.sync_child_approvals(cx);
                this.sync_draft_workspace(cx);
                this.sync_lineage_titles(cx);
                this.sync_older_settled_counts(&batch_id, cx);
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
            let session = saved.session(&environment.environment_id);
            let socket = session.and_then(|s| Some((s.client.clone()?, s.generation)));
            listed.push(id.clone());
            connections.push((id, socket, session.and_then(|s| s.endpoint.clone())));
        }
        let mut started = Vec::new();
        let mut closed = false;
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
                None if session.is_connected() => {
                    session.disconnect();
                    closed |= self.active_environment() == Some(&id);
                }
                None => {}
            }
        }
        if closed {
            self.end_target_streams();
        }
        let removed: Vec<SharedString> = self.sessions[PRIMARY + 1..]
            .iter()
            .filter_map(|s| s.id.clone())
            .filter(|id| !listed.contains(id))
            .collect();
        // A thread or draft on a machine that left has nowhere to go.
        if self.target.as_ref().is_some_and(|t| removed.contains(t.environment())) {
            self.close_thread(window, cx);
        }
        for id in &removed {
            self.sessions.retain(|s| !s.is(id));
            self.shell.remove_environment(id);
        }
        for id in started {
            self.start_session(id, window, cx);
        }
        if !removed.is_empty() {
            self.retract_hidden_alerts(window, cx);
            self.notify_sidebar(cx);
            self.sync_theme_feed(window, cx);
        }
        // Connections and grants both change here.
        self.publish_pull_request_routing(cx);
        // A launch that waited on the paired computers decides now.
        self.land(window, cx);
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
        self.publish_pull_request_routing(cx);
    }

    /// The primary session's socket opened or closed.
    pub(super) fn set_primary_connection(&mut self, client: Option<RpcClient>, endpoint: Option<Endpoint>, cx: &App) {
        let session = &mut self.sessions[PRIMARY];
        match client {
            Some(client) => {
                session.client = Some(client);
                session.endpoint = endpoint;
            }
            None => session.disconnect(),
        }
        self.publish_pull_request_routing(cx);
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

    /// `scratchEnvironmentId(currentProjectEnvironmentId)`: where "start without a project"
    /// opens its draft from the open thread or draft; `None` hides the way in.
    pub(super) fn scratch_environment(&self) -> Option<SharedString> {
        self.scratch_environment_from(self.target.as_ref().map(|target| target.environment().as_ref()))
    }

    /// `scratchEnvironmentId(fallback)`.
    pub(super) fn scratch_environment_from(&self, fallback: Option<&str>) -> Option<SharedString> {
        scratch_environment_of(&self.sessions, fallback)
    }

    /// `allEnvironmentShellsBootstrappedAtom`: the saved environments are read and every
    /// environment has its shell snapshot or is offline; the primary counts only once its id is
    /// known, and not at all while the local environment is off.
    pub(super) fn shells_bootstrapped(&self, cx: &App) -> bool {
        let saved = self.saved_environments.read(cx);
        let primary = &self.sessions[PRIMARY];
        let primary_settled = matches!(self.connection, super::Connection::Off)
            || primary.id.as_deref().is_some_and(|id| self.shell.has_snapshot(id) || !primary.is_connected());
        let saved_settled = |session: &EnvironmentSession| {
            session.id.as_deref().is_some_and(|id| {
                let state = saved.session(id).map(|s| &s.state);
                saved_shell_settled(self.shell.has_snapshot(id), state.unwrap_or(&ConnectionState::default()))
            })
        };
        !saved.loading() && primary_settled && self.sessions[PRIMARY + 1..].iter().all(saved_settled)
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

    /// Whether `key` is the open thread, on its environment.
    pub(super) fn is_open_thread(&self, key: &ScopedId) -> bool {
        matches!(&self.target, Some(Target::Thread { env, id }) if *env == key.env && *id == key.id)
    }

    /// A removed project's drafts go with it, the open one too.
    pub(super) fn forget_scoped_project_drafts(&mut self, project: &ScopedId, cx: &mut Context<Self>) {
        let of_project = |target: &Target| match target {
            Target::Draft { env, project_id, .. } => *env == project.env && *project_id == project.id,
            Target::Thread { .. } => false,
        };
        self.composer_drafts.retain(|target, _| !of_project(target));
        if self.target.as_ref().is_some_and(of_project) {
            self.target = None;
        }
        self.drafts.forget_project(project);
        self.drafts_changed(cx);
    }

    /// The session a settings page reads and its environment-wide controls act on: the
    /// scope's environment (`useSettingsScope().environment`), the primary on device pages.
    /// `None` when the scope selects no listed environment; it never falls back to the primary.
    fn settings_session(&self) -> Option<&EnvironmentSession> {
        if self.settings.as_deref() == Some("access") {
            return self.sessions.get(self.access_session_index());
        }
        let device = self.settings.as_deref().is_some_and(super::pages::settings::device_only);
        let scoped = if device { None } else { self.scoped_settings_environment() };
        settings_session_index(&self.sessions, device, scoped.as_deref()).map(|index| &self.sessions[index])
    }

    /// The session Sharing & access reads (`resolveAccessEnvironmentId`).
    fn access_session_index(&self) -> usize {
        access_session_index(&self.sessions, self.settings_ui.access_environment.as_deref())
    }

    /// Sharing & access shows the primary, the one server this app may have started.
    pub(super) fn access_is_primary(&self) -> bool {
        !matches!(self.connection, super::Connection::Off) && self.access_session_index() == PRIMARY
    }

    /// The scope's server config, or an empty one (the contract's defaults, no providers)
    /// when the scope selects no environment.
    pub(super) fn settings_info(&self) -> &ServerInfo {
        self.settings_session().map_or(&self.unscoped_server_info, |session| &session.server_info)
    }

    pub(super) fn settings_endpoint(&self) -> Option<&Endpoint> {
        self.settings_session()?.endpoint.as_ref()
    }

    pub(super) fn settings_client(&self) -> Option<RpcClient> {
        self.settings_session()?.client.clone()
    }

    /// The environment Settings reads, for a dialog or request to keep after the scope moves
    /// (with `env_client` and `env_info`).
    pub(super) fn settings_environment(&self) -> Option<SharedString> {
        self.settings_session()?.id.clone()
    }

    /// This client's session on the environment Settings reads, rechecked on each connection.
    pub(super) fn settings_auth(&self) -> Option<&SessionLoad> {
        Some(&self.settings_session()?.grants)
    }
}

/// The open target's session: its environment's, none once that one is gone (never another
/// environment's same-id thread); the primary only when nothing is open.
fn target_session_index(sessions: &[EnvironmentSession], target: Option<&str>) -> Option<usize> {
    match target {
        None => Some(PRIMARY),
        Some(environment) => sessions.iter().position(|session| session.is(environment)),
    }
}

/// `resolveAccessEnvironmentId`: the picked environment while it is listed, else the primary,
/// else (the local environment off) the first connected saved one, else the first.
pub(super) fn access_session_index(sessions: &[EnvironmentSession], selected: Option<&str>) -> usize {
    let listed = |id: &str| sessions.iter().position(|session| session.is(id));
    let saved = || sessions.iter().enumerate().skip(PRIMARY + 1);
    selected
        .and_then(listed)
        .or_else(|| sessions[PRIMARY].id.as_ref().map(|_| PRIMARY))
        .or_else(|| saved().find(|(_, session)| session.is_connected()).map(|(index, _)| index))
        .or_else(|| saved().next().map(|(index, _)| index))
        .unwrap_or(PRIMARY)
}

/// `allEnvironmentShellsBootstrappedAtom` for one saved environment: it has its snapshot, or is
/// disconnected and past the first retries, which may still bring it in.
fn saved_shell_settled(has_snapshot: bool, state: &ConnectionState) -> bool {
    has_snapshot
        || match state.phase {
            Phase::Connecting | Phase::Connected => false,
            Phase::Backoff => state.attempt > 2,
            Phase::Available | Phase::Blocked => true,
        }
}

/// Device pages read the primary; every other page reads only `scoped`'s session.
fn settings_session_index(sessions: &[EnvironmentSession], device_page: bool, scoped: Option<&str>) -> Option<usize> {
    if device_page {
        return Some(PRIMARY);
    }
    let scoped = scoped?;
    sessions.iter().position(|session| session.is(scoped))
}

fn scratch_environment_of(sessions: &[EnvironmentSession], current: Option<&str>) -> Option<SharedString> {
    let offering: Vec<&SharedString> = sessions
        .iter()
        .filter(|session| session.is_connected() && session.server_info.scratch_workspace_root.is_some())
        .filter_map(|session| session.id.as_ref())
        .collect();
    pick_scratch_environment(&offering, sessions[PRIMARY].id.as_deref(), current)
}

/// The primary wins when it offers a "No project" home, then `fallback`, then the only
/// environment that offers one.
fn pick_scratch_environment(
    offering: &[&SharedString],
    primary: Option<&str>,
    fallback: Option<&str>,
) -> Option<SharedString> {
    let offers = |id: &str| offering.iter().find(|offered| offered.as_ref() == id).map(|id| (*id).clone());
    primary.and_then(offers).or_else(|| fallback.and_then(offers)).or_else(|| match offering {
        [only] => Some((*only).clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_drafts_prefer_the_primary_then_the_draft_then_a_sole_offer() {
        let (primary, remote, other) = (SharedString::from("primary"), SharedString::from("remote"), "other".into());
        let pick =
            |offering: &[&SharedString], fallback| pick_scratch_environment(offering, Some("primary"), Some(fallback));
        assert_eq!(pick(&[&remote, &primary], "remote"), Some(primary.clone()));
        assert_eq!(pick(&[&remote, &other], "remote"), Some(remote.clone()));
        assert_eq!(pick(&[&other], "remote"), Some(other.clone()));
        assert_eq!(pick(&[&remote, &other], "elsewhere"), None);

        // From a thread on a remote, even one that went offline, the primary takes the draft.
        let session = |id: &SharedString, connected: bool| {
            let mut session = EnvironmentSession::saved(id.clone());
            session.client = connected.then(RpcClient::unopened);
            session.server_info.scratch_workspace_root = Some("scratch".into());
            session
        };
        let sessions = [session(&primary, true), session(&remote, true)];
        assert_eq!(scratch_environment_of(&sessions, Some("remote")), Some(primary.clone()));
        let sessions = [session(&primary, true), session(&remote, false)];
        assert_eq!(scratch_environment_of(&sessions, Some("remote")), Some(primary));
        let sessions = [session(&"primary".into(), false), session(&remote, true)];
        assert_eq!(scratch_environment_of(&sessions, Some("remote")), Some(remote));
    }

    #[test]
    fn a_target_whose_session_left_resolves_to_no_session() {
        let sessions = [EnvironmentSession::saved("primary".into()), EnvironmentSession::saved("remote".into())];
        assert_eq!(target_session_index(&sessions, None), Some(PRIMARY));
        assert_eq!(target_session_index(&sessions, Some("remote")), Some(1));
        assert_eq!(target_session_index(&sessions, Some("primary")), Some(PRIMARY));
        assert_eq!(target_session_index(&sessions, Some("removed")), None);
    }

    #[test]
    fn scoped_settings_never_fall_back_to_the_primary() {
        let sessions = [EnvironmentSession::saved("primary".into()), EnvironmentSession::saved("remote".into())];
        assert_eq!(settings_session_index(&sessions, false, Some("remote")), Some(1));
        assert_eq!(settings_session_index(&sessions, false, None), None);
        assert_eq!(settings_session_index(&sessions, false, Some("removed")), None);
        assert_eq!(settings_session_index(&sessions, true, None), Some(PRIMARY));

        // Sharing & access reads the machine it was switched to, the primary once that one leaves.
        assert_eq!(access_session_index(&sessions, Some("remote")), 1);
        assert_eq!(access_session_index(&sessions, Some("removed")), PRIMARY);
        assert_eq!(access_session_index(&sessions, None), PRIMARY);

        // With the local environment off there is no primary: the first connected machine, else the first.
        let connected = |id: &str| EnvironmentSession {
            client: Some(RpcClient::unopened()),
            ..EnvironmentSession::saved(id.into())
        };
        let sessions = [EnvironmentSession::default(), EnvironmentSession::saved("a".into()), connected("b")];
        assert_eq!(access_session_index(&sessions, None), 2);
        assert_eq!(access_session_index(&sessions, Some("a")), 1);
        assert_eq!(access_session_index(&sessions[..2], None), 1);
        assert_eq!(access_session_index(&sessions[..1], None), PRIMARY);
    }

    #[test]
    fn a_saved_shell_settles_once_it_has_a_snapshot_or_is_offline_past_its_first_retries() {
        let state = |phase, attempt| ConnectionState { phase, attempt, ..ConnectionState::default() };
        assert!(saved_shell_settled(true, &state(Phase::Connecting, 0)));
        assert!(!saved_shell_settled(false, &state(Phase::Connecting, 0)), "still connecting");
        assert!(!saved_shell_settled(false, &state(Phase::Connected, 1)), "connected, snapshot pending");
        assert!(!saved_shell_settled(false, &state(Phase::Backoff, 2)), "an early retry may still connect");
        assert!(saved_shell_settled(false, &state(Phase::Backoff, 3)));
        assert!(saved_shell_settled(false, &state(Phase::Blocked, 1)));
        assert!(saved_shell_settled(false, &state(Phase::Available, 0)));
    }
}
