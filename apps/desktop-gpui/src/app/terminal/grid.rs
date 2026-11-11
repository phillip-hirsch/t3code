//! Paints one terminal's visible grid. Each row becomes a few shaped text
//! runs (consecutive cells of one style) and merged background rects, so a
//! frame costs a handful of draw calls per row rather than one per cell.

use gpui_kit::{
    App, BorderStyle, Bounds, ContentMask, CursorStyle, Element, ElementId, ElementInputHandler, Entity, Font,
    FontFallbacks, FontStyle, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    IntoElement, LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba, ScrollEvent, ShapedLine,
    SharedString, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle, Window, fill, font, outline, point,
    px, relative, rgb, size,
};

use super::core;
use super::model::{Cell, CursorShape, Rgb, Snapshot, ViewportPoint, Width};
use super::session::TerminalSession;
use crate::theme::{self, Fonts, Tokens};

/// `CONTENT_PADDING` in `apps/web/src/terminal/ghostty/surface.ts`.
pub(super) const PADDING: Pixels = px(4.);

/// `terminalThemeFromApp` in `ThreadTerminalDrawer.tsx` over the default theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) struct TerminalTheme {
    pub background: Hsla,
    pub foreground: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
    pub palette: [Hsla; 16],
}

const DARK_PALETTE: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666, 0xf14c4c, 0x23d18b,
    0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5,
];
const LIGHT_PALETTE: [u32; 16] = [
    0x000000, 0xcd3131, 0x107c10, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555, 0x666666, 0xcd3131, 0x14ce14,
    0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5,
];

impl TerminalTheme {
    pub fn new(tokens: &Tokens) -> Self {
        let palette = if tokens.dark { DARK_PALETTE } else { LIGHT_PALETTE };
        let colors = tokens.terminal;
        Self {
            background: colors.background,
            foreground: colors.foreground,
            cursor: colors.cursor,
            selection: colors.selection,
            palette: palette.map(|value| rgb(value).into()),
        }
    }

    /// The colors Ghostty resolves cells with (`setTheme`).
    pub(super) fn core(&self) -> core::Theme {
        core::Theme {
            fg: to_rgb(self.foreground),
            bg: to_rgb(self.background),
            cursor: to_rgb(self.cursor),
            palette: self.palette.map(to_rgb),
        }
    }

    /// Faint (SGR 2) default text: the foreground blended toward the background, as Ghostty draws it.
    pub fn faint_foreground(&self) -> Hsla {
        hsla(core::faint(to_rgb(self.foreground), to_rgb(self.background)))
    }
}

fn to_rgb(color: Hsla) -> Rgb {
    let color = Rgba::from(color);
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a 0-255 channel; `as` saturates")]
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    Rgb { r: channel(color.r), g: channel(color.g), b: channel(color.b) }
}

fn rgb8(r: u8, g: u8, b: u8, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)).into();
    color.a = alpha;
    color
}

fn hsla(color: Rgb) -> Hsla {
    rgb8(color.r, color.g, color.b, 1.)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RunStyle {
    pub color: Hsla,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl RunStyle {
    fn new(cell: &Cell) -> Self {
        Self {
            color: hsla(cell.fg),
            bold: cell.bold,
            italic: cell.italic,
            underline: cell.underline,
            strikethrough: cell.strikethrough,
        }
    }
}

/// Cells of one style on one row, starting at `column`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Run {
    pub row: usize,
    pub column: usize,
    pub text: SharedString,
    pub style: RunStyle,
}

/// A run while `batch` still appends cells to it.
struct PendingRun {
    row: usize,
    column: usize,
    text: String,
    style: RunStyle,
}

/// A span of cells on one row painted with one color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Span {
    pub row: usize,
    pub column: usize,
    pub len: usize,
    pub color: Hsla,
}

#[derive(Default, Debug)]
pub(super) struct Batched {
    /// The terminal's background, painted under every span (`renderGhosttySnapshot`).
    pub background: Hsla,
    pub runs: Vec<Run>,
    pub backgrounds: Vec<Span>,
    pub selection: Vec<Span>,
    /// Overlined cells, spaces included, in their text color: text runs cannot draw an overline.
    pub overlines: Vec<Span>,
}

fn push_span(spans: &mut Vec<Span>, row: usize, column: usize, color: Hsla) {
    match spans.last_mut() {
        Some(span) if span.row == row && span.column + span.len == column && span.color == color => span.len += 1,
        _ => spans.push(Span { row, column, len: 1, color }),
    }
}

fn is_blank(cell: &Cell) -> bool {
    cell.invisible || matches!(cell.text.as_str(), "" | " " | "\t")
}

/// Groups the visible cells into text runs and background spans (`renderGhosttySnapshot`).
/// A wide glyph ends its run, so every glyph after it still starts on its own cell.
pub(super) fn batch(snapshot: &Snapshot, theme: &TerminalTheme) -> Batched {
    let mut out = Batched { background: hsla(snapshot.bg), ..Batched::default() };
    for (row, cells) in snapshot.rows.iter().enumerate() {
        let mut run: Option<PendingRun> = None;
        let mut wide = false;
        for (column, cell) in cells.cells.iter().enumerate() {
            if cell.bg != snapshot.bg {
                push_span(&mut out.backgrounds, row, column, hsla(cell.bg));
            }
            if cell.selected {
                push_span(&mut out.selection, row, column, theme.selection);
            }
            if matches!(cell.wide, Width::SpacerTail | Width::SpacerHead) {
                continue;
            }
            if cell.overline {
                push_span(&mut out.overlines, row, column, hsla(cell.fg));
                if cell.wide == Width::Wide {
                    push_span(&mut out.overlines, row, column + 1, hsla(cell.fg));
                }
            }
            let style = RunStyle::new(cell);
            let blank = is_blank(cell);
            let continues = !wide && run.as_ref().is_some_and(|run| run.style == style);
            if !continues {
                flush(&mut run, &mut out.runs);
                if blank && !style.underline && !style.strikethrough {
                    wide = false;
                    continue;
                }
                run = Some(PendingRun { row, column, text: String::new(), style });
            }
            if let Some(run) = &mut run {
                run.text.push_str(if blank { " " } else { &cell.text });
            }
            wide = cell.wide == Width::Wide;
        }
        flush(&mut run, &mut out.runs);
    }
    out
}

fn flush(run: &mut Option<PendingRun>, runs: &mut Vec<Run>) {
    if let Some(PendingRun { row, column, mut text, style }) = run.take() {
        let trimmed = text.trim_end_matches(' ').len();
        if trimmed > 0 || style.underline || style.strikethrough {
            if !style.underline && !style.strikethrough {
                text.truncate(trimmed);
            }
            runs.push(Run { row, column, text: text.into(), style });
        }
    }
}

/// The cursor where the snapshot places it, with what a filled block redraws over its cell.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CursorCell {
    pub at: ViewportPoint,
    pub shape: CursorShape,
    pub color: Hsla,
    pub wide: bool,
    /// The cell's glyph in the background color, for a filled block.
    pub glyph: Option<(SharedString, RunStyle)>,
}

impl CursorCell {
    pub fn of(snapshot: &Snapshot) -> Option<Self> {
        let at = snapshot.cursor?;
        let cell = snapshot.rows.get(at.y)?.cells.get(at.x);
        let glyph = cell.filter(|cell| !is_blank(cell)).map(|cell| {
            (SharedString::from(cell.text.clone()), RunStyle { color: hsla(snapshot.bg), ..RunStyle::new(cell) })
        });
        Some(Self {
            at,
            shape: snapshot.cursor_shape,
            color: hsla(snapshot.cursor_color),
            wide: cell.is_some_and(|cell| cell.wide == Width::Wide),
            glyph,
        })
    }
}

/// What the grid element draws this frame, read from the session in one update.
pub(super) struct Frame {
    pub batched: std::rc::Rc<Batched>,
    pub cursor: Option<CursorCell>,
    pub columns: usize,
    pub rows: usize,
    /// With scrollback, the last row sits on the bottom edge (`terminalContentOriginY`).
    pub anchor_bottom: bool,
}

/// Cell size and where the grid starts, in window coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Geometry {
    /// The grid element; mouse reports measure from its corner.
    pub bounds: Bounds<Pixels>,
    pub origin: Point<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub columns: usize,
    pub rows: usize,
    /// Where a selection drag starts scrolling: the pane's padded top and bottom, kept inside the window.
    pub scroll_edges: (Pixels, Pixels),
}

impl Geometry {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "non-negative cell coordinates; `as` saturates"
    )]
    fn cell(&self, position: Point<Pixels>) -> (f32, f32, usize, usize) {
        let x = (position.x - self.origin.x) / self.cell_width;
        let y = (position.y - self.origin.y) / self.line_height;
        (x, y, x.max(0.).floor() as usize, y.max(0.).floor() as usize)
    }

    /// `cellAt`: the visible cell under `position`, clamped to the grid.
    pub fn cell_at(&self, position: Point<Pixels>) -> ViewportPoint {
        let (_, _, x, y) = self.cell(position);
        ViewportPoint { x: x.min(self.columns.saturating_sub(1)), y: y.min(self.rows.saturating_sub(1)) }
    }

    /// `terminalGridCellAt`: the cell under `position`, or `None` off the grid.
    pub fn grid_cell_at(&self, position: Point<Pixels>) -> Option<ViewportPoint> {
        let (fx, fy, x, y) = self.cell(position);
        (fx >= 0. && fy >= 0. && x < self.columns && y < self.rows).then_some(ViewportPoint { x, y })
    }
}

/// `TERMINAL_GLYPH_FALLBACKS`: installed Nerd Fonts draw prompt glyphs the chosen font lacks.
const GLYPH_FALLBACKS: [&str; 9] = [
    "Symbols Nerd Font Mono",
    "Symbols Nerd Font",
    "JetBrainsMono Nerd Font",
    "JetBrainsMono NF",
    "FiraCode Nerd Font",
    "Hack Nerd Font",
    "MesloLGS NF",
    "CaskaydiaCove Nerd Font",
    "PowerlineSymbols",
];

/// The Terminal font setting, which follows the code font unless set separately.
fn terminal_font(fonts: &Fonts) -> (Font, Pixels) {
    static FALLBACKS: std::sync::OnceLock<FontFallbacks> = std::sync::OnceLock::new();
    let fallbacks = FALLBACKS.get_or_init(|| FontFallbacks::from_fonts(GLYPH_FALLBACKS.map(str::to_owned).to_vec()));
    (Font { fallbacks: Some(fallbacks.clone()), ..font(fonts.terminal) }, px(fonts.terminal_size))
}

/// `measureGhosttyCell`: the advance of "M", and 1.35× the font size or the glyph height.
pub(super) fn cell_size(window: &Window, fonts: &Fonts) -> (Pixels, Pixels) {
    let text_system = window.text_system();
    let (font, font_size) = terminal_font(fonts);
    let font_id = text_system.resolve_font(&font);
    let width = text_system.advance(font_id, font_size, 'M').map_or(px(7.), |size| size.width);
    let glyph = text_system.ascent(font_id, font_size) + text_system.descent(font_id, font_size).abs();
    let height = (f32::from(font_size) * 1.35).round().max(f32::from(glyph).ceil());
    (width, px(height))
}

pub(super) struct TerminalGrid {
    pub session: Entity<TerminalSession>,
    pub theme: TerminalTheme,
    pub focused: bool,
    /// Off in the dark half of the focused cursor's blink.
    pub cursor_on: bool,
}

impl IntoElement for TerminalGrid {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct Cursor {
    bounds: Bounds<Pixels>,
    hollow: bool,
    color: Hsla,
    glyph: Option<ShapedLine>,
}

pub(super) struct GridPrepaint {
    hitbox: Hitbox,
    geometry: Geometry,
    background: Hsla,
    backgrounds: Vec<Bounds<Pixels>>,
    background_colors: Vec<Hsla>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    /// `renderGhosttySnapshot`'s overlines: 1px, a pixel below each cell's top.
    overlines: Vec<(Bounds<Pixels>, Hsla)>,
    cursor: Option<Cursor>,
    link_hovered: bool,
    link_underlines: Vec<Bounds<Pixels>>,
}

impl Element for TerminalGrid {
    type RequestLayoutState = ();
    type PrepaintState = GridPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> GridPrepaint {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let fonts = theme::tokens(cx).fonts;
        let (cell_width, line_height) = cell_size(window, &fonts);
        // `terminalGridSize`.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a whole count of cells; `as` saturates"
        )]
        let columns = (((bounds.size.width - PADDING * 2.) / cell_width).floor() as usize).max(1);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a whole count of cells; `as` saturates"
        )]
        let rows = (((bounds.size.height - PADDING * 2.) / line_height).floor() as usize).max(1);
        let theme = self.theme;
        let frame = self.session.update(cx, |session, cx| {
            session.fit(columns, rows, (cell_width, line_height), cx);
            session.frame(&theme)
        });
        // The drawn grid: the shared PTY's, capped to this fit.
        let (columns, rows) = (frame.columns, frame.rows);
        let slack = bounds.size.height - PADDING * 2. - line_height * rows as f32;
        let top = if frame.anchor_bottom { PADDING + slack.max(px(0.)) } else { PADDING };
        let origin = bounds.origin + point(PADDING, top);
        let scroll_edges =
            (bounds.top().max(px(0.)) + PADDING, bounds.bottom().min(window.viewport_size().height) - PADDING);
        let geometry = Geometry { bounds, origin, cell_width, line_height, columns, rows, scroll_edges };
        let cell_origin =
            |row: usize, column: usize| origin + point(cell_width * column as f32, line_height * row as f32);
        let batched = &frame.batched;
        let mut backgrounds = Vec::with_capacity(batched.backgrounds.len() + batched.selection.len());
        let mut background_colors = Vec::with_capacity(backgrounds.capacity());
        for span in batched.backgrounds.iter().chain(&batched.selection) {
            backgrounds
                .push(Bounds::new(cell_origin(span.row, span.column), size(cell_width * span.len as f32, line_height)));
            background_colors.push(span.color);
        }
        let text_system = window.text_system().clone();
        let (base_font, font_size) = terminal_font(&fonts);
        let shape = |text: SharedString, style: &RunStyle| {
            let font = Font {
                weight: if style.bold { FontWeight::BOLD } else { FontWeight::NORMAL },
                style: if style.italic { FontStyle::Italic } else { FontStyle::Normal },
                ..base_font.clone()
            };
            let run = TextRun {
                len: text.len(),
                font,
                color: style.color,
                background_color: None,
                underline: style.underline.then_some(UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
                strikethrough: style.strikethrough.then_some(StrikethroughStyle { thickness: px(1.), color: None }),
            };
            text_system.shape_line(text, font_size, &[run], Some(cell_width))
        };
        let lines: Vec<_> = batched
            .runs
            .iter()
            .map(|run| (cell_origin(run.row, run.column), shape(run.text.clone(), &run.style)))
            .collect();
        let overlines = batched
            .overlines
            .iter()
            .map(|span| {
                let at = cell_origin(span.row, span.column) + point(px(0.), px(1.));
                (Bounds::new(at, size(cell_width * span.len as f32, px(1.))), span.color)
            })
            .collect();
        let cursor = frame.cursor.filter(|_| self.cursor_on).map(|cursor| {
            let at = cell_origin(cursor.at.y, cursor.at.x);
            let width = if cursor.wide { cell_width * 2. } else { cell_width };
            let hollow = !self.focused || cursor.shape == CursorShape::BlockHollow;
            let bounds = match cursor.shape {
                _ if hollow => Bounds::new(at, size(width, line_height)),
                CursorShape::Bar => Bounds::new(at, size(px(2.), line_height)),
                CursorShape::Underline => Bounds::new(at + point(px(0.), line_height - px(2.)), size(width, px(2.))),
                _ => Bounds::new(at, size(width, line_height)),
            };
            // The glyph under a filled block is redrawn in the background color.
            let glyph = (!hollow && cursor.shape == CursorShape::Block)
                .then_some(cursor.glyph)
                .flatten()
                .map(|(text, style)| shape(text, &style));
            Cursor { bounds, hollow, color: cursor.color, glyph }
        });
        let session = self.session.read(cx);
        let link = session.hovered_link();
        let link_underlines = link
            .into_iter()
            .flat_map(|link| link.spans(columns))
            .filter(|(row, _, _)| *row < rows)
            .map(|(row, start, end)| {
                let at = cell_origin(row, start) + point(px(0.), line_height - px(1.));
                Bounds::new(at, size(cell_width * end.saturating_sub(start) as f32, px(1.)))
            })
            .collect();
        session.geometry.set(geometry);
        GridPrepaint {
            hitbox,
            geometry,
            background: frame.batched.background,
            backgrounds,
            background_colors,
            lines,
            overlines,
            cursor,
            link_hovered: link.is_some(),
            link_underlines,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        prepaint: &mut GridPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let cursor = if prepaint.link_hovered { CursorStyle::PointingHand } else { CursorStyle::IBeam };
        window.set_cursor_style(cursor, &prepaint.hitbox);
        let line_height = prepaint.geometry.line_height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            // Cells in the terminal's own background paint no span; a program may have changed it.
            window.paint_quad(fill(bounds, prepaint.background));
            for (bounds, color) in prepaint.backgrounds.iter().zip(&prepaint.background_colors) {
                window.paint_quad(fill(*bounds, *color));
            }
            for (origin, line) in &prepaint.lines {
                let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
            }
            for (bounds, color) in &prepaint.overlines {
                window.paint_quad(fill(*bounds, *color));
            }
            for bounds in &prepaint.link_underlines {
                window.paint_quad(fill(*bounds, self.theme.foreground));
            }
            if let Some(cursor) = &prepaint.cursor {
                if cursor.hollow {
                    window.paint_quad(outline(cursor.bounds, cursor.color, BorderStyle::Solid));
                } else {
                    window.paint_quad(fill(cursor.bounds, cursor.color));
                    if let Some(glyph) = &cursor.glyph {
                        let _ = glyph.paint(cursor.bounds.origin, line_height, TextAlign::Left, None, window, cx);
                    }
                }
            }
        });

        let focus = self.session.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.session.clone()), cx);
        let hitbox = prepaint.hitbox.clone();
        let session = self.session.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase.bubble() && hitbox.is_hovered(window) {
                // A tracking program's right click is its own, so the menu stays shut.
                if session.update(cx, |session, cx| session.mouse_down(event, window, cx)) {
                    cx.stop_propagation();
                }
            }
        });
        let hitbox = prepaint.hitbox.clone();
        let session = self.session.clone();
        let hovering = prepaint.link_hovered;
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            let inside = hitbox.is_hovered(window);
            if phase.bubble() && (inside || hovering || event.pressed_button.is_some()) {
                session.update(cx, |session, cx| session.mouse_move(event, inside, cx));
            }
        });
        let session = self.session.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase.bubble() {
                session.update(cx, |session, cx| session.mouse_up(event, window, cx));
            }
        });
        let hitbox = prepaint.hitbox.clone();
        let session = self.session.clone();
        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
            if phase.bubble() && hitbox.is_hovered(window) {
                session.update(cx, |session, cx| session.scroll_wheel(event, cx));
                cx.stop_propagation();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::core::tests::core;
    use super::*;

    fn theme() -> TerminalTheme {
        TerminalTheme::new(&Tokens::dark())
    }

    fn snapshot(output: &str) -> Snapshot {
        let mut core = core(20, 3);
        core.set_theme(&theme().core());
        core.write(output.as_bytes());
        core.snapshot().clone()
    }

    #[test]
    fn one_run_per_style_and_no_runs_for_blank_cells() {
        let batched = batch(&snapshot("ls \x1b[31mred\x1b[0m  done\r\n\r\n   "), &theme());
        let runs: Vec<(usize, usize, &str)> =
            batched.runs.iter().map(|run| (run.row, run.column, run.text.as_ref())).collect();
        assert_eq!(runs, [(0, 0, "ls"), (0, 3, "red"), (0, 8, "done")]);
        assert_eq!(to_rgb(batched.runs[1].style.color), to_rgb(theme().palette[1]));
        assert!(batched.backgrounds.is_empty());
    }

    #[test]
    fn overlines_span_their_cells_including_spaces() {
        let theme = theme();
        let batched = batch(&snapshot("\x1b[53ma b\x1b[31m c\x1b[0m d \x1b[53m\u{4e2d}"), &theme);
        let spans: Vec<_> = batched.overlines.iter().map(|span| (span.column, span.len, to_rgb(span.color))).collect();
        // "a b" in the text color, " c" in red, then the wide glyph's two cells.
        let (fg, red) = (to_rgb(theme.foreground), to_rgb(theme.palette[1]));
        assert_eq!(spans, [(0, 3, fg), (3, 2, red), (8, 2, fg)]);
    }

    #[test]
    fn backgrounds_merge_and_inverse_swaps_colors() {
        let theme = theme();
        let batched = batch(&snapshot("\x1b[44mab\x1b[7m c\x1b[0m"), &theme);
        let spans: Vec<_> =
            batched.backgrounds.iter().map(|span| (span.column, span.len, to_rgb(span.color))).collect();
        // Blue under "ab", then the inverse cells paint the blue foreground's swap: the default text color.
        assert_eq!(spans, [(0, 2, to_rgb(theme.palette[4])), (2, 2, to_rgb(theme.foreground))]);
        assert_eq!(batched.runs.last().map(|run| to_rgb(run.style.color)), Some(to_rgb(theme.palette[4])));
    }

    #[test]
    fn the_frame_fills_the_terminal_background() {
        let theme = theme();
        assert_eq!(to_rgb(batch(&snapshot("ab"), &theme).background), to_rgb(theme.background));
        // A program's background (OSC 11) and DEC reverse video: default cells paint no span over it.
        for (output, background) in [
            ("\x1b]11;rgb:12/34/56\x07ab", Rgb { r: 0x12, g: 0x34, b: 0x56 }),
            ("\x1b[?5hab", to_rgb(theme.foreground)),
        ] {
            let batched = batch(&snapshot(output), &theme);
            assert_eq!(to_rgb(batched.background), background, "{output:?}");
            assert!(batched.backgrounds.is_empty(), "{output:?}");
        }
        // A later change redraws the rows already shown.
        let mut core = core(20, 3);
        core.set_theme(&theme.core());
        core.write(b"ab");
        let before = core.snapshot().revision;
        core.write(b"\x1b]11;rgb:12/34/56\x07");
        let batched = batch(core.snapshot(), &theme);
        assert!(core.snapshot().revision > before, "the cached batch is rebuilt");
        assert_eq!(to_rgb(batched.background), Rgb { r: 0x12, g: 0x34, b: 0x56 });
        assert!(batched.backgrounds.is_empty());
        // So do palette (OSC 4) and default foreground (OSC 10) changes, for the text on them.
        core.write(b"\x1b[31mr\x1b[0m");
        for (output, color) in [
            (&b"\x1b]4;1;rgb:aa/bb/cc\x07"[..], Rgb { r: 0xaa, g: 0xbb, b: 0xcc }),
            (b"\x1b]10;rgb:11/22/33\x07", Rgb { r: 0x11, g: 0x22, b: 0x33 }),
        ] {
            let before = core.snapshot().revision;
            core.write(output);
            let batched = batch(core.snapshot(), &theme);
            assert!(core.snapshot().revision > before, "{output:?}");
            assert!(batched.runs.iter().any(|run| to_rgb(run.style.color) == color), "{output:?}");
        }
    }

    #[test]
    fn wide_glyphs_end_their_run() {
        let batched = batch(&snapshot("a漢b"), &theme());
        let runs: Vec<(usize, &str)> = batched.runs.iter().map(|run| (run.column, run.text.as_ref())).collect();
        assert_eq!(runs, [(0, "a漢"), (3, "b")]);
    }

    #[test]
    fn invisible_text_and_selection_paint_no_glyphs_but_a_tint() {
        let mut core = core(20, 3);
        core.write(b"\x1b[8msecret\x1b[0m shown");
        let start = core.viewport_to_screen(ViewportPoint { x: 7, y: 0 }).unwrap();
        core.set_selection(start, super::super::model::ScreenPoint { x: 11, ..start });
        let batched = batch(core.snapshot(), &theme());
        let runs: Vec<(usize, &str)> = batched.runs.iter().map(|run| (run.column, run.text.as_ref())).collect();
        assert_eq!(runs, [(7, "shown")]);
        assert_eq!(batched.selection.iter().map(|span| (span.column, span.len)).collect::<Vec<_>>(), [(7, 5)]);
    }

    #[test]
    fn the_block_cursor_redraws_its_own_cell() {
        // "e" plus a combining acute accent, then "x"; the cursor goes back to each cell.
        for (output, glyph) in [("e\u{301}x\r", Some("e\u{301}")), ("e\u{301}x\r\x1b[C", Some("x")), ("ab ", None)] {
            let cursor = CursorCell::of(&snapshot(output)).unwrap();
            assert_eq!(cursor.glyph.map(|(text, _)| text.to_string()).as_deref(), glyph, "{output:?}");
        }
        let cursor = CursorCell::of(&snapshot("漢\x1b[D\x1b[D")).unwrap();
        assert!(cursor.wide && cursor.at == ViewportPoint { x: 0, y: 0 }, "a wide glyph's cursor covers both cells");
        assert_eq!(CursorCell::of(&snapshot("\x1b[?25l")), None, "a hidden cursor draws nothing");
    }

    #[test]
    fn the_theme_round_trips_through_ghostty_colors() {
        let theme = theme();
        let colors = theme.core();
        assert_eq!(to_rgb(hsla(colors.palette[1])), colors.palette[1]);
        assert_eq!(colors.palette[1], Rgb { r: 0xcd, g: 0x31, b: 0x31 });
    }
}
