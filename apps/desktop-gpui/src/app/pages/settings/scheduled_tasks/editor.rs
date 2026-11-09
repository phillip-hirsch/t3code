//! The task editor dialog (`ScheduledTaskEditorDialog` / `ScheduledTaskEditorForm`).

use std::rc::Rc;

use crate::app::pages::settings::widgets::Switch;
use chrono::{DateTime, Local, NaiveTime, Timelike as _, Utc};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::date_picker::{DatePicker, DatePickerEvent, DatePickerState};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::time_field::{TimeField, TimeFieldEvent, TimeFieldState, TimePrecision};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity,
    Window, div, px, svg,
};
use serde_json::{Value, json};

use super::logic::{
    self, DestinationChoice, DraftError, DraftState, MIN_INTERVAL_MS, NEW_THREAD_DESTINATION, Schedule, ScheduleMode,
    ScheduledTask, ThreadMode, WEEKDAY_LABELS, WEEKDAY_ORDER, WEEKDAY_SHORT, WorkspaceMode,
};
use super::webhooks::{self, DEFAULT_WEBHOOK_PROMPT, WebhookContext, WebhookFields};
use super::worktree_mode::{self, NewWorktreeMode};
use super::{ScheduledTasksView, notify_error};
use crate::app::Workspace;
use crate::app::model_picker::{OnPick, PickHost};
use crate::app::pages::settings::connections::logic::ORCHESTRATION_OPERATE;
use crate::app::pages::settings::widgets::select_trigger;
use crate::model::{ModelSelection, ProjectShell};
use crate::rpc::RpcClient;
use crate::settings::scope::Scope;
use crate::theme::{self, Tokens};

/// Opens the editor for `task`, or for a new task when it is `None`, on `target`'s environment
/// and socket, under Settings' `scope`.
pub(super) fn open(
    workspace: WeakEntity<Workspace>,
    list: WeakEntity<ScheduledTasksView>,
    target: (SharedString, RpcClient),
    task: Option<ScheduledTask>,
    scope: Scope,
    window: &mut Window,
    cx: &mut App,
) {
    let editor = cx.new(|cx| TaskEditor::new(workspace, list, target, task, scope, window, cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        // The form keeps the dialog open while a save is in flight, as `onOpenChange` does.
        let saving = editor.read(cx).saving;
        dialog.w(px(576.)).p_0().gap_0().overlay_closable(!saving).keyboard(!saving).child(editor.clone())
    });
}

pub(super) struct TaskEditor {
    workspace: WeakEntity<Workspace>,
    list: WeakEntity<ScheduledTasksView>,
    /// The environment the task is saved on: its config, projects and models.
    environment: SharedString,
    pub(super) client: RpcClient,
    task: Option<ScheduledTask>,
    /// Settings' scope when the editor opened: which environments and projects it offers.
    scope: Scope,
    pub(super) draft: DraftState,
    pub(super) saving: bool,
    /// Locks before the next render and stays locked after a successful create, like `submissionPending`.
    submission_pending: bool,
    title: Entity<InputState>,
    prompt: Entity<TextareaState>,
    interval: Entity<InputState>,
    checkout: Entity<InputState>,
    time_of_day: Entity<TimeFieldState>,
    run_at: Entity<DatePickerState>,
    webhook: Entity<WebhookFields>,
    worktree: Entity<NewWorktreeMode>,
    /// The stored task last handed to `webhook`, so a list refresh reaches it once.
    webhook_task: Option<Value>,
    /// The destination thread picker, while the Destination field shows it.
    destination: Option<DestinationPicker>,
    _subscriptions: Vec<Subscription>,
}

type DestinationSelect = SelectState<SearchableVec<DestinationChoice>>;

/// The searchable destination thread picker (`ScheduledTaskDestination`'s combobox).
struct DestinationPicker {
    /// What the rows were last derived from, so a render reads the shell only when it changed.
    key: (u64, SharedString, String, String, ThreadMode),
    /// The rows by value: only a new, removed or renamed row replaces the items, since that
    /// clears a search the reader is typing; a reorder alone waits for the next one.
    rows: Vec<DestinationChoice>,
    state: Entity<DestinationSelect>,
    _subscription: Subscription,
}

impl SelectItem for DestinationChoice {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone().into()
    }

    fn value(&self) -> &String {
        &self.value
    }

    fn matches(&self, query: &str) -> bool {
        DestinationChoice::matches(self, query)
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div().min_w_0().truncate().child(self.label.clone())
    }
}

/// The projects on `environment` that `scope` offers (`projectsOn`).
fn scoped_projects(workspace: &Workspace, scope: &Scope, environment: &str) -> Vec<ProjectShell> {
    let offered =
        |p: &&ProjectShell| p.environment_id == environment && logic::matches_scope(scope, environment, &p.id);
    workspace.shell.projects.iter().filter(offered).cloned().collect()
}

impl TaskEditor {
    fn new(
        workspace: WeakEntity<Workspace>,
        list: WeakEntity<ScheduledTasksView>,
        (environment, client): (SharedString, RpcClient),
        task: Option<ScheduledTask>,
        scope: Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = match &task {
            Some(task) => logic::task_to_draft(task, &Local),
            None => {
                let projects =
                    workspace.upgrade().map(|w| scoped_projects(w.read(cx), &scope, &environment)).unwrap_or_default();
                DraftState {
                    project_id: logic::default_project_id(&projects, scope.is_project()),
                    ..DraftState::default()
                }
            }
        };
        let text = |placeholder: &str, value: &str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder.to_owned()).default_value(value.to_owned()))
        };
        let title = text("e.g. Check for Sentry issues", &draft.title, window, cx);
        let interval = text("", &draft.interval_minutes, window, cx);
        let checkout = text("/path/to/checkout", &draft.existing_worktree_path, window, cx);
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 12)
                .placeholder("What should the agent do each time this runs?")
                .default_value(draft.prompt.clone())
        });
        let time = NaiveTime::parse_from_str(&draft.time_of_day, "%H:%M").unwrap_or_default();
        let time_of_day = cx.new(|cx| {
            let mut state = TimeFieldState::new(window, cx).precision(TimePrecision::Minute);
            state.set_time(time, window, cx);
            state
        });
        let run_at = cx.new(|cx| {
            let mut state = DatePickerState::new(window, cx).time_precision(TimePrecision::Minute);
            if let Some(value) = logic::parse_datetime_local(&draft.run_at) {
                state.set_date_time(value, window, cx);
            }
            state
        });
        let task_json = task.as_ref().map(ScheduledTask::to_json);
        let context = match workspace.upgrade() {
            Some(workspace) => WebhookContext::new(client.clone(), &environment, &workspace, cx),
            None => unreachable!("the editor opens from a live workspace"),
        };
        let webhook = cx.new(|cx| WebhookFields::new(context, task_json.clone(), window, cx));
        let cwd = workspace.upgrade().and_then(|w| {
            w.read(cx).shell.projects.iter().find(|p| p.id == draft.project_id).map(|p| p.workspace_root.clone())
        });
        let worktree = cx.new(|cx| NewWorktreeMode::new(client.clone(), cwd, task_json.as_ref(), window, cx));
        let on_text = |field: fn(&mut DraftState) -> &mut String| {
            move |this: &mut Self,
                  state: &Entity<InputState>,
                  event: &InputEvent,
                  _: &mut Window,
                  cx: &mut Context<Self>| {
                if matches!(event, InputEvent::Change) {
                    *field(&mut this.draft) = state.read(cx).value().to_string();
                }
            }
        };
        let mut subscriptions = vec![
            cx.subscribe_in(&title, window, on_text(|d| &mut d.title)),
            cx.subscribe_in(&interval, window, on_text(|d| &mut d.interval_minutes)),
            cx.subscribe_in(&checkout, window, on_text(|d| &mut d.existing_worktree_path)),
            cx.subscribe_in(&prompt, window, |this, state, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.draft.prompt = state.read(cx).value().to_string();
                }
            }),
            cx.subscribe(&time_of_day, |this, _, TimeFieldEvent::Change(time), cx| {
                this.draft.time_of_day = time.format("%H:%M").to_string();
                cx.notify();
            }),
            cx.subscribe(&run_at, |this, _, DatePickerEvent::Change(value), cx| {
                this.draft.run_at = value.start().map(|v| v.format("%Y-%m-%dT%H:%M").to_string()).unwrap_or_default();
                cx.notify();
            }),
        ];
        // The form shows the workspace's connection and projects, and its model picker's state.
        if let Some(workspace) = workspace.upgrade() {
            subscriptions.push(cx.observe(&workspace, |_, _, cx| cx.notify()));
        }
        // A list refresh hands the stored task to the webhook fields (its URL and stored secret).
        if let Some(list) = list.upgrade() {
            subscriptions.push(cx.observe_in(&list, window, |this, _, window, cx| this.sync_webhook_task(window, cx)));
        }
        Self {
            workspace,
            list,
            environment,
            client,
            task,
            scope,
            draft,
            saving: false,
            submission_pending: false,
            title,
            prompt,
            interval,
            checkout,
            time_of_day,
            run_at,
            webhook,
            worktree,
            webhook_task: task_json,
            destination: None,
            _subscriptions: subscriptions,
        }
    }

    /// The tasks the list has loaded on the editor's environment; `None` while they load or
    /// failed. A new task on an environment the list doesn't read has nothing to wait for.
    fn loaded_tasks(&self, cx: &App) -> Option<super::Tasks> {
        let list = self.list.upgrade()?;
        let list = list.read(cx);
        match list.tasks_on(&self.environment) {
            Some(tasks) => tasks,
            None => self.draft.editing_id.is_none().then(|| super::Tasks::from([])),
        }
    }

    /// `upsertScheduledTask.permissionAtom`: this connection may operate on the environment.
    fn can_operate(&self, cx: &App) -> bool {
        self.workspace
            .upgrade()
            .is_some_and(|w| w.read(cx).environment_grants(&self.environment, ORCHESTRATION_OPERATE))
    }

    /// "Runs on" picked another connected environment: its projects, models and checkouts
    /// replace the ones picked on the last (`setEnvironmentId` and the draft reset beside it).
    fn set_environment(&mut self, environment: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else { return };
        let Some(client) = workspace.read(cx).env_client(&environment) else { return };
        if self.task.is_some() || environment == self.environment {
            return;
        }
        let projects = scoped_projects(workspace.read(cx), &self.scope, &environment);
        self.draft.unbind_from_thread();
        self.draft.project_id = logic::default_project_id(&projects, self.scope.is_project());
        self.draft.model_key.clear();
        self.draft.base_model_selection = None;
        self.draft.existing_worktree_path.clear();
        self.checkout.update(cx, |input, cx| input.set_value("", window, cx));
        let context = WebhookContext::new(client.clone(), &environment, &workspace, cx);
        self.webhook.update(cx, |fields, cx| fields.set_context(context, cx));
        let cwd = projects.iter().find(|p| p.id == self.draft.project_id).map(|p| p.workspace_root.clone());
        let worktree_client = client.clone();
        self.worktree = cx.new(|cx| NewWorktreeMode::new(worktree_client, cwd, None, window, cx));
        self.environment = environment;
        self.client = client;
        cx.notify();
    }

    /// Every Destination and Project change goes through here, so a thread the task leaves is
    /// unbound (`unbindDraftFromThread`) and the worktree fields follow.
    fn change_destination(
        &mut self,
        change: impl FnOnce(&mut DraftState),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (project, workspace_mode) = (self.draft.project_id.clone(), self.draft.workspace_mode);
        change(&mut self.draft);
        if self.draft.project_id != project {
            self.sync_worktree_project(window, cx);
        }
        if workspace_mode == WorkspaceMode::Root && self.draft.workspace_mode == WorkspaceMode::Worktree {
            self.worktree.update(cx, NewWorktreeMode::start_from_origin);
        }
        cx.notify();
    }

    /// A row picked in the destination thread picker: "New thread each run", or one of the
    /// project's threads (`onSelect`). The current thread is already the destination.
    fn pick_destination(&mut self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        if value == NEW_THREAD_DESTINATION {
            self.change_destination(
                |draft| {
                    draft.unbind_from_thread();
                    draft.thread_mode = ThreadMode::NewThread;
                },
                window,
                cx,
            );
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else { return };
        let projects = self.projects(cx);
        let Some(project) = self.selected_project(&projects).map(|p| p.id.clone()) else { return };
        let movable = workspace.read(cx).shell.threads.iter().any(|t| {
            t.environment_id == self.environment
                && t.id == value
                && t.project_id == project
                && t.archived_at.is_none()
                && t.deleted_at.is_none()
                && t.is_listed()
        });
        if movable && value != self.draft.thread_id {
            self.change_destination(|draft| draft.move_to_thread(value, &project), window, cx);
        }
    }

    /// Keeps the destination picker on the shell's threads and the draft's destination; it is
    /// dropped while the Destination field hides it.
    fn sync_destination(&mut self, shown: bool, project_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade().filter(|_| shown) else {
            self.destination = None;
            return;
        };
        let key = (
            workspace.read(cx).shell.revision,
            self.environment.clone(),
            project_id.to_owned(),
            self.draft.thread_id.clone(),
            self.draft.thread_mode,
        );
        if self.destination.as_ref().is_some_and(|picker| picker.key == key) {
            return;
        }
        let (choices, selected) = {
            let threads = &workspace.read(cx).shell.threads;
            let choices = logic::destination_choices(threads, &self.environment, project_id, &self.draft);
            (choices, logic::destination_current(threads, &self.environment, &self.draft).1)
        };
        let mut rows = choices.clone();
        rows.sort_by(|a, b| a.value.cmp(&b.value));
        if let Some(picker) = &mut self.destination {
            picker.key = key;
            let state = picker.state.clone();
            if picker.rows != rows {
                picker.rows = rows;
                state.update(cx, |state, cx| state.set_items(SearchableVec::new(choices), window, cx));
            }
            if state.read(cx).selected_value() != selected.as_ref() {
                state.update(cx, |state, cx| match &selected {
                    Some(value) => state.set_selected_value(value, window, cx),
                    None => state.set_selected_index(None, window, cx),
                });
            }
            return;
        }
        let index = choices
            .iter()
            .position(|choice| Some(&choice.value) == selected.as_ref())
            .map(gpui_kit::component::IndexPath::new);
        let state = cx.new(|cx| SelectState::new(SearchableVec::new(choices), index, window, cx).searchable(true));
        let subscription = cx.subscribe_in(&state, window, |this, _, event: &SelectEvent<_>, window, cx| {
            if let SelectEvent::Confirm(Some(value)) = event {
                this.pick_destination(value, window, cx);
            }
        });
        self.destination = Some(DestinationPicker { key, rows, state, _subscription: subscription });
    }

    fn set_saving(&mut self, saving: bool, cx: &mut Context<Self>) {
        self.saving = saving;
        self.webhook.update(cx, |fields, cx| fields.set_disabled(saving, cx));
        self.worktree.update(cx, |mode, cx| mode.set_disabled(saving, cx));
    }

    fn sync_worktree_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.workspace.upgrade().and_then(|w| {
            w.read(cx).shell.projects.iter().find(|p| p.id == self.draft.project_id).map(|p| p.workspace_root.clone())
        });
        self.worktree.update(cx, |mode, cx| mode.set_project(cwd, window, cx));
    }

    /// Hands a refreshed copy of the edited task to the webhook fields (its URL and stored secret).
    fn sync_webhook_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live = self.live_task(cx).map(|task| task.to_json());
        if live.is_some() && live != self.webhook_task {
            self.webhook_task = live.clone();
            self.webhook.update(cx, |fields, cx| fields.set_task(live, window, cx));
        }
    }

    /// The stored version of the task being edited, as the list last saw it.
    fn live_task(&self, cx: &App) -> Option<Rc<ScheduledTask>> {
        match (self.loaded_tasks(cx), &self.draft.editing_id) {
            (Some(tasks), Some(id)) => tasks.iter().find(|task| &task.id == id).cloned(),
            (None, _) => self.task.clone().map(Rc::new),
            (Some(_), None) => None,
        }
    }

    fn editing_task_missing(&self, cx: &App) -> bool {
        self.draft.editing_id.is_some() && self.loaded_tasks(cx).is_some() && self.live_task(cx).is_none()
    }

    /// The scope's open projects, pinned first; the chosen one stays listed after it is archived.
    fn projects(&self, cx: &App) -> Vec<ProjectShell> {
        let Some(workspace) = self.workspace.upgrade() else { return Vec::new() };
        let keep = |p: &ProjectShell| {
            p.archived_at.is_none()
                || p.id == self.draft.project_id
                || self.task.as_ref().is_some_and(|task| task.project_id == p.id)
        };
        let mut projects = scoped_projects(workspace.read(cx), &self.scope, &self.environment);
        projects.retain(keep);
        projects.sort_by_key(|p| p.pinned_at.is_none());
        projects
    }

    /// The picked project among `projects`, else the first.
    fn selected_project<'a>(&self, projects: &'a [ProjectShell]) -> Option<&'a ProjectShell> {
        if self.draft.project_id.is_empty() {
            return projects.first();
        }
        projects.iter().find(|p| p.id == self.draft.project_id)
    }

    /// The picked model while its provider is usable, else the default (`activeSelection`).
    fn active_selection(&self, project: Option<&ProjectShell>, cx: &App) -> Option<ModelSelection> {
        let workspace = self.workspace.upgrade()?;
        let info = workspace.read(cx).env_info(&self.environment)?;
        logic::split_model_key(&self.draft.model_key)
            .filter(|s| info.providers.iter().any(|p| p.instance_id == s.instance_id && p.enabled))
            .or_else(|| logic::default_model(info, project))
    }

    fn connected(&self, cx: &App) -> bool {
        self.workspace.upgrade().is_some_and(|w| w.read(cx).env_client(&self.environment).is_some())
    }

    fn set_schedule_mode(&mut self, mode: ScheduleMode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == ScheduleMode::Once && self.draft.run_at.is_empty() {
            // The next full hour. Keep the instant too, so an hour that repeats when clocks go back stays exact.
            let now = Utc::now();
            let next = now.with_minute(0).and_then(|t| t.with_second(0)).and_then(|t| t.with_nanosecond(0));
            let next = next.map_or(now, |t| if t < now { t + chrono::Duration::hours(1) } else { t });
            let local: DateTime<Local> = next.with_timezone(&Local);
            self.draft.run_at = logic::to_datetime_local_value(&local);
            self.draft.saved_run_at = logic::to_iso(next);
            self.run_at.update(cx, |state, cx| state.set_date_time(local.naive_local(), window, cx));
        }
        if mode == ScheduleMode::Webhook && self.draft.prompt.trim().is_empty() {
            self.draft.prompt = DEFAULT_WEBHOOK_PROMPT.to_owned();
            self.prompt.update(cx, |state, cx| state.set_value(DEFAULT_WEBHOOK_PROMPT, window, cx));
        }
        self.draft.schedule_mode = mode;
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_operate(cx)
            || self.submission_pending
            || self.saving
            || self.editing_task_missing(cx)
            || !self.connected(cx)
        {
            return;
        }
        let Some(workspace) = self.workspace.upgrade() else { return };
        if self.loaded_tasks(cx).is_none() {
            return;
        }
        let fail = |error: DraftError, window: &mut Window, cx: &mut App| {
            notify_error(window, cx, error.title, error.description);
        };
        let draft = self.draft.clone();
        let projects = self.projects(cx);
        let project = self.selected_project(&projects).cloned();
        let selection = self.active_selection(project.as_ref(), cx);
        let (Some(project), Some(selection)) = (project, selection) else {
            return fail(
                DraftError::new("Scheduled task is incomplete", "Add a title, prompt, project, and model."),
                window,
                cx,
            );
        };
        if draft.title.trim().is_empty() || draft.prompt.trim().is_empty() {
            return fail(
                DraftError::new("Scheduled task is incomplete", "Add a title, prompt, project, and model."),
                window,
                cx,
            );
        }
        let schedule = if draft.schedule_mode == ScheduleMode::Webhook {
            match self.webhook.read(cx).schedule(cx) {
                Ok(schedule) => schedule,
                Err(errors) => {
                    for error in errors {
                        fail(DraftError::new(error.title, error.description), window, cx);
                    }
                    return;
                }
            }
        } else {
            match logic::schedule_from_draft(&draft, &Local) {
                Some(schedule) => schedule,
                None => {
                    for error in logic::schedule_draft_errors(&draft, &Local) {
                        fail(error, window, cx);
                    }
                    return;
                }
            }
        };
        if schedule["type"] == "interval"
            && !schedule["everyMs"].as_i64().is_some_and(|ms| ms as f64 >= MIN_INTERVAL_MS && ms < (1 << 53))
        {
            return fail(DraftError::new("Invalid interval", "Enter an interval of at least one minute."), window, cx);
        }
        let (offers_once, policy) = {
            let Some(info) = workspace.read(cx).env_info(&self.environment) else { return };
            (self.offers_once(info.capability("oneTimeSchedules")), info.managed_policy.clone())
        };
        if schedule["type"] == "once" && !offers_once {
            return fail(
                DraftError::new(
                    "Once is not available here",
                    "This environment runs an older T3 Code. Update it to schedule a one-time task, or pick another schedule.",
                ),
                window,
                cx,
            );
        }
        if draft.workspace_mode == WorkspaceMode::ExistingWorktree && draft.existing_worktree_path.trim().is_empty() {
            return fail(
                DraftError::new("Checkout path is required", "Enter the path of the checkout to run in."),
                window,
                cx,
            );
        }
        let model_selection = match &draft.base_model_selection {
            Some(base)
                if base["instanceId"] == selection.instance_id.as_str()
                    && base["model"] == selection.model.as_str() =>
            {
                base.clone()
            }
            _ => selection.to_json(),
        };
        let workspace_strategy = match draft.workspace_mode {
            WorkspaceMode::Root => json!({"type": "root"}),
            WorkspaceMode::ExistingWorktree => {
                json!({"type": "existing_worktree", "worktreePath": draft.existing_worktree_path.trim()})
            }
            WorkspaceMode::Worktree => self.worktree.read(cx).strategy(),
        };
        let mut input = json!({
            "title": draft.title.trim(),
            "prompt": draft.prompt.trim(),
            "enabled": draft.enabled,
            "schedule": schedule,
            "projectId": project.id,
            "threadId": if draft.thread_id.is_empty() { Value::Null } else { Value::from(draft.thread_id.clone()) },
            "workspaceStrategy": workspace_strategy,
            "modelSelection": model_selection,
            "runtimeMode": logic::clamp_runtime_mode(&policy, &draft.runtime_mode),
            "interactionMode": draft.interaction_mode,
            "creationSource": "web",
        });
        if self.offers_thread_modes(cx) {
            input["threadMode"] = json!(draft.thread_mode.wire());
        }
        if let Some(id) = &draft.editing_id {
            input["id"] = json!(id);
            input["requireExisting"] = json!(true);
        }
        self.submission_pending = true;
        self.set_saving(true, cx);
        cx.notify();
        let client = self.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("scheduledTasks.upsert", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_saving(false, cx);
                match result {
                    Ok(_) => window.close_dialog(cx),
                    Err(error) => {
                        this.submission_pending = false;
                        notify_error(window, cx, "Could not save scheduled task", error.message());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Servers before thread modes ignore `threadMode`, so the picker is offered only where it saves.
    fn offers_thread_modes(&self, cx: &App) -> bool {
        self.workspace.upgrade().is_some_and(|workspace| {
            workspace
                .read(cx)
                .env_info(&self.environment)
                .is_some_and(|info| info.capability("scheduledTaskThreadModes"))
        })
    }

    /// Older servers reject one-time schedules, so Once is offered only where it can save.
    fn offers_once(&self, capability: bool) -> bool {
        capability || self.task.as_ref().is_some_and(|task| matches!(task.schedule(), Schedule::Once { .. }))
    }
}

impl Render for TaskEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let Some(workspace) = self.workspace.upgrade() else { return div().into_any_element() };
        let editor = cx.entity();
        let connected = self.connected(cx);
        let tasks_loaded = self.loaded_tasks(cx).is_some();
        let missing = self.editing_task_missing(cx);
        let projects = self.projects(cx);
        let project = self.selected_project(&projects);
        let selection = self.active_selection(project, cx);
        let offers_thread_modes = self.offers_thread_modes(cx);
        let picks_thread = !offers_thread_modes || self.draft.thread_mode != ThreadMode::NewThread;
        let project_id = project.map(|p| p.id.clone()).unwrap_or_default();
        self.sync_destination(picks_thread, &project_id, window, cx);
        let (offers_once, denial, env_label, machine, environments) = {
            let workspace = workspace.read(cx);
            // `connectedEnvironments`: the selected environments this client is connected to.
            let environments: Vec<(SharedString, String, &'static str)> = self
                .scope
                .environment_ids
                .iter()
                .filter(|id| workspace.env_client(id).is_some())
                .filter_map(|id| {
                    let info = workspace.env_info(id)?;
                    let label = info.environment_label.clone().unwrap_or_else(|| id.clone());
                    Some((SharedString::from(id.clone()), label, machine_icon(info.machine_kind())))
                })
                .collect();
            let Some(info) = workspace.env_info(&self.environment) else { return div().into_any_element() };
            (
                self.offers_once(info.capability("oneTimeSchedules")),
                logic::webhook_denial(&info.managed_policy),
                info.environment_label.clone().unwrap_or_else(|| "Unavailable environment".into()),
                machine_icon(info.machine_kind()),
                environments,
            )
        };
        let disabled = self.saving;
        let draft = &self.draft;
        let thread_mode = draft.thread_mode;
        let bound = !draft.thread_id.is_empty();
        let thread_mode_select = offers_thread_modes.then(|| {
            select_trigger(&"scheduled-task-destination".into(), thread_mode.label(), 0., &t)
                .w_full()
                .disabled(disabled)
                .dropdown_menu({
                    let editor = editor.clone();
                    move |mut menu, window, _| {
                        for mode in ThreadMode::ALL {
                            menu = menu.item(PopupMenuItem::new(mode.label()).checked(mode == thread_mode).on_click(
                                window.listener_for(&editor, move |this: &mut TaskEditor, _, window, cx| {
                                    this.change_destination(|draft| draft.set_thread_mode(mode), window, cx);
                                }),
                            ));
                        }
                        menu
                    }
                })
        });
        // `ComboboxTrigger` as an outline `sm` button: the thread icon, the destination, the chevron.
        let thread_picker = self.destination.as_ref().map(|picker| {
            let select = t
                .breakpoint
                .control_text(Select::new(&picker.state).small())
                .w_full()
                .h(px(t.breakpoint.pick(32., 28.)))
                .pl(px(29.))
                .pr(px(9.))
                .menu_width(px(320.))
                .menu_max_h(px(288.))
                .placeholder("Started by the first run")
                .search_placeholder("Search all threads...")
                .disabled(disabled || !connected)
                .empty(|_, cx| {
                    div()
                        .p(px(8.))
                        .text_sm()
                        .text_color(theme::tokens(cx).muted_foreground)
                        .child("No threads found.")
                        .into_any_element()
                });
            let select = if offers_thread_modes { select.accessibility_label("Destination thread") } else { select };
            div().relative().w_full().child(select).child(
                div().absolute().left(px(10.)).top_0().bottom_0().flex().items_center().child(
                    gpui_kit::component::Icon::new(Lucide::MessageSquare).size(px(14.)).text_color(t.muted_foreground),
                ),
            )
        });

        // Runs on: a new task can move to any connected environment the scope selects.
        let environment = h_flex()
            .min_w_0()
            .gap(px(8.))
            .child(svg().path(machine).size(px(16.)).flex_none().text_color(t.foreground))
            .child(div().min_w_0().truncate().child(env_label));
        let current_environment = self.environment.clone();
        let runs_on = select_trigger(&"scheduled-task-environment".into(), environment, 0., &t)
            .w_full()
            .disabled(self.task.is_some() || disabled)
            .dropdown_menu({
                let editor = editor.clone();
                move |mut menu, window, _| {
                    for (id, label, icon) in &environments {
                        let id = id.clone();
                        let item = PopupMenuItem::new(label.clone())
                            .icon(gpui_kit::component::Icon::empty().path(*icon))
                            .checked(id == current_environment);
                        menu = menu.item(item.on_click(window.listener_for(
                            &editor,
                            move |this: &mut TaskEditor, _, window, cx| {
                                this.set_environment(id.clone(), window, cx);
                            },
                        )));
                    }
                    menu
                }
            });

        let duplicate_titles =
            |project: &ProjectShell| projects.iter().filter(|p| p.title == project.title).count() > 1;
        let project_menu: Vec<(String, String, Option<String>)> = projects
            .iter()
            .map(|p| (p.id.clone(), p.title.clone(), duplicate_titles(p).then(|| p.workspace_root.clone())))
            .collect();
        let selected_project_id = project.as_ref().map(|p| p.id.clone()).unwrap_or_default();
        let project_label = div()
            .min_w_0()
            .truncate()
            .when(project.is_none(), |this| this.text_color(t.muted_foreground))
            .child(project.as_ref().map_or_else(|| "Select a project".to_owned(), |p| p.title.clone()));
        let project_select = select_trigger(&"scheduled-task-project".into(), project_label, 0., &t)
            .w_full()
            .disabled(disabled)
            .dropdown_menu({
                let editor = editor.clone();
                move |mut menu, window, _| {
                    for (id, title, root) in &project_menu {
                        let label = match root {
                            Some(root) => format!("{title} — {root}"),
                            None => title.clone(),
                        };
                        let id = id.clone();
                        menu = menu.item(PopupMenuItem::new(label).checked(id == selected_project_id).on_click(
                            window.listener_for(&editor, move |this: &mut TaskEditor, _, window, cx| {
                                this.change_destination(|draft| draft.change_project(&id), window, cx);
                            }),
                        ));
                    }
                    menu.max_h(px(360.)).scrollable(true)
                }
            });

        let workspace_mode = draft.workspace_mode;
        let workspace_label = div().min_w_0().truncate().child(workspace_mode.label());
        let workspace_select = select_trigger(&"scheduled-task-workspace".into(), workspace_label, 0., &t)
            .w_full()
            .disabled(disabled)
            .dropdown_menu({
                let editor = editor.clone();
                move |mut menu, window, _| {
                    for mode in WorkspaceMode::ALL {
                        menu = menu.item(PopupMenuItem::new(mode.label()).checked(mode == workspace_mode).on_click(
                            window.listener_for(&editor, move |this: &mut TaskEditor, _, _, cx| {
                                this.draft.workspace_mode = mode;
                                cx.notify();
                            }),
                        ));
                    }
                    menu
                }
            });

        let model = workspace.update(cx, |workspace, cx| {
            let id = SharedString::from("scheduled-task-model");
            let editor = editor.clone();
            let disabled = disabled
                || !connected
                || workspace.env_info(&self.environment).is_none_or(|info| info.model_choices().is_empty());
            let environment = Some(self.environment.as_ref());
            let trigger = workspace
                .model_picker_trigger(id.clone(), selection.as_ref(), None, environment, cx)
                .disabled(disabled);
            let on_pick: OnPick = Rc::new(move |_, picked, _, cx| {
                editor.update(cx, |this, cx| {
                    this.draft.model_key = format!("{}:{}", picked.instance_id, picked.model);
                    cx.notify();
                });
            });
            workspace.undocked_model_picker(
                id,
                trigger,
                PickHost {
                    environment: Some(self.environment.clone()),
                    current: selection.clone(),
                    include: |_| true,
                    extra: Rc::default(),
                    on_pick,
                },
                cx,
            )
        });

        let mode = draft.schedule_mode;
        let modes: Vec<(ScheduleMode, &str)> = [
            Some((ScheduleMode::Fixed, "At a time")),
            (offers_once || mode == ScheduleMode::Once).then_some((ScheduleMode::Once, "Once")),
            Some((ScheduleMode::Interval, "Every interval")),
            Some((ScheduleMode::Webhook, "On webhook")),
        ]
        .into_iter()
        .flatten()
        .collect();
        let schedule_toggle = div().flex().items_center().children(modes.iter().map(|(m, label)| {
            let m = *m;
            let blocked = disabled || (m == ScheduleMode::Webhook && denial.is_some() && mode != ScheduleMode::Webhook);
            toggle(SharedString::from(format!("schedule-mode-{label}")), label, m == mode, false, blocked, &t).when(
                !blocked,
                |this| {
                    this.on_click(cx.listener(move |this, _, window, cx| {
                        if this.draft.schedule_mode != m {
                            this.set_schedule_mode(m, window, cx);
                        }
                    }))
                },
            )
        }));

        let schedule_fields: AnyElement = match mode {
            ScheduleMode::Webhook if denial.is_some() && draft.enabled => div().into_any_element(),
            ScheduleMode::Webhook => webhooks::render_webhook_fields(&self.webhook).into_any_element(),
            ScheduleMode::Once => inline(&t)
                .child(label("Run on", &t))
                .child(div().w(px(224.)).child(DatePicker::new(&self.run_at).placeholder("Pick a date and time")))
                .into_any_element(),
            ScheduleMode::Fixed => {
                let weekdays = &draft.weekdays;
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        inline(&t)
                            .child(label("Run at", &t))
                            .child(div().w(px(128.)).child(TimeField::new(&self.time_of_day)))
                            .child(div().text_xs().text_color(t.muted_foreground).child("on")),
                    )
                    .child(div().flex().items_center().gap(px(4.)).children(WEEKDAY_ORDER.iter().map(|day| {
                        let day = *day;
                        let on = weekdays.contains(&day);
                        let name = WEEKDAY_LABELS[day as usize];
                        toggle(
                            SharedString::from(format!("weekday-{name}")),
                            WEEKDAY_SHORT[day as usize],
                            on,
                            true,
                            disabled,
                            &t,
                        )
                        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(name).build(window, cx))
                        .when(!disabled, |this| {
                            this.on_click(cx.listener(move |this, _, _, cx| {
                                // The last day stays on, as `onValueChange` ignores an empty set.
                                if on && this.draft.weekdays.len() == 1 {
                                    return;
                                }
                                if on {
                                    this.draft.weekdays.remove(&day);
                                } else {
                                    this.draft.weekdays.insert(day);
                                }
                                cx.notify();
                            }))
                        })
                    })))
                    .into_any_element()
            }
            ScheduleMode::Interval => inline(&t)
                .child(label("Run every", &t))
                .child(div().w(px(96.)).child(Input::new(&self.interval)))
                .child(div().text_xs().text_color(t.muted_foreground).child("minutes"))
                .into_any_element(),
        };

        let legacy_interval = self.task.as_ref().is_some_and(
            |task| matches!(task.schedule(), Schedule::Interval { every_ms } if every_ms < MIN_INTERVAL_MS),
        );
        let webhook_blocked = draft.enabled && mode == ScheduleMode::Webhook && denial.is_some();
        let can_save = self.can_operate(cx) && connected && !disabled && !missing && tasks_loaded && !webhook_blocked;
        let editing = draft.editing_id.is_some();
        let enabled = draft.enabled;
        let new_worktree = (workspace_mode == WorkspaceMode::Worktree)
            .then(|| worktree_mode::render_new_worktree_mode(&self.worktree).into_any_element());
        let draft = &self.draft;
        let panel_max_h = (window.viewport_size().height - px(240.)).max(px(200.));

        div()
            .w_full()
            .flex()
            .flex_col()
            .text_color(t.foreground)
            // DialogHeader
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .p(px(24.))
                    .pb(px(12.))
                    .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(if editing { "Edit task" } else { "New task" }))
                    .child(div().text_sm().text_color(t.muted_foreground).child(
                        "Run a prompt automatically on an interval, at a fixed time, once at a set date, or when a webhook is called.",
                    )),
            )
            // DialogPanel
            .child(
                div()
                    .id("scheduled-task-form")
                    .max_h(panel_max_h)
                    .overflow_y_scroll()
                    .px(px(24.))
                    .pt(px(4.))
                    .pb(px(24.))
                    .flex()
                    .flex_col()
                    .gap(px(20.))
                    .when(!connected, |this| {
                        this.child(div().text_sm().text_color(t.error_foreground).child("Reconnect this environment before saving."))
                    })
                    .child(field("Runs on", None, runs_on, &t))
                    .when(missing, |this| {
                        this.child(div().text_xs().text_color(t.error_foreground).child("This scheduled task no longer exists."))
                    })
                    .child(field("Name", None, Input::new(&self.title).disabled(disabled), &t))
                    .child(field(
                        "Destination",
                        None,
                        div().flex().flex_col().gap(px(6.)).children(thread_mode_select).children(thread_picker).child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .text_sm()
                                .child(div().text_color(t.muted_foreground).child(thread_mode.detail(bound)))
                                .when(thread_mode != ThreadMode::NewThread, |this| {
                                    this.child(
                                        div().text_color(t.muted_foreground).child("Deleting the thread also deletes this task."),
                                    )
                                }),
                        ),
                        &t,
                    ))
                    .child(
                        div()
                            .flex()
                            .gap(px(12.))
                            .child(div().flex_1().min_w_0().child(field("Project", None, project_select, &t)))
                            .child(div().flex_1().min_w_0().child(field("Workspace", None, workspace_select, &t))),
                    )
                    .children(new_worktree)
                    .when(workspace_mode == WorkspaceMode::ExistingWorktree, |this| {
                        this.child(field("Checkout path", None, Input::new(&self.checkout).disabled(disabled), &t))
                    })
                    .child(field("Prompt", None, Textarea::new(&self.prompt).disabled(disabled), &t))
                    .child(field("Model", None, h_flex().child(model), &t))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .when(legacy_interval, |this| {
                                this.child(div().text_sm().text_color(t.muted_foreground).child(
                                    "This task uses a legacy interval below one minute. Saving updates it to at least one minute.",
                                ))
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_wrap()
                                    .items_center()
                                    .justify_between()
                                    .gap(px(8.))
                                    .child(label("Schedule", &t))
                                    .child(schedule_toggle),
                            )
                            .children(denial.map(|denial| {
                                let text = if mode == ScheduleMode::Webhook {
                                    format!("{denial} Switch this task to a time or an interval, or turn it off, to save it.")
                                } else {
                                    denial
                                };
                                managed_notice(text, &t)
                            }))
                            .child(schedule_fields),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(16.))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(label("Enabled", &t))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(t.muted_foreground)
                                            .child("Disabled tasks stay saved but do not run."),
                                    ),
                            )
                            .child(
                                Switch::new("scheduled-task-enabled")
                                    .checked(enabled)
                                    .disabled(disabled)
                                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                        this.draft.enabled = *checked;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            // DialogFooter
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .px(px(24.))
                    .py(px(16.))
                    .border_t_1()
                    .border_color(t.border)
                    .bg(t.muted.opacity(0.72))
                    .rounded_b(px(16.))
                    .child(
                        Button::new("scheduled-task-cancel")
                            .outline()
                            .small()
                            .h(px(28.))
                            .label("Cancel")
                            .disabled(disabled)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        primary_button("scheduled-task-save", &t)
                            .when(!can_save, |this| this.opacity(0.64))
                            .when(can_save, |this| {
                                this.cursor_pointer()
                                    .hover(|style| style.bg(t.primary_hover))
                                    .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx)))
                            })
                            .child(if draft.editing_id.is_some() { "Save task" } else { "Create task" }),
                    ),
            )
            .into_any_element()
    }
}

/// `Button size="sm"` in the default variant: the web's blue primary, which
/// gpui-component's buttons do not draw (its custom variant mutes the color).
fn primary_button(id: &'static str, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .h(px(28.))
        .px(px(9.))
        .flex()
        .items_center()
        .rounded(px(8.))
        .bg(t.primary)
        .text_color(t.primary_foreground)
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
}

/// `Toggle`: the default variant (schedule type) or `variant="outline" size="sm"` (weekdays).
fn toggle(
    id: SharedString,
    text: &'static str,
    pressed: bool,
    outline: bool,
    disabled: bool,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let (idle, on) = match (outline, t.dark) {
        (true, true) => (t.input.opacity(0.32), t.input),
        (true, false) => (t.background, t.input.opacity(0.64)),
        (false, _) => (gpui_kit::transparent_black(), t.input.opacity(0.64)),
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .border_1()
        .border_color(if outline { t.input } else { gpui_kit::transparent_black() })
        .bg(if pressed { on } else { idle })
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.foreground)
        .map(|this| {
            if outline {
                this.h(px(28.)).min_w(px(28.)).px(px(3.)).rounded(px(6.)).text_xs()
            } else {
                this.h(px(32.)).min_w(px(32.)).px(px(7.)).rounded(px(10.)).text_sm()
            }
        })
        .when(disabled, |this| this.opacity(0.64))
        .when(!disabled, gpui_kit::Styled::cursor_pointer)
        .when(!disabled && !pressed, |this| {
            let hover = if outline && t.dark { t.input.opacity(0.64) } else { t.accent };
            this.hover(move |style| style.bg(hover))
        })
        .child(text)
}

/// `Field`: a label above its control.
fn field(name: &str, hint: Option<&str>, control: impl IntoElement, t: &Tokens) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().flex().items_baseline().justify_between().child(label(name, t)).children(
            hint.map(|hint| {
                div().text_size(px(11.)).text_color(t.muted_foreground.opacity(0.8)).child(hint.to_owned())
            }),
        ))
        .child(control)
}

/// `Label`.
fn label(text: &str, t: &Tokens) -> impl IntoElement {
    div().text_sm().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(text.to_owned())
}

/// A label and its control on one line (`flex items-center gap-2`).
fn inline(_: &Tokens) -> gpui_kit::Div {
    div().flex().items_center().gap(px(8.))
}

/// `ManagedNotice`: why an administrator's policy blocks something.
fn managed_notice(text: String, t: &Tokens) -> impl IntoElement {
    div()
        .rounded(px(8.))
        .border_1()
        .border_color(t.warning.opacity(0.32))
        .bg(t.warning.opacity(0.04))
        .px(px(12.))
        .py(px(8.))
        .text_xs()
        .text_color(t.warning_foreground)
        .child(text)
}

/// `EnvironmentMachineIcon`, as the details card draws it.
fn machine_icon(kind: &str) -> &'static str {
    match kind {
        "desktop" | "mac-studio" | "mac-mini" => "icons/monitor.svg",
        "laptop" => "icons/laptop.svg",
        "cloud" => "icons/cloud.svg",
        _ => "icons/server.svg",
    }
}
