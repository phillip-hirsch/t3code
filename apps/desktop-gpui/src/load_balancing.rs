//! Which machine an automatically routed draft starts on (`load-balancing.ts`), and the
//! per-machine preferences Settings › Environments offers (`LoadBalancingSettings.tsx`).

use serde_json::Value;

/// How long a host sample stays usable (`HOST_RESOURCES_FRESH_MS`).
pub const FRESH_MS: i64 = 15_000;
/// A sample this far in the future is a clock problem, not a reading.
const FUTURE_SKEW_MS: i64 = 5_000;
/// A machine busier than this takes no new threads.
const CPU_SATURATED: f64 = 0.95;
/// A machine with less free memory than this share takes no new threads.
const MEMORY_EXHAUSTED: f64 = 0.05;
/// The weight of a machine with no preference set.
pub const DEFAULT_WEIGHT: f64 = 50.;

/// `HostResourcesSnapshot` (`packages/contracts/src/resourceTelemetry.ts`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostResources {
    pub sampled_at: i64,
    pub cpu_utilization: Option<f64>,
    pub cpu_count: u64,
    pub available_memory_bytes: u64,
    pub total_memory_bytes: u64,
}

impl HostResources {
    pub fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            sampled_at: value["sampledAt"].as_i64()?,
            cpu_utilization: value["cpuUtilization"].as_f64(),
            cpu_count: value["cpuCount"].as_u64()?,
            available_memory_bytes: value["availableMemoryBytes"].as_u64()?,
            total_memory_bytes: value["totalMemoryBytes"].as_u64()?,
        })
    }
}

/// One connected machine hosting the project and the selected provider.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub environment_id: String,
    pub resources: Option<HostResources>,
    /// When this client received the sample, so machines' clocks are never compared.
    pub received_at: Option<i64>,
    pub weight: f64,
}

/// `chooseLoadBalancedEnvironment`: the machine with the most weighted free CPU and memory.
pub fn choose(candidates: &[Candidate], now: i64) -> Option<&str> {
    let mut selected = None;
    let mut best = 0.;
    for candidate in candidates {
        let Some(resources) = candidate.resources else { continue };
        let sampled_at = candidate.received_at.unwrap_or(resources.sampled_at);
        let Some(cpu) = resources.cpu_utilization else { continue };
        let usable = candidate.weight.is_finite()
            && candidate.weight > 0.
            && now - sampled_at <= FRESH_MS
            && sampled_at <= now + FUTURE_SKEW_MS
            && cpu < CPU_SATURATED
            && resources.total_memory_bytes > 0
            && resources.cpu_count > 0;
        if !usable {
            continue;
        }
        let memory = resources.available_memory_bytes as f64 / resources.total_memory_bytes as f64;
        if memory <= MEMORY_EXHAUSTED {
            continue;
        }
        let score = candidate.weight * resources.cpu_count as f64 * (1. - cpu) * memory;
        if score > best {
            selected = Some(candidate.environment_id.as_str());
            best = score;
        }
    }
    selected
}

/// The preferences a machine can have, by weight (`preferences`).
pub const PREFERENCES: [(i64, &str); 4] = [(100, "Prefer"), (50, "Normal"), (25, "Less often"), (0, "Manual only")];

/// `loadPreferenceForWeight`: the preference a stored weight reads as.
pub fn preference_for_weight(weight: Option<f64>) -> i64 {
    match weight {
        None => 50,
        Some(50.) => 50,
        Some(0.) => 0,
        Some(weight) if weight < 50. => 25,
        Some(_) => 100,
    }
}

pub fn preference_label(preference: i64) -> &'static str {
    PREFERENCES.iter().find(|(value, _)| *value == preference).map_or("Normal", |(_, label)| label)
}

/// `summarizeLoadPreferences`: the machines whose preference is not "Normal".
pub fn summarize(environments: &[(&str, &str)], weights: &Value) -> Option<String> {
    let parts: Vec<String> = environments
        .iter()
        .filter_map(|(id, label)| {
            let preference = preference_for_weight(weights.get(*id).and_then(Value::as_f64));
            (preference != 50).then(|| format!("{label} {}", preference_label(preference).to_lowercase()))
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// A machine's weight, `DEFAULT_WEIGHT` when unset.
pub fn weight(weights: &Value, environment_id: &str) -> f64 {
    weights.get(environment_id).and_then(Value::as_f64).unwrap_or(DEFAULT_WEIGHT)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn host(cpu: Option<f64>, cpus: u64, free: u64) -> HostResources {
        HostResources {
            sampled_at: 1_000,
            cpu_utilization: cpu,
            cpu_count: cpus,
            available_memory_bytes: free,
            total_memory_bytes: 100,
        }
    }

    fn candidate(id: &str, resources: Option<HostResources>, weight: f64) -> Candidate {
        Candidate { environment_id: id.into(), resources, received_at: Some(1_000), weight }
    }

    #[test]
    fn picks_the_most_weighted_free_capacity_and_skips_unusable_machines() {
        let candidates = [
            candidate("busy", Some(host(Some(0.9), 8, 50)), 50.),
            candidate("idle", Some(host(Some(0.1), 4, 80)), 50.),
            candidate("saturated", Some(host(Some(0.96), 64, 90)), 50.),
            candidate("no-memory", Some(host(Some(0.0), 64, 5)), 50.),
            candidate("manual", Some(host(Some(0.0), 64, 90)), 0.),
            candidate("unknown-cpu", Some(host(None, 64, 90)), 50.),
            candidate("unsampled", None, 50.),
        ];
        assert_eq!(choose(&candidates, 2_000), Some("idle"));
        // A preference outweighs a small capacity gap.
        let preferred =
            [candidate("a", Some(host(Some(0.5), 4, 50)), 100.), candidate("b", Some(host(Some(0.4), 4, 50)), 50.)];
        assert_eq!(choose(&preferred, 2_000), Some("a"));
    }

    #[test]
    fn stale_or_future_samples_are_ignored() {
        let one = [candidate("a", Some(host(Some(0.1), 4, 80)), 50.)];
        assert_eq!(choose(&one, 1_000 + FRESH_MS), Some("a"));
        assert_eq!(choose(&one, 1_001 + FRESH_MS), None, "older than the freshness window");
        assert_eq!(choose(&one, 1_000 - 5_001), None, "too far in the future");
    }

    #[test]
    fn preferences_round_trip_weights_and_summarize_the_unusual_ones() {
        assert_eq!([None, Some(50.), Some(0.), Some(10.), Some(75.)].map(preference_for_weight), [50, 50, 0, 25, 100]);
        let weights = json!({"a": 100, "b": 50});
        assert_eq!(summarize(&[("a", "Studio"), ("b", "Laptop")], &weights).as_deref(), Some("Studio prefer"));
        assert_eq!(summarize(&[("b", "Laptop")], &weights), None);
        assert_eq!(weight(&weights, "c"), DEFAULT_WEIGHT);
    }
}
