//! The Welcome page's logic (`welcomePage.logic.ts`, `providerReadiness.logic.ts`): which agents
//! are ready to start with, and what finishing setup reports.

use serde_json::Value;

use crate::model::Provider;
use crate::settings::Settings;

/// `welcomeAgentState`: an agent's state for new threads on this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum AgentState {
    Ready,
    SignIn,
    Install,
    Checking,
    Disabled,
    Attention,
}

/// `welcomeAgentState` over `getOnboardingProviderState`. The server also lists agents only some
/// projects turn on, lists one it could not load as unavailable, and can pass a CLI whose
/// version it knows is broken.
pub(in crate::app) fn agent_state(provider: &Provider, settings: &Settings) -> AgentState {
    if provider.availability.as_deref() == Some("unavailable") {
        return AgentState::Attention;
    }
    if !crate::settings::scope::provider_instance_enabled(settings, None, &provider.instance_id) {
        return AgentState::Disabled;
    }
    let state = onboarding_state(provider);
    let broken = provider.compatibility_advisory.as_ref().and_then(|a| a.get("status")) == Some(&"broken".into());
    match state {
        AgentState::Ready if broken => AgentState::Attention,
        // An ACP agent that is not prepared reports the same fields as one still being checked.
        AgentState::Checking if provider.driver == "acpRegistry" => AgentState::Attention,
        state => state,
    }
}

/// `getOnboardingProviderState`: the provider's own report, without this computer's settings.
pub(in crate::app) fn onboarding_state(provider: &Provider) -> AgentState {
    if !provider.enabled || provider.status == "disabled" {
        AgentState::Disabled
    } else if !provider.installed && provider.status == "warning" && provider.auth.status == "unknown" {
        AgentState::Checking
    } else if !provider.installed {
        AgentState::Install
    } else if provider.auth.status == "unauthenticated" {
        AgentState::SignIn
    } else if provider.status == "ready" {
        AgentState::Ready
    } else {
        AgentState::Attention
    }
}

/// `isManagedDriverAllowed`.
pub(in crate::app) fn driver_allowed(policy: &Value, driver: &str) -> bool {
    match policy.pointer("/limits/providers/allowedDrivers").and_then(Value::as_array) {
        Some(allowed) => allowed.iter().any(|d| d.as_str() == Some(driver)),
        None => true,
    }
}

/// `groupWelcomeAgents(...).readyCount`: agents the policy allows that can start a chat now.
pub(in crate::app) fn ready_agent_count(providers: &[Provider], settings: &Settings, policy: &Value) -> usize {
    group_agents(Some(providers), settings, policy).ready_count
}

/// `groupWelcomeAgents`: a computer's allowed agents.
pub(in crate::app) struct WelcomeAgents<'a> {
    /// The list has not loaded, or an agent is still being checked.
    pub checking: bool,
    pub ready_count: usize,
    /// On this computer: ready first, then ones needing a sign-in, then a look; by name within.
    pub found: Vec<&'a Provider>,
    /// To add, by name; agents still being checked wait here, so a slow check never drops one.
    pub available: Vec<&'a Provider>,
}

/// `groupWelcomeAgents`; `providers` is `None` until the computer's config arrives.
pub(in crate::app) fn group_agents<'a>(
    providers: Option<&'a [Provider]>,
    settings: &Settings,
    policy: &Value,
) -> WelcomeAgents<'a> {
    let mut agents =
        WelcomeAgents { checking: providers.is_none(), ready_count: 0, found: Vec::new(), available: Vec::new() };
    let mut found = Vec::new();
    for provider in providers.unwrap_or_default().iter().filter(|p| driver_allowed(policy, &p.driver)) {
        let state = agent_state(provider, settings);
        agents.ready_count += usize::from(state == AgentState::Ready);
        agents.checking |= state == AgentState::Checking;
        match state {
            AgentState::Ready => found.push((0, provider)),
            AgentState::SignIn => found.push((1, provider)),
            AgentState::Attention => found.push((2, provider)),
            _ => agents.available.push(provider),
        }
    }
    let name = |p: &Provider| agent_name(p).to_lowercase();
    found.sort_by_cached_key(|(rank, provider)| (*rank, name(provider)));
    agents.found = found.into_iter().map(|(_, provider)| provider).collect();
    agents.available.sort_by_cached_key(|provider| name(provider));
    agents
}

/// `PROVIDER_DISPLAY_NAMES`, else the driver humanized.
fn kind_label(driver: &str) -> String {
    let known = match driver {
        "antigravity" => "Antigravity",
        "codex" => "Codex",
        "claudeAgent" => "Claude",
        "cursor" => "Cursor",
        "grok" => "Grok",
        "fx" => "fx",
        "kiro" => "Kiro",
        "muse" => "Muse Code",
        "acpRegistry" => "ACP Registry",
        "pi" => "Pi",
        "opencode" => "OpenCode",
        other => return humanize_slug(other),
    };
    known.to_owned()
}

/// `humanizeSlug`: `claudeAgent_work` reads "Claude Agent Work".
fn humanize_slug(slug: &str) -> String {
    let mut spaced = String::with_capacity(slug.len() + 4);
    let mut previous: Option<char> = None;
    for c in slug.chars() {
        if c.is_ascii_uppercase() && previous.is_some_and(|p| p.is_ascii_lowercase()) {
            spaced.push(' ');
        }
        if c == '_' || c == '-' {
            if !spaced.ends_with(' ') {
                spaced.push(' ');
            }
        } else {
            spaced.push(c);
        }
        previous = Some(c);
    }
    let mut word = false;
    spaced
        .trim()
        .chars()
        .map(|c| {
            let starts = !word && c.is_alphanumeric();
            word = c.is_alphanumeric();
            if starts { c.to_ascii_uppercase() } else { c }
        })
        .collect()
}

/// `resolveProviderInstanceDisplayName`: the name the user sees for an agent.
pub(in crate::app) fn agent_name(provider: &Provider) -> String {
    let kind = kind_label(&provider.driver);
    let named = provider.display_name.as_deref().map(str::trim).filter(|name| !name.is_empty());
    if let Some(name) = named.filter(|name| *name != kind) {
        return name.to_owned();
    }
    if provider.instance_id != provider.driver {
        let humanized = humanize_slug(&provider.instance_id);
        if !humanized.is_empty() {
            return humanized;
        }
    }
    named.map_or(kind, str::to_owned)
}

/// The agents the Welcome page installs and signs in itself, in a terminal.
pub(in crate::app) fn installable(driver: &str) -> bool {
    matches!(driver, "codex" | "claudeAgent")
}

/// `isBrokenVersion`: a CLI the server knows is too old or broken; Settings updates it.
pub(in crate::app) fn is_broken_version(provider: &Provider) -> bool {
    provider.installed
        && provider.compatibility_advisory.as_ref().and_then(|a| a.get("status")).and_then(Value::as_str)
            == Some("broken")
}

fn available(provider: &Provider) -> bool {
    provider.availability.as_deref() != Some("unavailable")
}

/// Whether the row's action opens a terminal to install or sign in.
pub(in crate::app) fn needs_install_or_sign_in(provider: &Provider, settings: &Settings) -> bool {
    installable(&provider.driver)
        && !is_broken_version(provider)
        && matches!(agent_state(provider, settings), AgentState::Install | AgentState::SignIn)
}

/// `codexNeedsSetup`: whether Codex's row opens its setup card (ChatGPT sign-in or the CLI).
pub(in crate::app) fn codex_needs_setup(provider: &Provider, settings: &Settings) -> bool {
    if provider.driver != "codex" || provider.auth.status == "authenticated" || is_broken_version(provider) {
        return false;
    }
    match agent_state(provider, settings) {
        AgentState::Install | AgentState::SignIn => true,
        AgentState::Attention => available(provider),
        _ => false,
    }
}

/// `codexCardProvider`'s test: the card stays while setup is still needed or being checked.
pub(in crate::app) fn codex_card_open(provider: &Provider, settings: &Settings) -> bool {
    codex_needs_setup(provider, settings) || agent_state(provider, settings) == AgentState::Checking
}

/// `cliTerminalStep`: what "Use existing CLI" does next for Codex.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum CliStep {
    /// Codex has not checked the CLI yet (or still reports the ChatGPT runtime).
    Wait,
    /// Open the CLI's install or sign-in terminal.
    Open,
    Done,
}

pub(in crate::app) fn cli_terminal_step(provider: &Provider, settings: &Settings) -> CliStep {
    if provider.setup.as_ref().is_some_and(|setup| setup.can_authenticate) {
        return CliStep::Wait;
    }
    let state = agent_state(provider, settings);
    if state == AgentState::Checking {
        return CliStep::Wait;
    }
    // A broken version is updated from its row, and an installed, signed-in CLI needs no terminal.
    if is_broken_version(provider) || (provider.installed && provider.auth.status == "authenticated") {
        return CliStep::Done;
    }
    // The terminal shows the command first, so it also helps a CLI whose check failed.
    match state {
        AgentState::Install | AgentState::SignIn => CliStep::Open,
        AgentState::Attention if available(provider) => CliStep::Open,
        _ => CliStep::Done,
    }
}

/// `actionFor`: one action per agent row, the same in the list and in the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum RowAction {
    TurnOn,
    /// Codex's setup card (ChatGPT sign-in or the CLI) opens below the list.
    CodexSetup,
    /// The install or sign-in terminal.
    Terminal {
        sign_in: bool,
    },
    /// Settings › Providers on the agent, labelled.
    Settings(&'static str),
}

pub(in crate::app) fn row_action(provider: &Provider, settings: &Settings) -> RowAction {
    let state = agent_state(provider, settings);
    if state == AgentState::Disabled {
        return RowAction::TurnOn;
    }
    // Settings updates a broken CLI the way it was installed, including a custom binary path.
    if is_broken_version(provider) {
        return RowAction::Settings("Update");
    }
    if codex_needs_setup(provider, settings) {
        return RowAction::CodexSetup;
    }
    if needs_install_or_sign_in(provider, settings) {
        return RowAction::Terminal { sign_in: state == AgentState::SignIn };
    }
    RowAction::Settings(match state {
        AgentState::SignIn => "Sign in",
        AgentState::Attention => "Details",
        _ => "Set up",
    })
}

/// `turnedOnInstance`: on, including an older `config.enabled: false`.
pub(in crate::app) fn turned_on_instance(instance: &Value, driver: &str) -> Value {
    let mut next = if instance.is_object() { instance.clone() } else { serde_json::json!({ "driver": driver }) };
    next["enabled"] = true.into();
    if next.pointer("/config/enabled") == Some(&Value::Bool(false)) {
        next["config"]["enabled"] = true.into();
    }
    next
}

/// `resolveOnboardingProviderInstallCommand`: native installers that need no Node.
pub(in crate::app) fn install_command(driver: &str, os: &str) -> &'static str {
    match (driver, os == "windows") {
        ("claudeAgent", true) => "irm https://claude.ai/install.ps1 | iex",
        ("claudeAgent", false) => "curl -fsSL https://claude.ai/install.sh | bash",
        (_, true) => "irm https://chatgpt.com/codex/install.ps1 | iex",
        (_, false) => "curl -fsSL https://chatgpt.com/codex/install.sh | sh",
    }
}

/// `resolveOnboardingProviderLoginCommand`: the CLI's own login, from its configured binary.
pub(in crate::app) fn login_command(provider: &Provider, settings: &Settings, os: &str) -> String {
    let (fallback, login) = match provider.driver.as_str() {
        "claudeAgent" => ("claude", "auth login"),
        "codex" => ("codex", "login"),
        other => return other.to_owned(),
    };
    let binary = settings["providerInstances"][provider.instance_id.as_str()]["config"]["binaryPath"]
        .as_str()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or(fallback);
    format!("{} {login}", quote_binary(binary, fallback, os))
}

/// `quoteProviderBinary`: a configured path as the computer's shell reads it.
fn quote_binary(path: &str, fallback: &str, os: &str) -> String {
    let safe = !path.is_empty()
        && path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | ':' | '\\' | '-'));
    if safe && (os == "windows" || !path.contains('\\')) {
        return path.to_owned();
    }
    let posix = |text: &str| format!("'{}'", text.replace('\'', r#"'"'"'"#));
    match os {
        "windows" => format!("& '{}'", path.replace('\'', "''")),
        "darwin" | "linux" => match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
            Some(rest) => format!("~/{}", posix(rest)),
            None => posix(path),
        },
        _ => fallback.to_owned(),
    }
}

/// What an import before finishing left to report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(in crate::app) struct ImportOutcome {
    /// Why some history was left out; it stays on screen until dismissed.
    pub warning: Option<String>,
    pub imported_threads: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) enum FinishToast {
    PartialHistory(String),
    Imported(String),
}

/// The toast after setup is saved: a partial import's warning wins over its count.
pub(in crate::app) fn finish_toast(outcome: &ImportOutcome) -> Option<FinishToast> {
    if let Some(warning) = outcome.warning.as_ref().filter(|w| !w.is_empty()) {
        return Some(FinishToast::PartialHistory(warning.clone()));
    }
    match outcome.imported_threads {
        0 => None,
        1 => Some(FinishToast::Imported("Imported 1 thread".into())),
        n => Some(FinishToast::Imported(format!("Imported {n} threads"))),
    }
}

/// `TelemetryNotice`'s text before its privacy policy link, under an administrator policy or not.
pub(in crate::app) fn telemetry_notice(policy: &Value) -> String {
    let organization =
        policy["organization"].as_str().filter(|_| policy.pointer("/limits/telemetry") == Some(&false.into()));
    match organization {
        Some(organization) => format!("{organization} turned off anonymous usage data. See the"),
        None => "T3 Code collects anonymous usage data to improve the app. To opt out, see the".into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn provider(fields: Value) -> Provider {
        let mut value = json!({
            "instanceId": "codex", "driver": "codex", "enabled": true, "installed": true,
            "status": "ready", "auth": {"status": "authenticated"},
        });
        if let (Some(base), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
            base.extend(fields.clone());
        }
        serde_json::from_value(value).expect("provider")
    }

    fn defaults() -> Settings {
        Settings::layered(&crate::settings::contract().server_defaults, &Value::Null)
    }

    #[test]
    fn agent_states_follow_the_web() {
        let settings = defaults();
        let cases = [
            (json!({}), AgentState::Ready),
            (json!({"availability": "unavailable"}), AgentState::Attention),
            (json!({"enabled": false}), AgentState::Disabled),
            (json!({"status": "disabled"}), AgentState::Disabled),
            (json!({"installed": false, "status": "warning", "auth": {"status": "unknown"}}), AgentState::Checking),
            (
                json!({"driver": "acpRegistry", "installed": false, "status": "warning", "auth": {"status": "unknown"}}),
                AgentState::Attention,
            ),
            (json!({"installed": false, "status": "error"}), AgentState::Install),
            (json!({"auth": {"status": "unauthenticated"}}), AgentState::SignIn),
            (json!({"status": "warning"}), AgentState::Attention),
            (json!({"compatibilityAdvisory": {"status": "broken"}}), AgentState::Attention),
            // Unconfigured, only the default Codex and Claude instances are on.
            (json!({"instanceId": "cursor", "driver": "cursor"}), AgentState::Disabled),
        ];
        for (fields, expected) in cases {
            assert_eq!(agent_state(&provider(fields.clone()), &settings), expected, "{fields}");
        }
        let off = Settings::layered(
            &crate::settings::contract().server_defaults,
            &json!({"providerInstances": {"codex": {"driver": "codex", "enabled": false}}}),
        );
        assert_eq!(agent_state(&provider(json!({})), &off), AgentState::Disabled);
    }

    #[test]
    fn only_ready_agents_the_policy_allows_count() {
        let providers = [
            provider(json!({})),
            provider(json!({"instanceId": "claudeAgent", "driver": "claudeAgent"})),
            provider(json!({"instanceId": "claude-2", "driver": "claudeAgent", "auth": {"status": "unauthenticated"}})),
        ];
        assert_eq!(ready_agent_count(&providers, &defaults(), &Value::Null), 2);
        let policy = json!({"limits": {"providers": {"allowedDrivers": ["claudeAgent"]}}});
        assert_eq!(ready_agent_count(&providers, &defaults(), &policy), 1);
        assert_eq!(ready_agent_count(&[], &defaults(), &Value::Null), 0);
    }

    #[test]
    fn agents_group_into_found_and_available_by_state_then_name() {
        let providers = [
            provider(json!({"instanceId": "codex-work", "auth": {"status": "unauthenticated"}})),
            provider(json!({"instanceId": "claudeAgent", "driver": "claudeAgent", "status": "warning"})),
            provider(json!({"instanceId": "codex", "displayName": "Zed Codex"})),
            provider(json!({"instanceId": "cursor", "driver": "cursor"})),
            provider(
                json!({"instanceId": "opencode", "driver": "opencode", "installed": false, "status": "warning", "auth": {"status": "unknown"}}),
            ),
        ];
        let settings = Settings::layered(
            &crate::settings::contract().server_defaults,
            &json!({"providerInstances": {
                "codex-work": {"driver": "codex"},
                "opencode": {"driver": "opencode", "enabled": true},
            }}),
        );
        let agents = group_agents(Some(&providers), &settings, &Value::Null);
        let ids = |list: &[&Provider]| list.iter().map(|p| p.instance_id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&agents.found), ["codex", "codex-work", "claudeAgent"]);
        // Off (Cursor) and still checking (OpenCode) wait with the agents to add.
        assert_eq!(ids(&agents.available), ["cursor", "opencode"]);
        assert!(agents.checking);
        assert_eq!(agents.ready_count, 1);
        let unloaded = group_agents(None, &settings, &Value::Null);
        assert!(unloaded.checking && unloaded.found.is_empty());
    }

    #[test]
    fn agent_names_follow_the_display_name_rules() {
        assert_eq!(agent_name(&provider(json!({}))), "Codex");
        assert_eq!(agent_name(&provider(json!({"displayName": " Codex "}))), "Codex");
        assert_eq!(agent_name(&provider(json!({"displayName": "Work"}))), "Work");
        assert_eq!(agent_name(&provider(json!({"instanceId": "codex_work-two"}))), "Codex Work Two");
        assert_eq!(agent_name(&provider(json!({"instanceId": "myCodex", "displayName": "Codex"}))), "My Codex");
        assert_eq!(agent_name(&provider(json!({"instanceId": "claudeAgent", "driver": "claudeAgent"}))), "Claude");
        assert_eq!(agent_name(&provider(json!({"instanceId": "newAgent", "driver": "newAgent"}))), "New Agent");
    }

    #[test]
    fn codex_setup_and_the_cli_terminal_follow_its_state() {
        let settings = defaults();
        let signed_out = provider(json!({"auth": {"status": "unauthenticated"}}));
        assert!(codex_needs_setup(&signed_out, &settings));
        assert!(!codex_needs_setup(&provider(json!({})), &settings), "signed in");
        assert!(!codex_needs_setup(&provider(json!({"compatibilityAdvisory": {"status": "broken"}})), &settings));
        let claude = provider(
            json!({"instanceId": "claudeAgent", "driver": "claudeAgent", "auth": {"status": "unauthenticated"}}),
        );
        assert!(!codex_needs_setup(&claude, &settings));
        assert!(needs_install_or_sign_in(&claude, &settings));
        let checking = provider(json!({"installed": false, "status": "warning", "auth": {"status": "unknown"}}));
        assert!(codex_card_open(&checking, &settings) && !codex_needs_setup(&checking, &settings));

        assert_eq!(cli_terminal_step(&checking, &settings), CliStep::Wait);
        let chatgpt = provider(
            json!({"auth": {"status": "unauthenticated"}, "setup": {"canAuthenticate": true, "canInstall": true}}),
        );
        assert_eq!(cli_terminal_step(&chatgpt, &settings), CliStep::Wait);
        assert_eq!(cli_terminal_step(&signed_out, &settings), CliStep::Open);
        assert_eq!(
            cli_terminal_step(&provider(json!({"installed": false, "status": "error"})), &settings),
            CliStep::Open
        );
        assert_eq!(cli_terminal_step(&provider(json!({"status": "warning"})), &settings), CliStep::Done);
        assert_eq!(
            cli_terminal_step(&provider(json!({"status": "warning", "auth": {"status": "unknown"}})), &settings),
            CliStep::Open
        );
        assert_eq!(
            cli_terminal_step(
                &provider(json!({"availability": "unavailable", "auth": {"status": "unknown"}})),
                &settings
            ),
            CliStep::Done
        );
    }

    #[test]
    fn each_row_offers_the_action_its_state_needs() {
        let settings = defaults();
        let action = |fields: Value| row_action(&provider(fields), &settings);
        assert_eq!(action(json!({"instanceId": "cursor", "driver": "cursor"})), RowAction::TurnOn);
        assert_eq!(action(json!({"compatibilityAdvisory": {"status": "broken"}})), RowAction::Settings("Update"));
        assert_eq!(action(json!({"auth": {"status": "unauthenticated"}})), RowAction::CodexSetup);
        let claude =
            json!({"instanceId": "claudeAgent", "driver": "claudeAgent", "installed": false, "status": "error"});
        assert_eq!(action(claude), RowAction::Terminal { sign_in: false });
        let claude =
            json!({"instanceId": "claudeAgent", "driver": "claudeAgent", "auth": {"status": "unauthenticated"}});
        assert_eq!(action(claude), RowAction::Terminal { sign_in: true });
        let kiro = |fields: Value| {
            let mut base = json!({"instanceId": "kiro", "driver": "kiro"});
            base.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
            let on = Settings::layered(
                &crate::settings::contract().server_defaults,
                &json!({"providerInstances": {"kiro": {"driver": "kiro", "enabled": true}}}),
            );
            row_action(&provider(base), &on)
        };
        assert_eq!(kiro(json!({"auth": {"status": "unauthenticated"}})), RowAction::Settings("Sign in"));
        assert_eq!(kiro(json!({"status": "warning"})), RowAction::Settings("Details"));
        assert_eq!(kiro(json!({"installed": false, "status": "error"})), RowAction::Settings("Set up"));
    }

    #[test]
    fn turning_on_also_clears_an_older_config_flag() {
        assert_eq!(turned_on_instance(&Value::Null, "cursor"), json!({"driver": "cursor", "enabled": true}));
        assert_eq!(
            turned_on_instance(
                &json!({"driver": "codex", "enabled": false, "config": {"enabled": false, "x": 1}}),
                "codex"
            ),
            json!({"driver": "codex", "enabled": true, "config": {"enabled": true, "x": 1}})
        );
    }

    #[test]
    fn setup_commands_match_the_computer_and_quote_custom_binaries() {
        assert_eq!(install_command("codex", "windows"), "irm https://chatgpt.com/codex/install.ps1 | iex");
        assert_eq!(install_command("claudeAgent", "darwin"), "curl -fsSL https://claude.ai/install.sh | bash");
        let settings = |path: &str| {
            Settings::layered(
                &crate::settings::contract().server_defaults,
                &json!({"providerInstances": {"codex": {"driver": "codex", "config": {"binaryPath": path}}}}),
            )
        };
        let codex = provider(json!({}));
        assert_eq!(login_command(&codex, &defaults(), "linux"), "codex login");
        assert_eq!(login_command(&codex, &settings("/opt/bin/codex"), "linux"), "/opt/bin/codex login");
        assert_eq!(login_command(&codex, &settings("~/my bin/codex"), "darwin"), "~/'my bin/codex' login");
        assert_eq!(login_command(&codex, &settings("it's/codex"), "linux"), r#"'it'"'"'s/codex' login"#);
        assert_eq!(login_command(&codex, &settings(r"C:\Tools\codex.exe"), "windows"), r"C:\Tools\codex.exe login");
        assert_eq!(login_command(&codex, &settings(r"C:\My Tools\codex"), "windows"), r"& 'C:\My Tools\codex' login");
        assert_eq!(login_command(&codex, &settings(r"a\b"), "unknown"), "codex login");
        let claude = provider(json!({"instanceId": "claudeAgent", "driver": "claudeAgent"}));
        assert_eq!(login_command(&claude, &defaults(), "linux"), "claude auth login");
    }

    #[test]
    fn finishing_reports_a_partial_import_before_its_count() {
        assert_eq!(finish_toast(&ImportOutcome::default()), None);
        let one = ImportOutcome { warning: None, imported_threads: 1 };
        assert_eq!(finish_toast(&one), Some(FinishToast::Imported("Imported 1 thread".into())));
        let many = ImportOutcome { warning: Some(String::new()), imported_threads: 3 };
        assert_eq!(finish_toast(&many), Some(FinishToast::Imported("Imported 3 threads".into())));
        let partial = ImportOutcome { warning: Some("Scan limit reached.".into()), imported_threads: 3 };
        assert_eq!(finish_toast(&partial), Some(FinishToast::PartialHistory("Scan limit reached.".into())));
    }

    #[test]
    fn the_telemetry_notice_names_an_organization_that_turned_it_off() {
        let lead = "T3 Code collects anonymous usage data to improve the app. To opt out, see the";
        assert_eq!(telemetry_notice(&Value::Null), lead);
        assert_eq!(telemetry_notice(&json!({"organization": "Acme", "limits": {"telemetry": true}})), lead);
        assert_eq!(
            telemetry_notice(&json!({"organization": "Acme", "limits": {"telemetry": false}})),
            "Acme turned off anonymous usage data. See the"
        );
    }
}
