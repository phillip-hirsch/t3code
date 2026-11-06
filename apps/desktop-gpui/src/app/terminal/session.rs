//! One terminal pane: an `alacritty_terminal` emulator fed by the server's
//! `terminal.attach` stream, plus keyboard, mouse selection, scrollback and
//! clipboard. Output only re-renders this view.

use std::cell::{Cell, RefCell};
use std::ops::{Deref, DerefMut, Range};
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
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render, Styled as _, Subscription,
    Task, UTF16Selection, Window, div, point, size,
};
use serde_json::{Value, json};

mod menu;
mod pointer;

use gpui_kit::component::input;
use gpui_kit::component::menu::ContextMenuExt as _;

use super::grid::{Batched, Geometry, TerminalGrid, TerminalTheme, batch};
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
    /// Set by project script runs: the server closes the terminal when the thread settles.
    pub stop_on_settle: Option<bool>,
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
        if let Some(stop) = self.stop_on_settle {
            input["stopOnSettle"] = stop.into();
        }
        input
    }
}

pub(super) enum SessionEvent {
    /// The process exited or the server closed the terminal.
    Exited,
    /// Pressed: the drawer makes this pane active.
    Activated,
    /// "Add to chat" on a selection.
    AddToChat(Selected),
    /// A clicked web link; Ctrl skips the in-app browser.
    OpenUrl { url: String, force_browser: bool },
    /// A clicked path that exists, resolved and with its `:line:column`.
    OpenPath(String),
}

/// A selection for the composer (`TerminalContextSelection` without the label).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Selected {
    pub line_start: usize,
    pub line_end: usize,
    pub text: String,
}

/// The emulator and a count of its mutable borrows, so the grid rebatches only after a change.
pub(super) struct Emulator {
    term: Term<Listener>,
    revision: u64,
}

impl Emulator {
    fn new(size: GridSize, events: &Listener) -> Self {
        Self { term: Term::new(Config::default(), &size, events.clone()), revision: 0 }
    }
}

impl Deref for Emulator {
    type Target = Term<Listener>;

    fn deref(&self) -> &Term<Listener> {
        &self.term
    }
}

impl DerefMut for Emulator {
    fn deref_mut(&mut self) -> &mut Term<Listener> {
        self.revision += 1;
        &mut self.term
    }
}

pub(super) struct TerminalSession {
    pub thread_id: String,
    pub terminal_id: String,
    launch: Launch,
    term: Emulator,
    /// The last batch, with the emulator revision and theme it was built from.
    batched: Option<(u64, TerminalTheme, Rc<Batched>)>,
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
    pointer: pointer::PointerState,
    _attach: Option<Task<()>>,
    _resize: Option<Task<()>>,
    _focus: [Subscription; 2],
}

impl EventEmitter<SessionEvent> for TerminalSession {}

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
            term: Emulator::new(GridSize { columns: 80, rows: 24 }, &events),
            batched: None,
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
            pointer: Default::default(),
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

    /// The visible grid's runs and spans, rebatched only after the terminal or the theme changed.
    pub fn batched(&mut self, theme: &TerminalTheme) -> Rc<Batched> {
        if let Some((revision, cached_theme, batched)) = &self.batched
            && *revision == self.term.revision
            && cached_theme == theme
        {
            return batched.clone();
        }
        let term = &*self.term;
        let batched = Rc::new(batch(term.renderable_content(), term.columns(), term.screen_lines(), theme));
        self.batched = Some((self.term.revision, *theme, batched.clone()));
        batched
    }

    /// The link to underline, if the pointer is on one.
    pub fn hovered_link(&self) -> Option<&pointer::HoveredLink> {
        self.pointer.hovered.as_ref()
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
                                if let (Err(error), false) = (&result, this.exited)
                                    && !matches!(error, RpcError::Disconnected(_))
                                {
                                    this.system_message(&error.to_string());
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
        *self.term = Term::new(Config::default(), &size, self.events.clone());
        self.parser = Processor::new();
    }

    /// Replayed history and our own notices must not answer the queries in
    /// them, or the shell would read stale replies as typed input.
    fn feed(&mut self, data: &str, reply: bool, cx: &mut Context<Self>) {
        self.parser.advance(&mut *self.term, data.as_bytes());
        self.pointer.forget_hover();
        let events = std::mem::take(&mut *self.events.0.borrow_mut());
        for event in events {
            match event {
                Event::PtyWrite(text) if reply => self.send_input(&text, cx),
                Event::TextAreaSizeRequest(format) if reply => {
                    let geometry = self.geometry.get();
                    #[expect(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "cell sizes in whole pixels"
                    )]
                    let text = format(WindowSize {
                        num_lines: u16::try_from(self.term.screen_lines()).unwrap_or(u16::MAX),
                        num_cols: u16::try_from(self.term.columns()).unwrap_or(u16::MAX),
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
        self.parser.advance(&mut *self.term, format!("\r\n[terminal] {message}\r\n").as_bytes());
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
    /// Without a connection the input is dropped: queued keystrokes would run on reconnect.
    fn send_input(&mut self, data: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.outbox.push_str(data);
        if self.writing {
            return;
        }
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
            let _ = this.update(cx, TerminalSession::send_size);
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

    /// `selectAll`: every line, scrollback included.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let grid = self.term.grid();
        let top = alacritty_terminal::index::Point::new(
            alacritty_terminal::index::Line(-super::line_count(grid.history_size())),
            alacritty_terminal::index::Column(0),
        );
        let bottom = alacritty_terminal::index::Point::new(
            alacritty_terminal::index::Line(super::line_count(grid.screen_lines()) - 1),
            grid.last_column(),
        );
        let mut selection = Selection::new(SelectionType::Simple, top, Side::Left);
        selection.update(bottom, Side::Right);
        self.term.selection = Some(selection);
        cx.notify();
    }

    /// `scrollToBottom`: back to the live screen.
    pub fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.term.scroll_display(Scroll::Bottom);
        cx.notify();
    }

    /// The selection as chat context: 1-based buffer lines (scrollback counts) and its text
    /// without surrounding blank lines (`readSelectionAction`).
    pub fn selected(&self) -> Option<Selected> {
        let range = self.term.selection.as_ref()?.to_range(&*self.term)?;
        let text = self.term.selection_to_string()?;
        let text = text.replace("\r\n", "\n").trim_matches('\n').to_owned();
        if text.is_empty() {
            return None;
        }
        let history = super::line_count(self.term.grid().history_size());
        let line_start = usize::try_from(range.start.line.0 + history + 1).unwrap_or(0).max(1);
        let line_end = usize::try_from(range.end.line.0 + history + 1).unwrap_or(0).max(1).max(line_start);
        Some(Selected { line_start, line_end, text })
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
        let mode = *self.term.mode();
        let (app_cursor, alt_screen) = (mode.contains(TermMode::APP_CURSOR), mode.contains(TermMode::ALT_SCREEN));
        self.pointer.cancel_selection_menu();
        match key_action(&event.keystroke, app_cursor, alt_screen, self.has_selection()) {
            None => return,
            Some(KeyAction::SelectAll) => self.select_all(cx),
            Some(KeyAction::Scroll(scroll)) => {
                self.term.scroll_display(scroll.scroll());
                cx.notify();
            }
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
}

impl Render for TerminalSession {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = TerminalTheme::new(crate::theme::tokens(cx));
        let focused = self.focus.is_focused(window);
        let menu = self.context_menu(cx);
        div()
            .id(gpui_kit::SharedString::from(format!("terminal-session-{}", self.terminal_id)))
            .size_full()
            .key_context("Terminal")
            .track_focus(&self.focus)
            .bg(theme.background)
            .on_key_down(cx.listener(Self::key_down))
            // Edit › Copy, Paste and Select All; the keys reach `key_down` instead.
            .on_action(cx.listener(|this, _: &input::Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &input::Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &input::SelectAll, _, cx| this.select_all(cx)))
            .child(TerminalGrid { session: cx.entity(), theme, focused })
            .children(self.render_selection_menu())
            .context_menu(menu)
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
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let row = usize::try_from(cursor.line.0 + super::line_count(grid.display_offset()))
            .ok()
            .filter(|row| *row < geometry.rows)?;
        let origin =
            geometry.origin + point(geometry.cell_width * cursor.column.0 as f32, geometry.line_height * row as f32);
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
