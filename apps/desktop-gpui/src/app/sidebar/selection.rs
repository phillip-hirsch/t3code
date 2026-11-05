//! Multi-select in the sidebar: Ctrl-click toggles a row, Shift-click selects a range, and a
//! right-click on a selected row acts on them all (`threadSelectionStore.ts`,
//! `handleMultiSelectContextMenu` in `Sidebar.tsx`).

use std::collections::HashSet;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{Context, PromptLevel, WeakEntity, Window, px};
use serde_json::json;

use super::actions::UndoAction;
use super::now_ms;
use crate::app::{Target, Workspace};
use crate::inbox::{self, Facts};

/// The selected rows and the anchor a Shift-click ranges from.
#[derive(Default)]
pub(super) struct Selection {
    keys: HashSet<String>,
    anchor: Option<String>,
}

impl Selection {
    pub(super) fn contains(&self, thread_id: &str) -> bool {
        self.keys.contains(thread_id)
    }

    pub(super) fn toggle(&mut self, thread_id: &str) {
        if !self.keys.remove(thread_id) {
            self.keys.insert(thread_id.to_owned());
            self.anchor = Some(thread_id.to_owned());
        }
    }

    /// Adds the rows from the anchor to `thread_id` in `ordered`; with no usable anchor, just it.
    pub(super) fn range_to(&mut self, thread_id: &str, ordered: &[String]) {
        let at = |key: &str| ordered.iter().position(|k| k == key);
        match self.anchor.as_deref().and_then(at).zip(at(thread_id)) {
            Some((from, to)) => self.keys.extend(ordered[from.min(to)..=from.max(to)].iter().cloned()),
            None => {
                self.keys.insert(thread_id.to_owned());
                self.anchor = Some(thread_id.to_owned());
            }
        }
    }

    pub(super) fn set_anchor(&mut self, thread_id: &str) {
        self.anchor = Some(thread_id.to_owned());
    }

    /// Clears the selection; `true` when it had any.
    pub(super) fn clear(&mut self) -> bool {
        let had = !self.keys.is_empty();
        self.keys.clear();
        self.anchor = None;
        had
    }

    pub(super) fn remove(&mut self, thread_ids: &[String]) {
        self.keys.retain(|key| !thread_ids.contains(key));
        if self.anchor.as_ref().is_some_and(|anchor| thread_ids.contains(anchor)) {
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
    ids: Vec<String>,
    pinned: Vec<String>,
    settling: Vec<String>,
    /// The snooze presets, when every selected thread can snooze.
    snooze: Option<(Vec<inbox::SnoozeSetting>, Vec<inbox::SnoozePreset>)>,
    regeneratable: Vec<String>,
    /// How many can regenerate titles at all (once the config has loaded).
    supported: usize,
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
        if !self.pinned.is_empty() {
            let pinned = self.pinned.clone();
            menu = menu.item(act!(counted("Unpin", pinned.len()), |this, window, cx| {
                this.unpin_threads(&pinned, window, cx);
                this.clear_selection(cx);
            }));
        }
        let settle = self.settling.clone();
        menu = menu.item(
            act!(counted("Settle", count), |this, window, cx| this.settle_threads(&settle, window, cx))
                .disabled(self.settling.is_empty()),
        );
        if let Some((settings, presets)) = self.snooze {
            let (view, ids) = (view.clone(), self.ids.clone());
            menu = menu.submenu(counted("Snooze", count), window, cx, move |mut menu, _, _| {
                for preset in &presets {
                    let (view, ids, presets, settings) = (view.clone(), ids.clone(), presets.clone(), settings.clone());
                    let id = preset.id.clone();
                    menu = menu.item(PopupMenuItem::new(format!("{} ({})", preset.label, preset.when_label)).on_click(
                        move |_, window, cx| {
                            let now = chrono::Local::now();
                            let Some(wake) = inbox::pick_snooze(&presets, &id, &settings, &now) else { return };
                            let _ = view.update(cx, |this, cx| this.snooze_threads(&ids, wake, window, cx));
                        },
                    ));
                }
                if !presets.is_empty() {
                    menu = menu.separator();
                }
                let (view, ids) = (view.clone(), ids.clone());
                menu.item(PopupMenuItem::new("Custom…").on_click(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| this.open_custom_snooze(&ids, true, window, cx));
                }))
            });
        }
        if self.supported > 0 {
            menu = menu.item(if self.regeneratable.is_empty() {
                PopupMenuItem::new(counted("Regenerating…", self.supported)).disabled(true)
            } else {
                let regenerate = self.regeneratable.clone();
                act!(counted("Regenerate titles", regenerate.len()), |this, _window, cx| {
                    let fields = || json!({"regenerateTitle": true});
                    let commands = regenerate
                        .iter()
                        .map(|id| Workspace::thread_command_value("thread.metadata.update", id, fields()))
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
        menu.item(act!(counted("Delete", count), |this, window, cx| this.delete_threads(&delete, window, cx)))
            .min_w(px(200.))
    }
}

impl Workspace {
    /// The selected threads that still exist, in sidebar order.
    fn selected_threads(&self) -> Vec<String> {
        let selection = &self.sidebar.selection;
        let mut ids: Vec<String> =
            self.shell.threads.iter().filter(|t| selection.contains(&t.id)).map(|t| t.id.clone()).collect();
        let rank = |id: &String| self.sidebar.thread_order.iter().position(|o| o == id).unwrap_or(usize::MAX);
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
    pub(super) fn bulk_menu(&self, thread_id: &str) -> Option<BulkMenu> {
        if !self.sidebar.selection.contains(thread_id) {
            return None;
        }
        let ids = self.selected_threads();
        let now = now_ms();
        let requests = inbox::child_requests(&self.shell.threads);
        let threads: Vec<_> = ids.iter().filter_map(|id| self.shell.threads.iter().find(|t| &t.id == id)).collect();
        let loaded = self.server_info().environment_id.is_some();
        let pick = |keep: &dyn Fn(&crate::model::ThreadShell) -> bool| -> Vec<String> {
            threads.iter().filter(|t| keep(t)).map(|t| t.id.clone()).collect()
        };
        let settings = inbox::snooze_settings(self.server_info().settings.get("snoozePresets"));
        let presets = inbox::snooze_presets(&settings, &chrono::Local::now());
        Some(BulkMenu {
            pinned: pick(&|t| loaded && t.pinned_at.is_some()),
            settling: pick(&|t| t.settled_override.as_deref() != Some("settled")),
            snooze: (loaded && !threads.is_empty() && threads.iter().all(|t| Facts::new(t, &requests).can_snooze(now)))
                .then_some((settings, presets)),
            regeneratable: pick(&|t| loaded && t.title_regeneration.is_none()),
            supported: if loaded { threads.len() } else { 0 },
            ids,
        })
    }

    /// `settleThreads`: settles each, moving on past all of them from the open one.
    fn settle_threads(&mut self, ids: &[String], window: &mut Window, cx: &mut Context<Self>) {
        if self.client().is_none() {
            return;
        }
        for id in ids {
            let entry = self.undo_entry(id);
            self.thread_command("thread.settle", id, json!({}), cx);
            self.push_undo(UndoAction::Settled, entry, cx);
        }
        self.clear_selection(cx);
        if let Some(Target::Thread { id: open, .. }) = self.target.clone()
            && ids.contains(&open)
        {
            self.leave_threads(&open, ids, window, cx);
        }
    }

    /// Snoozes each thread until `wake_ms`, moving on past all of them from the open one.
    pub(super) fn snooze_threads(&mut self, ids: &[String], wake_ms: i64, window: &mut Window, cx: &mut Context<Self>) {
        if wake_ms <= now_ms() {
            self.error = Some("That snooze time has passed. Choose another time.".into());
            cx.notify();
            return;
        }
        if self.client().is_none() {
            return;
        }
        self.clear_selection(cx);
        let until = inbox::iso(wake_ms);
        let mut commands = Vec::new();
        for id in ids {
            let entry = self.undo_entry(id);
            commands.push(Self::thread_command_value("thread.snooze", id, json!({"snoozedUntil": until})));
            self.push_undo(UndoAction::Snoozed, entry, cx);
        }
        self.dispatch_batch(
            commands,
            false,
            |failed, succeeded| match succeeded {
                0 => "Failed to snooze threads".into(),
                _ => format!("Failed to snooze {failed} thread{}", plural(failed)),
            },
            cx,
        );
        if let Some(Target::Thread { id: open, .. }) = self.target.clone()
            && ids.contains(&open)
        {
            self.leave_threads(&open, ids, window, cx);
        }
    }

    /// Deletes the threads, asked first unless `confirmThreadDelete` is off.
    fn delete_threads(&mut self, ids: &[String], window: &mut Window, cx: &mut Context<Self>) {
        let ids = ids.to_vec();
        let count = ids.len();
        let run = move |this: &mut Self, cx: &mut Context<Self>| {
            let commands = ids.iter().map(|id| Self::thread_command_value("thread.delete", id, json!({}))).collect();
            this.dispatch_batch(commands, true, |_, _| "Failed to delete threads".into(), cx);
            this.sidebar.selection.remove(&ids);
            this.notify_sidebar(cx);
        };
        if self.client_settings.values.get("confirmThreadDelete").as_bool() == Some(false) {
            run(self, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Critical,
            &format!("Delete {count} thread{}?", plural(count)),
            Some("This permanently clears conversation history for these threads."),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| run(this, cx));
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordered() -> Vec<String> {
        ["a", "b", "c", "d", "e"].map(String::from).to_vec()
    }

    fn keys(selection: &Selection) -> Vec<&str> {
        let mut keys: Vec<&str> = selection.keys.iter().map(String::as_str).collect();
        keys.sort();
        keys
    }

    #[test]
    fn a_plain_click_anchor_ranges_and_the_anchor_stays_put() {
        let mut selection = Selection::default();
        selection.set_anchor("b");
        selection.range_to("d", &ordered());
        assert_eq!(keys(&selection), ["b", "c", "d"]);
        selection.range_to("e", &ordered());
        assert_eq!(selection.anchor.as_deref(), Some("b"));
        assert_eq!(keys(&selection), ["b", "c", "d", "e"]);
    }

    #[test]
    fn a_range_without_a_usable_anchor_selects_the_row() {
        let mut selection = Selection::default();
        selection.set_anchor("gone");
        selection.range_to("c", &ordered());
        assert_eq!(keys(&selection), ["c"]);
        assert_eq!(selection.anchor.as_deref(), Some("c"));
    }

    #[test]
    fn a_range_keeps_rows_selected_outside_it() {
        let mut selection = Selection::default();
        selection.toggle("a");
        selection.toggle("c");
        selection.range_to("d", &ordered());
        assert_eq!(keys(&selection), ["a", "c", "d"]);
        selection.toggle("c");
        assert_eq!(keys(&selection), ["a", "d"]);
    }

    #[test]
    fn removing_the_anchor_drops_it() {
        let mut selection = Selection::default();
        selection.toggle("a");
        selection.toggle("b");
        selection.remove(&["b".into()]);
        assert_eq!(keys(&selection), ["a"]);
        assert_eq!(selection.anchor, None);
        assert!(selection.clear());
        assert!(!selection.clear());
    }
}
