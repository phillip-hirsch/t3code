//! Pointer input over one terminal: selection, link hover and clicks
//! (`terminalLinkAtPositionWithRange` in the web's Ghostty surface), mouse reports for
//! apps that track the mouse, and the wheel.

use std::collections::HashMap;

use alacritty_terminal::Term;
use alacritty_terminal::grid::{Dimensions as _, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use gpui_kit::{
    Context, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent, Task, Window,
};
use serde_json::json;

use super::super::links::{self, LinkKind};
use super::super::mouse::{self, MouseAction};
use super::{Listener, SessionEvent, TerminalSession};

/// `TERMINAL_LINK_DRAG_THRESHOLD_PX`: a press on a link that moves further selects instead.
const LINK_DRAG_THRESHOLD: f32 = 4.;

#[derive(Clone, Debug, PartialEq)]
pub(in crate::app::terminal) struct HoveredLink {
    kind: LinkKind,
    /// The URL, or the path resolved against the terminal's directory.
    target: String,
    /// Its cells by line: first column and one past the last.
    pub cells: Vec<(Line, usize, usize)>,
}

#[derive(Default)]
pub(in crate::app::terminal) struct PointerState {
    /// The link drawn underlined: a URL, or a path known to exist.
    pub hovered: Option<HoveredLink>,
    /// The link under the pointer, checked or not.
    candidate: Option<HoveredLink>,
    hover_cell: Option<GridPoint>,
    pending_link: Option<(HoveredLink, Point<Pixels>)>,
    /// Paths asked about with `filesystem.getMetadata`: whether they exist, `None` while asking.
    checked: HashMap<String, Option<bool>>,
    /// The button a tracking app saw go down, for its drag and release reports.
    reporting: Option<u8>,
    last_motion: Option<(usize, usize)>,
    pub(super) selection_menu: Option<super::menu::SelectionMenu>,
    pub(super) menu_timer: Option<Task<()>>,
}

impl PointerState {
    pub(super) fn cancel_selection_menu(&mut self) {
        self.menu_timer = None;
        self.selection_menu = None;
    }

    /// Output may have moved the text under the pointer.
    pub(super) fn forget_hover(&mut self) {
        self.hovered = None;
        self.candidate = None;
        self.hover_cell = None;
    }
}

/// One logical (soft-wrapped) line: its text and the cell and OSC 8 link of each char.
struct LogicalLine {
    text: String,
    points: Vec<GridPoint>,
    uris: Vec<Option<String>>,
}

fn logical_line(term: &Term<Listener>, line: Line) -> LogicalLine {
    let grid = term.grid();
    let (top, bottom) = (Line(-(grid.history_size() as i32)), Line(grid.screen_lines() as i32 - 1));
    let columns = grid.columns();
    let wraps = |line: Line| grid[line][Column(columns - 1)].flags.contains(Flags::WRAPLINE);
    let mut start = line;
    while start > top && wraps(Line(start.0 - 1)) {
        start = Line(start.0 - 1);
    }
    let mut out = LogicalLine { text: String::new(), points: Vec::new(), uris: Vec::new() };
    let mut current = start;
    loop {
        for column in 0..columns {
            let cell = &grid[current][Column(column)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let point = GridPoint::new(current, Column(column));
            let uri = cell.hyperlink().map(|link| link.uri().to_owned());
            for c in std::iter::once(cell.c).chain(cell.zerowidth().into_iter().flatten().copied()) {
                out.text.push(c);
                out.points.push(point);
                out.uris.push(uri.clone());
            }
        }
        if !wraps(current) || current >= bottom {
            break;
        }
        current = Line(current.0 + 1);
    }
    while out.text.ends_with(' ') && out.uris.last().is_some_and(Option::is_none) {
        out.text.pop();
        out.points.pop();
        out.uris.pop();
    }
    out
}

impl TerminalSession {
    /// The 0-based visible cell under `position`, as a tracking app counts it.
    fn screen_cell(&self, position: Point<Pixels>) -> (usize, usize) {
        let (point, _) = self.geometry.get().cell_at(position, 0);
        (point.column.0, point.line.0.max(0) as usize)
    }

    fn grid_point(&self, position: Point<Pixels>) -> (GridPoint, bool) {
        self.geometry.get().cell_at(position, self.term.grid().display_offset())
    }

    fn link_at(&self, point: GridPoint) -> Option<HoveredLink> {
        let line = logical_line(&self.term, point.line);
        let grid = self.term.grid();
        let point = if grid[point.line][point.column].flags.contains(Flags::WIDE_CHAR_SPACER) && point.column.0 > 0 {
            GridPoint::new(point.line, Column(point.column.0 - 1))
        } else {
            point
        };
        let index = line.points.iter().position(|p| *p == point)?;
        let (kind, target, start, end) = if let Some(uri) = line.uris[index].clone() {
            // An explicit OSC 8 link spans the cells that carry it.
            let same = |i: &usize| line.uris[*i].as_deref() == Some(uri.as_str());
            let start = (0..=index).rev().take_while(same).last()?;
            let end = (index..line.uris.len()).take_while(same).last()? + 1;
            if !links::is_terminal_url(&uri) {
                return None;
            }
            (LinkKind::Url, uri, start, end)
        } else {
            let found = links::extract(&line.text).into_iter().find(|m| m.start <= index && index < m.end)?;
            let target = match found.kind {
                LinkKind::Url => found.text,
                LinkKind::Path => links::resolve_path(&found.text, &self.launch.cwd),
            };
            (found.kind, target, found.start, found.end.min(line.points.len()))
        };
        let mut cells: Vec<(Line, usize, usize)> = Vec::new();
        for point in &line.points[start..end] {
            let width = if grid[point.line][point.column].flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            match cells.last_mut() {
                Some(last) if last.0 == point.line => last.2 = last.2.max(point.column.0 + width),
                _ => cells.push((point.line, point.column.0, point.column.0 + width)),
            }
        }
        Some(HoveredLink { kind, target, cells })
    }

    /// Underlines a URL now and a path once the server says it exists.
    fn hover(&mut self, point: GridPoint, cx: &mut Context<Self>) {
        if self.pointer.hover_cell == Some(point) {
            return;
        }
        self.pointer.hover_cell = Some(point);
        let candidate = self.link_at(point);
        let shown = match &candidate {
            Some(link) if link.kind == LinkKind::Path => {
                let exists = self.check_path(&link.target, false, cx);
                exists.then(|| link.clone())
            }
            other => other.clone(),
        };
        self.pointer.candidate = candidate;
        if self.pointer.hovered != shown {
            self.pointer.hovered = shown;
            cx.notify();
        }
    }

    /// Whether `path` is known to exist; unknown paths are asked about, and `open` opens
    /// the path once the answer is yes.
    fn check_path(&mut self, path: &str, open: bool, cx: &mut Context<Self>) -> bool {
        let key = links::split_position(path).0.to_owned();
        match self.pointer.checked.get(&key) {
            Some(Some(exists)) => return *exists,
            Some(None) if !open => return false,
            _ => {}
        }
        let Some(client) = self.client.clone() else { return false };
        self.pointer.checked.insert(key.clone(), None);
        let path = path.to_owned();
        cx.spawn(async move |this, cx| {
            let result = client.request("filesystem.getMetadata", json!({"paths": [key]})).await;
            let exists =
                result.is_ok_and(|value| matches!(value["entries"][0]["kind"].as_str(), Some("file" | "directory")));
            let _ = this.update(cx, |this, cx| {
                this.pointer.checked.insert(key.clone(), Some(exists));
                if !exists {
                    return;
                }
                if open {
                    cx.emit(SessionEvent::OpenPath(path));
                }
                let candidate = this.pointer.candidate.clone();
                if candidate.as_ref().is_some_and(|link| links::split_position(&link.target).0 == key) {
                    this.pointer.hovered = candidate;
                    cx.notify();
                }
            });
        })
        .detach();
        false
    }

    fn activate(&mut self, link: HoveredLink, ctrl: bool, cx: &mut Context<Self>) {
        match link.kind {
            LinkKind::Url => cx.emit(SessionEvent::OpenUrl { url: link.target, force_browser: ctrl }),
            LinkKind::Path => {
                if self.check_path(&link.target, true, cx) {
                    cx.emit(SessionEvent::OpenPath(link.target));
                }
            }
        }
    }

    /// `true` when the press is the terminal's: reported to the app, or on a link.
    pub fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.focus.focus(window, cx);
        cx.emit(SessionEvent::Activated);
        self.pointer.cancel_selection_menu();
        let mode = *self.term.mode();
        let modifiers = event.modifiers;
        if mouse::reports(mode, modifiers.shift, modifiers.control) {
            let button = match event.button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                _ => return false,
            };
            let (column, row) = self.screen_cell(event.position);
            if let Some(data) = mouse::encode(mode, MouseAction::Press, button, modifiers.alt, column, row) {
                self.pointer.reporting = Some(button);
                self.pointer.last_motion = Some((column, row));
                self.send_input(&data, cx);
            }
            return true;
        }
        if event.button != MouseButton::Left {
            return false;
        }
        let (point, right) = self.grid_point(event.position);
        if event.click_count == 1
            && !modifiers.shift
            && let Some(link) = self.link_at(point)
        {
            self.pointer.pending_link = Some((link, event.position));
            return true;
        }
        self.start_selection(point, right, event.click_count, modifiers.shift);
        cx.notify();
        false
    }

    fn start_selection(&mut self, point: GridPoint, right: bool, clicks: usize, extend: bool) {
        let side = if right { Side::Right } else { Side::Left };
        match (&mut self.term.selection, extend) {
            (Some(selection), true) => selection.update(point, side),
            _ => {
                let kind = match clicks {
                    2 => SelectionType::Semantic,
                    3.. => SelectionType::Lines,
                    _ => SelectionType::Simple,
                };
                self.term.selection = Some(Selection::new(kind, point, side));
            }
        }
        self.selecting = true;
    }

    /// Every move inside the pane, and moves outside it during a press.
    pub fn mouse_move(&mut self, event: &MouseMoveEvent, inside: bool, cx: &mut Context<Self>) {
        let mode = *self.term.mode();
        let tracking = mouse::reports(mode, event.modifiers.shift, event.modifiers.control);
        let button = self.pointer.reporting.or_else(|| (inside && tracking).then_some(mouse::NO_BUTTON));
        if let Some(button) = button.filter(|_| event.pressed_button.is_none() || self.pointer.reporting.is_some()) {
            let cell = self.screen_cell(event.position);
            if self.pointer.last_motion != Some(cell) {
                self.pointer.last_motion = Some(cell);
                if let Some(data) =
                    mouse::encode(mode, MouseAction::Motion, button, event.modifiers.alt, cell.0, cell.1)
                {
                    self.send_input(&data, cx);
                }
            }
            return;
        }
        if event.pressed_button == Some(MouseButton::Left) {
            if let Some((_, origin)) = &self.pointer.pending_link {
                let delta = event.position - *origin;
                if f32::from(delta.x).hypot(f32::from(delta.y)) <= LINK_DRAG_THRESHOLD {
                    return;
                }
                let origin = *origin;
                self.pointer.pending_link = None;
                let (point, right) = self.grid_point(origin);
                self.start_selection(point, right, 1, false);
            }
            if !self.selecting {
                return;
            }
            let (point, right) = self.grid_point(event.position);
            if let Some(selection) = &mut self.term.selection {
                selection.update(point, if right { Side::Right } else { Side::Left });
                cx.notify();
            }
            return;
        }
        if inside && !tracking {
            let (point, _) = self.grid_point(event.position);
            self.hover(point, cx);
        } else if self.pointer.hovered.is_some() || self.pointer.hover_cell.is_some() {
            self.pointer.forget_hover();
            cx.notify();
        }
    }

    pub fn mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self.pointer.reporting.take() {
            let mode = *self.term.mode();
            let (column, row) = self.screen_cell(event.position);
            self.pointer.last_motion = None;
            if let Some(data) = mouse::encode(mode, MouseAction::Release, button, event.modifiers.alt, column, row) {
                self.send_input(&data, cx);
            }
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        if let Some((link, _)) = self.pointer.pending_link.take() {
            self.activate(link, event.modifiers.control, cx);
            return;
        }
        if std::mem::replace(&mut self.selecting, false) && self.has_selection() {
            self.schedule_selection_menu(event.position, event.click_count >= 2, window, cx);
        }
    }

    pub fn scroll_wheel(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let line_height = self.geometry.get().line_height;
        if line_height <= Pixels::ZERO {
            return;
        }
        self.scroll_remainder += event.delta.pixel_delta(line_height).y / line_height;
        let lines = self.scroll_remainder.trunc();
        self.scroll_remainder -= lines;
        if lines == 0. {
            return;
        }
        let mode = *self.term.mode();
        if mouse::reports(mode, event.modifiers.shift, event.modifiers.control) {
            let button = if lines > 0. { mouse::WHEEL_UP } else { mouse::WHEEL_DOWN };
            let (column, row) = self.screen_cell(event.position);
            if let Some(data) = mouse::encode(mode, MouseAction::Press, button, event.modifiers.alt, column, row) {
                self.send_input(&data.repeat(lines.abs() as usize), cx);
            }
        } else if mode.contains(TermMode::ALT_SCREEN) {
            // Full-screen apps without mouse reporting scroll by cursor keys, as xterm does.
            let key = match (lines > 0., mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => "\u{1b}OA",
                (true, false) => "\u{1b}[A",
                (false, true) => "\u{1b}OB",
                (false, false) => "\u{1b}[B",
            };
            self.send_input(&key.repeat(lines.abs() as usize), cx);
        } else {
            self.term.scroll_display(Scroll::Delta(lines as i32));
            self.pointer.forget_hover();
            cx.notify();
        }
    }
}
