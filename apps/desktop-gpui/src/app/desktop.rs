//! The Workspace side of the desktop shell (`crate::desktop`): window bounds persistence and
//! app links handed over by the OS (`DesktopWebLinkCoordinator.tsx`, `NewThreadLinkView.tsx`,
//! `ThreadLinkPrompt.tsx`).

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FontWeight, IntoElement as _, ParentElement as _, SharedString, Styled as _, Subscription,
    Task, Window, div, px,
};
use serde_json::json;

use super::{Workspace, pages::Page};
use crate::desktop::links::{self, AppLink, LinkedPrompt, NewThreadLink, Repository};
use crate::desktop::settings::WindowRect;
use crate::desktop::window_state::{self, BoundsPersistence};
use crate::desktop::{self, Desktop};
use crate::model::ProjectShell;
use crate::pull_request_detail::{CheckoutMode, CheckoutRequest, terminology};

/// `MAIN_WINDOW_BOUNDS_PERSIST_DEBOUNCE_MS`.
const BOUNDS_PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);

/// Where an opened `/new` link stands (`NewThreadLinkView`'s `LinkState`).
#[derive(Clone)]
pub(super) enum LinkPage {
    Opening,
    Opened {
        link: NewThreadLink,
    },
    Failed {
        title: SharedString,
        message: SharedString,
    },
    MissingRepository {
        link: NewThreadLink,
        repository: Repository,
    },
    /// The pull request has no thread yet; checking it out runs setup from its code, so it waits
    /// for the user (`PullRequestLinkCheckout`).
    PullRequestCheckout {
        pull_request: links::PullRequest,
        project_id: String,
        prompt: Option<String>,
        checking: bool,
    },
}

pub(super) struct DesktopShell {
    persistence: BoundsPersistence,
    /// The last bounds seen and not yet written.
    pending: Option<(WindowRect, bool)>,
    _persist: Option<Task<()>>,
    /// Links that arrived before the first shell snapshot.
    pending_links: Vec<AppLink>,
    pub(super) link_page: Option<LinkPage>,
    _link_task: Option<Task<()>>,
    /// Settings > About's `t3` command row.
    pub(super) cli: Option<crate::desktop::cli::CliState>,
    pub(super) cli_pending: bool,
    pub(super) _cli_task: Option<Task<()>>,
    /// Settings > Sharing & access: this app's server exposure.
    pub(super) exposure: Option<crate::desktop::exposure::ExposureView>,
    pub(super) exposure_pending: bool,
    pub(super) _exposure_task: Option<Task<()>>,
    _launches: Task<()>,
}

impl DesktopShell {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> (Self, Vec<Subscription>) {
        let opened = desktop::normal_bounds(window.window_bounds());
        let desktop = cx.global::<Desktop>();
        let persistence = BoundsPersistence::new(&desktop.settings, desktop.initial, opened);
        let pending_links = desktop.launch_args.iter().filter_map(|arg| links::parse_app_link(arg)).collect();
        let receiver = desktop.launches.clone();
        let launches = cx.spawn_in(window, async move |this, cx| {
            while let Ok(args) = receiver.recv().await {
                if this.update_in(cx, |this, window, cx| this.receive_launch(args, window, cx)).is_err() {
                    return;
                }
            }
        });
        let subscriptions = vec![
            cx.observe_window_bounds(window, super::Workspace::window_bounds_changed),
            cx.on_app_quit(|this, cx| {
                this.desktop.flush_bounds(cx);
                async {}
            }),
        ];
        let shell = Self {
            persistence,
            pending: None,
            _persist: None,
            pending_links,
            link_page: None,
            _link_task: None,
            cli: None,
            cli_pending: false,
            _cli_task: None,
            exposure: None,
            exposure_pending: false,
            _exposure_task: None,
            _launches: launches,
        };
        (shell, subscriptions)
    }

    fn flush_bounds(&mut self, cx: &mut Context<Workspace>) {
        self._persist = None;
        let Some((rect, maximized)) = self.pending.take() else { return };
        let desktop = cx.global_mut::<Desktop>();
        if window_state::record(&mut desktop.settings, rect, maximized) {
            desktop.settings.save();
        }
    }
}

fn toast_error(window: &mut Window, cx: &mut gpui_kit::App, title: &str, message: impl Into<SharedString>) {
    window.push_notification(Notification::error(message).title(SharedString::from(title.to_owned())), cx);
}

impl Workspace {
    fn window_bounds_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = desktop::normal_bounds(window.window_bounds());
        if !self.desktop.persistence.should_save(current) {
            return;
        }
        self.desktop.pending = Some(current);
        // Replacing the task cancels the previous wait.
        self.desktop._persist = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(BOUNDS_PERSIST_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.desktop.flush_bounds(cx));
        }));
    }

    /// A later launch's arguments (`second-instance`): its app links open here, and the window
    /// comes forward either way.
    fn receive_launch(&mut self, args: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        window.activate_window();
        self.desktop.pending_links.extend(args.iter().filter_map(|arg| links::parse_app_link(arg)));
        self.open_pending_links(window, cx);
    }

    /// Opens queued links once the server has listed its projects; the shell's first
    /// snapshot calls this again.
    pub(super) fn open_pending_links(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.landed || self.client().is_none() {
            return;
        }
        for link in std::mem::take(&mut self.desktop.pending_links) {
            self.open_app_link(link, window, cx);
        }
    }

    fn open_app_link(&mut self, link: AppLink, window: &mut Window, cx: &mut Context<Self>) {
        match link {
            // Not wired to saved environments yet; Settings › Connections pairs by host and code.
            AppLink::Pair { .. } => toast_error(window, cx, "Could not add backend", "Failed to add backend."),
            AppLink::Thread { environment_id, thread_id, query } => {
                let listed = self.shell.threads.iter().any(|t| t.id == thread_id && t.environment_id == environment_id);
                if !listed {
                    toast_error(window, cx, "Could not open the link", "This thread is not on this machine.");
                    return;
                }
                self.select_thread_in(environment_id.into(), thread_id, window, cx);
                match links::parse_thread_link_prompt(&query) {
                    Err(error) => toast_error(window, cx, "Could not open the linked prompt", error),
                    Ok(Some(prompt)) => self.place_linked_prompt(prompt, Vec::new(), window, cx),
                    Ok(None) => {}
                }
            }
            AppLink::New { query } => match links::parse_new_thread_link(&query) {
                Err(message) => self.show_link_page(
                    LinkPage::Failed { title: "Could not open the link".into(), message: message.into() },
                    cx,
                ),
                Ok(link) => self.launch_new_thread_link(link, window, cx),
            },
        }
    }

    fn show_link_page(&mut self, state: LinkPage, cx: &mut Context<Self>) {
        self.desktop.link_page = Some(state);
        if self.page != Some(Page::Link) {
            self.open_page(Page::Link, cx);
        }
        cx.notify();
    }

    /// `NewThreadLinkView`'s launch decision: the pull request's thread, a draft in the
    /// repository's most recent project, or a draft without a project.
    fn launch_new_thread_link(&mut self, link: NewThreadLink, window: &mut Window, cx: &mut Context<Self>) {
        // Every machine's "No project" folder is left out, not only the open one's.
        let scratch = |p: &ProjectShell| {
            self.env_info(&p.environment_id).and_then(|info| info.scratch_workspace_root.as_deref())
                == Some(p.workspace_root.as_str())
        };
        let recent: Vec<_> = crate::inbox::projects_by_recency(&self.shell.projects, &self.shell.threads)
            .into_iter()
            .filter(|p| !scratch(p))
            .collect();
        match &link {
            NewThreadLink::New { repository: None, .. } => self.open_scratch_draft(link, window, cx),
            NewThreadLink::New { repository: Some(repository), .. } => {
                match links::find_projects(&recent, repository).first().map(|p| p.id.clone()) {
                    Some(project_id) => self.open_link_draft(project_id, &link, None, window, cx),
                    None => {
                        let repository = repository.clone();
                        self.show_link_page(LinkPage::MissingRepository { link, repository }, cx);
                    }
                }
            }
            NewThreadLink::PullRequest { pull_request, .. } => {
                let found = links::find_projects(&recent, &pull_request.repository);
                if found.is_empty() {
                    let repository = pull_request.repository.clone();
                    self.show_link_page(LinkPage::MissingRepository { link, repository }, cx);
                    return;
                }
                let project_id = found[0].id.clone();
                // Each project asks the machine it lives on.
                let projects: Vec<_> = found
                    .iter()
                    .filter_map(|p| Some((p.id.clone(), p.environment_id.clone(), self.env_client(&p.environment_id)?)))
                    .collect();
                if projects.is_empty() {
                    return;
                }
                self.show_link_page(LinkPage::Opening, cx);
                let repository = format!("{}/{}", pull_request.repository.owner, pull_request.repository.name);
                let number = pull_request.number;
                self.desktop._link_task = Some(cx.spawn_in(window, async move |this, cx| {
                    // A pull request's thread can be an older settled thread the shell no longer holds.
                    let mut found = None;
                    for (project_id, environment, client) in projects {
                        let input = json!({"projectId": project_id, "repository": repository, "number": number});
                        let Ok(answer) = client.request("pullRequests.linkedThreads", input).await else { continue };
                        found = answer["threads"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|t| t["archivedAt"].is_null())
                            .and_then(|t| t["id"].as_str().map(|id| (environment, id.to_owned())));
                        if found.is_some() {
                            break;
                        }
                    }
                    let _ = this.update_in(cx, |this, window, cx| {
                        let NewThreadLink::PullRequest { pull_request, prompt } = &link else { return };
                        match found {
                            Some((environment, thread_id)) => {
                                this.desktop.link_page = Some(LinkPage::Opened { link: link.clone() });
                                let prompt = prompt.clone();
                                this.select_thread_in(environment, thread_id, window, cx);
                                if let Some(prompt) = prompt {
                                    this.place_linked_prompt(prompt, Vec::new(), window, cx);
                                }
                            }
                            None => {
                                if let Some(prompt) = prompt {
                                    warn_hidden_characters(prompt.hidden_characters, window, cx);
                                }
                                this.show_link_page(
                                    LinkPage::PullRequestCheckout {
                                        pull_request: pull_request.clone(),
                                        project_id,
                                        prompt: prompt.as_ref().map(|p| p.text.clone()),
                                        checking: false,
                                    },
                                    cx,
                                );
                            }
                        }
                    });
                }));
            }
        }
    }

    /// `openScratchDraft`: the "No project" workspace, with the pull request's URL when the
    /// link names one.
    fn open_scratch_draft(&mut self, link: NewThreadLink, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client().filter(|_| self.server_info().scratch_workspace_root.is_some()) else {
            let message = "None of your machines can start a thread without a project right now.";
            self.show_link_page(
                LinkPage::Failed { title: "Could not open the link".into(), message: message.into() },
                cx,
            );
            return;
        };
        self.show_link_page(LinkPage::Opening, cx);
        self.desktop._link_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.ensureScratch", json!({})).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(answer) => match answer["projectId"].as_str() {
                    Some(project_id) => {
                        let extra = match &link {
                            NewThreadLink::PullRequest { pull_request, .. } => Some(pull_request.url()),
                            NewThreadLink::New { .. } => None,
                        };
                        this.open_link_draft(project_id.to_owned(), &link, extra, window, cx);
                    }
                    None => this.link_failed("T3 Code could not open a new thread.", cx),
                },
                Err(error) => {
                    toast_error(window, cx, "Could not open the link", error.to_string());
                    this.link_failed("None of your machines can start a thread without a project right now.", cx);
                }
            });
        }));
    }

    fn link_failed(&mut self, message: &'static str, cx: &mut Context<Self>) {
        self.show_link_page(LinkPage::Failed { title: "Could not open the link".into(), message: message.into() }, cx);
    }

    /// `openDraft`: a new thread in the project with the link's model and prompt.
    fn open_link_draft(
        &mut self,
        project_id: String,
        link: &NewThreadLink,
        extra: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.desktop.link_page = Some(LinkPage::Opened { link: link.clone() });
        self.start_draft(project_id, window, cx);
        let mut notes = Vec::new();
        if let NewThreadLink::New { provider: Some(provider), model, .. } = link {
            let resolved = links::resolve_model(&self.server_info().providers, provider, model.as_deref());
            if let Some((selection, _)) = &resolved {
                self.model_override = Some(selection.clone());
            }
            match resolved {
                None => notes.push(format!(
                    "The link asked for {provider}, which is not set up here, so your default model is used."
                )),
                Some((_, false)) => notes.push(format!(
                    "The link asked for {}, which {provider} does not offer here, so its default model is used.",
                    model.as_deref().unwrap_or_default()
                )),
                Some((_, true)) => {}
            }
        }
        let prompt = match link {
            NewThreadLink::New { prompt, .. } => Some(prompt),
            NewThreadLink::PullRequest { prompt, .. } => prompt.as_ref(),
        };
        let text = prompt.map(|p| p.text.clone()).into_iter().chain(extra).collect::<Vec<_>>().join("\n\n");
        if text.is_empty() {
            return;
        }
        let hidden_characters = prompt.map_or(0, |p| p.hidden_characters);
        self.place_linked_prompt(LinkedPrompt { text, hidden_characters }, notes, window, cx);
    }

    /// `placeLinkedPrompt`: fills the composer, or asks before replacing a draft already there.
    fn place_linked_prompt(
        &mut self,
        prompt: LinkedPrompt,
        notes: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        warn_hidden_characters(prompt.hidden_characters, window, cx);
        let description: SharedString =
            std::iter::once("Read it before you send.".to_owned()).chain(notes).collect::<Vec<_>>().join(" ").into();
        let existing = self.composer.read(cx).value().to_string();
        if existing.trim().is_empty() || existing == prompt.text {
            self.set_composer_text(prompt.text, window, cx);
            window.push_notification(Notification::info(description).title("Prompt added from a link"), cx);
            return;
        }
        let target = self.target.clone();
        let workspace = cx.weak_entity();
        let text: SharedString = prompt.text.into();
        let toast = Notification::info("This thread already has a draft. Replace it with the linked prompt?")
            .title("Linked prompt ready")
            .autohide(false)
            .action(move |_, _, cx| {
                let (workspace, target, text, description) =
                    (workspace.clone(), target.clone(), text.clone(), description.clone());
                Button::new("replace-draft").label("Replace draft").on_click(cx.listener(
                    move |toast, _, window, cx| {
                        toast.dismiss(window, cx);
                        let _ = workspace.update(cx, |this, cx| {
                            // The draft belongs to the thread the link opened.
                            if this.target != target {
                                return;
                            }
                            this.set_composer_text(text.clone(), window, cx);
                            window.push_notification(
                                Notification::info(description.clone()).title("Prompt added from a link"),
                                cx,
                            );
                        });
                    },
                ))
            });
        window.push_notification(toast, cx);
    }

    /// `PullRequestLinkCheckout`'s Check out: a worktree of the pull request with a draft on it,
    /// the link's prompt in its composer.
    fn check_out_linked_pull_request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(LinkPage::PullRequestCheckout { pull_request, project_id, prompt, checking }) =
            &mut self.desktop.link_page
        else {
            return;
        };
        if *checking {
            return;
        }
        *checking = true;
        let request = CheckoutRequest {
            project_id: project_id.clone(),
            url: pull_request.url(),
            term: terminology("github").0,
            task: prompt.clone(),
            mode: CheckoutMode::Worktree,
        };
        let settled = |this: &mut Workspace| {
            if let Some(LinkPage::PullRequestCheckout { checking, .. }) = &mut this.desktop.link_page {
                *checking = false;
            }
        };
        self.prepare_pr_checkout(request, settled, window, cx);
        cx.notify();
    }

    /// `LinkPage`: the open link's state, centered in the main column.
    pub(super) fn render_link_page(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *crate::theme::tokens(cx);
        let (title, description, buttons): (SharedString, Option<SharedString>, Vec<AnyElement>) = match self
            .desktop
            .link_page
            .clone()
            .unwrap_or(LinkPage::Opening)
        {
            LinkPage::Opening => ("Opening the link...".into(), None, Vec::new()),
            LinkPage::Opened { link } => {
                let again = Button::new("link-open-again").outline().small().label("Open it again").on_click(
                    cx.listener(move |this, _, window, cx| this.launch_new_thread_link(link.clone(), window, cx)),
                );
                ("This link is open".into(), None, vec![again.into_any_element()])
            }
            LinkPage::Failed { title, message } => (title, Some(message), Vec::new()),
            LinkPage::MissingRepository { link, repository } => {
                let name = format!("{}/{}", repository.owner, repository.name);
                let without =
                    Button::new("link-without-project").outline().small().label("Use without project").on_click(
                        cx.listener(move |this, _, window, cx| this.open_scratch_draft(link.clone(), window, cx)),
                    );
                (
                    format!("You don't have {name}").into(),
                    Some("Clone it to work on it here, or start without a project.".into()),
                    vec![without.into_any_element()],
                )
            }
            LinkPage::PullRequestCheckout { pull_request, checking, .. } => {
                let check_out = Button::new("link-check-out")
                    .primary()
                    .small()
                    .disabled(checking)
                    .label(if checking { "Checking out..." } else { "Check out" })
                    .on_click(cx.listener(|this, _, window, cx| this.check_out_linked_pull_request(window, cx)));
                let Repository { owner, name } = &pull_request.repository;
                (
                    format!("Check out {owner}/{name}#{}?", pull_request.number).into(),
                    Some(
                        "This puts the pull request's code in a new worktree and runs your project's setup script on it."
                            .into(),
                    ),
                    vec![check_out.into_any_element()],
                )
            }
        };
        v_flex()
            .size_full()
            .min_h_0()
            .items_center()
            .justify_center()
            .p(px(48.))
            .text_center()
            .child(
                v_flex()
                    .max_w(px(448.))
                    .items_center()
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(title))
                    .children(
                        description.map(|text| div().mt(px(4.)).text_sm().text_color(t.muted_foreground).child(text)),
                    )
                    .when_some((!buttons.is_empty()).then_some(buttons), |el, buttons| {
                        el.child(h_flex().mt(px(20.)).justify_center().gap(px(8.)).children(buttons))
                    }),
            )
            .into_any_element()
    }
}

/// `warnAboutHiddenCharacters`: a linked prompt lost characters that would not show.
fn warn_hidden_characters(count: usize, window: &mut Window, cx: &mut gpui_kit::App) {
    if count == 0 {
        return;
    }
    let message = format!(
        "T3 Code removed {count} hidden character{}. Read the prompt before you send it.",
        if count == 1 { "" } else { "s" }
    );
    let warning =
        Notification::warning(message).title("This prompt had hidden characters, it may be malicious").autohide(false);
    window.push_notification(warning, cx);
}
