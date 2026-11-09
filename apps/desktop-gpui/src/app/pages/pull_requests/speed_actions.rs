//! Shift's quick actions on list rows (`PullRequestSpeedActions`): Close, Merge, Ready for review
//! and Reopen act at once, and dragging from a row's Close across others closes them together
//! (`usePullRequestCloseBatch`, the close sweep in `PullRequestsPage`).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, DispatchPhase, ElementId, Entity, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement, KeyDownEvent, ModifiersChangedEvent, MouseButton, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Point, Styled as _, Window, canvas, px,
};
use serde_json::{Value, json};

use super::super::pull_request_detail::{toast_error, toast_success};
use super::{Row, reference_of};
use crate::app::git::SOURCE_CONTROL_WRITE;
use crate::app::{Workspace, keymap};
use crate::pull_request_detail::{self as detail_logic, Detail};
use crate::pull_requests::{self as logic, Entry, State};
use crate::theme::Tokens;

/// The pointer travel that turns a press on Close into a sweep (`distance: 6`).
const SWEEP_DISTANCE: f32 = 6.;

/// The quick actions' state on the Pull requests page.
#[derive(Default)]
pub(super) struct SpeedActions {
    /// Shift alone is held outside a text field (`PullRequestSpeedModeProvider`).
    pub(super) shift: bool,
    /// The action running on each row, by its key (`actionPending`).
    acting: HashMap<String, &'static str>,
    /// Rows a close batch is closing (`closingKeys`).
    closing: HashSet<String>,
    sweep: Option<Sweep>,
}

impl SpeedActions {
    /// Forgets a held Shift and a press whose release the page was away for.
    pub(super) fn reset(&mut self) {
        self.shift = false;
        self.sweep = None;
    }
}

/// A press on a row's Close: a click until the pointer travels, then a sweep across rows.
struct Sweep {
    origin: String,
    start: Point<Pixels>,
    started: bool,
    target: Option<String>,
    swept: Vec<String>,
}

/// One row's quick actions as the list draws them.
pub(super) struct RowSpeed {
    actions: Rc<[&'static str]>,
    shown: bool,
    busy: bool,
    writable: bool,
}

fn label(action: &str) -> &'static str {
    match action {
        "close" => "Close",
        "merge" => "Merge",
        "ready" => "Ready for review",
        _ => "Reopen",
    }
}

fn icon(action: &str) -> Lucide {
    match action {
        "close" => Lucide::GitPullRequestClosed,
        "merge" => Lucide::GitMerge,
        _ => Lucide::GitPullRequestArrow,
    }
}

impl Workspace {
    /// Shift alone shows the quick actions, unless a text field has focus.
    pub(in crate::app) fn pull_request_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.page != Some(super::super::Page::PullRequests) && !self.pull_requests.speed.shift {
            return;
        }
        let m = &event.modifiers;
        // `useShortcutModifierState(true)`: Shift in a text field is typing, not speed mode.
        let typing = keymap::focus_in(&window.context_stack(), keymap::INPUT_CONTEXT);
        let shift = m.shift && !m.control && !m.alt && !m.platform && !typing;
        if shift != self.pull_requests.speed.shift {
            self.pull_requests.speed.shift = shift;
            cx.notify();
        }
    }

    /// The quick actions a listed row draws, from its rebuilt row data.
    pub(super) fn row_speed(
        &self,
        entry: &Entry,
        actions: &Rc<[&'static str]>,
        focus: &FocusHandle,
        window: &Window,
        cx: &App,
    ) -> RowSpeed {
        let speed = &self.pull_requests.speed;
        let key = entry.key();
        let swept = speed.sweep.as_ref().is_some_and(|s| s.swept.contains(&key));
        let busy = speed.acting.contains_key(&key) || speed.closing.contains(&key) || swept;
        // A focused action keeps its group open once Shift is let go, so Enter can press it.
        let focused_inside = focus.contains_focused(window, cx) && !focus.is_focused(window);
        RowSpeed {
            actions: actions.clone(),
            shown: !actions.is_empty() && (speed.shift || busy || focused_inside),
            busy,
            writable: self.env_grant(&entry.environment_id, SOURCE_CONTROL_WRITE) == Some(true),
        }
    }

    fn listed_entry(&self, key: &str) -> Option<Rc<Entry>> {
        self.pull_requests.rows.iter().find_map(|row| match row {
            Row::Entry { entry, .. } if entry.key() == key => Some(entry.clone()),
            _ => None,
        })
    }

    /// The listed rows' keys in order, and those a sweep from `origin` may close (`canClose`):
    /// open, closable on their host and connection, idle, and in the origin's group.
    fn sweep_candidates(&self, origin: &str) -> (Vec<String>, HashMap<String, Rc<Entry>>) {
        let speed = &self.pull_requests.speed;
        let listed: Vec<(String, &Rc<Entry>, usize, bool)> = self
            .pull_requests
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Entry { entry, group, quick, .. } => Some((entry.key(), entry, *group, quick.contains(&"close"))),
                _ => None,
            })
            .collect();
        let Some(group) = listed.iter().find(|(key, ..)| key == origin).map(|row| row.2) else {
            return (Vec::new(), HashMap::new());
        };
        let closable = listed
            .iter()
            .filter(|(key, entry, row_group, closes)| {
                *row_group == group
                    && *closes
                    && entry.state == State::Open
                    && self.env_grant(&entry.environment_id, SOURCE_CONTROL_WRITE) == Some(true)
                    && !speed.closing.contains(key)
                    && !speed.acting.contains_key(key)
            })
            .map(|(key, entry, ..)| (key.clone(), Rc::clone(entry)))
            .collect();
        (listed.into_iter().map(|(key, ..)| key).collect(), closable)
    }

    /// Ends a sweep without closing anything (Escape, or a new list under it).
    pub(super) fn cancel_close_sweep(&mut self, cx: &mut Context<Self>) {
        if self.pull_requests.speed.sweep.take().is_some() {
            cx.notify();
        }
    }

    fn start_close_sweep(&mut self, key: String, start: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.sweep_candidates(&key).1.contains_key(&key) {
            return;
        }
        self.pull_requests.speed.sweep =
            Some(Sweep { origin: key, start, started: false, target: None, swept: Vec::new() });
        cx.notify();
    }

    fn move_close_sweep(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(sweep) = self.pull_requests.speed.sweep.as_mut() else { return };
        if !sweep.started {
            let (dx, dy) = (f32::from(position.x - sweep.start.x), f32::from(position.y - sweep.start.y));
            if dx.hypot(dy) < SWEEP_DISTANCE {
                return;
            }
            sweep.started = true;
            let origin = sweep.origin.clone();
            self.sweep_close_to(&origin, cx);
        }
        if let Some(key) = self.sweep_row_at(position.y) {
            self.sweep_close_to(&key, cx);
        }
    }

    /// The listed row level with `y`, held inside the list so a pointer past its edge takes the
    /// edge row.
    fn sweep_row_at(&self, y: Pixels) -> Option<String> {
        let state = &self.pull_requests;
        let bounds = state.list_state.viewport_bounds();
        let y = f32::from(y).max(f32::from(bounds.top())).min(f32::from(bounds.bottom()) - 1.);
        let first = state.list_state.logical_scroll_top().item_ix;
        let rows = state
            .rows
            .iter()
            .enumerate()
            .skip(first)
            .map_while(|(ix, row)| Some((row, state.list_state.bounds_for_item(ix)?)))
            .filter_map(|(row, bounds)| match row {
                Row::Entry { entry, .. } => Some((entry.key(), f32::from(bounds.top()))),
                _ => None,
            });
        logic::row_at(rows, y)
    }

    /// The pointer is over `key`'s row while a sweep runs.
    fn sweep_close_to(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(sweep) = self.pull_requests.speed.sweep.as_ref() else { return };
        if !sweep.started || sweep.target.as_deref() == Some(key) {
            return;
        }
        let origin = sweep.origin.clone();
        let (ordered, closable) = self.sweep_candidates(&origin);
        let swept = logic::sweep_keys(&ordered, &origin, key, |candidate| closable.contains_key(candidate));
        if let Some(sweep) = self.pull_requests.speed.sweep.as_mut() {
            sweep.target = Some(key.to_owned());
            sweep.swept = swept;
        }
        cx.notify();
    }

    /// The press ends: a sweep closes what it crossed, and a press that stayed put is Close's click.
    fn end_close_sweep(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sweep) = self.pull_requests.speed.sweep.take() else { return };
        cx.notify();
        if sweep.started {
            let mut closable = self.sweep_candidates(&sweep.origin).1;
            let batch: Vec<Rc<Entry>> = sweep.swept.iter().filter_map(|key| closable.remove(key)).collect();
            self.close_pull_requests(batch, window, cx);
            return;
        }
        let (dx, dy) = (f32::from(position.x - sweep.start.x), f32::from(position.y - sweep.start.y));
        if dx.hypot(dy) < SWEEP_DISTANCE
            && let Some(entry) = self.listed_entry(&sweep.origin)
        {
            self.run_quick_action(&entry, "close", window, cx);
        }
    }

    /// `perform`: one row's action, with the merge method chosen from a fresh read of it.
    fn run_quick_action(
        &mut self,
        entry: &Rc<Entry>,
        action: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = entry.key();
        let speed = &self.pull_requests.speed;
        if speed.acting.contains_key(&key) || speed.closing.contains(&key) {
            return;
        }
        if self.env_grant(&entry.environment_id, SOURCE_CONTROL_WRITE) != Some(true) {
            return;
        }
        let Some(client) = self.env_client(&entry.environment_id) else { return };
        let reference = reference_of(entry);
        let merging = (action == "merge")
            .then(|| (self.pr_project_merge_method(&reference), self.pull_requests.last_merge_method));
        let term = detail_logic::terminology(&entry.provider).0;
        let mut input = entry.reference();
        self.pull_requests.speed.acting.insert(key.clone(), action);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result: Result<Value, String> = async {
                if let Some((project_default, last)) = merging {
                    let mut read = input.clone();
                    read["allowStale"] = json!(false);
                    let detail = client
                        .request("pullRequests.detail", read.clone())
                        .await
                        .map_err(|e| logic::error_message(&e))?;
                    let detail: Detail = serde_json::from_value(detail).map_err(|e| e.to_string())?;
                    let method = detail_logic::quick_merge_method(&detail, project_default.as_deref(), last)?;
                    if detail.capabilities.stack_actions == Some(true) {
                        let stack =
                            client.request("pullRequests.stack", read).await.map_err(|e| logic::error_message(&e))?;
                        if !stack.is_null() {
                            return Err("Open this pull request to merge its stack.".to_owned());
                        }
                    }
                    input["mergeMethod"] = json!(method);
                }
                input["action"] = json!(action);
                input["replyWithSummary"] = json!(true);
                client.request("pullRequests.runAction", input).await.map_err(|e| logic::error_message(&e))
            }
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pull_requests.speed.acting.remove(&key);
                match result {
                    Err(message) => {
                        let (title, hint) = detail_logic::action_failure(action, term);
                        toast_error(window, cx, title, detail_logic::readable_failure(&message, &hint));
                    }
                    Ok(reply) => {
                        toast_success(window, cx, detail_logic::action_success(action, term));
                        this.apply_pr_action_reply(&reference.key(), &reference, &reply, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `usePullRequestCloseBatch`: closes the rows together and reports them in one toast.
    fn close_pull_requests(&mut self, entries: Vec<Rc<Entry>>, window: &mut Window, cx: &mut Context<Self>) {
        let speed = &mut self.pull_requests.speed;
        let batch: Vec<Rc<Entry>> =
            entries.into_iter().filter(|e| e.state == State::Open && speed.closing.insert(e.key())).collect();
        let sends: Vec<_> = batch
            .iter()
            .map(|entry| {
                let writable = self.env_grant(&entry.environment_id, SOURCE_CONTROL_WRITE) == Some(true);
                let client = self.env_client(&entry.environment_id).filter(|_| writable);
                let mut input = entry.reference();
                input["action"] = json!("close");
                input["replyWithSummary"] = json!(true);
                (entry.clone(), client, input)
            })
            .collect();
        if sends.is_empty() {
            return;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let replies = futures::future::join_all(sends.into_iter().map(|(entry, client, input)| async move {
                let result = match client {
                    Some(client) => {
                        client.request("pullRequests.runAction", input).await.map_err(|e| logic::error_message(&e))
                    }
                    None => Err("This connection cannot change source control.".to_owned()),
                };
                (entry, result)
            }))
            .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let total = replies.len();
                let mut failures = Vec::new();
                for (entry, result) in replies {
                    this.pull_requests.speed.closing.remove(&entry.key());
                    let term = detail_logic::terminology(&entry.provider).0;
                    match result {
                        Ok(reply) => {
                            let reference = reference_of(&entry);
                            this.apply_pr_action_reply(&reference.key(), &reference, &reply, cx);
                        }
                        Err(message) => {
                            let hint = detail_logic::action_failure("close", term).1;
                            failures.push(format!(
                                "#{}: {}",
                                entry.number,
                                detail_logic::readable_failure(&message, &hint)
                            ));
                        }
                    }
                }
                let (failed, title, description) = logic::close_batch_toast(total, &failures);
                match (failed, description) {
                    (true, Some(description)) => toast_error(window, cx, title, description),
                    _ => toast_success(window, cx, title),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// While Close is pressed: follows the pointer anywhere in the window, and Escape ends it.
    pub(super) fn close_sweep_tracker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.pull_requests.speed.sweep.as_ref()?;
        let view = cx.entity();
        Some(
            canvas(
                |_, _, _| {},
                move |_, (), window, _| {
                    let moved = view.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            moved.update(cx, |this, cx| this.move_close_sweep(event.position, cx));
                        }
                    });
                    let released = view.clone();
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                            released.update(cx, |this, cx| this.end_close_sweep(event.position, window, cx));
                        }
                    });
                    window.on_key_event(move |event: &KeyDownEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture && event.keystroke.key == "escape" {
                            cx.stop_propagation();
                            view.update(cx, Workspace::cancel_close_sweep);
                        }
                    });
                },
            )
            .absolute()
            .size_0()
            .into_any_element(),
        )
    }
}

/// `SpeedModeGroup`: the row's quick actions, shown while Shift is held or one is running.
pub(super) fn speed_group(
    entry: &Rc<Entry>,
    speed: &RowSpeed,
    view: &Entity<Workspace>,
    t: &Tokens,
    window: &mut Window,
) -> Option<AnyElement> {
    if !speed.shown {
        return None;
    }
    let key = entry.key();
    let term = detail_logic::terminology(&entry.provider).0;
    let buttons = speed.actions.iter().map(|&action| {
        let stacked = action == "merge" && entry.stack.is_some();
        let tooltip = if stacked {
            format!("Open this {term} to merge its stack")
        } else if action == "close" {
            "Close immediately, or drag across rows to close several".to_owned()
        } else {
            format!("{} immediately", label(action))
        };
        let glyph = if speed.busy {
            Spinner::new().xsmall().into_any_element()
        } else {
            Icon::new(icon(action)).with_size(px(12.)).into_any_element()
        };
        let button = Button::new(ElementId::Name(format!("pr-{key}-quick-{action}").into()))
            .xsmall()
            .h(px(24.))
            .px(px(8.))
            .gap(px(4.))
            .rounded(px(6.))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .map(|b| if action == "close" { b.danger().outline() } else { b.outline().border_color(t.input) })
            .disabled(!speed.writable || speed.busy || stacked)
            .tooltip(tooltip)
            .child(glyph)
            .child(label(action));
        let pressed = entry.clone();
        let swept = key.clone();
        button
            // The row under it must not open on this press.
            .on_mouse_down(
                MouseButton::Left,
                window.listener_for(view, move |this, event: &gpui_kit::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if action == "close" {
                        this.start_close_sweep(swept.clone(), event.position, cx);
                    }
                }),
            )
            .on_click(window.listener_for(view, move |this, event: &gpui_kit::ClickEvent, window, cx| {
                cx.stop_propagation();
                // A pointer's Close is the end of its press, which the sweep reads.
                if action != "close" || event.is_keyboard() {
                    this.run_quick_action(&pressed, action, window, cx);
                }
            }))
    });
    Some(h_flex().flex_none().ml(px(4.)).gap(px(4.)).children(buttons).into_any_element())
}
