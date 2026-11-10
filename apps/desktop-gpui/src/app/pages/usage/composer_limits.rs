//! The composer's usage-limits panel (`useUsageLimitsPanel.ts`, `ComposerUsageLimits.tsx`):
//! `/usage-limits` opens the selected provider's limits in the banner dock. The report is
//! rebuilt from the live provider statuses on every render, so it stays current while open;
//! a new thread, model, run or pending request closes it.

use std::cell::RefCell;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, black, div, px, relative, rgb, white,
};
use serde_json::Value;

use super::{driver_label, pace_icon, provider_color};
use crate::app::{Target, Workspace};
use crate::model::UsageWindow;
use crate::theme::Tokens;
use crate::usage::{self as logic, LimitsReport, ReportAccount};

const PROVIDERS_MANAGE: &str = "providers:manage";

/// The panel as opened: which composer state it belongs to and when it opened.
struct Opened {
    key: String,
    thread: Target,
    now: i64,
    revealed: bool,
    /// Reset credits being spent, or spent with the outcome, by account id.
    redeems: Vec<(String, Option<String>)>,
}

/// The open panel, if any. A render that finds a different key closes it for good, as the web's
/// `panel.key !== panelKey` reset does, hence the cell.
#[derive(Default)]
pub(in crate::app) struct UsageLimitsPanel {
    opened: RefCell<Option<Opened>>,
}

impl UsageLimitsPanel {
    fn open(&self, key: String, thread: Target, now: i64) {
        *self.opened.borrow_mut() = Some(Opened { key, thread, now, revealed: false, redeems: Vec::new() });
    }

    fn close(&self) {
        self.opened.borrow_mut().take();
    }

    /// When the panel showing under `key` opened; any other key closes it. Without a key (a
    /// thread still loading) it only hides.
    fn current(&self, key: Option<&str>) -> Option<i64> {
        let key = key?;
        let mut opened = self.opened.borrow_mut();
        if opened.as_ref().is_some_and(|o| o.key != key) {
            *opened = None;
        }
        opened.as_ref().map(|o| o.now)
    }

    /// `clearFor`: work that spends quota resumed on `thread`, so its panel goes stale.
    fn clear_for(&self, thread: &Target) {
        let mut opened = self.opened.borrow_mut();
        if opened.as_ref().is_some_and(|o| &o.thread == thread) {
            *opened = None;
        }
    }

    fn with<R>(&self, now: i64, f: impl FnOnce(&mut Opened) -> R) -> Option<R> {
        self.opened.borrow_mut().as_mut().filter(|o| o.now == now).map(f)
    }
}

/// `accountLabel`: the driver, and the instance when it is named apart from it.
fn account_label(account: &ReportAccount) -> String {
    let Some(instance_id) = &account.instance_id else { return account.label.clone() };
    let driver = driver_label(&account.driver);
    let named = account.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let instance = named.or((*instance_id != account.driver).then_some(instance_id.as_str())).unwrap_or_default();
    if !instance.is_empty() && instance.to_lowercase() != driver.to_lowercase() {
        format!("{driver} · {instance}")
    } else {
        driver
    }
}

impl Workspace {
    /// `usageLimitsKey`: the thread, the provider instance, its latest run and pending request.
    fn usage_limits_key(&self) -> Option<String> {
        let target = self.target.as_ref()?;
        let instance = self.composer_model()?.instance_id;
        let (thread, run, request) = match target {
            Target::Thread { id, .. } => {
                let projection = self.open_projection()?;
                let run = self.shell.threads.iter().find(|t| &t.id == id).and_then(|t| t.latest_run_id.clone());
                let request = projection
                    .pending_approvals()
                    .first()
                    .map(|(request, _)| request.id.clone())
                    .or_else(|| projection.pending_user_inputs().first().map(|(request, _)| request.id.clone()));
                (id.as_str(), run, request)
            }
            Target::Draft { project_id, .. } => (project_id.as_str(), None, None),
        };
        let env = target.environment();
        Some(format!("{env}\0{thread}\0{instance}\0{}\0{}", run.unwrap_or_default(), request.unwrap_or_default()))
    }

    /// Whether `/usage-limits` opens the panel instead of going to the provider (`offered`):
    /// Limits has data for the selected provider and the draft is only the command.
    pub(in crate::app) fn usage_limits_offered(&self) -> bool {
        let Some(provider) = self.composer_provider() else { return false };
        let info = self.server_info();
        !self.has_attachments()
            && self.usage_limits_key().is_some()
            && logic::has_provider_usage_limits(&provider.driver, &info.providers, &info.config["usageLimitSources"])
    }

    fn usage_limits_report(&self, now: i64) -> Option<LimitsReport> {
        let instance = self.composer_model()?.instance_id;
        let info = self.server_info();
        logic::provider_usage_limits(&instance, &info.providers, &info.config["usageLimitSources"], now)
    }

    /// `open`: shows the panel, or says why it cannot.
    pub(in crate::app) fn open_usage_limits(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let now = crate::timestamp::now_ms();
        let opened =
            self.usage_limits_key().zip(self.target.clone()).filter(|_| self.usage_limits_report(now).is_some());
        let panel = &self.composer_ui.usage_limits;
        cx.notify();
        if let Some((key, thread)) = opened {
            panel.open(key, thread, now);
            return true;
        }
        panel.close();
        window.push_notification(Notification::info("Usage limits are unavailable for this provider"), cx);
        false
    }

    pub(in crate::app) fn clear_usage_limits_for(&self, thread: &Target) {
        self.composer_ui.usage_limits.clear_for(thread);
    }

    /// "Use a reset credit?", then `provider.consumeResetCredit` on the composer's environment
    /// (`useResetCredit`); the outcome lands only on the opening that asked.
    fn redeem_panel_credit(
        &mut self,
        opened_at: i64,
        account: String,
        input: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(env) = self.target.as_ref().map(|t| t.environment().clone()) else { return };
        let Some(client) = self.env_client(&env).filter(|_| self.env_grants(&env, PROVIDERS_MANAGE)) else { return };
        let busy = self
            .composer_ui
            .usage_limits
            .with(opened_at, |o| o.redeems.iter().any(|(id, s)| *id == account && s.is_none()));
        if busy != Some(false) {
            return;
        }
        let answer = window.prompt(
            gpui_kit::PromptLevel::Warning,
            "Use a reset credit?",
            Some("This redeems one credit on your account and clears the current rate-limit windows. It cannot be undone."),
            &["Use credit", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let started = this.update(cx, |this, cx| {
                // Grants change on reconnect, so the confirmation does not carry one over.
                if !this.env_grants(&env, PROVIDERS_MANAGE) {
                    return false;
                }
                let account = account.clone();
                let started = this.composer_ui.usage_limits.with(opened_at, |o| {
                    o.redeems.retain(|(id, _)| *id != account);
                    o.redeems.push((account, None));
                });
                cx.notify();
                started.is_some()
            });
            if !started.unwrap_or(false) {
                return;
            }
            let status = match client.request("provider.consumeResetCredit", input).await {
                Ok(result) => logic::reset_outcome_text(&result),
                Err(error) => error.to_string(),
            };
            let _ = this.update(cx, |this, cx| {
                this.composer_ui.usage_limits.with(opened_at, |o| {
                    if let Some(entry) = o.redeems.iter_mut().find(|(id, _)| *id == account) {
                        entry.1 = Some(status);
                    }
                });
                cx.notify();
            });
        })
        .detach();
    }

    /// `usageLimitsBannerItem`: the open panel in the banner dock; a `joined` card follows another.
    pub(in crate::app) fn render_usage_limits_banner(
        &self,
        joined: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel = &self.composer_ui.usage_limits;
        let opened_at = panel.current(self.usage_limits_key().as_deref())?;
        let report = self.usage_limits_report(opened_at)?;
        let (revealed, redeems) = panel.with(opened_at, |o| (o.revealed, o.redeems.clone()))?;
        let env = self.target.as_ref()?.environment().clone();
        let can_manage = self.env_grants(&env, PROVIDERS_MANAGE);
        let summary = |account: &ReportAccount, id: &str, cx: &mut Context<Self>| -> AnyElement {
            let label = account_label(account);
            let plan = account.plan.as_ref().map(|plan| format!(" · {plan}"));
            let name = if label.contains('@') {
                super::super::settings::redacted_email(&format!("usage-limits-{id}"), &label, revealed, t)
                    .text_xs()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.composer_ui.usage_limits.with(opened_at, |o| o.revealed = !o.revealed);
                        cx.notify();
                    }))
                    .into_any_element()
            } else {
                div().child(label).into_any_element()
            };
            h_flex().min_w_0().child(name).children(plan).into_any_element()
        };
        let description = match report.accounts.as_slice() {
            [single] => summary(single, "summary", cx),
            accounts => div().child(format!("{} accounts", accounts.len())).into_any_element(),
        };
        let several = report.accounts.len() > 1;
        let mut body = v_flex().pl(px(28.)).pr(px(8.)).pt(px(4.)).pb(px(6.)).gap(px(8.));
        for account in &report.accounts {
            let notice = logic::limits_notice(&account.limits);
            let mut row = v_flex().min_w_0().gap(px(4.));
            if several {
                row = row.child(div().min_w_0().truncate().text_color(t.muted_foreground).child(summary(
                    account,
                    &account.id,
                    cx,
                )));
            }
            row = match notice {
                Some(notice) => row.child(div().text_color(t.muted_foreground).child(notice)),
                None => row.child(limit_windows(account, report.now, self.hour12(), t)),
            };
            if let Some(external) = &account.limits.external_usage {
                let url = external.url.clone();
                row = row.child(
                    Button::new(ElementId::Name(format!("usage-limits-manage-{}", account.id).into()))
                        .link()
                        .small()
                        .label("Manage usage")
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                );
            }
            let credits = account.limits.reset_credits.as_ref().zip(account.reset_credit_input.as_ref());
            if let Some((credits, input)) = credits {
                let status = redeems.iter().find(|(id, _)| *id == account.id).map(|(_, status)| status);
                let busy = status.is_some_and(Option::is_none);
                let status = status.cloned().flatten();
                if credits.available_count > 0 || status.is_some() {
                    let (id, input) = (account.id.clone(), input.clone());
                    row = row.child(
                        h_flex()
                            .flex_wrap()
                            .gap_x(px(12.))
                            .gap_y(px(4.))
                            .text_color(t.muted_foreground)
                            .child(logic::reset_credits_summary(credits, report.now, false))
                            .when(credits.available_count > 0, |el| {
                                el.child(
                                    Button::new(ElementId::Name(format!("usage-limits-reset-{id}").into()))
                                        .outline()
                                        .xsmall()
                                        .label(if busy { "Using…" } else { "Use reset" })
                                        .disabled(busy || !can_manage)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.redeem_panel_credit(opened_at, id.clone(), input.clone(), window, cx);
                                        })),
                                )
                            })
                            .children(status.map(|status| div().text_color(t.foreground).child(status))),
                    );
                }
            }
            body = body.child(row);
        }
        body = body.children(report.notices.iter().map(|n| div().text_color(t.muted_foreground).child(n.clone())));
        let outline = if t.dark { white().opacity(0.05) } else { black().opacity(0.08) };
        let header = h_flex()
            .gap(px(4.))
            .child(
                div()
                    .size(px(24.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(Lucide::Gauge).with_size(px(12.)).text_color(t.muted_foreground)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(div().flex_none().line_height(px(24.)).font_weight(FontWeight::MEDIUM).child("Usage limits"))
                    .child(div().flex_1().min_w_0().truncate().text_color(t.muted_foreground).child(description)),
            )
            .child(
                Button::new("usage-limits-dismiss")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::X))
                    .tooltip("Dismiss usage limits")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.composer_ui.usage_limits.close();
                        cx.notify();
                    })),
            );
        Some(
            div()
                .mb(px(-17.))
                .child(
                    v_flex()
                        .px(px(4.))
                        .pt(px(5.))
                        // 5px padding plus the 17px the composer covers.
                        .pb(px(22.))
                        .border_1()
                        .map(|el| if joined { el.border_t_0() } else { el.rounded_t(px(16.)) })
                        .border_color(outline)
                        .bg(t.card.opacity(t.glass))
                        .text_xs()
                        .child(header)
                        // `ComposerBanner.Scroll`: `max-h-[min(24rem,40dvh)]`.
                        .child(div().id("usage-limits-scroll").max_h(px(384.)).overflow_y_scroll().child(body)),
                )
                .into_any_element(),
        )
    }
}

/// `LimitWindows compact`: label and share left, the bar, then pace and reset, as three
/// columns so every row shares the widest label and reset text.
fn limit_windows(account: &ReportAccount, now: i64, hour12: bool, t: &Tokens) -> AnyElement {
    let kind = logic::driver_kind(&account.driver).unwrap_or_default();
    let color = crate::pull_requests::label_color(account.accent_color.as_deref())
        .map_or_else(|| provider_color(kind, t), |hex| rgb(hex).into());
    let windows = &account.limits.windows;
    let cell = || h_flex().h(px(24.)).min_w_0();
    let labels = windows.iter().map(|window| {
        cell()
            .gap(px(8.))
            .child(div().min_w_0().truncate().text_color(t.muted_foreground).child(window.label.clone()))
            .child(
                div()
                    .ml_auto()
                    .flex_none()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.foreground)
                    .child(format!("{}% left", window.remaining_percent())),
            )
    });
    let bars = windows.iter().map(|window| window_bar(&account.id, window, color, now, hour12, t));
    let ends = windows.iter().map(|window| {
        let pace = logic::pace_of(window, now);
        cell()
            .gap(px(8.))
            .text_color(t.muted_foreground)
            .children(pace.map(|pace| pace_icon(format!("limits-{}-{}", account.id, window.id), pace, t)))
            .child(div().ml_auto().flex_none().child(logic::format_resets_in(window, now).unwrap_or_default()))
    });
    h_flex()
        .w_full()
        .items_start()
        .gap(px(12.))
        .child(v_flex().max_w(px(144.)).min_w_0().flex_shrink(1.).gap(px(2.)).children(labels))
        .child(v_flex().flex_1().min_w(px(48.)).gap(px(2.)).children(bars))
        .child(v_flex().flex_none().gap(px(2.)).children(ends))
        .into_any_element()
}

/// `WindowBar`: what is left, with a tick where even spending would be, cut out of the bar.
fn window_bar(account: &str, window: &UsageWindow, color: Hsla, now: i64, hour12: bool, t: &Tokens) -> AnyElement {
    let remaining = window.remaining_percent();
    let time_left = logic::pace_remaining_percent(window, now);
    let resets_in = logic::format_resets_in(window, now);
    let resets_at = window.resets_at.as_deref().and_then(|at| crate::timestamp::upcoming_local(at, hour12));
    let left_line = match time_left {
        Some(left) => format!("{remaining}% left · {left}% of the window left"),
        None => format!("{remaining}% left"),
    };
    let reset_line = resets_at.map(|at| match &resets_in {
        Some(resets_in) => format!("Resets {at} · {resets_in}"),
        None => format!("Resets {at}"),
    });
    let (foreground, muted) = (t.foreground, t.muted_foreground);
    // The banner's own surface shows through the gap around the tick.
    let surface = t.background.blend(t.card.opacity(t.glass));
    div()
        .id(ElementId::Name(format!("usage-limits-bar-{account}-{}", window.id).into()))
        .relative()
        .h(px(24.))
        .child(div().absolute().left_0().right_0().top(px(6.)).bottom(px(6.)).rounded_full().bg(t.muted))
        .when(remaining > 0, |el| {
            el.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(6.))
                    .bottom(px(6.))
                    .w(relative(remaining as f32 / 100.))
                    .rounded_full()
                    .bg(color),
            )
        })
        .children(time_left.map(|left| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative((left as f32 / 100.).clamp(0.01, 0.99)))
                .ml(px(-4.))
                .w(px(8.))
                .flex()
                .justify_center()
                .py(px(4.))
                .child(div().absolute().top(px(6.)).bottom(px(6.)).left_0().right_0().bg(surface))
                .child(div().relative().w(px(2.)).h_full().rounded_full().bg(foreground))
        }))
        .tooltip(move |window, cx| {
            let (left_line, reset_line) = (left_line.clone(), reset_line.clone());
            Tooltip::element(move |_, _| {
                v_flex()
                    .gap(px(2.))
                    .child(div().text_color(foreground).child(left_line.clone()))
                    .when(time_left.is_some(), |el| {
                        el.child(div().text_color(muted).child("The line is where even spending would be."))
                    })
                    .children(reset_line.clone().map(|line| div().text_color(muted).child(line)))
            })
            .build(window, cx)
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui_kit::SharedString;

    use super::*;

    fn draft(project: &str) -> Target {
        Target::draft(SharedString::from("env"), project.into(), crate::app::draft_sessions::DraftId::for_test(project))
    }

    #[test]
    fn a_changed_key_closes_the_panel_for_good_and_clears_stay_on_their_thread() {
        let panel = UsageLimitsPanel::default();
        panel.open("a".into(), draft("p"), 1);
        assert_eq!(panel.current(Some("a")), Some(1));
        assert_eq!(panel.current(None), None, "a loading thread hides it");
        assert_eq!(panel.current(Some("a")), Some(1), "and shows it again once loaded");
        assert_eq!(panel.current(Some("b")), None, "a new run or request hides it");
        assert_eq!(panel.current(Some("a")), None, "and returning does not bring it back");
        panel.open("a".into(), draft("p"), 2);
        panel.clear_for(&draft("other"));
        assert_eq!(panel.current(Some("a")), Some(2), "another thread's send leaves it");
        assert_eq!(panel.with(1, |_| ()), None, "an older opening's result does not land");
        panel.clear_for(&draft("p"));
        assert_eq!(panel.current(Some("a")), None);
    }

    #[test]
    fn accounts_name_their_driver_and_a_distinct_instance() {
        let account = |instance: Option<&str>, name: Option<&str>| ReportAccount {
            id: "a".into(),
            driver: "codex".into(),
            label: "Hub · me".into(),
            plan: None,
            instance_id: instance.map(str::to_owned),
            display_name: name.map(str::to_owned),
            accent_color: None,
            reset_credit_input: None,
            limits: serde_json::from_value(serde_json::json!({})).unwrap(),
        };
        assert_eq!(account_label(&account(None, None)), "Hub · me");
        assert_eq!(account_label(&account(Some("codex"), None)), "Codex");
        assert_eq!(account_label(&account(Some("work"), Some(" "))), "Codex · work");
        assert_eq!(account_label(&account(Some("work"), Some("codex"))), "Codex");
    }
}
