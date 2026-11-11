//! Where the chat lane sits beside the thread details card.
//! Mirrors `resolveChatCanvasLayout` (apps/web/src/components/chat/chatCanvasLayout.ts)
//! and the inline test in `threadDetailsCardLayout.ts`.

use gpui_kit::Window;

use super::Workspace;
use super::titlebar::RIGHT_RESIZE_INSET;

/// The canvas's `ps-3 sm:ps-12`: 12px beside the lane in a window under 640px, else 48px.
fn gutter(viewport_width: f32) -> f32 {
    if viewport_width < 640. { 12. } else { 48. }
}
/// `--chat-content-max-width` for each `chatWidth`: 46rem, 72rem, or the whole canvas.
fn max_lane(chat_width: &str) -> f32 {
    match chat_width {
        "wide" => 1152.,
        "full" => f32::INFINITY,
        _ => 736.,
    }
}
const MIN_LANE: f32 = 640.;
pub(super) const CARD_WIDTH: f32 = 280.;
pub(super) const CARD_INSET: f32 = 12.;
const CARD_CLEARANCE: f32 = 32.;

/// The chat lane in canvas coordinates: rows and the composer share it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Lane {
    pub left: f32,
    pub width: f32,
    /// Space right of the lane, so a full-width container can pad to it.
    pub right: f32,
}

pub(super) fn resolve_lane(canvas: f32, card_left: Option<f32>, max_lane: f32, gutter: f32) -> Lane {
    let centered = (canvas - gutter * 2.).clamp(0., max_lane);
    let lane_right = card_left.map_or(canvas - gutter, |left| left - CARD_CLEARANCE);
    let width = centered.min(lane_right - gutter).max(0.);
    let left = ((canvas - width) / 2.).min(lane_right - width).max(gutter);
    Lane { left, width, right: (canvas - left - width).max(0.) }
}

/// The card's left edge when it fits inline next to a lane of at least 640px.
/// Narrower canvases show it as a popover under the header instead.
pub(super) fn inline_card_left(canvas: f32, gutter: f32) -> Option<f32> {
    let left = canvas - CARD_WIDTH - CARD_INSET;
    (left - CARD_CLEARANCE - gutter >= MIN_LANE).then_some(left)
}

impl Workspace {
    fn canvas_width(&self, window: &Window) -> f32 {
        let sidebar = if self.sidebar_open.docked() { f32::from(self.sidebar_width()) } else { 0. };
        let right_panel = self.right_panel_docked_width(window).unwrap_or(0.);
        f32::from(window.viewport_size().width) - sidebar - right_panel - f32::from(RIGHT_RESIZE_INSET)
    }

    /// Whether the details card fits inline; otherwise it is a popover.
    pub(super) fn details_inline(&self, window: &Window) -> bool {
        inline_card_left(self.canvas_width(window), gutter(f32::from(window.viewport_size().width))).is_some()
    }

    /// The lane, and the details card's left edge when the card is shown inline.
    pub(super) fn chat_layout(&self, window: &Window) -> (Lane, Option<f32>) {
        let canvas = self.canvas_width(window);
        let gutter = gutter(f32::from(window.viewport_size().width));
        let card = self.details_visibility().inline_open.then(|| inline_card_left(canvas, gutter)).flatten();
        let chat_width = self.client_settings.values.get("chatWidth").as_str().unwrap_or_default();
        (resolve_lane(canvas, card, max_lane(chat_width), gutter), card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_centers_without_a_card_and_clears_the_card_when_shown() {
        let comfortable = max_lane("comfortable");
        assert_eq!(resolve_lane(1400., None, comfortable, 48.), Lane { left: 332., width: 736., right: 332. });
        assert_eq!(resolve_lane(1400., None, max_lane("wide"), 48.).width, 1152.);
        assert_eq!(resolve_lane(1400., None, max_lane("full"), 48.), Lane { left: 48., width: 1304., right: 48. });
        // 1038px canvas: the card starts at 746, so the lane ends 32px before it and hugs the gutter.
        let card = inline_card_left(1038., 48.);
        assert_eq!(card, Some(746.));
        assert_eq!(resolve_lane(1038., card, comfortable, 48.), Lane { left: 48., width: 666., right: 324. });
        // A wide canvas keeps the lane centered when it already clears the card.
        let card = inline_card_left(2000., 48.);
        assert_eq!(resolve_lane(2000., card, comfortable, 48.).left, 632.);
    }

    #[test]
    fn card_is_not_inline_when_the_lane_would_drop_below_640() {
        assert_eq!(inline_card_left(1000., 48.), None);
    }

    #[test]
    fn a_window_under_640_narrows_the_gutter_to_12() {
        assert_eq!(gutter(639.), 12.);
        assert_eq!(gutter(640.), 48.);
        // 626px of window, no sidebar: the lane fills all but 12px on each side.
        assert_eq!(
            resolve_lane(620., None, max_lane("comfortable"), gutter(626.)),
            Lane { left: 12., width: 596., right: 12. }
        );
    }
}
