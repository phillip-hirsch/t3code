//! The relationships Thread details lists (`ThreadRelationshipsControl.tsx`): the parent, forks,
//! context transfers, subagents (a workflow with its members) and side chats, including the ones
//! the shell leaves out; a fork's Merge back, a running subagent's Stop and a workflow's Stop.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{Context, SharedString, Task, Window};
use serde_json::{Value, json};

use crate::app::{Target, Workspace, new_id};
use crate::model::{Subagent, ThreadProjection, ThreadShell};
use crate::workflow::{Member, Phase};

#[derive(Default)]
pub(in crate::app) struct LineageUi {
    /// `orchestration.getThreadTitles` answers for related threads the shell leaves out, by
    /// `environment:thread`.
    titles: HashMap<String, Lookup>,
    fetching: HashSet<String>,
    _fetches: Vec<Task<()>>,
    /// The child thread a subagent Stop, or the coordinator a workflow Stop, is in flight for.
    pub(super) stopping: Option<String>,
    /// The `environment:thread` whose Merge back is in flight.
    pub(super) merging: Option<String>,
    /// Expanded workflow rows, by [`row_key`].
    pub(super) workflows_open: HashSet<String>,
    /// Phases the user opened or closed, by [`row_key`] and phase index; the rest stay open
    /// while they run.
    pub(super) phases_open: HashMap<(String, i64), bool>,
    /// Repaints an open workflow's running elapsed times once a second.
    _tick: Option<Task<()>>,
    /// Whether the card this frame rendered such a time; taken by `sync_lineage_tick`.
    pub(super) card_ticks: std::cell::Cell<bool>,
}

/// A related thread's row in the open thread's details, unique across environments.
pub(super) fn row_key(environment: &str, thread_id: &str, related: &str) -> String {
    format!("{environment}:{thread_id}:{related}")
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Lookup {
    Found(SharedString),
    /// The server answered without it: the thread is gone.
    Gone,
    /// Only an answer proves a thread gone, so it stays listed.
    Failed,
}

/// How a row relates to the open thread (`relationshipLabel`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    ParentThread,
    ParentAgent,
    Fork,
    Transfer,
    Subagent,
    SideChat,
}

impl Kind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::ParentThread => "Parent thread",
            Self::ParentAgent => "Parent agent",
            Self::Fork => "Fork",
            Self::Transfer => "Context transfer",
            Self::Subagent => "Subagent",
            Self::SideChat => "Side chat",
        }
    }

    pub(super) fn icon(self) -> &'static str {
        match self {
            Self::ParentThread | Self::ParentAgent => "icons/corner-left-up.svg",
            Self::Subagent => "icons/bot.svg",
            Self::SideChat => "icons/messages-square.svg",
            Self::Fork | Self::Transfer => "icons/git-fork.svg",
        }
    }
}

/// One row: a related thread, by what Thread details shows of it.
#[derive(Debug, PartialEq)]
pub(super) struct Relative {
    pub thread_id: String,
    pub title: SharedString,
    pub status: Option<String>,
    pub kind: Kind,
    pub stop: Option<SubagentStop>,
    pub workflow: Option<Workflow>,
}

/// A subagent that coordinates a provider-native workflow, with its members by phase.
#[derive(Debug, PartialEq)]
pub(super) struct Workflow {
    pub subagent_id: String,
    pub driver: String,
    pub phases: Vec<Phase>,
    pub unphased: Vec<Member>,
    /// `canStopWorkflow` short of the grant: a running Claude workflow.
    pub stoppable: bool,
}

impl Workflow {
    /// Whether its open row shows a time that runs: a running phase or a working member that started.
    pub fn ticks(&self) -> bool {
        let live = |member: &Member| matches!(member.status.as_str(), "pending" | "running" | "waiting");
        self.phases.iter().any(|phase| phase.running() && phase.members.iter().any(|m| m.started_ms.is_some()))
            || self.unphased.iter().any(|m| live(m) && m.started_ms.is_some())
    }
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

/// A fork's way back into the thread it came from.
#[derive(Debug, PartialEq)]
pub(super) struct MergeBack {
    pub target_thread_id: String,
    /// `resolveLatestMergeBackRun`: none until a run in this fork finishes.
    pub run_id: Option<String>,
    /// The target's title, unless it is hidden or not loaded.
    pub target_title: Option<SharedString>,
}

#[derive(Debug, Default, PartialEq)]
pub(super) struct Relationships {
    /// Parents, forks and context transfers.
    pub related: Vec<Relative>,
    pub active: Vec<Relative>,
    pub previous: Vec<Relative>,
    pub side_chats: Vec<Relative>,
    pub merge: Option<MergeBack>,
}

/// What the open thread's relationships are read from.
pub(super) struct Sources<'a> {
    pub current: &'a str,
    /// The open thread's environment's threads.
    pub threads: &'a [&'a ThreadShell],
    /// The open projection's subagents and `contextTransfers`.
    pub subagents: &'a [Subagent],
    pub transfers: &'a [Value],
    pub merge_target: Option<&'a str>,
    /// Some environment hides threads by tag, so an unloaded parent, fork or transfer may be one.
    pub hiding: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeKind {
    Fork,
    Subagent,
    Transfer,
}

/// `ThreadRelationshipEdge`, kept to the edges that touch the open thread.
#[derive(Clone, Copy, Debug)]
struct Edge<'a> {
    source: &'a str,
    target: &'a str,
    kind: EdgeKind,
    status: Option<&'a str>,
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

/// A fork at a run names its source thread; anything else follows its lineage.
fn parent_of(thread: &ThreadShell) -> Option<&str> {
    match thread.forked_from.as_ref().filter(|forked| forked["type"] == "run") {
        Some(forked) => forked["threadId"].as_str(),
        None => thread.lineage.parent_thread_id.as_deref(),
    }
}

/// `resolveMergeBackTargetThreadId`: a fork merges back into the thread it was forked from.
pub(super) fn merge_target<'a>(current: &'a ThreadShell, projection: &'a ThreadProjection) -> Option<&'a str> {
    if current.lineage.relationship_to_parent.as_deref() != Some("fork") {
        return None;
    }
    match projection.thread.forked_from.as_ref().filter(|forked| forked["type"] == "run") {
        Some(forked) => forked["threadId"].as_str(),
        None => current.lineage.parent_thread_id.as_deref(),
    }
}

/// `threadRelationshipStatusLabel`.
pub(super) fn status_label(status: Option<&str>) -> &'static str {
    match status.unwrap_or_default() {
        "preparing" | "starting" => "Starting",
        "running" | "in_progress" => "Running",
        "pending" | "queued" => "Queued",
        "waiting" | "blocked" => "Waiting",
        "completed" => "Done",
        "failed" | "error" => "Failed",
        "cancelled" | "interrupted" => "Stopped",
        "rolled_back" => "Reverted",
        "resolved_native" => "Resolved (native)",
        "resolved_portable" => "Resolved (portable)",
        "consumed" => "Consumed",
        "superseded" => "Superseded",
        "idle" => "Idle",
        _ => "Unknown",
    }
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

fn add_edge<'a>(edges: &mut Vec<Edge<'a>>, mut edge: Edge<'a>) {
    if edge.kind == EdgeKind::Subagent && edge.status == Some("rolled_back") {
        edge.status = Some("interrupted");
    }
    let existing = edges.iter_mut().find(|e| e.source == edge.source && e.target == edge.target && e.kind == edge.kind);
    match existing {
        Some(existing) => *existing = edge,
        None => edges.push(edge),
    }
}

/// `deriveThreadRelationshipGraph`, kept to the open thread's edges, in the web's order:
/// lineage, then the projection's subagents, then its context transfers.
fn edges<'a>(sources: &Sources<'a>) -> Vec<Edge<'a>> {
    let current = sources.current;
    let mut edges = Vec::new();
    for &thread in sources.threads {
        let Some(parent) = parent_of(thread) else { continue };
        if thread.id != current && parent != current {
            continue;
        }
        let subagent = thread.lineage.relationship_to_parent.as_deref() == Some("subagent");
        let kind = if subagent { EdgeKind::Subagent } else { EdgeKind::Fork };
        let status = Some(activity(thread).unwrap_or(&thread.status));
        add_edge(&mut edges, Edge { source: parent, target: &thread.id, kind, status });
    }
    for agent in sources.subagents {
        let Some(child_id) = agent.child_thread_id.as_deref() else { continue };
        let child = sources.threads.iter().copied().find(|thread| thread.id == child_id);
        let status = child.and_then(activity).or_else(|| match child {
            Some(child) if child.latest_run_id.is_some() && !is_work_active(&agent.status) => Some(&child.status),
            _ => Some(&agent.status),
        });
        add_edge(&mut edges, Edge { source: current, target: child_id, kind: EdgeKind::Subagent, status });
    }
    for transfer in sources.transfers {
        let (Some(source), Some(target)) = (transfer["sourceThreadId"].as_str(), transfer["targetThreadId"].as_str())
        else {
            continue;
        };
        if source == target || (source != current && target != current) {
            continue;
        }
        let status = transfer["status"].as_str();
        add_edge(&mut edges, Edge { source, target, kind: EdgeKind::Transfer, status });
    }
    edges
}

/// A related thread through its first edge (`immediateThreadRelationships`).
struct Neighbour<'a> {
    id: &'a str,
    edge: Edge<'a>,
    thread: Option<&'a ThreadShell>,
    is_parent: bool,
    /// The projection's subagent behind a subagent child.
    agent: Option<&'a Subagent>,
}

impl Neighbour<'_> {
    /// Only `orchestration.getThreadTitles` knows its title.
    fn unlisted(&self) -> bool {
        self.thread.is_none()
            && (self.edge.kind != EdgeKind::Subagent || self.agent.is_none_or(|agent| agent.title.is_none()))
    }
}

fn neighbours<'a>(sources: &Sources<'a>) -> Vec<Neighbour<'a>> {
    let mut seen = HashSet::new();
    edges(sources)
        .into_iter()
        .filter_map(|edge| {
            let id = if edge.source == sources.current { edge.target } else { edge.source };
            if id == sources.current || !seen.insert(id) {
                return None;
            }
            let is_parent = edge.kind != EdgeKind::Transfer && edge.target == sources.current;
            let agent = (edge.kind == EdgeKind::Subagent && !is_parent)
                .then(|| sources.subagents.iter().find(|agent| agent.child_thread_id.as_deref() == Some(id)))
                .flatten();
            let thread = sources.threads.iter().copied().find(|thread| thread.id == id);
            Some(Neighbour { id, edge, thread, is_parent, agent })
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Related,
    Active,
    Previous,
    SideChats,
}

/// The open thread's relationships, grouped and ordered as Thread details shows them
/// (`orderWebThreadLineageRows`: the parent, then the merge-back target, then newest first).
/// Hidden threads drop out; an unlisted one stays until a title lookup proves it gone.
pub(super) fn relationships(
    sources: &Sources,
    hidden: impl Fn(&str) -> bool,
    titles: impl Fn(&str) -> Option<Lookup>,
) -> Relationships {
    let mut rows = Vec::new();
    for neighbour in neighbours(sources) {
        let unlisted = neighbour.unlisted();
        let Neighbour { id, edge, thread, is_parent, agent } = neighbour;
        let subagent = edge.kind == EdgeKind::Subagent;
        let hidden_unloaded = sources.hiding && thread.is_none() && (!subagent || is_parent);
        if hidden(id) || hidden_unloaded {
            continue;
        }
        let lookup = unlisted.then(|| titles(id)).flatten();
        if !subagent && lookup == Some(Lookup::Gone) {
            continue;
        }
        let workflow = agent.and_then(|agent| Some((agent, agent.workflow.as_ref().filter(|w| !w.is_null())?)));
        // A workflow row reads as its coordinator, not its thread.
        let status = match workflow {
            Some((agent, _)) => Some(agent.status.as_str()),
            None => edge.status,
        };
        let side_chat = !is_parent && thread.is_some_and(|thread| thread.lineage.side_chat == Some(true));
        let kind = match edge.kind {
            _ if side_chat => Kind::SideChat,
            EdgeKind::Subagent if is_parent => Kind::ParentAgent,
            EdgeKind::Subagent => Kind::Subagent,
            EdgeKind::Fork if is_parent => Kind::ParentThread,
            EdgeKind::Fork => Kind::Fork,
            EdgeKind::Transfer => Kind::Transfer,
        };
        let group = match () {
            () if side_chat => Group::SideChats,
            () if !subagent || is_parent => Group::Related,
            () if is_finished(status) => Group::Previous,
            () => Group::Active,
        };
        let name = workflow.and_then(|(_, workflow)| workflow["name"].as_str());
        let title = name
            .map(str::to_owned)
            .or_else(|| match &lookup {
                Some(Lookup::Found(title)) => Some(title.to_string()),
                _ => None,
            })
            .or_else(|| thread.map(|thread| thread.title.clone()))
            .or_else(|| agent.and_then(|agent| agent.title.clone()))
            .unwrap_or_else(|| id.to_owned());
        let title = if subagent { crate::timeline::subagent_title(&title) } else { title };
        let row_status = if edge.kind == EdgeKind::Transfer || id == edge.target {
            status
        } else {
            thread.map(|thread| activity(thread).unwrap_or(&thread.status))
        };
        let stop = agent
            .filter(|agent| workflow.is_none() && !side_chat && can_stop(agent, thread))
            .map(|agent| stop_of(agent, thread));
        let workflow = workflow.map(|(agent, workflow)| {
            let done = agent.completed_at.as_deref().and_then(crate::timestamp::parse_ms);
            let (phases, unphased) = crate::workflow::group(&agent.id, &agent.status, done, workflow);
            Workflow {
                subagent_id: agent.id.clone(),
                driver: agent.driver.clone(),
                phases,
                unphased,
                stoppable: agent.status == "running"
                    && agent.origin == "provider_native"
                    && agent.driver == "claudeAgent",
            }
        });
        let rank = if is_parent {
            0
        } else if Some(id) == sources.merge_target {
            1
        } else {
            2
        };
        let created = thread.and_then(|thread| crate::timestamp::parse_ms(&thread.created_at));
        let relative = Relative {
            thread_id: id.to_owned(),
            title: title.into(),
            status: row_status.map(str::to_owned),
            kind,
            stop,
            workflow,
        };
        rows.push((rank, created, group, relative));
    }
    rows.sort_by(|(rank_a, created_a, _, a), (rank_b, created_b, _, b)| {
        let created = match (created_a, created_b) {
            (Some(a), Some(b)) => b.cmp(a),
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (None, None) => Ordering::Equal,
        };
        rank_a.cmp(rank_b).then(created).then_with(|| a.thread_id.cmp(&b.thread_id))
    });
    let mut relationships = Relationships::default();
    for (_, _, group, relative) in rows {
        match group {
            Group::Related => relationships.related.push(relative),
            Group::Active => relationships.active.push(relative),
            Group::Previous => relationships.previous.push(relative),
            Group::SideChats => relationships.side_chats.push(relative),
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
fn unlisted(sources: &Sources) -> Vec<String> {
    neighbours(sources).iter().filter(|n| n.unlisted()).map(|n| n.id.to_owned()).collect()
}

impl Workspace {
    /// Repaints once a second while the card shows a running workflow time, and stops as soon
    /// as a frame renders none (the card closed, Lineage dropped out, or the target changed).
    pub(in crate::app) fn sync_lineage_tick(&mut self, cx: &mut Context<Self>) {
        if !self.lineage.card_ticks.take() {
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

    /// Runs `f` over the open thread's [`Sources`] and environment.
    fn with_lineage_sources<R>(&self, f: impl FnOnce(&Sources, &SharedString) -> R) -> Option<R> {
        let Some(Target::Thread { env, id }) = &self.target else { return None };
        let threads: Vec<&ThreadShell> = self.shell.threads.iter().filter(|t| t.environment_id == *env).collect();
        let current = threads.iter().copied().find(|thread| &thread.id == id)?;
        let projection = self.open_projection();
        let sources = Sources {
            current: id,
            threads: &threads,
            subagents: projection.map_or(&[][..], |p| p.subagents.as_slice()),
            transfers: projection.map_or(&[][..], |p| p.context_transfers.as_slice()),
            merge_target: projection.and_then(|projection| merge_target(current, projection)),
            hiding: false,
        };
        Some(f(&sources, env))
    }

    /// The open thread's relationships as Thread details lists them.
    pub(super) fn open_relationships(&self) -> Option<Relationships> {
        let hidden_tags = self.hidden_thread_tags();
        let hidden = crate::model::hidden_threads(&self.shell.threads, &hidden_tags);
        let titles = &self.lineage.titles;
        self.with_lineage_sources(|sources, env| {
            let sources = Sources { hiding: !hidden_tags.is_empty(), ..*sources };
            let is_hidden = |thread_id: &str| hidden.contains(&(env.as_ref(), thread_id));
            let mut relationships =
                relationships(&sources, is_hidden, |thread_id| titles.get(&format!("{env}:{thread_id}")).cloned());
            relationships.merge = sources.merge_target.map(|target| MergeBack {
                target_thread_id: target.to_owned(),
                run_id: self
                    .open_projection()
                    .and_then(super::super::side_chat::merge_back_run)
                    .map(|run| run.id.clone()),
                target_title: (!is_hidden(target))
                    .then(|| sources.threads.iter().find(|thread| thread.id == target))
                    .flatten()
                    .map(|thread| thread.title.clone().into()),
            });
            relationships
        })
    }

    /// Reads the titles of related threads the shell leaves out, once each.
    pub(in crate::app) fn sync_lineage_titles(&mut self, cx: &mut Context<Self>) {
        let wanted = self.with_lineage_sources(|sources, env| {
            let wanted: Vec<String> = unlisted(sources)
                .into_iter()
                .filter(|thread_id| {
                    let key = format!("{env}:{thread_id}");
                    !self.lineage.titles.contains_key(&key) && !self.lineage.fetching.contains(&key)
                })
                .collect();
            (env.clone(), wanted)
        });
        let Some((env, wanted)) = wanted else { return };
        let Some(client) = self.env_client(&env).filter(|_| !wanted.is_empty()) else { return };
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

    /// Merge back: sends the fork's latest finished run into the thread it came from, then
    /// opens that thread while this fork is still the open one.
    pub(super) fn merge_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Target::Thread { env, id }) = self.target.clone() else { return };
        if self.lineage.merging.is_some() || !self.can_operate(&env) {
            return;
        }
        let Some(MergeBack { target_thread_id, run_id: Some(run_id), .. }) =
            self.open_relationships().and_then(|relationships| relationships.merge)
        else {
            return;
        };
        let Some(client) = self.env_client(&env) else { return };
        let command = json!({
            "type": "thread.merge_back",
            "commandId": new_id(),
            "createdBy": "user",
            "creationSource": "web",
            "sourceThreadId": id,
            "targetThreadId": target_thread_id,
            "sourcePoint": {"type": "run", "runId": run_id},
        });
        self.lineage.merging = Some(format!("{env}:{id}"));
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.lineage.merging = None;
                match result {
                    Ok(_) if this.target == Some(Target::Thread { env: env.clone(), id }) => {
                        this.select_thread_in(env, target_thread_id, window, cx);
                    }
                    Ok(_) => {}
                    Err(_) => window.push_notification(Notification::error("").title("Could not merge back"), cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `stopWorkflow`: ends a running workflow coordinator and every agent in it.
    pub(super) fn stop_workflow(&mut self, subagent_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Target::Thread { env, id }) = self.target.clone() else { return };
        if self.lineage.stopping.is_some() || !self.can_operate(&env) {
            return;
        }
        let stoppable = self.open_relationships().is_some_and(|relationships| {
            relationships
                .active
                .iter()
                .chain(&relationships.previous)
                .any(|row| row.workflow.as_ref().is_some_and(|w| w.subagent_id == subagent_id && w.stoppable))
        });
        let Some(client) = self.env_client(&env).filter(|_| stoppable) else { return };
        let input = json!({"threadId": id, "subagentId": subagent_id});
        self.lineage.stopping = Some(subagent_id);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.stopWorkflow", input).await;
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

    fn sources<'a>(threads: &'a [&'a ThreadShell], subagents: &'a [Subagent], transfers: &'a [Value]) -> Sources<'a> {
        Sources { current: "current", threads, subagents, transfers, merge_target: None, hiding: false }
    }

    fn find(sources: &Sources, titles: impl Fn(&str) -> Option<Lookup>) -> Relationships {
        relationships(sources, |_| false, titles)
    }

    #[test]
    fn omitted_and_archived_relatives_stay_until_a_lookup_proves_them_gone() {
        let threads = [thread("current", json!({"lineage": {"parentThreadId": "parent"}}))];
        let threads: Vec<&ThreadShell> = threads.iter().collect();
        let agents = [agent("a1", Some("idle-child"), json!({"status": "completed", "title": "Idle child"}))];
        let sources = sources(&threads, &agents, &[]);
        let pending = find(&sources, |_| None);
        assert_eq!(pending.related.iter().map(|p| p.title.as_ref()).collect::<Vec<_>>(), ["parent"]);
        assert_eq!(pending.related[0].kind, Kind::ParentThread);
        assert_eq!(ids(&pending.previous), ["idle-child"]);
        assert_eq!(pending.previous[0].title.as_ref(), "Idle child");

        let found = find(&sources, |_| Some(Lookup::Found("Old".into())));
        assert_eq!(found.related[0].title.as_ref(), "Old");
        assert_eq!(find(&sources, |_| Some(Lookup::Failed)).related.len(), 1);
        assert!(find(&sources, |_| Some(Lookup::Gone)).related.is_empty());
        assert_eq!(unlisted(&sources), ["parent"]);

        // A subagent's parent agent stays even when the lookup misses it.
        let child =
            [thread("current", json!({"lineage": {"parentThreadId": "parent", "relationshipToParent": "subagent"}}))];
        let child: Vec<&ThreadShell> = child.iter().collect();
        let untitled = [agent("a2", Some("unnamed"), json!({}))];
        let sources = Sources { threads: &child, subagents: &untitled, ..sources };
        let parent_agent = find(&sources, |_| Some(Lookup::Gone));
        assert_eq!(parent_agent.related.iter().map(|r| r.kind).collect::<Vec<_>>(), [Kind::ParentAgent]);
        assert_eq!(unlisted(&sources), ["parent", "unnamed"]);
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
        let threads: Vec<&ThreadShell> = threads.iter().collect();
        // A task the provider rolled back reads as stopped.
        let agents = [agent("a", Some("reverted"), json!({"status": "rolled_back", "title": "Reverted"}))];
        let found = find(&sources(&threads, &agents, &[]), |_| None);
        assert_eq!(ids(&found.side_chats), ["side"]);
        assert_eq!(found.side_chats[0].status.as_deref(), Some("running"));
        assert_eq!(ids(&found.active), ["sub"]);
        assert_eq!(ids(&found.previous), ["done", "reverted"]);
        assert_eq!(found.previous[1].status.as_deref(), Some("interrupted"));
        assert_eq!((ids(&found.related), found.related[0].kind), (vec!["fork"], Kind::Fork));
    }

    #[test]
    fn forks_and_context_transfers_list_after_the_parent_newest_first() {
        let threads = [
            // A fork at a run names its source over its lineage.
            thread(
                "current",
                json!({"lineage": {"parentThreadId": "lineage", "relationshipToParent": "fork"},
                    "forkedFrom": {"type": "run", "threadId": "src", "runId": "r"}}),
            ),
            thread("src", json!({"createdAt": "2026-01-01T00:00:00Z", "activityRunStatus": "running"})),
            thread("old", json!({"lineage": {"parentThreadId": "current"}, "createdAt": "2026-01-02T00:00:00Z"})),
            thread("new", json!({"lineage": {"parentThreadId": "current"}, "createdAt": "2026-01-03T00:00:00Z"})),
            thread("unrelated", json!({"lineage": {"parentThreadId": "src"}})),
        ];
        let threads: Vec<&ThreadShell> = threads.iter().collect();
        let transfers = [
            json!({"sourceThreadId": "src", "targetThreadId": "current", "status": "resolved_native"}),
            json!({"sourceThreadId": "current", "targetThreadId": "away", "status": "consumed"}),
            json!({"sourceThreadId": "current", "targetThreadId": "current", "status": "pending"}),
            json!({"sourceThreadId": "x", "targetThreadId": "y", "status": "pending"}),
        ];
        let sources = Sources { merge_target: Some("src"), ..sources(&threads, &[], &transfers) };
        let found = find(&sources, |_| None);
        assert_eq!(ids(&found.related), ["src", "new", "old", "away"]);
        let kinds: Vec<Kind> = found.related.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [Kind::ParentThread, Kind::Fork, Kind::Fork, Kind::Transfer]);
        assert_eq!(found.related[0].status.as_deref(), Some("running"), "a parent reads as its own thread");
        assert_eq!(status_label(found.related[3].status.as_deref()), "Consumed");
        assert_eq!(unlisted(&sources), ["away"]);
        assert_eq!(ids(&find(&sources, |_| Some(Lookup::Gone)).related), ["src", "new", "old"]);

        // Hidden threads drop out, and while hiding so does an unloaded transfer.
        let hiding = relationships(&Sources { hiding: true, ..sources }, |id| id == "old", |_| None);
        assert_eq!(ids(&hiding.related), ["src", "new"]);
    }

    #[test]
    fn a_workflow_row_reads_as_its_coordinator_with_members_by_phase() {
        let threads = [
            thread("current", json!({})),
            thread(
                "wf",
                json!({"lineage": {"parentThreadId": "current", "relationshipToParent": "subagent"},
                    "activityRunStatus": "running"}),
            ),
        ];
        let threads: Vec<&ThreadShell> = threads.iter().collect();
        let coordinator = |status: &str, driver: &str| {
            agent(
                "s1",
                Some("wf"),
                json!({"status": status, "origin": "provider_native", "driver": driver,
                    "nativeTaskRef": {"nativeId": "t", "strength": "strong"},
                    "workflow": {"name": "review", "phases": [{"index": 0, "title": "Find"}],
                        "agents": [{"index": 0, "label": "bugs", "state": "running", "phaseIndex": 0, "startedAt": 1000},
                            {"index": 1, "label": "extra", "state": "queued"}]}}),
            )
        };
        let agents = [coordinator("running", "claudeAgent")];
        let found = find(&sources(&threads, &agents, &[]), |_| None);
        let row = &found.active[0];
        assert_eq!((row.title.as_ref(), row.stop.is_some()), ("review", false));
        let workflow = row.workflow.as_ref().unwrap();
        assert!(workflow.stoppable);
        assert_eq!(workflow.phases.iter().map(|p| (p.index, p.members.len())).collect::<Vec<_>>(), [(0, 1)]);
        assert_eq!(workflow.unphased[0].title, "extra");
        assert!(workflow.ticks(), "a running phase with a started member shows a running time");

        // A settled workflow files under Previous agents even while its thread still runs.
        let agents = [coordinator("completed", "claudeAgent")];
        let found = find(&sources(&threads, &agents, &[]), |_| None);
        assert_eq!((ids(&found.previous), found.previous[0].status.as_deref()), (vec!["wf"], Some("completed")));
        assert!(!found.previous[0].workflow.as_ref().unwrap().stoppable);
        assert!(!found.previous[0].workflow.as_ref().unwrap().ticks(), "a settled workflow's times stand still");
        let agents = [coordinator("running", "codex")];
        assert!(!find(&sources(&threads, &agents, &[]), |_| None).active[0].workflow.as_ref().unwrap().stoppable);
    }

    #[test]
    fn a_fork_merges_back_into_its_source() {
        let projection =
            |thread: Value| -> ThreadProjection { serde_json::from_value(json!({"thread": thread})).unwrap() };
        let fork = thread("current", json!({"lineage": {"parentThreadId": "lineage", "relationshipToParent": "fork"}}));
        let at_run = projection(
            json!({"id": "current", "title": "t", "forkedFrom": {"type": "run", "threadId": "src", "runId": "r"}}),
        );
        assert_eq!(merge_target(&fork, &at_run), Some("src"));
        let plain = projection(json!({"id": "current", "title": "t"}));
        assert_eq!(merge_target(&fork, &plain), Some("lineage"));
        let subagent =
            thread("current", json!({"lineage": {"parentThreadId": "lineage", "relationshipToParent": "subagent"}}));
        assert_eq!(merge_target(&subagent, &at_run), None);
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
