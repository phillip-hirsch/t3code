//! Agent handoffs (`PullRequestDetailPanel` `startAsk` / `startHandoff`): Ask and Explain open a
//! thread in the pull request's project with the question in its composer; Check out, Resolve
//! conflicts and Fix findings first prepare a checkout of the pull request
//! (`git.preparePullRequestThread`) and point the new thread at it. Nothing is sent: the user
//! reads the prompt over and sends it.

use super::*;
use crate::app::Target;
use crate::model::ProjectShell;
use crate::new_thread::{EnvMode, WorkspaceChoice};
use crate::pull_request_detail::{CheckoutMode, CheckoutRequest};

/// `PickableEnvironment`: a connected environment holding its own copy of the repository.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Pickable {
    pub(super) project_id: String,
    pub(super) label: SharedString,
    pub(super) machine: String,
}

/// `resolvePickableEnvironments`: the pull request's own project first, then the first project
/// of the same repository on each other environment; none when no other environment has one.
pub(super) fn pickable_environments(
    own_project: &str,
    projects: &[ProjectShell],
    environments: &[(SharedString, SharedString, String)],
) -> Vec<Pickable> {
    let key = |project: &ProjectShell| {
        project.repository_identity.as_ref().and_then(|i| i.canonical_key.as_deref()).map(str::to_lowercase)
    };
    let Some(own) = projects.iter().find(|p| p.id == own_project) else { return Vec::new() };
    let Some(repository) = key(own).filter(|k| !k.is_empty()) else { return Vec::new() };
    let Some((_, own_label, own_machine)) = environments.iter().find(|(id, _, _)| *id == own.environment_id) else {
        return Vec::new();
    };
    let others: Vec<Pickable> = environments
        .iter()
        .filter(|(id, _, _)| *id != own.environment_id)
        .filter_map(|(id, label, machine)| {
            let copy = projects.iter().find(|p| p.environment_id == *id && key(p).as_ref() == Some(&repository))?;
            Some(Pickable { project_id: copy.id.clone(), label: label.clone(), machine: machine.clone() })
        })
        .collect();
    if others.is_empty() {
        return Vec::new();
    }
    let own = Pickable { project_id: own.id.clone(), label: own_label.clone(), machine: own_machine.clone() };
    std::iter::once(own).chain(others).collect()
}

/// `ActOnEnvironmentPicker`: where a handoff runs, below a separator in the menus that start one.
pub(super) fn environment_items(
    mut menu: PopupMenu,
    pickable: &[Pickable],
    acting: &str,
    disabled: bool,
    view: &Entity<Workspace>,
    window: &mut Window,
) -> PopupMenu {
    if pickable.is_empty() {
        return menu;
    }
    menu = menu.separator();
    for environment in pickable {
        let project_id = environment.project_id.clone();
        menu = menu.item(
            PopupMenuItem::new(environment.label.clone())
                .icon(crate::app::host_picker::machine_icon(&environment.machine))
                .checked(environment.project_id == acting)
                .disabled(disabled)
                .on_click(window.listener_for(view, move |this: &mut Workspace, _, _, cx| {
                    this.pick_pr_acting_project(project_id.clone(), cx);
                })),
        );
    }
    menu
}

/// `pullRequestHandoffLabels`: beside a thread's composer the fixes land in that thread.
pub(super) struct FixLabels {
    pub(super) finding: &'static str,
    pub(super) check: &'static str,
    pub(super) findings: &'static str,
}

pub(super) fn fix_labels(in_this_thread: bool) -> FixLabels {
    if in_this_thread {
        FixLabels { finding: "Fix", check: "Fix in this thread", findings: "Fix findings in this thread" }
    } else {
        FixLabels { finding: "Fix in a thread", check: "Fix", findings: "Fix findings in a thread" }
    }
}

impl Workspace {
    /// `composerDraftTarget`: the panel is a thread's Pull request surface, so a handoff's prompt
    /// goes into the composer beside it instead of a new thread.
    pub(super) fn pr_attaches_to_composer(&self) -> bool {
        self.page.is_none()
            && self.target.is_some()
            && matches!(self.active_surface(), Some(crate::app::right_panel::Surface::PullRequest(_)))
    }

    /// The environments a handoff from the Pull requests page can run in (none beside a thread).
    pub(super) fn pr_pickable_environments(&self, detail: &Detail) -> Vec<Pickable> {
        if self.page != Some(super::super::Page::PullRequests) {
            return Vec::new();
        }
        pickable_environments(&detail.project_id, &self.shell.projects, &self.pull_request_servers())
    }

    /// `acting`: the picked project while it is still pickable, else the pull request's own.
    pub(super) fn pr_acting_project(&self, detail: &Detail) -> String {
        let chosen = self.pull_requests.panel.as_ref().and_then(|p| p.acting.as_deref());
        self.pr_pickable_environments(detail)
            .into_iter()
            .find(|p| Some(p.project_id.as_str()) == chosen)
            .map_or_else(|| detail.project_id.clone(), |p| p.project_id)
    }

    fn pick_pr_acting_project(&mut self, project_id: String, cx: &mut Context<Self>) {
        self.check_pr_write_grant(&project_id, cx);
        if let Some(panel) = self.pull_requests.panel.as_mut() {
            panel.acting = Some(project_id);
        }
        cx.notify();
    }

    fn pr_handoff_busy(&self) -> bool {
        self.pull_requests.panel.as_ref().is_some_and(|p| p.handoff.is_some())
    }

    fn set_pr_handoff(&mut self, kind: Option<String>) {
        if let Some(panel) = self.pull_requests.panel.as_mut() {
            panel.handoff = kind;
        }
    }

    /// Adds a handoff's prompt to the composer (`writeHandoffToComposer`).
    fn write_pr_task(&mut self, prompt: &str, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.composer.read(cx).value().to_string();
        let next = logic::append_handoff(&existing, prompt);
        self.set_composer_text(next, window, cx);
    }

    /// `startAsk`: a new thread that knows which pull request is meant.
    pub(super) fn start_pr_ask(&mut self, prompt: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        if self.pr_handoff_busy() {
            return;
        }
        let term = logic::terminology(&detail.provider).0;
        let question = !prompt.lines().next().unwrap_or_default().starts_with("PR #");
        let description = if question {
            "The question is in the composer. Read it over, then send.".to_owned()
        } else {
            format!("The {term} is in the composer. Type your question, then send.")
        };
        if self.pr_attaches_to_composer() {
            self.write_pr_task(&prompt, window, cx);
            window.push_notification(Notification::success(description).title("Added to the composer"), cx);
            return;
        }
        let project_id = self.pr_acting_project(&detail);
        if !self.shell.projects.iter().any(|p| p.id == project_id) {
            toast_error(window, cx, "Could not open a thread", "Try again from the project, or open a thread first.");
            return;
        }
        self.start_draft(project_id, window, cx);
        self.write_pr_task(&prompt, window, cx);
        window.push_notification(Notification::success(description).title("Asked in a thread"), cx);
    }

    /// `startHandoff` from the panel: a checkout of the shown pull request with a thread on it.
    pub(super) fn start_pr_handoff(
        &mut self,
        kind: String,
        task: Option<String>,
        mode: CheckoutMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        if self.pr_handoff_busy() {
            return;
        }
        if let Some(task) = task.as_deref().filter(|_| self.pr_attaches_to_composer()) {
            self.write_pr_task(task, window, cx);
            window.push_notification(
                Notification::success("The task is in the composer. Read it over, then send.")
                    .title("Added to the composer"),
                cx,
            );
            return;
        }
        let project_id = self.pr_acting_project(&detail);
        if self.pr_write_grant(&project_id) == Some(false) {
            return;
        }
        let request = CheckoutRequest {
            project_id,
            url: detail.url.clone(),
            term: logic::terminology(&detail.provider).0,
            task,
            mode,
        };
        self.set_pr_handoff(Some(kind));
        self.prepare_pr_checkout(request, |this| this.set_pr_handoff(None), window, cx);
    }

    /// `startHandoff`'s checkout: a new draft in the project, the pull request checked out for it,
    /// the draft pointed at the checkout and given the task. `settled` runs once it ends either way.
    pub(in crate::app) fn prepare_pr_checkout(
        &mut self,
        request: CheckoutRequest,
        settled: fn(&mut Workspace),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let root = self.shell.projects.iter().find(|p| p.id == request.project_id).map(|p| p.workspace_root.clone());
        let (Some(client), Some(root)) = (self.project_client(&request.project_id), root) else {
            settled(self);
            return;
        };
        let term = request.term;
        window.push_notification(Notification::info("").title(format!("Preparing the {term} checkout...")), cx);
        // The checkout is made for the thread, so the draft's thread id exists first.
        self.start_draft(request.project_id.clone(), window, cx);
        let Some(thread_id) = self.terminal_thread(true) else {
            settled(self);
            return;
        };
        let input = request.input(&root, &thread_id);
        let CheckoutRequest { project_id, task, .. } = request;
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("git.preparePullRequestThread", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                settled(this);
                let value = match result {
                    Ok(value) => value,
                    Err(error) => {
                        let message = match &error {
                            crate::rpc::RpcError::Failure(value) if value["_tag"] == "GitCommandError" => {
                                let detail = value["detail"].as_str().unwrap_or_default();
                                match value["reason"].as_str() {
                                    Some(reason) => format!("{detail} ({reason})"),
                                    None => detail.to_owned(),
                                }
                            }
                            error => list_logic::error_message(error),
                        };
                        toast_error(
                            window,
                            cx,
                            format!("Could not prepare the {term} checkout"),
                            logic::readable_failure(
                                &message,
                                "Check that the branch still exists and is not checked out elsewhere, then try again.",
                            ),
                        );
                        return;
                    }
                };
                let branch = value["branch"].as_str().unwrap_or_default().to_owned();
                let worktree = value["worktreePath"].as_str().map(str::to_owned);
                let on_head = value["isOnPullRequestHead"].as_bool() != Some(false);
                if !matches!(&this.target, Some(Target::Draft { project_id: p, .. }) if *p == project_id) {
                    toast_error(
                        window,
                        cx,
                        "Checked out, but the thread stayed where it was",
                        format!("The checkout is ready on `{branch}`. Point a thread at it from the branch picker, then ask again."),
                    );
                    return;
                }
                this.draft_workspace.choice = match &worktree {
                    Some(path) => WorkspaceChoice {
                        mode: Some(EnvMode::Worktree),
                        branch: Some(branch),
                        existing_worktree: Some(path.clone()),
                        start_from_origin: None,
                    },
                    None => WorkspaceChoice {
                        mode: Some(EnvMode::Local),
                        branch: Some(branch),
                        existing_worktree: None,
                        start_from_origin: None,
                    },
                };
                if let Some(task) = &task {
                    this.write_pr_task(task, window, cx);
                }
                let (title, description) = if !on_head {
                    (
                        "Checked out, but not confirmed on the latest commit".to_owned(),
                        format!("This checkout could not be confirmed at the {term}'s latest commit, so its code may differ. Local commits or uncommitted work keep a checkout where it is."),
                    )
                } else if task.is_some() {
                    ("Checkout ready".to_owned(), "The task is in the composer. Read it over, then send.".to_owned())
                } else if worktree.is_none() {
                    ("Checked out here".to_owned(), format!("This repository is on the {term}'s branch, with a thread open on it."))
                } else {
                    ("Checked out".to_owned(), format!("The {term} is in its own worktree, with a thread open on it."))
                };
                let note = if on_head { Notification::success(description) } else { Notification::warning(description) };
                window.push_notification(note.title(title), cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The project checkout a handoff prepares from (`checkoutRoot`), on the acting environment.
    fn pr_checkout_root(&self, detail: &Detail) -> Option<String> {
        let project_id = self.pr_acting_project(detail);
        self.shell.projects.iter().find(|p| p.id == project_id).map(|p| p.workspace_root.clone())
    }

    pub(super) fn ask_about_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        self.start_pr_ask(logic::ask_handoff(&logic::HandoffSubject::of(&detail)), window, cx);
    }

    pub(super) fn explain_pr(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        self.start_pr_ask(logic::explain_handoff(&logic::HandoffSubject::of(&detail)), window, cx);
    }

    pub(super) fn fix_pr_findings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let Some(detail) = panel.detail.clone() else { return };
        let activity = panel.activity.clone().unwrap_or_default();
        let (_, stale) = panel.checks_state(&detail);
        let checks = if stale { &[][..] } else { detail.checks.as_slice() };
        let prompt = logic::fix_findings_handoff(&logic::HandoffSubject::of(&detail), &activity, checks);
        self.start_pr_handoff("findings".into(), Some(prompt), CheckoutMode::Worktree, window, cx);
    }

    /// `startFixFinding`: one finding's fix, beside the composer or in a checkout.
    pub(super) fn fix_pr_finding(&mut self, finding: &logic::Finding, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        let prompt = logic::fix_finding_handoff(&logic::HandoffSubject::of(&detail), finding);
        self.start_pr_handoff(finding.key(), Some(prompt), CheckoutMode::Worktree, window, cx);
    }

    /// `addCommentToChat`: a remark, with its pull request, into the composer beside the panel.
    pub(super) fn add_pr_to_chat(&mut self, chat: &logic::ChatSubject, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pr_attaches_to_composer() {
            return;
        }
        let Some(panel) = self.pull_requests.panel.as_ref() else { return };
        let Some(detail) = panel.detail.clone() else { return };
        let threads = panel.activity.as_ref().map_or(&[][..], |a| a.review_threads.as_slice());
        let Some(text) = logic::chat_handoff(&logic::HandoffSubject::of(&detail), chat, threads) else { return };
        self.write_pr_task(&text, window, cx);
        window.push_notification(Notification::success("").title("Added to the composer"), cx);
    }

    pub(super) fn resolve_pr_conflicts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        let prompt = logic::resolve_conflicts_prompt(&logic::HandoffSubject::of(&detail));
        self.start_pr_handoff("conflicts".into(), Some(prompt), CheckoutMode::Worktree, window, cx);
    }

    /// The selected lines and a request about them, for an agent (`addSelectionToAgent`).
    pub(super) fn add_pr_selection_to_agent(
        &mut self,
        path: String,
        lines: String,
        request: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(detail) = self.pull_requests.panel.as_ref().and_then(|p| p.detail.clone()) else { return };
        let prompt = logic::selection_handoff(&logic::HandoffSubject::of(&detail), &path, &lines, &request);
        self.start_pr_ask(prompt, window, cx);
    }

    /// `PullRequestCheckoutMenu`: "Check out" with its two places.
    pub(super) fn render_pr_checkout(&self, detail: &Detail, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let term = logic::terminology(&detail.provider).0;
        let handoff = self.pull_requests.panel.as_ref().and_then(|p| p.handoff.clone());
        let checking = handoff.as_deref().is_some_and(|h| h.starts_with("checkout"));
        let root = self.pr_checkout_root(detail);
        let (pickable, acting) = (self.pr_pickable_environments(detail), self.pr_acting_project(detail));
        let view = cx.entity();
        xs_outline("pr-checkout", t)
            .disabled(handoff.is_some() || root.is_none())
            .tooltip(format!("Check out this {term}"))
            .child(Icon::new(Lucide::GitBranch).with_size(px(14.)))
            .child(if checking { "Checking out..." } else { "Check out" })
            .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)).text_color(t.muted_foreground))
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu: PopupMenu, window, cx| {
                let muted = theme::tokens(cx).muted_foreground;
                let entry = |label: &'static str, note: &'static str, icon: Lucide| {
                    PopupMenuItem::element(move |_, _| {
                        h_flex()
                            .items_start()
                            .gap(px(8.))
                            .child(div().mt(px(3.)).child(Icon::new(icon).with_size(px(14.))))
                            .child(v_flex().child(label).child(div().text_xs().text_color(muted).child(note)))
                    })
                };
                let menu = menu
                    .item(
                        entry(
                            "In a separate worktree",
                            "Its own folder and thread. Nothing you have open moves.",
                            Lucide::GitBranch,
                        )
                        .on_click(window.listener_for(&view, |this, _, window, cx| {
                            this.start_pr_handoff("checkout:worktree".into(), None, CheckoutMode::Worktree, window, cx);
                        })),
                    )
                    .item(
                        entry("In this repository", "Switches the branch you are working in.", Lucide::FolderGit2)
                            .on_click(window.listener_for(&view, |this, _, window, cx| {
                                this.start_pr_handoff("checkout:local".into(), None, CheckoutMode::Local, window, cx);
                            })),
                    );
                environment_items(menu, &pickable, &acting, false, &view, window)
            })
            .into_any_element()
    }

    /// `PullRequestResolveConflictsButton`.
    pub(super) fn render_pr_resolve_conflicts(
        &self,
        detail: &Detail,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let handoff = self.pull_requests.panel.as_ref().and_then(|p| p.handoff.clone());
        let preparing = handoff.as_deref() == Some("conflicts");
        let label = if preparing { "Preparing..." } else { "Resolve conflicts" };
        xs_outline("pr-resolve-conflicts", t)
            .disabled(handoff.is_some() || self.pr_checkout_root(detail).is_none())
            .text_color(t.error)
            .border_color(Hsla { a: 0.4, ..t.error })
            .tooltip(label)
            .child(Icon::new(Lucide::GitPullRequestClosed).with_size(px(14.)))
            .child(label)
            .on_click(cx.listener(|this, _, window, cx| this.resolve_pr_conflicts(window, cx)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(environment: &str, id: &str, key: Option<&str>) -> ProjectShell {
        let mut project: ProjectShell = serde_json::from_value(json!({
            "id": id,
            "title": id,
            "workspaceRoot": format!("/{environment}/{id}"),
            "repositoryIdentity": key.map(|key| json!({"canonicalKey": key})),
        }))
        .expect("a project");
        project.environment_id = environment.to_owned().into();
        project
    }

    fn environment(id: &str) -> (SharedString, SharedString, String) {
        (id.to_owned().into(), format!("{id} box").into(), "laptop".to_owned())
    }

    #[test]
    fn a_handoff_can_run_on_another_environment_with_the_same_repository() {
        let projects = [
            project("a", "p1", Some("github.com/T3/Code")),
            project("b", "p2", Some("github.com/t3/code")),
            project("b", "p3", Some("github.com/t3/code")),
            project("c", "p4", Some("github.com/other/repo")),
        ];
        let environments = [environment("a"), environment("b"), environment("c")];
        let picked: Vec<String> =
            pickable_environments("p1", &projects, &environments).into_iter().map(|p| p.project_id).collect();
        assert_eq!(picked, ["p1", "p2"], "its own first, then one copy per other environment");
        assert!(
            pickable_environments("p1", &projects[..1], &environments).is_empty(),
            "no picker while no other environment has the repository"
        );
        let unkeyed = [project("a", "p1", None), project("b", "p2", None)];
        assert!(pickable_environments("p1", &unkeyed, &environments).is_empty());
    }
}
