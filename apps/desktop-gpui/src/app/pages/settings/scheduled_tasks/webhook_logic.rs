//! Pure webhook-task logic, ported from `packages/client-runtime/src/scheduledTaskWebhook.ts`,
//! `packages/client-runtime/src/webhookAddress.ts`, the webhook parts of
//! `apps/web/src/components/settings/scheduledTasksSettings.logic.ts`, and the server checks it
//! leans on (`ScheduledTaskWebhookFilter`, `isWebhookDedupeTemplate` in
//! `packages/contracts/src/scheduledTask.ts`).

use serde_json::{Map, Value, json};

/// `MAX_WEBHOOK_FILTERS`.
pub(super) const MAX_WEBHOOK_FILTERS: usize = 16;
/// `MAX_WEBHOOK_DELIVERY_AGE_MINUTES`: how long the relay holds a request for an offline environment.
const MAX_WEBHOOK_DELIVERY_AGE_MINUTES: u32 = 24 * 60;
/// `DEFAULT_WEBHOOK_PROMPT`: the editor fills an empty prompt with it when switching to "On webhook".
pub(super) const DEFAULT_WEBHOOK_PROMPT: &str = "Handle this webhook:\n{{body}}";
/// `WEBHOOK_SIGNATURE_DEFAULTS`.
pub(super) const DEFAULT_SIGNATURE_HEADER: &str = "x-hub-signature-256";
pub(super) const DEFAULT_SIGNATURE_PREFIX: &str = "sha256=";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FilterOp {
    Equals,
    NotEquals,
    In,
    StartsWith,
    Exists,
}

impl FilterOp {
    /// `WEBHOOK_FILTER_OPS`, in menu order.
    pub(super) const ALL: [FilterOp; 5] =
        [FilterOp::Equals, FilterOp::NotEquals, FilterOp::In, FilterOp::StartsWith, FilterOp::Exists];

    pub(super) fn label(self) -> &'static str {
        match self {
            FilterOp::Equals => "equals",
            FilterOp::NotEquals => "does not equal",
            FilterOp::In => "is one of",
            FilterOp::StartsWith => "starts with",
            FilterOp::Exists => "exists",
        }
    }

    fn wire(self) -> &'static str {
        match self {
            FilterOp::Equals => "equals",
            FilterOp::NotEquals => "notEquals",
            FilterOp::In => "in",
            FilterOp::StartsWith => "startsWith",
            FilterOp::Exists => "exists",
        }
    }

    fn parse(wire: &str) -> Option<Self> {
        FilterOp::ALL.into_iter().find(|op| op.wire() == wire)
    }
}

/// One editable filter row (`WebhookFilterDraft` without its form identity). `in` takes a
/// comma-separated list, lossless because list values can't contain commas; `exists` ignores the value.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FilterDraft {
    pub path: String,
    pub op: FilterOp,
    pub value: String,
}

/// `webhookFilterDrafts`: the stored filters as form rows.
pub(super) fn filter_drafts(filters: Option<&Value>) -> Vec<FilterDraft> {
    let Some(filters) = filters.and_then(Value::as_array) else { return Vec::new() };
    filters
        .iter()
        .filter_map(|filter| {
            let op = FilterOp::parse(filter.get("op")?.as_str()?)?;
            let value = match (op, filter.get("value")) {
                (FilterOp::Exists, _) => String::new(),
                (FilterOp::In, Some(Value::Array(items))) => {
                    items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")
                }
                (_, value) => value.and_then(Value::as_str).unwrap_or_default().to_owned(),
            };
            Some(FilterDraft { path: filter.get("path")?.as_str()?.to_owned(), op, value })
        })
        .collect()
}

/// `parseWebhookFilterDrafts`: each row checked against the server's `ScheduledTaskWebhookFilter`,
/// so rows that parse here also save. The error names the first row that would not.
pub(super) fn parse_filter_drafts(drafts: &[FilterDraft]) -> Result<Vec<Value>, String> {
    if drafts.len() > MAX_WEBHOOK_FILTERS {
        return Err(format!("Use at most {MAX_WEBHOOK_FILTERS} filters."));
    }
    drafts
        .iter()
        .enumerate()
        .map(|(index, draft)| {
            let path = draft.path.trim();
            let filter = match draft.op {
                FilterOp::Exists => Some(json!({"path": path, "op": "exists"})),
                FilterOp::In => {
                    let values: Vec<&str> =
                        draft.value.split(',').map(str::trim).filter(|item| !item.is_empty()).collect();
                    (!values.is_empty()).then(|| json!({"path": path, "op": "in", "value": values}))
                }
                op => Some(json!({"path": path, "op": op.wire(), "value": draft.value})),
            };
            filter.filter(|_| is_filter_path(path)).ok_or_else(|| {
                let needs = match draft.op {
                    FilterOp::Exists => ".",
                    FilterOp::In => ", and at least one value.",
                    _ => ", and a value.",
                };
                format!("Filter {} needs a path under body, headers or query{needs}", index + 1)
            })
        })
        .collect()
}

/// `WebhookFilterPath`: non-empty, matching `^(body|headers|query)(\.|$)`.
fn is_filter_path(path: &str) -> bool {
    ["body", "headers", "query"]
        .iter()
        .any(|root| path.strip_prefix(root).is_some_and(|rest| rest.is_empty() || rest.starts_with('.')))
}

/// `webhookFilterDraftsChanged`. A save leaves `filters` out unless they changed, so editing only
/// the title can't overwrite filters another device changed meanwhile.
pub(super) fn filter_drafts_changed(saved: &[FilterDraft], current: &[FilterDraft]) -> bool {
    saved.len() != current.len()
        || saved.iter().zip(current).any(|(saved, current)| {
            saved.path != current.path
                || saved.op != current.op
                || (saved.op != FilterOp::Exists && saved.value != current.value)
        })
}

/// `parseMaxDeliveryAge`: `Ok(None)` is "no limit"; `Err` blocks saving.
fn parse_max_delivery_age(value: &str) -> Result<Option<u32>, ()> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    js_integer(value)
        .filter(|minutes| (1..=i64::from(MAX_WEBHOOK_DELIVERY_AGE_MINUTES)).contains(minutes))
        .map(|minutes| Some(minutes as u32))
        .ok_or(())
}

/// `Number(text)` when it is a whole number (`Number.isInteger`).
fn js_integer(text: &str) -> Option<i64> {
    let number: f64 = text.trim().parse().ok()?;
    (number.is_finite() && number.fract() == 0.0 && number.abs() < 9.0e15).then_some(number as i64)
}

/// `isWebhookDedupeTemplate`: at least one `{{body.x}}`, `{{headers.x}}`, `{{query.x}}` or
/// `{{request}}` placeholder, and no stray braces around them.
pub(super) fn is_dedupe_template(template: &str) -> bool {
    let mut remainder = String::with_capacity(template.len());
    let mut matches = 0;
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(len) = placeholder_len(rest) {
            matches += 1;
            rest = &rest[len..];
        } else {
            let next = rest.chars().next().map_or(1, char::len_utf8);
            remainder.push_str(&rest[..next]);
            rest = &rest[next..];
        }
    }
    matches > 0 && !remainder.contains("{{") && !remainder.contains("}}")
}

/// The byte length of the placeholder `\{\{\s*(?:(?:body|headers|query)(?:\.[^{}.\s]+)*|request)\s*\}\}`
/// at the start of `text`. Segment characters exclude braces, dots and spaces, so greedy is exact.
fn placeholder_len(text: &str) -> Option<usize> {
    let inner = text.strip_prefix("{{")?.trim_start();
    let mut rest = if let Some(rest) = inner.strip_prefix("request") {
        rest
    } else {
        let mut rest = ["body", "headers", "query"].iter().find_map(|root| inner.strip_prefix(root))?;
        while let Some(segment) = rest.strip_prefix('.') {
            let end =
                segment.find(|c: char| matches!(c, '{' | '}' | '.') || c.is_whitespace()).unwrap_or(segment.len());
            if end == 0 {
                break;
            }
            rest = &segment[end..];
        }
        rest
    };
    rest = rest.trim_start().strip_prefix("}}")?;
    Some(text.len() - rest.len())
}

/// A save blocked by the form, shown as one error toast each (`reportFailure(title, description)`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct FormError {
    pub title: &'static str,
    pub description: String,
}

impl FormError {
    fn new(title: &'static str, description: impl Into<String>) -> Self {
        Self { title, description: description.into() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Encoding {
    Hex,
    Base64,
}

impl Encoding {
    pub(super) fn label(self) -> &'static str {
        match self {
            Encoding::Hex => "hex",
            Encoding::Base64 => "base64",
        }
    }
}

/// The webhook fields of the editor's `DraftState`, as plain text.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WebhookDraft {
    pub max_delivery_age_minutes: String,
    pub replay_key: String,
    pub coalesce_key: String,
    pub coalesce_window_seconds: String,
    pub signature_enabled: bool,
    pub signature_header: String,
    pub signature_encoding: Encoding,
    pub signature_prefix: String,
    /// Write-only: empty keeps the secret already stored on the server.
    pub signature_secret: String,
    pub filters: Vec<FilterDraft>,
    /// The filter rows the form opened with.
    pub saved_filters: Vec<FilterDraft>,
}

impl WebhookDraft {
    /// The webhook half of `taskToDraft`; a non-webhook or missing schedule gives `EMPTY_DRAFT`'s values.
    pub(super) fn from_schedule(schedule: Option<&Value>) -> Self {
        let webhook = schedule.filter(|s| s["type"] == "webhook");
        let field = |key: &str| webhook.and_then(|s| s.get(key)).filter(|v| !v.is_null());
        let dedupe = |key: &str| field("dedupe").and_then(|d| d.get(key));
        let text = |value: Option<&Value>| match value {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Number(number)) => number.to_string(),
            _ => String::new(),
        };
        let signature = field("signature").filter(|s| s.is_object());
        let filters = filter_drafts(field("filters"));
        Self {
            max_delivery_age_minutes: text(field("maxDeliveryAgeMinutes")),
            replay_key: text(dedupe("replayKey")),
            coalesce_key: text(dedupe("coalesceKey")),
            coalesce_window_seconds: text(dedupe("coalesceWindowSeconds")),
            signature_enabled: signature.is_some(),
            signature_header: signature
                .and_then(|s| s["header"].as_str())
                .unwrap_or(DEFAULT_SIGNATURE_HEADER)
                .to_owned(),
            signature_encoding: match signature.and_then(|s| s["encoding"].as_str()) {
                Some("base64") => Encoding::Base64,
                _ => Encoding::Hex,
            },
            signature_prefix: signature
                .and_then(|s| s["prefix"].as_str())
                .unwrap_or(DEFAULT_SIGNATURE_PREFIX)
                .to_owned(),
            signature_secret: String::new(),
            saved_filters: filters.clone(),
            filters,
        }
    }

    /// The webhook half of `scheduleDraftErrors`.
    fn errors(&self) -> Vec<FormError> {
        if parse_max_delivery_age(&self.max_delivery_age_minutes).is_err() {
            return vec![FormError::new(
                "Invalid age limit",
                format!("Enter whole minutes from 1 to {MAX_WEBHOOK_DELIVERY_AGE_MINUTES}, or leave it blank."),
            )];
        }
        let mut errors = Vec::new();
        for (label, value) in [("Duplicate request key", &self.replay_key), ("Coalesce key", &self.coalesce_key)] {
            let value = value.trim();
            if !value.is_empty() && !is_dedupe_template(value) {
                errors.push(FormError::new(
                    "Invalid webhook settings",
                    format!("{label} must contain a valid request placeholder."),
                ));
            }
        }
        let window = self.coalesce_window_seconds.trim();
        if !window.is_empty() && !js_integer(window).is_some_and(|seconds| (10..=3600).contains(&seconds)) {
            errors.push(FormError::new(
                "Invalid webhook settings",
                "Coalesce window must be whole seconds from 10 to 3600.",
            ));
        }
        errors
    }

    /// The `schedule` for `scheduledTasks.upsert`, with the editor's submit checks in its order:
    /// filters, then `scheduleDraftErrors`, then the signing secret. `has_stored_secret` is the live
    /// task's `webhook.hasSecret`.
    pub(super) fn schedule(&self, has_stored_secret: bool) -> Result<Value, Vec<FormError>> {
        let filters =
            parse_filter_drafts(&self.filters).map_err(|error| vec![FormError::new("Invalid filter", error)])?;
        let errors = self.errors();
        if !errors.is_empty() {
            return Err(errors);
        }
        let header = self.signature_header.trim();
        let secret = self.signature_secret.trim();
        if self.signature_enabled && (header.is_empty() || (secret.is_empty() && !has_stored_secret)) {
            return Err(vec![FormError::new("Signing secret is required", "Enter the signature header and secret.")]);
        }
        let signature = if self.signature_enabled {
            let mut signature =
                json!({"header": header, "encoding": self.signature_encoding.label(), "prefix": self.signature_prefix});
            if !secret.is_empty() {
                signature["secret"] = secret.into();
            }
            signature
        } else {
            Value::Null
        };
        let mut dedupe = Map::new();
        for (key, value) in [("replayKey", self.replay_key.trim()), ("coalesceKey", self.coalesce_key.trim())] {
            if !value.is_empty() {
                dedupe.insert(key.into(), value.into());
            }
        }
        if let Some(seconds) = js_integer(&self.coalesce_window_seconds) {
            dedupe.insert("coalesceWindowSeconds".into(), seconds.into());
        }
        let max_age = parse_max_delivery_age(&self.max_delivery_age_minutes).unwrap_or_default();
        let mut schedule = json!({
            "type": "webhook",
            "signature": signature,
            "maxDeliveryAgeMinutes": max_age,
            "dedupe": dedupe,
        });
        // Only edited filters are sent; the server keeps the stored ones otherwise.
        if filter_drafts_changed(&self.saved_filters, &self.filters) {
            schedule["filters"] = filters.into();
        }
        Ok(schedule)
    }
}

/// `webhookAddress`: what the URL field shows and whether it can be copied.
#[derive(Debug, PartialEq)]
pub(super) struct WebhookAddress {
    pub address: String,
    pub copyable: bool,
    pub note: Option<&'static str>,
}

/// `endpoint` is a task's `webhook` (`ScheduledTaskWebhookEndpoint`); `http_base` the environment's HTTP origin.
pub(super) fn webhook_address(endpoint: &Value, http_base: Option<&str>) -> WebhookAddress {
    let path = endpoint["path"].as_str().unwrap_or_default();
    if let Some(url) = endpoint["url"].as_str() {
        return WebhookAddress { address: url.to_owned(), copyable: true, note: None };
    }
    let joined = http_base.and_then(|base| reqwest::Url::parse(base).ok()?.join(path).ok());
    let Some(url) = joined else {
        return WebhookAddress {
            address: path.to_owned(),
            copyable: false,
            note: Some("Link this environment to T3 Connect for a public URL."),
        };
    };
    // `isLocalLoopbackHost`: localhost, ::1 or 127.0.0.0/8.
    let host = url.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host == "::1"
        || host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.octets()[0] == 127);
    WebhookAddress {
        address: url.to_string(),
        copyable: true,
        note: Some(if loopback {
            "Only this computer can call this address. Link T3 Connect for a public URL."
        } else {
            "Works wherever this environment's address is reachable, for example over Tailscale or your own proxy. Link T3 Connect for a public URL."
        }),
    }
}

/// `webhookDeliveryLabel`.
pub(super) fn delivery_label(outcome: &str, pending_window: bool) -> String {
    match outcome {
        "accepted" if pending_window => "Opened window",
        "accepted" => "Ran",
        "dispatch_failed" => "Run failed",
        "duplicate" => "Duplicate",
        "coalesced" => "Coalesced",
        "filtered" => "Filtered",
        "rejected_signature" => "Bad signature",
        "disabled" => "Task paused",
        "rate_limited" => "Rate limited",
        "expired" => "Too old",
        other => return other.replace('_', " "),
    }
    .to_owned()
}

/// The web `Badge` variants a delivery or run status uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    Success,
    Warning,
    Error,
    #[allow(dead_code, reason = "the task list's running status badge uses it")]
    Info,
    Outline,
}

/// `deliveryOutcomeVariant`, with a pending coalesce window shown as "coalesced".
pub(super) fn delivery_tone(outcome: &str, pending_window: bool) -> Tone {
    match if pending_window { "coalesced" } else { outcome } {
        "accepted" => Tone::Success,
        "duplicate" | "coalesced" | "disabled" | "rate_limited" | "expired" => Tone::Warning,
        "dispatch_failed" | "rejected_signature" => Tone::Error,
        _ => Tone::Outline,
    }
}

/// `relativeLabel` in `ScheduledTasksSettings.tsx`: past times like `formatRelativeTime`, future ones as "in 5m".
pub(super) fn relative_label(value: Option<&str>, now_ms: i64) -> String {
    let Some(at) = value.and_then(crate::timestamp::parse_ms) else { return "Not scheduled".into() };
    let diff = at - now_ms;
    if diff <= 0 {
        let minutes = -diff / 60_000;
        return match minutes {
            0 => "just now".into(),
            1..60 => format!("{minutes}m ago"),
            60..1440 => format!("{}h ago", minutes / 60),
            _ => format!("{}d ago", minutes / 1440),
        };
    }
    let minutes = (diff + 59_999) / 60_000;
    if minutes < 2 {
        return "in under a minute".into();
    }
    if minutes < 60 {
        return format!("in {minutes}m");
    }
    let hours = (minutes as f64 / 60.0).round() as i64;
    if hours < 24 {
        return format!("in {hours}h");
    }
    format!("in {}d", (hours as f64 / 24.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, op: FilterOp, value: &str) -> FilterDraft {
        FilterDraft { path: path.into(), op, value: value.into() }
    }

    #[test]
    fn filter_rows_round_trip_and_name_the_first_bad_row() {
        let stored = json!([
            {"path": "body.action", "op": "in", "value": ["opened", "reopened"]},
            {"path": "headers.x-event", "op": "exists"},
            {"path": "query.ref", "op": "notEquals", "value": "main"},
        ]);
        let drafts = filter_drafts(Some(&stored));
        assert_eq!(drafts[0], row("body.action", FilterOp::In, "opened, reopened"));
        assert_eq!(drafts[1].value, "");
        assert_eq!(parse_filter_drafts(&drafts).unwrap(), stored.as_array().unwrap().clone());

        let in_list = parse_filter_drafts(&[row(" body.labels ", FilterOp::In, " a, ,b ,")]).unwrap();
        assert_eq!(in_list, vec![json!({"path": "body.labels", "op": "in", "value": ["a", "b"]})]);
        // An empty `equals` value is valid, as in the server schema.
        assert!(parse_filter_drafts(&[row("body", FilterOp::Equals, "")]).is_ok());

        let error = |rows: &[FilterDraft]| parse_filter_drafts(rows).unwrap_err();
        assert_eq!(
            error(&[row("body.a", FilterOp::Exists, ""), row("bodyx", FilterOp::Equals, "v")]),
            "Filter 2 needs a path under body, headers or query, and a value."
        );
        assert_eq!(
            error(&[row("body.a", FilterOp::In, " , ")]),
            "Filter 1 needs a path under body, headers or query, and at least one value."
        );
        assert_eq!(error(&[row("", FilterOp::Exists, "")]), "Filter 1 needs a path under body, headers or query.");
        assert_eq!(error(&vec![row("body", FilterOp::Exists, ""); 17]), "Use at most 16 filters.");
    }

    #[test]
    fn filters_change_ignores_exists_values() {
        let saved = vec![row("body.a", FilterOp::Exists, "")];
        assert!(!filter_drafts_changed(&saved, &[row("body.a", FilterOp::Exists, "typed")]));
        assert!(filter_drafts_changed(&saved, &[row("body.a", FilterOp::Equals, "")]));
        assert!(filter_drafts_changed(&saved, &[]));
    }

    #[test]
    fn dedupe_templates_need_a_placeholder_and_no_stray_braces() {
        for valid in
            ["{{headers.delivery-id}}", "{{ body.pull_request.id }}:{{request}}", "x-{{query.a.b}}", "{{{body}}"]
        {
            assert!(is_dedupe_template(valid), "{valid}");
        }
        for invalid in [
            "",
            "plain",
            "{{bodyx}}",
            "{{body.}}",
            "{{body}}}}",
            "{{other}}",
            "{{body}} {{",
            "{{requests}}",
            "{{body..a}}",
        ] {
            assert!(!is_dedupe_template(invalid), "{invalid}");
        }
    }

    fn draft() -> WebhookDraft {
        WebhookDraft::from_schedule(None)
    }

    #[test]
    fn schedule_matches_the_web_payload() {
        assert_eq!(
            draft().schedule(false).unwrap(),
            json!({"type": "webhook", "signature": null, "maxDeliveryAgeMinutes": null, "dedupe": {}})
        );
        let filled = WebhookDraft {
            max_delivery_age_minutes: " 30 ".into(),
            replay_key: " {{headers.id}} ".into(),
            coalesce_window_seconds: "120".into(),
            signature_enabled: true,
            signature_secret: " s3cret ".into(),
            filters: vec![row("body.action", FilterOp::Equals, "opened")],
            ..draft()
        };
        assert_eq!(
            filled.schedule(false).unwrap(),
            json!({
                "type": "webhook",
                "signature": {"header": "x-hub-signature-256", "encoding": "hex", "prefix": "sha256=", "secret": "s3cret"},
                "maxDeliveryAgeMinutes": 30,
                "dedupe": {"replayKey": "{{headers.id}}", "coalesceWindowSeconds": 120},
                "filters": [{"path": "body.action", "op": "equals", "value": "opened"}],
            })
        );
    }

    #[test]
    fn task_round_trips_through_the_draft() {
        let schedule = json!({
            "type": "webhook",
            "signature": {"header": "x-sig", "encoding": "base64", "prefix": ""},
            "maxDeliveryAgeMinutes": 60,
            "dedupe": {"coalesceKey": "{{body.id}}", "coalesceWindowSeconds": 30},
            "filters": [{"path": "headers.x-event", "op": "exists"}],
        });
        let draft = WebhookDraft::from_schedule(Some(&schedule));
        assert_eq!((draft.signature_encoding, draft.signature_prefix.as_str()), (Encoding::Base64, ""));
        assert_eq!(draft.coalesce_window_seconds, "30");
        // The stored secret stands in for an empty one; unchanged filters stay off the wire.
        let mut expected = schedule.clone();
        expected.as_object_mut().unwrap().remove("filters");
        assert_eq!(draft.schedule(true).unwrap(), expected);
        assert_eq!(draft.schedule(false).unwrap_err()[0].title, "Signing secret is required");
    }

    #[test]
    fn schedule_errors_follow_submit_order() {
        let titles = |draft: WebhookDraft| -> Vec<String> {
            draft.schedule(false).unwrap_err().into_iter().map(|e| format!("{}: {}", e.title, e.description)).collect()
        };
        let bad_filter = WebhookDraft {
            filters: vec![row("x", FilterOp::Exists, "")],
            max_delivery_age_minutes: "0".into(),
            ..draft()
        };
        assert_eq!(titles(bad_filter), ["Invalid filter: Filter 1 needs a path under body, headers or query."]);
        for age in ["0", "1.5", "1441", "abc"] {
            let age_draft = WebhookDraft { max_delivery_age_minutes: age.into(), replay_key: "bad".into(), ..draft() };
            assert_eq!(
                titles(age_draft),
                ["Invalid age limit: Enter whole minutes from 1 to 1440, or leave it blank."]
            );
        }
        let keys = WebhookDraft {
            replay_key: "x".into(),
            coalesce_key: "{{y}}".into(),
            coalesce_window_seconds: "9".into(),
            ..draft()
        };
        assert_eq!(
            titles(keys),
            [
                "Invalid webhook settings: Duplicate request key must contain a valid request placeholder.",
                "Invalid webhook settings: Coalesce key must contain a valid request placeholder.",
                "Invalid webhook settings: Coalesce window must be whole seconds from 10 to 3600.",
            ]
        );
    }

    #[test]
    fn webhook_address_prefers_the_relay_url() {
        let relay = json!({"path": "/hooks/t1", "url": "https://relay.example/h/t1", "hasSecret": false});
        assert_eq!(
            webhook_address(&relay, Some("http://127.0.0.1:3999")),
            WebhookAddress { address: "https://relay.example/h/t1".into(), copyable: true, note: None }
        );
        let local = json!({"path": "/hooks/t1", "url": null, "hasSecret": false});
        let on_loopback = webhook_address(&local, Some("http://127.0.0.1:3999"));
        assert_eq!(on_loopback.address, "http://127.0.0.1:3999/hooks/t1");
        assert!(on_loopback.note.unwrap().starts_with("Only this computer"));
        assert!(webhook_address(&local, Some("http://[::1]:3999/")).note.unwrap().starts_with("Only this computer"));
        assert!(webhook_address(&local, Some("http://box.tail1.ts.net")).note.unwrap().starts_with("Works wherever"));
        let unreachable = webhook_address(&local, None);
        assert_eq!((unreachable.address.as_str(), unreachable.copyable), ("/hooks/t1", false));
    }

    #[test]
    fn delivery_labels_and_tones() {
        assert_eq!(delivery_label("accepted", true), "Opened window");
        assert_eq!(delivery_label("rejected_signature", false), "Bad signature");
        assert_eq!(delivery_label("some_new_outcome", false), "some new outcome");
        assert_eq!(delivery_tone("accepted", true), Tone::Warning);
        assert_eq!(delivery_tone("filtered", false), Tone::Outline);
        assert_eq!(delivery_tone("dispatch_failed", false), Tone::Error);
    }

    #[test]
    fn relative_labels_cover_past_and_future() {
        let now = crate::timestamp::parse_ms("2026-11-05T12:00:00.000Z").unwrap();
        let at = |iso: &str| relative_label(Some(iso), now);
        assert_eq!(at("2026-11-05T11:59:30Z"), "just now");
        assert_eq!(at("2026-11-05T09:00:00Z"), "3h ago");
        assert_eq!(at("2026-11-05T12:00:30Z"), "in under a minute");
        assert_eq!(at("2026-11-05T12:45:00Z"), "in 45m");
        assert_eq!(at("2026-11-07T12:00:00Z"), "in 2d");
        assert_eq!(relative_label(None, now), "Not scheduled");
    }
}
