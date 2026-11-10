//! Multi-select in the sidebar: Ctrl-click toggles a row, Shift-click selects a range, and a
//! right-click on a selected row acts on them all (`threadSelectionStore.ts`,
//! `handleMultiSelectContextMenu` in `Sidebar.tsx`).

use std::collections::HashSet;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{Context, WeakEntity, Window, px};
use serde_json::json;

use super::actions::UndoAction;
use super::now_ms;
use crate::app::Workspace;
use crate::inbox::{self, Facts};
use crate::model::{ScopedId, ThreadShell};

/// The selected rows and the anchor a Shift-click ranges from, by environment and thread id.
#[derive(Default)]
pub(super) struct Selection {
    keys: HashSet<ScopedId>,
    anchor: Option<ScopedId>,
}

impl Selection {
    pub(super) fn contains(&self, key: &ScopedId) -> bool {
        self.keys.contains(key)
    }

    /// Whether `thread` is selected, without building its key.
    pub(super) fn has(&self, thread: &ThreadShell) -> bool {
        !self.keys.is_empty() && self.keys.iter().any(|key| key.is_thread(thread))
    }

    pub(super) fn toggle(&mut self, key: &ScopedId) {
        if !self.keys.remove(key) {
            self.keys.insert(key.clone());
            self.anchor = Some(key.clone());
        }
    }

    /// Adds the rows from the anchor to `key` in `ordered`; with no usable anchor, just it.
    pub(super) fn range_to(&mut self, key: &ScopedId, ordered: &[ScopedId]) {
        let at = |key: &ScopedId| ordered.iter().position(|k| k == key);
        match self.anchor.as_ref().and_then(at).zip(at(key)) {
            Some((from, to)) => self.keys.extend(ordered[from.min(to)..=from.max(to)].iter().cloned()),
            None => {
                self.keys.insert(key.clone());
                self.anchor = Some(key.clone());
            }
        }
    }

    pub(super) fn set_anchor(&mut self, key: &ScopedId) {
        self.anchor = Some(key.clone());
    }

    /// Clears the selection; `true` when it had any.
    pub(super) fn clear(&mut self) -> bool {
        let had = !self.keys.is_empty();
        self.keys.clear();
        self.anchor = None;
        had
    }

    pub(super) fn remove(&mut self, keys: &[ScopedId]) {
        self.keys.retain(|key| !keys.contains(key));
        if self.anchor.as_ref().is_some_and(|anchor| keys.contains(anchor)) {
            self.anchor = None;
        }
    }
}

fn counted(label: &str, count: usize) -> String {
    format!("{label} ({count})")
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// What the bulk menu offers, read before it is built.
pub(super) struct BulkMenu {
    ids: Vec<ScopedId>,
    pinned: Vec<ScopedId>,
    settling: Vec<ScopedId>,
    /// The snooze presets, when every selected thread can snooze.
    snooze: Option<(Vec<inbox::SnoozeSetting>, Vec<inbox::SnoozePreset>)>,
    regeneratable: Vec<ScopedId>,
    /// How many can regenerate titles at all (once the config has loaded).
    supported: usize,
    /// `canOperateThreads`: this connection may change every selected thread; read-only ones
    /// keep Mark unread.
    can_operate: bool,
}

impl BulkMenu {
    pub(super) fn build(
        self,
        view: &WeakEntity<Workspace>,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let count = self.ids.len();
        if count == 0 {
            return menu;
        }
        macro_rules! act {
            ($label:expr, |$this:ident, $window:ident, $cx:ident| $body:expr) => {{
                let view = view.clone();
                PopupMenuItem::new($label).on_click(move |_, $window, $cx| {
                    let _ = view.update($cx, |$this, $cx| $body);
                })
            }};
        }
        // `act!` for an action that changes the threads: disabled on a read-only connection,
        // and checked again on click, since a grant can go while the menu is open.
        let locked = !self.can_operate;
        macro_rules! op {
            ($label:expr, $ids:expr, |$this:ident, $window:ident, $cx:ident| $body:expr) => {{
                let (view, ids) = (view.clone(), $ids.clone());
                PopupMenuItem::new($label).disabled(locked).on_click(move |_, $window, $cx| {
                    let _ = view.update($cx, |$this, $cx| {
                        if $this.check_thread_operations(&ids, $cx) {
                            $body
                        }
                    });
                })
            }};
        }
        if !self.pinned.is_empty() {
            let pinned = self.pinned.clone();
            menu = menu.item(op!(counted("Unpin", pinned.len()), pinned, |this, window, cx| {
                this.unpin_threads(&pinned, window, cx);
                this.clear_selection(cx);
            }));
        }
        let settle = self.settling.clone();
        menu = menu.item(
            op!(counted("Settle", count), settle, |this, window, cx| this.settle_threads(&settle, window, cx))
                .disabled(locked || self.settling.is_empty()),
        );
        match self.snooze {
            // The web disables the submenu and its children.
            Some(_) if locked => {
                let empty = PopupMenu::build(window, cx, |menu, _, _| menu);
                menu = menu.item(PopupMenuItem::submenu(counted("Snooze", count), empty).disabled(true));
            }
            Some((settings, presets)) => {
                let (view, ids) = (view.clone(), self.ids.clone());
                menu = menu.submenu(counted("Snooze", count), window, cx, move |mut menu, _, _| {
                    for preset in &presets {
                        let (view, ids) = (view.clone(), ids.clone());
                        let (presets, settings, id) = (presets.clone(), settings.clone(), preset.id.clone());
                        let label = format!("{} ({})", preset.label, preset.when_label);
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            let now = chrono::Local::now();
                            let Some(wake) = inbox::pick_snooze(&presets, &id, &settings, &now) else { return };
                            let _ = view.update(cx, |this, cx| this.snooze_threads(&ids, wake, window, cx));
                        }));
                    }
                    if !presets.is_empty() {
                        menu = menu.separator();
                    }
                    let (view, ids) = (view.clone(), ids.clone());
                    menu.item(PopupMenuItem::new("Custom…").on_click(move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            if this.check_thread_operations(&ids, cx) {
                                this.open_custom_snooze(&ids, true, window, cx);
                            }
                        });
                    }))
                });
            }
            None => {}
        }
        if self.supported > 0 {
            menu = menu.item(if self.regeneratable.is_empty() {
                PopupMenuItem::new(counted("Regenerating…", self.supported)).disabled(true)
            } else {
                let regenerate = self.regeneratable.clone();
                op!(counted("Regenerate titles", regenerate.len()), regenerate, |this, _window, cx| {
                    if !this.threads_connected(&regenerate) {
                        return;
                    }
                    let fields = || json!({"regenerateTitle": true});
                    let commands = regenerate
                        .iter()
                        .map(|id| {
                            (
                                id.env.clone(),
                                Workspace::thread_command_value("thread.metadata.update", &id.id, fields()),
                            )
                        })
                        .collect();
                    this.dispatch_batch(commands, true, |_, _| "Failed to regenerate thread titles".into(), cx);
                    this.clear_selection(cx);
                })
            });
        }
        let unread = self.ids.clone();
        menu = menu.item(act!(counted("Mark unread", count), |this, _window, cx| {
            for id in &unread {
                this.thread_command("thread.mark-unread", id, json!({}), cx);
            }
            this.clear_selection(cx);
        }));
        let delete = self.ids;
        menu.item(op!(counted("Delete", count), delete, |this, window, cx| this.delete_threads(&delete, window, cx)))
            .min_w(px(200.))
    }
}

impl Workspace {
    /// The selected threads that still exist, in sidebar order.
    fn selected_threads(&self) -> Vec<ScopedId> {
        let selection = &self.sidebar.selection;
        let mut ids: Vec<ScopedId> =
            self.shell.threads.iter().filter(|t| selection.has(t)).map(ScopedId::of_thread).collect();
        let rank = |id: &ScopedId| self.sidebar.thread_order.iter().position(|o| o == id).unwrap_or(usize::MAX);
        ids.sort_by_key(rank);
        ids
    }

    pub(super) fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.selection.clear() {
            self.notify_sidebar(cx);
        }
    }

    /// The menu a right-click on a selected row opens (`handleMultiSelectContextMenu`), or
    /// `None` when the row is not selected.
    pub(super) fn bulk_menu(&self, key: &ScopedId) -> Option<BulkMenu> {
        if !self.sidebar.selection.contains(key) {
            return None;
        }
        let ids = self.selected_threads();
        let now = now_ms();
        let requests = inbox::child_requests(&self.shell.threads);
        let threads: Vec<_> = ids.iter().filter_map(|id| self.shell.thread(id)).collect();
        // Offered once every selected thread's machine has sent its config.
        let loaded =
            threads.iter().all(|t| self.env_info(&t.environment_id).is_some_and(|i| i.environment_id.is_some()));
        let pick = |keep: &dyn Fn(&ThreadShell) -> bool| -> Vec<ScopedId> {
            threads.iter().filter(|t| keep(t)).map(|t| ScopedId::of_thread(t)).collect()
        };
        let info = self.env_info(&key.env).unwrap_or_else(|| self.server_info());
        let settings = inbox::snooze_settings(info.settings.get("snoozePresets"));
        let presets = inbox::snooze_presets(&settings, &chrono::Local::now(), self.hour12());
        Some(BulkMenu {
            pinned: pick(&|t| loaded && t.pinned_at.is_some()),
            settling: pick(&|t| t.settled_override.as_deref() != Some("settled")),
            snooze: (loaded && !threads.is_empty() && threads.iter().all(|t| Facts::new(t, &requests).can_snooze(now)))
                .then_some((settings, presets)),
            regeneratable: pick(&|t| loaded && t.title_regeneration.is_none()),
            supported: if loaded { threads.len() } else { 0 },
            can_operate: threads.iter().all(|t| self.can_operate(&t.environment_id)),
            ids,
        })
    }

    /// Every thread's machine is connected, so no command of a batch is dropped.
    fn threads_connected(&self, ids: &[ScopedId]) -> bool {
        ids.iter().all(|id| self.env_client(&id.env).is_some())
    }

    /// `settleThreads`: settles each, moving on past all of them from the open one.
    fn settle_threads(&mut self, ids: &[ScopedId], window: &mut Window, cx: &mut Context<Self>) {
        if !self.threads_connected(ids) {
            return;
        }
        for id in ids {
            let entry = self.undo_entry(id);
            let plan = self.plan_leave(id, ids);
            self.thread_command_then("thread.settle", id, json!({}), window, cx, move |this, window, cx| {
                this.push_undo(UndoAction::Settled, entry, cx);
                this.leave(plan, window, cx);
            });
        }
        self.clear_selection(cx);
    }

    /// Snoozes each thread until `wake_ms`, moving on past all of them from the open one.
    pub(super) fn snooze_threads(
        &mut self,
        ids: &[ScopedId],
        wake_ms: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if wake_ms <= now_ms() {
            self.error = Some("That snooze time has passed. Choose another time.".into());
            cx.notify();
            return;
        }
        if !self.threads_connected(ids) || !self.check_thread_operations(ids, cx) {
            return;
        }
        self.clear_selection(cx);
        let until = crate::timestamp::iso(wake_ms).unwrap_or_default();
        let steps: Vec<_> = ids
            .iter()
            .filter_map(|id| {
                let command = Self::thread_command_value("thread.snooze", &id.id, json!({"snoozedUntil": until}));
                Some((self.env_client(&id.env)?, command, self.undo_entry(id), self.plan_leave(id, ids)))
            })
            .collect();
        // Each undo and the move past the open thread wait for the server to take its snooze.
        cx.spawn_in(window, async move |this, cx| {
            let (mut accepted, mut failed, mut first_error) = (Vec::new(), 0, None);
            for (client, command, entry, plan) in steps {
                match client.request("orchestration.dispatchCommand", command).await {
                    Ok(_) => accepted.push((entry, plan)),
                    Err(error) => {
                        failed += 1;
                        first_error.get_or_insert(error.to_string());
                    }
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                let title = match accepted.len() {
                    0 => "Failed to snooze threads".to_owned(),
                    _ => format!("Failed to snooze {failed} thread{}", plural(failed)),
                };
                for (entry, plan) in accepted {
                    this.push_undo(UndoAction::Snoozed, entry, cx);
                    this.leave(plan, window, cx);
                }
                if let Some(error) = first_error {
                    this.error = Some(super::actions::failure_message(Some(&title), &error).into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Deletes the threads, asked first unless `confirmThreadDelete` is off.
    fn delete_threads(&mut self, ids: &[ScopedId], window: &mut Window, cx: &mut Context<Self>) {
        if !self.threads_connected(ids) {
            return;
        }
        let ids = ids.to_vec();
        let count = ids.len();
        // `deleteSelectedThreadEntries`: a failure leaves the rest deleting and its row selected.
        let run = move |this: &mut Self, cx: &mut Context<Self>| {
            // `checkThreadOperations` after the question: a grant can go while it is open.
            if !this.check_thread_operations(&ids, cx) {
                return;
            }
            let steps: Vec<_> = ids
                .iter()
                .filter_map(|id| {
                    let command = Self::thread_command_value("thread.delete", &id.id, json!({}));
                    let environment = id.env.clone();
                    let (client, side_chats) =
                        (this.env_client(&environment)?, this.listed_side_chats(&environment, &id.id));
                    Some((id.clone(), environment, side_chats, client, command))
                })
                .collect();
            cx.spawn(async move |this, cx| {
                let (mut deleted, mut forget, mut first_error) = (HashSet::new(), Vec::new(), None);
                for (id, environment, side_chats, client, command) in steps {
                    match client.request("orchestration.dispatchCommand", command).await {
                        Ok(_) => {
                            forget.push((environment, id.clone(), side_chats));
                            deleted.insert(id);
                        }
                        Err(error) => {
                            first_error.get_or_insert(error.to_string());
                        }
                    }
                }
                let _ = this.update(cx, |this, cx| {
                    for (environment, id, side_chats) in &forget {
                        this.forget_deleted_thread(environment, &id.id, side_chats, cx);
                    }
                    if let Some(error) = first_error {
                        let message = super::actions::failure_message(Some("Failed to delete threads"), &error);
                        this.error = Some(message.into());
                        cx.notify();
                    }
                    let gone = deselect_after_delete(&ids, &deleted, |id| {
                        this.shell.thread(id).is_some_and(|t| t.deleted_at.is_none())
                    });
                    this.sidebar.selection.remove(&gone);
                    this.notify_sidebar(cx);
                });
            })
            .detach();
        };
        if self.client_settings.values.get("confirmThreadDelete").as_bool() == Some(false) {
            run(self, cx);
            return;
        }
        let message = format!(
            "Delete {count} thread{}?
This permanently clears conversation history for these threads.",
            plural(count)
        );
        let answer = super::super::confirm::confirm(&message, true, window, cx);
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(true) {
                let _ = this.update(cx, run);
            }
        })
        .detach();
    }
}

/// `getThreadKeysToDeselectAfterDelete`: the deleted threads and any already gone.
fn deselect_after_delete(
    selected: &[ScopedId],
    deleted: &HashSet<ScopedId>,
    exists: impl Fn(&ScopedId) -> bool,
) -> Vec<ScopedId> {
    selected.iter().filter(|id| deleted.contains(*id) || !exists(id)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str) -> ScopedId {
        ScopedId::new("env", id)
    }

    fn ordered() -> Vec<ScopedId> {
        ["a", "b", "c", "d", "e"].map(key).to_vec()
    }

    fn keys(selection: &Selection) -> Vec<&str> {
        let mut keys: Vec<&str> = selection.keys.iter().map(|k| k.id.as_str()).collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn a_plain_click_anchor_ranges_and_the_anchor_stays_put() {
        let mut selection = Selection::default();
        selection.set_anchor(&key("b"));
        selection.range_to(&key("d"), &ordered());
        assert_eq!(keys(&selection), ["b", "c", "d"]);
        selection.range_to(&key("e"), &ordered());
        assert_eq!(selection.anchor, Some(key("b")));
        assert_eq!(keys(&selection), ["b", "c", "d", "e"]);
    }

    #[test]
    fn a_range_without_a_usable_anchor_selects_the_row() {
        let mut selection = Selection::default();
        selection.set_anchor(&key("gone"));
        selection.range_to(&key("c"), &ordered());
        assert_eq!(keys(&selection), ["c"]);
        assert_eq!(selection.anchor, Some(key("c")));
    }

    #[test]
    fn a_range_keeps_rows_selected_outside_it() {
        let mut selection = Selection::default();
        selection.toggle(&key("a"));
        selection.toggle(&key("c"));
        selection.range_to(&key("d"), &ordered());
        assert_eq!(keys(&selection), ["a", "c", "d"]);
        selection.toggle(&key("c"));
        assert_eq!(keys(&selection), ["a", "d"]);
    }

    #[test]
    fn the_same_id_on_another_environment_is_another_row() {
        let mut selection = Selection::default();
        let elsewhere = ScopedId::new("copy", "a");
        selection.toggle(&key("a"));
        assert!(selection.contains(&key("a")));
        assert!(!selection.contains(&elsewhere));
        selection.toggle(&elsewhere);
        selection.toggle(&key("a"));
        assert!(selection.contains(&elsewhere) && !selection.contains(&key("a")));
    }

    #[test]
    fn removing_the_anchor_drops_it() {
        let mut selection = Selection::default();
        selection.toggle(&key("a"));
        selection.toggle(&key("b"));
        selection.remove(&[key("b")]);
        assert_eq!(keys(&selection), ["a"]);
        assert_eq!(selection.anchor, None);
        assert!(selection.clear());
        assert!(!selection.clear());
    }

    #[test]
    fn a_failed_delete_keeps_its_row_selected() {
        let deleted = HashSet::from([key("a")]);
        let gone = deselect_after_delete(&ordered()[..3], &deleted, |id| id.id != "c");
        assert_eq!(gone, [key("a"), key("c")], "b failed and still exists, so it stays selected");
    }
}
