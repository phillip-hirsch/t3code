//! SnapShots in the workspace: the capture shortcut follows the device settings
//! (`DesktopSnapShot.applySettings`), each press captures the foreground window
//! (`captureFromShortcut`), and the capture lands in the draft on screen when it was taken
//! (`SnapShotCoordinator.tsx`). Settings › SnapShots records shortcuts through [`Recorder`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, Image, ImageFormat, IntoElement as _, Keystroke, ModifiersChangedEvent, ObjectFit,
    ParentElement as _, SharedString, Styled as _, StyledImage as _, Task, Window, div, hsla, img, linear_color_stop,
    linear_gradient, px, rems,
};
use serde_json::Value;

use super::pages::settings::keybindings;
use super::{Target, Workspace, new_id};
use crate::desktop::snap_shot::{self as platform, Capture, Registration};
use crate::snap_shot::{self, Modifier, Shortcut, Sound};
use crate::theme::Tokens;

/// `SHORTCUT_COOLDOWN_NS`.
const COOLDOWN: Duration = Duration::from_millis(200);

pub(super) struct SnapShotUi {
    pub(super) state: snap_shot::State,
    /// Whether capture was on, and the shortcut, when the registration was last made.
    applied: Option<(bool, Shortcut)>,
    registration: Option<Registration>,
    trigger: async_channel::Sender<()>,
    presses: async_channel::Receiver<()>,
    _listener: Option<Task<()>>,
    last_press: Option<Instant>,
    capturing: bool,
    pub(super) recorder: Recorder,
}

impl Default for SnapShotUi {
    fn default() -> Self {
        let (trigger, presses) = async_channel::bounded(1);
        Self {
            state: Default::default(),
            applied: None,
            registration: None,
            trigger,
            presses,
            _listener: None,
            last_press: None,
            capturing: false,
            recorder: Recorder::default(),
        }
    }
}

/// Settings › SnapShots' shortcut field (`useSnapShotShortcutRecorder`) and its unsaved edit.
#[derive(Default)]
pub(super) struct Recorder {
    /// Keys go to the field, and the capture shortcut is suppressed, while this is set.
    pub(super) recording: bool,
    /// The recorded shortcut, while it differs from the saved one.
    pub(super) candidate: Option<Shortcut>,
    /// The availability of `candidate`: whether it can be saved, and why not.
    pub(super) check: Option<(Shortcut, bool, Option<String>)>,
}

impl Workspace {
    fn saved_snap_shot_shortcut(&self) -> Shortcut {
        Shortcut::from_value(self.client_settings.values.get("snapShotShortcut"))
    }

    /// Re-registers the shortcut when capture turns on or off or the shortcut changes; other
    /// saves leave the registration alone, as `applySettings` does.
    pub(super) fn sync_snap_shot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let enabled = self.client_settings.values.bool("snapShotEnabled") && platform::available();
        let shortcut = self.saved_snap_shot_shortcut();
        let ui = &mut self.snap_shot;
        if ui.applied.as_ref().is_some_and(|(on, applied)| *on == enabled && applied.same(&shortcut)) {
            return;
        }
        ui.applied = Some((enabled, shortcut.clone()));
        ui.registration = None;
        ui.state = snap_shot::State::default();
        if !enabled {
            cx.notify();
            return;
        }
        ui.registration = platform::register(&shortcut, ui.trigger.clone());
        ui.state.shortcut_registered = ui.registration.is_some();
        ui.state.shortcut_message = Some(match (&shortcut, ui.state.shortcut_registered) {
            (Shortcut::Pair(modifier), true) => snap_shot::observed_pair_message(*modifier),
            (_, true) => String::new(),
            (_, false) => snap_shot::registration_failure_message(&shortcut),
        })
        .filter(|message| !message.is_empty());
        if ui._listener.is_none() {
            let presses = ui.presses.clone();
            ui._listener = Some(cx.spawn_in(window, async move |this, cx| {
                while presses.recv().await.is_ok() {
                    if this.update_in(cx, |this, window, cx| this.on_snap_shot_press(window, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        cx.notify();
    }

    /// `captureFromShortcut`: a press during recording, a capture in flight, or the cooldown is dropped.
    fn on_snap_shot_press(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A recording left behind by closing Settings no longer holds the shortcut back.
        let recording = self.snap_shot.recorder.recording && self.settings.as_deref() == Some("snap-shot");
        let ui = &mut self.snap_shot;
        ui.recorder.recording = recording;
        if recording || ui.capturing {
            return;
        }
        ui.state.shortcut_verified = true;
        let now = Instant::now();
        if ui.last_press.is_some_and(|last| now.duration_since(last) < COOLDOWN) {
            return;
        }
        ui.last_press = Some(now);
        self.capture_snap_shot(window, cx);
    }

    fn capture_snap_shot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let values = &self.client_settings.values;
        if !values.bool("snapShotEnabled") {
            return;
        }
        // `showCaptureFeedback`: without the flight animation, the flash fades when animations are on.
        let animated = values.bool("snapShotAnimations") && !platform::reduced_motion();
        let flash = values.bool("snapShotFlash").then_some(animated);
        let text = values.bool("snapShotIncludeAccessibility");
        // `requested`: the capture belongs to the draft on screen now, wherever the user goes next.
        let pinned = self.target.clone();
        self.snap_shot.capturing = true;
        let (done, result) = futures::channel::oneshot::channel();
        let spawned = std::thread::Builder::new().name("snap-shot-capture".into()).spawn(move || {
            let _ = done.send(platform::capture(flash, text));
        });
        if spawned.is_err() {
            self.snap_shot.capturing = false;
            return self.snap_shot_failed(snap_shot::CAPTURE_FAILED.into(), window, cx);
        }
        cx.spawn_in(window, async move |this, cx| {
            let result = result.await.unwrap_or(Err(snap_shot::CAPTURE_FAILED));
            let _ = this.update_in(cx, |this, window, cx| {
                this.snap_shot.capturing = false;
                match result {
                    Ok(capture) => this.deliver_snap_shot(capture, pinned, window, cx),
                    Err(message) => this.snap_shot_failed(message.into(), window, cx),
                }
            });
        })
        .detach();
    }

    fn snap_shot_failed(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.snap_shot.state.message = Some(message.clone());
        window.push_notification(Notification::error(message).title("Snapshot failed"), cx);
        cx.notify();
    }

    /// `deliverSnapShot`: the capture joins the draft it was taken for, else the one on screen,
    /// else a new draft in the current project.
    fn deliver_snap_shot(
        &mut self,
        capture: Capture,
        pinned: Option<Target>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // No flight animation started, so the app comes forward (`desktopWindow.activate`).
        window.activate_window();
        self.snap_shot.state.message = None;
        let values = &self.client_settings.values;
        let sound = values.get("snapShotSound").as_str().unwrap_or_default();
        if let Some(bytes) = Sound::selected(values.bool("snapShotPlaySound"), sound).bytes() {
            platform::play_sound(bytes);
        }
        let id = new_id();
        let captured_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
        let accessibility = capture.accessible_text.as_deref().and_then(snap_shot::flat_accessibility);
        let icon = capture.icon_png.as_deref();
        let source = snap_shot::source(&captured_at, &capture.app_name, &capture.window_title, icon, accessibility);
        let name = snap_shot::capture_name(&captured_at, capture.extension());
        let source = SnapShotSource::new(source, capture.icon_png);
        let fail = |reason: &str, window: &mut Window, cx: &mut Context<Self>| {
            window
                .push_notification(Notification::error(format!("Capture {id}: {reason}")).title("Snapshot failed"), cx);
        };
        let pinned = pinned.filter(|pinned| Some(pinned) != self.target.as_ref() && self.target_exists(pinned));
        if let Some(pinned) = pinned {
            if !self.stash_snap_shot(pinned, name, capture.mime, capture.bytes, source, cx) {
                fail("Remove an attachment, then try this capture again.", window, cx);
            }
            return cx.notify();
        }
        if self.target.is_none() {
            let Some(project_id) = self.new_thread_project() else {
                let toast = Notification::error("Add a project, then capture the window again.")
                    .title("Snapshot taken, but no project is available");
                return window.push_notification(toast, cx);
            };
            self.start_draft(project_id, window, cx);
        }
        if !self.can_attach() {
            return fail("The captured window could not be saved to the draft.", window, cx);
        }
        if self.attachments.len() >= super::attachments::MAX_ATTACHMENTS {
            return fail("Remove an attachment, then try this capture again.", window, cx);
        }
        self.attach_snap_shot(name, capture.mime, capture.bytes, source, window, cx);
        // `dispatchSnapShotComposerFocus`.
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn target_exists(&self, target: &Target) -> bool {
        match target {
            Target::Thread(id) => self.shell.threads.iter().any(|thread| &thread.id == id),
            Target::Draft { project_id } => self.shell.projects.iter().any(|project| &project.id == project_id),
        }
    }

    /// Settings › SnapShots' field takes keys while it records; `true` consumes the key.
    pub(super) fn snap_shot_record_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        if !self.snap_shot.recorder.recording {
            return false;
        }
        if self.settings.as_deref() != Some("snap-shot") {
            self.snap_shot.recorder.recording = false;
            return false;
        }
        match keystroke.key.as_str() {
            "tab" => {
                self.stop_snap_shot_recording(cx);
                false
            }
            "escape" => {
                self.stop_snap_shot_recording(cx);
                true
            }
            _ => {
                let m = &keystroke.modifiers;
                if m.control || m.alt || m.shift || m.platform {
                    let input = keybindings::binding_from_keystroke(keystroke, cfg!(target_os = "macos"));
                    if let Some(shortcut) = input.as_deref().and_then(snap_shot::chord_from_input) {
                        self.record_snap_shot_shortcut(shortcut, cx);
                    }
                }
                true
            }
        }
    }

    /// Both keys of a modifier held while recording record a modifier pair.
    pub(super) fn snap_shot_record_modifiers(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        if !self.snap_shot.recorder.recording {
            return;
        }
        let m = &event.modifiers;
        let held = [
            (m.shift, Modifier::Shift),
            (m.control, Modifier::Control),
            (m.alt, Modifier::Alt),
            (m.platform, Modifier::Meta),
        ]
        .into_iter()
        .find(|(down, modifier)| *down && platform::pair_held(*modifier));
        if let Some((_, modifier)) = held {
            self.record_snap_shot_shortcut(Shortcut::Pair(modifier), cx);
        }
    }

    pub(super) fn start_snap_shot_recording(&mut self, cx: &mut Context<Self>) {
        let recorder = &mut self.snap_shot.recorder;
        recorder.recording = true;
        recorder.check = None;
        cx.notify();
    }

    pub(super) fn stop_snap_shot_recording(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.snap_shot.recorder.recording) {
            cx.notify();
        }
    }

    /// `checkShortcut`: a shortcut T3 Code binds needs no check; any other asks the system.
    fn record_snap_shot_shortcut(&mut self, shortcut: Shortcut, cx: &mut Context<Self>) {
        let saved = self.saved_snap_shot_shortcut();
        let recorder = &mut self.snap_shot.recorder;
        recorder.recording = false;
        recorder.check = None;
        if shortcut.same(&saved) {
            recorder.candidate = None;
            return cx.notify();
        }
        recorder.candidate = Some(shortcut.clone());
        if self.snap_shot_keybinding_conflict(&shortcut).is_none() {
            let (available, message) = match &shortcut {
                Shortcut::Pair(modifier) => (true, Some(snap_shot::observed_pair_message(*modifier))),
                Shortcut::Chord(chord) => match snap_shot::system_conflict(chord) {
                    Some(conflict) => (false, Some(conflict.to_owned())),
                    // Our own registration holds the saved chord, so only another chord is probed.
                    None if platform::check_chord(chord) => (true, None),
                    None => (false, Some(snap_shot::registration_failure_message(&shortcut))),
                },
            };
            self.snap_shot.recorder.check = Some((shortcut, available, message));
        }
        cx.notify();
    }

    /// The command already bound to `shortcut` (`snapShotKeybindingConflict`).
    pub(super) fn snap_shot_keybinding_conflict(&self, shortcut: &Shortcut) -> Option<String> {
        let Shortcut::Chord(chord) = shortcut else { return None };
        self.keymap
            .iter()
            .find(|rule| Shortcut::Chord(rule.shortcut.clone()).same(&Shortcut::Chord(chord.clone())))
            .map(|rule| rule.command.clone())
    }

    pub(super) fn cancel_snap_shot_shortcut(&mut self, cx: &mut Context<Self>) {
        self.snap_shot.recorder = Recorder::default();
        cx.notify();
    }

    pub(super) fn save_snap_shot_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(candidate) = self.snap_shot.recorder.candidate.take() else { return };
        self.snap_shot.recorder = Recorder::default();
        self.update_settings(serde_json::json!({ "snapShotShortcut": candidate.to_value() }), window, cx);
    }
}

/// A capture's `SnapShotSource`, with what its tile shows worked out once.
pub(super) struct SnapShotSource {
    pub(super) value: Value,
    icon: Option<Arc<Image>>,
    app_name: SharedString,
    window_title: SharedString,
    details: Option<SharedString>,
    includes_accessibility: bool,
}

impl SnapShotSource {
    fn new(value: Value, icon_png: Option<Vec<u8>>) -> Self {
        let text = |key: &str| SharedString::from(value[key].as_str().unwrap_or_default().to_owned());
        Self {
            icon: icon_png
                .filter(|_| value.get("appIconDataUrl").is_some())
                .map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png))),
            app_name: text("appName"),
            window_title: text("windowTitle"),
            details: snap_shot::accessibility_details(&value).map(Into::into),
            includes_accessibility: snap_shot::includes_accessibility(&value),
            value,
        }
    }

    /// `SnapShotAttachmentDetails`: the app and window along the tile's bottom edge.
    pub(super) fn render_details(&self, id: &str, t: &Tokens) -> AnyElement {
        let white = gpui_kit::white();
        let icon = match &self.icon {
            Some(icon) => img(icon.clone())
                .size(px(28.))
                .flex_none()
                .rounded(px(6.))
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            None => div()
                .size(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .bg(hsla(0., 0., 1., 0.2))
                .text_size(rems(0.625))
                .font_weight(FontWeight::MEDIUM)
                .text_color(white)
                .child(self.app_name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default())
                .into_any_element(),
        };
        let title = if self.window_title.is_empty() { "Captured window".into() } else { self.window_title.clone() };
        h_flex()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .min_w_0()
            .gap(px(6.))
            .px(px(10.))
            .pb(px(8.))
            .pt(px(24.))
            .bg(linear_gradient(
                0.,
                linear_color_stop(hsla(0., 0., 0., 0.85), 0.),
                linear_color_stop(hsla(0., 0., 0., 0.), 1.),
            ))
            .child(icon)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(px(6.))
                            .text_size(rems(0.6875))
                            .line_height(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(white)
                            .child(div().min_w_0().truncate().child(self.app_name.clone()))
                            .child(self.contents_button(id, t)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(rems(0.625))
                            .line_height(px(14.))
                            .text_color(hsla(0., 0., 1., 0.7))
                            .child(title),
                    ),
            )
            .into_any_element()
    }

    /// `SnapShotContentsButton`: the window's text, or why there is none.
    fn contents_button(&self, id: &str, t: &Tokens) -> AnyElement {
        let (icon, label) = if self.includes_accessibility {
            (IconName::TextAlignStart, "Accessibility data")
        } else {
            (IconName::Image, "No accessibility data")
        };
        let trigger = Button::new(SharedString::from(format!("snap-shot-contents-{id}")))
            .ghost()
            .xsmall()
            .size(px(18.))
            .p_0()
            .tooltip(label)
            .child(Icon::new(icon).size(px(12.)).text_color(gpui_kit::white()));
        let (details, includes, tokens) = (self.details.clone(), self.includes_accessibility, *t);
        Popover::new(SharedString::from(format!("snap-shot-contents-{id}-popover")))
            .anchor(gpui_kit::Anchor::BottomCenter)
            .trigger(trigger)
            .w(px(320.))
            .p(px(12.))
            .content(move |_, _, _| {
                let t = tokens;
                let frame = div()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.border.opacity(0.7))
                    .bg(t.muted.opacity(0.45))
                    .p(px(10.));
                let body = match (&details, includes) {
                    (Some(details), _) => frame
                        .max_h(px(256.))
                        .overflow_hidden()
                        .font_family(t.fonts.mono)
                        .text_size(rems(0.6875))
                        .line_height(px(16.))
                        .child(details.clone()),
                    (None, true) => frame.text_xs().text_color(t.muted_foreground).child(
                        "Structured accessibility elements were included, but they have no readable names or values.",
                    ),
                    (None, false) => frame
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child("The app or capture backend did not provide verified accessibility data."),
                };
                v_flex()
                    .gap(px(8.))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Accessibility data"))
                    .child(body)
            })
            .into_any_element()
    }
}
