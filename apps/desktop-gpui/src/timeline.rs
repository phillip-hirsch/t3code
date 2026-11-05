//! Turns a thread's `visibleTurnItems` into chat rows. Mirrors
//! `deriveTimelineEntriesFromVisibleTurnItems` in `apps/web/src/session-logic.ts`
//! and `deriveMessagesTimelineRows` in
//! `apps/web/src/components/chat/MessagesTimeline.logic.ts`.
//!
//! Not ported yet: superseded-attempt folds (groups ignore attempts), subagent
//! cards, worktree-setup rows, HTML-render and MCP-app cards, and the
//! compaction-run special cases.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::SharedString;
use serde_json::Value;

use crate::model::{Run, ThreadProjection, TurnItem, is_active_run_status};
use crate::work_log::{self, Activity, Entry, Status, Tone, WorkIcon};

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    User {
        id: SharedString,
        text: SharedString,
        steer: Option<Steer>,
        grouped_with_previous: bool,
        grouped_with_next: bool,
        /// The message above is a steer too, so the "Steer" label shows once.
        continues_steer: bool,
    },
    Assistant {
        id: SharedString,
        text: SharedString,
        streaming: bool,
        /// The turn's last reply once the turn finished.
        terminal: bool,
    },
    Plan {
        id: SharedString,
        markdown: SharedString,
    },
    Work(WorkRow),
    /// "Working for 12s" at the top of a running turn.
    Working {
        started_at: Option<SharedString>,
    },
    /// "Worked for 5m 3s": a finished turn's folded work.
    TurnFold {
        key: SharedString,
        label: SharedString,
        expanded: bool,
    },
    /// A centered label between hairlines, e.g. a context compaction.
    Divider {
        id: SharedString,
        label: SharedString,
        failed: bool,
    },
    Event {
        id: SharedString,
        label: SharedString,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Steer {
    Steered,
    Promoted,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkRow {
    pub id: SharedString,
    pub kind: WorkRowKind,
    /// The next row is a work row too, so this one adds no bottom space.
    pub continues: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WorkRowKind {
    /// One entry on its own: a lone tool call, an error, a notice.
    Single {
        entry: Rc<Entry>,
        label: SharedString,
        expanded: bool,
        pending_approval: bool,
    },
    /// A collapsed group's summary, "Ran 2 commands and read 1 file".
    Toggle {
        group: SharedString,
        summary: SharedString,
        icon: WorkIcon,
        expanded: bool,
    },
    /// The open list under an expanded group header; `bool` is each entry's own expansion.
    Details {
        entries: Vec<(Rc<Entry>, bool)>,
    },
    /// The running turn's latest activity.
    Live {
        group: SharedString,
        entry: Rc<Entry>,
        label: SharedString,
        thought: Option<SharedString>,
        expanded: bool,
        active: bool,
    },
    Thinking {
        group: Option<SharedString>,
        expanded: bool,
    },
}

impl Row {
    fn is_work(&self) -> bool {
        matches!(self, Row::Work(_))
    }

    fn is_own_user_message(&self) -> bool {
        matches!(self, Row::User { .. })
    }
}

/// Keys in the expanded set, namespaced so groups, folds and entries never collide.
pub fn group_key(anchor_id: &str) -> SharedString {
    format!("work-group:{anchor_id}").into()
}

pub fn fold_key(run_key: &str) -> SharedString {
    format!("fold:{run_key}").into()
}

pub fn entry_key(entry_id: &str) -> SharedString {
    format!("entry:{entry_id}").into()
}

enum Kind {
    User { text: SharedString, steer: Option<Steer> },
    Assistant { text: SharedString, streaming: bool },
    Plan { markdown: SharedString },
    Work(Rc<Entry>),
    Event { label: SharedString, item_type: SharedString },
}

struct TimelineEntry {
    id: SharedString,
    run_id: Option<SharedString>,
    created_at: SharedString,
    /// When the entry last changed: a message's `updatedAt`, else `created_at`.
    end: SharedString,
    /// The run of a top-level failed `error` item, and whether it was a usage limit.
    failure: Option<(SharedString, bool)>,
    kind: Kind,
}

impl TimelineEntry {
    fn work(&self) -> Option<&Rc<Entry>> {
        match &self.kind {
            Kind::Work(entry) => Some(entry),
            _ => None,
        }
    }

    fn is_steer(&self) -> bool {
        matches!(self.kind, Kind::User { steer: Some(_), .. })
    }

    fn starts_response(&self) -> bool {
        matches!(self.kind, Kind::User { steer: None, .. })
            || self.work().is_some_and(|entry| entry.item_type == "notification")
    }

    fn boundary_run(&self) -> Option<&SharedString> {
        match self.kind {
            Kind::User { .. } | Kind::Assistant { .. } | Kind::Work(_) => self.run_id.as_ref(),
            _ => None,
        }
    }

    /// `timelineEntryRunId`.
    fn response_run(&self) -> Option<&SharedString> {
        match self.kind {
            Kind::Assistant { .. } | Kind::Plan { .. } | Kind::Work(_) => self.run_id.as_ref(),
            _ => None,
        }
    }

    /// `timelineEntryFoldRunId`.
    fn fold_run(&self, runless: Option<&SharedString>) -> Option<SharedString> {
        let own = || self.run_id.clone().or_else(|| runless.cloned());
        match &self.kind {
            Kind::Work(entry) if entry.item_type == "system_notice" => None,
            Kind::Assistant { .. } | Kind::Work(_) => own(),
            Kind::Event { item_type, .. } if is_resource_card(item_type) || item_type == "subagent" => own(),
            _ => None,
        }
    }

    fn is_compaction(&self) -> bool {
        self.work().is_some_and(|entry| entry.activity == Some(Activity::Compaction))
    }
}

fn is_resource_card(item_type: &str) -> bool {
    matches!(item_type, "fork" | "thread_created" | "secret_request" | "link_request")
}

/// Entries from the previous refresh, reused while their item's `updatedAt`
/// and status are unchanged, so a streamed delta rebuilds only its own item.
#[derive(Default)]
pub struct EntryCache(HashMap<String, (String, Rc<TimelineEntry>)>);

fn timeline_entries(projection: &ThreadProjection, cache: &mut EntryCache) -> Vec<Rc<TimelineEntry>> {
    let mut next = HashMap::with_capacity(cache.0.len());
    let entries = projection
        .visible_turn_items
        .iter()
        .filter_map(|row| {
            let item = &row.item;
            let stamp = format!("{}|{}", item.str("updatedAt").unwrap_or_default(), item.status);
            let entry = match cache.0.get(&item.id) {
                Some((cached, entry)) if *cached == stamp && item.str("updatedAt").is_some() => entry.clone(),
                _ => Rc::new(timeline_entry(item)?),
            };
            next.insert(item.id.clone(), (stamp, entry.clone()));
            Some(entry)
        })
        .collect();
    cache.0 = next;
    entries
}

fn timeline_entry(item: &TurnItem) -> Option<TimelineEntry> {
    let raw = &item.raw;
    let text = |key: &str| SharedString::from(item.str(key).unwrap_or_default().to_owned());
    let created_at = text("startedAt");
    let created_at = if created_at.is_empty() { text("updatedAt") } else { created_at };
    let kind = match item.item_type.as_str() {
        "checkpoint" => return None,
        // Shown by the run's status, not as a command (`turnItemIsWorkspacePreparation`).
        "command_execution" if item.str("input") == Some("Preparing workspace") => return None,
        "error"
            if item.status == "cancelled"
                && raw.pointer("/failure/code").and_then(Value::as_str) == Some("workspace_preparation_failed") =>
        {
            return None;
        }
        "user_message" => {
            let intent = item.str("inputIntent");
            // A queued message shows in the queue control until its run starts.
            if intent == Some("queued_turn") && item.status == "pending" {
                return None;
            }
            let steer = match intent {
                Some("steer") => Some(Steer::Steered),
                Some("promoted_queued_to_steer") => Some(Steer::Promoted),
                _ => None,
            };
            Kind::User { text: text("text"), steer }
        }
        "assistant_message" => Kind::Assistant { text: text("text"), streaming: item.bool("streaming") },
        "proposed_plan" => Kind::Plan { markdown: text("markdown") },
        "fork"
        | "handoff"
        | "run_interrupt_request"
        | "run_interrupt_result"
        | "secret_request"
        | "link_request"
        | "subagent" => Kind::Event { label: event_label(item).into(), item_type: item.item_type.clone().into() },
        _ => Kind::Work(Rc::new(work_log::entry_from_item(item))),
    };
    let end = match kind {
        Kind::User { .. } | Kind::Assistant { .. } => text("updatedAt"),
        _ => created_at.clone(),
    };
    let failure =
        (item.item_type == "error" && item.status == "failed" && raw.get("parentItemId").is_none_or(Value::is_null))
            .then(|| item.run_id.clone())
            .flatten()
            .map(|run| (run.into(), raw.pointer("/failure/class").and_then(Value::as_str) == Some("usage_limit")));
    Some(TimelineEntry {
        id: item.id.clone().into(),
        run_id: item.run_id.clone().map(Into::into),
        created_at,
        end,
        failure,
        kind,
    })
}

fn event_label(item: &TurnItem) -> String {
    let text = |key: &str| item.str(key).map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
    match item.item_type.as_str() {
        "fork" => "Forked thread".into(),
        "handoff" => "Switched provider".into(),
        "secret_request" => "Asked for a secret".into(),
        "link_request" => "Asked to link an environment".into(),
        "subagent" => text("title")
            .or_else(|| text("prompt").and_then(|p| p.lines().next().map(str::to_owned)))
            .unwrap_or_else(|| "Subagent".into()),
        _ => text("message").unwrap_or_else(|| "Stopped".into()),
    }
}

struct Fold {
    key: SharedString,
    hidden: HashSet<usize>,
    label: SharedString,
}

/// Index of each turn's last assistant message (`deriveTerminalAssistantMessageIds`).
fn terminal_assistant_messages(entries: &[Rc<TimelineEntry>]) -> HashSet<usize> {
    let mut last_by_response: HashMap<String, usize> = HashMap::new();
    let mut prompts = 0;
    for (index, entry) in entries.iter().enumerate() {
        match entry.kind {
            Kind::User { .. } => prompts += 1,
            Kind::Assistant { .. } => {
                let key = match &entry.run_id {
                    Some(run) => format!("turn:{run}"),
                    None => format!("unkeyed:{prompts}"),
                };
                last_by_response.insert(key, index);
            }
            _ => {}
        }
    }
    last_by_response.into_values().collect()
}

fn elapsed_ms(start: &str, end: &str) -> Option<i64> {
    Some((work_log::parse_iso_ms(end)? - work_log::parse_iso_ms(start)?).max(0))
}

fn later<'a>(a: &'a str, b: &'a str) -> &'a str {
    if b > a { b } else { a }
}

/// `deriveTurnFolds`: each finished turn folds the work before its last reply
/// under one "Worked for …" row, split at steers into "Continued for …" rows.
fn turn_folds(
    entries: &[Rc<TimelineEntry>],
    terminal: &HashSet<usize>,
    latest_run: Option<&Run>,
    unfolded: &HashSet<SharedString>,
) -> HashMap<usize, Fold> {
    let mut interrupted: HashSet<&SharedString> = HashSet::new();
    let mut prompt_index: HashMap<&SharedString, usize> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.starts_response() {
            if let Some(run) = entry.boundary_run() {
                prompt_index.entry(run).or_insert(index);
            }
        }
        if let Kind::Event { item_type, .. } = &entry.kind {
            if matches!(item_type.as_ref(), "run_interrupt_request" | "run_interrupt_result") {
                interrupted.extend(entry.run_id.as_ref());
            }
        }
    }

    struct Group {
        entries: Vec<usize>,
        terminal: Option<usize>,
        streaming: bool,
        start: Option<SharedString>,
        anchor: usize,
        steers: Vec<(usize, usize)>,
    }
    let mut groups: Vec<(SharedString, Group)> = Vec::new();
    let mut runless_failed: HashSet<SharedString> = HashSet::new();
    let mut runless: Option<SharedString> = None;
    let mut response: Option<SharedString> = None;
    let mut boundary: Option<(SharedString, usize)> = None;
    for (index, entry) in entries.iter().enumerate() {
        if entry.starts_response() {
            boundary = (index + 1 < entries.len()).then(|| (entry.created_at.clone(), index + 1));
            let run = entry.boundary_run().cloned();
            runless = run.is_none().then(|| format!("runless:{}", entry.id).into());
            response = run.or_else(|| runless.clone());
            continue;
        }
        if entry.is_steer() {
            let run = entry.run_id.as_ref().or(response.as_ref());
            if let Some((_, group)) = groups.iter_mut().find(|(key, _)| Some(key) == run) {
                if !group.entries.is_empty() {
                    if group.steers.last().is_some_and(|(start, _)| *start == group.entries.len()) {
                        group.steers.pop();
                    }
                    group.steers.push((group.entries.len(), index));
                }
            }
            continue;
        }
        let Some(own) = entry.fold_run(runless.as_ref()) else { continue };
        // Resumed subagents keep their position and fold with their original turn.
        let run = match &response {
            Some(response) if prompt_index.get(&own).is_some_and(|prompt| *prompt > index) => response.clone(),
            _ => own,
        };
        if Some(&run) == runless.as_ref() && entry.failure.is_some() {
            runless_failed.insert(run.clone());
        }
        let position = match groups.iter().position(|(key, _)| *key == run) {
            Some(position) => position,
            None => {
                let (start, anchor) = match boundary.take() {
                    Some((start, anchor)) => (Some(start), anchor),
                    None => (None, index),
                };
                let group =
                    Group { entries: Vec::new(), terminal: None, streaming: false, start, anchor, steers: Vec::new() };
                groups.push((run, group));
                groups.len() - 1
            }
        };
        let group = &mut groups[position].1;
        group.entries.push(index);
        if let Kind::Assistant { streaming, .. } = entry.kind {
            if terminal.contains(&index) {
                group.terminal = Some(index);
            }
            group.streaming |= streaming;
        }
    }

    let mut folds = HashMap::new();
    for (run, group) in groups {
        if unfolded.contains(&run) || interrupted.contains(&run) || runless_failed.contains(&run) || group.streaming {
            continue;
        }
        let terminal_position = group
            .terminal
            .and_then(|terminal| group.entries.iter().position(|index| *index == terminal))
            .unwrap_or(group.entries.len());
        let mut hidden = HashSet::new();
        for (position, &index) in group.entries.iter().enumerate() {
            let entry = &entries[index];
            if Some(index) == group.terminal || entry.is_compaction() {
                continue;
            }
            let trailing_ok = entry.work().is_some_and(|e| e.status != Some(Status::InProgress) && !e.failed());
            if position > terminal_position && !trailing_ok {
                continue;
            }
            if matches!(&entry.kind, Kind::Event { item_type, .. } if is_resource_card(item_type)) {
                continue;
            }
            if entry.work().is_some_and(|e| e.item_type == "notification" || e.question_answered) {
                continue;
            }
            hidden.insert(index);
        }
        if hidden.is_empty() {
            continue;
        }
        let (Some(&first), Some(&last)) = (group.entries.first(), group.entries.last()) else { continue };
        let latest = latest_run.filter(|latest| latest.id == run.as_ref());
        let elapsed = match latest.and_then(|l| Some((l.started_at.as_deref()?, l.completed_at.as_deref()?))) {
            Some((start, end)) => elapsed_ms(start, end),
            None => {
                let start = group.start.as_ref().unwrap_or(&entries[first].created_at);
                let last_end = &entries[last].end;
                let end = group.terminal.map_or(last_end.as_ref(), |t| later(&entries[t].end, last_end));
                elapsed_ms(start, end)
            }
        };
        let duration = elapsed.map(work_log::format_duration);
        let label = match (latest.is_some_and(|l| l.status == "interrupted"), duration) {
            (true, Some(duration)) => format!("You stopped after {duration}"),
            (true, None) => "You stopped this response".into(),
            (false, Some(duration)) => format!("Worked for {duration}"),
            (false, None) => "Worked".into(),
        };
        let segment_end = group.steers.first().map_or(group.entries.len(), |(start, _)| *start);
        let segment_hidden =
            |segment: &[usize]| segment.iter().copied().filter(|index| hidden.contains(index)).collect::<HashSet<_>>();
        folds.insert(
            group.anchor,
            Fold { key: run.clone(), hidden: segment_hidden(&group.entries[..segment_end]), label: label.into() },
        );
        for (position, &(start, steer)) in group.steers.iter().enumerate() {
            let end = group.steers.get(position + 1).map_or(group.entries.len(), |(next, _)| *next);
            let segment = &group.entries[start..end];
            let hidden = segment_hidden(segment);
            let Some(&anchor) = segment.first() else { continue };
            if hidden.is_empty() {
                continue;
            }
            let steer = &entries[steer];
            let last_end =
                segment.iter().map(|index| entries[*index].end.as_ref()).fold(steer.created_at.as_ref(), later);
            let label = match elapsed_ms(&steer.created_at, last_end) {
                Some(ms) => format!("Continued for {}", work_log::format_duration(ms)),
                None => "Continued".into(),
            };
            folds.insert(anchor, Fold { key: format!("{run}:steer:{}", steer.id).into(), hidden, label: label.into() });
        }
    }
    folds
}

/// The chat rows for a thread. `expanded` holds the keys of open groups,
/// folds and entries ([`group_key`], [`fold_key`], [`entry_key`]).
pub fn rows(projection: &ThreadProjection, expanded: &HashSet<SharedString>, cache: &mut EntryCache) -> Vec<Row> {
    let entries = timeline_entries(projection, cache);
    let pending: HashSet<&str> =
        projection.pending_approvals().into_iter().map(|(request, _)| request.id.as_str()).collect();
    let is_pending = |entry: &Entry| entry.approval_request_id.as_deref().is_some_and(|id| pending.contains(id));
    let latest_run = projection.runs.last();
    let is_working = projection.is_running();
    let unsettled: Option<SharedString> =
        latest_run.filter(|run| is_active_run_status(&run.status)).map(|run| run.id.clone().into());
    let terminal = terminal_assistant_messages(&entries);

    let mut failed_runs: HashSet<SharedString> = HashSet::new();
    let (mut usage_limited, mut other_failures) = (HashSet::new(), HashSet::new());
    if let Some(run) = latest_run.filter(|run| run.status == "failed") {
        failed_runs.insert(SharedString::from(run.id.clone()));
    }
    for (run, usage_limit) in entries.iter().filter_map(|entry| entry.failure.as_ref()) {
        failed_runs.insert(run.clone());
        if *usage_limit {
            usage_limited.insert(run.clone())
        } else {
            other_failures.insert(run.clone())
        };
    }
    // Usage-limit pauses keep their tool groups; any other failure itemizes the run's work.
    let itemized: HashSet<&SharedString> =
        failed_runs.iter().filter(|run| !usage_limited.contains(*run) || other_failures.contains(*run)).collect();

    let last_boundary = entries.iter().rposition(|entry| entry.starts_response());
    let mut active_responses: HashSet<SharedString> = unsettled.iter().cloned().collect();
    if is_working && unsettled.is_some() {
        let after = last_boundary.map_or(0, |index| index + 1);
        active_responses.extend(entries[after..].iter().filter_map(|e| e.response_run().cloned()));
    }
    let unfolded: HashSet<SharedString> = active_responses.iter().chain(&failed_runs).cloned().collect();
    let folds = turn_folds(&entries, &terminal, latest_run, &unfolded);
    let mut collapsed: HashSet<usize> = folds
        .values()
        .filter(|fold| !expanded.contains(&fold_key(&fold.key)))
        .flat_map(|fold| fold.hidden.iter().copied())
        .collect();
    collapsed.retain(|index| !entries[*index].work().is_some_and(|e| is_pending(e)));

    let is_active_response = |run: Option<&SharedString>| run.is_some_and(|run| active_responses.contains(run));
    let in_active_run = |entry: &Entry| {
        is_working && entry.status == Some(Status::InProgress) && entry.run_id.is_some() && entry.run_id == unsettled
    };
    let active_header = if is_working { last_boundary.map_or(0, |index| index + 1) } else { entries.len() };

    // The running turn's trailing work becomes one live row.
    let mut active_tools: Vec<usize> = Vec::new();
    if is_working && unsettled.is_some() {
        for index in (active_header..entries.len()).rev() {
            let entry = &entries[index];
            let Some(work) = entry.work() else { break };
            let breaks = work.tone == Tone::Error
                || work.activity == Some(Activity::Error)
                || matches!(work.item_type.as_ref(), "system_notice" | "notification")
                || work.question_answered
                || is_pending(work)
                || !is_active_response(work.run_id.as_ref())
                || entry.is_compaction()
                || collapsed.contains(&index)
                || folds.contains_key(&index);
            if breaks {
                break;
            }
            active_tools.push(index);
        }
        active_tools.reverse();
    }
    let visible_active: Vec<usize> = active_tools
        .iter()
        .copied()
        .filter(|i| entries[*i].work().is_some_and(|e| e.is_visible_in_group(true)))
        .collect();
    let work_at = |index: usize| entries[index].work().expect("work entry").clone();
    let latest_visible = visible_active.last().copied();
    let latest_running = visible_active.iter().rev().copied().find(|i| work_at(*i).is_active_activity());
    let keeps_live =
        latest_running.is_some() || (latest_visible.is_some_and(|i| work_at(i).succeeded()) && pending.is_empty());
    let latest_failed = latest_running.is_none()
        && latest_visible.is_some_and(|i| {
            let entry = work_at(i);
            entry.status != Some(Status::Declined) && entry.failed()
        });
    let thought = visible_active
        .iter()
        .rev()
        .map(|i| work_at(*i))
        .find(|e| e.is_reasoning() && e.detail.as_ref().is_some_and(|d| !d.trim().is_empty()));
    let anchor = active_tools.first().map(|i| entries[*i].id.clone());
    let visible_active_entries: Vec<Rc<Entry>> = visible_active.iter().map(|i| work_at(*i)).collect();
    let details_row = |group: &SharedString, entries: &[Rc<Entry>]| {
        let entries = entries.iter().map(|e| (e.clone(), expanded.contains(&entry_key(&e.id)))).collect();
        Row::Work(WorkRow {
            id: format!("{group}:details").into(),
            kind: WorkRowKind::Details { entries },
            continues: false,
        })
    };
    let live_row = match (&anchor, latest_visible) {
        (Some(anchor), Some(latest)) if !latest_failed => {
            let group = group_key(anchor);
            let entry = work_at(latest_running.unwrap_or(latest));
            let open = expanded.contains(&group);
            Some((
                Row::Work(WorkRow {
                    id: if keeps_live { "live-activity-row".into() } else { format!("work-live:{anchor}").into() },
                    kind: WorkRowKind::Live {
                        label: work_log::collapse_whitespace(&work_log::live_label(&entry, keeps_live)).into(),
                        thought: thought
                            .as_ref()
                            .map(|t| work_log::live_thought_line(t.detail.as_deref().unwrap_or("")).into()),
                        group: group.clone(),
                        entry,
                        expanded: open,
                        active: keeps_live,
                    },
                    continues: false,
                }),
                open.then(|| details_row(&group, &visible_active_entries)),
            ))
        }
        _ => None,
    };
    let active_ids: HashSet<usize> =
        if live_row.is_some() || latest_failed { active_tools.iter().copied().collect() } else { HashSet::new() };
    let mut live_row = live_row;

    let mut rows: Vec<Row> = Vec::new();
    let mut has_activity = false;
    let mut index = 0;
    while index < entries.len() {
        let entry = &entries[index];
        if is_working && index == active_header {
            rows.push(Row::Working { started_at: latest_run.and_then(|r| r.started_at.clone()).map(Into::into) });
        }
        if Some(index) == latest_visible {
            if let Some((row, details)) = live_row.take() {
                has_activity |= keeps_live;
                rows.push(row);
                rows.extend(details);
            }
        }
        if matches!(&entry.kind, Kind::Event { item_type, .. } if item_type == "run_interrupt_request") {
            index += 1;
            continue;
        }
        if let Some(fold) = folds.get(&index) {
            let key = fold_key(&fold.key);
            rows.push(Row::TurnFold { expanded: expanded.contains(&key), key, label: fold.label.clone() });
        }
        if collapsed.contains(&index) || active_ids.contains(&index) {
            index += 1;
            continue;
        }
        match &entry.kind {
            Kind::Work(work) if entry.is_compaction() => rows.push(Row::Divider {
                id: entry.id.clone(),
                label: work.label.clone(),
                failed: work.status == Some(Status::Failed),
            }),
            Kind::Work(work) => {
                let standalone = |e: &Entry| {
                    (e.item_type == "error" && e.status == Some(Status::Failed))
                        || e.tone == Tone::Error
                        || e.activity == Some(Activity::Error)
                        || matches!(e.item_type.as_ref(), "system_notice" | "notification")
                        || e.question_answered
                        || is_pending(e)
                };
                if standalone(work) || work.run_id.as_ref().is_some_and(|run| itemized.contains(run)) {
                    rows.push(single_row(work, work.label.to_string(), is_pending(work), expanded));
                    index += 1;
                    continue;
                }
                let mut group = vec![work.clone()];
                let mut cursor = index + 1;
                while let Some(next) = entries.get(cursor) {
                    let Some(next_work) = next.work() else { break };
                    if next.is_compaction()
                        || standalone(next_work)
                        || active_ids.contains(&cursor)
                        || collapsed.contains(&cursor)
                        || folds.contains_key(&cursor)
                        || next_work.run_id != work.run_id
                    {
                        break;
                    }
                    group.push(next_work.clone());
                    cursor += 1;
                }
                let visible: Vec<Rc<Entry>> =
                    group.into_iter().filter(|e| e.is_visible_in_group(in_active_run(e))).collect();
                let visible_refs: Vec<&Entry> = visible.iter().map(Rc::as_ref).collect();
                let group_id = group_key(&entry.id);
                let open = expanded.contains(&group_id);
                if let Some(latest) = visible.iter().rev().find(|e| in_active_run(e)) {
                    has_activity = true;
                    rows.push(Row::Work(WorkRow {
                        id: format!("work-live:{}", entry.id).into(),
                        kind: WorkRowKind::Live {
                            group: group_id.clone(),
                            label: work_log::collapse_whitespace(&work_log::live_label(latest, true)).into(),
                            entry: latest.clone(),
                            thought: None,
                            expanded: open,
                            active: true,
                        },
                        continues: false,
                    }));
                    if open {
                        rows.push(details_row(&group_id, &visible));
                    }
                } else if let [single] = visible.as_slice() {
                    if single.is_tool_like() {
                        let label = if single.action == work_log::Action::Edit {
                            work_log::summarize_tool_group(&visible_refs).0
                        } else {
                            work_log::single_tool_call_label(single)
                        };
                        rows.push(single_row(single, label, false, expanded));
                    } else {
                        let label = work_log::collapse_whitespace(&single.label).into();
                        rows.push(toggle_row(&entry.id, &group_id, label, &visible_refs, open));
                        if open {
                            rows.push(details_row(&group_id, &visible));
                        }
                    }
                } else if !visible.is_empty() {
                    let summary = work_log::summarize_tool_group(&visible_refs).0;
                    rows.push(toggle_row(&entry.id, &group_id, summary.into(), &visible_refs, open));
                    if open {
                        rows.push(details_row(&group_id, &visible));
                    }
                }
                index = cursor;
                continue;
            }
            Kind::Plan { markdown } => rows.push(Row::Plan { id: entry.id.clone(), markdown: markdown.clone() }),
            Kind::Event { label, .. } => rows.push(Row::Event { id: entry.id.clone(), label: label.clone() }),
            Kind::User { text, steer } => rows.push(Row::User {
                id: entry.id.clone(),
                text: text.clone(),
                steer: *steer,
                grouped_with_previous: false,
                grouped_with_next: false,
                continues_steer: false,
            }),
            Kind::Assistant { text, streaming } => rows.push(Row::Assistant {
                id: entry.id.clone(),
                text: text.clone(),
                streaming: *streaming,
                terminal: terminal.contains(&index) && !is_active_response(entry.run_id.as_ref()),
            }),
        }
        index += 1;
    }

    if is_working && !rows.iter().any(|row| matches!(row, Row::Working { .. })) && active_header == entries.len() {
        rows.push(Row::Working { started_at: latest_run.and_then(|r| r.started_at.clone()).map(Into::into) });
    }
    if is_working && (latest_failed || (!has_activity && pending.is_empty())) {
        let group = anchor.as_ref().filter(|_| latest_failed).map(|anchor| group_key(anchor));
        let open = group.as_ref().is_some_and(|group| expanded.contains(group));
        rows.push(Row::Work(WorkRow {
            id: "live-activity-row".into(),
            kind: WorkRowKind::Thinking { group: group.clone(), expanded: open },
            continues: false,
        }));
        if let (Some(group), true) = (&group, open) {
            rows.push(details_row(group, &visible_active_entries));
        }
    }

    let flags: Vec<(bool, bool, bool)> = rows
        .iter()
        .map(|row| (row.is_work(), row.is_own_user_message(), matches!(row, Row::User { steer: Some(_), .. })))
        .collect();
    for (index, row) in rows.iter_mut().enumerate() {
        let next = flags.get(index + 1).copied().unwrap_or_default();
        let previous = index.checked_sub(1).and_then(|i| flags.get(i)).copied().unwrap_or_default();
        match row {
            Row::Work(work) => work.continues = next.0,
            Row::User { steer, grouped_with_previous, grouped_with_next, continues_steer, .. } => {
                *grouped_with_previous = previous.1;
                *grouped_with_next = next.1;
                *continues_steer = previous.2 && steer.is_some();
            }
            _ => {}
        }
    }
    rows
}

fn single_row(entry: &Rc<Entry>, label: String, pending_approval: bool, expanded: &HashSet<SharedString>) -> Row {
    Row::Work(WorkRow {
        id: entry.id.clone(),
        kind: WorkRowKind::Single {
            entry: entry.clone(),
            label: work_log::collapse_whitespace(&label).into(),
            expanded: expanded.contains(&entry_key(&entry.id)),
            pending_approval,
        },
        continues: false,
    })
}

fn toggle_row(id: &str, group: &SharedString, summary: SharedString, entries: &[&Entry], expanded: bool) -> Row {
    Row::Work(WorkRow {
        id: format!("work-toggle:{id}").into(),
        kind: WorkRowKind::Toggle {
            group: group.clone(),
            summary,
            icon: work_log::summary_icon(work_log::summary_kind(entries)),
            expanded,
        },
        continues: false,
    })
}

/// Range to splice so `old` becomes `new`: rows before the first difference stay.
pub fn changed_range(old: &[Row], new: &[Row]) -> Option<(std::ops::Range<usize>, usize)> {
    let common_prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    if common_prefix == old.len() && common_prefix == new.len() {
        return None;
    }
    let common_suffix =
        old[common_prefix..].iter().rev().zip(new[common_prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
    let old_end = old.len() - common_suffix;
    let new_count = new.len() - common_suffix - common_prefix;
    Some((common_prefix..old_end, new_count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(id: &str) -> Row {
        Row::User {
            id: id.into(),
            text: id.into(),
            steer: None,
            grouped_with_previous: false,
            grouped_with_next: false,
            continues_steer: false,
        }
    }

    fn projection(items: Vec<Value>, runs: Value) -> ThreadProjection {
        let rows: Vec<Value> = items
            .into_iter()
            .map(|item| {
                let id = item["id"].clone();
                json!({"visibility": "visible", "sourceThreadId": "t", "sourceItemId": id, "item": item})
            })
            .collect();
        serde_json::from_value(json!({
            "thread": {"id": "t", "title": "T"},
            "runs": runs,
            "visibleTurnItems": rows,
        }))
        .unwrap()
    }

    fn item(id: &str, kind: &str, run: &str, at: &str, extra: Value) -> Value {
        let mut value =
            json!({"id": id, "type": kind, "runId": run, "status": "completed", "startedAt": at, "updatedAt": at});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        value
    }

    fn describe(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::User { text, .. } => format!("user {text}"),
                Row::Assistant { text, .. } => format!("assistant {text}"),
                Row::TurnFold { label, .. } => format!("fold {label}"),
                Row::Working { .. } => "working".into(),
                Row::Work(work) => match &work.kind {
                    WorkRowKind::Single { label, .. } => format!("work {label}"),
                    WorkRowKind::Toggle { summary, .. } => format!("toggle {summary}"),
                    WorkRowKind::Details { entries } => format!("details {}", entries.len()),
                    WorkRowKind::Live { label, .. } => format!("live {label}"),
                    WorkRowKind::Thinking { .. } => "thinking".into(),
                },
                Row::Plan { .. } => "plan".into(),
                Row::Divider { label, .. } => format!("divider {label}"),
                Row::Event { label, .. } => format!("event {label}"),
            })
            .collect()
    }

    #[test]
    fn changed_range_covers_only_the_difference() {
        let a = [user("a"), user("b"), user("c")];
        assert_eq!(changed_range(&a, &a), None);
        assert_eq!(changed_range(&a, &[user("a"), user("b"), user("c"), user("d")]), Some((3..3, 1)));
        assert_eq!(changed_range(&a, &[user("a"), user("x"), user("c")]), Some((1..2, 1)));
        assert_eq!(changed_range(&a, &[user("a"), user("c")]), Some((1..2, 0)));
        assert_eq!(changed_range(&a, &[]), Some((0..3, 0)));
    }

    #[test]
    fn pending_queued_messages_and_checkpoints_are_hidden() {
        let item = |value: Value| serde_json::from_value::<TurnItem>(value).unwrap();
        let queued = item(
            json!({"id": "q", "type": "user_message", "inputIntent": "queued_turn", "status": "pending", "text": "later"}),
        );
        let checkpoint = item(json!({"id": "c", "type": "checkpoint", "status": "completed"}));
        let started = item(
            json!({"id": "s", "type": "user_message", "inputIntent": "queued_turn", "status": "completed", "text": "now"}),
        );
        assert!(timeline_entry(&queued).is_none());
        assert!(timeline_entry(&checkpoint).is_none());
        assert!(matches!(timeline_entry(&started).unwrap().kind, Kind::User { steer: None, .. }));
    }

    fn command(id: &str, run: &str, at: &str) -> Value {
        item(id, "command_execution", run, at, json!({"input": "ls"}))
    }

    fn turn(run: &str) -> Vec<Value> {
        vec![
            item(
                &format!("{run}-u"),
                "user_message",
                run,
                "2026-11-04T10:00:00.000Z",
                json!({"text": "go", "inputIntent": "turn_start"}),
            ),
            item(
                &format!("{run}-a1"),
                "assistant_message",
                run,
                "2026-11-04T10:00:01.000Z",
                json!({"text": "looking"}),
            ),
            command(&format!("{run}-c1"), run, "2026-11-04T10:00:02.000Z"),
            command(&format!("{run}-c2"), run, "2026-11-04T10:00:03.000Z"),
            item(
                &format!("{run}-r"),
                "dynamic_tool",
                run,
                "2026-11-04T10:00:04.000Z",
                json!({"toolName": "Read", "input": {"path": "a.rs"}}),
            ),
            item(&format!("{run}-a2"), "assistant_message", run, "2026-11-04T10:01:05.000Z", json!({"text": "done"})),
        ]
    }

    #[test]
    fn finished_turns_fold_their_work_and_expand_on_demand() {
        let p = projection(turn("r1"), json!([{"id": "r1", "status": "completed"}]));
        assert_eq!(
            describe(&rows(&p, &HashSet::new(), &mut EntryCache::default())),
            ["user go", "fold Worked for 1m 5s", "assistant done"]
        );
        let open = HashSet::from([fold_key("r1")]);
        assert_eq!(
            describe(&rows(&p, &open, &mut EntryCache::default())),
            [
                "user go",
                "fold Worked for 1m 5s",
                "assistant looking",
                "toggle Ran 2 commands and read 1 file",
                "assistant done"
            ]
        );
        let open = HashSet::from([fold_key("r1"), group_key("r1-c1")]);
        assert_eq!(
            describe(&rows(&p, &open, &mut EntryCache::default()))[3..5],
            ["toggle Ran 2 commands and read 1 file", "details 3"]
        );
    }

    #[test]
    fn running_turns_show_working_live_and_thinking_rows() {
        let mut items = turn("r1");
        items.pop();
        let mut running = command("r1-c3", "r1", "2026-11-04T10:00:05.000Z");
        running["status"] = json!("running");
        items.push(running);
        let p = projection(items, json!([{"id": "r1", "status": "running", "startedAt": "2026-11-04T10:00:00.000Z"}]));
        assert_eq!(
            describe(&rows(&p, &HashSet::new(), &mut EntryCache::default())),
            ["user go", "working", "assistant looking", "live Running ls"]
        );

        let mut items = turn("r1");
        items.pop();
        let p = projection(items, json!([{"id": "r1", "status": "running"}]));
        // The latest tool succeeded, so the live row stays up instead of "Thinking".
        assert_eq!(
            describe(&rows(&p, &HashSet::new(), &mut EntryCache::default())),
            ["user go", "working", "assistant looking", "live Read a.rs"]
        );

        let items = turn("r1")[..2].to_vec();
        let p = projection(items, json!([{"id": "r1", "status": "running"}]));
        assert_eq!(
            describe(&rows(&p, &HashSet::new(), &mut EntryCache::default())),
            ["user go", "working", "assistant looking", "thinking"]
        );
    }

    #[test]
    fn work_rows_know_when_the_next_row_continues_the_log() {
        let p = projection(turn("r1"), json!([{"id": "r1", "status": "completed"}]));
        let rows = rows(&p, &HashSet::from([fold_key("r1"), group_key("r1-c1")]), &mut EntryCache::default());
        let continues: Vec<bool> =
            rows.iter().filter_map(|row| if let Row::Work(work) = row { Some(work.continues) } else { None }).collect();
        assert_eq!(continues, [true, false]);
    }

    #[test]
    fn cached_entries_refresh_when_their_item_changes() {
        let mut cache = EntryCache::default();
        let mut items = turn("r1")[..3].to_vec();
        items[2]["status"] = json!("running");
        let runs = json!([{"id": "r1", "status": "running"}]);
        let live = |rows: Vec<Row>| describe(&rows).last().cloned().unwrap();
        assert_eq!(
            live(rows(&projection(items.clone(), runs.clone()), &HashSet::new(), &mut cache)),
            "live Running ls"
        );
        items[2]["input"] = json!("cargo test");
        items[2]["updatedAt"] = json!("2026-11-04T10:00:09.000Z");
        assert_eq!(live(rows(&projection(items, runs), &HashSet::new(), &mut cache)), "live Running cargo");
    }

    #[test]
    fn steers_and_consecutive_messages_group() {
        let items = vec![
            item(
                "u1",
                "user_message",
                "r1",
                "2026-11-04T10:00:00.000Z",
                json!({"text": "a", "inputIntent": "turn_start"}),
            ),
            item("u2", "user_message", "r1", "2026-11-04T10:00:01.000Z", json!({"text": "b", "inputIntent": "steer"})),
            item("u3", "user_message", "r1", "2026-11-04T10:00:02.000Z", json!({"text": "c", "inputIntent": "steer"})),
        ];
        let p = projection(items, json!([]));
        let rows = rows(&p, &HashSet::new(), &mut EntryCache::default());
        assert!(matches!(rows[0], Row::User { grouped_with_next: true, steer: None, .. }));
        // A steer under a plain message keeps its label; the second steer in a row drops it.
        assert!(matches!(
            rows[1],
            Row::User { grouped_with_previous: true, steer: Some(Steer::Steered), continues_steer: false, .. }
        ));
        assert!(matches!(rows[2], Row::User { continues_steer: true, .. }));
    }
}
