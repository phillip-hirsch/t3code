//! Settings > Scheduled tasks (`apps/web/src/components/settings/ScheduledTasksSettings.tsx`).
//!
//! The native client is connected to one environment, so this is the web's single-environment
//! path (`ScheduledTaskEnvironmentSection`); the web's multi-environment list, scope filtering
//! (`matchesScheduledTaskScope`, everything matches the global scope) and `?taskId=` deep links
//! do not apply yet. The section has no plain setting rows: every control is RPC-backed.
//! `section`, `group` and `row` below are placeholders for the foundation's settings widgets.

mod editor;
pub(in crate::app) mod logic;
mod webhook_logic;
mod webhooks;
mod worktree_mode;

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FontWeight, Hsla, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};
use serde_json::json;

use self::logic::{ScheduledTask, decode_tasks, relative_label, schedule_label};
use super::super::widgets::{RowFrame, section_with_action};
use crate::app::Workspace;
use crate::rpc::{RpcClient, RpcError, StreamEvent};
use crate::theme::{self, Tokens};

const SECTION: &str = "scheduled-tasks";

/// The section body: one retained view, so its subscription outlives a frame.
pub(super) fn render(workspace: &mut Workspace, _: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let view = workspace
        .scheduled_tasks_view
        .get_or_insert_with(|| {
            let owner = cx.entity();
            AnyView::from(cx.new(|cx| ScheduledTasksView::new(owner, cx)))
        })
        .clone();
    let settings = workspace.settings_view();
    // `scope.kind === "unavailable"`: the selection names a project or machine that is gone.
    if let crate::settings::scope::ScopeKind::Unavailable { message } = settings.scope.kind {
        let t = *crate::theme::tokens(cx);
        let row = RowFrame::new(message).render(&t).into_any_element();
        let section = section_with_action("scheduled-tasks-unavailable", "Unavailable selection", None, [row], &t);
        return workspace.settings_page("scheduled-tasks", &settings, [section], cx);
    }
    workspace.settings_page("scheduled-tasks", &settings, [view.into_any_element()], cx)
}

enum Load {
    Loading,
    Loaded(Rc<Vec<ScheduledTask>>),
    Failed(SharedString),
}

#[derive(Clone, Copy)]
enum Action {
    SetEnabled(bool),
    RunNow,
    Delete,
}

pub(super) struct ScheduledTasksView {
    workspace: WeakEntity<Workspace>,
    tasks: Load,
    /// `scheduledTasks.subscribe`, only while the section is on screen and connected.
    stream: Option<Task<()>>,
    subscribed: bool,
    /// Repaints relative times once a minute (`useNowMinute`).
    _minute: Option<Task<()>>,
    /// Tasks with a toggle, run or delete in flight.
    busy: HashSet<String>,
    _observe: Subscription,
}

impl ScheduledTasksView {
    fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        // Unsubscribe when Settings leaves this section or the socket drops.
        let observe = cx.observe(&workspace, |this, workspace, cx| {
            let workspace = workspace.read(cx);
            let shown = workspace.settings.as_deref() == Some(SECTION) && workspace.client().is_some();
            if !shown && (this.subscribed || this.stream.is_some()) {
                this.stop();
            }
        });
        Self {
            workspace: workspace.downgrade(),
            tasks: Load::Loading,
            stream: None,
            subscribed: false,
            _minute: None,
            busy: HashSet::new(),
            _observe: observe,
        }
    }

    fn stop(&mut self) {
        self.stream = None;
        self._minute = None;
        self.subscribed = false;
        self.tasks = Load::Loading;
    }

    fn subscribe(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        self.subscribed = true;
        let stream = client.stream("scheduledTasks.subscribe", json!({}));
        self.stream = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = matches!(event, StreamEvent::End(_));
                let applied = this.update(cx, |this, cx| {
                    match event {
                        StreamEvent::Item(value) => this.tasks = Load::Loaded(Rc::new(decode_tasks(&value))),
                        // A dropped socket shows as disconnected; the next connection resubscribes.
                        StreamEvent::End(Ok(()) | Err(RpcError::Disconnected(_))) => {
                            this.subscribed = false;
                            this.tasks = Load::Loading;
                        }
                        StreamEvent::End(Err(error)) => {
                            this.subscribed = false;
                            this.tasks = Load::Failed(error_message(&error).into());
                        }
                    }
                    cx.notify();
                });
                if ended || applied.is_err() {
                    return;
                }
            }
        }));
        self._minute = Some(cx.spawn(async move |this, cx| {
            loop {
                let into_minute = crate::app::sidebar::now_ms().rem_euclid(60_000) as u64;
                cx.background_executor().timer(Duration::from_millis(60_000 - into_minute)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
    }

    fn act(&mut self, task: &ScheduledTask, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.workspace.upgrade().and_then(|w| w.read(cx).client()) else { return };
        if !self.busy.insert(task.id.clone()) {
            return;
        }
        let id = task.id.clone();
        let (method, payload) = match action {
            Action::SetEnabled(enabled) => ("scheduledTasks.setEnabled", json!({"id": id, "enabled": enabled})),
            Action::RunNow => ("scheduledTasks.runNow", json!({"id": id})),
            Action::Delete => ("scheduledTasks.delete", json!({"id": id})),
        };
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request(method, payload).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy.remove(&id);
                if let Err(error) = result {
                    notify_error(window, cx, "Could not update scheduled task", error_message(&error));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_editor(&mut self, task: Option<ScheduledTask>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else { return };
        let Some(client) = workspace.read(cx).client() else { return };
        editor::open(workspace.downgrade(), cx.entity().downgrade(), client, task, window, cx);
    }

    /// The loaded list, for the editor's "task no longer exists" check and live webhook state.
    fn loaded_tasks(&self) -> Option<Rc<Vec<ScheduledTask>>> {
        match &self.tasks {
            Load::Loaded(tasks) => Some(tasks.clone()),
            _ => None,
        }
    }

    fn render_row(&self, task: &ScheduledTask, client: &RpcClient, t: &Tokens, cx: &mut Context<Self>) -> RowFrame {
        let busy = self.busy.contains(&task.id);
        let schedule = task.schedule();
        let webhook = schedule == logic::Schedule::Webhook;
        let when: SharedString = match (&task.blocked_by_policy, task.enabled, webhook, &task.next_run_at) {
            (Some(reason), ..) => reason.clone().into(),
            (None, false, ..) => "Paused".into(),
            (None, true, true, _) => "Listening".into(),
            (None, true, false, Some(next)) => {
                format!("Next run {}", relative_label(Some(next), crate::app::sidebar::now_ms())).into()
            }
            (None, true, false, None) => "Not scheduled".into(),
        };
        let status = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.))
            .child(format!("{} · {when}", schedule_label(&schedule, &chrono::Local)))
            .when(task.last_run_status != "never" && !task.last_run_status.is_empty(), |this| {
                this.child(badge(&task.last_run_status, t))
            })
            .children(task.last_run_error.clone().map(|error| div().text_color(t.error_foreground).child(error)));

        let id = task.id.clone();
        let view = cx.entity();
        let menu_task = Rc::new(task.clone());
        let client = client.clone();
        let context = self.workspace.upgrade().map(|w| webhooks::WebhookContext::new(client.clone(), &w, cx));
        let menu = Button::new(SharedString::from(format!("scheduled-task-actions-{id}")))
            .ghost()
            .small()
            .icon(Icon::new(Lucide::Ellipsis))
            .disabled(busy)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                let task = menu_task.clone();
                let edit = {
                    let (view, task) = (view.clone(), task.clone());
                    // After the menu dismisses itself, which would otherwise take the new dialog's focus with it.
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        let (view, task) = (view.clone(), task.clone());
                        window.defer(cx, move |window, cx| {
                            view.update(cx, |this, cx| this.open_editor(Some((*task).clone()), window, cx));
                        });
                    }
                };
                let act = |action: Action| {
                    let (view, task) = (view.clone(), task.clone());
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        view.update(cx, |this, cx| this.act(&task, action, window, cx));
                    }
                };
                let mut menu = menu.item(PopupMenuItem::new("Edit").icon(Icon::new(Lucide::Pencil)).on_click(edit));
                menu = if webhook {
                    match &context {
                        Some(context) => webhooks::webhook_menu_item(menu, context, &task.to_json()),
                        None => menu,
                    }
                } else {
                    menu.item(PopupMenuItem::new("Run now").icon(Icon::new(Lucide::Play)).on_click(act(Action::RunNow)))
                };
                menu.separator().item(
                    PopupMenuItem::new("Delete")
                        .icon(Icon::empty().path("icons/trash-2.svg"))
                        .on_click(act(Action::Delete)),
                )
            });

        let enabled = task.enabled;
        let toggle_task = task.clone();
        let control = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                Switch::new(SharedString::from(format!("scheduled-task-enabled-{id}")))
                    .checked(enabled)
                    .disabled(busy)
                    .accessibility_label(format!("Enable {}", task.title))
                    .on_change(cx.listener(move |this, _: &bool, window, cx| {
                        this.act(&toggle_task, Action::SetEnabled(!enabled), window, cx)
                    })),
            )
            .child(menu);

        RowFrame::new(task.title.clone())
            .description(div().line_clamp(2).child(task.prompt.clone()))
            .status(Some(status))
            .control(control)
    }
}

impl Render for ScheduledTasksView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let Some(workspace) = self.workspace.upgrade() else { return div().into_any_element() };
        let (client, label) = {
            let workspace = workspace.read(cx);
            let label =
                workspace.settings_info().environment_label.clone().unwrap_or_else(|| "this environment".into());
            (workspace.client(), label)
        };
        if let Some(client) = &client
            && !self.subscribed
            && !matches!(self.tasks, Load::Failed(_))
        {
            self.subscribe(client.clone(), cx);
        }

        let body: Vec<RowFrame> = match (&client, &self.tasks) {
            (None, _) => vec![
                RowFrame::new("Environment disconnected")
                    .description(format!("Reconnect {label} to view its scheduled tasks.")),
            ],
            (Some(_), Load::Failed(error)) => {
                vec![RowFrame::new("Could not load scheduled tasks").description(error.clone())]
            }
            (Some(_), Load::Loading) => vec![RowFrame::new("Loading scheduled tasks…")],
            (Some(_), Load::Loaded(tasks)) if tasks.is_empty() => vec![
                RowFrame::new("No scheduled tasks")
                    .description("No tasks match this environment and project selection."),
            ],
            (Some(client), Load::Loaded(tasks)) => {
                let tasks = tasks.clone();
                tasks.iter().map(|task| self.render_row(task, client, &t, cx)).collect()
            }
        };

        let new_task = Button::new("scheduled-tasks-new")
            .ghost()
            .xsmall()
            .text_color(t.muted_foreground)
            .icon(Icon::new(Lucide::Plus).with_size(px(12.)))
            .label("New task")
            .disabled(client.is_none())
            .on_click(cx.listener(|this, _, window, cx| this.open_editor(None, window, cx)));

        let rows = body.into_iter().map(|row| row.render(&t).into_any_element());
        section_with_action("scheduled-tasks", "Scheduled tasks", Some(new_task.into_any_element()), rows, &t)
    }
}

/// `squashAtomCommandFailure`: the server's message without its error tag.
fn error_message(error: &RpcError) -> String {
    match error {
        RpcError::Failure(value) => value["message"].as_str().map_or_else(|| error.to_string(), str::to_owned),
        _ => error.to_string(),
    }
}

/// `toastManager.add({type: "error", title, description})`.
fn notify_error(window: &mut Window, cx: &mut App, title: &str, description: impl Into<SharedString>) {
    window.push_notification(Notification::error(description).title(title.to_owned()), cx);
}

/// `Badge` (default size) in the status colors `statusVariant` picks.
fn badge(status: &str, t: &Tokens) -> impl IntoElement {
    let tinted = |color: Hsla| (color, color.opacity(if t.dark { 0.16 } else { 0.08 }), gpui_kit::transparent_black());
    let (text, bg, border) = match status {
        "failed" => tinted(t.error_foreground),
        "succeeded" => tinted(t.success_foreground),
        "running" => tinted(t.info_foreground),
        _ => (t.foreground, t.input.opacity(0.32), t.input),
    };
    div()
        .h(px(24.))
        .px(px(7.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(text)
        .child(status.to_owned())
}
