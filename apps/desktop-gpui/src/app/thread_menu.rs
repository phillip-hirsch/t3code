//! The thread title's actions menu (`ChatHeader.tsx`, `useThreadActionMenu.ts`): the shared
//! thread menu (`sidebar::ThreadMenu`), anchored under the title. Electron shows this menu
//! natively; here it is a popup.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::menu::PopupMenu;
use gpui_kit::{
    AnyElement, Bounds, Context, DismissEvent, Entity, Focusable as _, IntoElement, ParentElement as _, Pixels,
    Subscription, Task, Window, anchored, deferred, point, px,
};

use super::sidebar::{MenuOrigin, Renaming};
use super::{Target, Workspace};

/// Electron delays the menu through the double-click interval, so a double click renames instead.
const TITLE_MENU_DELAY: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(super) struct TitleState {
    menu: Option<(Entity<PopupMenu>, Subscription)>,
    pending: Option<Task<()>>,
    pub rename: Option<Renaming>,
    /// The title's bounds from the last paint; the menu opens under its left edge.
    pub bounds: Rc<Cell<Bounds<Pixels>>>,
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
    fn title_thread_id(&self) -> Option<String> {
        let Some(Target::Thread { id, .. }) = &self.target else { return None };
        Some(id.clone())
    }

    /// A click on the title: the menu after the double-click interval; the chevron opens it at once.
    pub(super) fn click_title(&mut self, click_count: usize, now: bool, window: &mut Window, cx: &mut Context<Self>) {
        if click_count >= 2 {
            self.title.pending = None;
            if let Some(id) = self.title_thread_id() {
                self.start_rename(&id, MenuOrigin::Title, window, cx);
            }
            return;
        }
        if now {
            self.open_thread_menu(window, cx);
            return;
        }
        self.title.pending = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(TITLE_MENU_DELAY).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.title.pending = None;
                this.open_thread_menu(window, cx);
            });
        }));
    }

    fn open_thread_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.title_thread_id().and_then(|id| self.thread_menu(&id, MenuOrigin::Title, cx)) else {
            return;
        };
        let view = cx.weak_entity();
        let menu =
            PopupMenu::build(window, cx, move |menu, window, cx| model.build(&view, menu.min_w(px(220.)), window, cx));
        menu.focus_handle(cx).focus(window, cx);
        let subscription = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, _, cx| {
            this.title.menu = None;
            cx.notify();
        });
        self.title.menu = Some((menu, subscription));
        cx.notify();
    }

    /// The open title menu, under the title's left edge.
    pub(super) fn render_title_menu(&self) -> Option<AnyElement> {
        let (menu, _) = self.title.menu.as_ref()?;
        let bounds = self.title.bounds.get();
        let corner = point(bounds.left(), bounds.bottom() + px(4.));
        Some(deferred(anchored().position(corner).child(menu.clone())).with_priority(2).into_any_element())
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
