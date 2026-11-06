//! The instance list and the selected instance's editor (`ProviderInstanceCard.tsx`,
//! `ProviderSettingsForm.tsx`, `providerStatus.ts`, the row building in `ProviderSettingsPanel.tsx`).

use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, svg,
};
use serde_json::{Value, json};

use super::super::widgets::{SettingsView, layer_row, reset_button};
use super::accent::{accent_color_picker, hex_color, normalize_accent_color};
use super::accounts::codex_setup_managed;
use super::drivers::{self, Driver, EnvironmentField};
use super::{
    ProvidersPage, divided, dot, ghost_icon_button, group, outline_button, section, settings_row, switch, text_input,
};
use crate::app::Workspace;
use crate::settings::Settings;
use crate::settings::scope::{self, ScopeKind};
use crate::theme::{self, Tokens};

/// One entry in the instance list (`InstanceRow`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct InstanceRow {
    pub id: String,
    pub driver: String,
    /// `None` for a default slot the user never configured.
    pub explicit: Option<Value>,
    pub is_default: bool,
    pub is_dirty: bool,
}

impl InstanceRow {
    /// The saved config, or `{driver}` for an unconfigured default slot.
    pub fn instance(&self) -> Value {
        self.explicit.clone().unwrap_or_else(|| json!({ "driver": self.driver }))
    }
}

pub(super) use crate::settings::scope::instance_enabled as resolve_enabled;

/// The projects whose provider switches the page edits (`projectIds`), when the server keeps
/// per-project provider overrides (`projectProviderInstanceOverrides`).
pub(super) fn override_projects(view: &SettingsView, supported: bool) -> Vec<String> {
    match &view.scope.kind {
        ScopeKind::Project { members } if supported => members.iter().map(|m| m.id.clone()).collect(),
        _ => Vec::new(),
    }
}

/// A card's `projectOverride`: the first project's own value and the enablement it resolves to.
#[derive(Clone, Copy)]
struct ProjectOverride {
    value: Option<bool>,
    effective: bool,
}

fn project_override(settings: &Settings, projects: &[String], instance_id: &str) -> Option<ProjectOverride> {
    let first = projects.first()?;
    Some(ProjectOverride {
        value: scope::provider_instance_override(settings, first, instance_id),
        effective: scope::provider_instance_enabled(settings, Some(first), instance_id),
    })
}

pub(super) fn instance_config(settings: &Value, instance_id: &str) -> Option<Value> {
    settings.get("providerInstances")?.get(instance_id).cloned()
}

/// The list rows, in the web's order: each known driver's default slot (when it applies) then its
/// other instances, then instances of drivers this build does not know.
pub(super) fn instance_rows(settings: &Value, providers: &[Value]) -> Vec<InstanceRow> {
    let configured = settings.get("providerInstances").and_then(Value::as_object).cloned().unwrap_or_default();
    // Cursor's default slot shows only once the server reports it.
    let visible: Vec<&Driver> = drivers::DRIVERS
        .iter()
        .filter(|d| d.kind != "cursor" || providers.iter().any(|p| p["instanceId"] == "cursor"))
        .collect();
    let driver_of = |instance: &Value| instance.get("driver").and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut rows = Vec::new();
    for driver in &visible {
        let explicit = configured.get(driver.kind).cloned();
        let is_dirty = explicit.is_some();
        let has_slot = driver.has_default_instance || is_dirty;
        let enabled = resolve_enabled(&explicit.clone().unwrap_or_else(|| json!({ "driver": driver.kind })));
        if has_slot && (driver.kind == "codex" || driver.kind == "claudeAgent" || is_dirty || enabled) {
            rows.push(InstanceRow {
                id: driver.kind.to_owned(),
                driver: driver.kind.to_owned(),
                explicit,
                is_default: true,
                is_dirty,
            });
        }
        for (id, instance) in &configured {
            if id != driver.kind && driver_of(instance) == driver.kind {
                rows.push(InstanceRow {
                    id: id.clone(),
                    driver: driver.kind.to_owned(),
                    explicit: Some(instance.clone()),
                    is_default: false,
                    is_dirty: false,
                });
            }
        }
    }
    // Unknown drivers, grouped by driver in first-seen order.
    let mut unknown: Vec<String> = Vec::new();
    for instance in configured.values() {
        let driver = driver_of(instance);
        if !visible.iter().any(|d| d.kind == driver) && !unknown.contains(&driver) {
            unknown.push(driver);
        }
    }
    for driver in unknown {
        for (id, instance) in &configured {
            if driver_of(instance) == driver {
                rows.push(InstanceRow {
                    id: id.clone(),
                    driver: driver.clone(),
                    explicit: Some(instance.clone()),
                    is_default: visible.iter().any(|d| d.kind == id),
                    is_dirty: false,
                });
            }
        }
    }
    rows
}

pub(super) fn live_provider<'a>(providers: &'a [Value], instance_id: &str) -> Option<&'a Value> {
    providers.iter().find(|p| p["instanceId"] == instance_id)
}

fn text<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StatusKey {
    Disabled,
    Error,
    Neutral,
    Ready,
    Warning,
}

impl StatusKey {
    fn needs_attention(self) -> bool {
        matches!(self, StatusKey::Warning | StatusKey::Error)
    }

    fn color(self, t: &Tokens) -> Hsla {
        match self {
            StatusKey::Disabled => t.muted_foreground.opacity(0.5),
            StatusKey::Error => t.error,
            StatusKey::Neutral => t.muted_foreground,
            StatusKey::Ready => t.success_foreground,
            StatusKey::Warning => t.warning,
        }
    }
}

/// Antigravity's health check verifies installation, not Google sign-in.
fn antigravity_sign_in_unchecked(provider: &Value) -> bool {
    provider["driver"] == "antigravity"
        && provider["installed"] == true
        && provider["status"] == "warning"
        && provider.pointer("/auth/status") == Some(&json!("unknown"))
        && matches!(text(provider, "/auth/type"), None | Some("oauth-personal" | "oauth-business"))
}

/// `getProviderStatusKey`.
pub(super) fn status_key(provider: Option<&Value>) -> StatusKey {
    let Some(provider) = provider else { return StatusKey::Warning };
    if provider["enabled"] != true {
        return StatusKey::Disabled;
    }
    if antigravity_sign_in_unchecked(provider) {
        return StatusKey::Neutral;
    }
    match provider["status"].as_str() {
        Some("ready") => StatusKey::Ready,
        Some("error") => StatusKey::Error,
        Some("disabled") => StatusKey::Disabled,
        _ => StatusKey::Warning,
    }
}

/// `getProviderSummary`: a headline and an optional detail.
pub(super) fn summary(provider: Option<&Value>) -> (String, Option<String>) {
    let Some(provider) = provider else {
        return (
            "Checking provider status".into(),
            Some("Waiting for the server to report installation and authentication details.".into()),
        );
    };
    let message = text(provider, "/message").map(str::to_owned);
    let auth_label = text(provider, "/auth/label").or(text(provider, "/auth/type"));
    let with_auth = |base: &str| auth_label.map_or(base.to_owned(), |label| format!("{base} · {label}"));
    let auth_status = provider.pointer("/auth/status").and_then(Value::as_str);
    if provider["enabled"] != true || provider["status"] == "disabled" {
        let detail = message.unwrap_or("This provider is installed but disabled for new sessions in T3 Code.".into());
        return ("Disabled".into(), Some(detail));
    }
    if provider["installed"] != true {
        return ("Not found".into(), Some(message.unwrap_or("CLI not detected on PATH.".into())));
    }
    if auth_status == Some("unauthenticated") {
        return (with_auth("Not authenticated"), message);
    }
    if antigravity_sign_in_unchecked(provider) {
        return ("Installed · Sign-in not checked".into(), message);
    }
    match provider["status"].as_str() {
        Some("warning") => (
            "Needs attention".into(),
            Some(message.unwrap_or("The provider is installed, but the server could not fully verify it.".into())),
        ),
        Some("error") => {
            ("Unavailable".into(), Some(message.unwrap_or("The provider failed its startup checks.".into())))
        }
        _ if auth_status == Some("authenticated") => (with_auth("Authenticated"), message),
        _ => ("Available".into(), message),
    }
}

/// `getProviderVersionLabel`: dates for Antigravity builds, `v` before numeric versions.
pub(super) fn version_label(version: Option<&str>) -> Option<String> {
    let version = version.filter(|v| !v.is_empty())?;
    if let Some(rest) = version.strip_prefix("agy_acp_server_") {
        let mut parts = rest.splitn(3, '_');
        let (date, build, candidate) = (parts.next()?, parts.next(), parts.next());
        let valid = date.len() == 8
            && date.bytes().all(|b| b.is_ascii_digit())
            && build.is_some_and(|b| !b.is_empty() && b.bytes().all(|c| c.is_ascii_digit()))
            && candidate.is_none_or(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        if valid {
            let date = format!("{}-{}-{}", &date[..4], &date[4..6], &date[6..]);
            return Some(candidate.map_or(date.clone(), |c| format!("{date} {c}")));
        }
    }
    Some(if version.starts_with(|c: char| c.is_ascii_digit()) { format!("v{version}") } else { version.to_owned() })
}

/// `getProviderVersionAdvisoryPresentation`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Advisory {
    pub title: String,
    pub detail: String,
    pub update_command: Option<String>,
    pub strong: bool,
    pub target_version: Option<String>,
}

pub(super) fn version_advisory(provider: &Value, show_compatibility: bool) -> Option<Advisory> {
    let advisory = provider.get("versionAdvisory").filter(|a| a.is_object());
    let compat = provider.get("compatibilityAdvisory").filter(|a| a.is_object());
    let latest_incompatible = compat
        .and_then(|c| c["latestVersionStatus"].as_str())
        .is_some_and(|status| status == "broken" || status == "unsupported");
    if let Some(compat) = compat.filter(|_| show_compatibility) {
        let title = match compat["status"].as_str() {
            Some("graceful") => Some("Limited support"),
            Some("unsupported") => Some("Unsupported version"),
            Some("broken") => Some("Known broken version"),
            _ => None,
        };
        if let Some(title) = title {
            let target_version = text(compat, "/recommendedVersion").map(str::to_owned);
            let recommendation =
                version_label(target_version.as_deref()).or(text(compat, "/recommendedRange").map(str::to_owned));
            let already_latest = advisory.is_some_and(|a| a["status"] == "current");
            return Some(Advisory {
                title: title.into(),
                detail: text(compat, "/message").map_or_else(
                    || match recommendation {
                        Some(recommendation) => format!("Use {recommendation} for full support."),
                        None => "Update for full support.".into(),
                    },
                    str::to_owned,
                ),
                update_command: if target_version.is_some() || latest_incompatible || already_latest {
                    None
                } else {
                    advisory.and_then(|a| text(a, "/updateCommand")).map(str::to_owned)
                },
                strong: compat["status"] != "graceful",
                target_version,
            });
        }
    }
    let advisory = advisory?;
    if advisory["status"] != "behind_latest" || latest_incompatible {
        return None;
    }
    let detail = text(advisory, "/message").map_or_else(
        || match version_label(text(advisory, "/latestVersion")) {
            Some(version) => format!("Update available: install {version}."),
            None => "Update available: install the latest provider version.".into(),
        },
        str::to_owned,
    );
    Some(Advisory {
        title: "Update available".into(),
        detail,
        update_command: text(advisory, "/updateCommand").map(str::to_owned),
        strong: false,
        target_version: None,
    })
}

fn latest_incompatible(provider: &Value) -> bool {
    matches!(
        provider.pointer("/compatibilityAdvisory/latestVersionStatus").and_then(Value::as_str),
        Some("broken" | "unsupported")
    )
}

/// `isProviderSettingsUpdateCandidate`.
fn is_settings_update_candidate(provider: &Value) -> bool {
    provider["enabled"] == true
        && !latest_incompatible(provider)
        && provider.pointer("/versionAdvisory/status") == Some(&json!("behind_latest"))
        && provider.pointer("/versionAdvisory/canUpdate") == Some(&json!(true))
        && text(provider, "/versionAdvisory/updateCommand").is_some()
}

/// `isProviderUpdateCandidate`.
fn is_update_candidate(provider: &Value) -> bool {
    provider["enabled"] == true
        && !latest_incompatible(provider)
        && provider.pointer("/versionAdvisory/status") == Some(&json!("behind_latest"))
        && text(provider, "/versionAdvisory/latestVersion").is_some()
}

/// `isProviderUpdateActive`.
fn is_update_active(provider: &Value) -> bool {
    matches!(provider.pointer("/updateState/status").and_then(Value::as_str), Some("queued" | "running"))
}

/// What "Update all" updates (`useProviderUpdates`): one instance per driver, and only when every
/// outdated instance of that driver updates with the same one-click command.
pub(super) fn one_click_update_candidates(providers: &[Value]) -> Vec<(String, String)> {
    let mut by_driver: Vec<&Value> = Vec::new();
    for provider in providers.iter().filter(|p| is_update_candidate(p)) {
        match by_driver.iter_mut().find(|current| current["driver"] == provider["driver"]) {
            None => by_driver.push(provider),
            Some(current) => {
                let default_id = provider["driver"].as_str().unwrap_or_default();
                let checked = |p: &Value| p["checkedAt"].as_str().unwrap_or_default().to_owned();
                if provider["instanceId"] == default_id
                    || (current["instanceId"] != default_id && checked(provider) >= checked(current))
                {
                    *current = provider;
                }
            }
        }
    }
    by_driver
        .into_iter()
        .filter(|candidate| {
            if is_update_active(candidate) || !is_settings_update_candidate(candidate) {
                return false;
            }
            let mut commands = Vec::new();
            for provider in providers.iter().filter(|p| p["driver"] == candidate["driver"] && is_update_candidate(p)) {
                if !is_settings_update_candidate(provider) {
                    return false;
                }
                let command = text(provider, "/versionAdvisory/updateCommand");
                if !commands.contains(&command) {
                    commands.push(command);
                }
            }
            commands.len() == 1
        })
        .map(|p| {
            (
                p["driver"].as_str().unwrap_or_default().to_owned(),
                p["instanceId"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// "Checked 5m ago" from the newest `checkedAt` (`ProviderLastChecked`).
pub(super) fn last_checked_label(providers: &[Value], now_ms: i64) -> Option<String> {
    let latest = providers.iter().filter_map(|p| p["checkedAt"].as_str()).max()?;
    let Some(checked) = crate::timestamp::parse_ms(latest) else { return Some("Checked unavailable".into()) };
    let minutes = (now_ms - checked).max(0) / 60_000;
    Some(match minutes {
        0 => "Checked just now".into(),
        1..60 => format!("Checked {minutes}m ago"),
        60..1440 => format!("Checked {}h ago", minutes / 60),
        _ => format!("Checked {}d ago", minutes / 1440),
    })
}

/// Replaces `key` in an instance config, or removes it for `None`.
pub(super) fn with_key(instance: &Value, key: &str, value: Option<Value>) -> Value {
    let mut next = instance.as_object().cloned().unwrap_or_default();
    match value {
        Some(value) => next.insert(key.to_owned(), value),
        None => next.remove(key),
    };
    Value::Object(next)
}

fn environment(instance: &Value) -> Vec<Value> {
    instance.get("environment").and_then(Value::as_array).cloned().unwrap_or_default()
}

/// `updateEnvironment`: drops unnamed variables and the key when nothing is left.
fn with_environment(instance: &Value, environment: Vec<Value>) -> Value {
    let cleaned: Vec<Value> =
        environment.into_iter().filter(|v| !v["name"].as_str().unwrap_or_default().trim().is_empty()).collect();
    with_key(instance, "environment", (!cleaned.is_empty()).then_some(Value::Array(cleaned)))
}

/// `nextProviderEnvironmentWithFieldValue`.
pub(super) fn environment_with_field_value(environment: &[Value], name: &str, value: &str) -> Vec<Value> {
    let trimmed = value.trim();
    let mut next = Vec::new();
    let mut found = false;
    for variable in environment {
        if variable["name"] != name {
            next.push(variable.clone());
            continue;
        }
        found = true;
        if !trimmed.is_empty() {
            next.push(json!({"name": name, "value": trimmed, "sensitive": true}));
        }
    }
    if !found && !trimmed.is_empty() {
        next.push(json!({"name": name, "value": trimmed, "sensitive": true}));
    }
    next
}

/// Generic variables being edited (`ProviderEnvironmentSection`'s `RowsDraft`).
#[derive(Default)]
pub(super) struct EnvironmentDraft {
    instance: Option<String>,
    seen: Vec<Value>,
    last_published: Option<Vec<Value>>,
    rows: Vec<EnvRow>,
    next_id: u64,
}

#[derive(Clone, Debug)]
struct EnvRow {
    id: u64,
    name: String,
    /// A string or a secret reference.
    value: Value,
    sensitive: bool,
    redacted: Option<bool>,
}

impl EnvironmentDraft {
    /// Reseeds the rows when the saved variables change from outside this editor.
    fn reconcile(&mut self, instance: &str, incoming: &[Value]) {
        if self.instance.as_deref() == Some(instance) && self.seen == incoming {
            return;
        }
        let echo = self.instance.as_deref() == Some(instance) && self.last_published.as_deref() == Some(incoming);
        if !echo {
            self.rows = incoming
                .iter()
                .map(|v| {
                    self.next_id += 1;
                    // The wire form keeps a reference in `secret` beside an empty value; it is always sensitive.
                    let secret = v.get("secret").filter(|s| s.is_object());
                    EnvRow {
                        id: self.next_id,
                        name: v["name"].as_str().unwrap_or_default().to_owned(),
                        value: secret.or_else(|| v.get("value")).cloned().unwrap_or(json!("")),
                        sensitive: secret.is_some() || v["sensitive"] != false,
                        redacted: v.get("valueRedacted").and_then(Value::as_bool),
                    }
                })
                .collect();
        }
        self.instance = Some(instance.to_owned());
        self.seen = incoming.to_vec();
        self.last_published = None;
    }

    /// `publishRows`: `None` while a row has an invalid name.
    fn published(&self) -> Option<Vec<Value>> {
        let mut published = Vec::new();
        for row in &self.rows {
            let name = row.name.trim();
            if !is_variable_name(name) {
                if !name.is_empty() || row.value != json!("") || !row.sensitive || row.redacted.is_some() {
                    return None;
                }
                continue;
            }
            let mut variable = if row.value.is_object() {
                json!({"name": name, "value": "", "secret": row.value, "sensitive": true})
            } else {
                json!({"name": name, "value": row.value, "sensitive": row.sensitive})
            };
            if let Some(redacted) = row.redacted {
                variable["valueRedacted"] = json!(redacted);
            }
            published.push(variable);
        }
        Some(published)
    }
}

/// `^[a-zA-Z_][a-zA-Z0-9_]*$`.
pub(super) fn is_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `redactedPlaceholder`: a stable stand-in of the same shape, for hidden account emails.
pub(super) fn redacted_placeholder(value: &str) -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut state: i32 = 0x811c9dc5_u32.cast_signed();
    for unit in value.encode_utf16() {
        state ^= i32::from(unit);
        state = state.wrapping_mul(0x01000193);
    }
    let mut next_char = || {
        state = (state ^ (state.cast_unsigned() >> 13).cast_signed()).wrapping_mul(0x85ebca6b_u32.cast_signed());
        state = (state ^ (state.cast_unsigned() >> 16).cast_signed()).wrapping_mul(0xc2b2ae35_u32.cast_signed());
        ALPHABET[state.unsigned_abs() as usize % ALPHABET.len()] as char
    };
    value.chars().map(|c| if matches!(c, '@' | '.' | '-' | '_') { c } else { next_char() }).collect()
}

/// `providerInstanceInitials`, for the accent badge.
pub(super) fn initials(label: &str) -> String {
    let words: Vec<String> = label
        .split(|c: char| c.is_whitespace() || (c.is_ascii_punctuation() && c != '\'') || c == '’')
        .map(|w| w.trim_matches(|c| c == '\'' || c == '’').to_owned())
        .filter(|w| !w.is_empty())
        .collect();
    match words.as_slice() {
        [] => String::new(),
        [word] => word.chars().take(2).collect::<String>().to_uppercase(),
        words => words.iter().take(2).filter_map(|w| w.chars().next()).collect::<String>().to_uppercase(),
    }
}

pub(super) fn accent_color(instance: &Value) -> Option<String> {
    normalize_accent_color(instance.get("accentColor").and_then(Value::as_str))
}

/// `providerAccentForegroundColor`: black or white, whichever contrasts more.
fn accent_foreground(hex: &str) -> Hsla {
    let channel = |offset: usize| {
        let value = f32::from(u8::from_str_radix(&hex[offset..offset + 2], 16).unwrap_or(0)) / 255.;
        if value <= 0.04045 { value / 12.92 } else { ((value + 0.055) / 1.055).powf(2.4) }
    };
    let luminance = 0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5);
    if (luminance + 0.05) / 0.05 >= 1.05 / (luminance + 0.05) { gpui_kit::black() } else { gpui_kit::white() }
}

fn display_name(row: &InstanceRow, instance: &Value) -> String {
    instance
        .get("displayName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| drivers::driver(&row.driver).map(|d| d.label.to_owned()))
        .unwrap_or_else(|| row.driver.clone())
}

/// What the list row and the editor header both show.
struct CardFacts {
    name: String,
    enabled: bool,
    status: StatusKey,
    headline: String,
    /// The compatibility verdict when there is one, else the summary detail.
    inline_detail: Option<String>,
    /// The summary detail, as a tooltip, when a compatibility warning shortens it.
    diagnostic: Option<String>,
    version: Option<String>,
    advisory: Option<Advisory>,
    compatibility_warning: bool,
    update_progress: Option<String>,
    update_problem: Option<String>,
    updating: bool,
}

/// `enabled` is the project's effective value at project scope, else the instance's own.
fn card_facts(row: &InstanceRow, live: Option<&Value>, dispatching: bool, enabled: bool) -> CardFacts {
    let instance = row.instance();
    let effective = live.map(|p| with_key(p, "enabled", Some(json!(enabled))));
    let status = if enabled { status_key(effective.as_ref()) } else { StatusKey::Disabled };
    let (headline, detail) = if enabled { summary(effective.as_ref()) } else { ("Disabled".into(), None) };
    let compat_status =
        live.filter(|_| enabled).and_then(|p| p.pointer("/compatibilityAdvisory/status")).and_then(Value::as_str);
    let compatibility_warning = matches!(compat_status, Some(s) if s != "supported" && s != "unknown");
    let inline_detail = if compatibility_warning {
        Some(match compat_status {
            Some("broken") => "Incompatible".into(),
            Some("unsupported") => "Unsupported".into(),
            _ => "Limited support".into(),
        })
    } else {
        detail.clone()
    };
    let updating = dispatching || live.is_some_and(is_update_active);
    let update_state = live.and_then(|p| p.get("updateState"));
    let update_message = update_state.and_then(|s| text(s, "/message")).map(str::to_owned);
    let update_status = update_state.and_then(|s| s["status"].as_str());
    CardFacts {
        name: display_name(row, &instance),
        enabled,
        status,
        headline,
        diagnostic: (compatibility_warning && status.needs_attention()).then_some(detail).flatten(),
        inline_detail,
        version: version_label(live.and_then(|p| text(p, "/version"))),
        advisory: live.and_then(|p| version_advisory(p, enabled)),
        compatibility_warning,
        update_progress: updating.then(|| {
            update_message
                .clone()
                .filter(|_| matches!(update_status, Some("queued" | "running")))
                .unwrap_or("Starting update".into())
        }),
        update_problem: (!updating && matches!(update_status, Some("failed" | "unchanged")))
            .then_some(update_message)
            .flatten(),
        updating,
    }
}

impl Workspace {
    /// A provider's logo with its accent badge (`ProviderInstanceIcon`).
    fn provider_instance_icon(&self, row: &InstanceRow, name: &str, t: &Tokens) -> AnyElement {
        let instance = row.instance();
        let accent = accent_color(&instance);
        let logo = match crate::assets::provider_icon(&row.driver) {
            Some((path, tint)) => {
                svg().path(path).size(px(16.)).text_color(tint.unwrap_or(t.foreground.opacity(0.8))).into_any_element()
            }
            None => div()
                .text_size(px(9.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.foreground.opacity(0.8))
                .child(initials(name))
                .into_any_element(),
        };
        div()
            .relative()
            .size(px(20.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(logo)
            .when_some(accent, |el, accent| {
                el.child(
                    div()
                        .absolute()
                        .right(px(-2.))
                        .bottom(px(-2.))
                        .h(px(12.))
                        .min_w(px(12.))
                        .px(px(2.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(t.border)
                        .bg(hex_color(&accent))
                        .text_size(px(7.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(accent_foreground(&accent))
                        .child(initials(name)),
                )
            })
            .into_any_element()
    }

    /// The selected-version popover: what changed, the update action and the manual command.
    fn render_version_advisory(
        &self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        facts: &CardFacts,
        compact: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let advisory = facts.advisory.clone()?;
        let live = super::instances::live_provider(&self.settings_info().providers_json, &row.id).cloned();
        let can_install = live.as_ref().is_some_and(|p| {
            text(p, "/compatibilityAdvisory/message").is_some()
                && text(p, "/compatibilityAdvisory/recommendedVersion").is_some()
                && p.pointer("/versionAdvisory/canInstallVersion") == Some(&json!(true))
        });
        let can_update = live.as_ref().is_some_and(is_settings_update_candidate);
        let action = if advisory.target_version.is_some() { can_install } else { can_update };
        let icon = if facts.compatibility_warning {
            gpui_kit::assets::IconName::TriangleAlert
        } else {
            gpui_kit::assets::IconName::CircleArrowUp
        };
        let tooltip = if facts.update_progress.is_some() { "Updating".to_owned() } else { advisory.title.clone() };
        let warn = facts.compatibility_warning;
        let trigger = ghost_icon_button(SharedString::from(format!("provider-advisory-{}", row.id)), icon, t)
            .when(compact, |b| b.size(px(20.)))
            .when(warn, |b| b.text_color(t.warning))
            .tooltip(tooltip);
        let (updating, progress, problem) =
            (facts.updating, facts.update_progress.clone(), facts.update_problem.clone());
        let (view, page, driver, instance_id, name) =
            (cx.entity().downgrade(), page.clone(), row.driver.clone(), row.id.clone(), facts.name.clone());
        let t = *t;
        Some(
            Popover::new(SharedString::from(format!("provider-advisory-popover-{}", row.id)))
                .anchor(gpui_kit::Anchor::TopRight)
                .trigger(trigger)
                .content(move |_, _, _| {
                    let run = {
                        let (view, page, driver, instance_id) =
                            (view.clone(), page.clone(), driver.clone(), instance_id.clone());
                        let target = advisory.target_version.clone();
                        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
                            let _ = view.update(cx, |this, cx| {
                                this.run_provider_update(&page, &driver, &instance_id, target.clone(), window, cx);
                            });
                        }
                    };
                    let command = advisory.update_command.clone();
                    let name = name.clone();
                    v_flex()
                        .w(px(320.))
                        .gap_3()
                        .child(
                            v_flex()
                                .gap(px(2.))
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(advisory.title.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if advisory.strong { t.warning } else { t.muted_foreground })
                                        .child(advisory.detail.clone()),
                                ),
                        )
                        .when(action, |el| {
                            let label = if updating {
                                "Updating".to_owned()
                            } else if let Some(target) = &advisory.target_version {
                                format!("Install {}", version_label(Some(target)).unwrap_or_default())
                            } else {
                                "Update now".to_owned()
                            };
                            el.child(
                                outline_button("provider-advisory-run", label)
                                    .xsmall()
                                    .h(px(24.))
                                    .w_full()
                                    .icon(Icon::new(gpui_kit::assets::IconName::Download).size(px(14.)))
                                    .loading(updating)
                                    .disabled(updating)
                                    .on_click(run),
                            )
                        })
                        .when_some(progress.clone().or(problem.clone()), |el, message| {
                            el.child(
                                div()
                                    .text_xs()
                                    .text_color(if problem.is_some() { t.warning } else { t.muted_foreground })
                                    .child(message),
                            )
                        })
                        .when(action && command.is_some(), |el| {
                            el.child(
                                h_flex()
                                    .gap_2()
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(t.muted_foreground)
                                    .child(div().h(px(1.)).flex_1().bg(t.border))
                                    .child("OR, UPDATE MANUALLY USING")
                                    .child(div().h(px(1.)).flex_1().bg(t.border)),
                            )
                        })
                        .when_some(command, |el, command| {
                            el.child(
                                h_flex()
                                    .gap_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(t.border.opacity(0.7))
                                    .bg(t.muted.opacity(0.4))
                                    .py(px(2.))
                                    .pr(px(2.))
                                    .pl_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(t.fonts.mono)
                                            .text_size(px(11.))
                                            .child(command.clone()),
                                    )
                                    .child(
                                        ghost_icon_button(
                                            "provider-advisory-copy",
                                            gpui_kit::assets::IconName::Copy,
                                            &t,
                                        )
                                        .tooltip("Copy command")
                                        .on_click(move |_, window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                                            window.push_notification(
                                                Notification::success(
                                                    "Run it in a terminal when you are ready to update.",
                                                )
                                                .title(format!("{name} update command copied")),
                                                cx,
                                            );
                                        }),
                                    ),
                            )
                        })
                })
                .into_any_element(),
        )
    }

    pub(super) fn render_instance_list_row(
        &mut self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        selected: bool,
        override_projects: &[String],
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let live = live_provider(&self.settings_info().providers_json, &row.id);
        let dispatching = page.read(cx).updating.contains(&row.id);
        let project_override = project_override(&self.settings_info().settings, override_projects, &row.id);
        let enabled = project_override.map_or_else(|| resolve_enabled(&row.instance()), |o| o.effective);
        let facts = card_facts(row, live, dispatching, enabled);
        let status_text = match &facts.update_progress {
            Some(progress) => format!("Updating · {progress}"),
            None => match (&facts.inline_detail, facts.status.needs_attention()) {
                (Some(detail), true) => format!("{} · {detail}", facts.headline),
                _ => facts.headline.clone(),
            },
        };
        let icon = self.provider_instance_icon(row, &facts.name, &t);
        let advisory = self.render_version_advisory(page, row, &facts, true, &t, cx);
        let select_page = page.clone();
        let select_id = row.id.clone();
        let toggle_row = row.clone();
        let diagnostic = facts.diagnostic.clone();
        h_flex()
            .id(SharedString::from(format!("provider-row-{}", row.id)))
            .min_h(px(72.))
            .px_4()
            .py_3()
            .gap_3()
            .when(selected, |el| el.bg(t.foreground.opacity(0.06)))
            .when(!selected, |el| el.hover(|s| s.bg(t.muted.opacity(0.25))))
            .child(
                h_flex()
                    .id(SharedString::from(format!("provider-select-{}", row.id)))
                    .flex_1()
                    .min_w_0()
                    .items_start()
                    .gap_3()
                    .cursor_pointer()
                    .when(!facts.enabled && !selected, |el| el.opacity(0.6))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        select_page.update(cx, |page, cx| {
                            page.selected = Some(select_id.clone());
                            cx.notify();
                        });
                    }))
                    .child(icon)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_2()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(t.foreground)
                                            .child(facts.name.clone()),
                                    )
                                    .when_some(facts.version.clone(), |el, version| {
                                        el.child(
                                            div()
                                                .max_w(px(96.))
                                                .flex_none()
                                                .truncate()
                                                .font_family(t.fonts.mono)
                                                .text_xs()
                                                .text_color(t.muted_foreground)
                                                .child(version),
                                        )
                                    })
                                    .children(advisory),
                            )
                            .child(
                                h_flex()
                                    .id(SharedString::from(format!("provider-status-{}", row.id)))
                                    .mt(px(2.))
                                    .items_start()
                                    .gap(px(6.))
                                    .text_xs()
                                    .text_color(t.muted_foreground.opacity(0.8))
                                    .when(facts.status.needs_attention() && facts.update_progress.is_none(), |el| {
                                        el.child(
                                            div().h(px(17.)).flex().items_center().child(dot(facts.status.color(&t))),
                                        )
                                    })
                                    .child(div().line_clamp(2).child(status_text))
                                    .when_some(diagnostic, |el, detail| {
                                        el.tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
                                    }),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .h(px(20.))
                    .flex_none()
                    .gap_1()
                    .when_some(project_override.and_then(|o| o.value), |el, project_enabled| {
                        let device_enabled = resolve_enabled(&row.instance());
                        el.child(self.render_project_override(
                            row,
                            &facts.name,
                            project_enabled,
                            device_enabled,
                            override_projects,
                            cx,
                        ))
                    })
                    .child(
                        switch(SharedString::from(format!("provider-enabled-{}", row.id)), facts.enabled)
                            .accessibility_label(format!("Enable {}", facts.name))
                            .on_change(cx.listener({
                                let projects = override_projects.to_vec();
                                move |this, checked: &bool, window, cx| {
                                    if !projects.is_empty() {
                                        this.write_provider_override(&projects, &toggle_row.id, Some(*checked), cx);
                                        return;
                                    }
                                    let next = with_key(&toggle_row.instance(), "enabled", Some(json!(*checked)));
                                    this.persist_provider_instance(&toggle_row.id, next, window, cx);
                                }
                            })),
                    ),
            )
            .into_any_element()
    }

    /// `ProjectOverrideInheritance` and the reset beside an overridden switch.
    fn render_project_override(
        &self,
        row: &InstanceRow,
        name: &str,
        project_enabled: bool,
        device_enabled: bool,
        projects: &[String],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let on_off = |enabled: bool| if enabled { "On" } else { "Off" }.to_owned();
        let layers = [
            scope::Layer { label: "Project".into(), value: on_off(project_enabled), effective: true, set: true },
            scope::Layer { label: "Device".into(), value: on_off(device_enabled), effective: false, set: true },
        ];
        let id = SharedString::from(format!("provider-override-{}", row.id));
        let reset_projects = projects.to_vec();
        let reset_id = row.id.clone();
        h_flex()
            .child(
                Popover::new(SharedString::from(format!("{id}-layers")))
                    .anchor(gpui_kit::Anchor::TopRight)
                    .p_0()
                    .trigger(
                        Button::new(SharedString::from(format!("{id}-trigger")))
                            .ghost()
                            .size(px(20.))
                            .p_0()
                            .rounded(px(4.))
                            .child(Icon::new(gpui_kit::assets::IconName::Layers).size(px(12.)).text_color(t.primary))
                            .tooltip("Overridden for this project"),
                    )
                    .content(move |_, _, cx| {
                        let t = theme::tokens(cx);
                        v_flex().w(px(320.)).px(px(12.)).py(px(10.)).children(layers.iter().map(|l| layer_row(l, t)))
                    }),
            )
            .child(
                reset_button(&id, "Reset to the device setting", cx)
                    .accessibility_label(format!("Reset {name} enablement to default"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.write_provider_override(&reset_projects, &reset_id, None, cx);
                    })),
            )
            .into_any_element()
    }

    /// `writeProjectOverride`: a project switch or its reset, written to the environment.
    fn write_provider_override(
        &mut self,
        projects: &[String],
        instance_id: &str,
        value: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        let patch = scope::provider_override_patch(&self.settings_info().settings, projects, instance_id, value);
        self.update_environment_settings(Value::Object(patch), cx, |_, _, _| {});
    }

    /// The selected instance's sections: header with status and name, Setup, Runtime,
    /// Environment, Models.
    pub(super) fn render_instance_editor(
        &mut self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        override_projects: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = *theme::tokens(cx);
        let live = live_provider(&self.settings_info().providers_json, &row.id).cloned();
        let dispatching = page.read(cx).updating.contains(&row.id);
        let enabled = project_override(&self.settings_info().settings, override_projects, &row.id)
            .map_or_else(|| resolve_enabled(&row.instance()), |o| o.effective);
        let facts = card_facts(row, live.as_ref(), dispatching, enabled);
        let instance = row.instance();
        let driver = drivers::driver(&row.driver);
        let mut sections = Vec::new();

        // Header: badge, version, update advisory, reset or delete.
        let advisory = self.render_version_advisory(page, row, &facts, false, &t, cx);
        let header_action = h_flex()
            .gap(px(6.))
            .when_some(driver.and_then(|d| d.badge_label), |el, badge| {
                el.child(
                    div()
                        .px(px(6.))
                        .rounded_sm()
                        .bg(t.warning.opacity(0.08))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.warning_foreground)
                        .child(badge),
                )
            })
            .when_some(facts.version.clone(), |el, version| {
                el.child(div().font_family(t.fonts.mono).text_xs().text_color(t.muted_foreground).child(version))
            })
            .children(advisory)
            .when(row.is_default && row.is_dirty, |el| {
                let reset = row.clone();
                el.child(
                    ghost_icon_button("provider-reset", gpui_kit::assets::IconName::Undo2, &t)
                        .size(px(20.))
                        .tooltip(format!("Reset {} provider settings", driver.map_or(row.driver.as_str(), |d| d.label)))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.confirm_reset_instance(&reset, window, cx)),
                        ),
                )
            })
            .when(!row.is_default, |el| {
                let remove = row.clone();
                el.child(
                    ghost_icon_button("provider-delete", Icon::empty().path("icons/trash-2.svg"), &t)
                        .tooltip(format!("Delete instance {}", row.id))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.confirm_delete_instance(&remove, window, cx)),
                        ),
                )
            });

        let status = self.render_editor_status(page, row, &facts, live.as_ref(), &t, cx);
        let display_name = self.render_display_name_control(page, row, &instance, window, cx);
        sections.push(
            section(
                facts.name.clone(),
                Some(self.provider_instance_icon(row, &facts.name, &t)),
                Some(header_action.into_any_element()),
                [
                    settings_row("Status", None, None, &t)
                        .child(status)
                        .flex_col()
                        .items_start()
                        .gap_1()
                        .into_any_element(),
                    settings_row("Display name", None, Some(display_name), &t).into_any_element(),
                ],
                false,
                &t,
            )
            .into_any_element(),
        );

        // Setup: the account slot where the web shows provider authentication, then API-key fields.
        let mut setup: Vec<AnyElement> = if row.driver == "antigravity" {
            vec![self.render_antigravity_setup(&row.id, window, cx)]
        } else {
            self.render_provider_account(&row.id, window, cx).into_iter().collect()
        };
        for field in driver.map_or(&[][..], |d| d.environment_fields) {
            setup.push(self.render_environment_field(page, row, &instance, field, &t, window, cx));
        }
        if !setup.is_empty() {
            sections.push(section("Setup", None, None, setup, false, &t).into_any_element());
        }

        // Runtime: a managed Codex instance's paths in a fold, the driver's settings form, or a note
        // for drivers this build does not know.
        let config = instance.get("config").cloned().unwrap_or_default();
        let managed_codex = row.driver == "codex" && codex_setup_managed(&config);
        match driver {
            _ if managed_codex => {
                let fields = self.render_codex_runtime_fields(&row.id, window, cx);
                sections.push(self.render_runtime_fold(page, &row.id, fields, &t, cx));
            }
            Some(driver) if !driver.fields.is_empty() => {
                let form = self.render_provider_runtime_form(&row.id, driver, config, window, cx);
                sections.push(section("Runtime", None, None, [form], false, &t).into_any_element());
            }
            Some(_) => {}
            None => sections.push(
                section(
                    "Runtime",
                    None,
                    None,
                    [settings_row(
                        "Driver",
                        Some(&format!(
                            "This instance uses {}, which is not available in this build. Its configuration is preserved.",
                            row.driver
                        )),
                        None,
                        &t,
                    )
                    .into_any_element()],
                    false,
                    &t,
                )
                .into_any_element(),
            ),
        }

        sections.push(
            section(
                "Environment",
                None,
                None,
                [self.render_environment_variables(page, row, &instance, &t, window, cx)],
                false,
                &t,
            )
            .into_any_element(),
        );

        sections.extend(self.render_acp_session_management(&row.id, false, window, cx));

        if driver.is_some() {
            let models = self.render_models_section(page, row, live.as_ref(), &instance, window, cx);
            sections.push(
                section(
                    "Models",
                    None,
                    None,
                    [v_flex()
                        .px_4()
                        .py_3()
                        .child(
                            div()
                                .mb_3()
                                .text_xs()
                                .text_color(t.muted_foreground)
                                .child("Model preferences and custom models are saved on the selected environment and shared across connected devices."),
                        )
                        .child(models)
                        .into_any_element()],
                    false,
                    &t,
                )
                .into_any_element(),
            );
        }
        sections
    }

    fn render_editor_status(
        &self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        facts: &CardFacts,
        live: Option<&Value>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let email = live
            .filter(|p| facts.enabled && p.pointer("/auth/status") == Some(&json!("authenticated")))
            .and_then(|p| text(p, "/auth/email"))
            .map(str::to_owned);
        let auth_label = live
            .filter(|_| facts.enabled)
            .filter(|p| p.pointer("/auth/status") == Some(&json!("authenticated")))
            .and_then(|p| text(p, "/auth/label").or(text(p, "/auth/type")))
            .map(str::to_owned);
        let attention = facts.status.needs_attention();
        let mut parts: Vec<AnyElement> = Vec::new();
        if attention {
            parts.push(div().h(px(16.)).flex().items_center().child(dot(facts.status.color(t))).into_any_element());
        }
        match email {
            Some(email) => {
                let revealed = page.read(cx).models.revealed_email;
                let toggle = page.clone();
                parts.push(div().child("Authenticated as").into_any_element());
                parts.push(
                    div()
                        .id("provider-auth-email")
                        .cursor_pointer()
                        .font_family(t.fonts.mono)
                        .text_size(px(11.))
                        .when(!revealed, |el| el.opacity(0.6))
                        .hover(|s| s.text_color(t.foreground))
                        .child(if revealed { email } else { redacted_placeholder(&email) })
                        .tooltip(move |window, cx| {
                            Tooltip::new(if revealed { "Click to hide email" } else { "Click to reveal email" })
                                .build(window, cx)
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            toggle.update(cx, |page, cx| {
                                page.models.revealed_email = !page.models.revealed_email;
                                cx.notify();
                            });
                        }))
                        .into_any_element(),
                );
                if let Some(label) = auth_label {
                    parts.push(div().child(format!("· {label}")).into_any_element());
                }
            }
            None => parts.push(div().child(facts.headline.clone()).into_any_element()),
        }
        if let Some(detail) = &facts.inline_detail {
            parts.push(div().min_w_0().child(format!("· {detail}")).into_any_element());
        }
        let diagnostic = facts.diagnostic.clone();
        let url_auth = live.and_then(|p| p.pointer("/auth/action")).filter(|a| a.is_object()).cloned();
        v_flex()
            .pt(px(2.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(
                h_flex()
                    .id(SharedString::from(format!("provider-editor-status-{}", row.id)))
                    .flex_wrap()
                    .gap_x(px(6.))
                    .children(parts)
                    .when_some(diagnostic, |el, detail| {
                        el.tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
                    }),
            )
            .when_some(url_auth, |el, action| {
                let message = action["message"].as_str().unwrap_or_default().to_owned();
                let url = action["url"].as_str().unwrap_or_default().to_owned();
                let elicitation = action["elicitationId"].clone();
                let instance_id = row.id.clone();
                el.child(
                    v_flex()
                        .max_w(px(576.))
                        .gap(px(6.))
                        .pt_1()
                        .child(div().text_color(t.foreground).child(message))
                        .child(div().font_family(t.fonts.mono).text_size(px(11.)).child(url.clone()))
                        .child(
                            outline_button("provider-continue-auth", "Continue authentication")
                                .xsmall()
                                .h(px(24.))
                                .icon(Icon::new(gpui_kit::assets::IconName::ExternalLink).size(px(14.)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.open_url(&url);
                                    let payload = json!({"instanceId": instance_id, "elicitationId": elicitation});
                                    this.providers_request_then(
                                        "server.acceptAcpRegistryUrlAuth",
                                        payload,
                                        None,
                                        window,
                                        cx,
                                        |_, _, result, window, cx| match result {
                                            Ok(reply) if reply["accepted"] == false => window.push_notification(
                                                Notification::warning(
                                                    "Refresh the provider and start the authentication flow again.",
                                                )
                                                .title("Authentication request expired"),
                                                cx,
                                            ),
                                            Err(error) => window.push_notification(
                                                Notification::error(error).title("Could not continue authentication"),
                                                cx,
                                            ),
                                            Ok(_) => {}
                                        },
                                    );
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    /// Accent color swatch and the display name field.
    fn render_display_name_control(
        &mut self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        instance: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity().downgrade();
        let accent_row = row.clone();
        let picker = accent_color_picker(
            SharedString::from(format!("provider-accent-{}", row.id)),
            &display_name(row, instance),
            accent_color(instance).as_deref(),
            move |color, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    let Some(row) = this.instance_row(&accent_row.id) else { return };
                    let next = with_key(&row.instance(), "accentColor", (!color.is_empty()).then(|| json!(color)));
                    this.persist_provider_instance(&row.id, next, window, cx);
                });
            },
            window,
            cx,
        );
        let name_row = row.clone();
        let input = self.draft_input(
            page,
            format!("display-name:{}", row.id),
            instance.get("displayName").and_then(Value::as_str).unwrap_or_default(),
            drivers::driver(&row.driver).map_or("Instance label", |d| d.label),
            false,
            window,
            cx,
            move |this, value, window, cx| {
                let trimmed = value.trim();
                let next = with_key(&name_row.instance(), "displayName", (!trimmed.is_empty()).then(|| json!(trimmed)));
                this.persist_provider_instance(&name_row.id, next, window, cx);
            },
        );
        h_flex().gap_2().justify_end().child(picker).child(text_input(&input, Some(224.))).into_any_element()
    }

    /// One `environmentFields` row, e.g. Cursor's API key, kept as a sensitive variable.
    fn render_environment_field(
        &mut self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        instance: &Value,
        field: &'static EnvironmentField,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let variable = environment(instance).into_iter().find(|v| v["name"] == field.name);
        let redacted = variable.as_ref().is_some_and(|v| v["valueRedacted"] == true);
        let value = variable.as_ref().filter(|_| !redacted).and_then(|v| v["value"].as_str()).unwrap_or_default();
        let placeholder = if redacted {
            "Stored secret, enter a new value to replace"
        } else {
            field.placeholder.unwrap_or("Not set")
        };
        let commit_row = row.clone();
        let input = self.draft_input(
            page,
            format!("env-field:{}:{}", row.id, field.name),
            value,
            placeholder,
            true,
            window,
            cx,
            move |this, value, window, cx| {
                let instance = commit_row.instance();
                let configured =
                    environment(&instance).iter().any(|v| v["name"] == field.name && v["valueRedacted"] == true);
                if configured && value.is_empty() {
                    return;
                }
                let next = with_environment(
                    &instance,
                    environment_with_field_value(&environment(&instance), field.name, &value),
                );
                this.persist_provider_instance(&commit_row.id, next, window, cx);
            },
        );
        let clear_row = row.clone();
        let control =
            h_flex().w(px(224.)).gap_2().child(text_input(&input, None).flex_1()).when(variable.is_some(), |el| {
                el.child(
                    ghost_icon_button(
                        SharedString::from(format!("env-field-clear-{}", field.name)),
                        gpui_kit::assets::IconName::X,
                        t,
                    )
                    .size(px(28.))
                    .tooltip(format!("Clear {}", field.label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let instance = clear_row.instance();
                        let rest = environment(&instance).into_iter().filter(|v| v["name"] != field.name).collect();
                        this.persist_provider_instance(&clear_row.id, with_environment(&instance, rest), window, cx);
                    })),
                )
            });
        settings_row(field.label, field.description, Some(control.into_any_element()), t).into_any_element()
    }

    /// The saved instance behind a list row, looked up again when a deferred edit commits.
    pub(super) fn instance_row(&self, instance_id: &str) -> Option<InstanceRow> {
        instance_rows(&self.settings_info().settings, &self.settings_info().providers_json)
            .into_iter()
            .find(|row| row.id == instance_id)
    }

    /// A managed Codex instance's Runtime section: folded, its header outside the card
    /// (`FoldedSettingsSection` with `headerPlacement="outside"`).
    fn render_runtime_fold(
        &self,
        page: &Entity<ProvidersPage>,
        instance_id: &str,
        fields: AnyElement,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = page.read(cx).runtime_open.contains(instance_id);
        let (page, id) = (page.clone(), instance_id.to_owned());
        v_flex()
            .w_full()
            .gap(px(10.))
            .child(
                h_flex().min_h(px(28.)).px_4().child(
                    h_flex()
                        .id(SharedString::from(format!("provider-instance-{instance_id}-runtime")))
                        .min_h(px(28.))
                        .gap_2()
                        .cursor_pointer()
                        .text_sm()
                        .text_color(t.foreground.opacity(0.7))
                        .child("Runtime")
                        .child(
                            Icon::new(if open {
                                gpui_kit::assets::IconName::ChevronDown
                            } else {
                                gpui_kit::assets::IconName::ChevronRight
                            })
                            .size(px(16.)),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            page.update(cx, |page, cx| {
                                if !page.runtime_open.remove(&id) {
                                    page.runtime_open.insert(id.clone());
                                }
                                cx.notify();
                            });
                        })),
                ),
            )
            .when(open, |el| el.child(group(divided([fields], t), false, t)))
            .into_any_element()
    }

    /// Generic variables (`ProviderEnvironmentSection`): rows of name, value and sensitivity.
    fn render_environment_variables(
        &mut self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        instance: &Value,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let dedicated: Vec<&str> =
            drivers::driver(&row.driver).map_or(vec![], |d| d.environment_fields.iter().map(|f| f.name).collect());
        let generic: Vec<Value> = environment(instance)
            .into_iter()
            .filter(|v| !dedicated.contains(&v["name"].as_str().unwrap_or_default()))
            .collect();
        page.update(cx, |page, _| page.environment.reconcile(&row.id, &generic));
        let rows = page.read(cx).environment.rows.clone();
        let add_page = page.clone();
        let add_button = outline_button("provider-env-add", "Add variable")
            .icon(Icon::new(gpui_kit::assets::IconName::Plus).size(px(12.)))
            .on_click(cx.listener(move |_, _, _, cx| {
                add_page.update(cx, |page, cx| {
                    page.environment.next_id += 1;
                    let id = page.environment.next_id;
                    page.environment.rows.push(EnvRow {
                        id,
                        name: String::new(),
                        value: json!(""),
                        sensitive: true,
                        redacted: None,
                    });
                    cx.notify();
                });
            }));
        let any_redacted = rows.iter().any(|r| r.redacted == Some(true));
        let editors = rows
            .iter()
            .enumerate()
            .map(|(index, env)| self.render_environment_variable(page, row, &dedicated, env, index, t, window, cx))
            .collect::<Vec<_>>();
        settings_row("Variables", Some("API keys, base URLs, and other per-instance CLI settings."), Some(add_button.into_any_element()), t)
            .flex_wrap()
            .when(!rows.is_empty(), |el| {
                el.child(
                    v_flex()
                        .w_full()
                        .mt_3()
                        .pb_2()
                        .gap_2()
                        .children(editors)
                        .child(div().text_xs().text_color(t.muted_foreground).child(if any_redacted {
                            "Sensitive values are stored separately and never returned to the app. Enter a new value to rename a saved one or make it plain text."
                        } else {
                            "Sensitive values are stored separately and never returned to the app."
                        })),
                )
            })
            .into_any_element()
    }

    fn render_environment_variable(
        &mut self,
        page: &Entity<ProvidersPage>,
        row: &InstanceRow,
        dedicated: &[&str],
        env: &EnvRow,
        index: usize,
        t: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = env.id;
        let dedicated: Vec<String> = dedicated.iter().map(|s| (*s).to_owned()).collect();
        let publish = move |this: &mut Workspace,
                            page: &Entity<ProvidersPage>,
                            instance_id: &str,
                            window: &mut Window,
                            cx: &mut Context<Workspace>| {
            let published = page.update(cx, |page, _| {
                let published = page.environment.published();
                if published.is_some() {
                    page.environment.last_published = published.clone();
                }
                published
            });
            let Some(published) = published else { return };
            let Some(row) = this.instance_row(instance_id) else { return };
            let instance = row.instance();
            let mut next: Vec<Value> = environment(&instance)
                .into_iter()
                .filter(|v| dedicated.contains(&v["name"].as_str().unwrap_or_default().to_owned()))
                .collect();
            next.extend(published);
            this.persist_provider_instance(&row.id, with_environment(&instance, next), window, cx);
        };
        let publish = std::rc::Rc::new(publish);
        let update = {
            let (page, instance_id, publish) = (page.clone(), row.id.clone(), publish.clone());
            move |this: &mut Workspace,
                  patch: &dyn Fn(&mut EnvRow),
                  window: &mut Window,
                  cx: &mut Context<Workspace>| {
                page.update(cx, |page, _| {
                    if let Some(row) = page.environment.rows.iter_mut().find(|r| r.id == id) {
                        patch(row);
                    }
                });
                publish(this, &page, &instance_id, window, cx);
            }
        };
        let update = std::rc::Rc::new(update);
        let name_input = {
            let update = update.clone();
            self.draft_input(
                page,
                format!("env:{}:{id}:name", row.id),
                &env.name,
                "VARIABLE_NAME",
                false,
                window,
                cx,
                move |this, name, window, cx| {
                    let name = name.trim().to_owned();
                    update(this, &move |row: &mut EnvRow| row.name = name.clone(), window, cx);
                },
            )
        };
        let value_text = if env.redacted == Some(true) { "" } else { env.value.as_str().unwrap_or_default() };
        let value_input = {
            let update = update.clone();
            let placeholder = if env.sensitive && (env.redacted == Some(true) || env.value.is_object()) {
                "Stored secret, enter a new value to replace"
            } else {
                "value"
            };
            self.draft_input(
                page,
                format!("env:{}:{id}:value", row.id),
                value_text,
                placeholder,
                env.sensitive,
                window,
                cx,
                move |this, value, window, cx| {
                    update(
                        this,
                        &move |row: &mut EnvRow| {
                            if row.sensitive
                                && (row.redacted == Some(true) || row.value.is_object())
                                && value.is_empty()
                            {
                                return;
                            }
                            row.value = json!(value);
                            row.redacted = Some(false);
                        },
                        window,
                        cx,
                    );
                },
            )
        };
        let label = if env.name.is_empty() { (index + 1).to_string() } else { env.name.clone() };
        let remove = {
            let (page, instance_id, publish) = (page.clone(), row.id.clone(), publish.clone());
            cx.listener(move |this, _, window, cx| {
                page.update(cx, |page, _| page.environment.rows.retain(|r| r.id != id));
                publish(this, &page, &instance_id, window, cx);
            })
        };
        let toggle = {
            let update = update.clone();
            cx.listener(move |this, _, window, cx| {
                update(
                    this,
                    &|row: &mut EnvRow| {
                        let sensitive = !row.sensitive;
                        row.sensitive = sensitive;
                        if !(sensitive && row.redacted.is_none()) {
                            row.redacted = if sensitive { row.redacted } else { Some(false) };
                        }
                    },
                    window,
                    cx,
                );
            })
        };
        let lock = if env.sensitive { gpui_kit::assets::IconName::Lock } else { gpui_kit::assets::IconName::LockOpen };
        v_flex()
            .min_w_0()
            .gap(px(6.))
            .rounded_lg()
            .border_1()
            .border_color(t.border)
            .p_2()
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(text_input(&name_input, None).flex_1().disabled(env.redacted == Some(true)))
                    .child(
                        ghost_icon_button(
                            SharedString::from(format!("env-remove-{id}")),
                            gpui_kit::assets::IconName::X,
                            t,
                        )
                        .size(px(20.))
                        .tooltip(format!("Remove environment variable {label}"))
                        .on_click(remove),
                    ),
            )
            .child(h_flex().gap(px(6.)).child(text_input(&value_input, None).flex_1()).when(
                env.value.is_string(),
                |el| {
                    el.child(
                        ghost_icon_button(SharedString::from(format!("env-sensitive-{id}")), lock, t)
                            .size(px(20.))
                            .disabled(env.redacted == Some(true))
                            .tooltip(if env.sensitive { "Sensitive, stored separately" } else { "Plain text" })
                            .on_click(toggle),
                    )
                },
            ))
            .into_any_element()
    }

    fn confirm_reset_instance(&mut self, row: &InstanceRow, window: &mut Window, cx: &mut Context<Self>) {
        let instance = row.instance();
        let name = display_name(row, &instance);
        let turns_off = resolve_enabled(&instance) && !resolve_enabled(&json!({ "driver": row.driver }));
        let message = format!(
            "Its saved settings, including any custom binary path, will be removed.{}",
            if turns_off { " It will also be turned off." } else { "" }
        );
        self.confirm_instance_removal(
            row.id.clone(),
            format!("Reset the {name} provider to its defaults?"),
            message,
            "Reset",
            None,
            window,
            cx,
        );
    }

    fn confirm_delete_instance(&mut self, row: &InstanceRow, window: &mut Window, cx: &mut Context<Self>) {
        let instance = row.instance();
        let name = display_name(row, &instance);
        // Registry agents T3 Code installed get their managed files removed too.
        let agent_id = (row.driver == "acpRegistry" && text(&instance, "/config/source") != Some("local"))
            .then(|| text(&instance, "/config/agentId").map(str::to_owned))
            .flatten();
        self.confirm_instance_removal(
            row.id.clone(),
            format!("Remove the {name} provider instance?"),
            "Its saved settings, including any custom binary path, will be removed.".into(),
            "Remove",
            agent_id,
            window,
            cx,
        );
    }

    /// `confirmProviderInstanceRemoval`, then the `remove` mutation.
    fn confirm_instance_removal(
        &mut self,
        instance_id: String,
        title: String,
        message: String,
        ok: &'static str,
        uninstall_agent: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let alert = alert.entrance(DialogEntrance::None);
            let (view, instance_id, uninstall_agent) = (view.clone(), instance_id.clone(), uninstall_agent.clone());
            alert
                .title(title.clone())
                .description(message.clone())
                .show_cancel(true)
                .ok_text(ok)
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let (instance_id, uninstall_agent) = (instance_id.clone(), uninstall_agent.clone());
                    let _ = view.update(cx, |this, cx| {
                        let payload = json!({
                            "patch": {},
                            "providerInstanceMutation": {"operation": "remove", "instanceId": instance_id},
                        });
                        let failure = if ok == "Reset" {
                            "Could not reset provider instance"
                        } else {
                            "Could not delete provider instance"
                        };
                        this.providers_request(
                            "server.updateSettings",
                            payload,
                            failure,
                            window,
                            cx,
                            move |this, environment, reply, window, cx| {
                                let event = json!({"type": "settingsUpdated", "payload": {"settings": reply}});
                                this.apply_environment_event(&environment, &event);
                                let Some(agent_id) = uninstall_agent else { return };
                                // The binary lives on the environment the instance was removed from.
                                this.providers_request_on(
                                    environment,
                                    "server.uninstallAcpRegistryManagedBinary",
                                    json!({ "agentId": agent_id }),
                                    None,
                                    window,
                                    cx,
                                    |_, _, result, window, cx| {
                                        if let Err(error) = result {
                                            window.push_notification(
                                                Notification::warning(error)
                                                    .title("Provider deleted, but managed files remain"),
                                                cx,
                                            );
                                        }
                                    },
                                );
                            },
                        );
                    });
                    true
                })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_follow_the_web_order() {
        let settings = json!({"providerInstances": {
            "codex_work": {"driver": "codex", "displayName": "Work"},
            "claudeAgent": {"driver": "claudeAgent", "enabled": false},
            "grok": {"driver": "grok", "enabled": true},
            "forked": {"driver": "someFork"},
        }});
        let rows = instance_rows(&settings, &[]);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        // Disabled-by-default drivers without config stay out; unknown drivers come last.
        assert_eq!(ids, ["codex", "codex_work", "claudeAgent", "grok", "forked"]);
        assert!(rows[0].is_default && !rows[0].is_dirty);
        assert!(rows[2].is_dirty && rows[2].explicit.is_some());
        assert!(!rows[1].is_default);
    }

    #[test]
    fn cursor_slot_shows_once_reported() {
        let settings = json!({"providerInstances": {}});
        let cursor = json!({"instanceId": "cursor", "driver": "cursor", "enabled": true});
        assert!(!instance_rows(&settings, &[]).iter().any(|r| r.id == "cursor"));
        let settings = json!({"providerInstances": {"cursor": {"driver": "cursor", "enabled": true}}});
        assert!(instance_rows(&settings, &[cursor]).iter().any(|r| r.id == "cursor"));
    }

    #[test]
    fn summaries() {
        let ready = json!({"enabled": true, "installed": true, "status": "ready", "auth": {"status": "authenticated", "type": "chatgpt"}});
        assert_eq!(summary(Some(&ready)).0, "Authenticated · chatgpt");
        assert_eq!(status_key(Some(&ready)), StatusKey::Ready);
        let missing = json!({"enabled": true, "installed": false, "status": "error", "auth": {"status": "unknown"}});
        assert_eq!(summary(Some(&missing)), ("Not found".into(), Some("CLI not detected on PATH.".into())));
        let agy = json!({"driver": "antigravity", "enabled": true, "installed": true, "status": "warning", "auth": {"status": "unknown"}});
        assert_eq!(status_key(Some(&agy)), StatusKey::Neutral);
        assert_eq!(summary(Some(&agy)).0, "Installed · Sign-in not checked");
        assert_eq!(summary(None).0, "Checking provider status");
    }

    #[test]
    fn version_labels() {
        assert_eq!(version_label(Some("1.2.3")).as_deref(), Some("v1.2.3"));
        assert_eq!(version_label(Some("nightly")).as_deref(), Some("nightly"));
        assert_eq!(version_label(Some("agy_acp_server_20260102_42")).as_deref(), Some("2026-01-02"));
        assert_eq!(version_label(Some("agy_acp_server_20260102_42_rc1")).as_deref(), Some("2026-01-02 rc1"));
        assert_eq!(version_label(None), None);
    }

    #[test]
    fn advisories() {
        let behind = json!({"versionAdvisory": {"status": "behind_latest", "latestVersion": "2.0.0", "updateCommand": "npm i -g x", "message": null}});
        let advisory = version_advisory(&behind, true).unwrap();
        assert_eq!(advisory.detail, "Update available: install v2.0.0.");
        assert_eq!(advisory.update_command.as_deref(), Some("npm i -g x"));
        let broken = json!({
            "versionAdvisory": {"status": "current", "updateCommand": "npm i -g x"},
            "compatibilityAdvisory": {"status": "broken", "message": null, "recommendedVersion": null, "recommendedRange": ">=2"},
        });
        let advisory = version_advisory(&broken, true).unwrap();
        assert_eq!(
            (advisory.title.as_str(), advisory.detail.as_str(), advisory.strong),
            ("Known broken version", "Use >=2 for full support.", true)
        );
        assert_eq!(advisory.update_command, None);
        assert_eq!(version_advisory(&json!({"versionAdvisory": {"status": "current"}}), true), None);
    }

    #[test]
    fn update_all_needs_one_command_per_driver() {
        let candidate = |id: &str, command: &str| {
            json!({"instanceId": id, "driver": "codex", "enabled": true, "checkedAt": "2026-01-01T00:00:00Z",
                "versionAdvisory": {"status": "behind_latest", "latestVersion": "2", "canUpdate": true, "updateCommand": command}})
        };
        let same = [candidate("codex", "a"), candidate("codex_2", "a")];
        assert_eq!(one_click_update_candidates(&same), [("codex".to_owned(), "codex".to_owned())]);
        let split = [candidate("codex", "a"), candidate("codex_2", "b")];
        assert!(one_click_update_candidates(&split).is_empty());
    }

    #[test]
    fn environment_field_values() {
        let env = vec![
            json!({"name": "A", "value": "1", "sensitive": false}),
            json!({"name": "KEY", "value": "old", "sensitive": true}),
        ];
        assert_eq!(
            environment_with_field_value(&env, "KEY", " new "),
            [env[0].clone(), json!({"name": "KEY", "value": "new", "sensitive": true})]
        );
        assert_eq!(environment_with_field_value(&env, "KEY", ""), [env[0].clone()]);
        assert_eq!(environment_with_field_value(&[], "KEY", "v").len(), 1);
    }

    #[test]
    fn environment_draft_publishes_valid_rows() {
        let mut draft = EnvironmentDraft::default();
        draft.reconcile("codex", &[json!({"name": "A", "value": "1", "sensitive": false})]);
        draft.rows.push(EnvRow { id: 99, name: String::new(), value: json!(""), sensitive: true, redacted: None });
        assert_eq!(draft.published(), Some(vec![json!({"name": "A", "value": "1", "sensitive": false})]));
        draft.rows[1].name = "1bad".into();
        assert_eq!(draft.published(), None);
        assert!(is_variable_name("_OK1") && !is_variable_name("") && !is_variable_name("A-B"));
    }

    #[test]
    fn environment_draft_keeps_secret_references() {
        let secret = json!({"provider": "onepassword", "reference": "op://Private/item/field"});
        let mut draft = EnvironmentDraft::default();
        draft.reconcile("codex", &[json!({"name": "API_KEY", "value": "", "sensitive": false, "secret": secret})]);
        assert_eq!((&draft.rows[0].value, draft.rows[0].sensitive), (&secret, true));
        assert_eq!(
            draft.published(),
            Some(vec![json!({"name": "API_KEY", "value": "", "secret": secret, "sensitive": true})])
        );
    }

    #[test]
    fn email_redaction_keeps_shape() {
        let redacted = redacted_placeholder("me@example.com");
        assert_eq!(redacted.len(), "me@example.com".len());
        assert_eq!(redacted.find('@'), Some(2));
        assert_eq!(redacted, redacted_placeholder("me@example.com"));
        assert_ne!(redacted, "me@example.com");
    }

    #[test]
    fn initials_and_accents() {
        assert_eq!(initials("Codex (hub)"), "CH");
        assert_eq!(initials("Claude"), "CL");
        assert_eq!(initials("Theo's Codex"), "TC");
        assert_eq!(accent_color(&json!({"accentColor": "#2563eb"})).as_deref(), Some("#2563eb"));
        assert_eq!(accent_color(&json!({"accentColor": "blue"})), None);
    }
}
