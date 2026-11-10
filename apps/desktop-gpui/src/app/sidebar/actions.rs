//! Thread actions and their menu, shared by sidebar rows and the chat title: settle, snooze,
//! pin, rename, archive, delete, mark unread, and their undo (`hooks/useThreadActions.ts`,
//! `hooks/useThreadActionMenu.ts`, `components/threadActionMenu.logic.ts`,
//! `SidebarThreadUndoNotice.tsx`).

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{MenuMetrics, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Point, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px,
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

/// Where the window goes once the open thread is parked: the next inbox thread, else a
/// draft in its project.
pub(super) struct Leave {
    thread_id: String,
    next: Option<String>,
    project_id: Option<String>,
}

/// `getFallbackThreadIdAfterDelete`: the newest thread still in the inbox of the deleted
/// thread's project, by the sidebar's thread sort order.
fn fallback_after_delete<'a>(
    threads: &'a [crate::model::ThreadShell],
    deleted: &str,
    sort_order: &str,
    now: i64,
) -> Option<&'a str> {
    let project = &threads.iter().find(|t| t.id == deleted)?.project_id;
    let requests = inbox::child_requests(threads);
    let parse = crate::timestamp::parse_ms;
    let sorted_at = |t: &crate::model::ThreadShell| match sort_order {
        "created_at" => parse(&t.created_at).or_else(|| parse(&t.updated_at)),
        _ => t.latest_user_message_at.as_deref().and_then(parse).or_else(|| parse(&t.updated_at)),
    };
    threads
        .iter()
        .filter(|t| {
            &t.project_id == project
                && t.id != deleted
                && t.is_listed()
                && t.archived_at.is_none()
                && t.deleted_at.is_none()
                && t.settled_override.as_deref() != Some("settled")
                && !Facts::new(t, &requests).is_snoozed(now)
        })
        .max_by(|a, b| sorted_at(a).cmp(&sorted_at(b)).then_with(|| a.id.cmp(&b.id)))
        .map(|t| t.id.as_str())
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
    destructive: bool,
}

const UNDO_TIMEOUT: Duration = Duration::from_secs(5);
/// A batch command's environment has no socket (`chatViewSend.ts`'s wording).
const NOT_CONNECTED: &str = "The environment is not connected.";
/// `threadOperationFailure`: the connection lacks `orchestration:operate`.
const NOT_OPERATE: &str = "This connection cannot change threads.";

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

    /// [`Self::thread_command`], running `accepted` only once the server takes the command:
    /// the web awaits the mutation before it registers an undo or moves on.
    pub(in crate::app) fn thread_command_then(
        &mut self,
        kind: &str,
        thread_id: &str,
        fields: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
        accepted: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let title = failure_title(kind, &fields);
        let command = Self::thread_command_value(kind, thread_id, fields);
        let Some(client) = self.thread_client(thread_id) else { return };
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(_) => accepted(this, window, cx),
                Err(error) => {
                    this.error = Some(failure_message(title, &error.to_string()).into());
                    cx.notify();
                }
            });
        })
        .detach();
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

    /// `planForwardNavigation`: after settling or snoozing the open thread, the web moves on
    /// to the next inbox thread in sidebar order (wrapping), passing over `parked` (threads
    /// parked with it, `coParkingKeys`), else a new draft in its project. It is planned
    /// before the command, as the thread leaves the inbox order once the server takes it.
    pub(super) fn plan_leave(&self, thread_id: &str, parked: &[String]) -> Option<Leave> {
        if self.open_thread_id() != Some(thread_id) {
            return None;
        }
        let order = &self.sidebar.inbox_order;
        let next = order.iter().position(|id| id == thread_id).and_then(|at| {
            order[at + 1..].iter().chain(&order[..at]).find(|id| *id != thread_id && !parked.contains(id)).cloned()
        });
        Some(Leave {
            thread_id: thread_id.to_owned(),
            next,
            project_id: self.thread(thread_id).map(|t| t.project_id.clone()),
        })
    }

    /// Runs the plan if the thread it leaves is still open.
    pub(super) fn leave(&mut self, plan: Option<Leave>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(plan) = plan.filter(|plan| self.open_thread_id() == Some(plan.thread_id.as_str())) else { return };
        match (plan.next, plan.project_id) {
            (Some(next), _) => self.select_thread(next, window, cx),
            (None, Some(project_id)) => self.start_draft(project_id, window, cx),
            (None, None) => {}
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
        let plan = advance.then(|| self.plan_leave(thread_id, &[])).flatten();
        self.thread_command_then("thread.settle", thread_id, json!({}), window, cx, move |this, window, cx| {
            this.push_undo(UndoAction::Settled, entry, cx);
            this.leave(plan, window, cx);
        });
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
        let plan = advance.then(|| self.plan_leave(thread_id, &[])).flatten();
        self.thread_command_then("thread.snooze", thread_id, wake, window, cx, move |this, window, cx| {
            this.push_undo(UndoAction::Snoozed, entry, cx);
            this.leave(plan, window, cx);
        });
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
            destructive: false,
        };
        self.confirm_then(question, window, cx, move |this, window, cx| {
            let entry = this.undo_entry(&thread_id);
            this.thread_command_then("thread.unpin", &thread_id, json!({}), window, cx, move |this, _, cx| {
                this.push_undo(UndoAction::Unpinned, entry, cx);
            });
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
            destructive: false,
        };
        self.confirm_then(question, window, cx, move |this, window, cx| {
            for id in &ids {
                let entry = this.undo_entry(id);
                this.thread_command_then("thread.unpin", id, json!({}), window, cx, move |this, _, cx| {
                    this.push_undo(UndoAction::Unpinned, entry, cx);
                });
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
            destructive: false,
        };
        self.confirm_then(question, window, cx, move |this, window, cx| {
            let entry = this.undo_entry(&thread_id);
            let was_open = this.open_thread_id() == Some(&*thread_id);
            let id = thread_id.clone();
            this.thread_command_then("thread.archive", &thread_id, json!({}), window, cx, move |this, window, cx| {
                this.push_undo(UndoAction::Archived, entry, cx);
                // Only when the user is still on the thread they archived.
                if was_open && this.open_thread_id() == Some(&*id) {
                    this.start_draft(project_id, window, cx);
                }
            });
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
            destructive: true,
        };
        self.confirm_then(question, window, cx, move |this, window, cx| this.delete_confirmed(&thread_id, window, cx));
    }

    /// After a delete of the open thread: the newest open thread in its project, else a draft in
    /// the most recently active project, as the index route lands. Another thread opened since stays.
    fn land_after_delete(
        &mut self,
        deleted: &str,
        fallback: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The deletion event may already have closed it.
        if !(self.target.is_none() || self.open_thread_id() == Some(deleted)) {
            return;
        }
        match fallback.filter(|next| self.thread(next).is_some()) {
            Some(next) => self.select_thread(next, window, cx),
            None => {
                let landing = inbox::most_recent_project(&self.shell.projects, &self.shell.threads);
                if let Some(project_id) = landing.map(|p| p.id.clone()) {
                    self.start_draft(project_id, window, cx);
                }
            }
        }
    }

    /// `deleteThread` once agreed: offers to remove a worktree only this thread uses, stops the
    /// thread's provider sessions, deletes it, then removes the worktree if asked to.
    fn delete_confirmed(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(thread)) = (self.thread_client(thread_id), self.thread(thread_id)) else { return };
        let was_open = self.open_thread_id() == Some(thread_id);
        let sort_order =
            self.client_settings.values.get("sidebarThreadSortOrder").as_str().unwrap_or_default().to_owned();
        let fallback = fallback_after_delete(&self.shell.threads, thread_id, &sort_order, now_ms()).map(str::to_owned);
        let environment = thread.environment_id.clone();
        let info = self.env_info(&environment).unwrap_or_else(|| self.server_info());
        let project = self.shell.projects.iter().find(|p| p.id == thread.project_id);
        let scratch_root = info.scratch_workspace_root.as_deref().map(crate::project_groups::normalize_path);
        let removable = project.filter(|p| {
            scratch_root.as_deref() != Some(crate::project_groups::normalize_path(&p.workspace_root).as_str())
        });
        let siblings: Vec<_> = self.shell.threads.iter().filter(|t| t.environment_id == environment).collect();
        let orphan = orphaned_worktree(&siblings, thread_id)
            .filter(|_| removable.is_some() && self.env_grants(&environment, "source-control:write"));
        let automatic = removable.is_some_and(|p| {
            crate::settings::scope::worktree_cleanup_rules(&info.settings, Some(&p.scope_project()))
                .get("worktreeOnDelete")
                .and_then(Value::as_bool)
                == Some(true)
        });
        // A shell that leaves idle subagent children out can't prove nobody else uses the worktree.
        let ask_server = info.config.get("shellThreadFamilies").and_then(Value::as_bool) == Some(true);
        let has_runtime = Facts::new(thread, &std::collections::HashMap::new()).runtime_status().is_some();
        let project_root = removable.map(|p| p.workspace_root.clone());
        let thread_id = thread_id.to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let mut orphan = orphan.filter(|_| !automatic);
            if let (Some(path), true) = (&orphan, ask_server) {
                let occupants =
                    client.request("orchestration.getWorktreeThreadIds", json!({"worktreePath": path})).await;
                let alone = occupants.ok().and_then(|value| {
                    let ids = value["threadIds"].as_array()?.clone();
                    Some(ids.iter().all(|id| id.as_str() == Some(&thread_id)))
                });
                if alone != Some(true) {
                    orphan = None;
                }
            }
            let mut remove = false;
            if let Some(path) = &orphan {
                let message = format!(
                    "This thread is the only one linked to this worktree:\n{}\n\nDelete the worktree too?",
                    worktree_display(path)
                );
                let Ok(answer) =
                    this.update_in(cx, |_, window, cx| super::super::confirm::confirm(&message, true, window, cx))
                else {
                    return;
                };
                let Ok(agreed) = answer.await else { return };
                remove = agreed;
            }
            if has_runtime {
                stop_provider_sessions(&client, &thread_id).await;
            }
            let command = Self::thread_command_value("thread.delete", &thread_id, json!({}));
            if let Err(error) = client.request("orchestration.dispatchCommand", command).await {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(failure_message(Some("Failed to delete thread"), &error.to_string()).into());
                    cx.notify();
                });
                return;
            }
            let removing = this.update_in(cx, |this, window, cx| {
                this.expire_undo(&thread_id);
                this.forget_thread_panel(&environment, &thread_id);
                if was_open {
                    this.land_after_delete(&thread_id, fallback, window, cx);
                }
                remove && this.env_grants(&environment, "source-control:write")
            });
            let (Some(path), Some(root), Ok(true)) = (orphan.filter(|_| remove), project_root, removing) else {
                return;
            };
            let removed = client
                .request("vcs.removeWorktree", json!({"cwd": root, "path": path, "force": true}))
                .await
                .map_err(|error| {
                    ("Failed to delete worktree", format!("Could not remove {}. {error}", worktree_display(&path)))
                });
            let failure = match removed {
                Ok(_) => client
                    .request("vcs.refreshStatus", json!({"cwd": root}))
                    .await
                    .err()
                    .map(|error| ("Worktree deleted, but Git status refresh failed", error.to_string())),
                Err(failure) => Some(failure),
            };
            if let Some((title, description)) = failure {
                let _ = this.update_in(cx, |_, window, cx| {
                    window.push_notification(Notification::error(description).title(title), cx);
                });
            }
        })
        .detach();
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
        let message = question.detail.map_or_else(
            || question.message.clone(),
            |detail| {
                format!(
                    "{}
{detail}",
                    question.message
                )
            },
        );
        let answer = super::super::confirm::confirm(&message, question.destructive, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(true) {
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
        // A read-only connection can't rename (`startRename`).
        let Some(thread) = self.thread(thread_id).filter(|t| self.can_operate(&t.environment_id)) else { return };
        let title = thread.title.clone();
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
            environment_id: thread.environment_id.clone(),
            can_operate: self.can_operate(&thread.environment_id),
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
            share: self.can_share_with_guest(&thread.environment_id),
            running: !facts.can_archive(),
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
                    super::keyboard_button(div().id("sidebar-undo"), tokens.ring)
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
        let presets = inbox::snooze_presets(&settings, &chrono::Local::now(), workspace.hour12());
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
                // `MenuItemDetail`: the wake time reads as text at the row's size.
                PopupMenuItem::element(move |_, _| {
                    h_flex()
                        .w_full()
                        .child(label.clone())
                        .child(div().ml_auto().flex_none().pl(px(16.)).text_color(muted).child(when.clone()))
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
    environment_id: gpui_kit::SharedString,
    /// A read-only connection keeps navigation, read markers and copying; the rest is disabled.
    can_operate: bool,
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
    /// "Share with guest": its own grant, so read-only connections keep it (`canShareWithGuest`).
    share: bool,
    /// A No project thread with its own folder, on a server that can promote it.
    turn_into_project: bool,
}

/// Electron shows a thread's menu natively: on Windows, Segoe UI at 9pt in 28px rows (26px plus
/// the menu's 2px row gap). macOS keeps the reviewed look.
/// Electron's Windows menu: Segoe UI rows 28px tall, 21px text inset, 17px separators, and its
/// own dark colors (the light ones follow the theme).
fn native_rows(menu: PopupMenu, cx: &gpui_kit::App) -> PopupMenu {
    if !cfg!(windows) {
        return menu;
    }
    let dark = gpui_kit::component::ActiveTheme::theme(cx).is_dark();
    menu.metrics(MenuMetrics {
        font_family: Some("Segoe UI".into()),
        text_size: px(12.),
        row_height: px(28.),
        padding_y: px(11.),
        item_padding_x: px(17.),
        separator_height: px(17.),
        background: dark.then(|| gpui_kit::rgb(0x1f1f1f).into()),
        separator: dark.then(|| gpui_kit::rgb(0x5e5e5e).into()),
    })
}

/// `getOrphanedWorktreePathForThread`: the thread's worktree when no other listed thread uses it.
fn orphaned_worktree(threads: &[&crate::model::ThreadShell], thread_id: &str) -> Option<String> {
    let path = threads.iter().find(|t| t.id == thread_id)?.worktree_path.as_deref()?.trim();
    if path.is_empty() {
        return None;
    }
    let key = crate::project_groups::normalize_path(path);
    let shared = threads.iter().any(|t| {
        t.id != thread_id && t.worktree_path.as_deref().is_some_and(|p| crate::project_groups::normalize_path(p) == key)
    });
    (!shared).then(|| path.to_owned())
}

/// `formatWorktreePathForDisplay`: the worktree's folder name.
fn worktree_display(path: &str) -> &str {
    let trimmed = path.trim();
    trimmed
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
}

/// `stopThreadSession`: detaches each provider session the thread holds; failures don't stop
/// the delete, which detaches them on the server too.
async fn stop_provider_sessions(client: &crate::rpc::RpcClient, thread_id: &str) {
    let Ok(projection) = client.request("orchestration.getThreadProjection", json!({"threadId": thread_id})).await
    else {
        return;
    };
    let command_id = new_id();
    let sessions = projection["providerSessions"].as_array().cloned().unwrap_or_default();
    for session in sessions.iter().filter_map(|s| s["id"].as_str()) {
        let command = json!({
            "type": "provider-session.detach",
            "commandId": format!("{command_id}:detach:{session}"),
            "threadId": thread_id,
            "providerSessionId": session,
            "reason": "client-requested",
        });
        let _ = client.request("orchestration.dispatchCommand", command).await;
    }
}

/// A submenu a read-only connection can't open: the web disables it and its children.
fn locked_submenu(label: &'static str, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenuItem {
    PopupMenuItem::submenu(label, PopupMenu::build(window, cx, |menu, _, _| menu)).disabled(true)
}

impl ThreadMenu {
    pub(in crate::app) fn build(
        self,
        view: &WeakEntity<Workspace>,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        // Electron shows this as a native menu: plain rows, a trash icon on Delete only (macOS).
        menu = native_rows(menu, cx);
        let item = |label: &str| PopupMenuItem::new(label.to_owned());
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
        // `act!` for an action that changes the thread: disabled on a read-only connection, and
        // checked again on click, since the grant can change while the menu is open.
        let can_operate = self.can_operate;
        macro_rules! op {
            ($item:expr, $disabled:expr, |$this:ident, $window:ident, $cx:ident, $id:ident| $body:expr) => {{
                let view = view.clone();
                let $id = self.thread_id.clone();
                let environment = self.environment_id.clone();
                $item.disabled($disabled || !can_operate).on_click(move |_, $window, $cx| {
                    let _ = view.update($cx, |$this, $cx| {
                        if $this.can_operate(&environment) {
                            let _ = $body;
                        } else {
                            $this.error = Some(failure_message(Some("Thread action unavailable"), NOT_OPERATE).into());
                            $cx.notify();
                        }
                    });
                })
            }};
        }
        if self.find {
            menu = menu.item(act!(item("Find in thread"), |this, window, cx, _id| {
                this.open_thread_find(window, cx);
            }));
        }
        if let Some(branch) = &self.branch {
            menu = menu.item(act!(item(&format!("New thread on {branch}")), |this, window, cx, id| this
                .new_thread_on_branch(&id, window, cx)));
        }
        if self.loaded {
            menu = menu.item(if self.pinned {
                op!(item("Unpin thread"), false, |this, window, cx, id| this.unpin_thread(&id, window, cx))
            } else {
                op!(item("Pin thread"), false, |this, _w, cx, id| this.pin_thread(&id, cx))
            });
            menu = menu.item(if self.settled {
                op!(item("Un-settle thread"), false, |this, _w, cx, id| this.unsettle_thread(&id, cx))
            } else {
                op!(item("Settle thread"), false, |this, window, cx, id| this.settle_thread(&id, advance, window, cx))
            });
            menu = match (&self.snooze, self.snoozed) {
                (_, true) => menu.item(op!(item("Wake thread"), false, |this, _w, cx, id| this.wake_thread(&id, cx))),
                (Some(_), false) if !can_operate => menu.item(locked_submenu("Snooze", window, cx)),
                (Some(snooze), false) => {
                    let (snooze, view) = (snooze.clone(), view.clone());
                    menu.submenu("Snooze", window, cx, move |menu, _, cx| {
                        snooze.add_to(&view, native_rows(menu, cx).min_w(px(160.)))
                    })
                }
                (None, false) => menu.item(item("Snooze").disabled(true)),
            };
        }
        menu = menu.separator();
        menu = menu
            .item(op!(item("Rename thread"), false, |this, window, cx, id| this.start_rename(&id, origin, window, cx)));
        if self.loaded {
            let regenerate = json!({"regenerateTitle": true});
            menu = menu.item(if self.regenerating {
                item("Regenerating…").disabled(true)
            } else {
                op!(item("Regenerate title"), false, |this, _w, cx, id| this.thread_command(
                    "thread.metadata.update",
                    &id,
                    regenerate.clone(),
                    cx
                ))
            });
        }
        menu = menu.item(act!(item("Mark unread"), |this, _w, cx, id| this.thread_command(
            "thread.mark-unread",
            &id,
            json!({}),
            cx
        )));
        if self.share {
            menu = menu
                .item(act!(item("Share with guest"), |this, window, cx, id| this.open_guest_share(&id, window, cx)));
        }
        if let Some((label, scope)) = self.filter {
            menu = menu.item(op!(item(&label), false, |this, _w, cx, _id| this.set_scope(scope.clone(), cx)));
        }
        if self.turn_into_project {
            menu = menu.item(op!(item("Turn into project"), self.running, |this, window, cx, id| this
                .turn_thread_into_project(&id, window, cx)));
        }
        if self.auto_settle.is_some() && !can_operate {
            menu = menu.item(locked_submenu("Auto-settle behavior", window, cx));
        } else if let Some(enabled) = self.auto_settle {
            let (view, id) = (view.clone(), self.thread_id.clone());
            menu = menu.submenu("Auto-settle behavior", window, cx, move |menu, _, cx| {
                let mut menu = native_rows(menu, cx);
                for (label, on) in [("Enabled", true), ("Disabled", false)] {
                    let (view, id) = (view.clone(), id.clone());
                    menu = menu.item(PopupMenuItem::new(label).checked(enabled == on).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.thread_command("thread.auto-settle.set", &id, json!({"enabled": on}), cx)
                        });
                    }));
                }
                menu
            });
        }
        menu = menu.separator();
        let (path, branch, link, id) =
            (self.path.clone(), self.branch.clone(), self.link.clone(), self.thread_id.clone());
        menu = menu.submenu("Copy", window, cx, move |menu, _, cx| {
            let mut menu = native_rows(menu, cx);
            let copy = |text: String| {
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                }
            };
            if let Some(path) = &path {
                menu = menu.item(PopupMenuItem::new("Path").on_click(copy(path.clone())));
            }
            if let Some(branch) = &branch {
                menu = menu.item(PopupMenuItem::new("Branch").on_click(copy(branch.clone())));
            }
            if let Some(link) = &link {
                menu = menu.item(PopupMenuItem::new("Thread").on_click(copy(link.clone())));
            }
            menu.item(PopupMenuItem::new("Thread ID").on_click(copy(id.clone())))
        });
        menu = menu.item(act!(item("Project settings"), |this, _w, cx, id| {
            if let Some(project_id) = this.shell.threads.iter().find(|t| t.id == id).map(|t| t.project_id.clone()) {
                this.open_project_settings(&project_id, cx);
            }
        }));
        menu = menu.separator();
        menu = menu.item(
            op!(item("Archive thread"), self.running, |this, window, cx, id| this.archive_thread(&id, window, cx))
        );
        let delete =
            if cfg!(target_os = "macos") { item("Delete").icon(Icon::new(Lucide::Trash)) } else { item("Delete") };
        menu.item(op!(delete, false, |this, window, cx, id| this.delete_thread(&id, window, cx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_worktree_is_orphaned_only_when_no_other_thread_uses_it() {
        let thread = |id: &str, worktree: Option<&str>| -> crate::model::ThreadShell {
            serde_json::from_value(json!({
                "id": id, "projectId": "p", "title": id, "status": "idle",
                "updatedAt": "2026-01-01T00:00:00.000Z", "worktreePath": worktree,
            }))
            .unwrap()
        };
        let (a, b) = (thread("a", Some("/repo/wt")), thread("b", Some("/repo/wt/")));
        let (c, local) = (thread("c", Some("/repo/other")), thread("d", None));
        assert_eq!(orphaned_worktree(&[&a, &c, &local], "a").as_deref(), Some("/repo/wt"));
        // The same checkout spelled with a trailing slash is shared.
        assert_eq!(orphaned_worktree(&[&a, &b], "a"), None);
        assert_eq!(orphaned_worktree(&[&a, &local], "d"), None);
        assert_eq!(orphaned_worktree(&[&a], "missing"), None);
    }

    #[test]
    fn worktree_paths_show_their_folder_name() {
        assert_eq!(worktree_display("/repo/.t3/worktrees/feature-x/"), "feature-x");
        assert_eq!(worktree_display(r"C:\repo\wt\feature"), "feature");
        assert_eq!(worktree_display("plain"), "plain");
    }

    fn thread(id: &str, project: &str, extra: serde_json::Value) -> crate::model::ThreadShell {
        let mut value = json!({
            "id": id, "projectId": project, "title": id, "status": "idle",
            "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z",
            "lineage": {"rootThreadId": id},
        });
        value.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn deleting_falls_back_to_the_newest_open_thread_in_its_project() {
        let now = crate::timestamp::parse_ms("2026-11-04T00:00:00.000Z").unwrap();
        let threads = [
            thread("gone", "p", json!({})),
            thread("old", "p", json!({"latestUserMessageAt": "2026-11-02T00:00:00.000Z"})),
            thread(
                "new",
                "p",
                json!({"latestUserMessageAt": "2026-11-03T00:00:00.000Z", "createdAt": "2026-10-01T00:00:00.000Z"}),
            ),
            thread(
                "settled",
                "p",
                json!({"latestUserMessageAt": "2026-11-03T12:00:00.000Z", "settledOverride": "settled"}),
            ),
            thread(
                "snoozed",
                "p",
                json!({"latestUserMessageAt": "2026-11-03T12:00:00.000Z", "snoozedUntil": "2026-11-05T00:00:00.000Z"}),
            ),
            thread(
                "archived",
                "p",
                json!({"latestUserMessageAt": "2026-11-03T12:00:00.000Z", "archivedAt": "2026-11-03T00:00:00.000Z"}),
            ),
            thread("elsewhere", "q", json!({"latestUserMessageAt": "2026-11-03T12:00:00.000Z"})),
        ];
        assert_eq!(fallback_after_delete(&threads, "gone", "updated_at", now), Some("new"));
        assert_eq!(fallback_after_delete(&threads, "gone", "created_at", now), Some("old"));
        assert_eq!(fallback_after_delete(&threads, "elsewhere", "updated_at", now), None);
        assert_eq!(fallback_after_delete(&threads, "missing", "updated_at", now), None);
    }
}
