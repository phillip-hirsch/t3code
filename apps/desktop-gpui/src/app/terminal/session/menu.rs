//! The terminal's right-click menu (`terminalContextMenuItems`) and the Add to chat / Copy
//! menu that follows a finished selection (`showSelectionAction`).

use std::time::Duration;

use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::{
    AnyElement, Context, DismissEvent, Entity, Focusable as _, IntoElement, ParentElement as _, Pixels, Point,
    Subscription, Window, anchored, deferred, px,
};

use super::{SessionEvent, TerminalSession};

/// `SELECTION_MULTI_CLICK_INTERVAL_MS`: a double click may still become a triple.
const MULTI_CLICK: Duration = Duration::from_millis(500);

pub(in crate::app::terminal) struct SelectionMenu {
    view: Entity<PopupMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

impl TerminalSession {
    /// Add to chat: the selection goes to the composer, then clears.
    pub fn add_selection_to_chat(&mut self, cx: &mut Context<Self>) {
        if let Some(selected) = self.selected() {
            self.term.selection = None;
            cx.emit(SessionEvent::AddToChat(selected));
            cx.notify();
        }
    }

    /// The right-click menu; a tracking app gets the click instead (see `mouse_down`).
    pub(super) fn context_menu(
        &self,
        cx: &mut Context<Self>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let session = cx.entity();
        move |menu, window, cx| {
            let has_selection = session.read(cx).selected().is_some();
            let on = |f: fn(&mut TerminalSession, &mut Window, &mut Context<TerminalSession>)| {
                window.listener_for(&session, move |this, _, window, cx| f(this, window, cx))
            };
            menu.min_w(px(160.))
                .item(
                    PopupMenuItem::new("Add to chat")
                        .disabled(!has_selection)
                        .on_click(on(|this, _, cx| this.add_selection_to_chat(cx))),
                )
                .item(PopupMenuItem::new("Copy").disabled(!has_selection).on_click(on(|this, _, cx| this.copy(cx))))
                .item(PopupMenuItem::new("Paste").on_click(on(|this, _, cx| this.paste(cx))))
                .item(PopupMenuItem::new("Select all").on_click(on(|this, _, cx| this.select_all(cx))))
                .item(PopupMenuItem::new("Jump to latest").on_click(on(|this, _, cx| this.scroll_to_bottom(cx))))
        }
    }

    /// Opens Add to chat / Copy at the pointer once a selection ends.
    pub(super) fn schedule_selection_menu(
        &mut self,
        position: Point<Pixels>,
        delay: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !delay {
            self.open_selection_menu(position, window, cx);
            return;
        }
        self.pointer.menu_timer = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(MULTI_CLICK).await;
            let _ = this.update_in(cx, |this, window, cx| this.open_selection_menu(position, window, cx));
        }));
    }

    fn open_selection_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.pointer.menu_timer = None;
        if self.selected().is_none() {
            return;
        }
        let session = cx.entity();
        let view = PopupMenu::build(window, cx, move |menu, window, _| {
            menu.min_w(px(140.))
                .item(
                    PopupMenuItem::new("Add to chat").on_click(
                        window.listener_for(&session, |this: &mut TerminalSession, _, _, cx| {
                            this.add_selection_to_chat(cx)
                        }),
                    ),
                )
                .item(
                    PopupMenuItem::new("Copy")
                        .on_click(window.listener_for(&session, |this: &mut TerminalSession, _, _, cx| this.copy(cx))),
                )
        });
        view.focus_handle(cx).focus(window, cx);
        let _dismiss = cx.subscribe_in(&view, window, |this, _, _: &DismissEvent, window, cx| {
            this.pointer.selection_menu = None;
            this.focus.focus(window, cx);
            cx.notify();
        });
        self.pointer.selection_menu = Some(SelectionMenu { view, position, _dismiss });
        cx.notify();
    }

    pub(super) fn render_selection_menu(&self) -> Option<AnyElement> {
        let menu = self.pointer.selection_menu.as_ref()?;
        Some(
            deferred(anchored().position(menu.position).snap_to_window_with_margin(px(8.)).child(menu.view.clone()))
                .with_priority(3)
                .into_any_element(),
        )
    }
}
