//! The Diagnostics page's data and formatting (`DiagnosticsSettings.tsx`,
//! `ResourceTelemetryDiagnostics.tsx` and its `.logic.ts`).

use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

/// `Schema.Option` on the wire: `{"_tag":"Some","value":…}` or `{"_tag":"None"}`.
pub(super) fn some(value: &Value) -> Option<&Value> {
    (value["_tag"] == "Some").then(|| &value["value"])
}

// ---------------------------------------------------------------------------------------------
// Wire shapes (packages/contracts/src/server.ts, resourceTelemetry.ts)

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct TraceDiagnostics {
    pub read_at: String,
    pub record_count: u64,
    pub parse_error_count: u64,
    pub failure_count: u64,
    pub slow_span_threshold_ms: f64,
    pub slow_span_count: u64,
    pub top_spans_by_count: Vec<SpanSummary>,
    pub slowest_spans: Vec<SpanOccurrence>,
    pub common_failures: Vec<FailureSummary>,
    pub latest_failures: Vec<RecentFailure>,
    pub latest_warning_and_error_logs: Vec<LogEvent>,
    pub partial_failure: Value,
    pub error: Value,
}

impl TraceDiagnostics {
    pub fn error_message(&self) -> Option<&str> {
        some(&self.error).and_then(|error| error["message"].as_str())
    }

    pub fn partial(&self) -> bool {
        some(&self.partial_failure).and_then(Value::as_bool).unwrap_or(false)
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct SpanSummary {
    pub name: String,
    pub count: u64,
    pub failure_count: u64,
    pub average_duration_ms: f64,
    pub max_duration_ms: f64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct SpanOccurrence {
    pub name: String,
    pub duration_ms: f64,
    pub ended_at: String,
    pub trace_id: String,
    pub span_id: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct FailureSummary {
    pub name: String,
    pub cause: String,
    pub count: u64,
    pub last_seen_at: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct RecentFailure {
    pub name: String,
    pub cause: String,
    pub duration_ms: f64,
    pub ended_at: String,
    pub trace_id: String,
    pub span_id: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct LogEvent {
    pub span_name: String,
    pub level: String,
    pub message: String,
    pub seen_at: String,
    pub trace_id: String,
    pub span_id: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct ProcessDiagnostics {
    pub server_pid: u64,
    pub read_at: String,
    pub process_count: u64,
    pub total_rss_bytes: f64,
    pub total_cpu_percent: f64,
    pub processes: Vec<ProcessEntry>,
    pub error: Value,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct ProcessEntry {
    pub pid: u64,
    pub start_time_ms: u64,
    pub cpu_percent: f64,
    pub rss_bytes: f64,
    pub command: String,
    pub depth: u32,
    pub child_pids: Vec<u64>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct ProcessHistory {
    pub read_at: String,
    pub sample_interval_ms: f64,
    pub retained_sample_count: u64,
    pub total_cpu_seconds_approx: f64,
    pub buckets: Vec<CpuBucket>,
    pub top_processes: Vec<ProcessHistoryEntry>,
    pub error: Value,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct CpuBucket {
    pub started_at: String,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct ProcessHistoryEntry {
    pub process_key: String,
    pub pid: u64,
    pub command: String,
    pub depth: u32,
    pub is_server_root: bool,
    pub current_cpu_percent: f64,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub cpu_seconds_approx: f64,
    pub max_rss_bytes: f64,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Identity {
    pub pid: u64,
    pub start_time_ms: u64,
}

impl Identity {
    /// `processIdentityKey`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.pid, self.start_time_ms)
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct TelemetryProcess {
    pub identity: Identity,
    pub ppid: u64,
    pub child_pids: Vec<u64>,
    pub depth: u32,
    pub name: String,
    pub command: String,
    pub category: String,
    pub cpu_percent: f64,
    pub cpu_time_ms: f64,
    pub resident_bytes: f64,
    pub io_read_bytes: f64,
    pub io_write_bytes: f64,
    pub io_read_bytes_per_second: f64,
    pub io_write_bytes_per_second: f64,
    pub io_semantics: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Aggregate {
    pub process_count: u64,
    pub current_cpu_percent: f64,
    pub cpu_time_ms: f64,
    pub current_rss_bytes: f64,
    pub peak_rss_bytes: f64,
    pub io_read_bytes: f64,
    pub io_write_bytes: f64,
    pub io_read_bytes_per_second: f64,
    pub io_write_bytes_per_second: f64,
    pub process_starts: u64,
    pub process_exits: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Groups {
    pub backend: Aggregate,
    pub electron: Aggregate,
    pub monitor: Aggregate,
    pub all_t3: Aggregate,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct SourceHealth {
    pub status: String,
    pub last_error: Value,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Health {
    pub native: SourceHealth,
    pub desktop: SourceHealth,
    pub sidecar_version: Value,
    pub sidecar_pid: Value,
    pub restart_count: u64,
    pub collection_duration_micros: f64,
    pub scanned_process_count: u64,
    pub retained_process_count: u64,
    pub inaccessible_process_count: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Power {
    pub idle: String,
    pub idle_seconds: Option<f64>,
    pub locked: String,
    pub suspended: bool,
    pub on_battery: String,
    pub low_power_mode: String,
    pub thermal_state: String,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct AttributionEntry {
    pub component: String,
    pub operation: String,
    pub logical_read_bytes: f64,
    pub logical_write_bytes: f64,
    pub count: u64,
    pub duration_ms: f64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Attribution {
    pub entries: Vec<AttributionEntry>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct Telemetry {
    pub read_at: String,
    pub sample_interval_ms: f64,
    pub processes: Vec<TelemetryProcess>,
    pub groups: Groups,
    pub power: Power,
    pub speed_limit_percent: Value,
    pub attribution: Attribution,
    pub health: Health,
}

impl Telemetry {
    pub fn speed_limit(&self) -> Option<f64> {
        some(&self.speed_limit_percent).and_then(Value::as_f64)
    }

    /// `hasHostPowerSignal`.
    pub fn has_host_power_signal(&self) -> bool {
        let power = &self.power;
        [&power.on_battery, &power.low_power_mode, &power.idle, &power.locked, &power.thermal_state]
            .iter()
            .any(|state| state.as_str() != "unknown")
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct TelemetryBucket {
    pub started_at: String,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub io_read_bytes: f64,
    pub io_write_bytes: f64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct TelemetrySummary {
    pub identity: Identity,
    pub name: String,
    pub command: String,
    pub category: String,
    pub max_cpu_percent: f64,
    pub cpu_time_ms: f64,
    pub peak_rss_bytes: f64,
    pub io_read_bytes: f64,
    pub io_write_bytes: f64,
    pub sample_count: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct TelemetryHistory {
    pub buckets: Vec<TelemetryBucket>,
    pub top_processes: Vec<TelemetrySummary>,
}

// ---------------------------------------------------------------------------------------------
// Formatting

/// `toFixed(digits).replace(/\.0+$/, "")`.
fn fixed_trimmed(value: f64, digits: usize) -> String {
    let text = format!("{value:.digits$}");
    match text.split_once('.') {
        Some((whole, fraction)) if fraction.bytes().all(|b| b == b'0') => whole.to_owned(),
        _ => text,
    }
}

/// `new Intl.NumberFormat().format` for counts: thousands separated by commas.
pub(super) fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `formatDuration`.
pub(super) fn format_duration(ms: f64) -> String {
    if ms < 1_000. {
        return format!("{} ms", ms.round());
    }
    format!("{} s", fixed_trimmed(ms / 1_000., if ms >= 10_000. { 1 } else { 2 }))
}

/// `formatCpuTime` (seconds): both pages' variant; the telemetry one passes ms / 1000.
pub(super) fn format_cpu_time(seconds: f64) -> String {
    if seconds < 60. {
        return format!("{}s", fixed_trimmed(seconds, if seconds >= 10. { 1 } else { 2 }));
    }
    let minutes = seconds / 60.;
    if minutes < 60. {
        return format!("{}m", fixed_trimmed(minutes, if minutes >= 10. { 1 } else { 2 }));
    }
    format!("{}h", fixed_trimmed(minutes / 60., 2))
}

/// `formatDurationMicros`.
pub(super) fn format_duration_micros(value: f64) -> String {
    if value < 1_000. {
        return format!("{} \u{b5}s", value.round());
    }
    if value < 1_000_000. {
        return format!("{} ms", fixed_trimmed(value / 1_000., 2));
    }
    format!("{} s", fixed_trimmed(value / 1_000_000., 2))
}

/// `formatSampleInterval`.
#[expect(clippy::float_cmp, reason = "the web compares exactly: `seconds === 1`")]
pub(super) fn format_sample_interval(ms: f64) -> String {
    if ms < 1_000. {
        return format!("{} ms", ms.round().max(0.));
    }
    let seconds = ms / 1_000.;
    let rounded = (seconds * 10.).round() / 10.;
    let text = if rounded.fract() == 0. { format!("{rounded}") } else { format!("{rounded:.1}") };
    format!("{text} {}", if seconds == 1. { "second" } else { "seconds" })
}

pub(super) fn format_rate(bytes_per_second: f64) -> String {
    format!("{}/s", super::super::system::format_bytes(bytes_per_second))
}

/// `toFixed(1)` with a percent sign.
pub(super) fn percent(value: f64) -> String {
    format!("{value:.1}%")
}

/// `shortenTraceId`.
pub(super) fn shorten_trace_id(trace_id: &str) -> String {
    let chars: Vec<char> = trace_id.chars().collect();
    if chars.len() <= 32 {
        return trace_id.to_owned();
    }
    let head: String = chars[..18].iter().collect();
    let tail: String = chars[chars.len() - 10..].iter().collect();
    format!("{head}...{tail}")
}

/// `isStaleProcessSignalMessage`.
pub(super) fn is_stale_signal_message(message: Option<&str>) -> bool {
    message.is_some_and(|message| message.contains("not a live descendant"))
}

/// `formatProcessName`: the executable's file name from a command line.
pub(super) fn process_name(command: &str) -> String {
    let Some(first) = command.split_whitespace().next() else { return command.to_owned() };
    let first = first.strip_prefix(['\'', '"']).unwrap_or(first);
    let normalized = first.strip_suffix(['\'', '"']).unwrap_or(first);
    normalized.split(['\\', '/']).rfind(|segment| !segment.is_empty()).unwrap_or(normalized).to_owned()
}

/// `formatShortProcessName`.
pub(super) fn short_process_name(command: &str) -> String {
    let name = process_name(command);
    if name.chars().count() > 42 { format!("{}...", name.chars().take(39).collect::<String>()) } else { name }
}

/// The telemetry table's `formatProcessName`: the sampled name, else the command's.
pub(super) fn telemetry_process_name(name: &str, command: &str) -> String {
    if name.trim().is_empty() { process_name(command) } else { name.to_owned() }
}

/// `formatProcessType`.
pub(super) fn process_type(depth: u32, command: &str) -> &'static str {
    if depth > 0 {
        return "Subprocess";
    }
    let lower = command.to_ascii_lowercase();
    let is_agent = lower
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|word| matches!(word, "codex" | "claude" | "opencode" | "cursor"));
    if is_agent { "Agent" } else { "Process" }
}

/// `categoryLabel`.
pub(super) fn category_label(category: &str) -> &'static str {
    match category {
        "server" => "Server",
        "server-child" => "Backend child",
        "provider-root" => "Provider",
        "terminal-root" => "Terminal",
        "electron-main" => "Electron main",
        "electron-renderer" => "Renderer",
        "electron-gpu" => "GPU",
        "electron-utility" => "Electron utility",
        "resource-monitor" => "Monitor",
        _ => "T3 process",
    }
}

/// `canSignalProcess`.
pub(super) fn can_signal(category: &str) -> bool {
    matches!(category, "server-child" | "provider-root" | "terminal-root")
}

/// `ioSemanticsLabel`.
pub(super) fn io_semantics_label(semantics: &str) -> &'static str {
    match semantics {
        "storage" => "Storage bytes",
        "logical" => "Logical bytes",
        "all-io" => "All I/O bytes",
        _ => "Unavailable",
    }
}

/// `booleanStateLabel`.
pub(super) fn boolean_state<'a>(value: &str, yes: &'a str, no: &'a str) -> &'a str {
    match value {
        "true" => yes,
        "false" => no,
        _ => "Unknown",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Tone {
    Neutral,
    Default,
    Warning,
    Danger,
}

/// `sourceStatusTone`.
pub(super) fn source_status_tone(status: &str) -> Tone {
    match status {
        "healthy" => Tone::Default,
        "starting" | "degraded" => Tone::Warning,
        _ => Tone::Danger,
    }
}

/// `HealthSource`'s browser case: the desktop source reports it needs the desktop app.
pub(super) fn expected_outside_desktop(health: &SourceHealth) -> bool {
    health.status == "unavailable"
        && some(&health.last_error).and_then(Value::as_str).is_some_and(|e| e.contains("'web' mode"))
}

/// `shouldShowResourceMonitorRetry`.
pub(super) fn should_show_monitor_retry(native_status: Option<&str>, has_error: bool) -> bool {
    match native_status {
        None => has_error,
        Some(status) => matches!(status, "degraded" | "unavailable" | "stopped"),
    }
}

/// `resourceHistoryBarHeight`, in percent.
pub(super) fn bar_height(value: f64, max: f64, minimum_visible: f64) -> f64 {
    if value <= 0. {
        return 0.;
    }
    minimum_visible.max(value / max.max(1.) * 100.)
}

/// `Math.max(1, ...values)`.
pub(super) fn scale_max(values: impl Iterator<Item = f64>) -> f64 {
    values.fold(1., f64::max)
}

/// `formatRelativeElapsed` as one label ("just now", "5m ago").
pub(super) fn relative_label(iso: &str, now_ms: i64) -> Option<String> {
    Some(crate::app::pages::settings::ago_label(now_ms - crate::timestamp::parse_ms(iso)?))
}

// ---------------------------------------------------------------------------------------------
// Trees

/// `visibleProcessTree`: rows in depth-first order; a collapsed row hides the deeper rows after it.
pub(super) fn visible_process_tree<'a>(
    processes: &'a [ProcessEntry],
    collapsed: &HashSet<u64>,
) -> Vec<&'a ProcessEntry> {
    let mut visible = Vec::with_capacity(processes.len());
    let mut hidden_below: Option<u32> = None;
    for process in processes {
        if let Some(depth) = hidden_below {
            if process.depth > depth {
                continue;
            }
            hidden_below = None;
        }
        visible.push(process);
        if collapsed.contains(&process.pid) {
            hidden_below = Some(process.depth);
        }
    }
    visible
}

/// `visibleResourceTelemetryProcesses`: hides every descendant (by ppid) of a collapsed row.
pub(super) fn visible_telemetry_processes<'a>(
    processes: &'a [TelemetryProcess],
    collapsed: &HashSet<String>,
) -> Vec<&'a TelemetryProcess> {
    let mut children: HashMap<u64, Vec<&TelemetryProcess>> = HashMap::new();
    for process in processes {
        children.entry(process.ppid).or_default().push(process);
    }
    let mut hidden = HashSet::new();
    let mut stack: Vec<u64> =
        processes.iter().filter(|p| collapsed.contains(&p.identity.key())).map(|p| p.identity.pid).collect();
    while let Some(pid) = stack.pop() {
        for child in children.get(&pid).into_iter().flatten() {
            if hidden.insert(child.identity) {
                stack.push(child.identity.pid);
            }
        }
    }
    processes.iter().filter(|p| !hidden.contains(&p.identity)).collect()
}

/// The history table's indent: the root at 0, the shallowest child at 1.
pub(super) fn history_visual_depths(processes: &[ProcessHistoryEntry]) -> Vec<u32> {
    let shallowest = processes.iter().filter(|p| !p.is_server_root).map(|p| p.depth).min();
    processes
        .iter()
        .map(|p| match shallowest {
            Some(min) if !p.is_server_root => (p.depth - min + 1).max(1),
            _ => 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn formats_match_the_web() {
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(format_count(12), "12");
        assert_eq!(format_duration(12.4), "12 ms");
        assert_eq!(format_duration(1500.), "1.50 s");
        assert_eq!(format_duration(2000.), "2 s");
        assert_eq!(format_duration(12_340.), "12.3 s");
        assert_eq!(format_cpu_time(5.), "5s");
        assert_eq!(format_cpu_time(12.34), "12.3s");
        assert_eq!(format_cpu_time(90.), "1.50m");
        assert_eq!(format_cpu_time(7200.), "2h");
        assert_eq!(format_duration_micros(250.), "250 \u{b5}s");
        assert_eq!(format_duration_micros(1500.), "1.50 ms");
        assert_eq!(format_sample_interval(500.), "500 ms");
        assert_eq!(format_sample_interval(1000.), "1 second");
        assert_eq!(format_sample_interval(2500.), "2.5 seconds");
        assert_eq!(shorten_trace_id("abc"), "abc");
        assert_eq!(shorten_trace_id(&"0123456789".repeat(4)), "012345678901234567...0123456789");
    }

    #[test]
    fn names_and_types_come_from_the_command() {
        assert_eq!(process_name("\"C:\\nodejs\\node.exe\" server.js"), "node.exe");
        assert_eq!(process_name("/usr/bin/git status"), "git");
        assert_eq!(process_name("  "), "  ");
        assert_eq!(process_type(0, "/opt/bin/claude --print"), "Agent");
        assert_eq!(process_type(0, "/opt/bin/claudette"), "Process");
        assert_eq!(process_type(2, "codex"), "Subprocess");
        assert_eq!(telemetry_process_name("", "/bin/zsh -l"), "zsh");
        assert_eq!(telemetry_process_name("node", "/bin/zsh"), "node");
        assert_eq!(short_process_name(&"a".repeat(50)).len(), 42);
    }

    #[test]
    fn collapsing_a_row_hides_its_descendants() {
        let entry = |pid: u64, depth: u32| ProcessEntry { pid, depth, ..Default::default() };
        let processes = [entry(1, 0), entry(2, 1), entry(3, 2), entry(4, 1), entry(5, 0)];
        let pids = |collapsed: &[u64]| -> Vec<u64> {
            let collapsed = collapsed.iter().copied().collect();
            visible_process_tree(&processes, &collapsed).iter().map(|p| p.pid).collect()
        };
        assert_eq!(pids(&[]), [1, 2, 3, 4, 5]);
        assert_eq!(pids(&[2]), [1, 2, 4, 5]);
        assert_eq!(pids(&[1]), [1, 5]);

        let telemetry = |pid: u64, ppid: u64| TelemetryProcess {
            identity: Identity { pid, start_time_ms: 7 },
            ppid,
            ..Default::default()
        };
        let processes = [telemetry(1, 0), telemetry(2, 1), telemetry(3, 2), telemetry(4, 0)];
        let collapsed = HashSet::from(["1:7".to_owned()]);
        let visible: Vec<u64> =
            visible_telemetry_processes(&processes, &collapsed).iter().map(|p| p.identity.pid).collect();
        assert_eq!(visible, [1, 4]);
    }

    #[test]
    fn history_rows_indent_from_the_shallowest_child() {
        let entry = |depth: u32, root: bool| ProcessHistoryEntry { depth, is_server_root: root, ..Default::default() };
        assert_eq!(history_visual_depths(&[entry(0, true), entry(2, false), entry(3, false)]), [0, 1, 2]);
    }

    #[test]
    fn monitor_retry_and_bars_follow_the_web() {
        assert!(should_show_monitor_retry(None, true));
        assert!(!should_show_monitor_retry(None, false));
        assert!(should_show_monitor_retry(Some("stopped"), false));
        assert!(!should_show_monitor_retry(Some("healthy"), true));
        assert_eq!(bar_height(0., 10., 2.), 0.);
        assert_eq!(bar_height(0.01, 10., 2.), 2.);
        assert_eq!(bar_height(5., 10., 2.), 50.);
        assert_eq!(scale_max([0.2, 0.5].into_iter()), 1.);
    }

    #[test]
    fn options_and_health_decode_from_the_wire() {
        let trace: TraceDiagnostics = serde_json::from_value(json!({
            "recordCount": 3,
            "partialFailure": {"_tag": "Some", "value": true},
            "error": {"_tag": "Some", "value": {"kind": "trace-file-read-failed", "message": "boom"}},
        }))
        .unwrap();
        assert_eq!(trace.error_message(), Some("boom"));
        assert!(trace.partial());
        let health = SourceHealth {
            status: "unavailable".into(),
            last_error: json!({"_tag": "Some", "value": "Not available in 'web' mode"}),
        };
        assert!(expected_outside_desktop(&health));
        assert_eq!(source_status_tone("degraded"), Tone::Warning);
        assert_eq!(boolean_state("true", "Battery", "External power"), "Battery");
        assert_eq!(boolean_state("unknown", "a", "b"), "Unknown");
    }
}
