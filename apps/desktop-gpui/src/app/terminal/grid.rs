//! Paints one terminal's visible grid. Each row becomes a few shaped text
//! runs (consecutive cells of one style) and merged background rects, so a
//! frame costs a handful of draw calls per row rather than one per cell.

use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line, Point as GridPoint};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{RenderableContent, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use gpui_kit::{
    App, BorderStyle, Bounds, ContentMask, CursorStyle, Element, ElementId, ElementInputHandler, Entity, Font,
    FontStyle, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent, ShapedLine,
    StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle, Window, fill, font, hsla, outline, point, px,
    relative, rgb, size,
};

use super::session::TerminalSession;
use crate::theme::{self, Fonts, Tokens};

/// `CONTENT_PADDING` in `apps/web/src/terminal/ghostty/surface.ts`.
pub(super) const PADDING: Pixels = px(4.);

/// `terminalThemeFromApp` in `ThreadTerminalDrawer.tsx` over the default theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TerminalTheme {
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
        // `--terminal-cursor` and `--terminal-selection-background` (`index.css:1184-1227`).
        let (cursor, selection) =
            if tokens.dark { ((180, 203, 255), (180, 203, 255, 0.25)) } else { ((38, 56, 78), (37, 63, 99, 0.2)) };
        Self {
            background: tokens.background,
            foreground: tokens.foreground,
            cursor: rgb8(cursor.0, cursor.1, cursor.2, 1.),
            selection: rgb8(selection.0, selection.1, selection.2, selection.3),
            palette: palette.map(|value| rgb(value).into()),
        }
    }

    fn resolve(&self, color: Color, colors: &alacritty_terminal::term::color::Colors) -> Hsla {
        match color {
            Color::Spec(Rgb { r, g, b }) => rgb8(r, g, b, 1.),
            Color::Indexed(index) => match colors[index as usize] {
                Some(Rgb { r, g, b }) => rgb8(r, g, b, 1.),
                None => self.indexed(index),
            },
            Color::Named(named) => {
                if let Some(Rgb { r, g, b }) = colors[named] {
                    return rgb8(r, g, b, 1.);
                }
                match named {
                    NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
                    NamedColor::Background => self.background,
                    NamedColor::Cursor => self.cursor,
                    NamedColor::DimForeground => dim(self.foreground),
                    named if (named as usize) < 16 => self.palette[named as usize],
                    // DimBlack..DimWhite follow DimForeground (index 259 onwards).
                    named => dim(self.palette[(named as usize).saturating_sub(NamedColor::DimBlack as usize) % 8]),
                }
            }
        }
    }

    /// xterm's 256-color table above the 16 theme colors.
    fn indexed(&self, index: u8) -> Hsla {
        match index {
            0..=15 => self.palette[index as usize],
            16..=231 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { v * 40 + 55 };
                rgb8(level(i / 36), level((i / 6) % 6), level(i % 6), 1.)
            }
            _ => {
                let v = (index - 232) * 10 + 8;
                rgb8(v, v, v, 1.)
            }
        }
    }
}

fn rgb8(r: u8, g: u8, b: u8, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into();
    color.a = alpha;
    color
}

fn dim(color: Hsla) -> Hsla {
    hsla(color.h, color.s, color.l * 0.66, color.a)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RunStyle {
    pub color: Hsla,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

/// Cells of one style on one row, starting at `column`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Run {
    pub row: usize,
    pub column: usize,
    pub text: String,
    pub style: RunStyle,
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
    pub runs: Vec<Run>,
    pub backgrounds: Vec<Span>,
    pub selection: Vec<Span>,
}

/// Groups the visible cells into text runs and background spans.
/// A wide glyph ends its run, so every glyph after it still starts on its own cell.
pub(super) fn batch(content: RenderableContent<'_>, columns: usize, rows: usize, theme: &TerminalTheme) -> Batched {
    let mut out = Batched::default();
    let offset = content.display_offset as i32;
    let colors = content.colors;
    let mut run: Option<Run> = None;
    let mut wide = false;
    for indexed in content.display_iter {
        let cell = indexed.cell;
        let row = (indexed.point.line.0 + offset) as usize;
        let column = indexed.point.column.0;
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let mut fg = theme.resolve(cell.fg, colors);
        let mut bg = theme.resolve(cell.bg, colors);
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::DIM) {
            fg = dim(fg);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        let cells = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        if bg != theme.background {
            match out.backgrounds.last_mut() {
                Some(span) if span.row == row && span.column + span.len == column && span.color == bg => {
                    span.len += cells
                }
                _ => out.backgrounds.push(Span { row, column, len: cells, color: bg }),
            }
        }
        let style = RunStyle {
            color: fg,
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            strikethrough: cell.flags.contains(Flags::STRIKEOUT),
        };
        let blank = cell.c == ' ' || cell.c == '\t';
        let continues = !wide && run.as_ref().is_some_and(|run| run.row == row && run.style == style);
        if !continues {
            flush(&mut run, &mut out.runs);
            if blank && !style.underline && !style.strikethrough {
                wide = false;
                continue;
            }
            run = Some(Run { row, column, text: String::new(), style });
        }
        if let Some(run) = &mut run {
            run.text.push(if blank { ' ' } else { cell.c });
            if let Some(zero_width) = cell.zerowidth() {
                run.text.extend(zero_width);
            }
        }
        wide = cells == 2;
    }
    flush(&mut run, &mut out.runs);
    if let Some(range) = content.selection {
        for row in 0..rows {
            let line = Line(row as i32 - offset);
            if line < range.start.line || line > range.end.line {
                continue;
            }
            let (start, end) = if range.is_block {
                (range.start.column.0, range.end.column.0)
            } else {
                let start = if line == range.start.line { range.start.column.0 } else { 0 };
                let end = if line == range.end.line { range.end.column.0 } else { columns - 1 };
                (start, end)
            };
            if end >= start {
                out.selection.push(Span { row, column: start, len: end - start + 1, color: theme.selection });
            }
        }
    }
    out
}

fn flush(run: &mut Option<Run>, runs: &mut Vec<Run>) {
    if let Some(mut run) = run.take() {
        let trimmed = run.text.trim_end_matches(' ').len();
        if trimmed > 0 || run.style.underline || run.style.strikethrough {
            if !run.style.underline && !run.style.strikethrough {
                run.text.truncate(trimmed);
            }
            runs.push(run);
        }
    }
}

/// Cell size and where the grid starts, in window coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Geometry {
    pub origin: Point<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub columns: usize,
    pub rows: usize,
}

impl Geometry {
    /// The grid cell under `position` (clamped) and whether it is in the cell's right half.
    pub fn cell_at(&self, position: Point<Pixels>, display_offset: usize) -> (GridPoint, bool) {
        let x = ((position.x - self.origin.x) / self.cell_width).max(0.);
        let y = ((position.y - self.origin.y) / self.line_height).max(0.);
        let column = (x as usize).min(self.columns.saturating_sub(1));
        let row = (y as usize).min(self.rows.saturating_sub(1));
        let right = x.fract() >= 0.5 || x as usize >= self.columns;
        (GridPoint::new(Line(row as i32 - display_offset as i32), Column(column)), right)
    }
}

/// The Terminal font setting, which follows the code font unless set separately.
fn terminal_font(fonts: &Fonts) -> (Font, Pixels) {
    (font(fonts.terminal), px(fonts.terminal_size))
}

/// `measureGhosttyCell`: the advance of "M", and 1.35× the font size or the glyph height.
pub(super) fn cell_size(window: &Window, fonts: &Fonts) -> (Pixels, Pixels) {
    let text_system = window.text_system();
    let (font, font_size) = terminal_font(fonts);
    let font_id = text_system.resolve_font(&font);
    let width = text_system.advance(font_id, font_size, 'M').map(|size| size.width).unwrap_or(px(7.));
    let glyph = text_system.ascent(font_id, font_size) + text_system.descent(font_id, font_size).abs();
    let height = (f32::from(font_size) * 1.35).round().max(f32::from(glyph).ceil());
    (width, px(height))
}

pub(super) struct TerminalGrid {
    pub session: Entity<TerminalSession>,
    pub theme: TerminalTheme,
    pub focused: bool,
}

impl IntoElement for TerminalGrid {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct Cursor {
    bounds: Bounds<Pixels>,
    shape: CursorShape,
    glyph: Option<ShapedLine>,
}

pub(super) struct GridPrepaint {
    hitbox: Hitbox,
    geometry: Geometry,
    backgrounds: Vec<Bounds<Pixels>>,
    background_colors: Vec<Hsla>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<Cursor>,
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
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> GridPrepaint {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let fonts = theme::tokens(cx).fonts;
        let (cell_width, line_height) = cell_size(window, &fonts);
        // `terminalGridSize`.
        let columns = (((bounds.size.width - PADDING * 2.) / cell_width).floor() as usize).max(1);
        let rows = (((bounds.size.height - PADDING * 2.) / line_height).floor() as usize).max(1);
        self.session.update(cx, |session, cx| session.fit(columns, rows, cx));
        let session = self.session.read(cx);
        let term = session.term();
        // `terminalContentOriginY`: with scrollback, the last row sits on the bottom edge.
        let slack = bounds.size.height - PADDING * 2. - line_height * rows as f32;
        let top = if term.grid().history_size() > 0 { PADDING + slack.max(px(0.)) } else { PADDING };
        let origin = bounds.origin + point(PADDING, top);
        let geometry = Geometry { origin, cell_width, line_height, columns, rows };
        let content = term.renderable_content();
        let cursor_point = content.cursor.point;
        let cursor_shape = content.cursor.shape;
        let display_offset = content.display_offset;
        let show_cursor = display_offset == 0 && !content.mode.contains(TermMode::VI);
        let batched = batch(content, term.columns(), term.screen_lines(), &self.theme);
        let cell_origin =
            |row: usize, column: usize| origin + point(cell_width * column as f32, line_height * row as f32);
        let mut backgrounds = Vec::with_capacity(batched.backgrounds.len() + batched.selection.len());
        let mut background_colors = Vec::with_capacity(backgrounds.capacity());
        for span in batched.backgrounds.iter().chain(&batched.selection) {
            backgrounds
                .push(Bounds::new(cell_origin(span.row, span.column), size(cell_width * span.len as f32, line_height)));
            background_colors.push(span.color);
        }
        let text_system = window.text_system().clone();
        let (base_font, font_size) = terminal_font(&fonts);
        let shape = |text: String, style: &RunStyle| {
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
            text_system.shape_line(text.into(), font_size, &[run], Some(cell_width))
        };
        let mut lines = Vec::with_capacity(batched.runs.len());
        let mut cursor_glyph = None;
        let cursor_row = cursor_point.line.0 + display_offset as i32;
        for run in batched.runs {
            if self.focused && show_cursor && run.row as i32 == cursor_row && cursor_shape == CursorShape::Block {
                // The glyph under a filled block cursor is redrawn in the background color.
                let at = cursor_point.column.0.checked_sub(run.column);
                if let Some(ch) = at.and_then(|at| run.text.chars().nth(at)).filter(|c| *c != ' ') {
                    let style = RunStyle { color: self.theme.background, ..run.style };
                    cursor_glyph = Some(shape(ch.to_string(), &style));
                }
            }
            let at = cell_origin(run.row, run.column);
            lines.push((at, shape(run.text, &run.style)));
        }
        let cursor = (show_cursor && cursor_shape != CursorShape::Hidden && cursor_row >= 0).then(|| {
            let at = cell_origin(cursor_row as usize, cursor_point.column.0);
            let wide = term.grid()[cursor_point].flags.contains(Flags::WIDE_CHAR);
            let width = if wide { cell_width * 2. } else { cell_width };
            let bounds = match cursor_shape {
                CursorShape::Beam => Bounds::new(at, size(px(2.), line_height)),
                CursorShape::Underline => Bounds::new(at + point(px(0.), line_height - px(2.)), size(width, px(2.))),
                _ => Bounds::new(at, size(width, line_height)),
            };
            let shape = if self.focused { cursor_shape } else { CursorShape::HollowBlock };
            Cursor { bounds, shape, glyph: cursor_glyph }
        });
        session.geometry.set(geometry);
        GridPrepaint { hitbox, geometry, backgrounds, background_colors, lines, cursor }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut GridPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.set_cursor_style(CursorStyle::IBeam, &prepaint.hitbox);
        let line_height = prepaint.geometry.line_height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (bounds, color) in prepaint.backgrounds.iter().zip(&prepaint.background_colors) {
                window.paint_quad(fill(*bounds, *color));
            }
            for (origin, line) in &prepaint.lines {
                let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
            }
            if let Some(cursor) = &prepaint.cursor {
                if cursor.shape == CursorShape::HollowBlock {
                    window.paint_quad(outline(cursor.bounds, self.theme.cursor, BorderStyle::Solid));
                } else {
                    window.paint_quad(fill(cursor.bounds, self.theme.cursor));
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
                session.update(cx, |session, cx| session.mouse_down(event, window, cx));
            }
        });
        let session = self.session.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase.bubble() && event.pressed_button == Some(MouseButton::Left) {
                session.update(cx, |session, cx| session.mouse_drag(event, cx));
            }
        });
        let session = self.session.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase.bubble() && event.button == MouseButton::Left {
                session.update(cx, |session, _| session.mouse_up());
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
    use alacritty_terminal::Term;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    use super::*;

    struct Size(usize, usize);

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.1
        }
        fn screen_lines(&self) -> usize {
            self.1
        }
        fn columns(&self) -> usize {
            self.0
        }
    }

    fn term(output: &str) -> Term<VoidListener> {
        let mut term = Term::new(Config::default(), &Size(20, 3), VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, output.as_bytes());
        term
    }

    fn theme() -> TerminalTheme {
        TerminalTheme::new(&Tokens::dark())
    }

    #[test]
    fn one_run_per_style_and_no_runs_for_blank_cells() {
        let term = term("ls \x1b[31mred\x1b[0m  done\r\n\r\n   ");
        let batched = batch(term.renderable_content(), 20, 3, &theme());
        let runs: Vec<(usize, usize, &str)> =
            batched.runs.iter().map(|run| (run.row, run.column, run.text.as_str())).collect();
        assert_eq!(runs, [(0, 0, "ls"), (0, 3, "red"), (0, 8, "done")]);
        assert_eq!(batched.runs[1].style.color, theme().palette[1]);
        assert!(batched.backgrounds.is_empty());
    }

    #[test]
    fn backgrounds_merge_and_inverse_swaps_colors() {
        let term = term("\x1b[44mab\x1b[7m c\x1b[0m");
        let batched = batch(term.renderable_content(), 20, 3, &theme());
        let theme = theme();
        // Blue under "ab", then the inverse cells paint the foreground as their background.
        assert_eq!(
            batched.backgrounds,
            [
                Span { row: 0, column: 0, len: 2, color: theme.palette[4] },
                Span { row: 0, column: 2, len: 2, color: theme.foreground },
            ]
        );
        assert_eq!(batched.runs.last().map(|run| run.style.color), Some(theme.palette[4]));
    }

    #[test]
    fn wide_glyphs_end_their_run() {
        let term = term("a漢b");
        let batched = batch(term.renderable_content(), 20, 3, &theme());
        let runs: Vec<(usize, &str)> = batched.runs.iter().map(|run| (run.column, run.text.as_str())).collect();
        assert_eq!(runs, [(0, "a漢"), (3, "b")]);
    }

    #[test]
    fn indexed_colors_follow_the_xterm_table() {
        let theme = theme();
        assert_eq!(theme.indexed(16), rgb8(0, 0, 0, 1.));
        assert_eq!(theme.indexed(196), rgb8(255, 0, 0, 1.));
        assert_eq!(theme.indexed(244), rgb8(128, 128, 128, 1.));
    }
}
