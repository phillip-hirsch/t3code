//! Thread actions and their menu, shared by sidebar rows and the chat title: settle, snooze,
//! pin, rename, archive, delete, mark unread, and their undo (`hooks/useThreadActions.ts`,
//! `hooks/useThreadActionMenu.ts`, `components/threadActionMenu.logic.ts`,
//! `SidebarThreadUndoNotice.tsx`).

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Point, PromptLevel, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};
use serde_json::{Value, json};

use super::{Shelf, now_ms};
use crate::app::thread_menu::hosted_thread_link;
use crate::app::{Workspace, new_id};
use crate::inbox::{self, Facts};

/// The undoable actions (`showThreadUndoNotice`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum UndoAction {
    Settled,
    Snoozed,
    Unpinned,
    Archived,
    Discarded,
}

impl UndoAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            UndoAction::Settled => "Settled",
            UndoAction::Snoozed => "Snoozed",
            UndoAction::Unpinned => "Unpinned",
            UndoAction::Archived => "Archived",
            UndoAction::Discarded => "Discarded",
        }
    }

    /// The toast title of a failed undo (`failureTitle` in `useThreadActions.ts`).
    fn failure_title(self) -> &'static str {
        match self {
            UndoAction::Settled => "Failed to undo settle",
            UndoAction::Snoozed => "Failed to wake thread",
            UndoAction::Unpinned => "Failed to undo unpin",
            UndoAction::Archived => "Failed to undo archive",
            UndoAction::Discarded => "Failed to restore draft",
        }
    }
}

/// The toast title the web shows when a thread command fails (`useThreadActions.ts`, `Sidebar.tsx`).
fn failure_title(kind: &str, fields: &Value) -> Option<&'static str> {
    Some(match kind {
        "thread.settle" => "Failed to settle thread",
        "thread.unsettle" => "Failed to un-settle thread",
        "thread.snooze" => "Failed to snooze thread",
        "thread.unsnooze" => "Failed to wake thread",
        "thread.pin" => "Failed to pin thread",
        "thread.unpin" => "Failed to unpin thread",
        "thread.archive" => "Failed to archive thread",
        "thread.delete" => "Failed to delete thread",
        "thread.auto-settle.set" => "Failed to update auto-settle",
        "thread.metadata.update" if fields.get("regenerateTitle").is_some() => "Failed to regenerate thread title",
        "thread.metadata.update" => "Failed to rename thread",
        _ => return None,
    })
}

/// A failure toast's text: its title, then the error.
pub(in crate::app) fn failure_message(title: Option<&str>, error: &str) -> String {
    match title {
        Some(title) => format!("{title}: {error}"),
        None => error.to_owned(),
    }
}

/// What undo restores for one thread.
pub(super) struct UndoEntry {
    thread_id: String,
    pin_key: Option<String>,
    was_pinned: bool,
    snoozed_until: Option<String>,
}

/// The footer's undo notice: the latest action and how many threads it covers.
pub(super) struct UndoNotice {
    pub(super) action: UndoAction,
    entries: Vec<UndoEntry>,
    /// A discarded draft, for `UndoAction::Discarded`.
    draft: Option<super::drafts::Discarded>,
    _expire: Task<()>,
}

impl UndoNotice {
    pub(super) fn count(&self) -> usize {
        self.entries.len().max(usize::from(self.draft.is_some()))
    }
}

/// Where a thread menu opened. The sidebar renames in the row, moves on from the open thread
/// after parking it, and offers the project filter; the chat title does none of these.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::app) enum MenuOrigin {
    Sidebar,
    Title,
}

/// The title editor of the thread being renamed, in its row or in the chat title.
pub(in crate::app) struct Renaming {
    pub(in crate::app) thread_id: String,
    pub(in crate::app) input: Entity<InputState>,
    original: String,
    _subscription: Subscription,
}

/// A confirmation a client setting turns on (`api.dialogs.confirm`).
struct Confirm {
    setting: &'static str,
    message: String,
    detail: Option<&'static str>,
    ok: &'static str,
    level: PromptLevel,
}

const UNDO_TIMEOUT: Duration = Duration::from_secs(5);
/// A batch command's environment has no socket (`chatViewSend.ts`'s wording).
const NOT_CONNECTED: &str = "The environment is not connected.";

impl Workspace {
    fn thread(&self, thread_id: &str) -> Option<&crate::model::ThreadShell> {
        self.shell.threads.iter().find(|t| t.id == thread_id)
    }

    /// Dispatches `{type, commandId, threadId, ..fields}`; `false` when there is no
    /// connection to send it on, so callers skip their optimistic undo and navigation.
    pub(in crate::app) fn thread_command(
        &mut self,
        kind: &str,
        thread_id: &str,
        fields: Value,
        cx: &mut Context<Self>,
    ) -> bool {
        let title = failure_title(kind, &fields);
        let command = Self::thread_command_value(kind, thread_id, fields);
        let Some(client) = self.thread_client(thread_id) else { return false };
        cx.spawn(async move |this, cx| {
            if let Err(error) = client.request("orchestration.dispatchCommand", command).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(failure_message(title, &error.to_string()).into());
                    cx.notify();
                });
            }
        })
        .detach();
        true
    }

    /// `{type, commandId, threadId, ..fields}`.
    pub(super) fn thread_command_value(kind: &str, thread_id: &str, fields: Value) -> Value {
        let mut command = json!({"type": kind, "commandId": new_id(), "threadId": thread_id});
        if let (Some(command), Value::Object(fields)) = (command.as_object_mut(), fields) {
            command.extend(fields);
        }
        command
    }

    /// Dispatches `commands` in order, then reports failures as one toast titled by
    /// `title(failed, succeeded)`; a command whose environment is down fails too.
    /// `stop_at_failure` ends the batch at the first one.
    pub(super) fn dispatch_batch(
        &mut self,
        commands: Vec<Value>,
        stop_at_failure: bool,
        title: fn(usize, usize) -> String,
        cx: &mut Context<Self>,
    ) {
        // Each command goes to the environment its thread or project lives on.
        let routed: Vec<_> = commands.into_iter().map(|command| (self.command_client(&command), command)).collect();
        cx.spawn(async move |this, cx| {
            let (mut failed, mut succeeded, mut first_error) = (0, 0, None);
            for (client, command) in routed {
                let result = match client {
                    Some(client) => {
                        client.request("orchestration.dispatchCommand", command).await.map_err(|e| e.to_string())
                    }
                    None => Err(NOT_CONNECTED.to_owned()),
                };
                match result {
                    Ok(_) => succeeded += 1,
                    Err(error) => {
                        failed += 1;
                        first_error.get_or_insert(error);
                        if stop_at_failure {
                            break;
                        }
                    }
                }
            }
            if let Some(error) = first_error {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(failure_message(Some(&title(failed, succeeded)), &error).into());
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn undo_entry(&self, thread_id: &str) -> UndoEntry {
        let thread = self.thread(thread_id);
        UndoEntry {
            thread_id: thread_id.to_owned(),
            pin_key: thread.and_then(|t| t.pin_order_key.clone()),
            was_pinned: thread.is_some_and(|t| t.pinned_at.is_some()),
            snoozed_until: thread.and_then(|t| t.snoozed_until.clone()),
        }
    }

    /// Adds `entry` to the notice, or replaces a notice for another action;
    /// every new action restarts the 5 s timeout.
    pub(super) fn push_undo(&mut self, action: UndoAction, entry: UndoEntry, cx: &mut Context<Self>) {
        let mut entries = match self.sidebar.undo.take() {
            Some(notice) if notice.action == action => notice.entries,
            _ => Vec::new(),
        };
        entries.retain(|e| e.thread_id != entry.thread_id);
        entries.push(entry);
        self.set_undo(action, entries, None, cx);
    }

    /// Shows the undo notice for 5 s.
    pub(super) fn set_undo(
        &mut self,
        action: UndoAction,
        entries: Vec<UndoEntry>,
        draft: Option<super::drafts::Discarded>,
        cx: &mut Context<Self>,
    ) {
        let expire = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_TIMEOUT).await;
            let _ = this.update(cx, |this, cx| {
                this.sidebar.undo = None;
                this.notify_sidebar(cx);
            });
        });
        self.sidebar.undo = Some(UndoNotice { action, entries, draft, _expire: expire });
        self.notify_sidebar(cx);
    }

    /// Drops a pending undo for `thread_id` that a newer action makes stale.
    fn expire_undo(&mut self, thread_id: &str) {
        if let Some(notice) = &mut self.sidebar.undo {
            notice.entries.retain(|e| e.thread_id != thread_id);
            if notice.entries.is_empty() {
                self.sidebar.undo = None;
            }
        }
    }

    /// Runs the notice's undo: each thread's steps in order, stopping at a failure.
    pub(in crate::app) fn undo_thread_action(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut notice) = self.sidebar.undo.take() else { return };
        self.notify_sidebar(cx);
        if let Some(draft) = notice.draft.take() {
            self.restore_discarded(draft, window, cx);
            return;
        }
        let now = now_ms();
        let action = notice.action;
        let mut plans = Vec::new();
        for entry in notice.entries {
            let command = |kind: &str, fields| Self::thread_command_value(kind, &entry.thread_id, fields);
            let repin =
                || command("thread.pin", entry.pin_key.as_ref().map_or(json!({}), |key| json!({"orderKey": key})));
            let resnooze = entry
                .snoozed_until
                .as_ref()
                .filter(|until| crate::timestamp::parse_ms(until).is_some_and(|until| until > now))
                .map(|until| command("thread.snooze", json!({"snoozedUntil": until})));
            let mut steps = Vec::new();
            match action {
                UndoAction::Settled => {
                    steps.push(command("thread.unsettle", json!({"reason": "user"})));
                    if entry.was_pinned {
                        steps.push(repin());
                    }
                    steps.extend(resnooze);
                }
                UndoAction::Snoozed => steps.push(command("thread.unsnooze", json!({"reason": "user"}))),
                UndoAction::Unpinned => {
                    steps.push(repin());
                    steps.extend(resnooze);
                }
                UndoAction::Archived => steps.push(command("thread.unarchive", json!({}))),
                UndoAction::Discarded => {}
            }
            plans.push((self.thread_client(&entry.thread_id), steps));
        }
        cx.spawn(async move |this, cx| {
            for (client, steps) in plans {
                let Some(client) = client else { continue };
                for step in steps {
                    if let Err(error) = client.request("orchestration.dispatchCommand", step).await {
                        let _ = this.update(cx, |this, cx| {
                            let message = failure_message(Some(action.failure_title()), &error.to_string());
                            this.error = Some(message.into());
                            cx.notify();
                        });
                        break;
                    }
                }
            }
        })
        .detach();
    }

    /// After settling, snoozing or archiving the open thread, the web moves on
    /// to the next inbox thread in sidebar order (wrapping), else a new draft
    /// in its project.
    fn leave_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_threads(thread_id, &[], window, cx);
    }

    /// `leave_thread`, also passing over `parked`: threads parked with it (`coParkingKeys`).
    pub(super) fn leave_threads(
        &mut self,
        thread_id: &str,
        parked: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.open_thread_id() != Some(thread_id) {
            return;
        }
        let order = &self.sidebar.inbox_order;
        let next = order.iter().position(|id| id == thread_id).and_then(|at| {
            order[at + 1..].iter().chain(&order[..at]).find(|id| *id != thread_id && !parked.contains(id)).cloned()
        });
        match next {
            Some(next) => self.select_thread(next, window, cx),
            None => {
                if let Some(project_id) = self.thread(thread_id).map(|t| t.project_id.clone()) {
                    self.start_draft(project_id, window, cx);
                }
            }
        }
    }

    /// `advance` moves on from the open thread, as the sidebar's settle does.
    pub(in crate::app) fn settle_thread(
        &mut self,
        thread_id: &str,
        advance: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry = self.undo_entry(thread_id);
        if !self.thread_command("thread.settle", thread_id, json!({}), cx) {
            return;
        }
        self.push_undo(UndoAction::Settled, entry, cx);
        if advance {
            self.leave_thread(thread_id, window, cx);
        }
    }

    pub(in crate::app) fn unsettle_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.expire_undo(thread_id);
        self.thread_command("thread.unsettle", thread_id, json!({"reason": "user"}), cx);
    }

    pub(super) fn snooze_thread(
        &mut self,
        thread_id: &str,
        wake_ms: i64,
        advance: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if wake_ms <= now_ms() {
            self.error = Some("That snooze time has passed. Choose another time.".into());
            cx.notify();
            return;
        }
        let entry = self.undo_entry(thread_id);
        let wake = json!({"snoozedUntil": crate::timestamp::iso(wake_ms).unwrap_or_default()});
        if !self.thread_command("thread.snooze", thread_id, wake, cx) {
            return;
        }
        self.push_undo(UndoAction::Snoozed, entry, cx);
        if advance {
            self.leave_thread(thread_id, window, cx);
        }
    }

    pub(super) fn wake_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.expire_undo(thread_id);
        self.thread_command("thread.unsnooze", thread_id, json!({"reason": "user"}), cx);
    }

    /// `useAcknowledgeThreadWoke`: marks the thread visited at its wake, which clears "Woke".
    pub(super) fn acknowledge_woke(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(thread_id).filter(|t| t.tracks_visits()) else { return };
        let requests = inbox::child_requests(&self.shell.threads);
        let Some(woke_at) = Facts::new(thread, &requests).woke_at(now_ms()).map(str::to_owned) else { return };
        self.thread_command("thread.visit", thread_id, json!({"visitedAt": woke_at}), cx);
    }

    pub(in crate::app) fn pin_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.expire_undo(thread_id);
        let fields = inbox::top_pin_key(&self.shell.threads).map_or(json!({}), |key| json!({"orderKey": key}));
        self.thread_command("thread.pin", thread_id, fields, cx);
    }

    /// `confirmAndUnpinThread`: asked first when `confirmThreadUnpin` is on.
    pub(in crate::app) fn unpin_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.thread(thread_id).map(|t| t.title.clone()).unwrap_or_default();
        let thread_id = thread_id.to_owned();
        let question = Confirm {
            setting: "confirmThreadUnpin",
            message: format!("Unpin thread \"{title}\"?"),
            detail: Some("This will move the thread out of your pinned section."),
            ok: "Unpin",
            level: PromptLevel::Info,
        };
        self.confirm_then(question, window, cx, move |this, _, cx| {
            let entry = this.undo_entry(&thread_id);
            if this.thread_command("thread.unpin", &thread_id, json!({}), cx) {
                this.push_undo(UndoAction::Unpinned, entry, cx);
            }
        });
    }

    /// The selection's Unpin: one question for all of them, unlike the web's one per thread.
    pub(super) fn unpin_threads(&mut self, ids: &[String], window: &mut Window, cx: &mut Context<Self>) {
        if let [id] = ids {
            self.unpin_thread(id, window, cx);
            return;
        }
        let ids = ids.to_vec();
        let question = Confirm {
            setting: "confirmThreadUnpin",
            message: format!("Unpin {} threads?", ids.len()),
            detail: Some("This will move the threads out of your pinned section."),
            ok: "Unpin",
            level: PromptLevel::Info,
        };
        self.confirm_then(question, window, cx, move |this, _, cx| {
            for id in &ids {
                let entry = this.undo_entry(id);
                if this.thread_command("thread.unpin", id, json!({}), cx) {
                    this.push_undo(UndoAction::Unpinned, entry, cx);
                }
            }
        });
    }

    /// `thread.archive`, asked first when `confirmThreadArchive` is on; the open thread
    /// gives way to a new draft in its project.
    pub(in crate::app) fn archive_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(thread_id) else { return };
        let (project_id, title) = (thread.project_id.clone(), thread.title.clone());
        let thread_id = thread_id.to_owned();
        let question = Confirm {
            setting: "confirmThreadArchive",
            message: format!("Archive thread \"{title}\"?"),
            detail: None,
            ok: "Archive",
            level: PromptLevel::Info,
        };
        self.confirm_then(question, window, cx, move |this, window, cx| {
            let entry = this.undo_entry(&thread_id);
            if !this.thread_command("thread.archive", &thread_id, json!({}), cx) {
                return;
            }
            this.push_undo(UndoAction::Archived, entry, cx);
            if this.open_thread_id() == Some(&*thread_id) {
                this.start_draft(project_id, window, cx);
            }
        });
    }

    /// `thread.delete`, asked first unless `confirmThreadDelete` is off; there is no undo.
    pub(in crate::app) fn delete_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.thread(thread_id).map(|t| t.title.clone()).unwrap_or_default();
        let thread_id = thread_id.to_owned();
        let question = Confirm {
            setting: "confirmThreadDelete",
            message: format!("Delete thread \"{title}\"?"),
            detail: Some("This permanently clears conversation history for this thread."),
            ok: "Delete",
            level: PromptLevel::Critical,
        };
        self.confirm_then(question, window, cx, move |this, _, cx| {
            if this.thread_client(&thread_id).is_some() {
                this.expire_undo(&thread_id);
                this.thread_command("thread.delete", &thread_id, json!({}), cx);
            }
        });
    }

    /// Runs `then` at once when the question's setting is off, else once the user agrees.
    fn confirm_then(
        &mut self,
        question: Confirm,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if self.client_settings.values.get(question.setting).as_bool() != Some(true) {
            then(self, window, cx);
            return;
        }
        let answer = window.prompt(question.level, &question.message, question.detail, &[question.ok, "Cancel"], cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| then(this, window, cx));
            }
        })
        .detach();
    }

    /// Starts editing a thread's title in its row (double-click, F2, the row menu) or in the
    /// chat title (double-click, the title menu).
    pub(in crate::app) fn start_rename(
        &mut self,
        thread_id: &str,
        origin: MenuOrigin,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(title) = self.thread(thread_id).map(|t| t.title.clone()) else { return };
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title.clone()));
        let subscription =
            cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(origin, window, cx),
                _ => {}
            });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        let renaming =
            Renaming { thread_id: thread_id.to_owned(), input, original: title, _subscription: subscription };
        *self.renaming_slot(origin) = Some(renaming);
        self.notify_sidebar(cx);
        cx.notify();
    }

    pub(in crate::app) fn cancel_rename(&mut self, origin: MenuOrigin, cx: &mut Context<Self>) {
        if self.renaming_slot(origin).take().is_some() {
            self.notify_sidebar(cx);
            cx.notify();
        }
    }

    fn renaming_slot(&mut self, origin: MenuOrigin) -> &mut Option<Renaming> {
        match origin {
            MenuOrigin::Sidebar => &mut self.sidebar.renaming,
            MenuOrigin::Title => &mut self.title.rename,
        }
    }

    /// `resolveRenameCommit`: an empty title is refused, an unchanged one is a no-op; both
    /// refusals and failures toast, as `ChatHeader` and the sidebar do.
    fn commit_rename(&mut self, origin: MenuOrigin, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming_slot(origin).take() else { return };
        let title = renaming.input.read(cx).value().trim().to_owned();
        if title.is_empty() {
            window.push_notification(Notification::warning("").title("Thread title cannot be empty"), cx);
        } else if let (true, Some(client)) = (title != renaming.original, self.thread_client(&renaming.thread_id)) {
            let command = json!({
                "type": "thread.metadata.update",
                "commandId": new_id(),
                "threadId": renaming.thread_id,
                "title": title,
            });
            cx.spawn_in(window, async move |_, cx| {
                if let Err(error) = client.request("orchestration.dispatchCommand", command).await {
                    let _ = cx.update(|window, cx| {
                        let toast = Notification::error(error.to_string()).title("Failed to rename thread");
                        window.push_notification(toast, cx);
                    });
                }
            })
            .detach();
        }
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// Opens the snooze menu for a card's clock button at `position`.
    pub(super) fn open_snooze_menu(
        &mut self,
        thread_id: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.weak_entity();
        let snooze = SnoozeItems::new(self, thread_id, MenuOrigin::Sidebar, false, cx);
        // Below the clock, right-aligned (`side="bottom" align="end"`).
        let position = gpui_kit::point(position.x + px(10.), position.y + px(14.));
        self.open_menu(position, gpui_kit::Anchor::TopRight, window, cx, move |menu, _, _| {
            snooze.add_to(&view, menu.min_w(px(160.)))
        });
    }

    /// What a thread's menu shows, read before the menu is built.
    pub(in crate::app) fn thread_menu(
        &self,
        thread_id: &str,
        origin: MenuOrigin,
        cx: &gpui_kit::App,
    ) -> Option<ThreadMenu> {
        let thread = self.thread(thread_id)?;
        let now = now_ms();
        let requests = inbox::child_requests(&self.shell.threads);
        let facts = Facts::new(thread, &requests);
        // The thread's own machine: the menu can open on any listed thread.
        let info = self.env_info(&thread.environment_id).unwrap_or_else(|| self.server_info());
        let project = self.shell.projects.iter().find(|p| p.id == thread.project_id);
        let sidebar = origin == MenuOrigin::Sidebar;
        let groups = self.project_groups();
        let group = project.and_then(|p| groups.iter().find(|g| g.contains(&p.id)));
        let filter = group.filter(|g| sidebar && !g.archived).map(|g| {
            let scoped = self.sidebar.scope == [g.key.clone()];
            let label = if scoped { "Show all projects".to_owned() } else { format!("Filter by {}", g.display_name) };
            (label, if scoped { Vec::new() } else { vec![g.key.clone()] })
        });
        let scratch_root = info.scratch_workspace_root.as_deref().map(crate::project_groups::normalize_path);
        let scratch = project.is_some_and(|p| {
            scratch_root.as_deref() == Some(crate::project_groups::normalize_path(&p.workspace_root).as_str())
        });
        Some(ThreadMenu {
            thread_id: thread.id.clone(),
            origin,
            // Only the chat title finds in its thread (`onFindInThread`).
            find: origin == MenuOrigin::Title && self.thread_find_available(),
            turn_into_project: thread.worktree_path.is_some() && info.capability("threadProjectFromScratch") && scratch,
            // The web offers these once the environment's config has loaded.
            loaded: info.environment_id.is_some(),
            pinned: thread.pinned_at.is_some(),
            settled: thread.settled_override.as_deref() == Some("settled"),
            snoozed: facts.is_snoozed(now),
            snooze: facts.can_snooze(now).then(|| SnoozeItems::new(self, thread_id, origin, true, cx)),
            regenerating: thread.title_regeneration.is_some(),
            auto_settle: info.capability("threadAutoSettleOptOut").then_some(thread.auto_settle_disabled_at.is_none()),
            filter,
            path: thread.worktree_path.clone().or_else(|| project.map(|p| p.workspace_root.clone())),
            branch: thread.branch.clone(),
            link: info.environment_id.as_deref().map(|env| hosted_thread_link(env, &thread.id)),
            running: matches!(facts.status(), inbox::Status::Working | inbox::Status::Preparing),
        })
    }

    /// The shelf a row's inline action acts on (slim rows).
    pub(super) fn slim_action(&mut self, shelf: Shelf, thread_id: &str, cx: &mut Context<Self>) {
        match shelf {
            Shelf::Settled => self.unsettle_thread(thread_id, cx),
            Shelf::Snoozed => self.wake_thread(thread_id, cx),
            Shelf::Working => {}
        }
    }

    /// The footer's undo notice text and button (`SidebarThreadUndoNotice`).
    pub(super) fn render_undo_notice(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let notice = self.sidebar.undo.as_ref()?;
        let tokens = crate::theme::tokens(cx);
        // The web wraps the "Ctrl+Z to undo" text away when it doesn't fit and shows the icon.
        let fits = self.sidebar_width() >= px(300.);
        // `threadUndoLabel`.
        let undo_text =
            self.shortcut_label("thread.undo").map_or_else(|| "Undo".to_owned(), |s| format!("{s} to undo"));
        let undo_tooltip: gpui_kit::SharedString = self.with_shortcut("Undo", "thread.undo").into();
        let label = match notice.count() {
            1 => format!("{},", notice.action.label()),
            n => format!("{} {n},", notice.action.label()),
        };
        Some(
            h_flex()
                .flex_1()
                .min_w_0()
                .h(px(24.))
                .gap(px(4.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_size(px(11.))
                .line_height(px(24.))
                .text_color(tokens.sidebar.muted_foreground)
                .child(label)
                .child(
                    div()
                        .id("sidebar-undo")
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(tokens.foreground)
                        .cursor_pointer()
                        .hover(gpui_kit::Styled::underline)
                        .on_click(cx.listener(|this, _, window, cx| this.undo_thread_action(window, cx)))
                        .tooltip(move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(undo_tooltip.clone()).build(window, cx)
                        })
                        .map(
                            |el| {
                                if fits {
                                    el.child(undo_text)
                                } else {
                                    el.child(Icon::new(Lucide::Undo2).size(px(16.)))
                                }
                            },
                        ),
                ),
        )
    }
}

/// The snooze menu rows for one thread, resolved when the menu opens.
#[derive(Clone)]
pub(super) struct SnoozeItems {
    thread_id: String,
    settings: Vec<inbox::SnoozeSetting>,
    presets: Vec<inbox::SnoozePreset>,
    /// A thread menu's submenu shows "Label (time)"; the card menu right-aligns the time.
    inline_when: bool,
    advance: bool,
    muted: gpui_kit::Hsla,
}

impl SnoozeItems {
    fn new(workspace: &Workspace, thread_id: &str, origin: MenuOrigin, inline_when: bool, cx: &gpui_kit::App) -> Self {
        let settings = inbox::snooze_settings(workspace.server_info().settings.get("snoozePresets"));
        let presets = inbox::snooze_presets(&settings, &chrono::Local::now());
        let muted = crate::theme::tokens(cx).sidebar.secondary_label;
        let advance = origin == MenuOrigin::Sidebar;
        Self { thread_id: thread_id.to_owned(), settings, presets, inline_when, advance, muted }
    }

    /// Adds the preset rows, a separator, and "Custom…" (`SnoozeMenuButton`).
    fn add_to(&self, view: &WeakEntity<Workspace>, mut menu: PopupMenu) -> PopupMenu {
        for preset in &self.presets {
            let (label, when, muted) = (preset.label.clone(), preset.when_label.clone(), self.muted);
            let item = if self.inline_when {
                PopupMenuItem::new(format!("{label} ({when})"))
            } else {
                PopupMenuItem::element(move |_, _| {
                    h_flex().w_full().gap(px(8.)).child(label.clone()).child(
                        div()
                            .ml_auto()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(muted)
                            .child(when.clone()),
                    )
                })
            };
            let (view, this, id) = (view.clone(), self.clone(), preset.id.clone());
            menu = menu.item(item.on_click(move |_, window, cx| {
                let now = chrono::Local::now();
                let Some(wake) = inbox::pick_snooze(&this.presets, &id, &this.settings, &now) else { return };
                let _ = view.update(cx, |workspace, cx| {
                    workspace.snooze_thread(&this.thread_id, wake, this.advance, window, cx);
                });
            }));
        }
        if !self.presets.is_empty() {
            menu = menu.separator();
        }
        let (view, thread_id, advance) = (view.clone(), self.thread_id.clone(), self.advance);
        menu.item(PopupMenuItem::new("Custom…").on_click(move |_, window, cx| {
            let _ = view.update(cx, |workspace, cx| {
                workspace.open_custom_snooze(std::slice::from_ref(&thread_id), advance, window, cx);
            });
        }))
    }
}

/// A thread's menu (`buildThreadActionMenuItems`), from a sidebar row or the chat title.
pub(in crate::app) struct ThreadMenu {
    thread_id: String,
    origin: MenuOrigin,
    /// The environment's config has loaded, so pin, settle, snooze and regenerate apply.
    loaded: bool,
    pinned: bool,
    settled: bool,
    snoozed: bool,
    /// `None` when the thread can't be snoozed now.
    snooze: Option<SnoozeItems>,
    regenerating: bool,
    /// Whether auto-settle is on, when the server lets a thread opt out.
    auto_settle: Option<bool>,
    /// The "Filter by …" label and the scope it sets.
    filter: Option<(String, Vec<String>)>,
    path: Option<String>,
    branch: Option<String>,
    link: Option<String>,
    running: bool,
    find: bool,
    /// A No project thread with its own folder, on a server that can promote it.
    turn_into_project: bool,
}

impl ThreadMenu {
    pub(in crate::app) fn build(
        self,
        view: &WeakEntity<Workspace>,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let item = |label: &str, icon: Lucide| PopupMenuItem::new(label.to_owned()).icon(Icon::new(icon));
        let origin = self.origin;
        let advance = origin == MenuOrigin::Sidebar;
        // Each item acts on this menu's thread through the workspace.
        macro_rules! act {
            ($item:expr, |$this:ident, $window:ident, $cx:ident, $id:ident| $body:expr) => {{
                let view = view.clone();
                let $id = self.thread_id.clone();
                $item.on_click(move |_, $window, $cx| {
                    let _ = view.update($cx, |$this, $cx| $body);
                })
            }};
        }
        let command = |label: &str, icon: Lucide, kind: &'static str, fields: Value| {
            act!(item(label, icon), |this, _w, cx, id| this.thread_command(kind, &id, fields.clone(), cx))
        };
        if self.find {
            menu = menu.item(act!(item("Find in thread", Lucide::TextSearch), |this, window, cx, _id| {
                this.open_thread_find(window, cx);
            }));
        }
        if let Some(branch) = &self.branch {
            menu = menu.item(act!(
                item(&format!("New thread on {branch}"), Lucide::MessageSquarePlus),
                |this, window, cx, id| this.new_thread_on_branch(&id, window, cx)
            ));
        }
        if self.loaded {
            menu = menu.item(if self.pinned {
                act!(item("Unpin thread", Lucide::PinOff), |this, window, cx, id| this.unpin_thread(&id, window, cx))
            } else {
                act!(item("Pin thread", Lucide::Pin), |this, _w, cx, id| this.pin_thread(&id, cx))
            });
            menu = menu.item(if self.settled {
                act!(item("Un-settle thread", Lucide::CircleCheck), |this, _w, cx, id| this.unsettle_thread(&id, cx))
            } else {
                act!(item("Settle thread", Lucide::CircleCheck), |this, window, cx, id| this
                    .settle_thread(&id, advance, window, cx))
            });
            menu = match (&self.snooze, self.snoozed) {
                (_, true) => {
                    menu.item(act!(item("Wake thread", Lucide::Clock), |this, _w, cx, id| this.wake_thread(&id, cx)))
                }
                (Some(snooze), false) => {
                    let (snooze, view) = (snooze.clone(), view.clone());
                    menu.submenu_with_icon(Some(Icon::new(Lucide::Clock)), "Snooze", window, cx, move |menu, _, _| {
                        snooze.add_to(&view, menu.min_w(px(160.)))
                    })
                }
                (None, false) => menu.item(item("Snooze", Lucide::Clock).disabled(true)),
            };
        }
        menu = menu.separator();
        menu = menu.item(act!(item("Rename thread", Lucide::Pencil), |this, window, cx, id| this
            .start_rename(&id, origin, window, cx)));
        if self.loaded {
            menu = menu.item(if self.regenerating {
                item("Regenerating…", Lucide::RefreshCw).disabled(true)
            } else {
                command(
                    "Regenerate title",
                    Lucide::RefreshCw,
                    "thread.metadata.update",
                    json!({"regenerateTitle": true}),
                )
            });
        }
        menu = menu.item(command("Mark unread", Lucide::MailOpen, "thread.mark-unread", json!({})));
        if let Some((label, scope)) = self.filter {
            let view = view.clone();
            menu = menu.item(item(&label, Lucide::FolderTree).on_click(move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.set_scope(scope.clone(), cx));
            }));
        }
        if self.turn_into_project {
            menu = menu.item(
                act!(item("Turn into project", Lucide::FolderPlus), |this, window, cx, id| this
                    .turn_thread_into_project(&id, window, cx))
                .disabled(self.running),
            );
        }
        if let Some(enabled) = self.auto_settle {
            let (view, id) = (view.clone(), self.thread_id.clone());
            menu = menu.submenu_with_icon(
                Some(Icon::new(Lucide::Timer)),
                "Auto-settle behavior",
                window,
                cx,
                move |mut menu, _, _| {
                    for (label, on) in [("Enabled", true), ("Disabled", false)] {
                        let (view, id) = (view.clone(), id.clone());
                        menu = menu.item(PopupMenuItem::new(label).checked(enabled == on).on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.thread_command("thread.auto-settle.set", &id, json!({"enabled": on}), cx)
                            });
                        }));
                    }
                    menu
                },
            );
        }
        menu = menu.separator();
        let (path, branch, link, id) =
            (self.path.clone(), self.branch.clone(), self.link.clone(), self.thread_id.clone());
        menu = menu.submenu_with_icon(Some(Icon::new(Lucide::Copy)), "Copy", window, cx, move |mut menu, _, _| {
            let copy = |text: String| {
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }
            };
            if let Some(path) = &path {
                menu =
                    menu.item(PopupMenuItem::new("Path").icon(Icon::new(Lucide::Folder)).on_click(copy(path.clone())));
            }
            if let Some(branch) = &branch {
                menu = menu.item(
                    PopupMenuItem::new("Branch").icon(Icon::new(Lucide::GitBranch)).on_click(copy(branch.clone())),
                );
            }
            if let Some(link) = &link {
                menu = menu.item(
                    PopupMenuItem::new("Thread").icon(Icon::new(Lucide::MessageSquare)).on_click(copy(link.clone())),
                );
            }
            menu.item(PopupMenuItem::new("Thread ID").icon(Icon::new(Lucide::Hash)).on_click(copy(id.clone())))
        });
        menu = menu
            .item(act!(item("Project settings", Lucide::Settings), |this, _w, cx, _id| this
                .open_settings("projects", cx)));
        menu = menu.separator();
        menu = menu.item(
            act!(item("Archive thread", Lucide::Archive), |this, window, cx, id| this.archive_thread(&id, window, cx))
                .disabled(self.running),
        );
        menu.item(act!(item("Delete", Lucide::Trash), |this, window, cx, id| this.delete_thread(&id, window, cx)))
    }
}
