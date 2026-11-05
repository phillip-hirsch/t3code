//! Thread actions the sidebar rows offer: settle, snooze, pin, rename, archive,
//! delete, mark unread, and their undo (`apps/web/src/hooks/useThreadActions.ts`,
//! `components/threadActionMenu.logic.ts`, `SidebarThreadUndoNotice.tsx`).

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    Point, PromptLevel, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};
use serde_json::{Value, json};

use super::{Shelf, now_ms};
use crate::app::{Target, Workspace, new_id, project_name};
use crate::inbox::{self, Facts};

/// The undoable actions (`showThreadUndoNotice`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum UndoAction {
    Settled,
    Snoozed,
    Unpinned,
    Archived,
}

impl UndoAction {
    pub(super) fn label(self) -> &'static str {
        match self {
            UndoAction::Settled => "Settled",
            UndoAction::Snoozed => "Snoozed",
            UndoAction::Unpinned => "Unpinned",
            UndoAction::Archived => "Archived",
        }
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
    _expire: Task<()>,
}

impl UndoNotice {
    pub(super) fn count(&self) -> usize {
        self.entries.len()
    }
}

/// The title editor of the row being renamed.
pub(super) struct Renaming {
    pub(super) thread_id: String,
    pub(super) input: Entity<InputState>,
    original: String,
    _subscription: Subscription,
}

const UNDO_TIMEOUT: Duration = Duration::from_secs(5);

impl Workspace {
    fn thread(&self, thread_id: &str) -> Option<&crate::model::ThreadShell> {
        self.shell.threads.iter().find(|t| t.id == thread_id)
    }

    /// Dispatches `{type, commandId, threadId, ..fields}`.
    pub(in crate::app) fn thread_command(
        &mut self,
        kind: &str,
        thread_id: &str,
        fields: Value,
        cx: &mut Context<Self>,
    ) {
        let mut command = json!({"type": kind, "commandId": new_id(), "threadId": thread_id});
        if let (Some(command), Value::Object(fields)) = (command.as_object_mut(), fields) {
            command.extend(fields);
        }
        self.dispatch(command, cx);
    }

    fn undo_entry(&self, thread_id: &str) -> UndoEntry {
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
    fn push_undo(&mut self, action: UndoAction, entry: UndoEntry, cx: &mut Context<Self>) {
        let mut entries = match self.sidebar.undo.take() {
            Some(notice) if notice.action == action => notice.entries,
            _ => Vec::new(),
        };
        entries.retain(|e| e.thread_id != entry.thread_id);
        entries.push(entry);
        let expire = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_TIMEOUT).await;
            let _ = this.update(cx, |this, cx| {
                this.sidebar.undo = None;
                this.notify_sidebar(cx);
            });
        });
        self.sidebar.undo = Some(UndoNotice { action, entries, _expire: expire });
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
    pub(in crate::app) fn undo_thread_action(&mut self, cx: &mut Context<Self>) {
        let (Some(notice), Some(client)) = (self.sidebar.undo.take(), self.client()) else { return };
        self.notify_sidebar(cx);
        let now = now_ms();
        let action = notice.action;
        let mut plans: Vec<Vec<Value>> = Vec::new();
        for entry in notice.entries {
            let command = |kind: &str, fields: Value| {
                let mut command = json!({"type": kind, "commandId": new_id(), "threadId": entry.thread_id});
                if let (Some(command), Value::Object(fields)) = (command.as_object_mut(), fields) {
                    command.extend(fields);
                }
                command
            };
            let repin =
                || command("thread.pin", entry.pin_key.as_ref().map_or(json!({}), |key| json!({"orderKey": key})));
            let resnooze = entry
                .snoozed_until
                .as_ref()
                .filter(|until| inbox::parse_ms(until).is_some_and(|until| until > now))
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
            }
            plans.push(steps);
        }
        cx.spawn(async move |this, cx| {
            for steps in plans {
                for step in steps {
                    if let Err(error) = client.request("orchestration.dispatchCommand", step).await {
                        let _ = this.update(cx, |this, cx| {
                            this.error = Some(format!("Could not undo: {error}").into());
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
        if self.target.as_ref() != Some(&Target::Thread(thread_id.to_owned())) {
            return;
        }
        let order = &self.sidebar.inbox_order;
        let next = order
            .iter()
            .position(|id| id == thread_id)
            .and_then(|at| order[at + 1..].iter().chain(&order[..at]).find(|id| *id != thread_id).cloned());
        match next {
            Some(next) => self.select_thread(next, window, cx),
            None => {
                if let Some(project_id) = self.thread(thread_id).map(|t| t.project_id.clone()) {
                    self.start_draft(project_id, window, cx);
                }
            }
        }
    }

    pub(in crate::app) fn settle_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let entry = self.undo_entry(thread_id);
        self.thread_command("thread.settle", thread_id, json!({}), cx);
        self.push_undo(UndoAction::Settled, entry, cx);
        self.leave_thread(thread_id, window, cx);
    }

    pub(super) fn unsettle_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.expire_undo(thread_id);
        self.thread_command("thread.unsettle", thread_id, json!({"reason": "user"}), cx);
    }

    pub(super) fn snooze_thread(&mut self, thread_id: &str, wake_ms: i64, window: &mut Window, cx: &mut Context<Self>) {
        if wake_ms <= now_ms() {
            self.error = Some("That snooze time has passed. Choose another time.".into());
            cx.notify();
            return;
        }
        let entry = self.undo_entry(thread_id);
        self.thread_command("thread.snooze", thread_id, json!({"snoozedUntil": inbox::iso(wake_ms)}), cx);
        self.push_undo(UndoAction::Snoozed, entry, cx);
        self.leave_thread(thread_id, window, cx);
    }

    pub(super) fn wake_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.expire_undo(thread_id);
        self.thread_command("thread.unsnooze", thread_id, json!({"reason": "user"}), cx);
    }

    pub(in crate::app) fn pin_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        self.expire_undo(thread_id);
        let fields = inbox::top_pin_key(&self.shell.threads).map_or(json!({}), |key| json!({"orderKey": key}));
        self.thread_command("thread.pin", thread_id, fields, cx);
    }

    pub(in crate::app) fn unpin_thread(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let entry = self.undo_entry(thread_id);
        self.thread_command("thread.unpin", thread_id, json!({}), cx);
        self.push_undo(UndoAction::Unpinned, entry, cx);
    }

    fn archive_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let entry = self.undo_entry(thread_id);
        let project_id = self.thread(thread_id).map(|t| t.project_id.clone());
        self.thread_command("thread.archive", thread_id, json!({}), cx);
        self.push_undo(UndoAction::Archived, entry, cx);
        if self.target.as_ref() == Some(&Target::Thread(thread_id.to_owned())) {
            if let Some(project_id) = project_id {
                self.start_draft(project_id, window, cx);
            }
        }
    }

    /// `confirm('Delete thread "…"?')`, then `thread.delete`; there is no undo.
    fn delete_thread(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.thread(thread_id).map(|t| t.title.clone()).unwrap_or_default();
        let answer = window.prompt(
            PromptLevel::Critical,
            &format!("Delete thread \"{title}\"?"),
            Some("This permanently clears conversation history for this thread."),
            &["Delete", "Cancel"],
            cx,
        );
        let thread_id = thread_id.to_owned();
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    this.expire_undo(&thread_id);
                    this.thread_command("thread.delete", &thread_id, json!({}), cx);
                });
            }
        })
        .detach();
    }

    /// Starts editing a row's title in place (double-click, F2, or the menu).
    pub(super) fn start_rename(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(title) = self.thread(thread_id).map(|t| t.title.clone()) else { return };
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(title.clone(), window, cx);
            state
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(window, cx),
            _ => {}
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.sidebar.renaming =
            Some(Renaming { thread_id: thread_id.to_owned(), input, original: title, _subscription: subscription });
        self.notify_sidebar(cx);
    }

    pub(super) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.renaming.take().is_some() {
            self.notify_sidebar(cx);
        }
    }

    /// `commitThreadRename`: an empty title is refused, an unchanged one is a no-op.
    fn commit_rename(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.sidebar.renaming.take() else { return };
        let title = renaming.input.read(cx).value().trim().to_owned();
        if title.is_empty() {
            self.error = Some("Thread title cannot be empty".into());
            cx.notify();
        } else if title != renaming.original {
            self.thread_command("thread.metadata.update", &renaming.thread_id, json!({"title": title}), cx);
        }
        self.notify_sidebar(cx);
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
        let snooze = SnoozeItems::new(self, thread_id, false, cx);
        // Below the clock, right-aligned (`side="bottom" align="end"`).
        let position = gpui_kit::point(position.x + px(10.), position.y + px(14.));
        self.open_menu(position, gpui_kit::Anchor::TopRight, window, cx, move |menu, _, _| {
            snooze.add_to(&view, menu.min_w(px(160.)))
        });
    }

    /// What the row's right-click menu shows, read before the menu is built.
    pub(super) fn thread_menu(&self, thread_id: &str, cx: &gpui_kit::App) -> Option<ThreadMenu> {
        let thread = self.thread(thread_id)?;
        let now = now_ms();
        let requests = inbox::child_requests(&self.shell.threads);
        let facts = Facts::new(thread, &requests);
        let project = self.shell.projects.iter().find(|p| p.id == thread.project_id);
        let filter = project.filter(|p| p.archived_at.is_none()).map(|p| {
            let scoped = self.sidebar.scope == [p.id.clone()];
            let label = if scoped { "Show all projects".to_owned() } else { format!("Filter by {}", project_name(p)) };
            (label, if scoped { Vec::new() } else { vec![p.id.clone()] })
        });
        Some(ThreadMenu {
            thread_id: thread.id.clone(),
            pinned: thread.pinned_at.is_some(),
            settled: thread.settled_override.as_deref() == Some("settled"),
            snoozed: facts.is_snoozed(now),
            snooze: facts.can_snooze(now).then(|| SnoozeItems::new(self, thread_id, true, cx)),
            regenerating: thread.title_regeneration.is_some(),
            filter,
            path: thread.worktree_path.clone().or_else(|| project.map(|p| p.workspace_root.clone())),
            branch: thread.branch.clone(),
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
                        .hover(|s| s.underline())
                        .on_click(cx.listener(|this, _, _, cx| this.undo_thread_action(cx)))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("Undo (Ctrl+Z)").build(window, cx)
                        })
                        .map(|el| {
                            if fits {
                                el.child("Ctrl+Z to undo")
                            } else {
                                el.child(Icon::new(Lucide::Undo2).size(px(16.)))
                            }
                        }),
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
    /// The context submenu shows "Label (time)"; the card menu right-aligns the time.
    inline_when: bool,
    muted: gpui_kit::Hsla,
}

impl SnoozeItems {
    fn new(workspace: &Workspace, thread_id: &str, inline_when: bool, cx: &gpui_kit::App) -> Self {
        let settings = inbox::snooze_settings(&workspace.server_info.snooze_presets);
        let presets = inbox::snooze_presets(&settings, &chrono::Local::now());
        let muted = crate::theme::tokens(cx).sidebar.secondary_label;
        Self { thread_id: thread_id.to_owned(), settings, presets, inline_when, muted }
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
                let _ = view.update(cx, |workspace, cx| workspace.snooze_thread(&this.thread_id, wake, window, cx));
            }));
        }
        if !self.presets.is_empty() {
            menu = menu.separator();
        }
        let (view, thread_id) = (view.clone(), self.thread_id.clone());
        menu.item(PopupMenuItem::new("Custom…").on_click(move |_, window, cx| {
            let _ = view.update(cx, |workspace, cx| workspace.open_custom_snooze(&thread_id, window, cx));
        }))
    }
}

/// The right-click menu of one row (`buildThreadActionMenuItems`).
pub(super) struct ThreadMenu {
    thread_id: String,
    pinned: bool,
    settled: bool,
    snoozed: bool,
    /// `None` when the thread can't be snoozed now.
    snooze: Option<SnoozeItems>,
    regenerating: bool,
    /// The "Filter by …" label and the scope it sets.
    filter: Option<(String, Vec<String>)>,
    path: Option<String>,
    branch: Option<String>,
    running: bool,
}

impl ThreadMenu {
    pub(super) fn build(
        self,
        view: &WeakEntity<Workspace>,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let item = |label: &str, icon: Lucide| PopupMenuItem::new(label.to_owned()).icon(Icon::new(icon));
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
        menu = menu.item(if self.pinned {
            act!(item("Unpin thread", Lucide::PinOff), |this, _w, cx, id| this.unpin_thread(&id, cx))
        } else {
            act!(item("Pin thread", Lucide::Pin), |this, _w, cx, id| this.pin_thread(&id, cx))
        });
        menu = menu.item(if self.settled {
            act!(item("Un-settle thread", Lucide::CircleCheck), |this, _w, cx, id| this.unsettle_thread(&id, cx))
        } else {
            act!(item("Settle thread", Lucide::CircleCheck), |this, window, cx, id| this.settle_thread(&id, window, cx))
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
        menu = menu.separator();
        menu = menu.item(
            act!(item("Rename thread", Lucide::Pencil), |this, window, cx, id| this.start_rename(&id, window, cx))
        );
        menu = menu.item(if self.regenerating {
            item("Regenerating…", Lucide::RefreshCw).disabled(true)
        } else {
            act!(item("Regenerate title", Lucide::RefreshCw), |this, _w, cx, id| this.thread_command(
                "thread.metadata.update",
                &id,
                json!({"regenerateTitle": true}),
                cx
            ))
        });
        menu = menu.item(act!(item("Mark unread", Lucide::MailOpen), |this, _w, cx, id| this.thread_command(
            "thread.mark-unread",
            &id,
            json!({}),
            cx
        )));
        if let Some((label, scope)) = self.filter {
            let view = view.clone();
            menu = menu.item(item(&label, Lucide::FolderTree).on_click(move |_, _, cx| {
                let _ = view.update(cx, |this, cx| this.set_scope(scope.clone(), cx));
            }));
        }
        menu = menu.separator();
        let (path, branch, id) = (self.path.clone(), self.branch.clone(), self.thread_id.clone());
        menu = menu.submenu_with_icon(Some(Icon::new(Lucide::Copy)), "Copy", window, cx, move |mut menu, _, _| {
            let copy = |text: String| {
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
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
