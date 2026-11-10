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

use super::project_clone::paths;
use super::{Workspace, pages::Page};
use crate::desktop::links::{self, AppLink, LinkedPrompt, NewThreadLink, Repository};
use crate::desktop::settings::WindowRect;
use crate::desktop::window_state::{self, BoundsPersistence};
use crate::desktop::{self, Desktop};
use crate::model::{ProjectShell, ScopedId};
use crate::pull_request_detail::{CheckoutMode, CheckoutRequest, terminology};

/// `MAIN_WINDOW_BOUNDS_PERSIST_DEBOUNCE_MS`.
const BOUNDS_PERSIST_DEBOUNCE: Duration = Duration::from_millis(500);
/// How long a link waits for its cloned project to be listed (`waitForProject(…, 5_000)`).
const CLONED_PROJECT_WAIT: Duration = Duration::from_secs(5);
const CANNOT_CLONE: &str =
    "This machine cannot clone from a link. Update T3 Code on it, or clone the repository from the command palette.";

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
    /// The missing repository is cloning into `project`, chosen before the clone starts;
    /// `started` once the server took it, and the link resumes when the project is listed.
    Cloning {
        link: NewThreadLink,
        repository: Repository,
        project: ScopedId,
        started: bool,
    },
    /// The pull request has no thread yet; checking it out runs setup from its code, so it waits
    /// for the user (`PullRequestLinkCheckout`).
    PullRequestCheckout {
        pull_request: links::PullRequest,
        environment_id: SharedString,
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
    /// Gives up on a cloned project that is not listed in time.
    _clone_wait: Option<Task<()>>,
    /// Settings > About's `t3` command row.
    pub(super) cli: Option<crate::desktop::cli::CliState>,
    pub(super) cli_pending: bool,
    pub(super) _cli_task: Option<Task<()>>,
    /// Settings > Sharing & access: this app's server exposure.
    pub(super) exposure: Option<crate::desktop::exposure::ExposureView>,
    pub(super) exposure_pending: bool,
    pub(super) _exposure_task: Option<Task<()>>,
    /// Pairing links already added; a repeated click would only fail on the spent one-time token.
    paired_urls: std::collections::HashSet<String>,
}

impl DesktopShell {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> (Self, Vec<Subscription>) {
        let opened = desktop::normal_bounds(window.window_bounds());
        let desktop = cx.global::<Desktop>();
        let persistence = BoundsPersistence::new(&desktop.settings, desktop.initial, opened);
        let pending_links = desktop.launch_args.iter().filter_map(|arg| links::parse_app_link(arg)).collect();
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
            _clone_wait: None,
            cli: None,
            cli_pending: false,
            _cli_task: None,
            exposure: None,
            exposure_pending: false,
            _exposure_task: None,
            paired_urls: std::collections::HashSet::new(),
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

    /// A later launch's arguments (`second-instance`), once [`super::main_window::ensure`]
    /// brought this window forward: its app links open here.
    /// Links wait for the launch landing; setup must not take their place.
    pub(super) fn has_pending_links(&self) -> bool {
        !self.desktop.pending_links.is_empty()
    }

    pub(super) fn receive_launch(&mut self, args: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.desktop.pending_links.extend(args.iter().filter_map(|arg| links::parse_app_link(arg)));
        self.open_pending_links(window, cx);
    }

    /// URLs macOS opened with the app (`open-url`, and `open-file` for web pages): app links,
    /// and ordinary pages for the browser or the Pull requests page.
    pub(super) fn receive_opened_urls(&mut self, urls: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.desktop.pending_links.extend(urls.iter().filter_map(|url| links::parse_opened_url(url)));
        self.open_pending_links(window, cx);
    }

    /// Opens queued links and `t3 app` requests once the server has listed its projects; the
    /// shell's snapshots call this again.
    pub(super) fn open_pending_links(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.process_activations(window, cx);
        self.resume_link_clone(window, cx);
        if !self.landed || !self.links_ready() {
            return;
        }
        for link in std::mem::take(&mut self.desktop.pending_links) {
            self.open_app_link(link, window, cx);
        }
    }

    /// `DesktopWebLinkCoordinator`'s `ready`: the primary is connected with its config, or the
    /// local environment is off and links run against the saved environments. Each link then
    /// asks for its own environment's client.
    fn links_ready(&self) -> bool {
        matches!(self.connection, super::Connection::Off)
            || self.primary_environment().is_some_and(|primary| {
                self.env_client(primary).is_some() && self.env_info(primary).is_some_and(|i| !i.config.is_null())
            })
    }

    fn open_app_link(&mut self, link: AppLink, window: &mut Window, cx: &mut Context<Self>) {
        match link {
            AppLink::Pair { pairing_url } => self.pair_from_link(pairing_url, window, cx),
            AppLink::Web { url } => self.open_web_link(url, window, cx),
            AppLink::Thread { environment_id, thread_id, query } => {
                let listed = self.shell.threads.iter().any(|t| t.id == thread_id && t.environment_id == environment_id);
                if listed {
                    return self.open_linked_thread(environment_id.into(), thread_id, &query, window, cx);
                }
                // An older or not yet listed thread: its own load decides whether it exists.
                let Some(client) = self.env_client(&environment_id) else {
                    return toast_error(window, cx, "Could not open the link", "This thread is not on this machine.");
                };
                cx.spawn_in(window, async move |this, cx| {
                    let input = json!({"threadId": thread_id});
                    let found = client
                        .request("orchestration.getThreadProjection", input)
                        .await
                        .is_ok_and(|projection| projection["thread"]["deletedAt"].is_null());
                    let _ = this.update_in(cx, |this, window, cx| {
                        // The environment may have been removed while the lookup ran.
                        if found && this.session(&environment_id).is_some() {
                            this.open_linked_thread(environment_id.into(), thread_id, &query, window, cx);
                        } else {
                            toast_error(window, cx, "Could not open the link", "This thread is not on this machine.");
                        }
                    });
                })
                .detach();
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

    /// `DesktopWebLinkCoordinator.openLink` for a page: a pull request link opens in the Pull
    /// requests page when that setting is on (`openChangeRequest`), anything else in the open
    /// thread's browser. Only this machine serves files, so an HTML file needs a thread on it.
    fn open_web_link(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_change_request_link(&url, cx) {
            return;
        }
        let file = url::Url::parse(&url).ok().filter(|u| u.scheme() == "file").and_then(|u| u.to_file_path().ok());
        let primary = self.primary_environment().cloned();
        let thread =
            self.preview_thread().filter(|thread| file.is_none() || primary.as_ref() == Some(&thread.environment));
        let Some(thread) = thread else {
            return toast_error(window, cx, "Could not open the link", url);
        };
        match file {
            Some(path) => self.open_host_file_in_browser(&path.to_string_lossy(), window, cx),
            None => self.request_preview_tab(thread, Some(url), false, window, cx),
        }
    }

    fn open_linked_thread(
        &mut self,
        environment_id: SharedString,
        thread_id: String,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_thread_in(environment_id, thread_id, window, cx);
        match links::parse_thread_link_prompt(query) {
            Err(error) => toast_error(window, cx, "Could not open the linked prompt", error),
            Ok(Some(prompt)) => self.place_linked_prompt(prompt, Vec::new(), window, cx),
            Ok(None) => {}
        }
    }

    /// `pairFromLink`: any page can open a pairing link, so the user confirms the backend's
    /// origin before its one-time token is spent; the paired backend is saved like one added in
    /// Settings › Connections.
    fn pair_from_link(&mut self, pairing_url: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.desktop.paired_urls.contains(&pairing_url) {
            return;
        }
        let fail = |error: &str, window: &mut Window, cx: &mut gpui_kit::App| {
            toast_error(window, cx, "Could not add backend", error.to_owned());
        };
        let Some((host, code)) = crate::environments::parse_pairing_url(&pairing_url) else {
            return fail("Failed to add backend.", window, cx);
        };
        // The origin keeps the scheme visible, so a plain HTTP backend is not mistaken for HTTPS.
        let origin = crate::environments::resolve_pairing_host(&host, &code)
            .map_err(str::to_owned)
            .and_then(|base| url::Url::parse(&base).map_err(|error| error.to_string()))
            .map(|base| base.origin().ascii_serialization());
        let origin = match origin {
            Ok(origin) => origin,
            Err(error) => return fail(&error, window, cx),
        };
        let confirmed = super::confirm::confirm(&format!("Add the backend at {origin}?"), false, window, cx);
        let saved = self.saved_environments.clone();
        let primary = self.primary_environment().cloned();
        cx.spawn_in(window, async move |this, cx| {
            if !confirmed.await.unwrap_or(false) {
                return;
            }
            let catalog = saved.read_with(cx, |saved, _| saved.environments().to_vec());
            let paired = crate::runtime().spawn(crate::environments::pair(host, code, catalog, None)).await;
            let paired = paired.unwrap_or_else(|error| Err(error.to_string()));
            let saving = match paired {
                // The workspace's own environment is already here.
                Ok(paired) if primary.as_deref() == Some(paired.environment_id.as_str()) => None,
                Ok(paired) => Some(saved.update(cx, |saved, cx| saved.register(paired, true, cx))),
                Err(error) => {
                    let _ = this.update_in(cx, |_, window, cx| fail(&error, window, cx));
                    return;
                }
            };
            let saved = match saving {
                Some(saving) => saving.finished().await,
                None => Ok(()),
            };
            let _ = this.update_in(cx, |this, window, cx| match saved {
                Ok(()) => {
                    this.desktop.paired_urls.insert(pairing_url);
                    let toast = Notification::success("The environment is saved and will reconnect on app startup.")
                        .title("Backend added");
                    window.push_notification(toast, cx);
                }
                Err(error) => fail(&error, window, cx),
            });
        })
        .detach();
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
                match links::find_projects(&recent, repository)
                    .first()
                    .map(|p| (p.environment_id.clone(), p.id.clone()))
                {
                    Some((environment, project_id)) => {
                        self.open_link_draft(environment, project_id, &link, None, window, cx);
                    }
                    None => {
                        let repository = repository.clone();
                        self.show_link_page(LinkPage::MissingRepository { link, repository }, cx);
                    }
                }
            }
            NewThreadLink::PullRequest { pull_request, .. } => {
                if let Some(thread) = links::find_pull_request_thread(&self.shell.threads, pull_request) {
                    let (environment, thread_id) = (thread.environment_id.clone(), thread.id.clone());
                    self.open_link_thread(link, environment, thread_id, window, cx);
                    return;
                }
                let found = links::find_projects(&recent, &pull_request.repository);
                if found.is_empty() {
                    let repository = pull_request.repository.clone();
                    self.show_link_page(LinkPage::MissingRepository { link, repository }, cx);
                    return;
                }
                let (environment_id, project_id) = (found[0].environment_id.clone(), found[0].id.clone());
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
                                this.open_link_thread(link.clone(), environment, thread_id, window, cx);
                            }
                            None => {
                                if let Some(prompt) = prompt {
                                    warn_hidden_characters(prompt.hidden_characters, window, cx);
                                }
                                this.show_link_page(
                                    LinkPage::PullRequestCheckout {
                                        pull_request: pull_request.clone(),
                                        environment_id,
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

    /// The link's `thread` launch: the pull request's thread, with the link's prompt in its composer.
    fn open_link_thread(
        &mut self,
        link: NewThreadLink,
        environment: SharedString,
        thread_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = match &link {
            NewThreadLink::PullRequest { prompt, .. } => prompt.clone(),
            NewThreadLink::New { prompt, .. } => Some(prompt.clone()),
        };
        self.desktop.link_page = Some(LinkPage::Opened { link });
        self.select_thread_in(environment, thread_id, window, cx);
        if let Some(prompt) = prompt {
            self.place_linked_prompt(prompt, Vec::new(), window, cx);
        }
    }

    /// `openScratchDraft`: the "No project" workspace, with the pull request's URL when the
    /// link names one, on `scratchEnvironmentId(lastActiveEnvironmentId())`.
    fn open_scratch_draft(&mut self, link: NewThreadLink, window: &mut Window, cx: &mut Context<Self>) {
        let last_active = self.last_active_environment();
        let environment = self.scratch_environment_from(last_active.as_deref());
        let Some((environment, client)) =
            environment.and_then(|environment| Some((environment.clone(), self.env_client(&environment)?)))
        else {
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
                        this.open_link_draft(environment, project_id.to_owned(), &link, extra, window, cx);
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

    /// `lastActiveEnvironmentId`: the environment of the thread updated last, else the primary.
    fn last_active_environment(&self) -> Option<SharedString> {
        self.shell
            .threads
            .iter()
            .fold(None, |latest: Option<&crate::model::ThreadShell>, thread| match latest {
                Some(latest) if thread.updated_at <= latest.updated_at => Some(latest),
                _ => Some(thread),
            })
            .map(|thread| thread.environment_id.clone())
            .or_else(|| self.primary_environment().cloned())
    }

    /// `cloneRepository`: clones the link's GitHub repository on the last active environment,
    /// into its new-projects folder (its add-project base with the ghq layout), then resumes the
    /// link once the project is listed.
    fn clone_link_repository(
        &mut self,
        link: NewThreadLink,
        repository: Repository,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let environment = self.last_active_environment();
        let target = environment.and_then(|environment| {
            let info = self.env_info(&environment).filter(|info| !info.config.is_null())?;
            let root = info.new_projects_root.clone()?;
            let ghq = info.settings.get("ghqCloneLayout").as_bool() == Some(true);
            let base = info.settings.get("addProjectBaseDirectory").as_str().map(str::to_owned);
            Some((environment, root, ghq, base))
        });
        let Some((environment, root, ghq, base)) = target else {
            let page = LinkPage::Failed { title: "Could not open the link".into(), message: CANNOT_CLONE.into() };
            return self.show_link_page(page, cx);
        };
        let remote_url = format!("https://github.com/{}/{}.git", repository.owner, repository.name);
        let directory = if ghq { paths::add_project_initial_query(base.as_deref()) } else { root };
        let destination =
            paths::clone_destination_path(&directory, &paths::clone_directory_path(&remote_url, ghq, &remote_url));
        let project = ScopedId::new(environment, super::new_id());
        let input = json!({
            "projectId": project.id,
            "title": repository.name,
            "createdAt": crate::timestamp::iso(crate::timestamp::now_ms()),
            "remoteUrl": remote_url,
            "destinationPath": destination,
        });
        if !self.env_permits(&project.env, "projectClone.start", &input) {
            return toast_error(
                window,
                cx,
                "Clone unavailable",
                "This connection needs permission to write source control.",
            );
        }
        let Some(client) = self.env_client(&project.env) else {
            return toast_error(window, cx, "Clone failed", "This environment is not connected.");
        };
        let page = LinkPage::Cloning { link, repository, project: project.clone(), started: false };
        self.show_link_page(page, cx);
        self.desktop._link_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projectClone.start", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                // Another link may have replaced this one meanwhile.
                let Some(LinkPage::Cloning { link, repository, project: cloning, started }) =
                    &mut this.desktop.link_page
                else {
                    return;
                };
                if *cloning != project {
                    return;
                }
                match result {
                    Err(error) => {
                        let page = LinkPage::MissingRepository { link: link.clone(), repository: repository.clone() };
                        toast_error(window, cx, "Clone failed", error.message());
                        this.show_link_page(page, cx);
                    }
                    Ok(_) => {
                        *started = true;
                        this.suggest_app_access(&project.env, &remote_url, window, cx);
                        this.desktop._clone_wait = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(CLONED_PROJECT_WAIT).await;
                            let _ = this.update(cx, |this, cx| {
                                let waiting = matches!(
                                    &this.desktop.link_page,
                                    Some(LinkPage::Cloning { project: cloning, .. }) if *cloning == project
                                );
                                if waiting {
                                    this.link_failed("The clone started, but its project did not appear.", cx);
                                }
                            });
                        }));
                        this.resume_link_clone(window, cx);
                    }
                }
            });
        }));
    }

    /// A link waiting on its clone goes on once the project is listed: a draft in it, or, for a
    /// pull request, a note to open the link again when the clone is done.
    fn resume_link_clone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(LinkPage::Cloning { link, repository, project, started: true }) = &self.desktop.link_page else {
            return;
        };
        if !self.shell.projects.iter().any(|p| ScopedId::of_project(p) == *project) {
            return;
        }
        let (link, repository, project) = (link.clone(), repository.clone(), project.clone());
        self.desktop._clone_wait = None;
        match link {
            NewThreadLink::PullRequest { .. } => {
                let page = LinkPage::Failed {
                    title: format!("Cloning {}/{}", repository.owner, repository.name).into(),
                    message: "Open this link again when the clone finishes.".into(),
                };
                self.show_link_page(page, cx);
            }
            NewThreadLink::New { .. } => self.open_link_draft(project.env, project.id, &link, None, window, cx),
        }
    }

    fn link_failed(&mut self, message: &'static str, cx: &mut Context<Self>) {
        self.show_link_page(LinkPage::Failed { title: "Could not open the link".into(), message: message.into() }, cx);
    }

    /// `openDraft`: a new thread in the project with the link's model and prompt.
    fn open_link_draft(
        &mut self,
        environment: SharedString,
        project_id: String,
        link: &NewThreadLink,
        extra: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.desktop.link_page = Some(LinkPage::Opened { link: link.clone() });
        // A scratch project made just now may not be listed yet.
        self.start_draft_in(environment, project_id, window, cx);
        let mut notes = Vec::new();
        if let NewThreadLink::New { provider: Some(provider), model, .. } = link {
            let resolved = links::resolve_model(&self.server_info().providers, provider, model.as_deref());
            if let Some((selection, _)) = &resolved {
                self.model_override = Some(selection.clone());
                self.model_explicit = true;
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
        let Some(LinkPage::PullRequestCheckout { pull_request, environment_id, project_id, prompt, checking }) =
            &mut self.desktop.link_page
        else {
            return;
        };
        if *checking {
            return;
        }
        *checking = true;
        let request = CheckoutRequest {
            environment_id: environment_id.to_string(),
            project_id: project_id.clone(),
            url: pull_request.url(),
            term: terminology("github").0,
            task: prompt.clone().map(Into::into),
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
            LinkPage::Opening | LinkPage::Cloning { .. } => ("Opening the link...".into(), None, Vec::new()),
            LinkPage::Opened { link } => {
                let again = Button::new("link-open-again").outline().small().label("Open it again").on_click(
                    cx.listener(move |this, _, window, cx| this.launch_new_thread_link(link.clone(), window, cx)),
                );
                ("This link is open".into(), None, vec![again.into_any_element()])
            }
            LinkPage::Failed { title, message } => (title, Some(message), Vec::new()),
            LinkPage::MissingRepository { link, repository } => {
                let name = format!("{}/{}", repository.owner, repository.name);
                // Not gated, as the web's isn't: the handler answers a refused clone.
                let clone_link = link.clone();
                let clone = Button::new("link-clone").primary().small().label(format!("Clone {name}")).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.clone_link_repository(clone_link.clone(), repository.clone(), window, cx);
                    }),
                );
                let without =
                    Button::new("link-without-project").outline().small().label("Use without project").on_click(
                        cx.listener(move |this, _, window, cx| this.open_scratch_draft(link.clone(), window, cx)),
                    );
                (
                    format!("You don't have {name}").into(),
                    Some("Clone it to work on it here, or start without a project.".into()),
                    vec![clone.into_any_element(), without.into_any_element()],
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
