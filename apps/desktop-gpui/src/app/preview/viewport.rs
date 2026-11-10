//! Viewport emulation sizes (`browserViewportLayout.ts`) and the device toolbar's typed size
//! (`BrowserDeviceToolbar.tsx`).

use crate::preview::{PRESETS, VIEWPORT_MAX_AREA, VIEWPORT_MAX_DIMENSION, VIEWPORT_MIN_DIMENSION, Viewport};

/// `BROWSER_DEVICE_TOOLBAR_HEIGHT`.
pub(super) const TOOLBAR_HEIGHT: f32 = 38.;
/// `BROWSER_VIEWPORT_RESIZE_RAIL_SIZE`: the margin beside and below a fixed viewport.
pub(super) const RAIL: f32 = 10.;
/// What "Show device toolbar" starts at without a panel size (`FALLBACK_RESPONSIVE_VIEWPORT_SIZE`).
const FALLBACK_RESPONSIVE: (u32, u32) = (1024, 768);

const MIN: f64 = VIEWPORT_MIN_DIMENSION as f64;
const MAX: f64 = VIEWPORT_MAX_DIMENSION as f64;
const AREA: f64 = VIEWPORT_MAX_AREA as f64;

/// The dimension a toolbar field (or a drag along one edge) sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Axis {
    Width,
    Height,
}

/// `resizeAtAspectRatio`: `desired` along `axis`, the other side following `ratio`, both in range.
fn resize_at_aspect(desired: f64, ratio: f64, axis: Axis) -> (f64, f64) {
    let (ratio_min, ratio_max, area) = match axis {
        Axis::Width => (MIN * ratio, MAX * ratio, (AREA * ratio).sqrt()),
        Axis::Height => (MIN / ratio, MAX / ratio, (AREA / ratio).sqrt()),
    };
    let minimum = MIN.max(ratio_min).ceil();
    let maximum = MAX.min(ratio_max).min(area).floor();
    let other = |primary: f64| match axis {
        Axis::Width => (primary / ratio).round(),
        Axis::Height => (primary * ratio).round(),
    };
    let mut primary = desired.round().max(minimum).min(maximum);
    let mut secondary = other(primary);
    while primary * secondary > AREA && primary > minimum {
        primary -= 1.;
        secondary = other(primary);
    }
    match axis {
        Axis::Width => (primary, secondary),
        Axis::Height => (secondary, primary),
    }
}

/// The edge or corner a resize handle drags (`BrowserViewportResizeDirection`); the toolbar
/// sits above the viewport, so there is no north handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    West,
    East,
    South,
    SouthWest,
    SouthEast,
}

impl Direction {
    /// How a pointer's x and y move this direction's edges: +1 grows the viewport, -1 shrinks it.
    fn signs(self) -> (f64, f64) {
        match self {
            Self::West => (-1., 0.),
            Self::East => (1., 0.),
            Self::South => (0., 1.),
            Self::SouthWest => (-1., 1.),
            Self::SouthEast => (1., 1.),
        }
    }
}

fn normal_zoom(zoom: f64) -> f64 {
    if zoom.is_finite() && zoom > 0. { zoom } else { 1. }
}

/// `resizeFreeformViewport`: `start` moved by a pointer `delta` (host px at `zoom`) on `direction`,
/// a locked `ratio` resolving from the axis that changed most.
fn resize(start: (f64, f64), delta: (f64, f64), zoom: f64, direction: Direction, ratio: Option<f64>) -> (f64, f64) {
    let zoom = normal_zoom(zoom);
    let (sx, sy) = direction.signs();
    let (horizontal, vertical) = (sx * delta.0, sy * delta.1);
    let desired = (start.0 + horizontal / zoom, start.1 + vertical / zoom);
    if let Some(ratio) = ratio.filter(|ratio| ratio.is_finite() && *ratio > 0.) {
        let controls_width = horizontal != 0. || matches!(direction, Direction::West | Direction::East);
        let controls_height = vertical != 0. || direction == Direction::South;
        let axis = match (controls_width, controls_height) {
            (true, false) => Axis::Width,
            (false, true) => Axis::Height,
            _ if (desired.0 - start.0).abs() / start.0 >= (desired.1 - start.1).abs() / start.1 => Axis::Width,
            _ => Axis::Height,
        };
        return resize_at_aspect(if axis == Axis::Width { desired.0 } else { desired.1 }, ratio, axis);
    }
    let clamp = |value: f64| value.round().clamp(MIN, MAX);
    let (mut width, mut height) = (clamp(desired.0), clamp(desired.1));
    if width * height <= AREA {
        return (width, height);
    }
    if horizontal.abs() >= vertical.abs() {
        width = MIN.max((AREA / height).floor());
    } else {
        height = MIN.max((AREA / width).floor());
    }
    (width, height)
}

/// `resizeFreeformViewport` for a change along one `axis` (an east or south edge) at zoom 1.
fn resize_freeform(start: (f64, f64), delta: f64, axis: Axis, ratio: Option<f64>) -> (f64, f64) {
    match axis {
        Axis::Width => resize(start, (delta, 0.), 1., Direction::East, ratio),
        Axis::Height => resize(start, (0., delta), 1., Direction::South, ratio),
    }
}

/// The new length of a centered viewport whose far edge moved `pointer` px (`resizeFromEndRail`):
/// while it fits, both edges move, so the grabbed one stays under the pointer.
fn from_end_rail(start: f64, pointer: f64, available: f64) -> f64 {
    let start_edge = if start < available { f64::midpoint(available, start) } else { start };
    let target = start_edge + pointer;
    if target <= available { target * 2. - available } else { target }
}

/// `resizeFromStartRail`, for the near (left) edge.
fn from_start_rail(start: f64, pointer: f64, available: f64) -> f64 {
    if start > available {
        let to_fit = start - available;
        return if pointer <= to_fit { start - pointer } else { available - (pointer - to_fit) * 2. };
    }
    let target = (available - start) / 2. + pointer;
    if target >= 0. { available - target * 2. } else { available - target }
}

/// `resizeBrowserViewportFromRail`: the size a handle dragged `pointer` px from `start` asks for,
/// in a centered `available` area, with `zoom` host px per CSS px.
pub(super) fn resize_from_rail(
    start: (u32, u32),
    pointer: (f32, f32),
    available: (f32, f32),
    zoom: f64,
    direction: Direction,
    ratio: Option<f64>,
) -> (u32, u32) {
    let zoom = normal_zoom(zoom);
    let start = (f64::from(start.0), f64::from(start.1));
    let scaled = (start.0 * zoom, start.1 * zoom);
    let (sx, sy) = direction.signs();
    let along = |sign: f64, start: f64, pointer: f32, available: f32| {
        let (pointer, available) = (f64::from(pointer), f64::from(available));
        if sign > 0. {
            from_end_rail(start, pointer, available)
        } else if sign < 0. {
            from_start_rail(start, pointer, available)
        } else {
            start
        }
    };
    let desired = (along(sx, scaled.0, pointer.0, available.0), along(sy, scaled.1, pointer.1, available.1));
    // `resize` reads a west pointer delta as shrinking, so it gets the width change negated.
    let delta = ((desired.0 - scaled.0) * if sx < 0. { -1. } else { 1. }, desired.1 - scaled.1);
    whole_size(resize(start, delta, zoom, direction, ratio))
}

/// `handleResizeKeyDown`: an arrow key on `direction`'s handle steps 10 CSS px (50 with Shift)
/// along the axes it controls; other keys pass.
pub(super) fn key_resize(
    size: (u32, u32),
    direction: Direction,
    key: &str,
    shift: bool,
    zoom: f64,
    ratio: Option<f64>,
) -> Option<(u32, u32)> {
    let zoom = normal_zoom(zoom);
    let step = if shift { 50. } else { 10. } * zoom;
    let (sx, sy) = direction.signs();
    let delta = match key {
        "left" if sx != 0. => (-step, 0.),
        "right" if sx != 0. => (step, 0.),
        "up" if sy != 0. => (0., -step),
        "down" if sy != 0. => (0., step),
        _ => return None,
    };
    Some(whole_size(resize((f64::from(size.0), f64::from(size.1)), delta, zoom, direction, ratio)))
}

#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "sizes are whole and in range")]
fn whole_size(size: (f64, f64)) -> (u32, u32) {
    (size.0 as u32, size.1 as u32)
}

/// `resolveResponsiveBrowserViewportSize`: the panel's page area, less the toolbar and the rails.
fn responsive_size(panel: (f32, f32), zoom: f64) -> (u32, u32) {
    let zoom = if zoom.is_finite() && zoom > 0. { zoom } else { 1. };
    let area = (f64::from((panel.0 - RAIL * 2.).max(1.)), f64::from((panel.1 - TOOLBAR_HEIGHT - RAIL).max(1.)));
    whole_size(resize_freeform((area.0 / zoom, area.1 / zoom), 0., Axis::Width, None))
}

/// `browserResponsiveViewportForToggle`: "Show device toolbar" opens the default viewport, or a
/// responsive one the size of the panel when the default fills it.
pub(super) fn toolbar_viewport(default: &Viewport, panel: Option<(f32, f32)>, zoom: f64) -> Viewport {
    if *default != Viewport::Fill {
        return default.clone();
    }
    let (width, height) = panel.map_or(FALLBACK_RESPONSIVE, |panel| responsive_size(panel, zoom));
    Viewport::Freeform { width, height }
}

/// The device select's pick (`selectViewport`): Responsive keeps the size, a preset takes its own;
/// nothing to send when it is already Responsive.
pub(super) fn picked(setting: &Viewport, choice: &str) -> Option<Viewport> {
    let (width, height) = whole_size(setting.size().map(|(w, h)| (f64::from(w), f64::from(h)))?);
    match choice {
        "responsive" if matches!(setting, Viewport::Freeform { .. }) => None,
        "responsive" => Some(Viewport::Freeform { width, height }),
        id => PRESETS.iter().find(|p| p.id == id).map(|p| Viewport::Preset {
            width: p.width,
            height: p.height,
            preset_id: p.id.to_owned(),
        }),
    }
}

fn number(text: &str) -> f64 {
    let text = text.trim();
    if text.is_empty() { 0. } else { text.parse().unwrap_or(f64::NAN) }
}

fn in_range(value: f64) -> bool {
    value.is_finite() && value.fract().abs() < f64::EPSILON && (MIN..=MAX).contains(&value)
}

/// The toolbar's width and height while they are being edited (`customSize`), and the field
/// typed in last, which a locked aspect ratio resolves from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Draft {
    pub width: String,
    pub height: String,
    axis: Option<Axis>,
}

impl Draft {
    pub fn seeded(size: (u32, u32)) -> Self {
        Self { width: size.0.to_string(), height: size.1.to_string(), axis: None }
    }

    /// `updateCustomDimension`: `value` typed into `axis`'s field. With the ratio locked, an
    /// in-range value sets the other field; the typed one keeps every digit as typed.
    pub fn edited(current: Option<&Self>, size: (u32, u32), axis: Axis, value: &str, ratio: Option<f64>) -> Self {
        let mut next = current.cloned().unwrap_or_else(|| Self::seeded(size));
        next.axis = Some(axis);
        let numeric = number(value);
        let linked = ratio.filter(|_| in_range(numeric)).map(|ratio| {
            let start = (f64::from(size.0), f64::from(size.1));
            let delta = numeric - if axis == Axis::Width { start.0 } else { start.1 };
            resize_freeform(start, delta, axis, Some(ratio))
        });
        match axis {
            Axis::Width => {
                next.width = value.to_owned();
                if let Some((_, height)) = linked {
                    next.height = format!("{height:.0}");
                }
            }
            Axis::Height => {
                next.height = value.to_owned();
                if let Some((width, _)) = linked {
                    next.width = format!("{width:.0}");
                }
            }
        }
        next
    }

    /// The size the fields stand for (`customWidth` × `customHeight`), when valid (`customValid`):
    /// a locked ratio resolves it from the field typed in last.
    pub fn resolved(&self, ratio: Option<f64>) -> Option<(u32, u32)> {
        let typed = (number(&self.width), number(&self.height));
        if !in_range(typed.0) || !in_range(typed.1) {
            return None;
        }
        let size = match (ratio, self.axis) {
            (Some(ratio), Some(axis)) => resize_freeform(typed, 0., axis, Some(ratio)),
            _ => typed,
        };
        (size.0 * size.1 <= AREA).then(|| whole_size(size))
    }

    /// `applyCustomSize`: the size to send, or nothing when it's invalid or unchanged, which
    /// just drops the edit.
    pub fn committed(&self, current: (u32, u32), ratio: Option<f64>) -> Option<(u32, u32)> {
        self.resolved(ratio).filter(|size| *size != current)
    }
}

/// `rotate`: the edited size when it is valid and new, else the setting, turned a quarter.
pub(super) fn rotated(setting: &Viewport, draft: Option<&Draft>, ratio: Option<f64>) -> Viewport {
    let current = setting.size().map(|(w, h)| whole_size((f64::from(w), f64::from(h))));
    match draft.and_then(|draft| draft.resolved(ratio)).filter(|size| Some(*size) != current) {
        Some((width, height)) => Viewport::Freeform { width: height, height: width },
        None => setting.rotated(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Types `typed` into `axis`'s field one key at a time, as the browser test does.
    fn type_into(size: (u32, u32), axis: Axis, typed: &str, ratio: Option<f64>) -> Draft {
        let mut draft = Draft::edited(None, size, axis, "", ratio);
        for end in 1..=typed.len() {
            draft = Draft::edited(Some(&draft), size, axis, &typed[..end], ratio);
        }
        draft
    }

    fn ratio_of(size: (u32, u32)) -> f64 {
        f64::from(size.0) / f64::from(size.1)
    }

    #[test]
    fn a_locked_field_keeps_its_digits_until_the_linked_size_commits() {
        let size = (1920, 1080);
        let draft = type_into(size, Axis::Width, "2560", Some(ratio_of(size)));
        assert_eq!((draft.width.as_str(), draft.height.as_str()), ("2560", "1440"), "the height follows");
        assert_eq!(draft.committed(size, Some(ratio_of(size))), Some((2560, 1440)), "the typed width commits");

        let size = (390, 844);
        let draft = type_into(size, Axis::Height, "2532", Some(ratio_of(size)));
        assert_eq!(draft.height, "2532", "every typed digit stays");
        assert_eq!(draft.committed(size, Some(ratio_of(size))), Some((1170, 2532)), "the width follows the ratio");
    }

    #[test]
    fn a_locked_size_clamps_to_the_minimum_and_the_area_cap_when_committed() {
        let size = (1920, 1080);
        let draft = type_into(size, Axis::Width, "256", Some(ratio_of(size)));
        assert_eq!(draft.width, "256", "the field shows what was typed");
        assert_eq!(draft.committed(size, Some(ratio_of(size))), Some((427, 240)), "the height can't go under 240");

        let size = (1920, 1920);
        let draft = type_into(size, Axis::Width, "3840", Some(ratio_of(size)));
        assert_eq!(draft.committed(size, Some(ratio_of(size))), Some((2880, 2880)), "3840² is over the area cap");
    }

    #[test]
    fn an_unlocked_size_commits_only_when_valid_and_changed() {
        let size = (1280, 800);
        let draft = type_into(size, Axis::Width, "100", None);
        assert_eq!(draft.height, "800", "unlocked, the other field stays");
        assert_eq!(draft.resolved(None), None, "under 240 is invalid");
        assert_eq!(draft.committed(size, None), None);
        assert_eq!(type_into(size, Axis::Width, "12a", None).resolved(None), None, "not a number");
        assert_eq!(type_into(size, Axis::Height, "2160", None).committed(size, None), Some((1280, 2160)));
        assert_eq!(type_into(size, Axis::Width, "1280", None).committed(size, None), None, "unchanged");
        let over = Draft { width: "3840".into(), height: "3840".into(), axis: Some(Axis::Width) };
        assert_eq!(over.resolved(None), None, "over the area cap");
    }

    #[test]
    fn rotating_turns_a_valid_edit_or_else_the_setting() {
        let setting = Viewport::Preset { width: 390, height: 844, preset_id: "iphone-12-pro".into() };
        assert_eq!(
            rotated(&setting, None, None),
            Viewport::Preset { width: 844, height: 390, preset_id: "iphone-12-pro".into() },
        );
        let draft = type_into((390, 844), Axis::Width, "500", None);
        assert_eq!(rotated(&setting, Some(&draft), None), Viewport::Freeform { width: 844, height: 500 });
        let invalid = type_into((390, 844), Axis::Width, "5", None);
        assert_eq!(rotated(&setting, Some(&invalid), None), setting.rotated(), "an invalid edit is ignored");
    }

    #[test]
    fn the_device_select_keeps_the_size_for_responsive() {
        let preset = Viewport::Preset { width: 390, height: 844, preset_id: "iphone-12-pro".into() };
        assert_eq!(picked(&preset, "responsive"), Some(Viewport::Freeform { width: 390, height: 844 }));
        assert_eq!(picked(&Viewport::Freeform { width: 500, height: 600 }, "responsive"), None, "already responsive");
        assert_eq!(
            picked(&Viewport::Freeform { width: 500, height: 600 }, "pixel-7"),
            Some(Viewport::Preset { width: 412, height: 915, preset_id: "pixel-7".into() }),
        );
        assert_eq!(picked(&preset, "nope"), None);
    }

    #[test]
    fn a_dragged_rail_stays_under_the_pointer() {
        let available = (1120., 818.);
        let rail = resize_from_rail;
        assert_eq!(rail((1120, 818), (-100., -50.), available, 1., Direction::SouthEast, None), (920, 718));
        assert_eq!(rail((800, 600), (300., 0.), (1200., 800.), 1., Direction::East, None), (1300, 600));
        assert_eq!(rail((560, 409), (-100., 0.), available, 2., Direction::East, None), (460, 409));
        // The left rail grows the viewport as it moves left, both edges moving while centered.
        assert_eq!(rail((800, 600), (-100., 0.), (1200., 800.), 1., Direction::West, None), (1000, 600));
        assert_eq!(rail((800, 600), (0., 50.), (1200., 800.), 1., Direction::SouthWest, None), (800, 700));
        assert_eq!(rail((800, 600), (500., 0.), (1200., 800.), 1., Direction::West, None), (240, 600), "min width");
    }

    #[test]
    fn a_resize_keeps_a_locked_ratio_and_the_size_caps() {
        let ratio = Some(4. / 3.);
        assert_eq!(whole_size(resize((400., 300.), (100., 0.), 1., Direction::SouthEast, ratio)), (500, 375));
        assert_eq!(whole_size(resize((400., 300.), (0., 30.), 1., Direction::South, ratio)), (440, 330));
        assert_eq!(whole_size(resize((400., 300.), (100., 0.), 2., Direction::East, None)), (450, 300), "at 200%");
        assert_eq!(
            whole_size(resize((3800., 2100.), (100., 100.), 1., Direction::SouthEast, None)),
            (3770, 2200),
            "the area cap trims the axis that moved most",
        );
    }

    #[test]
    fn arrow_keys_step_the_axes_a_handle_controls() {
        let size = (400, 300);
        assert_eq!(key_resize(size, Direction::East, "right", false, 1., None), Some((410, 300)));
        assert_eq!(key_resize(size, Direction::East, "right", true, 1., None), Some((450, 300)), "Shift steps 50");
        assert_eq!(key_resize(size, Direction::East, "up", false, 1., None), None, "east controls only the width");
        assert_eq!(key_resize(size, Direction::West, "left", false, 1., None), Some((410, 300)), "left grows it");
        assert_eq!(key_resize(size, Direction::SouthWest, "down", false, 1., None), Some((400, 310)));
        assert_eq!(key_resize(size, Direction::South, "up", false, 2., None), Some((400, 290)), "CSS px at 200%");
        assert_eq!(key_resize(size, Direction::South, "enter", false, 1., None), None);
        assert_eq!(key_resize((240, 240), Direction::East, "left", false, 1., None), Some((240, 240)), "clamped");
    }

    #[test]
    fn showing_the_toolbar_sizes_a_responsive_viewport_to_the_panel() {
        let fill = Viewport::Fill;
        assert_eq!(toolbar_viewport(&fill, Some((800., 600.)), 1.), Viewport::Freeform { width: 780, height: 552 });
        assert_eq!(toolbar_viewport(&fill, Some((800., 600.)), 2.), Viewport::Freeform { width: 390, height: 276 });
        assert_eq!(toolbar_viewport(&fill, Some((200., 100.)), 1.), Viewport::Freeform { width: 240, height: 240 });
        assert_eq!(toolbar_viewport(&fill, None, 1.), Viewport::Freeform { width: 1024, height: 768 });
        let preset = Viewport::Preset { width: 412, height: 915, preset_id: "pixel-7".into() };
        assert_eq!(toolbar_viewport(&preset, Some((800., 600.)), 1.), preset, "the default viewport wins");
    }
}
