//! The lines at the end of the timeline (`ThreadNow.tsx`, `ThreadStatusLine.tsx`) and
//! their actions (`useThreadLifecycle.tsx`, `useForceStopThread.ts`).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, h_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, Font, FontWeight, HighlightStyle, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText, Task, TextRun,
    WeakEntity, Window, div, prelude::FluentBuilder as _, px, rems,
};
use serde_json::{Value, json};

use super::side_chat::ChatPane;
use super::{Workspace, new_id};
use crate::inbox::{self, Facts};
use crate::model::{ScopedId, ThreadShell};
use crate::theme::Tokens;
use crate::thread_now::{self, NowLine, StatusLine};
use crate::timeline::Row;

/// `BACKGROUND_WORK_STOP_RETRY_AFTER_MS`: the Claude adapter forces a stuck query closed after 10 s.
const BACKGROUND_STOP_RETRY: Duration = Duration::from_secs(10);

/// A thread's environment and id; ids repeat across environments.
type Key = (SharedString, String);

#[derive(Default)]
pub(super) struct NowUi {
    /// The open thread's footer; a side chat's is on its view.
    main: NowFooter,
    /// Threads a Force stop is in flight for.
    force_stopping: HashSet<Key>,
    /// The background Stops sent, by thread; each stays pending while its tasks run.
    background_stops: HashMap<Key, BackgroundStop>,
    /// Threads a wake or un-settle is in flight for.
    status_busy: HashSet<Key>,
}

/// One chat's footer lines and the clock that moves their labels.
#[derive(Default)]
pub(super) struct NowFooter {
    lines: Vec<Rc<NowLine>>,
    _clock: Option<Task<()>>,
}

struct BackgroundStop {
    task_ids: Vec<String>,
    accepted: bool,
    _retry: Option<Task<()>>,
}

impl NowUi {
    /// `isBackgroundWorkStopPending` for one thread, given the tasks its shell still lists: an
    /// accepted Stop stays pending while its tasks run.
    fn background_stop_pending<'a>(&self, key: &Key, listed: impl IntoIterator<Item = &'a str>) -> bool {
        let Some(stop) = self.background_stops.get(key) else { return false };
        !stop.accepted || listed.into_iter().any(|task| stop.task_ids.iter().any(|id| id == task))
    }
}

impl Workspace {
    fn now_shell(&self, (env, thread_id): &Key) -> Option<&ThreadShell> {
        self.shell.thread(&ScopedId::new(env.clone(), thread_id))
    }

    fn now_footer_mut(&mut self, pane: ChatPane) -> Option<&mut NowFooter> {
        match pane {
            ChatPane::Main => Some(&mut self.now.main),
            ChatPane::Side => self.side_chat.view.as_mut().map(|view| &mut view.now),
        }
    }

    /// The pane's lines, in the web's order: what needs you, its work, its goal, its status.
    fn now_lines(&self, pane: ChatPane) -> Vec<Rc<NowLine>> {
        let (Some(key), Some(projection)) = (self.pane_thread(pane), self.pane_projection(pane)) else {
            return Vec::new();
        };
        let Some(shell) = self.now_shell(&key) else { return Vec::new() };
        let (env, thread_id) = (&key.0, &key.1);
        let working = projection.is_running();
        let stop = thread_now::run_stop(&projection.runs, &projection.turn_items);
        // The interrupt path also accepts a completed run while its provider still has background work.
        let tasks = if working { &[][..] } else { &shell.pending_background_tasks[..] };
        let stop = stop.filter(|stop| !stop.background || !tasks.is_empty());
        let children = inbox::child_thread_inputs(&self.shell.threads, env, thread_id);
        let parent_awaiting = shell.pending_runtime_request.is_some() || !projection.pending_approvals().is_empty();
        let child_input = (!children.is_empty() && !parent_awaiting).then(|| NowLine::ChildInput {
            title: thread_now::child_inputs_title(&children).into(),
            approvals: thread_now::has_pending_approval(children[0]),
            children: children.iter().map(|c| (ScopedId::of_thread(c), c.title.clone().into())).collect(),
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
            // The open thread's usage-limit notice shows this snooze and its Wake now.
            shell
                .snoozed_until
                .as_deref()
                .and_then(crate::timestamp::parse_ms)
                .filter(|_| pane == ChatPane::Side || !self.limit_recovery_holds_snooze())
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

    /// The pane's footer rows; also restarts the clock that moves their labels.
    pub(super) fn now_rows(&mut self, pane: ChatPane, cx: &mut Context<Self>) -> Vec<Row> {
        let lines = self.now_lines(pane);
        let Some(footer) = self.now_footer_mut(pane) else { return Vec::new() };
        if lines != footer.lines {
            footer.lines = lines;
            footer._clock = None;
            let now = super::sidebar::now_ms();
            let next = footer.lines.iter().find_map(|line| match line.as_ref() {
                NowLine::Status(status) => status.label(now).1,
                _ => None,
            });
            if let Some(delay) = next {
                // A side chat's clock goes with its view, so it never refreshes another thread.
                footer._clock = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(delay.max(1000).unsigned_abs())).await;
                    let _ = this.update(cx, |this, cx| {
                        if let Some(footer) = this.now_footer_mut(pane) {
                            footer.lines.clear();
                        }
                        this.refresh_pane_rows(pane, cx);
                        cx.notify();
                    });
                }));
            }
        }
        footer.lines.iter().map(|line| Row::Now(line.clone())).collect()
    }

    /// Recomputes the footers after a shell update and splices one in when it changed, or when the
    /// recovery notice moved to another run and the limit rows it hides change with it.
    pub(super) fn sync_now(&mut self, cx: &mut Context<Self>) {
        if self.now_lines(ChatPane::Main) != self.now.main.lines
            || self.usage_limit_notice_run() != self.rows_notice_run
        {
            self.refresh_rows(cx);
        }
        if self.side_chat.view.as_ref().is_some_and(|view| self.now_lines(ChatPane::Side) != view.now.lines) {
            self.refresh_side_chat_rows(cx);
        }
    }

    /// `canForceStop`: the environment supports it and this connection may operate there.
    fn can_force_stop(&self, environment: &str) -> bool {
        self.server_info().capability("threadForceStop")
            && self.env_permits(environment, "orchestration.forceStopThread", &Value::Null)
    }

    /// `useForceStopThread`: for an agent that no longer answers Stop.
    fn force_stop(&mut self, pane: ChatPane, run_id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some((client, env, thread_id)) = self.pane_client(pane, cx) else { return };
        if !self.can_force_stop(&env) {
            return;
        }
        let key = (env, thread_id.clone());
        if !self.now.force_stopping.insert(key.clone()) {
            return;
        }
        cx.notify();
        let input = json!({"threadId": thread_id, "runId": run_id});
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.forceStopThread", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.now.force_stopping.remove(&key);
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
    fn stop_background_work(&mut self, pane: ChatPane, cx: &mut Context<Self>) {
        let Some(key) = self.pane_thread(pane) else { return };
        if self.background_stop_pending(&key) {
            return;
        }
        let Some(shell) = self.now_shell(&key) else { return };
        let (env, thread_id) = key.clone();
        let task_ids = shell.pending_background_tasks.iter().map(|t| t.task_id.clone()).collect();
        let command = if self.server_info().capability("threadStop") {
            json!({"type": "thread.stop", "commandId": new_id(), "threadId": thread_id})
        } else {
            let Some(run) = self.pane_projection(pane).and_then(|p| p.runs.last()) else { return };
            json!({"type": "run.interrupt", "commandId": new_id(), "threadId": thread_id, "runId": run.id, "holdQueue": true})
        };
        let Some((client, ..)) = self.pane_client(pane, cx) else { return };
        let stop = BackgroundStop { task_ids, accepted: false, _retry: None };
        self.now.background_stops.insert(key.clone(), stop);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Err(error) => {
                        this.now.background_stops.remove(&key);
                        if this.pane_shows(pane, &env, &thread_id) {
                            this.error = Some(error.to_string().into());
                        }
                    }
                    Ok(_) => {
                        let retry_key = key.clone();
                        let retry = cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(BACKGROUND_STOP_RETRY).await;
                            let _ = this.update(cx, |this, cx| {
                                this.now.background_stops.remove(&retry_key);
                                cx.notify();
                            });
                        });
                        if let Some(stop) = this.now.background_stops.get_mut(&key) {
                            stop.accepted = true;
                            stop._retry = Some(retry);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn background_stop_pending(&self, key: &Key) -> bool {
        let tasks = self.now_shell(key).map(|t| &t.pending_background_tasks[..]);
        self.now.background_stop_pending(key, tasks.unwrap_or_default().iter().map(|t| t.task_id.as_str()))
    }

    /// `sendStandaloneCommand`: a `/goal` command sent as its own message.
    fn send_goal_command(&mut self, pane: ChatPane, text: &'static str, failure: &'static str, cx: &mut Context<Self>) {
        if self.pane_sending(pane) {
            return;
        }
        let Some((client, env, thread_id)) = self.pane_client(pane, cx) else { return };
        let running = self.pane_projection(pane).is_some_and(super::super::model::ThreadProjection::is_running);
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
        if let Some(model) = self.pane_model(pane) {
            command["modelSelection"] = model.to_json();
        }
        cx.spawn(async move |this, cx| {
            if client.request("orchestration.dispatchCommand", command).await.is_err() {
                let _ = this.update(cx, |this, cx| {
                    if this.pane_shows(pane, &env, &thread_id) {
                        this.error = Some(failure.into());
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Wake now / Un-settle from the status line, with the web's failure toasts.
    fn change_status(&mut self, pane: ChatPane, kind: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((client, env, thread_id)) = self.pane_client(pane, cx) else { return };
        let key = (env, thread_id);
        if self.now.status_busy.contains(&key) {
            return;
        }
        let Some(shell) = self.now_shell(&key) else { return };
        let thread_id = &key.1;
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
        self.now.status_busy.insert(key.clone());
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.now.status_busy.remove(&key);
                if let Err(error) = result {
                    window.push_notification(Notification::error(error.to_string()).title(title), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `useAcknowledgeThreadWoke`: marks the thread visited at the time it woke.
    fn acknowledge_pane_woke(&mut self, pane: ChatPane, woke_at: SharedString, cx: &mut Context<Self>) {
        let Some((client, _, thread_id)) = self.pane_client(pane, cx) else { return };
        let command =
            json!({"type": "thread.visit", "commandId": new_id(), "threadId": thread_id, "visitedAt": woke_at});
        self.dispatch_in_order(client, vec![command], cx);
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
pub(super) fn inline_button(
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

fn clock_text(start_ms: i64) -> String {
    let elapsed = (super::sidebar::now_ms() - start_ms).max(1000);
    crate::work_log::format_duration(elapsed / 1000 * 1000)
}

const UNRESPONSIVE_DETAIL: &str = "It may be short on memory. Force stop kills its processes.";

/// `NoticeText`'s overflow check: the title and detail are wider than the line leaves them
/// beside its icon and actions (`texts`, with their weights).
fn notice_overflows(
    title: &str,
    detail: Option<&str>,
    texts: &[(String, FontWeight)],
    lane_width: f32,
    window: &Window,
) -> bool {
    let font = window.text_style().font();
    let size = rems(0.75).to_pixels(window.rem_size());
    let measure = |text: &str, weight: FontWeight| {
        let run = TextRun {
            len: text.len(),
            font: Font { weight, ..font.clone() },
            color: Hsla::default(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        f32::from(window.text_system().shape_line(text.to_owned().into(), size, &[run], None).width)
    };
    let needed =
        measure(title, FontWeight::NORMAL) + detail.map_or(0., |detail| 6. + measure(detail, FontWeight::NORMAL));
    let actions = match texts.len() {
        0 => 0.,
        count => 6. + texts.iter().map(|(text, weight)| measure(text, *weight)).sum::<f32>() + 10. * (count - 1) as f32,
    };
    // The lane's 4px insets, the 14px icon and its gap.
    needed > lane_width - 8. - 14. - 6. - actions + 0.5
}

/// `NoticeText`'s details button: the line's full text, on hover.
fn notice_details(title: SharedString, detail: Option<String>, t: &Tokens) -> AnyElement {
    let (fg, accent, detail_color) = (t.foreground, t.accent, t.muted_foreground.opacity(0.6));
    let text: SharedString = match &detail {
        Some(detail) => format!("{title} {detail}").into(),
        None => title.clone(),
    };
    let detail_range = detail.map(|_| title.len() + 1..text.len());
    div()
        .id("now-details")
        .flex_none()
        .size(px(24.))
        // A details button must not make its line taller than the others.
        .my(px(-6.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .text_color(t.muted_foreground)
        .hover(move |style| style.bg(accent).text_color(fg))
        .child(Icon::new(IconName::Info).size(px(14.)))
        .tooltip(move |window, cx| {
            let (text, detail_range) = (text.clone(), detail_range.clone());
            Tooltip::element(move |_, _| {
                let highlight = HighlightStyle { color: Some(detail_color), ..Default::default() };
                div().max_w(px(480.)).whitespace_normal().child(
                    StyledText::new(text.clone()).with_highlights(detail_range.clone().map(|range| (range, highlight))),
                )
            })
            .build(window, cx)
        })
        .into_any_element()
}

/// One `ThreadNowLine` or `ThreadStatusLine`: icon, title and detail, then actions.
pub(super) fn now_row(
    line: &NowLine,
    pane: ChatPane,
    this: &WeakEntity<Workspace>,
    lane_width: f32,
    t: &Tokens,
    window: &Window,
    cx: &mut App,
) -> AnyElement {
    let Some(workspace) = this.upgrade() else { return div().into_any_element() };
    let workspace = workspace.read(cx);
    let key = workspace.pane_thread(pane).unwrap_or_default();
    let (fg, muted) = (t.foreground, t.muted_foreground);
    let act = |f: fn(&mut Workspace, ChatPane, &mut Window, &mut Context<Workspace>)| {
        let this = this.clone();
        move |window: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |workspace, cx| f(workspace, pane, window, cx));
        }
    };
    let open = |child: ScopedId| {
        let this = this.clone();
        move |window: &mut Window, cx: &mut App| {
            let _ = this.update(cx, |workspace, cx| {
                workspace.select_thread_in(child.env.clone(), child.id.clone(), window, cx);
            });
        }
    };
    let mut actions: Vec<AnyElement> = Vec::new();
    // What the actions read, to measure the room they leave the text.
    let mut texts: Vec<(String, FontWeight)> = Vec::new();
    let clock = |start_ms: i64, texts: &mut Vec<(String, FontWeight)>| {
        let text = clock_text(start_ms);
        texts.push((text.clone(), FontWeight::NORMAL));
        div().flex_none().child(text).into_any_element()
    };
    let (icon, icon_color, title, description): (IconName, Hsla, SharedString, Option<AnyElement>) = match line {
        NowLine::ChildInput { title, approvals, children } => {
            let names =
                h_flex().flex_wrap().gap(px(4.)).children(children.iter().enumerate().map(|(index, (id, name))| {
                    let label = if index + 1 < children.len() { format!("{name},") } else { name.to_string() };
                    inline_button(
                        ElementId::Name(format!("now-child:{}:{}", id.env, id.id).into()),
                        label,
                        muted,
                        fg,
                        false,
                        open(id.clone()),
                    )
                }));
            let label = if *approvals { "Open approval" } else { "Open question" };
            if let Some((first, _)) = children.first() {
                texts.push((label.into(), FontWeight::MEDIUM));
                actions.push(inline_button("now-open-child", label, fg, fg, false, open(first.clone())));
            }
            let icon = if *approvals { IconName::ShieldQuestionMark } else { IconName::MessageCircle };
            (icon, t.info_foreground, title.clone(), Some(names.into_any_element()))
        }
        NowLine::RunStop(stop) => {
            actions.push(clock(stop.requested_at_ms, &mut texts));
            if stop.unresponsive && workspace.can_force_stop(&key.0) {
                let stopping = workspace.now.force_stopping.contains(&key);
                let run_id = stop.run_id.clone();
                let this = this.clone();
                let label = if stopping { "Force stopping..." } else { "Force stop" };
                texts.push((label.into(), FontWeight::MEDIUM));
                actions.push(inline_button("now-force-stop", label, fg, fg, stopping, move |window, cx| {
                    let run_id = run_id.clone();
                    let _ = this.update(cx, |workspace, cx| workspace.force_stop(pane, run_id, window, cx));
                }));
            }
            if stop.unresponsive {
                (
                    IconName::TriangleAlert,
                    t.warning_foreground,
                    "Agent not responding to Stop".into(),
                    Some(div().child(UNRESPONSIVE_DETAIL).into_any_element()),
                )
            } else {
                (IconName::CircleStop, muted, "Stopping".into(), None)
            }
        }
        NowLine::Background(work) => {
            if let Some(start_ms) = work.started_at_ms {
                actions.push(clock(start_ms, &mut texts));
            }
            // A provider's background subagent lives on its parent's environment.
            if let Some(child) = &work.open_thread {
                texts.push(("Open".into(), FontWeight::MEDIUM));
                let child = ScopedId::new(key.0.clone(), child.as_ref());
                actions.push(inline_button("now-open-background", "Open", muted, fg, false, open(child)));
            }
            let stopping = workspace.background_stop_pending(&key);
            let label = if stopping { "Stopping..." } else { "Stop" };
            texts.push((label.into(), FontWeight::MEDIUM));
            actions.push(inline_button(
                "now-stop-background",
                label,
                fg,
                fg,
                stopping,
                act(|w, pane, _, cx| w.stop_background_work(pane, cx)),
            ));
            let icon = if work.commands_only { IconName::Terminal } else { IconName::CircleDashed };
            (icon, muted, work.title.clone(), work.description.clone().map(|d| div().child(d).into_any_element()))
        }
        NowLine::Goal(goal) => {
            let working =
                workspace.pane_projection(pane).is_some_and(super::super::model::ThreadProjection::is_running);
            if goal.can_resume && !working {
                texts.extend([("Resume".into(), FontWeight::MEDIUM), ("·".into(), FontWeight::NORMAL)]);
                actions.push(inline_button(
                    "now-goal-resume",
                    "Resume",
                    fg,
                    fg,
                    false,
                    act(|w, pane, _, cx| w.send_goal_command(pane, "/goal resume", "Failed to resume goal.", cx)),
                ));
                actions.push(div().child("·").into_any_element());
            }
            texts.push(("Clear".into(), FontWeight::MEDIUM));
            actions.push(inline_button(
                "now-goal-clear",
                "Clear",
                muted,
                fg,
                false,
                act(|w, pane, _, cx| w.send_goal_command(pane, "/goal clear", "Failed to clear goal.", cx)),
            ));
            (IconName::Target, muted, goal.title.clone(), Some(div().child(goal.objective.clone()).into_any_element()))
        }
        NowLine::Status(status) => {
            let busy = workspace.now.status_busy.contains(&key);
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
                        act(|w, pane, window, cx| w.change_status(pane, "thread.unsnooze", window, cx)),
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
                        act(|w, pane, window, cx| w.change_status(pane, "thread.unsettle", window, cx)),
                    ),
                ),
                StatusLine::Woke { woke_at } => {
                    let (this, woke_at) = (this.clone(), woke_at.clone());
                    (
                        IconName::AlarmClock,
                        inline_button("now-status", "Dismiss", fg, fg, false, move |_, cx| {
                            let woke_at = woke_at.clone();
                            let _ = this.update(cx, |w, cx| w.acknowledge_pane_woke(pane, woke_at, cx));
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
    let detail = match line {
        NowLine::ChildInput { children, .. } => {
            Some(children.iter().map(|(_, name)| name.to_string()).collect::<Vec<_>>().join(", "))
        }
        NowLine::RunStop(stop) if stop.unresponsive => Some(UNRESPONSIVE_DETAIL.to_owned()),
        NowLine::Background(work) => work.description.as_ref().map(ToString::to_string),
        NowLine::Goal(goal) => Some(goal.objective.to_string()),
        _ => None,
    };
    // When the text is cut, its details button shows all of it.
    let details = notice_overflows(&title, detail.as_deref(), &texts, lane_width, window)
        .then(|| notice_details(title.clone(), detail, t));
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
            .children(details)
            .when(!actions.is_empty(), |row| row.child(h_flex().flex_none().ml(px(6.)).gap(px(10.)).children(actions))),
    )
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_stops_are_pending_per_thread() {
        let mut ui = NowUi::default();
        let key = |env: &str, id: &str| (SharedString::from(env.to_owned()), id.to_owned());
        let stop = |accepted| BackgroundStop { task_ids: vec!["t1".into()], accepted, _retry: None };
        ui.background_stops.insert(key("a", "main"), stop(true));
        ui.background_stops.insert(key("a", "side"), stop(false));
        assert!(ui.background_stop_pending(&key("a", "main"), ["t1"]), "its task still runs");
        assert!(!ui.background_stop_pending(&key("a", "main"), ["t2"]), "its tasks ended");
        assert!(ui.background_stop_pending(&key("a", "side"), []), "not accepted yet");
        assert!(!ui.background_stop_pending(&key("a", "other"), ["t1"]), "another thread's Stop is not this one's");
        assert!(!ui.background_stop_pending(&key("b", "side"), []), "nor is the same id's on another environment");
    }
}
