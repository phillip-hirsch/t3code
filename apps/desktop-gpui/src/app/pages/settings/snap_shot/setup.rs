//! The SnapShots setup wizard (`SnapShotSetupDialog.tsx` and the setup half of
//! `SnapShotSettings.tsx`): Access, where macOS permissions are granted and tested or the portal
//! is checked, then Shortcut.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Div, FontWeight, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, hsla, img,
    prelude::FluentBuilder as _, px,
};
use serde_json::json;

use super::ShortcutParts;
use crate::app::Workspace;
use crate::app::snap_shot::{Recorder, clear_setup_resume, save_setup_resume};
use crate::desktop::snap_shot as platform;
use crate::snap_shot::Shortcut;
use crate::snap_shot::setup::{self as capture, Backend, MacPermission, Mode, Step};
use crate::theme::{self, Tokens};

const STEPS: [(Step, &str); 2] = [(Step::Access, "Access"), (Step::Shortcut, "Shortcut")];

/// `requestSnapShotPermissions`: Accessibility first when app text is on, then Screen Recording.
/// Blocks; run it off the UI thread.
pub(super) fn request_mac_permissions(include_accessibility: bool) {
    if include_accessibility {
        platform::request_mac_permission(MacPermission::Accessibility);
    }
    platform::request_mac_permission(MacPermission::ScreenRecording);
}

impl Workspace {
    /// `reportSetupError`: the wizard shows errors inline; without it they become a toast.
    fn report_snap_shot_error(&mut self, title: &str, message: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.snap_shot.setup.is_none() {
            window.push_notification(Notification::error(message.clone()).title(title.to_owned()), cx);
        }
        self.snap_shot.setup_error = Some(message);
        cx.notify();
    }

    /// `openSetup`: checks capture, turns it on when setup can skip Access, and opens the wizard.
    pub(in crate::app) fn open_snap_shot_setup(
        &mut self,
        requested: Option<Step>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.snap_shot_state().is_none() || self.snap_shot.busy {
            return;
        }
        self.snap_shot.recorder = Recorder::default();
        self.snap_shot.setup_error = None;
        self.snap_shot.busy = true;
        cx.notify();
        self.refresh_snap_shot(window, cx, move |this, window, cx| {
            this.snap_shot.busy = false;
            let was_enabled = this.client_settings.values.bool("snapShotEnabled");
            let requested = if this.snap_shot.state.mac_permissions.is_some() { Some(Step::Access) } else { requested };
            if !was_enabled && capture::initial_step(&this.snap_shot.state, requested) != Step::Access {
                this.update_settings(json!({ "snapShotEnabled": true }), window, cx);
            }
            let step = capture::initial_step(&this.snap_shot.state, requested);
            this.show_snap_shot_setup(step, was_enabled, window, cx);
        });
    }

    /// `enableForSetup`: tests capture on macOS, turns capture on, and moves on once access is ready.
    fn enable_snap_shots_for_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(setup) = &self.snap_shot.setup else { return };
        if self.snap_shot.busy {
            return;
        }
        let was_enabled = setup.was_enabled;
        let mac = self.snap_shot.state.mac_permissions.is_some();
        let text = self.client_settings.values.bool("snapShotIncludeAccessibility");
        self.snap_shot.busy = true;
        self.snap_shot.setup_error = None;
        if mac {
            // macOS can quit the app from its permission prompt.
            save_setup_resume(was_enabled);
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let tested = cx
                .background_spawn(async move {
                    if mac {
                        platform::test_capture()?;
                        request_mac_permissions(text);
                    }
                    Ok::<(), String>(())
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(message) = tested {
                    this.snap_shot.busy = false;
                    return this.report_snap_shot_error("Couldn't verify capture access", message, window, cx);
                }
                let enabled = this.client_settings.values.bool("snapShotEnabled");
                if !enabled || this.snap_shot.state.message.is_some() {
                    this.update_settings(json!({ "snapShotEnabled": true }), window, cx);
                }
                this.refresh_snap_shot(window, cx, |this, _, cx| {
                    this.snap_shot.busy = false;
                    if capture::access_ready(&this.snap_shot.state) {
                        this.change_snap_shot_step(Step::Shortcut, cx);
                    }
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// `allow-screen-recording` and `allow-accessibility`.
    fn allow_snap_shot_permission(&mut self, permission: MacPermission, window: &mut Window, cx: &mut Context<Self>) {
        if self.snap_shot.busy {
            return;
        }
        if let Some(setup) = &self.snap_shot.setup {
            save_setup_resume(setup.was_enabled);
        }
        self.snap_shot.busy = true;
        self.snap_shot.setup_error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_spawn(async move { platform::request_mac_permission(permission) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.refresh_snap_shot(window, cx, |this, _, cx| {
                    this.snap_shot.busy = false;
                    cx.notify();
                });
            });
        })
        .detach();
    }

    /// `retry-shortcut`.
    pub(super) fn retry_snap_shot_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.snap_shot.busy {
            return;
        }
        self.snap_shot.setup_error = None;
        if let Err(message) = self.configure_snap_shot_shortcut(window, cx) {
            self.report_snap_shot_error("Couldn't open shortcut permissions", message.to_owned(), window, cx);
        }
        cx.notify();
    }

    /// `closeSetup`: leaving early turns capture back off when setup turned it on.
    pub(in crate::app) fn close_snap_shot_setup(
        &mut self,
        completed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(setup) = &self.snap_shot.setup else { return };
        if self.snap_shot.busy {
            return;
        }
        let was_enabled = setup.was_enabled;
        if self.client_settings.values.bool("snapShotEnabled") && !was_enabled && !completed {
            self.update_settings(json!({ "snapShotEnabled": false }), window, cx);
        }
        self.snap_shot.recorder = Recorder::default();
        self.snap_shot.setup_error = None;
        self.snap_shot.setup = None;
        clear_setup_resume();
        window.close_dialog(cx);
        cx.notify();
    }

    /// "Save and finish" saves the recorded shortcut first; "Done" just closes.
    fn finish_snap_shot_setup(
        &mut self,
        parts_changed: bool,
        can_save: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.snap_shot.busy {
            return;
        }
        if parts_changed {
            if !can_save {
                return;
            }
            self.save_snap_shot_shortcut(window, cx);
            let state = &self.snap_shot.state;
            if !(state.shortcut_registered || state.shortcut_pending) {
                return cx.notify();
            }
        }
        self.close_snap_shot_setup(true, window, cx);
    }

    fn change_snap_shot_step(&mut self, step: Step, cx: &mut Context<Self>) {
        self.snap_shot.recorder.recording = false;
        if let Some(setup) = &mut self.snap_shot.setup {
            setup.step = step;
        }
        cx.notify();
    }

    /// The wizard's content, rendered into its dialog.
    pub(in crate::app) fn render_snap_shot_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let Some(setup) = &self.snap_shot.setup else { return div().into_any_element() };
        let (step, was_enabled, advanced) = (setup.step, setup.was_enabled, setup.advanced);
        let state = self.snap_shot.state.clone();
        let text = self.client_settings.values.bool("snapShotIncludeAccessibility");
        let busy = self.snap_shot.busy;
        let error = self.snap_shot.setup_error.clone();
        let backend = capture::backend(&state);
        let access_ready = capture::access_ready(&state);
        let mac = state.mac_permissions;
        let mac_ready = capture::mac_permissions_ready(&state, text);
        let ShortcutParts { field, status, changed, can_save } = self.snap_shot_shortcut_parts(&t, cx);
        let shortcut_ready = capture::shortcut_ready(&state, changed);
        let (title, description) = match step {
            Step::Access => access_copy(&state, backend, mac_ready),
            Step::Shortcut => (
                "Choose your shortcut",
                if state.mode == Mode::Portal {
                    "Choose your keys, then approve the permission prompt if asked."
                } else {
                    "Use both Shift keys, or record a different shortcut."
                },
            ),
        };
        let mut details: Vec<String> = Vec::new();
        for detail in [error.clone(), state.message.clone().filter(|_| step == Step::Access)].into_iter().flatten() {
            if !details.contains(&detail) {
                details.push(detail);
            }
        }

        let header_title = match capture::desktop_name(&state) {
            Some(desktop) => format!("Set up snapshots for {desktop}"),
            None => "Set up snapshots".into(),
        };
        let close = Button::new("snap-shot-setup-close")
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::X).size(px(16.)))
            .tooltip("Close")
            .on_click(cx.listener(|this, _, window, cx| this.close_snap_shot_setup(false, window, cx)));
        let header = v_flex()
            .gap(px(12.))
            .p(px(24.))
            .pb(px(12.))
            .child(
                h_flex()
                    .justify_between()
                    .gap(px(8.))
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(header_title))
                    .when(!busy, |el| el.child(close)),
            )
            .child(self.render_snap_shot_steps(step, busy, &t, cx));

        let mut body = panel(&t).child(
            v_flex()
                .gap(px(8.))
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                .child(div().text_sm().text_color(t.muted_foreground).child(description)),
        );
        match step {
            Step::Access => {
                if let Some(permissions) = mac {
                    let accessibility = if text {
                        "Include text and controls from the captured app."
                    } else {
                        "Optional. Include text and controls from the captured app."
                    };
                    body = body.child(
                        v_flex()
                            .gap(px(8.))
                            .child(self.permission_row(
                                MacPermission::ScreenRecording,
                                "Screen Recording",
                                "Capture the window you're using.",
                                permissions.screen_recording,
                                busy,
                                &t,
                                cx,
                            ))
                            .child(self.permission_row(
                                MacPermission::Accessibility,
                                "Accessibility",
                                accessibility,
                                permissions.accessibility,
                                busy,
                                &t,
                                cx,
                            )),
                    );
                }
            }
            Step::Shortcut => {
                let saved = self.saved_snap_shot_shortcut();
                let retry = !changed
                    && !state.shortcut_registered
                    && !state.shortcut_pending
                    && state.shortcut_can_retry != Some(false)
                    && !matches!(saved, Shortcut::Pair(_));
                body = body.child(
                    v_flex()
                        .gap(px(12.))
                        .items_start()
                        .child(field)
                        .children(status.map(|status| div().text_xs().text_color(t.muted_foreground).child(status)))
                        .when(retry, |el| {
                            el.child(
                                Button::new("snap-shot-setup-retry")
                                    .ghost()
                                    .small()
                                    .label(if state.mode == Mode::Portal {
                                        "Shortcut permissions"
                                    } else {
                                        "Try again"
                                    })
                                    .disabled(busy)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.retry_snap_shot_shortcut(window, cx)),
                                    ),
                            )
                        }),
                );
                if !access_ready {
                    body = body.child(
                        div()
                            .text_sm()
                            .text_color(t.error_foreground)
                            .child("Capture needs attention. Go back to check access."),
                    );
                }
            }
        }
        if error.is_some() {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(t.error_foreground)
                    .child("Couldn't finish this step. Try again or check Advanced for help."),
            );
        }
        if !details.is_empty() {
            let chevron = if advanced { IconName::ChevronDown } else { IconName::ChevronRight };
            let toggle = h_flex()
                .id("snap-shot-setup-advanced")
                .gap(px(4.))
                .cursor_pointer()
                .child(Icon::new(chevron).size(px(12.)))
                .child("Advanced")
                .on_click(cx.listener(|this, _, _, cx| {
                    if let Some(setup) = &mut this.snap_shot.setup {
                        setup.advanced = !setup.advanced;
                    }
                    cx.notify();
                }));
            body = body.child(
                v_flex()
                    .gap(px(12.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .child(toggle)
                    .when(advanced, |el| el.children(details.into_iter().map(|detail| div().child(detail)))),
            );
        }

        let mut footer = h_flex().justify_end().gap(px(8.)).px(px(24.)).py(px(16.));
        if step != Step::Access {
            footer = footer.child(
                Button::new("snap-shot-setup-back")
                    .ghost()
                    .label("Back")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.change_snap_shot_step(Step::Access, cx))),
            );
        }
        footer = footer.child(
            Button::new("snap-shot-setup-later")
                .ghost()
                .label(if was_enabled { "Close" } else { "Finish later" })
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.close_snap_shot_setup(false, window, cx))),
        );
        let primary = match step {
            Step::Access => {
                let label = if busy {
                    "Working…"
                } else if mac.is_some() {
                    "Test capture and continue"
                } else if backend == Backend::Direct {
                    "Allow capture"
                } else if !access_ready {
                    "Try again"
                } else {
                    "Continue"
                };
                Some(
                    Button::new("snap-shot-setup-continue")
                        .primary()
                        .label(label)
                        .disabled(!mac_ready || busy)
                        .on_click(cx.listener(|this, _, window, cx| this.enable_snap_shots_for_setup(window, cx))),
                )
            }
            Step::Shortcut => {
                let label = if busy {
                    "Saving…"
                } else if changed {
                    "Save and finish"
                } else {
                    "Done"
                };
                let disabled = busy || !access_ready || if changed { !can_save } else { !shortcut_ready };
                Some(Button::new("snap-shot-setup-done").primary().label(label).disabled(disabled).on_click(
                    cx.listener(move |this, _, window, cx| this.finish_snap_shot_setup(changed, can_save, window, cx)),
                ))
            }
        };
        footer = footer.children(primary);

        // `WizardPopup` scrolls its whole content inside the window.
        let max_h = window.viewport_size().height - px(64.);
        v_flex()
            .id("snap-shot-setup")
            .max_h(max_h)
            .overflow_y_scroll()
            .child(header)
            .child(body)
            .child(footer)
            .into_any_element()
    }

    /// `WizardSteps`: a finished step can be revisited; a later one can't be skipped to.
    fn render_snap_shot_steps(
        &self,
        current: Step,
        busy: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        let current_index = STEPS.iter().position(|(step, _)| *step == current).unwrap_or_default();
        h_flex()
            .gap(px(4.))
            .p(px(4.))
            .rounded(px(12.))
            .bg(if t.dark { white(0.04) } else { t.background })
            .border_1()
            .border_color(if t.dark { white(0.05) } else { black(0.05) })
            .children(STEPS.iter().enumerate().map(|(index, (step, label))| {
                let (step, label) = (*step, *label);
                let is_current = index == current_index;
                let done = index < current_index;
                let badge = div()
                    .size(px(20.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .border_1()
                    .map(|badge| {
                        if done {
                            badge
                                .bg(t.primary)
                                .border_color(t.primary)
                                .child(Icon::new(IconName::Check).size(px(14.)).text_color(t.primary_foreground))
                        } else if is_current {
                            badge
                                .bg(t.primary.opacity(0.1))
                                .border_color(t.primary.opacity(0.3))
                                .text_color(t.primary)
                                .child((index + 1).to_string())
                        } else {
                            badge
                                .bg(if t.dark { white(0.05) } else { t.card })
                                .border_color(if t.dark { white(0.1) } else { black(0.1) })
                                .text_color(t.muted_foreground)
                                .child((index + 1).to_string())
                        }
                    });
                let enabled = !busy && index < current_index;
                h_flex()
                    .id(SharedString::from(format!("snap-shot-setup-step-{label}")))
                    .flex_1()
                    .min_w_0()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .when(is_current, |el| {
                        el.bg(t.card).border_1().border_color(if t.dark { white(0.05) } else { black(0.05) })
                    })
                    .when(enabled, |el| {
                        el.cursor_pointer()
                            .hover(|s| s.bg(t.card))
                            .on_click(cx.listener(move |this, _, _, cx| this.change_snap_shot_step(step, cx)))
                    })
                    .child(badge)
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if is_current { t.foreground } else { t.muted_foreground })
                            .child(label),
                    )
            }))
    }

    /// One row of `PermissionChecklist`.
    fn permission_row(
        &self,
        permission: MacPermission,
        title: &'static str,
        description: &'static str,
        granted: bool,
        busy: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        let (id, icon) = match permission {
            MacPermission::ScreenRecording => {
                ("snap-shot-allow-screen-recording", "icons/app/mac-screen-recording.svg")
            }
            MacPermission::Accessibility => ("snap-shot-allow-accessibility", "icons/app/mac-accessibility.svg"),
        };
        let status = if granted {
            h_flex()
                .gap(px(4.))
                .text_xs()
                .text_color(t.success_foreground)
                .child(Icon::new(IconName::CircleCheck).size(px(16.)))
                .child("Allowed")
                .into_any_element()
        } else {
            Button::new(id)
                .xsmall()
                .outline()
                .label("Allow")
                .disabled(busy)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.allow_snap_shot_permission(permission, window, cx)),
                )
                .into_any_element()
        };
        h_flex()
            .items_center()
            .gap(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .px(px(12.))
            .py(px(8.))
            .child(img(icon).size(px(32.)).flex_none())
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                    .child(div().text_xs().text_color(t.muted_foreground).child(description)),
            )
            .child(status)
    }
}

/// The Access step's title and description for the backends this client has.
fn access_copy(state: &capture::State, backend: Backend, mac_ready: bool) -> (&'static str, &'static str) {
    let mac = state.mac_permissions.is_some();
    if state.message.is_some() && !mac {
        return ("Let's try that again", "Couldn't check snapshots. Try again to continue.");
    }
    let description = match backend {
        Backend::Picker => {
            return (
                "Choose a window each time",
                "Your desktop doesn't support automatic capture. You'll choose the window to capture instead.",
            );
        }
        Backend::Portal => "Your desktop may ask for permission when you first capture.",
        Backend::Direct if mac && mac_ready => {
            "Test a snapshot of the current window. If macOS asks to bypass its window picker, choose Allow. The test image is discarded."
        }
        Backend::Direct if mac => "Allow each permission, then continue.",
        Backend::Direct => "Allow access when prompted to start capturing windows.",
    };
    ("Allow snapshots", description)
}

fn panel(t: &Tokens) -> Div {
    v_flex()
        .min_w_0()
        .gap(px(16.))
        .px(px(24.))
        .py(px(20.))
        .bg(if t.dark { white(0.02) } else { t.background.opacity(0.8) })
        .border_y_1()
        .border_color(if t.dark { white(0.05) } else { black(0.05) })
}

fn white(alpha: f32) -> gpui_kit::Hsla {
    hsla(0., 0., 1., alpha)
}

fn black(alpha: f32) -> gpui_kit::Hsla {
    hsla(0., 0., 0., alpha)
}
