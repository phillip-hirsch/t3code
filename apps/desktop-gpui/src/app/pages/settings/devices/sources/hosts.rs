//! Device host settings and device state, ported from the web's
//! `deviceHostsSettings.logic.ts`, `easDeviceHostsSettings.logic.ts`,
//! `simlockDeviceHostsSettings.logic.ts`, `deviceHostConnectionChecks.ts` and the
//! `hostStatus` helper in `DeviceSourcesSettings.tsx`. Validation mirrors the
//! `SshDeviceHostConfig`, `EasDeviceHostConfig` and `SimlockDeviceHostConfig` schemas
//! in `packages/contracts/src/device.ts`.
//!
//! GPUI talks to one environment, so the web's per-environment plans reduce to one host
//! list, and `groupDeviceHosts` (which only merges lists across environments) has no port.

use crate::device::{DeviceState, HostStatus};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

fn ssh_kind() -> String {
    "ssh".into()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SshHost {
    /// Hosts saved before EAS hosts existed carry no kind.
    #[serde(default = "ssh_kind")]
    pub kind: String,
    pub id: String,
    pub label: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<i64>,
    /// Fields this client doesn't know, written back unchanged.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EasHost {
    pub kind: String,
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// A literal (redacted once saved) or a secret reference object.
    #[serde(default = "empty_token")]
    pub access_token: Value,
    #[serde(default = "thirty")]
    pub max_session_minutes: i64,
    #[serde(default = "ten")]
    pub idle_minutes: i64,
    #[serde(default = "two")]
    pub max_concurrent_sessions: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SimlockHost {
    pub kind: String,
    pub id: String,
    pub label: String,
    pub gateway_url: String,
    /// Redacted once saved; sending it back keeps it.
    #[serde(default)]
    pub agent_token: String,
    #[serde(default = "two")]
    pub max_leases: i64,
    #[serde(default = "sixty")]
    pub max_lease_minutes: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn empty_token() -> Value {
    Value::String(String::new())
}
fn two() -> i64 {
    2
}
fn ten() -> i64 {
    10
}
fn thirty() -> i64 {
    30
}
fn sixty() -> i64 {
    60
}

/// The saved host list to edit and write back. Fails when any entry doesn't decode:
/// writing a partial list would delete the entries this client can't read.
pub(super) fn host_list<T: DeserializeOwned>(settings: &Value, key: &str) -> Result<Vec<T>, String> {
    match settings.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(hosts) => Vec::<T>::deserialize(hosts)
            .map_err(|error| format!("The saved hosts can't be read ({error}), so they were left unchanged.")),
    }
}

/// The saved hosts that decode, for display.
pub(super) fn readable_hosts<T: DeserializeOwned>(settings: &Value, key: &str) -> Vec<T> {
    let Some(hosts) = settings.get(key).and_then(Value::as_array) else { return Vec::new() };
    hosts.iter().filter_map(|host| T::deserialize(host).ok()).collect()
}

/// `ConfiguredDeviceHostId`.
fn valid_host_id(id: &str) -> bool {
    let id = id.trim();
    let mut chars = id.chars();
    id.len() <= 128
        && id != "local"
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn non_blank(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// `parseDeviceHostDraft`: the host as the server would decode it, or `None`.
pub(super) fn parse_ssh_draft(host: &SshHost) -> Option<SshHost> {
    let target = non_blank(&host.target)?;
    let target_ok = !target.starts_with('-') && !target.chars().any(char::is_whitespace);
    let port_ok = host.port.is_none_or(|port| (1..=65535).contains(&port));
    (valid_host_id(&host.id) && target_ok && port_ok).then_some(())?;
    Some(SshHost {
        kind: ssh_kind(),
        id: host.id.trim().to_owned(),
        label: non_blank(&host.label)?,
        target,
        identity_file: host.identity_file.as_deref().and_then(non_blank),
        port: host.port,
        extra: host.extra.clone(),
    })
}

/// `deviceHostConnectionKey`: which connection a test result answers for.
pub(super) fn ssh_connection_key(host: &SshHost) -> (String, Option<i64>, Option<String>) {
    (host.target.trim().to_owned(), host.port, host.identity_file.as_deref().and_then(non_blank))
}

fn same_destination(a: &SshHost, b: &SshHost) -> bool {
    a.target == b.target && a.port == b.port && a.identity_file == b.identity_file
}

/// `updateDeviceHosts`: adds, replaces or removes `host`, finding the saved entry by
/// `original`'s id, then by its SSH destination, keeping the entry's own id.
pub(super) fn update_device_hosts(
    hosts: &[SshHost],
    host: &SshHost,
    remove: bool,
    original: &SshHost,
) -> Result<Vec<SshHost>, &'static str> {
    let find_destination = |destination: &SshHost| {
        let mut matches = hosts.iter().filter(|candidate| same_destination(candidate, destination));
        let first = matches.next();
        match matches.next() {
            Some(_) => Err("Multiple hosts match this SSH destination. Select the environment to edit its hosts."),
            None => Ok(first),
        }
    };
    let existing = match hosts.iter().find(|candidate| candidate.id == original.id) {
        Some(found) => Some(found),
        None => match find_destination(original)? {
            Some(found) => Some(found),
            None if remove => None,
            None => find_destination(host)?,
        },
    };
    let existing_id = existing.map(|existing| existing.id.clone());
    if remove {
        return Ok(hosts.iter().filter(|candidate| Some(&candidate.id) != existing_id.as_ref()).cloned().collect());
    }
    Ok(match existing_id {
        Some(id) => hosts
            .iter()
            .map(|candidate| {
                if candidate.id == id {
                    SshHost { id: id.clone(), extra: candidate.extra.clone(), ..host.clone() }
                } else {
                    candidate.clone()
                }
            })
            .collect(),
        None => hosts.iter().cloned().chain([host.clone()]).collect(),
    })
}

pub(super) fn new_eas_host(id: String) -> EasHost {
    EasHost {
        kind: "eas".into(),
        id,
        label: "EAS Simulator".into(),
        app_id: None,
        access_token: empty_token(),
        max_session_minutes: 30,
        idle_minutes: 10,
        max_concurrent_sessions: 2,
        extra: Map::new(),
    }
}

/// `parseEasDeviceHostDraft`: whether the server would accept the host.
pub(super) fn valid_eas_draft(host: &EasHost) -> bool {
    let token_ok = match &host.access_token {
        Value::String(_) => true,
        Value::Object(reference) => reference.contains_key("provider") && reference.contains_key("reference"),
        _ => false,
    };
    valid_host_id(&host.id)
        && non_blank(&host.label).is_some()
        && host.app_id.as_deref().is_none_or(|app| non_blank(app).is_some())
        && token_ok
        && (1..=115).contains(&host.max_session_minutes)
        && (1..=60).contains(&host.idle_minutes)
        && (1..=20).contains(&host.max_concurrent_sessions)
        && host.idle_minutes < host.max_session_minutes
}

/// `updateEasDeviceHosts`. With `keep_token`, the environment keeps the token it
/// holds: a saved token arrives redacted.
pub(super) fn update_eas_hosts(hosts: &[EasHost], host: &EasHost, remove: bool, keep_token: bool) -> Vec<EasHost> {
    if remove {
        return hosts.iter().filter(|candidate| candidate.id != host.id).cloned().collect();
    }
    let existing = hosts.iter().find(|candidate| candidate.id == host.id);
    let next = if keep_token {
        EasHost {
            access_token: existing.map_or_else(empty_token, |existing| existing.access_token.clone()),
            ..host.clone()
        }
    } else {
        host.clone()
    };
    match existing {
        Some(_) => hosts
            .iter()
            .map(|candidate| if candidate.id == host.id { next.clone() } else { candidate.clone() })
            .collect(),
        None => hosts.iter().cloned().chain([next]).collect(),
    }
}

pub(super) fn eas_session_limits(host: &EasHost) -> String {
    let sessions = if host.max_concurrent_sessions == 1 {
        "1 session".to_owned()
    } else {
        format!("{} sessions", host.max_concurrent_sessions)
    };
    format!(
        "Up to {sessions} at once, each stopped after {} min or {} idle min",
        host.max_session_minutes, host.idle_minutes
    )
}

pub(super) fn new_simlock_host(id: String) -> SimlockHost {
    SimlockHost {
        kind: "simlock".into(),
        id,
        label: "Simlock".into(),
        gateway_url: String::new(),
        agent_token: String::new(),
        max_leases: 2,
        max_lease_minutes: 60,
        extra: Map::new(),
    }
}

/// `^https?:\/\/[^\s/]+\/?$` on the trimmed URL.
fn valid_gateway_origin(url: &str) -> bool {
    let url = url.trim();
    let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) else { return false };
    let host = rest.strip_suffix('/').unwrap_or(rest);
    !host.is_empty() && !host.contains('/') && !host.chars().any(char::is_whitespace)
}

fn valid_simlock_draft(host: &SimlockHost) -> bool {
    valid_host_id(&host.id)
        && non_blank(&host.label).is_some()
        && valid_gateway_origin(&host.gateway_url)
        && (1..=20).contains(&host.max_leases)
        && (5..=240).contains(&host.max_lease_minutes)
}

/// `simlockDeviceHostProblem`: why the shared fields keep a draft from saving.
pub(super) fn simlock_problem(host: &SimlockHost) -> Option<&'static str> {
    if valid_simlock_draft(host) {
        return None;
    }
    if non_blank(&host.label).is_none() {
        return Some("Give the host a label.");
    }
    // With in-range limits, a draft that still fails has a bad gateway URL.
    let in_range = SimlockHost { max_leases: 1, max_lease_minutes: 60, ..host.clone() };
    Some(if valid_simlock_draft(&in_range) {
        "Keep the limits within the ranges shown."
    } else {
        "Enter the gateway's origin, such as https://gateway.example.ts.net:4700."
    })
}

pub(super) fn simlock_lease_limits(host: &SimlockHost) -> String {
    let devices = if host.max_leases == 1 { "1 device".to_owned() } else { format!("{} devices", host.max_leases) };
    format!("Up to {devices} at once, each released after {} min", host.max_lease_minutes)
}

/// `updateSimlockDeviceHosts`.
pub(super) fn update_simlock_hosts(hosts: &[SimlockHost], host: &SimlockHost, remove: bool) -> Vec<SimlockHost> {
    if remove {
        return hosts.iter().filter(|candidate| candidate.id != host.id).cloned().collect();
    }
    if hosts.iter().any(|candidate| candidate.id == host.id) {
        hosts.iter().map(|candidate| if candidate.id == host.id { host.clone() } else { candidate.clone() }).collect()
    } else {
        hosts.iter().cloned().chain([host.clone()]).collect()
    }
}

/// A secret field: the saved (redacted) value, a replacement, or none.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum TokenEntry {
    Saved,
    New(String),
    Cleared,
}

/// `planSimlockDeviceHostSave` for one environment: its host list with the shared
/// fields and its own token.
pub(super) fn plan_simlock_save(
    hosts: &[SimlockHost],
    draft: &SimlockHost,
    token: &TokenEntry,
) -> Result<Vec<SimlockHost>, &'static str> {
    if let Some(problem) = simlock_problem(draft) {
        return Err(problem);
    }
    let saved = hosts.iter().find(|host| host.id == draft.id).map(|host| host.agent_token.clone()).unwrap_or_default();
    let agent_token = match token {
        TokenEntry::Saved => saved,
        TokenEntry::New(value) => value.trim().to_owned(),
        TokenEntry::Cleared => String::new(),
    };
    Ok(update_simlock_hosts(hosts, &SimlockHost { agent_token, ..draft.clone() }, false))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Pending,
    Warning,
    Error,
}

/// Something in progress or needing a fix; a working source has none.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SourceStatus {
    pub tone: Tone,
    pub label: &'static str,
    pub detail: Option<String>,
}

/// `hostStatus`: what a configured host is doing, when it is not simply working.
pub(super) fn host_status(state: Option<&DeviceState>, host_id: &str) -> Option<SourceStatus> {
    let Some(state) = state else { return Some(SourceStatus { tone: Tone::Pending, label: "Loading", detail: None }) };
    let status = state.host_statuses.get(host_id)?;
    let (tone, label) = match status.status {
        HostStatus::Ready if status.android_discovery_incomplete => (Tone::Warning, "Android list may be incomplete"),
        HostStatus::Installing | HostStatus::Starting => (Tone::Pending, "Starting"),
        HostStatus::Failed => (Tone::Error, "Failed"),
        _ => return None,
    };
    Some(SourceStatus { tone, label, detail: status.detail.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(id: &str, label: &str, target: &str) -> SshHost {
        SshHost {
            kind: "ssh".into(),
            id: id.into(),
            label: label.into(),
            target: target.into(),
            identity_file: None,
            port: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn an_unreadable_entry_blocks_writes_without_hiding_the_others() {
        let settings = serde_json::json!({"deviceHosts": [
            {"kind": "ssh", "id": "mac", "label": "Mac", "target": "me@mac", "future": {"a": 1}},
            {"kind": "ssh", "id": "pi", "label": "Pi", "target": "me@pi", "port": "22"},
        ]});
        assert!(host_list::<SshHost>(&settings, "deviceHosts").is_err());
        let readable = readable_hosts::<SshHost>(&settings, "deviceHosts");
        assert_eq!(readable.iter().map(|host| host.id.as_str()).collect::<Vec<_>>(), ["mac"]);
        assert_eq!(host_list::<SshHost>(&serde_json::json!({}), "deviceHosts"), Ok(Vec::new()));
    }

    #[test]
    fn an_edit_keeps_fields_this_client_does_not_know() {
        let settings = serde_json::json!({"deviceHosts": [
            {"kind": "ssh", "id": "mac", "label": "Mac", "target": "me@mac", "future": {"a": 1}},
        ]});
        let saved = host_list::<SshHost>(&settings, "deviceHosts").unwrap();
        let edited = parse_ssh_draft(&ssh("mac", "Renamed", "me@mac")).unwrap();
        let next = serde_json::to_value(update(&saved, &edited, false)).unwrap();
        assert_eq!(next[0]["label"], "Renamed");
        assert_eq!(next[0]["future"], serde_json::json!({"a": 1}));
    }

    fn update(hosts: &[SshHost], host: &SshHost, remove: bool) -> Vec<SshHost> {
        update_device_hosts(hosts, host, remove, host).unwrap()
    }

    #[test]
    fn adds_to_differing_host_lists_without_losing_other_hosts_including_on_retry() {
        let shared = ssh("shared", "Mac mini", "julius@macmini");
        let local = ssh("other", "Android", "julius@android");
        let lists = [vec![], vec![local.clone()], vec![shared.clone(), local.clone()]];
        let saved: Vec<_> = lists.iter().map(|hosts| update(hosts, &shared, false)).collect();
        assert_eq!(saved, [vec![shared.clone()], vec![local.clone(), shared.clone()], vec![shared.clone(), local]]);
        let again: Vec<_> = saved.iter().map(|hosts| update(hosts, &shared, false)).collect();
        assert_eq!(again, saved);
    }

    #[test]
    fn edits_and_removes_the_host_while_preserving_unrelated_hosts() {
        let shared = ssh("shared", "Mac mini", "julius@macmini");
        let local = ssh("other", "Android", "julius@android");
        let edited = SshHost { target: "julius@new-address".into(), ..shared.clone() };
        let saved: Vec<_> = [vec![shared.clone()], vec![local.clone(), shared]]
            .iter()
            .map(|hosts| update(hosts, &edited, false))
            .collect();
        assert_eq!(saved, [vec![edited.clone()], vec![local.clone(), edited.clone()]]);
        let removed: Vec<_> = saved.iter().map(|hosts| update(hosts, &edited, true)).collect();
        assert_eq!(removed, [vec![], vec![local]]);
    }

    #[test]
    fn recognizes_a_host_added_separately_and_preserves_its_id() {
        let shared = ssh("shared", "Mac mini", "julius@macmini");
        let local = ssh("other", "Android", "julius@android");
        let remote = SshHost { id: "remote-id".into(), ..shared.clone() };
        let edited = SshHost { target: "julius@new-address".into(), ..shared.clone() };
        let hosts = [remote.clone(), local.clone()];
        assert_eq!(update(&hosts, &shared, false), hosts);
        let saved = update_device_hosts(&hosts, &edited, false, &shared).unwrap();
        assert_eq!(saved, [SshHost { id: remote.id, ..edited.clone() }, local.clone()]);
        assert_eq!(update_device_hosts(&saved, &edited, false, &shared).unwrap(), saved);
        assert_eq!(update(&hosts, &shared, true), [local]);
    }

    #[test]
    fn keeps_distinct_ssh_connections_to_the_same_target_separate() {
        let shared = ssh("shared", "Mac mini", "julius@macmini");
        let another_port = SshHost { id: "another-port".into(), port: Some(2222), ..shared.clone() };
        let another_identity =
            SshHost { id: "another-key".into(), identity_file: Some("~/.ssh/another".into()), ..shared.clone() };
        assert_eq!(
            update(&[another_port.clone(), another_identity.clone()], &shared, false),
            [another_port, another_identity, shared]
        );
    }

    #[test]
    fn prefers_the_selected_id_over_a_sibling_with_the_same_destination() {
        let shared = ssh("shared", "Mac mini", "julius@macmini");
        let sibling = SshHost { id: "sibling".into(), label: "Another entry".into(), ..shared.clone() };
        let edited = SshHost { label: "Renamed".into(), ..shared.clone() };
        let hosts = [sibling.clone(), shared.clone()];
        assert_eq!(update_device_hosts(&hosts, &edited, false, &shared).unwrap(), [sibling.clone(), edited]);
        assert_eq!(update(&hosts, &shared, true), [sibling]);
    }

    #[test]
    fn refuses_an_ambiguous_destination_instead_of_changing_a_sibling() {
        let shared = ssh("shared", "Mac mini", "julius@macmini");
        let hosts =
            [SshHost { id: "remote".into(), ..shared.clone() }, SshHost { id: "sibling".into(), ..shared.clone() }];
        for remove in [true, false] {
            let error = update_device_hosts(&hosts, &shared, remove, &shared).unwrap_err();
            assert!(error.starts_with("Multiple hosts"));
        }
    }

    #[test]
    fn ssh_drafts_decode_like_the_schema() {
        let host = SshHost { identity_file: Some("  ".into()), ..ssh("id", " Mac ", " me@mac ") };
        let parsed = parse_ssh_draft(&host).unwrap();
        assert_eq!((parsed.label.as_str(), parsed.target.as_str(), parsed.identity_file), ("Mac", "me@mac", None));
        assert!(parse_ssh_draft(&ssh("id", "Mac", "-oProxy")).is_none());
        assert!(parse_ssh_draft(&ssh("id", "Mac", "me @mac")).is_none());
        assert!(parse_ssh_draft(&ssh("local", "Mac", "me@mac")).is_none());
        assert!(parse_ssh_draft(&SshHost { port: Some(70000), ..ssh("id", "Mac", "me@mac") }).is_none());
    }

    #[test]
    fn rejects_eas_limits_eas_does_not_allow() {
        let host = new_eas_host("eas".into());
        assert!(valid_eas_draft(&host));
        assert!(!valid_eas_draft(&EasHost { max_session_minutes: 10, ..host.clone() }));
        assert!(!valid_eas_draft(&EasHost { max_session_minutes: 600, ..host.clone() }));
        assert!(!valid_eas_draft(&EasHost { label: " ".into(), ..host }));
    }

    #[test]
    fn keeps_the_environments_own_eas_token_when_a_save_leaves_it_unchanged() {
        let edited = EasHost { label: "Renamed".into(), ..new_eas_host("eas".into()) };
        let saved = EasHost { access_token: "••••••".into(), ..new_eas_host("eas".into()) };
        assert_eq!(
            update_eas_hosts(&[saved], &edited, false, true),
            [EasHost { access_token: "••••••".into(), ..edited.clone() }]
        );
        assert_eq!(
            update_eas_hosts(&[], &EasHost { access_token: "••••••".into(), ..edited.clone() }, false, true),
            [edited]
        );
    }

    fn simlock() -> SimlockHost {
        SimlockHost {
            gateway_url: "https://gateway.example.ts.net:4700".into(),
            agent_token: "agent".into(),
            ..new_simlock_host("simlock".into())
        }
    }

    #[test]
    fn names_what_keeps_a_simlock_draft_from_saving() {
        let host = simlock();
        assert_eq!(simlock_problem(&host), None);
        assert_eq!(simlock_problem(&SimlockHost { label: " ".into(), ..host.clone() }), Some("Give the host a label."));
        for url in ["gateway:4700", "https://gateway.example/api"] {
            let problem = simlock_problem(&SimlockHost { gateway_url: url.into(), ..host.clone() }).unwrap();
            assert!(problem.contains("origin"), "{url}");
        }
        assert!(simlock_problem(&SimlockHost { max_leases: 21, ..host.clone() }).unwrap().contains("ranges"));
        assert!(simlock_problem(&SimlockHost { max_lease_minutes: 4, ..host }).unwrap().contains("ranges"));
    }

    #[test]
    fn adds_replaces_and_removes_one_simlock_host() {
        let host = simlock();
        let other = SimlockHost { id: "other".into(), label: "Other".into(), ..host.clone() };
        let added = update_simlock_hosts(std::slice::from_ref(&other), &host, false);
        assert_eq!(added.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["other", "simlock"]);
        let renamed = update_simlock_hosts(&added, &SimlockHost { label: "Renamed".into(), ..host.clone() }, false);
        assert_eq!(renamed.iter().map(|h| h.label.as_str()).collect::<Vec<_>>(), ["Other", "Renamed"]);
        assert_eq!(update_simlock_hosts(&renamed, &host, true), [other]);
    }

    #[test]
    fn saves_the_shared_simlock_fields_and_the_environments_own_token() {
        let host = simlock();
        let draft = SimlockHost { label: "Pool".into(), agent_token: String::new(), ..host.clone() };
        let saved = [SimlockHost { agent_token: "<redacted>".into(), ..host }];
        assert_eq!(
            plan_simlock_save(&saved, &draft, &TokenEntry::Saved).unwrap(),
            [SimlockHost { agent_token: "<redacted>".into(), ..draft.clone() }]
        );
        assert_eq!(
            plan_simlock_save(&[], &draft, &TokenEntry::New(" cups-token ".into())).unwrap(),
            [SimlockHost { agent_token: "cups-token".into(), ..draft.clone() }]
        );
        assert_eq!(plan_simlock_save(&[], &draft, &TokenEntry::Cleared).unwrap(), [draft]);
    }

    #[test]
    fn host_status_names_what_is_not_simply_working() {
        assert_eq!(host_status(None, "mac").map(|status| status.label), Some("Loading"));
        let state = DeviceState::decode(&serde_json::json!({
            "hostStatuses": {
                "ready": {"status": "ready"},
                "partial": {"status": "ready", "androidDiscoveryIncomplete": true},
                "boot": {"status": "starting", "detail": "Installing hub"},
                "down": {"status": "failed", "detail": "ssh: timeout"},
            },
            "hosts": [], "easHosts": [{"id": "eas", "kind": "eas", "label": "EAS", "platforms": [{"platform": "ios", "available": true}]}],
        }))
        .unwrap();
        assert_eq!(host_status(Some(&state), "ready"), None);
        assert_eq!(host_status(Some(&state), "missing"), None);
        assert_eq!(host_status(Some(&state), "partial").unwrap().tone, Tone::Warning);
        assert_eq!(host_status(Some(&state), "boot").unwrap().label, "Starting");
        let down = host_status(Some(&state), "down").unwrap();
        assert_eq!((down.tone, down.detail.as_deref()), (Tone::Error, Some("ssh: timeout")));
        assert_eq!(state.platforms("eas").map(<[_]>::len), Some(1));
    }
}
