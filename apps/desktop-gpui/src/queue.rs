//! The thread's queued follow-ups: `deriveThreadQueueWorkflowState` in
//! `packages/client-runtime/src/state/threadWorkflows.ts`, joined to each run's
//! `user_message` turn item instead of the projection's message table.

use gpui_kit::SharedString;
use serde_json::Value;

use crate::composer_text::ATTACHMENT_ONLY_PROMPT;
use crate::model::{Run, ThreadProjection, TurnItem};

#[derive(Clone, Debug, PartialEq)]
pub struct QueuedMessage {
    pub run_id: SharedString,
    pub message_id: SharedString,
    /// The message as sent, which an edit starts from.
    pub text: SharedString,
    /// The text as the queue shows it: context chips as their labels.
    pub preview: SharedString,
    /// A guest's message keeps the guest's words; the owner can cancel it, not rewrite it.
    pub editable: bool,
    pub created_at: Option<SharedString>,
    /// "Sent by T3 Code" above the bubble.
    pub sent_by_t3code: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueueState {
    pub queued: Vec<QueuedMessage>,
    pub active_run_id: Option<SharedString>,
    pub can_send_now: bool,
    pub can_promote_to_steer: bool,
    /// The active run is a `/compact`, so promoting stops it instead of steering.
    pub promote_skips_compaction: bool,
}

impl QueueState {
    pub fn can_send(&self) -> bool {
        self.can_send_now || self.can_promote_to_steer
    }

    /// The "Send now" tooltip (`QueuedRunsControl`).
    pub fn send_tooltip(&self) -> &'static str {
        if self.can_send_now {
            "Start a turn with this message"
        } else if self.promote_skips_compaction {
            "Skip compaction and send this message"
        } else {
            "Send into the current turn"
        }
    }
}

/// The run `resolveActiveThreadRun` picks: the last one preparing, starting, running or waiting.
pub fn active_run(projection: &ThreadProjection) -> Option<&Run> {
    projection
        .runs
        .iter()
        .rev()
        .find(|run| matches!(run.status.as_str(), "preparing" | "starting" | "running" | "waiting"))
}

pub fn derive(projection: &ThreadProjection, archived: bool) -> QueueState {
    let active = active_run(projection);
    let message_item = |message_id: &str| -> Option<&TurnItem> {
        projection
            .turn_items
            .iter()
            .rev()
            .find(|item| item.item_type == "user_message" && item.str("messageId") == Some(message_id))
            .map(|item| item.as_ref())
    };
    let promote_skips_compaction =
        active.and_then(|run| run.user_message_id.as_deref()).and_then(message_item).is_some_and(is_compaction_command);
    let steerable_turn = active.is_some_and(|run| {
        run.status == "running"
            && run.active_attempt_id.as_ref().is_some_and(|attempt| {
                projection
                    .provider_turns
                    .iter()
                    .any(|turn| turn.run_attempt_id.as_ref() == Some(attempt) && turn.status == "running")
            })
    });
    let turns = projection.provider_session().map(|session| &session.capabilities["turns"]);
    let steers = turns.is_some_and(|turns| {
        turns["supportsActiveSteering"].as_bool() == Some(true)
            || turns["supportsSteeringByInterruptRestart"].as_bool() == Some(true)
    });
    // Subagent result deliveries and notifications queue too, but aren't the user's to change.
    let mut runs: Vec<&Run> = projection
        .runs
        .iter()
        .filter(|run| run.status == "queued" && run.delegated_completion.is_none())
        .filter(|run| {
            let id = run.user_message_id.as_deref();
            !projection.turn_items.iter().any(|item| item.item_type == "notification" && item.str("messageId") == id)
        })
        .collect();
    runs.sort_by_key(|run| (run.queue_position.unwrap_or(run.ordinal), run.ordinal));
    let queued = runs
        .into_iter()
        .map(|run| {
            let message_id = run.user_message_id.clone().unwrap_or_default();
            let item = message_item(&message_id);
            let guest = item.and_then(|item| item.raw.pointer("/context/guest"));
            let text = guest
                .and_then(|guest| guest.get("text"))
                .or_else(|| item.and_then(|item| item.raw.get("text")))
                .and_then(Value::as_str)
                .unwrap_or("Queued message")
                .to_owned();
            QueuedMessage {
                run_id: run.id.clone().into(),
                message_id: message_id.into(),
                preview: preview_text(&text).into(),
                text: text.into(),
                editable: guest.is_none(),
                created_at: item
                    .and_then(|item| item.str("createdAt").or(item.str("startedAt")))
                    .map(|at| at.to_owned().into()),
                sent_by_t3code: guest.is_none() && item.is_some_and(sent_by_t3code),
            }
        })
        .collect();
    QueueState {
        queued,
        active_run_id: active.map(|run| run.id.clone().into()),
        can_send_now: active.is_none() && !archived,
        can_promote_to_steer: promote_skips_compaction || (steerable_turn && steers),
        promote_skips_compaction,
    }
}

/// `resolveUserMessagePresentation`'s "t3code" attribution: T3 Code itself sent it.
pub fn sent_by_t3code(item: &TurnItem) -> bool {
    item.raw.get("scheduledTaskId").is_none_or(Value::is_null)
        && item.str("createdBy") == Some("agent")
        && item.str("creationSource") == Some("server")
}

/// `isCompactionCommandMessage`.
fn is_compaction_command(item: &TurnItem) -> bool {
    let attachments = item.raw.get("attachments").and_then(Value::as_array).map_or(0, Vec::len);
    item.str("text").is_some_and(|text| text.trim().eq_ignore_ascii_case("/compact")) && attachments == 0
}

/// The preview `QueuedRunsControl` shows: nothing for a files-only message, context
/// chips (`[label](t3-context://…)`) as their labels.
pub fn preview_text(text: &str) -> String {
    if text == ATTACHMENT_ONLY_PROMPT {
        return String::new();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let chip = rest[open + 1..].find("](t3-context://").and_then(|close| {
            let label_end = open + 1 + close;
            let end = rest[label_end..].find(')')?;
            Some((label_end, label_end + end + 1))
        });
        match chip {
            Some((label_end, end)) if !rest[open + 1..label_end].contains('[') => {
                out.push_str(&rest[..open]);
                out.push_str(&rest[open + 1..label_end]);
                rest = &rest[end..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// The message an edit loads into the composer (`stripAttachmentOnlyBootstrapPrompt`).
pub fn edit_text(text: &str) -> &str {
    if text == ATTACHMENT_ONLY_PROMPT { "" } else { text }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn projection(runs: Value, items: Vec<Value>, extra: Value) -> ThreadProjection {
        let mut value = json!({"thread": {"id": "t", "title": "T"}, "runs": runs, "turnItems": items});
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    fn message(id: &str, text: &str) -> Value {
        json!({"id": format!("item-{id}"), "type": "user_message", "messageId": id, "text": text,
            "inputIntent": "queued_turn", "status": "pending", "createdAt": "2026-01-01T00:00:00Z"})
    }

    #[test]
    fn queued_runs_sort_by_queue_position_and_skip_automatic_deliveries() {
        let runs = json!([
            {"id": "r1", "status": "completed", "ordinal": 1, "userMessageId": "m1"},
            {"id": "r2", "status": "queued", "ordinal": 2, "queuePosition": 2, "userMessageId": "m2"},
            {"id": "r3", "status": "queued", "ordinal": 3, "queuePosition": 1, "userMessageId": "m3"},
            {"id": "r4", "status": "queued", "ordinal": 4, "userMessageId": "m4", "delegatedCompletion": {}},
            {"id": "r5", "status": "queued", "ordinal": 5, "userMessageId": "m5"},
        ]);
        let items = vec![
            message("m2", "second"),
            message("m3", "first"),
            json!({"id": "n", "type": "notification", "messageId": "m5"}),
        ];
        let state = derive(&projection(runs, items, json!({})), false);
        let ids: Vec<&str> = state.queued.iter().map(|q| q.run_id.as_ref()).collect();
        assert_eq!(ids, ["r3", "r2"]);
        assert_eq!(state.queued[0].text.as_ref(), "first");
        // Nothing runs, so a queued message starts a turn.
        assert!(state.can_send_now && state.can_send());
        assert_eq!(state.send_tooltip(), "Start a turn with this message");
        assert!(!derive(&projection(json!([]), vec![], json!({})), true).can_send_now);
    }

    #[test]
    fn promoting_needs_a_running_provider_turn_that_steers_or_a_compaction() {
        let runs = json!([
            {"id": "a", "status": "running", "ordinal": 1, "userMessageId": "m1", "activeAttemptId": "att", "providerThreadId": "pt"},
            {"id": "q", "status": "queued", "ordinal": 2, "userMessageId": "m2"},
        ]);
        let session = |steering: bool| {
            json!({
                "providerThreads": [{"id": "pt", "providerSessionId": "s"}],
                "providerSessions": [{"id": "s", "status": "running",
                    "capabilities": {"turns": {"supportsActiveSteering": steering, "supportsSteeringByInterruptRestart": false}}}],
                "providerTurns": [{"id": "turn", "runAttemptId": "att", "status": "running"}],
            })
        };
        let items = vec![message("m1", "do it"), message("m2", "later")];
        let state = derive(&projection(runs.clone(), items.clone(), session(true)), false);
        assert!(!state.can_send_now && state.can_promote_to_steer && !state.promote_skips_compaction);
        assert_eq!(state.active_run_id.as_deref(), Some("a"));
        assert_eq!(state.send_tooltip(), "Send into the current turn");
        assert!(!derive(&projection(runs.clone(), items, session(false)), false).can_send());

        let compacting = vec![message("m1", " /COMPACT "), message("m2", "later")];
        let state = derive(&projection(runs, compacting, session(false)), false);
        assert!(state.can_promote_to_steer && state.promote_skips_compaction);
        assert_eq!(state.send_tooltip(), "Skip compaction and send this message");
    }

    #[test]
    fn guest_messages_show_the_guests_words_and_stay_read_only() {
        let runs = json!([{"id": "q", "status": "queued", "ordinal": 1, "userMessageId": "m"}]);
        let mut item = message("m", "Guest Ada says: hi");
        item["context"] = json!({"guest": {"text": "hi"}});
        let state = derive(&projection(runs, vec![item], json!({})), false);
        assert_eq!(state.queued[0].text.as_ref(), "hi");
        assert!(!state.queued[0].editable);
    }

    #[test]
    fn preview_shows_chip_labels_and_hides_the_files_only_prompt() {
        assert_eq!(preview_text("see [main.rs](t3-context://file/1) now "), "see main.rs now");
        assert_eq!(preview_text("[a] [b](https://x)"), "[a] [b](https://x)");
        assert_eq!(preview_text(ATTACHMENT_ONLY_PROMPT), "");
        assert_eq!(edit_text(ATTACHMENT_ONLY_PROMPT), "");
    }
}
