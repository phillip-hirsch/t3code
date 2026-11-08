//! "Link pull request" (`LinkPullRequestDialog.tsx`): attaches a pull request to a thread by its
//! URL, or by its number for the thread's repository (`resolveLinkPullRequestInput`).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Window,
    div, px,
};
use serde_json::json;

use super::{Workspace, new_id};
use crate::model::RepositoryIdentity;
use crate::pull_request_detail::parse_change_request_url;
use crate::rpc::RpcClient;
use crate::theme;

/// The source control host a project's pull requests live on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Host {
    GitHub,
    GitLab,
    Unknown,
}

/// `pullRequestLinkProject`: the thread project's repository, so a bare number resolves.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LinkProject {
    host: String,
    repository: String,
    kind: Host,
}

impl LinkProject {
    pub(super) fn of(identity: &RepositoryIdentity) -> Option<Self> {
        let key = identity.canonical_key.as_deref()?;
        let (host, path) = key.split_once('/')?;
        let host = host.trim().to_lowercase();
        let repository = identity.display_name.clone().unwrap_or_else(|| path.trim_end_matches(".git").to_owned());
        if host.is_empty() || repository.is_empty() {
            return None;
        }
        let kind = if host == "github.com" || host.ends_with(".ghe.com") || host.split('.').any(|p| p == "github") {
            Host::GitHub
        } else if host.split('.').any(|p| p == "gitlab") {
            Host::GitLab
        } else {
            Host::Unknown
        };
        Some(Self { host, repository, kind })
    }

    fn singular(&self) -> &'static str {
        if self.kind == Host::GitLab { "merge request" } else { "pull request" }
    }

    /// `changeRequestUrl` with no remote or web URL: the host's own route for `number`.
    fn web_url(&self, number: u64) -> Option<String> {
        let (host, repository) = (&self.host, &self.repository);
        match self.kind {
            Host::GitHub => {
                let segment = |s: &str| {
                    !s.is_empty()
                        && s != "."
                        && s != ".."
                        && s.chars().all(|c| c.is_alphanumeric() || "_.-".contains(c))
                };
                let parts: Vec<&str> = repository.split('/').collect();
                (parts.len() == 2 && parts.iter().all(|p| segment(p)))
                    .then(|| format!("https://{host}/{repository}/pull/{number}"))
            }
            Host::GitLab => Some(format!("https://{host}/{repository}/-/merge_requests/{number}")),
            Host::Unknown => None,
        }
    }
}

/// A pull request the dialog will link.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ResolvedLink {
    pub(super) host: String,
    pub(super) repository: String,
    pub(super) number: u64,
    pub(super) url: String,
}

/// `parsePullRequestReference`: a checkout command's argument, a pull request URL, or `123` / `#123`.
fn parse_reference(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let checkout = |command: [&str; 3]| {
        let mut words = trimmed.splitn(4, char::is_whitespace).filter(|w| !w.is_empty());
        let matches = command.iter().all(|word| words.next().is_some_and(|w| w.eq_ignore_ascii_case(word)));
        let rest = trimmed.split_whitespace().skip(3).collect::<Vec<_>>().join(" ");
        (matches && !rest.is_empty()).then_some(rest)
    };
    let normalized = checkout(["gh", "pr", "checkout"])
        .or_else(|| checkout(["glab", "mr", "checkout"]))
        .unwrap_or_else(|| trimmed.to_owned());
    if normalized.is_empty() {
        return None;
    }
    if parse_change_request_url(&normalized).is_some() {
        return Some(normalized);
    }
    let digits = normalized.strip_prefix('#').unwrap_or(&normalized);
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.to_owned())
}

/// `resolveLinkPullRequestInput`: `None` when the input reads as no pull request at all.
pub(super) fn resolve(reference: &str, project: Option<&LinkProject>) -> Option<Result<ResolvedLink, String>> {
    let trimmed = reference.trim();
    let parsed =
        if parse_change_request_url(trimmed).is_some() { trimmed.to_owned() } else { parse_reference(reference)? };
    if let Some((host, repository, number)) = parse_change_request_url(&parsed) {
        return Some(Ok(ResolvedLink { host, repository, number, url: parsed }));
    }
    let number: u64 = parsed.parse().ok().filter(|n| (1..=9_007_199_254_740_991).contains(n))?;
    let Some(project) = project else {
        return Some(Err(
            "Paste the full pull request URL; this thread has no repository to resolve the number against.".into(),
        ));
    };
    let resolved = project.web_url(number).and_then(|url| {
        let (host, repository, number) = parse_change_request_url(&url)?;
        Some(ResolvedLink { host, repository, number, url })
    });
    Some(
        resolved
            .ok_or_else(|| format!("Paste a full URL; this project's host has no known {} URL.", project.singular())),
    )
}

/// What the dialog says under the input once it has been typed in.
fn validation(reference: &str, resolved: Option<&Result<ResolvedLink, String>>) -> Option<String> {
    if reference.trim().is_empty() {
        return Some("Paste a pull request URL or enter 123 / #123.".into());
    }
    match resolved {
        None => Some("Use a pull request URL, 123, or #123.".into()),
        Some(Err(error)) => Some(error.clone()),
        Some(Ok(_)) => None,
    }
}

struct LinkForm {
    input: gpui_kit::Entity<InputState>,
    client: RpcClient,
    thread_id: String,
    project: Option<LinkProject>,
    dirty: bool,
    pending: bool,
    submit_error: Option<String>,
    _subscription: Subscription,
}

impl LinkForm {
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dirty = true;
        cx.notify();
        let reference = self.input.read(cx).value().to_string();
        let Some(Ok(link)) = resolve(&reference, self.project.as_ref()) else { return };
        if self.pending {
            return;
        }
        self.submit_error = None;
        self.pending = true;
        let command = json!({
            "type": "thread.pull-request.link", "commandId": new_id(), "threadId": self.thread_id,
            "host": link.host, "repository": link.repository, "number": link.number, "url": link.url,
            "source": "manual",
        });
        let client = self.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending = false;
                match result {
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => this.submit_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for LinkForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let reference = self.input.read(cx).value().to_string();
        let resolved = resolve(&reference, self.project.as_ref());
        let error =
            self.dirty.then(|| validation(&reference, resolved.as_ref())).flatten().or(self.submit_error.clone());
        let preview = match &resolved {
            Some(Ok(link)) => Some(format!("{}/{} #{}", link.host, link.repository, link.number)),
            _ => None,
        };
        let ready = matches!(resolved, Some(Ok(_)));
        v_flex()
            .gap(px(16.))
            .child(div().text_sm().text_color(t.muted_foreground).child(
                "Attach a pull request to this thread by its URL, or by its number for this thread's repository.",
            ))
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(Input::new(&self.input))
                    .children(preview.map(|text| div().truncate().text_xs().text_color(t.muted_foreground).child(text)))
                    .children(error.map(|text| div().text_xs().text_color(t.error).child(text))),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("link-pull-request-cancel")
                            .label("Cancel")
                            .outline()
                            .small()
                            .disabled(self.pending)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("link-pull-request-submit")
                            .label(if self.pending { "Linking..." } else { "Link" })
                            .primary()
                            .small()
                            .disabled(self.pending || !ready)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

impl Workspace {
    /// Pull requests link to threads where the environment reports capabilities
    /// (`threadPullRequestLinkMode`).
    pub(super) fn pull_request_linking(&self, thread_id: &str) -> bool {
        let environment = self.thread_environment(thread_id);
        environment.and_then(|e| self.env_info(&e)).is_some_and(|info| !info.capabilities.is_null())
    }

    /// `openLinkPullRequestDialog` for `thread_id`.
    pub(super) fn open_link_pull_request(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.thread_client(&thread_id) else { return };
        let shell = self.shell.threads.iter().find(|t| t.id == thread_id);
        let identity = shell
            .and_then(|t| self.shell.projects.iter().find(|p| p.id == t.project_id))
            .and_then(|p| p.repository_identity.as_ref());
        let project = identity.and_then(LinkProject::of);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Pull request URL or #42"));
        input.update(cx, |input, cx| input.focus(window, cx));
        let form = cx.new(|cx| LinkForm {
            _subscription: cx.subscribe_in(&input, window, |this: &mut LinkForm, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => this.submit(window, cx),
                    InputEvent::Change => {
                        this.dirty = true;
                        cx.notify();
                    }
                    _ => {}
                }
            }),
            input,
            client,
            thread_id,
            project,
            dirty: false,
            pending: false,
            submit_error: None,
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(SharedString::from("Link pull request")).w(px(576.)).child(form.clone())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(key: &str) -> LinkProject {
        let identity: RepositoryIdentity = serde_json::from_value(json!({"canonicalKey": key})).unwrap();
        LinkProject::of(&identity).unwrap()
    }

    fn link(host: &str, repository: &str, number: u64, url: &str) -> ResolvedLink {
        ResolvedLink { host: host.into(), repository: repository.into(), number, url: url.into() }
    }

    #[test]
    fn links_a_url_a_checkout_command_or_a_number_for_the_project() {
        let github = project("github.com/acme/app");
        let url = "https://github.com/acme/app/pull/7";
        assert_eq!(resolve(&format!("  {url}  "), None), Some(Ok(link("github.com", "acme/app", 7, url))));
        assert_eq!(resolve(&format!("gh pr checkout {url}"), None), Some(Ok(link("github.com", "acme/app", 7, url))));
        assert_eq!(
            resolve("#42", Some(&github)),
            Some(Ok(link("github.com", "acme/app", 42, "https://github.com/acme/app/pull/42")))
        );
        assert_eq!(resolve("gh pr checkout 3", Some(&github)), resolve("3", Some(&github)));
        let gitlab = project("gitlab.com/group/sub/app");
        assert_eq!(
            resolve("12", Some(&gitlab)),
            Some(Ok(link("gitlab.com", "group/sub/app", 12, "https://gitlab.com/group/sub/app/-/merge_requests/12")))
        );
    }

    #[test]
    fn says_why_a_reference_cannot_link() {
        assert_eq!(resolve("not a pr", None), None);
        assert_eq!(resolve("#0", Some(&project("github.com/acme/app"))), None);
        assert!(matches!(resolve("42", None), Some(Err(e)) if e.contains("has no repository")));
        assert!(matches!(
            resolve("42", Some(&project("git.example.com/acme/app"))),
            Some(Err(e)) if e == "Paste a full URL; this project's host has no known pull request URL."
        ));
        assert_eq!(validation(" ", None).as_deref(), Some("Paste a pull request URL or enter 123 / #123."));
        assert_eq!(validation("x", None).as_deref(), Some("Use a pull request URL, 123, or #123."));
    }
}
