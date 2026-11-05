//! The queue control after the timeline (`QueuedRunsControl.tsx`) and editing a
//! queued message in the composer (`useQueuedRunEdit.ts`).

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div, prelude::FluentBuilder as _, px, relative,
};
use serde_json::{Value, json};

use super::attachments::{ATTACHMENT_ONLY_PROMPT, Attachment, MAX_ATTACHMENTS};
use super::{Target, Workspace, new_id};
use crate::queue::{self, QueueState, QueuedMessage};
use crate::theme::Tokens;
use crate::timeline::Row;

// A longer queue shows its first messages and a button that shows the rest.
const VISIBLE_ROWS: usize = 3;

gpui_kit::actions!(workspace, [SteerQueuedMessage, EditQueuedMessage]);

#[derive(Default)]
pub(super) struct QueueUi {
    state: Rc<QueueState>,
    show_all: bool,
    /// The run a send or remove is in flight for.
    busy: Option<SharedString>,
    edit: Option<QueueEdit>,
}

/// A queued message open in the composer; the thread's own draft waits in `saved`.
pub(super) struct QueueEdit {
    thread_id: String,
    run_id: SharedString,
    original: String,
    attachments: Vec<Value>,
    records: Vec<Value>,
    saved: (String, Vec<Attachment>),
    saving: bool,
}

/// The rows after the timeline: each shown queued message, then "Show all N queued".
pub(crate) fn rows(state: &QueueState, show_all: bool) -> Vec<Row> {
    let total = state.queued.len();
    let collapsible = total > VISIBLE_ROWS;
    let shown = if collapsible && !show_all { VISIBLE_ROWS - 1 } else { total };
    let mut rows: Vec<Row> = state.queued[..shown]
        .iter()
        .enumerate()
        .map(|(position, message)| Row::Queued { message: Rc::new(message.clone()), last: position + 1 == shown })
        .collect();
    if collapsible {
        rows.push(Row::QueueToggle { total, expanded: show_all });
    }
    rows
}

impl Workspace {
    /// The queue rows for the open thread; also ends an edit whose run left the queue.
    pub(super) fn queue_rows(&mut self) -> Vec<Row> {
        let archived = self.thread.as_ref().is_some_and(|thread| {
            self.shell.threads.iter().any(|t| t.id == thread.id && (t.archived_at.is_some() || t.deleted_at.is_some()))
        });
        let state = match self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) {
            Some(projection) => queue::derive(projection, archived),
            None => QueueState::default(),
        };
        if *self.queue.state != state {
            self.queue.state = Rc::new(state);
        }
        rows(&self.queue.state, self.queue.show_all)
    }

    pub(super) fn editing_queued(&self) -> bool {
        self.queue.edit.is_some()
    }

    pub(super) fn saving_queue_edit(&self) -> bool {
        self.queue.edit.as_ref().is_some_and(|edit| edit.saving)
    }

    /// The thread's draft, while a queued message holds the composer; see `stash_composer`.
    pub(super) fn take_queue_edit_draft(&mut self) -> Option<(String, Vec<Attachment>)> {
        self.queue.edit.take().map(|edit| edit.saved)
    }

    fn edit_blocked_reason(&self) -> Option<&'static str> {
        self.question.is_some().then_some("Answer the question to edit queued messages")
    }

    fn toggle_queue(&mut self, cx: &mut Context<Self>) {
        self.queue.show_all = !self.queue.show_all;
        self.refresh_rows(cx);
        cx.notify();
    }

    /// "Send now": starts a turn when idle, else steers it into the running one.
    fn send_queued(&mut self, run_id: SharedString, cx: &mut Context<Self>) {
        let state = self.queue.state.clone();
        if !state.can_send() || self.queue.busy.is_some() {
            return;
        }
        let Some(thread_id) = self.thread.as_ref().map(|t| t.id.clone()) else { return };
        let command = match (state.can_send_now, &state.active_run_id) {
            (true, _) => json!({"type": "queued-run.start", "runId": run_id}),
            (false, Some(active)) => {
                json!({"type": "queued-message.promote-to-steer", "queuedRunId": run_id, "targetRunId": active})
            }
            (false, None) => return,
        };
        self.dispatch_queue_command(run_id, thread_id, command, cx);
    }

    fn remove_queued(&mut self, run_id: SharedString, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread.as_ref().map(|t| t.id.clone()) else { return };
        let command = json!({"type": "queued-run.cancel", "runId": run_id});
        self.dispatch_queue_command(run_id, thread_id, command, cx);
    }

    fn dispatch_queue_command(
        &mut self,
        run_id: SharedString,
        thread_id: String,
        mut command: Value,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client() else { return };
        command["commandId"] = new_id().into();
        command["threadId"] = thread_id.into();
        self.queue.busy = Some(run_id);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update(cx, |this, cx| {
                this.queue.busy = None;
                if let Err(error) = result {
                    this.error = Some(error.to_string().into());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens a queued message in the composer, setting the thread's draft aside.
    fn begin_queue_edit(&mut self, message: &QueuedMessage, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread.as_ref().map(|t| t.id.clone()) else { return };
        if !message.editable || self.queue.busy.is_some() || self.edit_blocked_reason().is_some() {
            return;
        }
        let item = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()).and_then(|projection| {
            projection.turn_items.iter().rev().find(|item| {
                item.item_type == "user_message" && item.str("messageId") == Some(message.message_id.as_ref())
            })
        });
        let attachments =
            item.and_then(|i| i.raw.get("attachments")).and_then(Value::as_array).cloned().unwrap_or_default();
        let records =
            item.and_then(|i| i.raw.pointer("/context/records")).and_then(Value::as_array).cloned().unwrap_or_default();
        let saved = match self.queue.edit.take() {
            Some(edit) => edit.saved,
            None => (self.composer.read(cx).value().to_string(), std::mem::take(&mut self.attachments)),
        };
        self.attachments.clear();
        let text = queue::edit_text(&message.text).to_owned();
        self.queue.edit = Some(QueueEdit {
            thread_id,
            run_id: message.run_id.clone(),
            original: text.clone(),
            attachments,
            records,
            saved,
            saving: false,
        });
        self.queue.show_all = true;
        self.set_composer_text(text, window, cx);
        self.refresh_rows(cx);
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn cancel_queue_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((text, attachments)) = self.take_queue_edit_draft() {
            self.attachments = attachments;
            self.set_composer_text(text, window, cx);
            self.focus_composer(window, cx);
        }
        cx.notify();
    }

    /// The composer's send while editing: `queued-run.edit` with the new text, plus
    /// any attachments added in the composer (`submitQueuedRunEdit`).
    pub(super) fn save_queue_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let Some(edit) = self.queue.edit.as_ref().filter(|edit| !edit.saving) else { return };
        let text = self.composer.read(cx).value().trim().to_owned();
        if edit.attachments.len() + self.attachments.len() > MAX_ATTACHMENTS {
            self.error = Some(format!("A message can have at most {MAX_ATTACHMENTS} attachments.").into());
            cx.notify();
            return;
        }
        if text.is_empty() && edit.attachments.is_empty() && self.attachments.is_empty() {
            return;
        }
        let mut command = json!({
            "type": "queued-run.edit",
            "commandId": new_id(),
            "threadId": edit.thread_id,
            "runId": edit.run_id,
        });
        let mut has_attachments = !edit.attachments.is_empty();
        if self.has_attachments() {
            let (added, context) = match self.attachment_payload(&text) {
                Ok(payload) => payload,
                Err(reason) => {
                    self.error = Some(reason.into());
                    cx.notify();
                    return;
                }
            };
            has_attachments = true;
            let added_records = context.as_ref().and_then(|c| c["records"].as_array()).cloned().unwrap_or_default();
            command["attachments"] = edit.attachments.iter().cloned().chain(added).collect::<Vec<_>>().into();
            command["context"] = json!({"version": 1, "records": merge_records(&edit.records, added_records)});
        }
        command["text"] =
            if text.is_empty() && has_attachments { ATTACHMENT_ONLY_PROMPT.to_owned() } else { text }.into();
        let run_id = edit.run_id.clone();
        if let Some(edit) = self.queue.edit.as_mut() {
            edit.saving = true;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                // The edit was cancelled or begun again while this save ran.
                let current = this.queue.edit.as_mut().filter(|edit| edit.run_id == run_id);
                match (result, current) {
                    (Ok(_), Some(_)) => this.cancel_queue_edit(window, cx),
                    (Err(error), current) => {
                        log::warn!("saving the edited queued message: {error}");
                        if let Some(edit) = current {
                            edit.saving = false;
                        }
                        this.error = Some("Could not save the edited queued message.".into());
                    }
                    (Ok(_), None) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The run started or was cancelled elsewhere: a changed edit moves into the
    /// thread's draft when that is empty, else it's dropped (`onRunLeftQueue`).
    pub(super) fn sync_queue_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.queue.edit.as_ref().filter(|edit| !edit.saving) else { return };
        if self.queue.state.queued.iter().any(|q| q.run_id == edit.run_id)
            || self.thread.as_ref().is_none_or(|t| t.state.projection.is_none())
        {
            return;
        }
        let text = self.composer.read(cx).value().to_string();
        let changed = text.trim() != edit.original.trim() || self.has_attachments();
        let keep = changed && edit.saved.0.trim().is_empty() && edit.saved.1.is_empty();
        let title = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()).map(|p| p.thread.title.clone());
        let Some(edit) = self.queue.edit.take() else { return };
        if !keep {
            self.attachments = edit.saved.1;
            self.set_composer_text(edit.saved.0, window, cx);
        }
        if changed {
            let place = title.map(|title| format!(" in “{title}”")).unwrap_or_default();
            let title = "Queued message is no longer queued";
            let note = if keep {
                Notification::info(format!("Your unsaved edit{place} was kept in the composer."))
            } else {
                Notification::warning(format!("Your unsaved edit{place} was discarded."))
            };
            window.push_notification(note.title(title), cx);
        }
        cx.notify();
    }

    /// `thread.steerQueuedMessage`: sends the first queued message now.
    pub(super) fn steer_next_queued(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(next) = self.queue.state.queued.first().map(|q| q.run_id.clone()) else { return false };
        if !matches!(self.target, Some(Target::Thread(_))) || !self.queue.state.can_send() {
            return false;
        }
        self.send_queued(next, cx);
        true
    }

    /// `thread.editQueuedMessage`: opens the last queued message, from an empty caret start.
    pub(super) fn edit_latest_queued(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(latest) = self.queue.state.queued.last().cloned() else { return false };
        let at_start = self.composer.read(cx).cursor() == 0;
        if !latest.editable
            || !at_start
            || self.queue.edit.is_some()
            || self.edit_blocked_reason().is_some()
            || self.queue.busy.is_some()
        {
            return false;
        }
        self.begin_queue_edit(&latest, window, cx);
        true
    }

    pub(super) fn on_steer_queued(&mut self, _: &SteerQueuedMessage, _: &mut Window, cx: &mut Context<Self>) {
        if !self.steer_next_queued(cx) {
            cx.propagate();
        }
    }

    pub(super) fn on_edit_queued(&mut self, _: &EditQueuedMessage, window: &mut Window, cx: &mut Context<Self>) {
        if !self.edit_latest_queued(window, cx) {
            cx.propagate();
        }
    }
}

/// Kept records first, new ones replacing any with the same `contextId`.
fn merge_records(kept: &[Value], added: Vec<Value>) -> Vec<Value> {
    let mut records: Vec<Value> = Vec::with_capacity(kept.len() + added.len());
    for record in kept.iter().cloned().chain(added) {
        match records.iter_mut().find(|r| r.get("contextId").is_some() && r.get("contextId") == record.get("contextId"))
        {
            Some(existing) => *existing = record,
            None => records.push(record),
        }
    }
    records
}

/// One queued message: the dashed bubble, then its time and controls (`QueuedMessage`).
pub(super) fn queued_row(
    message: &Rc<QueuedMessage>,
    last: bool,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
    let Some(workspace) = this.upgrade() else { return div().into_any_element() };
    let workspace = workspace.read(cx);
    let ui = &workspace.queue;
    let editing = ui.edit.as_ref().is_some_and(|edit| edit.run_id == message.run_id);
    let busy = ui.busy.is_some();
    let blocked = workspace.edit_blocked_reason();
    let id = &message.run_id;
    let muted = t.muted_foreground;
    let bubble = div()
        .min_w_0()
        .max_w(relative(0.8))
        .px(px(12.))
        .py(px(8.))
        .rounded(px(16.))
        .border_1()
        .border_dashed()
        .border_color(if editing { t.primary.opacity(0.6) } else { muted.opacity(0.4) })
        .text_sm()
        .text_color(if editing { t.foreground } else { muted })
        .when(!message.preview.is_empty(), |el| el.child(message.preview.clone()));
    let mut controls = h_flex().gap(px(2.)).pr(px(4.)).text_xs().text_color(muted);
    let time = message
        .created_at
        .as_ref()
        .and_then(|at| super::message_actions::timestamp(this, format!("queued-time:{id}").into(), at, t, cx));
    controls = controls.child(
        h_flex()
            .mr(px(4.))
            .px(px(4.))
            .opacity(0.)
            .group_hover(format!("queued:{id}"), |s| s.opacity(1.))
            .children(time),
    );
    if ui.state.can_send() && !editing {
        // The first message's Send now names `thread.steerQueuedMessage`.
        let first = ui.state.queued.first().is_some_and(|q| &q.run_id == id);
        let tooltip: SharedString = match workspace.shortcut_label("thread.steerQueuedMessage").filter(|_| first) {
            Some(shortcut) => format!("{} ({shortcut})", ui.state.send_tooltip()).into(),
            None => ui.state.send_tooltip().into(),
        };
        let (this, run) = (this.clone(), id.clone());
        controls = controls.child(
            text_button(format!("queued-send:{id}"), Some(IconName::ArrowUp), "Send now", busy, t)
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .when(!busy, |el| {
                    el.on_click(move |_, _, cx| {
                        let _ = this.update(cx, |w, cx| w.send_queued(run.clone(), cx));
                    })
                }),
        );
    }
    if editing {
        let this = this.clone();
        controls = controls.child(div().px(px(4.)).text_color(t.foreground).child("Editing in the composer")).child(
            text_button(format!("queued-cancel:{id}"), None, "Cancel", false, t).on_click(move |_, window, cx| {
                let _ = this.update(cx, |w, cx| w.cancel_queue_edit(window, cx));
            }),
        );
    } else {
        let edit_disabled = !message.editable || busy || blocked.is_some();
        // The last message's edit names `thread.editQueuedMessage`, labelled as from the composer.
        let edit_tooltip: SharedString = match blocked {
            Some(reason) => reason.into(),
            None => {
                let context = super::keymap::ShortcutContext { composer_focus: true, ..Default::default() };
                match super::keymap::label_for(&workspace.keymap, "thread.editQueuedMessage", &context)
                    .filter(|_| ui.state.queued.last().is_some_and(|q| &q.run_id == id))
                {
                    Some(shortcut) => format!("Edit in the composer ({shortcut})").into(),
                    None => "Edit in the composer".into(),
                }
            }
        };
        let (edit_this, edit_message) = (this.clone(), Rc::clone(message));
        let (remove_this, run) = (this.clone(), id.clone());
        controls = controls
            .child(
                icon_button(format!("queued-edit:{id}"), IconName::Pencil, edit_disabled, t)
                    .tooltip(move |window, cx| Tooltip::new(edit_tooltip.clone()).build(window, cx))
                    .when(!edit_disabled, |el| {
                        el.on_click(move |_, window, cx| {
                            let _ = edit_this.update(cx, |w, cx| w.begin_queue_edit(&edit_message, window, cx));
                        })
                    }),
            )
            .child(
                icon_button(format!("queued-remove:{id}"), IconName::X, busy, t)
                    .tooltip(|window, cx| Tooltip::new("Remove from queue").build(window, cx))
                    .when(!busy, |el| {
                        el.on_click(move |_, _, cx| {
                            let _ = remove_this.update(cx, |w, cx| w.remove_queued(run.clone(), cx));
                        })
                    }),
            );
    }
    let column = v_flex()
        .id(ElementId::Name(format!("queued:{id}").into()))
        .group(format!("queued:{id}"))
        .w_full()
        .items_end()
        .gap(px(4.))
        .when(message.sent_by_t3code, |el| {
            el.child(
                div()
                    .mr(px(4.))
                    .text_size(gpui_kit::rems(0.6875))
                    .text_color(muted.opacity(0.7))
                    .child("Sent by T3 Code"),
            )
        })
        .child(bubble)
        .child(controls);
    // `gap-3` between queued messages, the section's `pb-4` after the last.
    div().w_full().pb(px(if last { 16. } else { 12. })).child(column).into_any_element()
}

/// "Show all N queued" / "Show fewer".
pub(super) fn toggle_row(total: usize, expanded: bool, this: &WeakEntity<Workspace>, t: &Tokens) -> AnyElement {
    let this = this.clone();
    let label = if expanded { "Show fewer".to_owned() } else { format!("Show all {total} queued") };
    let chevron = if expanded { IconName::ChevronUp } else { IconName::ChevronDown };
    let button = h_flex()
        .id("queue-toggle")
        .gap(px(4.))
        .cursor_pointer()
        .text_xs()
        .text_color(t.muted_foreground)
        .hover(|s| s.text_color(t.foreground))
        .child(label)
        .child(Icon::new(chevron).size(px(14.)))
        .on_click(move |_, _, cx| {
            let _ = this.update(cx, |w, cx| w.toggle_queue(cx));
        });
    // `gap-2` above, the section's `pb-4` below.
    h_flex().w_full().justify_end().pr(px(4.)).pb(px(16.)).child(button).into_any_element()
}

/// `Button size="xs" variant="ghost-muted"` with an optional leading icon.
fn text_button(
    id: String,
    icon: Option<IconName>,
    label: &'static str,
    disabled: bool,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    h_flex()
        .id(ElementId::Name(id.into()))
        .h(px(24.))
        .px(px(8.))
        .gap(px(4.))
        .rounded(px(6.))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(t.muted_foreground)
        .children(icon.map(|icon| Icon::new(icon).size(px(14.))))
        .child(label)
        .map(|el| {
            if disabled {
                el.opacity(0.64)
            } else {
                el.cursor_pointer().hover(|s| s.bg(t.accent).text_color(t.foreground))
            }
        })
}

/// `Button size="icon-xs" variant="ghost-muted"`.
fn icon_button(id: String, icon: IconName, disabled: bool, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(ElementId::Name(id.into()))
        .size(px(24.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .text_color(t.muted_foreground)
        .child(Icon::new(icon).size(px(14.)))
        .map(|el| {
            if disabled {
                el.opacity(0.64)
            } else {
                el.cursor_pointer().hover(|s| s.bg(t.accent).text_color(t.foreground))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(count: usize) -> QueueState {
        let queued = (0..count)
            .map(|i| QueuedMessage {
                run_id: format!("r{i}").into(),
                message_id: format!("m{i}").into(),
                text: "x".into(),
                preview: "x".into(),
                editable: true,
                created_at: None,
                sent_by_t3code: false,
            })
            .collect();
        QueueState { queued, ..Default::default() }
    }

    fn shape(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                Row::Queued { message, last } => format!("{}{}", message.run_id, if *last { "." } else { "" }),
                Row::QueueToggle { total, expanded } => format!("toggle {total} {expanded}"),
                _ => "other".into(),
            })
            .collect()
    }

    #[test]
    fn a_long_queue_shows_two_messages_until_expanded() {
        assert_eq!(shape(&rows(&state(3), false)), ["r0", "r1", "r2."]);
        assert_eq!(shape(&rows(&state(4), false)), ["r0", "r1.", "toggle 4 false"]);
        assert_eq!(shape(&rows(&state(4), true)), ["r0", "r1", "r2", "r3.", "toggle 4 true"]);
        assert!(rows(&state(0), false).is_empty());
    }

    #[test]
    fn new_context_records_replace_kept_ones_with_the_same_id() {
        let kept = vec![json!({"contextId": "a", "v": 1}), json!({"contextId": "b"})];
        let merged = merge_records(&kept, vec![json!({"contextId": "a", "v": 2}), json!({"contextId": "c"})]);
        let ids: Vec<&str> = merged.iter().map(|r| r["contextId"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(merged[0]["v"], 2);
    }
}
