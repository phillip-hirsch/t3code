//! Sending the composer (`useComposerSend.ts`, `startThreadTurn` in
//! `packages/client-runtime/src/operations/commands.ts`): `message.dispatch`
//! for a thread, `orchestration.launchThread` for a draft.

use gpui_kit::{Context, Keystroke, Window};
use serde_json::{Value, json};

use super::attachments::ATTACHMENT_ONLY_PROMPT;
use super::composer::ComposerDraft;
use super::composer_plan;
use super::{Target, Workspace, draft_title, new_id};
use crate::model::ModelSelection;
use crate::new_thread::{self, DraftWorkspace};

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
/// alternate path the other of steer and queue.
pub(super) fn queues(follow_up: &str, intent: Intent) -> bool {
    (follow_up == "queue") != (intent == Intent::Alternate)
}

/// `message.dispatch`'s delivery fields.
fn dispatch_fields(running: bool, follow_up: &str, intent: Intent) -> (Value, Option<&'static str>) {
    match (running, queues(follow_up, intent)) {
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
        command
    }
}

impl Workspace {
    /// `followUpBehavior`, except that a send declines a waiting approval, so it steers.
    pub(super) fn follow_up_behavior(&self) -> &str {
        let projection = self.thread.as_ref().and_then(|t| t.state.projection.as_ref());
        if projection.is_some_and(|p| !p.pending_approvals().is_empty()) {
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
        let (plan_ui, interaction_mode) = self.interaction_mode();
        // `/plan` or `/default` alone switches modes without sending.
        if let Some(mode) =
            composer_plan::standalone_slash_command(&text).filter(|_| plan_ui && !self.has_attachments())
        {
            self.set_composer_text("", window, cx);
            self.set_interaction_mode(mode, window, cx);
            return;
        }
        let follow_up = self.plan_follow_up_target();
        if (text.is_empty() && !self.has_attachments() && follow_up.is_none())
            || self.sending
            || self.target_rewinding()
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
        let mut background = false;
        // Thread settings the message runs with, applied right before it.
        let mut settings_commands = Vec::new();
        let mut sent_mode = None;
        let (method, mut payload, new_thread_id, workspace) = match target {
            Target::Thread { id: thread_id, .. } => {
                // A plan follow-up refines in plan mode, or implements in build mode (`resolvePlanFollowUpSubmission`).
                let (message_text, interaction_mode) = match &follow_up {
                    Some((_, markdown)) => composer_plan::plan_follow_up(&text, markdown),
                    None => (message_text, interaction_mode),
                };
                settings_commands.extend(self.interaction_mode_command(&thread_id, interaction_mode));
                sent_mode = Some(interaction_mode);
                let running = self
                    .thread
                    .as_ref()
                    .and_then(|t| t.state.projection.as_ref())
                    .is_some_and(super::super::model::ThreadProjection::is_running);
                let command = ThreadMessage {
                    thread_id: &thread_id,
                    text: message_text,
                    attachments,
                    context,
                    dispatch: dispatch_fields(running, self.follow_up_behavior(), intent),
                    model: self.model_override.as_ref(),
                    source_plan: follow_up
                        .as_ref()
                        .filter(|_| interaction_mode == "default")
                        .map(|(plan_id, _)| plan_id.as_str()),
                }
                .command();
                ("orchestration.dispatchCommand", command, None, None)
            }
            Target::Draft { project_id, .. } => {
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
        self.sending = true;
        self.set_composer_text("", window, cx);
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
                        if let Some(mode) = sent_mode {
                            this.remember_interaction_mode(origin.clone(), mode);
                        }
                        this.record_prompt_sent(window, cx);
                        let here = this.target.as_ref() == Some(&origin);
                        match (new_thread_id, background) {
                            // A background launch leaves a fresh draft in its place.
                            (Some(_), true) if here => {
                                if let Some(project_id) = project_id {
                                    this.draft_workspace = Default::default();
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
                        this.error = Some(error.into());
                        if !text.is_empty() {
                            this.give_back_prompt(&origin, text, window, cx);
                        }
                        if this.target.as_ref() == Some(&origin) {
                            this.attachments.extend(sent_attachments);
                        } else if let Some(ComposerDraft::Prompt(_, kept)) = this.composer_drafts.get_mut(&origin) {
                            kept.extend(sent_attachments);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
                if let Err(error) = result {
                    this.error = Some(format!("Could not resume thread. {error}").into());
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
            dispatch_fields(false, "queue", Intent::Alternate),
            (json!({"type": "start_immediately"}), Some("auto"))
        );
        assert_eq!(dispatch_fields(true, "steer", Intent::Foreground), steer);
        assert_eq!(dispatch_fields(true, "steer", Intent::Alternate), queue);
        assert_eq!(dispatch_fields(true, "steer", Intent::AndNewThread), steer);
        assert_eq!(dispatch_fields(true, "queue", Intent::Foreground), queue);
        assert_eq!(dispatch_fields(true, "queue", Intent::Alternate), steer);
    }

    #[test]
    fn a_thread_message_carries_what_the_server_reads() {
        let without_ids = |mut command: Value| {
            let object = command.as_object_mut().unwrap();
            assert!(object.remove("commandId").is_some_and(|id| id.is_string()));
            assert!(object.remove("messageId").is_some_and(|id| id.is_string()));
            command
        };
        fn message<'a>(model: Option<&'a ModelSelection>, source_plan: Option<&'a str>) -> ThreadMessage<'a> {
            ThreadMessage {
                thread_id: "t1",
                text: "hi".into(),
                attachments: vec![json!({"id": "a1"})],
                context: Some(json!({"version": 1, "records": []})),
                dispatch: dispatch_fields(true, "steer", Intent::Foreground),
                model,
                source_plan,
            }
        }
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
        let queued =
            ThreadMessage { dispatch: dispatch_fields(true, "queue", Intent::Foreground), ..message(None, None) };
        assert!(queued.command().get("deliveryIntent").is_none(), "a queued message has no delivery intent");
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
