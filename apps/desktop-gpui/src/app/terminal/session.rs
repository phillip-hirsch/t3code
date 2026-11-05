//! One terminal pane: an `alacritty_terminal` emulator fed by the server's
//! `terminal.attach` stream, plus keyboard, mouse selection, scrollback and
//! clipboard. Output only re-renders this view.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use gpui_kit::{
    AppContext as _, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement as _, Pixels, Render, ScrollEvent, Styled as _, Subscription, Task, UTF16Selection, Window, div,
    point, size,
};
use serde_json::{Value, json};

use super::grid::{Geometry, TerminalGrid, TerminalTheme};
use super::keys::{KeyAction, key_action};
use crate::rpc::{RpcClient, RpcError, StreamEvent};

/// `TerminalWriteInput.data` is capped at 65,536 UTF-16 units.
const MAX_WRITE_UNITS: usize = 65_536;
/// Drawer drags resize every frame; the PTY hears the size once it settles.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Clone, Default)]
pub(super) struct Listener(Rc<RefCell<Vec<Event>>>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        self.0.borrow_mut().push(event);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridSize {
    columns: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// Where the shell starts (`terminal.open` / `terminal.attach` launch fields).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Launch {
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub project_root: String,
}

impl Launch {
    /// `projectScriptRuntimeEnv`.
    fn env(&self) -> Value {
        let mut env = json!({"T3CODE_PROJECT_ROOT": self.project_root});
        if let Some(path) = &self.worktree_path {
            env["T3CODE_WORKTREE_PATH"] = path.clone().into();
        }
        env
    }

    pub fn payload(&self, thread_id: &str, terminal_id: &str) -> Value {
        let mut input = json!({"threadId": thread_id, "terminalId": terminal_id, "cwd": self.cwd, "env": self.env()});
        if let Some(path) = &self.worktree_path {
            input["worktreePath"] = path.clone().into();
        }
        input
    }
}

pub(super) enum SessionEvent {
    /// The process exited or the server closed the terminal.
    Exited,
    /// Pressed: the drawer makes this pane active.
    Activated,
}

pub(super) struct TerminalSession {
    pub thread_id: String,
    pub terminal_id: String,
    launch: Launch,
    term: Term<Listener>,
    parser: Processor,
    events: Listener,
    pub focus: FocusHandle,
    /// Written by the grid element each prepaint, read by mouse and IME handlers.
    pub geometry: Cell<Geometry>,
    size: Option<GridSize>,
    sent_size: Option<GridSize>,
    running: bool,
    exited: bool,
    /// The attach stream replied once; later attaches resume instead of opening a shell.
    attached: bool,
    client: Option<RpcClient>,
    outbox: String,
    writing: bool,
    selecting: bool,
    scroll_remainder: f32,
    _attach: Option<Task<()>>,
    _resize: Option<Task<()>>,
    _focus: [Subscription; 2],
}

impl EventEmitter<SessionEvent> for TerminalSession {}

fn new_term(size: GridSize, events: &Listener) -> Term<Listener> {
    Term::new(Config::default(), &size, events.clone())
}

/// Takes the longest prefix of `outbox` that fits one `terminal.write`.
fn take_chunk(outbox: &mut String) -> Option<String> {
    if outbox.is_empty() {
        return None;
    }
    let mut units = 0;
    let end = outbox
        .char_indices()
        .find(|(_, c)| {
            units += c.len_utf16();
            units > MAX_WRITE_UNITS
        })
        .map_or(outbox.len(), |(index, _)| index);
    let rest = outbox.split_off(end);
    Some(std::mem::replace(outbox, rest))
}

/// What a paste sends: bracketed when the app asked for it, else with `\r` line ends.
fn paste_input(text: &str, bracketed: bool) -> String {
    if bracketed {
        format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', ""))
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    }
}

impl TerminalSession {
    pub fn new(
        thread_id: String,
        terminal_id: String,
        launch: Launch,
        client: Option<RpcClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let events = Listener::default();
        let focus = cx.focus_handle();
        let _focus =
            [cx.on_focus(&focus, window, |_, _, cx| cx.notify()), cx.on_blur(&focus, window, |_, _, cx| cx.notify())];
        let mut session = Self {
            thread_id,
            terminal_id,
            launch,
            term: new_term(GridSize { columns: 80, rows: 24 }, &events),
            parser: Processor::new(),
            events,
            focus,
            geometry: Cell::default(),
            size: None,
            sent_size: None,
            running: false,
            exited: false,
            attached: false,
            client: None,
            outbox: String::new(),
            writing: false,
            selecting: false,
            scroll_remainder: 0.,
            _attach: None,
            _resize: None,
            _focus,
        };
        if let Some(client) = client {
            session.attach(client, cx);
        }
        session
    }

    pub fn term(&self) -> &Term<Listener> {
        &self.term
    }

    /// Subscribes to the terminal's output; called again after a reconnect.
    pub fn attach(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        let mut input = self.launch.payload(&self.thread_id, &self.terminal_id);
        if self.attached {
            input["resume"] = true.into();
        }
        self.client = Some(client.clone());
        self.sent_size = None;
        let stream = client.stream("terminal.attach", input);
        self._attach = Some(cx.spawn(async move |this, cx| {
            while let Some(first) = stream.next().await {
                let mut events = vec![first];
                events.extend(std::iter::from_fn(|| stream.try_next()));
                let ended = this.update(cx, |this, cx| {
                    let mut ended = false;
                    for event in events {
                        match event {
                            StreamEvent::Item(value) => this.apply(&value, cx),
                            StreamEvent::End(result) => {
                                ended = true;
                                if let (Err(error), false) = (&result, this.exited) {
                                    if !matches!(error, RpcError::Disconnected(_)) {
                                        this.system_message(&error.to_string());
                                    }
                                }
                            }
                        }
                    }
                    cx.notify();
                    ended
                });
                if ended.unwrap_or(true) {
                    break;
                }
            }
        }));
    }

    /// One `TerminalAttachStreamEvent` (`applyTerminalAttachStreamEvent`).
    fn apply(&mut self, event: &Value, cx: &mut Context<Self>) {
        self.attached = true;
        match event["type"].as_str() {
            Some("snapshot" | "restarted") => {
                let snapshot = &event["snapshot"];
                self.reset();
                self.feed(snapshot["history"].as_str().unwrap_or_default(), false, cx);
                let status = snapshot["status"].as_str().unwrap_or_default();
                self.running = status == "running";
                if status == "exited" {
                    self.on_exit("Process exited", cx);
                } else {
                    self.exited = false;
                    // A new attachment has not seen this grid yet (`resendSize`).
                    self.sent_size = None;
                    self.send_size(cx);
                }
            }
            Some("output") => {
                self.feed(event["data"].as_str().unwrap_or_default(), true, cx);
            }
            Some("cleared") => self.reset(),
            Some("exited") => self.on_exit("Process exited", cx),
            Some("closed") => self.on_exit("Terminal closed", cx),
            Some("error") => {
                let message = event["message"].as_str().unwrap_or("Terminal error").to_owned();
                self.system_message(&message);
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        let size = self.size.unwrap_or(GridSize { columns: 80, rows: 24 });
        self.term = new_term(size, &self.events);
        self.parser = Processor::new();
    }

    /// Replayed history and our own notices must not answer the queries in
    /// them, or the shell would read stale replies as typed input.
    fn feed(&mut self, data: &str, reply: bool, cx: &mut Context<Self>) {
        self.parser.advance(&mut self.term, data.as_bytes());
        let events: Vec<Event> = self.events.0.borrow_mut().drain(..).collect();
        for event in events {
            match event {
                Event::PtyWrite(text) if reply => self.send_input(&text, cx),
                Event::TextAreaSizeRequest(format) if reply => {
                    let geometry = self.geometry.get();
                    let text = format(WindowSize {
                        num_lines: self.term.screen_lines() as u16,
                        num_cols: self.term.columns() as u16,
                        cell_width: f32::from(geometry.cell_width) as u16,
                        cell_height: f32::from(geometry.line_height) as u16,
                    });
                    self.send_input(&text, cx);
                }
                Event::ClipboardStore(_, text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
                _ => {}
            }
        }
    }

    fn system_message(&mut self, message: &str) {
        self.parser.advance(&mut self.term, format!("\r\n[terminal] {message}\r\n").as_bytes());
        self.events.0.borrow_mut().clear();
    }

    fn on_exit(&mut self, message: &str, cx: &mut Context<Self>) {
        self.running = false;
        if std::mem::replace(&mut self.exited, true) {
            return;
        }
        self.system_message(message);
        cx.emit(SessionEvent::Exited);
    }

    /// Queues `data` for the PTY; writes go out one at a time, in order.
    fn send_input(&mut self, data: &str, cx: &mut Context<Self>) {
        self.outbox.push_str(data);
        if self.writing {
            return;
        }
        let Some(client) = self.client.clone() else { return };
        self.writing = true;
        let (thread_id, terminal_id) = (self.thread_id.clone(), self.terminal_id.clone());
        cx.spawn(async move |this, cx| {
            loop {
                let next = this.update(cx, |this, _| {
                    let chunk = take_chunk(&mut this.outbox);
                    this.writing = chunk.is_some();
                    chunk
                });
                let Ok(Some(data)) = next else { break };
                let payload = json!({"threadId": thread_id, "terminalId": terminal_id, "data": data});
                if let Err(error) = client.request("terminal.write", payload).await {
                    let _ = this.update(cx, |this, cx| {
                        if !matches!(error, RpcError::Disconnected(_)) {
                            this.system_message(&error.to_string());
                            cx.notify();
                        }
                    });
                }
            }
        })
        .detach();
    }

    /// What the user typed or pasted: it follows the prompt, so scroll back down.
    fn input(&mut self, data: &str, cx: &mut Context<Self>) {
        self.term.scroll_display(Scroll::Bottom);
        self.term.selection = None;
        self.send_input(data, cx);
        cx.notify();
    }

    /// The grid that fits the pane, from the grid element's layout.
    pub fn fit(&mut self, columns: usize, rows: usize, cx: &mut Context<Self>) {
        let size = GridSize { columns, rows };
        if self.size == Some(size) {
            return;
        }
        self.size = Some(size);
        self.term.resize(size);
        self._resize = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.send_size(cx));
        }));
    }

    fn send_size(&mut self, cx: &mut Context<Self>) {
        let (Some(size), Some(client)) = (self.size, self.client.clone()) else { return };
        if !self.running || self.sent_size == Some(size) {
            return;
        }
        self.sent_size = Some(size);
        let payload = json!({
            "threadId": self.thread_id,
            "terminalId": self.terminal_id,
            "cols": size.columns.min(1000),
            "rows": size.rows.min(500),
        });
        cx.background_spawn(async move {
            if let Err(error) = client.request("terminal.resize", payload).await {
                log::debug!("terminal resize failed: {error}");
            }
        })
        .detach();
    }

    fn has_selection(&self) -> bool {
        self.term.selection.as_ref().is_some_and(|selection| !selection.is_empty())
    }

    fn copy(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.term.selection_to_string().filter(|text| !text.is_empty()) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        if !text.is_empty() {
            let bracketed = self.term.mode().contains(TermMode::BRACKETED_PASTE);
            self.input(&paste_input(&text, bracketed), cx);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let app_cursor = self.term.mode().contains(TermMode::APP_CURSOR);
        match key_action(&event.keystroke, app_cursor, self.has_selection()) {
            None => return,
            Some(KeyAction::Send(data)) => self.input(&data, cx),
            Some(KeyAction::Copy { keep_selection }) => {
                self.copy(cx);
                if !keep_selection {
                    self.term.selection = None;
                }
                cx.notify();
            }
            Some(KeyAction::Paste) => self.paste(cx),
        }
        cx.stop_propagation();
    }

    pub fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        cx.emit(SessionEvent::Activated);
        if event.button != MouseButton::Left {
            return;
        }
        let (point, right) = self.geometry.get().cell_at(event.position, self.term.grid().display_offset());
        let side = if right { Side::Right } else { Side::Left };
        match (&mut self.term.selection, event.modifiers.shift) {
            (Some(selection), true) => selection.update(point, side),
            _ => {
                let kind = match event.click_count {
                    2 => SelectionType::Semantic,
                    3.. => SelectionType::Lines,
                    _ => SelectionType::Simple,
                };
                self.term.selection = Some(Selection::new(kind, point, side));
            }
        }
        self.selecting = true;
        cx.notify();
    }

    pub fn mouse_drag(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        let (point, right) = self.geometry.get().cell_at(event.position, self.term.grid().display_offset());
        if let Some(selection) = &mut self.term.selection {
            selection.update(point, if right { Side::Right } else { Side::Left });
            cx.notify();
        }
    }

    pub fn mouse_up(&mut self) {
        self.selecting = false;
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
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && !mode.intersects(TermMode::MOUSE_MODE) {
            // Full-screen apps without mouse reporting scroll by cursor keys, as xterm does.
            let key = match (lines > 0., mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => "\x1bOA",
                (true, false) => "\x1b[A",
                (false, true) => "\x1bOB",
                (false, false) => "\x1b[B",
            };
            self.send_input(&key.repeat(lines.abs() as usize), cx);
        } else {
            self.term.scroll_display(Scroll::Delta(lines as i32));
            cx.notify();
        }
    }
}

impl Render for TerminalSession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = TerminalTheme::new(crate::theme::tokens(cx));
        let focused = self.focus.is_focused(window);
        div()
            .size_full()
            .key_context("Terminal")
            .track_focus(&self.focus)
            .bg(theme.background)
            .on_key_down(cx.listener(Self::key_down))
            .child(TerminalGrid { session: cx.entity(), theme, focused })
    }
}

/// Text and IME commits arrive here; the terminal has no editable text of its own.
impl EntityInputHandler for TerminalSession {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        Some(UTF16Selection { range: 0..0, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        if !text.is_empty() {
            self.input(text, cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        _: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
    }

    /// The cursor cell, so the IME window opens next to the prompt.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let geometry = self.geometry.get();
        let cursor = self.term.grid().cursor.point;
        let origin = geometry.origin
            + point(geometry.cell_width * cursor.column.0 as f32, geometry.line_height * cursor.line.0 as f32);
        Some(Bounds::new(origin, size(geometry.cell_width, geometry.line_height)))
    }

    fn character_index_for_point(
        &mut self,
        _: gpui_kit::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_split_below_the_server_limit_on_char_boundaries() {
        let mut outbox = "é".repeat(MAX_WRITE_UNITS + 3);
        let first = take_chunk(&mut outbox).unwrap();
        assert_eq!(first.chars().count(), MAX_WRITE_UNITS);
        assert_eq!(take_chunk(&mut outbox).as_deref(), Some("ééé"));
        assert_eq!(take_chunk(&mut outbox), None);
        // Astral characters count as two units.
        let mut outbox = "😀".repeat(MAX_WRITE_UNITS / 2 + 1);
        assert_eq!(take_chunk(&mut outbox).unwrap().chars().count(), MAX_WRITE_UNITS / 2);
    }

    #[test]
    fn paste_brackets_when_asked_and_otherwise_sends_carriage_returns() {
        assert_eq!(paste_input("a\r\nb\nc", false), "a\rb\rc");
        assert_eq!(paste_input("ls\x1b[201~\n", true), "\x1b[200~ls[201~\n\x1b[201~");
    }
}
