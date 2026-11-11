//! Pointer input over one terminal: selection (`beginSelection` / `extendSelectionTo`), link
//! hover and clicks (`findLinkAt`), mouse reports for programs that track the mouse, and the
//! wheel, as the web's Ghostty surface handles them.

use std::collections::HashMap;
use std::time::Duration;

use gpui_kit::{
    Context, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollEvent, Task,
    Window,
};
use serde_json::json;

use super::super::core::{self, Core, MouseAction, MouseInput};
use super::super::links::{self, LinkKind};
use super::super::model::{ScreenLink, ScreenPoint, ViewportPoint};
use super::super::mouse;
use super::{SessionEvent, TerminalSession};

/// `TERMINAL_LINK_DRAG_THRESHOLD_PX`: a press on a link that moves further selects instead.
const LINK_DRAG_THRESHOLD: f32 = 4.;
/// `SELECTION_AUTOSCROLL_RAMP_PX` and `SELECTION_AUTOSCROLL_MAX_ROWS`.
const AUTOSCROLL_RAMP: f32 = 50.;
const AUTOSCROLL_MAX_ROWS: f32 = 15.;
const AUTOSCROLL_TICK: Duration = Duration::from_millis(80);

/// `terminalSelectionAutoscrollRows`: the rows each tick scrolls while a selection drag is past
/// an edge, negative above it, ramping up to the maximum over the first 50px.
fn autoscroll_rows(y: Pixels, top: Pixels, bottom: Pixels) -> i32 {
    let distance = if y < top {
        y - top
    } else if y > bottom {
        y - bottom
    } else {
        return 0;
    };
    let ramp = (f32::from(distance) / AUTOSCROLL_RAMP).clamp(-1., 1.);
    // JavaScript's `Math.round`: halves go up.
    let rows = ramp.signum() + (ramp * (AUTOSCROLL_MAX_ROWS - 1.) + 0.5).floor();
    #[expect(clippy::cast_possible_truncation, reason = "at most the maximum row count")]
    let rows = rows as i32;
    rows
}

#[derive(Clone, Debug, PartialEq)]
pub(in crate::app::terminal) struct HoveredLink {
    /// The URL, or the path resolved against the terminal's directory.
    target: String,
    /// What the pointer is on, and where it is drawn.
    pub link: ScreenLink,
}

/// `selectionMode`: what a drag extends by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Cell,
    Word,
    Line,
}

/// A selection drag in progress.
#[derive(Default)]
struct Drag {
    mode: Mode,
    /// Where the selection starts, in screen rows (`selectionAnchorScreen`).
    anchor: Option<ScreenPoint>,
    /// The word or line first selected, which a drag keeps whole (`selectionBase`).
    base: Option<(ScreenPoint, ScreenPoint)>,
    /// The cell the drag last reached (`selectionEnd`).
    end: Option<ViewportPoint>,
    moved: bool,
}

#[derive(Default)]
pub(in crate::app::terminal) struct PointerState {
    /// The link drawn underlined: a URL, or a path known to exist.
    pub hovered: Option<HoveredLink>,
    /// The link under the pointer, checked or not.
    candidate: Option<HoveredLink>,
    hover_cell: Option<ViewportPoint>,
    pending_link: Option<(HoveredLink, Point<Pixels>, usize)>,
    /// Paths asked about with `filesystem.getMetadata`: whether they exist, `None` while asking.
    checked: HashMap<String, Option<bool>>,
    /// The button a tracking program saw go down, for its drag and release reports.
    reporting: Option<core::MouseButton>,
    /// The last motion report, so a still pointer does not repeat it (`lastMouseMotionData`).
    last_motion: String,
    /// Whether DEC 1003 was on at the last move (`mouseAnyEventTracking`).
    any_event: bool,
    drag: Option<Drag>,
    /// A selection drag past an edge: rows per tick, where the pointer is, and the ticking task.
    autoscroll: i32,
    drag_position: Option<Point<Pixels>>,
    autoscroll_task: Option<Task<()>>,
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

    /// A selection made some other way ends any drag.
    pub(super) fn end_drag(&mut self) {
        self.drag = None;
        self.autoscroll_task = None;
    }
}

/// `beginSelection`: a cell, or Ghostty's word or line around it, made the active selection.
fn begin(core: &mut Core, cell: ViewportPoint, clicks: usize) -> Drag {
    let mode = match clicks {
        0 | 1 => Mode::Cell,
        2 => Mode::Word,
        _ => Mode::Line,
    };
    if let Some(range) = (mode != Mode::Cell).then(|| core.select_around(cell, mode == Mode::Line)).flatten() {
        return Drag {
            mode,
            anchor: Some(range.start),
            base: Some((range.start, range.end)),
            end: Some(range.viewport_end),
            moved: false,
        };
    }
    let anchor = core.viewport_to_screen(cell);
    if let Some(anchor) = anchor {
        core.set_selection(anchor, anchor);
    }
    Drag { mode: Mode::Cell, anchor, base: None, end: Some(cell), moved: false }
}

/// `extendSelectionTo`: from the anchor to `cell`, by cell, word or line, keeping the first word
/// or line whole. Whether the selection changed.
fn drag_to(core: &mut Core, drag: &mut Drag, cell: ViewportPoint) -> bool {
    let Some(anchor) = drag.anchor else { return false };
    // Ghostty moves its selection with output; keep the drag on those rows too.
    let Some((now, _)) = core.selection_range() else { return false };
    if now.y != anchor.y {
        let shift = |point: ScreenPoint| ScreenPoint { y: (point.y + now.y).saturating_sub(anchor.y), ..point };
        drag.base = drag.base.map(|(start, end)| (shift(start), shift(end)));
    }
    drag.moved = true;
    drag.end = Some(cell);
    let range = (drag.mode != Mode::Cell).then(|| core.select_around(cell, drag.mode == Mode::Line)).flatten();
    let Some(at) = core.viewport_to_screen(cell) else {
        drag.anchor = Some(now);
        return false;
    };
    let before_base = drag.base.is_some_and(|(start, _)| at < start);
    let from = match drag.base {
        None => now,
        Some((start, end)) => {
            if before_base {
                end
            } else {
                start
            }
        }
    };
    let to = match range {
        None => at,
        Some(range) => {
            if before_base {
                range.start
            } else {
                range.end
            }
        }
    };
    drag.anchor = Some(from);
    core.set_selection(from, to);
    true
}

impl TerminalSession {
    /// `findLinkAt`: an OSC 8 link across the cells that carry it, or a URL or path in the
    /// visible text.
    fn link_at(&mut self, point: ViewportPoint) -> Option<HoveredLink> {
        if let Some(uri) = self.core.hyperlink_at(point) {
            if !links::is_terminal_url(&uri) {
                return None;
            }
            let rows = &self.core.snapshot().rows;
            let columns = rows.first().map_or(0, |row| row.cells.len());
            let wrapped: Vec<bool> = rows.iter().map(|row| row.wraps_to_next).collect();
            let wraps = |y: usize| wrapped.get(y).copied().unwrap_or(false);
            let (mut start, mut end) = (point, point);
            loop {
                let previous = if start.x > 0 {
                    ViewportPoint { x: start.x - 1, ..start }
                } else if start.y > 0 && wraps(start.y - 1) {
                    ViewportPoint { x: columns - 1, y: start.y - 1 }
                } else {
                    break;
                };
                if self.core.hyperlink_at(previous).as_deref() != Some(uri.as_str()) {
                    break;
                }
                start = previous;
            }
            loop {
                let next = if end.x + 1 < columns {
                    ViewportPoint { x: end.x + 1, ..end }
                } else if wraps(end.y) && end.y + 1 < wrapped.len() {
                    ViewportPoint { x: 0, y: end.y + 1 }
                } else {
                    break;
                };
                if self.core.hyperlink_at(next).as_deref() != Some(uri.as_str()) {
                    break;
                }
                end = next;
            }
            let link = ScreenLink { kind: LinkKind::Url, text: uri.clone(), start, end };
            return Some(HoveredLink { target: uri, link });
        }
        let link = super::super::model::link_at(&self.core.snapshot().rows, point.y, point.x)?;
        let target = match link.kind {
            LinkKind::Url => link.text.clone(),
            LinkKind::Path => links::resolve_path(&link.text, &self.launch.cwd),
        };
        Some(HoveredLink { target, link })
    }

    /// Underlines a URL now and a path once the server says it exists.
    fn hover(&mut self, point: ViewportPoint, cx: &mut Context<Self>) {
        if self.pointer.hover_cell == Some(point) {
            return;
        }
        self.pointer.hover_cell = Some(point);
        let candidate = self.link_at(point);
        let shown = match &candidate {
            Some(link) if link.link.kind == LinkKind::Path => {
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
        match link.link.kind {
            LinkKind::Url => cx.emit(SessionEvent::OpenUrl { url: link.target, force_browser: ctrl }),
            LinkKind::Path => {
                if self.check_path(&link.target, true, cx) {
                    cx.emit(SessionEvent::OpenPath(link.target));
                }
            }
        }
    }

    /// `sendMouse`: a report for the program, unless it repeats the last motion.
    fn send_mouse(
        &mut self,
        action: MouseAction,
        button: Option<core::MouseButton>,
        position: Point<Pixels>,
        modifiers: Modifiers,
        any_button_pressed: bool,
        cx: &mut Context<Self>,
    ) {
        let geometry = self.geometry.get();
        let corner = geometry.bounds.origin;
        let data = self.core.encode_mouse(&MouseInput {
            action,
            button,
            mods: mouse::modifiers(modifiers),
            x: f32::from(position.x - corner.x),
            y: f32::from(position.y - corner.y),
            width: f32::from(geometry.bounds.size.width),
            height: f32::from(geometry.bounds.size.height),
            cell_width: f32::from(geometry.cell_width),
            cell_height: f32::from(geometry.line_height),
            padding_left: f32::from(geometry.origin.x - corner.x),
            padding_top: f32::from(geometry.origin.y - corner.y),
            any_button_pressed,
        });
        let (send, next) = mouse::resolve(action, &data, &self.pointer.last_motion);
        self.pointer.last_motion = next;
        if send {
            self.send_input(&data, cx);
        }
    }

    /// `true` when the press is the terminal's: reported to the program, or on a link.
    pub fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.focus.focus(window, cx);
        cx.emit(SessionEvent::Activated);
        self.pointer.cancel_selection_menu();
        let modifiers = event.modifiers;
        if mouse::reports(self.core.mouse_tracking(), modifiers) {
            let Some(button) = mouse::button(event.button) else { return false };
            self.pointer.forget_hover();
            self.pointer.reporting = Some(button);
            self.send_mouse(MouseAction::Press, Some(button), event.position, modifiers, true, cx);
            return true;
        }
        // X11 and Wayland paste the selection on a middle click; macOS and Windows do not.
        if event.button == MouseButton::Middle && cfg!(target_os = "linux") {
            let text = self.core.selection_text();
            if !text.is_empty() {
                let data = self.core.encode_paste(&text);
                self.input(&data, cx);
            }
            return false;
        }
        if event.button != MouseButton::Left {
            return false;
        }
        let geometry = self.geometry.get();
        if event.click_count == 1
            && !modifiers.shift
            && let Some(link) = geometry.grid_cell_at(event.position).and_then(|cell| self.link_at(cell))
        {
            self.pointer.pending_link = Some((link, event.position, event.click_count));
            return true;
        }
        self.pointer.forget_hover();
        self.begin_selection(event.position, event.click_count);
        cx.notify();
        false
    }

    fn begin_selection(&mut self, position: Point<Pixels>, clicks: usize) {
        let cell = self.geometry.get().cell_at(position);
        self.pointer.drag = Some(begin(&mut self.core, cell, clicks));
    }

    /// Every move inside the pane, and moves outside it during a press.
    pub fn mouse_move(&mut self, event: &MouseMoveEvent, inside: bool, cx: &mut Context<Self>) {
        if event.pressed_button == Some(MouseButton::Left)
            && let Some((origin, clicks)) = self.pointer.pending_link.as_ref().map(|(_, at, n)| (*at, *n))
        {
            let delta = event.position - origin;
            if f32::from(delta.x).hypot(f32::from(delta.y)) <= LINK_DRAG_THRESHOLD {
                return;
            }
            self.pointer.pending_link = None;
            self.pointer.forget_hover();
            self.begin_selection(origin, clicks);
        }
        // Programs can toggle DEC 1003 at any time; a new tracking session starts fresh.
        let any_event = self.core.any_event_mouse();
        if std::mem::replace(&mut self.pointer.any_event, any_event) != any_event {
            self.pointer.last_motion.clear();
        }
        if let Some(button) = self.pointer.reporting {
            self.send_mouse(MouseAction::Motion, Some(button), event.position, event.modifiers, true, cx);
            return;
        }
        if inside && event.pressed_button.is_none() && mouse::reports(any_event, event.modifiers) {
            if self.pointer.hovered.take().is_some() {
                cx.notify();
            }
            self.send_mouse(MouseAction::Motion, None, event.position, event.modifiers, false, cx);
            return;
        }
        self.pointer.last_motion.clear();
        if event.pressed_button == Some(MouseButton::Left) && self.pointer.drag.is_some() {
            self.pointer.drag_position = Some(event.position);
            let (top, bottom) = self.geometry.get().scroll_edges;
            self.set_autoscroll(autoscroll_rows(event.position.y, top, bottom), cx);
            let cell = self.geometry.get().cell_at(event.position);
            if self.pointer.drag.as_ref().and_then(|drag| drag.end) != Some(cell) {
                self.extend_selection(event.position, cx);
            }
            return;
        }
        let tracking = mouse::reports(self.core.mouse_tracking(), event.modifiers);
        match self.geometry.get().grid_cell_at(event.position).filter(|_| inside && !tracking) {
            Some(cell) => self.hover(cell, cx),
            None if self.pointer.hovered.is_some() || self.pointer.hover_cell.is_some() => {
                self.pointer.forget_hover();
                cx.notify();
            }
            None => {}
        }
    }

    fn extend_selection(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let cell = self.geometry.get().cell_at(position);
        if let Some(drag) = &mut self.pointer.drag
            && drag_to(&mut self.core, drag, cell)
        {
            cx.notify();
        }
    }

    /// `setSelectionAutoscroll`: scrolls every tick while the drag stays past an edge.
    fn set_autoscroll(&mut self, rows: i32, cx: &mut Context<Self>) {
        self.pointer.autoscroll = rows;
        if rows == 0 {
            self.pointer.autoscroll_task = None;
            return;
        }
        if self.pointer.autoscroll_task.is_some() {
            return;
        }
        self.pointer.autoscroll_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_TICK).await;
                let ticked = this.update(cx, |this, cx| {
                    this.scroll_rows(i64::from(this.pointer.autoscroll), cx);
                    if let Some(position) = this.pointer.drag_position {
                        this.extend_selection(position, cx);
                    }
                });
                if ticked.is_err() {
                    break;
                }
            }
        }));
    }

    pub fn mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.pointer.autoscroll_task = None;
        self.pointer.drag_position = None;
        if let Some(button) = self.pointer.reporting.take() {
            self.send_mouse(MouseAction::Release, Some(button), event.position, event.modifiers, false, cx);
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        if let Some((link, _, _)) = self.pointer.pending_link.take() {
            // Output may have replaced the link before the release.
            let now = self.geometry.get().grid_cell_at(event.position).and_then(|cell| self.link_at(cell));
            if now.is_some_and(|now| now.link == link.link) {
                self.activate(link, event.modifiers.control, cx);
            }
            return;
        }
        let Some(drag) = self.pointer.drag.take() else { return };
        if !drag.moved && drag.mode == Mode::Cell {
            self.core.clear_selection();
            cx.notify();
        } else if self.has_selection() {
            self.schedule_selection_menu(event.position, event.click_count >= 2, window, cx);
        }
    }

    pub fn scroll_wheel(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let line_height = self.geometry.get().line_height;
        if line_height <= Pixels::ZERO {
            return;
        }
        self.scroll_remainder += f32::from(event.delta.pixel_delta(line_height).y) / f32::from(line_height);
        let lines = self.scroll_remainder.trunc();
        self.scroll_remainder -= lines;
        if lines == 0. {
            return;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "whole wheel lines (truncated above)"
        )]
        let count = lines.abs() as usize;
        // Positive lines scroll up, toward older output.
        if mouse::reports(self.core.mouse_tracking(), event.modifiers) {
            let button = if lines > 0. { core::MouseButton::WheelUp } else { core::MouseButton::WheelDown };
            for _ in 0..count {
                self.send_mouse(MouseAction::Press, Some(button), event.position, event.modifiers, false, cx);
            }
        } else if self.core.alt_screen() {
            // Full-screen programs without mouse reporting scroll by cursor keys, as xterm does.
            let key = match (lines > 0., self.core.app_cursor()) {
                (true, true) => "\u{1b}OA",
                (true, false) => "\u{1b}[A",
                (false, true) => "\u{1b}OB",
                (false, false) => "\u{1b}[B",
            };
            self.send_input(&key.repeat(count), cx);
        } else {
            #[expect(clippy::cast_possible_truncation, reason = "whole wheel lines (truncated above)")]
            self.scroll_rows(-(lines as i64), cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::px;

    use super::super::super::core::tests::core;
    use super::*;

    #[test]
    fn a_drag_keeps_its_anchor_on_text_that_output_scrolls() {
        let mut core = core(20, 10);
        let rows: Vec<String> = (0..10).map(|index| format!("row {index}")).collect();
        core.write(format!("\x1b[?1049h\x1b[H{}", rows.join("\r\n")).as_bytes());
        let mut drag = begin(&mut core, ViewportPoint { x: 0, y: 6 }, 1);
        assert!(drag_to(&mut core, &mut drag, ViewportPoint { x: 4, y: 6 }));
        assert_eq!(core.selection_text(), "row 6");
        // The pointer stays over the row that output scrolls "row 8" into.
        core.write(b"\r\nrow 10\r\nrow 11");
        assert!(drag_to(&mut core, &mut drag, ViewportPoint { x: 4, y: 6 }));
        assert_eq!(core.selection_text(), "row 6\nrow 7\nrow 8");
    }

    #[test]
    fn word_and_line_drags_keep_the_first_word_or_line_whole() {
        let mut core = core(30, 4);
        core.write(b"alpha beta gamma\r\nsecond line here");
        let mut drag = begin(&mut core, ViewportPoint { x: 7, y: 0 }, 2);
        assert_eq!(core.selection_text(), "beta");
        drag_to(&mut core, &mut drag, ViewportPoint { x: 13, y: 0 });
        assert_eq!(core.selection_text(), "beta gamma");
        drag_to(&mut core, &mut drag, ViewportPoint { x: 1, y: 0 });
        assert_eq!(core.selection_text(), "alpha beta", "dragging back keeps the first word");
        let mut drag = begin(&mut core, ViewportPoint { x: 3, y: 1 }, 3);
        assert_eq!(core.selection_text(), "second line here");
        drag_to(&mut core, &mut drag, ViewportPoint { x: 0, y: 0 });
        assert_eq!(core.selection_text(), "alpha beta gamma\nsecond line here");
    }

    #[test]
    fn selection_drags_past_an_edge_scroll_faster_further_out() {
        let rows = |y: f32| autoscroll_rows(px(y), px(100.), px(300.));
        assert_eq!(rows(200.), 0);
        assert_eq!(rows(100.), 0);
        assert_eq!(rows(99.), -1);
        assert_eq!(rows(75.), -8);
        assert_eq!(rows(50.), -15);
        assert_eq!(rows(-500.), -15);
        assert_eq!(rows(301.), 1);
        assert_eq!(rows(325.), 8);
        assert_eq!(rows(900.), 15);
    }
}
