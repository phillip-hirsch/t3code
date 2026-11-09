//! The right panel's pull request surfaces (`RightPanelSurfaceContent` "pull-request" and
//! "pull-requests"): the thread's pull request in the detail panel the Pull requests page uses,
//! and the thread's linked pull requests (`ThreadPullRequestsPanel`).

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity,
    Window, div, px,
};
use serde_json::json;

use super::super::Workspace;
use super::super::right_panel::Surface;
use super::pull_request_detail::PanelThread;
use super::pull_requests::tone;
use crate::pull_request_detail::Reference;
use crate::theme::{self, Tokens};
use crate::thread_pull_requests::{self as links, PullRequestLink};

impl Workspace {
    fn open_thread_shell(&self) -> Option<&crate::model::ThreadShell> {
        let Some(super::super::Target::Thread { env, id }) = &self.target else { return None };
        self.shell.threads.iter().find(|t| &t.id == id && &t.environment_id == env)
    }

    /// `pullRequestPanelTarget`: the open thread's pull request, if it has one.
    pub(in crate::app) fn thread_pull_request_target(&self) -> Option<Reference> {
        let shell = self.open_thread_shell()?;
        links::panel_target(
            &shell.environment_id,
            &shell.project_id,
            &shell.pull_requests,
            shell.linked_pull_request.as_ref(),
            shell.branch_pull_request.as_ref(),
        )
    }

    /// `pullRequestsSurfaceAvailable`: the thread links at least one pull request.
    pub(in crate::app) fn thread_links_pull_requests(&self) -> bool {
        self.open_thread_shell().is_some_and(|shell| links::visible(&shell.pull_requests).next().is_some())
    }

    /// `useOpenPanelPullRequestUrl`: `None` when no pull request is open in the panel (on the
    /// Pull requests page, its detail panel), else its URL once known.
    pub(in crate::app) fn open_panel_pull_request_url(&self) -> Option<Option<String>> {
        let panel_url = |reference: Option<&Reference>| {
            let panel = self.pull_requests.panel.as_ref().filter(|p| reference.is_none_or(|r| p.reference() == r));
            panel.and_then(|p| p.url()).map(str::to_owned)
        };
        if self.page == Some(super::Page::PullRequests) {
            return self.pull_requests.panel.is_some().then(|| panel_url(None));
        }
        let Some(Surface::PullRequest(reference)) = self.active_surface() else { return None };
        // Before the detail loads, a link the thread holds names the same pull request.
        let shell = self.open_thread_shell();
        let linked = shell.and_then(|shell| {
            let same = |repository: &str, number: u64| {
                repository.eq_ignore_ascii_case(&reference.repository) && number == reference.number
            };
            links::visible(&shell.pull_requests).find(|l| same(&l.repository, l.number)).map(|l| l.url.clone()).or_else(
                || {
                    [shell.linked_pull_request.as_ref(), shell.branch_pull_request.as_ref()]
                        .into_iter()
                        .flatten()
                        .find(|l| same(&l.repository, l.number))
                        .map(|l| l.url.clone())
                },
            )
        });
        Some(panel_url(Some(reference)).or(linked))
    }

    /// The visible linked pull requests as sorted `[host, repository, number]` keys
    /// (`proactivePullRequestsKey`).
    pub(in crate::app) fn thread_pull_request_keys(&self) -> Vec<String> {
        let Some(shell) = self.open_thread_shell() else { return Vec::new() };
        let mut keys: Vec<String> = links::visible(&shell.pull_requests)
            .map(|link| serde_json::json!([link.host, link.repository, link.number]).to_string())
            .collect();
        keys.sort();
        keys
    }

    /// The Pull request surface: the detail panel, retargeted to this tab's pull request.
    pub(in crate::app) fn render_pull_request_surface(
        &mut self,
        reference: &Reference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.pull_requests.panel.as_ref().is_none_or(|panel| panel.reference() != reference) {
            let seed = self.pull_requests.entry_for(reference);
            self.open_pull_request_panel(reference.clone(), seed, cx);
            self.follow_pull_requests(cx);
        }
        let thread = self.open_thread_shell().map(|shell| PanelThread {
            id: shell.id.clone(),
            back: links::visible(&shell.pull_requests).nth(1).is_some(),
        });
        self.render_pull_request_panel(thread.as_ref(), window, cx)
    }

    /// The Linked pull requests surface.
    pub(in crate::app) fn render_linked_pull_requests(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(shell) = self.open_thread_shell() else { return div().into_any_element() };
        let project_id = shell.project_id.clone();
        let visible: Vec<&PullRequestLink> = links::visible(&shell.pull_requests).collect();
        if visible.is_empty() {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .p_6()
                .text_center()
                .child(Icon::new(Lucide::Link).size(px(24.)).text_color(t.muted_foreground.opacity(0.6)))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("No linked pull requests"))
                .child(div().max_w(px(240.)).text_xs().text_color(t.muted_foreground).child(
                    "Pull requests the agent opens from this thread land here. Link one yourself from a URL or a number.",
                ))
                .child(
                    Button::new("link-pull-request-empty")
                        .outline()
                        .small()
                        .gap(px(6.))
                        .child(Icon::new(Lucide::Plus).size(px(14.)))
                        .child("Link pull request")
                        .on_click(cx.listener(|this, _, window, cx| this.open_link_pull_request_dialog(window, cx))),
                )
                .into_any_element();
        }
        let open = visible.iter().filter(|l| l.snapshot.as_ref().is_none_or(|s| s.state == "open")).count();
        let rows: Vec<AnyElement> = links::list_lines(&shell.pull_requests)
            .iter()
            .map(|line| linked_row(line, &shell.environment_id, &project_id, &t, cx))
            .collect();
        v_flex()
            .flex_1()
            .min_h_0()
            .child(v_flex().id("linked-pull-requests").flex_1().min_h_0().overflow_y_scroll().p(px(6.)).children(rows))
            .child(
                h_flex()
                    .flex_none()
                    .justify_between()
                    .px_2()
                    .py(px(6.))
                    .border_t_1()
                    .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                    .text_size(gpui_kit::rems(0.625))
                    .text_color(t.muted_foreground)
                    .child(format!("{open} open · {} linked", visible.len()))
                    .child(
                        Button::new("link-pull-request-footer")
                            .ghost()
                            .xsmall()
                            .gap(px(4.))
                            .child(Icon::new(Lucide::Plus).size(px(14.)))
                            .child("Link")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_link_pull_request_dialog(window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Workspace {
    /// Pull requests link to threads where the environment reports capabilities
    /// (`threadPullRequestLinkMode`).
    pub(in crate::app) fn pull_request_linking(&self, thread_id: &str) -> bool {
        let environment = self.thread_environment(thread_id);
        environment.and_then(|e| self.env_info(&e)).is_some_and(|info| !info.capabilities.is_null())
    }

    fn open_link_pull_request_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(thread_id) = self.open_thread_shell().map(|shell| shell.id.clone()) {
            self.open_link_pull_request(thread_id, window, cx);
        }
    }

    /// `LinkPullRequestDialog`: links a change request to `thread_id` by URL or number. The panel
    /// and the command palette open this one dialog.
    pub(in crate::app) fn open_link_pull_request(
        &mut self,
        thread_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shell) = self.shell.threads.iter().find(|t| t.id == thread_id) else { return };
        let identity =
            self.shell.projects.iter().find(|p| p.id == shell.project_id).and_then(|p| p.repository_identity.clone());
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Pull request URL or #42"));
        input.update(cx, |input, cx| input.focus(window, cx));
        let workspace = cx.entity().downgrade();
        let form = cx.new(|cx| LinkPullRequestForm {
            _subscription: cx.subscribe_in(&input, window, |this: &mut LinkPullRequestForm, _, event, window, cx| {
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
            dirty: false,
            pending: false,
            error: None,
            thread_id,
            identity,
            workspace,
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.entrance(DialogEntrance::None).title("Link pull request").w(px(576.)).child(form.clone())
        });
    }
}

/// The dialog's input and its answer (`LinkPullRequestDialog`).
struct LinkPullRequestForm {
    input: Entity<InputState>,
    dirty: bool,
    pending: bool,
    error: Option<SharedString>,
    thread_id: String,
    identity: Option<crate::model::RepositoryIdentity>,
    workspace: WeakEntity<Workspace>,
    _subscription: Subscription,
}

impl LinkPullRequestForm {
    fn resolved(&self, cx: &gpui_kit::App) -> Option<Result<links::ResolvedLink, String>> {
        links::resolve_link_input(&self.input.read(cx).value(), self.identity.as_ref())
    }

    /// `thread.pull-request.link` with `source: "manual"`; the dialog closes once it lands.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dirty = true;
        cx.notify();
        let Some(Ok(link)) = self.resolved(cx) else { return };
        if self.pending {
            return;
        }
        let client = self.workspace.upgrade().and_then(|w| w.read(cx).thread_client(&self.thread_id));
        let Some(client) = client else {
            self.error = Some("The pull request is not available in this environment.".into());
            return;
        };
        self.error = None;
        self.pending = true;
        let command = json!({
            "type": "thread.pull-request.link", "commandId": super::super::new_id(), "threadId": self.thread_id,
            "host": link.host, "repository": link.repository, "number": link.number, "url": link.url, "source": "manual",
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending = false;
                match result {
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for LinkPullRequestForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let empty = self.input.read(cx).value().trim().is_empty();
        let resolved = self.resolved(cx);
        let validation: Option<SharedString> = match &resolved {
            _ if !self.dirty => None,
            _ if empty => Some("Paste a pull request URL or enter 123 / #123.".into()),
            None => Some("Use a pull request URL, 123, or #123.".into()),
            Some(Err(error)) => Some(error.clone().into()),
            Some(Ok(_)) => None,
        };
        let preview = match &resolved {
            Some(Ok(link)) => Some(format!("{}/{} #{}", link.host, link.repository, link.number)),
            _ => None,
        };
        let ready = matches!(resolved, Some(Ok(_)));
        v_flex()
            .gap(px(12.))
            .child(div().text_sm().text_color(t.muted_foreground).child(
                "Attach a pull request to this thread by its URL, or by its number for this thread's repository.",
            ))
            .child(Input::new(&self.input))
            .children(preview.map(|p| div().truncate().text_xs().text_color(t.muted_foreground).child(p)))
            .children(
                validation
                    .or_else(|| self.error.clone())
                    .map(|message| div().text_xs().text_color(t.error).child(message)),
            )
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("link-pull-request-cancel")
                            .outline()
                            .small()
                            .label("Cancel")
                            .disabled(self.pending)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("link-pull-request-submit")
                            .primary()
                            .small()
                            .label(if self.pending { "Linking..." } else { "Link" })
                            .disabled(self.pending || !ready)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

/// `SOURCE_LABELS`.
fn source_label(source: &str) -> &'static str {
    match source {
        "created" => "Created from this thread",
        "agent" => "Linked by the agent",
        "stack" => "Found in the stack",
        "stack-dismissed" => "Dismissed",
        _ => "Linked by you",
    }
}

fn state_glyph(link: &PullRequestLink, t: &Tokens) -> (Lucide, Hsla) {
    match link.snapshot.as_ref().map(|s| (s.state.as_str(), s.is_draft)) {
        None => (Lucide::GitPullRequest, t.muted_foreground),
        Some(("merged", _)) => (Lucide::GitMerge, tone(t, 0x7f22fe, 0xc4b4ff, 0.9)),
        Some(("closed", _)) => (Lucide::GitPullRequestClosed, tone(t, 0xe7000b, 0xffa2a2, 0.9)),
        Some((_, true)) => (Lucide::GitPullRequestDraft, tone(t, 0x71717b, 0x9f9fa9, 0.8)),
        Some(_) => (Lucide::GitPullRequestArrow, tone(t, 0x009966, 0x5ee9b5, 0.9)),
    }
}

/// The stack chip's tooltip on a chain's base row.
fn stack_tip(link: &PullRequestLink, native: bool, size: usize) -> String {
    let parsed = crate::pull_request_detail::parse_change_request(&link.url);
    let provider = parsed.as_ref().map_or("", |l| l.provider);
    if native {
        let host = match provider {
            "github" => "GitHub",
            "gitlab" => "GitLab",
            "gitcafe" => "GitCafe",
            "forgejo" => "Forgejo",
            "bitbucket" => "Bitbucket",
            "azure-devops" => "Azure DevOps",
            _ => "the host",
        };
        format!("{host} stack of {size}: merging a layer lands the ones below it.")
    } else {
        let plural = if provider == "gitlab" { "merge requests" } else { "pull requests" };
        format!("{size} {plural} chained by base branch.")
    }
}

/// A `LinkRow`: state, number, title and repository, indented by its depth in its chain; it
/// opens in the Pull request surface.
fn linked_row(
    line: &links::ListLine,
    environment_id: &str,
    project_id: &str,
    t: &Tokens,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let link = line.link;
    let (glyph, color) = state_glyph(link, t);
    let title: SharedString =
        link.snapshot.as_ref().map_or_else(|| link.repository.clone(), |s| s.title.clone()).into();
    let reference = Reference {
        environment_id: environment_id.to_owned(),
        project_id: project_id.to_owned(),
        host: Some(link.host.clone()),
        repository: link.repository.clone(),
        number: link.number,
    };
    let source: SharedString = source_label(&link.source).into();
    let hover = t.accent.opacity(0.6);
    let stack = line.stack.map(|(native, size)| {
        let tip: SharedString = stack_tip(link, native, size).into();
        h_flex()
            .id(ElementId::Name(format!("linked-pr-stack:{}", link.number).into()))
            .flex_none()
            .gap(px(2.))
            .text_color(t.foreground.opacity(0.7))
            .child(Icon::new(Lucide::Layers).size(px(12.)))
            .child(size.to_string())
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx))
    });
    let depth = line.depth.min(3) as f32;
    h_flex()
        .id(ElementId::Name(format!("linked-pr:{}/{}#{}", link.host, link.repository, link.number).into()))
        .w_full()
        .gap_2()
        .py_1()
        .pl(px(8. + depth * 20.))
        .pr_1()
        .rounded(px(6.))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .when(line.depth > 0, |el| {
            el.child(div().flex_none().ml(px(-8.)).h(px(24.)).w(px(1.)).bg(t.border.opacity(0.7)))
        })
        .child(Icon::new(glyph).size(px(16.)).text_color(color))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            div()
                                .id(ElementId::Name(format!("linked-pr-number:{}", link.number).into()))
                                .flex_none()
                                .font_family(t.fonts.mono)
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .child(format!("#{}", link.number))
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(source.clone()).build(window, cx)
                                }),
                        )
                        .child(div().min_w_0().truncate().text_sm().child(title)),
                )
                .child(
                    h_flex().min_w_0().gap_1().text_xs().text_color(t.muted_foreground).children(stack).child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(t.fonts.mono)
                            .when(link.snapshot.is_none(), |el| el.child(format!("{}/{}", link.host, link.repository)))
                            .when(link.snapshot.is_some(), |el| el.child(link.repository.clone())),
                    ),
                ),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_surface(Surface::PullRequest(reference.clone()), window, cx);
        }))
        .into_any_element()
}
