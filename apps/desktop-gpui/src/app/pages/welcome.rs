//! The Welcome page (`WelcomePage.tsx`, `routes/welcome.tsx`): the primary computer's agents and
//! the projects found in its agent history, on one page. A fresh install opens it at launch;
//! Settings → About opens it again. Only one ready agent is needed to start.
//!
//! The import keeps running in the workspace if the page is left, so unlike the web nothing
//! blocks navigation meanwhile; it lands in the imported project only while the page still shows.
//! The Phone section needs a T3 Connect account and the "Add to PATH" row needs CLI install
//! support, neither of which this client has.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, Div, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, px, svg,
};
use serde_json::{Value, json};

use super::Page;
use crate::app::Workspace;
use crate::app::terminal::StandaloneTerminal;
use crate::rpc::{RpcClient, RpcError};
use crate::theme::{self, Tokens};
use crate::welcome::{self as logic, AgentState, Candidate, CliStep, ImportSelection};

const PROVIDERS_MANAGE: &str = "providers:manage";
const TERMINAL_OPERATE: &str = "terminal:operate";
/// `AGENT_ONBOARDING_THREAD_ID`: the thread the setup terminal runs under.
const SETUP_THREAD_ID: &str = "onboarding-agent-setup";
const SCAN_LIMIT_MESSAGE: &str = "Scan limit reached. Some projects or conversations may be missing.";
const IMPORT_PERMISSION_MESSAGE: &str = "This connection cannot import projects or thread history.";
const PRIVACY_POLICY_URL: &str = "https://t3.codes/privacy-policy";

/// One environment's `agentSessions.scan`.
#[derive(Default)]
struct ScanState {
    environment: Option<SharedString>,
    pending: bool,
    data: Option<logic::Scan>,
    error: Option<String>,
    generation: u64,
    _task: Option<Task<()>>,
}

/// What finished in the import runs since the page last finished setup; a second Start resumes.
#[derive(Default)]
struct ImportRun {
    /// Per project, so a retry that skips finished projects still counts their threads.
    imported_threads: HashMap<String, u64>,
    /// Projects whose history imported in full.
    completed: HashMap<String, (SharedString, String)>,
    /// Projects that brought at least one thread, where setup lands first.
    with_history: HashMap<String, (SharedString, String)>,
    last_selection: Vec<String>,
    /// A project being created keeps its id and command id across retries.
    attempts: HashMap<String, (String, String)>,
    warning: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SetupState {
    Preparing,
    Ready,
    OpenFailed,
    WriteFailed,
}

/// `AgentInstallTerminal`: a terminal with the install or sign-in command typed, not run.
struct SetupTerminal {
    environment: SharedString,
    instance_id: String,
    terminal_id: String,
    command: String,
    cwd: String,
    state: SetupState,
    pane: Option<StandaloneTerminal>,
    generation: u64,
    _task: Option<Task<()>>,
}

/// The page's state, kept by the workspace so a visit to Settings or an import in flight
/// survives; the folds and the setup terminal reset when the page is left.
#[derive(Default)]
pub(in crate::app) struct WelcomeState {
    scan: ScanState,
    /// Each computer's project choices, stored until setup finishes (`welcomeImportSelectionKey`).
    selections: HashMap<String, ImportSelection>,
    _persist: Option<Task<()>>,
    reviewing: bool,
    /// Repository groups folded in Review all; they start open.
    folded: HashSet<String>,
    other_open: bool,
    /// "Add more agents"; `None` until the user or a finished check with nothing found opens it.
    more_open: Option<bool>,
    /// The Codex instance whose setup card is open, and its last listed status.
    codex_setup: Option<String>,
    last_codex: Option<Value>,
    /// "Use existing CLI" opens this instance's terminal once Codex has checked the CLI.
    cli_terminal_for: Option<String>,
    terminal: Option<SetupTerminal>,
    terminal_generation: u64,
    importing: bool,
    /// Start before the project list loaded keeps saved picks, such as an import a quit cut short.
    keep_choices: bool,
    run: ImportRun,
    _import: Option<Task<()>>,
}

/// Where the choices are kept (the web's local storage).
fn selections_path() -> Option<PathBuf> {
    Some(crate::settings::userdata_dir()?.join("welcome-import-selection.json"))
}

impl WelcomeState {
    /// The choices an earlier visit left; a missing or unreadable file has none.
    pub(in crate::app) fn restored() -> Self {
        let selections = selections_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { selections, ..Self::default() }
    }

    fn persist(&mut self, cx: &App) {
        let Some(path) = selections_path() else { return };
        let Ok(document) = serde_json::to_value(&self.selections) else { return };
        self._persist = Some(cx.background_spawn(async move {
            if let Err(error) = crate::settings::write_atomically(&path, &document) {
                log::warn!("could not save {}: {error}", path.display());
            }
        }));
    }
}

/// The project list as the page reads it now.
struct Projects {
    candidates: Vec<Candidate>,
    /// Checked keys, in list order.
    selected: Vec<String>,
    new: HashSet<String>,
    defaults: Vec<String>,
}

impl Projects {
    fn is_selected(&self, key: &str) -> bool {
        self.selected.iter().any(|k| k == key)
    }
}

enum ImportStep {
    Stop,
    Skip,
    Import { client: RpcClient, project_id: String, create: Option<Value> },
}

fn is_import_source(driver: &str) -> bool {
    logic::IMPORT_SOURCES.iter().any(|(source, _)| *source == driver)
}

fn text_2xs(el: Div) -> Div {
    el.text_size(px(11.)).line_height(px(16.))
}

fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// `ProviderInstanceIcon`: the driver's logo, or a generic bot.
fn agent_icon(driver: &str, size: f32, color: Option<gpui_kit::Hsla>, t: &Tokens) -> AnyElement {
    match crate::assets::provider_icon(driver) {
        Some((path, tint)) => svg()
            .path(path)
            .size(px(size))
            .flex_none()
            .text_color(color.unwrap_or(tint.unwrap_or(t.foreground)))
            .into_any_element(),
        None => Icon::new(IconName::Bot).size(px(size)).text_color(t.muted_foreground).into_any_element(),
    }
}

/// Rows share one icon column, so names, checkboxes and steps line up down the page.
fn row_icon(child: impl IntoElement, t: &Tokens) -> Div {
    div().size(px(20.)).flex_none().flex().items_center().justify_center().text_color(t.muted_foreground).child(child)
}

/// Each part of the page is one band: its label on the left, its rows on the right.
fn section(title: &'static str, body: impl IntoElement, narrow: bool, t: &Tokens) -> Div {
    let label = div().w(px(96.)).flex_none().text_sm().line_height(px(40.)).text_color(t.muted_foreground).child(title);
    let body = div().flex_1().min_w_0().child(body);
    let band = div().border_b_1().border_color(t.border.opacity(0.6)).py(px(16.));
    if narrow {
        band.child(v_flex().child(label).child(body))
    } else {
        band.child(h_flex().items_start().gap(px(40.)).child(label).child(body))
    }
}

fn ready_label(t: &Tokens) -> AnyElement {
    h_flex()
        .gap(px(6.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.success_foreground)
        .child(Icon::new(IconName::Check).size(px(14.)))
        .child("Ready")
        .into_any_element()
}

fn muted_note(text: impl Into<SharedString>, t: &Tokens) -> AnyElement {
    div().text_xs().text_color(t.muted_foreground).child(text.into()).into_any_element()
}

impl Workspace {
    /// The computer the page sets up: this app's own.
    fn welcome_environment(&self) -> Option<SharedString> {
        self.primary_environment().cloned()
    }

    /// Settings → About → Welcome page, and the first-run landing.
    pub(in crate::app) fn open_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_page(Page::Welcome, cx);
        // The composer is not on screen, so it must not keep the keys.
        self.focus.focus(window, cx);
    }

    /// `FirstRunGate` at launch, once the primary workspace is known: a fresh install opens the
    /// page when nothing else was asked for; a workspace already in use counts as set up.
    pub(in crate::app) fn check_first_run(
        &mut self,
        may_open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.client_settings.read_failed {
            return false;
        }
        let saved = self.saved_environments.read(cx);
        // Until the paired computers are read, nothing is decided either way.
        if saved.loading() {
            return false;
        }
        let linked = saved.environments().iter().any(|e| !saved.is_primary(&e.environment_id));
        let completed = !self.client_settings.values.get("onboardingCompletedAt").is_null();
        match logic::first_run(completed, linked, self.shell.projects.len(), self.shell.threads.len()) {
            logic::FirstRun::App { persist } => {
                if persist {
                    self.complete_onboarding();
                }
                false
            }
            logic::FirstRun::Welcome if may_open => {
                self.open_welcome(window, cx);
                true
            }
            logic::FirstRun::Welcome => false,
        }
    }

    /// `completeOnboarding`; false when the settings file could not be written.
    fn complete_onboarding(&mut self) -> bool {
        let Some(now) = crate::timestamp::iso(crate::timestamp::now_ms()) else { return false };
        let mut patch = serde_json::Map::new();
        patch.insert("onboardingCompletedAt".into(), now.into());
        match self.client_settings.try_apply(&patch) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("could not save the onboarding completion: {error}");
                false
            }
        }
    }

    /// Opening the page asks the computer to check its agents again and looks for projects.
    pub(super) fn welcome_opened(&mut self, cx: &mut Context<Self>) {
        let Some(environment) = self.welcome_environment() else { return };
        self.welcome_refresh_providers(environment, None, cx);
        if !self.welcome.scan.pending {
            self.welcome_scan(cx);
        }
    }

    /// Keeps the page's derived state current; leaving the page closes its terminal and folds.
    pub(in crate::app) fn sync_welcome(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page != Some(Page::Welcome) {
            let welcome = &mut self.welcome;
            if welcome.terminal.is_some()
                || welcome.reviewing
                || welcome.codex_setup.is_some()
                || welcome.more_open.is_some()
            {
                self.welcome_close_terminal(cx);
                let welcome = &mut self.welcome;
                welcome.reviewing = false;
                welcome.more_open = None;
                welcome.codex_setup = None;
                welcome.cli_terminal_for = None;
            }
            return;
        }
        let Some(environment) = self.welcome_environment() else { return };
        let Some(info) = self.env_info(&environment) else { return };
        let settings = (!info.config.is_null()).then_some(&info.settings);
        let providers = (!info.config.is_null()).then_some(info.providers_json.as_slice());
        let policy = &info.managed_policy;
        let groups = logic::group_agents(providers, settings, |p| {
            logic::driver_allowed(policy, p["driver"].as_str().unwrap_or_default())
        });
        let find = |id: &str| info.providers_json.iter().find(|p| logic::instance_id(p) == id);
        let welcome = &self.welcome;
        // Opened once the finished checks find nothing; a later check never closes it.
        let open_more = welcome.more_open.is_none() && !groups.checking && groups.found.is_empty();
        // The Codex card keeps its last status while a rebuild briefly drops the instance. Once
        // it closes, a later status change does not reopen it and move the page.
        let codex = welcome.codex_setup.as_deref().and_then(find).map(|listed| {
            let open = logic::codex_needs_setup(listed, settings)
                || logic::agent_state(listed, settings) == AgentState::Checking;
            (open, (welcome.last_codex.as_ref() != Some(listed)).then(|| listed.clone()))
        });
        let cli = welcome.cli_terminal_for.as_deref().and_then(find).and_then(|provider| {
            let step = logic::cli_terminal_step(provider, settings);
            (step != CliStep::Wait).then(|| (step, provider.clone()))
        });
        let welcome = &mut self.welcome;
        if open_more {
            welcome.more_open = Some(true);
        }
        if let Some((open, changed)) = codex {
            if changed.is_some() {
                welcome.last_codex = changed;
            }
            if !open {
                welcome.codex_setup = None;
            }
        }
        if let Some((step, provider)) = cli {
            welcome.cli_terminal_for = None;
            if step == CliStep::Open {
                self.welcome_open_terminal(&provider, window, cx);
            }
        }
    }

    fn welcome_scan(&mut self, cx: &mut Context<Self>) {
        let Some(environment) = self.welcome_environment() else { return };
        let Some(client) = self.env_client(&environment) else { return };
        let scan = &mut self.welcome.scan;
        if scan.environment.as_ref() != Some(&environment) {
            *scan = ScanState { environment: Some(environment.clone()), ..ScanState::default() };
        }
        scan.pending = true;
        scan.generation += 1;
        let generation = scan.generation;
        scan._task = Some(cx.spawn(async move |this, cx| {
            let result = client.request("agentSessions.scan", json!({})).await;
            let _ = this.update(cx, |this, cx| {
                let scan = &mut this.welcome.scan;
                if scan.generation != generation || scan.environment.as_ref() != Some(&environment) {
                    return;
                }
                scan.pending = false;
                match result {
                    Ok(reply) => {
                        scan.data = Some(logic::Scan::parse(&environment, &reply));
                        scan.error = None;
                    }
                    // The next connection opens the page's reads again.
                    Err(RpcError::Disconnected(_)) => {}
                    Err(error) => scan.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn welcome_refresh_providers(
        &mut self,
        environment: SharedString,
        instance_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.env_client(&environment) else { return };
        let payload = instance_id.map_or_else(|| json!({}), |id| json!({ "instanceId": id }));
        cx.spawn(async move |this, cx| {
            let Ok(reply) = client.request("server.refreshProviders", payload).await else { return };
            let _ = this.update(cx, |this, cx| {
                if let Some(info) = this.env_info_mut(&environment)
                    && let Err(error) = info.apply(&json!({"type": "providerStatuses", "payload": reply}))
                {
                    log::warn!("provider statuses did not decode: {error}");
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// One-instance upsert (`persistEnvironmentProviderInstance`), which keeps an instance another
    /// client added meanwhile. `then` hears whether it saved.
    fn welcome_upsert_instance(
        &mut self,
        environment: SharedString,
        instance_id: String,
        instance: Value,
        failure: String,
        then: impl FnOnce(&mut Self, bool, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.env_client(&environment) else { return };
        let payload = json!({
            "patch": {},
            "providerInstanceMutation": {"operation": "upsert", "instanceId": instance_id, "instance": instance},
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", payload).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let saved = match result {
                    Ok(reply) => {
                        if let Some(info) = this.env_info_mut(&environment) {
                            let event = json!({"type": "settingsUpdated", "payload": {"settings": reply}});
                            if let Err(error) = info.apply(&event) {
                                log::warn!("settings reply did not decode: {error}");
                            }
                        }
                        true
                    }
                    Err(RpcError::Disconnected(_)) => false,
                    Err(error) => {
                        window.push_notification(Notification::error(error.to_string()).title(failure), cx);
                        false
                    }
                };
                then(this, saved, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Turn on: an agent that is off on this computer, including one only some projects use.
    fn welcome_turn_on(&mut self, instance_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.welcome_environment() else { return };
        if !self.env_grants(&environment, PROVIDERS_MANAGE) {
            return;
        }
        let Some(info) = self.env_info(&environment).filter(|info| !info.config.is_null()) else { return };
        let Some(provider) = info.providers_json.iter().find(|p| logic::instance_id(p) == instance_id) else { return };
        let driver = provider["driver"].as_str().unwrap_or_default().to_owned();
        let stored = &info.settings["providerInstances"][instance_id.as_str()];
        let instance = if stored.is_object() { stored.clone() } else { json!({ "driver": driver }) };
        let failure = format!("Could not turn on {}", logic::agent_name(provider));
        let next = logic::turned_on_instance(&instance);
        let id = instance_id.clone();
        self.welcome_upsert_instance(
            environment.clone(),
            instance_id,
            next,
            failure,
            move |this, saved, _, cx| {
                if !saved {
                    return;
                }
                this.welcome_refresh_providers(environment, Some(id), cx);
                // The project scan only reads agents that are on, so this can find more history.
                if is_import_source(&driver) {
                    this.welcome_scan(cx);
                }
            },
            window,
            cx,
        );
    }

    /// Codex's card: "Continue with ChatGPT" (`managed`) or "Use existing CLI" (`existing`). Signing
    /// in with ChatGPT continues on the instance's Settings page, where its runtime installs.
    fn welcome_codex_mode(&mut self, instance_id: String, managed: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.welcome_environment() else { return };
        let Some(info) = self.env_info(&environment) else { return };
        let Some(provider) = info.providers_json.iter().find(|p| logic::instance_id(p) == instance_id) else { return };
        let stored = &info.settings["providerInstances"][instance_id.as_str()];
        let mut instance =
            if stored.is_object() { stored.clone() } else { json!({"driver": provider["driver"], "config": {}}) };
        let mode = if managed { "managed" } else { "existing" };
        let unchanged =
            instance.pointer("/config/setupMode").and_then(Value::as_str) == Some(mode) && provider["enabled"] == true;
        if !managed {
            self.welcome.cli_terminal_for = Some(instance_id.clone());
        }
        if unchanged || !self.env_grants(&environment, PROVIDERS_MANAGE) {
            if managed {
                self.open_provider_settings(environment, instance_id, cx);
            }
            return;
        }
        let mut config = instance.get("config").filter(|c| c.is_object()).cloned().unwrap_or_else(|| json!({}));
        config["enabled"] = true.into();
        config["setupMode"] = mode.into();
        instance["enabled"] = true.into();
        instance["config"] = config;
        let id = instance_id.clone();
        let target = environment.clone();
        self.welcome_upsert_instance(
            environment,
            instance_id,
            instance,
            "Could not save Codex setup".into(),
            move |this, saved, _, cx| {
                if !saved {
                    this.welcome.cli_terminal_for = None;
                } else if managed {
                    this.open_provider_settings(target, id, cx);
                }
            },
            window,
            cx,
        );
    }

    /// Install or Sign in: a terminal on the computer with the right command typed in.
    fn welcome_open_terminal(&mut self, provider: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.welcome_environment() else { return };
        let driver = provider["driver"].as_str().unwrap_or_default();
        let Some(info) = self.env_info(&environment).filter(|info| !info.config.is_null()) else { return };
        if !logic::is_installable(driver) || !self.env_grants(&environment, TERMINAL_OPERATE) {
            return;
        }
        let os = info.platform_os.as_deref().unwrap_or_default();
        let command = if provider["installed"] == true {
            logic::login_command(provider, &info.settings, os)
        } else {
            let Some(command) = logic::install_command(driver, os) else { return };
            command.to_owned()
        };
        let cwd = info.config["cwd"].as_str().unwrap_or_default().to_owned();
        let instance_id = logic::instance_id(provider).to_owned();
        let terminal_id = format!("onboarding-{driver}-{}", crate::app::new_id());
        self.welcome_close_terminal(cx);
        self.welcome.terminal = Some(SetupTerminal {
            environment,
            instance_id,
            terminal_id,
            command,
            cwd,
            state: SetupState::Preparing,
            pane: None,
            generation: 0,
            _task: None,
        });
        self.welcome_prepare_terminal(window, cx);
    }

    /// Opens the setup PTY with the instance's environment, then types the command into it.
    fn welcome_prepare_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.welcome.terminal_generation += 1;
        let generation = self.welcome.terminal_generation;
        let Some(terminal) = &self.welcome.terminal else { return };
        let environment = terminal.environment.clone();
        let client = self.env_client(&environment);
        let operable = self.env_grants(&environment, TERMINAL_OPERATE);
        let open = json!({
            "threadId": SETUP_THREAD_ID,
            "terminalId": terminal.terminal_id,
            "cwd": terminal.cwd,
            "providerInstanceId": terminal.instance_id,
        });
        let write = json!({"threadId": SETUP_THREAD_ID, "terminalId": terminal.terminal_id, "data": terminal.command});
        let Some(terminal) = &mut self.welcome.terminal else { return };
        terminal.generation = generation;
        terminal.state = SetupState::Preparing;
        terminal.pane = None;
        let (Some(client), true) = (client, operable) else {
            terminal.state = SetupState::OpenFailed;
            cx.notify();
            return;
        };
        terminal._task = Some(cx.spawn_in(window, async move |this, cx| {
            let opened = client.request("terminal.open", open).await;
            let still = |this: &Workspace| this.welcome.terminal.as_ref().is_some_and(|t| t.generation == generation);
            let wrote = match opened {
                Err(_) => None,
                Ok(_) => {
                    let Ok((current, allowed)) =
                        this.update(cx, |this, _| (still(this), this.env_grants(&environment, TERMINAL_OPERATE)))
                    else {
                        return;
                    };
                    // Closed while it opened: the close went out first, so end this one too.
                    if !current {
                        if allowed {
                            let close = json!({"threadId": SETUP_THREAD_ID, "terminalId": write["terminalId"], "deleteHistory": true});
                            let _ = client.request("terminal.close", close).await;
                        }
                        return;
                    }
                    Some(allowed && client.request("terminal.write", write).await.is_ok())
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                if !still(this) {
                    return;
                }
                let state = match wrote {
                    None => SetupState::OpenFailed,
                    Some(true) => SetupState::Ready,
                    Some(false) => SetupState::WriteFailed,
                };
                let pane = (state != SetupState::OpenFailed).then(|| {
                    let terminal = this.welcome.terminal.as_ref().map(|t| (t.terminal_id.clone(), t.cwd.clone()));
                    let (terminal_id, cwd) = terminal.unwrap_or_default();
                    StandaloneTerminal::new(
                        SETUP_THREAD_ID.to_owned(),
                        terminal_id,
                        cwd,
                        client,
                        |this, _, cx| this.welcome_close_terminal(cx),
                        window,
                        cx,
                    )
                });
                if let Some(pane) = &pane {
                    pane.focus(window, cx);
                }
                if let Some(terminal) = &mut this.welcome.terminal {
                    terminal.state = state;
                    terminal.pane = pane;
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Close, or the shell exiting: ends the PTY while terminal access remains, then rechecks agents.
    fn welcome_close_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(terminal) = self.welcome.terminal.take() else { return };
        if self.env_grants(&terminal.environment, TERMINAL_OPERATE)
            && let Some(client) = self.env_client(&terminal.environment)
        {
            let close = json!({"threadId": SETUP_THREAD_ID, "terminalId": terminal.terminal_id, "deleteHistory": true});
            cx.background_spawn(async move { client.request("terminal.close", close).await }).detach();
        }
        self.welcome_refresh_providers(terminal.environment, None, cx);
        cx.notify();
    }

    fn welcome_projects(&self) -> Projects {
        let candidates = self.welcome.scan.data.as_ref().map(|scan| scan.candidates.clone()).unwrap_or_default();
        let roots = logic::ProjectRoots::new(
            self.shell.projects.iter().map(|p| (p.environment_id.as_ref(), p.workspace_root.as_str())),
        );
        let now = crate::timestamp::now_ms();
        let recent: Vec<&Candidate> =
            candidates.iter().filter(|c| logic::is_recent(c, now) && roots.is_new(c)).collect();
        let recent_keys: HashSet<&str> = recent.iter().map(|c| c.key.as_str()).collect();
        let selection =
            self.welcome.scan.environment.as_ref().and_then(|env| self.welcome.selections.get(env.as_ref()));
        let selected =
            logic::selected(&candidates, &recent_keys, selection).into_iter().map(|c| c.key.clone()).collect();
        let defaults = logic::default_keys(&candidates, &recent, &roots);
        let new = candidates.iter().filter(|c| roots.is_new(c)).map(|c| c.key.clone()).collect();
        Projects { candidates, selected, new, defaults }
    }

    /// The import needs `orchestration:operate` on the computer.
    fn welcome_can_import(&self) -> bool {
        self.welcome.scan.environment.as_ref().or(self.primary_environment()).is_some_and(|env| self.can_operate(env))
    }

    fn welcome_set_selected(&mut self, keys: Vec<String>, opted_out: bool, cx: &mut Context<Self>) {
        let Some(environment) = self.welcome.scan.environment.clone() else { return };
        let known_keys = self.welcome_projects().candidates.into_iter().map(|c| c.key).collect();
        self.welcome.selections.insert(environment.to_string(), ImportSelection { known_keys, keys, opted_out });
        self.welcome.persist(cx);
        cx.notify();
    }

    fn welcome_toggle_keys(&mut self, keys: &[String], checked: bool, cx: &mut Context<Self>) {
        let mut next = self.welcome_projects().selected;
        next.retain(|key| !keys.contains(key));
        if checked {
            next.extend(keys.iter().cloned());
        }
        self.welcome_set_selected(next, false, cx);
    }

    /// Start: imports the checked projects, then finishes setup.
    fn welcome_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.welcome.importing {
            return;
        }
        self.welcome.keep_choices = self.welcome.scan.data.is_none();
        let projects = self.welcome_projects();
        // Without import permission, starting simply skips the projects.
        let selected: Vec<Candidate> = if self.welcome_can_import() {
            projects.candidates.into_iter().filter(|c| projects.selected.contains(&c.key)).collect()
        } else {
            Vec::new()
        };
        if selected.is_empty() {
            self.welcome_finish_after_import(window, cx);
            return;
        }
        // Stored now, so after a restart mid-import these stay checked even once they exist.
        self.welcome_set_selected(selected.iter().map(|c| c.key.clone()).collect(), false, cx);
        self.welcome_run_import(selected, window, cx);
    }

    /// `resolveOnboardingProjectId`: the server's match, else a listed project at the same root.
    fn welcome_project_id(&self, candidate: &Candidate) -> Option<String> {
        if let Some(id) = &candidate.project_id {
            return Some(id.clone());
        }
        let root = crate::project_groups::normalize_path(&candidate.path);
        self.shell
            .projects
            .iter()
            .find(|p| {
                p.environment_id.as_ref() == candidate.environment
                    && crate::project_groups::normalize_path(&p.workspace_root) == root
            })
            .map(|p| p.id.clone())
    }

    fn welcome_import_step(&mut self, candidate: &Candidate) -> ImportStep {
        if !self.can_operate(&candidate.environment) {
            self.welcome.importing = false;
            return ImportStep::Stop;
        }
        if self.welcome.run.completed.contains_key(&candidate.key) {
            return ImportStep::Skip;
        }
        let Some(client) = self.env_client(&candidate.environment) else { return ImportStep::Skip };
        if let Some(project_id) = self.welcome_project_id(candidate) {
            return ImportStep::Import { client, project_id, create: None };
        }
        let (project_id, command_id) = self
            .welcome
            .run
            .attempts
            .entry(candidate.key.clone())
            .or_insert_with(|| {
                let id = crate::app::new_id();
                let command = format!("onboarding:project:create:{id}");
                (id, command)
            })
            .clone();
        let create = json!({
            "type": "project.create",
            "commandId": command_id,
            "projectId": project_id,
            "title": candidate.title,
            "workspaceRoot": candidate.path,
            "createWorkspaceRootIfMissing": false,
            "defaultModelSelection": Value::Null,
        });
        ImportStep::Import { client, project_id, create: Some(create) }
    }

    /// Creates each picked project, then imports its agent history, one at a time.
    fn welcome_run_import(&mut self, selection: Vec<Candidate>, window: &mut Window, cx: &mut Context<Self>) {
        if self.welcome.importing || !selection.iter().all(|c| self.can_operate(&c.environment)) {
            return;
        }
        self.welcome.importing = true;
        let run = &mut self.welcome.run;
        run.warning.clear();
        run.last_selection = selection.iter().map(|c| c.key.clone()).collect();
        let mut completed = selection.iter().filter(|c| run.completed.contains_key(&c.key)).count();
        self.welcome._import = Some(cx.spawn_in(window, async move |this, cx| {
            let mut skipped = 0;
            let mut rescan = false;
            for candidate in &selection {
                let Ok(step) = this.update(cx, |this, _| this.welcome_import_step(candidate)) else { return };
                let (client, project_id, create) = match step {
                    ImportStep::Stop => {
                        let _ = this.update(cx, |_, cx| cx.notify());
                        return;
                    }
                    ImportStep::Skip => continue,
                    ImportStep::Import { client, project_id, create } => (client, project_id, create),
                };
                if let Some(create) = create {
                    let result = client.request("projects.mutate", create).await;
                    if let Err(error) = result {
                        // A dropped socket keeps the attempt, so the retry reuses its command.
                        if !matches!(error, RpcError::Disconnected(_)) {
                            rescan = true;
                            let _ = this.update(cx, |this, _| this.welcome.run.attempts.remove(&candidate.key));
                        }
                        continue;
                    }
                }
                let Ok(allowed) = this.update(cx, |this, cx| {
                    let allowed = this.can_operate(&candidate.environment);
                    if !allowed {
                        this.welcome.importing = false;
                        cx.notify();
                    }
                    allowed
                }) else {
                    return;
                };
                if !allowed {
                    return;
                }
                let input = json!({"projectId": project_id, "expectedWorkspaceRoot": candidate.path});
                let result = client.request("agentSessions.import", input).await;
                let recorded = this.update(cx, |this, _| {
                    let run = &mut this.welcome.run;
                    match result {
                        Ok(reply) => {
                            let imported = reply["importedCount"].as_u64().unwrap_or(0);
                            let skips = reply["skippedCount"].as_u64().unwrap_or(0);
                            *run.imported_threads.entry(candidate.key.clone()).or_default() += imported;
                            skipped += skips;
                            let project = (SharedString::from(candidate.environment.clone()), project_id);
                            if imported > 0 {
                                run.with_history.insert(candidate.key.clone(), project.clone());
                            }
                            if skips == 0 {
                                completed += 1;
                                run.completed.insert(candidate.key.clone(), project);
                            }
                        }
                        Err(RpcError::Disconnected(_)) => {}
                        Err(_) => {
                            run.attempts.remove(&candidate.key);
                            rescan = true;
                        }
                    }
                });
                if recorded.is_err() {
                    return;
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if rescan {
                    this.welcome_scan(cx);
                }
                this.welcome.importing = false;
                if completed < selection.len() {
                    this.welcome.run.warning = logic::import_warning(this.welcome_imported_total(), skipped);
                }
                this.welcome_finish_after_import(window, cx);
            });
        }));
        cx.notify();
    }

    fn welcome_imported_total(&self) -> u64 {
        let run = &self.welcome.run;
        run.last_selection.iter().map(|key| run.imported_threads.get(key).copied().unwrap_or(0)).sum()
    }

    /// Saves setup as done, lands in the first imported project (or home) while the page still
    /// shows, and reports the import.
    fn welcome_finish_after_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.complete_onboarding() {
            self.welcome.importing = false;
            window.push_notification(
                Notification::error("Your settings could not be saved. Try again.").title("Could not finish setup"),
                cx,
            );
            cx.notify();
            return;
        }
        let run = std::mem::take(&mut self.welcome.run);
        let landing = logic::landing_project(&run.last_selection, &run.with_history, &run.completed).cloned();
        let imported =
            run.last_selection.iter().map(|key| run.imported_threads.get(key).copied().unwrap_or(0)).sum::<u64>();
        self.welcome.importing = false;
        if !std::mem::take(&mut self.welcome.keep_choices)
            && let Some(environment) = self.welcome.scan.environment.clone()
            && self.welcome.selections.remove(environment.as_ref()).is_some()
        {
            self.welcome.persist(cx);
        }
        if self.page == Some(Page::Welcome) {
            match landing {
                Some((environment, project_id)) => self.start_draft_in(environment, project_id, window, cx),
                None => self.go_home(window, cx),
            }
        }
        if !run.warning.is_empty() {
            let note = Notification::warning(run.warning).title("Some history was not imported").autohide(false);
            window.push_notification(note, cx);
        } else if imported > 0 {
            let label = format!("Imported {imported} {}", if imported == 1 { "thread" } else { "threads" });
            window.push_notification(Notification::success(label), cx);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Rendering

    pub(super) fn render_welcome(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let narrow = t.breakpoint == theme::Breakpoint::Base;
        let environment = self.welcome_environment();
        let info = environment.as_ref().and_then(|env| self.env_info(env));
        let label = info.and_then(|info| info.environment_label.clone()).unwrap_or_else(|| "this computer".into());
        let agents = self.render_welcome_agents(&label, narrow, &t, cx);
        let projects = self.render_welcome_projects(narrow, &t, cx);
        let ready = self.welcome_agent_groups().is_some_and(|groups| groups.ready_count > 0);
        let projects_view = self.welcome_projects();
        let can_import = self.welcome_can_import();
        let selected = if can_import { projects_view.selected.len() } else { 0 };
        let new = if can_import {
            projects_view.selected.iter().filter(|key| projects_view.new.contains(*key)).count()
        } else {
            0
        };
        let importing = self.welcome.importing;
        let start = Button::new("welcome-start")
            .primary()
            .small()
            .label(logic::start_label(importing, selected, new))
            .disabled(!ready || importing)
            .on_click(cx.listener(|this, _, window, cx| this.welcome_start(window, cx)));
        let footer = h_flex()
            .mt(px(24.))
            .flex_wrap()
            .justify_between()
            .gap(px(12.))
            .child(if ready {
                div().into_any_element()
            } else {
                Button::new("welcome-start-without-agent")
                    .ghost()
                    .small()
                    .label("Start without an agent")
                    .disabled(importing)
                    .on_click(cx.listener(|this, _, window, cx| this.welcome_start(window, cx)))
                    .into_any_element()
            })
            .child(
                h_flex()
                    .gap(px(12.))
                    .when(!ready, |el| el.child(muted_note("Set up one agent to start.", &t)))
                    .child(start),
            );
        let telemetry = self.render_welcome_telemetry(&t);
        div()
            .id("welcome-scroll")
            .size_full()
            .overflow_y_scroll()
            .bg(t.background)
            .child(
                v_flex()
                    .mx_auto()
                    .w_full()
                    .max_w(px(768.))
                    .min_h_full()
                    .px(px(24.))
                    .pt(px(if narrow { 48. } else { 80. }))
                    .pb(px(32.))
                    .child(
                        div()
                            .text_2xl()
                            .line_height(px(32.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child("Welcome to T3 Code"),
                    )
                    .child(
                        h_flex()
                            .mt(px(6.))
                            .text_sm()
                            .text_color(t.muted_foreground)
                            .child("Setting up\u{a0}")
                            .child(div().text_color(t.foreground).child(label))
                            .child("."),
                    )
                    .child(
                        div()
                            .mt(px(40.))
                            .border_t_1()
                            .border_color(t.border.opacity(0.6))
                            .child(agents)
                            .children(projects),
                    )
                    .child(footer)
                    .child(telemetry),
            )
            .into_any_element()
    }

    fn render_welcome_telemetry(&self, t: &Tokens) -> AnyElement {
        let policy = self.welcome_environment().and_then(|env| self.env_info(&env)).map(|info| &info.managed_policy);
        // `managedTelemetryOffMessage`.
        let off = policy
            .filter(|p| p.pointer("/limits/telemetry") == Some(&Value::Bool(false)))
            .map(|p| format!("{} turned off anonymous usage data.", p["organization"].as_str().unwrap_or_default()));
        let lead = match &off {
            Some(message) => format!("{message} See the"),
            None => "T3 Code collects anonymous usage data to improve the app. To opt out, see the".to_owned(),
        };
        let color = t.muted_foreground.opacity(0.7);
        text_2xs(h_flex())
            .mt_auto()
            .pt(px(48.))
            .flex_wrap()
            .text_color(color)
            .child(format!("{lead}\u{a0}"))
            .child(
                div()
                    .id("welcome-privacy-policy")
                    .cursor_pointer()
                    .underline()
                    .hover(|style| style.text_color(t.muted_foreground))
                    .child("privacy policy")
                    .on_click(|_, _, cx| cx.open_url(PRIVACY_POLICY_URL)),
            )
            .child(".")
            .into_any_element()
    }

    fn welcome_agent_groups(&self) -> Option<logic::AgentGroups> {
        let info = self.env_info(&self.welcome_environment()?)?;
        let loaded = !info.config.is_null();
        let policy = &info.managed_policy;
        Some(logic::group_agents(
            loaded.then_some(info.providers_json.as_slice()),
            loaded.then_some(&info.settings),
            |p| logic::driver_allowed(policy, p["driver"].as_str().unwrap_or_default()),
        ))
    }

    fn render_welcome_agents(&mut self, label: &str, narrow: bool, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(environment) = self.welcome_environment() else {
            return section("Agents", muted_note(format!("Checking {label}…"), t), narrow, t).into_any_element();
        };
        let groups = self.welcome_agent_groups().unwrap_or_default();
        let Some(info) = self.env_info(&environment) else { return div().into_any_element() };
        let loaded = !info.config.is_null();
        let settings = loaded.then_some(&info.settings);
        let providers = info.providers_json.clone();
        let can_terminal = self.env_grants(&environment, TERMINAL_OPERATE);
        let can_manage = self.env_grants(&environment, PROVIDERS_MANAGE);
        let acp_allowed = logic::driver_allowed(&info.managed_policy, "acpRegistry");
        let listed = || groups.found.iter().chain(&groups.available).map(|&i| &providers[i]);
        let mut notes = Vec::new();
        if !can_terminal
            && listed().any(|p| logic::needs_install_or_sign_in(p, settings) || logic::codex_needs_setup(p, settings))
        {
            notes.push("This connection cannot control terminals.");
        }
        if !can_manage
            && (acp_allowed
                || listed().any(|p| {
                    logic::agent_state(p, settings) == AgentState::Disabled || logic::codex_needs_setup(p, settings)
                }))
        {
            notes.push("This connection cannot manage agents.");
        }
        let states: Vec<AgentState> = providers.iter().map(|p| logic::agent_state(p, settings)).collect();
        let row = |this: &mut Self, index: usize, compact: bool, cx: &mut Context<Self>| {
            let provider = &providers[index];
            let action =
                this.welcome_agent_action(provider, states[index], loaded, can_terminal, can_manage, &environment, cx);
            agent_row(provider, states[index], action, compact, t)
        };
        let found: Vec<AnyElement> = groups.found.iter().map(|&i| row(self, i, false, cx)).collect();
        let more_open = self.welcome.more_open.unwrap_or(false);
        let available: Vec<AnyElement> =
            if more_open { groups.available.iter().map(|&i| row(self, i, true, cx)).collect() } else { Vec::new() };
        let logos = (!more_open).then(|| {
            h_flex()
                .gap(px(8.))
                .opacity(0.6)
                .children(groups.available.iter().take(6).map(|&i| {
                    agent_icon(providers[i]["driver"].as_str().unwrap_or_default(), 14., Some(t.muted_foreground), t)
                }))
                .when(acp_allowed, |el| el.child(agent_icon("acpRegistry", 14., Some(t.muted_foreground), t)))
        });
        let toggle_label = if groups.found.is_empty() { "Set up an agent" } else { "Add more agents" };
        let toggle = Button::new("welcome-more-agents")
            .ghost()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .min_h(px(40.))
                    .gap(px(12.))
                    .child(row_icon(Icon::new(IconName::Plus).size(px(16.)), t))
                    .child(
                        div()
                            .text_sm()
                            .text_color(if groups.found.is_empty() { t.foreground } else { t.muted_foreground })
                            .child(toggle_label),
                    )
                    .children(logos)
                    .child(div().flex_1())
                    .child(
                        Icon::new(if more_open { IconName::ChevronUp } else { IconName::ChevronDown })
                            .size(px(16.))
                            .text_color(t.muted_foreground),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.welcome.more_open = Some(!more_open);
                cx.notify();
            }));
        let grid = (!available.is_empty()).then(|| {
            let mut rows = available.into_iter();
            let mut grid = v_flex();
            if narrow {
                grid = grid.children(rows);
            } else {
                while let Some(left) = rows.next() {
                    let right = rows.next();
                    grid = grid.child(
                        h_flex()
                            .gap(px(32.))
                            .child(div().flex_1().min_w_0().child(left))
                            .child(div().flex_1().min_w_0().children(right)),
                    );
                }
            }
            grid
        });
        let acp = (more_open && acp_allowed).then(|| {
            let target = environment.clone();
            h_flex()
                .min_h(px(36.))
                .gap(px(12.))
                .child(row_icon(agent_icon("acpRegistry", 16., Some(t.muted_foreground), t), t))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child("More agents in the ACP Registry"),
                )
                .child(
                    Button::new("welcome-acp-browse")
                        .outline()
                        .xsmall()
                        .label("Browse")
                        .disabled(!can_manage)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Adding a provider lives on the computer's Providers page.
                            this.settings_ui.scope =
                                crate::settings::scope::ScopeSearch::default().with_machine(Some(target.to_string()));
                            this.open_settings("providers", cx);
                        })),
                )
        });
        let codex = self.render_welcome_codex_card(&environment, can_manage, can_terminal, t, cx);
        let terminal = self.render_welcome_terminal(t, cx);
        let body = v_flex()
            .children(found)
            .when(groups.checking && groups.found.is_empty(), |el| {
                el.child(
                    div()
                        .text_sm()
                        .line_height(px(40.))
                        .text_color(t.muted_foreground)
                        .child(format!("Checking {label}…")),
                )
            })
            .children(
                notes.into_iter().map(|note| div().pl(px(32.)).text_xs().text_color(t.muted_foreground).child(note)),
            )
            .child(toggle)
            .children(grid)
            .children(acp)
            .children(codex)
            .children(terminal);
        section("Agents", body, narrow, t).into_any_element()
    }

    /// One action per agent, the same in the list and in the grid.
    fn welcome_agent_action(
        &self,
        provider: &Value,
        state: AgentState,
        loaded: bool,
        can_terminal: bool,
        can_manage: bool,
        environment: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = logic::instance_id(provider).to_owned();
        let name = logic::agent_name(provider);
        let element_id = |action: &str| SharedString::from(format!("welcome-agent-{action}-{id}"));
        let settings = (loaded).then(|| self.env_info(environment).map(|info| &info.settings)).flatten();
        let settings_action = |label: &'static str, cx: &mut Context<Self>| {
            let (environment, id) = (environment.clone(), id.clone());
            Button::new(element_id("settings"))
                .outline()
                .xsmall()
                .label(label)
                .accessibility_label(format!("{label}: {name}"))
                .on_click(
                    cx.listener(move |this, _, _, cx| this.open_provider_settings(environment.clone(), id.clone(), cx)),
                )
                .into_any_element()
        };
        if state == AgentState::Disabled {
            let id = id.clone();
            return Button::new(element_id("turn-on"))
                .outline()
                .xsmall()
                .label("Turn on")
                .accessibility_label(format!("Turn on: {name}"))
                .disabled(!can_manage || !loaded)
                .on_click(cx.listener(move |this, _, window, cx| this.welcome_turn_on(id.clone(), window, cx)))
                .into_any_element();
        }
        // Settings updates a broken CLI the way it was installed, including a custom binary path.
        if logic::is_broken_version(provider) {
            return settings_action("Update", cx);
        }
        if loaded && logic::codex_needs_setup(provider, settings) {
            let open = self.welcome.codex_setup.as_deref() == Some(id.as_str());
            let id = id.clone();
            return Button::new(element_id("codex"))
                .outline()
                .xsmall()
                .label("Set up")
                .accessibility_label(format!("Set up: {name}"))
                .toggled(open)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.welcome.codex_setup = if open { None } else { Some(id.clone()) };
                    cx.notify();
                }))
                .into_any_element();
        }
        if logic::needs_install_or_sign_in(provider, settings) {
            let label = if state == AgentState::SignIn { "Sign in" } else { "Install" };
            let busy = self.welcome.terminal.as_ref().is_some_and(|t| t.instance_id == id);
            let provider = provider.clone();
            return Button::new(element_id("terminal"))
                .outline()
                .xsmall()
                .icon(Icon::new(IconName::Terminal))
                .label(label)
                .accessibility_label(format!("{label}: {name}"))
                .disabled(busy || !loaded || !can_terminal)
                .on_click(cx.listener(move |this, _, window, cx| this.welcome_open_terminal(&provider, window, cx)))
                .into_any_element();
        }
        settings_action(
            match state {
                AgentState::SignIn => "Sign in",
                AgentState::Attention => "Details",
                _ => "Set up",
            },
            cx,
        )
    }

    /// `OnboardingCodexSetup`: ChatGPT sign-in or the user's own Codex CLI, as one card so either
    /// choice can be undone here.
    fn render_welcome_codex_card(
        &self,
        environment: &SharedString,
        can_manage: bool,
        can_terminal: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let id = self.welcome.codex_setup.clone()?;
        let info = self.env_info(environment)?;
        let provider = info
            .providers_json
            .iter()
            .find(|p| logic::instance_id(p) == id)
            .cloned()
            .or_else(|| self.welcome.last_codex.clone().filter(|p| logic::instance_id(p) == id))?;
        let state = logic::onboarding_state(&provider);
        let checking = state == AgentState::Checking;
        let authenticated = provider.pointer("/auth/status").and_then(Value::as_str) == Some("authenticated");
        let headline = super::settings::provider_summary(Some(&provider)).0;
        let description = if checking {
            "Checking your Codex CLI...".to_owned()
        } else if provider["installed"] == true {
            headline.clone()
        } else {
            "Code with your ChatGPT subscription.".to_owned()
        };
        // A policy that owns Codex's launch settings also rules out ChatGPT's own runtime.
        let locked = info
            .managed_policy
            .pointer("/limits/providers/lockedInstanceFields/codex")
            .and_then(Value::as_array)
            .is_some_and(|paths| paths.iter().any(|p| p.as_str() == Some("config.setupMode")));
        let organization = info.managed_policy["organization"].as_str().unwrap_or("Your organization").to_owned();
        let control: AnyElement = if checking {
            muted_note("Checking...", t)
        } else if authenticated {
            muted_note(headline, t)
        } else {
            let managed_id = id.clone();
            Button::new("welcome-codex-chatgpt")
                .outline()
                .small()
                .label("Continue with ChatGPT")
                .disabled(!can_manage || locked)
                .when(locked, |el| el.tooltip(format!("{organization} manages this setting.")))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.welcome_codex_mode(managed_id.clone(), true, window, cx);
                }))
                .into_any_element()
        };
        let secondary = (!authenticated && !checking).then(|| {
            let cli_id = id.clone();
            Button::new("welcome-codex-cli").ghost().small().label("Use existing CLI").disabled(!can_terminal).on_click(
                cx.listener(move |this, _, window, cx| this.welcome_codex_mode(cli_id.clone(), false, window, cx)),
            )
        });
        Some(
            v_flex()
                .mt(px(8.))
                .gap(px(12.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.border.opacity(0.7))
                .p(px(16.))
                .child(
                    h_flex().gap(px(12.)).child(row_icon(agent_icon("codex", 18., None, t), t)).child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(logic::agent_name(&provider)))
                            .child(div().text_xs().text_color(t.muted_foreground).child(description)),
                    ),
                )
                .child(h_flex().gap(px(8.)).justify_end().children(secondary).child(control))
                .into_any_element(),
        )
    }

    fn render_welcome_terminal(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let terminal = self.welcome.terminal.as_ref()?;
        let operable = self.env_grants(&terminal.environment, TERMINAL_OPERATE);
        let status: AnyElement = if operable {
            match terminal.state {
                SetupState::WriteFailed => h_flex()
                    .flex_wrap()
                    .child("Run\u{a0}")
                    .child(
                        div()
                            .px(px(4.))
                            .rounded(px(4.))
                            .bg(t.muted)
                            .font_family(t.fonts.mono)
                            .child(terminal.command.clone()),
                    )
                    .child("\u{a0}in this terminal.")
                    .into_any_element(),
                SetupState::Ready => div().child("Review the command, then press Enter to run it.").into_any_element(),
                SetupState::OpenFailed => div().child("Could not open the setup terminal.").into_any_element(),
                SetupState::Preparing => div().child("Preparing command...").into_any_element(),
            }
        } else {
            div().child("This connection cannot control terminals.").into_any_element()
        };
        let retry = (terminal.state == SetupState::OpenFailed).then(|| {
            Button::new("welcome-terminal-retry").ghost().xsmall().label("Retry").disabled(!operable).on_click(
                cx.listener(|this, _, window, cx| {
                    let environment = this.welcome.terminal.as_ref().map(|t| t.environment.clone());
                    if environment.is_some_and(|env| this.env_grants(&env, TERMINAL_OPERATE)) {
                        this.welcome_prepare_terminal(window, cx);
                    }
                }),
            )
        });
        let close = Button::new("welcome-terminal-close")
            .ghost()
            .xsmall()
            .label("Close")
            .on_click(cx.listener(|this, _, _, cx| this.welcome_close_terminal(cx)));
        Some(
            v_flex()
                .mt(px(16.))
                .overflow_hidden()
                .rounded(px(8.))
                .border_1()
                .border_color(t.border.opacity(0.7))
                .bg(t.background)
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(8.))
                        .px(px(12.))
                        .py(px(6.))
                        .border_b_1()
                        .border_color(t.border.opacity(0.6))
                        .child(
                            text_2xs(div())
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.muted_foreground)
                                .child(status),
                        )
                        .child(h_flex().gap(px(4.)).children(retry).child(close)),
                )
                .child(div().h(px(256.)).children(terminal.pane.as_ref().map(StandaloneTerminal::view)))
                .into_any_element(),
        )
    }

    fn render_welcome_projects(&mut self, narrow: bool, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let scan = &self.welcome.scan;
        let projects = self.welcome_projects();
        let truncated = scan.data.as_ref().is_some_and(|data| data.truncated);
        let error = scan.error.clone();
        let retry = |id: &'static str, cx: &mut Context<Self>| {
            Button::new(id).ghost().small().label("Retry").on_click(cx.listener(|this, _, _, cx| this.welcome_scan(cx)))
        };
        if projects.candidates.is_empty() {
            let body: AnyElement = if scan.data.is_none() && error.is_none() || scan.pending {
                div()
                    .text_sm()
                    .line_height(px(40.))
                    .text_color(t.muted_foreground)
                    .child(format!("Looking for projects in your {} history…", logic::IMPORT_SOURCES_LABEL))
                    .into_any_element()
            } else if let Some(error) = error {
                h_flex()
                    .min_h(px(40.))
                    .justify_between()
                    .gap(px(12.))
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child(format!("Could not check projects. {error}"))
                    .child(retry("welcome-scan-retry", cx))
                    .into_any_element()
            } else if truncated {
                div()
                    .text_xs()
                    .line_height(px(40.))
                    .text_color(t.muted_foreground)
                    .child(SCAN_LIMIT_MESSAGE)
                    .into_any_element()
            } else {
                return None;
            };
            return Some(section("Projects", body, narrow, t).into_any_element());
        }
        let can_write = self.welcome_can_import();
        let importing = self.welcome.importing;
        let selected = if can_write { projects.selected.len() } else { 0 };
        let title = if selected > 0 {
            format!("Import {selected} {} and their threads", if selected == 1 { "project" } else { "projects" })
        } else {
            "Import projects and their threads".to_owned()
        };
        let defaults = projects.defaults.clone();
        let main = Checkbox::new("welcome-import")
            .checked(selected > 0)
            .disabled(importing || !can_write)
            .accessibility_label(title.clone())
            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                let keys = if *checked { defaults.clone() } else { Vec::new() };
                this.welcome_set_selected(keys, !*checked, cx);
            }));
        let reviewing = self.welcome.reviewing && can_write;
        let review = can_write.then(|| {
            Button::new("welcome-review")
                .ghost()
                .xsmall()
                .label(if reviewing { "Hide".to_owned() } else { format!("Review all {}", projects.candidates.len()) })
                .icon(Icon::new(if reviewing { IconName::ChevronUp } else { IconName::ChevronDown }))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.welcome.reviewing = !this.welcome.reviewing;
                    cx.notify();
                }))
        });
        let head = h_flex()
            .min_h(px(40.))
            .py(px(4.))
            .gap(px(12.))
            .child(row_icon(main, t))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(title))
                    .child(
                        div()
                            .text_xs()
                            .text_color(t.muted_foreground)
                            .child(format!("From your {} history", logic::IMPORT_SOURCES_LABEL)),
                    ),
            )
            .children(review);
        let list = reviewing.then(|| self.render_welcome_review(&projects, importing, t, cx));
        let body = v_flex()
            .child(head)
            .children(list)
            .when(truncated, |el| {
                el.child(div().pl(px(32.)).text_xs().text_color(t.muted_foreground).child(SCAN_LIMIT_MESSAGE))
            })
            .when_some(error, |el, error| {
                el.child(
                    h_flex()
                        .pl(px(32.))
                        .justify_between()
                        .gap(px(12.))
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child(format!("Could not check projects again. {error}"))
                        .child(retry("welcome-rescan-retry", cx)),
                )
            })
            .when(!can_write, |el| {
                el.child(
                    div()
                        .pt(px(4.))
                        .pl(px(32.))
                        .text_sm()
                        .text_color(t.error_foreground)
                        .child(IMPORT_PERMISSION_MESSAGE),
                )
            });
        Some(section("Projects", body, narrow, t).into_any_element())
    }

    /// Review all: every project found, by repository, with Select all and Select none.
    fn render_welcome_review(
        &self,
        projects: &Projects,
        importing: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let total = projects.candidates.len();
        let selected = projects.selected.len();
        let all: Vec<String> = projects.candidates.iter().map(|c| c.key.clone()).collect();
        let (groups, other) = logic::group_projects(&projects.candidates);
        let now = crate::timestamp::now_ms();
        let mut rows: Vec<AnyElement> = Vec::new();
        for group in &groups {
            let keys: Vec<String> = group.candidates.iter().map(|c| c.key.clone()).collect();
            if let [only] = group.candidates.as_slice() {
                rows.push(self.render_welcome_candidate(
                    only,
                    Some(&group.label),
                    false,
                    projects,
                    importing,
                    now,
                    t,
                    cx,
                ));
                continue;
            }
            let open = !self.welcome.folded.contains(&group.key);
            let count = keys.iter().filter(|k| projects.is_selected(k)).count();
            let sources: Vec<String> = {
                let mut seen: Vec<String> = Vec::new();
                for source in group.candidates.iter().flat_map(|c| &c.sources) {
                    if !seen.contains(source) {
                        seen.push(source.clone());
                    }
                }
                seen
            };
            let fold_key = group.key.clone();
            rows.push(
                group_row(
                    SharedString::from(format!("welcome-group-{}", group.key)),
                    count == keys.len(),
                    importing,
                    keys.clone(),
                    open,
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(group.label.clone())
                        .into_any_element(),
                    import_meta(Some(&sources), group.thread_count, group.last_active_at.as_deref(), now, t),
                    move |this: &mut Self, cx: &mut Context<Self>| {
                        if !this.welcome.folded.remove(&fold_key) {
                            this.welcome.folded.insert(fold_key.clone());
                        }
                        cx.notify();
                    },
                    t,
                    cx,
                )
                .into_any_element(),
            );
            if open {
                rows.extend(
                    group
                        .candidates
                        .iter()
                        .map(|c| self.render_welcome_candidate(c, None, true, projects, importing, now, t, cx)),
                );
            }
        }
        if !other.is_empty() {
            let keys: Vec<String> = other.iter().map(|c| c.key.clone()).collect();
            let count = keys.iter().filter(|k| projects.is_selected(k)).count();
            let open = self.welcome.other_open;
            let label = format!("{} {}", other.len(), if other.len() == 1 { "folder" } else { "folders" });
            rows.push(
                group_row(
                    "welcome-group-other".into(),
                    count == keys.len(),
                    importing,
                    keys,
                    open,
                    div().truncate().text_sm().text_color(t.muted_foreground).child("Other folders").into_any_element(),
                    div()
                        .ml_auto()
                        .flex_none()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(label)
                        .into_any_element(),
                    |this: &mut Self, cx: &mut Context<Self>| {
                        this.welcome.other_open = !this.welcome.other_open;
                        cx.notify();
                    },
                    t,
                    cx,
                )
                .into_any_element(),
            );
            if open {
                rows.extend(
                    other.iter().map(|c| self.render_welcome_candidate(c, None, true, projects, importing, now, t, cx)),
                );
            }
        }
        v_flex()
            .mt(px(4.))
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(8.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(format!("{selected} of {total} selected"))
                    .child(
                        h_flex()
                            .gap(px(4.))
                            .child(
                                Button::new("welcome-select-all")
                                    .ghost()
                                    .xsmall()
                                    .label("Select all")
                                    .disabled(importing || selected == total)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.welcome_set_selected(all.clone(), false, cx);
                                    })),
                            )
                            .child(
                                Button::new("welcome-select-none")
                                    .ghost()
                                    .xsmall()
                                    .label("Select none")
                                    .disabled(importing || selected == 0)
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.welcome_set_selected(Vec::new(), false, cx)),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .id("welcome-review-list")
                    .mt(px(4.))
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .pr(px(12.))
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_welcome_candidate(
        &self,
        candidate: &Candidate,
        label: Option<&str>,
        nested: bool,
        projects: &Projects,
        importing: bool,
        now: i64,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = candidate.key.clone();
        let checked = projects.is_selected(&key);
        let (name, parent) = logic::split_project_path(&candidate.path);
        let path_size = if label.is_some() { 11. } else { 12. };
        let path = h_flex()
            .min_w_0()
            .font_family(t.fonts.mono)
            .text_size(px(path_size))
            .when(label.is_some(), |el| el.text_color(t.muted_foreground))
            .when(!parent.is_empty(), |el| el.child(div().min_w_0().truncate().child(parent.to_owned())))
            .child(div().flex_none().max_w_full().truncate().child(name.to_owned()));
        h_flex()
            .id(SharedString::from(format!("welcome-candidate-{key}")))
            .gap(px(10.))
            .rounded(px(6.))
            .px(px(8.))
            .py(px(6.))
            .when(nested, |el| el.pl(px(32.)))
            .hover(|style| style.bg(t.muted.opacity(0.4)))
            .child(
                Checkbox::new(SharedString::from(format!("welcome-candidate-check-{key}")))
                    .checked(checked)
                    .disabled(importing)
                    .accessibility_label(candidate.path.clone())
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.welcome_toggle_keys(std::slice::from_ref(&key), *checked, cx);
                    })),
            )
            .child(
                h_flex()
                    .id(SharedString::from(format!("welcome-candidate-path-{}", candidate.key)))
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.))
                    .overflow_hidden()
                    .when_some(label, |el, label| {
                        el.child(div().truncate().text_sm().font_weight(FontWeight::MEDIUM).child(label.to_owned()))
                    })
                    .child(path)
                    .tooltip(tooltip(candidate.path.clone())),
            )
            .child(import_meta(
                (!nested).then_some(&candidate.sources),
                candidate.thread_count,
                candidate.last_active_at.as_deref(),
                now,
                t,
            ))
            .into_any_element()
    }
}

/// A group's row in Review all: a box for the whole group and a fold.
fn group_row(
    id: SharedString,
    all_checked: bool,
    importing: bool,
    keys: Vec<String>,
    open: bool,
    label: AnyElement,
    meta: AnyElement,
    on_fold: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static,
    t: &Tokens,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    h_flex()
        .gap(px(10.))
        .rounded(px(6.))
        .px(px(8.))
        .py(px(6.))
        .hover(|style| style.bg(t.muted.opacity(0.4)))
        .child(
            Checkbox::new(SharedString::from(format!("{id}-check"))).checked(all_checked).disabled(importing).on_click(
                cx.listener(move |this, checked: &bool, _, cx| this.welcome_toggle_keys(&keys, *checked, cx)),
            ),
        )
        .child(
            Button::new(SharedString::from(format!("{id}-fold")))
                .ghost()
                .xsmall()
                .flex_1()
                .min_w_0()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap(px(6.))
                        .child(
                            Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                                .size(px(14.))
                                .text_color(t.muted_foreground),
                        )
                        .child(label)
                        .child(meta),
                )
                .on_click(cx.listener(move |this, _, _, cx| on_fold(this, cx))),
        )
}

/// `ImportRowMeta`: which agents' history it has, its thread count and its age.
fn import_meta(
    sources: Option<&Vec<String>>,
    threads: u64,
    last_active_at: Option<&str>,
    now: i64,
    t: &Tokens,
) -> AnyElement {
    let slot = |source: &str| {
        let has = sources.is_some_and(|sources| sources.iter().any(|s| s == source));
        div()
            .size(px(16.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .when(has, |el| el.child(agent_icon(source, 12., Some(t.muted_foreground), t)))
    };
    h_flex()
        .ml_auto()
        .flex_none()
        .gap(px(4.))
        .text_xs()
        .text_color(t.muted_foreground)
        .children(logic::IMPORT_SOURCES.iter().map(|(source, _)| slot(source)))
        .child(div().w(px(40.)).text_right().child(threads.to_string()))
        .child(div().w(px(36.)).text_right().whitespace_nowrap().child(logic::age_label(last_active_at, now)))
        .into_any_element()
}

/// `AgentRow`; `compact` is a row in the "Add more agents" grid, where the button carries the state.
fn agent_row(provider: &Value, state: AgentState, action: AnyElement, compact: bool, t: &Tokens) -> AnyElement {
    let name = logic::agent_name(provider);
    let driver = provider["driver"].as_str().unwrap_or_default();
    let text =
        |pointer: &str| provider.pointer(pointer).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
    let broken = logic::is_broken_version(provider).then(|| text("/compatibilityAdvisory/message")).flatten();
    let headline = || super::settings::provider_summary(Some(provider)).0;
    let detail = match state {
        AgentState::Install => (!logic::is_installable(driver)).then(|| "Not installed".to_owned()),
        _ if compact || matches!(state, AgentState::Ready | AgentState::Checking) => None,
        AgentState::Attention => {
            Some(text("/unavailableReason").or(broken).or_else(|| text("/message")).unwrap_or_else(headline))
        }
        _ => Some(broken.unwrap_or_else(headline)),
    };
    let status = match state {
        AgentState::Ready => ready_label(t),
        AgentState::Checking => muted_note("Checking…", t),
        _ => action,
    };
    h_flex()
        .gap(px(12.))
        .map(|el| if compact { el.min_h(px(36.)) } else { el.min_h(px(40.)).py(px(4.)) })
        .child(row_icon(agent_icon(driver, if compact { 16. } else { 18. }, None, t), t))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .text_color(t.foreground)
                        .when(!compact, |el| el.font_weight(FontWeight::MEDIUM))
                        .child(name),
                )
                .children(
                    detail.map(|detail| div().text_xs().line_clamp(2).text_color(t.muted_foreground).child(detail)),
                ),
        )
        .child(div().flex_none().child(status))
        .into_any_element()
}
