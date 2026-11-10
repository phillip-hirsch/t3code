//! `DesktopAppActivationCoordinator` and `handleDesktopAppActivationRequest`: a `t3 app`
//! request ([`crate::desktop::app_control`]) opens its folder's project on the primary
//! environment, adding it when missing and unarchiving it when archived, in a new thread.

use std::collections::VecDeque;

use gpui_kit::{Context, SharedString, Window};
use serde_json::{Value, json};

use super::{Workspace, new_id};
use crate::desktop::app_control::{ErrorCode, Incoming, Outcome};

/// Requests wait here until the primary environment has listed its projects, and run one at
/// a time, as the broker dispatches them.
#[derive(Default)]
pub(super) struct ActivationQueue {
    waiting: VecDeque<Incoming>,
    busy: bool,
}

fn failed(code: ErrorCode, message: impl Into<String>) -> Outcome {
    Outcome::Failed { code, message: message.into() }
}

impl Workspace {
    pub(super) fn receive_activation(&mut self, incoming: Incoming, window: &mut Window, cx: &mut Context<Self>) {
        self.activation.waiting.push_back(incoming);
        self.process_activations(window, cx);
    }

    /// Runs the next request once the primary environment is connected and listed.
    pub(super) fn process_activations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ready = self.landed
            && self.primary_environment().is_some_and(|primary| {
                self.env_client(primary).is_some() && self.env_info(primary).is_some_and(|i| i.platform_os.is_some())
            });
        if self.activation.busy || !ready {
            return;
        }
        while let Some(incoming) = self.activation.waiting.pop_front() {
            if incoming.is_cancelled() {
                continue;
            }
            self.activation.busy = true;
            self.open_workspace(incoming, window, cx);
            return;
        }
    }

    fn finish_activation(&mut self, incoming: Incoming, outcome: Outcome, window: &mut Window, cx: &mut Context<Self>) {
        incoming.finish(outcome);
        self.activation.busy = false;
        self.process_activations(window, cx);
    }

    fn open_workspace(&mut self, incoming: Incoming, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.primary_environment().cloned().and_then(|environment| {
            let client = self.env_client(&environment)?;
            let os = self.env_info(&environment)?.platform_os.clone()?;
            Some((environment, client, os))
        });
        let Some((environment, client, os)) = target else {
            let message = "The desktop app's primary local environment is not connected.";
            return self.finish_activation(incoming, failed(ErrorCode::EnvironmentUnavailable, message), window, cx);
        };
        let requested = incoming.request.platform.environment_os();
        if requested != os {
            let message = format!(
                "The command path is for {requested}, but the desktop app's primary environment uses {os}. Cross-platform path mapping is not supported."
            );
            return self.finish_activation(incoming, failed(ErrorCode::PlatformMismatch, message), window, cx);
        }
        let root = incoming.request.workspace_root.clone();
        let key = crate::project_groups::normalize_path(&root);
        let existing = self
            .shell
            .projects
            .iter()
            .find(|p| {
                p.environment_id == environment && crate::project_groups::normalize_path(&p.workspace_root) == key
            })
            .map(|p| (p.id.clone(), p.archived_at.is_some()));
        if let Some((project_id, archived)) = existing {
            return self.open_activation_thread(incoming, environment, project_id, archived, window, cx);
        }
        let project_id = new_id();
        let mutation = json!({
            "type": "project.create",
            "commandId": new_id(),
            "projectId": project_id,
            "title": super::palette::project_title_from_path(&root),
            "workspaceRoot": root,
            "createWorkspaceRootIfMissing": false,
            "defaultModelSelection": Value::Null,
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.mutate", mutation).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.open_activation_thread(incoming, environment, project_id, false, window, cx),
                Err(error) => {
                    let message = Some(error.to_string())
                        .filter(|message| !message.trim().is_empty())
                        .unwrap_or_else(|| "T3 Code could not add the project.".to_owned());
                    this.finish_activation(incoming, failed(ErrorCode::ProjectCreateFailed, message), window, cx);
                }
            });
        })
        .detach();
    }

    /// `openThread`: a new thread in the project; opening an archived project's folder again
    /// brings the project back.
    fn open_activation_thread(
        &mut self,
        incoming: Incoming,
        environment: SharedString,
        project_id: String,
        archived: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if archived {
            let mutation =
                json!({"type": "project.update", "commandId": new_id(), "projectId": project_id, "archived": false});
            self.mutate_project(&environment, mutation, cx);
        }
        self.settings = None;
        self.start_draft_in(environment, project_id.clone(), window, cx);
        // The id the draft's thread launches with.
        let outcome = match self.terminal_thread(cx) {
            Some(thread_id) => Outcome::Opened { project_id, thread_id },
            None => failed(ErrorCode::ThreadOpenFailed, "T3 Code could not open a new thread for the project."),
        };
        self.finish_activation(incoming, outcome, window, cx);
    }
}
