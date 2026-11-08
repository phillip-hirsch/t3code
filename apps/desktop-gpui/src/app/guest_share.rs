//! Guest links (`GuestShareControls.tsx`, `guestShareAvailability.ts`): the "Share with guest"
//! dialog and the header's sharing status with its Stop sharing button.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogEntrance, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Window, div, px,
};
use serde_json::{Value, json};

use super::sidebar::now_ms;
use super::{Target, Workspace};
use crate::model::GuestShare;
use crate::theme;

/// The dialog's create request (`CreateState`).
enum Create {
    Idle,
    Creating,
    Created { url: String, input: Entity<InputState> },
    Failed(String),
}

/// What the header remembers between renders: the expiry it re-renders at, and the thread
/// whose share is being stopped.
#[derive(Default)]
pub(super) struct ShareHeader {
    tick: Cell<Option<i64>>,
    closing: Option<String>,
}

fn minute(ms: i64) -> i64 {
    ms.div_euclid(60_000)
}

/// `GuestShareStatus`: the share stays on the thread after it expires, until the owner clears it.
fn share_status(share: &GuestShare, now: i64) -> String {
    let expired = crate::timestamp::parse_ms(&share.expires_at).is_some_and(|at| minute(at) <= minute(now));
    match (&share.guest_name, expired) {
        (_, true) => "Guest link expired".to_owned(),
        (Some(name), false) => format!("{name} is a guest"),
        (None, false) => "Guest link active".to_owned(),
    }
}

impl Workspace {
    /// `canShareWithGuestAtom`: this client may create links there, the environment's Guest
    /// links beta is on, and no policy turns T3 Connect off.
    pub(super) fn can_share_with_guest(&self, environment_id: &str) -> bool {
        let Some(info) = self.env_info(environment_id) else { return false };
        self.env_grants(environment_id, "access:write")
            && info.settings.bool("guestSharingEnabled")
            && info.managed_policy.pointer("/limits/connect") != Some(&Value::Bool(false))
    }

    /// `GuestShareDialog`: creates a one-use link to the thread and shows it to copy.
    pub(super) fn open_guest_share(&mut self, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.thread_client(thread_id) else { return };
        let Some(environment) = self.shell.threads.iter().find(|t| t.id == thread_id).map(|t| t.environment_id.clone())
        else {
            return;
        };
        let open = matches!(&self.target, Some(Target::Thread { id, .. }) if id == thread_id);
        let shared = open && self.open_projection().is_some_and(|p| p.thread.guest_share.is_some());
        let note =
            if shared { "A new link ends the current one." } else { "The link works once and expires in 24 hours." };
        let (state, copied) = (Rc::new(RefCell::new(Create::Idle)), Rc::new(Cell::new(false)));
        let (view, thread_id) = (cx.weak_entity(), thread_id.to_owned());
        window.open_dialog(cx, move |dialog, _, cx| {
            let t = *theme::tokens(cx);
            let can_create = view.upgrade().is_some_and(|w| w.read(cx).can_share_with_guest(&environment));
            let creating = matches!(*state.borrow(), Create::Creating);
            let panel = match &*state.borrow() {
                Create::Created { url, input } => {
                    let (url, copied) = (url.clone(), copied.clone());
                    let label = if copied.get() { "Copied" } else { "Copy" };
                    let icon = if copied.get() { IconName::Check } else { IconName::Copy };
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(Input::new(input)))
                        .child(
                            Button::new("guest-share-copy")
                                .outline()
                                .small()
                                .icon(Icon::new(icon))
                                .label(label)
                                .on_click(move |_, window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                                    copied.set(true);
                                    window.refresh();
                                    let copied = copied.clone();
                                    window
                                        .spawn(cx, async move |cx| {
                                            cx.background_executor().timer(Duration::from_secs(2)).await;
                                            copied.set(false);
                                            let _ = cx.update(|window, _| window.refresh());
                                        })
                                        .detach();
                                }),
                        )
                        .into_any_element()
                }
                _ => div().text_sm().text_color(t.muted_foreground).child(note).into_any_element(),
            };
            let failure = match &*state.borrow() {
                Create::Failed(message) => Some(div().text_xs().text_color(t.error_foreground).child(message.clone())),
                _ => None,
            };
            let footer = if matches!(*state.borrow(), Create::Created { .. }) {
                DialogFooter::new().child(
                    Button::new("guest-share-done")
                        .primary()
                        .small()
                        .label("Done")
                        .on_click(|_, window, cx| window.close_dialog(cx)),
                )
            } else {
                let (state, client, thread_id) = (state.clone(), client.clone(), thread_id.clone());
                DialogFooter::new()
                    .child(
                        Button::new("guest-share-cancel")
                            .outline()
                            .small()
                            .label("Cancel")
                            .disabled(creating)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("guest-share-create")
                            .primary()
                            .small()
                            .label(if creating { "Creating..." } else { "Create link" })
                            .disabled(!can_create || creating)
                            .on_click(move |_, window, cx| {
                                *state.borrow_mut() = Create::Creating;
                                window.refresh();
                                let (state, client) = (state.clone(), client.clone());
                                let input = json!({"threadId": thread_id});
                                window
                                    .spawn(cx, async move |cx| {
                                        let result = client.request("guestShare.create", input).await;
                                        let _ = cx.update(|window, cx| {
                                            let url = result.as_ref().ok().and_then(|v| v["url"].as_str());
                                            *state.borrow_mut() = match (url, &result) {
                                                (Some(url), _) => Create::Created {
                                                    url: url.to_owned(),
                                                    input: cx.new(|cx| InputState::new(window, cx).default_value(url)),
                                                },
                                                (None, Err(error)) => Create::Failed(error.to_string()),
                                                (None, Ok(_)) => {
                                                    Create::Failed("Could not create a guest link.".into())
                                                }
                                            };
                                            window.refresh();
                                        });
                                    })
                                    .detach();
                            }),
                    )
            };
            dialog
                .entrance(DialogEntrance::None)
                .title(SharedString::from("Share with guest"))
                .width(px(512.))
                .overlay_closable(!creating)
                .keyboard(!creating)
                .close_button(!creating)
                .child(
                    v_flex()
                        .gap(px(12.))
                        .child(div().text_sm().text_color(t.muted_foreground).child(
                            "The guest sees the thread as you do, including command output and the agent's \
                             reasoning. Each message they send waits for you.",
                        ))
                        .child(panel)
                        .children(failure),
                )
                .footer(footer)
        });
    }

    /// `GuestShareIndicator`: the open thread's sharing status and Stop sharing.
    pub(super) fn render_guest_share_indicator(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Target::Thread { id, .. }) = &self.target else { return None };
        let share = self.open_projection()?.thread.guest_share.as_ref()?;
        let now = now_ms();
        // `useNowMinute`: "expired" shows from the minute the link expires.
        if let Some(at) = crate::timestamp::parse_ms(&share.expires_at).filter(|at| minute(*at) > minute(now))
            && self.share_header.tick.get() != Some(at)
        {
            self.share_header.tick.set(Some(at));
            let wait = u64::try_from(minute(at) * 60_000 - now).unwrap_or(0);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(wait)).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        let t = *theme::tokens(cx);
        let closing = self.share_header.closing.as_deref() == Some(id.as_str());
        let thread_id = id.clone();
        let (accent, foreground) = (t.accent, t.foreground);
        let ring = gpui_kit::BoxShadow::new(px(0.), px(0.), t.ring).spread_radius(px(2.)).inset();
        // `Button size="xs" variant="ghost-muted"`, `disabled` while the close is in flight.
        let stop = gpui_kit::base::Button::new("guest-share-stop")
            .occlude()
            .flex_none()
            .h(px(24.))
            .px(px(8.))
            .rounded(px(8.))
            .font_weight(FontWeight::MEDIUM)
            .disabled(closing)
            .focus_visible(move |s| s.shadow(vec![ring]))
            .child("Stop sharing")
            .when(closing, |el| el.opacity(0.64))
            .when(!closing, |el| el.cursor_pointer().hover(move |s| s.bg(accent).text_color(foreground)))
            .on_click(cx.listener(move |this, _, _, cx| this.stop_guest_share(&thread_id, cx)));
        Some(
            h_flex()
                .flex_none()
                .gap_1()
                .text_xs()
                .text_color(t.muted_foreground)
                .child(Icon::new(IconName::User).size(px(14.)))
                .child(share_status(share, now))
                .child(stop)
                .into_any_element(),
        )
    }

    /// `thread.guest-share.close`; the button stays disabled until the server answers.
    fn stop_guest_share(&mut self, thread_id: &str, cx: &mut Context<Self>) {
        let Some(client) = self.thread_client(thread_id) else { return };
        let command = json!({"type": "thread.guest-share.close", "commandId": super::new_id(), "threadId": thread_id});
        self.share_header.closing = Some(thread_id.to_owned());
        cx.spawn(async move |this, cx| {
            let result = client.request("orchestration.dispatchCommand", command).await;
            let _ = this.update(cx, |this, cx| {
                this.share_header.closing = None;
                if let Err(error) = result {
                    this.error = Some(format!("Failed to stop guest share: {error}").into());
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_names_the_guest_until_the_link_expires() {
        let share = |guest: Option<&str>| GuestShare {
            expires_at: "2026-01-01T10:30:40.000Z".into(),
            guest_name: guest.map(Into::into),
        };
        let at = |iso: &str| crate::timestamp::parse_ms(iso).unwrap();
        assert_eq!(share_status(&share(None), at("2026-01-01T10:29:59.000Z")), "Guest link active");
        assert_eq!(share_status(&share(Some("Ada")), at("2026-01-01T10:29:59.000Z")), "Ada is a guest");
        // Minutes compare, like the web's `expiresAt.slice(0, 16) <= nowMinute`.
        assert_eq!(share_status(&share(Some("Ada")), at("2026-01-01T10:30:00.000Z")), "Guest link expired");
    }
}
