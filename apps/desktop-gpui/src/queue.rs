//! The thread's queued follow-ups: `deriveThreadQueueWorkflowState` in
//! `packages/client-runtime/src/state/threadWorkflows.ts`.

use std::collections::HashSet;

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
    /// A guest's message keeps the guest's words; the owner can cancel it, not rewrite it. An
    /// unknown message can't be opened either, so its placeholder is never sent as its text.
    pub editable: bool,
    pub created_at: Option<SharedString>,
    /// "Sent by T3 Code" above the bubble.
    pub sent_by_t3code: bool,
    /// It starts a fresh provider session (`/clear`).
    pub fresh_context: bool,
    /// Its image attachments, shown as thumbnails above the text.
    pub images: Vec<QueuedImage>,
}

/// An image a queued message carries: its attachment id, and its name for a missing preview.
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedImage {
    pub id: SharedString,
    pub name: SharedString,
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
    /// `canSendRun`: a fresh message starts its own turn, so it may skip a compaction but
    /// never steer into a turn.
    pub fn can_send(&self, message: &QueuedMessage) -> bool {
        self.can_send_now || (self.can_promote_to_steer && (!message.fresh_context || self.promote_skips_compaction))
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
    let notifications: HashSet<Option<&str>> = projection
        .turn_items
        .iter()
        .filter(|item| item.item_type == "notification")
        .map(|item| item.str("messageId"))
        .chain(
            projection
                .messages
                .iter()
                .filter(|m| m.raw.get("notification").is_some_and(|n| !n.is_null()))
                .map(|m| Some(m.id.as_str())),
        )
        .collect();
    let message_item = |message_id: &str| projection.user_message(message_id);
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
        .filter(|run| !notifications.contains(&run.user_message_id.as_deref()))
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
            let images: Vec<QueuedImage> = item
                .and_then(|item| item.raw.get("attachments"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|attachment| attachment["type"] == "image")
                .filter_map(|attachment| {
                    let id = attachment["id"].as_str()?.to_owned().into();
                    Some(QueuedImage { id, name: attachment["name"].as_str().unwrap_or_default().to_owned().into() })
                })
                .collect();
            QueuedMessage {
                run_id: run.id.clone().into(),
                message_id: message_id.into(),
                preview: preview_text(&text, !images.is_empty()).into(),
                text: text.into(),
                editable: item.is_some() && guest.is_none(),
                created_at: item
                    .and_then(|item| item.str("createdAt").or(item.str("startedAt")))
                    .map(|at| at.to_owned().into()),
                sent_by_t3code: guest.is_none() && item.is_some_and(sent_by_t3code),
                fresh_context: run.fresh_context,
                images,
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

/// Who a user message reads as sent by, when not the user (`resolveUserMessagePresentation`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribution {
    /// T3 Code itself sent it: "Sent by T3 Code" above the bubble.
    T3Code,
    /// Another agent sent it (a subagent's task, say), from `sender_thread` when it names one.
    Agent { sender_thread: Option<SharedString> },
    /// A scheduled task's prompt.
    Automation { scheduled_task_id: Option<SharedString> },
    /// A guest's message, which the owner sent on.
    Guest { name: SharedString },
}

const LEGACY_AUTOMATION_PREFIX: &str = "[Triggered by schedule task: ";

/// `LEGACY_AUTOMATION_PREFIX`: the prefix's length when `text` starts with
/// `[Triggered by schedule task: <name>]` and a blank line.
fn legacy_automation_prefix(text: &str) -> Option<usize> {
    let rest = text.strip_prefix(LEGACY_AUTOMATION_PREFIX)?;
    let line = rest.find(['\r', '\n']).unwrap_or(rest.len());
    // A name of at least one character, which may hold `]` itself.
    if line < 2 || !rest[..line].ends_with(']') {
        return None;
    }
    let after = &rest[line..];
    let blank = ["\r\n\r\n", "\r\n\n", "\n\r\n", "\n\n"].into_iter().find(|blank| after.starts_with(blank))?;
    Some(LEGACY_AUTOMATION_PREFIX.len() + line + blank.len())
}

/// `LEGACY_AUTOMATION_MESSAGE_ID`: the task id in `scheduled-task-message:<id>:<n>:scheduled|manual`.
fn legacy_automation_task(message_id: &str) -> Option<&str> {
    let rest = message_id.strip_prefix("scheduled-task-message:")?;
    let (rest, trigger) = rest.rsplit_once(':')?;
    let (task, count) = rest.rsplit_once(':')?;
    let counted = !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit());
    (matches!(trigger, "scheduled" | "manual") && counted && !task.is_empty()).then_some(task)
}

/// `resolveUserMessagePresentation`: the words people see and who sent them.
pub fn user_message_presentation(item: &TurnItem) -> (String, Option<Attribution>) {
    let text = item.str("text").unwrap_or_default();
    // The stored text names the guest for the agent; people see the guest's own words.
    if let Some(guest) = item.raw.pointer("/context/guest").filter(|guest| guest.is_object()) {
        let field = |key: &str| guest.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
        return (field("text"), Some(Attribution::Guest { name: field("name").into() }));
    }
    if let Some(task) = item.str("scheduledTaskId") {
        let attribution = Attribution::Automation { scheduled_task_id: Some(task.to_owned().into()) };
        return (text.to_owned(), Some(attribution));
    }
    if let Some(prefix) = legacy_automation_prefix(text) {
        let task = item.str("messageId").and_then(legacy_automation_task);
        if task.is_some() || item.str("createdBy") == Some("agent") {
            let attribution = Attribution::Automation { scheduled_task_id: task.map(|t| t.to_owned().into()) };
            return (text[prefix..].to_owned(), Some(attribution));
        }
    }
    let attribution = (item.str("createdBy") == Some("agent")).then(|| {
        if item.str("creationSource") == Some("server") {
            Attribution::T3Code
        } else {
            Attribution::Agent { sender_thread: item.str("senderThreadId").map(|t| t.to_owned().into()) }
        }
    });
    (text.to_owned(), attribution)
}

/// `resolveUserMessagePresentation`'s "t3code" attribution: T3 Code itself sent it.
pub fn sent_by_t3code(item: &TurnItem) -> bool {
    user_message_presentation(item).1 == Some(Attribution::T3Code)
}

/// `resolveUserMessagePresentation`'s `null` attribution: the user typed it, not a guest, an
/// automation (or its legacy prefixed message), another agent or T3 Code itself.
pub fn sent_by_user(item: &TurnItem) -> bool {
    let legacy_automation = item.str("text").is_some_and(|text| text.starts_with("[Triggered by schedule task: "))
        && item.str("messageId").is_some_and(|id| {
            id.strip_prefix("scheduled-task-message:")
                .and_then(|rest| rest.rsplit_once(':'))
                .and_then(|(rest, trigger)| (trigger == "scheduled" || trigger == "manual").then_some(rest))
                .and_then(|rest| rest.rsplit_once(':'))
                .is_some_and(|(task, n)| !task.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
    item.raw.get("scheduledTaskId").is_none_or(Value::is_null)
        && item.raw.pointer("/context/guest").is_none_or(Value::is_null)
        && item.str("createdBy") != Some("agent")
        && !legacy_automation
}

/// `isCompactionCommandMessage`.
fn is_compaction_command(item: &TurnItem) -> bool {
    let attachments = item.raw.get("attachments").and_then(Value::as_array).map_or(0, Vec::len);
    item.str("text").is_some_and(|text| text.trim().eq_ignore_ascii_case("/compact")) && attachments == 0
}

/// The preview `QueuedRunsControl` shows: nothing for a files-only message, context
/// chips (`[label](t3-context://…)`) as their labels, and image chips (`![label](…/image/…)`)
/// as nothing when the message shows its thumbnails.
pub fn preview_text(text: &str, thumbnails: bool) -> String {
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
                let image = open > 0 && rest.as_bytes()[open - 1] == b'!';
                out.push_str(&rest[..if image { open - 1 } else { open }]);
                if !(image && thumbnails && rest[label_end..].starts_with("](t3-context://image/")) {
                    out.push_str(&rest[open + 1..label_end]);
                }
                rest = &rest[end..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    crate::assistant_citations::to_plain_text(&out).trim().to_owned()
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
            {"id": "r6", "status": "queued", "ordinal": 6, "userMessageId": "m6"},
        ]);
        let items = vec![
            message("m2", "second"),
            message("m3", "first"),
            json!({"id": "n", "type": "notification", "messageId": "m5"}),
        ];
        let notice = json!({"id": "m6", "role": "user", "text": "done", "notification": {"kind": "x"}});
        let state = derive(&projection(runs, items, json!({"messages": [notice]})), false);
        let ids: Vec<&str> = state.queued.iter().map(|q| q.run_id.as_ref()).collect();
        assert_eq!(ids, ["r3", "r2"]);
        assert_eq!(state.queued[0].text.as_ref(), "first");
        // Nothing runs, so a queued message starts a turn.
        assert!(state.can_send_now && state.can_send(&state.queued[0]));
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
        assert!(state.can_send(&state.queued[0]));
        let mut fresh_runs = runs.clone();
        fresh_runs[1]["freshContext"] = json!(true);
        let fresh = derive(&projection(fresh_runs.clone(), items.clone(), session(true)), false);
        assert!(!fresh.can_send(&fresh.queued[0]), "a fresh session never steers into a turn");
        let state = derive(&projection(runs.clone(), items, session(false)), false);
        assert!(!state.can_send(&state.queued[0]));

        let compacting = vec![message("m1", " /COMPACT "), message("m2", "later")];
        let state = derive(&projection(runs, compacting.clone(), session(false)), false);
        assert!(state.can_promote_to_steer && state.promote_skips_compaction);
        let fresh = derive(&projection(fresh_runs, compacting, session(false)), false);
        assert!(fresh.can_send(&fresh.queued[0]), "but it may skip a compaction");
        assert_eq!(state.send_tooltip(), "Skip compaction and send this message");
    }

    #[test]
    fn only_messages_the_user_typed_count_as_theirs() {
        let item = |patch: Value| {
            let mut raw = message("m", "hi");
            raw.as_object_mut().unwrap().extend(patch.as_object().unwrap().clone());
            serde_json::from_value::<TurnItem>(raw).unwrap()
        };
        assert!(sent_by_user(&item(json!({"createdBy": "user", "creationSource": "web"}))));
        assert!(sent_by_user(&item(json!({"scheduledTaskId": null}))));
        assert!(!sent_by_user(&item(json!({"context": {"guest": {"name": "Ada", "text": "hi"}}}))));
        assert!(!sent_by_user(&item(json!({"scheduledTaskId": "task-1"}))));
        assert!(!sent_by_user(&item(json!({"createdBy": "agent", "creationSource": "server"}))));
        assert!(!sent_by_user(&item(json!({"createdBy": "agent", "creationSource": "provider"}))));
        let legacy = "[Triggered by schedule task: Daily]

run it";
        assert!(!sent_by_user(&item(json!({"text": legacy, "messageId": "scheduled-task-message:t1:3:manual"}))));
        assert!(sent_by_user(&item(json!({"text": legacy, "messageId": "m"}))));
    }

    #[test]
    fn a_queued_message_reads_from_the_messages_the_server_sends_it_as() {
        // A newly queued message is only a `message.updated`: its user_message item comes when the run starts.
        let mut state = crate::model::ThreadState::default();
        let runs = json!([{"id": "q", "status": "queued", "ordinal": 1, "userMessageId": "m"}]);
        let snapshot = json!({"thread": {"id": "t", "title": "T"}, "runs": runs});
        state.apply(&json!({"kind": "snapshot", "snapshotSequence": 1, "projection": snapshot})).unwrap();
        let queued = |state: &crate::model::ThreadState| derive(state.projection.as_ref().unwrap(), false).queued;
        // Its message isn't known, so the shown placeholder can't be opened and sent as its text.
        assert_eq!(queued(&state)[0].text.as_ref(), "Queued message");
        assert!(!queued(&state)[0].editable);

        let message = |sequence: u64, text: &str, role: &str| {
            json!({"kind": "event", "sequence": sequence, "event": {"type": "message.updated", "threadId": "t", "payload": {
                "id": "m", "threadId": "t", "runId": "q", "nodeId": null, "role": role, "text": text,
                "attachments": [{"type": "image", "id": "att-1", "name": "a.png"}], "streaming": false,
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z"}}})
        };
        state.apply(&message(2, "queued three", "user")).unwrap();
        let shown = &queued(&state)[0];
        assert_eq!((shown.text.as_ref(), shown.editable), ("queued three", true));
        assert_eq!(shown.images, [QueuedImage { id: "att-1".into(), name: "a.png".into() }]);
        let projection = state.projection.as_ref().unwrap();
        assert_eq!(projection.user_message("m").and_then(|m| m.str("text")), Some("queued three"));
        // A save's `message.updated` is what the queue shows next.
        state.apply(&message(3, "queued three, edited", "user")).unwrap();
        assert_eq!(queued(&state)[0].text.as_ref(), "queued three, edited");
        // Assistant messages stream through the same event and aren't kept.
        state.apply(&message(4, "reply", "assistant")).unwrap();
        assert_eq!(queued(&state)[0].text.as_ref(), "queued three, edited");
    }

    #[test]
    fn queued_messages_carry_their_image_attachments() {
        let runs = json!([{"id": "q", "status": "queued", "ordinal": 1, "userMessageId": "m"}]);
        let mut item = message("m", "see ![a.png](t3-context://image/image_1)");
        item["attachments"] = json!([
            {"type": "image", "id": "att-1", "name": "a.png"},
            {"type": "file", "id": "att-2", "name": "notes.txt"},
        ]);
        let state = derive(&projection(runs, vec![item], json!({})), false);
        let queued = &state.queued[0];
        assert_eq!(queued.images, [QueuedImage { id: "att-1".into(), name: "a.png".into() }]);
        assert_eq!(queued.preview.as_ref(), "see");
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
        assert_eq!(preview_text("see [main.rs](t3-context://file/1) now ", false), "see main.rs now");
        assert_eq!(preview_text("[a] [b](https://x)", false), "[a] [b](https://x)");
        assert_eq!(preview_text(ATTACHMENT_ONLY_PROMPT, false), "");
        let image = "look ![shot.png](t3-context://image/image_1) here";
        assert_eq!(preview_text(image, false), "look shot.png here");
        assert_eq!(preview_text(image, true), "look  here", "the thumbnail stands for the image");
        let cited = "about [Assistant quote](t3-citation://v1/e/t/m?text=the+quote&start=0&end=9&prefix=&suffix=) ";
        assert_eq!(preview_text(cited, false), "about the quote", "a citation reads as its quote");
        assert_eq!(edit_text(ATTACHMENT_ONLY_PROMPT), "");
    }
}
