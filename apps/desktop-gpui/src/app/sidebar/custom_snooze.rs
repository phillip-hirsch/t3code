//! The "Custom snooze" dialog (`apps/web/src/components/CustomSnoozeDialog.tsx`).
//! The web's calendar popover and native time field are plain date and time
//! inputs here; validation and the wake time match `resolveCustomSnooze`.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString, Styled as _, WeakEntity,
    Window, div, prelude::FluentBuilder as _, px,
};

use crate::app::Workspace;
use crate::inbox::{self, CustomSnooze, SnoozeUnit};

pub(super) struct CustomSnoozeForm {
    workspace: WeakEntity<Workspace>,
    thread_id: String,
    duration: bool,
    date: Entity<InputState>,
    time: Entity<InputState>,
    amount: Entity<InputState>,
    unit: SnoozeUnit,
    error: Option<&'static str>,
}

impl CustomSnoozeForm {
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = if self.duration {
            CustomSnooze::Duration { amount: self.amount.read(cx).value().to_string(), unit: self.unit }
        } else {
            CustomSnooze::Date {
                date: self.date.read(cx).value().to_string(),
                time: self.time.read(cx).value().to_string(),
            }
        };
        match inbox::resolve_custom_snooze(&input, &chrono::Local::now()) {
            Ok(wake) => {
                let thread_id = self.thread_id.clone();
                let _ = self.workspace.update(cx, |this, cx| this.snooze_thread(&thread_id, wake, window, cx));
                window.close_dialog(cx);
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }
}

impl Render for CustomSnoozeForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *crate::theme::tokens(cx);
        let toggle = |id: &'static str, label: &'static str, on: bool, duration: bool, cx: &mut Context<Self>| {
            Button::new(id).label(label).small().flex_1().selected(on).when(!on, |b| b.ghost()).on_click(cx.listener(
                move |this, _, _, cx| {
                    this.duration = duration;
                    this.error = None;
                    cx.notify();
                },
            ))
        };
        let field = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .flex_1()
                .gap(px(6.))
                .child(div().text_sm().font_weight(gpui_kit::FontWeight::MEDIUM).child(label))
                .child(Input::new(input))
        };
        let unit = |id: &'static str, unit: SnoozeUnit, current: SnoozeUnit, cx: &mut Context<Self>| {
            Button::new(id)
                .label(unit.label())
                .small()
                .selected(unit == current)
                .when(unit != current, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.unit = unit;
                    this.error = None;
                    cx.notify();
                }))
        };
        v_flex()
            .gap(px(16.))
            .child(
                div()
                    .text_sm()
                    .text_color(tokens.muted_foreground)
                    .child("Choose when snoozed threads return to your inbox."),
            )
            .child(
                h_flex()
                    .gap(px(4.))
                    .child(toggle("custom-snooze-date-mode", "Date and time", !self.duration, false, cx))
                    .child(toggle("custom-snooze-duration-mode", "Duration", self.duration, true, cx)),
            )
            .map(|el| {
                if self.duration {
                    el.child(
                        h_flex().gap(px(12.)).items_end().child(field("Snooze for", &self.amount)).child(
                            v_flex()
                                .gap(px(6.))
                                .child(div().text_sm().font_weight(gpui_kit::FontWeight::MEDIUM).child("Unit"))
                                .child(
                                    h_flex()
                                        .gap(px(4.))
                                        .child(unit("custom-snooze-minutes", SnoozeUnit::Minutes, self.unit, cx))
                                        .child(unit("custom-snooze-hours", SnoozeUnit::Hours, self.unit, cx))
                                        .child(unit("custom-snooze-days", SnoozeUnit::Days, self.unit, cx)),
                                ),
                        ),
                    )
                } else {
                    el.child(h_flex().gap(px(12.)).child(field("Date", &self.date)).child(field("Time", &self.time)))
                }
            })
            .children(self.error.map(|error| div().text_sm().text_color(tokens.error_foreground).child(error)))
            .child(
                h_flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        div().mr_auto().child(
                            Button::new("custom-snooze-options")
                                .label("Edit snooze options")
                                .icon(gpui_kit::assets::IconName::Settings2)
                                .ghost()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = this
                                        .workspace
                                        .update(cx, |workspace, cx| workspace.open_settings("threads", cx));
                                })),
                        ),
                    )
                    .child(
                        Button::new("custom-snooze-cancel")
                            .label("Cancel")
                            .outline()
                            .on_click(cx.listener(|_, _, window, cx| window.close_dialog(cx))),
                    )
                    .child(
                        Button::new("custom-snooze-submit")
                            .label("Snooze")
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

impl Workspace {
    /// Opens the dialog, seeded with now + 1 h (date mode) and 2 hours (duration mode).
    pub(super) fn open_custom_snooze(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let seed = chrono::Local::now() + chrono::Duration::hours(1);
        let text_input = |value: String, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(value, window, cx);
                state
            })
        };
        let date = text_input(seed.format("%Y-%m-%d").to_string(), "YYYY-MM-DD", window, cx);
        let time = text_input(seed.format("%H:%M").to_string(), "HH:MM", window, cx);
        let amount = text_input("2".into(), "2", window, cx);
        let workspace = cx.weak_entity();
        let form = cx.new(|_| CustomSnoozeForm {
            workspace,
            thread_id: thread_id.to_owned(),
            duration: false,
            date,
            time,
            amount,
            unit: SnoozeUnit::Hours,
            error: None,
        });
        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title(SharedString::from("Custom snooze")).width(px(384.)).child(form.clone())
        });
    }
}
