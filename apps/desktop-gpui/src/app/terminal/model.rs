//! The terminal's visible screen as plain data (`GhosttySnapshot` in the web's Ghostty core),
//! and the link under a cell (`terminalLinkAtPositionWithRange` in its surface).

use super::links::{self, LinkKind};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

/// How a cell takes part in a wide (two-column) glyph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Width {
    #[default]
    Narrow,
    Wide,
    /// The second column of a wide glyph.
    SpacerTail,
    /// The last column of a row whose wide glyph wrapped to the next row.
    SpacerHead,
}

/// One cell with its colors resolved: inverse already swapped and faint already blended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Cell {
    /// The grapheme cluster; empty for a blank cell or a spacer.
    pub text: String,
    pub wide: Width,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: bool,
    pub selected: bool,
}

impl Cell {
    /// `terminalCellText`: spacers add nothing, blanks read as a space.
    fn link_text(&self) -> Option<&str> {
        match self.wide {
            Width::SpacerTail | Width::SpacerHead => None,
            _ if self.text.is_empty() => Some(" "),
            _ => Some(&self.text),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Row {
    pub cells: Vec<Cell>,
    /// The row continues the soft-wrapped row above it.
    pub wrap_continuation: bool,
    /// The row soft-wraps into the row below it.
    pub wraps_to_next: bool,
}

impl Row {
    /// `terminalRowText`.
    fn text(&self, trim_end: bool) -> String {
        let text: String = self.cells.iter().filter_map(Cell::link_text).collect();
        if trim_end { text.trim_end().to_owned() } else { text }
    }

    /// `terminalColumnOffset`: the char offset where `column` starts in [`Row::text`].
    fn column_offset(&self, column: usize) -> usize {
        (0..column).map(|i| self.cells.get(i).map_or(1, |cell| cell.link_text().map_or(0, |t| t.chars().count()))).sum()
    }

    /// `terminalColumnAtOffset`.
    fn column_at_offset(&self, offset: usize) -> usize {
        (0..self.cells.len())
            .find(|&column| offset < self.column_offset(column + 1))
            .unwrap_or(self.cells.len().saturating_sub(1))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum CursorShape {
    Bar,
    #[default]
    Block,
    Underline,
    BlockHollow,
}

/// What a frame draws (`GhosttySnapshot`). Rows are rebuilt only where Ghostty marks them dirty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Snapshot {
    pub cols: usize,
    pub rows: Vec<Row>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor_color: Rgb,
    /// The cursor's cell, when it is visible and inside the viewport.
    pub cursor: Option<ViewportPoint>,
    pub cursor_blinking: bool,
    pub cursor_shape: CursorShape,
    /// Bumped whenever a row changes, so painters can cache what they built from it.
    pub revision: u64,
}

/// A viewport cell: column `x` of visible row `y`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ViewportPoint {
    pub x: usize,
    pub y: usize,
}

/// A cell of the whole screen: column `x` of row `y` counted from the top of the scrollback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ScreenPoint {
    pub y: usize,
    pub x: usize,
}

/// A link on screen: its text and its first and last cells (inclusive).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ScreenLink {
    pub kind: LinkKind,
    pub text: String,
    pub start: ViewportPoint,
    pub end: ViewportPoint,
}

impl ScreenLink {
    /// The cells on each row, as `(row, first column, one past the last)`.
    pub fn spans(&self, columns: usize) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        (self.start.y..=self.end.y).map(move |y| {
            let first = if y == self.start.y { self.start.x } else { 0 };
            let last = if y == self.end.y { self.end.x + 1 } else { columns };
            (y, first, last)
        })
    }
}

/// One row of a soft-wrapped line (`WrappedTerminalLinkLineSegment`), in chars.
struct Segment {
    row: usize,
    start: usize,
    end: usize,
}

/// `collectWrappedTerminalLinkLine` over the visible rows.
fn wrapped_line(rows: &[Row], row: usize) -> Option<(String, Vec<Segment>)> {
    rows.get(row)?;
    let mut first = row;
    while first > 0 && rows[first].wrap_continuation {
        first -= 1;
    }
    let (mut text, mut segments) = (String::new(), Vec::new());
    for current in first..rows.len() {
        let continues = rows.get(current + 1).is_some_and(|next| next.wrap_continuation);
        let part = rows[current].text(!continues);
        let start = text.chars().count();
        text.push_str(&part);
        segments.push(Segment { row: current, start, end: start + part.chars().count() });
        if !continues {
            break;
        }
    }
    Some((text, segments))
}

/// `terminalLinkAtPositionWithRange`: the URL or path under the cell, read from the visible
/// rows only, so a link cut off at the viewport's top or bottom edge is not offered.
pub(super) fn link_at(rows: &[Row], row: usize, column: usize) -> Option<ScreenLink> {
    let (text, segments) = wrapped_line(rows, row)?;
    if rows[segments.first()?.row].wrap_continuation {
        return None;
    }
    let segment = segments.iter().find(|segment| segment.row == row)?;
    let current = &rows[row];
    let continues_below = rows[segments.last()?.row].wraps_to_next;
    let hit = match current.cells.get(column) {
        Some(cell) if cell.wide == Width::SpacerTail => column.saturating_sub(1),
        _ => column,
    };
    let offset = segment.start + current.column_offset(hit);
    let length = text.chars().count();
    let found = links::extract(&text).into_iter().find(|m| m.start <= offset && offset < m.end)?;
    if found.end == length && continues_below {
        return None;
    }
    let holding = |at: usize| segments.iter().find(|segment| segment.start <= at && at < segment.end);
    let (start_segment, end_segment) = (holding(found.start)?, holding(found.end - 1)?);
    let (start_row, end_row) = (&rows[start_segment.row], &rows[end_segment.row]);
    let end_column = end_row.column_at_offset(found.end - 1 - end_segment.start);
    let end_x = match end_row.cells.get(end_column + 1) {
        Some(cell) if cell.wide == Width::SpacerTail => end_column + 1,
        _ => end_column,
    };
    Some(ScreenLink {
        kind: found.kind,
        text: found.text,
        start: ViewportPoint { x: start_row.column_at_offset(found.start - start_segment.start), y: start_segment.row },
        end: ViewportPoint { x: end_x, y: end_segment.row },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(text: &str) -> Cell {
        Cell { text: text.into(), ..Cell::default() }
    }

    fn row(text: &str, width: usize, wrap_continuation: bool, wraps_to_next: bool) -> Row {
        let mut cells: Vec<Cell> = text.chars().map(|c| cell(&c.to_string())).collect();
        cells.resize_with(width.max(cells.len()), Cell::default);
        Row { cells, wrap_continuation, wraps_to_next }
    }

    fn text_at(rows: &[Row], y: usize, x: usize) -> Option<String> {
        link_at(rows, y, x).map(|link| link.text)
    }

    fn at(x: usize, y: usize) -> ViewportPoint {
        ViewportPoint { x, y }
    }

    /// Lays `text` out as the terminal does: wide glyphs take two cells, and one that does not
    /// fit leaves a spacer head before the row wraps.
    fn screen(text: &str, columns: usize) -> Vec<Row> {
        let mut rows = vec![Row::default()];
        for c in text.chars() {
            let width = if matches!(c, '日' | '本' | '🙂') { 2 } else { 1 };
            if rows.last().unwrap().cells.len() + width > columns {
                let last = rows.last_mut().unwrap();
                last.wraps_to_next = true;
                while last.cells.len() < columns {
                    last.cells.push(Cell { wide: Width::SpacerHead, ..Cell::default() });
                }
                rows.push(Row { wrap_continuation: true, ..Row::default() });
            }
            let cells = &mut rows.last_mut().unwrap().cells;
            cells.push(Cell { wide: if width == 2 { Width::Wide } else { Width::Narrow }, ..cell(&c.to_string()) });
            if width == 2 {
                cells.push(Cell { wide: Width::SpacerTail, ..Cell::default() });
            }
        }
        for row in &mut rows {
            row.cells.resize_with(columns, Cell::default);
        }
        rows
    }

    #[test]
    fn every_cell_of_a_link_with_wide_glyphs_is_clickable() {
        for (text, columns, end) in [
            ("https://example.com/日本", 80, at(23, 0)),
            ("src/日本/main.ts:3:1", 80, at(19, 0)),
            ("https://example.com/🙂", 80, at(21, 0)),
            ("https://example.com/日本", 23, at(1, 1)),
        ] {
            let rows = screen(&format!("{text} next"), columns);
            for y in 0..=end.y {
                let last = if y == end.y { end.x } else { columns - 2 };
                for x in 0..=last {
                    let link = link_at(&rows, y, x).unwrap_or_else(|| panic!("{text} at {x},{y}"));
                    assert_eq!((link.text.as_str(), link.start, link.end), (text, at(0, 0), end), "{text} at {x},{y}");
                }
            }
            assert_eq!(link_at(&rows, end.y, end.x + 1), None, "{text}");
        }
    }

    #[test]
    fn a_wrapped_quoted_windows_path_activates_from_its_spaces_and_position() {
        let (first, second) = (r#""C:\Users\Jane "#, r#"Doe\index.ts":12:7"#);
        let rows = [row(first, 0, false, false), row(second, 0, true, false)];
        let expected = ScreenLink {
            kind: LinkKind::Path,
            text: r"C:\Users\Jane Doe\index.ts:12:7".into(),
            start: at(1, 0),
            end: at(second.len() - 1, 1),
        };
        assert_eq!(link_at(&rows, 0, first.len() - 1), Some(expected.clone()));
        assert_eq!(link_at(&rows, 1, second.len() - 1), Some(expected));
        assert_eq!(link_at(&rows, 0, 0), None);
    }

    #[test]
    fn cells_after_a_wide_emoji_map_to_their_chars() {
        let mut cells = vec![Cell { wide: Width::Wide, ..cell("🙂") }, Cell { wide: Width::SpacerTail, ..cell("") }];
        cells.extend("https://t3.codes".chars().map(|c| cell(&c.to_string())));
        let last = cells.len() - 1;
        let rows = [Row { cells, wrap_continuation: false, wraps_to_next: false }];
        assert_eq!(text_at(&rows, 0, 2).as_deref(), Some("https://t3.codes"));
        assert_eq!(text_at(&rows, 0, last).as_deref(), Some("https://t3.codes"));
        assert_eq!(text_at(&rows, 0, 0), None);
        let link = link_at(&rows, 0, 8).unwrap();
        assert_eq!((link.start, link.end), (at(2, 0), at(last, 0)));
    }

    #[test]
    fn soft_wrapped_links_and_prompt_paths_rebuild_from_their_rows() {
        let rows = [
            row("https://example.", 16, false, false),
            row("com/reference", 16, true, false),
            row("~/project/file", 16, false, false),
            row(r"C:\repo\file.ts", 16, false, false),
            row(r"PS C:\repo\dir 1", 16, false, true),
            row(r"\test> ", 16, true, false),
        ];
        assert_eq!(text_at(&rows, 0, 8).as_deref(), Some("https://example.com/reference"));
        assert_eq!(text_at(&rows, 2, 2).as_deref(), Some("~/project/file"));
        assert_eq!(text_at(&rows, 3, 4).as_deref(), Some(r"C:\repo\file.ts"));
        let prompt = link_at(&rows, 4, 13).unwrap();
        assert_eq!((prompt.text.as_str(), prompt.start, prompt.end), (r"C:\repo\dir 1\test", at(3, 4), at(4, 5)));
        assert_eq!(text_at(&rows, 5, 2).as_deref(), Some(r"C:\repo\dir 1\test"));
        assert_eq!(text_at(&rows, 5, 5), None);
        let url = link_at(&rows, 1, 4).unwrap();
        assert_eq!((url.start, url.end), (at(0, 0), at(12, 1)));
        assert_eq!(url.spans(16).collect::<Vec<_>>(), [(0, 0, 16), (1, 0, 13)]);
    }

    #[test]
    fn links_cut_off_at_the_viewport_edges_are_refused() {
        let head_cut = [row("ple.com/missing", 16, true, false), row("head", 16, true, false)];
        assert_eq!(text_at(&head_cut, 0, 4), None);
        assert_eq!(text_at(&[row("https://t3.codes", 16, false, true)], 0, 8), None);
        let complete = [row("https://t3.codes", 16, false, false), row("", 16, false, false)];
        assert_eq!(text_at(&complete, 0, 8).as_deref(), Some("https://t3.codes"));
        let mut cells = vec![Cell { wide: Width::Wide, ..cell("🙂") }, Cell { wide: Width::SpacerTail, ..cell("") }];
        cells.extend("https://t3.code".chars().map(|c| cell(&c.to_string())));
        assert_eq!(text_at(&[Row { cells, wrap_continuation: false, wraps_to_next: true }], 0, 8), None);
        assert_eq!(text_at(&[row("https://t3.codes", 18, false, false)], 0, 8).as_deref(), Some("https://t3.codes"));
    }
}
