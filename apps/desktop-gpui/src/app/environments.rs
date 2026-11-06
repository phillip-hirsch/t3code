//! Saved remote environments and one background connection per enabled one
//! (`connection/supervisor.ts`): connecting, connected, backing off, or blocked until a retry.
//! The workspace runs each connected environment's streams on that socket (`super::sessions`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::channel::oneshot;
use gpui_kit::{Context, Subscription, Task};

use crate::environments::{self, Catalog, ConnectionState, Credential, Failure, Phase, SavedEnvironment};
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
    /// `None` until the file is read off the UI thread; nothing connects before.
    catalog: Option<Catalog>,
    /// The file could not be read, so saving would destroy it.
    read_only: bool,
    problem: Option<String>,
    writes: Writes,
    sessions: HashMap<String, Session>,
    /// The workspace's primary environment. Saved under the same id, it is that session
    /// (`EnvironmentRegistry.register` ignores platform environments), so it gets no socket here.
    primary: Option<String>,
    _load: Task<()>,
    _quit: Subscription,
}

/// Saves run one at a time off the UI thread. Edits made during one are written next as a
/// single catalog (newest wins), and the shown catalog changes only once its write succeeded.
#[derive(Default)]
struct Writes {
    writing: Option<Batch>,
    next: Option<Batch>,
    unsaved: Unsaved,
}

/// A catalog to write, and the edits waiting for it to be on disk.
struct Batch {
    version: u64,
    catalog: Catalog,
    waiters: Vec<oneshot::Sender<Result<(), String>>>,
}

/// A started write, to run off the UI thread.
struct Attempt {
    unsaved: Unsaved,
    version: u64,
    catalog: Catalog,
}

impl Attempt {
    fn run(&self, save: impl FnOnce(&Catalog) -> Result<(), String>) -> Result<(), String> {
        self.unsaved.write(self.version, &self.catalog, save)
    }
}

/// How long quitting waits for the last catalog write; the keyring may be slow or ask the user.
const QUIT_SAVE_WAIT: Duration = Duration::from_secs(2);

/// The newest queued catalog that no write has tried yet, shared with the quit handler, which
/// writes it. Writes take `writing`, so they never overlap and an older catalog never lands over
/// a newer one; the UI thread only takes `slot`, which no one holds across a write.
#[derive(Clone, Default)]
struct Unsaved(Arc<Shared>);

#[derive(Default)]
struct Shared {
    slot: Mutex<Slot>,
    writing: Mutex<()>,
}

#[derive(Default)]
struct Slot {
    newest: Option<(u64, Catalog)>,
    queued: u64,
    /// The newest version on disk.
    written: u64,
}

impl Unsaved {
    fn lock(&self) -> MutexGuard<'_, Slot> {
        self.0.slot.lock().expect("a catalog write panicked")
    }

    /// Records a queued catalog; returns its version.
    fn queue(&self, catalog: &Catalog) -> u64 {
        let mut slot = self.lock();
        slot.queued += 1;
        slot.newest = Some((slot.queued, catalog.clone()));
        slot.queued
    }

    /// Writes `catalog` unless the same or a newer version is on disk. Once tried, the quit
    /// handler leaves it alone: a failed write already answered its edits.
    fn write(
        &self,
        version: u64,
        catalog: &Catalog,
        save: impl FnOnce(&Catalog) -> Result<(), String>,
    ) -> Result<(), String> {
        let _writing = self.0.writing.lock().expect("a catalog write panicked");
        {
            let mut slot = self.lock();
            if slot.newest.as_ref().is_some_and(|(newest, _)| *newest <= version) {
                slot.newest = None;
            }
            if version <= slot.written {
                return Ok(());
            }
        }
        let result = save(catalog);
        if result.is_ok() {
            self.lock().written = version;
        }
        result
    }

    /// Writes the newest catalog no write has tried yet, after any write still running.
    fn flush(&self, save: impl FnOnce(&Catalog) -> Result<(), String>) {
        let pending = self.lock().newest.take();
        let Some((version, catalog)) = pending else {
            // Nothing newer is queued, but a running write may still be saving the last edit.
            drop(self.0.writing.lock().expect("a catalog write panicked"));
            return;
        };
        drop(self.write(version, &catalog, save));
    }

    /// Writes run in a detached task, which quitting would cut short.
    fn save_before_quit(&self) {
        let unsaved = self.clone();
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            unsaved.flush(write);
            // The quit handler may have stopped waiting.
            let _ = done.send(());
        });
        if finished.recv_timeout(QUIT_SAVE_WAIT).is_err() {
            log::warn!("saved environments were still being written when the app quit");
        }
    }
}

impl Writes {
    /// The newest catalog an edit made, if it is not on disk yet.
    fn newest(&self) -> Option<&Catalog> {
        self.next.as_ref().or(self.writing.as_ref()).map(|batch| &batch.catalog)
    }

    /// Queues the newest catalog; the receiver resolves once it is on disk.
    fn queue(&mut self, catalog: Catalog) -> oneshot::Receiver<Result<(), String>> {
        let (sender, receiver) = oneshot::channel();
        let version = self.unsaved.queue(&catalog);
        match &mut self.next {
            Some(next) => {
                next.version = version;
                next.catalog = catalog;
                next.waiters.push(sender);
            }
            None => self.next = Some(Batch { version, catalog, waiters: vec![sender] }),
        }
        receiver
    }

    /// The write to run now, unless one is running.
    fn start(&mut self) -> Option<Attempt> {
        if self.writing.is_some() {
            return None;
        }
        self.writing = self.next.take();
        let batch = self.writing.as_ref()?;
        Some(Attempt { unsaved: self.unsaved.clone(), version: batch.version, catalog: batch.catalog.clone() })
    }

    /// Records how the running write went; returns the catalog it put on disk.
    fn finish(&mut self, result: Result<(), String>) -> Option<Catalog> {
        let batch = self.writing.take()?;
        let (result, waiters) = match (result, &mut self.next) {
            // The next write carries these edits too, so they wait for it.
            (Err(_), Some(next)) => {
                next.waiters.extend(batch.waiters);
                return None;
            }
            (result, _) => (result, batch.waiters),
        };
        for waiter in waiters {
            drop(waiter.send(result.clone()));
        }
        result.is_ok().then_some(batch.catalog)
    }
}

const GONE: &str = "The environment list is no longer available.";

/// An edit's save: resolves once the edit is on disk, or with the message to show.
#[must_use = "an edit can still fail to save"]
pub(crate) struct Saving(Result<oneshot::Receiver<Result<(), String>>, String>);

impl Saving {
    pub async fn finished(self) -> Result<(), String> {
        match self.0 {
            Ok(receiver) => receiver.await.unwrap_or_else(|_| Err(GONE.into())),
            Err(error) => Err(error),
        }
    }
}

impl SavedEnvironments {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let load = cx.spawn(async move |this, cx| {
            let opened = cx.background_executor().spawn(async { Catalog::open() }).await;
            drop(this.update(cx, |this, cx| {
                this.read_only = opened.read_only;
                this.problem = opened.problem;
                this.commit(opened.catalog, cx);
            }));
        });
        Self {
            catalog: None,
            read_only: false,
            problem: None,
            writes: Writes::default(),
            sessions: HashMap::new(),
            primary: None,
            _load: load,
            _quit: cx.on_app_quit(|this, _| {
                this.writes.unsaved.save_before_quit();
                async {}
            }),
        }
    }

    /// Empty until the catalog is read.
    pub fn environments(&self) -> &[SavedEnvironment] {
        self.catalog.as_ref().map_or(&[], |catalog| &catalog.environments)
    }

    fn get(&self, environment_id: &str) -> Option<&SavedEnvironment> {
        self.catalog.as_ref()?.get(environment_id)
    }

    /// The catalog is still being read.
    pub fn loading(&self) -> bool {
        self.catalog.is_none()
    }

    /// Edits can be saved: the catalog is read, and saving it destroys nothing.
    pub fn editable(&self) -> bool {
        self.catalog.is_some() && !self.read_only
    }

    /// Why the saved catalog could not be read, for Settings › Environments.
    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
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

    /// `None` until the catalog is read.
    pub fn local_environment_enabled(&self) -> Option<bool> {
        Some(self.catalog.as_ref()?.local_environment_enabled)
    }

    /// Saves a paired environment and connects it (`registerPairingConnection`). Lists it only
    /// once it is saved.
    pub fn add(&mut self, environment: SavedEnvironment, cx: &mut Context<Self>) -> Saving {
        self.edit(cx, |catalog| {
            catalog.upsert(environment);
            Ok(())
        })
    }

    pub fn set_enabled(&mut self, environment_id: &str, enabled: bool, cx: &mut Context<Self>) -> Saving {
        self.edit(cx, |catalog| {
            if let Some(environment) = catalog.get_mut(environment_id) {
                environment.enabled = enabled;
            }
            Ok(())
        })
    }

    pub fn rename(&mut self, environment_id: &str, label: &str, cx: &mut Context<Self>) -> Saving {
        self.edit(cx, |catalog| {
            let label = environments::validate_label(label)?;
            if let Some(environment) = catalog.get_mut(environment_id) {
                environment.label = label;
            }
            Ok(())
        })
    }

    /// Forgets the pairing and credential here; the session on that machine stays until revoked there.
    pub fn remove(&mut self, environment_id: &str, cx: &mut Context<Self>) -> Saving {
        self.edit(cx, |catalog| {
            catalog.environments.retain(|environment| environment.environment_id != environment_id);
            Ok(())
        })
    }

    pub fn set_local_environment_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) -> Saving {
        self.edit(cx, |catalog| {
            catalog.local_environment_enabled = enabled;
            Ok(())
        })
    }

    /// Applies `change` to the newest catalog and queues its write. The shown catalog and the
    /// connections follow once it is on disk, so a failed save changes nothing.
    fn edit(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Catalog) -> Result<(), &str>) -> Saving {
        if self.read_only {
            return Saving(Err(self.problem.clone().unwrap_or_else(|| "Saved environments are read-only.".into())));
        }
        let Some(newest) = self.writes.newest().or(self.catalog.as_ref()) else {
            return Saving(Err("Saved environments are still loading.".into()));
        };
        let mut catalog = newest.clone();
        if let Err(error) = change(&mut catalog) {
            return Saving(Err(error.to_owned()));
        }
        let receiver = self.writes.queue(catalog);
        self.write_next(cx);
        Saving(Ok(receiver))
    }

    fn write_next(&mut self, cx: &mut Context<Self>) {
        let Some(attempt) = self.writes.start() else { return };
        // Detached: a started write finishes even if the list is dropped meanwhile.
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { attempt.run(write) }).await;
            drop(this.update(cx, |this, cx| {
                if let Some(saved) = this.writes.finish(result) {
                    this.commit(saved, cx);
                }
                this.write_next(cx);
            }));
        })
        .detach();
    }

    /// Shows a catalog that is on disk, restarting the connections it changed.
    fn commit(&mut self, catalog: Catalog, cx: &mut Context<Self>) {
        let changed = changed_connections(self.catalog.as_ref(), &catalog);
        self.catalog = Some(catalog);
        for (id, saved) in changed {
            if saved {
                self.restart(&id, cx);
            } else {
                self.sessions.remove(&id);
            }
        }
        cx.notify();
    }

    /// Starts an environment's connection loop afresh (startup, switched on, paired again).
    pub fn restart(&mut self, environment_id: &str, cx: &mut Context<Self>) {
        let Some(environment) = self.get(environment_id).cloned() else { return };
        let session = self.sessions.entry(environment_id.to_owned()).or_default();
        session.state = ConnectionState::default();
        session.client = None;
        let own = environment.enabled && self.primary.as_deref().is_none_or(|id| id != environment_id);
        session._task = own.then(|| Self::supervise(environment, 0, false, cx));
        cx.notify();
    }

    /// `retryNow`: attempts at once, keeping the failure and attempt count on screen meanwhile.
    /// A credential the keyring would not open is decrypted again first.
    pub fn retry(&mut self, environment_id: &str, cx: &mut Context<Self>) {
        let Some(environment) = self.get(environment_id).cloned().filter(|e| e.enabled) else { return };
        if self.is_primary(environment_id) {
            return;
        }
        let Some(session) = self.sessions.get_mut(environment_id) else { return };
        let failures = session.state.attempt;
        session._task = Some(Self::supervise(environment, failures, true, cx));
    }

    /// Puts a credential decrypted again in place of the locked one it came from, here and
    /// in queued writes, so a write that finishes later does not lock it again.
    fn reopened(&mut self, environment_id: &str, locked: &Credential, opened: &Credential, cx: &mut Context<Self>) {
        let queued = [&mut self.writes.writing, &mut self.writes.next].into_iter().flatten().map(|b| &mut b.catalog);
        for catalog in self.catalog.iter_mut().chain(queued) {
            if let Some(environment) = catalog.get_mut(environment_id)
                && environment.credential == *locked
            {
                environment.credential = opened.clone();
            }
        }
        cx.notify();
    }

    fn update_session(&mut self, environment_id: &str, cx: &mut Context<Self>, update: impl FnOnce(&mut Session)) {
        if let Some(session) = self.sessions.get_mut(environment_id) {
            update(session);
            cx.notify();
        }
    }

    /// `reopen` first decrypts a locked credential again, off the UI thread.
    fn supervise(mut environment: SavedEnvironment, failures: u32, reopen: bool, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let id = environment.environment_id.clone();
            if reopen && matches!(environment.credential, Credential::Locked { .. }) {
                let locked = environment.credential.clone();
                let opened = cx.background_executor().spawn(async move { locked.reopen() }).await;
                if let Some(opened) = opened {
                    let locked = std::mem::replace(&mut environment.credential, opened);
                    let updated = this.update(cx, |this, cx| this.reopened(&id, &locked, &environment.credential, cx));
                    if updated.is_err() {
                        return;
                    }
                }
            }
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

/// The environments whose connection `current` changes from `previous`: switched, readdressed,
/// given another credential, added (`true`) or removed (`false`). A rename changes none.
fn changed_connections(previous: Option<&Catalog>, current: &Catalog) -> Vec<(String, bool)> {
    fn route(environment: &SavedEnvironment) -> (bool, &str, &Credential) {
        (environment.enabled, &environment.http_base, &environment.credential)
    }
    let before = |id: &str| previous.and_then(|catalog| catalog.get(id)).map(route);
    let changed = current
        .environments
        .iter()
        .filter(|environment| before(&environment.environment_id) != Some(route(environment)))
        .map(|environment| (environment.environment_id.clone(), true));
    let removed = previous
        .into_iter()
        .flat_map(|catalog| &catalog.environments)
        .filter(|environment| current.get(&environment.environment_id).is_none())
        .map(|environment| (environment.environment_id.clone(), false));
    changed.chain(removed).collect()
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

/// Writes the catalog; the error is the message to show. Blocks on the file and the keyring.
fn write(catalog: &Catalog) -> Result<(), String> {
    catalog.save().map_err(|error| {
        log::warn!("could not save environments: {error:#}");
        format!("Could not save environments: {error:#}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(id: &str, enabled: bool) -> SavedEnvironment {
        SavedEnvironment {
            environment_id: id.into(),
            label: id.into(),
            http_base: format!("https://{id}"),
            enabled,
            credential: Credential::Token(format!("token-{id}")),
        }
    }

    fn catalog(environments: &[SavedEnvironment]) -> Catalog {
        let mut catalog = Catalog::default();
        catalog.environments = environments.to_vec();
        catalog
    }

    fn labels(catalog: &Catalog) -> Vec<&str> {
        catalog.environments.iter().map(|environment| environment.label.as_str()).collect()
    }

    #[test]
    fn writes_run_one_at_a_time_and_the_newest_edit_wins() {
        let mut writes = Writes::default();
        let mut first = writes.queue(catalog(&[environment("a", true)]));
        let started = writes.start().unwrap();
        assert_eq!(labels(&started.catalog), ["a"]);

        // Two edits while it runs: only the newest is written next, for both.
        let mut second = writes.queue(catalog(&[environment("a", true), environment("b", true)]));
        let mut third = writes.queue(catalog(&[environment("c", true)]));
        assert_eq!(labels(writes.newest().unwrap()), ["c"]);
        assert!(writes.start().is_none(), "one write at a time");

        // The first write fails, but the next one carries its edit, so it waits for that.
        assert!(writes.finish(Err("disk full".into())).is_none());
        assert_eq!(first.try_recv(), Ok(None));
        assert_eq!(labels(&writes.start().unwrap().catalog), ["c"]);
        assert!(writes.start().is_none());
        assert_eq!(labels(&writes.finish(Ok(())).unwrap()), ["c"]);
        for receiver in [&mut first, &mut second, &mut third] {
            assert_eq!(receiver.try_recv(), Ok(Some(Ok(()))));
        }
        assert!(writes.newest().is_none() && writes.start().is_none());

        // A failure with nothing queued after it is the edit's answer, and shows nothing new.
        let mut failed = writes.queue(catalog(&[]));
        writes.start().unwrap();
        assert!(writes.finish(Err("denied".into())).is_none());
        assert_eq!(failed.try_recv(), Ok(Some(Err("denied".into()))));
        assert!(writes.newest().is_none());
    }

    /// What quitting writes: the newest catalog no write has tried, never an older one after it.
    #[test]
    fn quitting_writes_the_newest_queued_catalog_once() {
        let log = std::cell::RefCell::new(Vec::new());
        let saved = &log;
        let save = move |result: Result<(), String>| {
            move |catalog: &Catalog| {
                saved.borrow_mut().push(labels(catalog).join(","));
                result
            }
        };
        let mut writes = Writes::default();
        drop(writes.queue(catalog(&[environment("a", true)])));
        let first = writes.start().unwrap();
        drop(writes.queue(catalog(&[environment("b", true)])));
        drop(writes.queue(catalog(&[environment("c", true)])));

        // The running write finishes, then quitting writes the queued one.
        assert_eq!(first.run(save(Ok(()))), Ok(()));
        writes.unsaved.flush(save(Ok(())));
        assert_eq!(*saved.borrow(), ["a", "c"]);

        // Its own write, starting after, does not write it again; nothing is left to flush.
        writes.finish(Ok(()));
        assert_eq!(writes.start().unwrap().run(save(Err("unexpected".into()))), Ok(()));
        writes.unsaved.flush(save(Err("unexpected".into())));
        assert_eq!(*saved.borrow(), ["a", "c"]);

        // A write still running when quitting flushed a newer one leaves the newer on disk.
        writes.finish(Ok(()));
        drop(writes.queue(catalog(&[environment("d", true)])));
        let stale = writes.start().unwrap();
        drop(writes.queue(catalog(&[environment("e", true)])));
        writes.unsaved.flush(save(Ok(())));
        assert_eq!(stale.run(save(Ok(()))), Ok(()));
        assert_eq!(*saved.borrow(), ["a", "c", "e"]);

        // A failed write already answered its edit, so quitting does not retry it.
        writes.finish(Ok(()));
        writes.start().unwrap();
        writes.finish(Ok(()));
        drop(writes.queue(catalog(&[environment("f", true)])));
        assert!(writes.start().unwrap().run(save(Err("denied".into()))).is_err());
        writes.unsaved.flush(save(Ok(())));
        assert_eq!(*saved.borrow(), ["a", "c", "e", "f"]);
    }

    #[test]
    fn queueing_never_waits_behind_a_running_write() {
        use std::sync::mpsc;

        let unsaved = Unsaved::default();
        let first = unsaved.queue(&catalog(&[environment("a", true)]));
        let (entered, save_started) = mpsc::channel();
        let (release, gate) = mpsc::channel::<()>();
        let writer = {
            let unsaved = unsaved.clone();
            let a = catalog(&[environment("a", true)]);
            std::thread::spawn(move || {
                unsaved.write(first, &a, |_| {
                    entered.send(()).expect("the test is waiting");
                    // Stuck, as on a keyring prompt, until the test lets it finish.
                    gate.recv().expect("the test releases the write");
                    Ok(())
                })
            })
        };
        save_started.recv().expect("the write starts");

        // The UI thread queues another edit while the save is stuck.
        let (queued, second) = mpsc::channel();
        let queuer = {
            let unsaved = unsaved.clone();
            std::thread::spawn(move || {
                queued.send(unsaved.queue(&catalog(&[environment("b", true)]))).expect("the test is waiting");
            })
        };
        let second = second.recv_timeout(Duration::from_secs(10)).expect("queueing waited behind the write");
        release.send(()).expect("the write is waiting");
        assert_eq!(writer.join().expect("the writer thread"), Ok(()));
        queuer.join().expect("the queueing thread");

        // The newest catalog is the one left to write.
        let mut written = Vec::new();
        unsaved.flush(|catalog| {
            written.push(labels(catalog).join(","));
            Ok(())
        });
        assert_eq!((second, written), (2, vec!["b".to_owned()]));
    }

    #[test]
    fn quitting_waits_for_a_running_write_even_with_nothing_newer_queued() {
        use std::sync::mpsc;

        let unsaved = Unsaved::default();
        let only = unsaved.queue(&catalog(&[environment("a", true)]));
        let (entered, save_started) = mpsc::channel();
        let (release, gate) = mpsc::channel::<()>();
        let writer = {
            let unsaved = unsaved.clone();
            let a = catalog(&[environment("a", true)]);
            std::thread::spawn(move || {
                unsaved.write(only, &a, |_| {
                    entered.send(()).expect("the test is waiting");
                    gate.recv().expect("the test releases the write");
                    Ok(())
                })
            })
        };
        save_started.recv().expect("the write starts");

        // Quitting flushes while that write is stuck; it must not report done before the write ends.
        let (flushed, done) = mpsc::channel();
        let flusher = {
            let unsaved = unsaved.clone();
            std::thread::spawn(move || {
                unsaved.flush(|_| Err("nothing newer should be written".into()));
                flushed.send(()).expect("the test is waiting");
            })
        };
        // A flush that skips the wait returns at once; one that waits is still blocked after this window.
        assert!(
            done.recv_timeout(Duration::from_millis(500)).is_err(),
            "flush returned while a write was still saving"
        );
        // Queueing stays independent of the stuck write.
        let next = unsaved.queue(&catalog(&[environment("b", true)]));
        release.send(()).expect("the write is waiting");
        assert_eq!(writer.join().expect("the writer thread"), Ok(()));
        done.recv().expect("flush finishes once the write ends");
        flusher.join().expect("the flushing thread");
        assert_eq!(next, 2);
    }

    #[test]
    fn only_connection_changes_restart_a_connection() {
        let a = environment("a", true);
        let before = catalog(&[a.clone(), environment("b", true), environment("c", false)]);
        assert_eq!(changed_connections(None, &before), [("a".into(), true), ("b".into(), true), ("c".into(), true)]);

        let renamed = SavedEnvironment { label: "Renamed".into(), ..a.clone() };
        let repaired = SavedEnvironment { credential: Credential::Token("new".into()), ..environment("b", true) };
        let after = catalog(&[renamed, repaired, environment("c", true), environment("d", true)]);
        assert_eq!(
            changed_connections(Some(&before), &after),
            [("b".into(), true), ("c".into(), true), ("d".into(), true)]
        );
        assert_eq!(changed_connections(Some(&before), &catalog(&[a])), [("b".into(), false), ("c".into(), false)]);
    }
}
