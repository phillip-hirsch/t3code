//! The context window meter beside the send button (`ContextWindowMeter.tsx`),
//! shown when `contextWindowMeterEnabled` is on.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::Popup;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, ThemeStyled as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement, Keystroke, ParentElement as _,
    PathBuilder, SharedString, StatefulInteractiveElement as _, Styled as _, Task, canvas, div, point,
    prelude::FluentBuilder as _, px,
};
use serde_json::json;

use super::{Target, Workspace, new_id};
use crate::model::TokenUsage;
use crate::theme::Tokens;

/// The ring's 24-unit viewBox drawn at 20px: radius 9.75 and a 3-unit stroke.
const RING: f32 = 20.;
const RADIUS: f32 = 9.75 * RING / 24.;
const STROKE: f32 = 3. * RING / 24.;

/// `PopoverTrigger openOnHover delay={150}`.
const OPEN_DELAY: Duration = Duration::from_millis(150);
const TRIGGER: usize = 0;
const CARD: usize = 1;

/// The meter's popover (`Popover` with `openOnHover`): hovering opens it after a delay and
/// leaving closes it, while a click or Enter pins it open until it is dismissed.
#[derive(Default)]
pub(super) struct MeterPopup {
    open: bool,
    pinned: bool,
    /// The trigger and the card under the pointer.
    hovered: [bool; 2],
    timer: Option<Task<()>>,
}

impl MeterPopup {
    /// The pointer entered or left the trigger or the card; `Some` asks for the open state after `delay`.
    fn hover(&mut self, part: usize, hovering: bool, close_delay: Duration) -> Option<(bool, Duration)> {
        self.hovered[part] = hovering;
        self.timer = None;
        match (hovering, self.open) {
            (true, false) if part == TRIGGER => Some((true, OPEN_DELAY)),
            (false, true) if !self.pinned && !self.hovered.contains(&true) => Some((false, close_delay)),
            _ => None,
        }
    }

    /// A click or Enter: pins a hover-opened card, else toggles it.
    fn toggle(&mut self) {
        self.timer = None;
        if self.open && !self.pinned {
            self.pinned = true;
        } else {
            self.open = !self.open;
            self.pinned = self.open;
        }
    }

    fn close(&mut self) {
        self.timer = None;
        self.open = false;
        self.pinned = false;
    }

    /// A delayed hover change lands unless the pointer moved on or a click pinned the card.
    fn settle(&mut self, open: bool) {
        self.timer = None;
        let hovered = self.hovered.contains(&true);
        if (open && hovered) || (!open && !hovered && !self.pinned) {
            self.open = open;
        }
    }
}

/// `formatPercentage`: one decimal under 10%, whole numbers above.
fn format_percentage(value: f64) -> String {
    if value < 10. {
        // `toFixed` rounds ties up, where `{:.1}` rounds them to even.
        let text = format!("{:.1}", (value * 10.).round() / 10.);
        format!("{}%", text.strip_suffix(".0").unwrap_or(&text))
    } else {
        format!("{}%", value.round())
    }
}

/// `deriveLatestContextWindowSnapshot`'s used share, capped at 100.
fn used_percentage(usage: &TokenUsage) -> Option<f64> {
    let max = usage.max_tokens.filter(|max| *max > 0)?;
    Some((usage.used_tokens as f64 / max as f64 * 100.).min(100.))
}

/// `formatContextWindowCompactionMessage`.
fn compaction_message(model: Option<&str>, threshold: Option<u64>) -> String {
    match (threshold.filter(|t| *t > 0), model) {
        (Some(threshold), _) => {
            format!("Compacts automatically at about {} tokens.", super::git::group_thousands(threshold))
        }
        (None, Some(model)) => format!("Context for {model} compacts automatically when needed."),
        (None, None) => "Context compacts automatically when needed.".into(),
    }
}

/// `formatContextWindowCost`: four decimals for amounts under a cent.
fn format_cost(amount: f64, currency: &str) -> String {
    let digits = if amount != 0. && amount.abs() < 0.01 { 4 } else { 2 };
    format!("{currency} {amount:.digits$}")
}

impl Workspace {
    /// The meter, a 28px placeholder while the thread loads, or nothing.
    pub(super) fn render_context_meter(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.client_settings.values.bool("contextWindowMeterEnabled")
            || !matches!(self.target, Some(Target::Thread { .. }))
        {
            return None;
        }
        let provider = self.composer_provider();
        let Some(projection) = self.open_projection() else {
            // `shouldReserveContextWindowMeter`: keep the slot while the thread loads.
            let reserve = provider.is_none_or(|p| p.reports_context_window != Some(false));
            return reserve.then(|| div().size(px(28.)).flex_none().into_any_element());
        };
        let (usage, live) = projection.context_window()?;
        let usage = usage.into_owned();
        let percentage = used_percentage(&usage);
        #[expect(clippy::cast_possible_truncation, reason = "a percentage clamped to 0..=100")]
        let shown = percentage.unwrap_or(0.).clamp(0., 100.) as f32;
        let color = if shown > 90. { t.error } else { t.muted_foreground.opacity(0.72) };
        let track = t.muted_foreground.opacity(0.24);
        let model = projection
            .thread
            .model_selection
            .as_ref()
            .and_then(|s| {
                let provider = self.server_info().provider(&s.instance_id)?;
                let model = provider.models.iter().find(|m| m.slug == s.model)?;
                Some(crate::model_label::display_model_name(model))
            })
            .or_else(|| projection.thread.model_selection.as_ref().map(|s| s.model.clone()));
        let compactable = provider.is_some_and(|p| p.slash_commands.iter().any(|c| c.name == "compact"));
        let compact_blocked = !compactable
            || projection.is_running()
            || self.sending
            || self.target_rewinding()
            || !projection.pending_approvals().is_empty()
            || !projection.pending_user_inputs().is_empty()
            || self.has_plan_follow_up()
            || !has_compactable_conversation(projection);
        let view = cx.entity();
        let tokens = *t;
        let close_delay = Duration::from_millis(if compactable { 150 } else { 0 });
        let trigger = div()
            .id("context-meter-trigger")
            .size(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .hover(|s| s.bg(tokens.accent))
            .when(self.composer_ui.meter.open, |el| el.bg(tokens.accent))
            .child(ring(shown, track, color))
            .on_hover(
                cx.listener(move |this, hovering: &bool, _, cx| this.hover_meter(TRIGGER, *hovering, close_delay, cx)),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.composer_ui.meter.toggle();
                cx.notify();
            }));
        let trigger = super::composer::action_control(trigger, &self.composer_action_focus[super::composer::METER], t);
        let popup = Popup::new("context-meter", trigger).anchor(gpui_kit::Anchor::BottomRight);
        if !self.composer_ui.meter.open {
            return Some(popup.into_any_element());
        }
        let card = {
            let t = &tokens;
            let used = super::details::format_context_tokens(usage.used_tokens);
            let header_value = match (usage.max_tokens, percentage) {
                (Some(max), Some(percentage)) => {
                    format!("{} · {used}/{}", format_percentage(percentage), super::details::format_context_tokens(max))
                }
                _ => used,
            };
            let detail_row = |label: &'static str, value: String| {
                h_flex()
                    .justify_between()
                    .gap(px(12.))
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(t.muted_foreground)
                    .child(label)
                    .child(div().font_weight(FontWeight::MEDIUM).child(value))
            };
            let total = usage.total_processed_tokens.filter(|total| *total > 0 && !live);
            let compacts = live || usage.compacts_automatically == Some(true);
            v_flex()
                .w(px(256.))
                .gap(px(8.))
                .child(
                    h_flex()
                        .justify_between()
                        .gap(px(12.))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.muted_foreground)
                                .child("Context Window"),
                        )
                        .child(div().text_size(px(11.)).text_color(t.muted_foreground).child(header_value)),
                )
                .when(usage.max_tokens.is_some(), |el| {
                    el.child(
                        div()
                            .h(px(6.))
                            .w_full()
                            .rounded_full()
                            .bg(t.muted.opacity(0.6))
                            .child(div().h_full().w(gpui_kit::relative(shown / 100.)).rounded_full().bg(color)),
                    )
                })
                .children(
                    total.map(|total| detail_row("Total processed", super::details::format_context_tokens(total))),
                )
                .children(
                    usage
                        .cost
                        .as_ref()
                        .filter(|_| !live)
                        .map(|cost| detail_row("Cost", format_cost(cost.amount, &cost.currency))),
                )
                .when(compacts, |el| {
                    el.child(
                        div()
                            .mt(px(4.))
                            .text_size(px(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.muted_foreground)
                            .child(compaction_message(model.as_deref(), usage.auto_compact_threshold)),
                    )
                })
                .when(compactable, |el| {
                    let view = view.clone();
                    el.child(
                        Button::new("compact-context")
                            .outline()
                            .xsmall()
                            .w_full()
                            .mt(px(4.))
                            .icon(Icon::new(Lucide::Minimize2))
                            .label("Compact context")
                            .disabled(compact_blocked)
                            .on_click(move |_, _, cx| view.update(cx, super::Workspace::compact_context)),
                    )
                    .when(compact_blocked, |el| {
                        el.child(
                            div()
                                .text_size(px(11.))
                                .text_color(t.muted_foreground)
                                .child(SharedString::from("Compacting is unavailable right now")),
                        )
                    })
                })
        };
        let content = v_flex()
            .id("context-meter-card")
            .occlude()
            .popover_style(cx)
            .p_3()
            .bottom_1()
            .overflow_hidden()
            .child(card)
            .on_hover(
                cx.listener(move |this, hovering: &bool, _, cx| this.hover_meter(CARD, *hovering, close_delay, cx)),
            )
            // The trigger's own click toggles it.
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                if !this.composer_ui.meter.hovered[TRIGGER] {
                    this.composer_ui.meter.close();
                    cx.notify();
                }
            }));
        Some(popup.content(content).into_any_element())
    }

    fn hover_meter(&mut self, part: usize, hovering: bool, close_delay: Duration, cx: &mut Context<Self>) {
        let Some((open, delay)) = self.composer_ui.meter.hover(part, hovering, close_delay) else { return };
        if delay.is_zero() {
            self.composer_ui.meter.settle(open);
            cx.notify();
            return;
        }
        self.composer_ui.meter.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.composer_ui.meter.settle(open);
                cx.notify();
            });
        }));
    }

    /// Escape closes the open card.
    pub(super) fn context_meter_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if keystroke.key != "escape" || !self.composer_ui.meter.open {
            return false;
        }
        self.composer_ui.meter.close();
        cx.notify();
        true
    }

    /// `sendStandaloneCommand("/compact")`, after the thread takes the composer's mode.
    fn compact_context(&mut self, cx: &mut Context<Self>) {
        let Some(Target::Thread { id: thread_id, .. }) = self.target.clone() else { return };
        let (_, mode) = self.interaction_mode();
        let mut commands: Vec<_> = self.interaction_mode_command(&thread_id, mode).into_iter().collect();
        let mut message = json!({
            "type": "message.dispatch",
            "commandId": new_id(),
            "createdBy": "user",
            "creationSource": "web",
            "threadId": thread_id,
            "messageId": new_id(),
            "text": "/compact",
            "attachments": [],
            "deliveryIntent": "auto",
            "dispatchMode": {"type": "start_immediately"},
        });
        if let Some(model) = &self.model_override {
            message["modelSelection"] = model.to_json();
        }
        commands.push(message);
        let Some(client) = self.client() else { return };
        self.sending = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let mut result = Ok(());
            for command in commands {
                result = client.request("orchestration.dispatchCommand", command).await.map(|_| ());
                if result.is_err() {
                    break;
                }
            }
            let _ = this.update(cx, |this, cx| {
                this.sending = false;
                if let Err(error) = result {
                    log::warn!("compact failed: {error}");
                    this.error = Some("Failed to compact context.".into());
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// A conversation worth compacting: a user message other than a bare `/compact`.
fn has_compactable_conversation(projection: &crate::model::ThreadProjection) -> bool {
    projection.visible_turn_items.iter().map(|row| &row.item).any(|item| {
        item.item_type == "user_message"
            && (item.str("text").is_some_and(|text| !text.trim().eq_ignore_ascii_case("/compact"))
                || item.raw.get("attachments").and_then(|a| a.as_array()).is_some_and(|a| !a.is_empty()))
    })
}

/// The track and the used arc, clockwise from 12 o'clock with round ends.
fn ring(percentage: f32, track: Hsla, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let center = bounds.center();
            let at = |angle: f32| point(center.x + px(RADIUS * angle.cos()), center.y + px(RADIUS * angle.sin()));
            let arc = |from: f32, sweep: f32| {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a small, positive step count"
                )]
                let steps = ((sweep / TAU) * 64.).ceil().max(1.) as usize;
                let mut path = PathBuilder::stroke(px(STROKE));
                path.move_to(at(from));
                for step in 1..=steps {
                    path.line_to(at(from + sweep * step as f32 / steps as f32));
                }
                path.build()
            };
            if let Ok(path) = arc(0., TAU) {
                window.paint_path(path, track);
            }
            if percentage <= 0. {
                return;
            }
            let start = -FRAC_PI_2;
            let sweep = TAU * percentage / 100.;
            if let Ok(path) = arc(start, sweep) {
                window.paint_path(path, color);
            }
            for angle in [start, start + sweep] {
                let mut cap = PathBuilder::fill();
                let end = at(angle);
                let r = px(STROKE / 2.);
                cap.move_to(point(end.x - r, end.y));
                cap.arc_to(point(r, r), px(0.), false, true, point(end.x + r, end.y));
                cap.arc_to(point(r, r), px(0.), false, true, point(end.x - r, end.y));
                cap.close();
                if let Ok(path) = cap.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .size(px(RING))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_meter_card_opens_on_hover_and_a_click_pins_it() {
        let close = Duration::from_millis(150);
        let mut meter = MeterPopup::default();
        assert_eq!(meter.hover(TRIGGER, true, close), Some((true, OPEN_DELAY)));
        meter.settle(true);
        assert!(meter.open);
        assert_eq!(meter.hover(TRIGGER, false, close), Some((false, close)));
        assert_eq!(meter.hover(CARD, true, close), None, "moving onto the card keeps it");
        assert_eq!(meter.hover(CARD, false, close), Some((false, close)));
        meter.settle(false);
        assert!(!meter.open);

        meter.hover(TRIGGER, true, close);
        meter.settle(true);
        meter.toggle();
        assert!(meter.open && meter.pinned, "a click pins a hovered card");
        assert_eq!(meter.hover(TRIGGER, false, close), None, "leaving a pinned card keeps it");
        meter.toggle();
        assert!(!meter.open, "a second click closes it");

        meter.toggle();
        assert!(meter.open && meter.pinned, "Enter opens it without a hover");
        meter.settle(false);
        assert!(meter.open, "a stale hover close does not undo a pin");
        meter.close();
        assert!(!meter.open && !meter.pinned);
    }

    #[test]
    fn percentages_keep_a_decimal_only_below_ten() {
        assert_eq!(format_percentage(0.), "0%");
        assert_eq!(format_percentage(4.25), "4.3%");
        assert_eq!(format_percentage(9.0), "9%");
        assert_eq!(format_percentage(42.6), "43%");
    }

    #[test]
    fn used_share_needs_a_known_window_and_caps_at_full() {
        let usage = |used: u64, max: Option<u64>| -> TokenUsage {
            serde_json::from_value(json!({"usedTokens": used, "maxTokens": max})).unwrap()
        };
        assert_eq!(used_percentage(&usage(50, Some(200))), Some(25.));
        assert_eq!(used_percentage(&usage(500, Some(200))), Some(100.));
        assert_eq!(used_percentage(&usage(50, None)), None);
        assert_eq!(used_percentage(&usage(50, Some(0))), None);
    }

    #[test]
    fn compaction_and_cost_copy_match_the_web() {
        assert_eq!(compaction_message(Some("Opus"), Some(160_000)), "Compacts automatically at about 160,000 tokens.");
        assert_eq!(compaction_message(Some("Opus"), None), "Context for Opus compacts automatically when needed.");
        assert_eq!(compaction_message(None, Some(0)), "Context compacts automatically when needed.");
        assert_eq!(format_cost(1.5, "USD"), "USD 1.50");
        assert_eq!(format_cost(0.004, "USD"), "USD 0.0040");
        assert_eq!(format_cost(0., "USD"), "USD 0.00");
    }
}
