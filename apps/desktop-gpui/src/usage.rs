//! The Usage page's logic: the `server.getUsageSummary` read model and its merge into totals,
//! the period window, number and date formatting, the chart's scale and curves, and the
//! subscription-limit pools (`packages/shared/src/{usageMerge,usageFormat,usageLimits}.ts`,
//! `apps/web/src/components/usage/*`).

use std::collections::HashMap;

use chrono::{DateTime, Datelike as _, Days, NaiveDate, TimeZone, Timelike as _};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{Provider, UsageLimits, UsageWindow};
use crate::timestamp::{iso, parse_ms};

/// `USAGE_CONTRACT_VERSION` and `USAGE_MERGE_COMPATIBLE_SINCE`.
const CONTRACT_VERSION: f64 = 6.;
const COMPATIBLE_SINCE: f64 = 4.;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;

/// `PROVIDER_PRESENTATION` in order: kind, label, driver, fixed color (`None` follows the theme).
pub const PROVIDERS: &[(&str, &str, &str, Option<u32>)] = &[
    ("codex", "Codex", "codex", None),
    ("claude", "Claude Code", "claudeAgent", Some(0xd97757)),
    ("grok", "Grok Build", "grok", None),
    ("cursor", "Cursor", "cursor", Some(0x8b8b8b)),
    ("opencode", "OpenCode", "opencode", Some(0x5b9bbd)),
    ("antigravity", "Antigravity", "antigravity", Some(0x8c7bd1)),
    ("pi", "Pi", "pi", Some(0x4d9f8a)),
    ("muse", "Muse Code", "muse", Some(0x0866ff)),
];

pub fn provider_label(kind: &str) -> &str {
    PROVIDERS.iter().find(|p| p.0 == kind).map_or(kind, |p| p.1)
}

pub fn provider_driver(kind: &str) -> &str {
    PROVIDERS.iter().find(|p| p.0 == kind).map_or(kind, |p| p.2)
}

/// The usage kind a provider driver reports under.
pub fn driver_kind(driver: &str) -> Option<&'static str> {
    PROVIDERS.iter().find(|p| p.2 == driver).map(|p| p.0)
}

pub const METRICS: &[(&str, &str)] = &[("cost", "Cost"), ("tokens", "Tokens"), ("limits", "Limits")];
pub const WINDOWS: &[(u32, &str)] = &[(1, "Past 24h"), (7, "7 days"), (30, "30 days"), (90, "90 days")];

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenTotals {
    pub uncached_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
}

/// Token counts come from outside, so their sums stop at the maximum instead of overflowing.
fn sum<const N: usize>(counts: [u64; N]) -> u64 {
    counts.into_iter().fold(0, u64::saturating_add)
}

impl TokenTotals {
    /// Reasoning tokens are already part of the output count.
    fn total(&self) -> u64 {
        sum([self.uncached_input_tokens, self.cached_input_tokens, self.cache_creation_tokens, self.output_tokens])
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryCost {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    pub day: String,
    #[serde(default)]
    pub hour_start: Option<String>,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub source_path: Option<String>,
    pub totals: TokenTotals,
    pub cost_usd: f64,
    #[serde(default)]
    pub cache_savings_usd: f64,
    #[serde(default)]
    pub category_cost_usd: Option<CategoryCost>,
    #[serde(default)]
    pub fast_cost_usd: Option<f64>,
    #[serde(default)]
    pub ultrafast_cost_usd: Option<f64>,
    #[serde(default)]
    pub speed_premium_usd: Option<f64>,
    #[serde(default)]
    pub records: u64,
    #[serde(default)]
    pub unpriced_records: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    pub provider: String,
    pub resolved_home_path: String,
    /// With `volume_id`, tells apart machines sharing a host name and home path.
    #[serde(default)]
    pub host_id: Option<String>,
    #[serde(default)]
    pub volume_id: Option<String>,
}

impl Fingerprint {
    /// `fingerprintKey`: one usage directory, whichever machine read it.
    fn key(&self) -> String {
        let host = self.host_id.as_deref().unwrap_or_default();
        let volume = self.volume_id.as_deref().unwrap_or_default();
        format!("{host} {} {} {volume}", self.provider, self.resolved_home_path)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub fingerprint: Fingerprint,
    pub status: String,
    #[serde(default)]
    pub malformed_records: u64,
    #[serde(default)]
    pub distinct_sessions: u64,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub refreshing: Option<bool>,
}

/// `UsageSummary`; unknown bucket or source variants are skipped, as `ForwardCompatibleArray` does.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub contract_version: f64,
    /// When the server read its sources; the newest read of a shared directory counts it.
    #[serde(default)]
    pub read_at: Option<String>,
    #[serde(deserialize_with = "forward_compatible")]
    pub buckets: Vec<Bucket>,
    #[serde(deserialize_with = "forward_compatible")]
    pub sources: Vec<Source>,
}

fn forward_compatible<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values.into_iter().filter_map(|value| serde_json::from_value(value).ok()).collect())
}

impl Summary {
    pub fn compatible(&self) -> bool {
        (COMPATIBLE_SINCE..=CONTRACT_VERSION).contains(&self.contract_version)
    }

    /// A slow source answered from cache; the web asks again with `awaitRefresh`.
    pub fn refreshing(&self) -> bool {
        self.sources.iter().any(|source| source.refreshing == Some(true))
    }

    /// `usageSourceMessages`: what the sources say about gaps in their counts.
    pub fn source_messages(&self, hidden: &[String]) -> Vec<String> {
        let mut messages: Vec<String> = Vec::new();
        let mut malformed: HashMap<String, u64> = HashMap::new();
        for source in self.sources.iter().filter(|s| !hidden.contains(&s.fingerprint.provider)) {
            let reports = matches!(source.status.as_str(), "partial" | "failed")
                || source.fingerprint.provider == "cursor"
                || (source.status == "missing" && source.malformed_records > 0);
            if source.action.is_some() || !reports {
                continue;
            }
            let mut message = source.message.clone();
            if source.malformed_records > 0 {
                let key = format!("{}\u{0}{}", source.fingerprint.provider, source.fingerprint.resolved_home_path);
                let held = malformed.entry(key).or_default();
                *held = (*held).max(source.malformed_records);
                let warning = malformed_warning(source.malformed_records);
                if let Some(text) = &message
                    && let Some(rest) = text.strip_suffix(&warning)
                {
                    message = Some(rest.trim().to_owned());
                }
            }
            if let Some(message) = message.filter(|m| !m.is_empty())
                && !messages.contains(&message)
            {
                messages.push(message);
            }
        }
        let skipped: u64 = malformed.values().sum();
        if skipped > 0 {
            messages.push(malformed_warning(skipped));
        }
        messages
    }
}

fn malformed_warning(count: u64) -> String {
    format!("{count} malformed or inconsistent usage record(s) were skipped; usage may be incomplete.")
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderTotals {
    pub provider: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub sessions: u64,
    pub cost_share: f64,
    pub token_share: f64,
}

#[derive(Clone, Debug, Default)]
pub struct ModelTotals {
    pub provider: String,
    pub model: String,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub records: u64,
    pub unpriced_records: u64,
    pub cost_share: f64,
    pub token_share: f64,
}

impl ModelTotals {
    /// Every record lacks rates, so the cost is unknown rather than zero.
    pub fn cost_unknown(&self) -> bool {
        self.records > 0 && self.unpriced_records >= self.records
    }

    /// `modelShare`: an unknown cost has no cost share.
    pub fn share(&self, tokens: bool) -> Option<f64> {
        if tokens { Some(self.token_share) } else { (!self.cost_unknown()).then_some(self.cost_share) }
    }
}

/// One day, or one hour when `hour_ms` is set.
#[derive(Clone, Debug, Default)]
pub struct Period {
    pub day: String,
    pub hour_ms: Option<i64>,
    pub cost_usd: f64,
    pub total_tokens: u64,
    pub by_provider: HashMap<String, (f64, u64)>,
}

impl Period {
    pub fn value(&self, provider: &str, tokens: bool) -> f64 {
        self.by_provider.get(provider).map_or(0., |(cost, count)| if tokens { *count as f64 } else { *cost })
    }

    fn add(&mut self, provider: &str, cost: f64, tokens: u64) {
        self.cost_usd += cost;
        self.total_tokens = self.total_tokens.saturating_add(tokens);
        let entry = self.by_provider.entry(provider.to_owned()).or_default();
        entry.0 += cost;
        entry.1 = entry.1.saturating_add(tokens);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SpeedCost {
    pub standard: f64,
    pub fast: f64,
    pub ultrafast: f64,
    pub premium: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Merged {
    pub cost_usd: f64,
    pub uncached_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub sessions: u64,
    pub unpriced_share: f64,
    pub cache_savings_usd: f64,
    /// Input, cache read, cache write, output, and what no category explains.
    pub category: [f64; 5],
    pub speed: SpeedCost,
    pub providers: Vec<ProviderTotals>,
    pub models: Vec<ModelTotals>,
    pub daily: Vec<Period>,
    pub hourly: Vec<Period>,
}

/// `mergeUsage` for one environment, without `hidden` providers: each scanned source counts its
/// buckets and distinct sessions once (bucket session counts double-count sessions spanning days).
/// One machine's answer to `server.getUsageSummary`.
pub struct EnvironmentUsage {
    pub environment_id: String,
    pub summary: Summary,
}

/// `claimSources` and `ownedContribution` of `mergeUsage`: every machine's answer as one summary
/// in which a usage directory read by several machines counts once, for the newest healthy read.
/// Incompatible answers are left out. The web's top-up from newer partial scans is not ported.
pub fn combine(environments: Vec<EnvironmentUsage>) -> Option<Summary> {
    let mut current: Vec<EnvironmentUsage> = environments.into_iter().filter(|e| e.summary.compatible()).collect();
    if current.is_empty() {
        return None;
    }
    let read_at = |e: &EnvironmentUsage| e.summary.read_at.as_deref().and_then(crate::timestamp::parse_ms).unwrap_or(0);
    current.sort_by(|a, b| read_at(b).cmp(&read_at(a)).then_with(|| a.environment_id.cmp(&b.environment_id)));
    let mut owners: HashMap<String, usize> = HashMap::new();
    for status in ["ok", "partial", "failed"] {
        for (index, environment) in current.iter().enumerate() {
            for source in environment.summary.sources.iter().filter(|s| s.status == status) {
                owners.entry(source.fingerprint.key()).or_insert(index);
            }
        }
    }
    let mut combined =
        Summary { contract_version: CONTRACT_VERSION, read_at: None, buckets: Vec::new(), sources: Vec::new() };
    for (index, environment) in current.into_iter().enumerate() {
        let Summary { buckets, sources, .. } = environment.summary;
        let owned: Vec<Source> = sources
            .into_iter()
            .filter(|s| s.status != "missing" && owners.get(&s.fingerprint.key()) == Some(&index))
            .collect();
        let owned_provider = |provider: &str| owned.iter().any(|s| s.fingerprint.provider == provider);
        let owned_path = |provider: &str, path: &str| {
            owned.iter().any(|s| s.fingerprint.provider == provider && s.fingerprint.resolved_home_path == path)
        };
        combined.buckets.extend(buckets.into_iter().filter(|bucket| match &bucket.source_path {
            None => owned_provider(&bucket.provider),
            Some(path) => owned_path(&bucket.provider, path),
        }));
        combined.sources.extend(owned);
    }
    Some(combined)
}

pub fn merge(summary: &Summary, hidden: &[String]) -> Merged {
    let mut merged = Merged::default();
    if !summary.compatible() {
        return merged;
    }
    let visible = |provider: &str| !hidden.iter().any(|h| h == provider);
    let mut owned_providers: Vec<&str> = Vec::new();
    let mut owned_sources: Vec<(&str, &str)> = Vec::new();
    let mut providers: Vec<ProviderTotals> = Vec::new();
    fn provider_slot<'a>(providers: &'a mut Vec<ProviderTotals>, kind: &str) -> &'a mut ProviderTotals {
        let index = match providers.iter().position(|p| p.provider == kind) {
            Some(index) => index,
            None => {
                providers.push(ProviderTotals { provider: kind.to_owned(), ..ProviderTotals::default() });
                providers.len() - 1
            }
        };
        &mut providers[index]
    }
    for source in &summary.sources {
        let provider = source.fingerprint.provider.as_str();
        if source.status == "missing" || !visible(provider) {
            continue;
        }
        owned_providers.push(provider);
        owned_sources.push((provider, source.fingerprint.resolved_home_path.as_str()));
        merged.sessions = merged.sessions.saturating_add(source.distinct_sessions);
        if source.distinct_sessions > 0 {
            let slot = provider_slot(&mut providers, provider);
            slot.sessions = slot.sessions.saturating_add(source.distinct_sessions);
        }
    }
    let mut records: u64 = 0;
    let mut unpriced: u64 = 0;
    let mut categories = CategoryCost::default();
    let mut models: Vec<ModelTotals> = Vec::new();
    let mut daily: HashMap<String, Period> = HashMap::new();
    let mut hourly: HashMap<i64, Period> = HashMap::new();
    let owned = |bucket: &Bucket| match &bucket.source_path {
        None => owned_providers.contains(&bucket.provider.as_str()),
        Some(path) => owned_sources.contains(&(bucket.provider.as_str(), path.as_str())),
    };
    for bucket in summary.buckets.iter().filter(|b| owned(b)) {
        let tokens = bucket.totals.total();
        merged.cost_usd += bucket.cost_usd;
        merged.cache_savings_usd += bucket.cache_savings_usd;
        merged.uncached_input_tokens = merged.uncached_input_tokens.saturating_add(bucket.totals.uncached_input_tokens);
        merged.cached_input_tokens = merged.cached_input_tokens.saturating_add(bucket.totals.cached_input_tokens);
        merged.cache_creation_tokens = merged.cache_creation_tokens.saturating_add(bucket.totals.cache_creation_tokens);
        merged.output_tokens = merged.output_tokens.saturating_add(bucket.totals.output_tokens);
        records = records.saturating_add(bucket.records);
        unpriced = unpriced.saturating_add(bucket.unpriced_records);
        if let Some(cost) = bucket.category_cost_usd {
            categories.input += cost.input;
            categories.cache_read += cost.cache_read;
            categories.cache_write += cost.cache_write;
            categories.output += cost.output;
        }
        merged.speed.fast += bucket.fast_cost_usd.unwrap_or(0.);
        merged.speed.ultrafast += bucket.ultrafast_cost_usd.unwrap_or(0.);
        merged.speed.premium += bucket.speed_premium_usd.unwrap_or(0.);
        let provider = provider_slot(&mut providers, &bucket.provider);
        provider.cost_usd += bucket.cost_usd;
        provider.total_tokens = provider.total_tokens.saturating_add(tokens);
        let model = match models.iter().position(|m| m.provider == bucket.provider && m.model == bucket.model) {
            Some(index) => &mut models[index],
            None => {
                models.push(ModelTotals {
                    provider: bucket.provider.clone(),
                    model: bucket.model.clone(),
                    ..ModelTotals::default()
                });
                models.last_mut().expect("just pushed")
            }
        };
        model.cost_usd += bucket.cost_usd;
        model.total_tokens = model.total_tokens.saturating_add(tokens);
        model.records = model.records.saturating_add(bucket.records);
        model.unpriced_records = model.unpriced_records.saturating_add(bucket.unpriced_records);
        daily.entry(bucket.day.clone()).or_insert_with(|| Period { day: bucket.day.clone(), ..Period::default() }).add(
            &bucket.provider,
            bucket.cost_usd,
            tokens,
        );
        if let Some(hour_ms) = bucket.hour_start.as_deref().and_then(parse_ms) {
            hourly
                .entry(hour_ms)
                .or_insert_with(|| Period { day: bucket.day.clone(), hour_ms: Some(hour_ms), ..Period::default() })
                .add(&bucket.provider, bucket.cost_usd, tokens);
        }
    }
    merged.total_tokens = sum([
        merged.uncached_input_tokens,
        merged.cached_input_tokens,
        merged.cache_creation_tokens,
        merged.output_tokens,
    ]);
    let (cost, total) = (merged.cost_usd, merged.total_tokens);
    let share = |part: f64, whole: f64| if whole == 0. { 0. } else { part / whole };
    for provider in &mut providers {
        provider.cost_share = share(provider.cost_usd, cost);
        provider.token_share = share(provider.total_tokens as f64, total as f64);
    }
    providers.sort_by(|l, r| r.cost_usd.total_cmp(&l.cost_usd));
    for model in &mut models {
        model.cost_share = share(model.cost_usd, cost);
        model.token_share = share(model.total_tokens as f64, total as f64);
    }
    models.sort_by(|l, r| r.cost_usd.total_cmp(&l.cost_usd).then(r.total_tokens.cmp(&l.total_tokens)));
    merged.unpriced_share = share(unpriced as f64, records as f64);
    merged.category = [
        categories.input,
        categories.cache_read,
        categories.cache_write,
        categories.output,
        (cost - categories.input - categories.cache_read - categories.cache_write - categories.output).max(0.),
    ];
    merged.speed.standard = (cost - merged.speed.fast - merged.speed.ultrafast).max(0.);
    merged.providers = providers;
    merged.models = models;
    merged.daily = daily.into_values().collect();
    merged.daily.sort_by(|l, r| l.day.cmp(&r.day));
    merged.hourly = hourly.into_values().collect();
    merged.hourly.sort_by_key(|p| p.hour_ms);
    merged
}

/// `providersWithUsage`: providers with any tokens or cost, in presentation order.
pub fn active_providers(merged: &Merged) -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .map(|p| p.0)
        .filter(|kind| merged.providers.iter().any(|t| t.provider == *kind && (t.total_tokens > 0 || t.cost_usd > 0.)))
        .collect()
}

/// `sortModelsByTokens`.
pub fn models_by_tokens(models: &[ModelTotals]) -> Vec<&ModelTotals> {
    let mut sorted: Vec<&ModelTotals> = models.iter().collect();
    sorted.sort_by(|l, r| r.total_tokens.cmp(&l.total_tokens).then(r.cost_usd.total_cmp(&l.cost_usd)));
    sorted
}

/// The period a usage read covers (`makeWindow`), in the viewer's IANA zone.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub days: u32,
    pub since_day: NaiveDate,
    pub until_day: NaiveDate,
    pub time_zone: String,
    /// Past 24h reads by the hour, ending on the minute.
    pub hours: Option<(i64, i64)>,
}

impl Window {
    /// `time_zone` is `tz`'s IANA name, which the server buckets days by.
    pub fn new<Tz: TimeZone>(days: u32, now_ms: i64, tz: &Tz, time_zone: String) -> Self {
        let day_of = |ms: i64| zoned(ms, tz).map(|time| time.date_naive());
        let until_day = day_of(now_ms).unwrap_or_default();
        if days == 1 {
            let until = now_ms.div_euclid(MINUTE) * MINUTE;
            let since = until - DAY;
            return Self {
                days,
                since_day: day_of(since).unwrap_or(until_day),
                until_day,
                time_zone,
                hours: Some((since, until)),
            };
        }
        let since_day = until_day.checked_sub_days(Days::new(u64::from(days.saturating_sub(1)))).unwrap_or(until_day);
        Self { days, since_day, until_day, time_zone, hours: None }
    }

    pub fn to_json(&self) -> Value {
        let mut input = json!({
            "sinceDay": self.since_day.to_string(),
            "untilDay": self.until_day.to_string(),
            "timeZone": self.time_zone,
            "resolution": if self.hours.is_some() { "hour" } else { "day" },
        });
        if let Some((since, until)) = self.hours {
            input["sinceTime"] = json!(iso(since).unwrap_or_default());
            input["untilTime"] = json!(iso(until).unwrap_or_default());
        }
        input
    }

    /// `enumerateDays` / `enumerateHourStarts`: every period the chart has a column for.
    pub fn periods(&self) -> Vec<PeriodKey> {
        match self.hours {
            Some((since, until)) => {
                (0..).map(|i| since + i * HOUR).take_while(|t| *t < until).map(PeriodKey::Hour).collect()
            }
            None => {
                let mut days = Vec::new();
                let mut day = self.since_day;
                while day <= self.until_day {
                    days.push(PeriodKey::Day(day.to_string()));
                    match day.succ_opt() {
                        Some(next) => day = next,
                        None => break,
                    }
                }
                days
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum PeriodKey {
    Day(String),
    Hour(i64),
}

impl PeriodKey {
    pub fn find<'a>(&self, merged: &'a Merged) -> Option<&'a Period> {
        match self {
            PeriodKey::Day(day) => merged.daily.iter().find(|p| &p.day == day),
            PeriodKey::Hour(ms) => merged.hourly.iter().find(|p| p.hour_ms == Some(*ms)),
        }
    }

    pub fn label<Tz: TimeZone>(&self, tz: &Tz) -> String {
        match self {
            PeriodKey::Day(day) => format_day_short(day),
            PeriodKey::Hour(ms) => format_hour_short(*ms, tz),
        }
    }
}

fn zoned<Tz: TimeZone>(ms: i64, tz: &Tz) -> Option<DateTime<Tz>> {
    Some(DateTime::from_timestamp_millis(ms)?.with_timezone(tz))
}

/// "3 PM".
fn hour_label<Tz: TimeZone>(time: &DateTime<Tz>) -> String {
    let (pm, hour) = time.hour12();
    format!("{hour} {}", if pm { "PM" } else { "AM" })
}

fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `Intl.NumberFormat("en-US", {style: "currency", currency: "USD"})`.
pub fn format_usd(value: f64) -> String {
    let cents = (value.abs() * 100.).round() as u64;
    let sign = if value < 0. && cents > 0 { "-" } else { "" };
    format!("{sign}${}.{:02}", thousands(cents / 100), cents % 100)
}

pub fn format_count(value: f64) -> String {
    thousands(value.round().max(0.) as u64)
}

/// `formatTokens`: 1.25K, 12.5M, 125B.
pub fn format_tokens(value: f64) -> String {
    let trim = |v: f64| {
        let digits = if v.abs() >= 100. {
            0
        } else if v.abs() >= 10. {
            1
        } else {
            2
        };
        let text = format!("{v:.digits$}");
        // `.replace(/\.0+$/, "")`: only all-zero decimals go.
        match text.split_once('.') {
            Some((whole, fraction)) if fraction.chars().all(|c| c == '0') => whole.to_owned(),
            _ => text,
        }
    };
    let abs = value.abs();
    for (scale, suffix) in [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")] {
        if abs >= scale {
            return format!("{}{suffix}", trim(value / scale));
        }
    }
    format_count(value)
}

/// `formatPercent`: one decimal, and `<0.1%` for a sliver.
pub fn format_percent(share: f64) -> String {
    let percent = share * 100.;
    if percent > 0. && percent < 0.1 {
        return "<0.1%".into();
    }
    format!("{percent:.1}%")
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `formatDayShort`: `2026-10-05` is "Oct 5".
pub fn format_day_short(day: &str) -> String {
    let mut parts = day.split('-').map(|p| p.parse::<usize>().ok());
    match (parts.next().flatten(), parts.next().flatten(), parts.next().flatten()) {
        (Some(_), Some(month @ 1..=12), Some(day_of_month)) => format!("{} {day_of_month}", MONTHS[month - 1]),
        _ => day.to_owned(),
    }
}

/// `formatHourShort` with a 12-hour clock: "3 PM".
pub fn format_hour_short<Tz: TimeZone>(ms: i64, tz: &Tz) -> String {
    zoned(ms, tz).map(|time| hour_label(&time)).unwrap_or_default()
}

/// `formatDateTimeShort`: "Nov 4, 3 PM".
pub fn format_date_time_short<Tz: TimeZone>(ms: i64, tz: &Tz) -> String {
    let Some(time) = zoned(ms, tz) else { return String::new() };
    format!("{} {}, {}", MONTHS[time.month0() as usize], time.day(), hour_label(&time))
}

/// `formatRelativeHourShort`: "3 PM today", "3 PM yesterday", else the date and hour.
pub fn format_relative_hour_short<Tz: TimeZone>(ms: i64, reference_ms: i64, tz: &Tz) -> String {
    let day = |ms: i64| zoned(ms, tz).map(|time| time.date_naive());
    let (Some(at), Some(reference)) = (day(ms), day(reference_ms)) else { return format_date_time_short(ms, tz) };
    match (reference - at).num_days() {
        0 => format!("{} today", format_hour_short(ms, tz)),
        1 => format!("{} yesterday", format_hour_short(ms, tz)),
        _ => format_date_time_short(ms, tz),
    }
}

/// The chart's 960x260 view box, which stretches to its 224px-tall plot.
pub const VIEW_WIDTH: f32 = 960.;
pub const VIEW_HEIGHT: f32 = 260.;
pub const PLOT_TOP: f32 = 8.;
const TICK_COUNT: f64 = 4.;

/// `niceScale`: a 1-2-5 step that puts about four gridlines under the peak.
pub fn nice_scale(peak: f64) -> (f64, Vec<f64>) {
    if peak <= 0. {
        return (0., vec![0.]);
    }
    let raw = peak / TICK_COUNT;
    let magnitude = 10f64.powf(raw.log10().floor());
    let normalized = raw / magnitude;
    let step = magnitude
        * if normalized > 5. {
            10.
        } else if normalized > 2. {
            5.
        } else if normalized > 1. {
            2.
        } else {
            1.
        };
    let max = (peak / step).ceil() * step;
    let ticks = (0..).map(|i| f64::from(i) * step).take_while(|v| *v <= max + step * 1e-6).collect();
    (max, ticks)
}

pub fn value_to_y(value: f64, max: f64) -> f32 {
    if max == 0. { VIEW_HEIGHT } else { VIEW_HEIGHT - (value / max) as f32 * (VIEW_HEIGHT - PLOT_TOP) }
}

/// One cubic segment: from, first control, second control, to.
pub type Segment = [(f32, f32); 4];

/// `smoothCurve`: monotone cubic interpolation, so a curve never overshoots its points.
pub fn smooth_curve(points: &[(f32, f32)]) -> Vec<Segment> {
    let count = points.len();
    if count < 2 {
        return Vec::new();
    }
    let slopes: Vec<f32> = points
        .windows(2)
        .map(|pair| {
            let dx = pair[1].0 - pair[0].0;
            if dx == 0. { 0. } else { (pair[1].1 - pair[0].1) / dx }
        })
        .collect();
    let mut tangents = vec![0f32; count];
    tangents[0] = slopes[0];
    tangents[count - 1] = slopes[count - 2];
    for index in 1..count - 1 {
        let (previous, next) = (slopes[index - 1], slopes[index]);
        tangents[index] = if previous * next <= 0. { 0. } else { (previous + next) / 2. };
    }
    for (index, &slope) in slopes.iter().enumerate() {
        if slope == 0. {
            tangents[index] = 0.;
            tangents[index + 1] = 0.;
            continue;
        }
        let a = tangents[index] / slope;
        let b = tangents[index + 1] / slope;
        let magnitude = a * a + b * b;
        if magnitude > 9. {
            let scale = 3. / magnitude.sqrt();
            tangents[index] = scale * a * slope;
            tangents[index + 1] = scale * b * slope;
        }
    }
    points
        .windows(2)
        .enumerate()
        .map(|(index, pair)| {
            let (from, to) = (pair[0], pair[1]);
            let dx = to.0 - from.0;
            [
                from,
                (from.0 + dx / 3., from.1 + tangents[index] * dx / 3.),
                (to.0 - dx / 3., to.1 - tangents[index + 1] * dx / 3.),
                to,
            ]
        })
        .collect()
}

// Subscription limits.

pub fn remaining_percent(window: &UsageWindow) -> u32 {
    window.remaining_percent()
}

fn reset_ms(window: &UsageWindow) -> Option<i64> {
    window.resets_at.as_deref().and_then(parse_ms)
}

/// How much of the window has elapsed, when it has a reset clock and a length.
pub fn elapsed_share(window: &UsageWindow, now: i64) -> Option<f64> {
    let resets = reset_ms(window)?;
    let length = window.window_duration_mins? * MINUTE as f64;
    (length > 0.).then(|| ((length - (resets - now) as f64) / length).clamp(0., 1.))
}

/// Bars show quota remaining, so even pace is the share of time remaining.
pub fn pace_remaining_percent(window: &UsageWindow, now: i64) -> Option<u32> {
    elapsed_share(window, now).map(|elapsed| ((1. - elapsed) * 100.).round() as u32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pace {
    Ahead,
    On,
    Under,
}

impl Pace {
    pub fn label(self) -> &'static str {
        match self {
            Pace::Ahead => "Ahead of pace: spending faster than the window elapses",
            Pace::On => "On pace with the window",
            Pace::Under => "Under pace: headroom left for the rest of the window",
        }
    }
}

fn pace_of_shares(used_percent: f64, elapsed: f64) -> Pace {
    let gap = used_percent - elapsed * 100.;
    if gap > 5. {
        Pace::Ahead
    } else if gap < -5. {
        Pace::Under
    } else {
        Pace::On
    }
}

/// `formatDuration`: "2d 3h", "4h 5m", "6m".
pub fn format_duration(ms: i64) -> String {
    let remaining = ms.max(0);
    let (days, hours, minutes) = (remaining / DAY, remaining % DAY / HOUR, remaining % HOUR / MINUTE);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

pub fn format_resets_in(window: &UsageWindow, now: i64) -> Option<String> {
    let resets = reset_ms(window)?;
    Some(if resets <= now {
        "resets now".into()
    } else if resets - now < MINUTE {
        "Resets momentarily".into()
    } else {
        format!("resets in {}", format_duration(resets - now))
    })
}

/// `limitsNotice`: why an account shows no bars.
pub fn limits_notice(limits: &UsageLimits) -> Option<String> {
    let reason = limits.unavailable.as_ref().and_then(|u| u.get("reason")).and_then(Value::as_str);
    let message = limits.unavailable.as_ref().and_then(|u| u.get("message")).and_then(Value::as_str);
    match reason {
        Some("unsupported") => Some(message.unwrap_or("This account has no subscription limits.").into()),
        Some("probeFailed") => Some(message.unwrap_or("Could not read limits.").into()),
        _ if limits.windows.is_empty() => Some("No limits reported.".into()),
        _ => None,
    }
}

/// One signed-in account whose limits pool with others on the same driver.
#[derive(Clone, Debug)]
pub struct Account {
    pub key: String,
    pub driver: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub accent_color: Option<String>,
    pub limits: UsageLimits,
    /// The environments signed in to it, as `(id, label)`; empty when only a source reports it.
    pub environments: Vec<(String, String)>,
    /// The usage-limit source that reports it, when no provider here is signed in to it.
    pub source_label: Option<String>,
    /// The environment to send `provider.consumeResetCredit` to, and the input that spends
    /// its next reset credit.
    pub redeem: Option<(String, Value)>,
}

/// One selected environment as the Limits view reads it (`LimitPresentations`' entry).
pub struct LimitEnvironment<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub providers: &'a [Provider],
    /// Its `config.usageLimitSources`.
    pub sources: &'a Value,
}

impl Account {
    pub fn name(&self) -> String {
        self.display_name.clone().or_else(|| self.email.clone()).unwrap_or_else(|| self.driver.clone())
    }
}

/// `PROVIDER_DISPLAY_NAMES`.
pub fn provider_display_name(driver: &str) -> &str {
    match driver {
        "antigravity" => "Antigravity",
        "codex" => "Codex",
        "claudeAgent" => "Claude",
        "cursor" => "Cursor",
        "grok" => "Grok",
        "fx" => "fx",
        "kiro" => "Kiro",
        "muse" => "Muse Code",
        "acpRegistry" => "ACP Registry",
        "pi" => "Pi",
        "opencode" => "OpenCode",
        other => other,
    }
}

fn reports_limits(provider: &Provider) -> bool {
    provider.enabled
        && provider.installed
        && provider.availability.as_deref() != Some("unavailable")
        && provider.usage_limits.is_some()
}

/// One account a usage-limit source (a hub) reports (`UsageLimitSourceAccount`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HubAccount {
    id: String,
    driver: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    usage_limits: UsageLimits,
}

/// `accountKey`: the login, else the credential, per driver.
fn base_key(driver: &str, email: Option<&str>, limits: Option<&UsageLimits>) -> Option<String> {
    match email.map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()) {
        Some(email) => Some(format!("{driver}:{email}")),
        None => limits
            .and_then(|l| l.credential_fingerprint.as_ref())
            .map(|fingerprint| format!("{driver}:credential:{fingerprint}")),
    }
}

/// `accountKeyResolver`: the same login on two instances or a hub is one account; a login with
/// several workspaces is one account per workspace.
fn resolve_key(
    base: Option<String>,
    workspace: Option<&str>,
    workspaces: &HashMap<String, Vec<String>>,
) -> Option<String> {
    let base = base?;
    if let Some(workspace) = workspace {
        return Some(format!("{base}:{workspace}"));
    }
    match workspaces.get(&base).map(Vec::as_slice) {
        None => Some(base),
        Some([only]) => Some(format!("{base}:{only}")),
        Some(_) => None,
    }
}

/// An account and what a merge keeps from its sources (`collectLimitAccounts`' `merge`).
struct Merging {
    key: String,
    account: Account,
    /// Some provider here is signed in to it, so it names no source.
    signed_in: bool,
    credits: Option<(i64, Option<crate::model::ResetCredits>, Option<(String, Value)>)>,
    hub_redeem: Option<(i64, (String, Value))>,
    plan: Option<(i64, String)>,
}

fn checked_ms(account: &Account) -> i64 {
    account.limits.checked_at.as_deref().and_then(parse_ms).unwrap_or(0)
}

/// `collectLimitAccounts` across the selected environments: their providers, then the hub
/// accounts their usage-limit sources (`config.usageLimitSources`) report.
pub fn limit_accounts(environments: &[LimitEnvironment<'_>], hidden_drivers: &[&str]) -> Vec<Account> {
    let visible = |driver: &str| !hidden_drivers.contains(&driver);
    let label_environment = environments.len() > 1;
    let mut providers: Vec<(&LimitEnvironment<'_>, &Provider)> = Vec::new();
    let mut hubs: Vec<(&LimitEnvironment<'_>, String, String, HubAccount)> = Vec::new();
    for environment in environments {
        providers.extend(environment.providers.iter().filter(|p| visible(&p.driver)).map(|p| (environment, p)));
        for source in environment.sources.as_array().into_iter().flatten() {
            let id = source["id"].as_str().unwrap_or_default().to_owned();
            let label = source["label"].as_str().unwrap_or_default();
            let label = if label_environment { format!("{} · {label}", environment.label) } else { label.to_owned() };
            hubs.extend(
                source["accounts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|account| HubAccount::deserialize(account).ok())
                    .filter(|account| visible(&account.driver))
                    .map(|account| (environment, id.clone(), label.clone(), account)),
            );
        }
    }
    let mut workspaces: HashMap<String, Vec<String>> = HashMap::new();
    let reports = providers
        .iter()
        .filter(|(_, p)| p.enabled)
        .map(|(_, p)| {
            (base_key(&p.driver, p.auth.email.as_deref(), p.usage_limits.as_ref()), p.auth.workspace_id.as_deref())
        })
        .chain(hubs.iter().map(|(_, _, _, a)| {
            (base_key(&a.driver, a.email.as_deref(), Some(&a.usage_limits)), a.workspace_id.as_deref())
        }));
    for (base, workspace) in reports {
        if let (Some(base), Some(workspace)) = (base, workspace) {
            let known = workspaces.entry(base).or_default();
            if !known.iter().any(|w| w == workspace) {
                known.push(workspace.to_owned());
            }
        }
    }
    let mut candidates: Vec<(String, Account, bool)> = Vec::new();
    for (environment, provider) in providers.into_iter().filter(|(_, p)| reports_limits(p)) {
        let Some(limits) = provider.usage_limits.clone() else { continue };
        if limits_notice(&limits).is_some() {
            continue;
        }
        let auth = &provider.auth;
        let own_key = format!("{}:{}", environment.id, provider.instance_id);
        let base = base_key(&provider.driver, auth.email.as_deref(), Some(&limits));
        let key = resolve_key(base, auth.workspace_id.as_deref(), &workspaces).unwrap_or_else(|| own_key.clone());
        let account = Account {
            key: own_key,
            driver: provider.driver.clone(),
            display_name: provider.display_name.as_ref().map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()),
            email: auth.email.clone(),
            plan: auth.label.clone(),
            accent_color: provider.accent_color.clone(),
            limits,
            environments: vec![(environment.id.to_owned(), environment.label.to_owned())],
            source_label: None,
            redeem: Some((environment.id.to_owned(), json!({"instanceId": provider.instance_id}))),
        };
        candidates.push((key, account, true));
    }
    for (environment, source_id, source_label, hub) in hubs {
        if limits_notice(&hub.usage_limits).is_some() {
            continue;
        }
        let own_key = match &hub.workspace_id {
            Some(workspace) => format!("{source_id}:{}:{workspace}", hub.id),
            None => format!("{source_id}:{}", hub.id),
        };
        let base = base_key(&hub.driver, hub.email.as_deref(), Some(&hub.usage_limits));
        let key = resolve_key(base, hub.workspace_id.as_deref(), &workspaces).unwrap_or_else(|| own_key.clone());
        let redeem = hub.usage_limits.reset_credits.as_ref().and_then(|c| c.next_credit_id.clone()).map(|credit| {
            (environment.id.to_owned(), json!({"sourceId": source_id, "accountId": hub.id, "creditId": credit}))
        });
        let name = hub.id.strip_suffix(".json").or_else(|| hub.id.strip_suffix(".JSON")).unwrap_or(&hub.id).to_owned();
        let account = Account {
            key: own_key,
            driver: hub.driver,
            display_name: hub.email.is_none().then_some(name),
            email: hub.email,
            plan: hub.plan,
            accent_color: None,
            limits: hub.usage_limits,
            environments: Vec::new(),
            source_label: Some(source_label),
            redeem,
        };
        candidates.push((key, account, false));
    }
    let mut merged: Vec<Merging> = Vec::new();
    for (key, next, signed_in) in candidates {
        let at = checked_ms(&next);
        let hub_redeem = next.redeem.clone().filter(|(_, r)| r.get("sourceId").is_some()).map(|r| (at, r));
        let credits =
            next.limits.reset_credits.is_some().then(|| (at, next.limits.reset_credits.clone(), next.redeem.clone()));
        let plan = next.plan.clone().map(|plan| (at, plan));
        let Some(group) = merged.iter_mut().find(|m| m.key == key) else {
            let account = Account { key: format!("{key}\u{0}{}", next.key), ..next };
            merged.push(Merging { key, account, signed_in, credits, hub_redeem, plan });
            continue;
        };
        // Each part comes from the freshest account that has it.
        if hub_redeem.is_some() && group.hub_redeem.as_ref().is_none_or(|(seen, _)| at > *seen) {
            group.hub_redeem = hub_redeem;
        }
        if credits.is_some() && group.credits.as_ref().is_none_or(|(seen, ..)| at > *seen) {
            group.credits = credits;
        }
        if plan.is_some() && group.plan.as_ref().is_none_or(|(seen, _)| at > *seen) {
            group.plan = plan;
        }
        let previous = &mut group.account;
        let winner_is_next = at > checked_ms(previous);
        // A hub e-mail names the account better than the driver's default instance name.
        let hub_email = [&*previous, &next].iter().any(|a| a.source_label.is_some() && a.email.is_some());
        let display_name = previous.display_name.take().or(next.display_name.clone());
        previous.display_name = display_name.filter(|name| {
            !(hub_email && name.to_lowercase() == provider_display_name(&previous.driver).to_lowercase())
        });
        previous.plan =
            group.plan.as_ref().map(|(_, plan)| plan.clone()).or(previous.plan.take()).or(next.plan.clone());
        previous.accent_color = previous.accent_color.take().or(next.accent_color.clone());
        for environment in &next.environments {
            if !previous.environments.iter().any(|(id, _)| *id == environment.0) {
                previous.environments.push(environment.clone());
            }
        }
        group.signed_in |= signed_in;
        previous.source_label =
            if group.signed_in { None } else { previous.source_label.take().or(next.source_label.clone()) };
        let winner_redeem = if winner_is_next { next.redeem.clone() } else { previous.redeem.clone() };
        previous.redeem = match (&group.hub_redeem, &group.credits) {
            (Some((_, redeem)), _) => Some(redeem.clone()),
            (None, Some((_, _, redeem))) => redeem.clone(),
            (None, None) => winner_redeem.or(previous.redeem.take()).or(next.redeem.clone()),
        };
        if winner_is_next {
            previous.limits = next.limits;
        }
        previous.limits.reset_credits = group.credits.as_ref().and_then(|(_, credits, _)| credits.clone());
    }
    merged.into_iter().map(|m| m.account).collect()
}

/// `resetCreditsSummary`: "2 reset credits banked · next expires in 3d", or compact "2 banked · expires in 3d".
pub fn reset_credits_summary(credits: &crate::model::ResetCredits, now: i64, compact: bool) -> String {
    let expires = credits.next_expires_at.as_deref().and_then(parse_ms).map(|at| format_duration(at - now));
    if credits.available_count == 0 {
        return "No reset credits banked".into();
    }
    let n = credits.available_count;
    match (compact, expires) {
        (true, Some(expires)) => format!("{n} banked · expires in {expires}"),
        (true, None) => format!("{n} banked"),
        (false, expires) => format!(
            "{n} {} banked{}",
            if n == 1 { "reset credit" } else { "reset credits" },
            expires.map(|e| format!(" · next expires in {e}")).unwrap_or_default()
        ),
    }
}

/// The status line a redeemed reset credit leaves (`OUTCOME_TEXT`, or the server's warning).
pub fn reset_outcome_text(result: &Value) -> String {
    if let Some(warning) = result["warning"].as_str() {
        return warning.to_owned();
    }
    match result["outcome"].as_str() {
        Some("reset") => "Reset applied. Your windows have cleared.",
        Some("nothingToReset") => "Nothing to reset right now.",
        Some("noCredit") => "No reset credit left.",
        Some("alreadyRedeemed") => "That credit was already redeemed.",
        _ => "Could not use the reset credit.",
    }
    .into()
}

/// `USAGE_PRICE_FIELDS`: settings key, column label, and whether a blank is allowed.
pub const PRICE_FIELDS: [(&str, &str, bool); 4] = [
    ("inputCostPerMillionTokens", "Input", false),
    ("outputCostPerMillionTokens", "Output", false),
    ("cacheReadCostPerMillionTokens", "Cache read", true),
    ("cacheWriteCostPerMillionTokens", "Cache write", true),
];

/// One Model prices row as edited (`UsagePriceDraft`): a `None` value or alias keeps the saved one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PriceDraft {
    pub model: String,
    pub is_new: bool,
    pub values: [Option<String>; 4],
    pub alias: Option<String>,
    pub removed: bool,
}

impl PriceDraft {
    /// `isEmptyUsagePriceDraft`.
    pub fn is_empty(&self) -> bool {
        self.is_new
            && self.model.trim().is_empty()
            && self.alias.as_deref().unwrap_or_default().trim().is_empty()
            && self.values.iter().flatten().all(|v| v.trim().is_empty())
    }

    /// Nothing differs from what is saved.
    pub fn is_unchanged(&self) -> bool {
        !self.is_new && !self.removed && self.alias.is_none() && self.values.iter().all(Option::is_none)
    }
}

/// A rate as JavaScript prints a number.
fn format_rate(value: f64) -> String {
    format!("{value}")
}

/// `usagePriceForm`: a saved price's four rates as text, blank where unset.
pub fn price_form(price: Option<&Value>) -> [String; 4] {
    PRICE_FIELDS.map(|(key, ..)| price.and_then(|p| p[key].as_f64()).map(format_rate).unwrap_or_default())
}

/// A non-negative decimal (`,` as the point allowed), optionally with an exponent.
fn parse_rate(raw: &str) -> Option<f64> {
    let raw = raw.trim().replacen(',', ".", 1);
    let (mantissa, exponent) = match raw.find(['e', 'E']) {
        Some(at) => (&raw[..at], Some(&raw[at + 1..])),
        None => (raw.as_str(), None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            (digits(whole) && (fraction.is_empty() || digits(fraction))) || (whole.is_empty() && digits(fraction))
        }
        None => digits(mantissa),
    };
    let exponent_ok = exponent.is_none_or(|e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
    if !mantissa_ok || !exponent_ok {
        return None;
    }
    raw.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.)
}

/// `parseUsagePriceForm`: blank cache rates use the input rate; zero means free.
pub fn parse_price_form(form: &[String; 4]) -> Option<Value> {
    let mut price = serde_json::Map::new();
    for ((key, _, optional), raw) in PRICE_FIELDS.iter().zip(form) {
        if raw.trim().is_empty() {
            if *optional {
                continue;
            }
            return None;
        }
        price.insert((*key).into(), json!(parse_rate(raw)?));
    }
    Some(Value::Object(price))
}

/// `usagePriceCell`: the saved rate and the placeholder its cell shows.
pub fn price_cell(prices: &Value, model: &str, field: usize) -> (String, &'static str) {
    let price = prices.get(model).filter(|p| p.is_object());
    let value = price.map(|p| price_form(Some(p))[field].clone());
    let placeholder = match (&value, PRICE_FIELDS[field].2) {
        (None, _) => "Automatic",
        (Some(_), true) => "Input rate",
        (Some(_), false) => "0.00",
    };
    (value.unwrap_or_default(), placeholder)
}

/// `usageAliasCell`: the saved "Map to" model.
pub fn alias_cell(aliases: &Value, model: &str) -> String {
    aliases.get(model).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// `usagePriceTableChanges` for this environment: the `usagePriceOverrides` and
/// `usageModelAliases` patches (`null` removes), or each failing row's error.
pub fn price_table_changes(
    prices: &Value,
    aliases: &Value,
    label: &str,
    drafts: &[PriceDraft],
) -> Result<(serde_json::Map<String, Value>, serde_json::Map<String, Value>), Vec<(usize, String)>> {
    let mut price_patch = serde_json::Map::new();
    let mut alias_patch = serde_json::Map::new();
    let mut errors = Vec::new();
    let saved_price = |model: &str| prices.get(model).filter(|p| p.is_object());
    let saved_alias = |model: &str| aliases.get(model).and_then(Value::as_str);
    for (index, draft) in drafts.iter().enumerate() {
        if draft.is_empty() || draft.is_unchanged() {
            continue;
        }
        let model = draft.model.trim();
        let current_alias = saved_alias(model);
        if draft.removed {
            if saved_price(model).is_some() {
                price_patch.insert(model.into(), Value::Null);
            }
            if current_alias.is_some() {
                alias_patch.insert(model.into(), Value::Null);
            }
            continue;
        }
        let alias = draft.alias.as_deref().or(current_alias).unwrap_or_default().trim();
        if !alias.is_empty() {
            if draft.alias.is_none() && draft.values.iter().any(Option::is_some) {
                errors.push((
                    index,
                    format!("{label} maps this model. Remove it from \"Apply to\" or clear its mapping first."),
                ));
            } else if model.is_empty() {
                errors.push((index, "Enter a model ID.".into()));
            } else if alias == model {
                errors.push((index, "Map to a different model.".into()));
            } else {
                if Some(alias) != current_alias {
                    alias_patch.insert(model.into(), json!(alias));
                }
                if saved_price(model).is_some() {
                    price_patch.insert(model.into(), Value::Null);
                }
            }
            continue;
        }
        if current_alias.is_some() {
            alias_patch.insert(model.into(), Value::Null);
            if saved_price(model).is_none() && draft.values.iter().flatten().all(|v| v.trim().is_empty()) {
                continue;
            }
        }
        let original = price_form(saved_price(model));
        let form: [String; 4] = std::array::from_fn(|i| draft.values[i].clone().unwrap_or_else(|| original[i].clone()));
        let Some(price) = parse_price_form(&form).filter(|_| !model.is_empty()) else {
            let missing =
                PRICE_FIELDS.iter().zip(&form).find(|((_, _, optional), raw)| !optional && raw.trim().is_empty());
            errors.push((
                index,
                match missing {
                    _ if model.is_empty() => "Enter a model ID.".into(),
                    Some(((_, field, _), _)) => format!("{field} is required on {label}."),
                    None => "Use non-negative numbers for prices.".into(),
                },
            ));
            continue;
        };
        if price_form(Some(&price)) != original {
            price_patch.insert(model.into(), price);
        }
    }
    // Mappings must not chain back to themselves once the batch applies.
    let mut final_aliases: std::collections::HashMap<String, String> = aliases
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect();
    for (model, alias) in &alias_patch {
        match alias.as_str() {
            Some(alias) => final_aliases.insert(model.clone(), alias.to_owned()),
            None => final_aliases.remove(model),
        };
    }
    for (index, draft) in drafts.iter().enumerate() {
        if draft.removed || errors.iter().any(|(i, _)| *i == index) {
            continue;
        }
        let mut model = draft.model.trim().to_owned();
        let mut seen = Vec::new();
        while let Some(next) = final_aliases.get(&model) {
            if seen.contains(&model) {
                errors.push((index, format!("Model mappings cannot form a cycle on {label}.")));
                break;
            }
            seen.push(model.clone());
            model = next.clone();
        }
    }
    if errors.is_empty() { Ok((price_patch, alias_patch)) } else { Err(errors) }
}

/// `WINDOW_KIND_ORDER`.
fn kind_order(kind: &str) -> u8 {
    match kind {
        "session" => 0,
        "weekly" => 1,
        "monthly" => 2,
        _ => 3,
    }
}

/// Codex window ids name slots, so they pool by kind; other drivers' ids are distinct allowances.
fn pool_key(driver: &str, window: &UsageWindow) -> String {
    if driver == "codex" { window.kind.clone() } else { format!("{}:{}", window.kind, window.id) }
}

#[derive(Clone, Debug)]
pub struct Member {
    /// Index into [`Pool::accounts`].
    pub account: usize,
    pub window: UsageWindow,
    pub refill_percent: f64,
}

#[derive(Clone, Debug)]
pub struct PoolWindow {
    pub id: String,
    pub kind: String,
    pub label: String,
    /// One slot per pool account, in account order; `None` where the account lacks the window.
    pub columns: Vec<Option<Member>>,
    pub remaining_percent: u32,
    pub pace: Option<Pace>,
    /// The soonest refill: when it lands and how much of the pool it restores.
    pub next_refill: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct Pool {
    pub driver: String,
    pub accounts: Vec<Account>,
    pub windows: Vec<PoolWindow>,
}

/// `poolMember`: a session window behind a spent weekly window is blocked until the weekly
/// window resets, so it shows as spent and refills when the weekly one does.
fn pool_member(account: &Account, index: usize, reported: &UsageWindow, now: i64) -> Member {
    let spent_weekly = (reported.kind == "session")
        .then(|| {
            account.limits.windows.iter().find(|w| {
                w.used_percent >= 100.
                    && match account.driver.as_str() {
                        "claudeAgent" => w.id == "seven_day",
                        "codex" => w.kind == "weekly",
                        _ => false,
                    }
            })
        })
        .flatten();
    let Some(spent_weekly) = spent_weekly else {
        return Member { account: index, window: reported.clone(), refill_percent: reported.used_percent };
    };
    let cutoff = now.max(account.limits.checked_at.as_deref().and_then(parse_ms).unwrap_or(0));
    let (weekly, session) = (reset_ms(spent_weekly), reset_ms(reported));
    let mut resets_at = None;
    let mut refill_percent = 0.;
    if let (Some(weekly), Some(session)) = (weekly, session)
        && weekly > cutoff
        && session > cutoff
    {
        if session <= weekly {
            resets_at = spent_weekly.resets_at.clone();
            refill_percent = 100.;
        } else if reported.used_percent >= 100. {
            resets_at = reported.resets_at.clone();
            refill_percent = 100.;
        } else {
            resets_at = spent_weekly.resets_at.clone();
            refill_percent = 100. - reported.used_percent.clamp(0., 100.);
        }
    }
    Member {
        account: index,
        window: UsageWindow { used_percent: 100., resets_at, window_duration_mins: None, ..reported.clone() },
        refill_percent,
    }
}

/// `collectLimitPools`: accounts grouped by driver, ordered by their soonest reset, with one
/// pooled bar per window.
pub fn limit_pools(accounts: Vec<Account>, now: i64) -> Vec<Pool> {
    let mut by_driver: Vec<(String, Vec<Account>)> = Vec::new();
    for account in accounts {
        match by_driver.iter_mut().find(|(driver, _)| *driver == account.driver) {
            Some((_, list)) => list.push(account),
            None => by_driver.push((account.driver.clone(), vec![account])),
        }
    }
    by_driver
        .into_iter()
        .map(|(driver, mut accounts)| {
            let first_window = accounts
                .iter()
                .flat_map(|a| a.limits.windows.iter())
                .min_by_key(|w| kind_order(&w.kind))
                .map(|w| pool_key(&driver, w));
            let order_reset = |account: &Account| {
                first_window
                    .as_ref()
                    .and_then(|key| account.limits.windows.iter().find(|w| pool_key(&driver, w) == *key))
                    .and_then(|w| reset_ms(&pool_member(account, 0, w, now).window))
                    .unwrap_or(i64::MAX)
            };
            let sort_name = |a: &Account| {
                a.display_name.clone().or_else(|| a.email.clone()).unwrap_or(a.key.clone()).to_lowercase()
            };
            accounts.sort_by_cached_key(|account| (order_reset(account), sort_name(account), account.key.clone()));
            let windows = pool_windows(&driver, &accounts, now);
            Pool { driver, accounts, windows }
        })
        .collect()
}

fn pool_windows(driver: &str, accounts: &[Account], now: i64) -> Vec<PoolWindow> {
    let mut by_key: Vec<(String, Vec<Member>)> = Vec::new();
    for (index, account) in accounts.iter().enumerate() {
        for reported in &account.limits.windows {
            let member = pool_member(account, index, reported, now);
            let pooled = pool_key(driver, &member.window);
            let taken =
                by_key.iter().any(|(key, members)| *key == pooled && members.iter().any(|m| m.account == index));
            let key = if taken { format!("{}:{}", member.window.kind, member.window.id) } else { pooled };
            match by_key.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push(member),
                None => by_key.push((key, vec![member])),
            }
        }
    }
    let mut windows: Vec<PoolWindow> = by_key
        .into_iter()
        .map(|(key, members)| {
            let first = &members[0].window;
            let used = members.iter().map(|m| m.window.used_percent).sum::<f64>() / members.len() as f64;
            let timed: Vec<(f64, f64)> = members
                .iter()
                .filter_map(|m| elapsed_share(&m.window, now).map(|share| (m.window.used_percent, share)))
                .collect();
            let pace = (!timed.is_empty()).then(|| {
                let n = timed.len() as f64;
                pace_of_shares(timed.iter().map(|t| t.0).sum::<f64>() / n, timed.iter().map(|t| t.1).sum::<f64>() / n)
            });
            let mut resets: Vec<(i64, u32)> = members
                .iter()
                .filter_map(|m| {
                    reset_ms(&m.window).map(|at| (at, (m.refill_percent / members.len() as f64).round() as u32))
                })
                .collect();
            resets.sort_by_key(|r| r.0);
            PoolWindow {
                id: if driver == "codex" { key } else { first.id.clone() },
                kind: first.kind.clone(),
                label: first.label.clone(),
                columns: (0..accounts.len()).map(|i| members.iter().find(|m| m.account == i).cloned()).collect(),
                remaining_percent: (100. - used).round() as u32,
                pace,
                next_refill: resets.iter().map(|r| r.1).find(|percent| *percent > 0),
            }
        })
        .collect();
    windows.sort_by_key(|w| kind_order(&w.kind));
    if driver == "cursor" {
        let has = |id: &str| windows.iter().any(|w| w.id == id);
        if has("autoPercentUsed") && has("apiPercentUsed") {
            windows.retain(|w| w.id != "totalPercentUsed");
        }
        let rank = |id: &str| CURSOR_WINDOWS.iter().position(|w| w.0 == id).unwrap_or(CURSOR_WINDOWS.len());
        windows.sort_by_key(|w| rank(&w.id));
    }
    windows
}

/// `CURSOR_USAGE_WINDOWS`: Cursor's allowances renamed and explained.
pub const CURSOR_WINDOWS: &[(&str, &str, &str)] = &[
    ("totalPercentUsed", "Overall", "Combined usage across both allowances, not a third quota."),
    ("autoPercentUsed", "Cursor Models", "Grok and Composer use this first. Auto can use either pool."),
    ("apiPercentUsed", "Other Models", "Claude, GPT, and Gemini use this pool. Grok and Composer fall back here."),
];

/// `collectLimitNotices`: accounts and sources that could not report, by name, each named
/// with its environment when several are selected.
pub fn limit_notices(environments: &[LimitEnvironment<'_>], hidden_drivers: &[&str]) -> Vec<String> {
    let several = environments.len() > 1;
    let mut notices = Vec::new();
    for environment in environments {
        let label =
            |subject: &str| if several { format!("{} · {subject}", environment.label) } else { subject.to_owned() };
        for provider in
            environment.providers.iter().filter(|p| reports_limits(p) && !hidden_drivers.contains(&p.driver.as_str()))
        {
            let Some(limits) = provider.usage_limits.as_ref() else { continue };
            let reason = limits.unavailable.as_ref().and_then(|u| u.get("reason")).and_then(Value::as_str);
            if reason == Some("unsupported") {
                continue;
            }
            let name =
                provider.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&provider.driver);
            if let Some(notice) = limits_notice(limits) {
                notices.push(format!("{}: {notice}", label(name)));
            }
        }
    }
    notices
}

/// `collectExternalUsageLinks`: hosts that show usage on their own site.
pub fn external_links(
    environments: &[LimitEnvironment<'_>],
    hidden_drivers: &[&str],
) -> Vec<(String, String, Option<String>, Vec<String>)> {
    let mut links: Vec<(String, String, Option<String>, Vec<String>)> = Vec::new();
    for environment in environments {
        for provider in
            environment.providers.iter().filter(|p| reports_limits(p) && !hidden_drivers.contains(&p.driver.as_str()))
        {
            let Some(limits) = &provider.usage_limits else { continue };
            let Some(external) = &limits.external_usage else { continue };
            if provider.auth.status != "authenticated" {
                continue;
            }
            let name = provider.display_name.as_deref().unwrap_or(&provider.instance_id);
            let account = format!("{name} on {}", environment.label);
            let message =
                limits.unavailable.as_ref().and_then(|u| u.get("message")).and_then(Value::as_str).map(str::to_owned);
            match links.iter_mut().find(|l| l.1 == external.url) {
                Some(link) => {
                    if !link.3.contains(&account) {
                        link.3.push(account);
                    }
                    link.2 = message;
                }
                None => links.push((external.label.clone(), external.url.clone(), message, vec![account])),
            }
        }
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(buckets: Value, sources: Value) -> Summary {
        serde_json::from_value(json!({"contractVersion": 6, "buckets": buckets, "sources": sources})).unwrap()
    }

    fn bucket(day: &str, provider: &str, model: &str, cost: f64, output: u64) -> Value {
        json!({
            "day": day, "provider": provider, "model": model, "sourcePath": format!("/{provider}"),
            "totals": {"uncachedInputTokens": 10, "cachedInputTokens": 0, "cacheCreationTokens": 0, "outputTokens": output, "reasoningTokens": 0},
            "costUsd": cost, "cacheSavingsUsd": 0, "costSource": "modelPriced", "records": 2, "unpricedRecords": 0, "sessions": 9,
        })
    }

    #[test]
    fn a_directory_two_machines_read_counts_once_for_the_newest_read() {
        let summary = |read_at: &str, host: &str, cost: f64| -> Summary {
            serde_json::from_value(json!({
                "contractVersion": CONTRACT_VERSION, "readAt": read_at,
                "buckets": [{"day": "2026-11-01", "provider": "codex", "model": "m", "sourcePath": "/home/me/.codex",
                    "totals": {"uncachedInputTokens": 1, "cachedInputTokens": 0, "cacheCreationTokens": 0, "outputTokens": 1},
                    "costUsd": cost, "records": 1}],
                "sources": [{"fingerprint": {"provider": "codex", "resolvedHomePath": "/home/me/.codex", "hostId": host},
                    "status": "ok", "distinctSessions": 2}],
            }))
            .unwrap()
        };
        let usage = |id: &str, summary: Summary| EnvironmentUsage { environment_id: id.into(), summary };
        let shared = combine(vec![
            usage("old", summary("2026-11-01T00:00:00Z", "box", 1.)),
            usage("new", summary("2026-11-02T00:00:00Z", "box", 2.)),
        ])
        .unwrap();
        assert_eq!((shared.buckets.len(), shared.buckets[0].cost_usd, shared.sources.len()), (1, 2., 1));
        let separate = combine(vec![
            usage("a", summary("2026-11-01T00:00:00Z", "box-a", 1.)),
            usage("b", summary("2026-11-01T00:00:00Z", "box-b", 2.)),
        ])
        .unwrap();
        assert_eq!(merge(&separate, &[]).sessions, 4, "different machines both count");
    }

    fn source(provider: &str, status: &str, sessions: u64) -> Value {
        json!({"fingerprint": {"hostId": "h", "provider": provider, "resolvedHomePath": format!("/{provider}"), "volumeId": ""},
            "status": status, "scannedFiles": 1, "skippedFiles": 0, "malformedRecords": 0, "distinctSessions": sessions, "message": null})
    }

    #[test]
    fn merges_buckets_by_provider_model_and_day() {
        let summary = summary(
            json!([
                bucket("2026-10-01", "claude", "opus", 3., 90),
                bucket("2026-10-01", "codex", "gpt", 1., 40),
                bucket("2026-10-02", "claude", "opus", 1., 100),
                // A source that was not scanned owns nothing.
                bucket("2026-10-02", "grok", "grok-4", 5., 1),
                {"unknown": "variant"},
            ]),
            json!([source("claude", "ok", 4), source("codex", "ok", 3), source("grok", "missing", 0)]),
        );
        let merged = merge(&summary, &[]);
        assert_eq!(merged.cost_usd, 5.);
        assert_eq!(merged.total_tokens, 30 + 230);
        assert_eq!(merged.sessions, 7, "distinct sessions per source, not bucket sums");
        assert_eq!(merged.providers.iter().map(|p| p.provider.as_str()).collect::<Vec<_>>(), ["claude", "codex"]);
        assert_eq!(merged.providers[0].cost_share, 0.8);
        assert_eq!(merged.models[0].model, "opus");
        assert_eq!(merged.daily.iter().map(|d| d.day.as_str()).collect::<Vec<_>>(), ["2026-10-01", "2026-10-02"]);
        assert_eq!(merged.daily[0].value("codex", true), 50.);
        assert_eq!(active_providers(&merged), ["codex", "claude"]);

        let without_claude = merge(&summary, &["claude".to_string()]);
        assert_eq!((without_claude.cost_usd, without_claude.sessions), (1., 3));

        let huge = self::summary(
            json!([
                bucket("2026-10-01", "claude", "opus", 1., u64::MAX),
                bucket("2026-10-02", "claude", "opus", 1., 1)
            ]),
            json!([source("claude", "ok", u64::MAX), source("claude", "ok", 1)]),
        );
        let merged = merge(&huge, &[]);
        assert_eq!((merged.total_tokens, merged.sessions), (u64::MAX, u64::MAX), "outside counts saturate");
    }

    #[test]
    fn formats_like_the_web() {
        assert_eq!(format_usd(1234.567), "$1,234.57");
        assert_eq!(format_usd(0.), "$0.00");
        assert_eq!(format_tokens(999.), "999");
        assert_eq!(format_tokens(1250.), "1.25K");
        assert_eq!(format_tokens(12_500_000.), "12.5M");
        assert_eq!(format_tokens(2_000_000_000.), "2B");
        assert_eq!(format_tokens(150_000.), "150K");
        assert_eq!(format_count(1234567.4), "1,234,567");
        assert_eq!(format_percent(0.8), "80.0%");
        assert_eq!(format_percent(0.0004), "<0.1%");
        assert_eq!(format_day_short("2026-10-05"), "Oct 5");
        assert_eq!(format_duration(2 * DAY + 3 * HOUR + 5 * MINUTE), "2d 3h");
        assert_eq!(format_duration(4 * HOUR + 5 * MINUTE), "4h 5m");
    }

    #[test]
    fn windows_cover_calendar_days_or_the_past_24_hours() {
        // Pacific Standard Time, as America/Los_Angeles reads in November 2026.
        let tz = chrono::FixedOffset::west_opt(8 * 3600).unwrap();
        let now = parse_ms("2026-11-05T18:30:42Z").unwrap();
        let month = Window::new(30, now, &tz, "America/Los_Angeles".into());
        assert_eq!(
            (month.since_day.to_string(), month.until_day.to_string()),
            ("2026-10-07".into(), "2026-11-05".into())
        );
        assert_eq!(month.periods().len(), 30);
        assert_eq!(month.to_json()["resolution"], "day");
        let day = Window::new(1, now, &tz, "America/Los_Angeles".into());
        let input = day.to_json();
        assert_eq!(
            (input["sinceTime"].as_str(), input["untilTime"].as_str()),
            (Some("2026-11-04T18:30:00.000Z"), Some("2026-11-05T18:30:00.000Z"))
        );
        assert_eq!(day.periods().len(), 24);
        assert_eq!(format_hour_short(parse_ms("2026-11-05T18:00:00Z").unwrap(), &tz), "10 AM");
        let reference = parse_ms("2026-11-05T18:30:00Z").unwrap();
        assert_eq!(
            format_relative_hour_short(parse_ms("2026-11-05T07:00:00Z").unwrap(), reference, &tz),
            "11 PM yesterday"
        );
    }

    #[test]
    fn chart_scale_and_curve() {
        assert_eq!(nice_scale(0.), (0., vec![0.]));
        assert_eq!(nice_scale(37.).1, vec![0., 10., 20., 30., 40.]);
        assert_eq!(nice_scale(7.).1, vec![0., 2., 4., 6., 8.]);
        let curve = smooth_curve(&[(0., 100.), (10., 50.), (20., 50.)]);
        assert_eq!(curve.len(), 2);
        assert_eq!(curve[1][1], (40. / 3., 50.), "a flat stretch stays flat: no overshoot");
        assert!(curve[0][2].1 >= 50.);
    }

    fn provider(instance: &str, driver: &str, email: &str, windows: Value) -> Provider {
        serde_json::from_value(json!({
            "instanceId": instance, "driver": driver, "enabled": true, "installed": true, "status": "ready",
            "auth": {"status": "authenticated", "email": email},
            "usageLimits": {"checkedAt": "2026-11-05T00:00:00Z", "windows": windows},
        }))
        .unwrap()
    }

    /// One environment, as a single-machine Limits view reads it.
    fn here<'a>(providers: &'a [Provider], sources: &'a Value) -> [LimitEnvironment<'a>; 1] {
        [LimitEnvironment { id: "env-a", label: "Here", providers, sources }]
    }

    #[test]
    fn model_price_rows_become_settings_patches() {
        let prices = json!({"old": {"inputCostPerMillionTokens": 1, "outputCostPerMillionTokens": 2}});
        let aliases = json!({"b": "a"});
        let row = |model: &str, values: [Option<&str>; 4]| PriceDraft {
            model: model.into(),
            is_new: true,
            values: values.map(|v| v.map(str::to_owned)),
            ..Default::default()
        };
        let (price_patch, alias_patch) =
            price_table_changes(&prices, &aliases, "Here", &[row("m1", [Some("3"), Some("15"), Some(""), None])])
                .unwrap();
        assert_eq!(
            Value::Object(price_patch),
            json!({"m1": {"inputCostPerMillionTokens": 3.0, "outputCostPerMillionTokens": 15.0}})
        );
        assert!(alias_patch.is_empty());
        let reset = PriceDraft { model: "old".into(), removed: true, ..Default::default() };
        let (price_patch, _) = price_table_changes(&prices, &aliases, "Here", &[reset]).unwrap();
        assert_eq!(Value::Object(price_patch), json!({"old": null}));
        let errors =
            price_table_changes(&prices, &aliases, "Here", &[row("m2", [Some("3"), None, None, None])]).unwrap_err();
        assert_eq!(errors, [(0, "Output is required on Here.".to_owned())]);
        let cycle = PriceDraft { model: "a".into(), is_new: true, alias: Some("b".into()), ..Default::default() };
        let errors = price_table_changes(&prices, &aliases, "Here", &[cycle]).unwrap_err();
        assert_eq!(errors, [(0, "Model mappings cannot form a cycle on Here.".to_owned())]);
        let same = PriceDraft { model: "c".into(), is_new: true, alias: Some("c".into()), ..Default::default() };
        assert_eq!(
            price_table_changes(&prices, &aliases, "Here", &[same]).unwrap_err()[0].1,
            "Map to a different model."
        );
        assert_eq!(parse_rate(",5"), Some(0.5));
        assert_eq!(parse_rate("1e-3"), Some(0.001));
        assert_eq!(parse_rate("-1"), None);
        assert_eq!(price_cell(&prices, "old", 2), (String::new(), "Input rate"));
        assert_eq!(price_cell(&prices, "new", 0), (String::new(), "Automatic"));
    }

    #[test]
    fn a_hub_account_merges_with_the_same_login_and_redeems_through_the_hub() {
        let window = json!([{"id": "w", "kind": "session", "label": "5-hour", "usedPercent": 40}]);
        let providers = vec![provider("codex", "codex", "me@x.dev", window.clone())];
        let sources = json!([{
            "id": "hub", "kind": "cliproxy", "label": "Team hub", "checkedAt": "2026-11-05T01:00:00Z",
            "accounts": [
                {"id": "me.json", "driver": "codex", "email": "ME@x.dev", "plan": "Pro",
                 "usageLimits": {"checkedAt": "2026-11-05T01:00:00Z", "windows": window,
                                 "resetCredits": {"availableCount": 2, "nextCreditId": "c1"}}},
                {"id": "other.json", "driver": "codex", "usageLimits": {"checkedAt": "2026-11-05T01:00:00Z", "windows": window}},
            ],
        }]);
        let accounts = limit_accounts(&here(&providers, &sources), &[]);
        assert_eq!(accounts.len(), 2);
        let me = &accounts[0];
        assert_eq!(
            (me.plan.as_deref(), me.source_label.as_deref()),
            (Some("Pro"), None),
            "signed in here, so no source"
        );
        assert_eq!(me.limits.reset_credits.as_ref().map(|c| c.available_count), Some(2));
        assert_eq!(
            me.redeem,
            Some(("env-a".into(), json!({"sourceId": "hub", "accountId": "me.json", "creditId": "c1"})))
        );
        let other = &accounts[1];
        assert_eq!((other.display_name.as_deref(), other.source_label.as_deref()), (Some("other"), Some("Team hub")));
        assert_eq!(other.redeem, None);
        let credits = me.limits.reset_credits.clone().unwrap();
        assert_eq!(reset_credits_summary(&credits, 0, false), "2 reset credits banked");
        assert_eq!(reset_outcome_text(&json!({"outcome": "nothingToReset"})), "Nothing to reset right now.");
    }

    #[test]
    fn pools_accounts_and_blocks_sessions_behind_a_spent_week() {
        let now = parse_ms("2026-11-05T12:00:00Z").unwrap();
        let session = |used: f64, resets: &str| json!({"id": "five_hour", "kind": "session", "label": "5-hour", "usedPercent": used, "resetsAt": resets, "windowDurationMins": 300});
        let providers = vec![
            provider("a", "claudeAgent", "me@x.dev", json!([session(20., "2026-11-05T14:00:00Z")])),
            // The same login on a second instance is one account.
            provider("b", "claudeAgent", "ME@x.dev", json!([session(20., "2026-11-05T14:00:00Z")])),
            provider(
                "c",
                "claudeAgent",
                "you@x.dev",
                json!([
                    session(10., "2026-11-05T13:00:00Z"),
                    {"id": "seven_day", "kind": "weekly", "label": "Weekly", "usedPercent": 100, "resetsAt": "2026-11-06T00:00:00Z"},
                ]),
            ),
        ];
        let pools = limit_pools(limit_accounts(&here(&providers, &Value::Null), &[]), now);
        assert_eq!(pools.len(), 1);
        let pool = &pools[0];
        assert_eq!(pool.accounts.len(), 2);
        let session = &pool.windows[0];
        assert_eq!(session.kind, "session");
        // you@ is blocked by its spent week: its session counts as spent until the week resets,
        // which puts its reset after me@'s.
        assert_eq!(pool.accounts[0].email.as_deref(), Some("me@x.dev"), "soonest reset first");
        let blocked = session.columns[1].as_ref().unwrap();
        assert_eq!((blocked.window.used_percent, blocked.refill_percent), (100., 100.));
        assert_eq!(blocked.window.resets_at.as_deref(), Some("2026-11-06T00:00:00Z"));
        assert_eq!(session.remaining_percent, 40);
        assert_eq!(session.next_refill, Some(10), "me@'s 20% back, as a share of two accounts");
        assert!(pool.windows[1].columns[0].is_none(), "me@ has no weekly window");
        assert!(limit_accounts(&here(&providers, &Value::Null), &["claudeAgent"]).is_empty());
    }

    #[test]
    fn limits_collect_across_environments_and_redeem_where_the_credit_is() {
        let window = json!([{"id": "w", "kind": "session", "label": "5-hour", "usedPercent": 40}]);
        let mut credited = provider("codex", "codex", "me@x.dev", window.clone());
        let with_credit =
            json!({"checkedAt": "2026-11-05T02:00:00Z", "windows": window, "resetCredits": {"availableCount": 1}});
        credited.usage_limits = serde_json::from_value(with_credit).unwrap();
        let mut unavailable = provider("claude", "claudeAgent", "me@x.dev", json!([]));
        unavailable.usage_limits =
            serde_json::from_value(json!({"checkedAt": "2026-11-05T00:00:00Z", "windows": [], "unavailable": {"reason": "error", "message": "Expired."}}))
                .unwrap();
        let (on_a, on_b) = (vec![provider("codex", "codex", "ME@x.dev", window.clone())], vec![credited, unavailable]);
        let hub = json!([{"id": "hub", "label": "Team hub", "accounts": [
            {"id": "other.json", "driver": "codex", "usageLimits": {"checkedAt": "2026-11-05T01:00:00Z", "windows": window}},
        ]}]);
        let environments = [
            LimitEnvironment { id: "env-a", label: "Laptop", providers: &on_a, sources: &Value::Null },
            LimitEnvironment { id: "env-b", label: "Server", providers: &on_b, sources: &hub },
        ];
        let accounts = limit_accounts(&environments, &[]);
        assert_eq!(accounts.len(), 2);
        let me = &accounts[0];
        let signed_in: Vec<&str> = me.environments.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(signed_in, ["Laptop", "Server"], "one login on two machines is one account");
        // The credit is on the server's instance, so the server spends it.
        assert_eq!(me.redeem, Some(("env-b".into(), json!({"instanceId": "codex"}))));
        assert_eq!(accounts[1].source_label.as_deref(), Some("Server · Team hub"));
        assert!(limit_notices(&environments, &[]).iter().any(|n| n.starts_with("Server · claude")));
    }

    #[test]
    fn pace_compares_spend_with_elapsed_time() {
        let now = parse_ms("2026-11-05T12:00:00Z").unwrap();
        let window = |used: f64| -> UsageWindow {
            serde_json::from_value(json!({"id": "w", "kind": "session", "label": "5h", "usedPercent": used, "resetsAt": "2026-11-05T14:30:00Z", "windowDurationMins": 300})).unwrap()
        };
        assert_eq!(elapsed_share(&window(0.), now), Some(0.5));
        assert_eq!(pace_remaining_percent(&window(0.), now), Some(50));
        assert_eq!(format_resets_in(&window(0.), now).as_deref(), Some("resets in 2h 30m"));
        let pace = |used| pace_of_shares(used, elapsed_share(&window(used), now).unwrap());
        assert_eq!((pace(80.), pace(52.), pace(20.)), (Pace::Ahead, Pace::On, Pace::Under));
    }
}
