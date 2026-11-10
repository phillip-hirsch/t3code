//! Settings > Scheduled tasks (`apps/web/src/components/settings/ScheduledTasksSettings.tsx`).
//!
//! Every selected environment's tasks, filtered to the scope (`matchesScheduledTaskScope`): one
//! environment reads as its own section (`ScheduledTaskEnvironmentSection`), several as one list
//! grouped by environment with a summary of the machines that show none
//! (`ScheduledTaskEnvironmentList`). A link to one task (`?taskId=`) opens its editor once it
//! loads. The section has no plain setting rows: every control is RPC-backed.

mod editor;
pub(in crate::app) mod logic;
mod webhook_logic;
mod webhooks;
mod worktree_mode;

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use crate::app::pages::settings::widgets::Switch;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Context, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task,
    WeakEntity, Window, div, px, svg,
};
use serde_json::json;

use self::logic::{
    ListState, ScheduledTask, decode_tasks, list_summaries, matches_scope, relative_label, schedule_label,
};
use super::super::widgets::{RowFrame, card, section_frame, section_with_action};
use crate::app::Workspace;
use crate::app::pages::settings::connections::logic::{ORCHESTRATION_OPERATE, machine_icon};
use crate::app::pages::settings::connections::{empty_media, ui as connection_ui};
use crate::app::pages::settings::widgets::SettingsView;
use crate::rpc::{RpcClient, RpcError, StreamEvent};
use crate::settings::scope::{self, Scope};
use crate::theme::{self, Tokens};

const SECTION: &str = "scheduled-tasks";

/// The section body: one retained view, so its subscriptions outlive a frame.
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

/// One environment's loaded tasks within the scope, or why they failed to load.
type ScopedTasks = Result<Vec<Rc<ScheduledTask>>, SharedString>;

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

/// One environment's `scheduledTasks.subscribe`, on the socket it started on.
struct EnvironmentTasks {
    client: RpcClient,
    tasks: Load,
    /// The stream closed without an error; the next frame subscribes again.
    ended: bool,
    _stream: Task<()>,
}

/// A selected environment as the list shows it.
struct Shown {
    id: SharedString,
    label: String,
    /// `None` while disconnected.
    client: Option<RpcClient>,
    machine: String,
    /// The connection's address, for the transport line and telling same-named machines apart.
    url: Option<String>,
    primary: bool,
    /// `connectionStatusTitle`.
    status: String,
}

pub(super) struct ScheduledTasksView {
    workspace: WeakEntity<Workspace>,
    /// Each selected, connected environment's tasks, only while the section is on screen.
    lists: HashMap<SharedString, EnvironmentTasks>,
    /// Repaints relative times once a minute (`useNowMinute`).
    _minute: Option<Task<()>>,
    /// Tasks with a toggle, run or delete in flight, by environment and id.
    busy: HashSet<(SharedString, String)>,
    /// The linked task whose editor already opened, so a link opens it once.
    opened_link: Option<String>,
    /// An editor dialog is open; closing it spends the link (`closeEditor`).
    editor_open: bool,
    _observe: Subscription,
}

impl ScheduledTasksView {
    fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        // Unsubscribe when Settings leaves this section.
        let observe = cx.observe(&workspace, |this, workspace, cx| {
            if workspace.read(cx).settings.as_deref() == Some(SECTION) {
                return;
            }
            if !this.lists.is_empty() {
                this.stop();
            }
            // Leaving the section drops its link, as navigating away drops `taskId`.
            if workspace.read(cx).settings_ui.scheduled_task_link.is_some() {
                workspace.update(cx, |workspace, _| workspace.settings_ui.scheduled_task_link = None);
                this.opened_link = None;
            }
        });
        Self {
            workspace: workspace.downgrade(),
            lists: HashMap::new(),
            _minute: None,
            busy: HashSet::new(),
            opened_link: None,
            editor_open: false,
            _observe: observe,
        }
    }

    fn stop(&mut self) {
        self.lists.clear();
        self._minute = None;
    }

    /// Follows `wanted` (each connected, selected environment and its socket): a list on another
    /// socket or no longer selected stops, and a new or ended one subscribes. A failed list stays
    /// failed until its environment reconnects.
    fn sync(&mut self, wanted: &[(SharedString, RpcClient)], cx: &mut Context<Self>) {
        self.lists.retain(|id, list| {
            !list.ended
                && wanted.iter().any(|(environment, client)| environment == id && client.same_socket(&list.client))
        });
        for (environment, client) in wanted {
            if !self.lists.contains_key(environment) {
                self.subscribe(environment.clone(), client.clone(), cx);
            }
        }
        if self.lists.is_empty() {
            self._minute = None;
        } else if self._minute.is_none() {
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
    }

    fn subscribe(&mut self, environment: SharedString, client: RpcClient, cx: &mut Context<Self>) {
        let stream = client.stream("scheduledTasks.subscribe", json!({}));
        let key = environment.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Some(event) = stream.next().await {
                let ended = matches!(event, StreamEvent::End(_));
                let applied = this.update(cx, |this, cx| {
                    if let Some(list) = this.lists.get_mut(&key) {
                        match event {
                            StreamEvent::Item(value) => {
                                list.tasks = Load::Loaded(decode_tasks(&value).into_iter().map(Rc::new).collect());
                            }
                            // A dropped socket shows as loading until it reconnects or reads as disconnected.
                            StreamEvent::End(Ok(()) | Err(RpcError::Disconnected(_))) => {
                                list.tasks = Load::Loading;
                                list.ended = true;
                            }
                            StreamEvent::End(Err(error)) => list.tasks = Load::Failed(error.message().into()),
                        }
                    }
                    cx.notify();
                });
                if ended || applied.is_err() {
                    return;
                }
            }
        });
        self.lists.insert(environment, EnvironmentTasks { client, tasks: Load::Loading, ended: false, _stream: task });
    }

    /// `upsertScheduledTask.permissionAtom`: this connection may operate on `environment`.
    fn can_operate(&self, environment: &str, cx: &App) -> bool {
        self.workspace.upgrade().is_some_and(|w| w.read(cx).environment_grants(environment, ORCHESTRATION_OPERATE))
    }

    fn client_on(&self, environment: &str) -> Option<RpcClient> {
        self.lists.get(environment).map(|list| list.client.clone())
    }

    fn act(
        &mut self,
        environment: &SharedString,
        task: &ScheduledTask,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client_on(environment) else { return };
        let key = (environment.clone(), task.id.clone());
        if !self.can_operate(environment, cx) || !self.busy.insert(key.clone()) {
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
                this.busy.remove(&key);
                if let Err(error) = result {
                    notify_error(window, cx, "Could not update scheduled task", error.message());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Edits one of `environment`'s tasks.
    fn open_editor(
        &mut self,
        environment: SharedString,
        task: ScheduledTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client_on(&environment) else { return };
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

    /// `environment`'s tasks when this view reads it (`None` while they load), else `None`.
    pub(super) fn tasks_on(&self, environment: &str) -> Option<Option<Tasks>> {
        self.lists.get(environment).map(|list| match &list.tasks {
            Load::Loaded(tasks) => Some(tasks.clone()),
            _ => None,
        })
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
            return false;
        };
        let Some(Some(tasks)) = self.tasks_on(environment) else { return false };
        let linked = tasks.iter().find(|t| &t.id == task_id && matches_scope(scope, environment, &t.project_id));
        let Some(task) = linked.cloned() else { return true };
        if self.opened_link.as_ref() != Some(task_id) {
            self.opened_link = Some(task_id.clone());
            if let (false, Some(client)) = (self.editor_open, self.client_on(environment)) {
                let environment = environment.clone();
                cx.defer_in(window, move |this, window, cx| {
                    this.open_editor_on(environment, client, Some((*task).clone()), window, cx);
                });
            }
        }
        false
    }

    /// `ScheduledTaskThreadLink`: a one-thread task's thread, by its title.
    fn thread_link(
        &self,
        environment: &SharedString,
        task: &ScheduledTask,
        t: &Tokens,
        cx: &App,
    ) -> Option<AnyElement> {
        let thread_id = task.thread_id.clone().filter(|_| task.thread_mode() != logic::ThreadMode::NewThread)?;
        let workspace = self.workspace.upgrade()?;
        let title = workspace
            .read(cx)
            .shell
            .threads
            .iter()
            .find(|thread| thread.environment_id == *environment && thread.id == thread_id)
            .map(|thread| thread.title.trim().to_owned())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "Open thread".into());
        let fg = t.foreground;
        let environment = environment.clone();
        Some(
            div()
                .id(SharedString::from(format!("scheduled-task-thread-{environment}-{}", task.id)))
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
        environment: &SharedString,
        task: &Rc<ScheduledTask>,
        client: &RpcClient,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> RowFrame {
        let can_operate = self.can_operate(environment, cx);
        let busy = self.busy.contains(&(environment.clone(), task.id.clone()));
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
            .children(self.thread_link(environment, task, t, cx))
            .children(task.last_run_error.clone().map(|error| div().text_color(t.error_foreground).child(error)));

        let id = format!("{environment}-{}", task.id);
        let view = cx.entity();
        let menu_task = Rc::clone(task);
        let client = client.clone();
        let context = webhook
            .then(|| self.workspace.upgrade())
            .flatten()
            .map(|w| webhooks::WebhookContext::new(client.clone(), environment, &w, cx));
        let menu_environment = environment.clone();
        let menu = Button::new(SharedString::from(format!("scheduled-task-actions-{id}")))
            .ghost()
            .small()
            .icon(Icon::new(Lucide::Ellipsis))
            .disabled(busy || !can_operate)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
                let task = menu_task.clone();
                let edit = {
                    let (view, task, environment) = (view.clone(), task.clone(), menu_environment.clone());
                    // After the menu dismisses itself, which would otherwise take the new dialog's focus with it.
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        let (view, task, environment) = (view.clone(), task.clone(), environment.clone());
                        window.defer(cx, move |window, cx| {
                            view.update(cx, |this, cx| this.open_editor(environment, (*task).clone(), window, cx));
                        });
                    }
                };
                let act = |action: Action| {
                    let (view, task, environment) = (view.clone(), task.clone(), menu_environment.clone());
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        view.update(cx, |this, cx| this.act(&environment, &task, action, window, cx));
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
        let (toggle_task, toggle_environment) = (Rc::clone(task), environment.clone());
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
                        this.act(&toggle_environment, &toggle_task, Action::SetEnabled(!enabled), window, cx);
                    })),
            )
            .child(menu);

        RowFrame::new(task.title.clone())
            .description(div().line_clamp(2).child(task.prompt.clone()))
            .status(Some(status))
            .control(control)
    }

    /// `environment`'s tasks within the scope, `None` until they load.
    fn scoped_tasks(&self, environment: &SharedString, scope: &Scope) -> Option<ScopedTasks> {
        match &self.lists.get(environment)?.tasks {
            Load::Loading => None,
            Load::Failed(error) => Some(Err(error.clone())),
            Load::Loaded(tasks) => Some(Ok(tasks
                .iter()
                .filter(|task| matches_scope(scope, environment, &task.project_id))
                .cloned()
                .collect())),
        }
    }

    /// `ScheduledTaskEnvironmentSection`: one environment's rows.
    fn environment_rows(
        &self,
        environment: &Shown,
        scope: &Scope,
        link_unavailable: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<RowFrame> {
        let Some(client) = &environment.client else {
            return vec![
                RowFrame::new("Environment disconnected")
                    .description(format!("Reconnect {} to view its scheduled tasks.", environment.label)),
            ];
        };
        let tasks = match self.scoped_tasks(&environment.id, scope) {
            None => return vec![RowFrame::new("Loading scheduled tasks…")],
            Some(Err(error)) => return vec![RowFrame::new("Could not load scheduled tasks").description(error)],
            Some(Ok(tasks)) => tasks,
        };
        let mut rows: Vec<RowFrame> = Vec::new();
        if link_unavailable {
            rows.push(task_unavailable());
        }
        if tasks.is_empty() {
            rows.push(no_scheduled_tasks());
        }
        rows.extend(tasks.iter().map(|task| self.render_row(&environment.id, task, client, t, cx)));
        rows
    }

    /// `ScheduledTaskEnvironmentList`: each environment with tasks under its own header, then a
    /// line per kind of machine that shows none.
    fn environment_list(
        &self,
        shown: &[Shown],
        scope: &Scope,
        link_unavailable: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let labeled: Vec<scope::Labeled> =
            shown.iter().map(|e| (e.id.as_ref(), e.label.as_str(), e.url.as_deref())).collect();
        let states: Vec<(&Shown, Option<ScopedTasks>)> =
            shown.iter().map(|e| (e, e.client.as_ref().and_then(|_| self.scoped_tasks(&e.id, scope)))).collect();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (environment, state) in &states {
            let (Some(client), Some(Ok(tasks))) = (&environment.client, state) else { continue };
            if tasks.is_empty() {
                continue;
            }
            let secondary = scope::environment_secondary(&environment.id, &labeled);
            rows.push(environment_header(environment, secondary, t));
            rows.extend(
                tasks
                    .iter()
                    .map(|task| self.render_row(&environment.id, task, client, t, cx).render(t).into_any_element()),
            );
        }
        let populated = !rows.is_empty();
        if link_unavailable {
            rows.push(task_unavailable().render(t).into_any_element());
        }
        let loaded = states.iter().any(|(e, state)| e.client.is_some() && matches!(state, Some(Ok(_))));
        if !populated && loaded {
            rows.push(no_scheduled_tasks().render(t).into_any_element());
        }
        let names: Vec<String> = shown
            .iter()
            .map(|e| match scope::environment_secondary(&e.id, &labeled) {
                Some(secondary) => format!("{} \u{b7} {secondary}", e.label),
                None => e.label.clone(),
            })
            .collect();
        let summary_states: Vec<(&str, ListState)> = states
            .iter()
            .zip(&names)
            .map(|((environment, state), name)| {
                let state = match (&environment.client, state) {
                    (None, _) => ListState::Disconnected(&environment.status),
                    (Some(_), None) => ListState::Loading,
                    (Some(_), Some(Err(error))) => ListState::Failed(error),
                    (Some(_), Some(Ok(tasks))) => ListState::Loaded(tasks.len()),
                };
                (name.as_str(), state)
            })
            .collect();
        for (index, summary) in list_summaries(&summary_states).into_iter().enumerate() {
            let tip = SharedString::from(summary.clone());
            rows.push(
                div()
                    .id(SharedString::from(format!("scheduled-tasks-summary-{index}")))
                    .px(px(16.))
                    .py(px(10.))
                    .truncate()
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .tooltip(connection_ui::tooltip(tip))
                    .child(summary)
                    .into_any_element(),
            );
        }
        rows
    }

    /// The selected environments, the primary first (`useSettingsScope().environments`).
    fn shown_environments(workspace: &Workspace, view: &SettingsView, cx: &App) -> Vec<Shown> {
        let saved = workspace.saved_environments.read(cx);
        view.selected_environments()
            .into_iter()
            .map(|selected| {
                let session = workspace.session(selected.id);
                let client = selected.settings.and_then(|_| workspace.env_client(selected.id));
                let connected = client.is_some();
                let status = saved.session(selected.id).map_or_else(
                    || if connected { "Connected" } else { "Reconnecting..." }.to_owned(),
                    |session| session.state.status_title(),
                );
                Shown {
                    id: selected.id.to_owned().into(),
                    label: selected.label.to_owned(),
                    client,
                    machine: session.map_or("server", |s| s.server_info.machine_kind()).to_owned(),
                    url: session.and_then(|s| s.endpoint.as_ref()).map(|e| e.http_base.clone()),
                    primary: workspace.primary_environment().is_some_and(|id| id.as_ref() == selected.id),
                    status,
                }
            })
            .collect()
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
        let (shown, scope, link, creation) = {
            let workspace = workspace.read(cx);
            let default = workspace.settings_environment();
            let view = workspace.settings_view();
            let shown = Self::shown_environments(workspace, &view, cx);
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
            (shown, view.scope, workspace.settings_ui.scheduled_task_link.clone(), creation)
        };
        let wanted: Vec<(SharedString, RpcClient)> =
            shown.iter().filter_map(|e| Some((e.id.clone(), e.client.clone()?))).collect();
        self.sync(&wanted, cx);
        let link_unavailable = self.follow_link(link.as_ref(), &scope, window, cx);

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
            }))
            .into_any_element();

        let rows: Vec<AnyElement> = match shown.as_slice() {
            [] => return section_frame("scheduled-tasks", "Scheduled tasks", Some(new_task), no_environments(&t)),
            [environment] => {
                let link_here = link_unavailable && link.as_ref().is_some_and(|(id, _)| *id == environment.id);
                self.environment_rows(environment, &scope, link_here, &t, cx)
                    .into_iter()
                    .map(|row| row.render(&t).into_any_element())
                    .collect()
            }
            several => self.environment_list(several, &scope, link_unavailable, &t, cx),
        };
        section_frame("scheduled-tasks", "Scheduled tasks", Some(new_task), card(rows, &t))
    }
}

fn task_unavailable() -> RowFrame {
    RowFrame::new("Task unavailable")
        .description("This task was deleted or belongs to a different project. Check your project filter.")
}

fn no_scheduled_tasks() -> RowFrame {
    RowFrame::new("No scheduled tasks").description("No tasks match this environment and project selection.")
}

/// `Empty` with a clock: no environment is selected.
fn no_environments(t: &Tokens) -> AnyElement {
    v_flex()
        .min_h(px(208.))
        .p(px(48.))
        .gap(px(24.))
        .items_center()
        .justify_center()
        .text_center()
        .child(empty_media("icons/clock-3.svg", t))
        .child(
            v_flex()
                .max_w(px(384.))
                .items_center()
                .child(
                    div()
                        .text_xl()
                        .line_height(px(28.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("No environments available"),
                )
                .child(
                    div()
                        .mt(px(4.))
                        .text_sm()
                        .text_color(t.muted_foreground)
                        .child("Connect an environment to manage scheduled tasks."),
                ),
        )
        .into_any_element()
}

/// `EnvironmentRow` heading one environment's tasks: its machine, name, how this client reaches
/// it (`environmentTransportLabel`), and its connection.
fn environment_header(environment: &Shown, secondary: Option<String>, t: &Tokens) -> AnyElement {
    let transport = if environment.primary {
        "This machine".to_owned()
    } else {
        environment.url.clone().unwrap_or_else(|| "Remote link".to_owned())
    };
    let secondary = secondary.filter(|secondary| *secondary != transport);
    let status: SharedString = "Connected".into();
    h_flex()
        .px(px(16.))
        .py(px(10.))
        .gap(px(12.))
        .items_center()
        .child(svg().path(machine_icon(&environment.machine)).size(px(16.)).flex_none().text_color(t.muted_foreground))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.foreground)
                        .child(environment.label.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .children(secondary.map(|secondary| div().truncate().child(secondary)))
                        .child(div().truncate().child(transport)),
                ),
        )
        .child(
            h_flex()
                .flex_none()
                .gap(px(4.))
                .child(connection_ui::status_dot(
                    SharedString::from(format!("scheduled-tasks-environment-dot-{}", environment.id)),
                    connection_ui::success(),
                    Some(status.clone()),
                ))
                .child(div().text_xs().text_color(t.muted_foreground).child(status)),
        )
        .into_any_element()
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
