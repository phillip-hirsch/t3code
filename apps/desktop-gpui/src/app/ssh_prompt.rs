//! `SshPasswordPromptDialog`: asks for the password or verification code an SSH connection
//! needs, one prompt at a time, and hands the answer to the waiting `ssh` (`crate::ssh`).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use futures::channel::oneshot;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Disableable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, FontWeight, IntoElement, ParentElement as _, Render, Styled as _, Subscription,
    Task, Window, div, px,
};

use super::Workspace;
use crate::ssh::PasswordPrompt;
use crate::theme;

const EXPIRED: &str = "This SSH verification prompt expired. Try connecting again.";

/// `formatRemainingSeconds`: `m:ss`.
fn remaining_label(remaining_ms: i64) -> String {
    let seconds = (remaining_ms.max(0) + 999) / 1_000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// `describeSshTarget`.
fn describe_target(destination: &str, username: Option<&str>) -> String {
    match username {
        Some(user) => format!("{user}@{destination}"),
        None => destination.to_owned(),
    }
}

/// The answer to send, taken once: the first of Continue, Cancel or closing the dialog wins.
type Reply = Rc<RefCell<Option<oneshot::Sender<Option<String>>>>>;

fn answer(reply: &Reply, password: Option<String>) {
    if let Some(sender) = reply.borrow_mut().take() {
        drop(sender.send(password));
    }
}

struct PromptView {
    input: Entity<InputState>,
    prompt: String,
    expires_at_ms: i64,
    reply: Reply,
    _tick: Task<()>,
    _subscription: Subscription,
}

impl PromptView {
    fn expired(&self) -> bool {
        crate::timestamp::now_ms() >= self.expires_at_ms
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.expired() {
            return cx.notify();
        }
        let password = self.input.read(cx).value().to_string();
        answer(&self.reply, Some(password));
        window.close_dialog(cx);
    }
}

impl Render for PromptView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let expired = self.expired();
        let remaining = self.expires_at_ms - crate::timestamp::now_ms();
        let note = if expired {
            div().text_color(t.error).child(EXPIRED)
        } else {
            div()
                .text_color(t.muted_foreground)
                .child("SSH keys may replace a password, but your server can still require a verification code.")
        };
        v_flex()
            .gap(px(12.))
            .child(
                v_flex()
                    .gap(px(8.))
                    .child(
                        h_flex()
                            .justify_between()
                            .gap(px(12.))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(self.prompt.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(if expired { t.error } else { t.muted_foreground })
                                    .when(expired, |el| el.font_weight(FontWeight::MEDIUM))
                                    .child(if expired { "Expired".to_owned() } else { remaining_label(remaining) }),
                            ),
                    )
                    .child(Input::new(&self.input).disabled(expired)),
            )
            .child(note.text_sm())
    }
}

/// Shows `prompt` in this window's dialog layer; resolves once it is answered or closed.
fn show(prompt: PasswordPrompt, window: &mut Window, cx: &mut Context<Workspace>) -> oneshot::Receiver<()> {
    let PasswordPrompt { destination, username, prompt, expires_at_ms, reply } = prompt;
    let target = describe_target(&destination, username.as_deref());
    let (done, finished) = oneshot::channel::<()>();
    let reply: Reply = Rc::new(RefCell::new(Some(reply)));
    let done = Rc::new(RefCell::new(Some(done)));
    let input = cx.new(|cx| InputState::new(window, cx).masked(true));
    let focus = input.clone();
    let view = cx.new(|cx| PromptView {
        _subscription: cx.subscribe_in(&input, window, |this: &mut PromptView, _, event: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.submit(window, cx);
            }
        }),
        _tick: cx.spawn(async move |this, cx| {
            // The countdown, once a second until the prompt expires or the dialog goes.
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let ticking = this.update(cx, |this, cx| {
                    cx.notify();
                    !this.expired()
                });
                if !ticking.unwrap_or(false) {
                    return;
                }
            }
        }),
        input,
        prompt,
        expires_at_ms,
        reply: reply.clone(),
    });
    window.open_dialog(cx, move |dialog, _, cx| {
        let t = theme::tokens(cx);
        let expired = view.read(cx).expired();
        let (cancel_reply, close_reply, done) = (reply.clone(), reply.clone(), done.clone());
        let submit_view = view.clone();
        dialog
            .title("SSH verification required")
            .w(px(448.))
            .close_button(false)
            .overlay_closable(false)
            .child(div().mb(px(16.)).text_sm().text_color(t.muted_foreground).child(format!(
                "Enter the password or verification code required by {target}. T3 Code passes your response to the local SSH process and keeps it in memory to reuse for this host. T3 Code does not write it to disk."
            )))
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .child(Button::new("ssh-prompt-cancel").label(if expired { "Dismiss" } else { "Cancel" }).outline().on_click(
                        move |_, window, cx| {
                            answer(&cancel_reply, None);
                            window.close_dialog(cx);
                        },
                    ))
                    .child(Button::new("ssh-prompt-continue").label("Continue").primary().disabled(expired).on_click(
                        move |_, window, cx| submit_view.update(cx, |view, cx| view.submit(window, cx)),
                    )),
            )
            .on_close(move |_, _, _| {
                // Escape or a close from elsewhere counts as Cancel.
                answer(&close_reply, None);
                if let Some(done) = done.borrow_mut().take() {
                    let _closed = done.send(());
                }
            })
    });
    // After the dialog takes focus for itself.
    focus.update(cx, |input, cx| input.focus(window, cx));
    finished
}

/// Takes the SSH prompts for this window, one at a time, for as long as it is open. A prompt
/// that arrives as the window closes goes back for another window.
pub(super) fn listen(window: &mut Window, cx: &mut Context<Workspace>) {
    let prompts = crate::ssh::password_prompts();
    cx.spawn_in(window, async move |this, cx| {
        while let Ok(prompt) = prompts.recv().await {
            let mut prompt = Some(prompt);
            let shown = this.update_in(cx, |_, window, cx| {
                let prompt = prompt.take().expect("a prompt to show");
                show(prompt, window, cx)
            });
            match shown {
                Ok(finished) => drop(finished.await),
                Err(_) => {
                    if let Some(prompt) = prompt {
                        crate::ssh::requeue(prompt);
                    }
                    return;
                }
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdowns_round_up_to_whole_seconds() {
        assert_eq!(remaining_label(180_000), "3:00");
        assert_eq!(remaining_label(61_001), "1:02");
        assert_eq!(remaining_label(999), "0:01");
        assert_eq!(remaining_label(-5), "0:00");
        assert_eq!(describe_target("box", Some("me")), "me@box");
        assert_eq!(describe_target("box", None), "box");
    }
}
