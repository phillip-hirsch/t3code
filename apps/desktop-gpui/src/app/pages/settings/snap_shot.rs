//! Settings → SnapShots (`SnapShotSettings.tsx`). Capture is Electron's
//! `DesktopSnapShot` (global shortcut, window capture, permission setup), which this
//! client doesn't have, so the section renders as the web does without a capture bridge:
//! the `SettingsUnavailableGroup` notice over the disabled toggle, with every other row hidden.

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{AnyElement, Context, IntoElement as _, ParentElement as _, Styled as _, Window, div, px};

use super::widgets::{SettingRow, settings_section};
use crate::app::Workspace;
use crate::theme;

/// `snapShotUnavailableMessage` inside a desktop app that lacks the capture bridge.
const UNAVAILABLE: &str = "Update the desktop app to use snapshots.";

impl Workspace {
    pub(super) fn render_snap_shot_section(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme::tokens(cx);
        let view = self.settings_view();
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
}
