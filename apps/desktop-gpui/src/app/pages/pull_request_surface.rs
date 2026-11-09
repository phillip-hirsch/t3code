//! The right panel's pull request surfaces (`RightPanelSurfaceContent` "pull-request" and
//! "pull-requests"): the thread's pull request in the detail panel the Pull requests page uses,
//! and the thread's linked pull requests (`ThreadPullRequestsPanel`).

use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, ClipboardItem, Context, ElementId, Entity, FocusHandle, FontWeight,
    Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, div, px, relative, rgb,
};
use serde_json::json;

use super::super::Workspace;
use super::super::right_panel::Surface;
use super::super::sidebar::now_ms;
use super::pull_request_detail::{PanelThread, breakdown, toast_error};
use super::pull_requests::speed_actions::{self, RowSpeed};
use super::pull_requests::{checks_glyph, glyph, relative_time, review_glyph, tone};
use crate::pull_request_detail::Reference;
use crate::pull_requests::Entry;
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
    pub(in crate::app) fn render_linked_pull_requests(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(shell) = self.open_thread_shell() else { return div().into_any_element() };
        let environment_id = shell.environment_id.to_string();
        let keys: HashSet<String> =
            links::visible(&shell.pull_requests).map(|link| linked_key(&environment_id, link)).collect();
        let focus = &mut self.pull_requests.linked_focus;
        focus.retain(|key, _| keys.contains(key));
        for key in keys {
            focus.entry(key).or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
        let Some(shell) = self.open_thread_shell() else { return div().into_any_element() };
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
        let authors = visible.iter().filter_map(|l| l.snapshot.as_ref()?.author.as_ref()?.avatar_url.as_deref());
        super::pull_request_detail::avatars::read_avatars(authors, cx);
        let now = now_ms();
        let synced = links::latest_synced_ms(&shell.pull_requests)
            .map_or_else(String::new, |at| format!(" · synced {}", relative_time(now - at)));
        let watchable = self.env_info(&environment_id).is_some_and(|info| info.capability("threadPullRequestWatch"));
        let projects: Vec<(&str, Option<&str>)> = self
            .shell
            .projects
            .iter()
            .filter(|p| p.environment_id == shell.environment_id)
            .map(|p| (p.id.as_str(), p.repository_identity.as_ref().and_then(|i| i.canonical_key.as_deref())))
            .collect();
        let view = cx.entity();
        let rows: Vec<AnyElement> = links::list_lines(&shell.pull_requests)
            .iter()
            .map(|line| {
                let link = line.link;
                let key = linked_key(&environment_id, link);
                // `findProjectForChangeRequest`: the environment's project for the repository, else the thread's.
                let project_id = crate::pull_request_detail::project_for_link(
                    projects.iter().copied(),
                    &link.host,
                    &link.repository,
                )
                .unwrap_or(shell.project_id.as_str());
                let entry = link.action_entry(&environment_id, project_id).map(Rc::new);
                let speed = entry.map(|entry| {
                    let speed = self.linked_row_speed(&entry);
                    (entry, speed)
                });
                let row = LinkedRow {
                    line,
                    reference: Reference {
                        environment_id: environment_id.clone(),
                        project_id: project_id.to_owned(),
                        host: Some(link.host.clone()),
                        repository: link.repository.clone(),
                        number: link.number,
                    },
                    thread_id: shell.id.clone(),
                    focus: self.pull_requests.linked_focus.get(&key).cloned(),
                    key,
                    speed,
                    watchable,
                    now,
                };
                linked_row(row, &view, &t, window, cx)
            })
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
                    .child(format!("{open} open · {} linked{synced}", visible.len()))
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
/// `PullRequestStateGlyph`: the icon, its tone and its label.
fn state_glyph(link: &PullRequestLink, t: &Tokens) -> (Lucide, Hsla, &'static str) {
    match link.snapshot.as_ref().map(|s| (s.state.as_str(), s.is_draft)) {
        None => (Lucide::GitPullRequest, t.muted_foreground, "Waiting for host state"),
        Some(("merged", _)) => (Lucide::GitMerge, tone(t, 0x7f22fe, 0xc4b4ff, 0.9), "Merged"),
        Some(("closed", _)) => (Lucide::GitPullRequestClosed, tone(t, 0xe7000b, 0xffa2a2, 0.9), "Closed"),
        Some((_, true)) => (Lucide::GitPullRequestDraft, tone(t, 0x71717b, 0x9f9fa9, 0.8), "Draft"),
        Some(_) => (Lucide::GitPullRequestArrow, tone(t, 0x009966, 0x5ee9b5, 0.9), "Open"),
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

/// A linked row's key: unique across the environment's hosts and repositories.
fn linked_key(environment_id: &str, link: &PullRequestLink) -> String {
    format!("{environment_id}:{}/{}#{}", link.host, link.repository, link.number)
}

/// What one `LinkRow` draws and acts on.
struct LinkedRow<'a> {
    line: &'a links::ListLine<'a>,
    /// The pull request in its project on the thread's environment.
    reference: Reference,
    thread_id: String,
    key: String,
    focus: Option<FocusHandle>,
    /// The Shift quick actions' subject and state, for a synced, unmerged link.
    speed: Option<(Rc<Entry>, RowSpeed)>,
    /// The environment can watch linked pull requests (`threadPullRequestWatch`).
    watchable: bool,
    now: i64,
}

/// The row menu's subject (`LinkRow`'s `Menu`).
#[derive(Clone)]
struct LinkMenu {
    url: String,
    reference: Reference,
    thread_id: String,
    /// `Some(watching)` when the environment watches and the change request is open.
    watch: Option<bool>,
    dismiss: bool,
}

/// Copy link, Open, Watch or Stop watching, and Unlink (Dismiss for a stack member).
fn link_menu(mut menu: PopupMenu, item: &LinkMenu, view: &Entity<Workspace>, window: &mut Window) -> PopupMenu {
    let url = item.url.clone();
    menu = menu.item(
        PopupMenuItem::new("Copy link")
            .icon(Lucide::Link)
            .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))),
    );
    let (url, reference) = (item.url.clone(), item.reference.clone());
    menu = menu.item(PopupMenuItem::new("Open").icon(Lucide::ArrowUpRight).on_click(window.listener_for(
        view,
        move |this, event: &ClickEvent, window, cx| {
            this.open_linked_pull_request(&url, reference.clone(), event, window, cx);
        },
    )));
    if let Some(watching) = item.watch {
        let (label, icon) =
            if watching { ("Stop watching", Lucide::EyeOff) } else { ("Watch for changes", Lucide::Eye) };
        let (thread_id, reference) = (item.thread_id.clone(), item.reference.clone());
        menu = menu.item(PopupMenuItem::new(label).icon(icon).on_click(window.listener_for(
            view,
            move |this, _, window, cx| {
                this.watch_linked_pull_request(&thread_id, &reference, !watching, window, cx);
            },
        )));
    }
    let label = if item.dismiss { "Dismiss from thread" } else { "Unlink from thread" };
    let (thread_id, reference) = (item.thread_id.clone(), item.reference.clone());
    menu.item(PopupMenuItem::new(label).icon(Lucide::Unlink2).on_click(window.listener_for(
        view,
        move |this, _, window, cx| {
            this.unlink_linked_pull_request(&thread_id, &reference, window, cx);
        },
    )))
}

impl Workspace {
    /// `useOpenPrLink`: in the thread's Pull request surface while "Open pull requests in T3
    /// Code" is on; Ctrl or Cmd, a middle click, or the setting off open the link instead.
    fn open_linked_pull_request(
        &mut self,
        url: &str,
        reference: Reference,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let modifiers = event.modifiers();
        let middle = matches!(event, ClickEvent::Mouse(click) if click.down.button == MouseButton::Middle);
        let external = modifiers.control || modifiers.platform || middle;
        if external || !self.client_settings.values.bool("pullRequestLinksOpenInApp") {
            self.open_link(url, event, window, cx);
        } else {
            self.open_surface(Surface::PullRequest(reference), window, cx);
        }
    }

    /// `thread.pull-request.watch`: the agent wakes, or stops waking, on the change request.
    fn watch_linked_pull_request(
        &mut self,
        thread_id: &str,
        reference: &Reference,
        watching: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let environment = &reference.environment_id;
        if !self.env_info(environment).is_some_and(|info| info.capability("threadPullRequestWatch")) {
            return;
        }
        let command = json!({
            "type": "thread.pull-request.watch", "commandId": super::super::new_id(), "threadId": thread_id,
            "host": reference.host, "repository": reference.repository, "number": reference.number,
            "watching": watching,
        });
        let title =
            if watching { "Could not watch the pull request" } else { "Could not stop watching the pull request" };
        self.dispatch_linked_command(environment, command, title, window, cx);
    }

    /// `thread.pull-request.unlink`: the link leaves the thread; a stack member stays dismissed.
    fn unlink_linked_pull_request(
        &mut self,
        thread_id: &str,
        reference: &Reference,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let command = json!({
            "type": "thread.pull-request.unlink", "commandId": super::super::new_id(), "threadId": thread_id,
            "host": reference.host, "repository": reference.repository, "number": reference.number,
        });
        self.dispatch_linked_command(
            &reference.environment_id,
            command,
            "Could not unlink the pull request",
            window,
            cx,
        );
    }

    /// Sends a link command to the thread's environment; the thread's next shell shows the result.
    fn dispatch_linked_command(
        &mut self,
        environment_id: &str,
        command: serde_json::Value,
        failure: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.env_client(environment_id) else {
            toast_error(window, cx, failure, "This environment is not connected.");
            return;
        };
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = client.request("orchestration.dispatchCommand", command).await {
                let _ = cx.update(|window, cx| toast_error(window, cx, failure, error.to_string()));
            }
        })
        .detach();
    }
}

fn hint(id: String, child: impl IntoElement, tip: impl Into<SharedString>) -> gpui_kit::Stateful<gpui_kit::Div> {
    let tip: SharedString = tip.into();
    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .flex()
        .items_center()
        .child(child)
        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
}

/// A `LinkRow`: state, number, title, the open change request's signals and diff, and its stack,
/// author, repository, branches and last update, indented by its depth in its chain. It opens in
/// the Pull request surface; its menu and Shift's quick actions act on it.
fn linked_row(row: LinkedRow, view: &Entity<Workspace>, t: &Tokens, window: &mut Window, cx: &mut App) -> AnyElement {
    let LinkedRow { line, reference, thread_id, key, focus, speed, watchable, now } = row;
    let link = line.link;
    let snapshot = link.snapshot.as_ref();
    let id = |part: &str| format!("linked-pr-{key}-{part}");
    let open = snapshot.is_none_or(|s| s.state == "open");
    let watching = link.watch.is_some();
    let (state_icon, color, state_label) = state_glyph(link, t);
    let title: SharedString = snapshot.map_or_else(|| link.repository.clone(), |s| s.title.clone()).into();
    let linked = crate::timestamp::parse_ms(&link.linked_at)
        .map_or_else(String::new, |at| format!(" · {}", relative_time(now - at)));
    let number_tip = format!("{}{linked}", source_label(&link.source));

    let mut signals = h_flex().flex_none().gap(px(4.));
    if open {
        if watching {
            signals = signals.child(hint(
                id("watching"),
                Icon::new(Lucide::Eye).with_size(px(14.)),
                "Watching: the agent wakes when checks finish, someone comments, or the branch conflicts",
            ));
        }
        if let Some((icon, color, label)) = snapshot.and_then(|s| checks_glyph(s.checks_state.as_deref()?, t)) {
            signals = signals.child(glyph(id("checks"), icon, 14., color, label));
        }
        if let Some((icon, color, label)) = snapshot.and_then(|s| review_glyph(s.review_decision.as_deref()?, t)) {
            signals = signals.child(glyph(id("review"), icon, 14., color, label));
        }
    }
    let stat = snapshot.filter(|s| s.additions + s.deletions > 0).map(|s| {
        let addition: Hsla = if t.dark { rgb(0x00d492).into() } else { rgb(0x007a55).into() };
        let stat = h_flex()
            .flex_none()
            .gap(px(4.))
            .font_family(t.fonts.mono)
            .text_size(px(11.))
            .child(div().text_color(addition).child(format!("+{}", s.additions)))
            .child(div().text_color(t.error).child(format!("-{}", s.deletions)));
        let subject = breakdown::Subject {
            environment_id: reference.environment_id.clone(),
            reference: reference.to_json(),
            additions: s.additions,
            deletions: s.deletions,
        };
        breakdown::breakdown_stat(id("breakdown"), stat, subject, view)
    });
    let title_line = h_flex()
        .min_w_0()
        .gap(px(6.))
        .child(hint(
            id("number"),
            div().font_family(t.fonts.mono).text_xs().text_color(t.muted_foreground).child(format!("#{}", link.number)),
            number_tip,
        ))
        .child(div().min_w_0().truncate().text_sm().child(title))
        .child(signals)
        .children(stat.map(|stat| h_flex().ml_auto().flex_none().child(stat)));

    let stack = line.stack.map(|(native, size)| {
        hint(
            id("stack"),
            h_flex()
                .gap(px(2.))
                .text_color(t.foreground.opacity(0.7))
                .child(Icon::new(Lucide::Layers).size(px(12.)))
                .child(size.to_string()),
            stack_tip(link, native, size),
        )
    });
    let author = snapshot.and_then(|s| s.author.as_ref()).map(|author| {
        let login = author.login.clone();
        let tip = match author.name.as_deref() {
            Some(name) if name != login => format!("{name} (@{login})"),
            _ => login.clone(),
        };
        let initial = login.chars().next().unwrap_or('?').to_uppercase().to_string();
        hint(
            id("author"),
            h_flex()
                .min_w_0()
                .gap(px(4.))
                .child(super::pull_request_detail::avatars::avatar_at(
                    author.avatar_url.as_deref(),
                    14.,
                    div()
                        .size(px(14.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(t.muted)
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(initial),
                ))
                .child(div().max_w(px(112.)).truncate().child(login)),
            tip,
        )
    });
    let place = match snapshot {
        Some(s) => h_flex()
            .min_w_0()
            .gap(px(6.))
            .child(
                hint(
                    id("repository"),
                    div().truncate().font_family(t.fonts.mono).child(link.repository.clone()),
                    link.repository.clone(),
                )
                .flex_shrink(1.)
                .min_w_0()
                .max_w(px(128.)),
            )
            .child(
                h_flex()
                    .min_w_0()
                    .gap(px(4.))
                    .font_family(t.fonts.mono)
                    .child(div().min_w_0().truncate().child(s.head_branch.clone()))
                    .child(div().flex_none().child("→"))
                    .child(div().flex_shrink(0.).max_w(relative(0.45)).truncate().child(s.base_branch.clone())),
            ),
        None => h_flex()
            .min_w_0()
            .child(div().truncate().font_family(t.fonts.mono).child(format!("{}/{}", link.host, link.repository))),
    };
    let updated = snapshot
        .and_then(|s| crate::timestamp::parse_ms(s.updated_at.as_deref()?))
        .map(|at| div().ml_auto().flex_none().whitespace_nowrap().child(relative_time(now - at)));
    let meta_line = h_flex()
        .min_w_0()
        .gap(px(6.))
        .overflow_hidden()
        .text_size(px(11.))
        .text_color(t.muted_foreground)
        .children(stack)
        .children(author)
        .child(place)
        .children(updated);

    let conflict = snapshot.filter(|s| s.conflicts()).map(|s| {
        let label = if s.base_branch.is_empty() {
            "Has conflicts".to_owned()
        } else {
            format!("Conflicts with {}", s.base_branch)
        };
        div().absolute().right(px(-4.)).bottom(px(-4.)).rounded_full().bg(t.background).child(glyph(
            id("conflict"),
            Lucide::TriangleAlert,
            12.,
            t.error,
            label,
        ))
    });

    let quick = speed.as_ref().and_then(|(entry, speed)| speed_actions::speed_group(entry, speed, view, t, window));
    let menu_hidden = speed.as_ref().is_some_and(|(_, speed)| speed.shown());
    let menu = LinkMenu {
        url: link.url.clone(),
        reference: reference.clone(),
        thread_id,
        watch: (watchable && open).then_some(watching),
        dismiss: link.source == "stack",
    };
    let group: SharedString = id("row").into();
    let focus_within = focus.as_ref().is_some_and(|f| f.contains_focused(window, cx));
    let more = (!menu_hidden).then(|| {
        let (menu, view) = (menu.clone(), view.clone());
        div()
            .absolute()
            .right_0()
            .bottom(px(2.))
            .pl(px(20.))
            .pr_1()
            .rounded(px(6.))
            .bg(t.background)
            .when(!focus_within, |el| el.invisible().group_hover(group.clone(), gpui_kit::Styled::visible))
            // The row under it must not open on this press.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                Button::new(ElementId::Name(id("menu").into()))
                    .ghost()
                    .xsmall()
                    .size(px(20.))
                    .p_0()
                    .tooltip(format!("Actions for #{}", link.number))
                    .child(Icon::new(Lucide::Ellipsis).with_size(px(14.)))
                    .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |popup: PopupMenu, window, _| {
                        link_menu(popup, &menu, &view, window)
                    }),
            )
    });

    let ring = focus.as_ref().filter(|f| f.is_focused(window) && window.last_input_was_keyboard()).map(|_| {
        // `focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-ring`
        vec![gpui_kit::BoxShadow::new(px(0.), px(0.), t.ring).spread_radius(px(1.)).inset()]
    });
    let hover = t.accent.opacity(0.6);
    let depth = line.depth.min(3) as f32;
    let url = link.url.clone();
    let context_view = view.clone();
    h_flex()
        .id(ElementId::Name(group.clone()))
        .group(group)
        .relative()
        .when_some(focus.clone(), |el, focus| el.track_focus(&focus))
        .when_some(ring, gpui_kit::Styled::shadow)
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
        .child(
            div()
                .relative()
                .flex_none()
                .w(px(16.))
                .child(glyph(id("state"), state_icon, 16., color, state_label))
                .children(conflict),
        )
        .child(v_flex().flex_1().min_w_0().child(title_line).child(meta_line))
        .children(quick)
        .children(more)
        .on_click(window.listener_for(view, move |this, event: &ClickEvent, window, cx| {
            if let Some(focus) = &focus {
                // Enter on a control inside the row is that control's.
                if event.is_keyboard() && !focus.is_focused(window) {
                    return;
                }
                window.focus(focus, cx);
            }
            this.open_linked_pull_request(&url, reference.clone(), event, window, cx);
        }))
        .context_menu(move |popup, window, _| link_menu(popup, &menu, &context_view, window))
        .into_any_element()
}
