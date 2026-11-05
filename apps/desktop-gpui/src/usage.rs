//! The Usage page's logic: the `server.getUsageSummary` read model and its merge into totals,
//! the period window, number and date formatting, the chart's scale and curves, and the
//! subscription-limit pools (`packages/shared/src/{usageMerge,usageFormat,usageLimits}.ts`,
//! `apps/web/src/components/usage/*`).

use std::collections::HashMap;

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan as _, civil::Date};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::inbox::parse_ms;
use crate::model::{Provider, UsageLimits, UsageWindow};

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

impl TokenTotals {
    /// Reasoning tokens are already part of the output count.
    fn total(&self) -> u64 {
        self.uncached_input_tokens + self.cached_input_tokens + self.cache_creation_tokens + self.output_tokens
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
        self.total_tokens += tokens;
        let entry = self.by_provider.entry(provider.to_owned()).or_default();
        entry.0 += cost;
        entry.1 += tokens;
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
        merged.sessions += source.distinct_sessions;
        if source.distinct_sessions > 0 {
            provider_slot(&mut providers, provider).sessions += source.distinct_sessions;
        }
    }
    let mut records = 0;
    let mut unpriced = 0;
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
        merged.uncached_input_tokens += bucket.totals.uncached_input_tokens;
        merged.cached_input_tokens += bucket.totals.cached_input_tokens;
        merged.cache_creation_tokens += bucket.totals.cache_creation_tokens;
        merged.output_tokens += bucket.totals.output_tokens;
        records += bucket.records;
        unpriced += bucket.unpriced_records;
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
        provider.total_tokens += tokens;
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
        model.total_tokens += tokens;
        model.records += bucket.records;
        model.unpriced_records += bucket.unpriced_records;
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
    merged.total_tokens =
        merged.uncached_input_tokens + merged.cached_input_tokens + merged.cache_creation_tokens + merged.output_tokens;
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
pub fn models_by_tokens(models: &[ModelTotals]) -> Vec<ModelTotals> {
    let mut sorted = models.to_vec();
    sorted.sort_by(|l, r| r.total_tokens.cmp(&l.total_tokens).then(r.cost_usd.total_cmp(&l.cost_usd)));
    sorted
}

/// The period a usage read covers (`makeWindow`), in the viewer's IANA zone.
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub days: u32,
    pub since_day: Date,
    pub until_day: Date,
    pub time_zone: String,
    /// Past 24h reads by the hour, ending on the minute.
    pub hours: Option<(i64, i64)>,
}

impl Window {
    pub fn new(days: u32, now: Timestamp, tz: &TimeZone) -> Self {
        let time_zone = tz.iana_name().unwrap_or("UTC").to_owned();
        let day_of = |ms: i64| Timestamp::from_millisecond(ms).unwrap_or(now).to_zoned(tz.clone()).date();
        let now_ms = now.as_millisecond();
        let until_day = day_of(now_ms);
        if days == 1 {
            let until = now_ms.div_euclid(MINUTE) * MINUTE;
            let since = until - DAY;
            return Self { days, since_day: day_of(since), until_day, time_zone, hours: Some((since, until)) };
        }
        let since_day = until_day.checked_sub((i64::from(days) - 1).days()).unwrap_or(until_day);
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
            input["sinceTime"] = json!(iso(since));
            input["untilTime"] = json!(iso(until));
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
                    match day.tomorrow() {
                        Ok(next) => day = next,
                        Err(_) => break,
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

    pub fn label(&self, tz: &TimeZone) -> String {
        match self {
            PeriodKey::Day(day) => format_day_short(day),
            PeriodKey::Hour(ms) => format_hour_short(*ms, tz),
        }
    }
}

fn iso(ms: i64) -> String {
    Timestamp::from_millisecond(ms).map(|t| t.strftime("%Y-%m-%dT%H:%M:%S.000Z").to_string()).unwrap_or_default()
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
pub fn format_hour_short(ms: i64, tz: &TimeZone) -> String {
    Timestamp::from_millisecond(ms).map(|t| t.to_zoned(tz.clone()).strftime("%-I %p").to_string()).unwrap_or_default()
}

/// `formatDateTimeShort`: "Nov 4, 3 PM".
pub fn format_date_time_short(ms: i64, tz: &TimeZone) -> String {
    let Ok(time) = Timestamp::from_millisecond(ms) else { return String::new() };
    let zoned = time.to_zoned(tz.clone());
    format!("{} {}, {}", MONTHS[zoned.month() as usize - 1], zoned.day(), zoned.strftime("%-I %p"))
}

/// `formatRelativeHourShort`: "3 PM today", "3 PM yesterday", else the date and hour.
pub fn format_relative_hour_short(ms: i64, reference_ms: i64, tz: &TimeZone) -> String {
    let day = |ms: i64| Timestamp::from_millisecond(ms).ok().map(|t| t.to_zoned(tz.clone()).date());
    let (Some(at), Some(reference)) = (day(ms), day(reference_ms)) else { return format_date_time_short(ms, tz) };
    match at.until(reference).map(|span| span.get_days()) {
        Ok(0) => format!("{} today", format_hour_short(ms, tz)),
        Ok(1) => format!("{} yesterday", format_hour_short(ms, tz)),
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
}

impl Account {
    pub fn name(&self) -> String {
        self.display_name.clone().or_else(|| self.email.clone()).unwrap_or_else(|| self.driver.clone())
    }
}

fn reports_limits(provider: &Provider) -> bool {
    provider.enabled
        && provider.installed
        && provider.availability.as_deref() != Some("unavailable")
        && provider.usage_limits.is_some()
}

/// `accountKey` + `accountKeyResolver`: the same login on two instances is one account.
fn account_key(provider: &Provider, workspaces: &HashMap<String, Vec<String>>) -> Option<String> {
    let base = account_base_key(provider)?;
    let auth = Some(&provider.auth);
    if let Some(workspace) = auth.and_then(|a| a.workspace_id.as_ref()) {
        return Some(format!("{base}:{workspace}"));
    }
    match workspaces.get(&base).map(Vec::as_slice) {
        None => Some(base),
        Some([only]) => Some(format!("{base}:{only}")),
        Some(_) => None,
    }
}

fn account_base_key(provider: &Provider) -> Option<String> {
    let email = Some(&provider.auth).and_then(|a| a.email.as_deref()).map(|e| e.trim().to_lowercase());
    match email.filter(|e| !e.is_empty()) {
        Some(email) => Some(format!("{}:{email}", provider.driver)),
        None => provider
            .usage_limits
            .as_ref()
            .and_then(|l| l.credential_fingerprint.as_ref())
            .map(|fingerprint| format!("{}:credential:{fingerprint}", provider.driver)),
    }
}

/// `collectLimitAccounts` for one environment's providers.
pub fn limit_accounts(providers: &[Provider], hidden_drivers: &[&str]) -> Vec<Account> {
    let providers: Vec<&Provider> = providers.iter().filter(|p| !hidden_drivers.contains(&p.driver.as_str())).collect();
    let mut workspaces: HashMap<String, Vec<String>> = HashMap::new();
    for provider in providers.iter().filter(|p| p.enabled) {
        if let (Some(base), Some(workspace)) =
            (account_base_key(provider), Some(&provider.auth).and_then(|a| a.workspace_id.clone()))
        {
            let known = workspaces.entry(base).or_default();
            if !known.contains(&workspace) {
                known.push(workspace);
            }
        }
    }
    let mut accounts: Vec<Account> = Vec::new();
    for provider in providers.into_iter().filter(|p| reports_limits(p)) {
        let Some(limits) = provider.usage_limits.clone() else { continue };
        if limits_notice(&limits).is_some() {
            continue;
        }
        let key = account_key(provider, &workspaces).unwrap_or_else(|| provider.instance_id.clone());
        let auth = Some(&provider.auth);
        let next = Account {
            key: provider.instance_id.clone(),
            driver: provider.driver.clone(),
            display_name: provider.display_name.as_ref().map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()),
            email: auth.and_then(|a| a.email.clone()),
            plan: auth.and_then(|a| a.label.clone()),
            accent_color: provider.accent_color.clone(),
            limits,
        };
        let checked = |account: &Account| account.limits.checked_at.as_deref().and_then(parse_ms).unwrap_or(0);
        match accounts.iter_mut().find(|a| account_key_of(a, &key)) {
            None => accounts.push(Account { key: format!("{key}\u{0}{}", next.key), ..next }),
            Some(previous) => {
                let fresher = checked(&next) > checked(previous);
                if fresher {
                    previous.plan = next.plan.or(previous.plan.take());
                    previous.limits = next.limits;
                } else if previous.plan.is_none() {
                    previous.plan = next.plan;
                }
                previous.display_name = previous.display_name.take().or(next.display_name);
                previous.accent_color = previous.accent_color.take().or(next.accent_color);
            }
        }
    }
    accounts
}

fn account_key_of(account: &Account, key: &str) -> bool {
    account.key.split('\u{0}').next() == Some(key)
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
            accounts.sort_by(|l, r| {
                order_reset(l)
                    .cmp(&order_reset(r))
                    .then_with(|| sort_name(l).cmp(&sort_name(r)))
                    .then_with(|| l.key.cmp(&r.key))
            });
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

/// `collectLimitNotices`: accounts that could not report, by name.
pub fn limit_notices(providers: &[Provider], hidden_drivers: &[&str]) -> Vec<String> {
    providers
        .iter()
        .filter(|p| reports_limits(p) && !hidden_drivers.contains(&p.driver.as_str()))
        .filter_map(|provider| {
            let limits = provider.usage_limits.as_ref()?;
            let reason = limits.unavailable.as_ref().and_then(|u| u.get("reason")).and_then(Value::as_str);
            if reason == Some("unsupported") {
                return None;
            }
            let name =
                provider.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&provider.driver);
            limits_notice(limits).map(|notice| format!("{name}: {notice}"))
        })
        .collect()
}

/// `collectExternalUsageLinks`: hosts that show usage on their own site.
pub fn external_links(
    providers: &[Provider],
    environment: &str,
    hidden_drivers: &[&str],
) -> Vec<(String, String, Option<String>, Vec<String>)> {
    let mut links: Vec<(String, String, Option<String>, Vec<String>)> = Vec::new();
    for provider in providers.iter().filter(|p| reports_limits(p) && !hidden_drivers.contains(&p.driver.as_str())) {
        let Some(limits) = &provider.usage_limits else { continue };
        let Some(external) = &limits.external_usage else { continue };
        if Some(&provider.auth).map(|a| a.status.as_str()) != Some("authenticated") {
            continue;
        }
        let account = format!("{} on {environment}", provider.display_name.as_deref().unwrap_or(&provider.instance_id));
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
        let tz = TimeZone::get("America/Los_Angeles").unwrap();
        let now: Timestamp = "2026-11-05T18:30:42Z".parse().unwrap();
        let month = Window::new(30, now, &tz);
        assert_eq!(
            (month.since_day.to_string(), month.until_day.to_string()),
            ("2026-10-07".into(), "2026-11-05".into())
        );
        assert_eq!(month.periods().len(), 30);
        assert_eq!(month.to_json()["resolution"], "day");
        let day = Window::new(1, now, &tz);
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
        let pools = limit_pools(limit_accounts(&providers, &[]), now);
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
        assert!(limit_accounts(&providers, &["claudeAgent"]).is_empty());
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
