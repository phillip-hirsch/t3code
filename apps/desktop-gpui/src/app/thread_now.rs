//! The lines at the end of the timeline (`ThreadNow.tsx`, `ThreadStatusLine.tsx`) and
//! their actions (`useThreadLifecycle.tsx`, `useForceStopThread.ts`).

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _, h_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::{Target, Workspace, new_id};
use crate::inbox::{self, Facts};
use crate::theme::Tokens;
use crate::thread_now::{self, NowLine, StatusLine};
use crate::timeline::Row;

/// `BACKGROUND_WORK_STOP_RETRY_AFTER_MS`: the Claude adapter forces a stuck query closed after 10 s.
const BACKGROUND_STOP_RETRY: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct NowUi {
    lines: Vec<Rc<NowLine>>,
    /// The thread a Force stop is in flight for.
    force_stopping: Option<String>,
    /// The thread and tasks a background Stop was sent for; it stays pending while they run.
    background_stop: Option<(String, Vec<String>, bool)>,
    /// A wake or un-settle in flight, by thread.
    status_busy: Option<String>,
    _clock: Option<Task<()>>,
    _retry: Option<Task<()>>,
}

impl Workspace {
    /// The open thread's lines, in the web's order: what needs you, its work, its goal, its status.
    fn now_lines(&self) -> Vec<Rc<NowLine>> {
        let (Some(Target::Thread { id: thread_id, .. }), Some(projection)) =
            (&self.target, self.thread.as_ref().and_then(|t| t.state.projection.as_ref()))
        else {
            return Vec::new();
        };
        let Some(shell) = self.shell.threads.iter().find(|t| &t.id == thread_id) else { return Vec::new() };
        let working = projection.is_running();
        let stop = thread_now::run_stop(&projection.runs, &projection.turn_items);
        // The interrupt path also accepts a completed run while its provider still has background work.
        let tasks = if working { &[][..] } else { &shell.pending_background_tasks[..] };
        let stop = stop.filter(|stop| !stop.background || !tasks.is_empty());
        let children = inbox::child_thread_inputs(&self.shell.threads, thread_id);
        let parent_awaiting = shell.pending_runtime_request.is_some() || !projection.pending_approvals().is_empty();
        let child_input = (!children.is_empty() && !parent_awaiting).then(|| NowLine::ChildInput {
            title: thread_now::child_inputs_title(&children).into(),
            approvals: thread_now::has_pending_approval(children[0]),
            children: children.iter().map(|c| (c.id.clone().into(), c.title.clone().into())).collect(),
        });
        let background = thread_now::background_work(tasks, &projection.turn_items).map(NowLine::Background);
        let (attention, work) = match stop {
            Some(stop) if stop.unresponsive => (Some(NowLine::RunStop(stop)), background),
            Some(stop) => (child_input, Some(NowLine::RunStop(stop))),
            None => (child_input, background),
        };
        let goal = shell.goal.as_ref().map(|goal| NowLine::Goal(thread_now::goal(goal, working)));
        let now = super::sidebar::now_ms();
        let facts = Facts::new(shell, &Default::default());
        let status = if facts.is_snoozed(now) {
            shell
                .snoozed_until
                .as_deref()
                .and_then(crate::timestamp::parse_ms)
                .map(|until_ms| StatusLine::Snoozed { until_ms })
        } else if shell.settled_override.as_deref() == Some("settled") {
            Some(StatusLine::Settled { at_ms: shell.settled_at.as_deref().and_then(crate::timestamp::parse_ms) })
        } else if facts.is_woke(now) {
            facts.woke_at(now).map(|woke_at| StatusLine::Woke { woke_at: woke_at.to_owned().into() })
        } else {
            None
        };
        [attention, work, goal, status.map(NowLine::Status)].into_iter().flatten().map(Rc::new).collect()
    }

    /// The footer rows; also restarts the clock that moves their labels.
    pub(super) fn now_rows(&mut self, cx: &mut Context<Self>) -> Vec<Row> {
        let lines = self.now_lines();
        if lines != self.now.lines {
            self.now.lines = lines;
            self.now._clock = None;
            let now = super::sidebar::now_ms();
            let next = self.now.lines.iter().find_map(|line| match line.as_ref() {
                NowLine::Status(status) => status.label(now).1,
                _ => None,
            });
            if let Some(delay) = next {
                self.now._clock = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(delay.max(1000) as u64)).await;
                    let _ = this.update(cx, |this, cx| {
                        this.now.lines.clear();
                        this.refresh_rows(cx);
                        cx.notify();
                    });
                }));
            }
        }
        self.now.lines.iter().map(|line| Row::Now(line.clone())).collect()
    }

    /// Recomputes the footer after a shell update and splices it in when it changed.
    pub(super) fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.now_lines() != self.now.lines {
            self.refresh_rows(cx);
        }
    }

    /// `useForceStopThread`: for an agent that no longer answers Stop.
    fn force_stop(&mut self, run_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(Target::Thread { id: thread_id, .. })) = (self.client(), self.target.clone()) else {
            return;
        };
        if !self.server_info().capability("threadForceStop") || self.now.force_stopping.is_some() {
            return;
        }
        self.now.force_stopping = Some(thread_id.clone());
        cx.notify();
        let input = json!({"threadId": thread_id, "runId": run_id});
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.forceStopThread", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.now.force_stopping = None;
                match result {
                    Err(error) => window.push_notification(
                        Notification::error(error.to_string()).title("Could not force stop the agent"),
                        cx,
                    ),
                    Ok(result) => {
                        if let Some(note) = force_stop_notice(&result) {
                            window.push_notification(note, cx);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Stops the background work a finished run left running (`handleStopBackgroundWork`).
    fn stop_background_work(&mut self, cx: &mut Context<Self>) {
        let Some(Target::Thread { id: thread_id, .. }) = self.target.clone() else { return };
        if self.background_stop_pending(&thread_id) {
            return;
        }
        let Some(shell) = self.shell.threads.iter().find(|t| t.id == thread_id) else { return };
        let task_ids = shell.pending_background_tasks.iter().map(|t| t.task_id.clone()).collect();
        let command = if self.server_info().capability("threadStop") {
            json!({"type": "thread.stop", "commandId": new_id(), "threadId": thread_id})
        } else {
            let Some(run) = self.open_projection().and_then(|p| p.runs.last()) else { return };
            json!({"type": "run.interrupt", "commandId": new_id(), "threadId": thread_id, "runId": run.id, "holdQueue": true})
        };
        let Some(client) = self.client() else { return };
        self.now.background_stop = Some((thread_id.clone(), task_ids, false));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Err(error) => {
                        this.now.background_stop = None;
                        this.error = Some(error.to_string().into());
                    }
                    Ok(_) => {
                        if let Some(stop) = this.now.background_stop.as_mut() {
                            stop.2 = true;
                        }
                        this.now._retry = Some(cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(BACKGROUND_STOP_RETRY).await;
                            let _ = this.update(cx, |this, cx| {
                                this.now.background_stop = None;
                                cx.notify();
                            });
                        }));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `isBackgroundWorkStopPending`: an accepted Stop stays pending while its tasks are listed.
    fn background_stop_pending(&self, thread_id: &str) -> bool {
        let Some((stopped, task_ids, accepted)) = &self.now.background_stop else { return false };
        if stopped != thread_id {
            return false;
        }
        let tasks = self.shell.threads.iter().find(|t| t.id == thread_id).map(|t| &t.pending_background_tasks[..]);
        !accepted || tasks.unwrap_or_default().iter().any(|task| task_ids.contains(&task.task_id))
    }

    /// `sendStandaloneCommand`: a `/goal` command sent as its own message.
    fn send_goal_command(&mut self, text: &'static str, failure: &'static str, cx: &mut Context<Self>) {
        let (Some(client), Some(Target::Thread { id: thread_id, .. })) = (self.client(), self.target.clone()) else {
            return;
        };
        if self.sending {
            return;
        }
        let running = self.open_projection().is_some_and(|p| p.is_running());
        let mut command = json!({
            "type": "message.dispatch",
            "commandId": new_id(),
            "createdBy": "user",
            "creationSource": "web",
            "threadId": thread_id,
            "messageId": new_id(),
            "text": text,
            "attachments": [],
            "dispatchMode": {"type": "start_immediately"},
            "deliveryIntent": if running { "steer" } else { "auto" },
        });
        if let Some(model) = &self.model_override {
            command["modelSelection"] = model.to_json();
        }
        cx.spawn(async move |this, cx| {
            if client.request("orchestration.dispatchCommand", command).await.is_err() {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(failure.into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Wake now / Un-settle from the status line, with the web's failure toasts.
    fn change_status(&mut self, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(Target::Thread { id: thread_id, .. })) = (self.client(), self.target.clone()) else {
            return;
        };
        if self.now.status_busy.is_some() {
            return;
        }
        let Some(shell) = self.shell.threads.iter().find(|t| t.id == thread_id) else { return };
        // A snooze the usage-limit recovery set is undone through the recovery itself.
        let recovery = shell.limit_recovery.as_ref();
        let recovery_snooze = kind == "thread.unsnooze"
            && recovery.is_some_and(|r| {
                r["snooze"] == true
                    && r["runId"].as_str() == shell.latest_run_id.as_deref()
                    && r["resetAt"].as_str().and_then(crate::timestamp::parse_ms)
                        == shell.snoozed_until.as_deref().and_then(crate::timestamp::parse_ms)
            });
        let command = match recovery.filter(|_| recovery_snooze) {
            Some(r) => json!({"type": "thread.metadata.update", "commandId": new_id(), "threadId": thread_id,
                "limitRecovery": {"runId": r["runId"], "resetAt": r["resetAt"], "snooze": false}}),
            None => json!({"type": kind, "commandId": new_id(), "threadId": thread_id, "reason": "user"}),
        };
        let title = if kind == "thread.unsnooze" { "Failed to wake thread" } else { "Failed to un-settle thread" };
        self.now.status_busy = Some(thread_id);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.now.status_busy = None;
                if let Err(error) = result {
                    window.push_notification(Notification::error(error.to_string()).title(title), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `useAcknowledgeThreadWoke`: marks the thread visited at the time it woke.
    fn acknowledge_open_thread_woke(&mut self, woke_at: SharedString, cx: &mut Context<Self>) {
        let Some(Target::Thread { id: thread_id, .. }) = self.target.clone() else { return };
        let command =
            json!({"type": "thread.visit", "commandId": new_id(), "threadId": thread_id, "visitedAt": woke_at});
        self.dispatch(command, cx);
    }
}

/// The info toasts for a Force stop that could not end everything at once.
fn force_stop_notice(result: &Value) -> Option<Notification> {
    let others = result["sharedWithThreadIds"].as_array().map_or(0, Vec::len);
    match result["outcome"].as_str()? {
        "releasing" => Some(Notification::info("").title("Turn stopped; the agent is still shutting down")),
        "shared" => {
            let count = if others == 0 { "other".to_owned() } else { others.to_string() };
            let noun = if others == 1 { "thread" } else { "threads" };
            Some(
                Notification::info(format!("It is shared with {count} working {noun}. Stop those to free it."))
                    .title("Turn stopped; the agent process kept running"),
            )
        }
        _ => None,
    }
}

/// `InlineButton`: medium text that underlines on hover.
fn inline_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    color: Hsla,
    hover: Hsla,
    disabled: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .flex_none()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .when(disabled, |el| el.opacity(0.64))
        .when(!disabled, |el| {
            el.cursor_pointer()
                .hover(move |style| style.underline().text_color(hover))
                .on_click(move |_, window, cx| on_click(window, cx))
        })
        .child(label.into())
        .into_any_element()
}

fn clock(start_ms: i64) -> AnyElement {
    let elapsed = (super::sidebar::now_ms() - start_ms).max(1000);
    div().flex_none().child(crate::work_log::format_duration(elapsed / 1000 * 1000)).into_any_element()
}

/// One `ThreadNowLine` or `ThreadStatusLine`: icon, title and detail, then actions.
pub(super) fn now_row(line: &NowLine, this: &WeakEntity<Workspace>, t: &Tokens, cx: &mut App) -> AnyElement {
    let Some(workspace) = this.upgrade() else { return div().into_any_element() };
    let workspace = workspace.read(cx);
    let thread_id = match &workspace.target {
        Some(Target::Thread { id, .. }) => id.clone(),
        _ => String::new(),
    };
    let (fg, muted) = (t.foreground, t.muted_foreground);
    let act = |f: fn(&mut Workspace, &mut Window, &mut Context<Workspace>)| {
        let this = this.clone();
        move |window: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |workspace, cx| f(workspace, window, cx));
        }
    };
    let open = |id: SharedString| {
        let this = this.clone();
        move |window: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |workspace, cx| workspace.select_thread(id.to_string(), window, cx));
        }
    };
    let mut actions: Vec<AnyElement> = Vec::new();
    let (icon, icon_color, title, description): (IconName, Hsla, SharedString, Option<AnyElement>) = match line {
        NowLine::ChildInput { title, approvals, children } => {
            let names =
                h_flex().flex_wrap().gap(px(4.)).children(children.iter().enumerate().map(|(index, (id, name))| {
                    let label = if index + 1 < children.len() { format!("{name},") } else { name.to_string() };
                    inline_button(
                        ElementId::Name(format!("now-child:{id}").into()),
                        label,
                        muted,
                        fg,
                        false,
                        open(id.clone()),
                    )
                }));
            let label = if *approvals { "Open approval" } else { "Open question" };
            if let Some((first, _)) = children.first() {
                actions.push(inline_button("now-open-child", label, fg, fg, false, open(first.clone())));
            }
            let icon = if *approvals { IconName::ShieldQuestionMark } else { IconName::MessageCircle };
            (icon, t.info_foreground, title.clone(), Some(names.into_any_element()))
        }
        NowLine::RunStop(stop) => {
            actions.push(clock(stop.requested_at_ms));
            if stop.unresponsive && workspace.server_info().capability("threadForceStop") {
                let stopping = workspace.now.force_stopping.as_deref() == Some(thread_id.as_str());
                let run_id = stop.run_id.clone();
                let this = this.clone();
                actions.push(inline_button(
                    "now-force-stop",
                    if stopping { "Force stopping..." } else { "Force stop" },
                    fg,
                    fg,
                    stopping,
                    move |window, cx| {
                        let run_id = run_id.clone();
                        let _ = this.update(cx, |workspace, cx| workspace.force_stop(run_id, window, cx));
                    },
                ));
            }
            match stop.unresponsive {
                true => (
                    IconName::TriangleAlert,
                    t.warning_foreground,
                    "Agent not responding to Stop".into(),
                    Some(div().child("It may be short on memory. Force stop kills its processes.").into_any_element()),
                ),
                false => (IconName::CircleStop, muted, "Stopping".into(), None),
            }
        }
        NowLine::Background(work) => {
            actions.extend(work.started_at_ms.map(clock));
            if let Some(child) = &work.open_thread {
                actions.push(inline_button("now-open-background", "Open", muted, fg, false, open(child.clone())));
            }
            let stopping = workspace.background_stop_pending(&thread_id);
            actions.push(inline_button(
                "now-stop-background",
                if stopping { "Stopping..." } else { "Stop" },
                fg,
                fg,
                stopping,
                act(|w, _, cx| w.stop_background_work(cx)),
            ));
            let icon = if work.commands_only { IconName::Terminal } else { IconName::CircleDashed };
            (icon, muted, work.title.clone(), work.description.clone().map(|d| div().child(d).into_any_element()))
        }
        NowLine::Goal(goal) => {
            let working = workspace.open_projection().is_some_and(|p| p.is_running());
            if goal.can_resume && !working {
                actions.push(inline_button(
                    "now-goal-resume",
                    "Resume",
                    fg,
                    fg,
                    false,
                    act(|w, _, cx| w.send_goal_command("/goal resume", "Failed to resume goal.", cx)),
                ));
                actions.push(div().child("·").into_any_element());
            }
            actions.push(inline_button(
                "now-goal-clear",
                "Clear",
                muted,
                fg,
                false,
                act(|w, _, cx| w.send_goal_command("/goal clear", "Failed to clear goal.", cx)),
            ));
            (IconName::Target, muted, goal.title.clone(), Some(div().child(goal.objective.clone()).into_any_element()))
        }
        NowLine::Status(status) => {
            let busy = workspace.now.status_busy.as_deref() == Some(thread_id.as_str());
            let (label, _) = status.label(super::sidebar::now_ms());
            let (icon, action): (IconName, AnyElement) = match status {
                StatusLine::Snoozed { .. } => (
                    IconName::AlarmClock,
                    inline_button(
                        "now-status",
                        if busy { "Waking..." } else { "Wake now" },
                        fg,
                        fg,
                        busy,
                        act(|w, window, cx| w.change_status("thread.unsnooze", window, cx)),
                    ),
                ),
                StatusLine::Settled { .. } => (
                    IconName::CircleCheck,
                    inline_button(
                        "now-status",
                        if busy { "Un-settling..." } else { "Un-settle" },
                        fg,
                        fg,
                        busy,
                        act(|w, window, cx| w.change_status("thread.unsettle", window, cx)),
                    ),
                ),
                StatusLine::Woke { woke_at } => {
                    let (this, woke_at) = (this.clone(), woke_at.clone());
                    (
                        IconName::AlarmClock,
                        inline_button("now-status", "Dismiss", fg, fg, false, move |_, cx| {
                            let woke_at = woke_at.clone();
                            let _ = this.update(cx, |w, cx| w.acknowledge_open_thread_woke(woke_at, cx));
                        }),
                    )
                }
            };
            return super::chat::lane(
                0.,
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap(px(6.))
                    .px(px(4.))
                    .pb(px(8.))
                    .text_xs()
                    .text_color(muted)
                    .child(Icon::new(icon).size(px(14.)).flex_none())
                    .child(div().min_w_0().truncate().child(label))
                    .child(div().flex_none().child("·"))
                    .child(action),
            )
            .into_any_element();
        }
    };
    super::chat::lane(
        0.,
        h_flex()
            .w_full()
            .min_w_0()
            .gap(px(6.))
            .px(px(4.))
            .pb(px(8.))
            .text_xs()
            .text_color(muted)
            .child(Icon::new(icon).size(px(14.)).flex_none().text_color(icon_color))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap(px(6.))
                    .child(div().flex_none().max_w_full().truncate().child(title))
                    .children(description.map(|d| div().min_w_0().truncate().text_color(muted.opacity(0.6)).child(d))),
            )
            .when(!actions.is_empty(), |row| row.child(h_flex().flex_none().ml(px(6.)).gap(px(10.)).children(actions))),
    )
    .into_any_element()
}
