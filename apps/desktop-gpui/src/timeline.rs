//! Turns a thread's `visibleTurnItems` into chat rows.
//! Mirrors `deriveTimelineEntriesFromVisibleTurnItems` in `apps/web/src/session-logic.ts`.

use gpui_kit::SharedString;
use serde_json::Value;

use crate::model::{ThreadProjection, TurnItem};

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    User { id: SharedString, text: SharedString },
    Assistant { id: SharedString, text: SharedString, streaming: bool },
    Plan { id: SharedString, markdown: SharedString },
    Work { id: SharedString, kind: WorkKind, label: SharedString, detail: Option<SharedString>, status: SharedString },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkKind {
    Thinking,
    Command,
    Edit,
    Search,
    Tool,
    Approval,
    Question,
    Error,
    Notice,
    Agent,
}

pub fn rows(projection: &ThreadProjection) -> Vec<Row> {
    projection.visible_turn_items.iter().filter_map(|row| row_for(&row.item)).collect()
}

fn row_for(item: &TurnItem) -> Option<Row> {
    let id = SharedString::from(item.id.clone());
    let text = |key: &str| item.str(key).unwrap_or_default().to_owned();
    let status = SharedString::from(item.status.clone());
    let work = |kind, label: String, detail: Option<String>| {
        Some(Row::Work {
            id: id.clone(),
            kind,
            label: label.into(),
            detail: detail.map(Into::into),
            status: status.clone(),
        })
    };
    match item.item_type.as_str() {
        "checkpoint" => None,
        "user_message" => {
            // A queued message shows in the queue control until its run starts.
            if item.str("inputIntent") == Some("queued_turn") && item.status == "pending" {
                return None;
            }
            Some(Row::User { id, text: text("text").into() })
        }
        "assistant_message" => {
            Some(Row::Assistant { id, text: text("text").into(), streaming: item.bool("streaming") })
        }
        "proposed_plan" => Some(Row::Plan { id, markdown: text("markdown").into() }),
        "reasoning" => {
            let body = text("text");
            let summary = first_line(&body).unwrap_or("Thinking").to_owned();
            work(WorkKind::Thinking, summary, None)
        }
        // Shown by the run's status, not as a command (`turnItemIsWorkspacePreparation`).
        "command_execution" if item.str("input") == Some("Preparing workspace") => None,
        "error"
            if item.status == "cancelled"
                && item.raw.pointer("/failure/code").and_then(Value::as_str)
                    == Some("workspace_preparation_failed") =>
        {
            None
        }
        "command_execution" => {
            let exit = item.raw.get("exitCode").and_then(Value::as_i64);
            let detail = exit.filter(|code| *code != 0).map(|code| format!("exit {code}"));
            work(WorkKind::Command, format!("Ran {}", text("input")), detail)
        }
        "file_change" => {
            let mut label = format!("Edited {}", item.str("fileName").unwrap_or("files"));
            if let (Some(add), Some(del)) = (number(&item.raw, "additions"), number(&item.raw, "deletions")) {
                label.push_str(&format!("  +{add} −{del}"));
            }
            work(WorkKind::Edit, label, None)
        }
        "file_search" => work(WorkKind::Search, format!("Searched files for {}", text("pattern")), None),
        "web_search" => {
            let patterns = item.raw.get("patterns").and_then(Value::as_array);
            let query = patterns.and_then(|p| p.first()).and_then(Value::as_str).unwrap_or("the web");
            work(WorkKind::Search, format!("Searched {query}"), None)
        }
        "dynamic_tool" => {
            work(WorkKind::Tool, item.str("toolName").or(item.str("title")).unwrap_or("Tool call").to_owned(), None)
        }
        "todo_list" => work(WorkKind::Notice, "Updated the plan".to_owned(), todo_summary(item)),
        "approval_request" => work(
            WorkKind::Approval,
            format!("Asked for approval: {}", item.str("requestKind").unwrap_or("action")),
            item.str("prompt").map(str::to_owned),
        ),
        "user_input_request" => work(WorkKind::Question, "Asked a question".to_owned(), None),
        "error" => {
            let message = item.raw.pointer("/failure/message").and_then(Value::as_str).unwrap_or("Error");
            work(WorkKind::Error, message.to_owned(), None)
        }
        "notification" => work(WorkKind::Notice, text("summary"), item.str("detail").map(str::to_owned)),
        "system_notice" | "run_interrupt_request" | "run_interrupt_result" => {
            work(WorkKind::Notice, text("message"), None)
        }
        "compaction" => work(WorkKind::Notice, "Compacted the conversation".to_owned(), None),
        "subagent" => {
            let label = item
                .str("title")
                .map(str::to_owned)
                .unwrap_or_else(|| first_line(&text("prompt")).unwrap_or("Subagent").to_owned());
            work(WorkKind::Agent, label, None)
        }
        "handoff" => work(WorkKind::Notice, "Switched provider".to_owned(), None),
        "fork" => work(WorkKind::Notice, "Forked thread".to_owned(), None),
        _ => item.str("title").and_then(|title| work(WorkKind::Tool, title.to_owned(), None)),
    }
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(|line| line.trim().trim_matches('*').trim()).find(|line| !line.is_empty())
}

fn number(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

fn todo_summary(item: &TurnItem) -> Option<String> {
    let steps = item.raw.get("steps")?.as_array()?;
    let done = steps.iter().filter(|s| s.get("status").and_then(Value::as_str) == Some("completed")).count();
    Some(format!("{done}/{} done", steps.len()))
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

    fn user(id: &str) -> Row {
        Row::User { id: id.into(), text: id.into() }
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
        let item = |value: serde_json::Value| serde_json::from_value::<TurnItem>(value).unwrap();
        let queued = item(
            serde_json::json!({"id": "q", "type": "user_message", "inputIntent": "queued_turn", "status": "pending", "text": "later"}),
        );
        let checkpoint = item(serde_json::json!({"id": "c", "type": "checkpoint", "status": "completed"}));
        let started = item(
            serde_json::json!({"id": "s", "type": "user_message", "inputIntent": "queued_turn", "status": "completed", "text": "now"}),
        );
        assert_eq!(row_for(&queued), None);
        assert_eq!(row_for(&checkpoint), None);
        assert_eq!(row_for(&started), Some(Row::User { id: "s".into(), text: "now".into() }));
    }
}
