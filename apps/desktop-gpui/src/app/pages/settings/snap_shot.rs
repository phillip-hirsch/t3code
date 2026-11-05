//! Settings → SnapShots (`SnapShotSettings.tsx`) for direct capture on Windows. Its setup
//! wizard never opens here: Windows needs no permission or helper, so `snapShotSetupComplete`
//! holds and the switch turns capture on directly. Builds without capture show the web's
//! `SettingsUnavailableGroup` notice over the disabled toggle.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _, SharedString,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use super::keybindings::command_label;
use super::widgets::{SettingRow, select_trigger, settings_section};
use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;
use crate::desktop::snap_shot as platform;
use crate::snap_shot::{self, Shortcut, Sound};
use crate::theme::{self, Tokens};

/// `snapShotUnavailableMessage` inside a desktop app that lacks the capture bridge.
const UNAVAILABLE: &str = "Update the desktop app to use snapshots.";

impl Workspace {
    pub(super) fn render_snap_shot_section(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        if !platform::available() {
            return self.render_snap_shot_unavailable(view, window, cx);
        }
        let enabled = view.bool("snapShotEnabled");
        let description = "Capture a window and attach it to your current draft.";
        let status = snap_shot::status(&self.snap_shot.state, enabled);
        let row = SettingRow::new("snap-shot-enabled").description(description).status(Some(status));
        let mut rows = vec![self.setting_row(&view, row.setting("snapShotEnabled").switch(), window, cx)];
        if enabled {
            let accessibility = SettingRow::new("snap-shot-accessibility")
                .description("Include text and controls when the app makes them available.")
                .setting("snapShotIncludeAccessibility")
                .switch();
            rows.push(self.setting_row(&view, accessibility, window, cx));
            let shortcut = self.snap_shot_shortcut_row(&t, cx);
            rows.push(self.setting_row(&view, shortcut, window, cx));
            let sound = SettingRow::new("snap-shot-sound")
                .description("Choose the sound played when capture starts.")
                .control(self.snap_shot_sound_menu(&t, cx));
            rows.push(self.setting_row(&view, sound, window, cx));
            let flash = SettingRow::new("snap-shot-flash")
                .description("Show a gentle cue on the captured window.")
                .setting("snapShotFlash")
                .switch();
            rows.push(self.setting_row(&view, flash, window, cx));
            let animations = SettingRow::new("snap-shot-animations")
                .description("Animate captured windows into your draft.")
                .setting("snapShotAnimations")
                .switch();
            rows.push(self.setting_row(&view, animations, window, cx));
        }
        let sections = [settings_section("snap-shots", "SnapShots", rows, &t)];
        self.settings_page("snap-shot", &view, sections, cx)
    }

    fn render_snap_shot_unavailable(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme::tokens(cx);
        let notice = h_flex()
            .items_start()
            .gap(px(8.))
            .px(px(16.))
            .py(px(8.))
            .text_xs()
            .line_height(px(19.5))
            .text_color(t.muted_foreground)
            .child(div().mt(px(2.)).child(Icon::new(IconName::Info).size(px(14.)).text_color(t.warning)))
            .child(UNAVAILABLE);
        // `snapShotDescription` without a capture state.
        let row = SettingRow::new("snap-shot-enabled")
            .description("Capture a window and attach it to your current draft.")
            .setting("snapShotEnabled")
            .switch()
            .unavailable();
        let row = self.setting_row(&view, row, window, cx);
        let group = v_flex().py(px(6.)).bg(t.muted.opacity(0.2)).child(notice).child(row).into_any_element();
        let sections = [settings_section("snap-shots", "SnapShots", [group], &t)];
        self.settings_page("snap-shot", &view, sections, cx)
    }

    /// The shortcut field, and Save and Cancel while it holds an unsaved shortcut.
    fn snap_shot_shortcut_row(&self, t: &Tokens, cx: &mut Context<Self>) -> SettingRow {
        let recorder = &self.snap_shot.recorder;
        let saved = Shortcut::from_value(self.client_settings.values.get("snapShotShortcut"));
        let candidate = recorder.candidate.clone().filter(|candidate| !candidate.same(&saved));
        let conflict = candidate.as_ref().and_then(|candidate| self.snap_shot_keybinding_conflict(candidate));
        let check = recorder.check.as_ref().filter(|(checked, ..)| candidate.as_ref().is_some_and(|c| c.same(checked)));
        let can_save = candidate.is_some() && conflict.is_none() && check.is_some_and(|(_, available, _)| *available);
        let status = if recorder.recording {
            Some("Press your shortcut. Esc cancels.".to_owned())
        } else if let Some(command) = &conflict {
            Some(format!("T3 Code already uses this for \"{}\".", command_label(command)))
        } else if let Some((_, available, message)) = check {
            if *available { Some("Ready to save.".to_owned()) } else { message.clone() }
        } else {
            snap_shot::shortcut_status(&self.snap_shot.state)
        };
        let shown = candidate.clone().unwrap_or(saved);
        let field = Button::new("snap-shot-shortcut-record")
            .xsmall()
            .map(|button| if recorder.recording { button.secondary() } else { button.outline() })
            .map(|button| {
                if recorder.recording {
                    button.label("Press shortcut…")
                } else {
                    button
                        .child(h_flex().gap(px(4.)).children(shown.key_labels().into_iter().map(|key| key_cap(key, t))))
                }
            })
            .tooltip(SharedString::from(format!("Record snapshot shortcut, currently {}", shown.label())))
            .on_click(cx.listener(|this, _, _, cx| this.start_snap_shot_recording(cx)));
        let field = div().id("snap-shot-shortcut-field").child(field).when(recorder.recording, |el| {
            el.on_mouse_down_out(cx.listener(|this, _, _, cx| this.stop_snap_shot_recording(cx)))
        });
        let controls = h_flex().gap(px(8.)).child(field).when(candidate.is_some(), |el| {
            el.child(
                Button::new("snap-shot-shortcut-save")
                    .xsmall()
                    .primary()
                    .label("Save")
                    .disabled(!can_save)
                    .on_click(cx.listener(|this, _, window, cx| this.save_snap_shot_shortcut(window, cx))),
            )
            .child(
                Button::new("snap-shot-shortcut-cancel")
                    .xsmall()
                    .ghost()
                    .label("Cancel")
                    .on_click(cx.listener(|this, _, _, cx| this.cancel_snap_shot_shortcut(cx))),
            )
        });
        SettingRow::new("snap-shot-shortcut")
            .description("Capture the window you're using without switching apps.")
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
