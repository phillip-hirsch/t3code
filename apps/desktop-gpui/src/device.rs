//! The environment's device service (`DeviceServiceState` in `packages/contracts/src/device.ts`,
//! streamed by `subscribeDeviceState`) and the readiness logic of `apps/web/src/components/device/DeviceSetup.tsx`.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum HostStatus {
    #[default]
    Disabled,
    Idle,
    Installing,
    Starting,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostStatusEntry {
    pub status: HostStatus,
    pub detail: Option<String>,
    #[serde(default)]
    pub android_discovery_incomplete: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolVersion {
    pub required_version: String,
    pub installed_versions: Vec<String>,
    pub running_version: Option<String>,
}

impl ToolVersion {
    /// The installed required version is missing; the server can install it (`list { updateTool }`).
    pub fn needs_update(&self) -> bool {
        !self.installed_versions.contains(&self.required_version)
    }

    /// The version to show: running, else the required one if installed, else the newest installed.
    pub fn shown_version(&self) -> Option<&str> {
        self.running_version.as_deref().or_else(|| {
            if self.needs_update() {
                self.installed_versions.iter().max_by(|a, b| compare_numeric(a, b)).map(String::as_str)
            } else {
                Some(&self.required_version)
            }
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ToolVersions {
    pub hub: ToolVersion,
    pub agent: ToolVersion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Hub,
    Agent,
}

impl Tool {
    pub fn wire(self) -> &'static str {
        match self {
            Tool::Hub => "hub",
            Tool::Agent => "agent",
        }
    }

    /// `DeviceToolVersions`' popover title.
    pub fn label(self) -> &'static str {
        match self {
            Tool::Hub => "Device hub",
            Tool::Agent => "Agent device",
        }
    }
}

impl ToolVersions {
    pub fn get(&self, tool: Tool) -> &ToolVersion {
        match tool {
            Tool::Hub => &self.hub,
            Tool::Agent => &self.agent,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlatformAvailability {
    pub platform: String,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostSummary {
    pub id: String,
    /// `local`, `ssh`, `eas` or `simlock`.
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub platforms: Vec<PlatformAvailability>,
    pub tools: Option<ToolVersions>,
    pub tool_inspection_error: Option<String>,
    #[serde(default)]
    pub hub_installed: bool,
    #[serde(default)]
    pub agent_device_installed: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub host_id: String,
    pub id: String,
    pub platform: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceState {
    pub supports_host_retry: bool,
    pub supports_tool_update: bool,
    pub supports_tool_inspection: bool,
    pub supports_eas_hosts: bool,
    pub supports_simlock_hosts: bool,
    /// This machine's hub; remote hosts are in `host_statuses`.
    pub host_status: HostStatus,
    pub host_status_detail: Option<String>,
    pub host_statuses: HashMap<String, HostStatusEntry>,
    pub devices: Vec<DeviceSummary>,
    /// Every host: the wire's `hosts`, `easHosts` and `simlockHosts` merged, like the contract's decode.
    pub hosts: Vec<HostSummary>,
    pub onboarding_completed: bool,
    pub agent_access_enabled: bool,
    pub revision: i64,
}

impl DeviceState {
    pub fn decode(value: &Value) -> Result<Self, serde_json::Error> {
        let mut state = Self::deserialize(value)?;
        for key in ["easHosts", "simlockHosts"] {
            if let Some(hosts) = value.get(key) {
                state.hosts.extend(Vec::<HostSummary>::deserialize(hosts)?);
            }
        }
        Ok(state)
    }

    pub fn local_host(&self) -> Option<&HostSummary> {
        self.hosts.iter().find(|host| host.kind == "local")
    }

    /// `deviceHubReadiness`: `host_status` only tracks this machine. A machine without simulator
    /// tooling never starts its own hub, so remote device hosts count too.
    pub fn readiness(&self) -> Readiness {
        let statuses = || self.host_statuses.values().map(|entry| entry.status);
        let ready = self.host_status == HostStatus::Ready || statuses().any(|status| status == HostStatus::Ready);
        let busy = !ready
            && std::iter::once(self.host_status)
                .chain(statuses())
                .any(|status| matches!(status, HostStatus::Installing | HostStatus::Starting));
        Readiness { ready, busy }
    }

    /// `DeviceHostUpdates`: the hosts of `hosts` that are installing, starting or failed, in order.
    pub fn host_updates<'a>(&'a self, hosts: impl Iterator<Item = &'a HostSummary>) -> Vec<HostUpdate<'a>> {
        if self.host_status == HostStatus::Disabled {
            return Vec::new();
        }
        hosts
            .filter_map(|host| {
                let entry = self.host_statuses.get(&host.id)?;
                let failed = entry.status == HostStatus::Failed;
                let detail = match (&entry.detail, entry.status) {
                    (Some(detail), _) => detail.as_str(),
                    (None, HostStatus::Failed) => "Device support could not start.",
                    (None, HostStatus::Installing) => "Installing device tools…",
                    (None, HostStatus::Starting) => "Starting device tools…",
                    _ => return None,
                };
                matches!(entry.status, HostStatus::Installing | HostStatus::Starting | HostStatus::Failed)
                    .then_some(HostUpdate { host, detail, failed })
            })
            .collect()
    }

    /// `DeviceHubSetupStatus`: the line beside the hub switch, while `pending` or once ready.
    pub fn hub_status(&self, pending: bool, compact: bool) -> Option<SetupStatus> {
        if pending {
            let label = match (self.host_status, compact) {
                (HostStatus::Installing, true) => "Installing…",
                (HostStatus::Installing, false) => "Installing device hub…",
                (HostStatus::Starting, true) => "Starting…",
                (HostStatus::Starting, false) => "Starting device hub…",
                (_, true) => "Updating…",
                (_, false) => "Updating device hub…",
            };
            return Some(SetupStatus { label, pending: true });
        }
        self.readiness().ready.then_some(SetupStatus { label: "Device hub is ready.", pending: false })
    }

    /// `AgentDeviceSetupStatus`: the line beside the agent access switch.
    pub fn agent_status(&self, pending: bool, compact: bool) -> Option<SetupStatus> {
        if pending {
            let label = match (self.host_status, compact) {
                (HostStatus::Installing, true) => "Installing…",
                (HostStatus::Installing, false) => "Installing agent tools…",
                (HostStatus::Starting, true) => "Starting…",
                (HostStatus::Starting, false) => "Starting agent tools…",
                (_, true) => "Updating…",
                (_, false) => "Updating agent access…",
            };
            return Some(SetupStatus { label, pending: true });
        }
        (self.agent_access_enabled
            && self.readiness().ready
            && self.hosts.iter().any(|host| host.agent_device_installed))
        .then_some(SetupStatus { label: "Agent tools are ready.", pending: false })
    }
}

/// The workspace's device slot: the streamed state plus what Settings has in flight on it.
#[derive(Default)]
pub struct Devices {
    /// `None` until this connection's first state arrives.
    pub state: Option<DeviceState>,
    /// `pending` in the web's `DeviceIntegrationControls`: one device action at a time.
    pub pending: Option<Pending>,
    /// The last failed tool update, shown in that tool's version popover.
    pub update_error: Option<(Tool, String)>,
    /// The host whose `DeviceHostUpdates` retry is in flight.
    pub retrying: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    Hub,
    Agent,
    Check,
    Update(Tool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    /// Something is installing or starting and nothing is ready yet; device actions wait for it.
    pub busy: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupStatus {
    pub label: &'static str,
    /// Spinner while pending, check mark once ready.
    pub pending: bool,
}

#[derive(Debug, PartialEq)]
pub struct HostUpdate<'a> {
    pub host: &'a HostSummary,
    pub detail: &'a str,
    pub failed: bool,
}

/// The `DeviceToolVersions` trigger: `v1.2.3`, `Not installed` or `Version unknown`.
pub fn tool_version_label(tools: Option<&ToolVersions>, tool: Tool) -> String {
    match tools.map(|tools| tools.get(tool)) {
        Some(version) => match version.shown_version() {
            Some(shown) => format!("v{shown}"),
            None => "Not installed".into(),
        },
        None => "Version unknown".into(),
    }
}

/// `localeCompare(_, _, { numeric: true })` for version strings: digit runs compare as numbers.
fn compare_numeric(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        let (Some(x), Some(y)) = (a.chars().next(), b.chars().next()) else { return a.len().cmp(&b.len()) };
        let ordering = if x.is_ascii_digit() && y.is_ascii_digit() {
            let split = |s: &str| s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            let (an, bn) = (split(a), split(b));
            let (na, nb) = (a[..an].trim_start_matches('0'), b[..bn].trim_start_matches('0'));
            let ordering = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            (a, b) = (&a[an..], &b[bn..]);
            ordering
        } else {
            let ordering = x.cmp(&y);
            (a, b) = (&a[x.len_utf8()..], &b[y.len_utf8()..]);
            ordering
        };
        if ordering.is_ne() {
            return ordering;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn state(value: Value) -> DeviceState {
        let mut base = json!({
            "hosts": [{
                "id": "local", "kind": "local", "label": "This machine",
                "hubInstalled": false, "agentDeviceInstalled": false,
                "platforms": [{"platform": "ios", "available": true}],
            }],
            "hostStatus": "ready", "hostStatuses": {}, "devices": [], "sessions": [],
            "onboardingCompleted": false, "agentAccessEnabled": false, "hubBasePath": "/api/device-hub", "revision": 0,
        });
        for (key, value) in value.as_object().unwrap() {
            base[key] = value.clone();
        }
        DeviceState::decode(&base).unwrap()
    }

    #[test]
    fn decodes_eas_and_simlock_hosts_into_hosts() {
        let state = state(json!({
            "easHosts": [{"id": "eas", "kind": "eas", "label": "EAS", "platforms": [], "hubInstalled": false, "agentDeviceInstalled": false}],
            "simlockHosts": [{"id": "pool", "kind": "simlock", "label": "Pool", "platforms": [], "hubInstalled": false, "agentDeviceInstalled": false}],
        }));
        let ids: Vec<_> = state.hosts.iter().map(|host| host.id.as_str()).collect();
        assert_eq!(ids, ["local", "eas", "pool"]);
    }

    #[test]
    fn counts_a_ready_remote_host_when_this_machine_has_no_hub() {
        let remote = state(json!({"hostStatus": "idle", "hostStatuses": {"macmini": {"status": "ready"}}}));
        assert_eq!(remote.readiness(), Readiness { ready: true, busy: false });
        let starting = state(json!({"hostStatus": "idle", "hostStatuses": {"macmini": {"status": "starting"}}}));
        assert_eq!(starting.readiness(), Readiness { ready: false, busy: true });
        let installing = state(json!({"hostStatus": "installing"}));
        assert_eq!(installing.readiness(), Readiness { ready: false, busy: true });
        // A ready host makes the rest not block anything.
        let mixed = state(json!({"hostStatus": "installing", "hostStatuses": {"macmini": {"status": "ready"}}}));
        assert_eq!(mixed.readiness(), Readiness { ready: true, busy: false });
    }

    #[test]
    fn setup_status_follows_the_local_hub() {
        let installing = state(json!({"hostStatus": "installing"}));
        assert_eq!(installing.hub_status(true, true).unwrap().label, "Installing…");
        assert_eq!(installing.hub_status(true, false).unwrap().label, "Installing device hub…");
        assert_eq!(installing.hub_status(false, true), None);
        assert_eq!(state(json!({})).hub_status(false, true).unwrap().label, "Device hub is ready.");
        assert_eq!(state(json!({})).agent_status(true, true).unwrap().label, "Updating…");

        let agent_ready = state(json!({
            "agentAccessEnabled": true,
            "hosts": [{"id": "local", "kind": "local", "label": "This machine", "platforms": [], "hubInstalled": true, "agentDeviceInstalled": true}],
        }));
        assert_eq!(agent_ready.agent_status(false, true).unwrap().label, "Agent tools are ready.");
        assert_eq!(state(json!({"agentAccessEnabled": true})).agent_status(false, true), None);
    }

    #[test]
    fn host_updates_list_busy_and_failed_hosts() {
        let failed = state(json!({"hostStatuses": {"local": {"status": "failed"}}}));
        let updates = failed.host_updates(failed.hosts.iter());
        assert_eq!(updates.len(), 1);
        assert!(updates[0].failed);
        assert_eq!(updates[0].detail, "Device support could not start.");

        let installing =
            state(json!({"hostStatuses": {"local": {"status": "installing", "detail": "Downloading 2/3"}}}));
        assert_eq!(installing.host_updates(installing.hosts.iter())[0].detail, "Downloading 2/3");
        let ready = state(json!({"hostStatuses": {"local": {"status": "ready", "detail": "warning"}}}));
        assert!(ready.host_updates(ready.hosts.iter()).is_empty());
        let disabled = state(json!({"hostStatus": "disabled", "hostStatuses": {"local": {"status": "failed"}}}));
        assert!(disabled.host_updates(disabled.hosts.iter()).is_empty());
    }

    #[test]
    fn tool_version_label_prefers_running_then_required_then_newest() {
        let tools =
            |version: Value| ToolVersions::deserialize(json!({"hub": version.clone(), "agent": version})).unwrap();
        let running =
            tools(json!({"requiredVersion": "2.0.0", "installedVersions": ["2.0.0"], "runningVersion": "1.9.0"}));
        assert_eq!(tool_version_label(Some(&running), Tool::Hub), "v1.9.0");
        let installed =
            tools(json!({"requiredVersion": "2.0.0", "installedVersions": ["1.0.0", "2.0.0"], "runningVersion": null}));
        assert_eq!(tool_version_label(Some(&installed), Tool::Agent), "v2.0.0");
        assert!(!installed.hub.needs_update());
        let outdated = tools(
            json!({"requiredVersion": "2.0.0", "installedVersions": ["1.10.0", "1.9.0"], "runningVersion": null}),
        );
        assert_eq!(tool_version_label(Some(&outdated), Tool::Hub), "v1.10.0");
        assert!(outdated.hub.needs_update());
        let missing = tools(json!({"requiredVersion": "2.0.0", "installedVersions": [], "runningVersion": null}));
        assert_eq!(tool_version_label(Some(&missing), Tool::Hub), "Not installed");
        assert_eq!(tool_version_label(None, Tool::Hub), "Version unknown");
    }
}
