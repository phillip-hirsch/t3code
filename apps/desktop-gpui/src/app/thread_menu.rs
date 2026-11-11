//! The thread title's actions menu (`ChatHeader.tsx`, `useThreadActionMenu.ts`): the shared
//! thread menu (`sidebar::ThreadMenu`), anchored under the title. Electron shows this menu
//! natively; here it is a popup.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{
    AnyElement, App, Bounds, Context, DismissEvent, Entity, FocusHandle, Focusable as _, IntoElement,
    ParentElement as _, Pixels, Point, Subscription, Task, Window, anchored, deferred, point, px,
};

use super::sidebar::{MenuOrigin, Renaming};
use super::{Target, Workspace};

/// Electron delays the menu through the double-click interval, so a double click renames instead.
const TITLE_MENU_DELAY: Duration = Duration::from_millis(500);

pub(super) struct TitleState {
    /// The open menu, and the pointer it opened at (a right-click); else it hangs under the title.
    menu: Option<(Entity<PopupMenu>, Option<Point<Pixels>>, Subscription)>,
    pending: Option<Task<()>>,
    pub rename: Option<Renaming>,
    /// The title's bounds from the last paint; the menu opens under its left edge.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The breadcrumb's buttons (project, parent thread, title), tab stops like the web's.
    pub crumbs: [FocusHandle; 3],
    /// Focus came back to the title from a menu the pointer opened: Electron's native menu
    /// keeps its Escape from the page, so no `:focus-visible` ring until the next key there.
    pub quiet_ring: Cell<bool>,
}

impl TitleState {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            menu: None,
            pending: None,
            rename: None,
            bounds: Rc::default(),
            crumbs: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)),
            quiet_ring: Cell::new(false),
        }
    }
}

/// `hostedThreadLink`: every character outside `[A-Za-z0-9_-]` percent-encoded.
pub(super) fn hosted_thread_link(environment_id: &str, thread_id: &str) -> String {
    use std::fmt::Write as _;
    let segment = |id: &str| {
        let mut out = String::with_capacity(id.len());
        for byte in id.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => out.push(byte as char),
                _ => {
                    let _ = write!(out, "%{byte:02X}");
                }
            }
        }
        out
    };
    format!("https://app.t3.codes/threads/{}/{}", segment(environment_id), segment(thread_id))
}

impl Workspace {
    fn title_thread(&self) -> Option<crate::model::ScopedId> {
        self.target.as_ref().and_then(Target::thread_key)
    }

    /// A click on the title: the menu after the double-click interval; the chevron opens it at once.
    pub(super) fn click_title(&mut self, click_count: usize, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        if click_count >= 2 {
            self.title.pending = None;
            if let Some(id) = self.title_thread() {
                self.start_rename(&id, MenuOrigin::Title, window, cx);
            }
            return;
        }
        if now {
            self.open_thread_menu(None, window, cx);
            return;
        }
        self.title.pending = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(TITLE_MENU_DELAY).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.title.pending = None;
                this.open_thread_menu(None, window, cx);
            });
        }));
    }

    /// `handleHeaderContextMenu`: a right-click on the breadcrumb opens the thread's menu at the
    /// pointer, or "Project settings" for a draft in a project; nothing while renaming.
    pub(super) fn context_click_header(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.title.rename.is_some() {
            return;
        }
        match &self.target {
            Some(Target::Thread { .. }) => {
                self.title.pending = None;
                self.open_thread_menu(Some(position), window, cx);
            }
            Some(Target::Draft { .. }) if self.target_project().is_some() => {
                self.title.pending = None;
                let view = cx.weak_entity();
                let menu = PopupMenu::build(window, cx, move |menu, _, _| {
                    menu.min_w(px(200.)).item(
                        PopupMenuItem::new("Project settings").icon(Icon::new(IconName::Settings)).on_click(
                            move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| this.open_settings("projects", cx));
                            },
                        ),
                    )
                });
                self.show_title_menu(menu, Some(position), window, cx);
            }
            _ => {}
        }
    }

    fn open_thread_menu(&mut self, at: Option<Point<Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.title_thread().and_then(|id| self.thread_menu(&id, MenuOrigin::Title, cx)) else {
            return;
        };
        let view = cx.weak_entity();
        let menu =
            PopupMenu::build(window, cx, move |menu, window, cx| model.build(&view, menu.min_w(px(220.)), window, cx));
        self.show_title_menu(menu, at, window, cx);
    }

    /// Focuses `menu` until it dismisses, then hands focus back (to the title, after a keyboard open).
    fn show_title_menu(
        &mut self,
        menu: Entity<PopupMenu>,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let previous = window.focused(cx);
        let pointer = !window.last_input_was_keyboard();
        menu.focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&menu, window, move |this, menu, _: &DismissEvent, window, cx| {
            this.title.menu = None;
            // An item that moved focus (Rename's input) keeps it.
            if let Some(previous) = previous.as_ref().filter(|_| menu.focus_handle(cx).contains_focused(window, cx)) {
                previous.focus(window, cx);
                this.title.quiet_ring.set(pointer);
            }
            cx.notify();
        });
        self.title.menu = Some((menu, at, subscription));
        cx.notify();
    }

    /// The open title menu: at the pointer for a right-click, else under the title's left edge.
    pub(super) fn render_title_menu(&self) -> Option<AnyElement> {
        let (menu, at, _) = self.title.menu.as_ref()?;
        let bounds = self.title.bounds.get();
        let corner = at.unwrap_or_else(|| point(bounds.left(), bounds.bottom() + px(4.)));
        let menu = anchored().position(corner).snap_to_window_with_margin(px(8.)).child(menu.clone());
        Some(deferred(menu).with_priority(2).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_links_escape_everything_but_ids_safe_characters() {
        assert_eq!(hosted_thread_link("env-1", "thread-1"), "https://app.t3.codes/threads/env-1/thread-1");
        assert_eq!(
            hosted_thread_link("e", "thread:provider:codex.x"),
            "https://app.t3.codes/threads/e/thread%3Aprovider%3Acodex%2Ex"
        );
    }
}
