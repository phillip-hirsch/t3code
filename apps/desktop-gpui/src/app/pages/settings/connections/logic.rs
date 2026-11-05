//! Pure logic behind the connections sections: auth access records and scopes
//! (`ConnectionsSettings.logic.ts`, `authScopeOptions.ts`), secret provider drafts
//! (`SecretProviderDialog.logic.ts`), labels and timestamps.

use serde_json::Value;

use crate::timestamp::parse_ms;

pub(super) const ORCHESTRATION_READ: &str = "orchestration:read";
pub(in crate::app::pages::settings) const ORCHESTRATION_OPERATE: &str = "orchestration:operate";
pub(in crate::app::pages::settings) const SETTINGS_WRITE: &str = "settings:write";
pub(super) const PROVIDERS_MANAGE: &str = "providers:manage";
pub(in crate::app::pages::settings) const ENVIRONMENT_MAINTAIN: &str = "environment:maintain";
const PREVIEW_OPERATE: &str = "preview:operate";
pub(in crate::app::pages::settings) const DIAGNOSTICS_READ: &str = "diagnostics:read";
pub(super) const TERMINAL_READ: &str = "terminal:read";
const TERMINAL_OPERATE: &str = "terminal:operate";
const SOURCE_CONTROL_WRITE: &str = "source-control:write";
pub(super) const FILESYSTEM_READ: &str = "filesystem:read";
const FILESYSTEM_WRITE: &str = "filesystem:write";
pub(super) const ACCESS_READ: &str = "access:read";
pub(super) const ACCESS_WRITE: &str = "access:write";
const RELAY_READ: &str = "relay:read";
const RELAY_WRITE: &str = "relay:write";

/// `AUTH_SCOPE_OPTIONS` in `packages/shared/src/authScopeOptions.ts`: scope, title, description.
pub(super) const SCOPE_OPTIONS: &[(&str, &str, &str)] = &[
    (ORCHESTRATION_READ, "View environment", "Read threads, status, checkpoints, and configuration."),
    (ORCHESTRATION_OPERATE, "Operate tasks", "Start, update, and stop tasks."),
    (SETTINGS_WRITE, "Change environment settings", "Edit environment preferences and keybindings."),
    (PROVIDERS_MANAGE, "Manage providers", "Configure, install, sign in to, and update providers and usage sources."),
    (ENVIRONMENT_MAINTAIN, "Maintain environment", "Update the server and control environment processes."),
    (PREVIEW_OPERATE, "Control previews", "Open browser previews and host browser automation."),
    (DIAGNOSTICS_READ, "View diagnostics and usage", "Read process diagnostics, resource history, and usage totals."),
    (TERMINAL_READ, "View terminals", "Read existing terminal output and status."),
    (TERMINAL_OPERATE, "Use terminals", "Create terminals and send input to running shells."),
    (
        SOURCE_CONTROL_WRITE,
        "Change source control",
        "Commit, push, manage branches and repositories, and change pull requests.",
    ),
    (FILESYSTEM_READ, "Read files", "Browse host files, search workspaces, and inspect local changes."),
    (FILESYSTEM_WRITE, "Write files", "Edit workspace files and save plans to disk."),
    (ACCESS_READ, "View access", "Inspect pairing links and authorized clients."),
    (ACCESS_WRITE, "Manage access", "Issue and revoke credentials for other clients."),
    (RELAY_READ, "View relay", "Inspect managed relay connectivity."),
    (RELAY_WRITE, "Manage relay", "Change managed tunnel connectivity."),
];

/// `AuthStandardClientScopes` in `packages/contracts/src/auth.ts`.
pub(super) const STANDARD_SCOPES: &[&str] = &[
    ORCHESTRATION_READ,
    ORCHESTRATION_OPERATE,
    SETTINGS_WRITE,
    PROVIDERS_MANAGE,
    ENVIRONMENT_MAINTAIN,
    PREVIEW_OPERATE,
    DIAGNOSTICS_READ,
    TERMINAL_READ,
    TERMINAL_OPERATE,
    SOURCE_CONTROL_WRITE,
    FILESYSTEM_READ,
    FILESYSTEM_WRITE,
    RELAY_READ,
    ACCESS_READ,
    ACCESS_WRITE,
    RELAY_WRITE,
];

/// The "Read only" preset of the create-link dialog.
pub(super) const READ_ONLY_SCOPES: &[&str] = &[ORCHESTRATION_READ, FILESYSTEM_READ, DIAGNOSTICS_READ, TERMINAL_READ];

/// Older servers grant these through a broader parent scope (`legacyParents` in `auth.ts`).
fn legacy_parent(scope: &str) -> Option<&'static str> {
    Some(match scope {
        FILESYSTEM_READ | DIAGNOSTICS_READ => ORCHESTRATION_READ,
        SETTINGS_WRITE | PROVIDERS_MANAGE | ENVIRONMENT_MAINTAIN | PREVIEW_OPERATE | SOURCE_CONTROL_WRITE
        | FILESYSTEM_WRITE => ORCHESTRATION_OPERATE,
        TERMINAL_READ => TERMINAL_OPERATE,
        _ => return None,
    })
}

/// `GET /api/auth/session` (`AuthSessionState`).
#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::app::pages::settings) struct Session {
    pub authenticated: bool,
    /// `auth.policy`, e.g. "loopback-browser" or "remote-reachable".
    pub policy: Option<String>,
    scopes: Option<Vec<String>>,
    permissions: Option<Vec<String>>,
    server_update_scope: Option<String>,
}

impl Session {
    pub fn from_json(value: &Value) -> Self {
        Self {
            authenticated: value["authenticated"].as_bool() == Some(true),
            policy: text(&value["auth"]["policy"]),
            scopes: strings(&value["scopes"]),
            permissions: strings(&value["permissions"]),
            server_update_scope: text(&value["auth"]["serverUpdateScope"]),
        }
    }

    /// `sessionGrantsScope`.
    pub fn grants(&self, scope: &str) -> bool {
        if !self.authenticated {
            return false;
        }
        if let Some(permissions) = &self.permissions {
            return permissions.iter().any(|p| p == scope);
        }
        let has = |s: &str| self.scopes.as_ref().is_some_and(|scopes| scopes.iter().any(|x| x == s));
        if has(scope) {
            return true;
        }
        if self.server_update_scope.is_some() {
            return false;
        }
        legacy_parent(scope).is_some_and(has)
    }

    /// The scopes a pairing link from this session may carry (`permissions ?? scopes`).
    pub fn delegatable(&self) -> &[String] {
        self.permissions.as_deref().or(self.scopes.as_deref()).unwrap_or_default()
    }
}

/// `togglePairingScopeSelection`: terminal:operate keeps terminal:read with it.
pub(super) fn toggle_scope(current: &[String], scope: &str, checked: bool) -> Vec<String> {
    let mut next: Vec<String> = current.iter().filter(|s| *s != scope).cloned().collect();
    if !checked {
        if scope == TERMINAL_READ {
            next.retain(|s| s != TERMINAL_OPERATE);
        }
        return next;
    }
    next.push(scope.to_owned());
    if scope == TERMINAL_OPERATE && !next.iter().any(|s| s == TERMINAL_READ) {
        next.push(TERMINAL_READ.to_owned());
    }
    next
}

/// `sessionPermissionsUnavailableMessage`.
pub(super) fn permissions_unavailable_message(connected: bool, failed: bool, rejected: bool) -> &'static str {
    if !connected {
        "Connect to this environment to view this session\u{2019}s permissions."
    } else if rejected {
        "This environment did not accept your session. Reconnect to try again."
    } else if failed {
        "Could not verify your permissions. Retrying automatically."
    } else {
        "Checking permissions\u{2026}"
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct PairingLink {
    pub id: String,
    pub label: Option<String>,
    pub scopes: Vec<String>,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ClientSession {
    pub session_id: String,
    pub subject: String,
    pub label: Option<String>,
    pub device_type: String,
    pub os: Option<String>,
    pub browser: Option<String>,
    pub ip_address: Option<String>,
    pub scopes: Vec<String>,
    pub issued_at: String,
    pub last_connected_at: Option<String>,
    pub connected: bool,
    pub current: bool,
}

impl PairingLink {
    fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            id: text(&value["id"])?,
            label: text(&value["label"]),
            scopes: granted(value),
            created_at: text(&value["createdAt"]).unwrap_or_default(),
            expires_at: text(&value["expiresAt"]).unwrap_or_default(),
        })
    }
}

impl ClientSession {
    fn from_json(value: &Value) -> Option<Self> {
        let client = &value["client"];
        Some(Self {
            session_id: text(&value["sessionId"])?,
            subject: text(&value["subject"]).unwrap_or_default(),
            label: text(&client["label"]),
            device_type: text(&client["deviceType"]).unwrap_or_else(|| "unknown".into()),
            os: text(&client["os"]),
            browser: text(&client["browser"]),
            ip_address: text(&client["ipAddress"]),
            scopes: granted(value),
            issued_at: text(&value["issuedAt"]).unwrap_or_default(),
            last_connected_at: text(&value["lastConnectedAt"]),
            connected: value["connected"].as_bool() == Some(true),
            current: value["current"].as_bool() == Some(true),
        })
    }

    /// The row title: the client's label, else "os · browser", else its subject.
    pub fn title(&self) -> String {
        if let Some(label) = &self.label {
            return label.clone();
        }
        let joined = [&self.os, &self.browser].into_iter().flatten().cloned().collect::<Vec<_>>().join(" · ");
        if joined.is_empty() { self.subject.clone() } else { joined }
    }

    /// Device type (capitalized), OS, browser and IP address, as known.
    pub fn device_bits(&self) -> Vec<String> {
        let device = (self.device_type != "unknown").then(|| capitalize(&self.device_type));
        [device, self.os.clone(), self.browser.clone(), self.ip_address.clone()].into_iter().flatten().collect()
    }
}

/// `AuthAccessSnapshot` as `subscribeAuthAccess` keeps it current.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct AccessSnapshot {
    pub pairing_links: Vec<PairingLink>,
    pub client_sessions: Vec<ClientSession>,
}

impl AccessSnapshot {
    /// `applyAuthAccessStreamEvent`.
    pub fn apply(&mut self, event: &Value) {
        let payload = &event["payload"];
        match event["type"].as_str().unwrap_or_default() {
            "snapshot" => {
                let list = |key: &str| payload[key].as_array().cloned().unwrap_or_default();
                self.pairing_links = list("pairingLinks").iter().filter_map(PairingLink::from_json).collect();
                self.client_sessions = list("clientSessions").iter().filter_map(ClientSession::from_json).collect();
            }
            "pairingLinkUpserted" => {
                if let Some(link) = PairingLink::from_json(payload) {
                    self.pairing_links.retain(|l| l.id != link.id);
                    self.pairing_links.push(link);
                }
            }
            "pairingLinkRemoved" => {
                let id = payload["id"].as_str();
                self.pairing_links.retain(|l| Some(l.id.as_str()) != id);
            }
            "clientUpserted" => {
                if let Some(session) = ClientSession::from_json(payload) {
                    self.client_sessions.retain(|s| s.session_id != session.session_id);
                    self.client_sessions.push(session);
                }
            }
            "clientRemoved" => {
                let id = payload["sessionId"].as_str();
                self.client_sessions.retain(|s| Some(s.session_id.as_str()) != id);
            }
            _ => {}
        }
    }

    /// Newest first (`sortDesktopPairingLinks`).
    pub fn sorted_links(&self) -> Vec<&PairingLink> {
        let mut links: Vec<_> = self.pairing_links.iter().collect();
        links.sort_by_cached_key(|l| std::cmp::Reverse(ms(&l.created_at)));
        links
    }

    /// This device, then connected ones, then newest (`sortDesktopClientSessions`).
    pub fn sorted_sessions(&self) -> Vec<&ClientSession> {
        let mut sessions: Vec<_> = self.client_sessions.iter().collect();
        sessions.sort_by_cached_key(|s| (!s.current, !s.connected, std::cmp::Reverse(ms(&s.issued_at))));
        sessions
    }

    /// `summarizeAuthorizedClients`.
    pub fn summary(&self) -> String {
        let sessions = self.client_sessions.len();
        let links = self.pairing_links.len();
        let mut summary = format!("{sessions} {}", if sessions == 1 { "client" } else { "clients" });
        if links > 0 {
            summary.push_str(&format!(" · {links} {}", if links == 1 { "pairing link" } else { "pairing links" }));
        }
        summary
    }

    /// `canRevokeOtherClients`.
    pub fn has_other_clients(&self) -> bool {
        self.client_sessions.iter().any(|s| !s.current)
    }
}

/// `formatExpiresInLabel` in `apps/web/src/timestampFormat.ts`.
pub(super) fn expires_in_label(expires_at_ms: i64, now_ms: i64) -> String {
    let diff = expires_at_ms - now_ms;
    if diff <= 0 {
        return "Expired".into();
    }
    let total = diff / 1000;
    if total < 5 {
        return "Expires in a moment".into();
    }
    if total < 60 {
        return format!("Expires in {total}s");
    }
    let (days, hours, minutes, seconds) = (total / 86_400, total % 86_400 / 3600, total % 3600 / 60, total % 60);
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    // Under an hour the minutes always show; above, only the non-zero units after the first.
    if hours > 0 || (days == 0 && total >= 3600) {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 || total < 3600 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 {
        parts.push(format!("{seconds}s"));
    }
    format!("Expires in {}", parts.join(" "))
}

/// `formatElapsedDurationLabel`.
pub(super) fn elapsed_label(since_ms: i64, now_ms: i64) -> String {
    let seconds = (now_ms - since_ms) / 1000;
    match seconds {
        ..5 => "just now".into(),
        5..60 => format!("{seconds}s"),
        60..3600 => format!("{}m", seconds / 60),
        3600..86_400 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// `formatAccessTimestamp` (`dateStyle: medium, timeStyle: short`) in this machine's zone.
pub(super) fn format_timestamp(iso: &str) -> String {
    format_timestamp_in(iso, &chrono::Local)
}

fn format_timestamp_in<Tz: chrono::TimeZone>(iso: &str, zone: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|at| at.with_timezone(zone).format("%b %-d, %Y, %-I:%M %p").to_string())
        .unwrap_or_else(|_| iso.to_owned())
}

pub(super) fn ms(iso: &str) -> i64 {
    parse_ms(iso).unwrap_or(0)
}

/// `ENVIRONMENT_MACHINE_KIND_LABELS`, in `ENVIRONMENT_MACHINE_KINDS` order.
pub(super) const MACHINE_KINDS: &[(&str, &str)] = &[
    ("server", "Server"),
    ("cloud", "Cloud VM"),
    ("linux", "Linux/WSL"),
    ("desktop", "Desktop"),
    ("laptop", "Laptop"),
    ("mac-mini", "Mini PC"),
    ("mac-studio", "Workstation"),
];

/// `EnvironmentMachineIcon`'s glyph for a machine kind.
pub(super) fn machine_icon(kind: &str) -> &'static str {
    match kind {
        "cloud" => "icons/cloud.svg",
        "linux" => "icons/app/linux.svg",
        "desktop" => "icons/monitor.svg",
        "laptop" => "icons/laptop.svg",
        "mac-mini" => "icons/app/mac-mini.svg",
        "mac-studio" => "icons/app/mac-studio.svg",
        _ => "icons/server.svg",
    }
}

/// `managedCapabilityDenial(policy, "remoteClients")`.
pub(super) fn remote_clients_denial(policy: &Value) -> Option<String> {
    let organization = policy["organization"].as_str()?;
    let clients = policy["limits"]["clients"].as_array()?;
    (!clients.iter().any(|c| c == "remote"))
        .then(|| format!("{organization}'s policy does not allow connecting from other devices."))
}

// ---------------------------------------------------------------------------
// Secret providers

/// Name and account typed into the 1Password provider dialog.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SecretProviderDraft {
    pub name: String,
    pub account: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct SecretProviderIssues {
    pub name: Option<String>,
    pub account: Option<String>,
}

/// The entry a valid draft writes: its id, label and 1Password account.
#[derive(Debug, PartialEq)]
pub(super) struct SecretProviderEntry {
    pub id: String,
    pub label: String,
    pub account: String,
}

/// The account of a `one_password` provider whose config still decodes, else `None`
/// (`classifySecretProvider`: such rows are read-only, removal stays available).
pub(super) fn one_password_account(provider: &Value) -> Option<String> {
    if provider["kind"] != "one_password" {
        return None;
    }
    let account = provider["config"]["account"].as_str()?;
    valid_account(account).then(|| account.to_owned())
}

/// `OnePasswordAccount`: at most 253 characters, `^[A-Za-z0-9][A-Za-z0-9._-]*$`.
fn valid_account(account: &str) -> bool {
    let mut chars = account.chars();
    account.len() <= 253
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `secretProviderIdFromName`: a stable, readable slug, so re-adding a provider
/// under the same name restores its variables.
fn secret_provider_id(name: &str) -> Option<String> {
    let mut slug = String::new();
    for c in name.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(64).collect();
    let slug = slug.trim_end_matches('-');
    (!slug.is_empty()).then(|| slug.to_owned())
}

/// `secretProviderFromDraft`: the entry to write, or one message per blocking field.
/// `editing` is the id being edited; adding checks the new id against `existing_ids`.
pub(super) fn secret_provider_from_draft(
    draft: &SecretProviderDraft,
    existing_ids: &[String],
    editing: Option<&str>,
) -> Result<SecretProviderEntry, SecretProviderIssues> {
    let label = draft.name.trim();
    let account = draft.account.trim();
    let mut issues = SecretProviderIssues::default();
    let mut id = None;
    if label.is_empty() {
        issues.name = Some("Enter a name.".into());
    } else {
        id = editing.map(str::to_owned).or_else(|| secret_provider_id(label));
        match &id {
            None => issues.name = Some("The name needs at least one letter or digit.".into()),
            Some(id) if editing.is_none() && existing_ids.contains(id) => {
                issues.name = Some("A secret provider with this name exists.".into())
            }
            _ => {}
        }
    }
    if account.is_empty() {
        issues.account = Some("Enter the 1Password account.".into());
    } else if !valid_account(account) {
        issues.account = Some("1Password account must be a shorthand, sign-in address, or ID with no spaces.".into());
    }
    match id {
        Some(id) if issues == SecretProviderIssues::default() => {
            Ok(SecretProviderEntry { id, label: label.to_owned(), account: account.to_owned() })
        }
        _ => Err(issues),
    }
}

/// `redactedPlaceholder` in `RedactedSensitiveText.tsx`: stable look-alike text that keeps
/// `@ . - _` so the shape of an address survives.
pub(super) fn redacted_placeholder(value: &str) -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut state: u32 = 0x811c_9dc5;
    for unit in value.encode_utf16() {
        state ^= u32::from(unit);
        state = state.wrapping_mul(0x0100_0193);
    }
    value
        .chars()
        .map(|c| {
            if matches!(c, '@' | '.' | '-' | '_') {
                return c;
            }
            state = (state ^ (state >> 13)).wrapping_mul(0x85eb_ca6b);
            state = (state ^ (state >> 16)).wrapping_mul(0xc2b2_ae35);
            let index = i64::from(state as i32).unsigned_abs() as usize % ALPHABET.len();
            ALPHABET[index] as char
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Linked environments

/// `accessConfig` labels for `AuthMcpClientAccess`, in literal order: value, label, description.
pub(super) const LINK_ACCESS: &[(&str, &str, &str)] = &[
    ("read-only", "Read only", "Read projects and threads. Cannot start, message or change anything."),
    ("approval-required", "Supervised", "Ask before commands and file changes."),
    ("auto-accept-edits", "Auto-accept edits", "Auto-approve edits, ask before other actions."),
    ("auto", "Auto", "Supported providers approve routine actions; others still ask."),
    ("full-access", "Full access", "Allow commands and edits without prompts."),
];

pub(super) fn link_access(value: &str) -> (&'static str, &'static str) {
    LINK_ACCESS
        .iter()
        .find(|(v, ..)| *v == value)
        .map_or(("Unknown", ""), |(_, label, description)| (label, description))
}

/// How a linked environment's dot is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinkTone {
    Reachable,
    Expiring,
    Broken,
    Idle,
}

const EXPIRY_WARNING_DAYS: i64 = 5;

/// `peerLinkHealth` and the row's status line.
pub(super) fn link_health(link: &Value, now_ms: i64) -> (LinkTone, String) {
    let expires = link["expiresAt"].as_str().map_or(0, ms);
    let last_error = link["lastError"].as_str();
    if link["status"] == "expired" || expires <= now_ms {
        return (LinkTone::Broken, "Expired. Link it again with a new pairing code.".into());
    }
    if link["status"] == "unreachable" {
        return (LinkTone::Idle, last_error.unwrap_or("Not answering right now.").to_owned());
    }
    if let Some(error) = last_error {
        return (LinkTone::Broken, error.to_owned());
    }
    let days = (expires - now_ms + 86_399_999) / 86_400_000;
    if days <= EXPIRY_WARNING_DAYS {
        let unit = if days == 1 { "day" } else { "days" };
        return (LinkTone::Expiring, format!("Expires in {days} {unit}. Link it again to renew."));
    }
    (LinkTone::Reachable, "Reachable".into())
}

// ---------------------------------------------------------------------------

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|items| items.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect())
}

/// `permissions ?? scopes` of a link or session.
fn granted(value: &Value) -> Vec<String> {
    strings(&value["permissions"]).or_else(|| strings(&value["scopes"])).unwrap_or_default()
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn session_grants_follow_permissions_then_scopes_then_legacy_parents() {
        let modern = Session::from_json(&json!({
            "authenticated": true, "auth": {"policy": "loopback-browser"},
            "scopes": ["orchestration:operate"], "permissions": ["access:read"],
        }));
        assert!(modern.grants("access:read") && !modern.grants("settings:write"));
        let legacy =
            Session::from_json(&json!({"authenticated": true, "auth": {}, "scopes": ["orchestration:operate"]}));
        assert!(legacy.grants("settings:write"), "a legacy parent grants its children");
        let scoped = Session::from_json(&json!({
            "authenticated": true, "auth": {"serverUpdateScope": "environment:maintain"},
            "scopes": ["orchestration:operate"],
        }));
        assert!(!scoped.grants("settings:write"), "servers that split scopes grant them by name only");
        assert!(
            !Session::from_json(&json!({"authenticated": false, "permissions": ["access:read"]})).grants("access:read")
        );
    }

    #[test]
    fn toggling_terminal_scopes_keeps_read_with_operate() {
        let scopes = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(toggle_scope(&[], TERMINAL_OPERATE, true), scopes(&[TERMINAL_OPERATE, TERMINAL_READ]));
        assert_eq!(
            toggle_scope(&scopes(&[TERMINAL_OPERATE, TERMINAL_READ, ACCESS_READ]), TERMINAL_READ, false),
            scopes(&[ACCESS_READ])
        );
        assert_eq!(toggle_scope(&scopes(&[ACCESS_READ]), ACCESS_READ, false), scopes(&[]));
    }

    #[test]
    fn access_events_keep_the_snapshot_and_sort_it_like_the_web() {
        let mut access = AccessSnapshot::default();
        let session = |id: &str, current: bool, connected: bool, issued: &str| {
            json!({"sessionId": id, "subject": "s", "client": {"deviceType": "desktop", "os": "Windows"},
                "permissions": ["access:read"], "issuedAt": issued, "lastConnectedAt": null,
                "connected": connected, "current": current})
        };
        access.apply(&json!({"type": "snapshot", "payload": {
            "pairingLinks": [
                {"id": "old", "scopes": [], "subject": "x", "createdAt": "2026-01-01T00:00:00.000Z", "expiresAt": "2026-01-02T00:00:00.000Z"},
                {"id": "new", "scopes": [], "subject": "x", "createdAt": "2026-01-03T00:00:00.000Z", "expiresAt": "2026-01-04T00:00:00.000Z"},
            ],
            "clientSessions": [
                session("idle", false, false, "2026-01-05T00:00:00.000Z"),
                session("me", true, true, "2026-01-01T00:00:00.000Z"),
                session("live", false, true, "2026-01-02T00:00:00.000Z"),
            ],
        }}));
        assert_eq!(access.sorted_links().iter().map(|l| l.id.as_str()).collect::<Vec<_>>(), ["new", "old"]);
        assert_eq!(
            access.sorted_sessions().iter().map(|s| s.session_id.as_str()).collect::<Vec<_>>(),
            ["me", "live", "idle"]
        );
        assert_eq!(access.summary(), "3 clients · 2 pairing links");
        access.apply(&json!({"type": "pairingLinkRemoved", "payload": {"id": "old"}}));
        access.apply(&json!({"type": "clientRemoved", "payload": {"sessionId": "idle"}}));
        access.apply(&json!({"type": "clientRemoved", "payload": {"sessionId": "live"}}));
        assert_eq!(access.summary(), "1 client · 1 pairing link");
        assert!(!access.has_other_clients());
        assert_eq!(access.client_sessions[0].device_bits(), ["Desktop", "Windows"]);
        assert_eq!(access.client_sessions[0].title(), "Windows");
    }

    #[test]
    fn expiry_and_elapsed_labels_match_the_web_formatters() {
        let s = 1000;
        assert_eq!(expires_in_label(0, 0), "Expired");
        assert_eq!(expires_in_label(3 * s, 0), "Expires in a moment");
        assert_eq!(expires_in_label(42 * s, 0), "Expires in 42s");
        assert_eq!(expires_in_label(300 * s, 0), "Expires in 5m");
        assert_eq!(expires_in_label(301 * s, 0), "Expires in 5m 1s");
        assert_eq!(expires_in_label(3600 * s, 0), "Expires in 1h");
        assert_eq!(expires_in_label(3661 * s, 0), "Expires in 1h 1m 1s");
        assert_eq!(expires_in_label(86_400 * s, 0), "Expires in 1d");
        assert_eq!(expires_in_label((86_400 + 60) * s, 0), "Expires in 1d 1m");
        assert_eq!(elapsed_label(0, 90 * s), "1m");
        assert_eq!(elapsed_label(0, 2 * s), "just now");
    }

    #[test]
    fn timestamps_format_as_medium_date_and_short_time() {
        assert_eq!(format_timestamp_in("2026-11-05T15:04:00.000Z", &chrono::Utc), "Nov 5, 2026, 3:04 PM");
        assert_eq!(format_timestamp_in("2024-02-29T00:30:00Z", &chrono::Utc), "Feb 29, 2024, 12:30 AM");
        let tokyo = chrono::FixedOffset::east_opt(9 * 3600).unwrap();
        assert_eq!(format_timestamp_in("2024-02-29T20:30:00Z", &tokyo), "Mar 1, 2024, 5:30 AM");
        assert_eq!(format_timestamp("nonsense"), "nonsense");
    }

    #[test]
    fn secret_provider_drafts_validate_like_the_dialog() {
        let draft = |name: &str, account: &str| SecretProviderDraft { name: name.into(), account: account.into() };
        assert_eq!(
            secret_provider_from_draft(&draft(" Work Vault! ", "my.1password.com"), &[], None),
            Ok(SecretProviderEntry {
                id: "work-vault".into(),
                label: "Work Vault!".into(),
                account: "my.1password.com".into()
            })
        );
        let taken = secret_provider_from_draft(&draft("Work vault", "a"), &["work-vault".into()], None).unwrap_err();
        assert_eq!(taken.name.as_deref(), Some("A secret provider with this name exists."));
        let edit = secret_provider_from_draft(&draft("Renamed", "a"), &["work-vault".into()], Some("work-vault"));
        assert_eq!(edit.map(|e| e.id), Ok("work-vault".into()), "editing keeps the id");
        let issues = secret_provider_from_draft(&draft("!!!", "-bad"), &[], None).unwrap_err();
        assert_eq!(issues.name.as_deref(), Some("The name needs at least one letter or digit."));
        assert!(issues.account.is_some());
        let empty = secret_provider_from_draft(&draft("", ""), &[], None).unwrap_err();
        assert_eq!(
            (empty.name.as_deref(), empty.account.as_deref()),
            (Some("Enter a name."), Some("Enter the 1Password account."))
        );
    }

    #[test]
    fn only_decodable_one_password_configs_classify() {
        assert_eq!(
            one_password_account(&json!({"kind": "one_password", "config": {"account": "my.1password.com"}})),
            Some("my.1password.com".into())
        );
        assert_eq!(one_password_account(&json!({"kind": "one_password", "config": {"account": "has space"}})), None);
        assert_eq!(one_password_account(&json!({"kind": "vault", "config": {}})), None);
    }

    #[test]
    fn redacted_placeholders_keep_separators_and_are_stable() {
        // The web's output for the same input, so a reload shows the same placeholder.
        let redacted = redacted_placeholder("me@my.1password.com");
        assert_eq!(redacted, "sa@wj.fs2bvgbcz.3n8");
        assert_ne!(redacted, redacted_placeholder("me@my.1password.co"));
    }

    #[test]
    fn link_health_reads_expiry_status_and_errors() {
        let day = 86_400_000;
        let link = |status: &str, expires: &str, error: Value| json!({"status": status, "expiresAt": expires, "lastError": error});
        let now = ms("2026-01-01T00:00:00.000Z");
        assert_eq!(
            link_health(&link("reachable", "2026-02-01T00:00:00.000Z", Value::Null), now).0,
            LinkTone::Reachable
        );
        assert_eq!(
            link_health(&link("reachable", "2026-01-03T00:00:00.000Z", Value::Null), now).1,
            "Expires in 2 days. Link it again to renew."
        );
        assert_eq!(link_health(&link("reachable", "2025-12-01T00:00:00.000Z", Value::Null), now).0, LinkTone::Broken);
        assert_eq!(
            link_health(&link("unreachable", "2026-02-01T00:00:00.000Z", json!("down")), now),
            (LinkTone::Idle, "down".into())
        );
        assert_eq!(link_health(&link("reachable", "2026-02-01T00:00:00.000Z", json!("401")), now).0, LinkTone::Broken);
        assert_eq!(now + day, ms("2026-01-02T00:00:00.000Z"));
    }

    #[test]
    fn managed_policies_deny_remote_clients_only_when_they_list_clients_without_remote() {
        assert_eq!(remote_clients_denial(&Value::Null), None);
        assert_eq!(remote_clients_denial(&json!({"organization": "Acme", "limits": {"clients": ["remote"]}})), None);
        assert_eq!(
            remote_clients_denial(&json!({"organization": "Acme", "limits": {"clients": ["desktop"]}})).as_deref(),
            Some("Acme's policy does not allow connecting from other devices.")
        );
    }
}
