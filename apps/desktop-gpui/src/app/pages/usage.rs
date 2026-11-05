//! The Usage modal (`apps/web/src/routes/_chat.usage.tsx`: `UsagePage` in an `AppRouteModal`
//! over the last main view). Cost and Tokens read `server.getUsageSummary`; Limits shows the
//! subscription windows providers report, refreshed through `server.refreshProviders`.

mod prices;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::Local;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Bounds, Context, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    MouseMoveEvent, ParentElement as _, PathBuilder, Pixels, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, canvas, div, point, px, relative, rgb, svg,
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
    /// The environment filter: this window reads one environment, so it is on or off.
    environment_selected: bool,
    window: UsageWindow,
    /// The answer on screen and the window it answers; a new window keeps it until it lands.
    summary: Option<(Rc<Summary>, UsageWindow)>,
    merged: Rc<Merged>,
    error: Option<SharedString>,
    loading: bool,
    refreshing: bool,
    generation: u64,
    limits_now: i64,
    limits_refreshed_at: Option<i64>,
    by_time: bool,
    /// The chart column under the pointer, and where the pointer is in the plot.
    hover: Option<(usize, Pixels, Pixels)>,
    scroll: ScrollHandle,
    /// Reset credits being confirmed, spent, or spent with the outcome, one per account key.
    redeemed: Vec<Redeem>,
    /// The chart's data, kept across pointer moves; render may refill it, hence the cell.
    chart: RefCell<Option<Rc<ChartData>>>,
    /// The limit pools and what they were built from: each environment's config revision,
    /// the hidden drivers, `limits_now`.
    pools: Option<(PoolsKey, Rc<Vec<Pool>>)>,
}

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
    series: Rc<Vec<(usize, Vec<logic::Segment>, f64)>>,
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
            series: Rc::new(series),
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

/// A usage window ending now, in the system time zone.
fn local_window(days: u32) -> UsageWindow {
    let zone = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
    UsageWindow::new(days, crate::timestamp::now_ms(), &Local, zone)
}

impl Default for UsageState {
    fn default() -> Self {
        Self {
            metric: "limits",
            days: 30,
            hidden: Vec::new(),
            environment_selected: true,
            window: local_window(30),
            summary: None,
            merged: Rc::default(),
            error: None,
            loading: false,
            refreshing: false,
            generation: 0,
            limits_now: now_ms(),
            limits_refreshed_at: None,
            by_time: false,
            hover: None,
            scroll: ScrollHandle::new(),
            redeemed: Vec::new(),
            pools: None,
            chart: RefCell::default(),
        }
    }
}

impl UsageState {
    fn tokens(&self) -> bool {
        self.metric == "tokens"
    }

    fn hidden_drivers(&self) -> Vec<&'static str> {
        logic::PROVIDERS.iter().filter(|p| self.hidden.iter().any(|h| h == p.0)).map(|p| p.2).collect()
    }
}

impl Workspace {
    pub(in crate::app) fn usage_opened(&mut self, cx: &mut Context<Self>) {
        self.usage.limits_now = now_ms();
        if self.usage.metric == "limits" {
            self.refresh_limits(true, cx);
        } else if self.usage.summary.is_none() && !self.usage.loading {
            self.read_usage(false, cx);
        }
    }

    fn select_usage_metric(&mut self, metric: &'static str, cx: &mut Context<Self>) {
        self.usage.metric = metric;
        self.usage.hover = None;
        if metric == "limits" {
            self.usage.limits_now = now_ms();
            self.refresh_limits(true, cx);
        } else if self.usage.summary.is_none() && !self.usage.loading {
            self.read_usage(false, cx);
        }
        cx.notify();
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
        self.usage.window = local_window(days);
        self.usage.hover = None;
        self.read_usage(false, cx);
    }

    fn set_hidden_usage_providers(&mut self, hidden: Vec<String>, cx: &mut Context<Self>) {
        self.usage.hidden = hidden;
        self.remerge_usage();
        cx.notify();
    }

    fn remerge_usage(&mut self) {
        self.usage.merged = Rc::new(match &self.usage.summary {
            Some((summary, _)) => logic::merge(summary, &self.usage.hidden),
            None => Merged::default(),
        });
    }

    /// The environments the page reads: every connected one.
    fn usage_machines(&self) -> Vec<(SharedString, crate::rpc::RpcClient)> {
        self.sessions().iter().filter_map(|s| Some((s.id.clone()?, s.client.clone()?))).collect()
    }

    /// The selected environments' provider and limit-source reads, with their labels.
    fn limit_environments(&self) -> Vec<(SharedString, SharedString, &crate::model::ServerInfo)> {
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
        let machines: Vec<(String, crate::rpc::RpcClient)> =
            self.usage_machines().into_iter().map(|(id, client)| (id.to_string(), client)).collect();
        if machines.is_empty() {
            return;
        }
        let usage = &mut self.usage;
        usage.generation += 1;
        let generation = usage.generation;
        usage.loading = true;
        let window = usage.window.clone();
        let input = window.to_json();
        cx.spawn(async move |this, cx| {
            if refresh_rates {
                let refreshes =
                    machines.iter().map(|(_, client)| client.request("server.refreshUsageRates", json!({})));
                futures::future::join_all(refreshes).await;
            }
            let mut input = input;
            for attempt in 0..2 {
                let reads = machines.iter().map(|(id, client)| {
                    let input = input.clone();
                    async move { (id.clone(), client.request("server.getUsageSummary", input).await) }
                });
                let results = futures::future::join_all(reads).await;
                let mut again = false;
                let applied = this.update(cx, |this, cx| {
                    let usage = &mut this.usage;
                    if usage.generation != generation {
                        return;
                    }
                    // A machine that fails to answer leaves the others' usage showing.
                    let mut answers = Vec::new();
                    let mut failure = None;
                    for (environment_id, result) in results {
                        match result.map(serde_json::from_value::<Summary>) {
                            Ok(Ok(summary)) => answers.push(logic::EnvironmentUsage { environment_id, summary }),
                            Ok(Err(error)) => {
                                failure.get_or_insert_with(|| format!("Unreadable usage summary: {error}"));
                            }
                            Err(error) => {
                                failure.get_or_insert_with(|| usage_error(&error));
                            }
                        }
                    }
                    again = attempt == 0 && answers.iter().any(|answer| answer.summary.refreshing());
                    let answered = !answers.is_empty();
                    match (logic::combine(answers), failure) {
                        (Some(summary), _) => {
                            usage.error = None;
                            usage.summary = Some((Rc::new(summary), window.clone()));
                            this.remerge_usage();
                        }
                        (None, Some(failure)) if !answered => usage.error = Some(failure.into()),
                        (None, _) => {}
                    }
                    this.usage.loading = again;
                    if !again {
                        this.usage.refreshing = false;
                    }
                    cx.notify();
                });
                if applied.is_err() || !again {
                    return;
                }
                input["awaitRefresh"] = json!(true);
            }
        })
        .detach();
        cx.notify();
    }

    /// Re-probes provider limits (`refreshUsageLimits`): automatic refreshes wait five minutes
    /// between probes.
    fn refresh_limits(&mut self, automatic: bool, cx: &mut Context<Self>) {
        let machines = self.usage_machines();
        if machines.is_empty() {
            return;
        }
        let now = now_ms();
        if automatic && self.usage.limits_refreshed_at.is_some_and(|at| now - at < LIMITS_AUTO_REFRESH_MS) {
            return;
        }
        self.usage.limits_refreshed_at = Some(now);
        if !automatic {
            self.usage.refreshing = true;
        }
        cx.spawn(async move |this, cx| {
            let probes = machines.iter().map(|(id, client)| async move {
                (id, client.request("server.refreshProviders", json!({"workspaceSnapshotsOnDemand": true})).await)
            });
            let results = futures::future::join_all(probes).await;
            let _ = this.update(cx, |this, cx| {
                // Each answer lands on the environment that gave it, if it is still here.
                for (id, result) in results {
                    let (Ok(payload), Some(info)) = (result, this.env_info_mut(id)) else { continue };
                    let event = json!({"type": "providerStatuses", "payload": payload});
                    if let Err(error) = info.apply(&event) {
                        log::warn!("refreshed providers did not decode: {error}");
                    }
                }
                this.usage.limits_now = now_ms();
                this.usage.limits_refreshed_at = Some(now_ms());
                this.usage.refreshing = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
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
        let viewport = window.viewport_size();
        let padding = if viewport.width >= px(1024.) {
            40.
        } else if viewport.width >= px(640.) {
            24.
        } else {
            0.
        };
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
                    .child(self.render_usage_header(&t, cx))
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
                                    .px(px(24.))
                                    .pt(px(24.))
                                    .pb(px(48.))
                                    .gap(px(24.))
                                    .child(self.render_usage_body(&t, cx)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_usage_header(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let usage = &self.usage;
        let limits = usage.metric == "limits";
        let view = cx.entity();
        let separator = || div().flex_none().text_color(t.muted_foreground).child("/");
        let labels: Vec<SharedString> = self.limit_environments().into_iter().map(|(_, label, _)| label).collect();
        let environment: SharedString =
            if labels.is_empty() { "This environment".into() } else { labels.join(", ").into() };
        let summary = usage.summary.as_ref().map(|(s, _)| s.clone());
        let has_issue = !limits && (usage.error.is_some() || summary.as_ref().is_some_and(|s| !s.compatible()));
        let env_label =
            if usage.environment_selected { "All environments".to_owned() } else { "0 environments".into() };
        let env_status: SharedString = if usage.error.is_some() {
            "Unavailable".into()
        } else if summary.as_ref().is_some_and(|s| !s.compatible()) {
            "Update required".into()
        } else if usage.loading && summary.is_none() {
            "Scanning…".into()
        } else if usage.loading {
            "Refreshing…".into()
        } else {
            "Ready".into()
        };
        let selected = usage.environment_selected;
        let env_filter = inline_menu_button("usage-environment", t)
            .text_color(t.foreground)
            .child(env_label)
            .child(if has_issue {
                Icon::new(Lucide::CircleAlert).with_size(px(14.)).text_color(t.warning_foreground)
            } else {
                Icon::new(Lucide::ChevronDown).with_size(px(14.)).text_color(Hsla { a: 0.6, ..t.muted_foreground })
            })
            .dropdown_menu({
                let view = view.clone();
                let muted = t.muted_foreground;
                let error = usage.error.clone();
                move |menu, window, _| {
                    let env = environment.clone();
                    let status = env_status.clone();
                    let error_line = error.clone();
                    menu.item(PopupMenuItem::new("All environments").checked(selected).on_click(window.listener_for(
                        &view,
                        |this, _, _, cx| {
                            this.usage.environment_selected = !this.usage.environment_selected;
                            cx.notify();
                        },
                    )))
                    .separator()
                    .item(
                        PopupMenuItem::element(move |_, _| {
                            h_flex()
                                .min_w(px(200.))
                                .gap(px(12.))
                                .child(div().flex_1().min_w_0().truncate().child(env.clone()))
                                .when(!limits, |el| {
                                    el.child(div().flex_none().text_xs().text_color(muted).child(status.clone()))
                                })
                        })
                        .checked(selected)
                        .on_click(window.listener_for(&view, |this, _, _, cx| {
                            this.usage.environment_selected = !this.usage.environment_selected;
                            cx.notify();
                        })),
                    )
                    .when_some(error_line.filter(|_| !limits), |menu, error| menu.separator().label(error))
                    .separator()
                    .item(
                        PopupMenuItem::new("Model prices").icon(Lucide::SlidersHorizontal).on_click(
                            window.listener_for(&view, |this, _, window, cx| this.open_model_prices(None, window, cx)),
                        ),
                    )
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
                                this.set_hidden_usage_providers(next, cx)
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
                            this.set_hidden_usage_providers(next, cx)
                        })));
                    }
                    menu
                }
            });

        let periods = segmented(
            "usage-period",
            logic::WINDOWS.iter().map(|(days, label)| (*days, *label)),
            usage.days,
            t,
            cx,
            |this, days, cx| this.select_usage_window(days, cx),
        );
        let metrics = segmented(
            "usage-metric",
            logic::METRICS.iter().map(|(value, label)| (*value, *label)),
            usage.metric,
            t,
            cx,
            |this, metric, cx| this.select_usage_metric(metric, cx),
        );
        let disabled = usage.refreshing || (!limits && !usage.environment_selected);
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
                h_flex().flex_none().gap(px(8.)).when(!limits, |el| el.child(periods)).child(metrics).child(
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

    fn render_usage_body(&mut self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let usage = &self.usage;
        let limits = usage.metric == "limits";
        if !usage.environment_selected {
            let what = if limits { "limits" } else { "usage" };
            return muted_line(format!("Select an environment to see {what}."), t).into_any_element();
        }
        if limits {
            return self.render_limits(t, cx);
        }
        match (&usage.summary, &usage.error) {
            (None, Some(error)) => v_flex()
                .py(px(48.))
                .items_center()
                .text_sm()
                .text_color(t.muted_foreground)
                .child(error.clone())
                .into_any_element(),
            (None, None) => usage_skeleton(t).into_any_element(),
            (Some((summary, window)), _) => self.render_usage_overview(summary, window, t, cx),
        }
    }

    /// The limit pools, rebuilt only when the server config, the hidden drivers or the clock moved.
    fn limit_pools(&mut self) -> Rc<Vec<Pool>> {
        let environments = self.limit_environments();
        let revisions = environments.iter().map(|(id, _, info)| (id.clone(), info.revision)).collect();
        let key = (revisions, self.usage.hidden.clone(), self.usage.limits_now);
        if let Some((held, pools)) = &self.usage.pools
            && *held == key
        {
            return pools.clone();
        }
        let accounts = logic::limit_accounts(&limit_inputs(&environments), &self.usage.hidden_drivers());
        let pools = Rc::new(logic::limit_pools(accounts, self.usage.limits_now));
        self.usage.pools = Some((key, pools.clone()));
        pools
    }

    fn render_limits(&mut self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
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
        for pool in pools.iter() {
            let view = cx.weak_entity();
            let redeemed = &self.usage.redeemed;
            body = body.child(pool_section(pool, now, redeemed, &view, t));
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
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let usage = &self.usage;
        let merged = usage.merged.clone();
        let tokens = usage.tokens();
        let dim = |el: gpui_kit::Div| if usage.loading || usage.refreshing { el.opacity(0.4) } else { el };
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
        for kind in &providers {
            let totals = merged.providers.iter().find(|p| p.provider == *kind).cloned().unwrap_or_default();
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
        overview = overview.child(
            h_flex().items_start().gap(px(24.)).child(summary_column.w(px(288.)).flex_none()).child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(12.))
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(chart_title))
                    .child(self.render_usage_chart(&merged, window, &providers, t, cx)),
            ),
        );

        let metric = |label: &str, value: String| {
            v_flex()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_xs().text_color(t.muted_foreground).child(label.to_owned()))
                .child(dim(div()).text_base().font_weight(FontWeight::MEDIUM).child(value))
        };
        overview = overview.child(
            v_flex().gap(px(8.)).mt(px(18.)).child(section_title("Totals")).child(
                h_flex().py(px(4.)).children(
                    [
                        metric("Processed tokens", logic::format_tokens(merged.total_tokens as f64)),
                        metric("Cached input", logic::format_tokens(merged.cached_input_tokens as f64)),
                        metric("Uncached input", logic::format_tokens(merged.uncached_input_tokens as f64)),
                        metric("Output", logic::format_tokens(merged.output_tokens as f64)),
                        metric("Cache savings", logic::format_usd(merged.cache_savings_usd)),
                    ]
                    .into_iter()
                    .map(|m| m.flex_1().pr(px(24.))),
                ),
            ),
        );

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
                h_flex()
                    .mt(px(18.))
                    .gap(px(48.))
                    .children(bars.into_iter().map(|bar| div().flex_1().min_w_0().child(bar))),
            );
        }

        overview.child(self.render_usage_breakdown(&merged, window, &providers, t, cx).mt(px(18.))).into_any_element()
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
            move |bounds, _, window, _| {
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
                (PeriodKey::Hour(ms), Some((_, until))) => logic::format_relative_hour_short(*ms, until, &tz),
                _ => key.label(&tz),
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
        let label_at = |index: usize| periods.get(index).map(|key| key.label(&tz).to_uppercase()).unwrap_or_default();
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
                            let cell = bounds_cell.clone();
                            cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                                let Some(bounds) = cell.get() else { return };
                                if period_count == 0 || !bounds.contains(&event.position) {
                                    return;
                                }
                                let local = event.position - bounds.origin;
                                let fraction = (local.x / bounds.size.width).clamp(0., 1.);
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
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let usage = &self.usage;
        let tokens = usage.tokens();
        let hourly = window.hours.is_some();
        let by_time = usage.by_time;
        let toggle = segmented(
            "usage-breakdown",
            [(false, "Model"), (true, if hourly { "Hour" } else { "Day" })].into_iter(),
            by_time,
            t,
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
        if !by_time {
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
                let bar = (value > 0. && peak > 0.).then(|| (value / peak) as f32);
                let share = model.share(tokens).map(logic::format_percent).unwrap_or_default();
                table = table.child(
                    h_flex()
                        .id(ElementId::Name(format!("usage-model-{}-{}", model.provider, model.model).into()))
                        .border_b_1()
                        .border_color(row_border)
                        .text_color(t.muted_foreground)
                        .hover(|s| s.bg(Hsla { a: 0.5, ..t.muted }))
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
        } else {
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
                    Some(ms) => logic::format_hour_short(ms, &Local),
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
                            .hover(|s| s.bg(Hsla { a: 0.5, ..t.muted }))
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
    Button::new(id)
        .ghost()
        .xsmall()
        .h(px(28.))
        .px(px(0.))
        .gap(px(4.))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.muted_foreground)
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

/// `ToggleGroup variant="segmented"`.
pub(super) fn segmented<V: Copy + PartialEq + std::fmt::Debug + 'static>(
    id: &'static str,
    options: impl Iterator<Item = (V, &'static str)>,
    current: V,
    t: &Tokens,
    cx: &mut Context<Workspace>,
    pick: fn(&mut Workspace, V, &mut Context<Workspace>),
) -> gpui_kit::Div {
    let track = Hsla { a: t.input.a * 0.4, ..t.input };
    h_flex().flex_none().gap(px(2.)).p(px(2.)).rounded(px(8.)).bg(track).children(options.map(|(value, label)| {
        let pressed = value == current;
        div()
            .id(ElementId::Name(format!("{id}-{value:?}").into()))
            .h(px(24.))
            .px(px(10.))
            .flex()
            .items_center()
            .rounded(px(6.))
            .text_xs()
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
            .on_click(cx.listener(move |this, _, _, cx| pick(this, value, cx)))
    }))
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
                div()
                    .id(ElementId::Name(format!("share-{label}-{name}").into()))
                    .h_full()
                    .min_w(px(4.))
                    .flex_basis(relative((value / total) as f32))
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

/// `PoolSection`: one driver's windows, each pooled across its accounts.
fn pool_section(pool: &Pool, now: i64, redeemed: &[Redeem], view: &WeakEntity<Workspace>, t: &Tokens) -> AnyElement {
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
        .children(pool.windows.iter().map(|window| pool_window_card(pool, window, color, now, redeemed, view, t)))
        // The confirm closed with no popover to report back in, so progress and the outcome show here.
        .children(redeemed.iter().filter(|r| pool.accounts.iter().any(|a| a.key == r.key)).filter_map(|r| {
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

fn pool_window_card(
    pool: &Pool,
    window: &PoolWindow,
    color: Hsla,
    now: i64,
    redeemed: &[Redeem],
    view: &WeakEntity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let cursor = (pool.driver == "cursor").then(|| logic::CURSOR_WINDOWS.iter().find(|w| w.0 == window.id)).flatten();
    let label = cursor.map_or(window.label.clone(), |w| w.1.to_owned());
    let several = window.columns.len() > 1;
    let segments = window.columns.iter().map(|member| {
        let Some(member) = member else { return div().flex_1().min_w_0().into_any_element() };
        pool_segment(pool, window, member, several, color, now, redeemed, view, t)
    });
    v_flex()
        .gap(px(12.))
        .rounded(px(8.))
        .border_1()
        .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
        .p(px(16.))
        .child(
            h_flex()
                .gap(px(24.))
                .child(
                    v_flex()
                        .w(px(176.))
                        .flex_none()
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
                                .child(div().text_sm().text_color(t.muted_foreground).child("left"))
                                .children(
                                    window
                                        .pace
                                        .map(|pace| pace_icon(format!("{}-{}", pool.driver, window.id), pace, t)),
                                ),
                        )
                        .when_some(window.next_refill.filter(|_| several), |el, refill| {
                            el.child(div().text_xs().font_weight(FontWeight::MEDIUM).child(format!("↻ +{refill}%")))
                        }),
                )
                .child(h_flex().flex_1().min_w_0().gap(px(4.)).children(segments)),
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

#[expect(clippy::too_many_arguments, reason = "render helper; the arguments are its render context")]
fn pool_segment(
    pool: &Pool,
    window: &PoolWindow,
    member: &logic::Member,
    several: bool,
    color: Hsla,
    now: i64,
    redeemed: &[Redeem],
    view: &WeakEntity<Workspace>,
    t: &Tokens,
) -> AnyElement {
    let account = &pool.accounts[member.account];
    let credits = account.limits.reset_credits.clone().filter(|c| c.available_count > 0);
    let redeem = account.redeem.clone().filter(|_| credits.is_some());
    let color = crate::pull_requests::label_color(account.accent_color.as_deref()).map_or(color, |hex| rgb(hex).into());
    let remaining = logic::remaining_percent(&member.window);
    let time_left = logic::pace_remaining_percent(&member.window, now);
    let resets_in = logic::format_resets_in(&member.window, now);
    let name: AnyElement = match (&account.display_name, &account.email) {
        (Some(name), _) => {
            div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(name.clone()).into_any_element()
        }
        (None, Some(email)) => account_chip(email).into_any_element(),
        (None, None) => div().child(driver_label(&account.driver)).into_any_element(),
    };
    let signed_in = account.environments.iter().map(|(_, label)| label.as_str()).collect::<Vec<_>>().join(", ");
    let where_label = match &account.source_label {
        Some(source) => Some(("Via", source.clone())),
        None => (!signed_in.is_empty()).then_some(("Signed in", signed_in)),
    };
    let details: Vec<(&'static str, String)> = [
        account.plan.clone().map(|plan| ("Plan", plan)),
        where_label,
        Some(("Left", format!("{remaining}%"))),
        resets_in.clone().map(|r| ("Resets", r.replace("resets in ", "in "))),
        time_left.map(|left| ("Even pace", format!("{left}% left by now"))),
        (member.refill_percent > 0. && several && remaining < 100).then(|| {
            ("Restores", format!("+{}% of pool", (member.refill_percent / window.columns.len() as f64).round()))
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    // The web's popover holds a "Use reset" button; here the segment itself is that button.
    let credit_line = credits.as_ref().filter(|_| redeem.is_some()).map(|c| logic::reset_credits_summary(c, now, true));
    let border = t.border;
    let title = account.name();
    let email = account.email.clone().filter(|_| account.display_name.is_some());
    let fill = Hsla { a: color.a * 0.35, ..color };
    div()
        .id(ElementId::Name(format!("pool-{}-{}-{}", pool.driver, window.id, account.key).into()))
        .relative()
        .flex_1()
        .min_w_0()
        .h(px(32.))
        .rounded(px(6.))
        .bg(t.muted)
        .overflow_hidden()
        .cursor_pointer()
        .child(
            div().absolute().top_0().bottom_0().left_0().w(relative(remaining as f32 / 100.)).rounded(px(6.)).bg(fill),
        )
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
        .child(
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
        .when_some(redeem.filter(|_| !redeem_busy(redeemed, &account.key)), |el, input| {
            let (view, key, name) = (view.clone(), account.key.clone(), account.name());
            el.on_click(move |_, window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.redeem_reset_credit(key.clone(), name.clone(), input.clone(), window, cx)
                });
            })
        })
        .tooltip(move |window, cx| {
            let details = details.clone();
            let title = title.clone();
            let email = email.clone();
            let credit_line = credit_line.clone();
            Tooltip::element(move |_, _| {
                v_flex()
                    .w(px(288.))
                    .gap(px(10.))
                    .text_xs()
                    .child(
                        v_flex()
                            .gap(px(2.))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title.clone()))
                            .children(email.clone().map(|e| div().child(e))),
                    )
                    .children(details.iter().map(|(label, value)| {
                        h_flex().gap(px(12.)).child(div().w(px(72.)).flex_none().child(*label)).child(value.clone())
                    }))
                    .children(credit_line.clone().map(|line| {
                        h_flex().gap(px(12.)).child(line).child(
                            div()
                                .ml_auto()
                                .rounded(px(6.))
                                .border_1()
                                .border_color(border)
                                .px(px(6.))
                                .child("Use reset"),
                        )
                    }))
            })
            .build(window, cx)
        })
        .into_any_element()
}

fn usage_skeleton(t: &Tokens) -> impl IntoElement {
    let bar = |w: f32, h: f32| div().w(px(w)).h(px(h)).rounded(px(6.)).bg(t.muted);
    v_flex()
        .gap(px(24.))
        .child(
            h_flex()
                .items_start()
                .gap(px(24.))
                .child(
                    v_flex()
                        .w(px(288.))
                        .gap(px(20.))
                        .child(v_flex().gap(px(4.)).child(bar(144., 40.)).child(bar(128., 16.)))
                        .children(logic::PROVIDERS.iter().map(|_| {
                            v_flex()
                                .gap(px(4.))
                                .child(h_flex().justify_between().child(bar(104., 14.)).child(bar(56., 14.)))
                                .child(bar(144., 16.))
                        })),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .gap(px(12.))
                        .child(bar(96., 20.))
                        .child(div().ml(px(64.)).h(px(224.)).rounded(px(6.)).bg(t.muted)),
                ),
        )
        .child(v_flex().gap(px(8.)).child(section_title("Totals")).child(h_flex().children(
            ["Processed tokens", "Cached input", "Uncached input", "Output", "Cache savings"].map(|label| {
                v_flex()
                    .flex_1()
                    .gap(px(2.))
                    .child(div().text_xs().text_color(t.muted_foreground).child(label))
                    .child(bar(64., 24.))
            }),
        )))
        .child(
            v_flex()
                .gap(px(12.))
                .child(h_flex().justify_between().child(section_title("Breakdown")).child(bar(112., 28.)))
                .child(div().h(px(176.)).rounded(px(6.)).bg(t.muted)),
        )
}
