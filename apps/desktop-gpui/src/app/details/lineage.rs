//! The relationships Thread details lists (`ThreadRelationshipsControl.tsx`): the parent, the
//! subagents and the side chats, including the ones the shell leaves out, and a running
//! subagent's Stop.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{Context, SharedString, Task, Window};
use serde_json::{Value, json};

use crate::app::{Target, Workspace, new_id};
use crate::model::{Subagent, ThreadShell};

#[derive(Default)]
pub(in crate::app) struct LineageUi {
    /// `orchestration.getThreadTitles` answers for related threads the shell leaves out, by
    /// `environment:thread`.
    titles: HashMap<String, Lookup>,
    fetching: HashSet<String>,
    _fetches: Vec<Task<()>>,
    /// The child thread a Stop is in flight for.
    pub(super) stopping: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Lookup {
    Found(SharedString),
    /// The server answered without it: the thread is gone.
    Gone,
    /// Only an answer proves a thread gone, so it stays listed.
    Failed,
}

/// One row: a related thread, by what Thread details shows of it.
#[derive(Debug, PartialEq)]
pub(super) struct Relative {
    pub thread_id: String,
    pub title: SharedString,
    pub status: Option<String>,
    pub stop: Option<SubagentStop>,
}

/// What a subagent's Stop needs (`interruptThreadTurn` with a `subagentId`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SubagentStop {
    subagent_id: String,
    owner_thread_id: String,
    run_id: Option<String>,
    /// An app-owned task's child thread, stopped too; the parent ends the task even when the
    /// child lost track of it.
    child: Option<(String, Option<String>)>,
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Relationships {
    pub parent: Option<Relative>,
    pub active: Vec<Relative>,
    pub previous: Vec<Relative>,
    pub side_chats: Vec<Relative>,
}

fn is_work_active(status: &str) -> bool {
    matches!(status, "pending" | "running" | "waiting")
}

fn activity(thread: &ThreadShell) -> Option<&str> {
    thread.activity_run_status.as_ref().and_then(Option::as_deref)
}

/// `isFinishedAgentStatus`.
fn is_finished(status: Option<&str>) -> bool {
    status
        .is_some_and(|status| matches!(status, "completed" | "failed" | "error" | "cancelled" | "interrupted" | "idle"))
}

/// Whether a subagent row offers Stop: a started task the app can end, read through
/// `currentSubagent` when the child thread runs again after the task settled.
fn can_stop(agent: &Subagent, child: Option<&ThreadShell>) -> bool {
    let native_active = is_work_active(&agent.status);
    let strong_native_ref =
        agent.native_task_ref.as_ref().is_some_and(|task| task.strength == "strong" && task.native_id.is_some());
    let stoppable = agent.origin == "app_owned"
        || (agent.origin == "provider_native"
            && agent.driver == "claudeAgent"
            && agent.run_id.is_some()
            && strong_native_ref
            && native_active);
    let (active, started) = if native_active {
        (true, agent.started_at.is_some())
    } else {
        let live = child.and_then(|child| {
            activity(child)
                .map(|status| (status, child.activity_run_started_at.as_ref().and_then(Option::as_ref).is_some()))
                .or_else(|| (child.status == "queued").then_some(("pending", false)))
        });
        match live {
            Some((status, started)) => {
                let status = if matches!(status, "preparing" | "starting" | "queued") { "pending" } else { status };
                (is_work_active(status), started)
            }
            None => (false, false),
        }
    };
    stoppable && active && started
}

/// `deriveThreadRelationshipGraph` and `immediateThreadRelationships` for `current`, grouped
/// as Thread details shows them. Idle subagent children the shell leaves out come from the
/// projection; an unlisted parent's title from `titles`.
pub(super) fn relationships(
    current: &str,
    parent_id: Option<&str>,
    threads: &[ThreadShell],
    subagents: &[Subagent],
    titles: impl Fn(&str) -> Option<Lookup>,
) -> Relationships {
    let listed = |id: &str| threads.iter().find(|thread| thread.id == id);
    let mut relationships = Relationships::default();
    if let Some(parent_id) = parent_id {
        let thread = listed(parent_id);
        let lookup = thread.is_none().then(|| titles(parent_id)).flatten();
        let parent_is_agent =
            listed(current).is_some_and(|thread| thread.lineage.relationship_to_parent.as_deref() == Some("subagent"));
        // Only a settled lookup hides an unlisted parent; a parent agent always stays.
        if parent_is_agent || lookup != Some(Lookup::Gone) {
            let title = match (&lookup, thread) {
                (Some(Lookup::Found(title)), _) => title.clone(),
                (_, Some(thread)) => thread.title.clone().into(),
                _ => parent_id.to_owned().into(),
            };
            let status = thread.and_then(|thread| activity(thread).or(Some(thread.status.as_str())));
            relationships.parent = Some(Relative {
                thread_id: parent_id.to_owned(),
                title,
                status: status.map(str::to_owned),
                stop: None,
            });
        }
    }
    let mut seen = HashSet::new();
    let children = threads.iter().filter(|thread| thread.lineage.parent_thread_id.as_deref() == Some(current));
    for child in children {
        let side_chat = child.lineage.side_chat == Some(true);
        if !side_chat && child.lineage.relationship_to_parent.as_deref() != Some("subagent") {
            continue;
        }
        seen.insert(child.id.as_str());
        let agent = subagents.iter().find(|agent| agent.child_thread_id.as_deref() == Some(child.id.as_str()));
        let status = activity(child).or_else(|| match agent {
            Some(agent) if child.latest_run_id.is_none() || is_work_active(&agent.status) => {
                Some(agent.status.as_str())
            }
            _ => Some(child.status.as_str()),
        });
        let row = Relative {
            thread_id: child.id.clone(),
            title: child.title.clone().into(),
            status: status.map(str::to_owned),
            stop: agent
                .filter(|agent| !side_chat && can_stop(agent, Some(child)))
                .map(|agent| stop_of(agent, Some(child))),
        };
        match () {
            () if side_chat => relationships.side_chats.push(row),
            () if is_finished(row.status.as_deref()) => relationships.previous.push(row),
            () => relationships.active.push(row),
        }
    }
    for agent in subagents {
        let Some(child_id) = agent.child_thread_id.as_deref() else { continue };
        if seen.contains(child_id) || Some(child_id) == parent_id {
            continue;
        }
        let title = agent
            .title
            .clone()
            .map(SharedString::from)
            .or_else(|| match titles(child_id) {
                Some(Lookup::Found(title)) => Some(title),
                _ => None,
            })
            .unwrap_or_else(|| child_id.to_owned().into());
        let row = Relative {
            thread_id: child_id.to_owned(),
            title,
            status: Some(agent.status.clone()),
            stop: can_stop(agent, None).then(|| stop_of(agent, None)),
        };
        if is_finished(row.status.as_deref()) {
            relationships.previous.push(row);
        } else {
            relationships.active.push(row);
        }
    }
    relationships
}

fn stop_of(agent: &Subagent, child: Option<&ThreadShell>) -> SubagentStop {
    SubagentStop {
        subagent_id: agent.id.clone(),
        owner_thread_id: agent.thread_id.clone(),
        run_id: agent.run_id.clone(),
        child: (agent.origin == "app_owned")
            .then(|| agent.child_thread_id.clone().map(|id| (id, child.and_then(|c| c.active_run_id.clone()))))
            .flatten(),
    }
}

/// The related threads whose titles only `orchestration.getThreadTitles` knows.
fn unlisted(current: &str, parent_id: Option<&str>, threads: &[ThreadShell], subagents: &[Subagent]) -> Vec<String> {
    let listed = |id: &str| threads.iter().any(|thread| thread.id == id);
    let children = subagents
        .iter()
        .filter(|agent| agent.title.is_none())
        .filter_map(|agent| agent.child_thread_id.as_deref())
        .filter(|id| *id != current);
    parent_id.into_iter().chain(children).filter(|id| !listed(id)).map(str::to_owned).collect()
}

impl Workspace {
    /// The open thread's relationships as Thread details lists them.
    pub(super) fn open_relationships(&self) -> Option<Relationships> {
        let Some(Target::Thread { env, id }) = &self.target else { return None };
        let thread = self.shell.threads.iter().find(|thread| &thread.id == id)?;
        let parent = thread.lineage.parent_thread_id.as_deref();
        let subagents = self.open_projection().map_or(&[][..], |p| p.subagents.as_slice());
        let titles = &self.lineage.titles;
        Some(relationships(id, parent, &self.shell.threads, subagents, |thread_id| {
            titles.get(&format!("{env}:{thread_id}")).cloned()
        }))
    }

    /// Reads the titles of related threads the shell leaves out, once each.
    pub(in crate::app) fn sync_lineage_titles(&mut self, cx: &mut Context<Self>) {
        let Some(Target::Thread { env, id }) = &self.target else { return };
        let Some(thread) = self.shell.threads.iter().find(|thread| &thread.id == id) else { return };
        let parent = thread.lineage.parent_thread_id.as_deref();
        let subagents = self.open_projection().map_or(&[][..], |p| p.subagents.as_slice());
        let wanted: Vec<String> = unlisted(id, parent, &self.shell.threads, subagents)
            .into_iter()
            .filter(|thread_id| {
                let key = format!("{env}:{thread_id}");
                !self.lineage.titles.contains_key(&key) && !self.lineage.fetching.contains(&key)
            })
            .collect();
        let Some(client) = self.client().filter(|_| !wanted.is_empty()) else { return };
        let env = env.clone();
        let keys: Vec<String> = wanted.iter().map(|thread_id| format!("{env}:{thread_id}")).collect();
        self.lineage.fetching.extend(keys.iter().cloned());
        let task = cx.spawn(async move |this, cx| {
            let answer = client.request("orchestration.getThreadTitles", json!({"threadIds": wanted})).await;
            let _ = this.update(cx, |this, cx| {
                for (key, thread_id) in keys.iter().zip(&wanted) {
                    this.lineage.fetching.remove(key);
                    let lookup = match &answer {
                        Ok(titles) => match titles[thread_id].as_str() {
                            Some(title) => Lookup::Found(title.to_owned().into()),
                            None => Lookup::Gone,
                        },
                        Err(error) => {
                            log::warn!("orchestration.getThreadTitles: {error}");
                            Lookup::Failed
                        }
                    };
                    this.lineage.titles.insert(key.clone(), lookup);
                }
                this.lineage._fetches.retain(|task| !task.is_ready());
                cx.notify();
            });
        });
        self.lineage._fetches.push(task);
    }

    /// `stopSubagent`: ends the task on its owner thread, and an app-owned task's child thread.
    pub(super) fn stop_subagent(
        &mut self,
        child_thread_id: String,
        stop: SubagentStop,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.lineage.stopping.is_some() {
            return;
        }
        let Some(client) = self.client() else { return };
        let thread_stop = self.server_info().capability("threadStop");
        let task_stop = stop.run_id.as_ref().map(|run_id| {
            json!({
                "type": "subagent.stop",
                "commandId": new_id(),
                "threadId": stop.owner_thread_id,
                "runId": run_id,
                "subagentId": stop.subagent_id,
            })
        });
        let child_stop: Option<Value> = stop.child.as_ref().and_then(|(thread_id, active_run)| match active_run {
            _ if thread_stop => Some(json!({"type": "thread.stop", "commandId": new_id(), "threadId": thread_id})),
            Some(run_id) => Some(json!({
                "type": "run.interrupt", "commandId": new_id(), "threadId": thread_id, "runId": run_id, "holdQueue": true,
            })),
            None => None,
        });
        if task_stop.is_none() && child_stop.is_none() {
            return;
        }
        self.lineage.stopping = Some(child_thread_id);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let mut result = Ok(());
            if let Some(command) = task_stop {
                let sent = client.request("orchestration.dispatchCommand", command).await.map(|_| ());
                // Older servers reject this for app-owned tasks, so the child stop still runs.
                if child_stop.is_none() {
                    result = sent;
                }
            }
            if let Some(command) = child_stop {
                result = client.request("orchestration.dispatchCommand", command).await.map(|_| ());
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.lineage.stopping = None;
                if result.is_err() {
                    window.push_notification(Notification::error("").title("Could not stop subagent"), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str, extra: Value) -> ThreadShell {
        let mut value = json!({"id": id, "projectId": "p", "title": format!("{id} title"), "status": "idle", "updatedAt": "2026-01-01"});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn agent(id: &str, child: Option<&str>, extra: Value) -> Subagent {
        let mut value = json!({"id": id, "threadId": "current", "runId": "run-1", "childThreadId": child,
            "origin": "app_owned", "driver": "codex", "title": null, "status": "running", "startedAt": "2026-01-01T00:00:00Z"});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn ids(rows: &[Relative]) -> Vec<&str> {
        rows.iter().map(|row| row.thread_id.as_str()).collect()
    }

    #[test]
    fn omitted_and_archived_relatives_stay_until_a_lookup_proves_them_gone() {
        let threads = [thread("current", json!({"lineage": {"parentThreadId": "parent"}}))];
        let agents = [agent("a1", Some("idle-child"), json!({"status": "completed", "title": "Idle child"}))];
        let pending = relationships("current", Some("parent"), &threads, &agents, |_| None);
        assert_eq!(pending.parent.as_ref().map(|p| p.title.as_ref()), Some("parent"));
        assert_eq!(ids(&pending.previous), ["idle-child"]);
        assert_eq!(pending.previous[0].title.as_ref(), "Idle child");

        let found = relationships("current", Some("parent"), &threads, &agents, |_| Some(Lookup::Found("Old".into())));
        assert_eq!(found.parent.map(|p| p.title), Some("Old".into()));
        let failed = relationships("current", Some("parent"), &threads, &agents, |_| Some(Lookup::Failed));
        assert!(failed.parent.is_some());
        let gone = relationships("current", Some("parent"), &threads, &agents, |_| Some(Lookup::Gone));
        assert!(gone.parent.is_none());

        // A subagent's parent agent stays even when the lookup misses it.
        let threads =
            [thread("current", json!({"lineage": {"parentThreadId": "parent", "relationshipToParent": "subagent"}}))];
        assert!(relationships("current", Some("parent"), &threads, &[], |_| Some(Lookup::Gone)).parent.is_some());

        assert_eq!(unlisted("current", Some("parent"), &threads, &agents), ["parent"]);
        let untitled = [agent("a2", Some("unnamed"), json!({}))];
        assert_eq!(unlisted("current", None, &threads, &untitled), ["unnamed"]);
    }

    #[test]
    fn side_chats_list_apart_from_subagents() {
        let threads = [
            thread("current", json!({})),
            thread(
                "side",
                json!({"lineage": {"parentThreadId": "current", "sideChat": true}, "activityRunStatus": "running"}),
            ),
            thread(
                "sub",
                json!({"lineage": {"parentThreadId": "current", "relationshipToParent": "subagent"}, "status": "running"}),
            ),
            thread(
                "done",
                json!({"lineage": {"parentThreadId": "current", "relationshipToParent": "subagent"}, "status": "completed"}),
            ),
            thread("fork", json!({"lineage": {"parentThreadId": "current", "relationshipToParent": "fork"}})),
        ];
        let found = relationships("current", None, &threads, &[], |_| None);
        assert_eq!(ids(&found.side_chats), ["side"]);
        assert_eq!(found.side_chats[0].status.as_deref(), Some("running"));
        assert_eq!(ids(&found.active), ["sub"]);
        assert_eq!(ids(&found.previous), ["done"]);
    }

    #[test]
    fn stop_shows_for_started_tasks_the_app_can_end() {
        let child =
            thread("child", json!({"lineage": {"parentThreadId": "current", "relationshipToParent": "subagent"}}));
        assert!(can_stop(&agent("a", Some("child"), json!({})), None));
        assert!(!can_stop(&agent("a", Some("child"), json!({"startedAt": null})), None));
        assert!(!can_stop(&agent("a", Some("child"), json!({"status": "completed"})), Some(&child)));
        // A settled task whose child runs again stops through the child.
        let running =
            thread("child", json!({"activityRunStatus": "running", "activityRunStartedAt": "2026-01-02T00:00:00Z"}));
        assert!(can_stop(&agent("a", Some("child"), json!({"status": "completed"})), Some(&running)));

        let native = |extra: Value| {
            let mut fields = json!({"origin": "provider_native", "driver": "claudeAgent",
                "nativeTaskRef": {"driver": "claudeAgent", "nativeId": "task-1", "strength": "strong"}});
            fields.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            agent("n", Some("child"), fields)
        };
        assert!(can_stop(&native(json!({})), None));
        assert!(!can_stop(&native(json!({"driver": "codex"})), None));
        assert!(!can_stop(
            &native(json!({"nativeTaskRef": {"driver": "claudeAgent", "nativeId": null, "strength": "strong"}})),
            None
        ));
        assert!(!can_stop(&native(json!({"runId": null})), None));
        assert!(!can_stop(&native(json!({"status": "completed"})), Some(&running)));

        let stop = stop_of(&agent("a", Some("child"), json!({})), Some(&thread("child", json!({"activeRunId": "r2"}))));
        assert_eq!(stop.child, Some(("child".into(), Some("r2".into()))));
        assert_eq!(stop_of(&native(json!({})), None).child, None);
    }
}
