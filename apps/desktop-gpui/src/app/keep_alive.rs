//! Running threads stay synced in the background, as the desktop app's
//! `RunningThreadKeepAlive` (`apps/web/src/state/threads.ts`) keeps their detail streams
//! mounted: opening one resumes from its sequence instead of downloading a snapshot. A thread
//! that stopped stays until its own stream is live and shows no running run.
//!
//! These streams update state off screen, so they never repaint the window.

use std::collections::HashMap;

use gpui_kit::{Context, Task};
use serde_json::json;

use super::Workspace;
use crate::model::ThreadState;
use crate::rpc::StreamEvent;

#[derive(Default)]
pub(super) struct KeepAlive {
    threads: HashMap<String, Warm>,
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
    state.deleted || (state.live && !state.projection.as_ref().is_some_and(|p| p.is_running()))
}

impl Workspace {
    fn shell_running(&self, thread_id: &str) -> bool {
        self.shell.threads.iter().any(|t| t.id == thread_id && running(&t.status))
    }

    /// After each shell batch: mount every running thread that is not on screen, drop the
    /// ones that settled.
    pub(super) fn keep_running_threads(&mut self, cx: &mut Context<Self>) {
        let open = self.thread.as_ref().map(|t| t.id.clone());
        let running: Vec<String> = self
            .shell
            .threads
            .iter()
            .filter(|t| running(&t.status) && Some(&t.id) != open.as_ref())
            .map(|t| t.id.clone())
            .collect();
        self.keep_alive.threads.retain(|id, warm| running.contains(id) || !settled(&warm.state));
        for id in running {
            if !self.keep_alive.threads.contains_key(&id) {
                self.keep_warm(id, ThreadState::default(), cx);
            }
        }
    }

    /// Keeps a thread leaving the screen synced while it still runs.
    pub(super) fn retire_thread(&mut self, thread_id: String, state: ThreadState, cx: &mut Context<Self>) {
        if self.shell_running(&thread_id) || (state.projection.is_some() && !settled(&state)) {
            self.keep_warm(thread_id, state, cx);
        }
    }

    /// The background state of a thread about to open, to resume from.
    pub(super) fn take_warm_thread(&mut self, thread_id: &str) -> Option<ThreadState> {
        self.keep_alive.threads.remove(thread_id).map(|warm| warm.state)
    }

    fn keep_warm(&mut self, thread_id: String, state: ThreadState, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let mut input = json!({"threadId": thread_id, "requestCompletionMarker": true});
        if state.sequence > 0 {
            input["afterSequence"] = state.sequence.into();
        }
        let stream = client.stream("orchestration.subscribeThread", input);
        let id = thread_id.clone();
        self.keep_alive.generation += 1;
        let generation = self.keep_alive.generation;
        let task = cx.spawn(async move |this, cx| {
            while let Some(first) = stream.next().await {
                let mut events = vec![first];
                events.extend(std::iter::from_fn(|| stream.try_next()));
                let done = this.update(cx, |this, _| {
                    let shell_running = this.shell_running(&id);
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
        self.keep_alive.threads.insert(thread_id, Warm { state, generation, _stream: Some(task) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_settles_once_its_own_stream_is_live_without_a_run() {
        let mut state = ThreadState::default();
        assert!(!settled(&state), "not live yet");
        state.apply(&json!({"kind": "synchronized"})).unwrap();
        assert!(settled(&state));
        assert!(running("starting") && !running("waiting") && !running("idle"));
    }
}
