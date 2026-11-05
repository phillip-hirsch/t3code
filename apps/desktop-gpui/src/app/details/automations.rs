//! The details card's Automations section (`ThreadAutomationsPanel.tsx`): scheduled tasks
//! bound to the thread, from `scheduledTasks.subscribe`, with run now, pause/resume, edit
//! and a link to Settings › Scheduled tasks.

use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px, rgb,
};
use serde_json::json;

use super::{CardColors, Section, row, section_heading};
use crate::app::pages::settings::agents::scheduled_tasks::logic::{
    Schedule, ScheduledTask, decode_tasks, relative_label, schedule_label,
};
use crate::app::{Target, Workspace};
use crate::rpc::StreamEvent;
use crate::theme::Tokens;

const TASK_ROW_HEIGHT: f32 = 48.;
const HEADING_HEIGHT: f32 = 36.;
/// `max-h-[13.5rem]`.
const LIST_MAX_HEIGHT: f32 = 216.;
const SECTION: &str = "scheduled-tasks";

#[derive(Default)]
pub(in crate::app) struct AutomationsWatch {
    /// The environment the tasks are scheduled on.
    environment: Option<SharedString>,
    tasks: Rc<Vec<ScheduledTask>>,
    error: Option<SharedString>,
    task: Option<Task<()>>,
    busy: Option<String>,
}

/// `taskScheduleLine`.
fn schedule_line(task: &ScheduledTask, now_ms: i64) -> String {
    let label = schedule_label(&task.schedule(), &chrono::Local);
    let state = match (&task.blocked_by_policy, task.enabled, &task.next_run_at) {
        (Some(reason), ..) => format!(" · {reason}"),
        (None, true, Some(next)) => format!(" · next {}", relative_label(Some(next), now_ms)),
        (None, true, None) => String::new(),
        (None, false, _) => " · paused".into(),
    };
    format!("{label}{state}")
}

impl Workspace {
    /// The connected environment whose tasks the section shows, while it can show.
    fn automations_wanted(&self, window: &Window) -> Option<SharedString> {
        let shown = matches!(self.target, Some(Target::Thread { .. }))
            && self.details_shown(window)
            && self.details_item_shown("automations");
        let environment = self.active_environment().filter(|_| shown)?;
        self.env_client(environment).is_some().then(|| environment.clone())
    }

    /// Starts or drops the subscription as the section and its environment come and go; called each render.
    pub(in crate::app) fn sync_automations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.automations_wanted(window);
        if wanted == self.automations.environment && (wanted.is_none() || self.automations.task.is_some()) {
            return;
        }
        if wanted != self.automations.environment {
            self.automations = AutomationsWatch { environment: wanted.clone(), ..AutomationsWatch::default() };
        }
        let Some(client) = wanted.as_deref().and_then(|environment| self.env_client(environment)) else { return };
        let stream = client.stream("scheduledTasks.subscribe", json!({}));
        self.automations.task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = matches!(event, StreamEvent::End(_));
                let applied = this.update(cx, |this, cx| {
                    match event {
                        StreamEvent::Item(value) => {
                            this.automations.tasks = Rc::new(decode_tasks(&value));
                            this.automations.error = None;
                        }
                        StreamEvent::End(Err(error)) => this.automations.error = Some(error.to_string().into()),
                        StreamEvent::End(Ok(())) => {}
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
                if ended {
                    break;
                }
            }
            super::resubscribe_after_backoff(&this, cx, |this| this.automations.task = None).await;
        }));
    }

    fn automation_action(&mut self, id: String, enabled: Option<bool>, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.automations.environment.as_deref().and_then(|environment| self.env_client(environment));
        let Some(client) = client else { return };
        if self.automations.busy.is_some() {
            return;
        }
        let (method, payload, failure) = match enabled {
            Some(enabled) => {
                ("scheduledTasks.setEnabled", json!({"id": id, "enabled": enabled}), "Could not update automation")
            }
            None => ("scheduledTasks.runNow", json!({"id": id}), "Could not run automation"),
        };
        self.automations.busy = Some(id);
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(method, payload).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.automations.busy = None;
                if let Err(error) = result {
                    window.push_notification(Notification::error(error.to_string()).title(failure), cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Hidden while no task is bound to the thread and the list loaded fine.
    pub(super) fn automations_section(
        &self,
        tokens: &Tokens,
        colors: &CardColors,
        cx: &mut Context<Self>,
    ) -> Option<Section> {
        let Some(Target::Thread { id: thread_id, .. }) = &self.target else { return None };
        let tasks: Vec<&ScheduledTask> =
            self.automations.tasks.iter().filter(|t| t.thread_id.as_ref() == Some(thread_id)).collect();
        let error = self.automations.error.clone();
        if tasks.is_empty() && error.is_none() {
            return None;
        }
        let now = crate::app::sidebar::now_ms();
        let busy = self.automations.busy.is_some();
        let icon_button = |id: String, icon: Lucide, tip: &'static str| {
            div()
                .id(ElementId::Name(id.into()))
                .size(px(24.))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .text_color(tokens.muted_foreground)
                .hover(|s| s.bg(colors.hover).text_color(tokens.foreground))
                .child(Icon::new(icon).with_size(px(14.)))
                .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        };
        let rows: Vec<AnyElement> = tasks
            .iter()
            .map(|task| {
                let dot = match task.last_run_status.as_str() {
                    "running" => rgb(0x0ea5e9).into(),
                    "succeeded" => rgb(0x10b981).into(),
                    "failed" => tokens.error,
                    _ => tokens.muted_foreground.opacity(0.4),
                };
                let line = schedule_line(task, now);
                let (run_id, toggle_id) = (task.id.clone(), task.id.clone());
                let enabled = task.enabled;
                let running = task.last_run_status == "running";
                row(colors)
                    .h(px(TASK_ROW_HEIGHT))
                    .gap(px(8.))
                    .child(
                        div()
                            .relative()
                            .size(px(16.))
                            .flex_none()
                            .child(
                                Icon::new(Lucide::CalendarClock).with_size(px(16.)).text_color(tokens.muted_foreground),
                            )
                            .child(div().absolute().top(px(-4.)).right(px(-4.)).size(px(6.)).rounded_full().bg(dot)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .text_color(tokens.foreground.opacity(0.8))
                                    .child(task.title.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.))
                                    .font_weight(gpui_kit::FontWeight::NORMAL)
                                    .text_color(tokens.muted_foreground)
                                    .child(line),
                            ),
                    )
                    .child(
                        icon_button(format!("automation-edit-{}", task.id), Lucide::Pencil, "Edit automation")
                            .on_click(cx.listener(|this, _, _, cx| this.open_settings(SECTION, cx))),
                    )
                    .when(!matches!(task.schedule(), Schedule::Webhook), |el| {
                        el.child(
                            icon_button(format!("automation-run-{}", task.id), Lucide::Play, "Run now")
                                .when(busy || running, |el| el.opacity(0.5))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if !running {
                                        this.automation_action(run_id.clone(), None, window, cx);
                                    }
                                })),
                        )
                    })
                    .child(
                        Switch::new(ElementId::Name(format!("automation-enabled-{}", task.id).into()))
                            .checked(enabled)
                            .small()
                            .disabled(busy)
                            .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                                this.automation_action(toggle_id.clone(), Some(*checked), window, cx);
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        let list_height = (tasks.len() as f32 * TASK_ROW_HEIGHT).min(LIST_MAX_HEIGHT);
        let error_row = error.map(|error| {
            div()
                .px(px(10.))
                .py(px(6.))
                .text_size(px(11.))
                .text_color(tokens.error)
                .child(format!("Could not load automations: {error}"))
        });
        let height = HEADING_HEIGHT + list_height + if error_row.is_some() { 28. } else { 0. };
        let heading = section_heading("Automations".into(), tokens).child(
            icon_button("automations-manage".into(), Lucide::Settings2, "Manage scheduled tasks")
                .on_click(cx.listener(|this, _, _, cx| this.open_settings(SECTION, cx))),
        );
        Some(Section {
            element: v_flex()
                .child(heading)
                .child(v_flex().id("automations-list").max_h(px(LIST_MAX_HEIGHT)).overflow_y_scroll().children(rows))
                .children(error_row)
                .into_any_element(),
            height,
            min_density: super::Density::Full,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(value: serde_json::Value) -> ScheduledTask {
        decode_tasks(&json!({ "tasks": [value] })).remove(0)
    }

    #[test]
    fn schedule_lines_follow_the_web() {
        let base = json!({
            "id": "a", "title": "Nightly", "prompt": "p", "enabled": true,
            "schedule": {"type": "webhook"}, "projectId": "p", "workspaceStrategy": {},
            "modelSelection": {}, "runtimeMode": "full-access", "interactionMode": "default",
            "lastRunStatus": "never",
        });
        let mut paused = base.clone();
        paused["enabled"] = false.into();
        assert!(schedule_line(&task(paused), 0).ends_with(" · paused"));
        let mut blocked = base.clone();
        blocked["blockedByPolicy"] = "Blocked by policy".into();
        assert!(schedule_line(&task(blocked), 0).ends_with(" · Blocked by policy"));
        let mut next = base;
        next["nextRunAt"] = "1970-01-01T00:10:00Z".into();
        assert!(schedule_line(&task(next), 0).contains(" · next in "));
    }
}
