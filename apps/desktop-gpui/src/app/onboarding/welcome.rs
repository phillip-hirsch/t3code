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
    let state = if !provider.enabled || provider.status == "disabled" {
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
    };
    let broken = provider.compatibility_advisory.as_ref().and_then(|a| a.get("status")) == Some(&"broken".into());
    match state {
        AgentState::Ready if broken => AgentState::Attention,
        // An ACP agent that is not prepared reports the same fields as one still being checked.
        AgentState::Checking if provider.driver == "acpRegistry" => AgentState::Attention,
        state => state,
    }
}

/// `isManagedDriverAllowed`.
fn driver_allowed(policy: &Value, driver: &str) -> bool {
    match policy.pointer("/limits/providers/allowedDrivers").and_then(Value::as_array) {
        Some(allowed) => allowed.iter().any(|d| d.as_str() == Some(driver)),
        None => true,
    }
}

/// `groupWelcomeAgents(...).readyCount`: agents the policy allows that can start a chat now.
pub(in crate::app) fn ready_agent_count(providers: &[Provider], settings: &Settings, policy: &Value) -> usize {
    providers
        .iter()
        .filter(|p| driver_allowed(policy, &p.driver) && agent_state(p, settings) == AgentState::Ready)
        .count()
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
