//! Checking a change request out into the draft (`PullRequestThreadDialog`), and its way in, the
//! branch picker's "Checkout pull request" row: the reference a URL, checkout command or number
//! names is read from the host, then checked out in the project or a new worktree, and the draft
//! points at the checkout (`usePullRequestDraftThread`).

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Stateful, Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};
use serde_json::json;

use super::super::git::SOURCE_CONTROL_WRITE;
use super::super::{Target, Workspace};
use super::pull_requests::{host_mark, tone};
use crate::new_thread::{EnvMode, WorkspaceChoice};
use crate::pull_request_checkout::{self as logic, Access, CheckoutState, HostTerms, Prepared, ResolvedPullRequest};
use crate::pull_request_detail::CheckoutMode;
use crate::rpc::RpcClient;
use crate::theme::{self, Tokens};

/// `useDebouncedValue`'s wait before a typed reference is read.
const SETTLE: Duration = Duration::from_millis(450);

/// The draft a checkout is for, as the dialog opened on it.
#[derive(Clone, Debug, PartialEq)]
struct Origin {
    /// The draft itself; a project can hold several.
    draft: Target,
    environment_id: SharedString,
    project_id: String,
    /// The project checkout the change request is read and prepared from.
    cwd: String,
    /// The draft's thread, which a worktree checkout is made for.
    thread_id: String,
}

/// The draft's source control host (`getSourceControlPresentation`).
#[derive(Clone, Debug)]
struct Host {
    kind: Option<String>,
    name: String,
    terms: HostTerms,
}

/// The host's mark (`SourceControlIcon`); the unknown host draws the pull request glyph.
fn host_icon(kind: Option<&str>, size: f32, color: Hsla) -> AnyElement {
    match kind {
        Some(kind @ ("github" | "gitlab" | "bitbucket" | "azure-devops" | "forgejo" | "gitcafe")) => {
            host_mark(kind, size, color)
        }
        _ => Icon::new(Lucide::GitPullRequest).with_size(px(size)).text_color(color).into_any_element(),
    }
}

/// `pullRequestStatusTone`.
fn state_color(state: &str, t: &Tokens) -> Hsla {
    match state {
        "merged" => tone(t, 0x7f22fe, 0xc4b4ff, 0.9),
        "closed" => tone(t, 0x71717b, 0x9f9fa9, 0.8),
        "open" => tone(t, 0x009966, 0x5ee9b5, 0.9),
        _ => t.muted_foreground,
    }
}

impl Workspace {
    /// `canCheckoutPullRequestIntoThread`: a draft whose environment allows source control writes.
    pub(in crate::app) fn can_checkout_pull_request(&self) -> bool {
        matches!(&self.target, Some(Target::Draft { env, .. }) if self.env_grants(env, SOURCE_CONTROL_WRITE))
    }

    /// The open checkout's host, else the project's remote's.
    fn checkout_host(&self) -> Host {
        let provider = self.vcs.status.as_ref().and_then(|s| s.local.source_control_provider.as_ref());
        let kind = provider
            .map(|p| p.kind.clone())
            .filter(|k| !k.is_empty())
            .or_else(|| self.target_project().and_then(|p| p.repository_identity.as_ref()?.provider.clone()));
        let terms = logic::host_terms(kind.as_deref());
        let name = provider.and_then(|p| p.name.clone()).filter(|n| !n.is_empty());
        Host { name: name.unwrap_or_else(|| terms.label.to_owned()), kind, terms }
    }

    /// `openPullRequestThreadDialog`: the checkout dialog for the open draft, on `reference`.
    pub(in crate::app) fn open_pull_request_checkout(
        &mut self,
        reference: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.can_checkout_pull_request() {
            return;
        }
        let Some(draft @ Target::Draft { .. }) = self.target.clone() else { return };
        let Target::Draft { env, project_id, .. } = draft.clone() else { return };
        let project = self.shell.projects.iter().find(|p| p.id == project_id && p.environment_id == env);
        let Some(cwd) = project.map(|p| p.workspace_root.clone()) else { return };
        let Some(thread_id) = self.terminal_thread(cx) else { return };
        let origin = Origin { draft, environment_id: env, project_id, cwd, thread_id };
        let host = self.checkout_host();
        let placeholder = format!("{} URL, checkout command, or #42", host.terms.short);
        let initial = reference.clone().unwrap_or_default();
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(initial));
        let workspace = cx.entity().downgrade();
        // The workspace is mid-update here, so the first read gets its client now.
        let client = self.env_client(&origin.environment_id);
        let form = cx.new(|cx| {
            let subscription =
                cx.subscribe_in(&input, window, |this: &mut CheckoutForm, input, event, window, cx| match event {
                    InputEvent::Change => {
                        this.state.edit(input.read(cx).value().to_string());
                        this.schedule_settle(cx);
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } if !this.state.is_resolving() && this.state.preparing().is_none() => {
                        this.confirm(CheckoutMode::Worktree, window, cx);
                    }
                    _ => {}
                });
            let mut form = CheckoutForm {
                state: CheckoutState::new(reference.as_deref()),
                origin,
                host,
                input: input.clone(),
                workspace,
                _settle: None,
                _subscription: subscription,
            };
            if let Some(reference) = form.state.settle() {
                form.read(reference, client, cx);
            }
            form
        });
        window.open_dialog(cx, move |dialog, _, cx| {
            let form_state = form.read(cx);
            let pending = form_state.state.preparing().is_some();
            let t = theme::tokens(cx);
            let host = &form_state.host;
            let title = h_flex()
                .items_center()
                .child(div().mr(px(8.)).child(host_icon(host.kind.as_deref(), 16., t.foreground)))
                .child(format!("Checkout {}", host.terms.singular));
            dialog
                .title(title)
                .w(px(576.))
                .close_button(!pending)
                .keyboard(!pending)
                .overlay_closable(!pending)
                .child(form.clone())
        });
        // After the dialog takes focus, so closing it gives focus back to the composer.
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    /// Points the draft at a prepared checkout (`setDraftThreadContext` with its branch, its
    /// worktree and the matching mode).
    pub(in crate::app) fn point_draft_at_checkout(&mut self, prepared: &Prepared) {
        let worktree = prepared.worktree_path.clone();
        let mode = if worktree.is_some() { EnvMode::Worktree } else { EnvMode::Local };
        self.draft_workspace.choice = WorkspaceChoice {
            mode: Some(mode),
            branch: Some(prepared.branch.clone()),
            existing_worktree: worktree,
            start_from_origin: None,
        };
    }

    /// `openOrReuseProjectDraftThread`: the checkout lands on its draft, opened again if the user
    /// left it meanwhile, or on the project's draft once that one is gone.
    fn place_checkout(&mut self, origin: &Origin, prepared: &Prepared, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_ref() != Some(&origin.draft) {
            let known = self
                .shell
                .projects
                .iter()
                .any(|p| p.id == origin.project_id && p.environment_id == origin.environment_id);
            if !known {
                return;
            }
            self.open_draft_entry(origin.draft.clone(), window, cx);
        }
        self.point_draft_at_checkout(prepared);
        cx.notify();
    }

    /// The branch picker's "Checkout pull request" row for `reference`.
    pub(in crate::app) fn render_checkout_branch_item(
        &self,
        reference: &str,
        highlighted: bool,
        t: &Tokens,
    ) -> Stateful<Div> {
        let host = self.checkout_host();
        h_flex()
            .id("checkout-pull-request")
            .flex_none()
            .min_w_0()
            .px(px(8.))
            .py(px(4.))
            .gap(px(8.))
            .rounded(px(4.))
            .text_sm()
            .cursor_pointer()
            .when(highlighted, |el| el.bg(t.foreground.opacity(0.09)))
            .child(div().flex_none().child(host_icon(host.kind.as_deref(), 14., t.muted_foreground)))
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.foreground)
                            .child(format!("Checkout {}", host.terms.singular)),
                    )
                    .child(div().truncate().text_xs().text_color(t.muted_foreground).child(reference.to_owned())),
            )
    }
}

/// The dialog's body: the reference, the change request it resolved to, and the two checkouts.
struct CheckoutForm {
    state: CheckoutState,
    origin: Origin,
    host: Host,
    input: Entity<InputState>,
    workspace: WeakEntity<Workspace>,
    _settle: Option<Task<()>>,
    _subscription: Subscription,
}

impl CheckoutForm {
    /// The draft environment's client now: a reconnect replaces it.
    fn client(&self, cx: &gpui_kit::App) -> Option<RpcClient> {
        self.workspace.upgrade()?.read(cx).env_client(&self.origin.environment_id)
    }

    /// The grants on the draft's environment, read when asked.
    fn access(&self, cx: &gpui_kit::App) -> Access {
        let Some(workspace) = self.workspace.upgrade() else { return Access::default() };
        let workspace = workspace.read(cx);
        let environment = &self.origin.environment_id;
        Access {
            write: workspace.env_grants(environment, SOURCE_CONTROL_WRITE),
            operate: workspace.can_operate(environment),
        }
    }

    fn schedule_settle(&mut self, cx: &mut Context<Self>) {
        self._settle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let _ = this.update(cx, CheckoutForm::settle);
        }));
    }

    fn settle(&mut self, cx: &mut Context<Self>) {
        if let Some(reference) = self.state.settle() {
            let client = self.client(cx);
            self.read(reference, client, cx);
        }
        cx.notify();
    }

    /// `git.resolvePullRequest` for `reference` in the project checkout.
    fn read(&mut self, reference: String, client: Option<RpcClient>, cx: &mut Context<Self>) {
        let Some(client) = client else {
            self.state.landed(reference, Err("This environment is not connected.".into()));
            return;
        };
        let input = json!({"cwd": self.origin.cwd, "reference": reference});
        cx.spawn(async move |this, cx| {
            let answer = client.request("git.resolvePullRequest", input).await;
            let _ = this.update(cx, |this, cx| {
                let answer = match answer {
                    Ok(value) => ResolvedPullRequest::of_answer(&value)
                        .ok_or_else(|| "The host's answer could not be read.".to_owned()),
                    Err(error) => Err(logic::git_error_message(&error).unwrap_or_else(|| error.to_string())),
                };
                this.state.landed(reference, answer);
                cx.notify();
            });
        })
        .detach();
    }

    /// `handleConfirm`: `git.preparePullRequestThread` in `mode`, then the draft points at it.
    fn confirm(&mut self, mode: CheckoutMode, window: &mut Window, cx: &mut Context<Self>) {
        let Some(reference) = self.state.start(mode, self.access(cx)) else {
            cx.notify();
            return;
        };
        let singular = self.host.terms.singular;
        let Some(client) = self.client(cx) else {
            self.state.finished(Some(format!("Failed to prepare {singular} thread.")));
            cx.notify();
            return;
        };
        let origin = self.origin.clone();
        let mut input = json!({"cwd": origin.cwd, "reference": reference, "mode": match mode {
            CheckoutMode::Local => "local",
            CheckoutMode::Worktree => "worktree",
        }});
        if mode == CheckoutMode::Worktree {
            input["threadId"] = json!(origin.thread_id);
        }
        // The dialog's own close controls follow the pending state.
        window.refresh();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("git.preparePullRequestThread", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let prepared = match result {
                    Ok(value) => Prepared::of_answer(&value).ok_or(None),
                    Err(error) => Err(logic::git_error_message(&error)),
                };
                let prepared = match prepared {
                    Ok(prepared) => prepared,
                    Err(message) => {
                        this.state.finished(Some(message.unwrap_or_else(|| format!("Failed to prepare {singular} thread."))));
                        window.refresh();
                        cx.notify();
                        return;
                    }
                };
                this.state.finished(None);
                let _ = this.workspace.update(cx, |workspace, cx| workspace.place_checkout(&origin, &prepared, window, cx));
                if !prepared.on_head {
                    let description = format!(
                        "This checkout could not be confirmed at the {singular}'s latest commit, so its code may differ. Local commits or uncommitted work keep a checkout where it is."
                    );
                    let note = Notification::warning(description).title("Checked out, but not confirmed on the latest commit");
                    window.push_notification(note, cx);
                }
                window.close_dialog(cx);
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for CheckoutForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let access = self.access(cx);
        let terms = self.host.terms;
        let resolving = self.state.is_resolving();
        let preparing = self.state.preparing();
        let resolved = self.state.resolved().cloned();
        let error = self.state.error(terms.singular);
        let preview = resolved.map(|pull_request| {
            let color = state_color(&pull_request.state, &t);
            div()
                .rounded(px(12.))
                .border_1()
                .border_color(t.border.opacity(0.7))
                .bg(t.muted.opacity(0.24))
                .p(px(12.))
                .child(
                    h_flex()
                        .items_start()
                        .justify_between()
                        .gap(px(12.))
                        .child(
                            v_flex()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(pull_request.title),
                                )
                                .child(div().truncate().text_xs().text_color(t.muted_foreground).child(format!(
                                    "#{} · {} to {}",
                                    pull_request.number, pull_request.head_branch, pull_request.base_branch
                                ))),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(color)
                                .child(logic::capitalize_words(&pull_request.state)),
                        ),
                )
        });
        let button = |id: &'static str, mode: CheckoutMode, label: &'static str, busy: &'static str| {
            let label = if preparing == Some(mode) { busy } else { label };
            Button::new(id)
                .small()
                .label(label)
                .disabled(!self.state.can_confirm(mode, access))
                .on_click(cx.listener(move |this, _, window, cx| this.confirm(mode, window, cx)))
        };
        v_flex()
            .gap(px(16.))
            .child(div().text_sm().text_color(t.muted_foreground).child(format!(
                "Resolve a {} {}, then create the draft thread in the main repo or in a dedicated worktree.",
                self.host.name, terms.singular
            )))
            .child(
                v_flex()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.foreground)
                            .child(logic::capitalize_words(terms.singular)),
                    )
                    .child(Input::new(&self.input)),
            )
            .children(preview)
            .when(resolving, |el| {
                el.child(
                    h_flex()
                        .gap(px(8.))
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(Spinner::new().small())
                        .child(format!("Resolving {}...", terms.singular)),
                )
            })
            .children(error.map(|message| div().text_xs().text_color(t.error).child(message)))
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("checkout-pull-request-cancel")
                            .outline()
                            .small()
                            .label("Cancel")
                            .disabled(preparing.is_some())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        button("checkout-pull-request-local", CheckoutMode::Local, "Local", "Preparing local...")
                            .outline(),
                    )
                    .child(
                        button(
                            "checkout-pull-request-worktree",
                            CheckoutMode::Worktree,
                            "Worktree",
                            "Preparing worktree...",
                        )
                        .primary(),
                    ),
            )
    }
}
