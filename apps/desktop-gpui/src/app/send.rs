//! Sending the composer (`useComposerSend.ts`, `startThreadTurn` in
//! `packages/client-runtime/src/operations/commands.ts`): `message.dispatch`
//! for a thread, `orchestration.launchThread` for a draft.

use gpui_kit::{Context, Window};
use serde_json::{Value, json};

use super::attachments::ATTACHMENT_ONLY_PROMPT;
use super::composer::ComposerDraft;
use super::{Target, Workspace, draft_title, new_id};
use crate::new_thread::{self, DraftWorkspace};

/// What a send shortcut asks for (`composer-logic.ts`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Intent {
    /// Enter or a plain click.
    Foreground,
    /// Ctrl+Enter or Ctrl-click: queue instead of steer while a turn runs;
    /// in a draft, launch in the background and stay on a fresh draft.
    Alternate,
    /// Ctrl+Alt+Enter: send, then open a new draft (background in a draft).
    AndNewThread,
}

/// `message.dispatch`'s delivery fields (`resolveComposerDispatchMode`, steer by default).
fn dispatch_fields(running: bool, intent: Intent) -> (Value, Option<&'static str>) {
    match (running, intent) {
        (false, _) => (json!({"type": "start_immediately"}), Some("auto")),
        (true, Intent::Alternate) => (json!({"type": "queue_after_active"}), None),
        (true, _) => (json!({"type": "start_immediately"}), Some("steer")),
    }
}

impl Workspace {
    pub(super) fn submit_with(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        if self.question.is_some() {
            self.advance_question(window, cx);
            return;
        }
        let text = self.composer.read(cx).value().trim().to_owned();
        if (text.is_empty() && !self.has_attachments()) || self.sending {
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
        let (method, mut payload, new_thread_id, workspace) = match target {
            Target::Thread(thread_id) => {
                let running =
                    self.thread.as_ref().and_then(|t| t.state.projection.as_ref()).is_some_and(|p| p.is_running());
                let (dispatch_mode, delivery) = dispatch_fields(running, intent);
                let mut command = json!({
                    "type": "message.dispatch",
                    "commandId": new_id(),
                    "createdBy": "user",
                    "creationSource": "web",
                    "threadId": thread_id,
                    "messageId": new_id(),
                    "text": message_text,
                    "attachments": attachments,
                    "dispatchMode": dispatch_mode,
                });
                if let Some(delivery) = delivery {
                    command["deliveryIntent"] = delivery.into();
                }
                // Omitted, the server keeps the thread's own model.
                if let Some(model) = &self.model_override {
                    command["modelSelection"] = model.to_json();
                }
                if let Some(context) = context.clone() {
                    command["context"] = context;
                }
                ("orchestration.dispatchCommand", command, None, None)
            }
            Target::Draft { project_id } => {
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
                    Some((DraftWorkspace::new(&self.server_info, project, has_threads), project.workspace_root.clone()))
                });
                let thread_id = self.terminals.take_draft_thread(&project_id).unwrap_or_else(new_id);
                let mut initial = json!({"messageId": new_id(), "text": message_text, "attachments": attachments});
                if let Some(context) = context.clone() {
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
                    "interactionMode": "default",
                    "workspaceStrategy": strategy.unwrap_or_else(|| json!({"type": "root"})),
                    "initialMessage": initial,
                });
                background = intent != Intent::Foreground;
                ("orchestration.launchThread", input, Some(thread_id), workspace.flatten())
            }
        };
        let new_draft_after = intent == Intent::AndNewThread && matches!(origin, Target::Thread(_));
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
                                this.select_thread(thread_id, window, cx);
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
                        this.give_back_prompt(&origin, text, window, cx);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_turns_steer_unless_ctrl_queues() {
        assert_eq!(dispatch_fields(false, Intent::Alternate), (json!({"type": "start_immediately"}), Some("auto")));
        assert_eq!(dispatch_fields(true, Intent::Foreground), (json!({"type": "start_immediately"}), Some("steer")));
        assert_eq!(dispatch_fields(true, Intent::Alternate), (json!({"type": "queue_after_active"}), None));
        assert_eq!(dispatch_fields(true, Intent::AndNewThread), (json!({"type": "start_immediately"}), Some("steer")));
    }
}
