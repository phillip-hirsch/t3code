//! Background activity: `packages/shared/src/backgroundActivitySettings.ts` and the
//! profile helpers of `SettingsPanels.logic.ts`, over the raw settings JSON.

use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Profile {
    Balanced,
    Performance,
    BatterySaver,
}

impl Profile {
    pub(super) const ALL: [Profile; 3] = [Profile::Balanced, Profile::Performance, Profile::BatterySaver];

    pub(super) fn id(self) -> &'static str {
        match self {
            Profile::Balanced => "balanced",
            Profile::Performance => "performance",
            Profile::BatterySaver => "battery-saver",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Profile::Balanced => "Balanced",
            Profile::Performance => "Performance",
            Profile::BatterySaver => "Battery saver",
        }
    }

    pub(super) fn description(self) -> &'static str {
        match self {
            Profile::Balanced => "Pauses probes for idle clients, locked hosts, or low power mode.",
            Profile::Performance => "Allows scoped background probes while any subscribed client remains connected.",
            Profile::BatterySaver => "Also pauses background probes when the host or client is on battery.",
        }
    }

    fn parse(value: &Value) -> Option<Profile> {
        Profile::ALL.into_iter().find(|p| value.as_str() == Some(p.id()))
    }
}

/// The interval overrides, in milliseconds on the wire (`DurationFromMillis`).
pub(super) const DURATION_KEYS: [&str; 4] = [
    "automaticGitFetchInterval",
    "providerHealthRefreshInterval",
    "hostPowerMonitorActiveInterval",
    "hostPowerMonitorIdleInterval",
];
pub(super) const PAUSE_KEYS: [&str; 4] =
    ["pauseWhenHostLocked", "pauseWhenHostLowPower", "pauseWhenClientLowPower", "pauseWhenOnBattery"];

const DEFAULT_GIT_FETCH_MS: u64 = 30_000;
const DEFAULT_PROVIDER_HEALTH_MS: u64 = 300_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Resolved {
    pub(super) profile: Profile,
    pub(super) durations_ms: [u64; 4],
    pub(super) pauses: [bool; 4],
}

impl Resolved {
    /// Equal intervals and pauses; the profile does not count (`resolvedSettingsEqual`).
    fn same_values(&self, other: &Resolved) -> bool {
        self.durations_ms == other.durations_ms && self.pauses == other.pauses
    }

    pub(super) fn seconds(&self, index: usize) -> u64 {
        (self.durations_ms[index] as f64 / 1000.).round() as u64
    }

    fn overrides(&self) -> Map<String, Value> {
        let durations = DURATION_KEYS.iter().zip(self.durations_ms).map(|(k, v)| (k.to_string(), json!(v)));
        let pauses = PAUSE_KEYS.iter().zip(self.pauses).map(|(k, v)| (k.to_string(), json!(v)));
        durations.chain(pauses).collect()
    }
}

pub(super) fn preset(profile: Profile) -> Resolved {
    let (durations_ms, pauses) = match profile {
        Profile::Performance => ([15_000, 60_000, 30_000, 120_000], [true, false, false, false]),
        Profile::Balanced => {
            ([DEFAULT_GIT_FETCH_MS, DEFAULT_PROVIDER_HEALTH_MS, 30_000, 300_000], [true, true, true, false])
        }
        Profile::BatterySaver => ([0, 900_000, 60_000, 600_000], [true, true, true, true]),
    };
    Resolved { profile, durations_ms, pauses }
}

/// `getBackgroundActivityBaseProfile`.
fn base_profile(background: &Value) -> Profile {
    if background["profile"] == "custom" {
        Profile::parse(&background["baseProfile"]).unwrap_or(Profile::Balanced)
    } else {
        Profile::parse(&background["profile"]).unwrap_or(Profile::Balanced)
    }
}

/// `resolveBackgroundActivitySettings`.
fn resolve(background: &Value) -> Resolved {
    let base = preset(base_profile(background));
    let overrides = if background["profile"] == "custom" { &background["overrides"] } else { &Value::Null };
    Resolved {
        profile: base.profile,
        durations_ms: std::array::from_fn(|i| overrides[DURATION_KEYS[i]].as_u64().unwrap_or(base.durations_ms[i])),
        pauses: std::array::from_fn(|i| overrides[PAUSE_KEYS[i]].as_bool().unwrap_or(base.pauses[i])),
    }
}

/// `resolveServerBackgroundActivitySettings`: the legacy flat fields still count while
/// `backgroundActivity` is untouched.
pub(super) fn resolve_server(settings: &Value) -> Resolved {
    let background = &settings["backgroundActivity"];
    let untouched = background["profile"].as_str().unwrap_or("balanced") == "balanced"
        && background.get("baseProfile").is_none()
        && background["overrides"].as_object().is_none_or(Map::is_empty);
    let legacy = Profile::parse(&settings["backgroundActivityProfile"]).unwrap_or(Profile::Balanced);
    let git = settings["automaticGitFetchInterval"].as_u64().unwrap_or(DEFAULT_GIT_FETCH_MS);
    let health = settings["providerHealthRefreshInterval"].as_u64().unwrap_or(DEFAULT_PROVIDER_HEALTH_MS);
    let has_legacy = legacy != Profile::Balanced || git != DEFAULT_GIT_FETCH_MS || health != DEFAULT_PROVIDER_HEALTH_MS;
    if !(untouched && has_legacy) {
        return resolve(background);
    }
    let legacy_preset = preset(legacy);
    let mut overrides = Map::new();
    if git != legacy_preset.durations_ms[0] {
        overrides.insert(DURATION_KEYS[0].into(), json!(git));
    }
    if health != legacy_preset.durations_ms[1] {
        overrides.insert(DURATION_KEYS[1].into(), json!(health));
    }
    let profile = if overrides.is_empty() { legacy.id() } else { "custom" };
    resolve(&json!({"schemaVersion": 1, "profile": profile, "baseProfile": legacy.id(), "overrides": overrides}))
}

/// The preset the resolved values match, base profile first; `None` is "Advanced".
/// (`resolveBackgroundActivityProfileOption` via `normalizeBackgroundActivitySettings`.)
pub(super) fn profile_option(settings: &Value) -> Option<Profile> {
    matching_preset(&resolve_server(settings))
}

fn matching_preset(resolved: &Resolved) -> Option<Profile> {
    [resolved.profile].into_iter().chain(Profile::ALL).find(|p| resolved.same_values(&preset(*p)))
}

/// The patch picking a preset from the row's select.
pub(super) fn profile_patch(profile: Profile) -> Value {
    json!({"backgroundActivity": {"schemaVersion": 1, "profile": profile.id(), "overrides": {}}})
}

/// `DEFAULT_UNIFIED_SETTINGS.backgroundActivity`.
pub(super) fn default_background() -> Value {
    json!({"schemaVersion": 1, "profile": "balanced", "overrides": {}})
}

pub(super) fn can_reset(settings: &Value) -> bool {
    settings["backgroundActivity"] != default_background()
}

/// `backgroundActivitySharedPolicySettings`: a new base profile that keeps the custom values.
pub(super) fn shared_policy_patch(settings: &Value, profile: Profile) -> Value {
    let resolved = resolve_server(settings);
    let overrides = if matching_preset(&resolved).is_some() {
        Map::new()
    } else {
        let base = preset(resolved.profile);
        let mut overrides = resolved.overrides();
        overrides.retain(|key, value| *value != base.overrides()[key]);
        overrides
    };
    json!({"backgroundActivity": {"schemaVersion": 1, "profile": "custom", "baseProfile": profile.id(), "overrides": overrides}})
}

/// `backgroundActivityOverrideSettings`: every resolved value, with `key` set to `value`.
pub(super) fn override_patch(settings: &Value, key: &str, value: Value) -> Value {
    let mut overrides = resolve_server(settings).overrides();
    overrides.insert(key.into(), value);
    let base = base_profile(&settings["backgroundActivity"]);
    json!({"backgroundActivity": {"schemaVersion": 1, "profile": "custom", "baseProfile": base.id(), "overrides": overrides}})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(background: Value) -> Value {
        json!({"backgroundActivity": background, "backgroundActivityProfile": "balanced",
            "automaticGitFetchInterval": 30_000, "providerHealthRefreshInterval": 300_000})
    }

    #[test]
    fn custom_values_matching_a_preset_read_as_that_preset() {
        let custom = settings(json!({"profile": "custom", "baseProfile": "balanced",
            "overrides": {"automaticGitFetchInterval": 15_000, "providerHealthRefreshInterval": 60_000,
                "hostPowerMonitorIdleInterval": 120_000, "pauseWhenHostLowPower": false, "pauseWhenClientLowPower": false}}));
        assert_eq!(profile_option(&custom), Some(Profile::Performance));
        let tweaked = settings(
            json!({"profile": "custom", "baseProfile": "balanced", "overrides": {"pauseWhenOnBattery": true}}),
        );
        assert_eq!(profile_option(&tweaked), None, "no preset matches, so it is Advanced");
        assert_eq!(profile_option(&settings(default_background())), Some(Profile::Balanced));
    }

    #[test]
    fn legacy_flat_fields_apply_until_background_activity_is_set() {
        let mut legacy = settings(default_background());
        legacy["backgroundActivityProfile"] = json!("battery-saver");
        legacy["automaticGitFetchInterval"] = json!(0);
        legacy["providerHealthRefreshInterval"] = json!(900_000);
        assert_eq!(profile_option(&legacy), Some(Profile::BatterySaver));
        legacy["automaticGitFetchInterval"] = json!(45_000);
        let resolved = resolve_server(&legacy);
        assert_eq!((resolved.profile, resolved.durations_ms[0]), (Profile::BatterySaver, 45_000));
        assert_eq!(profile_option(&legacy), None);
    }

    #[test]
    fn overrides_and_shared_policy_keep_the_custom_values() {
        let base = settings(default_background());
        let patch = override_patch(&base, "pauseWhenOnBattery", json!(true));
        let background = &patch["backgroundActivity"];
        assert_eq!(
            (background["profile"].as_str(), background["baseProfile"].as_str()),
            (Some("custom"), Some("balanced"))
        );
        assert_eq!(background["overrides"].as_object().map(Map::len), Some(8), "every resolved value is written");

        let custom = settings(background.clone());
        let shared = shared_policy_patch(&custom, Profile::Performance);
        assert_eq!(shared["backgroundActivity"]["baseProfile"], "performance");
        assert_eq!(
            shared["backgroundActivity"]["overrides"],
            json!({"pauseWhenOnBattery": true}),
            "only the diff from the old base"
        );
        let preset_values = shared_policy_patch(&base, Profile::BatterySaver);
        assert_eq!(preset_values["backgroundActivity"]["overrides"], json!({}));
    }
}
