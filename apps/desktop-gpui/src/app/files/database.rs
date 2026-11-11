//! SQLite databases (`BinaryViewer.tsx` with `kind="sqlite"`): the tables with their row counts,
//! 50 rows of the chosen one at a time, and the read-only notice. `crate::sqlite` reads the
//! database off the UI thread.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, div, px,
};

use super::{FileKey, chrome, logic};
use crate::app::Workspace;
use crate::app::attachments::format_size;
use crate::sqlite::{self, PAGE_SIZE};
use crate::theme::Tokens;

/// `min-w-32` and `max-w-80`: a cell's width bounds; its header may widen it, as it does not wrap.
const MIN_COLUMN: f32 = 128.;
const MAX_COLUMN: f32 = 320.;
/// About one `text-xs` character, to size columns to their text the way a table lays itself out.
const CHAR_WIDTH: f32 = 7.;
/// `px-3` on both sides and the cell's right border.
const CELL_PADDING: f32 = 25.;

/// A page of rows, laid out once when it arrives.
struct Grid {
    columns: Vec<SharedString>,
    rows: Vec<Vec<SharedString>>,
    widths: Vec<f32>,
}

/// The width a column of `header` over `cells` takes, from the longest line of each.
fn column_width<'a>(header: &str, cells: impl Iterator<Item = &'a str>) -> f32 {
    let longest = |text: &str| text.lines().map(|line| line.chars().count()).max().unwrap_or(0) as f32;
    let header = longest(header) * CHAR_WIDTH + CELL_PADDING;
    let cells = cells.map(longest).fold(0., f32::max) * CHAR_WIDTH + CELL_PADDING;
    header.max(cells.min(MAX_COLUMN)).max(MIN_COLUMN)
}

impl Grid {
    fn new(rows: sqlite::Rows) -> Self {
        let widths = (0..rows.columns.len())
            .map(|column| {
                column_width(
                    &rows.columns[column],
                    rows.rows.iter().filter_map(|row| row.get(column)).map(String::as_str),
                )
            })
            .collect();
        Self {
            columns: rows.columns.into_iter().map(SharedString::from).collect(),
            rows: rows.rows.into_iter().map(|row| row.into_iter().map(SharedString::from).collect()).collect(),
            widths,
        }
    }
}

/// `1–50 of 1,234`, or `0 rows` for an empty table; only the total is grouped, as the web's is.
fn page_range(page: usize, count: u64) -> String {
    if count == 0 {
        return "0 rows".into();
    }
    let first = page as u64 * PAGE_SIZE as u64 + 1;
    let last = count.min((page as u64 + 1) * PAGE_SIZE as u64);
    format!("{first}–{last} of {}", logic::grouped(usize::try_from(count).unwrap_or(usize::MAX)))
}

/// Whether a page follows `page` in a table of `count` rows.
fn has_next(page: usize, count: u64) -> bool {
    ((page as u64 + 1) * PAGE_SIZE as u64) < count
}

pub(super) struct Database {
    database: Arc<Mutex<sqlite::Database>>,
    tables: Rc<[sqlite::Table]>,
    size: u64,
    table: usize,
    page: usize,
    /// The page shown, or its failure; `None` while it loads.
    rows: Option<Result<Rc<Grid>, String>>,
    _task: Option<Task<()>>,
}

impl Database {
    pub(super) fn new(database: sqlite::Database, size: u64) -> Self {
        let tables = Rc::from(database.tables());
        Self { database: Arc::new(Mutex::new(database)), tables, size, table: 0, page: 0, rows: None, _task: None }
    }

    /// A page is loading: the table picker and the page buttons wait for it.
    fn busy(&self) -> bool {
        self._task.is_some()
    }
}

impl Workspace {
    fn database_mut(&mut self, key: &FileKey) -> Option<&mut Database> {
        match self.files.assets.viewer_mut(key)? {
            super::asset::Viewer::Database(database) => Some(database),
            _ => None,
        }
    }

    /// Shows page `page` of table `table`, read off the UI thread. A result that arrives after the
    /// view moved to another page or file is dropped.
    pub(super) fn load_database_rows(&mut self, key: &FileKey, table: usize, page: usize, cx: &mut Context<Self>) {
        let Some(view) = self.database_mut(key) else { return };
        let Some(name) = view.tables.get(table).map(|table| table.name.clone()) else { return };
        view.table = table;
        view.page = page;
        view.rows = None;
        let database = view.database.clone();
        let request_key = key.clone();
        view._task = Some(cx.spawn(async move |this, cx| {
            let read = async move {
                let database = database.lock().map_err(|_| "Could not preview this file.".to_owned())?;
                database.rows(&name, page)
            };
            let rows = cx.background_executor().spawn(read).await;
            let _ = this.update(cx, |this, cx| {
                let Some(view) = this.database_mut(&request_key) else { return };
                if (view.table, view.page) != (table, page) {
                    return;
                }
                view._task = None;
                view.rows = Some(rows.map(|rows| Rc::new(Grid::new(rows))));
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub(super) fn render_database(&mut self, key: &FileKey, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(view) = self.database_mut(key) else { return div().into_any_element() };
        let muted = tokens.muted_foreground;
        let Some(table) = view.tables.get(view.table).cloned() else {
            return div()
                .p(px(24.))
                .text_xs()
                .text_color(muted)
                .child("This database has no readable tables.")
                .into_any_element();
        };
        let (busy, page, size) = (view.busy(), view.page, view.size);
        let tables = view.tables.clone();
        let rows = view.rows.clone();
        let current = view.table;
        let picker = self.render_table_picker(key, &tables, current, busy, tokens, cx);
        let (previous_key, next_key) = (key.clone(), key.clone());
        let previous = chrome::action_button(
            "sqlite-previous",
            IconName::ChevronLeft,
            "Previous rows",
            !busy && page > 0,
            tokens,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            let Some(view) = this.database_mut(&previous_key) else { return };
            if !view.busy() && view.page > 0 {
                let (table, page) = (view.table, view.page - 1);
                this.load_database_rows(&previous_key, table, page, cx);
            }
        }));
        let next_enabled = !busy && has_next(page, table.count);
        let next = chrome::action_button("sqlite-next", IconName::ChevronRight, "Next rows", next_enabled, tokens, cx)
            .on_click(cx.listener(move |this, _, _, cx| {
                let Some(view) = this.database_mut(&next_key) else { return };
                let count = view.tables.get(view.table).map_or(0, |table| table.count);
                if !view.busy() && has_next(view.page, count) {
                    let (table, page) = (view.table, view.page + 1);
                    this.load_database_rows(&next_key, table, page, cx);
                }
            }));
        let border = tokens.border.opacity(0.6);
        let toolbar = h_flex()
            .flex_none()
            .flex_wrap()
            .min_h(px(40.))
            .gap(px(8.))
            .px(px(12.))
            .py(px(4.))
            .border_b_1()
            .border_color(border)
            .text_xs()
            .child(div().min_w(px(128.)).flex_1().child(picker))
            .child(previous)
            .child(div().flex_none().child(page_range(page, table.count)))
            .child(next);
        let columns = match &rows {
            Some(Ok(grid)) => logic::grouped(grid.columns.len()),
            _ => "…".into(),
        };
        let status = div()
            .flex_none()
            .px(px(12.))
            .py(px(8.))
            .border_b_1()
            .border_color(border)
            .text_xs()
            .text_color(muted)
            .child(format!(
                "{} rows · {columns} columns · {}",
                logic::grouped(usize::try_from(table.count).unwrap_or(usize::MAX)),
                format_size(size)
            ));
        let notice = chrome::notice(
            "Read-only. Up to 50 columns and 1,024 characters per cell. Blob contents are omitted.",
            tokens,
        );
        let body = match rows {
            None => chrome::loading(tokens),
            Some(Err(message)) => {
                let retry = key.clone();
                let button =
                    chrome::retry_button("sqlite-retry", tokens).on_click(cx.listener(move |this, _, _, cx| {
                        this.files.assets.forget(&retry);
                        cx.notify();
                    }));
                chrome::failure(message, Some(button), tokens)
            }
            Some(Ok(grid)) => render_grid(&grid, &format!("{}:{}:{page}", key.path, table.name), tokens),
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(toolbar)
            .child(status)
            .child(notice)
            .child(body)
            .into_any_element()
    }

    /// `SelectTrigger size="xs"` over the tables, each with its row count; it waits while a page loads.
    fn render_table_picker(
        &self,
        key: &FileKey,
        tables: &Rc<[sqlite::Table]>,
        current: usize,
        busy: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = |table: &sqlite::Table| {
            SharedString::from(format!(
                "{} ({} rows)",
                table.name,
                logic::grouped(usize::try_from(table.count).unwrap_or(usize::MAX))
            ))
        };
        let selected = label(&tables[current]);
        let trigger = Button::new("sqlite-table")
            .ghost()
            .w_full()
            .min_w(px(144.))
            .h(px(tokens.breakpoint.pick(28., 24.)))
            .px(px(7.))
            .rounded(px(6.))
            .border_1()
            .border_color(tokens.input)
            .bg(if tokens.dark { tokens.input.opacity(0.32) } else { tokens.background })
            .font_weight(FontWeight::NORMAL)
            .text_color(tokens.foreground)
            .map(|trigger| if tokens.breakpoint.pick(false, true) { trigger.text_xs() } else { trigger.text_sm() })
            .disabled(busy)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap(px(4.))
                    .child(div().min_w_0().truncate().child(selected))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(14.))
                            .mr(px(-4.))
                            .flex_none()
                            .text_color(tokens.muted_foreground.opacity(0.7)),
                    ),
            );
        let (workspace, tables, key) = (cx.entity(), tables.clone(), key.clone());
        trigger
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopLeft, move |mut menu, window, _| {
                for (index, table) in tables.iter().enumerate() {
                    let key = key.clone();
                    menu = menu.item(PopupMenuItem::new(label(table)).checked(index == current).on_click(
                        window.listener_for(&workspace, move |this, _, _, cx| {
                            if this.database_mut(&key).is_some_and(|view| !view.busy()) {
                                this.load_database_rows(&key, index, 0, cx);
                            }
                        }),
                    ));
                }
                menu
            })
            .into_any_element()
    }
}

/// The page's table: its column names over the rows, striped, scrolling both ways under a header
/// that stays in view.
fn render_grid(grid: &Grid, id: &str, tokens: &Tokens) -> AnyElement {
    let border = tokens.border.opacity(0.6);
    let total: f32 = grid.widths.iter().sum();
    let last = grid.widths.len().saturating_sub(1);
    // The last column takes what is left of a wider panel, as a `min-w-full` table stretches.
    let cell = |column: usize, width: f32| {
        div()
            .flex_none()
            .w(px(width))
            .when(column == last, |cell| cell.flex_1().min_w(px(width)))
            .px(px(12.))
            .border_r_1()
            .border_b_1()
            .border_color(border)
    };
    let header = div().flex().flex_none().w_full().bg(tokens.background).font_weight(FontWeight::MEDIUM).children(
        grid.columns.iter().enumerate().map(|(column, name)| {
            cell(column, grid.widths[column]).py(px(8.)).whitespace_nowrap().overflow_hidden().child(name.clone())
        }),
    );
    let rows = grid.rows.iter().enumerate().map(|(index, row)| {
        div().flex().w_full().when(index % 2 == 1, |row| row.bg(tokens.muted.opacity(0.3))).children(
            grid.widths.iter().enumerate().map(|(column, width)| {
                cell(column, *width).py(px(6.)).child(row.get(column).cloned().unwrap_or_default())
            }),
        )
    });
    let empty = grid
        .rows
        .is_empty()
        .then(|| div().p(px(24.)).text_xs().text_color(tokens.muted_foreground).child("This table is empty."));
    div()
        .id(ElementId::Name(format!("sqlite-x:{id}").into()))
        .flex_1()
        .min_h_0()
        .overflow_x_scroll()
        .text_xs()
        .child(
            v_flex().min_w_full().w(px(total)).h_full().child(header).child(
                div()
                    .id(ElementId::Name(format!("sqlite-y:{id}").into()))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows)
                    .children(empty),
            ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_bar_counts_rows_like_the_web() {
        assert_eq!(page_range(0, 0), "0 rows");
        assert_eq!(page_range(0, 7), "1–7 of 7");
        assert_eq!(page_range(1, 1234), "51–100 of 1,234");
        assert_eq!(page_range(24, 1234), "1201–1234 of 1,234");
        assert!(!has_next(0, 0));
        assert!(!has_next(0, 50));
        assert!(has_next(0, 51));
        assert!(!has_next(1, 100));
    }

    #[test]
    fn columns_fit_their_text_within_the_cell_bounds() {
        // Whole pixels, so the bounds compare exactly.
        #[expect(clippy::cast_possible_truncation, reason = "widths are a few hundred pixels")]
        let width = |header: &str, cell: &str| column_width(header, std::iter::once(cell)).round() as i32;
        assert_eq!(width("id", "1"), 128, "narrow columns keep the minimum");
        assert_eq!(width("id", &"x".repeat(200)), 320, "cells wrap past 320px");
        let header = "a_very_long_column_name_that_does_not_wrap_at_all";
        assert!(width(header, "1") > 320, "headers never wrap");
        let wide = width("n", "a much longer second line here");
        assert!(wide > 128, "a long cell widens its column");
        assert_eq!(width("n", "short\na much longer second line here"), wide, "a cell is as wide as its longest line");
    }
}
