//! The Add provider dialog (`AddProviderInstanceDialog.tsx`, its `.logic.ts`,
//! `AddProviderInstanceWizardSteps.tsx`, `ProviderWizardAuthenticationStep.tsx`,
//! `MultipleAccountsNote.tsx`) and the driver connection form (`ProviderSettingsForm.tsx`),
//! which the instance card's Runtime section reuses through [`Workspace::render_provider_runtime_form`].
//!
//! The web derives the form from each driver's Effect schema annotations; [`DRIVERS`] is that
//! derivation captured as data (labels, order and options copied from the schemas).

use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    Colorize as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Div, ElementId, Entity, EventEmitter, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, WeakEntity, Window, div, px, svg,
};
use serde_json::{Map, Value, json};

use super::acp::{AcpRegistrySearch, AcpSearchEvent, RegistryAgent, rpc_message};
use crate::app::Workspace;
use crate::rpc::StreamEvent;
use crate::theme::{FONT_MONO, Tokens, tokens};

pub(super) const ACP_REGISTRY: &str = "acpRegistry";

// MARK: Driver catalog (`providerClients` + each driver's settings schema)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Control {
    Text,
    Password,
    Select(&'static [(&'static str, &'static str)]),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct FieldSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub placeholder: &'static str,
    pub control: Control,
    /// `clearWhenEmpty: "persist"`: an empty value is saved instead of removing the key.
    pub persist_empty: bool,
    /// Kept in the server's secret store; a saved value never comes back.
    pub secret: bool,
}

const fn text(
    key: &'static str,
    label: &'static str,
    description: &'static str,
    placeholder: &'static str,
) -> FieldSpec {
    FieldSpec { key, label, description, placeholder, control: Control::Text, persist_empty: false, secret: false }
}

const fn secret(key: &'static str, label: &'static str, description: &'static str) -> FieldSpec {
    FieldSpec { control: Control::Password, secret: true, ..text(key, label, description, "Optional") }
}

const fn select(
    key: &'static str,
    label: &'static str,
    description: &'static str,
    options: &'static [(&'static str, &'static str)],
) -> FieldSpec {
    FieldSpec { control: Control::Select(options), ..text(key, label, description, "") }
}

pub(super) struct DriverSpec {
    pub kind: &'static str,
    pub label: &'static str,
    /// `featured: false` lists the driver under "Show more".
    pub featured: bool,
    pub badge: Option<&'static str>,
    /// Registry entries for the same agent, left out of the Registry list.
    pub acp_ids: &'static [&'static str],
    pub fields: &'static [FieldSpec],
}

const BINARY: &str = "Binary path";

pub(super) const DRIVERS: &[DriverSpec] = &[
    DriverSpec {
        kind: "codex",
        label: "Codex",
        featured: true,
        badge: None,
        acp_ids: &["codex-acp"],
        fields: &[
            text("binaryPath", BINARY, "Path to the Codex binary used by this instance.", "codex"),
            text("homePath", "CODEX_HOME path", "Custom Codex home and config directory.", "~/.codex"),
            text(
                "shadowHomePath",
                "Shadow home path",
                "Account-specific Codex home. Keeps auth.json separate while sharing state from CODEX_HOME.",
                "~/.codex-t3/personal",
            ),
            text(
                "launchArgs",
                "Launch arguments",
                "Additional CLI arguments passed to codex app-server on session start.",
                "",
            ),
        ],
    },
    DriverSpec {
        kind: "claudeAgent",
        label: "Claude",
        featured: true,
        badge: None,
        acp_ids: &["claude-acp"],
        fields: &[
            text("binaryPath", BINARY, "Path to the Claude binary used by this instance.", "claude"),
            text(
                "homePath",
                "CLAUDE_CONFIG_DIR path",
                "Custom Claude home and config directory. Keeps .claude.json and .claude separate.",
                "~/.claude",
            ),
            text(
                "autoCompactWindow",
                "Auto-compact after",
                "Compact after 100,000 to 1,000,000 tokens. Leave empty to use Claude's default.",
                "e.g. 300000",
            ),
            select(
                "idleCompaction",
                "Compact idle threads",
                "When to compact an idle thread over 200,000 tokens: after you return, just before its prompt cache expires, or never. Overrides the default in Providers > Advanced.",
                &[
                    ("default", "Use default"),
                    ("onReturn", "When you return"),
                    ("beforeCacheExpires", "Before the cache expires (beta)"),
                    ("off", "Never"),
                ],
            ),
            text(
                "launchArgs",
                "Launch arguments",
                "Additional CLI arguments passed on session start.",
                "e.g. --chrome",
            ),
        ],
    },
    DriverSpec { kind: "cursor", label: "Cursor", featured: true, badge: None, acp_ids: &["cursor"], fields: &[] },
    DriverSpec {
        kind: "grok",
        label: "Grok",
        featured: true,
        badge: None,
        acp_ids: &["grok-build"],
        fields: &[text("binaryPath", BINARY, "Path to the Grok CLI binary.", "grok")],
    },
    DriverSpec {
        kind: "fx",
        label: "fx",
        featured: false,
        badge: None,
        acp_ids: &[],
        fields: &[text(
            "binaryPath",
            BINARY,
            "Path to the fx binary. All fx instances on this machine share its fx login.",
            "fx",
        )],
    },
    DriverSpec {
        kind: "opencode",
        label: "OpenCode",
        featured: true,
        badge: None,
        acp_ids: &["opencode", "opencode-v2"],
        fields: &[
            text("binaryPath", BINARY, "Path to the OpenCode binary.", "opencode"),
            text(
                "serverUrl",
                "Server URL",
                "Leave blank to let T3 Code spawn the server when needed.",
                "http://127.0.0.1:4096",
            ),
            secret("serverPassword", "Server password", "Stored separately on the host."),
        ],
    },
    DriverSpec {
        kind: "antigravity",
        label: "Antigravity",
        featured: true,
        badge: None,
        acp_ids: &["antigravity-acp"],
        fields: &[
            select(
                "authMethod",
                "Sign-in method",
                "Google accounts use your subscription; API keys and Agent Platform bill usage.",
                &[
                    ("oauth-personal", "Google account"),
                    ("oauth-business", "Gemini Enterprise"),
                    ("gemini-api-key", "Gemini API key"),
                    ("agent-platform", "Agent Platform (Vertex AI)"),
                ],
            ),
            secret("apiKey", "API key", "Gemini or Vertex AI express key. Stored separately on the host."),
            text(
                "gcpProject",
                "GCP project",
                "Required for Gemini Enterprise. Agent Platform uses it when no API key is set.",
                "my-project-id",
            ),
            text("gcpLocation", "GCP location", "Region for Gemini Enterprise or Agent Platform.", "us-central1"),
            FieldSpec {
                persist_empty: true,
                ..text("binaryPath", BINARY, "Custom ACP executable. Leave empty to select automatically.", "Automatic")
            },
        ],
    },
    DriverSpec {
        kind: "kiro",
        label: "Kiro",
        featured: false,
        badge: Some("Early Access"),
        acp_ids: &[],
        fields: &[text("binaryPath", BINARY, "Path to the Kiro CLI binary.", "kiro-cli")],
    },
    DriverSpec {
        kind: "muse",
        label: "Muse Code",
        featured: true,
        badge: None,
        acp_ids: &[],
        fields: &[text(
            "binaryPath",
            BINARY,
            "Path to the Muse Code binary on this environment. Sign in with muse login on that host.",
            "muse",
        )],
    },
    DriverSpec {
        kind: "pi",
        label: "Pi",
        featured: true,
        badge: None,
        acp_ids: &["pi-acp"],
        fields: &[
            text("binaryPath", BINARY, "Path to the Pi coding agent binary.", "pi"),
            text(
                "launchArgs",
                "Launch arguments",
                "Additional CLI arguments passed to pi --mode rpc on session start.",
                "",
            ),
        ],
    },
    DriverSpec {
        kind: ACP_REGISTRY,
        label: "ACP Registry",
        featured: true,
        badge: None,
        acp_ids: &[],
        fields: &[
            select("source", "ACP source", "", &[("registry", "ACP Registry"), ("local", "Local command")]),
            FieldSpec {
                persist_empty: true,
                ..text(
                    "agentId",
                    "Registry agent ID",
                    "Agent identifier from the official ACP Registry, for example 'devin'.",
                    "devin",
                )
            },
            text(
                "commandPath",
                "Executable override",
                "Executable on this environment. For registry agents, this overrides the distribution executable while keeping its arguments and environment.",
                "Registry default",
            ),
            text(
                "authMethodId",
                "Authentication method",
                "Optional ACP authentication method ID. By default, the first agent-managed method is selected.",
                "auto",
            ),
        ],
    },
];

pub(super) fn driver_spec(kind: &str) -> Option<&'static DriverSpec> {
    DRIVERS.iter().find(|driver| driver.kind == kind)
}

// MARK: Pure logic

fn config_str<'a>(config: &'a Value, key: &str) -> &'a str {
    config.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn is_local_acp(driver: &str, config: &Value) -> bool {
    driver == ACP_REGISTRY && config_str(config, "source") == "local"
}

/// `deriveProviderSettingsFields`: a local ACP command shows only its source and executable.
pub(super) fn visible_fields(driver: &DriverSpec, config: &Value) -> Vec<FieldSpec> {
    if !is_local_acp(driver.kind, config) {
        return driver.fields.to_vec();
    }
    driver
        .fields
        .iter()
        .filter(|field| field.key == "source" || field.key == "commandPath")
        .map(|field| match field.key {
            "commandPath" => FieldSpec {
                label: "Executable",
                description: "Executable name or path on this environment.",
                placeholder: "e.g. dsh",
                ..*field
            },
            _ => *field,
        })
        .collect()
}

/// `nextProviderConfigWithFieldValue` for string values: an empty value removes the key unless
/// the field persists empties; an empty config is `None`.
pub(super) fn next_config(config: &Value, field: &FieldSpec, value: &str) -> Option<Value> {
    let mut base = config.as_object().cloned().unwrap_or_default();
    if !field.persist_empty && value.trim().is_empty() {
        base.remove(field.key);
    } else {
        base.insert(field.key.into(), value.into());
    }
    (!base.is_empty()).then_some(Value::Object(base))
}

fn with_command_args(config: &Value, args: Vec<String>) -> Value {
    let mut base = config.as_object().cloned().unwrap_or_default();
    base.insert("commandArgs".into(), args.into());
    Value::Object(base)
}

fn command_args(config: &Value) -> Vec<String> {
    config
        .get("commandArgs")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default()
}

fn validate_instance_id(id: &str, existing: &HashSet<String>) -> Option<String> {
    let mut chars = id.chars();
    Some(if id.is_empty() {
        "Instance ID is required.".into()
    } else if id.chars().count() > 64 {
        "Instance ID must be 64 characters or fewer.".into()
    } else if !chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        "Instance ID must start with a letter and use only letters, digits, '-', or '_'.".into()
    } else if existing.contains(id) {
        format!("An instance named '{id}' already exists.")
    } else {
        return None;
    })
}

/// `slugifyLabel` in `packages/client-runtime/src/providerInstanceIds.ts`, capped at 48 chars.
fn slugify_label(label: &str) -> String {
    let mut slug = String::new();
    for c in label.trim().to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    let slug = slug.trim_matches('_');
    slug[..slug.len().min(48)].to_owned()
}

pub(super) fn derive_instance_id(driver: &str, label: &str) -> String {
    let slug = slugify_label(label);
    if slug.is_empty() { String::new() } else { format!("{driver}_{slug}") }
}

pub(super) fn derive_available_instance_id(
    derive: impl Fn(&str) -> String,
    label: &str,
    existing: &HashSet<String>,
) -> String {
    let base = derive(label);
    if base.is_empty() || !existing.contains(&base) {
        return base;
    }
    (2..)
        .map(|suffix| {
            let suffix = format!("_{suffix}");
            format!("{}{suffix}", &base[..base.len().min(64 - suffix.len())])
        })
        .find(|candidate| !existing.contains(candidate))
        .expect("an unused suffix")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Navigation {
    Navigate(usize),
    Blocked(usize),
}

/// `resolveWizardNavigation`: moving past a prerequisite step or the identity step stops there
/// while that step has an error.
pub(super) fn resolve_navigation(
    current: usize,
    requested: usize,
    step_count: usize,
    identity_step: usize,
    instance_id_error: bool,
    prerequisite: Option<(usize, bool)>,
) -> Navigation {
    let last = step_count.saturating_sub(1);
    let target = requested.min(last);
    if let Some((step, true)) = prerequisite {
        if current <= step && target > step {
            return Navigation::Blocked(step.min(last));
        }
    }
    if instance_id_error && current <= identity_step && target > identity_step {
        return Navigation::Blocked(identity_step.min(last));
    }
    Navigation::Navigate(target)
}

/// Whether a registry (not local) ACP instance already runs `agent_id`.
pub(super) fn is_configured_acp_agent(instances: &Value, agent_id: &str) -> bool {
    instances.as_object().is_some_and(|instances| {
        instances.values().any(|instance| {
            instance.get("driver").and_then(Value::as_str) == Some(ACP_REGISTRY)
                && instance.get("config").is_some_and(|config| {
                    config_str(config, "source") != "local" && config_str(config, "agentId") == agent_id
                })
        })
    })
}

/// `normalizeProviderAccentColor`: a `#rrggbb` color or nothing.
fn normalize_accent(value: &str) -> Option<String> {
    let value = value.trim();
    (value.len() == 7 && value.starts_with('#') && value[1..].chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| value.to_owned())
}

/// `isManagedDriverAllowed`.
fn driver_allowed(policy: &Value, driver: &str) -> bool {
    match policy.pointer("/limits/providers/allowedDrivers").and_then(Value::as_array) {
        Some(allowed) => allowed.iter().any(|d| d.as_str() == Some(driver)),
        None => true,
    }
}

/// `managedInstanceLocks(...).configKeys` and its reason, when the policy pins any of `driver`'s config.
fn config_locks(policy: &Value, driver: &str) -> Option<(HashSet<String>, SharedString)> {
    let paths = policy.pointer(&format!("/limits/providers/lockedInstanceFields/{driver}"))?.as_array()?;
    if paths.is_empty() {
        return None;
    }
    let keys = paths.iter().filter_map(|p| p.as_str()?.strip_prefix("config.")).map(str::to_owned).collect();
    let organization = policy.get("organization").and_then(Value::as_str).unwrap_or_default();
    Some((keys, format!("Managed by {organization}.").into()))
}

// MARK: Shared row layout (`SettingsRow` in settingsLayout.tsx)

pub(super) fn settings_row(
    id: impl Into<ElementId>,
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    status: Option<AnyElement>,
    control: Option<AnyElement>,
    t: &Tokens,
) -> gpui_kit::Stateful<Div> {
    div().id(id).px(px(16.)).py(px(12.)).child(
        h_flex()
            .gap(px(32.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(div().min_h(px(20.)).text_size(px(14.)).font_weight(FontWeight::MEDIUM).child(title.into()))
                    .children(description.map(|d| {
                        div()
                            .max_w(px(672.))
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(t.muted_foreground)
                            .child(d)
                    }))
                    .children(
                        status.map(|s| div().pt(px(2.)).text_size(px(12.)).text_color(t.muted_foreground).child(s)),
                    ),
            )
            .children(control.map(|c| h_flex().flex_none().gap(px(8.)).justify_end().child(c))),
    )
}

fn notify_error(window: &mut Window, cx: &mut App, title: &str, message: String) {
    window.push_notification(Notification::error(message).title(title.to_owned()), cx);
}

fn input_value(input: &Entity<InputState>, cx: &App) -> String {
    input.read(cx).value().to_string()
}

// MARK: Driver connection form (`ProviderSettingsForm`, variant "settings")

pub(super) struct ConfigChanged(pub Option<Value>);

pub(super) struct ConfigForm {
    driver: &'static DriverSpec,
    config: Value,
    /// The last config set from outside; an unchanged one (e.g. our own save in flight) is ignored.
    seen: Value,
    id_prefix: SharedString,
    inputs: HashMap<&'static str, Entity<InputState>>,
    /// Local ACP `commandArgs`, one row per argument, keyed by a row id for stable element ids.
    args: Vec<(u64, Entity<InputState>)>,
    next_arg: u64,
    locks: Option<(HashSet<String>, SharedString)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ConfigChanged> for ConfigForm {}

impl ConfigForm {
    pub(super) fn new(
        driver: &'static DriverSpec,
        config: Value,
        id_prefix: impl Into<SharedString>,
        locks: Option<(HashSet<String>, SharedString)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut form = Self {
            driver,
            config: Value::Null,
            seen: Value::Null,
            id_prefix: id_prefix.into(),
            inputs: HashMap::new(),
            args: Vec::new(),
            next_arg: 0,
            locks,
            _subscriptions: Vec::new(),
        };
        for field in driver.fields.iter().filter(|f| !matches!(f.control, Control::Select(_))) {
            let key = field.key;
            let input = cx.new(|cx| InputState::new(window, cx).masked(field.control == Control::Password));
            form._subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.commit_field(key, window, cx);
                    }
                },
            ));
            form.inputs.insert(key, input);
        }
        form.seen = config.clone();
        form.apply_config(config, window, cx);
        form
    }

    /// Takes a config from outside (the saved instance) when it changed there.
    pub(super) fn set_config(&mut self, config: Value, window: &mut Window, cx: &mut Context<Self>) {
        if config == self.seen {
            return;
        }
        self.seen = config.clone();
        self.apply_config(config, window, cx);
    }

    /// Shows `config`, leaving a field being edited alone.
    fn apply_config(&mut self, config: Value, window: &mut Window, cx: &mut Context<Self>) {
        for field in visible_fields(self.driver, &config) {
            let Some(input) = self.inputs.get(field.key) else { continue };
            let focused = input.read(cx).focus_handle(cx).is_focused(window);
            let saved = config_str(&config, field.key).to_owned();
            input.update(cx, |input, cx| {
                if field.secret {
                    let placeholder =
                        if saved.is_empty() { "Not set" } else { "Stored secret, enter a new value to replace" };
                    input.set_placeholder(placeholder, window, cx);
                } else {
                    input.set_placeholder(field.placeholder, window, cx);
                    if !focused && input.value().as_ref() != saved {
                        input.set_value(saved, window, cx);
                    }
                }
            });
        }
        let args = command_args(&config);
        let current: Vec<String> = self.args.iter().map(|(_, input)| input_value(input, cx)).collect();
        if current != args {
            self.args.clear();
            for arg in args {
                self.push_arg(arg, window, cx);
            }
        }
        self.config = config;
        cx.notify();
    }

    fn push_arg(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_arg;
        self.next_arg += 1;
        let input = cx.new(|cx| InputState::new(window, cx).default_value(value));
        self._subscriptions.push(cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                this.publish_args(window, cx);
            }
        }));
        self.args.push((id, input));
    }

    fn publish(&mut self, next: Option<Value>, window: &mut Window, cx: &mut Context<Self>) {
        let next_value = next.clone().unwrap_or(Value::Null);
        if next_value == self.config {
            return;
        }
        // Fields and placeholders follow the config (a local ACP source shows other fields).
        self.apply_config(next_value, window, cx);
        cx.emit(ConfigChanged(next));
    }

    fn publish_args(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let args = self.args.iter().map(|(_, input)| input_value(input, cx)).collect();
        let next = with_command_args(&self.config, args);
        self.publish(Some(next), window, cx);
    }

    fn commit_field(&mut self, key: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(field), Some(input)) = (
            visible_fields(self.driver, &self.config).into_iter().find(|f| f.key == key),
            self.inputs.get(key).cloned(),
        ) else {
            return;
        };
        let value = input_value(&input, cx);
        if field.secret {
            // `SecretInput`: an empty draft keeps the stored secret.
            if value.is_empty() {
                return;
            }
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        let next = next_config(&self.config, &field, &value);
        self.publish(next, window, cx);
    }

    fn select_option(&mut self, field: FieldSpec, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Control::Select(options) = field.control else { return };
        let fallback = options.first().map(|(v, _)| *v).unwrap_or_default();
        let next = next_config(&self.config, &field, if value == fallback { "" } else { value });
        self.publish(next, window, cx);
    }

    fn clear_secret(&mut self, field: FieldSpec, window: &mut Window, cx: &mut Context<Self>) {
        let next = next_config(&self.config, &field, "");
        self.publish(next, window, cx);
    }

    fn render_control(&self, field: FieldSpec, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let locked = self.locks.as_ref().filter(|(keys, _)| keys.contains(field.key)).map(|(_, reason)| reason.clone());
        let id = |part: &str| ElementId::Name(format!("{}-{}{part}", self.id_prefix, field.key).into());
        let control = match field.control {
            Control::Select(options) => {
                let fallback = options.first().map(|(v, _)| *v).unwrap_or_default();
                let current = Some(config_str(&self.config, field.key)).filter(|v| !v.is_empty()).unwrap_or(fallback);
                let label = options.iter().find(|(v, _)| *v == current).map(|(_, l)| *l).unwrap_or(current).to_owned();
                let current = current.to_owned();
                let view = cx.entity();
                Button::new(id("-select"))
                    .outline()
                    .small()
                    .w(px(224.))
                    .disabled(locked.is_some())
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .child(label)
                            .child(Icon::new(Lucide::ChevronDown).size(px(14.)).text_color(t.muted_foreground)),
                    )
                    .dropdown_menu(move |mut menu, window, _| {
                        for (value, label) in options {
                            menu = menu.item(PopupMenuItem::new(*label).checked(*value == current.as_str()).on_click(
                                window.listener_for(&view, move |this, _, window, cx| {
                                    this.select_option(field, value, window, cx)
                                }),
                            ));
                        }
                        menu
                    })
                    .into_any_element()
            }
            _ => {
                let input = self.inputs[field.key].clone();
                let configured = field.secret && !config_str(&self.config, field.key).is_empty();
                h_flex()
                    .w(px(224.))
                    .gap(px(8.))
                    .child(Input::new(&input).small().flex_1().disabled(locked.is_some()))
                    .when(configured && locked.is_none(), |row| {
                        row.child(
                            Button::new(id("-clear"))
                                .ghost()
                                .small()
                                .icon(IconName::Close)
                                .tooltip(format!("Clear {}", field.label))
                                .on_click(cx.listener(move |this, _, window, cx| this.clear_secret(field, window, cx))),
                        )
                    })
                    .into_any_element()
            }
        };
        match locked {
            Some(reason) => div()
                .id(id("-managed"))
                .opacity(0.5)
                .child(control)
                .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx))
                .into_any_element(),
            None => control,
        }
    }

    fn render_args(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let add = Button::new(SharedString::from(format!("{}-add-argument", self.id_prefix)))
            .outline()
            .small()
            .icon(IconName::Plus)
            .label("Add argument")
            .on_click(cx.listener(|this, _, window, cx| {
                this.push_arg(String::new(), window, cx);
                this.publish_args(window, cx);
            }));
        v_flex()
            .child(settings_row(
                SharedString::from(format!("{}-arguments", self.id_prefix)),
                "Arguments",
                Some("One literal argument per row, in launch order.".into()),
                None,
                Some(add.into_any_element()),
                t,
            ))
            .when(!self.args.is_empty(), |list| {
                list.child(v_flex().px(px(16.)).pb(px(8.)).gap(px(8.)).children(self.args.iter().map(
                    |(row, input)| {
                        let row = *row;
                        h_flex().gap(px(6.)).child(Input::new(input).small().flex_1().font_family(FONT_MONO)).child(
                            Button::new(SharedString::from(format!("{}-remove-argument-{row}", self.id_prefix)))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.args.retain(|(id, _)| *id != row);
                                    this.publish_args(window, cx);
                                })),
                        )
                    },
                )))
            })
            .into_any_element()
    }
}

impl Render for ConfigForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let fields = visible_fields(self.driver, &self.config);
        let local = is_local_acp(self.driver.kind, &self.config);
        v_flex()
            .gap(px(4.))
            .children(fields.into_iter().map(|field| {
                let control = self.render_control(field, &t, cx);
                settings_row(
                    SharedString::from(format!("{}-{}", self.id_prefix, field.key)),
                    field.label,
                    (!field.description.is_empty()).then(|| field.description.into()),
                    None,
                    Some(control),
                    &t,
                )
            }))
            .when(local, |form| form.child(self.render_args(&t, cx)))
    }
}

// MARK: The instance card's Runtime fields

impl Workspace {
    /// `ProviderSettingsForm` for a saved instance: each committed change upserts the instance.
    /// `None` when the instance or its driver is unknown (the card then shows its "Driver" note).
    pub(in crate::app) fn render_provider_runtime_form(
        &mut self,
        instance_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let instance = self.server_info.provider_instances.get(instance_id)?.clone();
        let driver = driver_spec(instance.get("driver")?.as_str()?)?;
        if driver.fields.is_empty() {
            return None;
        }
        let config = instance.get("config").cloned().unwrap_or_default();
        let locks = config_locks(&self.server_info.managed_policy, driver.kind);
        let workspace = cx.weak_entity();
        let owned_id = instance_id.to_owned();
        let form = window.use_keyed_state(
            SharedString::from(format!("provider-runtime-{instance_id}-{}", driver.kind)),
            cx,
            |window, cx| {
                let form =
                    ConfigForm::new(driver, config.clone(), format!("provider-instance-{owned_id}"), locks, window, cx);
                cx.subscribe(&cx.entity(), move |_, _, ConfigChanged(next), cx| {
                    let next = next.clone();
                    let id = owned_id.clone();
                    let _ = workspace.update(cx, |workspace, cx| workspace.update_instance_config(id, next, cx));
                })
                .detach();
                form
            },
        );
        form.update(cx, |form, cx| form.set_config(config, window, cx));
        Some(form.into_any_element())
    }

    /// `updateConfig` in `ProviderInstanceCard`: replaces the instance's `config` and upserts it.
    fn update_instance_config(&mut self, instance_id: String, config: Option<Value>, cx: &mut Context<Self>) {
        let Some(mut instance) = self.server_info.provider_instances.get(&instance_id).cloned() else { return };
        let Some(fields) = instance.as_object_mut() else { return };
        match config {
            Some(config) => fields.insert("config".into(), config),
            None => fields.remove("config"),
        };
        self.mutate_provider_instance(
            json!({"operation": "upsert", "instanceId": instance_id, "instance": instance}),
            cx,
        );
    }

    /// `usePersistEnvironmentProviderInstanceMutation`: `server.updateSettings` with an empty patch
    /// and a `ProviderInstanceMutation` (`create`, `upsert` or `remove`).
    pub(in crate::app) fn mutate_provider_instance(&mut self, mutation: Value, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        cx.spawn(async move |this, cx| {
            let payload = json!({"patch": {}, "providerInstanceMutation": mutation});
            if let Err(error) = client.request("server.updateSettings", payload).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(format!("Could not save provider instance: {}", rpc_message(&error)).into());
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

// MARK: Seams owned by the other Providers agents (page shell, instance card)

impl Workspace {
    /// `onCreated`: the page selects the new instance. The page shell owns that selection.
    fn provider_instance_created(&mut self, _instance_id: &str, _cx: &mut Context<Self>) {}

    /// `ProviderAuthenticationSection` for `instance_id`, owned by the instance card.
    fn render_provider_auth_section(
        &mut self,
        _instance_id: &str,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        None
    }

    /// Whether `AddManagedCodexAccountDialog` ("Continue with ChatGPT", owned by the Codex setup
    /// code) is ported; the button stays hidden until it is.
    fn has_codex_account_dialog(&self) -> bool {
        false
    }

    fn open_add_codex_account_dialog(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}
}

// MARK: The wizard

const PROVIDER_STEPS: &[&str] = &["Provider", "Identity", "Config"];
const ACP_STEPS: &[&str] = &["Provider", "Identity", "Sign in"];
const LOCAL_ACP_STEPS: &[&str] = &["Provider", "Identity"];
const IDENTITY_STEP: usize = 1;
const MULTIPLE_ACCOUNTS_FAQ_URL: &str =
    "https://github.com/pingdotgg/t3code/blob/main/docs/user/faq.md#can-i-add-a-bunch-of-accounts-for-one-provider";

#[derive(Clone, Default)]
struct Identity {
    label: String,
    accent: String,
    instance_id_override: Option<String>,
}

pub(super) struct AddProviderWizard {
    workspace: WeakEntity<Workspace>,
    step: usize,
    driver: &'static DriverSpec,
    identity: HashMap<&'static str, Identity>,
    label_input: Entity<InputState>,
    id_input: Entity<InputState>,
    accent: Entity<ColorPickerState>,
    /// `configByDriver`: one draft form per driver.
    forms: HashMap<&'static str, Entity<ConfigForm>>,
    configs: HashMap<&'static str, Value>,
    selected_acp: Option<RegistryAgent>,
    manual_acp: bool,
    show_more: bool,
    attempted: bool,
    saving: bool,
    created: Option<String>,
    /// Latest `provider.auth.subscribe` state of the created instance and its stream error.
    auth: Option<Value>,
    auth_error: Option<SharedString>,
    registry: Entity<AcpRegistrySearch>,
    _auth_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl AddProviderWizard {
    fn new(workspace: Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let policy = workspace.read(cx).server_info.managed_policy.clone();
        let first = DRIVERS
            .iter()
            .filter(|d| d.kind != ACP_REGISTRY && driver_allowed(&policy, d.kind))
            .min_by_key(|d| !d.featured)
            .or_else(|| driver_allowed(&policy, ACP_REGISTRY).then(|| driver_spec(ACP_REGISTRY)).flatten())
            .unwrap_or(&DRIVERS[0]);
        let label_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Work"));
        let id_input = cx.new(|cx| InputState::new(window, cx));
        let accent = cx.new(|cx| ColorPickerState::new(window, cx));
        let registry = cx.new(|cx| AcpRegistrySearch::new(workspace.downgrade(), window, cx));
        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.subscribe_in(&label_input, window, |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    let label = input_value(input, cx);
                    this.identity_mut().label = label;
                    this.sync_identity_inputs(false, window, cx);
                }
            }),
            cx.subscribe(&id_input, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.identity_mut().instance_id_override = Some(input_value(&input, cx));
                    cx.notify();
                }
            }),
            cx.subscribe(&accent, |this, _, ColorPickerEvent::Change(color), cx| {
                this.identity_mut().accent = color.map(|c| c.to_hex()).unwrap_or_default();
                cx.notify();
            }),
            cx.subscribe_in(&registry, window, |this, _, event: &AcpSearchEvent, window, cx| match event {
                AcpSearchEvent::Prepared(agent) => this.acp_prepared(agent.clone(), window, cx),
                AcpSearchEvent::Manual => this.manual_acp_configuration(window, cx),
                AcpSearchEvent::Local => this.local_acp_configuration(window, cx),
            }),
        ];
        let mut wizard = Self {
            workspace: workspace.downgrade(),
            step: 0,
            driver: first,
            identity: HashMap::new(),
            label_input,
            id_input,
            accent,
            forms: HashMap::new(),
            configs: HashMap::new(),
            selected_acp: None,
            manual_acp: false,
            show_more: false,
            attempted: false,
            saving: false,
            created: None,
            auth: None,
            auth_error: None,
            registry,
            _auth_task: None,
            _subscriptions: subscriptions,
        };
        wizard.sync_identity_inputs(true, window, cx);
        wizard
    }

    fn is_acp(&self) -> bool {
        self.driver.kind == ACP_REGISTRY
    }

    fn config(&self) -> &Value {
        self.configs.get(self.driver.kind).unwrap_or(&Value::Null)
    }

    fn is_local_acp(&self) -> bool {
        self.is_acp() && self.manual_acp && is_local_acp(ACP_REGISTRY, self.config())
    }

    fn default_identity(&self) -> Identity {
        Identity { label: self.driver.label.to_owned(), ..Default::default() }
    }

    fn identity(&self) -> Identity {
        self.identity.get(self.driver.kind).cloned().unwrap_or_else(|| self.default_identity())
    }

    fn identity_mut(&mut self) -> &mut Identity {
        let default = self.default_identity();
        self.identity.entry(self.driver.kind).or_insert(default)
    }

    fn set_identity(&mut self, driver: &'static str, label: &str, instance_id_override: Option<String>) {
        let entry = self.identity.entry(driver).or_default();
        entry.label = label.to_owned();
        entry.instance_id_override = instance_id_override;
    }

    /// Codex and Claude default ids are reserved; they run before being configured.
    fn existing_ids(&self, cx: &App) -> HashSet<String> {
        let mut ids: HashSet<String> = ["codex".into(), "claudeAgent".into()].into();
        if let Some(workspace) = self.workspace.upgrade() {
            if let Some(instances) = workspace.read(cx).server_info.provider_instances.as_object() {
                ids.extend(instances.keys().cloned());
            }
        }
        ids
    }

    fn instance_id(&self, cx: &App) -> String {
        let identity = self.identity();
        if let Some(id) = identity.instance_id_override {
            return id;
        }
        let driver = self.driver;
        let acp = self.is_acp();
        derive_available_instance_id(
            |label| {
                if label.trim().is_empty() || label == driver.label {
                    if acp { format!("{}_custom", driver.kind) } else { driver.kind.to_owned() }
                } else {
                    derive_instance_id(driver.kind, label)
                }
            },
            &identity.label,
            &self.existing_ids(cx),
        )
    }

    fn instance_id_error(&self, cx: &App) -> Option<String> {
        validate_instance_id(&self.instance_id(cx), &self.existing_ids(cx))
    }

    fn acp_selection_error(&self) -> Option<&'static str> {
        if self.is_local_acp() {
            config_str(self.config(), "commandPath").trim().is_empty().then_some("Executable is required.")
        } else if self.selected_acp.is_some()
            || (self.manual_acp && !config_str(self.config(), "agentId").trim().is_empty())
        {
            None
        } else {
            Some("Select an ACP or configure one manually.")
        }
    }

    fn steps(&self) -> &'static [&'static str] {
        if !self.is_acp() {
            PROVIDER_STEPS
        } else if self.is_local_acp() {
            LOCAL_ACP_STEPS
        } else {
            ACP_STEPS
        }
    }

    fn busy(&self, cx: &App) -> bool {
        self.saving || self.registry.read(cx).is_preparing()
    }

    /// Puts the current driver's identity into the label and id inputs. `label` also resets the label text.
    fn sync_identity_inputs(&mut self, label: bool, window: &mut Window, cx: &mut Context<Self>) {
        let identity = self.identity();
        if label {
            self.label_input.update(cx, |input, cx| input.set_value(identity.label.clone(), window, cx));
            let accent = normalize_accent(&identity.accent).and_then(|hex| Hsla::parse_hex(&hex).ok());
            self.accent.update(cx, |picker, cx| match accent {
                Some(color) => picker.set_value(color, window, cx),
                None => picker.clear_value(window, cx),
            });
        }
        let id = self.instance_id(cx);
        let placeholder = format!("{}_work", self.driver.kind);
        self.id_input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            if input.value().as_ref() != id {
                input.set_value(id, window, cx);
            }
        });
        cx.notify();
    }

    fn form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ConfigForm> {
        let driver = self.driver;
        if let Some(form) = self.forms.get(driver.kind) {
            return form.clone();
        }
        let config = self.config().clone();
        let locks =
            self.workspace.upgrade().and_then(|w| config_locks(&w.read(cx).server_info.managed_policy, driver.kind));
        let form =
            cx.new(|cx| ConfigForm::new(driver, config, format!("add-provider-{}", driver.kind), locks, window, cx));
        self._subscriptions.push(cx.subscribe(&form, move |this, _, ConfigChanged(next), cx| {
            match next {
                Some(config) => this.configs.insert(driver.kind, config.clone()),
                None => this.configs.remove(driver.kind),
            };
            cx.notify();
        }));
        self.forms.insert(driver.kind, form.clone());
        form
    }

    /// Replaces a driver's config draft, rebuilding its form around it.
    fn reset_config(&mut self, driver: &'static str, config: Option<Value>) {
        self.forms.remove(driver);
        match config {
            Some(config) => self.configs.insert(driver, config),
            None => self.configs.remove(driver),
        };
    }

    fn select_driver(&mut self, driver: &'static DriverSpec, window: &mut Window, cx: &mut Context<Self>) {
        self.driver = driver;
        self.manual_acp = false;
        self.attempted = false;
        self.sync_identity_inputs(true, window, cx);
    }

    fn acp_prepared(&mut self, agent: RegistryAgent, window: &mut Window, cx: &mut Context<Self>) {
        let acp = driver_spec(ACP_REGISTRY).expect("acpRegistry driver");
        self.driver = acp;
        let id = derive_available_instance_id(
            |label| derive_instance_id(ACP_REGISTRY, label),
            &agent.name,
            &self.existing_ids(cx),
        );
        self.set_identity(ACP_REGISTRY, &agent.name, Some(id));
        let mut config = Map::new();
        config.insert("agentId".into(), agent.id.clone().into());
        config.insert("distribution".into(), "auto".into());
        if let Some(url) = super::acp::official_icon_url(agent.icon.as_deref()) {
            config.insert("registryIconUrl".into(), url.into());
        }
        self.reset_config(ACP_REGISTRY, Some(Value::Object(config)));
        self.selected_acp = Some(agent);
        self.manual_acp = false;
        self.attempted = false;
        self.step = 1;
        self.sync_identity_inputs(true, window, cx);
    }

    fn manual_acp_configuration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.driver = driver_spec(ACP_REGISTRY).expect("acpRegistry driver");
        self.selected_acp = None;
        self.manual_acp = true;
        self.reset_config(ACP_REGISTRY, None);
        self.set_identity(ACP_REGISTRY, "", None);
        self.attempted = false;
        self.sync_identity_inputs(true, window, cx);
    }

    fn local_acp_configuration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.driver = driver_spec(ACP_REGISTRY).expect("acpRegistry driver");
        self.selected_acp = None;
        self.manual_acp = true;
        self.reset_config(ACP_REGISTRY, Some(json!({"source": "local", "commandArgs": []})));
        self.set_identity(ACP_REGISTRY, "Local ACP", None);
        self.attempted = false;
        self.sync_identity_inputs(true, window, cx);
    }

    fn navigate(&mut self, requested: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy(cx) || self.created.is_some() {
            return;
        }
        let id_error = self.instance_id_error(cx).is_some();
        let prerequisite = self.is_acp().then(|| (0, self.acp_selection_error().is_some()));
        let navigation =
            resolve_navigation(self.step, requested, self.steps().len(), IDENTITY_STEP, id_error, prerequisite);
        let step = match navigation {
            Navigation::Blocked(step) => {
                self.attempted = true;
                step
            }
            Navigation::Navigate(step) => step,
        };
        if self.is_acp() && !self.is_local_acp() && navigation == Navigation::Navigate(2) {
            self.save(window, cx);
            return;
        }
        self.step = step;
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.created.is_some() {
            return;
        }
        self.attempted = true;
        if self.instance_id_error(cx).is_some() || (self.is_acp() && self.acp_selection_error().is_some()) {
            cx.notify();
            return;
        }
        let Some(client) = self.workspace.upgrade().and_then(|w| w.read(cx).client()) else { return };
        let identity = self.identity();
        let mut config = self.config().as_object().cloned().unwrap_or_default();
        if self.driver.kind == "codex" {
            config.insert("setupMode".into(), "existing".into());
        }
        let mut instance = Map::new();
        instance.insert("driver".into(), self.driver.kind.into());
        instance.insert("enabled".into(), true.into());
        if !identity.label.trim().is_empty() {
            instance.insert("displayName".into(), identity.label.trim().into());
        }
        if let Some(accent) = normalize_accent(&identity.accent) {
            instance.insert("accentColor".into(), accent.into());
        }
        if !config.is_empty() {
            instance.insert("config".into(), Value::Object(config));
        }
        let instance_id = self.instance_id(cx);
        let payload = json!({
            "patch": {},
            "providerInstanceMutation": {"operation": "create", "instanceId": instance_id, "instance": instance},
        });
        let then_sign_in = self.is_acp() && !self.is_local_acp();
        let driver_label = self.driver.label;
        self.saving = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("server.updateSettings", payload).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                cx.notify();
                if let Err(error) = result {
                    return notify_error(window, cx, "Could not add provider instance", rpc_message(&error));
                }
                if let Some(workspace) = this.workspace.upgrade() {
                    workspace.update(cx, |w, cx| w.provider_instance_created(&instance_id, cx));
                }
                if then_sign_in {
                    this.step = 2;
                    this.watch_auth(instance_id.clone(), window, cx);
                    this.created = Some(instance_id);
                    return;
                }
                window.push_notification(
                    Notification::success(format!("{driver_label} instance '{instance_id}' was added."))
                        .title("Provider instance added"),
                    cx,
                );
                window.close_dialog(cx);
            });
        })
        .detach();
    }

    /// `serverEnvironment.providerAuthState`: the `provider.auth.subscribe` stream.
    fn watch_auth(&mut self, instance_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.workspace.upgrade().and_then(|w| w.read(cx).client()) else { return };
        let stream = client.stream("provider.auth.subscribe", json!({"instanceId": instance_id}));
        self._auth_task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = matches!(event, StreamEvent::End(_));
                let _ = this.update(cx, |this, cx| {
                    match event {
                        StreamEvent::Item(state) => this.auth = Some(state),
                        StreamEvent::End(Err(error)) => this.auth_error = Some(rpc_message(&error).into()),
                        StreamEvent::End(Ok(())) => {}
                    }
                    cx.notify();
                });
                if ended {
                    break;
                }
            }
        }));
    }

    fn close(window: &mut Window, cx: &mut App) {
        window.close_dialog(cx);
    }
}

// MARK: Wizard rendering

impl AddProviderWizard {
    fn render_steps(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let disabled = self.busy(cx) || self.created.is_some();
        let steps = self.steps();
        h_flex()
            .gap(px(4.))
            .p(px(4.))
            .rounded(px(12.))
            .bg(if t.dark { white(0.04) } else { t.background })
            .border_1()
            .border_color(if t.dark { white(0.05) } else { black(0.05) })
            .children(steps.iter().enumerate().map(|(index, label)| {
                let current = index == self.step;
                let done = index < self.step;
                let badge = div()
                    .size(px(20.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(14.))
                    .font_weight(FontWeight::MEDIUM)
                    .border_1()
                    .map(|badge| {
                        if done {
                            badge
                                .bg(t.primary)
                                .border_color(t.primary)
                                .text_color(t.primary_foreground)
                                .child(Icon::new(IconName::Check).size(px(14.)).text_color(t.primary_foreground))
                        } else if current {
                            badge
                                .bg(t.primary.opacity(0.1))
                                .border_color(t.primary.opacity(0.3))
                                .text_color(t.primary)
                                .child((index + 1).to_string())
                        } else {
                            badge
                                .bg(if t.dark { white(0.05) } else { t.card })
                                .border_color(if t.dark { white(0.1) } else { black(0.1) })
                                .text_color(t.muted_foreground)
                                .child((index + 1).to_string())
                        }
                    });
                h_flex()
                    .id(SharedString::from(format!("add-provider-step-{label}")))
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .when(current, |step| {
                        step.bg(t.card).border_1().border_color(if t.dark { white(0.05) } else { black(0.05) })
                    })
                    .when(!disabled, |step| {
                        step.cursor_pointer()
                            .when(!current, |step| step.hover(|s| s.bg(t.card)))
                            .on_click(cx.listener(move |this, _, window, cx| this.navigate(index, window, cx)))
                    })
                    .child(badge)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if current { t.foreground } else { t.muted_foreground })
                            .child(*label),
                    )
            }))
    }

    fn render_option_grid(
        &self,
        id: &'static str,
        options: Vec<&'static DriverSpec>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let disabled = self.registry.read(cx).is_preparing();
        div().id(id).grid().grid_cols(2).gap(px(8.)).children(options.into_iter().map(|option| {
            let checked = option.kind == self.driver.kind;
            let base = if t.dark { white(0.03) } else { t.card };
            h_flex()
                .id(SharedString::from(format!("add-provider-driver-{}", option.kind)))
                .gap(px(12.))
                .px(px(12.))
                .py(px(12.))
                .rounded(px(8.))
                .text_color(if checked { t.foreground } else { t.muted_foreground })
                .map(|card| {
                    if checked {
                        card.bg(t.primary.opacity(if t.dark { 0.15 } else { 0.08 })).border_2().border_color(t.primary)
                    } else {
                        card.bg(base)
                            .border_1()
                            .border_color(if t.dark { white(0.05) } else { black(0.05) })
                            .hover(|s| s.bg(if t.dark { white(0.05) } else { t.muted }))
                    }
                })
                .when(!disabled, |card| {
                    card.cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| this.select_driver(option, window, cx)))
                })
                .child(provider_glyph(option.kind, px(16.), t))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(14.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(option.label),
                )
                .when(checked, |card| {
                    card.child(
                        div()
                            .size(px(20.))
                            .flex_none()
                            .rounded_full()
                            .bg(t.primary)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::Check).size(px(14.)).text_color(t.primary_foreground)),
                    )
                })
                .children(option.badge.map(|badge| {
                    div()
                        .px(px(6.))
                        .rounded(px(4.))
                        .bg(t.warning.opacity(0.16))
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.warning_foreground)
                        .child(badge)
                }))
        }))
    }

    /// `MultipleAccountsNote`: adding a second Codex or Claude account while one is signed in.
    fn render_multiple_accounts_note(&self, t: &Tokens, cx: &App) -> Option<impl IntoElement> {
        let name = match self.driver.kind {
            "codex" => "Codex",
            "claudeAgent" => "Claude",
            _ => return None,
        };
        let workspace = self.workspace.upgrade()?;
        let signed_in = workspace
            .read(cx)
            .server_info
            .providers
            .iter()
            .any(|p| p.driver == self.driver.kind && p.auth.status == "authenticated");
        signed_in.then(|| {
            h_flex()
                .gap(px(8.))
                .px(px(14.))
                .py(px(10.))
                .rounded(px(10.))
                .border_1()
                .border_color(t.info.opacity(0.32))
                .bg(t.info.opacity(0.04))
                .text_size(px(14.))
                .child(Icon::new(IconName::Info).size(px(16.)).text_color(t.info_foreground))
                .child(format!("We do not recommend managing multiple {name} accounts in T3 Code."))
                .child(
                    div()
                        .id("multiple-accounts-faq")
                        .cursor_pointer()
                        .underline()
                        .hover(|s| s.text_color(t.foreground))
                        .child("More info")
                        .on_click(|_, _, cx| cx.open_url(MULTIPLE_ACCOUNTS_FAQ_URL)),
                )
        })
    }

    fn render_more(&mut self, window: &mut Window, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let policy =
            self.workspace.upgrade().map(|w| w.read(cx).server_info.managed_policy.clone()).unwrap_or_default();
        let more: Vec<_> = DRIVERS
            .iter()
            .filter(|d| !d.featured && d.kind != ACP_REGISTRY && driver_allowed(&policy, d.kind))
            .collect();
        let acp_allowed = driver_allowed(&policy, ACP_REGISTRY);
        if more.is_empty() && !acp_allowed {
            return div().into_any_element();
        }
        let toggle = h_flex().pt(px(8.)).child(
            Button::new("add-provider-show-more")
                .ghost()
                .xsmall()
                .label(if self.show_more { "Show less" } else { "Show more" })
                .child(
                    Icon::new(if self.show_more { Lucide::ChevronUp } else { Lucide::ChevronDown })
                        .size(px(14.))
                        .text_color(t.muted_foreground),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_more = !this.show_more;
                    cx.notify();
                })),
        );
        let error = (self.is_acp() && self.attempted).then(|| self.acp_selection_error()).flatten();
        v_flex()
            .child(toggle)
            .when(self.show_more, |fold| {
                fold.child(
                    v_flex()
                        .pt(px(16.))
                        .gap(px(16.))
                        .child(self.render_option_grid("add-provider-more", more, t, cx))
                        .when(acp_allowed, |fold| {
                            let rule = || div().flex_1().h(px(1.)).bg(t.border.opacity(0.7));
                            fold.child(
                                h_flex()
                                    .gap(px(12.))
                                    .text_size(px(12.))
                                    .text_color(t.muted_foreground)
                                    .child(rule())
                                    .child("Or choose from ACP Registry")
                                    .child(rule()),
                            )
                            .child(if self.is_acp() && self.manual_acp {
                                self.render_manual_acp(window, t, cx)
                            } else {
                                self.registry.clone().into_any_element()
                            })
                            .children(error.map(|e| div().text_size(px(11.)).text_color(t.error_foreground).child(e)))
                        }),
                )
            })
            .into_any_element()
    }

    fn render_manual_acp(&mut self, window: &mut Window, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let local = self.is_local_acp();
        let environment = self.environment_label(cx);
        let form = self.form(window, cx);
        v_flex()
            .gap(px(16.))
            .child(
                h_flex()
                    .items_start()
                    .justify_between()
                    .gap(px(12.))
                    .child(
                        v_flex()
                            .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).child(if local {
                                "Local ACP command"
                            } else {
                                "Enter manually"
                            }))
                            .child(div().mt(px(2.)).text_size(px(12.)).text_color(t.muted_foreground).child(
                                if local {
                                    format!("Run an installed ACP executable on {environment}.")
                                } else {
                                    "Enter an official registry ID and any local executable or auth override."
                                        .to_owned()
                                },
                            )),
                    )
                    .child(
                        Button::new("add-provider-search-registry").ghost().xsmall().label("Search registry").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.manual_acp = false;
                                this.attempted = false;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(form)
            .into_any_element()
    }

    fn environment_label(&self, cx: &App) -> String {
        self.workspace
            .upgrade()
            .and_then(|w| w.read(cx).server_info.environment_label.clone())
            .unwrap_or_else(|| "this environment".to_owned())
    }

    fn render_selected_acp(&self, t: &Tokens) -> Option<impl IntoElement> {
        let agent = self.selected_acp.as_ref().filter(|_| self.is_acp() && self.step == 1)?;
        let link = |id: &'static str, label: &'static str, url: Option<String>| {
            url.map(|url| {
                div()
                    .id(id)
                    .cursor_pointer()
                    .text_color(t.muted_foreground)
                    .hover(|s| s.text_color(t.foreground))
                    .child(label)
                    .on_click(move |_, _, cx| cx.open_url(&url))
            })
        };
        Some(
            h_flex()
                .mb(px(16.))
                .pb(px(12.))
                .items_start()
                .justify_between()
                .gap(px(12.))
                .border_b_1()
                .border_color(t.border.opacity(0.7))
                .child(
                    v_flex()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .text_size(px(14.))
                                .font_weight(FontWeight::MEDIUM)
                                .child(agent.name.clone()),
                        )
                        .child(
                            div()
                                .mt(px(2.))
                                .text_size(px(12.))
                                .text_color(t.muted_foreground)
                                .child(format!("v{} · {}", agent.version, agent.distribution)),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap(px(8.))
                        .text_size(px(11.))
                        .children(link("selected-acp-docs", "Docs", agent.website.clone()))
                        .children(link("selected-acp-source", "Source", agent.repository.clone())),
                ),
        )
    }

    fn render_identity(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.instance_id_error(cx).filter(|_| self.attempted);
        let accent_set = normalize_accent(&self.identity().accent).is_some();
        let accent = h_flex()
            .gap(px(4.))
            .child(ColorPicker::new(&self.accent).accessibility_label("Accent color"))
            .when(accent_set, |row| {
                row.child(
                    Button::new("add-provider-clear-accent")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Clear color")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.identity_mut().accent.clear();
                            this.accent.update(cx, |picker, cx| picker.clear_value(window, cx));
                            cx.notify();
                        })),
                )
            });
        v_flex()
            .gap(px(4.))
            .child(settings_row(
                "add-provider-label",
                "Label",
                Some("Shown in the provider list.".into()),
                None,
                Some(Input::new(&self.label_input).small().w(px(224.)).into_any_element()),
                t,
            ))
            .child(settings_row(
                "add-provider-instance-id",
                "Instance ID",
                Some("Letters, digits, '-', or '_'.".into()),
                error.map(|e| div().text_color(t.error_foreground).child(e).into_any_element()),
                Some(Input::new(&self.id_input).small().w(px(224.)).into_any_element()),
                t,
            ))
            .child(settings_row(
                "add-provider-accent",
                "Accent color",
                Some("Optional marker shown in the picker.".into()),
                None,
                Some(accent.into_any_element()),
                t,
            ))
    }

    fn render_auth_step(&mut self, window: &mut Window, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let instance_id = self.created.clone().unwrap_or_default();
        let workspace = self.workspace.upgrade();
        let provider = workspace.as_ref().and_then(|w| w.read(cx).server_info.provider(&instance_id).cloned());
        let phase = self.auth.as_ref().and_then(|a| a.get("phase")).and_then(Value::as_str).unwrap_or_default();
        let active = matches!(phase, "starting" | "waiting" | "verifying");
        let status = provider.as_ref().map(|p| p.auth.status.as_str()).unwrap_or_default();
        let signed_in = status == "authenticated" || (status == "unknown" && phase == "succeeded");
        let methods = self.auth.as_ref().and_then(|a| a.get("methods")).and_then(Value::as_array);
        let discovering = !signed_in && self.auth_error.is_none() && methods.is_none();
        let can_authenticate = methods.is_some_and(|m| !m.is_empty());
        let section = (provider.is_some() && (can_authenticate || signed_in))
            .then(|| workspace.as_ref())
            .flatten()
            .and_then(|w| w.update(cx, |w, cx| w.render_provider_auth_section(&instance_id, window, cx)));
        let body = section.unwrap_or_else(|| {
            let message = self.auth.as_ref().and_then(|a| a.get("message")).and_then(Value::as_str);
            let description: SharedString = if discovering {
                "Discovering sign-in methods…".into()
            } else {
                self.auth_error.clone().or_else(|| message.map(|m| m.to_owned().into())).unwrap_or_else(|| {
                    "No in-app sign-in advertised. Follow the provider's docs to finish setup.".into()
                })
            };
            let docs = provider.as_ref().and_then(|p| p.setup.as_ref()?.documentation_url.clone());
            let control = if discovering {
                Some(Button::new("auth-sign-in").outline().small().label("Sign in").disabled(true).into_any_element())
            } else {
                docs.map(|url| {
                    Button::new("auth-open-docs")
                        .outline()
                        .small()
                        .label("Open docs")
                        .on_click(move |_, _, cx| cx.open_url(&url))
                        .into_any_element()
                })
            };
            settings_row("auth-account", "Account", Some(description), None, control, t).into_any_element()
        });
        let finish = Button::new("auth-finish")
            .small()
            .label(if signed_in { "Done" } else { "Skip for now" })
            .disabled(active)
            .on_click(|_, window, cx| Self::close(window, cx));
        v_flex()
            .child(panel(t).min_h(px(288.)).child(body))
            .child(footer().child(if signed_in { finish.primary() } else { finish.outline() }))
            .into_any_element()
    }

    fn render_footer(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy(cx);
        let preparing = self.registry.read(cx).is_preparing();
        let step = self.step;
        let back = Button::new("add-provider-back")
            .small()
            .label(if step == 0 { "Cancel" } else { "Back" })
            .disabled(busy)
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.step == 0 {
                    Self::close(window, cx);
                } else {
                    this.step -= 1;
                    cx.notify();
                }
            }));
        let back = if step == 0 { back.ghost() } else { back.outline() };
        let primary: AnyElement = if step == 0 && self.driver.kind == "codex" {
            let workspace = self.workspace.upgrade();
            let locked = workspace.as_ref().is_none_or(|w| {
                let w = w.read(cx);
                !w.has_codex_account_dialog()
                    || config_locks(&w.server_info.managed_policy, "codex")
                        .is_some_and(|(keys, _)| keys.contains("setupMode"))
            });
            h_flex()
                .gap(px(8.))
                .child(
                    Button::new("add-provider-configure-manually")
                        .outline()
                        .small()
                        .label("Configure manually")
                        .on_click(cx.listener(|this, _, window, cx| this.navigate(1, window, cx))),
                )
                .when(!locked, |row| {
                    row.child(
                        Button::new("add-provider-chatgpt")
                            .primary()
                            .small()
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .child(
                                        svg()
                                            .path("icons/app/openai.svg")
                                            .size(px(16.))
                                            .text_color(t.primary_foreground),
                                    )
                                    .child("Continue with ChatGPT"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                let Some(workspace) = this.workspace.upgrade() else { return };
                                Self::close(window, cx);
                                workspace.update(cx, |w, cx| w.open_add_codex_account_dialog(window, cx));
                            })),
                    )
                })
                .into_any_element()
        } else if step < if self.is_acp() { 1 } else { 2 } {
            Button::new("add-provider-next")
                .primary()
                .small()
                .label("Next")
                .disabled(preparing)
                .on_click(cx.listener(move |this, _, window, cx| this.navigate(step + 1, window, cx)))
                .into_any_element()
        } else {
            let label = if self.saving {
                "Adding..."
            } else if self.is_acp() && !self.is_local_acp() {
                "Continue to sign-in"
            } else {
                "Add instance"
            };
            Button::new("add-provider-save")
                .primary()
                .small()
                .label(label)
                .disabled(self.saving)
                .on_click(cx.listener(|this, _, window, cx| this.save(window, cx)))
                .into_any_element()
        };
        footer().child(back).child(primary)
    }
}

impl Render for AddProviderWizard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *tokens(cx);
        let environment = self.environment_label(cx);
        let header = v_flex()
            .gap(px(8.))
            .p(px(24.))
            .pb(px(12.))
            .child(
                div().text_size(px(20.)).line_height(px(27.5)).font_weight(FontWeight::SEMIBOLD).child("Add provider"),
            )
            .child(
                div()
                    .text_size(px(14.))
                    .text_color(t.muted_foreground)
                    .child(format!("Add an account or configure a provider on {environment}.")),
            )
            .child(self.render_steps(&t, cx));
        // `WizardPopup` scrolls its whole content inside the window.
        let max_h = window.viewport_size().height - px(64.);
        let root = v_flex().id("add-provider-wizard").max_h(max_h).overflow_y_scroll().child(header);
        if self.created.is_some() {
            return root.child(self.render_auth_step(window, &t, cx));
        }
        let policy =
            self.workspace.upgrade().map(|w| w.read(cx).server_info.managed_policy.clone()).unwrap_or_default();
        let featured: Vec<_> = DRIVERS
            .iter()
            .filter(|d| d.featured && d.kind != ACP_REGISTRY && driver_allowed(&policy, d.kind))
            .collect();
        let mut body = panel(&t);
        match self.step {
            0 => {
                body = body
                    .child(
                        v_flex()
                            .gap(px(8.))
                            .child(div().text_size(px(14.)).font_weight(FontWeight::MEDIUM).child("Provider"))
                            .child(self.render_option_grid("add-provider-featured", featured, &t, cx))
                            .children(self.render_multiple_accounts_note(&t, cx)),
                    )
                    .child(self.render_more(window, &t, cx));
            }
            1 => {
                body = body
                    .when(self.is_acp(), |b| b.min_h(px(288.)))
                    .children(self.render_selected_acp(&t))
                    .child(self.render_identity(&t, cx));
            }
            _ if !self.is_acp() => {
                body = if self.driver.fields.is_empty() {
                    body.child(
                        div()
                            .text_size(px(14.))
                            .text_color(t.muted_foreground)
                            .child("This driver has no required configuration. You can add the instance now."),
                    )
                } else {
                    body.child(self.form(window, cx))
                };
            }
            _ => {}
        }
        root.child(body).child(self.render_footer(&t, cx))
    }
}

fn panel(t: &Tokens) -> Div {
    v_flex()
        .min_w_0()
        .gap(px(16.))
        .px(px(24.))
        .py(px(20.))
        .bg(if t.dark { white(0.02) } else { t.background.opacity(0.8) })
        .border_y_1()
        .border_color(if t.dark { white(0.05) } else { black(0.05) })
}

fn footer() -> Div {
    h_flex().justify_end().gap(px(8.)).px(px(24.)).py(px(16.))
}

/// `ProviderInstanceIcon` for a driver kind; drivers without a bundled logo show a generic bot glyph.
pub(super) fn provider_glyph(driver: &str, size: gpui_kit::Pixels, t: &Tokens) -> AnyElement {
    match crate::assets::provider_icon(driver) {
        Some((path, tint)) => {
            svg().path(path).size(size).flex_none().text_color(tint.unwrap_or(t.foreground)).into_any_element()
        }
        None => Icon::new(Lucide::Bot).size(size).text_color(t.muted_foreground).into_any_element(),
    }
}

fn white(alpha: f32) -> Hsla {
    gpui_kit::hsla(0., 0., 1., alpha)
}

fn black(alpha: f32) -> Hsla {
    gpui_kit::hsla(0., 0., 0., alpha)
}

impl Workspace {
    /// Opens the Add provider wizard over the Settings modal.
    pub(in crate::app) fn open_add_provider_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity();
        // The wizard reads the workspace, which this call is still updating.
        window.defer(cx, move |window, cx| {
            let wizard = cx.new(|cx| AddProviderWizard::new(workspace, window, cx));
            window.open_dialog(cx, move |dialog, _, _| dialog.p_0().w(px(768.)).centered().child(wizard.clone()));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> HashSet<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    // regression: #2813 the wizard advanced past an invalid instance id
    #[test]
    fn blocks_leaving_identity_while_the_instance_id_is_invalid() {
        assert_eq!(resolve_navigation(1, 2, 3, 1, true, None), Navigation::Blocked(1));
        assert_eq!(resolve_navigation(0, 2, 3, 1, true, None), Navigation::Blocked(1));
        assert_eq!(resolve_navigation(2, 0, 3, 1, true, None), Navigation::Navigate(0));
        assert_eq!(resolve_navigation(0, 9, 3, 1, false, None), Navigation::Navigate(2));
    }

    #[test]
    fn registry_wizard_needs_a_selection_before_identity() {
        assert_eq!(resolve_navigation(0, 1, 3, 1, false, Some((0, true))), Navigation::Blocked(0));
        assert_eq!(resolve_navigation(0, 1, 3, 1, false, Some((0, false))), Navigation::Navigate(1));
    }

    #[test]
    fn derives_available_instance_ids_from_labels() {
        assert_eq!(derive_instance_id("codex", "  My Work! "), "codex_my_work");
        assert_eq!(derive_instance_id("codex", "!!!"), "");
        assert_eq!(derive_instance_id("codex", "Café Ünï"), "codex_caf_n");
        let existing = ids(&["codex_work", "codex_work_2"]);
        let derive = |label: &str| derive_instance_id("codex", label);
        assert_eq!(derive_available_instance_id(derive, "Work", &existing), "codex_work_3");
        let long = "x".repeat(60);
        let existing = ids(&[&format!("acpRegistry_{}", "x".repeat(48))]);
        let id = derive_available_instance_id(|l| derive_instance_id("acpRegistry", l), &long, &existing);
        assert_eq!(id.len(), 62);
        assert!(id.ends_with("_2"));
    }

    #[test]
    fn validates_instance_ids() {
        let existing = ids(&["codex"]);
        assert_eq!(validate_instance_id("", &existing).as_deref(), Some("Instance ID is required."));
        assert!(validate_instance_id("1abc", &existing).unwrap().starts_with("Instance ID must start"));
        assert!(validate_instance_id("a b", &existing).is_some());
        assert_eq!(
            validate_instance_id("codex", &existing).as_deref(),
            Some("An instance named 'codex' already exists.")
        );
        assert_eq!(
            validate_instance_id(&"a".repeat(65), &existing).as_deref(),
            Some("Instance ID must be 64 characters or fewer.")
        );
        assert_eq!(validate_instance_id("codex_work-2", &existing), None);
    }

    #[test]
    fn empty_values_clear_keys_unless_the_field_persists_them() {
        let codex = driver_spec("codex").unwrap();
        let binary = codex.fields[0];
        assert_eq!(next_config(&Value::Null, &binary, "/bin/codex"), Some(json!({"binaryPath": "/bin/codex"})));
        assert_eq!(next_config(&json!({"binaryPath": "x"}), &binary, "  "), None);
        let agent_id = driver_spec(ACP_REGISTRY).unwrap().fields[1];
        assert_eq!(next_config(&Value::Null, &agent_id, ""), Some(json!({"agentId": ""})));
    }

    #[test]
    fn local_acp_shows_only_its_source_and_executable() {
        let acp = driver_spec(ACP_REGISTRY).unwrap();
        let local = visible_fields(acp, &json!({"source": "local"}));
        assert_eq!(local.iter().map(|f| f.key).collect::<Vec<_>>(), ["source", "commandPath"]);
        assert_eq!(local[1].label, "Executable");
        assert_eq!(visible_fields(acp, &Value::Null).len(), 4);
    }

    #[test]
    fn registry_agents_count_as_added_only_for_registry_instances() {
        let instances = json!({
            "a": {"driver": "acpRegistry", "config": {"agentId": "gemini"}},
            "b": {"driver": "acpRegistry", "config": {"source": "local", "agentId": "kilo"}},
        });
        assert!(is_configured_acp_agent(&instances, "gemini"));
        assert!(!is_configured_acp_agent(&instances, "kilo"));
        assert!(!is_configured_acp_agent(&Value::Null, "gemini"));
    }

    #[test]
    fn managed_policy_filters_drivers_and_locks_config_keys() {
        let policy = json!({
            "organization": "Acme",
            "limits": {"providers": {"allowedDrivers": ["codex"], "lockedInstanceFields": {"codex": ["config.setupMode", "environment"]}}},
        });
        assert!(driver_allowed(&policy, "codex"));
        assert!(!driver_allowed(&policy, "claudeAgent"));
        assert!(driver_allowed(&Value::Null, "claudeAgent"));
        let (keys, reason) = config_locks(&policy, "codex").unwrap();
        assert!(keys.contains("setupMode") && keys.len() == 1);
        assert_eq!(reason.as_ref(), "Managed by Acme.");
        assert!(config_locks(&policy, "claudeAgent").is_none());
    }

    #[test]
    fn accent_colors_must_be_six_digit_hex() {
        assert_eq!(normalize_accent(" #2563eb ").as_deref(), Some("#2563eb"));
        assert_eq!(normalize_accent("#fff"), None);
        assert_eq!(normalize_accent("2563eb0"), None);
    }
}
