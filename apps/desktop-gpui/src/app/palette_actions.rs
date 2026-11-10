//! What the palette's thread and project actions run (`CommandPalette.tsx`): a thread without
//! a project (`useScratchProject`), triage (`useStartTriage`), prompt links (`copyPromptLink.ts`),
//! copying the thread or its pull request's link (`threadReference.ts`), and restarting the agent session.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::{ClipboardItem, Context, SharedString, Window};
use serde_json::json;

use super::{Target, Workspace, new_id};
use crate::model::RepositoryIdentity;
use crate::thread_pull_requests::ReferenceCopy;

/// `TRIAGE_CLI_COMMAND`: triage from a terminal on the machine.
pub(super) const TRIAGE_CLI: &str = "npx t3 triage";

/// `isTriagePrompt`: a draft is a triage draft while its message still links the playbook file
/// (`TRIAGE_PROMPT_FILE_NAME`).
pub(super) fn is_triage_prompt(prompt: &str) -> bool {
    prompt.contains("[triage-playbook.md](")
}

/// What `thread.copyReference` copies.
pub(super) enum CopyTarget {
    PullRequest(String),
    Thread { thread_id: String, title: String },
}

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
    /// `startScratchThread`: `projects.ensureScratch` on `environment`, then a draft there,
    /// holding `prompt` if given and the draft has no user content.
    pub(super) fn start_scratch_thread(
        &mut self,
        environment: SharedString,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
                // Never over what the user wrote: the draft may already hold some.
                if let Some(prompt) = prompt.filter(|_| !this.composer_has_user_content(cx)) {
                    this.set_composer_text(prompt, window, cx);
                }
            });
        })
        .detach();
    }

    /// `useStartTriage`: the server prepares a triage session, then a No project draft opens
    /// with its playbook linked.
    pub(super) fn start_triage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let fail = |message: &str| format!("{message} Run {TRIAGE_CLI} in a terminal on that machine instead.");
        if self.server_info().scratch_workspace_root.is_none() {
            toast_error(
                "Could not start triage",
                fail("This machine cannot start threads without a project."),
                window,
                cx,
            );
            return;
        }
        let (Some(environment), Some(client)) = (self.active_environment().cloned(), self.client()) else { return };
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
                    this.start_scratch_thread(environment, prompt, window, cx);
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
            let project = self.current_project();
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

    /// What `thread.copyReference` copies (`resolveThreadReferenceCopyTarget`): the pull request
    /// open in the panel or the thread's current one, else the open thread; `None` while the
    /// panel's pull request URL loads, or with no thread.
    pub(super) fn reference_copy_target(&self) -> Option<CopyTarget> {
        let open_panel = self.open_panel_pull_request_url();
        if self.page == Some(super::pages::Page::PullRequests) {
            return open_panel.flatten().map(CopyTarget::PullRequest);
        }
        let key = self.target.as_ref().and_then(Target::thread_key)?;
        let (id, shell) = (&key.id, self.shell.thread(&key));
        let empty = Vec::new();
        let copy = crate::thread_pull_requests::reference_copy(
            open_panel.as_ref().map(Option::as_deref),
            shell.map_or(&empty, |t| &t.pull_requests),
            shell.and_then(|t| t.linked_pull_request.as_ref()),
            shell.and_then(|t| t.branch_pull_request.as_ref()),
        )?;
        Some(match copy {
            ReferenceCopy::PullRequest(url) => CopyTarget::PullRequest(url.to_owned()),
            ReferenceCopy::Thread => {
                CopyTarget::Thread { thread_id: id.clone(), title: shell.map(|t| t.title.clone()).unwrap_or_default() }
            }
        })
    }

    /// `thread.copyReference`: the pull request's link, or a link that opens the thread from
    /// anywhere (`threadChipCopyTarget`).
    pub(super) fn copy_thread_reference(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let note = match self.reference_copy_target() {
            Some(CopyTarget::PullRequest(url)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                Notification::success(url).title("PR link copied")
            }
            Some(CopyTarget::Thread { thread_id, title }) => {
                let Some(environment_id) = self.server_info().environment_id.as_deref() else { return };
                let link = super::thread_menu::hosted_thread_link(environment_id, &thread_id);
                cx.write_to_clipboard(ClipboardItem::new_string(link));
                Notification::success(title).title("Thread copied")
            }
            None => return,
        };
        window.push_notification(note, cx);
    }

    /// "Restart agent session": detaches the thread's provider sessions, so the next message
    /// starts a fresh one, then refreshes the provider for its checkout.
    pub(super) fn restart_agent_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.target.as_ref().and_then(Target::thread_key) else { return };
        let thread_id = key.id.clone();
        let Some(client) = self.client() else { return };
        let projection = self.thread.as_ref().and_then(|t| t.state.projection.as_ref());
        let sessions: Vec<String> =
            projection.map(|p| p.provider_sessions.iter().map(|s| s.id.clone()).collect()).unwrap_or_default();
        let shell = self.shell.thread(&key);
        let project = shell.and_then(|t| self.shell.project(&crate::model::ScopedId::thread_project(t)));
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
                        toast_error("Could not restart the agent session", error.to_string(), window, cx);
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
                    toast_error("Could not restart the agent session", error.to_string(), window, cx);
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
    fn a_draft_is_triage_while_it_links_the_playbook() {
        assert!(is_triage_prompt("Read [triage-playbook.md](/tmp/t3/triage-playbook.md) first."));
        assert!(!is_triage_prompt("Read triage-playbook.md first."), "a bare name is not the link");
        assert!(!is_triage_prompt(""));
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
