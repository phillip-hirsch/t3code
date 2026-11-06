//! `ConfirmDialogHost`: the in-app dialog behind `api.dialogs.confirm`, instead of the
//! platform's alert.

use std::cell::RefCell;
use std::rc::Rc;

use futures::channel::oneshot;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogDescription, DialogFooter};
use gpui_kit::{App, ParentElement as _, SharedString, Window, px};

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
pub(super) fn confirm(message: &str, destructive: bool, window: &mut Window, cx: &mut App) -> oneshot::Receiver<bool> {
    let (title, description) = copy(message);
    let (title, description) = (SharedString::from(title), description.map(SharedString::from));
    let (sender, receiver) = oneshot::channel();
    let sender = Rc::new(RefCell::new(Some(sender)));
    let answer = move |confirmed: bool| {
        let sender = sender.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, _: &mut App| {
            if let Some(sender) = sender.borrow_mut().take() {
                let _ = sender.send(confirmed);
            }
            true
        }
    };
    let (on_ok, on_cancel) = (answer(true), answer(false));
    window.open_dialog(cx, move |dialog, _, _| {
        let action = Button::new("confirm-dialog-ok").label("Confirm");
        let action = if destructive { action.danger() } else { action.primary() };
        dialog
            .title(title.clone())
            .w(px(512.))
            .close_button(false)
            .children(description.clone().map(|text| DialogDescription::new().child(text)))
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().trigger(|button| button.outline().label("Cancel")))
                    .child(DialogAction::new().child(action)),
            )
            .on_ok(on_ok.clone())
            .on_cancel(on_cancel.clone())
    });
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
