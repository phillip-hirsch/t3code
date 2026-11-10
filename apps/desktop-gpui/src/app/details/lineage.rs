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
use crate::workflow::{self, Member, Phase};

#[derive(Default)]
pub(in crate::app) struct LineageUi {
    /// `orchestration.getThreadTitles` answers for related threads the shell leaves out, by
    /// `environment:thread`.
    titles: HashMap<String, Lookup>,
    fetching: HashSet<String>,
    _fetches: Vec<Task<()>>,
    /// The child thread a Stop is in flight for.
    pub(super) stopping: Option<String>,
    /// Workflow rows shown open, by coordinator subagent; each starts closed.
    pub(super) open_workflows: HashSet<String>,
    /// Phases opened or closed by hand, by coordinator subagent and phase index; the others
    /// are open while they run (`phaseOpen`).
    pub(super) phase_open: HashMap<(String, i64), bool>,
    /// Repaints an open workflow's running elapsed times once a second.
    _tick: Option<Task<()>>,
    /// Whether the card this frame rendered such a time; taken by `sync_lineage_tick`.
    pub(super) card_ticks: bool,
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
    /// A workflow coordinator's agents, shown under its row (`ThreadLineageWorkflowRow`).
    pub workflow: Option<WorkflowGroup>,
}

/// `SubagentWorkflowGroup`: a coordinator's members by phase.
#[derive(Debug, PartialEq)]
pub(super) struct WorkflowGroup {
    pub subagent_id: String,
    pub driver: String,
    pub phases: Vec<Phase>,
    pub unphased: Vec<Member>,
    /// `canStopWorkflow` short of the grant: a running provider-native Claude workflow.
    pub stoppable: bool,
}

impl WorkflowGroup {
    fn of(agent: &Subagent) -> Option<Self> {
        let workflow = agent.workflow.as_ref()?;
        let (phases, unphased) = workflow::groups(workflow::Coordinator {
            id: &agent.id,
            status: &agent.status,
            completed_ms: agent.completed_at.as_deref().and_then(crate::timestamp::parse_ms),
            workflow,
        });
        Some(Self {
            subagent_id: agent.id.clone(),
            driver: agent.driver.clone(),
            phases,
            unphased,
            stoppable: agent.status == "running" && agent.origin == "provider_native" && agent.driver == "claudeAgent",
        })
    }

    /// Whether an open row of this workflow shows a time that runs.
    pub fn ticks(&self) -> bool {
        self.phases.iter().any(|phase| phase.running && phase.members.iter().any(|m| m.started_ms.is_some()))
            || self.unphased.iter().any(|m| workflow::is_active(&m.status) && m.started_ms.is_some())
    }
}

/// Whether a phase shows its members: as the user left it, else while it runs.
pub(super) fn phase_is_open(ui: &LineageUi, subagent_id: &str, phase: &Phase) -> bool {
    ui.phase_open.get(&(subagent_id.to_owned(), phase.index)).copied().unwrap_or(phase.running)
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

impl Relationships {
    /// Whether one of the `open` workflow rows shows a time that runs.
    pub fn ticks(&self, open: &HashSet<String>) -> bool {
        self.active
            .iter()
            .chain(&self.previous)
            .filter_map(|row| row.workflow.as_ref())
            .any(|group| open.contains(&group.subagent_id) && group.ticks())
    }
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
                workflow: None,
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
        let group = agent.filter(|_| !side_chat).and_then(WorkflowGroup::of);
        let status = activity(child).or_else(|| match agent {
            Some(agent) if child.latest_run_id.is_none() || is_work_active(&agent.status) => {
                Some(agent.status.as_str())
            }
            _ => Some(child.status.as_str()),
        });
        // A workflow's row reads as its coordinator: its name and status.
        let (title, status) = match (&group, agent) {
            (Some(_), Some(agent)) => {
                (workflow_name(agent).map_or_else(|| child.title.clone(), str::to_owned), Some(agent.status.as_str()))
            }
            _ => (child.title.clone(), status),
        };
        let row = Relative {
            thread_id: child.id.clone(),
            title: title.into(),
            status: status.map(str::to_owned),
            stop: agent
                .filter(|agent| !side_chat && group.is_none() && can_stop(agent, Some(child)))
                .map(|agent| stop_of(agent, Some(child))),
            workflow: group,
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
        let group = WorkflowGroup::of(agent);
        let title = workflow_name(agent)
            .filter(|_| group.is_some())
            .or(agent.title.as_deref())
            .map(|title| SharedString::from(title.to_owned()))
            .or_else(|| match titles(child_id) {
                Some(Lookup::Found(title)) => Some(title),
                _ => None,
            })
            .unwrap_or_else(|| child_id.to_owned().into());
        let row = Relative {
            thread_id: child_id.to_owned(),
            title,
            status: Some(agent.status.clone()),
            stop: (group.is_none() && can_stop(agent, None)).then(|| stop_of(agent, None)),
            workflow: group,
        };
        if is_finished(row.status.as_deref()) {
            relationships.previous.push(row);
        } else {
            relationships.active.push(row);
        }
    }
    relationships
}

/// `workflow.name`, which names a workflow's row before any thread title.
fn workflow_name(agent: &Subagent) -> Option<&str> {
    agent.workflow.as_ref()?["name"].as_str()
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

    /// Keeps a once-a-second repaint alive while the card's rendered Lineage shows an open
    /// workflow's running time; called after each render builds the view.
    pub(in crate::app) fn sync_lineage_tick(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.lineage.card_ticks) {
            self.lineage._tick = None;
        } else if self.lineage._tick.is_none() {
            self.lineage._tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
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

    /// `stopWorkflow`: asks the open thread's server to end a workflow and every agent in it.
    pub(super) fn stop_workflow(&mut self, subagent_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Target::Thread { env, id }) = &self.target else { return };
        if self.lineage.stopping.is_some() || !self.can_operate(env) {
            return;
        }
        let Some(client) = self.env_client(env) else { return };
        let payload = json!({"threadId": id, "subagentId": subagent_id});
        self.lineage.stopping = Some(subagent_id);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.stopWorkflow", payload).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.lineage.stopping = None;
                if result.is_err() {
                    window.push_notification(Notification::error("").title("Could not stop workflow"), cx);
                }
                cx.notify();
            });
        })
        .detach();
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
    fn a_workflow_row_reads_as_its_coordinator_with_its_agents_by_phase() {
        let threads = [
            thread("current", json!({})),
            thread(
                "coordinator",
                json!({"lineage": {"parentThreadId": "current", "relationshipToParent": "subagent"}, "status": "idle"}),
            ),
        ];
        let workflow = json!({"name": "review", "phases": [{"index": 0, "title": "Find"}, {"index": 2, "title": "Later"}],
        "agents": [
            {"index": 1, "label": "perf", "state": "running", "phaseIndex": 0, "startedAt": 1000, "childThreadId": "perf-thread"},
            {"index": 0, "label": "bugs", "state": "failed", "phaseIndex": 1, "startedAt": 1000, "completedAt": 3000},
            {"index": 2, "label": "loose", "state": "queued"}
        ]});
        let coordinator = |status: &str| {
            agent(
                "w1",
                Some("coordinator"),
                json!({"origin": "provider_native", "driver": "claudeAgent", "status": status, "workflow": workflow}),
            )
        };
        let found = relationships("current", None, &threads, &[coordinator("running")], |_| None);
        let row = &found.active[0];
        assert_eq!((row.title.as_ref(), row.status.as_deref(), &row.stop), ("review", Some("running"), &None));
        let group = row.workflow.as_ref().unwrap();
        assert!(group.stoppable);
        let phases: Vec<_> = group.phases.iter().map(|p| (p.index, p.title.as_str(), p.running, p.failed())).collect();
        // A declared phase without members is left out; a member's own phase gets a name.
        assert_eq!(phases, [(0, "Find", true, false), (1, "Phase 2", false, true)]);
        assert_eq!(group.unphased[0].title, "loose");
        assert_eq!(group.phases[0].members[0].child_thread_id.as_deref(), Some("perf-thread"));
        assert_eq!((group.phases[0].elapsed_ms(4000), group.phases[1].elapsed_ms(9000)), (Some(3000), Some(2000)));
        assert!(group.ticks());
        assert!(found.ticks(&HashSet::from(["w1".to_owned()])) && !found.ticks(&HashSet::new()), "only open rows tick");

        // A settled workflow moves to previous agents, and its queued agents read as it ended.
        let done = relationships("current", None, &threads, &[coordinator("cancelled")], |_| None);
        let group = done.previous[0].workflow.as_ref().unwrap();
        assert!(!group.stoppable && !group.ticks());
        assert_eq!(group.unphased[0].status, "cancelled");

        let ui = LineageUi::default();
        assert!(phase_is_open(&ui, "w1", &found.active[0].workflow.as_ref().unwrap().phases[0]), "running opens");
        let mut ui = ui;
        ui.phase_open.insert(("w1".into(), 0), false);
        assert!(!phase_is_open(&ui, "w1", &found.active[0].workflow.as_ref().unwrap().phases[0]));
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
