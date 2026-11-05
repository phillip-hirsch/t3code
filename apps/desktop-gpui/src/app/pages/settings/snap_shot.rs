//! Settings → SnapShots (`SnapShotSettings.tsx`). Capture is Electron's
//! `DesktopSnapShot` (global shortcut, window capture, permission setup), which this
//! client doesn't have, so the section renders as the web does without a capture bridge:
//! the `SettingsUnavailableGroup` notice over the disabled toggle, with every other row hidden.

use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{AnyElement, Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};

use super::placeholder::{Control, SettingRowSpec, page, section};
use crate::app::Workspace;
use crate::theme;

static ENABLED: SettingRowSpec = SettingRowSpec {
    id: "snap-shot-enabled",
    label: "SnapShots",
    // `snapShotDescription` without a capture state.
    description: "Capture a window and attach it to your current draft.",
    path: "snapShotEnabled",
    control: Control::Switch,
    scopes: &[],
};

/// `snapShotUnavailableMessage` inside a desktop app that lacks the capture bridge.
const UNAVAILABLE: &str = "Update the desktop app to use snapshots.";

impl Workspace {
    pub(super) fn render_snap_shot_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
        let row = self.render_client_row(&ENABLED, SharedString::new_static(ENABLED.description), true, cx);
        let unavailable_group =
            v_flex().py(px(6.)).bg(t.muted.opacity(0.2)).child(notice).child(row).into_any_element();
        page("settings-snap-shot", [section(Some("SnapShots"), None, [unavailable_group], &t).into_any_element()])
            .into_any_element()
    }
}
