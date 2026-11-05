//! Settings > Scheduled tasks (`apps/web/src/components/settings/ScheduledTasksSettings.tsx`).
//!
//! The native client is connected to one environment, so this is the web's single-environment
//! path (`ScheduledTaskEnvironmentSection`); the web's multi-environment list, scope filtering
//! (`matchesScheduledTaskScope`, everything matches the global scope) and `?taskId=` deep links
//! do not apply yet. The section has no plain setting rows: every control is RPC-backed.
//! `section`, `group` and `row` below are placeholders for the foundation's settings widgets.

mod editor;
mod logic;
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
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    WeakEntity, Window, div, px, svg,
};
use serde_json::json;

use self::logic::{ScheduledTask, decode_tasks, relative_label, schedule_label};
use crate::app::Workspace;
use crate::model::{ModelSelection, ServerInfo};
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
    view.into_any_element()
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

    fn render_row(&self, task: &ScheduledTask, client: &RpcClient, t: &Tokens, cx: &mut Context<Self>) -> Row {
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

        row(SharedString::from(format!("scheduled-task-{id}")), &task.title, t)
            .description(div().line_clamp(2).child(task.prompt.clone()))
            .status(status)
            .control(control)
    }
}

impl Render for ScheduledTasksView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let Some(workspace) = self.workspace.upgrade() else { return div().into_any_element() };
        let (client, label) = {
            let workspace = workspace.read(cx);
            let label = workspace.server_info.environment_label.clone().unwrap_or_else(|| "this environment".into());
            (workspace.client(), label)
        };
        if let Some(client) = &client
            && !self.subscribed
            && !matches!(self.tasks, Load::Failed(_))
        {
            self.subscribe(client.clone(), cx);
        }

        let body: Vec<Row> = match (&client, &self.tasks) {
            (None, _) => vec![
                row("scheduled-tasks-disconnected", "Environment disconnected", &t)
                    .description(format!("Reconnect {label} to view its scheduled tasks.")),
            ],
            (Some(_), Load::Failed(error)) => {
                vec![row("scheduled-tasks-error", "Could not load scheduled tasks", &t).description(error.clone())]
            }
            (Some(_), Load::Loading) => vec![row("scheduled-tasks-loading", "Loading scheduled tasks…", &t)],
            (Some(_), Load::Loaded(tasks)) if tasks.is_empty() => vec![
                row("scheduled-tasks-empty", "No scheduled tasks", &t)
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

        page(&t)
            .child(
                section("Scheduled tasks", Some(new_task.into_any_element()), &t).child(
                    group(&t).children(
                        body.into_iter().enumerate().map(|(index, row)| row.into_any_element(index == 0, &t)),
                    ),
                ),
            )
            .into_any_element()
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

/// The editor's model picker: a provider-grouped menu of every usable model. Stands in for
/// the composer's `ProviderModelPicker`, which the parent swaps in here at merge.
fn pick_model_control(
    id: impl Into<SharedString>,
    info: &ServerInfo,
    current: Option<&ModelSelection>,
    disabled: bool,
    on_pick: Rc<dyn Fn(ModelSelection, &mut Window, &mut App)>,
    t: &Tokens,
) -> AnyElement {
    let provider = current.and_then(|c| info.provider(&c.instance_id));
    let name: SharedString = current
        .map(|c| info.model(c).and_then(|m| m.name.clone()).unwrap_or_else(|| c.model.clone()))
        .unwrap_or_else(|| "Select a model".into())
        .into();
    let icon = provider.and_then(|p| crate::assets::provider_icon(&p.driver));
    let choices = Rc::new(info.model_choices());
    let current = current.cloned();
    select_trigger(id, t)
        .disabled(disabled || choices.is_empty())
        .children(
            icon.map(|(path, tint)| {
                svg().path(path).size(px(16.)).flex_none().text_color(tint.unwrap_or(t.foreground))
            }),
        )
        .child(div().flex_1().min_w_0().truncate().child(name))
        .child(chevron(t))
        .dropdown_menu(move |mut menu, _, _| {
            let mut group: Option<&str> = None;
            for (label, selection) in choices.iter() {
                let (provider, model) = label.split_once(" · ").unwrap_or(("", label.as_ref()));
                if group != Some(provider) {
                    if group.is_some() {
                        menu = menu.separator();
                    }
                    menu = menu.label(provider.to_owned());
                    group = Some(provider);
                }
                let checked = current
                    .as_ref()
                    .is_some_and(|c| c.instance_id == selection.instance_id && c.model == selection.model);
                let (picked, on_pick) = (selection.clone(), on_pick.clone());
                menu = menu.item(
                    PopupMenuItem::new(model.to_owned())
                        .checked(checked)
                        .on_click(move |_, window, cx| on_pick(picked.clone(), window, cx)),
                );
            }
            menu.max_h(px(420.)).scrollable(true)
        })
        .into_any_element()
}

/// A bordered select trigger (`SelectTrigger size="sm"`); callers add the label and a menu.
fn select_trigger(id: impl Into<SharedString>, t: &Tokens) -> Button {
    Button::new(id.into())
        .outline()
        .small()
        .w_full()
        .h(px(32.))
        .px(px(9.))
        .gap(px(8.))
        .justify_start()
        .text_sm()
        .font_weight(FontWeight::NORMAL)
        .text_color(t.foreground)
}

fn chevron(t: &Tokens) -> Icon {
    Icon::new(Lucide::ChevronDown).with_size(px(16.)).text_color(t.muted_foreground.opacity(0.7))
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

/// `SettingsPageContainer`: the scrolling, centered `max-w-4xl` column.
fn page(t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id("settings-scheduled-tasks-page")
        .size_full()
        .overflow_y_scroll()
        .text_color(t.foreground)
        .flex()
        .flex_col()
        .items_center()
}

/// `SettingsSection variant="plain"`: heading with an action, then its rows.
fn section(title: &str, action: Option<AnyElement>, t: &Tokens) -> gpui_kit::Div {
    div().w_full().max_w(px(896.)).px(px(24.)).pt(px(24.)).pb(px(48.)).flex().flex_col().gap(px(10.)).child(
        div()
            .min_h(px(28.))
            .px(px(16.))
            .flex()
            .items_start()
            .justify_between()
            .gap(px(16.))
            .child(
                div()
                    .min_h(px(28.))
                    .flex()
                    .items_center()
                    .text_base()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.foreground)
                    .child(title.to_owned()),
            )
            .child(div().min_h(px(28.)).min_w(px(28.)).flex().items_center().justify_end().children(action)),
    )
}

/// `SettingsGroup` (grouped): the bordered card whose rows are divided.
fn group(t: &Tokens) -> gpui_kit::Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(12.))
        .border_1()
        .border_color(t.border.opacity(0.6))
        .bg(t.card.opacity(0.4))
        .overflow_hidden()
}

/// `SettingsRow`: title, description and status on the left, the control on the right.
struct Row {
    id: SharedString,
    title: SharedString,
    description: Option<AnyElement>,
    status: Option<AnyElement>,
    control: Option<AnyElement>,
}

fn row(id: impl Into<SharedString>, title: &str, _: &Tokens) -> Row {
    Row { id: id.into(), title: title.to_owned().into(), description: None, status: None, control: None }
}

impl Row {
    fn description(mut self, description: impl IntoElement) -> Self {
        self.description = Some(description.into_any_element());
        self
    }

    fn status(mut self, status: impl IntoElement) -> Self {
        self.status = Some(status.into_any_element());
        self
    }

    fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// Rows after the first are divided by a top border (`[&>*+*]:border-t`).
    fn into_any_element(self, first: bool, t: &Tokens) -> AnyElement {
        div()
            .id(self.id)
            .w_full()
            .px(px(16.))
            .py(px(12.))
            .when(!first, |this| this.border_t_1().border_color(t.border.opacity(0.5)))
            .flex()
            .items_center()
            .gap(px(32.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .min_h(px(20.))
                            .flex()
                            .items_center()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(t.foreground)
                            .child(self.title),
                    )
                    .children(self.description.map(|description| {
                        div().max_w(px(672.)).text_xs().text_color(t.muted_foreground).child(description)
                    }))
                    .children(
                        self.status
                            .map(|status| div().pt(px(2.)).text_xs().text_color(t.muted_foreground).child(status)),
                    ),
            )
            .children(self.control.map(|control| div().flex_none().flex().items_center().justify_end().child(control)))
            .into_any_element()
    }
}
