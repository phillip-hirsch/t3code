//! The queue control after the timeline (`QueuedRunsControl.tsx`) and editing a
//! queued message in the composer (`useQueuedRunEdit.ts`).

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _, WeakEntity, Window, div, img,
    prelude::FluentBuilder as _, px, relative,
};
use serde_json::{Value, json};

use super::attachments::{ATTACHMENT_ONLY_PROMPT, Attachment, MAX_ATTACHMENTS};
use super::side_chat::ChatPane;
use super::{Target, Workspace, new_id};
use crate::composer_context::ContextRecords;
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
    /// The attachments it keeps; removals stay local until the save (`existingAttachments`).
    attachments: Vec<Value>,
    removed_attachment: bool,
    /// The message's context records as queued; a save keeps them (`keptRecords`).
    records: Vec<Value>,
    saved: (String, Vec<Attachment>, ContextRecords),
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

impl QueueUi {
    /// Ends an edit, handing back the draft it set aside.
    pub(super) fn take_edit_draft(&mut self) -> Option<(String, Vec<Attachment>, ContextRecords)> {
        self.edit.take().map(|edit| edit.saved)
    }
}

impl Workspace {
    fn queue_ui(&self, pane: ChatPane) -> Option<&QueueUi> {
        match pane {
            ChatPane::Main => Some(&self.queue),
            ChatPane::Side => self.side_chat.view.as_ref().map(|view| &view.queue),
        }
    }

    fn queue_ui_mut(&mut self, pane: ChatPane) -> Option<&mut QueueUi> {
        match pane {
            ChatPane::Main => Some(&mut self.queue),
            ChatPane::Side => self.side_chat.view.as_mut().map(|view| &mut view.queue),
        }
    }

    /// The pane's queue rows.
    pub(super) fn queue_rows(&mut self, pane: ChatPane) -> Vec<Row> {
        let state = match (self.pane_thread(pane), self.pane_projection(pane)) {
            (Some((_, id)), Some(projection)) => {
                let archived = self
                    .shell
                    .threads
                    .iter()
                    .any(|t| t.id == id && (t.archived_at.is_some() || t.deleted_at.is_some()));
                queue::derive(projection, archived)
            }
            _ => QueueState::default(),
        };
        let Some(ui) = self.queue_ui_mut(pane) else { return Vec::new() };
        if *ui.state != state {
            ui.state = Rc::new(state);
        }
        rows(&ui.state, ui.show_all)
    }

    pub(super) fn editing_queued(&self) -> bool {
        self.queue.edit.is_some()
    }

    pub(super) fn saving_queue_edit(&self) -> bool {
        self.queue.edit.as_ref().is_some_and(|edit| edit.saving)
    }

    /// The thread's own draft while a queued message holds the composer, left in place.
    pub(super) fn queue_edit_draft(&self) -> Option<(&str, &[Attachment], &ContextRecords)> {
        self.queue.edit.as_ref().map(|edit| (edit.saved.0.as_str(), edit.saved.1.as_slice(), &edit.saved.2))
    }

    /// The thread's draft, while a queued message holds the composer; see `stash_composer`.
    pub(super) fn take_queue_edit_draft(&mut self) -> Option<(String, Vec<Attachment>, ContextRecords)> {
        self.queue.take_edit_draft()
    }

    /// A queued message open in the pane's composer: whether its save is in flight.
    pub(super) fn pane_queue_edit(&self, pane: ChatPane) -> Option<bool> {
        self.queue_ui(pane)?.edit.as_ref().map(|edit| edit.saving)
    }

    fn edit_blocked_reason(&self, pane: ChatPane) -> Option<&'static str> {
        let asking = match pane {
            ChatPane::Main => self.question.is_some(),
            ChatPane::Side => self.side_chat_projection().is_some_and(|p| !p.pending_user_inputs().is_empty()),
        };
        asking.then_some("Answer the question to edit queued messages")
    }

    fn toggle_queue(&mut self, pane: ChatPane, cx: &mut Context<Self>) {
        let Some(ui) = self.queue_ui_mut(pane) else { return };
        ui.show_all = !ui.show_all;
        self.refresh_pane_rows(pane, cx);
        cx.notify();
    }

    /// "Send now": starts a turn when idle, else steers it into the running one.
    fn send_queued(&mut self, pane: ChatPane, run_id: SharedString, cx: &mut Context<Self>) {
        let Some(ui) = self.queue_ui(pane).filter(|ui| ui.busy.is_none()) else { return };
        let state = ui.state.clone();
        let Some(message) = state.queued.iter().find(|q| q.run_id == run_id) else { return };
        if !state.can_send(message) {
            return;
        }
        let command = match (state.can_send_now, &state.active_run_id) {
            (true, _) => json!({"type": "queued-run.start", "runId": run_id}),
            (false, Some(active)) => {
                json!({"type": "queued-message.promote-to-steer", "queuedRunId": run_id, "targetRunId": active})
            }
            (false, None) => return,
        };
        self.dispatch_queue_command(pane, run_id, command, cx);
    }

    fn remove_queued(&mut self, pane: ChatPane, run_id: SharedString, cx: &mut Context<Self>) {
        if self.queue_ui(pane).is_none_or(|ui| ui.busy.is_some()) {
            return;
        }
        let command = json!({"type": "queued-run.cancel", "runId": run_id});
        self.dispatch_queue_command(pane, run_id, command, cx);
    }

    fn dispatch_queue_command(
        &mut self,
        pane: ChatPane,
        run_id: SharedString,
        mut command: Value,
        cx: &mut Context<Self>,
    ) {
        let Some((client, thread_id)) = self.pane_client(pane, cx) else { return };
        command["commandId"] = new_id().into();
        command["threadId"] = thread_id.clone().into();
        let Some(ui) = self.queue_ui_mut(pane) else { return };
        ui.busy = Some(run_id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update(cx, |this, cx| {
                // Run ids are unique, so this frees only the pane that sent it.
                if let Some(ui) = this.queue_ui_mut(pane).filter(|ui| ui.busy.as_ref() == Some(&run_id)) {
                    ui.busy = None;
                }
                if let Err(error) = result
                    && this.pane_shows(pane, &thread_id)
                {
                    this.error = Some(error.to_string().into());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens a queued message in the pane's composer, setting its draft aside.
    fn begin_queue_edit(
        &mut self,
        pane: ChatPane,
        message: &QueuedMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, thread_id)) = self.pane_thread(pane) else { return };
        if !message.editable
            || self.queue_ui(pane).is_none_or(|ui| ui.busy.is_some())
            || self.edit_blocked_reason(pane).is_some()
        {
            return;
        }
        let item = self.pane_projection(pane).and_then(|projection| {
            projection.turn_items.iter().rev().find(|item| {
                item.item_type == "user_message" && item.str("messageId") == Some(message.message_id.as_ref())
            })
        });
        let attachments =
            item.and_then(|i| i.raw.get("attachments")).and_then(Value::as_array).cloned().unwrap_or_default();
        let records =
            item.and_then(|i| i.raw.pointer("/context/records")).and_then(Value::as_array).cloned().unwrap_or_default();
        let previous = self.queue_ui_mut(pane).and_then(QueueUi::take_edit_draft);
        let saved = match (previous, pane) {
            (Some(saved), _) => saved,
            (None, ChatPane::Main) => (
                self.composer.read(cx).value().to_string(),
                std::mem::take(&mut self.attachments),
                self.take_context_records(),
            ),
            // Side chats take text only.
            (None, ChatPane::Side) => (self.pane_text(pane, cx), Vec::new(), ContextRecords::default()),
        };
        if pane == ChatPane::Main {
            self.attachments.clear();
            // Its chips show and copy from the records it was queued with.
            let mut held = ContextRecords::default();
            held.extend(records.iter().filter(|record| record.get("attachmentId").is_none()).cloned());
            self.context.records = held;
        }
        let text = queue::edit_text(&message.text).to_owned();
        let Some(ui) = self.queue_ui_mut(pane) else { return };
        ui.edit = Some(QueueEdit {
            thread_id,
            run_id: message.run_id.clone(),
            original: text.clone(),
            attachments,
            removed_attachment: false,
            records,
            saved,
            saving: false,
        });
        ui.show_all = true;
        self.set_pane_text(pane, text, window, cx);
        self.refresh_pane_rows(pane, cx);
        self.focus_pane_composer(pane, window, cx);
        cx.notify();
    }

    fn cancel_queue_edit(&mut self, pane: ChatPane, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((text, attachments, records)) = self.queue_ui_mut(pane).and_then(QueueUi::take_edit_draft) {
            if pane == ChatPane::Main {
                self.attachments = attachments;
                self.context.records = records;
            }
            self.set_pane_text(pane, text, window, cx);
            self.focus_pane_composer(pane, window, cx);
        }
        cx.notify();
    }

    /// The composer's send while editing: `queued-run.edit` with the new text, plus
    /// any attachments added in the composer (`submitQueuedRunEdit`).
    pub(super) fn save_queue_edit(&mut self, pane: ChatPane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.queue_ui(pane).and_then(|ui| ui.edit.as_ref()).filter(|edit| !edit.saving) else {
            return;
        };
        let (thread_id, run_id) = (edit.thread_id.clone(), edit.run_id.clone());
        let (kept, removed_attachment) = (edit.attachments.clone(), edit.removed_attachment);
        let kept_records = retained_records(&edit.records, &edit.attachments);
        // Only the open thread's composer holds attachments; a side chat's edit keeps the queued ones.
        let main = pane == ChatPane::Main;
        let text = self.pane_text(pane, cx).trim().to_owned();
        let added_count = if main { self.attachments.len() } else { 0 };
        if kept.len() + added_count > MAX_ATTACHMENTS {
            self.error = Some(format!("A message can have at most {MAX_ATTACHMENTS} attachments.").into());
            cx.notify();
            return;
        }
        if text.is_empty() && kept.is_empty() && added_count == 0 {
            return;
        }
        let Some((client, shown)) = self.pane_client(pane, cx) else { return };
        // An edit left over from another thread is never sent to this one.
        if shown != thread_id {
            return;
        }
        let mut command = json!({
            "type": "queued-run.edit",
            "commandId": new_id(),
            "threadId": thread_id,
            "runId": run_id,
        });
        let mut has_attachments = !kept.is_empty();
        let mut added_records = if main { self.message_context_records(&text) } else { Vec::new() };
        // Absent attachments keep the queued ones, so only an added or removed one sends the set.
        if main && (self.has_attachments() || removed_attachment) {
            let (added, context) = match self.attachment_payload(&text) {
                Ok(payload) => payload,
                Err(reason) => {
                    self.error = Some(reason.into());
                    cx.notify();
                    return;
                }
            };
            has_attachments |= !added.is_empty();
            added_records = context.as_ref().and_then(|c| c["records"].as_array()).cloned().unwrap_or_default();
            command["attachments"] = kept.into_iter().chain(added).collect::<Vec<_>>().into();
        }
        let records = merge_records(&kept_records, added_records);
        if !records.is_empty() {
            command["context"] = json!({"version": 1, "records": records});
        }
        command["text"] =
            if text.is_empty() && has_attachments { ATTACHMENT_ONLY_PROMPT.to_owned() } else { text }.into();
        if let Some(edit) = self.queue_ui_mut(pane).and_then(|ui| ui.edit.as_mut()) {
            edit.saving = true;
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| {
                // The edit was cancelled, begun again, or its pane moved on while this save ran.
                let current =
                    this.queue_ui_mut(pane).and_then(|ui| ui.edit.as_mut()).filter(|edit| edit.run_id == run_id);
                match (result, current) {
                    (Ok(_), Some(_)) => this.cancel_queue_edit(pane, window, cx),
                    (Err(error), current) => {
                        log::warn!("saving the edited queued message: {error}");
                        let shown = current.is_some();
                        if let Some(edit) = current {
                            edit.saving = false;
                        }
                        if shown {
                            this.error = Some("Could not save the edited queued message.".into());
                        }
                    }
                    (Ok(_), None) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Drops one of the edited message's attachments; the save sends the rest.
    fn remove_queued_attachment(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(edit) = self.queue.edit.as_mut().filter(|edit| !edit.saving) else { return };
        edit.attachments.retain(|attachment| attachment["id"].as_str() != Some(id));
        edit.removed_attachment = true;
        cx.notify();
    }

    /// Loads the pane's queue image thumbnails not seen yet.
    pub(super) fn load_queue_images(&mut self, pane: ChatPane, cx: &mut Context<Self>) {
        let Some(state) = self.queue_ui(pane).map(|ui| ui.state.clone()) else { return };
        let images = state.queued.iter().flat_map(|message| message.images.iter().map(|image| image.id.as_ref()));
        self.load_attachment_images(images, cx);
    }

    /// `ComposerQueuedAttachmentRow`: the edited message's attachments, already uploaded.
    pub(super) fn render_queue_edit_attachments(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let edit = self.queue.edit.as_ref().filter(|edit| !edit.attachments.is_empty() && self.question.is_none())?;
        let tiles: Vec<AnyElement> = edit
            .attachments
            .iter()
            .filter_map(|attachment| {
                let id = attachment["id"].as_str()?.to_owned();
                let name = attachment["name"].as_str().unwrap_or_default().to_owned();
                let image = (attachment["type"] == "image").then(|| self.attachment_image(&id)).flatten();
                let label = format!("Remove {name}");
                let body = match image {
                    Some(image) => img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
                    None => div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(4.))
                        .text_size(px(10.))
                        .text_color(t.muted_foreground)
                        .child(name)
                        .into_any_element(),
                };
                let remove = super::attachments::overlay_button(
                    ElementId::Name(format!("queued-attachment-{id}").into()),
                    IconName::X,
                )
                .right(px(4.))
                .top(px(4.))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| this.remove_queued_attachment(&id, cx)));
                Some(
                    div()
                        .relative()
                        .size(px(64.))
                        .flex_none()
                        .overflow_hidden()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(t.border.opacity(0.8))
                        .bg(t.background)
                        .child(body)
                        .child(remove)
                        .into_any_element(),
                )
            })
            .collect();
        Some(h_flex().mb(px(12.)).gap(px(8.)).flex_wrap().children(tiles).into_any_element())
    }

    /// The run started or was cancelled elsewhere: a changed edit moves into the
    /// pane's draft when that is empty, else it's dropped (`onRunLeftQueue`).
    pub(super) fn sync_queue_edit(&mut self, pane: ChatPane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ui) = self.queue_ui(pane) else { return };
        let Some(edit) = ui.edit.as_ref().filter(|edit| !edit.saving) else { return };
        if ui.state.queued.iter().any(|q| q.run_id == edit.run_id) || self.pane_projection(pane).is_none() {
            return;
        }
        let text = self.pane_text(pane, cx);
        let attached = pane == ChatPane::Main && self.has_attachments();
        let changed = text.trim() != edit.original.trim() || attached;
        let keep = changed && edit.saved.0.trim().is_empty() && edit.saved.1.is_empty();
        let title = self.pane_projection(pane).map(|p| p.thread.title.clone());
        let Some((saved_text, saved_attachments, saved_records)) =
            self.queue_ui_mut(pane).and_then(QueueUi::take_edit_draft)
        else {
            return;
        };
        if !keep {
            if pane == ChatPane::Main {
                self.attachments = saved_attachments;
                self.context.records = saved_records;
            }
            self.set_pane_text(pane, saved_text, window, cx);
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

    /// `thread.steerQueuedMessage`: sends the open thread's first queued message now.
    pub(super) fn steer_next_queued(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let Some(next) = self.queue.state.queued.first() else { return false };
        if !matches!(self.target, Some(Target::Thread { .. }))
            || !self.queue.state.can_send(next)
            || self.side_chat_focused(window, cx)
        {
            return false;
        }
        self.send_queued(ChatPane::Main, next.run_id.clone(), cx);
        true
    }

    /// `thread.editQueuedMessage`: opens the open thread's last queued message, from an empty caret start.
    pub(super) fn edit_latest_queued(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(latest) = self.queue.state.queued.last().cloned() else { return false };
        let at_start = self.composer.read(cx).cursor() == 0;
        if !latest.editable
            || !at_start
            || self.side_chat_focused(window, cx)
            || self.queue.edit.is_some()
            || self.edit_blocked_reason(ChatPane::Main).is_some()
            || self.queue.busy.is_some()
        {
            return false;
        }
        self.begin_queue_edit(ChatPane::Main, &latest, window, cx);
        true
    }

    pub(super) fn on_steer_queued(&mut self, _: &SteerQueuedMessage, window: &mut Window, cx: &mut Context<Self>) {
        if !self.steer_next_queued(window, cx) {
            cx.propagate();
        }
    }

    pub(super) fn on_edit_queued(&mut self, _: &EditQueuedMessage, window: &mut Window, cx: &mut Context<Self>) {
        if !self.edit_latest_queued(window, cx) {
            cx.propagate();
        }
    }
}

/// The edit's context records, less those of attachments it no longer carries.
fn retained_records(records: &[Value], attachments: &[Value]) -> Vec<Value> {
    let kept = |id: &Value| attachments.iter().any(|attachment| attachment["id"] == *id);
    records.iter().filter(|record| record.get("attachmentId").is_none_or(kept)).cloned().collect()
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
    pane: ChatPane,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &App,
) -> AnyElement {
    let Some(workspace) = this.upgrade() else { return div().into_any_element() };
    let workspace = workspace.read(cx);
    let Some(ui) = workspace.queue_ui(pane) else { return div().into_any_element() };
    let editing = ui.edit.as_ref().is_some_and(|edit| edit.run_id == message.run_id);
    let busy = ui.busy.is_some();
    let blocked = workspace.edit_blocked_reason(pane);
    // Sent but not queued on the server yet: no run to act on.
    let pending = message.run_id.is_empty();
    let id = if pending { &message.message_id } else { &message.run_id };
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
        .flex()
        .flex_col()
        .gap(px(8.))
        .when(!message.images.is_empty(), |el| {
            el.child(h_flex().flex_wrap().gap(px(4.)).children(message.images.iter().map(|image| {
                let tile = div()
                    .size(px(32.))
                    .flex_none()
                    .overflow_hidden()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.border.opacity(0.7))
                    .bg(t.background);
                match workspace.attachment_image(&image.id) {
                    Some(image) => tile.child(img(image).size_full().object_fit(ObjectFit::Cover)),
                    None => tile.child(div().size_full().bg(t.muted.opacity(0.6))),
                }
            })))
        })
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
    if !pending && ui.state.can_send(message) && !editing {
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
                        let _ = this.update(cx, |w, cx| w.send_queued(pane, run.clone(), cx));
                    })
                }),
        );
    }
    if editing {
        let this = this.clone();
        controls = controls.child(div().px(px(4.)).text_color(t.foreground).child("Editing in the composer")).child(
            text_button(format!("queued-cancel:{id}"), None, "Cancel", false, t).on_click(move |_, window, cx| {
                let _ = this.update(cx, |w, cx| w.cancel_queue_edit(pane, window, cx));
            }),
        );
    } else if !pending {
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
                            let _ = edit_this.update(cx, |w, cx| w.begin_queue_edit(pane, &edit_message, window, cx));
                        })
                    }),
            )
            .child(
                icon_button(format!("queued-remove:{id}"), IconName::X, busy, t)
                    .tooltip(|window, cx| Tooltip::new("Remove from queue").build(window, cx))
                    .when(!busy, |el| {
                        el.on_click(move |_, _, cx| {
                            let _ = remove_this.update(cx, |w, cx| w.remove_queued(pane, run.clone(), cx));
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
pub(super) fn toggle_row(
    total: usize,
    expanded: bool,
    pane: ChatPane,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> AnyElement {
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
            let _ = this.update(cx, |w, cx| w.toggle_queue(pane, cx));
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
                fresh_context: false,
                images: Vec::new(),
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
    fn removing_an_attachment_drops_its_context_record() {
        let records = vec![
            json!({"contextId": "a", "attachmentId": "att-1"}),
            json!({"contextId": "b", "attachmentId": "att-2"}),
            json!({"contextId": "terminal"}),
        ];
        let kept = retained_records(&records, &[json!({"id": "att-2"})]);
        let ids: Vec<&str> = kept.iter().map(|r| r["contextId"].as_str().unwrap()).collect();
        assert_eq!(ids, ["b", "terminal"]);
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
