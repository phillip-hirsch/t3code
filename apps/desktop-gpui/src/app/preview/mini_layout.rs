//! Where the floating preview sits and how big it is (`previewMiniPlayerLayout.ts`): a frame at
//! the source's aspect ratio, kept inside the chat canvas and clear of the composer.

/// `PREVIEW_MINI_PLAYER_EDGE_GAP`.
pub(super) const GAP: f64 = 12.;
/// `PREVIEW_MINI_PLAYER_CORNER_RADIUS`.
pub(super) const CORNER_RADIUS: f32 = 12.;
const DEFAULT_BOX: Size = Size { width: 320., height: 320. };
const MIN_SIZE: Size = Size { width: 240., height: 150. };
/// The CSS size a fill tab lays out at when the panel never showed it.
const FALLBACK_SOURCE: Size = Size { width: 1280., height: 800. };

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Size {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Position {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Frame {
    fn new(position: Position, size: Size) -> Self {
        Self { x: position.x, y: position.y, width: size.width, height: size.height }
    }

    pub fn size(&self) -> Size {
        Size { width: self.width, height: self.height }
    }
}

/// The composer along the canvas's bottom, over the columns it spans.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Composer {
    pub left: f64,
    pub right: f64,
    pub height: f64,
}

/// The inline details card along the canvas's top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Card {
    pub left: f64,
    pub right: f64,
    pub bottom: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Obstacles {
    pub composer: Option<Composer>,
    pub details_card: Option<Card>,
}

/// The edge or corner a resize drags (`BrowserViewportResizeDirection`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
}

impl Direction {
    pub const ALL: [Direction; 8] = [
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
        Direction::NorthWest,
        Direction::NorthEast,
        Direction::SouthWest,
        Direction::SouthEast,
    ];

    fn east(self) -> bool {
        matches!(self, Direction::NorthEast | Direction::East | Direction::SouthEast)
    }

    fn west(self) -> bool {
        matches!(self, Direction::NorthWest | Direction::West | Direction::SouthWest)
    }

    fn north(self) -> bool {
        matches!(self, Direction::NorthWest | Direction::North | Direction::NorthEast)
    }

    fn south(self) -> bool {
        matches!(self, Direction::SouthWest | Direction::South | Direction::SouthEast)
    }
}

/// `resolvePreviewMiniPlayerSourceSize`: what the frame keeps the aspect of. A fixed viewport is
/// its own size, a fill tab the panel page it was floated from (`fitted`), both at its zoom.
pub(super) fn source_size(viewport: Option<(f32, f32)>, fitted: Option<(f32, f32)>, zoom: f64) -> Size {
    let zoom = if zoom.is_finite() && zoom > 0. { zoom } else { 1. };
    let css = match (viewport, fitted) {
        (Some((width, height)), _) => Size { width: f64::from(width), height: f64::from(height) },
        (None, Some((width, height))) => Size {
            width: (f64::from(width) / zoom).round().max(1.),
            height: (f64::from(height) / zoom).round().max(1.),
        },
        (None, None) => FALLBACK_SOURCE,
    };
    Size { width: css.width * zoom, height: css.height * zoom }
}

#[derive(Clone, Copy)]
struct Span {
    left: f64,
    right: f64,
}

fn span_of(x: f64, width: f64) -> Span {
    Span { left: x, right: x + width }
}

fn overlaps(span: Span, left: f64, right: f64) -> bool {
    span.left < right && span.right > left
}

fn floor_for(span: Span, container: Size, obstacles: Obstacles) -> f64 {
    match obstacles.composer {
        Some(composer) if overlaps(span, composer.left, composer.right) => container.height - composer.height.max(0.),
        _ => container.height,
    }
}

fn ceiling_for(span: Span, obstacles: Obstacles) -> f64 {
    match obstacles.details_card {
        Some(card) if overlaps(span, card.left, card.right) => card.bottom.max(0.),
        _ => 0.,
    }
}

fn available_area(container: Size, obstacles: Obstacles, span: Option<Span>) -> Size {
    let height = match span {
        Some(span) => floor_for(span, container, obstacles) - ceiling_for(span, obstacles),
        None => container.height - obstacles.composer.map_or(0., |composer| composer.height.max(0.)),
    };
    Size { width: container.width - GAP * 2., height: height - GAP * 2. }
}

fn minimum_width(source: Size) -> f64 {
    MIN_SIZE.width.max(MIN_SIZE.height * source.width / source.height)
}

fn fit_width(desired: f64, source: Size, max: Size, minimum: f64) -> Size {
    let ratio = source.width / source.height;
    let width = desired.max(minimum).min(max.width.max(1.)).min((max.height * ratio).max(1.));
    Size { width: width.round(), height: (width / ratio).round() }
}

fn default_width(source: Size) -> f64 {
    DEFAULT_BOX.width.min(DEFAULT_BOX.height * source.width / source.height)
}

fn clamp_to_container(position: Position, container: Size, player: Size, minimum_x: f64) -> Position {
    Position {
        x: position.x.max(minimum_x).max(GAP).min((container.width - player.width - GAP).max(GAP)),
        y: position.y.max(GAP).min((container.height - player.height - GAP).max(GAP)),
    }
}

fn overlaps_obstacle(position: Position, player: Size, container: Size, obstacles: Obstacles) -> bool {
    let span = span_of(position.x, player.width);
    position.y + player.height > floor_for(span, container, obstacles) || position.y < ceiling_for(span, obstacles)
}

/// `clampPreviewMiniPlayerPosition`: inside the container, then the nearest spot clear of the
/// composer and the card: above or below them, or beside them when that's the shorter move.
pub(super) fn clamp_position(
    position: Position,
    container: Size,
    player: Size,
    obstacles: Obstacles,
    minimum_x: f64,
) -> Position {
    let inside = clamp_to_container(position, container, player, minimum_x);
    if !overlaps_obstacle(inside, player, container, obstacles) {
        return inside;
    }
    let fit_rows = |x: f64| {
        let span = span_of(x, player.width);
        let y = inside
            .y
            .min(floor_for(span, container, obstacles) - GAP - player.height)
            .max(ceiling_for(span, obstacles) + GAP)
            .max(GAP);
        Position { x, y: (container.height - GAP - player.height).min(y) }
    };
    let fallback = fit_rows(inside.x);
    let span = span_of(inside.x, player.width);
    let composer = obstacles.composer.map(|c| (c.left, c.right));
    let card = obstacles.details_card.map(|c| (c.left, c.right));
    let beside = [composer, card]
        .into_iter()
        .flatten()
        .filter(|(left, right)| overlaps(span, *left, *right))
        .flat_map(|(left, right)| [left - GAP - player.width, right + GAP]);
    let mut best = fallback;
    let mut best_distance = if overlaps_obstacle(fallback, player, container, obstacles) {
        f64::INFINITY
    } else {
        (fallback.y - inside.y).abs()
    };
    for x in beside {
        let candidate = fit_rows(x);
        // The clamp returns `x` itself when it is inside.
        let clamped = (clamp_to_container(candidate, container, player, minimum_x).x - x).abs() > f64::EPSILON;
        if clamped || overlaps_obstacle(candidate, player, container, obstacles) {
            continue;
        }
        let distance = (x - inside.x).abs() + (candidate.y - inside.y).abs();
        if distance < best_distance {
            best = candidate;
            best_distance = distance;
        }
    }
    best
}

/// `resolvePreviewMiniPlayerFrame`: the stored width (else the default box) fitted to the space,
/// at the stored position (else the top-right corner, or under the card). A narrow canvas only
/// shrinks this layout, never the stored width.
pub(super) fn resolve_frame(
    width: Option<f64>,
    position: Option<Position>,
    source: Size,
    container: Size,
    obstacles: Obstacles,
) -> Frame {
    let stored = position.zip(width.filter(|width| *width > 0.));
    let area_obstacles =
        if stored.is_some() { Obstacles { composer: obstacles.composer, details_card: None } } else { obstacles };
    let size = fit_width(
        width.unwrap_or_else(|| default_width(source)),
        source,
        available_area(container, area_obstacles, stored.map(|(position, width)| span_of(position.x, width))),
        width.unwrap_or_else(|| minimum_width(source)),
    );
    let anchored = position.unwrap_or(match obstacles.details_card {
        Some(card) => Position { x: card.right - size.width, y: card.bottom + GAP },
        None => Position { x: container.width - GAP - size.width, y: GAP },
    });
    Frame::new(clamp_position(anchored, container, size, obstacles, GAP), size)
}

/// `resizePreviewMiniPlayer`: `start` dragged by `delta` at `direction`, at the source's aspect
/// ratio; a corner follows the ratio's diagonal, and the opposite edge stays put.
pub(super) fn resize(
    start: Frame,
    direction: Direction,
    delta: Position,
    source: Size,
    container: Size,
    obstacles: Obstacles,
) -> Frame {
    let (east, west, north, south) = (direction.east(), direction.west(), direction.north(), direction.south());
    let right = start.x + start.width;
    let bottom = start.y + start.height;
    let span = span_of(start.x, start.width);
    let floor = floor_for(span, container, obstacles);
    let ceiling = ceiling_for(span, obstacles);
    let max = Size {
        width: if west {
            right - GAP
        } else if east {
            container.width - GAP - start.x
        } else {
            container.width - GAP * 2.
        },
        height: if north {
            bottom - ceiling - GAP
        } else if south {
            floor - GAP - start.y
        } else {
            floor - ceiling - GAP * 2.
        },
    };
    let desired_width = start.width
        + if east {
            delta.x
        } else if west {
            -delta.x
        } else {
            0.
        };
    let desired_height = start.height
        + if south {
            delta.y
        } else if north {
            -delta.y
        } else {
            0.
        };
    let ratio = source.width / source.height;
    // The aspect ratio's diagonal: switching the dominant axis would jump when the axes move apart.
    let desired = match (east || west, north || south) {
        (true, true) => (desired_width + desired_height / ratio) / (1. + 1. / ratio.powi(2)),
        (true, false) => desired_width,
        _ => desired_height * ratio,
    };
    let size = fit_width(desired, source, max, start.width.min(minimum_width(source)));
    let anchored = Position {
        x: if west { right - size.width } else { start.x },
        y: if north { bottom - size.height } else { start.y },
    };
    Frame::new(clamp_position(anchored, container, size, obstacles, GAP), size)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTAINER: Size = Size { width: 1_000., height: 700. };
    const SOURCE: Size = Size { width: 1_600., height: 1_000. };
    const PHONE: Size = Size { width: 390., height: 844. };
    const COMPOSER: Composer = Composer { left: 100., right: 900., height: 150. };
    const CARD: Card = Card { left: 680., right: 988., bottom: 260. };

    fn composer(height: f64) -> Obstacles {
        Obstacles { composer: Some(Composer { height, ..COMPOSER }), details_card: None }
    }

    fn with_card() -> Obstacles {
        Obstacles { composer: Some(COMPOSER), details_card: Some(CARD) }
    }

    fn frame(x: f64, y: f64, width: f64, height: f64) -> Frame {
        Frame { x, y, width, height }
    }

    fn at(x: f64, y: f64) -> Position {
        Position { x, y }
    }

    fn size(width: f64, height: f64) -> Size {
        Size { width, height }
    }

    fn beside() -> Obstacles {
        Obstacles { composer: Some(Composer { left: 300., right: 900., height: 300. }), details_card: None }
    }

    #[test]
    fn a_fill_tab_keeps_the_size_it_was_floated_at() {
        assert_eq!(source_size(None, Some((1280., 800.)), 1.), size(1_280., 800.));
        assert_eq!(source_size(None, Some((1280., 800.)), 2.), size(1_280., 800.), "the page laid out at 640 × 400");
        assert_eq!(source_size(Some((390., 844.)), Some((1280., 800.)), 2.), size(780., 1_688.));
        assert_eq!(source_size(None, None, 1.), size(1_280., 800.));
    }

    #[test]
    fn shrinks_to_the_space_above_the_composer_without_losing_the_stored_width() {
        let frame = resolve_frame(Some(800.), Some(at(100., 80.)), SOURCE, CONTAINER, composer(300.));
        assert_eq!(frame, Frame { x: 100., y: GAP, width: 602., height: 376. });
    }

    #[test]
    fn keeps_a_tall_frame_parked_beside_the_composer_across_layout_passes() {
        let resized = resize(frame(12., 100., 240., 519.), Direction::East, at(10., 0.), PHONE, CONTAINER, beside());
        let laid_out = resolve_frame(Some(resized.width), Some(at(resized.x, resized.y)), PHONE, CONTAINER, beside());
        assert_eq!(laid_out, resized);
    }

    #[test]
    fn corner_motion_does_not_jump_across_opposing_dominant_axes() {
        let start = frame(300., 200., 320., 200.);
        let widths: Vec<f64> = [-99., -100., -101.]
            .into_iter()
            .map(|y| resize(start, Direction::SouthEast, at(160., y), SOURCE, CONTAINER, Obstacles::default()).width)
            .collect();
        assert!((widths[0] - widths[2]).abs() <= 2., "widths {widths:?} jumped");
    }

    #[test]
    fn corner_motion_follows_the_aspect_ratio_diagonal() {
        let start = frame(300., 200., 320., 200.);
        let resized = resize(start, Direction::SouthEast, at(20., 100.), SOURCE, CONTAINER, Obstacles::default());
        assert_eq!(resized, frame(300., 200., 379., 237.));
    }

    #[test]
    fn stops_at_the_container_edge_in_the_drag_direction() {
        let start = frame(600., 12., 320., 200.);
        let resized = resize(start, Direction::East, at(500., 0.), SOURCE, CONTAINER, Obstacles::default());
        assert_eq!(resized, frame(600., 12., 388., 243.));
    }

    #[test]
    fn shifts_the_player_when_the_free_axis_would_overflow() {
        let start = frame(12., 400., 320., 200.);
        let resized = resize(start, Direction::East, at(300., 0.), SOURCE, CONTAINER, Obstacles::default());
        assert_eq!(resized, frame(12., 300., 620., 388.));
    }

    #[test]
    fn a_player_beside_a_tall_composer_keeps_its_height_on_an_edge_drag() {
        let start = frame(12., 100., 240., 519.);
        assert_eq!(
            resize(start, Direction::East, at(10., 0.), PHONE, CONTAINER, beside()),
            frame(12., 100., 250., 541.),
        );
        let over = resize(frame(400., 100., 240., 519.), Direction::East, at(10., 0.), PHONE, CONTAINER, beside());
        assert!((over.height - 376.).abs() < f64::EPSILON, "over the composer it shrinks, got {over:?}");
    }

    #[test]
    fn stops_growing_downward_at_the_composer_beneath_its_columns() {
        let start = frame(300., 100., 320., 200.);
        let resized = resize(start, Direction::South, at(0., 400.), SOURCE, CONTAINER, composer(300.));
        assert_eq!(resized, frame(300., 100., 461., 288.));
    }

    #[test]
    fn opens_under_the_inline_details_card_with_right_edges_aligned() {
        let frame = resolve_frame(None, None, SOURCE, CONTAINER, with_card());
        assert_eq!(frame, Frame { x: 988. - 320., y: 260. + GAP, width: 320., height: 200. });
    }

    #[test]
    fn opens_left_of_the_card_when_it_cannot_fit_beneath_it() {
        let tall = Obstacles { details_card: Some(Card { bottom: 480., ..CARD }), ..with_card() };
        let frame = resolve_frame(None, None, SOURCE, CONTAINER, tall);
        assert_eq!(frame, Frame { x: 680. - GAP - 320., y: 700. - 150. - GAP - 200., width: 320., height: 200. });
    }

    #[test]
    fn keeps_a_stored_size_when_the_card_covers_its_columns() {
        let frame = resolve_frame(Some(480.), Some(at(700., 280.)), SOURCE, CONTAINER, with_card());
        assert_eq!(frame, Frame { x: 680. - GAP - 480., y: 700. - 150. - GAP - 300., width: 480., height: 300. });
    }

    #[test]
    fn drops_into_the_margin_beside_the_composer() {
        let position = clamp_position(at(20., 500.), CONTAINER, size(60., 150.), composer(150.), GAP);
        assert_eq!(position, at(20., 500.));
    }

    #[test]
    fn slides_sideways_past_the_composer_when_that_is_the_shorter_move() {
        let position = clamp_position(at(850., 500.), CONTAINER, size(60., 150.), composer(150.), GAP);
        assert_eq!(position, at(COMPOSER.right + GAP, 500.));
    }

    #[test]
    fn clears_the_card_and_the_composer_together_in_a_tight_column() {
        let tight =
            Obstacles { composer: Some(Composer { left: 0., right: 1_000., height: 300. }), details_card: Some(CARD) };
        let player = size(360., 240.);
        assert_eq!(
            clamp_position(at(700., 600.), CONTAINER, player, tight, GAP),
            at(680. - GAP - 360., 700. - 300. - GAP - 240.)
        );
        assert_eq!(clamp_position(at(700., 600.), CONTAINER, size(360., 400.), tight, GAP), at(628., 260. + GAP));
    }

    #[test]
    fn stays_inside_the_container_when_a_full_width_card_leaves_no_open_rows() {
        let wall =
            Obstacles { composer: Some(COMPOSER), details_card: Some(Card { left: 0., right: 1_000., bottom: 690. }) };
        let player = size(360., 240.);
        assert_eq!(clamp_position(at(300., 500.), CONTAINER, player, wall, GAP), at(300., 700. - GAP - 240.));
    }

    #[test]
    fn sits_above_the_composer_when_too_wide_for_either_margin() {
        let position = clamp_position(at(100., 100.), CONTAINER, size(976., 500.), composer(150.), GAP);
        assert_eq!(position, at(GAP, 700. - 150. - GAP - 500.));
    }
}
