//! Settings → SnapShots (`SnapShotSettings.tsx`). Windows turns capture on directly; macOS and
//! Linux go through the setup wizard ([`setup`]) for permissions and the portal shortcut.

mod setup;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _,
    SharedString, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use serde_json::json;

use super::keybindings::command_label;
use super::widgets::{SettingRow, Switch, select_trigger, settings_section};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;
use crate::desktop::snap_shot as platform;
use crate::snap_shot::setup::{self as capture, Mode};
use crate::snap_shot::{Shortcut, Sound};
use crate::theme::{self, Tokens};

/// The shortcut field and what goes with it, shared by the settings row and the wizard.
pub(in crate::app) struct ShortcutParts {
    field: AnyElement,
    status: Option<String>,
    /// The field holds an unsaved shortcut (`shortcutChanged`).
    changed: bool,
    can_save: bool,
}

impl Workspace {
    pub(super) fn render_snap_shot_section(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        // A dialog closed some other way takes setup with it.
        if self.snap_shot.setup.is_some() && !window.has_active_dialog(cx) {
            self.snap_shot.setup = None;
        }
        let values = &self.client_settings.values;
        let (enabled, text) = (values.bool("snapShotEnabled"), values.bool("snapShotIncludeAccessibility"));
        let (flash, animations) = (values.bool("snapShotFlash"), values.bool("snapShotAnimations"));
        let state = self.snap_shot_state().cloned();
        let state = state.as_ref();
        let (busy, wizard) = (self.snap_shot.busy, self.snap_shot.setup.is_some());
        let available = state.is_some_and(|state| state.mode != Mode::Unavailable);
        let status =
            if busy && !wizard { "Updating capture settings…".to_owned() } else { capture::status(state, enabled) };
        let setup_button = (enabled && !capture::setup_complete(state, text)).then(|| {
            Button::new("snap-shot-setup")
                .xsmall()
                .outline()
                .label(capture::setup_button_label(state))
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| this.open_snap_shot_setup(None, window, cx)))
        });
        let workspace = cx.entity();
        let toggle = Switch::new("snap-shot-enabled-switch")
            .checked(enabled || wizard)
            .disabled(!available || busy)
            .accessibility_label("Enable snapshots")
            .on_change(move |checked, window, cx| {
                let checked = *checked;
                workspace.update(cx, |this, cx| this.toggle_snap_shots(checked, window, cx));
            });
        let row = SettingRow::new("snap-shot-enabled")
            .description(capture::description(state))
            .status(Some(status))
            .control(h_flex().gap(px(8.)).items_center().children(setup_button).child(toggle));
        let mut rows = vec![self.setting_row(view, row, window, cx)];
        if enabled && available {
            let unavailable = capture::accessibility_unavailable(state);
            let workspace = cx.entity();
            let accessibility = Switch::new("snap-shot-accessibility-switch")
                .checked(unavailable.is_none() && text)
                .disabled(unavailable.is_some())
                .accessibility_label("Include app text in snapshots")
                .on_change(move |checked, window, cx| {
                    let checked = *checked;
                    workspace.update(cx, |this, cx| this.set_snap_shot_accessibility(checked, window, cx));
                });
            let accessibility = SettingRow::new("snap-shot-accessibility")
                .description("Include text and controls when the app makes them available.")
                .status(unavailable)
                .control(accessibility);
            rows.push(self.setting_row(view, accessibility, window, cx));
            let shortcut = self.snap_shot_shortcut_row(&t, cx);
            rows.push(self.setting_row(view, shortcut, window, cx));
            let sound = SettingRow::new("snap-shot-sound")
                .description("Choose the sound played when capture starts.")
                .control(self.snap_shot_sound_menu(&t, cx));
            rows.push(self.setting_row(view, sound, window, cx));
            let effects = capture::feedback_unavailable(state);
            let effect =
                |id: &'static str, key: &'static str, on: bool, label: &'static str, cx: &mut Context<Self>| {
                    let workspace = cx.entity();
                    Switch::new(id)
                        .checked(effects.is_none() && on)
                        .disabled(effects.is_some())
                        .accessibility_label(label)
                        .on_change(move |checked, window, cx| {
                            let patch =
                                serde_json::Value::Object([(key.to_owned(), json!(*checked))].into_iter().collect());
                            workspace.update(cx, |this, cx| this.update_settings(patch, window, cx));
                        })
                };
            let flash = SettingRow::new("snap-shot-flash")
                .description("Show a gentle cue on the captured window.")
                .status(effects)
                .control(effect("snap-shot-flash-switch", "snapShotFlash", flash, "Flash captured window", cx));
            rows.push(self.setting_row(view, flash, window, cx));
            let animations = SettingRow::new("snap-shot-animations")
                .description("Animate captured windows into your draft.")
                .status(effects)
                .control(effect(
                    "snap-shot-animations-switch",
                    "snapShotAnimations",
                    animations,
                    "Animate snapshots",
                    cx,
                ));
            rows.push(self.setting_row(view, animations, window, cx));
        }
        let sections = [settings_section("snap-shots", "SnapShots", rows, &t)];
        self.settings_page("snap-shot", view, sections, cx)
    }

    /// The switch: off saves, on saves on Windows and sets up anywhere else.
    fn toggle_snap_shots(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !on || self.snap_shot.state.windows {
            self.update_settings(json!({ "snapShotEnabled": on }), window, cx);
        } else {
            self.open_snap_shot_setup(None, window, cx);
        }
    }

    /// `saveIncludeAccessibility`: macOS asks for Accessibility before app text turns on.
    fn set_snap_shot_accessibility(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let enabled = self.client_settings.values.bool("snapShotEnabled");
        if !(on && enabled && self.snap_shot.state.mac_permissions.is_some()) {
            return self.update_settings(json!({ "snapShotIncludeAccessibility": on }), window, cx);
        }
        cx.spawn_in(window, async move |this, cx| {
            cx.background_spawn(async { setup::request_mac_permissions(true) }).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.update_settings(json!({ "snapShotIncludeAccessibility": true }), window, cx);
                this.refresh_snap_shot(window, cx, |_, _, _| {});
            });
        })
        .detach();
    }

    /// The shortcut field with its status (`useSnapShotShortcutRecorder` and `shortcutStatus`).
    pub(in crate::app) fn snap_shot_shortcut_parts(&self, t: &Tokens, cx: &mut Context<Self>) -> ShortcutParts {
        let recorder = &self.snap_shot.recorder;
        let state = self.snap_shot_state();
        let portal = state.is_some_and(|state| state.mode == Mode::Portal);
        let saved = self.saved_snap_shot_shortcut();
        let candidate = recorder.candidate.clone().filter(|candidate| !candidate.same(&saved));
        let changed = candidate.is_some();
        let edited = candidate.clone().unwrap_or_else(|| saved.clone());
        let conflict = candidate.as_ref().and_then(|candidate| self.snap_shot_keybinding_conflict(candidate));
        let check = recorder.check.as_ref().filter(|(checked, ..)| checked.same(&edited));
        let can_save = changed && conflict.is_none() && check.is_some_and(|(_, available, _)| *available);
        // `displayShortcut`: an unchanged portal shortcut shows the desktop's own label.
        let label = state.and_then(|state| state.shortcut_label.as_deref()).filter(|_| !changed);
        let shown = match label {
            Some(label) => capture::chord_from_desktop_label(label).map(Shortcut::Chord),
            None => Some(edited),
        };
        let status = if recorder.recording {
            Some("Press your shortcut. Esc cancels.".to_owned())
        } else if let Some(command) = &conflict {
            Some(format!("T3 Code already uses this for \"{}\".", command_label(command)))
        } else if let Some((_, available, message)) = check {
            if *available { Some("Ready to save.".to_owned()) } else { message.clone() }
        } else if portal && label.is_none() && matches!(shown, Some(Shortcut::Pair(_))) {
            Some("Try a shortcut such as Ctrl+Shift+2.".to_owned())
        } else {
            capture::shortcut_status(state)
        };
        let tooltip = shown.as_ref().map_or_else(|| label.unwrap_or_default().to_owned(), Shortcut::label);
        let field = Button::new("snap-shot-shortcut-record")
            .xsmall()
            .disabled(self.snap_shot.busy)
            .map(|button| if recorder.recording { button.secondary() } else { button.outline() })
            .map(|button| match &shown {
                _ if recorder.recording => button.label("Press shortcut…"),
                None => button.label("Change shortcut"),
                Some(Shortcut::Pair(_)) if portal => button.label("Choose shortcut"),
                Some(shown) => button
                    .child(h_flex().gap(px(4.)).children(shown.key_labels().into_iter().map(|key| key_cap(key, t)))),
            })
            .tooltip(SharedString::from(format!("Record snapshot shortcut, currently {tooltip}")))
            .on_click(cx.listener(|this, _, _, cx| this.start_snap_shot_recording(cx)));
        let field = div()
            .id("snap-shot-shortcut-field")
            .child(field)
            .when(recorder.recording, |el| {
                el.on_mouse_down_out(cx.listener(|this, _, _, cx| this.stop_snap_shot_recording(cx)))
            })
            .into_any_element();
        ShortcutParts { field, status, changed, can_save }
    }

    /// The shortcut field, and Save and Cancel while it holds an unsaved shortcut, or the
    /// portal's shortcut permissions.
    fn snap_shot_shortcut_row(&self, t: &Tokens, cx: &mut Context<Self>) -> SettingRow {
        let ShortcutParts { field, status, changed, can_save } = self.snap_shot_shortcut_parts(t, cx);
        let state = self.snap_shot_state();
        let busy = self.snap_shot.busy;
        let retry = !changed
            && state.is_some_and(|state| state.mode == Mode::Portal && state.shortcut_can_retry != Some(false))
            && !matches!(self.saved_snap_shot_shortcut(), Shortcut::Pair(_));
        let pending = state.is_some_and(|state| state.shortcut_pending);
        let controls = h_flex()
            .gap(px(8.))
            .child(field)
            .when(changed, |el| {
                el.child(
                    Button::new("snap-shot-shortcut-save")
                        .xsmall()
                        .primary()
                        .label(if busy { "Saving…" } else { "Save" })
                        .disabled(!can_save || busy)
                        .on_click(cx.listener(|this, _, window, cx| this.save_snap_shot_shortcut(window, cx))),
                )
                .child(
                    Button::new("snap-shot-shortcut-cancel")
                        .xsmall()
                        .ghost()
                        .label("Cancel")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_snap_shot_shortcut(cx))),
                )
            })
            .when(retry, |el| {
                el.child(
                    Button::new("snap-shot-shortcut-permissions")
                        .xsmall()
                        .ghost()
                        .label("Shortcut permissions")
                        .disabled(busy || pending)
                        .on_click(cx.listener(|this, _, window, cx| this.retry_snap_shot_shortcut(window, cx))),
                )
            });
        let picker = state.is_some_and(|state| state.linux_backend == Some(capture::LinuxBackend::Picker));
        SettingRow::new("snap-shot-shortcut")
            .description(if picker {
                "Choose a window to capture from any app."
            } else {
                "Capture the window you're using without switching apps."
            })
            .status(status)
            .control(controls)
    }

    /// Off, Whoosh (the default) and Click, each sound with a button that plays it.
    fn snap_shot_sound_menu(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let values = &self.client_settings.values;
        let selected =
            Sound::selected(values.bool("snapShotPlaySound"), values.get("snapShotSound").as_str().unwrap_or_default());
        let muted = t.muted_foreground;
        let label = move |sound: Sound| {
            h_flex()
                .gap(px(4.))
                .child(sound.label())
                .when(sound == Sound::Whoosh, |el| el.child(div().text_color(muted).child("(Default)")))
        };
        let workspace = cx.entity();
        let tokens = *t;
        select_trigger(&"snap-shot-sound".into(), label(selected), 0., t)
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |mut menu, window, _| {
                for sound in [Sound::Off, Sound::Whoosh, Sound::Click] {
                    let pick = window.listener_for(&workspace, move |this, _, window, cx| {
                        this.update_settings(sound.patch(), window, cx);
                    });
                    let item = PopupMenuItem::element(move |_, _| {
                        h_flex()
                            .w_full()
                            .min_w(px(150.))
                            .justify_between()
                            .gap(px(12.))
                            .child(label(sound))
                            .when_some(sound.bytes(), |el, bytes| el.child(play_button(sound, bytes, &tokens)))
                    });
                    menu = menu.item(item.checked(sound == selected).on_click(pick));
                }
                menu
            })
            .into_any_element()
    }
}

/// The play button beside a sound; it plays without picking the sound.
fn play_button(sound: Sound, bytes: &'static [u8], t: &Tokens) -> impl gpui_kit::IntoElement {
    div()
        .id(SharedString::from(format!("snap-shot-play-{}", sound.label())))
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .hover(|el| el.bg(t.accent))
        .child(Icon::new(IconName::Play).size(px(12.)).text_color(t.muted_foreground))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            platform::play_sound(bytes);
        })
}

/// One key of the shortcut (`SnapShotShortcutKeys`' `Kbd`).
fn key_cap(key: String, t: &Tokens) -> impl gpui_kit::IntoElement {
    div()
        .h(px(18.))
        .min_w(px(18.))
        .px(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(t.foreground.opacity(0.07))
        .text_xs()
        .text_color(t.muted_foreground)
        .child(key)
}
