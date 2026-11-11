//! Provider drivers the client knows (`providerClients` in
//! `packages/client-runtime/src/providerClients.ts`), with each driver's settings form as
//! `deriveProviderSettingsFields` derives it from the settings schema annotations.
//! Generated from those schemas; regenerate when a driver's settings schema changes.

use serde_json::Value;

pub(super) struct Driver {
    pub kind: &'static str,
    pub label: &'static str,
    pub has_default_instance: bool,
    /// `featured: false` lists the driver under the Add provider wizard's "Show more".
    pub featured: bool,
    /// Registry entries for the same agent, left out of the ACP Registry list.
    pub acp_ids: &'static [&'static str],
    pub badge_label: Option<&'static str>,
    pub environment_fields: &'static [EnvironmentField],
    pub fields: &'static [Field],
}

pub(super) struct EnvironmentField {
    pub name: &'static str,
    pub label: &'static str,
    pub description: Option<&'static str>,
    pub placeholder: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Control {
    Text,
    Password,
    Select(&'static [(&'static str, &'static str)]),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Field {
    pub key: &'static str,
    pub control: Control,
    pub label: &'static str,
    pub description: Option<&'static str>,
    pub placeholder: Option<&'static str>,
    /// `clearWhenEmpty: "persist"` keeps an empty value instead of dropping the key.
    pub persist_empty: bool,
    /// Kept in the server's secret store; a saved value comes back redacted.
    pub secret: bool,
}

pub(super) const DRIVERS: &[Driver] = &[
    Driver {
        kind: "codex",
        label: "Codex",
        has_default_instance: true,
        featured: true,
        acp_ids: &["codex-acp"],
        badge_label: None,
        environment_fields: &[],
        fields: &[
            Field {
                key: "binaryPath",
                control: Control::Text,
                label: "Binary path",
                description: Some("Path to the Codex binary used by this instance."),
                placeholder: Some("codex"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "homePath",
                control: Control::Text,
                label: "CODEX_HOME path",
                description: Some("Custom Codex home and config directory."),
                placeholder: Some("~/.codex"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "shadowHomePath",
                control: Control::Text,
                label: "Shadow home path",
                description: Some(
                    "Account-specific Codex home. Keeps auth.json separate while sharing state from CODEX_HOME.",
                ),
                placeholder: Some("~/.codex-t3/personal"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "launchArgs",
                control: Control::Text,
                label: "Launch arguments",
                description: Some("Additional CLI arguments passed to codex app-server on session start."),
                placeholder: None,
                persist_empty: false,
                secret: false,
            },
        ],
    },
    Driver {
        kind: "claudeAgent",
        label: "Claude",
        has_default_instance: true,
        featured: true,
        acp_ids: &["claude-acp"],
        badge_label: None,
        environment_fields: &[],
        fields: &[
            Field {
                key: "binaryPath",
                control: Control::Text,
                label: "Binary path",
                description: Some("Path to the Claude binary used by this instance."),
                placeholder: Some("claude"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "homePath",
                control: Control::Text,
                label: "CLAUDE_CONFIG_DIR path",
                description: Some("Custom Claude home and config directory. Keeps .claude.json and .claude separate."),
                placeholder: Some("~/.claude"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "autoCompactWindow",
                control: Control::Text,
                label: "Auto-compact after",
                description: Some("Compact after 100,000 to 1,000,000 tokens. Leave empty to use Claude's default."),
                placeholder: Some("e.g. 300000"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "idleCompaction",
                control: Control::Select(&[
                    ("default", "Use default"),
                    ("onReturn", "When you return"),
                    ("beforeCacheExpires", "Before the cache expires (beta)"),
                    ("off", "Never"),
                ]),
                label: "Compact idle threads",
                description: Some(
                    "When to compact an idle thread over 200,000 tokens: after you return, just before its prompt cache expires, or never. Overrides the default in Providers > Advanced.",
                ),
                placeholder: None,
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "launchArgs",
                control: Control::Text,
                label: "Launch arguments",
                description: Some("Additional CLI arguments passed on session start."),
                placeholder: Some("e.g. --chrome"),
                persist_empty: false,
                secret: false,
            },
        ],
    },
    Driver {
        kind: "cursor",
        label: "Cursor",
        has_default_instance: true,
        featured: true,
        acp_ids: &["cursor"],
        badge_label: None,
        environment_fields: &[EnvironmentField {
            name: "CURSOR_API_KEY",
            label: "Cursor API key",
            description: Some("Optional. Overrides browser sign-in for this provider."),
            placeholder: Some("Paste API key"),
        }],
        fields: &[],
    },
    Driver {
        kind: "grok",
        label: "Grok",
        has_default_instance: true,
        featured: true,
        acp_ids: &["grok-build"],
        badge_label: None,
        environment_fields: &[],
        fields: &[Field {
            key: "binaryPath",
            control: Control::Text,
            label: "Binary path",
            description: Some("Path to the Grok CLI binary."),
            placeholder: Some("grok"),
            persist_empty: false,
            secret: false,
        }],
    },
    Driver {
        kind: "fx",
        label: "fx",
        has_default_instance: true,
        featured: false,
        acp_ids: &[],
        badge_label: None,
        environment_fields: &[],
        fields: &[Field {
            key: "binaryPath",
            control: Control::Text,
            label: "Binary path",
            description: Some("Path to the fx binary. All fx instances on this machine share its fx login."),
            placeholder: Some("fx"),
            persist_empty: false,
            secret: false,
        }],
    },
    Driver {
        kind: "opencode",
        label: "OpenCode",
        has_default_instance: true,
        featured: true,
        acp_ids: &["opencode", "opencode-v2"],
        badge_label: None,
        environment_fields: &[],
        fields: &[
            Field {
                key: "binaryPath",
                control: Control::Text,
                label: "Binary path",
                description: Some("Path to the OpenCode binary."),
                placeholder: Some("opencode"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "serverUrl",
                control: Control::Text,
                label: "Server URL",
                description: Some("Leave blank to let T3 Code spawn the server when needed."),
                placeholder: Some("http://127.0.0.1:4096"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "serverPassword",
                control: Control::Password,
                label: "Server password",
                description: Some("Stored separately on the host."),
                placeholder: Some("Optional"),
                persist_empty: false,
                secret: true,
            },
        ],
    },
    Driver {
        kind: "antigravity",
        label: "Antigravity",
        has_default_instance: true,
        featured: true,
        acp_ids: &["antigravity-acp"],
        badge_label: None,
        environment_fields: &[],
        fields: &[
            Field {
                key: "authMethod",
                control: Control::Select(&[
                    ("oauth-personal", "Google account"),
                    ("oauth-business", "Gemini Enterprise"),
                    ("gemini-api-key", "Gemini API key"),
                    ("agent-platform", "Agent Platform (Vertex AI)"),
                ]),
                label: "Sign-in method",
                description: Some("Google accounts use your subscription; API keys and Agent Platform bill usage."),
                placeholder: None,
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "apiKey",
                control: Control::Password,
                label: "API key",
                description: Some("Gemini or Vertex AI express key. Stored separately on the host."),
                placeholder: Some("Optional"),
                persist_empty: false,
                secret: true,
            },
            Field {
                key: "gcpProject",
                control: Control::Text,
                label: "GCP project",
                description: Some("Required for Gemini Enterprise. Agent Platform uses it when no API key is set."),
                placeholder: Some("my-project-id"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "gcpLocation",
                control: Control::Text,
                label: "GCP location",
                description: Some("Region for Gemini Enterprise or Agent Platform."),
                placeholder: Some("us-central1"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "binaryPath",
                control: Control::Text,
                label: "Binary path",
                description: Some("Custom ACP executable. Leave empty to select automatically."),
                placeholder: Some("Automatic"),
                persist_empty: true,
                secret: false,
            },
        ],
    },
    Driver {
        kind: "kiro",
        label: "Kiro",
        has_default_instance: true,
        featured: false,
        acp_ids: &[],
        badge_label: Some("Early Access"),
        environment_fields: &[EnvironmentField {
            name: "KIRO_API_KEY",
            label: "Kiro API key",
            description: Some("Optional. Signs in without `kiro-cli login` (Kiro Pro and above)."),
            placeholder: Some("Paste API key"),
        }],
        fields: &[Field {
            key: "binaryPath",
            control: Control::Text,
            label: "Binary path",
            description: Some("Path to the Kiro CLI binary."),
            placeholder: Some("kiro-cli"),
            persist_empty: false,
            secret: false,
        }],
    },
    Driver {
        kind: "muse",
        label: "Muse Code",
        has_default_instance: true,
        featured: true,
        acp_ids: &[],
        badge_label: None,
        environment_fields: &[],
        fields: &[Field {
            key: "binaryPath",
            control: Control::Text,
            label: "Binary path",
            description: Some(
                "Path to the Muse Code binary on this environment. Sign in with muse login on that host.",
            ),
            placeholder: Some("muse"),
            persist_empty: false,
            secret: false,
        }],
    },
    Driver {
        kind: "pi",
        label: "Pi",
        has_default_instance: true,
        featured: true,
        acp_ids: &["pi-acp"],
        badge_label: None,
        environment_fields: &[],
        fields: &[
            Field {
                key: "binaryPath",
                control: Control::Text,
                label: "Binary path",
                description: Some("Path to the Pi coding agent binary."),
                placeholder: Some("pi"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "launchArgs",
                control: Control::Text,
                label: "Launch arguments",
                description: Some("Additional CLI arguments passed to pi --mode rpc on session start."),
                placeholder: None,
                persist_empty: false,
                secret: false,
            },
        ],
    },
    Driver {
        kind: "acpRegistry",
        label: "ACP Registry",
        has_default_instance: false,
        featured: true,
        acp_ids: &[],
        badge_label: None,
        environment_fields: &[],
        fields: &[
            Field {
                key: "source",
                control: Control::Select(&[("registry", "ACP Registry"), ("local", "Local command")]),
                label: "ACP source",
                description: None,
                placeholder: None,
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "agentId",
                control: Control::Text,
                label: "Registry agent ID",
                description: Some("Agent identifier from the official ACP Registry, for example 'devin'."),
                placeholder: Some("devin"),
                persist_empty: true,
                secret: false,
            },
            Field {
                key: "commandPath",
                control: Control::Text,
                label: "Executable override",
                description: Some(
                    "Executable on this environment. For registry agents, this overrides the distribution executable while keeping its arguments and environment.",
                ),
                placeholder: Some("Registry default"),
                persist_empty: false,
                secret: false,
            },
            Field {
                key: "authMethodId",
                control: Control::Text,
                label: "Authentication method",
                description: Some(
                    "Optional ACP authentication method ID. By default, the first agent-managed method is selected.",
                ),
                placeholder: Some("auto"),
                persist_empty: false,
                secret: false,
            },
        ],
    },
];

pub(super) fn driver(kind: &str) -> Option<&'static Driver> {
    DRIVERS.iter().find(|driver| driver.kind == kind)
}

pub(super) fn is_local_acp(driver: &str, config: &Value) -> bool {
    driver == "acpRegistry" && config.get("source").and_then(Value::as_str) == Some("local")
}

/// `deriveProviderSettingsFields`: a local ACP command shows only its source and executable.
pub(super) fn visible_fields(driver: &Driver, config: &Value) -> Vec<Field> {
    if !is_local_acp(driver.kind, config) {
        return driver.fields.to_vec();
    }
    driver
        .fields
        .iter()
        .filter(|field| field.key == "source" || field.key == "commandPath")
        .map(|field| match field.key {
            "commandPath" => Field {
                label: "Executable",
                description: Some("Executable name or path on this environment."),
                placeholder: Some("e.g. dsh"),
                ..*field
            },
            _ => *field,
        })
        .collect()
}

/// `nextProviderConfigWithFieldValue` for string values: an empty value removes the key unless
/// the field persists empties; an empty config is `None`.
pub(super) fn next_config(config: &Value, field: &Field, value: &str) -> Option<Value> {
    let mut base = config.as_object().cloned().unwrap_or_default();
    if !field.persist_empty && value.trim().is_empty() {
        base.remove(field.key);
    } else {
        base.insert(field.key.into(), value.into());
    }
    (!base.is_empty()).then_some(Value::Object(base))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn empty_values_clear_keys_unless_the_field_persists_them() {
        let binary = driver("codex").unwrap().fields[0];
        assert_eq!(next_config(&Value::Null, &binary, "/bin/codex"), Some(json!({"binaryPath": "/bin/codex"})));
        assert_eq!(next_config(&json!({"binaryPath": "x"}), &binary, "  "), None);
        let agent_id = driver("acpRegistry").unwrap().fields[1];
        assert_eq!(next_config(&Value::Null, &agent_id, ""), Some(json!({"agentId": ""})));
    }

    #[test]
    fn local_acp_shows_only_its_source_and_executable() {
        let acp = driver("acpRegistry").unwrap();
        let local = visible_fields(acp, &json!({"source": "local"}));
        assert_eq!(local.iter().map(|f| f.key).collect::<Vec<_>>(), ["source", "commandPath"]);
        assert_eq!(local[1].label, "Executable");
        assert_eq!(visible_fields(acp, &Value::Null).len(), 4);
    }
}
