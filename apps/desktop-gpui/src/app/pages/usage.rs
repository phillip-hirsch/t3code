//! The Usage modal (`apps/web/src/routes/_chat.usage.tsx`: `UsagePage` in an `AppRouteModal`
//! over the last main view). Cost and Tokens read `server.getUsageSummary`; Limits shows the
//! subscription windows providers report, refreshed through `server.refreshProviders`.

mod prices;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use chrono::Local;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::{FocusTrapElement as _, Popup};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, ThemeStyled as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, BoxShadow, Context, ElementId, FocusHandle, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseMoveEvent, ParentElement as _, PathBuilder, Pixels, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window, canvas, div, point, px,
    relative, rgb, svg,
};
use serde_json::{Value, json};

use super::super::Workspace;
use super::super::sidebar::now_ms;
use crate::theme::{self, Tokens};
use crate::usage::{self as logic, Merged, PeriodKey, Pool, PoolWindow, Summary, Window as UsageWindow};

/// Limits re-probe on their own at most this often; the refresh button always does.
const LIMITS_AUTO_REFRESH_MS: i64 = 5 * 60_000;

pub(in crate::app) struct UsageState {
    metric: &'static str,
    days: u32,
    /// Usage provider kinds the reader turned off.
    hidden: Vec<String>,
    /// The environments the reader picked; `None` is all of them, new ones included.
    selected: Option<Vec<SharedString>>,
    window: UsageWindow,
    /// Each environment's last answer and the window they answer; a new window keeps them until it lands.
    answers: Option<(Vec<logic::EnvironmentUsage>, UsageWindow)>,
    /// Environments whose last read failed, with why.
    failures: Vec<(SharedString, String)>,
    /// The selected answers combined, as shown.
    summary: Option<(Rc<Summary>, UsageWindow)>,
    merged: Rc<Merged>,
    /// Answers left out of the totals for their contract version.
    mismatches: Vec<logic::ContractMismatch>,
    /// When the last answer landed, and each environment's prices and aliases the read went out with.
    answered_at: Option<i64>,
    scan_settings: Vec<UsageScan>,
    /// Environments whose usage read is still out.
    pending: Vec<SharedString>,
    refreshing: bool,
    generation: u64,
    limits_now: i64,
    /// Per environment, as the web's `refreshUsageLimits`: probes still out, and when an
    /// automatic probe may run again.
    limits_pending: Vec<SharedString>,
    limits_after: HashMap<SharedString, i64>,
    by_time: bool,
    /// The chart column under the pointer, and where the pointer is in the plot.
    hover: Option<(usize, Pixels, Pixels)>,
    scroll: ScrollHandle,
    /// The panel's focus and focus trap, made on its first render; opening moves focus into it.
    focus: Option<FocusHandle>,
    focus_on_render: bool,
    /// Reset credits being confirmed, spent, or spent with the outcome, one per account key.
    redeemed: Vec<Redeem>,
    /// The chart's data, kept across pointer moves; render may refill it, hence the cell.
    chart: RefCell<Option<Rc<ChartData>>>,
    /// The limit pools and what they were built from: each environment's config revision,
    /// the hidden drivers, `limits_now`.
    pools: Option<(PoolsKey, Rc<[Pool]>)>,
    card: SegmentCard,
    /// Each limit segment's focus handle, by [`segment_key`].
    segment_focus: HashMap<String, FocusHandle>,
}

/// A connected environment, its socket and what a read there is priced and filtered with.
type UsageScan = (SharedString, crate::rpc::SocketId, Value);

type PoolsKey = (Vec<(SharedString, u64)>, Vec<String>, i64);

/// What the usage chart draws, derived when its inputs change rather than on every pointer move.
pub(in crate::app) struct ChartData {
    /// The merged answer (held, so its address stays unique), window, providers and metric it was built from.
    key: (Rc<Merged>, UsageWindow, Vec<&'static str>, bool),
    periods: Vec<PeriodKey>,
    columns: Vec<Vec<f64>>,
    peak: f64,
    max: f64,
    ticks: Vec<f64>,
    step_x: f32,
    /// Each provider's slot, curve and total, the largest total first.
    series: Rc<[(usize, Vec<logic::Segment>, f64)]>,
}

impl ChartData {
    fn new(merged: &Rc<Merged>, window: &UsageWindow, providers: &[&'static str], tokens: bool) -> Self {
        let periods = window.periods();
        let columns: Vec<Vec<f64>> = periods
            .iter()
            .map(|key| {
                let period = key.find(merged);
                providers.iter().map(|p| period.map_or(0., |period| period.value(p, tokens))).collect()
            })
            .collect();
        let peak = columns.iter().flatten().fold(0f64, |max, v| max.max(*v));
        let (max, ticks) = logic::nice_scale(peak);
        let step_x = if periods.len() < 2 { 0. } else { logic::VIEW_WIDTH / (periods.len() - 1) as f32 };
        // Each series is drawn from zero; the largest total sits at the back.
        let mut series: Vec<(usize, Vec<logic::Segment>, f64)> = (0..providers.len())
            .map(|slot| {
                let points: Vec<(f32, f32)> = columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| (index as f32 * step_x, logic::value_to_y(column[slot], max)))
                    .collect();
                let total = columns.iter().map(|c| c[slot]).sum();
                (slot, logic::smooth_curve(&points), total)
            })
            .collect();
        series.sort_by(|l, r| r.2.total_cmp(&l.2));
        Self {
            key: (merged.clone(), window.clone(), providers.to_vec(), tokens),
            periods,
            columns,
            peak,
            max,
            ticks,
            step_x,
            series: Rc::from(series),
        }
    }
}

pub(in crate::app) struct Redeem {
    key: String,
    name: String,
    status: RedeemStatus,
}

enum RedeemStatus {
    Confirming,
    Running,
    Done(String),
}

impl UsageState {
    /// A credit for this account is being confirmed or spent, so another click must not spend a second one.
    fn redeem_busy(&self, key: &str) -> bool {
        redeem_busy(&self.redeemed, key)
    }

    fn set_redeem(&mut self, key: &str, name: &str, status: RedeemStatus) {
        self.redeemed.retain(|r| r.key != key);
        self.redeemed.push(Redeem { key: key.to_owned(), name: name.to_owned(), status });
    }
}

fn redeem_busy(redeemed: &[Redeem], key: &str) -> bool {
    redeemed.iter().any(|r| r.key == key && !matches!(r.status, RedeemStatus::Done(_)))
}

/// `PopoverTrigger`'s default `delay` before a hovered segment opens its card.
const CARD_OPEN_DELAY: Duration = Duration::from_millis(300);
/// Long enough for the pointer to cross the gap between a segment and its card.
const CARD_CLOSE_DELAY: Duration = Duration::from_millis(100);

/// The limit segments' one shared card (`SegmentPopoverContext`): hovering a segment opens it
/// there and moves it between segments; a click, Enter or Space pins it until dismissed.
#[derive(Default)]
pub(in crate::app) struct SegmentCard {
    /// The segment the card describes.
    open: Option<String>,
    pinned: bool,
    /// The segment under the pointer, and whether the pointer is on the card.
    hovered_segment: Option<String>,
    hovered_card: bool,
    email_revealed: bool,
    timer: Option<Task<()>>,
}

impl SegmentCard {
    /// The pointer entered or left segment `key`; `Some` asks to [`Self::settle`] after a delay.
    fn hover_segment(&mut self, key: &str, hovering: bool) -> Option<Duration> {
        if hovering {
            self.hovered_segment = Some(key.to_owned());
        } else if self.hovered_segment.as_deref() == Some(key) {
            self.hovered_segment = None;
        } else {
            return None;
        }
        self.timer = None;
        match (self.open.as_deref(), hovering) {
            (None, true) => Some(CARD_OPEN_DELAY),
            (Some(open), true) if !self.pinned && open != key => {
                self.show(key);
                None
            }
            (Some(_), false) if !self.pinned && !self.hovered_card => Some(CARD_CLOSE_DELAY),
            _ => None,
        }
    }

    fn hover_card(&mut self, hovering: bool) -> Option<Duration> {
        self.hovered_card = hovering;
        self.timer = None;
        (!hovering && self.open.is_some() && !self.pinned && self.hovered_segment.is_none()).then_some(CARD_CLOSE_DELAY)
    }

    /// A delayed hover change lands: the hovered segment's card opens, or it closes once
    /// neither a segment nor the card is under the pointer.
    fn settle(&mut self) {
        self.timer = None;
        if self.pinned {
            return;
        }
        match self.hovered_segment.clone() {
            Some(key) if self.open.is_none() => self.show(&key),
            None if !self.hovered_card => self.close(),
            _ => {}
        }
    }

    /// A click, Enter or Space on segment `key`: pins its hover-opened card, opens it pinned,
    /// or closes it when it was already pinned.
    fn press(&mut self, key: &str) {
        self.timer = None;
        if self.open.as_deref() == Some(key) && self.pinned {
            self.close();
        } else {
            self.show(key);
            self.pinned = true;
        }
    }

    fn show(&mut self, key: &str) {
        if self.open.as_deref() != Some(key) {
            self.email_revealed = false;
        }
        self.open = Some(key.to_owned());
    }

    fn close(&mut self) {
        *self = Self { hovered_segment: self.hovered_segment.take(), ..Self::default() };
    }
}

/// A usage window ending now, in the system time zone.
fn local_window(days: u32) -> UsageWindow {
    let zone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
    UsageWindow::new(days, crate::timestamp::now_ms(), &Local, zone)
}

/// `t3code:usage-page-preferences:v1`, kept in `client-settings.json` as the web keeps it in local storage.
const PREFERENCES_KEY: &str = "usagePagePreferences";

/// `readUsagePagePreferences`: the stored metric, period and hidden providers, or the
/// defaults (Limits, 30 days, every provider) when any of them does not decode.
fn read_preferences(stored: &Value) -> (&'static str, u32, Vec<String>) {
    let metric = logic::METRICS.iter().map(|(value, _)| *value).find(|value| stored["metric"] == *value);
    let days = logic::WINDOWS.iter().map(|(days, _)| *days).find(|days| stored["windowDays"] == *days);
    let known =
        |kind: &Value| kind.as_str().filter(|kind| logic::PROVIDERS.iter().any(|p| p.0 == *kind)).map(str::to_owned);
    let hidden = match &stored["hiddenProviders"] {
        Value::Null => Some(Vec::new()),
        Value::Array(kinds) => kinds.iter().map(known).collect(),
        _ => None,
    };
    match (metric, days, hidden) {
        (Some(metric), Some(days), Some(hidden)) => (metric, days, hidden),
        _ => ("limits", 30, Vec::new()),
    }
}

impl UsageState {
    /// The page as the last session left it.
    pub(in crate::app) fn new(client_settings: &crate::settings::Settings) -> Self {
        let (metric, days, hidden) = read_preferences(client_settings.get(PREFERENCES_KEY));
        Self { metric, days, hidden, window: local_window(days), ..Self::default() }
    }

    /// `saveUsagePagePreferences`.
    fn preferences(&self) -> serde_json::Map<String, Value> {
        let stored = json!({"metric": self.metric, "windowDays": self.days, "hiddenProviders": self.hidden});
        serde_json::Map::from_iter([(PREFERENCES_KEY.to_owned(), stored)])
    }
}

impl Default for UsageState {
    fn default() -> Self {
        Self {
            metric: "limits",
            days: 30,
            hidden: Vec::new(),
            selected: None,
            window: local_window(30),
            answers: None,
            failures: Vec::new(),
            summary: None,
            merged: Rc::default(),
            mismatches: Vec::new(),
            answered_at: None,
            scan_settings: Vec::new(),
            pending: Vec::new(),
            refreshing: false,
            generation: 0,
            limits_now: now_ms(),
            limits_pending: Vec::new(),
            limits_after: HashMap::new(),
            by_time: false,
            hover: None,
            scroll: ScrollHandle::new(),
            focus: None,
            focus_on_render: false,
            redeemed: Vec::new(),
            pools: None,
            chart: RefCell::default(),
            card: SegmentCard::default(),
            segment_focus: HashMap::new(),
        }
    }
}

impl UsageState {
    fn tokens(&self) -> bool {
        self.metric == "tokens"
    }

    fn is_selected(&self, environment_id: &str) -> bool {
        self.selected.as_ref().is_none_or(|ids| ids.iter().any(|id| id == environment_id))
    }

    /// The selected environments' answers as one summary, or `None` while none answered.
    fn recombine(&mut self) {
        let (summary, mismatches) = self
            .answers
            .as_ref()
            .and_then(|(answers, window)| {
                let picked = answers.iter().filter(|a| self.is_selected(&a.environment_id)).cloned().collect();
                let (summary, mismatches) = logic::combine(picked)?;
                Some((Some((Rc::new(summary), window.clone())), mismatches))
            })
            .unwrap_or_default();
        (self.summary, self.mismatches) = (summary, mismatches);
        self.merged = Rc::new(match &self.summary {
            Some((summary, _)) => logic::merge(summary, &self.hidden),
            None => Merged::default(),
        });
    }

    /// Whether the shown usage is read again: the scan's environments or settings changed,
    /// whether or not the last read failed, or a pick of environments the last read missed.
    fn needs_reread(&self, scan_changed: bool) -> bool {
        let unread = self.summary.is_none() && self.answers.is_some() && self.error().is_none();
        !self.loading() && (scan_changed || unread)
    }

    /// A read of `ids` goes out; answers and failures from environments it no longer reads drop.
    fn start_read(&mut self, ids: Vec<SharedString>) {
        self.generation += 1;
        self.failures.retain(|(id, _)| ids.contains(id));
        if let Some((answers, _)) = &mut self.answers {
            answers.retain(|kept| ids.iter().any(|id| id.as_ref() == kept.environment_id));
        }
        self.pending = ids;
        self.recombine();
    }

    /// A selected environment's usage read is still out.
    fn loading(&self) -> bool {
        self.pending.iter().any(|id| self.is_selected(id))
    }

    /// One environment's answer to a read of `window`, shown as soon as it lands; the first
    /// answer for a new window replaces the kept ones. Whether to ask again once its slow
    /// sources finish (`awaitRefresh`).
    fn publish(
        &mut self,
        environment_id: &SharedString,
        result: Result<Value, crate::rpc::RpcError>,
        window: &UsageWindow,
        first_attempt: bool,
    ) -> bool {
        self.failures.retain(|(id, _)| id != environment_id);
        let summary = match result.map(serde_json::from_value::<Summary>) {
            Ok(Ok(summary)) => Some(summary),
            Ok(Err(error)) => {
                self.failures.push((environment_id.clone(), format!("Unreadable usage summary: {error}")));
                None
            }
            Err(error) => {
                self.failures.push((environment_id.clone(), usage_error(&error)));
                None
            }
        };
        let again = first_attempt && summary.as_ref().is_some_and(Summary::refreshing);
        match summary {
            Some(summary) => {
                let answer = logic::EnvironmentUsage { environment_id: environment_id.to_string(), summary };
                let answers = match &mut self.answers {
                    Some((answers, shown)) if shown == window => answers,
                    answers => &mut answers.insert((Vec::new(), window.clone())).0,
                };
                match answers.iter_mut().find(|kept| kept.environment_id == environment_id.as_ref()) {
                    Some(kept) => *kept = answer,
                    None => answers.push(answer),
                }
                self.answered_at = Some(now_ms());
            }
            // A machine that fails to answer leaves the others' usage showing.
            None => {
                if let Some((answers, _)) = &mut self.answers {
                    answers.retain(|kept| kept.environment_id != environment_id.as_ref());
                }
            }
        }
        if !again {
            self.pending.retain(|id| id != environment_id);
        }
        if !self.loading() {
            self.refreshing = false;
        }
        self.recombine();
        again
    }

    /// Whether to probe this environment's limits now: never while a probe is out, and an
    /// automatic one only past its own cooldown.
    fn limits_due(&self, environment_id: &SharedString, automatic: bool, now: i64) -> bool {
        !self.limits_pending.contains(environment_id)
            && (!automatic || self.limits_after.get(environment_id).is_none_or(|after| now >= *after))
    }

    /// A probe of this environment's limits settled.
    fn limits_probed(&mut self, environment_id: &SharedString, now: i64) {
        self.limits_pending.retain(|id| id != environment_id);
        self.limits_after.insert(environment_id.clone(), now + LIMITS_AUTO_REFRESH_MS);
        self.limits_now = now;
        if !self.limits_pending.iter().any(|id| self.is_selected(id)) {
            self.refreshing = false;
        }
    }

    /// Why nothing shows: the first selected environment's failure.
    fn error(&self) -> Option<&str> {
        self.failures.iter().find(|(id, _)| self.is_selected(id)).map(|(_, message)| message.as_str())
    }

    fn hidden_drivers(&self) -> Vec<&'static str> {
        logic::PROVIDERS.iter().filter(|p| self.hidden.iter().any(|h| h == p.0)).map(|p| p.2).collect()
    }
}

/// What a usage read is priced and filtered with on one environment (`usageScanSettingsAtom`
/// and `usageModelFilterAtom`).
fn scan_settings(info: &crate::model::ServerInfo) -> Value {
    let settings = &info.settings;
    json!([settings.get("usagePriceOverrides"), settings.get("usageModelAliases"), settings.get("usageModelFilter")])
}

impl Workspace {
    pub(in crate::app) fn usage_opened(&mut self, cx: &mut Context<Self>) {
        self.usage.focus_on_render = true;
        self.usage.limits_now = now_ms();
        if self.usage.metric == "limits" {
            self.refresh_limits(true, cx);
        } else {
            self.revalidate_usage(cx);
        }
    }

    fn save_usage_preferences(&mut self) {
        let patch = self.usage.preferences();
        self.client_settings.apply(&patch);
    }

    fn select_usage_metric(&mut self, metric: &'static str, cx: &mut Context<Self>) {
        self.usage.metric = metric;
        self.save_usage_preferences();
        self.usage.hover = None;
        if metric == "limits" {
            self.usage.limits_now = now_ms();
            self.refresh_limits(true, cx);
        } else {
            self.revalidate_usage(cx);
        }
        cx.notify();
    }

    /// Reads usage again when the shown answer is stale or was priced differently; the old one
    /// stays on screen until the new one lands.
    fn revalidate_usage(&mut self, cx: &mut Context<Self>) {
        if !self.usage.loading()
            && logic::needs_read(self.usage.answered_at, now_ms(), self.usage_scan_settings_changed())
        {
            self.read_usage(false, cx);
        }
    }

    /// An environment connected, reconnected or left, or its prices or model aliases changed,
    /// since the last read went out.
    fn usage_scan_settings_changed(&self) -> bool {
        self.usage_scan() != self.usage.scan_settings
    }

    fn usage_scan(&self) -> Vec<UsageScan> {
        self.sessions()
            .iter()
            .filter_map(|s| Some((s.id.clone()?, s.client.as_ref()?.socket_id(), scan_settings(&s.server_info))))
            .collect()
    }

    /// `usage.*` on the open Usage page (`UsagePage.tsx`): `None` when `command` is not one;
    /// text fields keep their keys, and Limits has no period.
    pub(in crate::app) fn usage_shortcut(
        &mut self,
        command: &str,
        context: &crate::app::keymap::ShortcutContext,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        let name = command.strip_prefix("usage.");
        let metric = logic::METRICS.iter().map(|(value, _)| *value).find(|value| name == Some(*value));
        let days = match command {
            "usage.period.day" => Some(1),
            "usage.period.week" => Some(7),
            "usage.period.month" => Some(30),
            "usage.period.quarter" => Some(90),
            _ => None,
        };
        if metric.is_none() && days.is_none() {
            return None;
        }
        if !context.usage_page_open || context.editable_focus {
            return Some(false);
        }
        if let Some(metric) = metric {
            self.select_usage_metric(metric, cx);
        }
        if let Some(days) = days.filter(|_| self.usage.metric != "limits") {
            self.select_usage_window(days, cx);
        }
        Some(true)
    }

    fn select_usage_window(&mut self, days: u32, cx: &mut Context<Self>) {
        self.usage.days = days;
        self.save_usage_preferences();
        self.usage.window = local_window(days);
        self.usage.hover = None;
        self.read_usage(false, cx);
    }

    fn set_hidden_usage_providers(&mut self, hidden: Vec<String>, cx: &mut Context<Self>) {
        self.usage.hidden = hidden;
        self.save_usage_preferences();
        self.usage.recombine();
        cx.notify();
    }

    fn select_usage_environments(&mut self, selected: Option<Vec<SharedString>>, cx: &mut Context<Self>) {
        self.usage.selected = selected;
        self.usage.hover = None;
        self.usage.recombine();
        if self.usage.metric == "limits" {
            self.refresh_limits(true, cx);
        }
        cx.notify();
    }

    /// The environments the page reads: every connected one, with its `usageModelFilter`.
    fn usage_machines(&self) -> Vec<(SharedString, crate::rpc::RpcClient, Value)> {
        self.sessions()
            .iter()
            .filter_map(|s| {
                Some((s.id.clone()?, s.client.clone()?, s.server_info.settings.get("usageModelFilter").clone()))
            })
            .collect()
    }

    /// The selected environments' provider and limit-source reads, with their labels.
    fn limit_environments(&self) -> Vec<(SharedString, SharedString, &crate::model::ServerInfo)> {
        let mut environments = self.usage_environments();
        environments.retain(|(id, ..)| self.usage.is_selected(id));
        environments
    }

    /// Every connected environment, with its label.
    fn usage_environments(&self) -> Vec<(SharedString, SharedString, &crate::model::ServerInfo)> {
        self.sessions()
            .iter()
            .filter(|s| s.is_connected())
            .filter_map(|s| {
                let id = s.id.clone()?;
                let label = s.server_info.environment_label.clone().map_or_else(|| id.clone(), SharedString::from);
                Some((id, label, &s.server_info))
            })
            .collect()
    }

    /// `server.getUsageSummary` for the current window on every connected machine, combined so a
    /// directory several machines read counts once. A refresh re-reads model prices first
    /// (`server.refreshUsageRates`); answers with slow sources still scanning are shown, then
    /// asked again with `awaitRefresh`.
    fn read_usage(&mut self, refresh_rates: bool, cx: &mut Context<Self>) {
        let window = self.usage.window.clone();
        let machines: Vec<(SharedString, crate::rpc::RpcClient, Value)> = self
            .usage_machines()
            .into_iter()
            .map(|(id, client, filter)| (id, client, window.summary_input(&filter)))
            .collect();
        self.usage.scan_settings = self.usage_scan();
        let usage = &mut self.usage;
        usage.start_read(machines.iter().map(|(id, ..)| id.clone()).collect());
        if machines.is_empty() {
            usage.refreshing = false;
            return;
        }
        let generation = usage.generation;
        for (id, client, mut input) in machines {
            // A refresh re-prices the selected environments; every one is read so a pick shows at once.
            let reprice = refresh_rates && self.usage.is_selected(&id);
            let window = window.clone();
            // Each environment's answer shows when it lands, not when the slowest one does.
            cx.spawn(async move |this, cx| {
                if reprice {
                    let _ = client.request("server.refreshUsageRates", json!({})).await;
                }
                for attempt in 0..2 {
                    let result = client.request("server.getUsageSummary", input.clone()).await;
                    let mut again = false;
                    let applied = this.update(cx, |this, cx| {
                        if this.usage.generation == generation {
                            again = this.usage.publish(&id, result, &window, attempt == 0);
                            cx.notify();
                        }
                    });
                    if applied.is_err() || !again {
                        return;
                    }
                    input["awaitRefresh"] = json!(true);
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// Re-probes provider limits (`refreshUsageLimits`): automatic refreshes wait five minutes
    /// between probes.
    fn refresh_limits(&mut self, automatic: bool, cx: &mut Context<Self>) {
        let now = now_ms();
        let usage = &self.usage;
        let mut machines = self.usage_machines();
        machines.retain(|(id, ..)| usage.is_selected(id) && usage.limits_due(id, automatic, now));
        let usage = &mut self.usage;
        if !automatic {
            // A probe already out counts as this refresh, as the web awaits it.
            usage.refreshing = !machines.is_empty() || usage.limits_pending.iter().any(|id| usage.is_selected(id));
        }
        for (id, client, _) in machines {
            usage.limits_pending.push(id.clone());
            cx.spawn(async move |this, cx| {
                let result =
                    client.request("server.refreshProviders", json!({"workspaceSnapshotsOnDemand": true})).await;
                let _ = this.update(cx, |this, cx| {
                    // The answer lands on the environment that gave it, if it is still here.
                    if let (Ok(payload), Some(info)) = (result, this.env_info_mut(&id)) {
                        let event = json!({"type": "providerStatuses", "payload": payload});
                        if let Err(error) = info.apply(&event) {
                            log::warn!("refreshed providers did not decode: {error}");
                        }
                    }
                    this.usage.limits_probed(&id, now_ms());
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn hover_usage_card(&mut self, segment: Option<&str>, hovering: bool, cx: &mut Context<Self>) {
        let card = &mut self.usage.card;
        let delay = match segment {
            Some(key) => card.hover_segment(key, hovering),
            None => card.hover_card(hovering),
        };
        cx.notify();
        let Some(delay) = delay else { return };
        self.usage.card.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.usage.card.settle();
                cx.notify();
            });
        }));
    }

    /// Escape closes a limit segment's card before the modal.
    pub(in crate::app) fn usage_card_escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.usage.card.open.is_none() {
            return false;
        }
        self.usage.card.close();
        cx.notify();
        true
    }

    fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        if self.usage.refreshing {
            return;
        }
        if self.usage.metric == "limits" {
            self.refresh_limits(false, cx);
            return;
        }
        self.usage.window = local_window(self.usage.days);
        self.usage.refreshing = true;
        self.read_usage(true, cx);
    }

    /// "Use a reset credit?", then `provider.consumeResetCredit` (`useResetCredit`).
    fn redeem_reset_credit(
        &mut self,
        key: String,
        name: String,
        (environment, input): (String, Value),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.env_client(&environment) else { return };
        if self.usage.redeem_busy(&key) {
            return;
        }
        let previous = self.usage.redeemed.iter().position(|r| r.key == key).map(|i| self.usage.redeemed.remove(i));
        self.usage.set_redeem(&key, &name, RedeemStatus::Confirming);
        let answer = window.prompt(
            gpui_kit::PromptLevel::Warning,
            "Use a reset credit?",
            Some("This redeems one credit on your account and clears the current rate-limit windows. It cannot be undone."),
            &["Use credit", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            let confirmed = answer.await == Ok(0);
            let _ = this.update(cx, |this, cx| {
                if confirmed {
                    this.usage.set_redeem(&key, &name, RedeemStatus::Running);
                } else {
                    this.usage.redeemed.retain(|r| r.key != key);
                    this.usage.redeemed.extend(previous);
                }
                cx.notify();
            });
            if !confirmed {
                return;
            }
            let status = match client.request("provider.consumeResetCredit", input).await {
                Ok(result) => logic::reset_outcome_text(&result),
                Err(error) => error.to_string(),
            };
            let _ = this.update(cx, |this, cx| {
                this.usage.set_redeem(&key, &name, RedeemStatus::Done(status));
                cx.notify();
            });
        })
        .detach();
    }

    pub(in crate::app) fn render_usage_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let viewport = f32::from(window.viewport_size().width);
        let padding = modal_padding(viewport);
        // `@4xl/usage-header` (56rem) on the header's content box, inside the border and `ps-5 pe-2`.
        let compact = modal_panel_width(viewport) - 2. - 28. < 896.;
        // `AppRouteModal` focuses its surface, not its first control, and keeps Tab inside it.
        let focus = self.usage.focus.get_or_insert_with(|| cx.focus_handle()).clone();
        if std::mem::take(&mut self.usage.focus_on_render) {
            window.focus(&focus, cx);
        }
        div()
            .id("usage-modal")
            .absolute()
            .inset_0()
            .occlude()
            .bg(Hsla { a: if t.dark { 0.64 } else { 0.6 }, ..t.background })
            .flex()
            .items_center()
            .justify_center()
            .p(px(padding))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_page(cx)))
            .child(
                v_flex()
                    .id("usage-panel")
                    .size_full()
                    .max_w(px(1280.))
                    .max_h(px(960.))
                    .overflow_hidden()
                    .rounded(px(16.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.background)
                    .shadow_2xl()
                    .text_color(t.foreground)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    // `AppRouteModal` is a modal `Dialog`: Tab stays inside the panel.
                    .focus_trap("usage-trap", &focus)
                    .child(self.render_usage_header(&t, compact, window, cx))
                    .child(
                        div()
                            .id("usage-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.usage.scroll)
                            .child(
                                v_flex()
                                    .w_full()
                                    .max_w(px(1024.))
                                    .mx_auto()
                                    .px(px(page_gutter(viewport)))
                                    .pt(px(24.))
                                    .pb(px(48.))
                                    .gap(px(24.))
                                    .child(self.render_usage_body(&t, window, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_usage_header(
        &self,
        t: &Tokens,
        compact: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let usage = &self.usage;
        let limits = usage.metric == "limits";
        let view = cx.entity();
        let separator = || div().flex_none().text_color(t.muted_foreground).child("/");
        let environments = self.usage_environments();
        let selected: Vec<(SharedString, SharedString)> = environments
            .iter()
            .filter(|(id, ..)| usage.is_selected(id))
            .map(|(id, label, _)| (id.clone(), label.clone()))
            .collect();
        let failures: Vec<String> = usage
            .failures
            .iter()
            .filter_map(|(id, message)| {
                let (_, label) = selected.iter().find(|(selected, _)| selected == id)?;
                Some(format!("{label}: {message}"))
            })
            .collect();
        let mismatch_lines: Vec<String> = usage
            .mismatches
            .iter()
            .map(|mismatch| {
                let label = environments
                    .iter()
                    .find(|(id, ..)| id.as_ref() == mismatch.environment_id)
                    .map_or_else(|| mismatch.environment_id.clone(), |(_, label, _)| label.to_string());
                mismatch.message(&label)
            })
            .collect();
        let has_issue = !limits && (!failures.is_empty() || !mismatch_lines.is_empty());
        let env_label: SharedString = match (&usage.selected, selected.as_slice()) {
            (None, _) => "All environments".into(),
            (Some(_), [(_, label)]) => label.clone(),
            (Some(_), selected) => format!("{} environments", selected.len()).into(),
        };
        // `UsageEnvironmentFilter`'s per-environment status.
        let rows: Vec<(SharedString, SharedString, bool, SharedString, bool)> = environments
            .iter()
            .map(|(id, label, _)| {
                let failed = usage.failures.iter().any(|(failed, _)| failed == id);
                let answer = usage
                    .answers
                    .as_ref()
                    .and_then(|(answers, _)| answers.iter().find(|answer| answer.environment_id == id.as_ref()));
                let status = if failed {
                    "Unavailable"
                } else if answer.is_some_and(|answer| !answer.summary.compatible()) {
                    "Update required"
                } else if usage.pending.contains(id) && answer.is_none() {
                    "Scanning…"
                } else if usage.pending.contains(id) {
                    "Refreshing…"
                } else {
                    "Ready"
                };
                (id.clone(), label.clone(), usage.is_selected(id), status.into(), failed)
            })
            .collect();
        let all_ids: Vec<SharedString> = environments.iter().map(|(id, ..)| id.clone()).collect();
        let selected_ids: Vec<SharedString> = selected.iter().map(|(id, _)| id.clone()).collect();
        let all_selected = usage.selected.is_none();
        let env_filter = inline_menu_button("usage-environment", t)
            .text_color(t.foreground)
            .child(div().min_w_0().truncate().child(env_label))
            .child(if has_issue {
                Icon::new(Lucide::CircleAlert).with_size(px(14.)).text_color(t.warning_foreground)
            } else {
                Icon::new(Lucide::ChevronDown).with_size(px(14.)).text_color(Hsla { a: 0.6, ..t.muted_foreground })
            })
            .dropdown_menu({
                let view = view.clone();
                let (muted, destructive) = (t.muted_foreground, t.error_foreground);
                move |mut menu, window, _| {
                    menu = menu
                        .item(PopupMenuItem::new("All environments").checked(all_selected).on_click(
                            window.listener_for(&view, move |this, _, _, cx| {
                                this.select_usage_environments(if all_selected { Some(Vec::new()) } else { None }, cx);
                            }),
                        ))
                        .separator();
                    for (id, label, checked, status, failed) in rows.iter().cloned() {
                        let (all_ids, selected_ids) = (all_ids.clone(), selected_ids.clone());
                        let item = PopupMenuItem::element(move |_, _| {
                            h_flex()
                                .min_w(px(200.))
                                .gap(px(12.))
                                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                                .when(!limits, |el| {
                                    el.child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(if failed { destructive } else { muted })
                                            .child(status.clone()),
                                    )
                                })
                        })
                        .checked(checked)
                        .on_click(window.listener_for(&view, move |this, _, _, cx| {
                            let next = toggle_environment(&selected_ids, &id, &all_ids);
                            this.select_usage_environments(next, cx);
                        }));
                        menu = menu.item(item);
                    }
                    if rows.is_empty() {
                        menu = menu.label("No environments connected.");
                    }
                    // `UsageCoverageNotice`: why selected environments are missing from the totals.
                    if !limits && (!failures.is_empty() || !mismatch_lines.is_empty()) {
                        let lines = failures.iter().chain(&mismatch_lines).cloned();
                        menu = lines.fold(menu.separator(), gpui_kit::component::menu::PopupMenu::label);
                    }
                    menu.separator().item(PopupMenuItem::new("Model prices").icon(Lucide::SlidersHorizontal).on_click(
                        window.listener_for(&view, |this, _, window, cx| this.open_model_prices(None, window, cx)),
                    ))
                }
            });

        let hidden = usage.hidden.clone();
        let visible: Vec<&str> =
            logic::PROVIDERS.iter().map(|p| p.0).filter(|k| !hidden.iter().any(|h| h == k)).collect();
        let provider_label: String = match visible.len() {
            n if n == logic::PROVIDERS.len() => "All providers".into(),
            0 => "No providers".into(),
            1 => logic::provider_label(visible[0]).into(),
            n => format!("{n} providers"),
        };
        let provider_filter = inline_menu_button("usage-providers", t)
            .text_color(t.foreground)
            .child(provider_label)
            .child(Icon::new(Lucide::ChevronDown).with_size(px(14.)).text_color(Hsla { a: 0.6, ..t.muted_foreground }))
            .dropdown_menu({
                let view = view.clone();
                move |mut menu, window, _| {
                    let none_hidden = hidden.is_empty();
                    menu =
                        menu.item(PopupMenuItem::new("All providers").checked(none_hidden).on_click(
                            window.listener_for(&view, move |this, _, _, cx| {
                                let next = if none_hidden {
                                    logic::PROVIDERS.iter().map(|p| p.0.to_owned()).collect()
                                } else {
                                    Vec::new()
                                };
                                this.set_hidden_usage_providers(next, cx);
                            }),
                        ))
                        .separator();
                    for (kind, label, driver, _) in logic::PROVIDERS {
                        let kind: &'static str = kind;
                        let shown = !hidden.iter().any(|h| h == kind);
                        let mut item = PopupMenuItem::new(*label).checked(shown);
                        if let Some((path, _)) = crate::assets::provider_icon(driver) {
                            item = item.icon(Icon::empty().path(path));
                        }
                        menu = menu.item(item.on_click(window.listener_for(&view, move |this, _, _, cx| {
                            let mut next = this.usage.hidden.clone();
                            match next.iter().position(|h| h == kind) {
                                Some(index) => {
                                    next.remove(index);
                                }
                                None => next.push(kind.to_owned()),
                            }
                            this.set_hidden_usage_providers(next, cx);
                        })));
                    }
                    menu
                }
            });

        let periods = logic::WINDOWS.iter().map(|(days, label)| (*days, *label));
        let metrics = logic::METRICS.iter().map(|(value, label)| (*value, *label));
        let (periods, metrics) = if compact {
            // Below 56rem the groups fold into compact selects.
            let period = compact_select("usage-period", periods, usage.days, t, &view, Workspace::select_usage_window);
            let metric =
                compact_select("usage-metric", metrics, usage.metric, t, &view, Workspace::select_usage_metric);
            (period.into_any_element(), metric.into_any_element())
        } else {
            let pick_period = Workspace::select_usage_window;
            let period = segmented("usage-period", periods, usage.days, t, window, cx, pick_period);
            let metric =
                segmented("usage-metric", metrics, usage.metric, t, window, cx, Workspace::select_usage_metric);
            (period.into_any_element(), metric.into_any_element())
        };
        let disabled = usage.refreshing || (!limits && selected.is_empty());
        h_flex()
            .min_h(px(48.))
            .flex_none()
            .pl(px(20.))
            .pr(px(8.))
            .gap(px(4.))
            .border_b_1()
            .border_color(t.border)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(12.))
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(div().flex_none().text_color(t.muted_foreground).child("Usage"))
                    .child(separator())
                    .child(env_filter)
                    .child(separator())
                    .child(provider_filter),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(if compact { 4. } else { 8. }))
                    .when(!limits, |el| el.child(periods))
                    .child(metrics)
                    .child(
                        ghost_icon_button("usage-refresh", Lucide::RefreshCw, 14., t)
                            .disabled(disabled)
                            .tooltip(if limits { "Refresh limits" } else { "Refresh usage" })
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_usage(cx))),
                    ),
            )
            .child(
                ghost_icon_button("usage-close", Lucide::X, 16., t)
                    .tooltip("Close (Esc)")
                    .on_click(cx.listener(|this, _, _, cx| this.close_page(cx))),
            )
    }

    fn render_usage_body(&mut self, t: &Tokens, win: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let usage = &self.usage;
        let limits = usage.metric == "limits";
        if self.limit_environments().is_empty() {
            let what = if limits { "limits" } else { "usage" };
            let ask = if self.usage_environments().is_empty() { "Connect" } else { "Select" };
            return muted_line(format!("{ask} an environment to see {what}."), t).into_any_element();
        }
        let viewport = f32::from(win.viewport_size().width);
        if limits {
            return self.render_limits(t, viewport, cx);
        }
        // Saved prices or aliases, or a connection that came or went, reprice the scan
        // (`refreshTrigger`), even after a failure; a stale answer waits for a reopen.
        // So does a pick of environments the last read missed.
        if usage.needs_reread(self.usage_scan_settings_changed()) {
            let view = cx.entity();
            cx.defer(move |cx| view.update(cx, |this, cx| this.read_usage(false, cx)));
        }
        let usage = &self.usage;
        match (&usage.summary, usage.error()) {
            (None, Some(error)) => v_flex()
                .py(px(48.))
                .items_center()
                .text_sm()
                .text_color(t.muted_foreground)
                .child(error.to_owned())
                .into_any_element(),
            (None, None) => usage_skeleton(t, viewport).into_any_element(),
            (Some((summary, window)), _) => self.render_usage_overview(summary, window, t, win, cx),
        }
    }

    /// The limit pools, rebuilt only when the server config, the hidden drivers or the clock moved.
    fn limit_pools(&mut self) -> Rc<[Pool]> {
        let environments = self.limit_environments();
        let revisions = environments.iter().map(|(id, _, info)| (id.clone(), info.revision)).collect();
        let key = (revisions, self.usage.hidden.clone(), self.usage.limits_now);
        if let Some((held, pools)) = &self.usage.pools
            && *held == key
        {
            return pools.clone();
        }
        let accounts = logic::limit_accounts(&limit_inputs(&environments), &self.usage.hidden_drivers());
        let pools: Rc<[Pool]> = Rc::from(logic::limit_pools(accounts, self.usage.limits_now));
        self.usage.pools = Some((key, pools.clone()));
        pools
    }

    fn render_limits(&mut self, t: &Tokens, viewport: f32, cx: &mut Context<Self>) -> AnyElement {
        let pools = self.limit_pools();
        let usage = &self.usage;
        let hidden = usage.hidden_drivers();
        let now = usage.limits_now;
        let environments = self.limit_environments();
        let inputs = limit_inputs(&environments);
        let notices = logic::limit_notices(&inputs, &hidden);
        let links = logic::external_links(&inputs, &hidden);
        let mut body = v_flex().gap(px(32.));
        if pools.is_empty() && notices.is_empty() && links.is_empty() {
            body = body
                .child(muted_line("No provider on the selected environments reports subscription limits.".into(), t));
        }
        let keys: Vec<String> = pools
            .iter()
            .flat_map(|pool| {
                pool.windows.iter().flat_map(move |window| {
                    window.columns.iter().flatten().map(move |m| segment_key(pool, window, &pool.accounts[m.account]))
                })
            })
            .collect();
        self.usage.segment_focus.retain(|key, _| keys.contains(key));
        for key in &keys {
            self.usage.segment_focus.entry(key.clone()).or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
        // The card's segment is gone, so it has nothing to anchor to.
        if self.usage.card.open.as_ref().is_some_and(|open| !keys.contains(open)) {
            self.usage.card.close();
        }
        // A card's heading stacks above its bar below `md`; the bar's own `@2xl/pool` (42rem) container
        // query, inside the card's border, `p-4` and heading column, picks the wide segments.
        let stack = viewport < MD;
        let pool_width = page_content_width(viewport) - 2. - 32. - if stack { 0. } else { 176. + 24. };
        let segments = Segments {
            stack,
            wide: pool_width >= 672.,
            now,
            hour12: self.hour12(),
            redeemed: &self.usage.redeemed,
            card: &self.usage.card,
            focus: &self.usage.segment_focus,
            view: cx.weak_entity(),
            t,
        };
        for pool in pools.iter() {
            body = body.child(pool_section(pool, &segments, cx));
        }
        for (label, url, message, accounts) in links {
            let open = url.clone();
            body = body.child(
                h_flex()
                    .flex_wrap()
                    .justify_between()
                    .gap(px(12.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(t.border)
                    .p(px(16.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(4.))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                            .child(div().text_xs().text_color(t.muted_foreground).child(accounts.join(", ")))
                            .children(
                                message
                                    .map(|m| div().max_w(px(576.)).text_xs().text_color(t.muted_foreground).child(m)),
                            ),
                    )
                    .child(
                        Button::new(ElementId::Name(format!("usage-manage-{url}").into()))
                            .ghost()
                            .xsmall()
                            .text_color(t.muted_foreground)
                            .label("Manage usage")
                            .icon(Icon::new(Lucide::ExternalLink).with_size(px(14.)))
                            .on_click(move |_, _, cx| cx.open_url(&open)),
                    ),
            );
        }
        if !notices.is_empty() {
            body = body.child(
                h_flex()
                    .items_start()
                    .gap(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(Hsla { a: 0.32, ..t.warning })
                    .bg(Hsla { a: 0.08, ..t.warning })
                    .px(px(14.))
                    .py(px(12.))
                    .text_sm()
                    .child(Icon::new(Lucide::TriangleAlert).with_size(px(16.)).text_color(t.warning_foreground))
                    .child(
                        v_flex()
                            .min_w_0()
                            .gap(px(2.))
                            .children(notices.into_iter().map(|n| div().font_weight(FontWeight::MEDIUM).child(n))),
                    ),
            );
        }
        body.into_any_element()
    }

    fn render_usage_overview(
        &self,
        summary: &Summary,
        window: &UsageWindow,
        t: &Tokens,
        win: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let usage = &self.usage;
        let merged = usage.merged.clone();
        let tokens = usage.tokens();
        let dim = |el: gpui_kit::Div| if usage.loading() || usage.refreshing { el.opacity(0.4) } else { el };
        let providers = logic::active_providers(&merged);
        let hourly = window.hours.is_some();
        let messages = summary.source_messages(&usage.hidden);

        let mut overview = v_flex().gap(px(6.)).children(messages.into_iter().map(|m| muted_line(m, t).mb(px(16.))));

        let mut summary_column = v_flex().min_w_0().gap(px(20.)).child(
            v_flex()
                .gap(px(4.))
                .child(
                    dim(div())
                        .text_size(gpui_kit::rems(2.25))
                        .line_height(px(40.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(if tokens {
                            logic::format_tokens(merged.total_tokens as f64)
                        } else {
                            logic::format_usd(merged.cost_usd)
                        }),
                )
                .child(
                    h_flex()
                        .gap(px(4.))
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(format!(
                            "{} sessions{}",
                            logic::format_count(merged.sessions as f64),
                            if tokens { "" } else { " · API estimate" }
                        ))
                        .when(!tokens && merged.unpriced_share > 0., |el| {
                            let text: SharedString = format!(
                                "API estimate excludes {} unpriced records.",
                                logic::format_percent(merged.unpriced_share)
                            )
                            .into();
                            el.child(
                                div()
                                    .id("usage-unpriced")
                                    .child(Icon::new(Lucide::Info).with_size(px(12.)))
                                    .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx)),
                            )
                        }),
                ),
        );
        let chatgpt_sharing = !usage.hidden.iter().any(|h| h == "codex")
            && self
                .limit_environments()
                .iter()
                .any(|(_, _, info)| info.providers.iter().any(|p| super::settings::uses_chatgpt_sharing(&p.auth)));
        if chatgpt_sharing {
            summary_column = summary_column.child(
                h_flex()
                    .justify_between()
                    .gap(px(8.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child("ChatGPT shared usage")
                    .child(super::settings::chatgpt_usage_button("usage-chatgpt-sharing", t).xsmall()),
            );
        }
        for kind in logic::rank_providers(&providers, &merged.providers, tokens) {
            let totals = merged.providers.iter().find(|p| p.provider == kind).cloned().unwrap_or_default();
            let share = if tokens { totals.token_share } else { totals.cost_share };
            let sessions = format!(
                "{} {}",
                logic::format_count(totals.sessions as f64),
                if totals.sessions == 1 { "session" } else { "sessions" }
            );
            summary_column = summary_column.child(
                v_flex()
                    .gap(px(4.))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(px(16.))
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap(px(8.))
                                    .text_sm()
                                    .child(div().size(px(8.)).flex_none().rounded_full().bg(provider_color(kind, t)))
                                    .children(provider_mark(kind, 16., t))
                                    .child(div().min_w_0().truncate().child(logic::provider_label(kind).to_owned()))
                                    .child(
                                        dim(div())
                                            .flex_none()
                                            .text_size(px(11.))
                                            .text_color(t.muted_foreground)
                                            .child(sessions),
                                    ),
                            )
                            .child(dim(div()).flex_none().text_sm().font_weight(FontWeight::MEDIUM).child(if tokens {
                                logic::format_tokens(totals.total_tokens as f64)
                            } else {
                                logic::format_usd(totals.cost_usd)
                            })),
                    )
                    .child(dim(div()).text_xs().text_color(t.muted_foreground).child(if tokens {
                        format!("{} of tokens · {}", logic::format_percent(share), logic::format_usd(totals.cost_usd))
                    } else {
                        format!(
                            "{} of cost · {} tokens",
                            logic::format_percent(share),
                            logic::format_tokens(totals.total_tokens as f64)
                        )
                    })),
            );
        }

        let chart_title =
            format!("{} {}", if hourly { "Hourly" } else { "Daily" }, if tokens { "processed tokens" } else { "cost" });
        let viewport = f32::from(win.viewport_size().width);
        let (lg, md) = (viewport >= LG, viewport >= MD);
        let chart = v_flex()
            .min_w_0()
            .gap(px(12.))
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(chart_title))
            .child(self.render_usage_chart(&merged, window, &providers, t, cx));
        overview = overview.child(summary_and_chart(lg, summary_column, chart));

        let metric = |label: &str, value: String| {
            v_flex()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_xs().text_color(t.muted_foreground).child(label.to_owned()))
                .child(dim(div()).text_base().font_weight(FontWeight::MEDIUM).child(value))
        };
        overview = overview.child(v_flex().gap(px(8.)).mt(px(18.)).child(section_title("Totals")).child(
            totals_grid(md).children([
                metric("Processed tokens", logic::format_tokens(merged.total_tokens as f64)),
                metric("Cached input", logic::format_tokens(merged.cached_input_tokens as f64)),
                metric("Uncached input", logic::format_tokens(merged.uncached_input_tokens as f64)),
                metric("Output", logic::format_tokens(merged.output_tokens as f64)),
                metric("Cache savings", logic::format_usd(merged.cache_savings_usd)),
            ]),
        ));

        if merged.total_tokens > 0 {
            let ink = |share: f32| mix(t.foreground, t.background, share);
            let bars = if tokens {
                vec![share_bar(
                    "Tokens by type",
                    vec![
                        ("Input", merged.uncached_input_tokens as f64, ink(0.6)),
                        ("Cache read", merged.cached_input_tokens as f64, ink(0.3)),
                        ("Cache write", merged.cache_creation_tokens as f64, ink(0.72)),
                        ("Output", merged.output_tokens as f64, ink(1.)),
                    ],
                    true,
                    None,
                    t,
                )]
            } else {
                let [input, read, write, output, other] = merged.category;
                let mut bars = vec![share_bar(
                    "Cost by type",
                    vec![
                        ("Input", input, ink(0.6)),
                        ("Cache read", read, ink(0.3)),
                        ("Cache write", write, ink(0.72)),
                        ("Output", output, ink(1.)),
                        ("Other", if other >= 0.005 { other } else { 0. }, ink(0.44)),
                    ],
                    false,
                    None,
                    t,
                )];
                let speed = merged.speed;
                if speed.fast + speed.ultrafast > 0. {
                    bars.push(share_bar(
                        "Cost by speed",
                        vec![
                            ("Standard", speed.standard, ink(0.34)),
                            ("Fast", speed.fast, ink(0.66)),
                            ("Ultrafast", speed.ultrafast, ink(1.)),
                        ],
                        false,
                        Some(logic::format_usd(speed.premium)),
                        t,
                    ));
                }
                bars
            };
            overview = overview.child(
                share_bars_grid(lg).mt(px(18.)).children(bars.into_iter().map(|bar| div().min_w_0().child(bar))),
            );
        }

        overview
            .child(self.render_usage_breakdown(&merged, window, &providers, t, win, cx).mt(px(18.)))
            .into_any_element()
    }

    fn render_usage_chart(
        &self,
        merged: &Rc<Merged>,
        window: &UsageWindow,
        providers: &[&'static str],
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tokens = self.usage.tokens();
        let tz = Local;
        let hour12 = self.hour12();
        let data = {
            let mut held = self.usage.chart.borrow_mut();
            let fresh = |d: &&Rc<ChartData>| {
                Rc::ptr_eq(&d.key.0, merged) && d.key.1 == *window && d.key.2 == providers && d.key.3 == tokens
            };
            match held.as_ref().filter(fresh) {
                Some(data) => data.clone(),
                None => held.insert(Rc::new(ChartData::new(merged, window, providers, tokens))).clone(),
            }
        };
        let (periods, columns, ticks) = (&data.periods, &data.columns, &data.ticks);
        let (peak, max, step_x) = (data.peak, data.max, data.step_x);
        let colors: Vec<Hsla> = providers.iter().map(|kind| provider_color(kind, t)).collect();
        let series = data.series.clone();
        let format = move |value: f64| if tokens { logic::format_tokens(value) } else { logic::format_usd(value) };
        let grid = t.border;
        let hover = self.usage.hover;
        let hover_x = hover.map(|(index, ..)| index as f32 * step_x);
        let muted = t.muted_foreground;
        let bounds_cell: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::default();
        let period_count = periods.len();

        let plot_ticks = ticks.clone();
        let plot = canvas(
            {
                let cell = bounds_cell.clone();
                move |bounds, _, _| cell.set(Some(bounds))
            },
            move |bounds, (), window, _| {
                let at = |x: f32, y: f32| {
                    point(
                        bounds.origin.x + bounds.size.width * (x / logic::VIEW_WIDTH),
                        bounds.origin.y + bounds.size.height * (y / logic::VIEW_HEIGHT),
                    )
                };
                for tick in &plot_ticks {
                    let y = logic::value_to_y(*tick, max);
                    let mut line = PathBuilder::stroke(px(1.));
                    line.move_to(at(0., y));
                    line.line_to(at(logic::VIEW_WIDTH, y));
                    if let Ok(path) = line.build() {
                        window.paint_path(path, grid);
                    }
                }
                let curve = |builder: &mut PathBuilder, segments: &[logic::Segment]| {
                    if let Some(first) = segments.first() {
                        builder.move_to(at(first[0].0, first[0].1));
                    }
                    for [_, c1, c2, to] in segments {
                        builder.cubic_bezier_to(at(to.0, to.1), at(c1.0, c1.1), at(c2.0, c2.1));
                    }
                };
                for (slot, segments, _) in series.iter() {
                    let color = &colors[*slot];
                    let mut area = PathBuilder::fill();
                    curve(&mut area, segments);
                    if !segments.is_empty() {
                        area.line_to(at(logic::VIEW_WIDTH, logic::VIEW_HEIGHT));
                        area.line_to(at(0., logic::VIEW_HEIGHT));
                        area.close();
                    }
                    if let Ok(path) = area.build() {
                        window.paint_path(path, Hsla { a: color.a * 0.12, ..*color });
                    }
                }
                for (slot, segments, _) in series.iter() {
                    let color = &colors[*slot];
                    let mut line = PathBuilder::stroke(px(2.));
                    curve(&mut line, segments);
                    if let Ok(path) = line.build() {
                        window.paint_path(path, *color);
                    }
                }
                if let Some(x) = hover_x {
                    let mut line = PathBuilder::stroke(px(1.));
                    line.move_to(at(x, logic::PLOT_TOP));
                    line.line_to(at(x, logic::VIEW_HEIGHT));
                    if let Ok(path) = line.build() {
                        window.paint_path(path, muted);
                    }
                }
            },
        )
        .size_full();

        let tooltip = hover.and_then(|(index, x, y)| {
            let key = periods.get(index)?;
            let label = match (key, window.hours) {
                (PeriodKey::Hour(ms), Some((_, until))) => logic::format_relative_hour_short(*ms, until, &tz, hour12),
                _ => key.label(&tz, hour12),
            };
            let column = columns.get(index)?;
            let total: f64 = column.iter().sum();
            let bounds = bounds_cell.get();
            let width = bounds.map_or(px(600.), |b| b.size.width);
            // Beside the pointer, flipped and clamped to stay inside the plot (`positionTooltip`).
            let (gap, box_width, plot_height) = (px(12.), px(160.), px(224.));
            let box_height = px(66. + 18. * column.iter().filter(|v| **v > 0.).count() as f32);
            let left = if x + gap + box_width <= width { x + gap } else { x - gap - box_width };
            let top = if y + gap + box_height <= plot_height { y + gap } else { y - gap - box_height };
            Some(
                v_flex()
                    .absolute()
                    .left(left.min(width - box_width).max(px(0.)))
                    .top(top.min(plot_height - box_height).max(px(0.)))
                    .min_w(px(144.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(Hsla { a: t.border.a * 0.5, ..t.border })
                    .bg(t.popover)
                    .shadow_lg()
                    .px(px(10.))
                    .py(px(8.))
                    .text_xs()
                    .child(div().mb(px(4.)).text_color(t.muted_foreground).child(label))
                    .children(providers.iter().zip(column).filter(|(_, v)| **v > 0.).map(|(kind, value)| {
                        h_flex()
                            .justify_between()
                            .gap(px(12.))
                            .child(
                                h_flex()
                                    .gap(px(6.))
                                    .text_color(t.muted_foreground)
                                    .child(div().size(px(8.)).rounded_full().bg(provider_color(kind, t)))
                                    .child(logic::provider_label(kind).to_owned()),
                            )
                            .child(format(*value))
                    }))
                    .child(
                        h_flex()
                            .mt(px(4.))
                            .pt(px(4.))
                            .border_t_1()
                            .border_color(t.border)
                            .justify_between()
                            .gap(px(12.))
                            .child(div().text_color(t.muted_foreground).child("Total"))
                            .child(format(total)),
                    ),
            )
        });

        let axis = div().relative().w(px(56.)).h(px(224.)).flex_none().children(if peak > 0. {
            ticks
                .iter()
                .map(|tick| {
                    let top = logic::value_to_y(*tick, max) / logic::VIEW_HEIGHT;
                    div()
                        .absolute()
                        .right_0()
                        .top(relative(top))
                        .mt(px(-7.))
                        .text_size(px(10.))
                        .text_color(t.muted_foreground)
                        .child(if *tick == 0. { "0".to_owned() } else { format(*tick) })
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        });
        let label_at =
            |index: usize| periods.get(index).map(|key| key.label(&tz, hour12).to_uppercase()).unwrap_or_default();
        v_flex()
            .gap(px(4.))
            .child(
                h_flex().gap(px(8.)).child(axis).child(
                    div()
                        .id("usage-chart")
                        .relative()
                        .flex_1()
                        .h(px(224.))
                        .child(plot)
                        .children(tooltip)
                        .on_mouse_move({
                            let cell = bounds_cell;
                            cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                                let Some(bounds) = cell.get() else { return };
                                if period_count == 0 || !bounds.contains(&event.position) {
                                    return;
                                }
                                let local = event.position - bounds.origin;
                                let fraction = (local.x / bounds.size.width).clamp(0., 1.);
                                #[expect(
                                    clippy::cast_possible_truncation,
                                    clippy::cast_sign_loss,
                                    reason = "an index into the periods: fraction is 0..=1"
                                )]
                                let index = (fraction * (period_count - 1) as f32).round() as usize;
                                let next = Some((index, local.x, local.y));
                                if this.usage.hover != next {
                                    this.usage.hover = next;
                                    cx.notify();
                                }
                            })
                        })
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if !hovered && this.usage.hover.take().is_some() {
                                cx.notify();
                            }
                        })),
                ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .pl(px(64.))
                    .text_size(px(10.))
                    .text_color(t.muted_foreground)
                    .child(label_at(0))
                    .child(label_at(periods.len() / 2))
                    .child(label_at(periods.len().saturating_sub(1))),
            )
    }

    fn render_usage_breakdown(
        &self,
        merged: &Rc<Merged>,
        window: &UsageWindow,
        providers: &[&'static str],
        t: &Tokens,
        win: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let usage = &self.usage;
        let tokens = usage.tokens();
        let hour12 = self.hour12();
        let hourly = window.hours.is_some();
        let by_time = usage.by_time;
        let toggle = segmented(
            "usage-breakdown",
            [(false, "Model"), (true, if hourly { "Hour" } else { "Day" })].into_iter(),
            by_time,
            t,
            win,
            cx,
            |this, by_time, cx| {
                this.usage.by_time = by_time;
                cx.notify();
            },
        );
        let header = h_flex().justify_between().gap(px(12.)).child(section_title("Breakdown")).child(toggle);
        let head_cell = |label: String| div().py(px(8.)).text_xs().text_color(t.muted_foreground).child(label);
        let empty = || {
            div()
                .py(px(24.))
                .flex()
                .justify_center()
                .text_sm()
                .text_color(t.muted_foreground)
                .child("No activity in this window.")
        };
        let row_border = Hsla { a: t.border.a * 0.5, ..t.border };
        let mut table = v_flex().text_sm();
        if by_time {
            let periods: Vec<&logic::Period> =
                if hourly { merged.hourly.iter().rev().collect() } else { merged.daily.iter().rev().collect() };
            let cell = |child: String| div().flex_1().flex().justify_end().pl(px(16.)).child(child);
            table = table.child(
                h_flex()
                    .border_b_1()
                    .border_color(t.border)
                    .child(head_cell(if hourly { "Hour" } else { "Day" }.into()).w(relative(0.4)).pl(px(8.)))
                    .children(providers.iter().map(|p| {
                        cell(logic::provider_label(p).to_owned()).py(px(8.)).text_xs().text_color(t.muted_foreground)
                    }))
                    .child(cell("Total".into()).py(px(8.)).text_xs().text_color(t.muted_foreground))
                    .child(cell("Tokens".into()).pr(px(8.)).py(px(8.)).text_xs().text_color(t.muted_foreground)),
            );
            if periods.is_empty() {
                table = table.child(empty());
            }
            for period in periods {
                let label = match period.hour_ms {
                    Some(ms) => logic::format_hour_short(ms, &Local, hour12),
                    None => logic::format_day_short(&period.day),
                };
                table =
                    table.child(
                        h_flex()
                            .id(ElementId::Name(
                                format!("usage-period-{}-{}", period.day, period.hour_ms.unwrap_or(0)).into(),
                            ))
                            .py(px(8.))
                            .border_b_1()
                            .border_color(row_border)
                            .hover(|s| s.bg(t.muted.opacity(0.5)))
                            .child(div().w(relative(0.4)).pl(px(8.)).text_color(t.foreground).child(label))
                            .children(providers.iter().map(|p| {
                                cell(logic::format_usd(period.value(p, false))).text_color(t.muted_foreground)
                            }))
                            .child(cell(logic::format_usd(period.cost_usd)).text_color(t.foreground))
                            .child(
                                cell(logic::format_tokens(period.total_tokens as f64))
                                    .pr(px(8.))
                                    .text_color(t.muted_foreground),
                            ),
                    );
            }
        } else {
            let models = if tokens { logic::models_by_tokens(&merged.models) } else { merged.models.iter().collect() };
            let peak =
                models.iter().fold(0f64, |max, m| max.max(if tokens { m.total_tokens as f64 } else { m.cost_usd }));
            table = table.child(
                h_flex()
                    .border_b_1()
                    .border_color(t.border)
                    .child(head_cell("#".into()).w(px(32.)).pl(px(8.)))
                    .child(head_cell("Model".into()).flex_1())
                    .child(head_cell("Cost".into()).w(px(104.)).flex().justify_end())
                    .child(head_cell("Share".into()).w(px(88.)).flex().justify_end())
                    .child(head_cell("Tokens".into()).w(px(96.)).pr(px(8.)).flex().justify_end()),
            );
            if models.is_empty() {
                table = table.child(empty());
            }
            for (index, model) in models.iter().enumerate() {
                let value = if tokens { model.total_tokens as f64 } else { model.cost_usd };
                #[expect(clippy::cast_possible_truncation, reason = "a 0..=1 share; f32 is what GPUI lays out with")]
                let bar = (value > 0. && peak > 0.).then(|| (value / peak) as f32);
                let share = model.share(tokens).map(logic::format_percent).unwrap_or_default();
                table = table.child(
                    h_flex()
                        .id(ElementId::Name(format!("usage-model-{}-{}", model.provider, model.model).into()))
                        .border_b_1()
                        .border_color(row_border)
                        .text_color(t.muted_foreground)
                        .hover(|s| s.bg(t.muted.opacity(0.5)))
                        .child(div().w(px(32.)).pl(px(8.)).py(px(10.)).text_xs().child((index + 1).to_string()))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .py(px(10.))
                                .child(
                                    h_flex()
                                        .gap(px(8.))
                                        .text_color(t.foreground)
                                        .when(bar.is_none(), |el| el.py(px(4.)))
                                        .children(provider_mark(&model.provider, 14., t))
                                        .child(model.model.clone()),
                                )
                                .children(bar.map(|share| {
                                    div().mt(px(6.)).h(px(2.)).max_w(px(192.)).w_full().child(
                                        div()
                                            .h_full()
                                            .min_w(px(8.))
                                            .w(relative(share))
                                            .rounded_full()
                                            .bg(provider_color(&model.provider, t)),
                                    )
                                })),
                        )
                        .child(div().w(px(104.)).flex().justify_end().text_color(t.foreground).child(
                            if model.cost_unknown() {
                                div().text_color(t.muted_foreground).child("Unpriced")
                            } else {
                                div().child(logic::format_usd(model.cost_usd))
                            },
                        ))
                        .child(div().w(px(88.)).flex().justify_end().child(share))
                        .child(
                            div()
                                .w(px(96.))
                                .pr(px(8.))
                                .flex()
                                .justify_end()
                                .child(logic::format_tokens(model.total_tokens as f64)),
                        ),
                );
            }
        }
        v_flex().gap(px(12.)).child(header).child(table)
    }
}

fn limit_inputs<'a>(
    environments: &'a [(SharedString, SharedString, &'a crate::model::ServerInfo)],
) -> Vec<logic::LimitEnvironment<'a>> {
    environments
        .iter()
        .map(|(id, label, info)| logic::LimitEnvironment {
            id,
            label,
            providers: &info.providers,
            sources: &info.config["usageLimitSources"],
        })
        .collect()
}

/// `UsageEnvironmentFilter`'s pick: the selection with `id` toggled, `None` once every environment is in it.
fn toggle_environment(selected: &[SharedString], id: &SharedString, all: &[SharedString]) -> Option<Vec<SharedString>> {
    let mut next: Vec<SharedString> = selected.iter().filter(|picked| *picked != id).cloned().collect();
    if next.len() == selected.len() {
        next.push(id.clone());
    }
    (next.len() != all.len()).then_some(next)
}

fn usage_error(error: &crate::rpc::RpcError) -> String {
    match error {
        crate::rpc::RpcError::Failure(value) => {
            let detail = value.get("detail").and_then(Value::as_str);
            let reason = value.get("reason").and_then(Value::as_str);
            match (value.get("_tag").and_then(Value::as_str), reason, detail) {
                (Some("UsageReadError"), Some(reason), Some(detail)) => {
                    format!("Usage read failed ({reason}): {detail}")
                }
                _ => error.to_string(),
            }
        }
        _ => error.to_string(),
    }
}

/// Tailwind's `md` and `lg` viewport breakpoints.
const MD: f32 = 768.;
const LG: f32 = 1024.;

/// `AppRouteModal`'s `sm:p-6 lg:p-10` around its panel.
fn modal_padding(viewport: f32) -> f32 {
    if viewport >= LG {
        40.
    } else if viewport >= 640. {
        24.
    } else {
        0.
    }
}

/// The panel's `sm:max-w-[80rem]` border box.
fn modal_panel_width(viewport: f32) -> f32 {
    (viewport - 2. * modal_padding(viewport)).min(1280.)
}

/// `WorkspacePageContainer`'s `px-5 sm:px-6`.
fn page_gutter(viewport: f32) -> f32 {
    if viewport >= 640. { 24. } else { 20. }
}

/// The width inside `WorkspacePageContainer width="wide"` (`max-w-5xl`), within the panel's border.
fn page_content_width(viewport: f32) -> f32 {
    (modal_panel_width(viewport) - 2.).min(1024.) - 2. * page_gutter(viewport)
}

/// `grid gap-6 lg:grid-cols-[minmax(0,18rem)_minmax(0,1fr)]`: the summary beside the chart, above it below `lg`.
fn summary_and_chart(lg: bool, summary: gpui_kit::Div, chart: gpui_kit::Div) -> gpui_kit::Div {
    if lg {
        h_flex().items_start().gap(px(24.)).child(summary.w(px(288.)).flex_none()).child(chart.flex_1())
    } else {
        v_flex().gap(px(24.)).child(summary).child(chart)
    }
}

/// `grid grid-cols-2 gap-x-6 gap-y-4 py-1 md:grid-cols-5`.
fn totals_grid(md: bool) -> gpui_kit::Div {
    div().grid().grid_cols(if md { 5 } else { 2 }).gap_x(px(24.)).gap_y(px(16.)).py(px(4.))
}

/// `grid gap-x-12 gap-y-8 lg:grid-cols-2`.
fn share_bars_grid(lg: bool) -> gpui_kit::Div {
    div().grid().grid_cols(if lg { 2 } else { 1 }).gap_x(px(48.)).gap_y(px(32.))
}

fn section_title(title: &str) -> impl IntoElement {
    div().text_sm().font_weight(FontWeight::MEDIUM).child(title.to_owned())
}

fn muted_line(text: String, t: &Tokens) -> gpui_kit::Div {
    div().text_sm().text_color(t.muted_foreground).child(text)
}

/// `color-mix(in srgb, a share, b)`.
fn mix(a: Hsla, b: Hsla, share: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let lerp = |x: f32, y: f32| x * share + y * (1. - share);
    gpui_kit::Rgba { r: lerp(a.r, b.r), g: lerp(a.g, b.g), b: lerp(a.b, b.b), a: 1. }.into()
}

/// `PROVIDER_PRESENTATION[kind].color`: Codex follows the foreground, Grok a softer foreground.
fn provider_color(kind: &str, t: &Tokens) -> Hsla {
    match logic::PROVIDERS.iter().find(|p| p.0 == kind).and_then(|p| p.3) {
        Some(hex) => rgb(hex).into(),
        None if kind == "grok" => mix(t.foreground, t.background, 0.72),
        None => t.foreground,
    }
}

fn provider_mark(kind: &str, size: f32, t: &Tokens) -> Option<impl IntoElement> {
    let (path, tint) = crate::assets::provider_icon(logic::provider_driver(kind))?;
    Some(svg().path(path).size(px(size)).flex_none().text_color(tint.unwrap_or(t.foreground)))
}

/// The driver's own name (`providerClients`), which heads its limits.
fn driver_label(driver: &str) -> String {
    match driver {
        "codex" => "Codex",
        "claudeAgent" => "Claude",
        "cursor" => "Cursor",
        "grok" => "Grok",
        "opencode" => "OpenCode",
        "antigravity" => "Antigravity",
        "kiro" => "Kiro",
        "muse" => "Muse Code",
        "pi" => "Pi",
        other => other,
    }
    .to_owned()
}

/// A breadcrumb menu trigger that reads as text (`InlineButton`).
fn inline_menu_button(id: &'static str, t: &Tokens) -> Button {
    // `small`: the button sizes its label's text itself, and `xsmall` would make it 12px.
    Button::new(id)
        .ghost()
        .small()
        .h(px(28.))
        .px(px(0.))
        .gap(px(4.))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.muted_foreground)
}

/// `Select` with a compact ghost `SelectTrigger`: the picked option's label over a menu of the options.
fn compact_select<V: Copy + PartialEq + 'static>(
    id: &'static str,
    options: impl Iterator<Item = (V, &'static str)>,
    current: V,
    t: &Tokens,
    view: &gpui_kit::Entity<Workspace>,
    pick: fn(&mut Workspace, V, &mut Context<Workspace>),
) -> impl IntoElement {
    let options: Vec<(V, &'static str)> = options.collect();
    let label = options.iter().find(|(value, _)| *value == current).map_or("", |(_, label)| *label);
    let view = view.clone();
    Button::new(id)
        .ghost()
        .small()
        .h(px(28.))
        .px(px(7.))
        .gap(px(4.))
        .rounded(px(6.))
        .text_xs()
        .text_color(t.sidebar.secondary_label)
        .child(label)
        .child(
            Icon::new(Lucide::ChevronDown).with_size(px(14.)).text_color(Hsla { a: 0.7, ..t.sidebar.secondary_label }),
        )
        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, window, _| {
            options.iter().fold(menu, |menu, (value, label)| {
                let value = *value;
                menu.item(
                    PopupMenuItem::new(*label)
                        .checked(value == current)
                        .on_click(window.listener_for(&view, move |this, _, _, cx| pick(this, value, cx))),
                )
            })
        })
}

fn ghost_icon_button(id: &'static str, icon: Lucide, size: f32, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .size(px(28.))
        .p_0()
        .rounded(px(8.))
        .child(Icon::new(icon).with_size(px(size)).text_color(t.muted_foreground))
}

/// A segmented group's toggles' focus handles, kept while the group renders.
#[derive(Default)]
struct SegmentFocus(Vec<FocusHandle>);

/// `ToggleGroup`'s arrow keys: the toggle `key` moves focus to from `from` of `len`, wrapping.
fn segment_step(key: &str, from: usize, len: usize) -> Option<usize> {
    match key {
        "left" => Some((from + len - 1) % len),
        "right" => Some((from + 1) % len),
        "home" => Some(0),
        "end" => Some(len - 1),
        _ => None,
    }
}

/// `ToggleGroup variant="segmented"`: one tab stop (the pressed toggle until the arrow keys move
/// on), arrows and Home/End move between toggles, and Enter or Space presses the focused one.
pub(super) fn segmented<V: Copy + PartialEq + std::fmt::Debug + 'static>(
    id: &'static str,
    options: impl Iterator<Item = (V, &'static str)>,
    current: V,
    t: &Tokens,
    window: &mut Window,
    cx: &mut Context<Workspace>,
    pick: fn(&mut Workspace, V, &mut Context<Workspace>),
) -> gpui_kit::Div {
    let options: Vec<(V, &'static str)> = options.collect();
    let state = window.use_keyed_state(SharedString::from(format!("{id}-focus")), cx, |_, _| SegmentFocus::default());
    let handles = state.update(cx, |state, cx| {
        state.0.resize_with(options.len(), || cx.focus_handle());
        state.0.clone()
    });
    let focused = handles.iter().position(|handle| handle.is_focused(window));
    let stop = focused.or_else(|| options.iter().position(|(value, _)| *value == current)).unwrap_or(0);
    let keyboard = window.last_input_was_keyboard();
    let track = Hsla { a: t.input.a * 0.4, ..t.input };
    let len = options.len();
    h_flex().flex_none().gap(px(2.)).p(px(2.)).rounded(px(8.)).bg(track).children(options.into_iter().enumerate().map(
        |(index, (value, label))| {
            let pressed = value == current;
            let focus = handles[index].clone().tab_stop(index == stop);
            // `focus-visible:ring-2 focus-visible:ring-inset ring-ring`
            let ring = (keyboard && focus.is_focused(window))
                .then(|| vec![BoxShadow::new(px(0.), px(0.), t.ring).spread_radius(px(2.)).inset()]);
            let handles = handles.clone();
            div()
                .id(ElementId::Name(format!("{id}-{value:?}").into()))
                .track_focus(&focus)
                .when_some(ring, gpui_kit::Styled::shadow)
                .h(px(24.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(6.))
                // `text-xs` with the base's `sm:text-sm`, which wins at desktop widths.
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .map(|el| {
                    if pressed {
                        el.text_color(t.foreground).bg(if t.dark {
                            Hsla { a: t.input.a * 0.72, ..t.input }
                        } else {
                            t.background
                        })
                    } else {
                        el.text_color(t.muted_foreground)
                            .hover(|s| s.text_color(t.foreground).bg(Hsla { a: t.input.a * 0.32, ..t.input }))
                    }
                })
                .child(label)
                .on_key_down(cx.listener(move |_, event: &KeyDownEvent, window, cx| {
                    let keystroke = &event.keystroke;
                    if keystroke.modifiers.modified() {
                        return;
                    }
                    if let Some(next) = segment_step(&keystroke.key, index, len) {
                        window.focus(&handles[next], cx);
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                // A pressed button takes focus in a browser, so the arrow keys go on from it.
                .on_click(cx.listener(move |this, _, window, cx| {
                    window.focus(&focus, cx);
                    pick(this, value, cx);
                }))
        },
    ))
}

/// `UsageShareBar`: one bar split by share, with a legend.
fn share_bar(
    label: &'static str,
    segments: Vec<(&'static str, f64, Hsla)>,
    tokens: bool,
    premium: Option<String>,
    t: &Tokens,
) -> AnyElement {
    let visible: Vec<_> = segments.into_iter().filter(|s| s.1 > 0.).collect();
    let total: f64 = visible.iter().map(|s| s.1).sum();
    if total <= 0. {
        return div().into_any_element();
    }
    let format = |v: f64| if tokens { logic::format_tokens(v) } else { logic::format_usd(v) };
    let count = visible.len();
    v_flex()
        .min_w_0()
        .gap(px(10.))
        .child(
            h_flex()
                .justify_between()
                .gap(px(12.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                .children(premium.map(|premium| {
                    h_flex()
                        .gap(px(4.))
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child("Premium")
                        .child(div().text_color(t.foreground).child(premium))
                })),
        )
        .child(h_flex().h(px(8.)).gap(px(2.)).children(visible.iter().enumerate().map(
            |(index, (name, value, color))| {
                let text: SharedString =
                    format!("{name} · {} · {}", format(*value), logic::format_percent(value / total)).into();
                let radius = |first: bool| if first { px(999.) } else { px(2.) };
                #[expect(clippy::cast_possible_truncation, reason = "a 0..=1 share; f32 is what GPUI lays out with")]
                let share = (value / total) as f32;
                div()
                    .id(ElementId::Name(format!("share-{label}-{name}").into()))
                    .h_full()
                    .min_w(px(4.))
                    .flex_basis(relative(share))
                    .flex_shrink(1.)
                    .bg(*color)
                    .rounded_tl(radius(index == 0))
                    .rounded_bl(radius(index == 0))
                    .rounded_tr(radius(index + 1 == count))
                    .rounded_br(radius(index + 1 == count))
                    .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
            },
        )))
        .child(h_flex().flex_wrap().gap_x(px(16.)).gap_y(px(4.)).text_xs().children(visible.iter().map(
            |(name, value, color)| {
                h_flex()
                    .gap(px(6.))
                    .child(div().size(px(8.)).rounded(px(2.)).bg(*color))
                    .child(div().text_color(t.muted_foreground).child(*name))
                    .child(format(*value))
            },
        )))
        .into_any_element()
}

/// What every limit segment reads: the layout, the clock, redeem progress, the shared card and focus handles.
struct Segments<'a> {
    stack: bool,
    wide: bool,
    now: i64,
    hour12: bool,
    redeemed: &'a [Redeem],
    card: &'a SegmentCard,
    focus: &'a HashMap<String, FocusHandle>,
    view: WeakEntity<Workspace>,
    t: &'a Tokens,
}

/// A segment's key: its account on one pooled window.
fn segment_key(pool: &Pool, window: &PoolWindow, account: &logic::Account) -> String {
    format!("{}-{}-{}-{}", pool.driver, window.kind, window.id, account.key)
}

/// `PoolSection`: one driver's windows, each pooled across its accounts.
fn pool_section(pool: &Pool, segments: &Segments<'_>, cx: &App) -> AnyElement {
    let t = segments.t;
    let color = logic::driver_kind(&pool.driver).map_or(t.foreground, |kind| provider_color(kind, t));
    v_flex()
        .gap(px(12.))
        .child(
            h_flex()
                .gap(px(8.))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .children(crate::assets::provider_icon(&pool.driver).map(|(path, tint)| {
                    div().size(px(20.)).flex().items_center().justify_center().child(
                        svg().path(path).size(px(16.)).text_color(tint.unwrap_or(Hsla { a: 0.8, ..t.foreground })),
                    )
                }))
                .child(driver_label(&pool.driver)),
        )
        .children(pool.windows.iter().map(|window| pool_window_card(pool, window, color, segments, cx)))
        // The confirm closed the card, so progress and the outcome show here.
        .children(segments.redeemed.iter().filter(|r| pool.accounts.iter().any(|a| a.key == r.key)).filter_map(|r| {
            let status = match &r.status {
                RedeemStatus::Confirming => return None,
                RedeemStatus::Running => "Using reset credit…".to_owned(),
                RedeemStatus::Done(status) => status.clone(),
            };
            Some(
                h_flex()
                    .gap(px(4.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(div().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(r.name.clone()))
                    .child(status),
            )
        }))
        .into_any_element()
}

fn pool_window_card(pool: &Pool, window: &PoolWindow, color: Hsla, segments: &Segments<'_>, cx: &App) -> AnyElement {
    let t = segments.t;
    let cursor = (pool.driver == "cursor").then(|| logic::CURSOR_WINDOWS.iter().find(|w| w.0 == window.id)).flatten();
    let label = cursor.map_or(window.label.clone(), |w| w.1.to_owned());
    let several = window.columns.len() > 1;
    let bars = window.columns.iter().enumerate().map(|(position, member)| {
        let Some(member) = member else { return div().flex_1().min_w_0().into_any_element() };
        pool_segment(pool, window, member, position + 1, color, segments, cx)
    });
    let bar = h_flex().min_w_0().gap(px(4.)).children(bars);
    // Below `@2xl/pool` the segments show numbers, and a legend row per account carries their details.
    let bar = if segments.wide {
        bar
    } else {
        v_flex().min_w_0().gap(px(4.)).child(bar).children(window.columns.iter().enumerate().filter_map(
            |(position, member)| {
                member.as_ref().map(|member| legend_row(pool, window, member, position + 1, color, segments))
            },
        ))
    };
    // Below `md` the heading sits above the bar.
    let row = if segments.stack { v_flex().gap(px(12.)) } else { h_flex().gap(px(24.)) };
    v_flex()
        .gap(px(12.))
        .rounded(px(8.))
        .border_1()
        .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
        .p(px(16.))
        .child(
            row.child(
                v_flex()
                    .when(!segments.stack, |el| el.w(px(176.)).flex_none())
                    .gap(px(4.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label))
                    .child(
                        h_flex()
                            .items_baseline()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_3xl()
                                    .line_height(px(36.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{}%", window.remaining_percent)),
                            )
                            // One baseline item, as the web's `inline-flex items-center` span: the
                            // pace icon centers on "left" instead of hanging below the line.
                            .child(
                                h_flex().gap(px(8.)).text_sm().text_color(t.muted_foreground).child("left").children(
                                    window
                                        .pace
                                        .map(|pace| pace_icon(format!("{}-{}", pool.driver, window.id), pace, t)),
                                ),
                            ),
                    )
                    .when_some(window.next_refill.filter(|_| several), |el, refill| {
                        el.child(div().text_xs().font_weight(FontWeight::MEDIUM).child(format!("↻ +{refill}%")))
                    }),
            )
            .child(if segments.stack { bar } else { bar.flex_1() }),
        )
        .children(cursor.map(|w| div().text_xs().text_color(t.muted_foreground).child(w.2)))
        .into_any_element()
}

fn pace_icon(id: String, pace: logic::Pace, t: &Tokens) -> impl IntoElement {
    let icon = match pace {
        logic::Pace::Ahead => Lucide::TrendingUp,
        logic::Pace::On => Lucide::Gauge,
        logic::Pace::Under => Lucide::TrendingDown,
    };
    div()
        .id(ElementId::Name(format!("pace-{id}").into()))
        .flex()
        .child(Icon::new(icon).with_size(px(14.)).text_color(t.muted_foreground))
        .tooltip(move |window, cx| Tooltip::new(pace.label()).build(window, cx))
}

/// `oklch(l c h)` in sRGB, for the account chips' hashed hues.
fn oklch(l: f32, c: f32, hue: f32) -> Hsla {
    let (a, b) = (c * hue.to_radians().cos(), c * hue.to_radians().sin());
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let linear = [
        4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_,
        -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_,
    ];
    let gamma = |v: f32| {
        let v = v.clamp(0., 1.);
        if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1. / 2.4) - 0.055 }
    };
    gpui_kit::Rgba { r: gamma(linear[0]), g: gamma(linear[1]), b: gamma(linear[2]), a: 1. }.into()
}

/// `AccountChip`: the email's initials on a hue hashed from it.
fn account_chip(email: &str) -> impl IntoElement {
    let (local, domain) = email.split_once('@').unwrap_or((email, ""));
    let initials: String = local.chars().take(1).chain(domain.chars().take(1)).collect::<String>().to_uppercase();
    let hash = email.chars().fold(0i32, |hash, c| hash.wrapping_mul(31).wrapping_add(c as i32));
    let hue = (hash.unsigned_abs() % 360) as f32;
    div()
        .size(px(16.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(oklch(0.85, 0.08, hue))
        .text_color(oklch(0.35, 0.1, hue))
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(if initials.is_empty() { "?".to_owned() } else { initials })
}

/// `PoolSegment`: one account's bar, a button whose shared card (`SegmentPopover`) opens on
/// hover, click, Enter or Space.
fn pool_segment(
    pool: &Pool,
    window: &PoolWindow,
    member: &logic::Member,
    index: usize,
    color: Hsla,
    segments: &Segments<'_>,
    cx: &App,
) -> AnyElement {
    let (t, now) = (segments.t, segments.now);
    let account = &pool.accounts[member.account];
    let key = segment_key(pool, window, account);
    let several = window.columns.len() > 1;
    let credits = account.limits.reset_credits.clone().filter(|c| c.available_count > 0);
    let color = account_color(account, color);
    let remaining = logic::remaining_percent(&member.window);
    let time_left = logic::pace_remaining_percent(&member.window, now);
    let resets_in = logic::format_resets_in(&member.window, now);
    let name = account_name(account);
    let open = segments.card.open.as_deref() == Some(key.as_str());
    let fill = Hsla { a: color.a * 0.35, ..color };
    // What the next reset gives back, hatched at the bar's spent end.
    #[expect(clippy::cast_possible_truncation, reason = "a percentage")]
    let refill = (100. - f64::from(remaining)).min(member.refill_percent) as f32;
    let hatched = (remaining < 100 && logic::reset_ms(&member.window).is_some() && refill > 0.)
        .then(|| hatch(Hsla { a: color.a * 0.2, ..color }, refill / 100.));
    let ring = t.ring;
    let view = segments.view.clone();
    let mut segment = div()
        .id(ElementId::Name(format!("pool-{key}").into()))
        .relative()
        .w_full()
        .h(px(if segments.wide { 32. } else { 20. }))
        .rounded(px(6.))
        .bg(t.muted)
        .overflow_hidden()
        .cursor_pointer()
        // `focus-visible:ring-2 ring-inset ring-ring`, and `ring-1 ring-border` while its card is open.
        .focus_visible(move |s| s.shadow(vec![BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.)).inset()]))
        .when(open, |el| el.shadow(vec![BoxShadow::new(px(0.), px(0.), t.border).spread_radius(px(1.)).inset()]))
        .child(
            div().absolute().top_0().bottom_0().left_0().w(relative(remaining as f32 / 100.)).rounded(px(6.)).bg(fill),
        )
        .children(hatched)
        .children(time_left.map(|left| {
            div()
                .absolute()
                .top(px(2.))
                .bottom(px(2.))
                .left(relative((left as f32 / 100.).clamp(0.01, 0.99)))
                .ml(px(-1.))
                .w(px(2.))
                .rounded_full()
                .bg(t.foreground)
        }))
        .when(!segments.wide, |el| {
            el.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(gpui_kit::rems(0.625))
                    .line_height(relative(1.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(Hsla { a: 0.8, ..t.foreground })
                    .child(index.to_string()),
            )
        })
        .when(segments.wide, |el| {
            el.child(
                h_flex()
                    .relative()
                    .size_full()
                    .px(px(8.))
                    .gap(px(6.))
                    .text_xs()
                    .when(several, |el| {
                        el.child(name)
                            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).child(format!("{remaining}%")))
                    })
                    .when(resets_in.is_some() || credits.is_some(), |el| {
                        el.child(
                            h_flex()
                                .ml_auto()
                                .flex_none()
                                .gap(px(6.))
                                .rounded(px(4.))
                                .bg(Hsla { a: 0.85, ..t.background })
                                .px(px(6.))
                                .py(px(2.))
                                .text_size(gpui_kit::rems(0.6875))
                                .children(resets_in.as_ref().map(|r| r.replace("resets in ", "↻ ")))
                                .when_some(credits.as_ref(), |el, credits| {
                                    el.when(resets_in.is_some(), |el| {
                                        el.child(div().text_color(t.muted_foreground).child("·"))
                                    })
                                    .child(
                                        h_flex()
                                            .gap(px(2.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(Icon::new(Lucide::Ticket).with_size(px(12.)))
                                            .child(credits.available_count.to_string()),
                                    )
                                }),
                        )
                    }),
            )
        })
        .on_hover({
            let (view, key) = (view.clone(), key.clone());
            move |hovering: &bool, _, cx| {
                let _ = view.update(cx, |this, cx| this.hover_usage_card(Some(&key), *hovering, cx));
            }
        })
        .on_click({
            let (view, key) = (view.clone(), key.clone());
            move |_, _, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.usage.card.press(&key);
                    cx.notify();
                });
            }
        });
    if let Some(focus) = segments.focus.get(&key) {
        segment = segment.track_focus(focus);
    }
    let popup = Popup::new(ElementId::Name(format!("pool-popup-{key}").into()), segment)
        .flex_1()
        .min_w_0()
        .anchor(gpui_kit::Anchor::BottomCenter)
        .offset(px(6.));
    if !open {
        return popup.into_any_element();
    }
    let card = v_flex()
        .id(ElementId::Name(format!("pool-card-{key}").into()))
        .occlude()
        .popover_style(cx)
        .p(px(16.))
        .child(segment_card(pool, window, member, segments))
        .on_hover({
            let view = view.clone();
            move |hovering: &bool, _, cx| {
                let _ = view.update(cx, |this, cx| this.hover_usage_card(None, *hovering, cx));
            }
        })
        // The segment's own click toggles the card.
        .on_mouse_down_out(move |_, _, cx| {
            let _ = view.update(cx, |this, cx| {
                let card = &mut this.usage.card;
                if card.hovered_segment.is_none() || card.hovered_segment != card.open {
                    card.close();
                    cx.notify();
                }
            });
        });
    popup.content(card).into_any_element()
}

/// An account's accent color, or its driver's.
fn account_color(account: &logic::Account, color: Hsla) -> Hsla {
    crate::pull_requests::label_color(account.accent_color.as_deref()).map_or(color, |hex| rgb(hex).into())
}

/// `AccountName`: the display name, the email's chip, or the driver.
fn account_name(account: &logic::Account) -> AnyElement {
    match (&account.display_name, &account.email) {
        (Some(name), _) => {
            div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(name.clone()).into_any_element()
        }
        (None, Some(email)) => account_chip(email).into_any_element(),
        (None, None) => div().child(driver_label(&account.driver)).into_any_element(),
    }
}

/// `LegendRow`: a narrow pool's numbered account line, which opens the segment's card.
fn legend_row(
    pool: &Pool,
    window: &PoolWindow,
    member: &logic::Member,
    index: usize,
    color: Hsla,
    segments: &Segments<'_>,
) -> AnyElement {
    let (t, now) = (segments.t, segments.now);
    let account = &pool.accounts[member.account];
    let key = segment_key(pool, window, account);
    let color = account_color(account, color);
    let resets_in = logic::format_resets_in(&member.window, now);
    let credits = account.limits.reset_credits.as_ref().map_or(0, |c| c.available_count);
    let view = segments.view.clone();
    Button::new(ElementId::Name(format!("pool-legend-{key}").into()))
        .ghost()
        .small()
        .w_full()
        .h(px(28.))
        .px(px(7.))
        .rounded(px(6.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.foreground)
        .child(
            div()
                .size(px(16.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .bg(Hsla { a: color.a * 0.35, ..color })
                .text_size(gpui_kit::rems(0.625))
                .line_height(relative(1.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(Hsla { a: 0.8, ..t.foreground })
                .child(index.to_string()),
        )
        .child(account_name(account))
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::SEMIBOLD)
                .child(format!("{}%", logic::remaining_percent(&member.window))),
        )
        .child(
            h_flex()
                .ml_auto()
                .flex_none()
                .gap(px(6.))
                .text_size(gpui_kit::rems(0.6875))
                .font_weight(FontWeight::NORMAL)
                .text_color(t.muted_foreground)
                .children(resets_in.as_ref().map(|r| r.replace("resets in ", "↻ ")))
                .when(credits > 0, |el| {
                    el.when(resets_in.is_some(), |el| el.child("·")).child(
                        h_flex()
                            .gap(px(2.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child(Icon::new(Lucide::Ticket).with_size(px(12.)))
                            .child(credits.to_string()),
                    )
                }),
        )
        .on_click(move |_, _, cx| {
            let _ = view.update(cx, |this, cx| {
                this.usage.card.press(&key);
                cx.notify();
            });
        })
        .into_any_element()
}

/// `repeating-linear-gradient(135deg, color 0 1px, transparent 1px 5px)` over the right
/// `width` of a bar.
fn hatch(color: Hsla, width: f32) -> impl IntoElement {
    const PERIOD: f32 = 5. * std::f32::consts::SQRT_2;
    div().absolute().top_0().bottom_0().right_0().w(relative(width)).overflow_hidden().child(
        canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
                let mut offset = 0.;
                while offset < width + height {
                    let mut line = PathBuilder::stroke(px(1.));
                    line.move_to(bounds.origin + point(px(offset), px(0.)));
                    line.line_to(bounds.origin + point(px(offset - height), px(height)));
                    if let Ok(path) = line.build() {
                        window.paint_path(path, color);
                    }
                    offset += PERIOD;
                }
            },
        )
        .size_full(),
    )
}

/// `SegmentPopover`: the account, its plan and where it is signed in, the window, and its reset credits.
fn segment_card(pool: &Pool, window: &PoolWindow, member: &logic::Member, segments: &Segments<'_>) -> impl IntoElement {
    let (t, now) = (segments.t, segments.now);
    let account = &pool.accounts[member.account];
    let remaining = logic::remaining_percent(&member.window);
    let time_left = logic::pace_remaining_percent(&member.window, now);
    let resets_in = logic::format_resets_in(&member.window, now);
    let signed_in = account.environments.iter().map(|(_, label)| label.as_str()).collect::<Vec<_>>().join(", ");
    let where_row = if account.environments.is_empty() {
        account.source_label.clone().map(|source| ("Via", source))
    } else {
        Some(("Signed in", signed_in))
    };
    let resets = member.window.resets_at.as_deref().and_then(|at| {
        let local = chrono::DateTime::from_timestamp_millis(now)?.with_timezone(&Local);
        let when = crate::timestamp::upcoming(at, &local, segments.hour12)?;
        Some(match &resets_in {
            Some(r) => format!("{when} · {}", r.replace("resets in ", "in ")),
            None => when,
        })
    });
    // `pool.resets`: a member with a reset clock restores its refill as a share of the pool.
    let members = window.columns.iter().flatten().count().max(1);
    let restores = (member.refill_percent / members as f64).round();
    let restores = (member.window.resets_at.is_some() && restores > 0.).then(|| format!("+{restores}% of pool"));
    let credits = account.limits.reset_credits.clone().filter(|c| c.available_count > 0 && account.redeem.is_some());
    let row = |label: &'static str, value: String| {
        h_flex()
            .gap(px(12.))
            .child(div().w(px(72.)).flex_none().text_color(t.muted_foreground).child(label))
            .child(div().min_w_0().text_color(t.foreground).child(value))
    };
    let section =
        || v_flex().gap(px(4.)).border_t_1().border_color(Hsla { a: t.border.a * 0.6, ..t.border }).pt(px(10.));
    let name = account.display_name.clone().unwrap_or_else(|| driver_label(&account.driver));
    let avatar: Option<AnyElement> = if account.redeem.is_some() {
        crate::assets::provider_icon(&account.driver).map(|(path, tint)| {
            div()
                .size(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(svg().path(path).size(px(16.)).text_color(tint.unwrap_or(Hsla { a: 0.8, ..t.foreground })))
                .into_any_element()
        })
    } else {
        account.email.as_deref().map(|email| account_chip(email).into_any_element())
    };
    let view = segments.view.clone();
    v_flex()
        .w(px(288.))
        .gap(px(10.))
        .text_xs()
        .child(
            v_flex()
                .min_w_0()
                .gap(px(2.))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .children(avatar)
                        .child(div().min_w_0().truncate().child(name)),
                )
                .children(account.email.as_deref().map(|email| {
                    let revealed = segments.card.email_revealed;
                    let view = view.clone();
                    super::settings::redacted_email(&format!("pool-{}", account.key), email, revealed, t).on_click(
                        move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.usage.card.email_revealed = !revealed;
                                cx.notify();
                            });
                        },
                    )
                })),
        )
        .child(
            section()
                .children(account.plan.clone().map(|plan| row("Plan", plan)))
                .children(where_row.map(|(label, value)| row(label, value))),
        )
        .child(
            section()
                .child(row("Left", format!("{remaining}%")))
                .children(resets.map(|resets| row("Resets", resets)))
                .children(time_left.map(|left| row("Even pace", format!("{left}% left by now"))))
                .children(restores.map(|restores| row("Restores", restores))),
        )
        .children(credits.map(|credits| {
            let busy = redeem_busy(segments.redeemed, &account.key);
            let input = account.redeem.clone();
            let (key, name) = (account.key.clone(), account.name());
            section().text_color(t.muted_foreground).child(
                h_flex().gap(px(12.)).child(logic::reset_credits_summary(&credits, now, true)).child(
                    Button::new(ElementId::Name(format!("pool-reset-{}", account.key).into()))
                        .outline()
                        .xsmall()
                        .ml_auto()
                        .label(if busy { "Using…" } else { "Use reset" })
                        .disabled(busy)
                        .on_click(move |_, window, cx| {
                            let Some(input) = input.clone() else { return };
                            let _ = view.update(cx, |this, cx| {
                                this.usage.card.close();
                                this.redeem_reset_credit(key.clone(), name.clone(), input, window, cx);
                            });
                        }),
                ),
            )
        }))
}

fn usage_skeleton(t: &Tokens, viewport: f32) -> impl IntoElement {
    let bar = |w: f32, h: f32| div().w(px(w)).h(px(h)).rounded(px(6.)).bg(t.muted);
    let fill = |h: f32| div().h(px(h)).rounded(px(6.)).bg(t.muted);
    let summary = v_flex()
        .gap(px(20.))
        .child(v_flex().gap(px(4.)).child(bar(144., 40.)).child(bar(128., 16.)))
        .children(logic::PROVIDERS.iter().map(|_| {
            v_flex()
                .gap(px(4.))
                .child(h_flex().justify_between().child(bar(104., 14.)).child(bar(56., 14.)))
                .child(bar(144., 16.))
        }));
    let chart = v_flex().gap(px(12.)).child(bar(96., 20.)).child(fill(224.).ml(px(64.)));
    v_flex()
        .gap(px(24.))
        .child(summary_and_chart(viewport >= LG, summary, chart))
        .child(v_flex().gap(px(8.)).child(section_title("Totals")).child(totals_grid(viewport >= MD).children(
            ["Processed tokens", "Cached input", "Uncached input", "Output", "Cache savings"].map(|label| {
                v_flex()
                    .gap(px(2.))
                    .child(div().text_xs().text_color(t.muted_foreground).child(label))
                    .child(bar(64., 24.))
            }),
        )))
        .child(
            share_bars_grid(viewport >= LG)
                .child(v_flex().gap(px(10.)).child(bar(112., 20.)).child(fill(8.)).child(bar(288., 16.))),
        )
        .child(
            v_flex()
                .gap(px(12.))
                .child(h_flex().justify_between().child(section_title("Breakdown")).child(bar(112., 28.)))
                .child(div().h(px(176.)).rounded(px(6.)).bg(t.muted)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_card_follows_the_hovered_segment_and_a_press_pins_it() {
        let mut card = SegmentCard::default();
        assert_eq!(card.hover_segment("a", true), Some(CARD_OPEN_DELAY));
        card.settle();
        assert_eq!(card.open.as_deref(), Some("a"));
        card.email_revealed = true;
        assert_eq!(card.hover_segment("a", false), Some(CARD_CLOSE_DELAY));
        assert_eq!(card.hover_segment("b", true), None, "an open card moves to the next segment at once");
        assert_eq!((card.open.as_deref(), card.email_revealed), (Some("b"), false));
        assert_eq!(card.hover_segment("b", false), Some(CARD_CLOSE_DELAY));
        assert_eq!(card.hover_card(true), None, "crossing onto the card keeps it");
        card.settle();
        assert_eq!(card.open.as_deref(), Some("b"));
        assert_eq!(card.hover_card(false), Some(CARD_CLOSE_DELAY));
        card.settle();
        assert_eq!(card.open, None);

        card.hover_segment("a", true);
        card.press("a");
        assert!(card.pinned && card.open.as_deref() == Some("a"), "Enter or a click opens it without waiting");
        assert_eq!(card.hover_segment("a", false), None, "a pinned card stays when the pointer leaves");
        assert_eq!(card.hover_segment("b", true), None);
        assert_eq!(card.open.as_deref(), Some("a"), "and does not follow the pointer");
        card.press("b");
        assert_eq!(card.open.as_deref(), Some("b"));
        card.press("b");
        assert_eq!(card.open, None, "a second press closes it");
        assert_eq!(card.hovered_segment.as_deref(), Some("b"), "closing keeps track of the pointer");
    }

    #[test]
    fn segmented_arrows_wrap_and_home_end_jump() {
        assert_eq!(segment_step("right", 2, 3), Some(0));
        assert_eq!(segment_step("left", 0, 3), Some(2));
        assert_eq!((segment_step("home", 2, 3), segment_step("end", 0, 3)), (Some(0), Some(2)));
        assert_eq!(segment_step("down", 0, 3), None, "a horizontal group leaves up and down alone");
    }

    #[test]
    fn page_preferences_survive_a_restart_and_bad_ones_fall_back() {
        assert_eq!(read_preferences(&Value::Null), ("limits", 30, Vec::new()));
        let usage = UsageState { metric: "tokens", days: 7, hidden: vec!["codex".into()], ..UsageState::default() };
        let stored = Value::Object(usage.preferences());
        assert_eq!(read_preferences(&stored[PREFERENCES_KEY]), ("tokens", 7, vec!["codex".to_owned()]));
        assert_eq!(read_preferences(&json!({"metric": "cost", "windowDays": 1})), ("cost", 1, Vec::new()));
        for bad in [
            json!({"metric": "cost", "windowDays": 14}),
            json!({"metric": "spend", "windowDays": 7}),
            json!({"metric": "cost", "windowDays": 7, "hiddenProviders": ["codex", "nope"]}),
        ] {
            assert_eq!(read_preferences(&bad), ("limits", 30, Vec::new()), "{bad}");
        }
    }

    fn summary(cost: f64, host: &str) -> Summary {
        serde_json::from_value(summary_json(cost, host)).unwrap()
    }

    fn summary_json(cost: f64, host: &str) -> Value {
        json!({
            "contractVersion": 6, "readAt": "2026-11-01T00:00:00Z",
            "buckets": [{
                "day": "2026-11-01", "provider": "codex", "model": "m", "sourcePath": "/codex",
                "totals": {"uncachedInputTokens": 1, "cachedInputTokens": 0, "cacheCreationTokens": 0, "outputTokens": 1, "reasoningTokens": 0},
                "costUsd": cost, "cacheSavingsUsd": 0, "costSource": "modelPriced", "records": 1, "unpricedRecords": 0, "sessions": 1,
            }],
            "sources": [{"fingerprint": {"provider": "codex", "resolvedHomePath": "/codex", "hostId": host}, "status": "ok"}],
        })
    }

    #[test]
    fn environments_are_picked_one_at_a_time_and_totals_follow() {
        let ids: Vec<SharedString> = vec!["a".into(), "b".into(), "c".into()];
        assert_eq!(toggle_environment(&ids, &ids[1], &ids), Some(vec!["a".into(), "c".into()]));
        assert_eq!(toggle_environment(&ids[..2], &ids[2], &ids), None, "every one picked is all, new ones included");
        assert_eq!(toggle_environment(&ids[..1], &ids[0], &ids), Some(Vec::new()));

        let answers = vec![
            logic::EnvironmentUsage { environment_id: "a".into(), summary: summary(1., "box-a") },
            logic::EnvironmentUsage { environment_id: "b".into(), summary: summary(2., "box-b") },
        ];
        let mut usage = UsageState { answers: Some((answers, local_window(30))), ..UsageState::default() };
        usage.recombine();
        assert_eq!(usage.merged.cost_usd, 3.);
        usage.selected = Some(vec!["b".into()]);
        usage.recombine();
        assert_eq!(usage.merged.cost_usd, 2., "only the picked environment counts");
        usage.failures = vec![("a".into(), "down".into())];
        assert_eq!(usage.error(), None, "an unpicked environment's failure is not shown");
        usage.selected = Some(Vec::new());
        usage.recombine();
        assert!(usage.summary.is_none());
    }

    #[test]
    fn each_environment_shows_as_soon_as_it_answers() {
        let (a, b): (SharedString, SharedString) = ("a".into(), "b".into());
        let month = local_window(30);
        let mut usage = UsageState {
            selected: Some(vec![a.clone()]),
            pending: vec![a.clone(), b.clone()],
            ..UsageState::default()
        };
        assert!(usage.loading());
        assert!(!usage.publish(&a, Ok(summary_json(1., "box-a")), &month, true));
        assert_eq!(usage.merged.cost_usd, 1., "a shows while b is still reading");
        assert!(!usage.loading(), "only the picked environments' reads count");
        usage.selected = None;
        assert!(usage.loading());
        usage.publish(&b, Err(crate::rpc::RpcError::Disconnected("gone".into())), &month, true);
        assert_eq!((usage.loading(), usage.merged.cost_usd), (false, 1.), "b's failure leaves a showing");
        assert!(usage.error().is_some());
        usage.publish(&b, Ok(summary_json(2., "box-b")), &month, true);
        assert_eq!((usage.error(), usage.merged.cost_usd), (None, 3.));
        usage.failures.push(("c".into(), "down".into()));
        usage.start_read(vec![a.clone()]);
        assert_eq!((usage.error(), usage.merged.cost_usd), (None, 1.), "environments that left drop out");
        let mut refreshing = summary_json(4., "box-a");
        refreshing["sources"][0]["refreshing"] = json!(true);
        assert!(usage.publish(&a, Ok(refreshing), &local_window(7), true), "slow sources are asked again");
        assert_eq!(usage.merged.cost_usd, 4., "the first answer for a new period replaces the kept ones");
    }

    #[test]
    fn limits_cool_down_per_environment() {
        let (a, b): (SharedString, SharedString) = ("a".into(), "b".into());
        let mut usage = UsageState { limits_pending: vec![a.clone()], ..UsageState::default() };
        assert!(!usage.limits_due(&a, false, 0), "a probe still out is not repeated");
        usage.limits_probed(&a, 0);
        assert!(!usage.limits_due(&a, true, 1));
        assert!(usage.limits_due(&b, true, 1), "probing a leaves b due");
        assert!(usage.limits_due(&a, false, 1), "the refresh button always probes");
        assert!(usage.limits_due(&a, true, LIMITS_AUTO_REFRESH_MS));
    }

    #[test]
    fn a_failed_read_runs_again_once_its_connection_changes() {
        let usage = UsageState { failures: vec![("a".into(), "down".into())], ..UsageState::default() };
        assert!(!usage.needs_reread(false));
        assert!(usage.needs_reread(true), "a reconnect reads again though nothing answered");
        let client = crate::rpc::RpcClient::unopened();
        let socket = client.socket_id();
        assert!(socket == client.socket_id(), "every clone names the one socket");
        drop(client);
        assert!(socket != crate::rpc::RpcClient::unopened().socket_id(), "a new socket differs");
    }
}
