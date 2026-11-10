//! The Welcome page's Agents band (`WelcomePage.tsx`: `AgentsSection`, `AgentRow`,
//! `OnboardingCodexSetup`, `AgentInstallTerminal`): the computer's agents and their states,
//! turning one on, Codex's setup card, and a terminal that installs or signs in a CLI.
//!
//! The band's state lives while the page shows, as the web component's does: leaving the page
//! closes its setup terminal.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, Entity, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div,
    px, svg,
};
use serde_json::json;

use super::welcome::{self, AgentState, CliStep, RowAction};
use crate::app::Workspace;
use crate::app::pages::settings::connections::ui::{Size, Variant, WebButton};
use crate::app::pages::settings::{ModeChange, codex_setup_managed, provider_summary, with_setup_mode};
use crate::app::terminal::{Launch, SessionEvent, TerminalSession};
use crate::model::Provider;
use crate::rpc::{RpcClient, RpcError};
use crate::theme::Tokens;

/// `AGENT_ONBOARDING_THREAD_ID`: setup terminals belong to no thread.
const SETUP_THREAD_ID: &str = "onboarding-agent-setup";
const PROVIDERS_MANAGE: &str = "providers:manage";
const TERMINAL_OPERATE: &str = "terminal:operate";

#[derive(Default)]
pub(super) struct AgentsStage {
    /// The computer the band shows; another one starts it over.
    environment: Option<SharedString>,
    /// `moreOpen`: `None` until the user, or the first finished check that finds nothing, decides.
    more_open: Option<bool>,
    /// The Codex instance whose setup card is open.
    codex_setup: Option<String>,
    /// Its last listed status: a rebuild briefly drops it, and a sign-in must not be lost.
    last_codex: Option<Provider>,
    /// "Use existing CLI" opens this instance's terminal once Codex has checked the CLI.
    /// Kept with the choice that asked, so only that choice's failed save takes it back.
    cli_terminal_for: Option<(String, u64)>,
    /// Each Codex choice bumps it; it outlives a reset, so a save from before one stays older.
    codex_choice: u64,
    terminal: Option<SetupTerminal>,
}

impl AgentsStage {
    /// The band shown afresh for `environment`, keeping the choice count.
    fn reset(&mut self, environment: Option<SharedString>) {
        *self = Self { environment, codex_choice: self.codex_choice, ..Self::default() };
    }

    /// A choice on `instance_id`'s Codex card, returning its generation. "Use existing CLI" asks
    /// for the CLI's terminal and ChatGPT takes that back (`onUseCliChange`).
    fn choose_codex_mode(&mut self, instance_id: &str, existing: bool) -> u64 {
        self.codex_choice += 1;
        self.cli_terminal_for = existing.then(|| (instance_id.to_owned(), self.codex_choice));
        self.codex_choice
    }

    /// A save that failed takes back the terminal its own choice asked for, not a later one's.
    fn codex_save_failed(&mut self, choice: u64, instance_id: &str) {
        self.cli_terminal_for.take_if(|(id, asked)| id == instance_id && *asked == choice);
    }

    /// The setup terminal while `attempt` of `terminal_id` is its latest open: a replacement
    /// counts its attempts from one again.
    fn setup_attempt(&mut self, terminal_id: &str, attempt: u64) -> Option<&mut SetupTerminal> {
        self.terminal.as_mut().filter(|t| t.terminal_id == terminal_id && t.attempt == attempt)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupState {
    Preparing,
    Ready,
    OpenFailed,
    WriteFailed,
}

/// `AgentInstallTerminal`'s session: a PTY with the command typed in, waiting for Enter.
struct SetupTerminal {
    environment: SharedString,
    instance_id: String,
    cwd: String,
    command: String,
    terminal_id: String,
    state: SetupState,
    /// Bumped by each open (`setupGeneration`): a late result of an earlier one is dropped.
    attempt: u64,
    /// `terminal.open` and typing the command; a later step or a close waits for it.
    task: Option<Task<()>>,
    view: Option<(Entity<TerminalSession>, Subscription)>,
}

/// One agent row as the band shows it.
struct Row {
    provider: Provider,
    name: String,
    state: AgentState,
    action: RowAction,
    detail: Option<String>,
}

impl Row {
    /// `AgentRow`'s detail line; a compact row's button carries its state.
    fn new(provider: &Provider, info: &crate::model::ServerInfo, compact: bool) -> Self {
        let settings = &info.settings;
        let state = welcome::agent_state(provider, settings);
        let json = info.providers_json.iter().find(|p| p["instanceId"] == provider.instance_id.as_str());
        let text = |key: &str| json.and_then(|p| p[key].as_str()).filter(|s| !s.is_empty()).map(str::to_owned);
        let broken = welcome::is_broken_version(provider)
            .then(|| provider.compatibility_advisory.as_ref()?.get("message")?.as_str().map(str::to_owned))
            .flatten();
        let headline = || provider_summary(json).0;
        let detail = match state {
            AgentState::Install => (!welcome::installable(&provider.driver)).then(|| "Not installed".to_owned()),
            _ if compact || matches!(state, AgentState::Ready | AgentState::Checking) => None,
            AgentState::Attention => {
                Some(text("unavailableReason").or(broken).or_else(|| provider.message.clone()).unwrap_or_else(headline))
            }
            _ => Some(broken.unwrap_or_else(headline)),
        };
        Self {
            provider: provider.clone(),
            name: welcome::agent_name(provider),
            state,
            action: welcome::row_action(provider, settings),
            detail,
        }
    }
}

/// `WelcomeSection`: one band, its label left of its rows (stacked below `sm`).
pub(super) fn welcome_section(title: &'static str, wide: bool, t: &Tokens, content: impl gpui_kit::IntoElement) -> Div {
    let label = div().text_sm().line_height(px(40.)).text_color(t.muted_foreground).child(title);
    div()
        .flex()
        .when(wide, |el| el.flex_row().gap(px(40.)))
        .when(!wide, gpui_kit::Styled::flex_col)
        .py(px(16.))
        .border_b_1()
        .border_color(t.border.opacity(0.6))
        .child(label.when(wide, |el| el.w(px(96.)).flex_none()))
        .child(div().flex_1().min_w_0().child(content))
}

/// `RowIcon`: the icon column names, checkboxes and steps line up on.
fn row_icon(child: impl gpui_kit::IntoElement, t: &Tokens) -> Div {
    div().size(px(20.)).flex_none().flex().items_center().justify_center().text_color(t.muted_foreground).child(child)
}

/// `ProviderInstanceIcon`; `muted` draws the fold's quiet preview.
fn provider_glyph(driver: &str, size: f32, muted: bool, t: &Tokens) -> AnyElement {
    match crate::assets::provider_icon(driver) {
        Some((path, tint)) => {
            let color = if muted { t.muted_foreground } else { tint.unwrap_or(t.foreground) };
            svg().path(path).size(px(size)).flex_none().text_color(color).into_any_element()
        }
        None => Icon::new(Lucide::Bot).size(px(size)).text_color(t.muted_foreground).into_any_element(),
    }
}

fn ready_badge(t: &Tokens) -> AnyElement {
    h_flex()
        .gap(px(6.))
        .text_xs()
        .line_height(px(16.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.success_foreground)
        .child(Icon::new(Lucide::Check).size(px(14.)))
        .child("Ready")
        .into_any_element()
}

impl Workspace {
    /// Leaving the Welcome page (for another page or Settings) ends its band.
    pub(in crate::app) fn sync_welcome_agents(&mut self, cx: &mut Context<Self>) {
        if !(self.welcome_open() && self.settings.is_none()) && self.onboarding.agents.environment.is_some() {
            self.close_setup_terminal(cx);
            self.onboarding.agents.reset(None);
        }
    }

    /// A reconnected computer's setup terminal attaches again, as the drawer's panes do.
    pub(in crate::app) fn reattach_setup_terminal(
        &mut self,
        environment: &SharedString,
        client: &RpcClient,
        cx: &mut Context<Self>,
    ) {
        let terminal = self.onboarding.agents.terminal.as_ref().filter(|t| t.environment == *environment);
        if let Some((session, _)) = terminal.and_then(|t| t.view.as_ref()) {
            session.update(cx, |session, cx| session.attach(client.clone(), cx));
        }
    }

    /// Mounting the band asks the computer to check its agents again.
    fn mount_agents(&mut self, environment: &SharedString, cx: &mut Context<Self>) {
        if self.onboarding.agents.environment.as_ref() == Some(environment) {
            return;
        }
        self.close_setup_terminal(cx);
        self.onboarding.agents.reset(Some(environment.clone()));
        self.refresh_agents(environment, None, cx);
    }

    /// `serverEnvironment.refreshProviders`, for one instance or all.
    fn refresh_agents(&self, environment: &str, instance_id: Option<&str>, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(environment) else { return };
        let input = instance_id.map_or_else(|| json!({}), |id| json!({ "instanceId": id }));
        cx.background_spawn(async move {
            if let Err(error) = client.request("server.refreshProviders", input).await {
                log::debug!("provider refresh failed: {error}");
            }
        })
        .detach();
    }

    /// `turnOn`: one-instance upsert, so a provider another client added meanwhile is kept.
    fn turn_on_agent(
        &mut self,
        environment: SharedString,
        instance_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = self.env_info(&environment).filter(|info| !info.config.is_null()) else { return };
        let Some(provider) = info.provider(&instance_id) else { return };
        let instance =
            welcome::turned_on_instance(&info.settings["providerInstances"][instance_id.as_str()], &provider.driver);
        let name = welcome::agent_name(provider);
        let client = self.env_client(&environment).filter(|_| self.env_grants(&environment, PROVIDERS_MANAGE));
        let Some(client) = client else { return };
        let payload = json!({
            "patch": {},
            "providerInstanceMutation": { "operation": "upsert", "instanceId": instance_id, "instance": instance },
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", payload).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => this.refresh_agents(&environment, Some(&instance_id), cx),
                Err(RpcError::Disconnected(_)) => {}
                Err(error) => window.push_notification(
                    Notification::error(error.to_string()).title(format!("Could not turn on {name}")),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// `OnboardingCodexSetup`'s `changeMode`: "Use existing CLI" also asks for the CLI's terminal.
    fn change_codex_mode(
        &mut self,
        environment: &SharedString,
        instance_id: &str,
        mode: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.onboarding.agents.environment.as_ref() != Some(environment) {
            return;
        }
        let choice = self.onboarding.agents.choose_codex_mode(instance_id, mode == "existing");
        cx.notify();
        let Some(info) = self.env_info(environment) else { return };
        let Some(provider) = info.provider(instance_id) else { return };
        let instance = &info.settings["providerInstances"][instance_id];
        let current = if codex_setup_managed(&instance["config"]) { "managed" } else { "existing" };
        if (current == mode && provider.enabled) || !self.env_grants(environment, PROVIDERS_MANAGE) {
            return;
        }
        let next = with_setup_mode(instance, &provider.driver, mode);
        let Some(client) = self.env_client(environment) else { return };
        let payload = json!({
            "patch": {},
            "providerInstanceMutation": { "operation": "upsert", "instanceId": instance_id, "instance": next },
        });
        let instance_id = instance_id.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let Err(error) = client.request("server.updateSettings", payload).await else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                this.onboarding.agents.codex_save_failed(choice, &instance_id);
                if !matches!(error, RpcError::Disconnected(_)) {
                    let toast = Notification::error(error.to_string()).title("Could not save Codex setup");
                    window.push_notification(toast, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `openTerminal`: the CLI's sign-in when it is installed, else its native installer.
    fn open_setup_terminal(&mut self, environment: &SharedString, provider: &Provider, cx: &mut Context<Self>) {
        let Some(info) = self.env_info(environment).filter(|info| !info.config.is_null()) else { return };
        if !welcome::installable(&provider.driver) || !self.env_grants(environment, TERMINAL_OPERATE) {
            return;
        }
        let os = info.platform_os.as_deref().unwrap_or("unknown");
        let command = if provider.installed {
            welcome::login_command(provider, &info.settings, os)
        } else {
            welcome::install_command(&provider.driver, os).to_owned()
        };
        let cwd = info.config["cwd"].as_str().unwrap_or_default().to_owned();
        self.close_setup_terminal(cx);
        self.onboarding.agents.terminal = Some(SetupTerminal {
            environment: environment.clone(),
            instance_id: provider.instance_id.clone(),
            cwd,
            command,
            terminal_id: format!("onboarding-{}-{}", provider.driver, crate::app::new_id()),
            state: SetupState::Preparing,
            attempt: 0,
            task: None,
            view: None,
        });
        self.prepare_setup_terminal(false, cx);
    }

    /// Opens the PTY and types the command into it; a retry closes the failed one first.
    fn prepare_setup_terminal(&mut self, retry: bool, cx: &mut Context<Self>) {
        let Some(terminal) = self.onboarding.agents.terminal.as_mut() else { return };
        terminal.attempt += 1;
        terminal.state = SetupState::Preparing;
        terminal.view = None;
        let attempt = (terminal.terminal_id.clone(), terminal.attempt);
        let id = json!({ "threadId": SETUP_THREAD_ID, "terminalId": terminal.terminal_id });
        let mut open = id.clone();
        open["cwd"] = terminal.cwd.clone().into();
        open["providerInstanceId"] = terminal.instance_id.clone().into();
        let mut write = id.clone();
        write["data"] = terminal.command.clone().into();
        let mut close = id;
        close["deleteHistory"] = true.into();
        let previous = terminal.task.take();
        terminal.task = Some(cx.spawn(async move |this, cx| {
            if let Some(previous) = previous {
                previous.await;
            }
            let step = |failed| {
                let attempt = attempt.clone();
                move |this: &mut Self, cx: &mut Context<Self>| this.setup_client(&attempt, failed, cx)
            };
            let Ok(Some(client)) = this.update(cx, step(SetupState::OpenFailed)) else { return };
            if retry {
                let _ = client.request("terminal.close", close).await;
            }
            if client.request("terminal.open", open).await.is_err() {
                let _ = this.update(cx, |this, cx| this.settle_setup(&attempt, SetupState::OpenFailed, cx));
                return;
            }
            let Ok(Some(client)) = this.update(cx, step(SetupState::WriteFailed)) else { return };
            let state = match client.request("terminal.write", write).await {
                Ok(_) => SetupState::Ready,
                Err(_) => SetupState::WriteFailed,
            };
            let _ = this.update(cx, |this, cx| this.settle_setup(&attempt, state, cx));
        }));
        cx.notify();
    }

    /// The socket for the next step while `attempt` is current and terminals may be operated;
    /// otherwise the step fails as `failed` (a revoked grant) or quietly ends (a newer attempt).
    fn setup_client(
        &mut self,
        attempt: &(String, u64),
        failed: SetupState,
        cx: &mut Context<Self>,
    ) -> Option<RpcClient> {
        let environment = self.onboarding.agents.setup_attempt(&attempt.0, attempt.1)?.environment.clone();
        let client = self.env_client(&environment).filter(|_| self.env_grants(&environment, TERMINAL_OPERATE));
        if client.is_none() {
            self.settle_setup(attempt, failed, cx);
        }
        client
    }

    fn settle_setup(&mut self, attempt: &(String, u64), state: SetupState, cx: &mut Context<Self>) {
        if let Some(terminal) = self.onboarding.agents.setup_attempt(&attempt.0, attempt.1) {
            terminal.state = state;
            cx.notify();
        }
    }

    /// Ends the setup terminal; its PTY closes after any open still on its way, and only while
    /// this connection may operate terminals (a revoked grant leaves it running).
    fn close_setup_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(mut terminal) = self.onboarding.agents.terminal.take() else { return };
        let pending = terminal.task.take();
        let client =
            self.env_client(&terminal.environment).filter(|_| self.env_grants(&terminal.environment, TERMINAL_OPERATE));
        let close = json!({ "threadId": SETUP_THREAD_ID, "terminalId": terminal.terminal_id, "deleteHistory": true });
        cx.spawn(async move |_, _| {
            if let Some(pending) = pending {
                pending.await;
            }
            if let Some(client) = client
                && let Err(error) = client.request("terminal.close", close).await
            {
                log::debug!("setup terminal close failed: {error}");
            }
        })
        .detach();
        cx.notify();
    }

    /// Close, or the shell exiting (`onSessionExited`): the agent's status is read again.
    fn finish_setup_terminal(&mut self, cx: &mut Context<Self>) {
        let environment = self.onboarding.agents.terminal.as_ref().map(|t| t.environment.clone());
        self.close_setup_terminal(cx);
        if let Some(environment) = environment {
            self.refresh_agents(&environment, None, cx);
        }
    }

    /// The terminal view once the command is typed (or typing failed), focused (`autoFocus`).
    fn mount_setup_view(&mut self, operable: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(terminal) = self.onboarding.agents.terminal.as_ref() else { return };
        if let Some((session, _)) = &terminal.view {
            session.update(cx, |session, cx| session.set_read_only(!operable, cx));
            return;
        }
        if !matches!(terminal.state, SetupState::Ready | SetupState::WriteFailed) {
            return;
        }
        let Some(client) = self.env_client(&terminal.environment) else { return };
        let launch = Launch {
            cwd: terminal.cwd.clone(),
            provider_instance_id: Some(terminal.instance_id.clone()),
            ..Launch::default()
        };
        let (thread_id, terminal_id) = (SETUP_THREAD_ID.to_owned(), terminal.terminal_id.clone());
        let session = cx.new(|cx| TerminalSession::standalone(thread_id, terminal_id, launch, client, window, cx));
        let events = cx.subscribe_in(&session, window, |this, _, event: &SessionEvent, window, cx| {
            this.on_setup_terminal_event(event, window, cx);
        });
        let focus = session.read(cx).focus.clone();
        focus.focus(window, cx);
        if let Some(terminal) = self.onboarding.agents.terminal.as_mut() {
            terminal.view = Some((session, events));
        }
    }

    fn on_setup_terminal_event(&mut self, event: &SessionEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Exited => self.finish_setup_terminal(cx),
            SessionEvent::OpenUrl { url, .. } => cx.open_url(url),
            SessionEvent::OpenPath(path) => {
                let environment = self.onboarding.agents.terminal.as_ref().map(|t| t.environment.clone());
                match self.preferred_editor_id().zip(environment) {
                    Some((editor, environment)) => {
                        let failure = "Unable to open path".to_owned();
                        self.open_path_in_editor(&environment, path.clone(), editor, false, failure, window, cx);
                    }
                    None => {
                        self.error = Some("No editor is available to open this path.".into());
                        cx.notify();
                    }
                }
            }
            SessionEvent::Activated
            | SessionEvent::AddToChat(_)
            | SessionEvent::Input(_)
            | SessionEvent::Resize { .. } => {}
        }
    }

    /// `AgentsSection` for `environment`, labelled `label`.
    pub(super) fn render_agents(
        &mut self,
        environment: SharedString,
        label: &SharedString,
        wide: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.mount_agents(&environment, cx);
        let t = *crate::theme::tokens(cx);
        let Some(info) = self.env_info(&environment) else { return div().into_any_element() };
        let loaded = !info.config.is_null();
        let settings = info.settings.clone();
        let acp_allowed = welcome::driver_allowed(&info.managed_policy, "acpRegistry");
        let agents =
            welcome::group_agents(loaded.then_some(info.providers.as_slice()), &settings, &info.managed_policy);
        let checking = agents.checking;
        let found: Vec<Row> = agents.found.iter().map(|p| Row::new(p, info, false)).collect();
        let available: Vec<Row> = agents.available.iter().map(|p| Row::new(p, info, true)).collect();
        let stage = &self.onboarding.agents;
        let listed = |id: &Option<String>| id.as_ref().and_then(|id| info.provider(id)).cloned();
        let listed_codex = listed(&stage.codex_setup);
        let cli_provider = stage.cli_terminal_for.as_ref().and_then(|(id, _)| info.provider(id)).cloned();

        // Codex's card keeps its last status while a rebuild drops it; once it closes, a later
        // status change does not reopen it and move the page.
        let stage = &mut self.onboarding.agents;
        if listed_codex.is_some() {
            stage.last_codex.clone_from(&listed_codex);
        }
        let codex_card = stage
            .codex_setup
            .as_ref()
            .and_then(|id| listed_codex.clone().or_else(|| stage.last_codex.clone().filter(|p| p.instance_id == *id)))
            .filter(|p| welcome::codex_card_open(p, &settings));
        if listed_codex.is_some() && codex_card.is_none() {
            stage.codex_setup = None;
        }
        if stage.more_open.is_none() && !checking && found.is_empty() {
            stage.more_open = Some(true);
        }
        if let Some(provider) = cli_provider {
            let step = welcome::cli_terminal_step(&provider, &settings);
            if step != CliStep::Wait {
                stage.cli_terminal_for = None;
                if step == CliStep::Open {
                    self.open_setup_terminal(&environment, &provider, cx);
                }
            }
        }

        let can_terminal = self.env_grants(&environment, TERMINAL_OPERATE);
        let can_manage = self.env_grants(&environment, PROVIDERS_MANAGE);
        let rows = || found.iter().chain(&available);
        let terminal_note =
            !can_terminal && rows().any(|r| matches!(r.action, RowAction::Terminal { .. } | RowAction::CodexSetup));
        let manage_note = !can_manage
            && (acp_allowed || rows().any(|r| r.state == AgentState::Disabled || r.action == RowAction::CodexSetup));
        let notes = [
            terminal_note.then_some("This connection cannot control terminals."),
            manage_note.then_some("This connection cannot manage agents."),
        ];
        let show_more = self.onboarding.agents.more_open.unwrap_or(false);
        let busy_instance = self.onboarding.agents.terminal.as_ref().map(|t| t.instance_id.clone());
        let codex_open = self.onboarding.agents.codex_setup.clone();
        let workspace = cx.weak_entity();
        let action = |row: &Row| -> AnyElement {
            let id = row.provider.instance_id.clone();
            let element = |key: &str| SharedString::from(format!("welcome-agent-{key}-{id}"));
            let (environment, workspace) = (environment.clone(), workspace.clone());
            match row.action {
                RowAction::TurnOn => WebButton::new(element("turn-on"), "Turn on")
                    .disabled(!can_manage || !loaded)
                    .on_click(move |_, window, cx| {
                        let (environment, id) = (environment.clone(), id.clone());
                        let _ = workspace.update(cx, |this, cx| this.turn_on_agent(environment, id, window, cx));
                    })
                    .into_any_element(),
                RowAction::Settings(label) => WebButton::new(element("settings"), label)
                    .on_click(move |_, _, cx| {
                        let (environment, id) = (environment.clone(), id.clone());
                        let _ = workspace.update(cx, |this, cx| this.open_provider_settings(environment, id, cx));
                    })
                    .into_any_element(),
                RowAction::CodexSetup => {
                    let open = codex_open.as_ref() == Some(&id);
                    WebButton::new(element("set-up"), "Set up")
                        .on_click(move |_, _, cx| {
                            let id = id.clone();
                            let _ = workspace.update(cx, |this, cx| {
                                this.onboarding.agents.codex_setup = (!open).then_some(id);
                                cx.notify();
                            });
                        })
                        .into_any_element()
                }
                RowAction::Terminal { sign_in } => {
                    let provider = row.provider.clone();
                    WebButton::new(element("terminal"), if sign_in { "Sign in" } else { "Install" })
                        .icon(Lucide::Terminal)
                        .disabled(busy_instance.as_ref() == Some(&id) || !loaded || !can_terminal)
                        .on_click(move |_, _, cx| {
                            let _ =
                                workspace.update(cx, |this, cx| this.open_setup_terminal(&environment, &provider, cx));
                        })
                        .into_any_element()
                }
            }
        };
        let row_view = |row: &Row, compact: bool| {
            let status = match row.state {
                AgentState::Ready => ready_badge(&t),
                AgentState::Checking => div()
                    .text_xs()
                    .line_height(px(16.))
                    .text_color(t.muted_foreground)
                    .child("Checking…")
                    .into_any_element(),
                _ => action(row),
            };
            h_flex()
                .gap(px(12.))
                .min_h(px(if compact { 36. } else { 40. }))
                .when(!compact, |el| el.py(px(4.)))
                .child(row_icon(provider_glyph(&row.provider.driver, if compact { 16. } else { 18. }, false, &t), &t))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_sm()
                                .line_height(px(20.))
                                .text_color(t.foreground)
                                .when(!compact, |el| el.font_weight(FontWeight::MEDIUM))
                                .child(row.name.clone()),
                        )
                        .when_some(row.detail.clone(), |el, detail| {
                            el.child(
                                div()
                                    .line_clamp(2)
                                    .text_xs()
                                    .line_height(px(19.5))
                                    .text_color(t.muted_foreground)
                                    .child(detail),
                            )
                        }),
                )
                .child(div().flex_none().child(status))
        };

        let toggle = {
            let workspace = workspace.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                let _ = workspace.update(cx, |this, cx| {
                    this.onboarding.agents.more_open = Some(!show_more);
                    cx.notify();
                });
            }
        };
        let (ring, foreground) = (t.ring, t.foreground);
        let trigger = h_flex()
            .id("welcome-more-agents")
            .group("welcome-more-agents")
            .w_full()
            .min_h(px(40.))
            .gap(px(12.))
            .rounded(px(6.))
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(move |style| {
                style.shadow(vec![gpui_kit::BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.))])
            })
            .on_click(toggle)
            .child(row_icon(Icon::new(Lucide::Plus).size(px(16.)), &t))
            .child(
                div()
                    .text_sm()
                    .line_height(px(20.))
                    .text_color(if found.is_empty() { t.foreground } else { t.muted_foreground })
                    .group_hover("welcome-more-agents", move |style| style.text_color(foreground))
                    .child(if found.is_empty() { "Set up an agent" } else { "Add more agents" }),
            )
            .when(!show_more && (!available.is_empty() || acp_allowed), |el| {
                el.child(
                    h_flex()
                        .gap(px(8.))
                        .opacity(0.6)
                        .children(
                            available.iter().take(6).map(|row| provider_glyph(&row.provider.driver, 14., true, &t)),
                        )
                        .when(acp_allowed, |el| el.child(provider_glyph("acpRegistry", 14., true, &t))),
                )
            })
            .child(
                div().ml_auto().child(
                    Icon::new(if show_more { Lucide::ChevronUp } else { Lucide::ChevronDown })
                        .size(px(16.))
                        .text_color(t.muted_foreground),
                ),
            );
        let browse = acp_allowed.then(|| {
            let (workspace, environment) = (workspace.clone(), environment.clone());
            h_flex()
                .min_h(px(36.))
                .gap(px(12.))
                .child(row_icon(provider_glyph("acpRegistry", 16., false, &t), &t))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .line_height(px(20.))
                        .text_color(t.muted_foreground)
                        .child("More agents in the ACP Registry"),
                )
                .child(WebButton::new("welcome-acp-browse", "Browse").disabled(!can_manage).on_click(
                    move |_, window, cx| {
                        let environment = environment.clone();
                        let _ = workspace.update(cx, |this, cx| this.open_add_agent_dialog(environment, window, cx));
                    },
                ))
        });
        let panel = show_more.then(|| {
            v_flex()
                .child(
                    div()
                        .grid()
                        .grid_cols(if wide { 2 } else { 1 })
                        .gap_x(px(32.))
                        .children(available.iter().map(|row| row_view(row, true))),
                )
                .children(browse)
        });

        let codex = codex_card.map(|provider| {
            let (workspace, card_environment, id) =
                (workspace.clone(), environment.clone(), provider.instance_id.clone());
            let on_mode_change: ModeChange = Rc::new(move |mode, window, cx| {
                let _ =
                    workspace.update(cx, |this, cx| this.change_codex_mode(&card_environment, &id, mode, window, cx));
            });
            let card =
                self.render_onboarding_codex(environment.clone(), &provider.instance_id, on_mode_change, window, cx);
            div().mt(px(8.)).child(card)
        });
        self.mount_setup_view(can_terminal, window, cx);
        let terminal = self.render_setup_terminal(can_terminal, &t, cx);

        let content = v_flex()
            .children(found.iter().map(|row| row_view(row, false)))
            .when(checking && found.is_empty(), |el| {
                el.child(
                    div()
                        .text_sm()
                        .line_height(px(40.))
                        .text_color(t.muted_foreground)
                        .child(format!("Checking {label}…")),
                )
            })
            .children(notes.into_iter().flatten().map(|note| {
                div().pl(px(32.)).text_xs().line_height(px(16.)).text_color(t.muted_foreground).child(note)
            }))
            .child(trigger)
            .children(panel)
            .children(codex)
            .children(terminal);
        welcome_section("Agents", wide, &t, content).into_any_element()
    }

    /// `AgentInstallTerminal`'s frame: what to do, Retry and Close over the terminal.
    fn render_setup_terminal(&self, operable: bool, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let terminal = self.onboarding.agents.terminal.as_ref()?;
        let line = |text: &str| div().child(text.to_owned());
        let message = match terminal.state {
            _ if !operable => line("This connection cannot control terminals."),
            SetupState::WriteFailed => h_flex()
                .flex_wrap()
                .child("Run\u{a0}")
                .child(
                    div()
                        .rounded(px(4.))
                        .bg(t.muted)
                        .px(px(4.))
                        .font_family(t.fonts.mono)
                        .child(terminal.command.clone()),
                )
                .child("\u{a0}in this terminal."),
            SetupState::Ready => line("Review the command, then press Enter to run it."),
            SetupState::OpenFailed => line("Could not open the setup terminal."),
            SetupState::Preparing => line("Preparing command..."),
        };
        let workspace = cx.weak_entity();
        let retry = (terminal.state == SetupState::OpenFailed).then(|| {
            let workspace = workspace.clone();
            WebButton::new("welcome-setup-retry", "Retry")
                .variant(Variant::Ghost)
                .size(Size::Xs)
                .disabled(!operable)
                .on_click(move |_, _, cx| {
                    let _ = workspace.update(cx, |this, cx| {
                        let environment = this.onboarding.agents.terminal.as_ref().map(|t| t.environment.clone());
                        if environment.is_some_and(|e| this.env_grants(&e, TERMINAL_OPERATE)) {
                            this.prepare_setup_terminal(true, cx);
                        }
                    });
                })
        });
        let close =
            WebButton::new("welcome-setup-close", "Close").variant(Variant::GhostMuted).on_click(move |_, _, cx| {
                let _ = workspace.update(cx, Self::finish_setup_terminal);
            });
        let view = terminal.view.as_ref().map(|(session, _)| session.clone());
        Some(
            v_flex()
                .id(SharedString::from(format!("welcome-setup-{}", terminal.terminal_id)))
                .mt(px(16.))
                .overflow_hidden()
                .rounded(px(8.))
                .border_1()
                .border_color(t.border.opacity(0.7))
                .bg(t.background)
                .text_color(t.foreground)
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(8.))
                        .border_b_1()
                        .border_color(t.border.opacity(0.6))
                        .bg(t.background.opacity(0.6))
                        .px(px(12.))
                        .py(px(6.))
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(11.))
                                .line_height(px(16.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.muted_foreground)
                                .child(message),
                        )
                        .child(h_flex().flex_none().gap(px(4.)).children(retry).child(close)),
                )
                .child(div().h(px(256.)).children(view))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(terminal_id: &str) -> SetupTerminal {
        SetupTerminal {
            environment: "env".into(),
            instance_id: "codex".into(),
            cwd: String::new(),
            command: String::new(),
            terminal_id: terminal_id.to_owned(),
            state: SetupState::Preparing,
            attempt: 1,
            task: None,
            view: None,
        }
    }

    #[test]
    fn a_replaced_setup_terminal_ignores_the_late_steps_of_the_one_it_replaced() {
        let mut stage = AgentsStage { terminal: Some(terminal("a")), ..AgentsStage::default() };
        assert!(stage.setup_attempt("a", 1).is_some());
        stage.terminal = Some(terminal("b"));
        assert!(stage.setup_attempt("a", 1).is_none(), "both are on their first attempt");
        assert!(stage.setup_attempt("b", 1).is_some());
        assert!(stage.setup_attempt("b", 2).is_none());
    }

    #[test]
    fn chatgpt_takes_back_the_cli_terminal_and_only_its_own_failed_save_does() {
        let mut stage = AgentsStage::default();
        let cli = stage.choose_codex_mode("a", true);
        assert_eq!(stage.cli_terminal_for, Some(("a".to_owned(), cli)));
        let chatgpt = stage.choose_codex_mode("a", false);
        assert_eq!(stage.cli_terminal_for, None, "ChatGPT cancels the pending CLI terminal");

        let again = stage.choose_codex_mode("a", true);
        stage.codex_save_failed(cli, "a");
        stage.codex_save_failed(chatgpt, "a");
        assert_eq!(stage.cli_terminal_for, Some(("a".to_owned(), again)), "older saves failing keep the newer ask");
        stage.codex_save_failed(again, "a");
        assert_eq!(stage.cli_terminal_for, None);

        // Leaving the page and coming back starts the band over, but not the count.
        stage.reset(Some("env".into()));
        let after = stage.choose_codex_mode("a", true);
        stage.codex_save_failed(again, "a");
        assert_eq!(stage.cli_terminal_for, Some(("a".to_owned(), after)), "a save from before the reset is older");
    }
}
