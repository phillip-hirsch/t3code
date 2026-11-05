//! The task editor dialog (`ScheduledTaskEditorDialog` / `ScheduledTaskEditorForm`).

use std::rc::Rc;

use chrono::{DateTime, Local, NaiveTime, Timelike as _, Utc};
use gpui_kit::component::button::Button;
use gpui_kit::component::date_picker::{DatePicker, DatePickerEvent, DatePickerState};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
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
    self, DraftError, DraftState, MIN_INTERVAL_MS, Schedule, ScheduleMode, ScheduledTask, WEEKDAY_LABELS,
    WEEKDAY_ORDER, WEEKDAY_SHORT, WorkspaceMode,
};
use super::webhooks::{self, DEFAULT_WEBHOOK_PROMPT, WebhookContext, WebhookFields};
use super::worktree_mode::{self, NewWorktreeMode};
use super::{ScheduledTasksView, notify_error};
use crate::app::Workspace;
use crate::app::model_picker::{OnPick, PickHost};
use crate::app::pages::settings::widgets::select_trigger;
use crate::model::{ModelSelection, ProjectShell};
use crate::rpc::RpcClient;
use crate::theme::{self, Tokens};

/// Opens the editor for `task`, or for a new task when it is `None`.
pub(super) fn open(
    workspace: WeakEntity<Workspace>,
    list: WeakEntity<ScheduledTasksView>,
    client: RpcClient,
    task: Option<ScheduledTask>,
    window: &mut Window,
    cx: &mut App,
) {
    let editor = cx.new(|cx| TaskEditor::new(workspace, list, client, task, window, cx));
    window.open_dialog(cx, move |dialog, _, cx| {
        // The form keeps the dialog open while a save is in flight, as `onOpenChange` does.
        let saving = editor.read(cx).saving;
        dialog.w(px(576.)).p_0().gap_0().overlay_closable(!saving).keyboard(!saving).child(editor.clone())
    });
}

pub(super) struct TaskEditor {
    workspace: WeakEntity<Workspace>,
    list: WeakEntity<ScheduledTasksView>,
    pub(super) client: RpcClient,
    task: Option<ScheduledTask>,
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
    _subscriptions: Vec<Subscription>,
}

impl TaskEditor {
    fn new(
        workspace: WeakEntity<Workspace>,
        list: WeakEntity<ScheduledTasksView>,
        client: RpcClient,
        task: Option<ScheduledTask>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = match &task {
            Some(task) => logic::task_to_draft(task, &Local),
            None => {
                let projects = workspace.upgrade().map(|w| w.read(cx).shell.projects.clone()).unwrap_or_default();
                DraftState { project_id: logic::default_project_id(&projects), ..DraftState::default() }
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
            Some(workspace) => WebhookContext::new(client.clone(), &workspace, cx),
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
        Self {
            workspace,
            list,
            client,
            task,
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
            _subscriptions: subscriptions,
        }
    }

    /// The tasks the list has loaded; `None` while it loads or failed.
    fn loaded_tasks(&self, cx: &App) -> Option<Rc<Vec<ScheduledTask>>> {
        self.list.upgrade()?.read(cx).loaded_tasks()
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
    fn live_task(&self, cx: &App) -> Option<ScheduledTask> {
        match (self.loaded_tasks(cx), &self.draft.editing_id) {
            (Some(tasks), Some(id)) => tasks.iter().find(|task| &task.id == id).cloned(),
            (None, _) => self.task.clone(),
            (Some(_), None) => None,
        }
    }

    fn editing_task_missing(&self, cx: &App) -> bool {
        self.draft.editing_id.is_some() && self.loaded_tasks(cx).is_some() && self.live_task(cx).is_none()
    }

    /// Open projects pinned first; the chosen one stays listed after it is archived.
    fn projects(&self, cx: &App) -> Vec<ProjectShell> {
        let Some(workspace) = self.workspace.upgrade() else { return Vec::new() };
        let keep = |p: &&ProjectShell| {
            p.archived_at.is_none()
                || p.id == self.draft.project_id
                || self.task.as_ref().is_some_and(|task| task.project_id == p.id)
        };
        let mut projects: Vec<_> = workspace.read(cx).shell.projects.iter().filter(keep).cloned().collect();
        projects.sort_by_key(|p| p.pinned_at.is_none());
        projects
    }

    pub(super) fn selected_project(&self, cx: &App) -> Option<ProjectShell> {
        let projects = self.projects(cx);
        let id =
            if self.draft.project_id.is_empty() { projects.first()?.id.clone() } else { self.draft.project_id.clone() };
        projects.into_iter().find(|p| p.id == id)
    }

    /// The picked model while its provider is usable, else the default (`activeSelection`).
    fn active_selection(&self, cx: &App) -> Option<ModelSelection> {
        let workspace = self.workspace.upgrade()?;
        let info = &workspace.read(cx).server_info;
        logic::split_model_key(&self.draft.model_key)
            .filter(|s| info.providers.iter().any(|p| p.instance_id == s.instance_id && p.enabled))
            .or_else(|| logic::default_model(info, self.selected_project(cx).as_ref()))
    }

    fn connected(&self, cx: &App) -> bool {
        self.workspace.upgrade().is_some_and(|w| w.read(cx).client().is_some())
    }

    fn set_schedule_mode(&mut self, mode: ScheduleMode, window: &mut Window, cx: &mut Context<Self>) {
        if mode == ScheduleMode::Once && self.draft.run_at.is_empty() {
            // The next full hour. Keep the instant too, so an hour that repeats when clocks go back stays exact.
            let now = Utc::now();
            let next = now.with_minute(0).and_then(|t| t.with_second(0)).and_then(|t| t.with_nanosecond(0));
            let next = next.map(|t| if t < now { t + chrono::Duration::hours(1) } else { t }).unwrap_or(now);
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
        if self.submission_pending || self.saving || self.editing_task_missing(cx) || !self.connected(cx) {
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
        let project = self.selected_project(cx);
        let selection = self.active_selection(cx);
        let (Some(project), Some(selection)) = (project.filter(|p| projects.iter().any(|q| q.id == p.id)), selection)
        else {
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
            let info = &workspace.read(cx).server_info;
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

    /// Older servers reject one-time schedules, so Once is offered only where it can save.
    fn offers_once(&self, capability: bool) -> bool {
        capability || self.task.as_ref().is_some_and(|task| matches!(task.schedule(), Schedule::Once { .. }))
    }
}

impl Render for TaskEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webhook_task(window, cx);
        let t = *theme::tokens(cx);
        let Some(workspace) = self.workspace.upgrade() else { return div().into_any_element() };
        let editor = cx.entity();
        let connected = self.connected(cx);
        let tasks_loaded = self.loaded_tasks(cx).is_some();
        let missing = self.editing_task_missing(cx);
        let projects = self.projects(cx);
        let project = self.selected_project(cx);
        let selection = self.active_selection(cx);
        let (offers_once, denial, env_label, machine, destination) = {
            let workspace = workspace.read(cx);
            let info = &workspace.server_info;
            let destination = (!self.draft.thread_id.is_empty()).then(|| {
                workspace
                    .shell
                    .threads
                    .iter()
                    .find(|thread| thread.id == self.draft.thread_id)
                    .map(|thread| thread.title.trim().to_owned())
                    .filter(|title| !title.is_empty())
                    .unwrap_or_else(|| format!("Thread {}", self.draft.thread_id))
            });
            (
                self.offers_once(info.capability("oneTimeSchedules")),
                logic::webhook_denial(&info.managed_policy),
                info.environment_label.clone().unwrap_or_else(|| "Unavailable environment".into()),
                machine_icon(info.machine_kind()),
                destination,
            )
        };
        let disabled = self.saving;
        let draft = &self.draft;

        // Runs on: the one environment this client is connected to.
        let environment = h_flex()
            .min_w_0()
            .gap(px(8.))
            .child(svg().path(machine).size(px(16.)).flex_none().text_color(t.foreground))
            .child(div().min_w_0().truncate().child(env_label.clone()));
        let runs_on = select_trigger(&"scheduled-task-environment".into(), environment, 0., &t)
            .w_full()
            .disabled(self.task.is_some() || disabled)
            .dropdown_menu(move |menu, _, _| menu.item(PopupMenuItem::new(env_label.clone()).checked(true)));

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
                                this.draft.project_id = id.clone();
                                this.sync_worktree_project(window, cx);
                                cx.notify();
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
            let disabled = disabled || !connected || workspace.server_info.model_choices().is_empty();
            let trigger = workspace.model_picker_trigger(id.clone(), selection.as_ref(), None, cx).disabled(disabled);
            let on_pick: OnPick = Rc::new(move |_, picked, _, cx| {
                editor.update(cx, |this, cx| {
                    this.draft.model_key = format!("{}:{}", picked.instance_id, picked.model);
                    cx.notify();
                });
            });
            workspace.undocked_model_picker(
                id,
                trigger,
                PickHost { current: selection.clone(), include: |_| true, on_pick },
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
            toggle(SharedString::from(format!("schedule-mode-{label}")), *label, m == mode, false, blocked, &t).when(
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
        let can_save = connected && !disabled && !missing && tasks_loaded && !webhook_blocked;
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
                        match destination {
                            None => div()
                                .text_sm()
                                .text_color(t.muted_foreground)
                                .child("Each run starts a new thread with the workspace and model below."),
                            Some(title) => div().flex().flex_col().gap(px(4.)).text_sm().child(title).child(
                                div()
                                    .text_color(t.muted_foreground)
                                    .child("Each run continues this conversation, in its workspace and with its current model."),
                            ),
                        },
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
        .when(!disabled, |this| this.cursor_pointer())
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
