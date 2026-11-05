//! Turns a thread's `visibleTurnItems` into chat rows. Mirrors
//! `deriveTimelineEntriesFromVisibleTurnItems` in `apps/web/src/session-logic.ts`
//! and `deriveMessagesTimelineRows` in
//! `apps/web/src/components/chat/MessagesTimeline.logic.ts`.
//!
//! Not ported yet: worktree-setup rows, grouped subagent batches ("3 subagents"),
//! the separate assistant-meta row after trailing work, and the compaction-run
//! special cases.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::SharedString;
use serde_json::Value;

use crate::model::{self, Run, ThreadProjection, TurnItem, is_active_run_status};
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
        created_at: SharedString,
        /// The checkpoint "Edit from here" rewinds to.
        revert: Option<Checkpoint>,
        /// "Sent by T3 Code" above the bubble; not the user's own words, so it never groups.
        sent_by_t3code: bool,
    },
    Assistant {
        id: SharedString,
        text: SharedString,
        streaming: bool,
        /// The turn's last reply once the turn finished.
        terminal: bool,
        updated_at: SharedString,
        /// The item's status, shown as a pill unless it completed.
        status: SharedString,
        /// The thread and run "Fork from this response" forks at.
        fork: Option<(SharedString, SharedString)>,
        run_id: Option<SharedString>,
    },
    /// A finished reply's actions, moved under the tool groups that ran after it.
    AssistantMeta {
        id: SharedString,
        text: SharedString,
        updated_at: SharedString,
        status: SharedString,
        fork: Option<(SharedString, SharedString)>,
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
        /// A failed compaction's summary.
        failure: Option<SharedString>,
    },
    Event {
        id: SharedString,
        label: SharedString,
    },
    /// A centered lifecycle label between hairlines (`TimelineSystemDivider`): a stop, a fork.
    System(Rc<SystemDivider>),
    /// A workflow thread's opening: its script as the plan, with the members' progress.
    Workflow {
        row: Rc<crate::workflow::WorkflowRow>,
        expanded: bool,
    },
    /// Subagents one provider turn started, folded under "3 subagents" (`V2SubagentGroup`).
    SubagentGroup {
        key: SharedString,
        cards: Vec<Rc<SubagentCard>>,
        expanded: bool,
    },
    /// A subagent's card, opening its thread (`SubagentTimelineLink`).
    Subagent(Rc<SubagentCard>),
    /// A page an agent rendered (`html_render`) or an MCP app, opened in the browser.
    Embed(Rc<Embed>),
    /// "Superseded attempt": the work a steer cut off, folded.
    AttemptFold {
        key: SharedString,
        expanded: bool,
    },
    /// A secret or link an agent asked for, while it waits on the user (`SecretRequestCard`, `LinkRequestCard`).
    Request(Rc<RequestCard>),
    /// A queued follow-up, after the timeline (`QueuedRunsControl`).
    Queued {
        message: Rc<crate::queue::QueuedMessage>,
        last: bool,
    },
    /// A line of what the thread is doing now, before the queue (`ThreadNow`).
    Now(Rc<crate::thread_now::NowLine>),
    /// A new worktree's setup (`WorktreeSetupCard`); `embedded` folds it to a summary once the agent runs.
    WorktreeSetup {
        snapshot: Rc<crate::worktree_setup::Snapshot>,
        embedded: bool,
    },
    /// "Show all N queued" under a long queue.
    QueueToggle {
        total: usize,
        expanded: bool,
    },
}

/// An `html_render` page or an MCP app's attachment (`htmlRenderFromToolItem`, `mcpAppFromToolItem`).
#[derive(Clone, Debug, PartialEq)]
pub struct Embed {
    pub id: SharedString,
    pub title: SharedString,
    pub attachment_id: SharedString,
    /// The MCP server, for an app; `None` for a rendered page.
    pub app_server: Option<SharedString>,
}

impl Embed {
    /// `htmlRenderFileName`: the title made safe for a file name.
    pub fn file_name(&self) -> String {
        let cleaned: String =
            self.title.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { ' ' } else { c }).collect();
        let name: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect();
        let name = name.trim();
        format!("{}.html", if name.is_empty() { "Page" } else { name })
    }
}

fn embed(item: &TurnItem) -> Option<Embed> {
    if item.item_type != "dynamic_tool" {
        return None;
    }
    let output = item.raw.get("output")?;
    if output.get("isError").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let tool = item.str("toolName").unwrap_or_default();
    fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
        value.get(key).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty())
    }
    if let Some(app) = output.get("t3McpApp") {
        let (server, name) = (text(app, "server")?, text(app, "tool")?);
        if tool != format!("{server}.{name}") && tool != format!("mcp__{server}__{name}") {
            return None;
        }
        return Some(Embed {
            id: item.id.clone().into(),
            title: format!("{server} app").into(),
            attachment_id: text(app, "attachmentId")?.to_owned().into(),
            app_server: Some(server.to_owned().into()),
        });
    }
    let page = output.get("htmlRender").filter(|_| item.status == "completed" && tool.ends_with("html_render"))?;
    let title: String = text(page, "title").unwrap_or("HTML").chars().take(200).collect();
    Some(Embed {
        id: item.id.clone().into(),
        title: title.into(),
        attachment_id: text(page, "attachmentId")?.to_owned().into(),
        app_server: None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemDivider {
    pub id: SharedString,
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub fork: bool,
    /// The action's label and the thread it opens.
    pub action: Option<(SharedString, SharedString)>,
}

/// `V2LifecycleRow`'s divider for a finished stop or a fork.
fn system_divider(item: &TurnItem) -> Option<SystemDivider> {
    let text = |pointer: &str| {
        item.raw
            .pointer(pointer)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(|v| SharedString::from(v.to_owned()))
    };
    let (label, detail, action): (&str, _, _) = match item.item_type.as_str() {
        "run_interrupt_result" => ("Run interrupted", text("/message"), None),
        "fork" if item.raw.pointer("/source/type").and_then(Value::as_str) == Some("run") => (
            "Forked from conversation",
            None,
            text("/source/threadId").map(|id| (SharedString::from("Open source conversation"), id)),
        ),
        "fork" => ("Conversation fork", None, text("/targetThreadId").map(|id| (SharedString::from("Open fork"), id))),
        _ => return None,
    };
    Some(SystemDivider {
        id: item.id.clone().into(),
        label: label.into(),
        detail,
        fork: item.item_type == "fork",
        action,
    })
}

/// A pending `secret_request` or `link_request` asked in this thread.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestCard {
    pub item_id: SharedString,
    pub thread_id: SharedString,
    pub reason: SharedString,
    pub kind: RequestKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RequestKind {
    Secret {
        label: SharedString,
        placeholder: Option<SharedString>,
        /// `delivery: "file"`: the agent's commands can read the value.
        file: bool,
    },
    Link {
        label: Option<SharedString>,
        url: Option<SharedString>,
        environment_id: Option<SharedString>,
        requested_access: Option<SharedString>,
    },
}

impl RequestCard {
    /// `linkRequestTargetName`.
    pub fn link_target(&self) -> String {
        match &self.kind {
            RequestKind::Link { label, url, .. } => label
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .or(url.as_deref())
                .unwrap_or("a machine")
                .to_owned(),
            RequestKind::Secret { label, .. } => label.to_string(),
        }
    }

    /// The agent left the machine for the user to pick (`picksMachine`).
    pub fn picks_machine(&self) -> bool {
        matches!(&self.kind, RequestKind::Link { environment_id: None, url: None, .. })
    }
}

fn request_card(item: &TurnItem) -> RequestCard {
    let text = |key: &str| item.str(key).map(|v| SharedString::from(v.to_owned()));
    let kind = match item.item_type.as_str() {
        "secret_request" => RequestKind::Secret {
            label: text("label").unwrap_or_else(|| "Secret".into()),
            placeholder: text("placeholder"),
            file: item.str("delivery") == Some("file"),
        },
        _ => RequestKind::Link {
            label: text("label"),
            url: text("url"),
            environment_id: text("environmentId"),
            requested_access: text("requestedAccess"),
        },
    };
    RequestCard {
        item_id: item.id.clone().into(),
        thread_id: item.thread_id.clone().into(),
        reason: text("reason").unwrap_or_default(),
        kind,
    }
}

/// `accessConfig[access].label`.
pub fn access_label(access: &str) -> &'static str {
    match access {
        "read-only" => "Read only",
        "approval-required" => "Supervised",
        "auto-accept-edits" => "Auto-accept edits",
        "auto" => "Auto",
        "full-access" => "Full access",
        _ => "Unknown",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    Info,
    Muted,
    Success,
    Destructive,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubagentCard {
    pub id: SharedString,
    pub title: SharedString,
    pub status_label: &'static str,
    pub dot: Dot,
    /// The result or progress, as plain text.
    pub detail: Option<SharedString>,
    pub failed: bool,
    /// The status shows beside the detail too, until the subagent completes.
    pub status_beside_detail: bool,
    pub live: bool,
    pub started_at: Option<SharedString>,
    pub completed_at: Option<SharedString>,
    pub child_thread_id: Option<SharedString>,
    pub driver: SharedString,
    pub status: SharedString,
    /// Subagents one provider turn started together group into one row.
    pub run_id: Option<SharedString>,
    pub provider_turn_id: Option<SharedString>,
}

/// `subagentGroupSummary` + `summarizeSubagentStatuses`: "2 working · 1 done", and whether
/// any still runs or failed.
pub fn subagent_group_status(cards: &[Rc<SubagentCard>]) -> (String, bool, bool) {
    let mut counts = [("working", 0), ("done", 0), ("failed", 0), ("stopped", 0), ("idle", 0)];
    for card in cards {
        let slot = match card.status.as_ref() {
            "pending" | "running" | "waiting" => 0,
            "completed" => 1,
            "failed" => 2,
            "idle" => 4,
            _ => 3,
        };
        counts[slot].1 += 1;
    }
    let summary = counts
        .iter()
        .filter(|(_, count)| *count > 0)
        .map(|(key, count)| format!("{count} {key}"))
        .collect::<Vec<_>>()
        .join(" · ");
    (summary, counts[0].1 > 0, counts[2].1 > 0)
}

/// `subagentGroupTiming`: the earliest start to the latest end, open while any runs.
pub fn subagent_group_timing(cards: &[Rc<SubagentCard>]) -> (Option<i64>, Option<i64>, bool) {
    let ms = |at: &Option<SharedString>| at.as_deref().and_then(work_log::parse_iso_ms);
    let start = cards.iter().filter_map(|c| ms(&c.started_at)).min();
    let live = cards.iter().any(|c| c.live);
    let end_unknown = cards.iter().any(|c| ms(&c.completed_at).is_none());
    let end = cards.iter().filter_map(|c| ms(&c.completed_at)).max().filter(|_| !live && !end_unknown);
    (start, end, live)
}

pub fn subagent_group_key(first_id: &str) -> SharedString {
    format!("subagent-group:{first_id}").into()
}

/// Joins `card` to the subagent row before it when both came from the same provider turn.
fn push_subagent(rows: &mut Vec<Row>, card: Rc<SubagentCard>, expanded: &HashSet<SharedString>) {
    let same = |other: &SubagentCard| other.run_id == card.run_id && other.provider_turn_id == card.provider_turn_id;
    match rows.last_mut() {
        Some(Row::Subagent(previous)) if same(previous) => {
            let key = subagent_group_key(&previous.id);
            let cards = vec![previous.clone(), card];
            *rows.last_mut().expect("a last row") =
                Row::SubagentGroup { expanded: expanded.contains(&key), key, cards };
        }
        Some(Row::SubagentGroup { cards, .. }) if cards.first().is_some_and(|first| same(first)) => cards.push(card),
        _ => rows.push(Row::Subagent(card)),
    }
}

/// `formatSubagentDisplayTitle`: drops "Subagent: " and names a `/root/.../name_x` path "Name X".
pub(crate) fn subagent_title(title: &str) -> String {
    let title = match title.get(..9) {
        Some(prefix) if prefix.eq_ignore_ascii_case("subagent:") => title[9..].trim_start(),
        _ => title,
    };
    let Some(path) = title.strip_prefix("/root/") else { return title.to_owned() };
    let Some(name) = path.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()) else {
        return title.to_owned();
    };
    let words: Vec<String> = name
        .split(|c: char| c == '_' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        })
        .collect();
    if words.is_empty() { title.to_owned() } else { words.join(" ") }
}

/// `plainDetail`: markdown links and backticks dropped, list markers removed, whitespace collapsed.
pub fn plain_detail(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|close| after[close + 2..].find(')').map(|end| (close, close + 2 + end))) {
            Some((close, end)) if !after[..close].contains(']') => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..close]);
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let lines: Vec<&str> = out
        .lines()
        .map(|line| {
            let trimmed = line.trim_start_matches([' ', '\t']);
            trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).unwrap_or(line)
        })
        .collect();
    lines.join(" ").replace('`', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A subagent status's label and avatar dot.
pub(crate) fn subagent_status(status: &str) -> (&'static str, Dot) {
    match status {
        "pending" => ("Queued", Dot::Info),
        "running" => ("Running", Dot::Info),
        "waiting" => ("Waiting", Dot::Info),
        "idle" => ("Idle · resumable", Dot::Muted),
        "completed" => ("Completed", Dot::Success),
        "failed" => ("Failed", Dot::Destructive),
        _ => ("Stopped", Dot::Muted),
    }
}

fn subagent_card(item: &TurnItem) -> SubagentCard {
    let text = |key: &str| item.str(key).map(str::trim).filter(|t| !t.is_empty());
    let (status_label, dot) = subagent_status(&item.status);
    let settled = matches!(item.status.as_str(), "completed" | "failed" | "cancelled" | "interrupted");
    let (result, progress) = (text("result"), text("progress"));
    let raw = if settled { result.or(progress) } else { progress.or(result) };
    let detail = raw
        .filter(|raw| !raw.to_ascii_lowercase().starts_with("child task ended with status"))
        .map(plain_detail)
        .filter(|detail| !detail.is_empty());
    SubagentCard {
        id: item.id.clone().into(),
        title: subagent_title(text("title").unwrap_or("Subagent")).into(),
        status_label,
        dot,
        status_beside_detail: detail.is_some() && item.status != "completed",
        detail: detail.map(Into::into),
        failed: item.status == "failed",
        live: matches!(item.status.as_str(), "pending" | "running" | "waiting"),
        started_at: text("startedAt").map(|s| s.to_owned().into()),
        completed_at: text("completedAt").map(|s| s.to_owned().into()),
        child_thread_id: text("childThreadId").map(|s| s.to_owned().into()),
        driver: text("driver").unwrap_or_default().to_owned().into(),
        status: item.status.clone().into(),
        run_id: item.run_id.clone().map(Into::into),
        provider_turn_id: text("providerTurnId").map(|s| s.to_owned().into()),
    }
}

/// `formatElapsedSeconds`: "12s", "3m 05s", "1h 02m".
pub fn agent_elapsed(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    let minutes = seconds / 60;
    match (minutes, minutes / 60) {
        (0, _) => format!("{seconds}s"),
        (_, 0) => format!("{minutes}m {:02}s", seconds % 60),
        (_, hours) => format!("{hours}h {:02}m", minutes % 60),
    }
}

/// A checkpoint `checkpoint.rollback` can target.
#[derive(Clone, Debug, PartialEq)]
pub struct Checkpoint {
    pub scope_id: SharedString,
    pub checkpoint_id: SharedString,
    /// The run ordinal the thread rewinds to; 0 is the thread start.
    pub turn_count: i64,
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
    },
    /// A collapsed group's summary, "Ran 2 commands and read 1 file".
    Toggle {
        group: SharedString,
        summary: SharedString,
        icon: WorkIcon,
        expanded: bool,
        run_id: Option<SharedString>,
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
        matches!(self, Row::User { sent_by_t3code: false, .. })
    }
}

/// Keys in the expanded set, namespaced so groups, folds and entries never collide.
pub fn group_key(anchor_id: &str) -> SharedString {
    format!("work-group:{anchor_id}").into()
}

pub fn fold_key(run_key: &str) -> SharedString {
    format!("fold:{run_key}").into()
}

pub fn attempt_key(attempt_id: &str) -> SharedString {
    format!("attempt:{attempt_id}").into()
}

pub fn entry_key(entry_id: &str) -> SharedString {
    format!("entry:{entry_id}").into()
}

enum Kind {
    /// `revertible`: a turn start or queued turn, which "Edit from here" can rewind.
    User {
        text: SharedString,
        steer: Option<Steer>,
        revertible: bool,
        sent_by_t3code: bool,
    },
    Assistant {
        text: SharedString,
        streaming: bool,
    },
    Plan {
        markdown: SharedString,
    },
    Work(Rc<Entry>),
    Event {
        label: SharedString,
        item_type: SharedString,
        subagent: Option<Rc<SubagentCard>>,
        system: Option<Rc<SystemDivider>>,
    },
    Embed(Rc<Embed>),
    Request(Rc<RequestCard>),
}

struct TimelineEntry {
    id: SharedString,
    run_id: Option<SharedString>,
    created_at: SharedString,
    /// When the entry last changed: a message's `updatedAt`, else `created_at`.
    end: SharedString,
    /// The run of a top-level failed `error` item, and whether it was a usage limit.
    failure: Option<(SharedString, bool)>,
    status: SharedString,
    /// The thread the item lives in; another one for inherited history.
    source_thread_id: SharedString,
    inherited: bool,
    node_id: Option<SharedString>,
    /// The item's own `completedAt`.
    completed_at: Option<SharedString>,
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
            Kind::Assistant { .. } | Kind::Plan { .. } | Kind::Work(_) | Kind::Embed(_) => self.run_id.as_ref(),
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

/// Entries from the previous refresh, reused while their item's revision is
/// unchanged, so a streamed delta rebuilds only its own item.
#[derive(Default)]
pub struct EntryCache(HashMap<String, (u64, Rc<TimelineEntry>)>);

fn timeline_entries(projection: &ThreadProjection, cache: &mut EntryCache) -> Vec<Rc<TimelineEntry>> {
    let mut next = HashMap::with_capacity(cache.0.len());
    let entries = projection
        .visible_turn_items
        .iter()
        .filter_map(|row| {
            let item = &row.item;
            let entry = match cache.0.get(&item.id) {
                Some((revision, entry)) if *revision == item.revision && item.revision != 0 => entry.clone(),
                _ => {
                    let mut entry = timeline_entry(item, row.visibility == "inherited")?;
                    entry.source_thread_id = row.source_thread_id.clone().into();
                    entry.inherited = row.visibility == "inherited";
                    Rc::new(entry)
                }
            };
            next.insert(item.id.clone(), (item.revision, entry.clone()));
            Some(entry)
        })
        .collect();
    cache.0 = next;
    entries
}

fn timeline_entry(item: &Rc<TurnItem>, inherited: bool) -> Option<TimelineEntry> {
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
            let revertible = matches!(intent, Some("turn_start" | "queued_turn"));
            Kind::User { text: text("text"), steer, revertible, sent_by_t3code: crate::queue::sent_by_t3code(item) }
        }
        "assistant_message" => Kind::Assistant { text: text("text"), streaming: item.bool("streaming") },
        "proposed_plan" => Kind::Plan { markdown: text("markdown") },
        "secret_request" if item.str("secretStatus") == Some("pending") && !inherited => {
            Kind::Request(Rc::new(request_card(item)))
        }
        "link_request" if item.str("linkStatus") == Some("pending") && !inherited => {
            Kind::Request(Rc::new(request_card(item)))
        }
        "fork"
        | "handoff"
        | "run_interrupt_request"
        | "run_interrupt_result"
        | "secret_request"
        | "link_request"
        | "subagent" => Kind::Event {
            label: event_label(item, inherited).into(),
            item_type: item.item_type.clone().into(),
            subagent: (item.item_type == "subagent").then(|| Rc::new(subagent_card(item))),
            system: system_divider(item).map(Rc::new),
        },
        _ => match embed(item) {
            Some(embed) => Kind::Embed(Rc::new(embed)),
            None => Kind::Work(Rc::new(work_log::entry_from_item(item))),
        },
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
        status: item.status.clone().into(),
        source_thread_id: item.thread_id.clone().into(),
        inherited: false,
        node_id: item.node_id.clone().map(Into::into),
        completed_at: item.str("completedAt").map(|at| at.to_owned().into()),
        kind,
    })
}

fn event_label(item: &TurnItem, inherited: bool) -> String {
    let text = |key: &str| item.str(key).map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
    match item.item_type.as_str() {
        "fork" => "Forked thread".into(),
        "handoff" => "Switched provider".into(),
        // `secretRequestDisplay` for an answered request.
        "secret_request" => {
            let outcome = match item.str("secretStatus") {
                Some("saved") => "Saved securely and kept private",
                Some("declined") => "Declined",
                Some("cancelled") => "Request ended",
                Some("pending") if inherited => "Waiting for an answer in the original thread",
                _ => return "Asked for a secret".into(),
            };
            format!("{} · {outcome}", text("label").unwrap_or_else(|| "Secret".into()))
        }
        // `linkRequestDisplay`.
        "link_request" => match item.str("linkStatus") {
            Some("linked") => {
                let target = text("linkedLabel").unwrap_or_else(|| request_card(item).link_target());
                match item.str("linkedAccess") {
                    Some(access) => format!("Linked {target} · {}", access_label(access)),
                    None => format!("Linked {target}"),
                }
            }
            Some("declined") => "Declined".into(),
            Some("failed") => {
                format!("Could not link: {}", item.str("failure").unwrap_or("the other environment refused"))
            }
            Some("cancelled") => "Request ended".into(),
            _ if inherited => "Waiting for an answer in the original thread".into(),
            _ => "Asked to link an environment".into(),
        },
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
            if matches!(&entry.kind, Kind::Event { subagent: Some(card), .. } if card.live) {
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

/// `findRevertCheckpoint`: the ready checkpoint that ends run `turn_count`, or starts the thread.
fn revert_checkpoint(checkpoints: &[model::Checkpoint], turn_count: i64) -> Option<Checkpoint> {
    let checkpoint = checkpoints.iter().rev().find(|c| {
        c.status == "ready"
            && if turn_count == 0 {
                c.ordinal_within_scope == 0 && c.app_run_ordinal.is_none()
            } else {
                c.app_run_ordinal == Some(turn_count)
            }
    })?;
    Some(Checkpoint {
        scope_id: checkpoint.scope_id.clone().into(),
        checkpoint_id: checkpoint.id.clone().into(),
        turn_count,
    })
}

/// `deriveAvailableRevertTurnCounts`: the runs whose provider turn is on the live provider
/// thread, so the provider conversation can rewind with the files; 0 is the thread start.
fn available_revert_turn_counts(projection: &ThreadProjection) -> HashSet<i64> {
    let mut available = HashSet::new();
    let active = projection.thread.active_provider_thread_id.as_deref();
    let Some(provider_thread) = projection.provider_threads.iter().find(|t| Some(t.id.as_str()) == active) else {
        return available;
    };
    let instance = projection.thread.model_selection.as_ref().map(|m| m.instance_id.as_str());
    if provider_thread.provider_session_id.is_none() || provider_thread.provider_instance_id.as_deref() != instance {
        return available;
    }
    available.insert(0);
    for run in &projection.runs {
        let Some(attempt_id) = run.active_attempt_id.as_deref() else { continue };
        let latest = projection
            .provider_turns
            .iter()
            .filter(|turn| turn.run_attempt_id.as_deref() == Some(attempt_id))
            .max_by_key(|turn| turn.ordinal);
        let turn = latest.or_else(|| {
            let attempt = projection.attempts.iter().find(|a| a.id == attempt_id)?;
            let turn_id = attempt.provider_turn_id.as_deref()?;
            projection.provider_turns.iter().find(|turn| turn.id == turn_id)
        });
        if turn.and_then(|turn| turn.provider_thread_id.as_deref()) == Some(provider_thread.id.as_str()) {
            available.insert(run.ordinal);
        }
    }
    available
}

/// `deriveRevertTurnCountByUserMessageId`: each turn-starting message rewinds to the
/// checkpoint the previous visible run ended on.
fn revert_targets(
    projection: &ThreadProjection,
    entries: &[Rc<TimelineEntry>],
    unsettled: Option<&SharedString>,
) -> HashMap<SharedString, Checkpoint> {
    let available = available_revert_turn_counts(projection);
    let mut ready: HashMap<&str, i64> = HashMap::new();
    for checkpoint in projection.checkpoints.iter().filter(|c| c.status == "ready") {
        if let (Some(run), Some(count)) = (checkpoint.run_id.as_deref(), checkpoint.app_run_ordinal) {
            ready.insert(run, count);
        }
    }
    let start_ready = revert_checkpoint(&projection.checkpoints, 0).is_some();
    let mut target_by_run: HashMap<&SharedString, Option<i64>> = HashMap::new();
    let mut previous: Option<&SharedString> = None;
    let mut targets = HashMap::new();
    for entry in entries.iter().filter(|entry| !entry.inherited) {
        let run = match &entry.kind {
            Kind::User { .. } | Kind::Assistant { .. } => entry.run_id.as_ref(),
            Kind::Work(work) if work.item_type == "notification" => entry.run_id.as_ref(),
            _ => None,
        };
        let Some(run) = run else { continue };
        if !target_by_run.contains_key(run) {
            let target = match previous {
                None => start_ready.then_some(0),
                Some(previous) => ready.get(previous.as_ref()).copied(),
            };
            target_by_run.insert(run, target);
            previous = Some(run);
        }
        let Kind::User { revertible: true, .. } = entry.kind else { continue };
        if Some(run) == unsettled {
            continue;
        }
        let checkpoint = target_by_run[run]
            .filter(|target| available.contains(target))
            .and_then(|target| revert_checkpoint(&projection.checkpoints, target));
        if let Some(checkpoint) = checkpoint {
            targets.insert(entry.id.clone(), checkpoint);
        }
    }
    targets
}

/// `resolveAttempt`: the run attempt an item belongs to, through its node's ancestry.
fn entry_attempt<'a>(entry: &TimelineEntry, projection: &'a ThreadProjection) -> Option<&'a model::RunAttempt> {
    let run = entry.run_id.as_deref()?;
    let by_root =
        |node: &str| projection.attempts.iter().find(|a| a.root_node_id.as_deref() == Some(node) && a.run_id == run);
    let mut node_id = entry.node_id.as_deref();
    let mut visited = HashSet::new();
    while let Some(id) = node_id.filter(|id| visited.insert(*id)) {
        if let Some(attempt) = by_root(id) {
            return Some(attempt);
        }
        let node = projection.nodes.iter().find(|node| node.id == id)?;
        if let Some(attempt) = node.root_node_id.as_deref().and_then(by_root) {
            return Some(attempt);
        }
        node_id = node.parent_node_id.as_deref();
    }
    None
}

struct AttemptFold {
    key: SharedString,
    hidden: HashSet<usize>,
}

/// `deriveSupersededAttemptFolds`, by anchor index: a steer's restart folds the attempt
/// it cut off, unless that attempt had already finished its reply.
fn attempt_folds(
    entries: &[Rc<TimelineEntry>],
    attempts: &[Option<&model::RunAttempt>],
    failed_runs: &HashSet<SharedString>,
) -> HashMap<usize, AttemptFold> {
    let superseded = |index: usize| attempts[index].filter(|attempt| attempt.status == "superseded");
    let mut last_output: HashMap<&str, usize> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let Some(attempt) = superseded(index) else { continue };
        let output = match &entry.kind {
            Kind::Assistant { .. } => true,
            Kind::Work(work) => !matches!(work.item_type.as_ref(), "system_notice" | "notification"),
            _ => false,
        };
        if output {
            last_output.insert(attempt.id.as_str(), index);
        }
    }
    let complete: HashSet<&str> = last_output
        .iter()
        .filter(|(attempt, index)| {
            let entry = &entries[**index];
            let superseded_at = attempts[**index].and_then(|a| a.completed_at.as_deref());
            matches!(entry.kind, Kind::Assistant { streaming: false, .. })
                && entry.status == "completed"
                && matches!((entry.completed_at.as_deref(), superseded_at), (Some(done), Some(cut)) if done < cut)
                && !attempt.is_empty()
        })
        .map(|(attempt, _)| *attempt)
        .collect();
    let mut by_attempt: Vec<(&str, Vec<usize>)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let Some(attempt) = superseded(index) else { continue };
        let skip = complete.contains(attempt.id.as_str())
            || failed_runs.contains(attempt.run_id.as_str())
            || matches!(&entry.kind, Kind::User { .. } | Kind::Request(_))
            || matches!(&entry.kind, Kind::Event { item_type, .. } if item_type == "handoff" || is_resource_card(item_type))
            || matches!(&entry.kind, Kind::Event { subagent: Some(card), .. } if card.live)
            || entry.work().is_some_and(|work| work.item_type == "system_notice" || work.item_type == "thread_created");
        if skip {
            continue;
        }
        match by_attempt.iter_mut().find(|(id, _)| *id == attempt.id) {
            Some((_, indexes)) => indexes.push(index),
            None => by_attempt.push((attempt.id.as_str(), vec![index])),
        }
    }
    by_attempt
        .into_iter()
        .map(|(attempt, indexes)| {
            (indexes[0], AttemptFold { key: attempt_key(attempt), hidden: indexes.into_iter().collect() })
        })
        .collect()
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
    let reverts = revert_targets(projection, &entries, unsettled.as_ref());

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
    let attempts: Vec<Option<&model::RunAttempt>> =
        entries.iter().map(|entry| entry_attempt(entry, projection)).collect();
    let attempt_folds = attempt_folds(&entries, &attempts, &failed_runs);
    let attempt_hidden: HashSet<usize> = attempt_folds
        .values()
        .filter(|fold| !expanded.contains(&fold.key))
        .flat_map(|fold| fold.hidden.iter().copied())
        .collect();
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
        if let Some(fold) = attempt_folds.get(&index) {
            rows.push(Row::AttemptFold { expanded: expanded.contains(&fold.key), key: fold.key.clone() });
        }
        if attempt_hidden.contains(&index) {
            index += 1;
            continue;
        }
        match &entry.kind {
            Kind::Work(work) if entry.is_compaction() => rows.push(Row::Divider {
                id: entry.id.clone(),
                label: work.label.clone(),
                failed: work.status == Some(Status::Failed),
                failure: work
                    .detail
                    .as_ref()
                    .filter(|_| work.status == Some(Status::Failed))
                    .map(|summary| summary.trim().to_owned().into())
                    .filter(|summary: &SharedString| !summary.is_empty()),
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
                // A pending approval is drawn once, with its buttons, by the composer.
                if is_pending(work) {
                    index += 1;
                    continue;
                }
                if standalone(work) || work.run_id.as_ref().is_some_and(|run| itemized.contains(run)) {
                    rows.push(single_row(work, work.label.to_string(), expanded));
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
                        || attempt_folds.contains_key(&cursor)
                        || attempt_hidden.contains(&cursor)
                        || attempts[cursor].map(|a| &a.id) != attempts[index].map(|a| &a.id)
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
                        rows.push(single_row(single, label, expanded));
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
            Kind::Event { subagent: Some(card), .. } => push_subagent(&mut rows, card.clone(), expanded),
            Kind::Event { system: Some(divider), .. } => rows.push(Row::System(divider.clone())),
            Kind::Embed(embed) => rows.push(Row::Embed(embed.clone())),
            Kind::Request(card) => rows.push(Row::Request(card.clone())),
            Kind::Event { label, .. } => rows.push(Row::Event { id: entry.id.clone(), label: label.clone() }),
            Kind::User { text, steer, sent_by_t3code, .. } => rows.push(Row::User {
                id: entry.id.clone(),
                text: text.clone(),
                steer: *steer,
                grouped_with_previous: false,
                grouped_with_next: false,
                continues_steer: false,
                created_at: entry.created_at.clone(),
                revert: reverts.get(&entry.id).cloned(),
                sent_by_t3code: *sent_by_t3code,
            }),
            Kind::Assistant { text, streaming } => rows.push(Row::Assistant {
                id: entry.id.clone(),
                text: text.clone(),
                streaming: *streaming,
                terminal: terminal.contains(&index) && !is_active_response(entry.run_id.as_ref()),
                updated_at: entry.end.clone(),
                status: entry.status.clone(),
                // `canForkProjectedAssistantItem`.
                fork: entry
                    .run_id
                    .clone()
                    .filter(|_| entry.status == "completed")
                    .map(|run| (entry.source_thread_id.clone(), run)),
                run_id: entry.run_id.clone(),
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

    let mut rows = attach_trailing_meta(rows);
    let flags: Vec<(bool, bool, bool)> = rows
        .iter()
        .map(|row| (row.is_work(), row.is_own_user_message(), matches!(row, Row::User { steer: Some(_), .. })))
        .collect();
    for (index, row) in rows.iter_mut().enumerate() {
        let next = flags.get(index + 1).copied().unwrap_or_default();
        let previous = index.checked_sub(1).and_then(|i| flags.get(i)).copied().unwrap_or_default();
        match row {
            Row::Work(work) => work.continues = next.0,
            Row::User { steer, grouped_with_previous, grouped_with_next, continues_steer, sent_by_t3code, .. } => {
                *grouped_with_previous = !*sent_by_t3code && previous.1;
                *grouped_with_next = !*sent_by_t3code && next.1;
                *continues_steer = previous.2 && steer.is_some();
            }
            _ => {}
        }
    }
    rows
}

/// `attachTrailingToolGroupsToAssistant`: when tool groups of the same run follow a finished
/// reply, its actions move under the last of them.
fn attach_trailing_meta(rows: Vec<Row>) -> Vec<Row> {
    let failed_or_tool = |e: &Entry| e.is_tool_like() || (e.item_type == "error" && e.status == Some(Status::Failed));
    let mut meta_after: HashMap<usize, usize> = HashMap::new();
    for (index, row) in rows.iter().enumerate() {
        let Row::Assistant { terminal: true, run_id: Some(run), .. } = row else { continue };
        let mut last = None;
        let mut trailing = false;
        for (candidate_index, candidate) in rows.iter().enumerate().skip(index + 1) {
            let Row::Work(work) = candidate else {
                if matches!(candidate, Row::User { .. } | Row::Assistant { .. }) {
                    break;
                }
                continue;
            };
            match &work.kind {
                WorkRowKind::Toggle { run_id: Some(toggle_run), expanded: false, .. } if toggle_run == run => {
                    trailing = true;
                    last = Some(candidate_index);
                }
                WorkRowKind::Toggle { run_id: Some(toggle_run), expanded: true, .. }
                    if toggle_run == run && trailing =>
                {
                    last = Some(candidate_index)
                }
                WorkRowKind::Single { entry, .. } if entry.run_id.as_ref() == Some(run) => {
                    trailing |= failed_or_tool(entry);
                    if trailing {
                        last = Some(candidate_index);
                    }
                }
                WorkRowKind::Details { entries }
                    if trailing && entries.iter().any(|(e, _)| e.run_id.as_ref() == Some(run)) =>
                {
                    last = Some(candidate_index)
                }
                _ => {}
            }
        }
        if let Some(last) = last {
            meta_after.insert(last, index);
        }
    }
    if meta_after.is_empty() {
        return rows;
    }
    let moved: HashSet<usize> = meta_after.values().copied().collect();
    let mut out = Vec::with_capacity(rows.len() + meta_after.len());
    for (index, mut row) in rows.iter().cloned().enumerate() {
        if let (true, Row::Assistant { terminal, .. }) = (moved.contains(&index), &mut row) {
            *terminal = false;
        }
        out.push(row);
        if let Some(Row::Assistant { id, text, updated_at, status, fork, .. }) =
            meta_after.get(&index).map(|i| &rows[*i])
        {
            out.push(Row::AssistantMeta {
                id: id.clone(),
                text: text.clone(),
                updated_at: updated_at.clone(),
                status: status.clone(),
                fork: fork.clone(),
            });
        }
    }
    out
}

fn single_row(entry: &Rc<Entry>, label: String, expanded: &HashSet<SharedString>) -> Row {
    Row::Work(WorkRow {
        id: entry.id.clone(),
        kind: WorkRowKind::Single {
            entry: entry.clone(),
            label: work_log::collapse_whitespace(&label).into(),
            expanded: expanded.contains(&entry_key(&entry.id)),
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
            run_id: entries.first().and_then(|e| e.run_id.clone()),
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
            created_at: SharedString::default(),
            revert: None,
            sent_by_t3code: false,
        }
    }

    fn projection_value(items: Vec<Value>, runs: Value) -> Value {
        let rows: Vec<Value> = items
            .into_iter()
            .map(|item| {
                let id = item["id"].clone();
                json!({"visibility": "visible", "sourceThreadId": "t", "sourceItemId": id, "item": item})
            })
            .collect();
        json!({"thread": {"id": "t", "title": "T"}, "runs": runs, "visibleTurnItems": rows})
    }

    fn projection(items: Vec<Value>, runs: Value) -> ThreadProjection {
        serde_json::from_value(projection_value(items, runs)).unwrap()
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
                Row::Subagent(card) => format!("subagent {}", card.title),
                Row::SubagentGroup { cards, .. } => format!("subagents {}", cards.len()),
                Row::AttemptFold { expanded, .. } => format!("attempt-fold {expanded}"),
                Row::Embed(embed) => format!("embed {}", embed.title),
                Row::Queued { message, .. } => format!("queued {}", message.text),
                Row::QueueToggle { total, .. } => format!("queue-toggle {total}"),
                Row::Now(_) => "now".into(),
                Row::AssistantMeta { .. } => "meta".into(),
                Row::Workflow { row, .. } => format!("workflow {}", row.opening.message_id),
                Row::WorktreeSetup { snapshot, .. } => format!("setup {}", snapshot.phase),
                Row::System(divider) => format!("system {}", divider.label),
                Row::Request(card) => format!("request {}", card.item_id),
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
        let item = |value: Value| Rc::new(serde_json::from_value::<TurnItem>(value).unwrap());
        let queued = item(
            json!({"id": "q", "type": "user_message", "inputIntent": "queued_turn", "status": "pending", "text": "later"}),
        );
        let checkpoint = item(json!({"id": "c", "type": "checkpoint", "status": "completed"}));
        let started = item(
            json!({"id": "s", "type": "user_message", "inputIntent": "queued_turn", "status": "completed", "text": "now"}),
        );
        assert!(timeline_entry(&queued, false).is_none());
        assert!(timeline_entry(&checkpoint, false).is_none());
        assert!(matches!(timeline_entry(&started, false).unwrap().kind, Kind::User { steer: None, .. }));
    }

    #[test]
    fn answered_secret_requests_name_their_outcome() {
        let label = |status: &str| {
            let raw = json!({"id": "s", "type": "secret_request", "label": "GitHub token", "secretStatus": status});
            event_label(&serde_json::from_value::<TurnItem>(raw).unwrap(), true)
        };
        assert_eq!(label("saved"), "GitHub token · Saved securely and kept private");
        assert_eq!(label("declined"), "GitHub token · Declined");
        assert_eq!(label("cancelled"), "GitHub token · Request ended");
        assert_eq!(label("pending"), "GitHub token · Waiting for an answer in the original thread");
    }

    #[test]
    fn a_replys_actions_follow_the_tools_that_ran_after_it() {
        let mut items = turn("r1");
        items.push(command("r1-late", "r1", "2026-11-04T10:01:06.000Z"));
        let p = projection(items, json!([{"id": "r1", "status": "completed", "ordinal": 1}]));
        let mut expanded = HashSet::new();
        expanded.insert(fold_key("r1"));
        let described = describe(&rows(&p, &expanded, &mut EntryCache::default()));
        let reply = described.iter().position(|row| row == "assistant done").expect("the reply");
        assert!(described[reply + 1].starts_with("work"), "{described:?}");
        assert_eq!(described.get(reply + 2).map(String::as_str), Some("meta"), "{described:?}");
    }

    #[test]
    fn subagents_from_one_provider_turn_group() {
        let agent = |id: &str, turn: &str, status: &str| {
            item(
                id,
                "subagent",
                "r1",
                "2026-01-01T00:00:01Z",
                json!({"title": id, "status": status, "providerTurnId": turn}),
            )
        };
        let mut items = turn("r1");
        items.insert(1, agent("a", "p1", "running"));
        items.insert(2, agent("b", "p1", "completed"));
        items.insert(3, agent("c", "p2", "failed"));
        let p = projection(items, json!([{"id": "r1", "status": "running", "ordinal": 1}]));
        let rows = rows(&p, &HashSet::new(), &mut EntryCache::default());
        let Some(Row::SubagentGroup { cards, expanded: false, .. }) =
            rows.iter().find(|r| matches!(r, Row::SubagentGroup { .. }))
        else {
            panic!("expected a group: {:?}", describe(&rows))
        };
        assert_eq!(cards.len(), 2);
        assert_eq!(subagent_group_status(cards), ("1 working · 1 done".to_owned(), true, false));
        assert!(rows.iter().any(|r| matches!(r, Row::Subagent(card) if card.title == "c")));
    }

    #[test]
    fn stops_and_forks_read_as_dividers() {
        let item = |raw: Value| serde_json::from_value::<TurnItem>(raw).unwrap();
        let stopped =
            system_divider(&item(json!({"id": "s", "type": "run_interrupt_result", "message": "Stopped by user"})));
        assert_eq!(
            stopped.as_ref().map(|d| (d.label.as_ref(), d.detail.as_deref())),
            Some(("Run interrupted", Some("Stopped by user")))
        );
        let forked =
            system_divider(&item(json!({"id": "f", "type": "fork", "source": {"type": "run", "threadId": "src"}})))
                .unwrap();
        assert_eq!(forked.label, "Forked from conversation");
        assert_eq!(forked.action, Some(("Open source conversation".into(), "src".into())));
        let fork = system_divider(&item(
            json!({"id": "f", "type": "fork", "source": {"type": "thread"}, "targetThreadId": "t"}),
        ))
        .unwrap();
        assert_eq!(fork.action, Some(("Open fork".into(), "t".into())));
    }

    #[test]
    fn link_requests_card_while_pending_and_name_their_outcome() {
        let item = |extra: Value| {
            let mut raw = json!({"id": "l", "type": "link_request", "threadId": "t", "reason": "Run tests there"});
            raw.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            Rc::new(serde_json::from_value::<TurnItem>(raw).unwrap())
        };
        let pending = item(json!({"linkStatus": "pending", "hint": "my box"}));
        let Kind::Request(card) = timeline_entry(&pending, false).unwrap().kind else { panic!("expected a card") };
        assert!(card.picks_machine());
        assert_eq!(card.link_target(), "a machine");
        assert!(matches!(timeline_entry(&pending, true).unwrap().kind, Kind::Event { .. }));
        let linked = item(json!({"linkStatus": "linked", "url": "https://box.ts.net", "linkedAccess": "full-access"}));
        assert_eq!(event_label(&linked, false), "Linked https://box.ts.net · Full access");
        let failed = item(json!({"linkStatus": "failed"}));
        assert_eq!(event_label(&failed, false), "Could not link: the other environment refused");
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
        let mut state = crate::model::ThreadState::default();
        let mut items = turn("r1")[..3].to_vec();
        items[2]["status"] = json!("running");
        let runs = json!([{"id": "r1", "status": "running"}]);
        state
            .apply(&json!({"kind": "snapshot", "snapshotSequence": 1, "projection": projection_value(items.clone(), runs)}))
            .unwrap();
        let mut live = |state: &crate::model::ThreadState| {
            describe(&rows(state.projection.as_ref().unwrap(), &HashSet::new(), &mut cache)).last().cloned().unwrap()
        };
        assert_eq!(live(&state), "live Running ls");
        // Same `updatedAt` and status, new input: the revision still changes.
        items[2]["input"] = json!("cargo test");
        state
            .apply(
                &json!({"kind": "event", "sequence": 2, "event": {"type": "turn-item.updated", "payload": items[2]}}),
            )
            .unwrap();
        assert_eq!(live(&state), "live Running cargo");
    }

    #[test]
    fn a_steer_folds_the_attempt_it_cut_off() {
        let at = |s: u32| format!("2026-11-04T10:00:{s:02}.000Z");
        let node = |id: &str, extra: Value| {
            let mut value = item(id, "command_execution", "r1", &at(2), json!({"input": "ls", "nodeId": id}));
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            value
        };
        let mut items = vec![
            item("u", "user_message", "r1", &at(0), json!({"text": "go", "inputIntent": "turn_start"})),
            node("n1", json!({})),
            node("n2", json!({})),
            item("s", "user_message", "r1", &at(3), json!({"text": "stop", "inputIntent": "steer", "nodeId": "n3"})),
            node("n4", json!({})),
        ];
        items.push(item("a", "assistant_message", "r1", &at(5), json!({"text": "done", "nodeId": "n4"})));
        let mut value = projection_value(items, json!([{"id": "r1", "status": "running"}]));
        value["attempts"] = json!([
            {"id": "a1", "runId": "r1", "rootNodeId": "root1", "status": "superseded", "completedAt": at(3)},
            {"id": "a2", "runId": "r1", "rootNodeId": "root2", "status": "running"},
        ]);
        value["nodes"] = json!([
            {"id": "n1", "rootNodeId": "root1"}, {"id": "n2", "parentNodeId": "n1"},
            {"id": "n3", "rootNodeId": "root2"}, {"id": "n4", "rootNodeId": "root2"},
        ]);
        let p: ThreadProjection = serde_json::from_value(value).unwrap();
        let shown = describe(&rows(&p, &HashSet::new(), &mut EntryCache::default()));
        assert_eq!(
            shown,
            ["user go", "working", "attempt-fold false", "user stop", "work ls", "assistant done", "thinking"]
        );
        let open = describe(&rows(&p, &HashSet::from([attempt_key("a1")]), &mut EntryCache::default()));
        assert_eq!(open[2..4], ["attempt-fold true", "toggle Ran 2 commands"]);
    }

    #[test]
    fn rendered_pages_and_mcp_apps_become_embeds() {
        let tool = |tool: &str, output: Value| {
            let value =
                json!({"id": "t", "type": "dynamic_tool", "status": "completed", "toolName": tool, "output": output});
            embed(&serde_json::from_value(value).unwrap())
        };
        let page =
            tool("mcp__t3-code__html_render", json!({"htmlRender": {"attachmentId": "a1", "title": "Q3: plan?"}}));
        assert_eq!(
            page.as_ref().map(|p| (p.attachment_id.as_ref(), p.file_name())),
            Some(("a1", "Q3 plan.html".into()))
        );
        let app =
            tool("mcp__maps__show", json!({"t3McpApp": {"attachmentId": "a2", "server": "maps", "tool": "show"}}));
        assert_eq!(app.map(|a| a.title), Some("maps app".into()));
        // An app reference from another tool is ignored, so tools cannot pose as an app.
        assert_eq!(
            tool("mcp__other__x", json!({"t3McpApp": {"attachmentId": "a2", "server": "maps", "tool": "show"}})),
            None
        );
        assert_eq!(
            tool("mcp__t3-code__html_render", json!({"isError": true, "htmlRender": {"attachmentId": "a1"}})),
            None
        );
    }

    #[test]
    fn subagent_cards_read_like_the_web() {
        assert_eq!(subagent_title("Subagent: /root/work/code_reviewer"), "Code Reviewer");
        assert_eq!(subagent_title("subagent: Fix the build"), "Fix the build");
        assert_eq!(
            plain_detail("See [the PR](https://x/1) and `cargo test`\n- done"),
            "See the PR and cargo test done"
        );
        let card = |extra: Value| {
            let mut value = json!({"id": "s", "type": "subagent", "status": "completed", "title": "Review"});
            value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            subagent_card(&serde_json::from_value(value).unwrap())
        };
        let done = card(json!({"result": "All good", "progress": "Reading", "childThreadId": "child"}));
        assert_eq!(
            (done.detail.as_deref(), done.status_beside_detail, done.child_thread_id.as_deref()),
            (Some("All good"), false, Some("child"))
        );
        let running = card(json!({"status": "running", "result": "Old", "progress": "Reading"}));
        assert_eq!((running.detail.as_deref(), running.status_label, running.live), (Some("Reading"), "Running", true));
        let generic = card(json!({"status": "failed", "result": "Child task ended with status failed"}));
        assert_eq!((generic.detail, generic.failed), (None, true));
        assert_eq!([5_000, 185_000, 3_725_000].map(agent_elapsed), ["5s", "3m 05s", "1h 02m"]);
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

        // A message T3 Code sent is labeled and stands apart from the user's own.
        let sent = json!({"text": "d", "inputIntent": "turn_start", "createdBy": "agent", "creationSource": "server"});
        let mut items = vec![item("u4", "user_message", "r2", "2026-11-04T10:00:03.000Z", sent)];
        items.insert(0, item("u0", "user_message", "r2", "2026-11-04T10:00:00.500Z", json!({"text": "z"})));
        let p = projection(items, json!([]));
        let rows = super::rows(&p, &HashSet::new(), &mut EntryCache::default());
        assert!(matches!(rows[0], Row::User { grouped_with_next: false, sent_by_t3code: false, .. }));
        assert!(matches!(rows[1], Row::User { grouped_with_previous: false, sent_by_t3code: true, .. }));
    }

    #[test]
    fn a_pending_approval_is_left_to_the_composer() {
        let items = vec![
            item(
                "u",
                "user_message",
                "r1",
                "2026-11-04T10:00:00.000Z",
                json!({"text": "go", "inputIntent": "turn_start"}),
            ),
            item(
                "ap",
                "approval_request",
                "r1",
                "2026-11-04T10:00:01.000Z",
                json!({"requestId": "q", "requestKind": "command", "prompt": "rm -rf build"}),
            ),
        ];
        let mut p = projection(items, json!([{"id": "r1", "status": "waiting"}]));
        let request = |status: &str| {
            serde_json::from_value(
                json!({"id": "q", "kind": "command", "status": status, "responseCapability": {"type": "live"}}),
            )
            .unwrap()
        };
        p.runtime_requests = vec![request("pending")];
        let pending = describe(&rows(&p, &HashSet::new(), &mut EntryCache::default()));
        p.runtime_requests = vec![request("resolved")];
        let resolved = describe(&rows(&p, &HashSet::new(), &mut EntryCache::default()));
        assert_eq!(pending, ["user go", "working"]);
        assert_eq!(resolved, ["user go", "working", "live rm -rf build"]);
    }
}
