//! Work-log entries and their labels. Ports
//! `packages/client-runtime/src/work-log/presentation.ts`, `t3ToolSummary.ts`,
//! `packages/shared/src/t3McpToolPresentation.ts` and the label helpers of
//! `apps/web/src/components/chat/MessagesTimeline.logic.ts`.
//!
//! Everything a row needs is derived once per item into an [`Entry`], so rows
//! compare cheaply and rendering reads no JSON.

use std::rc::Rc;

use gpui_kit::SharedString;
use serde_json::Value;

use crate::model::TurnItem;

/// The item an entry came from, for the details an expanded row shows.
/// Equal by identity: an update replaces the item.
#[derive(Clone, Debug)]
pub struct ItemRef(pub Rc<TurnItem>);

impl PartialEq for ItemRef {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Thinking,
    Tool,
    Info,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    InProgress,
    Completed,
    Idle,
    Failed,
    Declined,
    Stopped,
}

/// `ToolGroupAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    LinkPr,
    UnlinkPr,
    ListPrs,
    WatchPr,
    UnwatchPr,
    Read,
    Edit,
    Command,
    ThreadCreate,
    Browser,
    Device,
    CodeSearch,
    Search,
    Other,
    Update,
}

/// `ToolGroupSummaryKind`: picks a collapsed group's icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryKind {
    PullRequest,
    Action(Action),
    DynamicTool,
    Reasoning,
    AgentTool,
    ToneTool,
    Mixed,
}

/// Lucide icons the work log uses (`WorkEntryIconName`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkIcon {
    Terminal,
    SquarePen,
    Eye,
    Search,
    Globe,
    T3,
    Browser,
    Device,
    PullRequest,
    Wrench,
    Hammer,
    Brain,
    Bot,
    Zap,
    MessageCircle,
    ListTodo,
    Code,
    CircleAlert,
    Check,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
    Unfinished,
}

/// What `sourceActivityKind` the web tags an entry with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Warning,
    Error,
    Compaction,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolSource {
    pub key: SharedString,
    pub name: SharedString,
    pub integration: bool,
}

/// A call to one of T3 Code's own MCP tools, reduced to what its label and
/// summary read.
#[derive(Clone, Debug, PartialEq)]
pub struct T3Call {
    pub tool: &'static T3Tool,
    pub summary: &'static str,
    pub outcome: Outcome,
    pub result_failed: bool,
    pub thread: Option<SharedString>,
    pub message: Option<SharedString>,
    pub project: Option<SharedString>,
    /// The id the summary counts, e.g. `taskId` for `delegate`.
    pub entity: Option<SharedString>,
    /// `Some` when every thread the call reported creating had an id.
    pub created_threads: Option<Vec<SharedString>>,
    pub pr_count: Option<i64>,
    pub pr_target: Option<SharedString>,
    pub attachments: Option<usize>,
    /// Per-guide outcome of `write_pull_request_guides`: `true` when started.
    pub guides: Vec<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: SharedString,
    pub item_type: SharedString,
    pub run_id: Option<SharedString>,
    pub created_at: SharedString,
    pub tone: Tone,
    pub status: Option<Status>,
    pub label: SharedString,
    pub detail: Option<SharedString>,
    pub command: Option<SharedString>,
    pub changed_files: Vec<SharedString>,
    pub tool_title: Option<SharedString>,
    pub tool_name: Option<SharedString>,
    pub source: Option<ToolSource>,
    pub activity: Option<Activity>,
    pub t3: Option<T3Call>,
    pub exit_code: Option<i64>,
    pub output_failed: bool,
    pub viewed_image: bool,
    pub question_answered: bool,
    pub provider_retry: bool,
    pub approval_request_id: Option<SharedString>,
    /// `source.kind` of a notification, or `"failed"` when it failed.
    pub notification: Option<SharedString>,
    /// `dynamicToolTitle` (JS REPL title, `Skill: name`).
    pub dynamic_title: Option<SharedString>,
    pub search_label: Option<SharedString>,
    pub paths: Vec<SharedString>,
    pub action: Action,
    pub item: ItemRef,
}

// ---------------------------------------------------------------------------
// Entries

/// `projectedWorkEntry` in `apps/web/src/session-logic.ts`, plus the
/// notification entry built in `deriveTimelineEntriesFromVisibleTurnItems`.
pub fn entry_from_item(item: &Rc<TurnItem>) -> Entry {
    let raw = &item.raw;
    let text = |key: &str| str_at(raw, key).map(SharedString::from);
    let title = str_at(raw, "title").map(str::trim).filter(|t| !t.is_empty()).map(SharedString::from);
    let item_type = item.item_type.as_str();
    let tone = match item_type {
        "reasoning" => Tone::Thinking,
        "command_execution" | "file_change" | "file_search" | "web_search" | "dynamic_tool" | "subagent"
        | "thread_created" | "user_input_request" | "approval_request" => Tone::Tool,
        _ => Tone::Info,
    };
    let status = match item.status.as_str() {
        "pending" | "running" | "waiting" => Status::InProgress,
        "completed" => Status::Completed,
        "idle" => Status::Idle,
        "failed" => Status::Failed,
        _ => Status::Stopped,
    };
    let source = raw.get("toolSource").and_then(tool_source);
    let mut entry = Entry {
        id: item.id.clone().into(),
        item_type: item.item_type.clone().into(),
        run_id: item.run_id.clone().map(Into::into),
        created_at: str_at(raw, "startedAt").or(str_at(raw, "updatedAt")).unwrap_or_default().to_owned().into(),
        tone,
        status: Some(status),
        label: SharedString::default(),
        detail: None,
        command: None,
        changed_files: Vec::new(),
        tool_title: None,
        tool_name: None,
        source,
        activity: None,
        t3: None,
        exit_code: raw.get("exitCode").and_then(Value::as_i64),
        output_failed: raw.get("outputIndicatesFailure").and_then(Value::as_bool) == Some(true),
        viewed_image: str_at(raw, "viewedImagePath").is_some_and(|p| !p.trim().is_empty() && !p.contains('\n')),
        question_answered: false,
        provider_retry: false,
        approval_request_id: None,
        notification: None,
        dynamic_title: None,
        search_label: None,
        paths: Vec::new(),
        action: Action::Other,
        item: ItemRef(item.clone()),
    };
    let fallback = |title: &Option<SharedString>, text: &str| title.clone().unwrap_or_else(|| text.to_owned().into());
    match item_type {
        "notification" => {
            entry.tone = Tone::Info;
            entry.status = None;
            entry.label = text("summary").unwrap_or_default();
            let failed = str_at(raw, "outcome") == Some("failed");
            entry.notification = if failed {
                Some("failed".into())
            } else {
                raw.pointer("/source/kind").and_then(Value::as_str).map(|k| k.to_owned().into())
            };
        }
        "thread_created" => entry.label = "Created thread".into(),
        "compaction" => {
            entry.label = compaction_label(item).into();
            entry.activity = Some(Activity::Compaction);
            entry.detail = text("summary");
        }
        "reasoning" => {
            entry.label = fallback(&title, "Thinking");
            entry.detail = text("text").filter(|t| !t.is_empty());
        }
        "command_execution" => {
            entry.label = fallback(&title, "Ran command");
            entry.command = text("input");
            entry.tool_title = Some(fallback(&title, "Command"));
        }
        "file_change" => {
            let changes = raw.get("changes").and_then(Value::as_array);
            let file_name = str_at(raw, "fileName").unwrap_or_default();
            entry.label = title.clone().unwrap_or_else(|| match changes {
                Some(changes) if changes.len() > 1 => format!("Changed {} files", changes.len()).into(),
                _ => format!("Changed {file_name}").into(),
            });
            entry.changed_files = match changes {
                Some(changes) => {
                    changes.iter().filter_map(|c| str_at(c, "path")).map(|p| p.to_owned().into()).collect()
                }
                None => vec![file_name.to_owned().into()],
            };
            entry.tool_title = Some(fallback(&title, "File change"));
        }
        "file_search" => {
            entry.search_label = format_search_label(raw).map(Into::into);
            entry.label = title.clone().or(entry.search_label.clone()).unwrap_or_else(|| "Searched files".into());
            entry.detail = text("pattern").filter(|p| !p.is_empty());
            entry.tool_title = Some(fallback(&title, "File search"));
        }
        "web_search" => {
            entry.label = fallback(&title, "Searched the web");
            let patterns: Vec<&str> =
                raw.get("patterns").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
            entry.detail = (!patterns.is_empty()).then(|| patterns.join(", ").into());
            entry.tool_title = Some(fallback(&title, "Web search"));
            entry.search_label = format_search_label(raw).map(Into::into);
        }
        "system_notice" => {
            entry.label = text("message").unwrap_or_default();
            entry.activity = Some(Activity::Warning);
        }
        "error" => {
            let (label, detail) = provider_error(item);
            entry.label = label.into();
            entry.detail = Some(detail.into());
            entry.provider_retry = raw.get("retry").is_some_and(|r| !r.is_null());
            let usage_limit = raw.pointer("/failure/class").and_then(Value::as_str) == Some("usage_limit");
            entry.activity = if usage_limit && item.status != "completed" {
                Some(Activity::Warning)
            } else if !entry.provider_retry {
                Some(Activity::Error)
            } else {
                None
            };
        }
        "dynamic_tool" => {
            let tool_name = str_at(raw, "toolName");
            let input = raw.get("input").unwrap_or(&Value::Null);
            let classified = classify(Some("dynamic_tool_call"), tool_name, None);
            entry.paths = collect_paths(input);
            entry.search_label = format_search_label(input).or_else(|| format_search_label(raw)).map(Into::into);
            let label: SharedString = match classified {
                Classified::Read => format_read_label(entry.paths.first().map_or("", |p| p), 0).into(),
                Classified::Search => {
                    entry.search_label.clone().unwrap_or_else(|| tool_name.unwrap_or("Tool call").to_owned().into())
                }
                _ => tool_name.unwrap_or("Tool call").to_owned().into(),
            };
            entry.label = title.clone().unwrap_or(label);
            entry.tool_title = Some(title.clone().unwrap_or_else(|| tool_name.unwrap_or("Tool").to_owned().into()));
            entry.tool_name = tool_name.map(|n| n.to_owned().into());
            entry.dynamic_title = dynamic_tool_title(tool_name, input, title.as_deref()).map(Into::into);
        }
        "todo_list" => {
            let steps: Vec<&Value> = raw.get("steps").and_then(Value::as_array).into_iter().flatten().collect();
            let step_status = |s: &Value| str_at(s, "status").unwrap_or_default().to_owned();
            let done = steps.iter().filter(|s| step_status(s) == "completed").count();
            let current = steps
                .iter()
                .find(|s| step_status(s) == "running")
                .or_else(|| steps.iter().find(|s| step_status(s) == "pending"))
                .and_then(|s| str_at(s, "text"));
            entry.label = match current {
                Some(current) => format!("Tasks {done}/{} · {current}", steps.len()),
                None => format!("Tasks {done}/{}", steps.len()),
            }
            .into();
        }
        "approval_request" => {
            entry.label = fallback(&title, "Approval requested");
            entry.detail = text("prompt").or(text("requestKind"));
            entry.approval_request_id = text("requestId");
        }
        "user_input_request" => {
            entry.question_answered = raw.get("questionAnswer").is_some_and(|a| !a.is_null());
            let label = if entry.question_answered { "Answered questions" } else { "Input requested" };
            entry.label = fallback(&title, label);
        }
        other => entry.label = title.unwrap_or_else(|| other.replace('_', " ").into()),
    }
    entry.t3 = t3_call(&entry, raw);
    entry.action = compute_action(&entry);
    entry
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn trimmed<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    str_at(value, key).map(str::trim).filter(|s| !s.is_empty())
}

fn tool_source(value: &Value) -> Option<ToolSource> {
    let key = trimmed(value, "key")?;
    let name = trimmed(value, "name")?;
    let kind = str_at(value, "kind")?;
    matches!(kind, "browser" | "computer" | "integration").then(|| ToolSource {
        key: key.to_owned().into(),
        name: name.to_owned().into(),
        integration: kind == "integration",
    })
}

/// `contextCompactionLabel` in presentation.ts.
fn compaction_label(item: &TurnItem) -> String {
    match item.status.as_str() {
        "failed" => return "Context compaction failed".into(),
        "cancelled" | "interrupted" => return "Context compaction stopped".into(),
        "pending" | "running" | "waiting" => return "Compacting context".into(),
        _ => {}
    }
    let count = |key: &str| item.raw.get(key).and_then(Value::as_f64);
    match (count("beforeTokenCount"), count("afterTokenCount")) {
        (Some(before), Some(after)) => {
            format!("Context compacted {} → {} tokens", format_tokens(before), format_tokens(after))
        }
        _ => "Context compacted".into(),
    }
}

/// `formatTokens` in `packages/shared/src/usageFormat.ts`: 120K, 45.6K, 1.50K.
fn format_tokens(count: f64) -> String {
    let trim = |value: f64| {
        let digits = if value.abs() >= 100. {
            0
        } else if value.abs() >= 10. {
            1
        } else {
            2
        };
        let text = format!("{value:.digits$}");
        match text.split_once('.') {
            Some((whole, fraction)) if fraction.bytes().all(|b| b == b'0') => whole.to_owned(),
            _ => text,
        }
    };
    for (scale, suffix) in [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")] {
        if count.abs() >= scale {
            return format!("{}{suffix}", trim(count / scale));
        }
    }
    format!("{count:.0}")
}

/// `providerErrorPresentation` in session-logic.ts.
fn provider_error(item: &TurnItem) -> (String, String) {
    let raw = &item.raw;
    let message = raw.pointer("/failure/message").and_then(Value::as_str).unwrap_or_default().to_owned();
    let usage_limit = raw.pointer("/failure/class").and_then(Value::as_str) == Some("usage_limit");
    let Some(retry) = raw.get("retry").filter(|r| !r.is_null()) else {
        let label = if usage_limit {
            "Usage limit reached".to_owned()
        } else {
            trimmed(raw, "title").unwrap_or("Provider error").to_owned()
        };
        return (label, message);
    };
    let attempt = retry.get("attempt").and_then(Value::as_i64).unwrap_or_default();
    let progress = match retry.get("maxAttempts").and_then(Value::as_i64) {
        Some(max) => format!("{attempt}/{max}"),
        None => attempt.to_string(),
    };
    let label = match item.status.as_str() {
        "running" => format!("Retrying provider ({progress})"),
        "completed" => format!("Provider recovered ({progress} retries)"),
        "failed" => {
            format!("{} after {progress} retries", if usage_limit { "Usage limit reached" } else { "Provider error" })
        }
        _ => format!("Provider retry stopped ({progress})"),
    };
    let delay = retry.get("retryDelayMs").and_then(Value::as_f64).filter(|d| *d > 0. && item.status == "running");
    let delay = match delay {
        Some(ms) if ms < 1000. => format!(" Retrying in {ms:.0}ms."),
        Some(ms) => format!(" Retrying in {}s.", format!("{:.1}", ms / 1000.).trim_end_matches(".0")),
        None => String::new(),
    };
    (label, format!("{message}{delay}"))
}

// ---------------------------------------------------------------------------
// Tool classification (`packages/shared/src/toolActivity.ts`)

#[derive(Clone, Copy, PartialEq, Eq)]
enum Classified {
    Command,
    Read,
    FileChange,
    Search,
    Other,
}

/// `classifyToolActivity`, for the inputs the work log passes it.
fn classify(item_type: Option<&str>, tool_name: Option<&str>, kind: Option<&str>) -> Classified {
    match item_type {
        Some("command_execution") => return Classified::Command,
        Some("image_view") => return Classified::Read,
        Some("file_change") => return Classified::FileChange,
        Some("web_search") => return Classified::Search,
        _ => {}
    }
    let kind = kind.map(str::to_lowercase);
    let token = tool_name
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.contains("__") && !name.contains(['.', '/']))
        .map(|name| name.chars().filter(|c| !matches!(c, '_' | '-') && !c.is_whitespace()).collect::<String>())
        .map(|name| name.to_lowercase());
    let (kind, token) = (kind.as_deref(), token.as_deref());
    if kind == Some("execute") {
        Classified::Command
    } else if matches!(kind, Some("edit" | "move" | "delete" | "write")) {
        Classified::FileChange
    } else if kind == Some("search") || matches!(token, Some("find" | "grep" | "glob" | "rg" | "ls")) {
        Classified::Search
    } else if kind == Some("read") {
        Classified::Read
    } else if matches!(token, Some("terminal" | "bash" | "shell")) {
        Classified::Command
    } else if matches!(token, Some("read" | "readfile")) {
        Classified::Read
    } else {
        Classified::Other
    }
}

/// `collectToolFilePaths`: up to 8 paths under the usual keys, 4 levels deep.
fn collect_paths(value: &Value) -> Vec<SharedString> {
    fn visit(value: &Value, paths: &mut Vec<SharedString>, depth: usize) {
        if depth > 4 || paths.len() >= 8 {
            return;
        }
        if let Some(array) = value.as_array() {
            for entry in array {
                visit(entry, paths, depth + 1);
            }
            return;
        }
        if !value.is_object() {
            return;
        }
        for key in ["path", "filePath", "file_path", "relativePath", "filename", "fileName", "newPath", "oldPath"] {
            if let Some(path) = trimmed(value, key)
                && paths.len() < 8
                && !paths.iter().any(|p| p == path)
            {
                paths.push(path.to_owned().into());
            }
        }
        for key in ["locations", "item", "input", "result", "rawInput", "data", "changes"] {
            if let Some(nested) = value.get(key) {
                visit(nested, paths, depth + 1);
            }
        }
    }
    let mut paths = Vec::new();
    visit(value, &mut paths, 0);
    paths
}

/// `formatSearchToolLabel`.
fn format_search_label(data: &Value) -> Option<String> {
    let has_keys = |v: &&Value| v.as_object().is_some_and(|o| !o.is_empty());
    let input = [data.get("rawInput"), data.get("input"), data.pointer("/item/input")]
        .into_iter()
        .flatten()
        .find(has_keys)
        .unwrap_or(data);
    let first = |keys: &[&str]| keys.iter().find_map(|key| trimmed(input, key));
    let query = first(&["pattern", "query", "searchTerm", "regex", "grep", "needle"]);
    let glob = first(&["glob", "globPattern", "glob_pattern", "include", "filePattern", "file_pattern"]);
    let target = first(&["path", "target_directory", "targetDirectory", "directory", "cwd", "root"])
        .and_then(|t| t.split(['\\', '/']).rfind(|part| !part.is_empty() && *part != "."));
    Some(match (query, glob, target) {
        (Some(query), _, Some(target)) => format!("Searched {query} in {target}"),
        (None, Some(glob), Some(target)) => format!("Searched files {glob} in {target}"),
        (None, Some(glob), None) => format!("Searched files {glob}"),
        (Some(query), _, None) => format!("Searched {query}"),
        (None, None, Some(target)) => format!("Searched in {target}"),
        (None, None, None) => return None,
    })
}

/// `formatReadToolLabel`.
fn format_read_label(path: &str, extra: usize) -> String {
    let suffix = if extra > 0 { format!(" +{extra} more") } else { String::new() };
    match path.trim() {
        "" => format!("Read file{suffix}"),
        path => format!("Read {}{suffix}", path.replace('\\', "/")),
    }
}

/// `dynamicToolTitle`.
fn dynamic_tool_title(tool_name: Option<&str>, input: &Value, retained: Option<&str>) -> Option<String> {
    if matches!(tool_name, Some("js" | "cua_repl.js" | "node_repl.js")) {
        let name = tool_name.unwrap_or_default();
        let meaningful = retained.filter(|t| !t.eq_ignore_ascii_case("js") && !t.eq_ignore_ascii_case(name));
        let computer = name == "cua_repl.js"
            || str_at(input, "code").is_some_and(|code| code.contains("sky.") || code.contains("@oai/sky"));
        return Some(
            trimmed(input, "title")
                .or(meaningful)
                .unwrap_or(if computer { "Computer use" } else { "Run JavaScript" })
                .to_owned(),
        );
    }
    (tool_name == Some("Skill")).then(|| trimmed(input, "skill").map(|skill| format!("Skill: {skill}"))).flatten()
}

// ---------------------------------------------------------------------------
// T3 Code MCP tools

#[derive(Debug, PartialEq, Eq)]
pub struct T3Tool {
    pub name: &'static str,
    /// Action, running, completed, detail.
    pub labels: [&'static str; 4],
    pub summary: &'static str,
    pub icon: T3Icon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum T3Icon {
    T3,
    Browser,
    Device,
    PullRequest,
}

macro_rules! t3_tools {
    ($($name:literal => [$a:literal, $r:literal, $c:literal, $d:literal], $summary:literal $(, $icon:ident)?;)*) => {
        const T3_TOOLS: &[T3Tool] = &[
            $(T3Tool { name: $name, labels: [$a, $r, $c, $d], summary: $summary, icon: t3_tools!(@icon $($icon)?) },)*
        ];
    };
    (@icon) => { T3Icon::T3 };
    (@icon $icon:ident) => { T3Icon::$icon };
}

t3_tools! {
    "link_pull_request" => ["Link", "Linking", "Linked", "a pull request"], "link-pr", PullRequest;
    "link_pull_requests" => ["Link", "Linking", "Linked", "pull requests"], "link-pr", PullRequest;
    "unlink_pull_request" => ["Unlink", "Unlinking", "Unlinked", "a pull request"], "unlink-pr", PullRequest;
    "unlink_pull_requests" => ["Unlink", "Unlinking", "Unlinked", "pull requests by state"], "unlink-pr", PullRequest;
    "list_thread_pull_requests" => ["Check", "Checking", "Checked", "linked pull requests"], "list-prs", PullRequest;
    "watch_pull_request" => ["Watch", "Watching", "Watching", "a pull request"], "watch-pr", PullRequest;
    "unwatch_pull_request" => ["Stop watching", "Unwatching", "Stopped watching", "a pull request"], "unwatch-pr", PullRequest;
    "write_pull_request_guides" => ["Write", "Writing", "Started writing", "review guides"], "write-pr-guides", PullRequest;
    "read_pull_request_guide" => ["Read", "Reading", "Read", "a review guide"], "read-pr-guide", PullRequest;
    "orchestrator_capabilities" => ["Get", "Getting", "Got", "orchestration capabilities"], "capabilities";
    "delegate_task" => ["Delegate", "Delegating", "Delegated", "a child task"], "delegate";
    "task_status" => ["Get", "Getting", "Got", "delegated task status"], "task-status";
    "task_cancel" => ["Cancel", "Canceling", "Requested cancellation of", "a delegated task"], "task-cancel";
    "schedule_task" => ["Schedule", "Scheduling", "Scheduled", "a task"], "schedule-create";
    "list_scheduled_tasks" => ["List", "Listing", "Listed", "scheduled tasks"], "schedule-list";
    "update_scheduled_task" => ["Update", "Updating", "Updated", "a scheduled task"], "schedule-update";
    "delete_scheduled_task" => ["Delete", "Deleting", "Requested deletion of", "a scheduled task"], "schedule-delete";
    "request_secret" => ["Ask for", "Asking for", "Asked for", "a secret"], "secret-request";
    "create_threads" => ["Create", "Creating", "Created", "T3 threads"], "thread-create";
    "t3_thread_start" => ["Start", "Starting", "Started", "a T3 thread"], "thread-create";
    "t3_thread_list" => ["List", "Listing", "Listed", "T3 threads"], "thread-list";
    "t3_thread_read" => ["Read", "Reading", "Read", "a T3 thread"], "thread-read";
    "t3_thread_send" => ["Send", "Sending", "Sent", "to a T3 thread"], "thread-send";
    "t3_thread_wait" => ["Wait", "Waiting", "Waited", "for a T3 thread"], "thread-wait";
    "t3_workflow_stop" => ["Stop", "Stopping", "Requested a stop for", "a workflow"], "workflow-stop";
    "t3_background_wait" => ["Update", "Updating", "Updated", "background waits"], "background-wait";
    "t3_thread_interrupt" => ["Interrupt", "Interrupting", "Requested an interrupt of", "a T3 thread"], "thread-interrupt";
    "t3_worktree_handoff" => ["Hand off", "Handing off", "Handed off", "thread to a git worktree"], "worktree-handoff";
    "t3_worktree_status" => ["Get", "Getting", "Got", "thread worktree status"], "worktree-status";
    "preview_status" => ["Get", "Getting", "Got", "preview browser status"], "browser", Browser;
    "preview_open" => ["Open", "Opening", "Opened", "a page in the preview browser"], "browser", Browser;
    "preview_navigate" => ["Navigate", "Navigating", "Navigated", "the preview browser"], "browser", Browser;
    "preview_dialog" => ["Respond", "Responding", "Responded", "to a preview browser dialog"], "browser", Browser;
    "preview_snapshot" => ["Take a snapshot of", "Taking a snapshot of", "Took a snapshot of", "the preview page"], "browser", Browser;
    "preview_act" => ["Use", "Using", "Used", "the preview browser"], "browser", Browser;
    "preview_click" => ["Click", "Clicking", "Clicked", "in the preview browser"], "browser", Browser;
    "preview_press" => ["Press", "Pressing", "Pressed", "a key in the preview browser"], "browser", Browser;
    "preview_type" => ["Type", "Typing", "Typed", "in the preview browser"], "browser", Browser;
    "preview_hover" => ["Hover", "Hovering", "Hovered", "in the preview browser"], "browser", Browser;
    "preview_select" => ["Choose", "Choosing", "Chose", "an option in the preview browser"], "browser", Browser;
    "preview_drag" => ["Drag", "Dragging", "Dragged", "in the preview browser"], "browser", Browser;
    "preview_upload" => ["Upload", "Uploading", "Uploaded", "files to the preview browser"], "browser", Browser;
    "preview_scroll" => ["Scroll", "Scrolling", "Scrolled", "the preview browser"], "browser", Browser;
    "preview_resize" => ["Resize", "Resizing", "Resized", "the preview browser"], "browser", Browser;
    "preview_evaluate" => ["Evaluate", "Evaluating", "Evaluated", "script in the preview browser"], "browser", Browser;
    "preview_wait_for" => ["Wait", "Waiting", "Waited", "for the preview page"], "browser", Browser;
    "preview_set_appearance" => ["Set", "Setting", "Set", "preview browser appearance"], "browser", Browser;
    "preview_recording_start" => ["Start", "Starting", "Started", "recording the preview browser"], "browser", Browser;
    "preview_recording_stop" => ["Stop", "Stopping", "Stopped", "recording the preview browser"], "browser", Browser;
    "device_list" => ["List", "Listing", "Listed", "simulators and emulators"], "device", Device;
    "device_open" => ["Open", "Opening", "Opened", "a device in the Device panel"], "device", Device;
    "device_screenshot" => ["Take a screenshot of", "Taking a screenshot of", "Took a screenshot of", "the device"], "device", Device;
    "device_close" => ["Close", "Closing", "Closed", "a device"], "device", Device;
    "run_scheduled_task_now" => ["Run", "Running", "Requested a run of", "a scheduled task"], "schedule-run";
    "t3_queue" => ["Manage", "Managing", "Managed", "queued messages"], "queue-edit";
    "t3_pending_request" => ["Handle", "Handling", "Handled", "pending questions"], "question-respond";
    "t3_queue_list" => ["List", "Listing", "Listed", "queued messages"], "queue-list";
    "t3_queue_read" => ["Read", "Reading", "Read", "a queued message"], "queue-read";
    "t3_queue_edit" => ["Edit", "Editing", "Edited", "a queued message"], "queue-edit";
    "t3_queue_cancel" => ["Cancel", "Canceling", "Requested cancellation of", "a queued run"], "queue-cancel";
    "t3_queue_reorder" => ["Reorder", "Reordering", "Reordered", "a queued run"], "queue-reorder";
    "t3_queue_promote_to_steer" => ["Steer with", "Steering with", "Requested steering with", "a queued message"], "queue-steer";
    "t3_pending_request_list" => ["List", "Listing", "Listed", "pending questions"], "question-list";
    "t3_pending_request_read" => ["Read", "Reading", "Read", "pending questions"], "question-read";
    "t3_pending_request_respond" => ["Answer", "Answering", "Answered", "pending questions"], "question-respond";
    "t3_thread_configuration" => ["Read", "Reading", "Read", "thread configuration"], "thread-configuration";
    "t3_thread_configure" => ["Use", "Using", "Used", "thread model settings"], "thread-configure";
    "t3_thread_transfer" => ["Use", "Using", "Used", "thread transfers"], "thread-fork";
    "t3_thread_fork" => ["Fork", "Forking", "Requested a fork of", "this thread"], "thread-fork";
    "t3_thread_merge_back" => ["Merge", "Merging", "Requested a merge of", "thread context"], "thread-merge";
    "t3_thread_search" => ["Search", "Searching", "Searched", "thread content"], "thread-search";
    "t3_thread_transfers" => ["Read", "Reading", "Read", "thread transfers"], "thread-transfers";
    "t3_thread_organize" => ["Organize", "Organizing", "Organized", "a thread"], "thread-organize";
    "t3_thread_update" => ["Update", "Updating", "Updated", "T3 thread metadata"], "thread-update";
    "t3_worktree_list" => ["List", "Listing", "Listed", "workspace branches"], "worktree-list";
    "t3_preview_list" => ["List", "Listing", "Listed", "preview tabs"], "browser", Browser;
    "t3_preview_close" => ["Close", "Closing", "Closed", "a preview tab"], "browser", Browser;
    "t3_environment_read" => ["Read", "Reading", "Read", "environment preferences"], "environment-read";
    "t3_environment_links" => ["List", "Listing", "Listed", "linked environments"], "environment-links";
    "t3_environment_link" => ["Ask to link", "Asking to link", "Asked to link", "an environment"], "environment-link";
    "t3_environment_unlink" => ["Forget", "Forgetting", "Forgot", "a linked environment"], "environment-unlink";
    "t3_environment_preferences_update" => ["Update", "Updating", "Updated", "environment preferences"], "environment-update";
    "t3_thread_launch" => ["Launch", "Launching", "Launched", "a project thread"], "thread-create";
    "t3_project_list" => ["List", "Listing", "Listed", "projects"], "project-list";
    "t3_project_read" => ["Read", "Reading", "Read", "a project"], "project-read";
    "t3_project_create" => ["Register", "Registering", "Registered", "a project"], "project-create";
    "t3_project_update" => ["Update", "Updating", "Updated", "a project"], "project-update";
    "t3_project_delete" => ["Delete", "Deleting", "Deleted", "a project"], "project-delete";
    "t3_project_clone" => ["Clone", "Cloning", "Cloned", "a repository"], "project-clone";
    "t3_attachment_prepare_upload" => ["Prepare", "Preparing", "Prepared", "an attachment upload"], "attachment-prepare";
    "t3_attachment_discard" => ["Discard", "Discarding", "Discarded", "a pending attachment"], "attachment-discard";
    "t3_thread_send_attachments" => ["Send", "Sending", "Sent", "attachments"], "attachment-send";
    "html_preview" => ["Preview", "Previewing", "Previewed", "an HTML page"], "html-preview";
    "html_render" => ["Render", "Rendering", "Rendered", "an HTML page"], "html-render";
}

fn t3_tool(name: &str) -> Option<&'static T3Tool> {
    T3_TOOLS.iter().find(|tool| tool.name == name)
}

const T3_SERVERS: [&str; 5] = ["t3-code", "t3_code", "t3code", "t3-code-direct", "t3_code_direct"];

/// `parseT3McpToolName`: the tool behind a provider's label.
fn t3_tool_name(label: &str) -> Option<&'static T3Tool> {
    let label = normalize_compact_label(label);
    if let Some(rest) = strip_prefix_ci(&label, "mcp__") {
        let (server, tool) = rest.split_once("__")?;
        return T3_SERVERS.contains(&server.to_lowercase().as_str()).then(|| t3_tool(tool)).flatten();
    }
    for server in ["t3-code-direct", "t3_code_direct", "t3-code", "t3_code", "t3code"] {
        if let Some(rest) = strip_prefix_ci(&label, server)
            && let Some(tool) = rest.trim_start().strip_prefix(['.', ':', '/', '·'])
        {
            return t3_tool(tool.trim_start());
        }
    }
    if let Some(tool) = t3_tool(&label) {
        return Some(tool);
    }
    // OpenCode 2 thread prefixes carry underscores: take the longest known suffix.
    strip_prefix_ci(&label, "t3-code-")?;
    T3_TOOLS
        .iter()
        .filter(|tool| label.strip_suffix(tool.name).is_some_and(|head| head.ends_with('_')))
        .max_by_key(|tool| tool.name.len())
}

/// ASCII-case-insensitive `strip_prefix` that never slices inside a character.
fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

/// ASCII-case-insensitive `strip_suffix` that never slices inside a character.
fn strip_suffix_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let start = text.len().checked_sub(suffix.len())?;
    text.get(start..)?.eq_ignore_ascii_case(suffix).then(|| &text[..start])
}

/// `resolveT3McpToolDefinition`: action tools resolve to the tool their action replaced.
fn t3_definition(tool: &'static T3Tool, input: &Value) -> &'static T3Tool {
    let name = tool.name;
    let action = str_at(input, "action");
    let replaced = action.and_then(|action| match name {
        "preview_act" => Some(format!("preview_{action}")),
        "t3_queue" => Some(format!("t3_queue_{action}")),
        "t3_pending_request" => Some(format!("t3_pending_request_{action}")),
        "t3_thread_transfer" => match action {
            "fork" => Some("t3_thread_fork".into()),
            "merge_back" => Some("t3_thread_merge_back".into()),
            "status" => Some("t3_thread_transfers".into()),
            _ => None,
        },
        _ => None,
    });
    replaced.and_then(|name| t3_tool(&name)).unwrap_or(tool)
}

fn t3_call(entry: &Entry, raw: &Value) -> Option<T3Call> {
    // `workEntryToolName` and `workEntryToolInput` / `workEntryToolOutput`.
    let dynamic = entry.item_type == "dynamic_tool";
    let server_tool = match (str_at(raw, "server"), str_at(raw, "tool")) {
        (Some(server), Some(tool)) => Some(format!("{server}.{tool}")),
        _ => None,
    };
    let title = entry.tool_title.as_deref().filter(|title| t3_tool_name(title).is_some());
    let source = match (entry.tool_name.as_deref().filter(|_| dynamic), server_tool, str_at(raw, "toolName")) {
        (Some(name), ..) => name.to_owned(),
        (None, Some(server_tool), _) => server_tool,
        (None, None, Some(name)) => name.to_owned(),
        (None, None, None) => title.unwrap_or(&entry.label).to_owned(),
    };
    let parsed = t3_tool_name(&source)?;
    let name = parsed.name;
    let input_value =
        if dynamic { raw.get("input") } else { raw.get("arguments").or(raw.get("input")).or(raw.get("rawInput")) };
    let output_value = if dynamic {
        raw.get("output")
    } else {
        raw.get("output").or(raw.get("result")).or(raw.get("rawOutput")).or(raw.get("content"))
    };
    let tool = t3_definition(parsed, input_value.unwrap_or(&Value::Null));
    let output = output_value.map(read_result).unwrap_or_default();
    let input = input_value.and_then(|value| {
        let data = read_result(value).data?;
        // Historical Cursor items keep the MCP args envelope.
        Some(if data.get("toolName").is_some_and(Value::is_string) { data.get("args").cloned()? } else { data })
    });
    let input = input.unwrap_or(Value::Null);
    let output_data = output.data.unwrap_or(Value::Null);
    let id = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(|s| SharedString::from(s.to_owned()))
    };
    let summary = match (name, &input) {
        ("t3_thread_configure", input) if input.is_object() => {
            if input.get("modelSelection").is_some() {
                "thread-configure"
            } else {
                "thread-configuration"
            }
        }
        ("schedule_task", input) if input.is_object() => {
            if input.get("scheduledTaskId").is_some() {
                "schedule-update"
            } else {
                "schedule-create"
            }
        }
        _ => tool.summary,
    };
    let entity_key = match summary {
        "delegate" | "task-cancel" => "taskId",
        "schedule-create" | "schedule-update" | "schedule-delete" => "scheduledTaskId",
        "queue-read" | "queue-edit" | "queue-cancel" | "queue-reorder" | "queue-steer" => "queuedRunId",
        "question-read" | "question-respond" => "requestId",
        "worktree-handoff" => "worktreePath",
        "project-clone" => "cwd",
        "environment-unlink" => "environmentId",
        "attachment-prepare" | "attachment-discard" => "attachmentId",
        "workflow-stop" => "subagentId",
        _ => "",
    };
    let created_threads = {
        let threads: Vec<&Value> = match output_data.get("threads").and_then(Value::as_array) {
            Some(threads) => threads.iter().collect(),
            None => vec![&output_data],
        };
        let mut ids = Vec::new();
        let known = threads.iter().all(|thread| {
            if str_at(thread, "status") == Some("rolled_back") {
                return true;
            }
            match id(thread, "threadId") {
                Some(thread_id) => {
                    ids.push(thread_id);
                    true
                }
                None => false,
            }
        });
        known.then_some(ids)
    };
    let count_key = if summary == "unlink-pr" { "unlinkedCount" } else { "linkedCount" };
    let pr_count = output_data
        .get(count_key)
        .and_then(Value::as_i64)
        .or_else(|| (input.get("url").is_some() || input.get("number").is_some()).then_some(1));
    let pr_target = matches!(summary, "link-pr" | "unlink-pr" | "watch-pr" | "unwatch-pr")
        .then(|| {
            let url = str_at(&input, "url");
            let (short, number) = match url.and_then(parse_change_request_url) {
                Some((short, number)) => (short, Some(number)),
                None => ("PR", input.get("number").and_then(Value::as_i64)),
            };
            number.filter(|n| *n > 0).map(|n| SharedString::from(format!("{short} #{n}")))
        })
        .flatten();
    let outcome = match entry.status {
        _ if entry.tone == Tone::Error => Outcome::Failed,
        Some(Status::Failed | Status::Declined) => Outcome::Failed,
        Some(Status::Completed) => Outcome::Completed,
        _ => Outcome::Unfinished,
    };
    Some(T3Call {
        tool,
        summary,
        outcome: if output.failed { Outcome::Failed } else { outcome },
        result_failed: output.failed,
        thread: id(&output_data, "threadId")
            .or_else(|| output_data.get("thread").and_then(|t| id(t, "threadId")))
            .or_else(|| id(&input, "threadId")),
        message: id(&output_data, "messageId"),
        project: id(&output_data, "id").or_else(|| id(&output_data, "projectId")).or_else(|| id(&input, "projectId")),
        entity: (!entity_key.is_empty())
            .then(|| id(&output_data, entity_key).or_else(|| id(&input, entity_key)))
            .flatten(),
        created_threads,
        pr_count,
        pr_target,
        attachments: input.get("attachments").and_then(Value::as_array).map(Vec::len),
        guides: output_data
            .get("guides")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|guide| str_at(guide, "outcome") != Some("failed"))
            .collect(),
    })
}

/// `sourceControlClients.parseChangeRequestUrl`, for GitHub and GitLab.
fn parse_change_request_url(url: &str) -> Option<(&'static str, i64)> {
    let path = url.split_once("://").map_or(url, |(_, rest)| rest);
    let number =
        |marker: &str| path.split_once(marker).and_then(|(_, rest)| rest.split(['/', '?', '#']).next()?.parse().ok());
    if let Some(n) = number("/-/merge_requests/") {
        return Some(("MR", n));
    }
    number("/pull/").map(|n| ("PR", n))
}

#[derive(Default)]
struct ToolResult {
    data: Option<Value>,
    failed: bool,
}

/// `readResult` in t3ToolSummary.ts: unwraps MCP content blocks and JSON text.
fn read_result(value: &Value) -> ToolResult {
    fn read(value: &Value, depth: usize) -> ToolResult {
        if depth > 4 {
            return ToolResult::default();
        }
        match value {
            Value::String(text) => {
                serde_json::from_str::<Value>(text).map(|parsed| read(&parsed, depth + 1)).unwrap_or_default()
            }
            Value::Array(blocks) => {
                let mut result = ToolResult::default();
                for block in blocks {
                    let text = block.get("text").or_else(|| block.pointer("/content/text"));
                    let text = text.map(|t| t.get("text").unwrap_or(t));
                    let inner = text.map(|t| read(t, depth + 1)).unwrap_or_default();
                    result.data = result.data.or(inner.data);
                    result.failed |= inner.failed;
                }
                result
            }
            Value::Object(record) => {
                let failed = record.get("isError") == Some(&Value::Bool(true))
                    || record.get("is_error") == Some(&Value::Bool(true))
                    || record
                        .get("_tag")
                        .and_then(Value::as_str)
                        .is_some_and(|tag| tag.ends_with("Error") || tag.ends_with("Failure"))
                    || record.get("error").is_some_and(|e| !e.is_null());
                match record.get("structuredContent").or(record.get("content")) {
                    Some(content) => {
                        let inner = read(content, depth + 1);
                        ToolResult { data: inner.data, failed: failed || inner.failed }
                    }
                    None => ToolResult { data: Some(value.clone()), failed },
                }
            }
            _ => ToolResult::default(),
        }
    }
    read(value, 0)
}

/// `resolveT3McpToolPresentation` in presentation.ts: the label for one call.
fn t3_display_name(call: &T3Call, status: Option<Status>) -> String {
    let [action, running, completed, detail] = call.tool.labels;
    let status = if call.result_failed { Some(Status::Failed) } else { status };
    let verb = match status {
        Some(Status::InProgress | Status::Idle) | None => running.to_owned(),
        Some(Status::Completed) => completed.to_owned(),
        Some(Status::Failed) => format!("Failed to {}", action.to_lowercase()),
        Some(Status::Declined) => format!("Declined to {}", action.to_lowercase()),
        Some(Status::Stopped) => format!("Stopped {}", running.to_lowercase()),
    };
    format!("{verb} {}", call.pr_target.as_deref().unwrap_or(detail))
}

fn quantity(count: usize, noun: &str) -> String {
    quantity_plural(count, noun, &format!("{noun}s"))
}

fn quantity_plural(count: usize, noun: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { noun } else { plural })
}

/// Distinct known ids plus one per call without an id (`countEntities`).
fn count_entities<'a>(ids: impl Iterator<Item = Option<&'a SharedString>>) -> usize {
    let mut seen: Vec<&SharedString> = Vec::new();
    let mut unknown = 0;
    for id in ids {
        match id {
            Some(id) if !seen.contains(&id) => seen.push(id),
            Some(_) => {}
            None => unknown += 1,
        }
    }
    seen.len() + unknown
}

/// `summarizeT3ToolCalls`.
fn summarize_t3_calls(summary: &str, calls: &[&T3Call]) -> (String, usize) {
    let completed: Vec<&T3Call> = calls.iter().copied().filter(|c| c.outcome == Outcome::Completed).collect();
    let mut failed = calls.iter().filter(|c| c.outcome == Outcome::Failed).count();
    let selected: &[&T3Call] = if completed.is_empty() { calls } else { &completed };
    let times = quantity(selected.len(), "time");
    let phrase = |past: &str, infinitive: &str, object: String| {
        if completed.is_empty() { format!("Tried to {infinitive} {object}") } else { format!("{past} {object}") }
    };
    let entities = |noun: &str| quantity(count_entities(selected.iter().map(|c| c.entity.as_ref())), noun);
    let threads_known = selected.iter().all(|c| c.thread.is_some());
    let distinct_threads = {
        let mut ids: Vec<&SharedString> = selected.iter().filter_map(|c| c.thread.as_ref()).collect();
        ids.sort();
        ids.dedup();
        ids.len()
    };
    let label = match summary {
        "thread-send" => {
            let messages = count_entities(selected.iter().map(|c| c.message.as_ref()));
            let object = if !threads_known {
                quantity(messages, "message")
            } else if messages == distinct_threads && messages > 1 {
                format!("messages to {}", quantity(distinct_threads, "thread"))
            } else {
                format!("{} to {}", quantity(messages, "message"), quantity(distinct_threads, "thread"))
            };
            phrase("Sent", "send", object)
        }
        "thread-create" => {
            let created = (!completed.is_empty())
                .then(|| completed.iter().map(|c| c.created_threads.as_ref()).collect::<Option<Vec<_>>>())
                .flatten();
            match created {
                Some(lists) => {
                    let mut ids: Vec<&SharedString> = lists.into_iter().flatten().collect();
                    ids.sort();
                    ids.dedup();
                    format!("Created {}", quantity(ids.len(), "thread"))
                }
                None => format!("Requested thread creation {times}"),
            }
        }
        "delegate" => phrase("Delegated", "delegate", entities("task")),
        "thread-read" | "thread-wait" => {
            let targets = if threads_known { quantity(distinct_threads, "thread") } else { format!("threads {times}") };
            if summary == "thread-read" {
                phrase("Read", "read", targets)
            } else {
                phrase("Waited on", "wait on", targets)
            }
        }
        "thread-interrupt" => phrase(
            "Requested interrupts for",
            "interrupt",
            quantity(count_entities(selected.iter().map(|c| c.thread.as_ref())), "thread"),
        ),
        "thread-update" => {
            phrase("Updated", "update", quantity(count_entities(selected.iter().map(|c| c.thread.as_ref())), "thread"))
        }
        "workflow-stop" => phrase("Requested stops for", "stop", entities("workflow")),
        "task-cancel" => phrase("Requested cancellation of", "cancel", entities("task")),
        "schedule-create" => phrase("Scheduled", "schedule", entities("task")),
        "schedule-update" => phrase("Updated", "update", entities("scheduled task")),
        "schedule-delete" => phrase("Requested deletion of", "delete", entities("scheduled task")),
        "schedule-run" => phrase("Requested", "request", quantity(selected.len(), "scheduled task run")),
        "thread-fork" => phrase("Requested", "request", quantity(selected.len(), "thread fork")),
        "thread-merge" => phrase("Requested", "request", quantity(selected.len(), "context merge")),
        "queue-read" => phrase("Read", "read", entities("queued message")),
        "queue-edit" => phrase("Edited", "edit", entities("queued message")),
        "queue-cancel" => phrase("Requested cancellation of", "cancel", entities("queued run")),
        "queue-reorder" => phrase("Reordered", "reorder", entities("queued run")),
        "queue-steer" => phrase("Requested steering with", "steer with", entities("queued message")),
        "question-read" => phrase("Read", "read", entities("pending question request")),
        "question-respond" => phrase("Answered", "answer", entities("pending question request")),
        "secret-request" => phrase("Asked for", "ask for", quantity(selected.len(), "secret")),
        "worktree-handoff" => phrase("Handed off to", "hand off to", entities("worktree")),
        "project-read" | "project-create" | "project-update" | "project-delete" => {
            let projects = quantity(count_entities(selected.iter().map(|c| c.project.as_ref())), "project");
            match summary {
                "project-read" => phrase("Read", "read", projects),
                "project-create" => phrase("Registered", "register", projects),
                "project-update" => phrase("Updated", "update", projects),
                _ => phrase("Deleted", "delete", projects),
            }
        }
        "project-clone" => phrase(
            "Cloned",
            "clone",
            quantity_plural(count_entities(selected.iter().map(|c| c.entity.as_ref())), "repository", "repositories"),
        ),
        "environment-link" => phrase("Asked to link", "ask to link", quantity(selected.len(), "environment")),
        "environment-unlink" => phrase("Forgot", "forget", entities("linked environment")),
        "attachment-prepare" => phrase("Prepared", "prepare", entities("attachment upload")),
        "attachment-discard" => phrase("Discarded", "discard", entities("pending attachment")),
        "attachment-send" => {
            let targets =
                if threads_known { format!(" to {}", quantity(distinct_threads, "thread")) } else { String::new() };
            let counts_known = selected.iter().all(|c| c.attachments.is_some_and(|n| n > 0));
            let object = if counts_known {
                let total = selected.iter().filter_map(|c| c.attachments).sum();
                format!("{}{targets}", quantity(total, "attachment"))
            } else {
                format!("attachments{targets} {times}")
            };
            phrase("Sent", "send", object)
        }
        "link-pr" | "unlink-pr" => {
            let counts: Option<Vec<i64>> = selected.iter().map(|c| c.pr_count).collect();
            let object = match counts {
                Some(counts) => {
                    let total = counts.iter().fold(0i64, |total, count| total.saturating_add(*count));
                    quantity(usize::try_from(total).unwrap_or(0), "pull request")
                }
                None => "pull requests".to_owned(),
            };
            if summary == "link-pr" { phrase("Linked", "link", object) } else { phrase("Unlinked", "unlink", object) }
        }
        "watch-pr" => phrase("Watching", "watch", quantity(selected.len(), "pull request")),
        "unwatch-pr" => phrase("Stopped watching", "stop watching", quantity(selected.len(), "pull request")),
        "list-prs" => phrase(
            "Checked",
            "check",
            if selected.len() == 1 { "linked pull requests".into() } else { format!("linked pull requests {times}") },
        ),
        "write-pr-guides" => {
            let outcomes: Vec<bool> = selected.iter().flat_map(|c| c.guides.iter().copied()).collect();
            let started = outcomes.iter().filter(|s| **s).count();
            let guide_failures = outcomes.len() - started;
            failed += guide_failures;
            if outcomes.is_empty() {
                let object =
                    if selected.len() == 1 { "review guides".into() } else { format!("review guides {times}") };
                phrase("Started writing", "start writing", object)
            } else if started == 0 {
                format!("Could not start {}", quantity(guide_failures, "review guide"))
            } else {
                let suffix = if guide_failures > 0 { format!(", {guide_failures} failed") } else { String::new() };
                phrase("Started", "start", format!("{}{suffix}", quantity(started, "review guide")))
            }
        }
        "read-pr-guide" => phrase("Read", "read", quantity(selected.len(), "review guide")),
        "html-preview" => phrase("Previewed", "preview", quantity(selected.len(), "HTML page")),
        "html-render" => phrase("Rendered", "render", quantity(selected.len(), "HTML page")),
        other => {
            let (past, infinitive, object) = match other {
                "thread-list" => ("Listed", "list", "threads"),
                "task-status" => ("Checked", "check", "task status"),
                "background-wait" => ("Updated", "update", "background waits"),
                "schedule-list" => ("Listed", "list", "scheduled tasks"),
                "thread-configuration" => ("Checked", "check", "thread configuration"),
                "thread-configure" => ("Set", "set", "thread model"),
                "thread-search" => ("Searched", "search", "threads"),
                "thread-transfers" => ("Checked", "check", "thread transfers"),
                "thread-organize" => ("Organized", "organize", "threads"),
                "queue-list" => ("Listed", "list", "queued messages"),
                "question-list" => ("Listed", "list", "pending questions"),
                "worktree-list" => ("Listed", "list", "workspace branches"),
                "worktree-status" => ("Checked", "check", "worktree status"),
                "project-list" => ("Listed", "list", "projects"),
                "environment-read" => ("Checked", "check", "environment preferences"),
                "environment-update" => ("Updated", "update", "environment preferences"),
                "environment-links" => ("Listed", "list", "linked environments"),
                "browser" => ("Used", "use", "browser"),
                "device" => ("Used", "use", "device controls"),
                _ => ("Checked", "check", "orchestration capabilities"),
            };
            phrase(past, infinitive, format!("{object} {times}"))
        }
    };
    (label, failed)
}

// ---------------------------------------------------------------------------
// Classification and failure

fn normalize_compact_label(value: &str) -> String {
    let trimmed = value.trim_end();
    for suffix in [" completed", " complete"] {
        if let Some(head) = strip_suffix_ci(trimmed, suffix) {
            return head.trim().to_owned();
        }
    }
    trimmed.trim().to_owned()
}

impl Entry {
    /// `workLogEntryIsToolLike`.
    pub fn is_tool_like(&self) -> bool {
        matches!(self.tone, Tone::Tool | Tone::Thinking | Tone::Error)
            || self.command.as_ref().is_some_and(|c| !c.trim().is_empty())
    }

    pub fn is_reasoning(&self) -> bool {
        self.item_type == "reasoning"
    }

    /// `workEntryDisplayIndicatesToolFailure`. The web also scans tool output
    /// text for failure phrases; the stream omits command output, so exit
    /// codes and `outputIndicatesFailure` stand in for it here.
    pub fn failed(&self) -> bool {
        if self.tone == Tone::Error || matches!(self.status, Some(Status::Failed | Status::Declined)) {
            return true;
        }
        if !self.is_tool_like() || self.is_reasoning() {
            return false;
        }
        if self.t3.as_ref().is_some_and(|t3| t3.result_failed) {
            return true;
        }
        if self.item_type == "command_execution" {
            return self.exit_code.map_or(self.output_failed, |code| code != 0);
        }
        false
    }

    /// `workEntryIndicatesToolSuccess`.
    pub fn succeeded(&self) -> bool {
        self.is_tool_like()
            && !self.failed()
            && (self.tone != Tone::Thinking || self.is_reasoning())
            && !matches!(
                self.status,
                Some(Status::Failed | Status::Declined | Status::InProgress | Status::Stopped | Status::Idle)
            )
    }

    /// `workEntryIsActiveTurnActivity`.
    pub fn is_active_activity(&self) -> bool {
        self.status == Some(Status::InProgress) || (self.status.is_none() && self.is_tool_like())
    }

    /// `workEntryIsVisibleInGroup`.
    pub fn is_visible_in_group(&self, expanded_active: bool) -> bool {
        if self.is_reasoning() {
            return self.detail.as_ref().is_some_and(|d| !d.trim().is_empty());
        }
        let neutral = self.is_tool_like() && !self.failed() && !self.succeeded();
        (expanded_active && self.status == Some(Status::InProgress)) || self.status == Some(Status::Stopped) || !neutral
    }

    fn t3_presentation(&self, status: Option<Status>) -> Option<String> {
        self.t3.as_ref().map(|t3| t3_display_name(t3, status))
    }

    fn t3_icon(&self) -> Option<T3Icon> {
        self.t3.as_ref().map(|t3| t3.tool.icon)
    }

    /// The per-entry icon (`workEntryIconName`).
    pub fn icon(&self) -> WorkIcon {
        if self.item_type == "notification" {
            return match self.notification.as_deref() {
                Some("failed") => WorkIcon::CircleAlert,
                Some("subagent" | "delegated_task") => WorkIcon::Bot,
                Some("command") => WorkIcon::Terminal,
                Some("monitor") => WorkIcon::Eye,
                Some("system") => WorkIcon::T3,
                _ => WorkIcon::Zap,
            };
        }
        match self.item_type.as_ref() {
            "user_input_request" | "approval_request" => return WorkIcon::MessageCircle,
            "todo_list" => return WorkIcon::ListTodo,
            _ => {}
        }
        if self.tool_name.as_deref().is_some_and(|n| matches!(n, "js" | "cua_repl.js" | "node_repl.js")) {
            return WorkIcon::Code;
        }
        if let Some(icon) = self.t3_icon() {
            return t3_work_icon(icon);
        }
        if self.action != Action::Other {
            return summary_icon(SummaryKind::Action(self.action));
        }
        match self.item_type.as_ref() {
            "dynamic_tool" => WorkIcon::Wrench,
            "subagent" => WorkIcon::Bot,
            _ => match self.tone {
                Tone::Error => WorkIcon::CircleAlert,
                Tone::Thinking => WorkIcon::Brain,
                Tone::Info => WorkIcon::Check,
                Tone::Tool => WorkIcon::Zap,
            },
        }
    }
}

fn t3_work_icon(icon: T3Icon) -> WorkIcon {
    match icon {
        T3Icon::T3 => WorkIcon::T3,
        T3Icon::Browser => WorkIcon::Browser,
        T3Icon::Device => WorkIcon::Device,
        T3Icon::PullRequest => WorkIcon::PullRequest,
    }
}

/// `toolGroupSummaryIconName`.
pub fn summary_icon(kind: SummaryKind) -> WorkIcon {
    match kind {
        SummaryKind::PullRequest => WorkIcon::PullRequest,
        SummaryKind::Action(action) => match action {
            Action::LinkPr | Action::UnlinkPr | Action::ListPrs | Action::WatchPr | Action::UnwatchPr => {
                WorkIcon::PullRequest
            }
            Action::Read => WorkIcon::Eye,
            Action::Edit => WorkIcon::SquarePen,
            Action::Command => WorkIcon::Terminal,
            Action::ThreadCreate => WorkIcon::T3,
            Action::Browser => WorkIcon::Browser,
            Action::Device => WorkIcon::Device,
            Action::Search => WorkIcon::Globe,
            Action::CodeSearch => WorkIcon::Search,
            Action::Other => WorkIcon::Wrench,
            Action::Update => WorkIcon::Hammer,
        },
        SummaryKind::DynamicTool | SummaryKind::Mixed => WorkIcon::Hammer,
        SummaryKind::Reasoning => WorkIcon::Brain,
        SummaryKind::AgentTool => WorkIcon::Bot,
        SummaryKind::ToneTool => WorkIcon::Zap,
    }
}

/// `computeToolGroupAction`.
fn compute_action(entry: &Entry) -> Action {
    if entry.item_type == "thread_created" {
        return Action::ThreadCreate;
    }
    if let Some(t3) = &entry.t3 {
        match t3.tool.summary {
            "link-pr" => return Action::LinkPr,
            "unlink-pr" => return Action::UnlinkPr,
            "list-prs" => return Action::ListPrs,
            "watch-pr" => return Action::WatchPr,
            "unwatch-pr" => return Action::UnwatchPr,
            _ => {}
        }
        match t3.tool.icon {
            T3Icon::Browser => return Action::Browser,
            T3Icon::Device => return Action::Device,
            _ => {}
        }
    }
    if entry.viewed_image {
        return Action::Read;
    }
    let other = || if entry.is_tool_like() { Action::Other } else { Action::Update };
    if matches!(entry.item_type.as_ref(), "approval_request" | "user_input_request") {
        return other();
    }
    let item_type = match entry.item_type.as_ref() {
        t @ ("command_execution" | "file_change" | "web_search") => Some(t),
        "dynamic_tool" => Some("dynamic_tool_call"),
        _ => None,
    };
    let tool_name = entry.tool_name.as_deref().or(entry.tool_title.as_deref());
    let classified = classify(item_type, tool_name, None);
    let local_code_search = entry.item_type == "file_search"
        || (entry.item_type == "web_search"
            && has_word(&normalize_compact_label(entry.tool_title.as_deref().unwrap_or(&entry.label)), "grep"));
    if classified == Classified::Read {
        Action::Read
    } else if classified == Classified::FileChange || entry.item_type == "file_change" {
        Action::Edit
    } else if classified == Classified::Command || entry.item_type == "command_execution" || entry.command.is_some() {
        Action::Command
    } else if classified == Classified::Search {
        if entry.item_type == "web_search" && !local_code_search { Action::Search } else { Action::CodeSearch }
    } else if local_code_search {
        Action::CodeSearch
    } else if entry.item_type == "web_search" {
        Action::Search
    } else if !entry.changed_files.is_empty() {
        Action::Edit
    } else {
        other()
    }
}

fn has_word(text: &str, word: &str) -> bool {
    text.to_lowercase().split(|c: char| !c.is_alphanumeric() && c != '_').any(|token| token == word)
}

// ---------------------------------------------------------------------------
// Group summaries

fn action_label(action: Action, count: usize) -> String {
    let n = |singular: &str, plural: &str| if count == 1 { singular.to_owned() } else { plural.to_owned() };
    match action {
        Action::LinkPr => format!("Linked {count} {}", n("pull request", "pull requests")),
        Action::UnlinkPr => format!("Unlinked {count} {}", n("pull request", "pull requests")),
        Action::WatchPr => format!("Watching {count} {}", n("pull request", "pull requests")),
        Action::UnwatchPr => format!("Stopped watching {count} {}", n("pull request", "pull requests")),
        Action::ListPrs if count == 1 => "Checked linked pull requests".into(),
        Action::ListPrs => format!("Checked linked pull requests {count} times"),
        Action::Read => format!("Read {count} {}", n("file", "files")),
        Action::Edit => format!("Changed {count} {}", n("file", "files")),
        Action::Command => format!("Ran {count} {}", n("command", "commands")),
        Action::ThreadCreate => format!("Created {count} {}", n("thread", "threads")),
        Action::Device => format!("Used device controls {count} {}", n("time", "times")),
        Action::Browser => format!("Used browser {count} {}", n("time", "times")),
        Action::Search => format!("Searched the web {count} {}", n("time", "times")),
        Action::CodeSearch => format!("Searched code {count} {}", n("time", "times")),
        Action::Other => format!("Used {count} {}", n("tool", "tools")),
        Action::Update => format!("Received {count} {}", n("update", "updates")),
    }
}

/// `toolGroupActionCount`: edits count unique files.
fn action_count(action: Action, entries: &[&Entry]) -> usize {
    if action != Action::Edit {
        return entries.len();
    }
    let mut files: Vec<&SharedString> = Vec::new();
    let mut without_files = 0;
    for entry in entries {
        if entry.changed_files.is_empty() {
            without_files += 1;
        }
        for file in &entry.changed_files {
            if !files.contains(&file) {
                files.push(file);
            }
        }
    }
    files.len() + without_files
}

/// `summaryActionPriority`: mutations first, generic tools last.
fn priority(key: &str) -> u8 {
    match key {
        "command" | "edit" | "delegate" | "task-cancel" | "thread-create" | "thread-send" | "thread-interrupt"
        | "workflow-stop" | "background-wait" | "schedule-create" | "schedule-update" | "schedule-delete"
        | "schedule-run" | "thread-configure" | "thread-fork" | "thread-merge" | "thread-organize"
        | "thread-update" | "queue-edit" | "queue-cancel" | "queue-reorder" | "queue-steer" | "question-respond"
        | "worktree-handoff" | "project-create" | "project-update" | "project-delete" | "project-clone"
        | "environment-update" | "attachment-prepare" | "attachment-discard" | "attachment-send" => 0,
        "other" | "update" => 2,
        _ => 1,
    }
}

fn action_key(action: Action) -> &'static str {
    match action {
        Action::LinkPr => "link-pr",
        Action::UnlinkPr => "unlink-pr",
        Action::ListPrs => "list-prs",
        Action::WatchPr => "watch-pr",
        Action::UnwatchPr => "unwatch-pr",
        Action::Read => "read",
        Action::Edit => "edit",
        Action::Command => "command",
        Action::ThreadCreate => "thread-create",
        Action::Browser => "browser",
        Action::Device => "device",
        Action::CodeSearch => "code-search",
        Action::Search => "search",
        Action::Other => "other",
        Action::Update => "update",
    }
}

/// Joins labels as a sentence: "a and b", or "a, b, and c".
fn sentence(labels: &[String]) -> String {
    let labels: Vec<String> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| if index == 0 { label.clone() } else { lowercase_first(label) })
        .collect();
    match labels.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

/// `summarizeToolGroup`: "Ran 2 commands and read 1 file". Returns the
/// summary and whether any summarized call failed.
pub fn summarize_tool_group(entries: &[&Entry]) -> (String, bool) {
    struct Group<'a> {
        key: &'static str,
        action: Action,
        t3: bool,
        entries: Vec<&'a Entry>,
    }
    let tools: Vec<&Entry> = entries.iter().copied().filter(|e| !e.is_reasoning()).collect();
    if !entries.is_empty() && tools.is_empty() {
        let summary = if entries.len() == 1 { "Thought".into() } else { format!("Thought (×{})", entries.len()) };
        return (summary, false);
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut sources: Vec<&ToolSource> = Vec::new();
    for &entry in &tools {
        let t3_summary = entry.t3.as_ref().map(|t3| t3.summary);
        if let (Some(source), None) = (&entry.source, t3_summary) {
            if !sources.iter().any(|s| s.key == source.key) {
                sources.push(source);
            }
            continue;
        }
        let key = t3_summary.unwrap_or(action_key(entry.action));
        match groups.iter_mut().find(|g| g.key == key) {
            Some(group) => group.entries.push(entry),
            None => groups.push(Group { key, action: entry.action, t3: t3_summary.is_some(), entries: vec![entry] }),
        }
    }
    let summaries: Vec<(usize, u8, usize, String, usize)> = groups
        .iter()
        .enumerate()
        .map(|(index, group)| {
            let (label, failed) = if group.t3 {
                let calls: Vec<&T3Call> = group.entries.iter().filter_map(|e| e.t3.as_ref()).collect();
                summarize_t3_calls(group.key, &calls)
            } else {
                let count = action_count(group.action, &group.entries);
                (action_label(group.action, count), group.entries.iter().filter(|e| e.failed()).count())
            };
            (index, priority(group.key), group.entries.len(), label, failed)
        })
        .collect();
    let mut selected: Vec<&(usize, u8, usize, String, usize)> = summaries.iter().collect();
    selected.sort_by_key(|(index, priority, ..)| (*priority, *index));
    selected.truncate(2);
    selected.sort_by_key(|(index, ..)| *index);
    let mut labels: Vec<String> = selected.iter().map(|(.., label, _)| label.clone()).collect();
    if !sources.is_empty() {
        let mut names: Vec<&str> = Vec::new();
        for source in &sources {
            if !names.contains(&source.name.as_ref()) {
                names.push(&source.name);
            }
        }
        let formatted = match names.as_slice() {
            [one] => one.to_string(),
            [a, b] => format!("{a} and {b}"),
            [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
            [] => String::new(),
        };
        let suffix = if sources.iter().all(|s| s.integration) {
            if names.len() == 1 { " integration" } else { " integrations" }
        } else {
            ""
        };
        labels.insert(0, format!("Used {formatted}{suffix}"));
    }
    let sourced = tools.iter().filter(|e| e.source.is_some() && e.t3.is_none()).count();
    let kept: usize = selected.iter().map(|(_, _, count, ..)| count).sum();
    let remaining = tools.len().saturating_sub(sourced + kept);
    if remaining > 0 {
        labels.push(format!("Performed {remaining} other {}", if remaining == 1 { "action" } else { "actions" }));
    }
    (sentence(&labels), summaries.iter().any(|(.., failed)| *failed > 0))
}

/// `toolGroupSummaryKind`.
pub fn summary_kind(entries: &[&Entry]) -> SummaryKind {
    let tools: Vec<&Entry> = entries.iter().copied().filter(|e| !e.is_reasoning()).collect();
    if !entries.is_empty() && tools.is_empty() {
        return SummaryKind::Reasoning;
    }
    if !tools.is_empty() && tools.iter().all(|e| e.t3_icon() == Some(T3Icon::PullRequest)) {
        return SummaryKind::PullRequest;
    }
    let first = tools.first().map(|e| e.action);
    if tools.iter().any(|e| Some(e.action) != first) {
        return SummaryKind::Mixed;
    }
    match first {
        Some(Action::Other) => {}
        Some(action) => return SummaryKind::Action(action),
        None => return SummaryKind::Mixed,
    }
    let fallback = |e: &Entry| {
        if e.item_type == "dynamic_tool" {
            SummaryKind::DynamicTool
        } else if e.item_type == "subagent" || e.tone == Tone::Thinking {
            SummaryKind::AgentTool
        } else if e.tone == Tone::Tool {
            SummaryKind::ToneTool
        } else {
            SummaryKind::Action(Action::Other)
        }
    };
    let kind = fallback(tools[0]);
    if tools.iter().all(|e| fallback(e) == kind) { kind } else { SummaryKind::Mixed }
}

// ---------------------------------------------------------------------------
// Row labels

/// Single-line labels collapse whitespace like CSS `white-space: nowrap`.
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `workEntryDisplayLabel` (no workspace root, as the timeline calls it).
pub fn display_label(entry: &Entry) -> String {
    if entry.item_type == "system_notice" {
        return entry.label.to_string();
    }
    if entry.is_reasoning() || entry.tone == Tone::Thinking {
        let thought = entry.detail.as_deref().map(collapse_whitespace).unwrap_or_default();
        return if thought.is_empty() { entry.label.to_string() } else { thought };
    }
    if let Some(label) = entry.t3_presentation(entry.status) {
        return label;
    }
    if let Some(command) = entry.command.as_deref().filter(|c| !c.trim().is_empty()) {
        return command_display_text(command);
    }
    let action = entry.action;
    let searching = matches!(action, Action::CodeSearch | Action::Search);
    if searching {
        let label =
            if entry.item_type == "file_search" { Some(entry.label.clone()) } else { entry.search_label.clone() };
        if let Some(label) = label {
            return label.to_string();
        }
    }
    let read_paths: &[SharedString] = if entry.changed_files.is_empty() { &entry.paths } else { &entry.changed_files };
    if action == Action::Read
        && let Some(first) = read_paths.first()
    {
        return format_read_label(first, read_paths.len() - 1);
    }
    if let Some(title) = &entry.dynamic_title {
        return title.to_string();
    }
    if let Some(detail) = entry.detail.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        let search_output = searching && detail.contains(['\r', '\n']);
        if !entry.provider_retry && action != Action::Read && !search_output {
            return detail.to_owned();
        }
    }
    if let Some(first) = entry.changed_files.first() {
        let path = first.replace('\\', "/");
        return match entry.changed_files.len() {
            1 => path,
            n => format!("{path} +{} more", n - 1),
        };
    }
    if action == Action::Read && !entry.viewed_image {
        return "Read file".into();
    }
    let heading =
        normalize_compact_label(entry.tool_title.as_deref().filter(|t| !t.is_empty()).unwrap_or(&entry.label));
    let mut chars = heading.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// `singleToolCallLabel`: a lone work row's label.
pub fn single_tool_call_label(entry: &Entry) -> String {
    if entry.is_reasoning() {
        let thought = entry.detail.as_deref().map(collapse_whitespace).unwrap_or_default();
        return if thought.is_empty() { "Thought".into() } else { thought };
    }
    if let Some(label) = entry.t3_presentation(entry.status.or(Some(Status::Completed))) {
        return label;
    }
    if let Some(title) = &entry.dynamic_title {
        return title.to_string();
    }
    if entry.item_type == "web_search" {
        return entry.tool_title.as_deref().unwrap_or("Web search").to_owned();
    }
    display_label(entry)
}

/// `liveWorkEntryLabel`: "Running vp", "Thinking", or the tool's present tense.
pub fn live_label(entry: &Entry, active: bool) -> String {
    let status = match entry.status {
        Some(s @ (Status::Failed | Status::Declined | Status::Stopped | Status::Idle)) => s,
        _ if active || entry.status == Some(Status::InProgress) => Status::InProgress,
        _ => Status::Completed,
    };
    if entry.is_reasoning() {
        let thought = entry.detail.as_deref().map(collapse_whitespace).unwrap_or_default();
        if !thought.is_empty() {
            return thought;
        }
        return if status == Status::InProgress { "Thinking".into() } else { "Thought".into() };
    }
    if let Some(label) = entry.t3_presentation(Some(status)) {
        return label;
    }
    if let Some(command) = entry.command.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        let verb = match status {
            Status::InProgress => "Running",
            Status::Failed => "Failed",
            Status::Declined => "Declined",
            Status::Stopped => "Stopped",
            _ => "Ran",
        };
        return format!("{verb} {}", command_program_name(command).unwrap_or_else(|| "command".into()));
    }
    display_label(entry)
}

/// `liveThoughtLine`: the first sentence (or bold heading) of a thought.
pub fn live_thought_line(markdown: &str) -> String {
    let heading = markdown.trim_start().strip_prefix("**").and_then(|rest| {
        let (heading, after) = rest.split_once("**")?;
        let after = after.trim_start_matches([' ', '\t']);
        let after = after.strip_prefix('\r').unwrap_or(after);
        let own_line = after.is_empty() || after.starts_with('\n');
        (!heading.is_empty() && !heading.contains(['*', '\r', '\n']) && own_line).then_some(heading)
    });
    let source = heading.unwrap_or(markdown);
    let text: String = source
        .lines()
        .map(|line| {
            let line = line.trim_start();
            let line = line.trim_start_matches('#');
            let line = line.strip_prefix("- ").or(line.strip_prefix("* ")).or(line.strip_prefix("+ ")).unwrap_or(line);
            line.replace("**", "").replace('`', "").replace("~~", "")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let text = collapse_whitespace(&text);
    if heading.is_some() {
        return text;
    }
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(byte, b'.' | b'?' | b'!') {
            let mut end = index + 1;
            if end < bytes.len() && matches!(bytes[end], b'"' | b'\'' | b')') {
                end += 1;
            }
            if end < bytes.len() && bytes[end] == b' ' {
                return text[..end].to_owned();
            }
        }
    }
    text
}

// ---------------------------------------------------------------------------
// Commands (`commandLabel.ts`, reduced to the common wrappers)

fn shell_tokens(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some('"') if c == '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started || !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => current.push(c),
        }
    }
    if quote.is_some() {
        return None;
    }
    if started || !current.is_empty() {
        tokens.push(current);
    }
    Some(tokens)
}

fn program_basename(token: &str) -> String {
    let name = token.rsplit(['\\', '/']).next().unwrap_or(token);
    strip_suffix_ci(name, ".exe").unwrap_or(name).to_owned()
}

/// `commandDisplayText`: strips a `bash -lc '…'` wrapper.
pub fn command_display_text(command: &str) -> String {
    let trimmed = command.trim();
    let Some(tokens) = shell_tokens(trimmed) else { return trimmed.to_owned() };
    let is_shell = tokens.first().is_some_and(|t| {
        matches!(program_basename(t).as_str(), "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" | "fish")
    });
    if !is_shell || tokens.len() < 3 {
        return trimmed.to_owned();
    }
    let flag = &tokens[tokens.len() - 2];
    let script = &tokens[tokens.len() - 1];
    let flags_ok = tokens[1..tokens.len() - 1].iter().all(|t| t.starts_with('-'));
    if flags_ok && flag.starts_with('-') && flag.ends_with('c') && !script.trim().is_empty() {
        script.trim().to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `commandProgramName`, for the first meaningful command of a script.
pub fn command_program_name(command: &str) -> Option<String> {
    let script = command_display_text(command);
    let segments = script.split(['\n', ';', '|', '&']).map(str::trim).filter(|s| !s.is_empty());
    for segment in segments {
        let tokens = shell_tokens(segment).unwrap_or_else(|| segment.split_whitespace().map(str::to_owned).collect());
        let program = tokens.iter().map(String::as_str).find(|t| {
            (!t.contains('=') || t.starts_with('=')) && !matches!(*t, "sudo" | "env" | "time" | "exec" | "command")
        });
        let Some(program) = program else { continue };
        let name = program_basename(program);
        let lower = name.to_lowercase();
        if matches!(lower.as_str(), "cd" | "set-location" | "push-location" | "pop-location" | "export" | "set") {
            continue;
        }
        if name.is_empty() || name.starts_with(['$', '(', '<', '>', '#']) {
            continue;
        }
        return Some(name);
    }
    None
}

// ---------------------------------------------------------------------------
// Time

/// `formatDuration` in `packages/shared/src/orchestrationTiming.ts`.
pub fn format_duration(ms: i64) -> String {
    if ms < 0 {
        return "0ms".into();
    }
    if ms < 1000 {
        return format!("{}ms", ms.max(1));
    }
    if ms < 10_000 {
        let tenths = (ms as f64 / 100.).round() / 10.;
        return format!("{}s", format!("{tenths:.1}").trim_end_matches(".0"));
    }
    #[expect(clippy::cast_possible_truncation, reason = "rounded first, and `as` saturates out-of-range values")]
    let total = (ms as f64 / 1000.).round() as i64;
    if total < 60 {
        return format!("{total}s");
    }
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    let parts: Vec<String> = [(hours, "h"), (minutes, "m"), (seconds, "s")]
        .into_iter()
        .filter(|(value, _)| *value > 0)
        .map(|(value, unit)| format!("{value}{unit}"))
        .collect();
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn prefix_match_never_splits_a_character() {
        assert_eq!(strip_prefix_ci("正在思考", "tool:"), None);
        assert_eq!(strip_prefix_ci("Tool:read", "tool:"), Some("read"));
    }

    use super::*;
    use serde_json::json;

    fn entry(value: Value) -> Entry {
        let mut value = value;
        value["status"] = value.get("status").cloned().unwrap_or(json!("completed"));
        value["id"] = value.get("id").cloned().unwrap_or(json!("item"));
        entry_from_item(&Rc::new(serde_json::from_value::<TurnItem>(value).unwrap()))
    }

    #[test]
    fn token_counts_format_like_the_web() {
        let formatted: Vec<String> = [120_000., 45_600., 1_500., 1_000., 999., 2_500_000.].map(format_tokens).into();
        assert_eq!(formatted, ["120K", "45.6K", "1.50K", "1K", "999", "2.50M"]);
    }

    fn command(input: &str) -> Entry {
        entry(json!({"type": "command_execution", "input": input}))
    }

    fn read(path: &str) -> Entry {
        entry(json!({"type": "dynamic_tool", "toolName": "Read", "input": {"file_path": path}}))
    }

    fn edit(path: &str) -> Entry {
        entry(json!({"type": "file_change", "fileName": path}))
    }

    fn tool(name: &str) -> Entry {
        entry(json!({"type": "dynamic_tool", "toolName": name, "input": {}}))
    }

    fn reasoning(text: &str) -> Entry {
        entry(json!({"type": "reasoning", "text": text}))
    }

    fn summary(entries: &[Entry]) -> String {
        summarize_tool_group(&entries.iter().collect::<Vec<_>>()).0
    }

    #[test]
    fn summaries_follow_the_web_grammar() {
        let wait = || tool("mcp__t3-code__t3_background_wait");
        let cases: Vec<(Vec<Entry>, &str)> = vec![
            (vec![command("ls"), command("pwd")], "Ran 2 commands"),
            (vec![tool("PowerShell"), read("a.rs")], "Used 1 tool and read 1 file"),
            (vec![edit("a.rs"), command("cargo test")], "Changed 1 file and ran 1 command"),
            (
                vec![command("ls"), wait(), wait(), tool("PowerShell")],
                "Ran 1 command, updated background waits 2 times, and performed 1 other action",
            ),
            (vec![edit("a.rs"), edit("a.rs"), edit("b.rs")], "Changed 2 files"),
            (vec![reasoning("hmm")], "Thought"),
            (vec![reasoning("a"), reasoning("b")], "Thought (×2)"),
            (vec![reasoning("a"), command("ls")], "Ran 1 command"),
            (
                vec![read("a"), read("b"), tool("Grep"), tool("Glob"), command("ls")],
                "Read 2 files, ran 1 command, and performed 2 other actions",
            ),
            (vec![tool("PowerShell"), tool("TodoWrite")], "Used 2 tools"),
        ];
        for (entries, expected) in cases {
            assert_eq!(summary(&entries), expected);
        }
    }

    #[test]
    fn external_sources_lead_the_summary() {
        let sourced = |name: &str, kind: &str| {
            entry(
                json!({"type": "dynamic_tool", "toolName": "x", "input": {}, "toolSource": {"key": name, "name": name, "kind": kind}}),
            )
        };
        assert_eq!(summary(&[sourced("Linear", "integration")]), "Used Linear integration");
        assert_eq!(
            summary(&[sourced("Linear", "integration"), sourced("Slack", "integration"), command("ls")]),
            "Used Linear and Slack integrations and ran 1 command"
        );
        assert_eq!(
            summary(&[sourced("Chrome", "browser"), sourced("Linear", "integration"), sourced("Slack", "integration")]),
            "Used Chrome, Linear, and Slack"
        );
    }

    #[test]
    fn t3_tool_summaries_count_their_targets() {
        let send = |thread: &str| {
            entry(
                json!({"type": "dynamic_tool", "toolName": "mcp__t3-code__t3_thread_send", "input": {"threadId": thread}}),
            )
        };
        assert_eq!(summary(&[send("a"), send("b")]), "Sent messages to 2 threads");
        assert_eq!(summary(&[send("a")]), "Sent 1 message to 1 thread");
        let pending = entry(
            json!({"type": "dynamic_tool", "status": "running", "toolName": "t3-code.delegate_task", "input": {}}),
        );
        assert_eq!(summary(&[pending]), "Tried to delegate 1 task");
        assert_eq!(summary(&[tool("T3-code.task_status"), tool("t3_code/task_status")]), "Checked task status 2 times");
    }

    #[test]
    fn summary_kind_picks_the_group_icon() {
        let kind = |entries: &[Entry]| summary_kind(&entries.iter().collect::<Vec<_>>());
        assert_eq!(kind(&[command("a"), command("b")]), SummaryKind::Action(Action::Command));
        assert_eq!(kind(&[command("a"), read("b")]), SummaryKind::Mixed);
        assert_eq!(kind(&[tool("PowerShell"), tool("TodoWrite")]), SummaryKind::DynamicTool);
        assert_eq!(kind(&[reasoning("a")]), SummaryKind::Reasoning);
        assert_eq!(summary_icon(kind(&[read("a")])), WorkIcon::Eye);
        assert_eq!(tool("PowerShell").icon(), WorkIcon::Wrench);
    }

    #[test]
    fn t3_tool_names_resolve_across_providers() {
        for label in [
            "mcp__t3-code__preview_click",
            "mcp__t3_code__preview_click",
            "mcp__t3code__preview_click",
            "T3-code.preview_click",
            "t3-code · preview_click completed",
            "t3_code/preview_click",
            "preview_click",
        ] {
            assert_eq!(t3_tool_name(label).map(|tool| tool.name), Some("preview_click"), "{label}");
        }
        for label in ["mcp__github__preview_click", "t3-code.unknown_tool", "Search files"] {
            assert!(t3_tool_name(label).is_none(), "{label}");
        }
    }

    #[test]
    fn labels_and_commands_with_case_changing_characters_do_not_panic() {
        // `to_lowercase` changes byte lengths for these: Kelvin sign 3→1, Ⱥ 2→3, İ 2→3.
        let tricky =
            ["t3code·Ⱥ", "T3CODEȺ.x", "\u{212A}.exe", "İİ complete", "Ⱥ COMPLETE", "t3-code-Ⱥ_preview_click", "é"];
        for label in tricky {
            let _ = t3_tool_name(label);
            let _ = normalize_compact_label(label);
            let _ = program_basename(label);
            let _ = command_display_text(label);
        }
        assert_eq!(program_basename("C:\\Tools\\\u{212A}.EXE"), "\u{212A}");
        assert_eq!(program_basename("bash.Exe"), "bash");
        assert_eq!(normalize_compact_label("Read file COMPLETED"), "Read file");
    }

    #[test]
    fn single_labels_use_tense_and_targets() {
        let click = |status: &str| {
            entry(json!({"type": "dynamic_tool", "status": status, "toolName": "T3-code.preview_click", "input": {}}))
        };
        assert_eq!(single_tool_call_label(&click("completed")), "Clicked in the preview browser");
        assert_eq!(live_label(&click("running"), true), "Clicking in the preview browser");
        assert_eq!(single_tool_call_label(&click("failed")), "Failed to click in the preview browser");
        let failed_clone = entry(
            json!({"type": "dynamic_tool", "status": "running", "toolName": "T3-code.t3_project_clone", "input": {}, "output": {"isError": true}}),
        );
        assert_eq!(live_label(&failed_clone, true), "Failed to clone a repository");
        assert!(failed_clone.failed());
        for (url, target) in [
            ("https://github.com/acme/web/pull/7", "Linked PR #7"),
            ("https://gitlab.com/acme/web/-/merge_requests/7", "Linked MR #7"),
        ] {
            let link =
                entry(json!({"type": "dynamic_tool", "toolName": "T3-code.link_pull_request", "input": {"url": url}}));
            assert_eq!(single_tool_call_label(&link), target);
        }
        assert_eq!(single_tool_call_label(&command("bash -lc 'git status'")), "git status");
        assert_eq!(
            live_label(
                &entry(json!({"type": "command_execution", "status": "running", "input": "cd app && vp test"})),
                true
            ),
            "Running vp"
        );
        assert_eq!(single_tool_call_label(&read("src\\main.rs")), "Read src/main.rs");
        assert_eq!(single_tool_call_label(&reasoning("The user  wants\nthis.")), "The user wants this.");
    }

    #[test]
    fn failures_trust_exit_codes() {
        let with = |fields: Value| {
            let mut value = json!({"type": "command_execution", "input": "rg x"});
            value.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
            entry(value)
        };
        assert!(with(json!({"exitCode": 2})).failed());
        assert!(!with(json!({"exitCode": 0, "outputIndicatesFailure": true})).failed());
        assert!(with(json!({"outputIndicatesFailure": true})).failed());
        assert!(!reasoning("exit code 2 is expected").failed());
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(500), "500ms");
        assert_eq!(format_duration(1_250), "1.3s");
        assert_eq!(format_duration(59_500), "1m");
        assert_eq!(format_duration(65_000), "1m 5s");
        assert_eq!(format_duration(3_723_000), "1h 2m 3s");
    }

    #[test]
    fn thought_lines_end_at_the_first_sentence() {
        assert_eq!(live_thought_line("**Planning the port**\nMore text."), "Planning the port");
        assert_eq!(live_thought_line("I need to `read` it. Then edit."), "I need to read it.");
    }
}
