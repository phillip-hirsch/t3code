//! What the palette's thread and project actions run (`CommandPalette.tsx`): a thread without
//! a project (`useScratchProject`), triage (`useStartTriage`), prompt links (`copyPromptLink.ts`),
//! copying the thread (`threadReference.ts`), and restarting the agent session.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{ClipboardItem, Context, Window};
use serde_json::json;

use super::{Target, Workspace, new_id};
use crate::model::RepositoryIdentity;

/// `promptLinkRepositoryOf`: a GitHub repository's `owner/name`.
pub(super) fn github_repository(identity: &RepositoryIdentity) -> Option<(String, String)> {
    let key = identity.origin.as_ref().map(|o| o.canonical_key.as_str()).or(identity.canonical_key.as_deref())?;
    let (owner, name) = key.strip_prefix("github.com/")?.trim().split_once('/')?;
    let name = name.strip_suffix(".git").or_else(|| name.strip_suffix(".GIT")).unwrap_or(name);
    let owner_ok = (1..=39).contains(&owner.len())
        && owner.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let name_ok = (1..=100).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.chars().all(|c| c == '.');
    (owner_ok && name_ok).then(|| (owner.to_owned(), name.to_owned()))
}

/// `URLSearchParams` encoding of one value.

/// `buildNewThreadLink` with its context: the repository, and the model once a provider is known.
pub(super) fn prompt_link_with(
    prompt: &str,
    repository: Option<(String, String)>,
    provider: Option<&str>,
    model: Option<&str>,
) -> Option<String> {
    use super::message_actions::form_encode_into;
    let mut link = super::message_actions::prompt_link(prompt)?;
    if let Some((owner, name)) = repository {
        link.push_str("&repo=");
        form_encode_into(&mut link, &format!("{owner}/{name}"));
    }
    if let Some(provider) = provider {
        link.push_str("&provider=");
        form_encode_into(&mut link, provider);
        if let Some(model) = model {
            link.push_str("&model=");
            form_encode_into(&mut link, model);
        }
    }
    Some(link)
}

fn toast_error(title: &str, message: impl Into<gpui_kit::SharedString>, window: &mut Window, cx: &mut gpui_kit::App) {
    window.push_notification(Notification::error(message).title(title.to_owned()), cx);
}

impl Workspace {
    /// `startScratchThread`: `projects.ensureScratch`, then a draft there, holding `prompt` if given.
    pub(super) fn start_scratch_thread(&mut self, prompt: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.active_environment().cloned() else { return };
        let Some(client) = self.env_client(&environment) else { return };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.ensureScratch", json!({})).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let project_id = match result {
                    Ok(value) => value["projectId"].as_str().map(str::to_owned),
                    Err(error) => {
                        toast_error("Could not start without a project", error.to_string(), window, cx);
                        return;
                    }
                };
                let Some(project_id) = project_id else { return };
                // A scratch project made just now may not be listed yet.
                this.start_draft_in(environment, project_id, window, cx);
                if let Some(prompt) = &prompt {
                    this.set_composer_text(prompt.clone(), window, cx);
                }
            });
        })
        .detach();
    }

    /// `useStartTriage`: the server prepares a triage session, then a No project draft opens
    /// with its playbook linked.
    pub(super) fn start_triage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const CLI: &str = "npx t3 triage";
        let fail = |message: &str| format!("{message} Run {CLI} in a terminal on that machine instead.");
        if self.server_info().scratch_workspace_root.is_none() {
            toast_error(
                "Could not start triage",
                fail("This machine cannot start threads without a project."),
                window,
                cx,
            );
            return;
        }
        let Some(client) = self.client() else { return };
        let os = match std::env::consts::OS {
            "windows" => "Windows",
            "macos" => "macOS",
            _ => "Linux",
        };
        let input = json!({"client": {
            "deviceType": "desktop",
            "surface": "desktop",
            "os": os,
            "appVersion": env!("CARGO_PKG_VERSION"),
        }});
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.prepareTriage", input).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(value) => {
                    let prompt = value["prompt"].as_str().map(str::to_owned);
                    this.start_scratch_thread(prompt, window, cx);
                }
                Err(error) => toast_error("Could not start triage", fail(&error.to_string()), window, cx),
            });
        })
        .detach();
    }

    /// `composerPromptLink` for the composer's text; `with_context` adds the repository and model.
    pub(super) fn copy_prompt_link(&mut self, with_context: bool, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.composer.read(cx).value().to_string();
        let link = if with_context {
            let project_id = match &self.target {
                Some(Target::Thread { id, .. }) => {
                    self.shell.threads.iter().find(|t| &t.id == id).map(|t| &t.project_id)
                }
                Some(Target::Draft { project_id, .. }) => Some(project_id),
                None => None,
            };
            let project = project_id.and_then(|id| self.shell.projects.iter().find(|p| &p.id == id));
            let repository = project.and_then(|p| p.repository_identity.as_ref()).and_then(github_repository);
            let selection = self.composer_model();
            let provider =
                selection.as_ref().and_then(|s| self.server_info().provider(&s.instance_id)).map(|p| p.driver.clone());
            let model = selection.map(|s| s.model);
            prompt_link_with(&prompt, repository, provider.as_deref(), model.as_deref())
        } else {
            super::message_actions::prompt_link(&prompt)
        };
        let Some(link) = link else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(link));
        window.push_notification(Notification::success("").title("Prompt link copied"), cx);
    }

    /// `thread.copyReference`: a link that opens the thread from anywhere (`threadChipCopyTarget`).
    pub(super) fn copy_thread_reference(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Target::Thread { id: thread_id, .. }) = &self.target else { return };
        let Some(environment_id) = self.server_info().environment_id.as_deref() else { return };
        let title = self.shell.threads.iter().find(|t| &t.id == thread_id).map(|t| t.title.clone()).unwrap_or_default();
        let link = super::thread_menu::hosted_thread_link(environment_id, thread_id);
        cx.write_to_clipboard(ClipboardItem::new_string(link));
        window.push_notification(Notification::success(title).title("Thread copied"), cx);
    }

    /// "Restart agent session": detaches the thread's provider sessions, so the next message
    /// starts a fresh one, then refreshes the provider for its checkout.
    pub(super) fn restart_agent_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Target::Thread { id: thread_id, .. }) = self.target.clone() else { return };
        let Some(client) = self.client() else { return };
        let projection = self.thread.as_ref().and_then(|t| t.state.projection.as_ref());
        let sessions: Vec<String> =
            projection.map(|p| p.provider_sessions.iter().map(|s| s.id.clone()).collect()).unwrap_or_default();
        let shell = self.shell.threads.iter().find(|t| t.id == thread_id);
        let project = shell.and_then(|t| self.shell.projects.iter().find(|p| p.id == t.project_id));
        let instance =
            shell.and_then(|t| t.provider_instance_id.clone()).or_else(|| self.composer_model().map(|s| s.instance_id));
        let cwd = shell.and_then(|t| t.worktree_path.clone()).or_else(|| project.map(|p| p.workspace_root.clone()));
        let command_id = new_id();
        cx.spawn_in(window, async move |this, cx| {
            for session in sessions {
                let command = json!({
                    "type": "provider-session.detach",
                    "commandId": format!("{command_id}:detach:{session}"),
                    "threadId": thread_id,
                    "providerSessionId": session,
                    "reason": "client-requested",
                });
                if let Err(error) = client.request("orchestration.dispatchCommand", command).await {
                    let _ = this.update_in(cx, |_, window, cx| {
                        toast_error("Could not restart the agent session", error.to_string(), window, cx)
                    });
                    return;
                }
            }
            let _ = this.update_in(cx, |_, window, cx| {
                let note = Notification::success("Your next message starts a fresh session.")
                    .title("Agent session will restart");
                window.push_notification(note, cx);
            });
            let (Some(instance), Some(cwd)) = (instance, cwd) else { return };
            let input = json!({"instanceId": instance, "cwd": cwd, "fresh": true, "workspaceSnapshotsOnDemand": true});
            if let Err(error) = client.request("server.refreshProviders", input).await {
                let _ = this.update_in(cx, |_, window, cx| {
                    toast_error("Could not restart the agent session", error.to_string(), window, cx)
                });
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(key: &str) -> RepositoryIdentity {
        serde_json::from_value(json!({"canonicalKey": key})).unwrap()
    }

    #[test]
    fn prompt_links_name_github_repositories_only() {
        assert_eq!(
            github_repository(&identity("github.com/pingdotgg/t3code.git")),
            Some(("pingdotgg".into(), "t3code".into()))
        );
        assert_eq!(github_repository(&identity("gitlab.com/a/b")), None);
        assert_eq!(github_repository(&identity("github.com/-bad/x")), None);
        let link = prompt_link_with("fix it", Some(("o".into(), "r".into())), Some("codex"), Some("gpt-5")).unwrap();
        assert_eq!(link, "https://app.t3.codes/new?prompt=fix+it&repo=o%2Fr&provider=codex&model=gpt-5");
        assert_eq!(prompt_link_with("x", None, None, Some("m")).unwrap(), "https://app.t3.codes/new?prompt=x");
    }
}
