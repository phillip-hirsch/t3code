//! Sending the composer (`useComposerSend.ts`, `startThreadTurn` in
//! `packages/client-runtime/src/operations/commands.ts`): `message.dispatch`
//! for a thread, `orchestration.launchThread` for a draft.

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{Context, Keystroke, SharedString, Window};
use serde_json::{Value, json};

use super::attachments::ATTACHMENT_ONLY_PROMPT;
use super::composer::ComposerDraft;
use super::composer_plan;
use super::{Target, Workspace, draft_title, new_id};
use crate::model::{ModelSelection, ThreadProjection};
use crate::new_thread::{self, DraftWorkspace};
use crate::queue::QueuedMessage;
use crate::timeline::Row;

/// A message sent to a thread that its projection doesn't show yet (`optimisticUserMessages`).
pub(super) struct PendingSend {
    thread_id: String,
    message_id: String,
    text: SharedString,
    created_at: SharedString,
    queued: bool,
}

impl PendingSend {
    /// The projection shows it, as a message or as a queued run.
    fn landed(&self, projection: &ThreadProjection) -> bool {
        let id = Some(self.message_id.as_str());
        projection.runs.iter().any(|run| run.user_message_id.as_deref() == id)
            || projection.turn_items.iter().any(|item| item.item_type == "user_message" && item.str("messageId") == id)
    }

    fn row(&self) -> Row {
        if self.queued {
            let message = QueuedMessage {
                run_id: SharedString::default(),
                message_id: self.message_id.clone().into(),
                text: self.text.clone(),
                preview: crate::queue::preview_text(&self.text, false).into(),
                editable: false,
                created_at: Some(self.created_at.clone()),
                sent_by_t3code: false,
                fresh_context: false,
                images: Vec::new(),
            };
            return Row::Queued { message: Rc::new(message), last: true };
        }
        Row::User {
            id: format!("optimistic:{}", self.message_id).into(),
            text: self.text.clone(),
            steer: None,
            grouped_with_previous: false,
            grouped_with_next: false,
            continues_steer: false,
            created_at: self.created_at.clone(),
            revert: None,
            attribution: None,
        }
    }
}

/// Drops the sends `projection` (of `thread_id`) now shows, and returns the rows of the rest:
/// messages for after the timeline, queued ones for after the queue.
pub(super) fn pending_rows(
    pending: &mut Vec<PendingSend>,
    thread_id: &str,
    projection: &ThreadProjection,
) -> (Vec<Row>, Vec<Row>) {
    pending.retain(|send| send.thread_id != thread_id || !send.landed(projection));
    let shown = pending.iter().filter(|send| send.thread_id == thread_id);
    let (queued, sent): (Vec<_>, Vec<_>) = shown.partition(|send| send.queued);
    (sent.into_iter().map(PendingSend::row).collect(), queued.into_iter().map(PendingSend::row).collect())
}

/// What a send shortcut asks for (`composer-logic.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Intent {
    /// Enter or a plain click.
    Foreground,
    /// Ctrl+Enter or Ctrl-click: the other of steer and queue while a turn runs;
    /// in a draft, launch in the background and stay on a fresh draft.
    Alternate,
    /// Ctrl+Alt+Enter: send, then open a new draft (background in a draft).
    AndNewThread,
}

/// `composerRequiresModifier`: plain Enter adds a line instead of sending.
pub(super) fn requires_modifier(send_shortcut: &str, prompt: &str) -> bool {
    send_shortcut == "mod-enter" || (send_shortcut == "mod-enter-multiline" && prompt.contains(['\r', '\n']))
}

/// `composerSubmissionIntentForKey` once no keybinding command took the key: Enter (or
/// Ctrl+Enter) sends, unless Enter needs a modifier and none is held.
pub(super) fn sends_on(keystroke: &Keystroke, requires_modifier: bool) -> bool {
    let m = keystroke.modifiers;
    let modifier = m.control || m.platform;
    keystroke.key == "enter" && !m.shift && !m.alt && (modifier || !requires_modifier)
}

/// `resolveComposerDispatchMode`: a running turn takes `followUpBehavior`, and the
/// alternate path the other of steer and queue; a turn that cannot steer (compacting) queues.
pub(super) fn queues(follow_up: &str, intent: Intent, can_steer: bool) -> bool {
    !can_steer || (follow_up == "queue") != (intent == Intent::Alternate)
}

/// `message.dispatch`'s delivery fields; `queue` picks queueing over steering while a turn runs.
fn dispatch_fields(running: bool, queue: bool) -> (Value, Option<&'static str>) {
    match (running, queue) {
        (false, _) => (json!({"type": "start_immediately"}), Some("auto")),
        (true, true) => (json!({"type": "queue_after_active"}), None),
        (true, false) => (json!({"type": "start_immediately"}), Some("steer")),
    }
}

/// A started thread's `message.dispatch` (`startThreadTurn`).
struct ThreadMessage<'a> {
    thread_id: &'a str,
    text: String,
    attachments: Vec<Value>,
    context: Option<Value>,
    /// [`dispatch_fields`].
    dispatch: (Value, Option<&'static str>),
    /// Omitted, the server keeps the thread's own model.
    model: Option<&'a ModelSelection>,
    /// The plan a build-mode follow-up implements.
    source_plan: Option<&'a str>,
    /// Starts a new provider session without the thread's earlier context (`/clear`).
    fresh_context: bool,
}

impl ThreadMessage<'_> {
    fn command(self) -> Value {
        let (dispatch_mode, delivery) = self.dispatch;
        let mut command = json!({
            "type": "message.dispatch",
            "commandId": new_id(),
            "createdBy": "user",
            "creationSource": "web",
            "threadId": self.thread_id,
            "messageId": new_id(),
            "text": self.text,
            "attachments": self.attachments,
            "dispatchMode": dispatch_mode,
        });
        if let Some(delivery) = delivery {
            command["deliveryIntent"] = delivery.into();
        }
        if let Some(model) = self.model {
            command["modelSelection"] = model.to_json();
        }
        if let Some(context) = self.context {
            command["context"] = context;
        }
        if let Some(plan_id) = self.source_plan {
            command["sourcePlanRef"] = json!({"threadId": self.thread_id, "planId": plan_id});
        }
        if self.fresh_context {
            command["freshContext"] = true.into();
        }
        command
    }
}

impl Workspace {
    /// `followUpBehavior`, except that a send declines a waiting approval, so it steers.
    pub(super) fn follow_up_behavior(&self) -> &str {
        if !self.shown_approvals().is_empty() {
            return "steer";
        }
        self.client_settings.values.get("followUpBehavior").as_str().unwrap_or("steer")
    }

    /// The open thread is still rolling back, so a send would race the rewind.
    pub(super) fn target_rewinding(&self) -> bool {
        matches!(&self.target, Some(Target::Thread { id, .. }) if self.rewinding.contains(id))
    }

    pub(super) fn submit_with(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        // `/side` beats an open question, unless side chats are off and it may be a literal answer.
        let prompt = self.composer.read(cx).value().to_string();
        if (self.question.is_none() || self.side_chat_disabled_reason().is_none())
            && self.submit_side_chat_command(&prompt, window, cx)
        {
            return;
        }
        if self.question.is_some() {
            self.advance_question(window, cx);
            return;
        }
        if self.editing_queued() {
            self.save_queue_edit(window, cx);
            return;
        }
        let text = self.composer.read(cx).value().trim().to_owned();
        let fresh_session = self.fresh_session_command_available();
        if fresh_session && !self.has_attachments() && super::fresh_session::is_clear_command(&text) {
            self.arm_fresh_session(cx);
            self.set_composer_text("", window, cx);
            return;
        }
        // Read once: an arm made while this send is in flight belongs to the next message.
        let fresh_arm = self.fresh_session_arm().filter(|_| fresh_session);
        let (plan_ui, interaction_mode) = self.interaction_mode();
        // `/plan` or `/default` alone switches modes without sending.
        if let Some(mode) =
            composer_plan::standalone_slash_command(&text).filter(|_| plan_ui && !self.has_attachments())
        {
            self.set_composer_text("", window, cx);
            self.set_interaction_mode(mode, window, cx);
            return;
        }
        let follow_up = self.plan_follow_up_target().filter(|_| fresh_arm.is_none());
        if (text.is_empty() && !self.has_attachments() && follow_up.is_none())
            || self.sending
            || self.target_rewinding()
            || self.runtime_mode_ceiling().is_some()
        {
            return;
        }
        if let Some(error) = crate::composer_text::prompt_length_error(&text) {
            self.composer_ui.length_error = Some(error);
            cx.notify();
            return;
        }
        let (Some(client), Some(target)) = (self.client(), self.target.clone()) else { return };
        let (attachments, context) = match self.attachment_payload(&text) {
            Ok(payload) => payload,
            Err(reason) => {
                self.error = Some(reason.into());
                cx.notify();
                return;
            }
        };
        let message_text = if text.is_empty() { ATTACHMENT_ONLY_PROMPT.to_owned() } else { text.clone() };
        let origin = target.clone();
        let (declines, text_is_feedback) = self.approval_declines(&text);
        // Feedback answers the approval as text alone; nothing else may ride along.
        if text_is_feedback && (!attachments.is_empty() || context.is_some()) {
            self.error = Some("Send approval feedback without attachments or added context.".into());
            cx.notify();
            return;
        }
        let mut background = false;
        // Thread settings the message runs with, applied right before it.
        let mut settings_commands = Vec::new();
        let mut sent_mode = None;
        let mut pending_send = None;
        let (method, mut payload, new_thread_id, workspace) = match target {
            Target::Thread { id: thread_id, .. } => {
                // A plan follow-up refines in plan mode, or implements in build mode (`resolvePlanFollowUpSubmission`).
                let (message_text, interaction_mode) = match &follow_up {
                    Some((_, markdown)) => composer_plan::plan_follow_up(&text, markdown),
                    None => (message_text, interaction_mode),
                };
                settings_commands.extend(self.interaction_mode_command(&thread_id, interaction_mode));
                sent_mode = Some(interaction_mode);
                let projection = self.thread.as_ref().and_then(|t| t.state.projection.as_ref());
                let running = projection.is_some_and(super::super::model::ThreadProjection::is_running);
                let can_steer = !projection.is_some_and(super::super::model::ThreadProjection::is_compacting);
                let command = ThreadMessage {
                    thread_id: &thread_id,
                    text: message_text,
                    attachments,
                    context,
                    // A fresh session starts its own turn, so it never steers into the running one.
                    dispatch: dispatch_fields(
                        running,
                        fresh_arm.is_some() || queues(self.follow_up_behavior(), intent, can_steer),
                    ),
                    model: self.model_override.as_ref(),
                    source_plan: follow_up
                        .as_ref()
                        .filter(|_| interaction_mode == "default")
                        .map(|(plan_id, _)| plan_id.as_str()),
                    fresh_context: fresh_arm.is_some(),
                }
                .command();
                // Feedback answers an approval rather than posting a message.
                pending_send = (!text_is_feedback).then(|| PendingSend {
                    thread_id: thread_id.clone(),
                    message_id: command["messageId"].as_str().unwrap_or_default().to_owned(),
                    text: command["text"].as_str().unwrap_or_default().to_owned().into(),
                    created_at: crate::timestamp::iso(crate::timestamp::now_ms()).unwrap_or_default().into(),
                    queued: running && command.get("deliveryIntent").is_none(),
                });
                ("orchestration.dispatchCommand", command, None, None)
            }
            Target::Draft { project_id, .. } => {
                if let Some((title, description)) = self.load_balancing_send_block() {
                    window.push_notification(Notification::warning(description).title(title), cx);
                    return;
                }
                let Some(model) = self.composer_model() else {
                    self.error = Some("No provider is ready. Set one up in the T3 server first.".into());
                    cx.notify();
                    return;
                };
                let strategy = match self.draft_launch_strategy() {
                    Some(Err(error)) => {
                        self.error = Some(error.into());
                        cx.notify();
                        return;
                    }
                    Some(Ok(strategy)) => Some(strategy),
                    None => None,
                };
                // Still loading the workspace controls: resolve it the same way at send.
                let workspace = strategy.is_none().then(|| {
                    let project = self.shell.projects.iter().find(|p| p.id == project_id)?;
                    let has_threads = self.shell.threads.iter().any(|t| t.project_id == project_id);
                    Some((
                        DraftWorkspace::new(self.server_info(), project, has_threads),
                        project.workspace_root.clone(),
                    ))
                });
                let thread_id = self.terminals.take_draft_thread(&project_id).unwrap_or_else(new_id);
                let mut initial = json!({"messageId": new_id(), "text": message_text, "attachments": attachments});
                if let Some(context) = context {
                    initial["context"] = context;
                }
                let input = json!({
                    "commandId": new_id(),
                    "creationSource": "web",
                    "threadId": thread_id,
                    "projectId": project_id,
                    "title": draft_title(&text),
                    "generateTitle": true,
                    "modelSelection": model.to_json(),
                    "runtimeMode": self.runtime_mode(),
                    "interactionMode": interaction_mode,
                    "workspaceStrategy": strategy.unwrap_or_else(|| json!({"type": "root"})),
                    "initialMessage": initial,
                });
                background = intent != Intent::Foreground;
                ("orchestration.launchThread", input, Some(thread_id), workspace.flatten())
            }
        };
        let new_draft_after = intent == Intent::AndNewThread && matches!(origin, Target::Thread { .. });
        let project_id = self.target_project().map(|p| p.id.clone());
        let sent_attachments = std::mem::take(&mut self.attachments);
        let sent_records = self.take_context_records();
        let pending_id = pending_send.as_ref().map(|send| send.message_id.clone());
        self.pending_sends.extend(pending_send);
        self.sending = true;
        self.set_composer_text("", window, cx);
        self.release_typing_hold(false, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            // Declines land first; a failed one keeps the text and sends nothing more.
            let mut result = Ok(Value::Null);
            for decline in declines {
                result = client.request("orchestration.dispatchCommand", decline).await.map_err(|e| e.to_string());
                if result.is_err() {
                    break;
                }
            }
            if result.is_ok() && !text_is_feedback {
                for command in settings_commands {
                    result = client.request("orchestration.dispatchCommand", command).await.map_err(|e| e.to_string());
                    if result.is_err() {
                        break;
                    }
                }
            }
            if result.is_ok() && !text_is_feedback {
                if let Some((workspace, root)) = workspace {
                    let mode = workspace.default_mode(&client).await;
                    let choice = new_thread::WorkspaceChoice::default();
                    match new_thread::list_refs(&client, &root).await.and_then(|refs| {
                        new_thread::launch_strategy(mode, &choice, &refs, workspace.start_from_origin())
                    }) {
                        Ok(strategy) => payload["workspaceStrategy"] = strategy,
                        Err(error) => result = Err(error),
                    }
                }
                if result.is_ok() {
                    result = client.request(method, payload).await.map_err(|e| e.to_string());
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.sending = false;
                match result {
                    Ok(_) => {
                        if fresh_arm.is_some() {
                            this.disarm_fresh_session(&origin, fresh_arm);
                        }
                        if let Some(mode) = sent_mode {
                            this.remember_interaction_mode(origin.clone(), mode);
                        }
                        this.record_prompt_sent(window, cx);
                        if new_thread_id.is_some() {
                            // The launched draft's picks went with it.
                            this.composer_selections.remove(&origin);
                        }
                        let here = this.target.as_ref() == Some(&origin);
                        match (new_thread_id, background) {
                            // A background launch leaves a fresh draft in its place.
                            (Some(_), true) if here => {
                                if let Some(project_id) = project_id {
                                    this.draft_workspace = Default::default();
                                    this.forget_open_draft();
                                    this.start_draft(project_id, window, cx);
                                }
                            }
                            // Follow the new thread only from the draft that launched it.
                            (Some(thread_id), false) if here => {
                                // The draft is used up; the project's next draft starts fresh.
                                this.draft_workspace = Default::default();
                                // It launched on the draft's environment; the shell may not list it yet.
                                let environment = origin.environment().clone();
                                this.select_thread_in(environment, thread_id, window, cx);
                            }
                            _ if new_draft_after && here => {
                                if let Some(project_id) = project_id {
                                    this.start_draft(project_id, window, cx);
                                }
                            }
                            _ => {}
                        }
                    }
                    Err(error) => {
                        this.pending_sends.retain(|send| Some(&send.message_id) != pending_id.as_ref());
                        this.set_error_for(&origin, error.into());
                        if !text.is_empty() {
                            this.give_back_context_records(&origin, sent_records);
                            this.give_back_prompt(&origin, text, window, cx);
                        }
                        if this.target.as_ref() == Some(&origin) {
                            this.attachments.extend(sent_attachments);
                        } else if let Some(ComposerDraft::Prompt(_, kept, _)) = this.composer_drafts.get_mut(&origin) {
                            kept.extend(sent_attachments);
                        }
                    }
                }
                this.refresh_rows(cx);
                cx.notify();
            });
        })
        .detach();
        self.refresh_rows(cx);
        cx.notify();
    }

    /// The open thread can pick up where it stopped (`deriveResumableRunId`, held queue).
    pub(super) fn can_resume(&self) -> bool {
        matches!(self.target, Some(Target::Thread { .. }))
            && self.open_projection().is_some_and(|p| p.resumable_run_id().is_some() || p.has_held_queued_runs())
    }

    /// `useComposerResume`: "Continue where you left off." on the resumable run, then the
    /// held queue; with nothing to continue, only the queue.
    pub(super) fn resume_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Target::Thread { id: thread_id, .. }) = self.target.clone() else { return };
        let (Some(client), Some(projection)) = (self.client(), self.open_projection()) else { return };
        if self.sending || self.target_rewinding() {
            return;
        }
        let run = projection.resumable_run_id().map(str::to_owned);
        let held = projection.has_held_queued_runs();
        let queue_resume = || json!({"type": "queue.resume", "commandId": new_id(), "threadId": thread_id});
        let mut commands = Vec::new();
        match run {
            None if !held => return,
            None => commands.push(queue_resume()),
            Some(run) => {
                let (_, mode) = self.interaction_mode();
                commands.extend(self.interaction_mode_command(&thread_id, mode));
                let mut message = json!({
                    "type": "message.dispatch",
                    "commandId": new_id(),
                    "createdBy": "user",
                    "creationSource": "web",
                    "threadId": thread_id,
                    "messageId": new_id(),
                    "manualContinuationOfRunId": run,
                    "text": "Continue where you left off.",
                    "attachments": [],
                    "dispatchMode": {"type": "start_immediately"},
                });
                if let Some(model) = &self.model_override {
                    message["modelSelection"] = model.to_json();
                }
                commands.push(message);
                if held {
                    commands.push(queue_resume());
                }
            }
        }
        self.sending = true;
        self.error = None;
        let origin = self.target.clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut result = Ok(Value::Null);
            for command in commands {
                result = client.request("orchestration.dispatchCommand", command).await;
                if result.is_err() {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                if let (Err(error), Some(origin)) = (result, &origin) {
                    this.set_error_for(origin, format!("Could not resume thread. {error}").into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_up_behavior_picks_the_default_and_the_alternate_inverts_it() {
        let (steer, queue) =
            ((json!({"type": "start_immediately"}), Some("steer")), (json!({"type": "queue_after_active"}), None));
        assert_eq!(
            dispatch_fields(false, queues("queue", Intent::Alternate, true)),
            (json!({"type": "start_immediately"}), Some("auto"))
        );
        assert_eq!(dispatch_fields(true, queues("steer", Intent::Foreground, true)), steer);
        assert_eq!(dispatch_fields(true, queues("steer", Intent::Alternate, true)), queue);
        assert_eq!(dispatch_fields(true, queues("steer", Intent::AndNewThread, true)), steer);
        assert_eq!(dispatch_fields(true, queues("queue", Intent::Foreground, true)), queue);
        assert_eq!(dispatch_fields(true, queues("queue", Intent::Alternate, true)), steer);
        assert_eq!(dispatch_fields(true, queues("steer", Intent::Foreground, false)), queue, "compaction queues");
        assert_eq!(dispatch_fields(true, queues("queue", Intent::Alternate, false)), queue);
    }

    #[test]
    fn a_thread_message_carries_what_the_server_reads() {
        fn message<'a>(model: Option<&'a ModelSelection>, source_plan: Option<&'a str>) -> ThreadMessage<'a> {
            ThreadMessage {
                thread_id: "t1",
                text: "hi".into(),
                attachments: vec![json!({"id": "a1"})],
                context: Some(json!({"version": 1, "records": []})),
                dispatch: dispatch_fields(true, queues("steer", Intent::Foreground, true)),
                model,
                source_plan,
                fresh_context: false,
            }
        }
        let without_ids = |mut command: Value| {
            let object = command.as_object_mut().unwrap();
            assert!(object.remove("commandId").is_some_and(|id| id.is_string()));
            assert!(object.remove("messageId").is_some_and(|id| id.is_string()));
            command
        };
        assert_eq!(
            without_ids(message(None, None).command()),
            json!({
                "type": "message.dispatch", "createdBy": "user", "creationSource": "web", "threadId": "t1",
                "text": "hi", "attachments": [{"id": "a1"}], "dispatchMode": {"type": "start_immediately"},
                "deliveryIntent": "steer", "context": {"version": 1, "records": []},
            })
        );
        let model = ModelSelection { instance_id: "codex".into(), model: "gpt".into(), options: None };
        let command = message(Some(&model), Some("p1")).command();
        assert_eq!(command["modelSelection"], model.to_json());
        assert_eq!(command["sourcePlanRef"], json!({"threadId": "t1", "planId": "p1"}));
        let queued = ThreadMessage {
            dispatch: dispatch_fields(true, queues("queue", Intent::Foreground, true)),
            ..message(None, None)
        };
        let queued = queued.command();
        assert!(queued.get("deliveryIntent").is_none(), "a queued message has no delivery intent");
        assert!(queued.get("freshContext").is_none());
        let fresh = ThreadMessage { fresh_context: true, ..message(None, None) };
        assert_eq!(fresh.command()["freshContext"], true);
    }

    #[test]
    fn a_sent_message_shows_until_its_thread_does() {
        let projection = |items: Value, runs: Value| {
            let projection = json!({"thread": {"id": "t", "title": "t"}, "runs": runs, "turnItems": items});
            let mut state = crate::model::ThreadState::default();
            state.apply(&json!({"kind": "snapshot", "snapshotSequence": 1, "projection": projection})).unwrap();
            state.projection.unwrap()
        };
        let send = |thread: &str, id: &str, queued: bool| PendingSend {
            thread_id: thread.into(),
            message_id: id.into(),
            text: "hi".into(),
            created_at: "now".into(),
            queued,
        };
        let mut pending = vec![send("t", "m1", false), send("t", "m2", true), send("other", "m3", false)];
        let empty = projection(json!([]), json!([]));
        let (sent, queued) = pending_rows(&mut pending, "t", &empty);
        assert!(matches!(sent.as_slice(), [Row::User { id, .. }] if id.as_ref() == "optimistic:m1"));
        assert!(matches!(queued.as_slice(), [Row::Queued { message, .. }] if message.message_id.as_ref() == "m2"));

        let landed = projection(
            json!([{"id": "i1", "type": "user_message", "threadId": "t", "messageId": "m1", "ordinal": 1}]),
            json!([{"id": "r2", "status": "queued", "ordinal": 2, "userMessageId": "m2"}]),
        );
        let (sent, queued) = pending_rows(&mut pending, "t", &landed);
        assert!(sent.is_empty() && queued.is_empty(), "both landed");
        assert_eq!(pending[0].message_id, "m3", "another thread's send waits for that thread");
    }

    #[test]
    fn send_shortcut_decides_whether_enter_needs_a_modifier() {
        let key = |text: &'static str| Keystroke::parse(text).unwrap();
        assert!(sends_on(&key("enter"), requires_modifier("enter", "a\nb")));
        assert!(sends_on(&key("enter"), requires_modifier("mod-enter-multiline", "one line")));
        assert!(!sends_on(&key("enter"), requires_modifier("mod-enter-multiline", "a\nb")));
        assert!(sends_on(&key("ctrl-enter"), requires_modifier("mod-enter-multiline", "a\nb")));
        assert!(!sends_on(&key("enter"), requires_modifier("mod-enter", "one line")));
        assert!(sends_on(&key("ctrl-enter"), requires_modifier("mod-enter", "one line")));
        assert!(!sends_on(&key("shift-enter"), false));
        assert!(!sends_on(&key("ctrl-shift-enter"), true));
    }
}
