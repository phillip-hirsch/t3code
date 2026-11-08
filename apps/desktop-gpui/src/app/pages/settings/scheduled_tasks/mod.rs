//! Settings > Scheduled tasks (`apps/web/src/components/settings/ScheduledTasksSettings.tsx`).
//!
//! The list is the web's single-environment section (`ScheduledTaskEnvironmentSection`) for the
//! environment Settings reads, filtered to the scope (`matchesScheduledTaskScope`); the web's
//! multi-environment list is not ported. A link to one task (`?taskId=`) opens its editor once
//! it loads, from its own environment's list when that's another one. The section has no plain
//! setting rows: every control is RPC-backed.

mod editor;
pub(in crate::app) mod logic;
mod webhook_logic;
mod webhooks;
mod worktree_mode;

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use crate::app::pages::settings::widgets::Switch;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    WeakEntity, Window, div, px,
};
use serde_json::json;

use self::logic::{ScheduledTask, decode_tasks, matches_scope, relative_label, schedule_label};
use super::super::widgets::{RowFrame, section_with_action};
use crate::app::Workspace;
use crate::app::pages::settings::connections::logic::ORCHESTRATION_OPERATE;
use crate::app::pages::settings::widgets::SettingsView;
use crate::rpc::{RpcClient, RpcError, StreamEvent};
use crate::settings::scope::Scope;
use crate::theme::{self, Tokens};

const SECTION: &str = "scheduled-tasks";

/// The section body: one retained view, so its subscription outlives a frame.
pub(super) fn render(
    workspace: &mut Workspace,
    settings: &SettingsView,
    _: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let view = workspace
        .scheduled_tasks_view
        .get_or_insert_with(|| {
            let owner = cx.entity();
            AnyView::from(cx.new(|cx| ScheduledTasksView::new(owner, cx)))
        })
        .clone();
    // `scope.kind === "unavailable"`: the selection names a project or machine that is gone.
    if let crate::settings::scope::ScopeKind::Unavailable { message } = settings.scope.kind {
        let t = *crate::theme::tokens(cx);
        let row = RowFrame::new(message).render(&t).into_any_element();
        let section = section_with_action("scheduled-tasks-unavailable", "Unavailable selection", None, [row], &t);
        return workspace.settings_page("scheduled-tasks", settings, [section], cx);
    }
    workspace.settings_page("scheduled-tasks", settings, [view.into_any_element()], cx)
}

impl Workspace {
    /// Opens Settings › Scheduled tasks on one task's editor (`?environmentId&taskId`).
    pub(in crate::app) fn open_scheduled_task(
        &mut self,
        environment: SharedString,
        task_id: String,
        cx: &mut Context<Self>,
    ) {
        self.open_settings(SECTION, cx);
        self.settings_ui.scheduled_task_link = Some((environment, task_id));
    }
}

/// The task list, shared so a row or the editor holds a task without copying it.
pub(super) type Tasks = Rc<[Rc<ScheduledTask>]>;

enum Load {
    Loading,
    Loaded(Tasks),
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
    /// The settings environment and socket the list follows; its tasks act there.
    following: Option<(SharedString, RpcClient)>,
    /// Repaints relative times once a minute (`useNowMinute`).
    _minute: Option<Task<()>>,
    /// Tasks with a toggle, run or delete in flight.
    busy: HashSet<String>,
    /// The linked task whose editor already opened, so a link opens it once.
    opened_link: Option<String>,
    /// An editor dialog is open; closing it spends the link (`closeEditor`).
    editor_open: bool,
    /// The linked task's environment, read while the link is open when the list reads another.
    linked: Option<Linked>,
    _observe: Subscription,
}

struct Linked {
    environment: SharedString,
    tasks: Option<Tasks>,
    _stream: Task<()>,
}

impl ScheduledTasksView {
    fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        // Unsubscribe when Settings leaves this section or the socket drops.
        let observe = cx.observe(&workspace, |this, workspace, cx| {
            let on_section = workspace.read(cx).settings.as_deref() == Some(SECTION);
            let shown = on_section && workspace.read(cx).settings_client().is_some();
            if !shown && (this.subscribed || this.stream.is_some()) {
                this.stop();
            }
            // Leaving the section drops its link, as navigating away drops `taskId`.
            if !on_section && workspace.read(cx).settings_ui.scheduled_task_link.is_some() {
                workspace.update(cx, |workspace, _| workspace.settings_ui.scheduled_task_link = None);
                this.opened_link = None;
            }
        });
        Self {
            workspace: workspace.downgrade(),
            tasks: Load::Loading,
            stream: None,
            subscribed: false,
            following: None,
            _minute: None,
            busy: HashSet::new(),
            opened_link: None,
            editor_open: false,
            linked: None,
            _observe: observe,
        }
    }

    fn stop(&mut self) {
        self.stream = None;
        self._minute = None;
        self.subscribed = false;
        self.following = None;
        self.tasks = Load::Loading;
        self.linked = None;
    }

    /// Reads the linked task's environment (`linkedTasksQuery`) when the list reads another.
    fn subscribe_linked(&mut self, environment: SharedString, client: &RpcClient, cx: &mut Context<Self>) {
        let stream = client.stream("scheduledTasks.subscribe", json!({}));
        let task = cx.spawn(async move |this, cx| {
            while let Some(StreamEvent::Item(value)) = stream.next().await {
                let tasks: Tasks = decode_tasks(&value).into_iter().map(Rc::new).collect();
                let applied = this.update(cx, |this, cx| {
                    if let Some(linked) = &mut this.linked {
                        linked.tasks = Some(tasks);
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
            }
        });
        self.linked = Some(Linked { environment, tasks: None, _stream: task });
    }

    fn subscribe(&mut self, environment: SharedString, client: RpcClient, cx: &mut Context<Self>) {
        self.subscribed = true;
        self.following = Some((environment, client.clone()));
        let stream = client.stream("scheduledTasks.subscribe", json!({}));
        self.stream = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = matches!(event, StreamEvent::End(_));
                let applied = this.update(cx, |this, cx| {
                    match event {
                        StreamEvent::Item(value) => {
                            this.tasks = Load::Loaded(decode_tasks(&value).into_iter().map(Rc::new).collect());
                        }
                        // A dropped socket shows as disconnected; the next connection resubscribes.
                        StreamEvent::End(Ok(()) | Err(RpcError::Disconnected(_))) => {
                            this.subscribed = false;
                            this.tasks = Load::Loading;
                        }
                        StreamEvent::End(Err(error)) => {
                            this.subscribed = false;
                            this.tasks = Load::Failed(error.message().into());
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

    /// `upsertScheduledTask.permissionAtom`: this connection may operate on `environment`.
    fn can_operate(&self, environment: &str, cx: &App) -> bool {
        self.workspace.upgrade().is_some_and(|w| w.read(cx).environment_grants(environment, ORCHESTRATION_OPERATE))
    }

    fn act(&mut self, task: &ScheduledTask, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        let Some((environment, client)) = self.following.clone() else { return };
        if !self.can_operate(&environment, cx) || !self.busy.insert(task.id.clone()) {
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
                    notify_error(window, cx, "Could not update scheduled task", error.message());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Edits one of the list's tasks.
    fn open_editor(&mut self, task: ScheduledTask, window: &mut Window, cx: &mut Context<Self>) {
        let Some((environment, client)) = self.following.clone() else { return };
        self.open_editor_on(environment, client, Some(task), window, cx);
    }

    fn open_editor_on(
        &mut self,
        environment: SharedString,
        client: RpcClient,
        task: Option<ScheduledTask>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else { return };
        let scope = workspace.read(cx).settings_view().scope;
        let list = cx.entity().downgrade();
        editor::open(self.workspace.clone(), list, (environment, client), task, scope, window, cx);
        self.editor_open = true;
    }

    /// The loaded list, for the editor's "task no longer exists" check and live webhook state.
    fn loaded_tasks(&self) -> Option<Tasks> {
        match &self.tasks {
            Load::Loaded(tasks) => Some(tasks.clone()),
            _ => None,
        }
    }

    /// `environment`'s tasks when this view reads it (`None` while they load), else `None`.
    pub(super) fn tasks_on(&self, environment: &str) -> Option<Option<Tasks>> {
        if self.following.as_ref().is_some_and(|(id, _)| id == environment) {
            return Some(self.loaded_tasks());
        }
        self.linked.as_ref().filter(|linked| linked.environment == environment).map(|linked| linked.tasks.clone())
    }

    /// A `taskId` link: opens its editor once the task loads within the scope, unless an editor
    /// the user opened is already up. True when the loaded list has no such task.
    fn follow_link(
        &mut self,
        link: Option<&(SharedString, String)>,
        scope: &Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((environment, task_id)) = link else {
            self.opened_link = None;
            self.linked = None;
            return false;
        };
        let reads_link = |this: &Self| {
            this.following.as_ref().is_some_and(|(id, _)| id == environment)
                || this.linked.as_ref().is_some_and(|linked| &linked.environment == environment)
        };
        if !reads_link(self) && scope.environment_ids.iter().any(|id| id == environment.as_ref()) {
            let client = self.workspace.upgrade().and_then(|w| w.read(cx).env_client(environment));
            if let Some(client) = client {
                self.subscribe_linked(environment.clone(), &client, cx);
            }
        }
        let Some(Some(tasks)) = self.tasks_on(environment) else { return false };
        let linked = tasks.iter().find(|t| &t.id == task_id && matches_scope(scope, environment, &t.project_id));
        let Some(task) = linked.cloned() else { return true };
        if self.opened_link.as_ref() != Some(task_id) {
            self.opened_link = Some(task_id.clone());
            let client = self.workspace.upgrade().and_then(|w| w.read(cx).env_client(environment));
            if let (false, Some(client)) = (self.editor_open, client) {
                let environment = environment.clone();
                cx.defer_in(window, move |this, window, cx| {
                    this.open_editor_on(environment, client, Some((*task).clone()), window, cx);
                });
            }
        }
        false
    }

    /// `ScheduledTaskThreadLink`: a one-thread task's thread, by its title.
    fn thread_link(&self, task: &ScheduledTask, t: &Tokens, cx: &App) -> Option<AnyElement> {
        let thread_id = task.thread_id.clone().filter(|_| task.thread_mode() != logic::ThreadMode::NewThread)?;
        let (environment, _) = self.following.clone()?;
        let workspace = self.workspace.upgrade()?;
        let title = workspace
            .read(cx)
            .shell
            .threads
            .iter()
            .find(|thread| thread.id == thread_id)
            .map(|thread| thread.title.trim().to_owned())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Open thread".into());
        let fg = t.foreground;
        Some(
            div()
                .id(SharedString::from(format!("scheduled-task-thread-{}", task.id)))
                .max_w(px(256.))
                .truncate()
                .underline()
                .cursor_pointer()
                .hover(move |style| style.text_color(fg))
                .on_click(move |_, window, cx| {
                    workspace.update(cx, |this, cx| {
                        this.close_settings(window, cx);
                        this.select_thread_in(environment.clone(), thread_id.clone(), window, cx);
                    });
                })
                .child(title)
                .into_any_element(),
        )
    }

    fn render_row(
        &self,
        task: &Rc<ScheduledTask>,
        client: &RpcClient,
        can_operate: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> RowFrame {
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
            .children(self.thread_link(task, t, cx))
            .children(task.last_run_error.clone().map(|error| div().text_color(t.error_foreground).child(error)));

        let id = task.id.clone();
        let view = cx.entity();
        let menu_task = Rc::clone(task);
        let client = client.clone();
        let context = match (&self.following, webhook) {
            (Some((environment, _)), true) => {
                self.workspace.upgrade().map(|w| webhooks::WebhookContext::new(client.clone(), environment, &w, cx))
            }
            _ => None,
        };
        let menu = Button::new(SharedString::from(format!("scheduled-task-actions-{id}")))
            .ghost()
            .small()
            .icon(Icon::new(Lucide::Ellipsis))
            .disabled(busy || !can_operate)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                let task = menu_task.clone();
                let edit = {
                    let (view, task) = (view.clone(), task.clone());
                    // After the menu dismisses itself, which would otherwise take the new dialog's focus with it.
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        let (view, task) = (view.clone(), task.clone());
                        window.defer(cx, move |window, cx| {
                            view.update(cx, |this, cx| this.open_editor((*task).clone(), window, cx));
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
        let toggle_task = Rc::clone(task);
        let control = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                Switch::new(SharedString::from(format!("scheduled-task-enabled-{id}")))
                    .checked(enabled)
                    .disabled(busy || !can_operate)
                    .accessibility_label(format!("Enable {}", task.title))
                    .on_change(cx.listener(move |this, _: &bool, window, cx| {
                        this.act(&toggle_task, Action::SetEnabled(!enabled), window, cx);
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let Some(workspace) = self.workspace.upgrade() else { return div().into_any_element() };
        // The editor closed: its link is spent (`closeEditor`).
        if self.editor_open && !window.has_active_dialog(cx) {
            self.editor_open = false;
            workspace.update(cx, |workspace, _| workspace.settings_ui.scheduled_task_link = None);
        }
        let (target, label, scope, link, creation) = {
            let workspace = workspace.read(cx);
            let label =
                workspace.settings_info().environment_label.clone().unwrap_or_else(|| "this environment".into());
            let default = workspace.settings_environment();
            let target = default.clone().and_then(|id| Some((workspace.env_client(&id)?, id)));
            let view = workspace.settings_view();
            // `creationEnvironment`: the default when it may create tasks, else the first that may.
            let permitted: Vec<SharedString> = view
                .connected_environment_ids()
                .into_iter()
                .filter(|id| workspace.environment_grants(id, ORCHESTRATION_OPERATE))
                .collect();
            let creation = permitted
                .iter()
                .find(|id| Some(*id) == default.as_ref())
                .or(permitted.first())
                .and_then(|id| Some((id.clone(), workspace.env_client(id)?)));
            (target, label, view.scope, workspace.settings_ui.scheduled_task_link.clone(), creation)
        };
        // Another environment, or the same one on a new socket, starts the list over.
        let moved = |(id, socket): &(SharedString, RpcClient)| {
            target.as_ref().is_none_or(|(client, target)| target != id || !client.same_socket(socket))
        };
        if self.following.as_ref().is_some_and(moved) {
            self.stop();
        }
        let client = target.as_ref().map(|(client, _)| client.clone());
        if let Some((client, environment)) = target.clone()
            && !self.subscribed
            && !matches!(self.tasks, Load::Failed(_))
        {
            self.subscribe(environment, client, cx);
        }
        let link_unavailable = self.follow_link(link.as_ref(), &scope, window, cx);
        let can_operate = target.as_ref().is_some_and(|(_, environment)| self.can_operate(environment, cx));

        let body: Vec<RowFrame> = match (&client, &self.tasks) {
            (None, _) => vec![
                RowFrame::new("Environment disconnected")
                    .description(format!("Reconnect {label} to view its scheduled tasks.")),
            ],
            (Some(_), Load::Failed(error)) => {
                vec![RowFrame::new("Could not load scheduled tasks").description(error.clone())]
            }
            (Some(_), Load::Loading) => vec![RowFrame::new("Loading scheduled tasks…")],
            (Some(client), Load::Loaded(tasks)) => {
                let environment = target.as_ref().map(|(_, id)| id.clone()).unwrap_or_default();
                let tasks: Vec<Rc<ScheduledTask>> = tasks
                    .iter()
                    .filter(|task| matches_scope(&scope, &environment, &task.project_id))
                    .cloned()
                    .collect();
                let mut rows: Vec<RowFrame> = Vec::new();
                if link_unavailable {
                    rows.push(RowFrame::new("Task unavailable").description(
                        "This task was deleted or belongs to a different project. Check your project filter.",
                    ));
                }
                if tasks.is_empty() {
                    rows.push(
                        RowFrame::new("No scheduled tasks")
                            .description("No tasks match this environment and project selection."),
                    );
                }
                rows.extend(tasks.iter().map(|task| self.render_row(task, client, can_operate, &t, cx)));
                rows
            }
        };

        let new_task = Button::new("scheduled-tasks-new")
            .ghost()
            .xsmall()
            .text_color(t.muted_foreground)
            .icon(Icon::new(Lucide::Plus).with_size(px(12.)))
            .label("New task")
            .disabled(creation.is_none())
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some((environment, client)) = creation.clone() {
                    this.open_editor_on(environment, client, None, window, cx);
                }
            }));

        let rows = body.into_iter().map(|row| row.render(&t).into_any_element());
        section_with_action("scheduled-tasks", "Scheduled tasks", Some(new_task.into_any_element()), rows, &t)
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
