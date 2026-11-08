//! `ConfirmDialogHost`: the in-app dialog behind `api.dialogs.confirm`, instead of the
//! platform's alert.

use std::cell::RefCell;
use std::rc::Rc;

use futures::channel::oneshot;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogEntrance, DialogFooter};
use gpui_kit::component::{WindowExt as _, v_flex};
use gpui_kit::{App, FontWeight, ParentElement as _, SharedString, Styled as _, Window, div, px, relative, white};

use crate::theme;

/// `resolveConfirmDialogCopy`: the line that asks the question is the title, the rest its
/// description.
fn copy(message: &str) -> (String, Option<String>) {
    let message = message.trim();
    let lines: Vec<&str> = message.split('\n').collect();
    if let Some(at) = lines.iter().position(|line| line.trim().ends_with('?')) {
        let rest: Vec<&str> = lines.iter().enumerate().filter(|(i, _)| *i != at).map(|(_, line)| *line).collect();
        let description = rest.join("\n").trim().to_owned();
        return (lines[at].trim().to_owned(), (!description.is_empty()).then_some(description));
    }
    if let Some(at) = message.find('?') {
        let description = message[at + 1..].trim();
        return (message[..=at].trim().to_owned(), (!description.is_empty()).then(|| description.to_owned()));
    }
    let description = if message.is_empty() { "This action requires your confirmation." } else { message };
    ("Confirm action".to_owned(), Some(description.to_owned()))
}

/// Asks `message` in an alert dialog with Cancel and Confirm (red when `destructive`);
/// resolves to whether the user confirmed.
pub(in crate::app) fn confirm(
    message: &str,
    destructive: bool,
    window: &mut Window,
    cx: &mut App,
) -> oneshot::Receiver<bool> {
    let (title, description) = copy(message);
    let (title, description) = (SharedString::from(title), description.map(SharedString::from));
    let (sender, receiver) = oneshot::channel();
    let sender = Rc::new(RefCell::new(Some(sender)));
    // Closing any other way drops the sender, which reads as Cancel.
    let send = move |confirmed: bool| {
        if let Some(sender) = sender.borrow_mut().take() {
            let _ = sender.send(confirmed);
        }
    };
    // `AlertDialogPopup` focuses its first tabbable control, Cancel, and Enter presses
    // whichever button has focus, so a stray Enter never confirms a destructive action.
    let cancel_focus = cx.focus_handle();
    let ok_focus = cx.focus_handle();
    let focus = cancel_focus.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = theme::tokens(cx);
        let ok = send.clone();
        let action =
            Button::new("confirm-dialog-ok").label("Confirm").track_focus(&ok_focus).on_click(move |_, window, cx| {
                ok(true);
                window.close_dialog(cx);
            });
        let action = if destructive { action.danger() } else { action.primary() };
        let enter = send.clone();
        let (enter_ok, enter_cancel) = (ok_focus.clone(), cancel_focus.clone());
        // `AlertDialogPopup`: centred, `max-w-lg`, `dialog-glass` (`rounded-2xl`, a white/8% edge
        // in dark mode); the header and the footer bring their own padding.
        dialog
            .entrance(DialogEntrance::None)
            .centered()
            .w(px(512.))
            .p_0()
            .gap_0()
            .rounded(px(16.))
            .border_color(if t.dark { white().opacity(0.08) } else { t.foreground.opacity(0.1) })
            .close_button(false)
            .overlay_closable(false)
            .child(
                v_flex()
                    .gap(px(8.))
                    .p(px(24.))
                    .child(
                        div()
                            .text_xl()
                            .line_height(relative(1.375))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.foreground)
                            .child(title.clone()),
                    )
                    .children(
                        description.clone().map(|text| div().text_sm().text_color(t.muted_foreground).child(text)),
                    ),
            )
            .footer(
                // `AlertDialogFooter`: right-aligned buttons on a `bg-muted/72` strip under a border.
                DialogFooter::new()
                    .px(px(24.))
                    .py(px(16.))
                    .border_t_1()
                    .border_color(t.border)
                    .bg(t.muted.opacity(0.72))
                    .rounded_b(px(15.))
                    .child(
                        Button::new("confirm-dialog-cancel")
                            .label("Cancel")
                            .outline()
                            .track_focus(&cancel_focus)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(action),
            )
            .on_ok(move |_, window, _| {
                if enter_ok.is_focused(window) {
                    enter(true);
                    return true;
                }
                enter_cancel.is_focused(window)
            })
    });
    focus.focus(window, cx);
    receiver
}

#[cfg(test)]
mod tests {
    use super::copy;

    #[test]
    fn the_question_line_is_the_title() {
        let owned = |(title, description): (&str, Option<&str>)| (title.to_owned(), description.map(str::to_owned));
        assert_eq!(
            copy("Delete thread \"Fix\"?\nThis permanently clears conversation history for this thread."),
            owned(("Delete thread \"Fix\"?", Some("This permanently clears conversation history for this thread.")))
        );
        assert_eq!(copy("Archive thread \"Fix\"?"), owned(("Archive thread \"Fix\"?", None)));
        assert_eq!(copy("Remove it? It cannot come back."), owned(("Remove it?", Some("It cannot come back."))));
        assert_eq!(copy("Proceed now"), owned(("Confirm action", Some("Proceed now"))));
        assert_eq!(copy(""), owned(("Confirm action", Some("This action requires your confirmation."))));
    }
}
