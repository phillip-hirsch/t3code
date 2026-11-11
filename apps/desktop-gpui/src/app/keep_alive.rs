//! Running threads stay synced in the background, as the desktop app's
//! `RunningThreadKeepAlive` (`apps/web/src/state/threads.ts`) keeps their detail streams
//! mounted: opening one resumes from its sequence instead of downloading a snapshot. A thread
//! that stopped stays until its own stream is live and shows no running run.
//!
//! These streams update state off screen, so they never repaint the window.

use std::collections::HashMap;

use gpui_kit::{Context, SharedString, Task};
use serde_json::json;

use super::{Target, Workspace};
use crate::model::{ScopedId, ThreadShell, ThreadState};
use crate::rpc::StreamEvent;

#[derive(Default)]
pub(super) struct KeepAlive {
    /// By environment and thread: an imported thread keeps its id on another environment.
    threads: HashMap<ScopedId, Warm>,
    /// Tells a finished stream from the one that replaced it.
    generation: u64,
}

struct Warm {
    state: ThreadState,
    generation: u64,
    _stream: Option<Task<()>>,
}

/// `isRunning` on a shell's status.
fn running(status: &str) -> bool {
    matches!(status, "preparing" | "queued" | "starting" | "running")
}

/// `isDetailDone`: the thread's own stream is live and has no run in flight.
fn settled(state: &ThreadState) -> bool {
    state.deleted
        || (state.live && !state.projection.as_ref().is_some_and(super::super::model::ThreadProjection::is_running))
}

/// The shell lists `key` as running.
fn shell_running(threads: &[ThreadShell], key: &ScopedId) -> bool {
    threads.iter().any(|t| key.is_thread(t) && running(&t.status))
}

/// The running threads besides the open one, which keep-alive mounts.
fn running_off_screen(threads: &[ThreadShell], open: Option<&ScopedId>) -> Vec<ScopedId> {
    threads
        .iter()
        .filter(|t| running(&t.status) && !open.is_some_and(|open| open.is_thread(t)))
        .map(ScopedId::of_thread)
        .collect()
}

impl Workspace {
    /// After each shell batch: mount every running thread that is not on screen, drop the
    /// ones that settled.
    pub(super) fn keep_running_threads(&mut self, cx: &mut Context<Self>) {
        let open = self.thread.as_ref().and(self.target.as_ref()).and_then(Target::thread_key);
        let running = running_off_screen(&self.shell.threads, open.as_ref());
        self.keep_alive.threads.retain(|key, warm| running.contains(key) || !settled(&warm.state));
        for key in running {
            if !self.keep_alive.threads.contains_key(&key) {
                self.keep_warm(key, ThreadState::default(), cx);
            }
        }
    }

    /// Keeps a thread leaving the screen synced on `environment`, its own, while it still runs.
    pub(super) fn retire_thread(
        &mut self,
        environment: &str,
        thread_id: String,
        state: ThreadState,
        cx: &mut Context<Self>,
    ) {
        let key = ScopedId::new(SharedString::from(environment.to_owned()), thread_id);
        if shell_running(&self.shell.threads, &key) || (state.projection.is_some() && !settled(&state)) {
            self.keep_warm(key, state, cx);
        }
    }

    /// The background state of a thread about to open, to resume from.
    pub(super) fn take_warm_thread(&mut self, key: &ScopedId) -> Option<ThreadState> {
        self.keep_alive.threads.remove(key).map(|warm| warm.state)
    }

    fn keep_warm(&mut self, key: ScopedId, state: ThreadState, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(&key.env) else { return };
        let mut input = json!({"threadId": key.id, "requestCompletionMarker": true});
        if state.sequence > 0 {
            input["afterSequence"] = state.sequence.into();
        }
        let stream = client.stream("orchestration.subscribeThread", input);
        let id = key.clone();
        self.keep_alive.generation += 1;
        let generation = self.keep_alive.generation;
        let task = cx.spawn(async move |this, cx| {
            while let Some(first) = stream.next().await {
                let mut events = vec![first];
                events.extend(std::iter::from_fn(|| stream.try_next()));
                let done = this.update(cx, |this, _| {
                    let shell_running = shell_running(&this.shell.threads, &id);
                    let Some(warm) = this.keep_alive.threads.get_mut(&id).filter(|w| w.generation == generation) else {
                        return true;
                    };
                    for event in events {
                        match event {
                            StreamEvent::Item(value) => {
                                // A bad snapshot leaves nothing to resume from; opening fetches one.
                                if warm.state.apply(&value).is_err() {
                                    return true;
                                }
                            }
                            StreamEvent::End(_) => return true,
                        }
                    }
                    !shell_running && settled(&warm.state)
                });
                if done.unwrap_or(true) {
                    break;
                }
            }
            let _ = this.update(cx, |this, _| {
                if this.keep_alive.threads.get(&id).is_some_and(|w| w.generation == generation) {
                    this.keep_alive.threads.remove(&id);
                }
            });
        });
        self.keep_alive.threads.insert(key, Warm { state, generation, _stream: Some(task) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(env: &str, id: &str, status: &str) -> ThreadShell {
        let mut thread: ThreadShell = serde_json::from_value(json!({
            "id": id, "projectId": "p", "title": id, "status": status,
            "updatedAt": "2026-01-01T00:00:00.000Z", "createdAt": "2026-01-01T00:00:00.000Z",
        }))
        .unwrap();
        thread.environment_id = env.to_owned().into();
        thread
    }

    #[test]
    fn a_running_thread_stays_warm_while_another_environments_same_id_is_open() {
        let threads = [thread("a", "t", "running"), thread("b", "t", "running"), thread("b", "u", "idle")];
        let open = ScopedId::new("b", "t");
        assert_eq!(running_off_screen(&threads, Some(&open)), [ScopedId::new("a", "t")]);
        assert_eq!(running_off_screen(&threads, None).len(), 2);
        assert!(shell_running(&threads, &ScopedId::new("a", "t")));
        assert!(!shell_running(&threads, &ScopedId::new("c", "t")));
    }

    #[test]
    fn a_thread_settles_once_its_own_stream_is_live_without_a_run() {
        let mut state = ThreadState::default();
        assert!(!settled(&state), "not live yet");
        state.apply(&json!({"kind": "synchronized"})).unwrap();
        assert!(settled(&state));
        assert!(running("starting") && !running("waiting") && !running("idle"));
    }
}
