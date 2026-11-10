//! The scrollback scrollbar over a terminal's right edge (`terminalScrollbarGeometry` and
//! `onScrollbarKeyDown` in the web's Ghostty surface).

/// `MIN_SCROLLBAR_THUMB_HEIGHT`.
const MIN_THUMB_HEIGHT: f32 = 18.;

/// `GhosttyScrollbar`: every row, the rows shown, and the first row shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ScrollState {
    pub total: usize,
    pub len: usize,
    pub offset: usize,
}

impl ScrollState {
    pub fn max_offset(self) -> usize {
        self.total.saturating_sub(self.len.min(self.total))
    }

    /// `scrollViewport`: `delta` rows (positive toward the end), kept inside the scrollback.
    pub fn clamp_delta(self, delta: i64) -> i64 {
        let offset = i64::try_from(self.offset).unwrap_or(i64::MAX);
        let max = i64::try_from(self.max_offset()).unwrap_or(i64::MAX);
        (offset.saturating_add(delta)).clamp(0, max) - offset.min(max)
    }

    /// The rows a key on the focused scrollbar moves the view.
    pub fn key_delta(self, key: &str) -> Option<i64> {
        let count = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        Some(match key {
            "up" => -1,
            "down" => 1,
            "pageup" => -count(self.len.max(1)),
            "pagedown" => count(self.len.max(1)),
            "home" => -count(self.offset),
            "end" => count(self.total) - count(self.len) - count(self.offset),
            _ => return None,
        })
    }
}

/// The thumb's height and its top within the track, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Thumb {
    pub height: f32,
    pub top: f32,
}

/// `terminalScrollbarGeometry`: `None` when there is nothing to scroll.
pub(super) fn thumb(state: ScrollState, track: f32) -> Option<Thumb> {
    let max_offset = state.max_offset();
    if track <= 0. || state.len == 0 || max_offset == 0 {
        return None;
    }
    #[expect(clippy::cast_precision_loss, reason = "row counts far below f32's exact range")]
    let (len, total, offset, max) = (state.len as f32, state.total as f32, state.offset as f32, max_offset as f32);
    let height = track.min((track * len / total).max(MIN_THUMB_HEIGHT));
    let travel = (track - height).max(0.);
    Some(Thumb { height, top: travel * (offset.min(max) / max) })
}

/// `terminalScrollbarOffsetAtPointer`: the first row shown with the thumb held `grab` pixels
/// below its top at `pointer_y` in the track.
pub(super) fn offset_at_pointer(state: ScrollState, track: f32, pointer_y: f32, grab: f32) -> usize {
    let Some(thumb) = thumb(state, track) else { return 0 };
    let travel = (track - thumb.height).max(0.);
    if travel == 0. {
        return 0;
    }
    let top = (pointer_y - grab).clamp(0., travel);
    #[expect(clippy::cast_precision_loss, reason = "row counts far below f32's exact range")]
    let max = state.max_offset() as f32;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a row within the scrollback")]
    let offset = (top / travel * max).round() as usize;
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATE: ScrollState = ScrollState { total: 100, len: 20, offset: 40 };

    #[test]
    fn the_thumb_shows_the_visible_share_of_the_scrollback() {
        assert_eq!(thumb(STATE, 200.), Some(Thumb { height: 40., top: 80. }));
        let long = ScrollState { total: 10_000, len: 20, offset: 9_980 };
        assert_eq!(thumb(long, 200.), Some(Thumb { height: 18., top: 182. }), "never thinner than 18px");
        assert_eq!(thumb(ScrollState { total: 20, len: 20, offset: 0 }, 200.), None, "no scrollback");
    }

    #[test]
    fn dragging_the_thumb_maps_the_pointer_to_a_row() {
        assert_eq!(offset_at_pointer(STATE, 200., 100., 20.), 40);
        assert_eq!(offset_at_pointer(STATE, 200., -50., 20.), 0);
        assert_eq!(offset_at_pointer(STATE, 200., 500., 20.), 80);
    }

    #[test]
    fn keys_move_by_a_row_a_page_or_to_either_end() {
        assert_eq!(STATE.key_delta("up"), Some(-1));
        assert_eq!(STATE.key_delta("pagedown"), Some(20));
        assert_eq!(STATE.key_delta("home"), Some(-40));
        assert_eq!(STATE.key_delta("end"), Some(40));
        assert_eq!(STATE.key_delta("a"), None);
        assert_eq!(STATE.clamp_delta(-100), -40);
        assert_eq!(STATE.clamp_delta(100), 40);
        assert_eq!(STATE.clamp_delta(5), 5);
    }
}
