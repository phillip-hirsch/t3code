//! Saved remote environments and one background connection per enabled one
//! (`connection/supervisor.ts`): connecting, connected, backing off, or blocked until a retry.
//! The workspace runs each connected environment's streams on that socket (`super::sessions`).

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::{Context, Task};

use crate::environments::{self, Catalog, ConnectionState, Failure, Phase, SavedEnvironment};
use crate::rpc::{self, ConnectOptions, RpcClient};
use crate::runtime;

#[derive(Default)]
pub(crate) struct Session {
    pub state: ConnectionState,
    /// From the environment's descriptor, read on each attempt.
    pub server_version: Option<String>,
    pub machine: Option<String>,
    /// The open socket while connected.
    pub client: Option<RpcClient>,
    /// Counts connections, so the workspace notices a new socket and resubscribes on it.
    pub generation: u64,
    _task: Option<Task<()>>,
}

pub(crate) struct SavedEnvironments {
    catalog: Catalog,
    sessions: HashMap<String, Session>,
    /// The workspace's primary environment. Saved under the same id, it is that session
    /// (`EnvironmentRegistry.register` ignores platform environments), so it gets no socket here.
    primary: Option<String>,
}

impl SavedEnvironments {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self { catalog: Catalog::load(), sessions: HashMap::new(), primary: None };
        let ids: Vec<String> = this.catalog.environments.iter().map(|e| e.environment_id.clone()).collect();
        for id in ids {
            this.restart(&id, cx);
        }
        this
    }

    pub fn environments(&self) -> &[SavedEnvironment] {
        &self.catalog.environments
    }

    pub fn session(&self, environment_id: &str) -> Option<&Session> {
        self.sessions.get(environment_id)
    }

    /// The saved environment is the primary one, connected through the workspace's own socket.
    pub fn is_primary(&self, environment_id: &str) -> bool {
        self.primary.as_deref() == Some(environment_id)
    }

    /// Learns the primary environment's id and stops a duplicate connection to it.
    pub fn set_primary(&mut self, environment_id: &str, cx: &mut Context<Self>) {
        if self.is_primary(environment_id) {
            return;
        }
        let previous = self.primary.replace(environment_id.to_owned());
        for id in previous.into_iter().chain([environment_id.to_owned()]) {
            self.restart(&id, cx);
        }
    }

    pub fn local_environment_enabled(&self) -> bool {
        self.catalog.local_environment_enabled
    }

    fn save(&self) -> Result<(), String> {
        self.catalog.save().map_err(|error| {
            log::warn!("could not save environments: {error:#}");
            format!("Could not save environments: {error:#}")
        })
    }

    /// Saves a paired environment and connects it (`registerPairingConnection`).
    pub fn add(&mut self, environment: SavedEnvironment, cx: &mut Context<Self>) -> Result<(), String> {
        let id = environment.environment_id.clone();
        self.catalog.upsert(environment);
        self.save()?;
        self.restart(&id, cx);
        cx.notify();
        Ok(())
    }

    pub fn set_enabled(&mut self, environment_id: &str, enabled: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let Some(environment) = self.catalog.get_mut(environment_id) else { return Ok(()) };
        environment.enabled = enabled;
        self.save()?;
        self.restart(environment_id, cx);
        cx.notify();
        Ok(())
    }

    pub fn rename(&mut self, environment_id: &str, label: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let label = environments::validate_label(label)?;
        let Some(environment) = self.catalog.get_mut(environment_id) else { return Ok(()) };
        environment.label = label;
        self.save()?;
        cx.notify();
        Ok(())
    }

    /// Forgets the pairing and credential here; the session on that machine stays until revoked there.
    pub fn remove(&mut self, environment_id: &str, cx: &mut Context<Self>) -> Result<(), String> {
        self.sessions.remove(environment_id);
        self.catalog.environments.retain(|environment| environment.environment_id != environment_id);
        cx.notify();
        self.save()
    }

    pub fn set_local_environment_enabled(&mut self, enabled: bool) -> Result<(), String> {
        self.catalog.local_environment_enabled = enabled;
        self.save()
    }

    /// Starts an environment's connection loop afresh (startup, switched on, paired again).
    pub fn restart(&mut self, environment_id: &str, cx: &mut Context<Self>) {
        let Some(environment) = self.catalog.get(environment_id).cloned() else { return };
        let session = self.sessions.entry(environment_id.to_owned()).or_default();
        session.state = ConnectionState::default();
        session.client = None;
        let own = environment.enabled && self.primary.as_deref().is_none_or(|id| id != environment_id);
        session._task = own.then(|| Self::supervise(environment, 0, cx));
        cx.notify();
    }

    /// `retryNow`: attempts at once, keeping the failure and attempt count on screen meanwhile.
    pub fn retry(&mut self, environment_id: &str, cx: &mut Context<Self>) {
        let Some(environment) = self.catalog.get(environment_id).cloned().filter(|e| e.enabled) else { return };
        if self.is_primary(environment_id) {
            return;
        }
        let Some(session) = self.sessions.get_mut(environment_id) else { return };
        let failures = session.state.attempt;
        session._task = Some(Self::supervise(environment, failures, cx));
    }

    fn update_session(&mut self, environment_id: &str, cx: &mut Context<Self>, update: impl FnOnce(&mut Session)) {
        if let Some(session) = self.sessions.get_mut(environment_id) {
            update(session);
            cx.notify();
        }
    }

    fn supervise(environment: SavedEnvironment, failures: u32, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let id = environment.environment_id.clone();
            let mut failures = failures;
            loop {
                let attempt = failures + 1;
                let entered = this.update(cx, |this, cx| {
                    this.update_session(&id, cx, |s| {
                        s.state =
                            ConnectionState { phase: Phase::Connecting, attempt, retry_at: None, ..s.state.clone() };
                    });
                });
                if entered.is_err() {
                    return;
                }
                let attempt_result = runtime().spawn(connect(environment.clone())).await;
                let result =
                    attempt_result.unwrap_or_else(|error| Err(Box::new((Failure::transport(error.to_string()), None))));
                match result {
                    Ok((client, descriptor)) => {
                        failures = 0;
                        let connected = this.update(cx, |this, cx| {
                            this.update_session(&id, cx, |s| {
                                s.state = ConnectionState { phase: Phase::Connected, attempt, ..Default::default() };
                                s.server_version = descriptor.server_version.clone();
                                s.machine = descriptor.machine();
                                s.client = Some(client.clone());
                                s.generation += 1;
                            });
                        });
                        if connected.is_err() {
                            return;
                        }
                        let reason = client.closed().await;
                        // A remote that accepts and then drops the socket must not be redialed in a loop.
                        let delay = environments::retry_delay_ms(0, jitter());
                        let dropped = this.update(cx, |this, cx| {
                            this.update_session(&id, cx, |s| {
                                s.client = None;
                                s.state = ConnectionState {
                                    phase: Phase::Backoff,
                                    attempt,
                                    retry_at: Some(retry_at(delay)),
                                    last_failure: Some(Failure::transport(reason)),
                                };
                            });
                        });
                        if dropped.is_err() {
                            return;
                        }
                        cx.background_executor().timer(Duration::from_millis(delay)).await;
                    }
                    Err(error) => {
                        let (failure, descriptor) = *error;
                        let blocks = failure.reason.blocks();
                        let delay = (!blocks).then(|| {
                            failures += 1;
                            environments::retry_delay_ms(failures - 1, jitter())
                        });
                        let failed = this.update(cx, |this, cx| {
                            this.update_session(&id, cx, |s| {
                                if let Some(descriptor) = &descriptor {
                                    s.server_version = descriptor.server_version.clone();
                                    s.machine = descriptor.machine();
                                }
                                s.state = ConnectionState {
                                    phase: if blocks { Phase::Blocked } else { Phase::Backoff },
                                    attempt,
                                    retry_at: delay.map(retry_at),
                                    last_failure: Some(failure),
                                };
                            });
                        });
                        let Some(delay) = delay else { return };
                        if failed.is_err() {
                            return;
                        }
                        cx.background_executor().timer(Duration::from_millis(delay)).await;
                    }
                }
            }
        })
    }
}

/// A uniform random number in 0..=1 for [`environments::retry_delay_ms`].
fn jitter() -> f64 {
    f64::from(getrandom::u32().unwrap_or(0)) / f64::from(u32::MAX)
}

/// When a retry `delay_ms` from now happens, in epoch ms.
fn retry_at(delay_ms: u64) -> i64 {
    crate::app::sidebar::now_ms().saturating_add(i64::try_from(delay_ms).unwrap_or(i64::MAX))
}

/// Why an attempt failed, with the descriptor when it was read. Boxed: the pair is large.
type ConnectError = Box<(Failure, Option<environments::Descriptor>)>;

/// One attempt: the descriptor (identity and protocol), then the socket. Runs on the tokio runtime.
async fn connect(environment: SavedEnvironment) -> Result<(RpcClient, environments::Descriptor), ConnectError> {
    let Some(endpoint) = environment.endpoint() else {
        return Err(Box::new((Failure::credential_missing(&environment), None)));
    };
    let descriptor =
        environments::fetch_descriptor(&endpoint.http_base).await.map_err(|failure| Box::new((failure, None)))?;
    if descriptor.environment_id != environment.environment_id {
        let failure = Failure::environment_mismatch(&environment.environment_id, &descriptor.environment_id);
        return Err(Box::new((failure, Some(descriptor))));
    }
    if let Some(failure) = descriptor.compatibility_error() {
        return Err(Box::new((failure, Some(descriptor))));
    }
    let options = ConnectOptions { ws_url: endpoint.ws_url(), bearer_token: endpoint.bearer_token.clone() };
    match RpcClient::connect(runtime().handle(), options).await {
        Ok(client) => Ok((client, descriptor)),
        Err(error) if rpc::is_unauthorized(&error) => {
            Err(Box::new((Failure::http(401, &endpoint.ws_url()), Some(descriptor))))
        }
        Err(error) => Err(Box::new((Failure::transport(format!("{error:#}")), Some(descriptor)))),
    }
}
