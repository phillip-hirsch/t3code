//! CSV and TSV tables (`DelimitedTablePreview`, `LargeTextViewer`, `VirtualTablePreview.tsx`):
//! the first row heads the columns, rows are numbered, and only the visible rows render.

use std::ops::Range;
use std::rc::Rc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px, uniform_list,
};

use crate::app::Workspace;
use crate::theme::Tokens;

const ROW_HEIGHT: f32 = 29.;
const ROW_HEADER: f32 = 56.;
/// `LARGE_TEXT_MAX_COLUMNS` and the cell bound of the first screen.
const MAX_COLUMNS: usize = 1024;
const MAX_CELL: usize = 16 * 1024;
const MAX_ROWS: usize = 250_000;

/// A delimited file's cells, parseable off the UI thread.
pub(super) struct Cells {
    header: Vec<SharedString>,
    rows: Vec<Vec<SharedString>>,
    columns: usize,
}

impl Cells {
    /// The first row heads the columns.
    pub(super) fn parse(text: &str, delimiter: char) -> Self {
        let (mut rows, _) = parse_delimited(text, delimiter, MAX_ROWS + 1, MAX_COLUMNS, MAX_CELL);
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let header = if rows.is_empty() { Vec::new() } else { rows.remove(0) };
        let shared = |row: Vec<String>| row.into_iter().map(SharedString::from).collect::<Vec<_>>();
        Self { header: shared(header), rows: rows.into_iter().map(shared).collect(), columns }
    }
}

pub(super) struct Table {
    header: Vec<SharedString>,
    rows: Vec<Vec<SharedString>>,
    columns: usize,
    /// The horizontal scroll, which picks the columns that render (the web's column window).
    scroll: ScrollHandle,
}

/// The columns in view at horizontal `offset` (zero or negative), with one spare each side.
pub(super) fn visible_columns(offset: f32, viewport: f32, column_width: f32, columns: usize) -> Range<usize> {
    let count = (viewport / column_width).ceil() as usize + 3;
    let first = ((-offset - ROW_HEADER).max(0.) / column_width) as usize;
    let first = first.saturating_sub(1).min(columns.saturating_sub(count));
    first..(first + count).min(columns)
}

/// `parseDelimitedPreview`: quoted cells, `""` escapes, CRLF, a leading BOM, and bounds on rows,
/// columns and cell length. Returns the rows and whether anything was cut.
pub(super) fn parse_delimited(
    text: &str,
    delimiter: char,
    max_rows: usize,
    max_columns: usize,
    max_cell: usize,
) -> (Vec<Vec<String>>, bool) {
    // Streamed: a full file can be hundreds of megabytes.
    let mut chars = text.strip_prefix('\u{feff}').unwrap_or(text).chars().peekable();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut cell_len = 0usize;
    let mut quoted = false;
    let mut truncated = false;
    // Characters were read since the last row ended.
    let mut in_row = false;
    let end_cell = |row: &mut Vec<String>, cell: &mut String, cell_len: &mut usize, truncated: &mut bool| {
        if *cell_len > max_cell {
            *cell = cell.chars().take(max_cell).collect();
            *truncated = true;
        }
        if row.len() < max_columns {
            row.push(std::mem::take(cell));
        } else {
            *truncated = true;
            cell.clear();
        }
        *cell_len = 0;
    };
    while let Some(char) = chars.next() {
        in_row = true;
        if char == '"' {
            if quoted && chars.peek() == Some(&'"') {
                chars.next();
                if cell_len <= max_cell {
                    cell.push('"');
                    cell_len += 1;
                } else {
                    truncated = true;
                }
                continue;
            }
            if quoted || cell.is_empty() {
                quoted = !quoted;
                continue;
            }
        }
        if !quoted && (char == delimiter || char == '\n' || char == '\r') {
            end_cell(&mut row, &mut cell, &mut cell_len, &mut truncated);
            if char != delimiter {
                rows.push(std::mem::take(&mut row));
                if char == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                in_row = false;
                if rows.len() == max_rows {
                    return (rows, truncated || chars.peek().is_some());
                }
            }
        } else if cell_len <= max_cell {
            cell.push(char);
            cell_len += 1;
        } else {
            truncated = true;
        }
    }
    if in_row {
        end_cell(&mut row, &mut cell, &mut cell_len, &mut truncated);
        rows.push(row);
    }
    (rows, truncated || quoted)
}

/// `columnLabel`: A, B, … Z, AA, AB, …
pub(super) fn column_label(mut column: usize) -> String {
    let mut label = String::new();
    loop {
        label.insert(0, (b'A' + (column % 26) as u8) as char);
        if column < 26 {
            return label;
        }
        column = column / 26 - 1;
    }
}

impl Table {
    /// A spreadsheet's cells under lettered columns.
    pub(super) fn grid(rows: &[Vec<String>]) -> Self {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let rows = rows.iter().map(|row| row.iter().map(|c| SharedString::from(c.clone())).collect()).collect();
        Self { header: Vec::new(), rows, columns, scroll: ScrollHandle::new() }
    }

    pub(super) fn new(Cells { header, rows, columns }: Cells) -> Self {
        Self { header, rows, columns, scroll: ScrollHandle::new() }
    }

    pub(super) fn parse(text: &str, delimiter: char) -> Self {
        Self::new(Cells::parse(text, delimiter))
    }
}

impl Workspace {
    pub(super) fn render_table(
        &mut self,
        id: &str,
        table: Rc<Table>,
        width: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if table.columns == 0 {
            return div()
                .p(px(24.))
                .text_xs()
                .text_color(tokens.muted_foreground)
                .child("This table is empty.")
                .into_any_element();
        }
        let column_width = ((width - ROW_HEADER) / table.columns.min(6) as f32).clamp(128., 320.);
        let table_width = (ROW_HEADER + column_width * table.columns as f32).max(width);
        let border = tokens.border;
        let visible = visible_columns(f32::from(table.scroll.offset().x), width, column_width, table.columns);
        let spacer = |columns: usize| div().flex_none().w(px(column_width * columns as f32));
        let header = h_flex()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .border_b_1()
            .border_color(border)
            .bg(tokens.muted.opacity(0.6))
            .font_weight(FontWeight::MEDIUM)
            .child(div().w(px(ROW_HEADER)).h_full().flex_none().border_r_1().border_color(border).bg(tokens.background))
            .child(spacer(visible.start))
            .children(visible.map(|column| {
                let label = table.header.get(column).cloned().unwrap_or_else(|| column_label(column).into());
                div()
                    .w(px(column_width))
                    .flex_none()
                    .px(px(12.))
                    .py(px(6.))
                    .border_r_1()
                    .border_color(border.opacity(0.4))
                    .truncate()
                    .child(label)
            }));
        let rows = table.clone();
        let scroll = table.scroll.clone();
        let list = uniform_list(
            ElementId::Name(format!("table:{id}").into()),
            table.rows.len(),
            cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                let tokens = *crate::theme::tokens(cx);
                let visible = visible_columns(f32::from(scroll.offset().x), width, column_width, rows.columns);
                range
                    .map(|index| {
                        let row = &rows.rows[index];
                        h_flex()
                            .h(px(ROW_HEIGHT))
                            .border_b_1()
                            .border_color(tokens.border.opacity(0.6))
                            .when(index % 2 == 1, |el| el.bg(tokens.muted.opacity(0.3)))
                            .child(
                                div()
                                    .w(px(ROW_HEADER))
                                    .h_full()
                                    .flex_none()
                                    .px(px(8.))
                                    .py(px(6.))
                                    .border_r_1()
                                    .border_color(tokens.border)
                                    .bg(tokens.background)
                                    .text_right()
                                    .text_color(tokens.muted_foreground)
                                    .child(SharedString::from((index + 1).to_string())),
                            )
                            .child(div().flex_none().w(px(column_width * visible.start as f32)))
                            .children(visible.clone().map(|column| {
                                div()
                                    .w(px(column_width))
                                    .flex_none()
                                    .px(px(12.))
                                    .py(px(6.))
                                    .truncate()
                                    .child(row.get(column).cloned().unwrap_or_default())
                            }))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1();
        div()
            .id(ElementId::Name(format!("table-scroll:{id}").into()))
            .flex_1()
            .min_h_0()
            .overflow_x_scroll()
            .track_scroll(&table.scroll)
            .text_xs()
            .child(v_flex().w(px(table_width)).h_full().child(header).child(list))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimited_text_parses_like_the_web() {
        let (rows, truncated) = parse_delimited("\u{feff}a,\"b,\"\"c\"\"\"\r\n1,2\n", ',', 100, 30, 2000);
        assert_eq!(rows, [vec!["a".to_owned(), "b,\"c\"".to_owned()], vec!["1".into(), "2".into()]]);
        assert!(!truncated);
        let (rows, truncated) = parse_delimited("a\tb\tc\nd", '\t', 100, 2, 2000);
        assert_eq!(rows, [vec!["a".to_owned(), "b".to_owned()], vec!["d".into()]]);
        assert!(truncated, "columns past the bound are cut");
        let (rows, truncated) = parse_delimited("1\n2\n3\n", ',', 2, 30, 2000);
        assert_eq!(rows.len(), 2);
        assert!(truncated);
        assert!(!parse_delimited("1\n2\r\n", ',', 2, 30, 2000).1, "the bound met at the end cuts nothing");
        assert_eq!(
            parse_delimited("a\n\nb", ',', 10, 10, 10).0,
            [vec!["a".to_owned()], vec!["".into()], vec!["b".into()]]
        );
        assert!(parse_delimited("\"open", ',', 10, 10, 10).1, "an unclosed quote is truncation");
        assert_eq!(parse_delimited("abcdef", ',', 10, 10, 3).0, [vec!["abc".to_owned()]]);
    }

    #[test]
    fn only_the_columns_in_view_render() {
        assert_eq!(visible_columns(0., 600., 128., 1024), 0..8);
        assert_eq!(visible_columns(-(56. + 128. * 10.), 600., 128., 1024), 9..17);
        assert_eq!(visible_columns(-5000., 600., 128., 20), 12..20, "the last columns stay in range");
        assert_eq!(visible_columns(0., 600., 128., 3), 0..3);
    }

    #[test]
    fn columns_are_labeled_like_a_spreadsheet() {
        assert_eq!(column_label(0), "A");
        assert_eq!(column_label(25), "Z");
        assert_eq!(column_label(26), "AA");
        assert_eq!(column_label(701), "ZZ");
        assert_eq!(column_label(702), "AAA");
    }
}
