//! One terminal pane: a libghostty-vt emulator ([`Core`]) fed by the server's
//! `terminal.attach` stream, plus keyboard, mouse selection, scrollback and
//! clipboard. Output only re-renders this view.

use std::cell::Cell;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::{
    AppContext as _, Bounds, ClipboardItem, Context, EntityInputHandler, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Render, Styled as _, Subscription, Task, UTF16Selection, Window, canvas,
    div, fill, point, px, size,
};
use serde_json::{Value, json};

mod menu;
mod pointer;

use gpui_kit::component::input;
use gpui_kit::component::menu::ContextMenuExt as _;

use super::core::{Core, KeyAction as Press};
use super::grid::{Batched, CursorCell, Frame, Geometry, TerminalGrid, TerminalTheme, batch};
use super::keys::{self, KeyAction, key_action};
use super::model::{ScreenLink, ViewportPoint};
use super::scrollbar;
use crate::rpc::{RpcClient, RpcError, StreamEvent};

/// `TerminalWriteInput.data` is capped at 65,536 UTF-16 units.
const MAX_WRITE_UNITS: usize = 65_536;
/// Drawer drags resize every frame; the PTY hears the size once it settles.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(50);
/// `CURSOR_BLINK_INTERVAL_MS`: a focused cursor blinks unless the program or reduced motion stops it.
const CURSOR_BLINK: Duration = Duration::from_millis(500);
/// `SYNCHRONIZED_OUTPUT_RENDER_TIMEOUT_MS`: a synchronized frame that never ends is shown anyway.
const SYNCHRONIZED_OUTPUT_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridSize {
    columns: usize,
    rows: usize,
}

impl GridSize {
    /// The server's grid when it reports one, read from a snapshot or an `activity` event.
    fn shared(source: &Value) -> Option<Self> {
        let count = |key: &str| source[key].as_u64().and_then(|n| usize::try_from(n).ok());
        Some(Self { columns: count("cols")?, rows: count("rows")? })
    }

    /// `applyGrid`: other clients share the PTY, so its grid is drawn, each axis capped to the
    /// pane's own fit (the server lags a local resize, and observers never constrain it).
    fn shown(measured: Self, shared: Option<Self>) -> Self {
        let Some(shared) = shared else { return measured };
        Self { columns: shared.columns.min(measured.columns), rows: shared.rows.min(measured.rows) }
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

    /// The output stream a pane subscribes to: `terminal.attach` (which starts the shell when
    /// `resume` is unset), or `terminal.observe` without `terminal:operate`.
    fn stream_request(
        &self,
        thread_id: &str,
        terminal_id: &str,
        read_only: bool,
        resume: bool,
    ) -> (&'static str, Value) {
        if read_only {
            return ("terminal.observe", json!({"threadId": thread_id, "terminalId": terminal_id}));
        }
        let mut input = self.payload(thread_id, terminal_id);
        if resume {
            input["resume"] = true.into();
        }
        ("terminal.attach", input)
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

pub(super) struct TerminalSession {
    pub thread_id: String,
    pub terminal_id: String,
    launch: Launch,
    core: Core,
    /// The theme Ghostty last resolved colors with.
    theme: TerminalTheme,
    /// The last batch, with the snapshot revision and theme it was built from.
    batched: Option<(u64, TerminalTheme, Rc<Batched>)>,
    /// The cursor as last drawn, where the IME opens.
    cursor: Option<ViewportPoint>,
    pub focus: FocusHandle,
    /// Written by the grid element each prepaint, read by mouse and IME handlers.
    pub geometry: Cell<Geometry>,
    /// The grid that fits the pane; the PTY hears this one.
    size: Option<GridSize>,
    /// The PTY's grid as the server last reported it.
    shared_size: Option<GridSize>,
    sent_size: Option<GridSize>,
    /// The cell size the grid was last laid out with, for Ghostty's pixel size reports.
    cell: (Pixels, Pixels),
    running: bool,
    /// Without `terminal:operate` nothing is written to the PTY, and it is never resized.
    read_only: bool,
    exited: bool,
    /// The attach stream replied once; later attaches resume instead of opening a shell.
    attached: bool,
    client: Option<RpcClient>,
    outbox: String,
    writing: bool,
    /// Keys whose press went to the program, which hears their release too (`encodedKeyCodes`).
    encoded_keys: HashSet<String>,
    scroll_remainder: f32,
    pointer: pointer::PointerState,
    _attach: Option<Task<()>>,
    _resize: Option<Task<()>>,
    /// Shows a program's unfinished synchronized frame after a timeout.
    _synchronized: Option<Task<()>>,
    scrollbar: Scrollbar,
    focused: bool,
    /// Lit, or in the dark half of a blink.
    cursor_on: bool,
    /// Output since the last blink tick keeps the cursor lit through the next.
    cursor_held: bool,
    /// A blink loop is running.
    blinking: bool,
    _focus: [Subscription; 4],
}

/// The scrollback scrollbar: a tab stop, its track as last laid out, and where a drag holds the thumb.
struct Scrollbar {
    focus: FocusHandle,
    track: Rc<Cell<Bounds<Pixels>>>,
    grab: Option<f32>,
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

/// `MAX_COMPARED_SELECTION_CELLS`: larger selections are not formatted on every output update.
const MAX_COMPARED_SELECTION_CELLS: usize = 3_000;

/// `synchronizeTerminalOutput`: output that changes the selected text ends the selection, as
/// does any output under a selection too large to compare.
fn write_output(core: &mut Core, data: &[u8]) {
    let cells = core.selection_cells();
    let compared = cells <= MAX_COMPARED_SELECTION_CELLS;
    let before = if cells > 0 && compared { core.selection_text() } else { String::new() };
    core.write(data);
    if !compared || (cells > 0 && core.selection_text() != before) {
        core.clear_selection();
    }
}

impl TerminalSession {
    pub fn new(
        thread_id: String,
        terminal_id: String,
        launch: Launch,
        client: Option<RpcClient>,
        read_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let theme = TerminalTheme::new(crate::theme::tokens(cx));
        let core = Core::new(80, 24, &theme.core()).expect("libghostty-vt allocates a terminal");
        let focus = cx.focus_handle();
        let scrollbar = Scrollbar { focus: cx.focus_handle().tab_stop(true), track: Rc::default(), grab: None };
        let _focus = [
            cx.on_focus(&focus, window, |this, _, cx| {
                this.focused = true;
                this.cursor_on = true;
                this.blink(cx);
                cx.notify();
            }),
            cx.on_blur(&focus, window, |this, _, cx| {
                this.focused = false;
                this.cursor_on = true;
                cx.notify();
            }),
            cx.on_focus(&scrollbar.focus, window, |_, _, cx| cx.notify()),
            cx.on_blur(&scrollbar.focus, window, |_, _, cx| cx.notify()),
        ];
        let mut session = Self {
            thread_id,
            terminal_id,
            launch,
            core,
            theme,
            batched: None,
            cursor: None,
            focus,
            geometry: Cell::default(),
            size: None,
            shared_size: None,
            sent_size: None,
            cell: (px(8.), px(16.)),
            running: false,
            read_only,
            exited: false,
            attached: false,
            client: None,
            outbox: String::new(),
            writing: false,
            encoded_keys: HashSet::new(),
            scroll_remainder: 0.,
            pointer: Default::default(),
            scrollbar,
            focused: false,
            cursor_on: true,
            cursor_held: false,
            blinking: false,
            _attach: None,
            _resize: None,
            _synchronized: None,
            _focus,
        };
        if let Some(client) = client {
            session.attach(client, cx);
        }
        session
    }

    /// What the grid draws: the snapshot's runs and spans, rebatched only after Ghostty changed a
    /// row or the theme changed.
    pub fn frame(&mut self, theme: &TerminalTheme) -> Frame {
        if *theme != self.theme {
            self.theme = *theme;
            self.core.set_theme(&theme.core());
        }
        let anchor_bottom = {
            let state = self.core.scroll_state();
            state.total > state.len
        };
        let snapshot = self.core.snapshot();
        let batched = match &self.batched {
            Some((revision, cached, batched)) if *revision == snapshot.revision && cached == theme => batched.clone(),
            _ => {
                let batched = Rc::new(batch(snapshot, theme));
                self.batched = Some((snapshot.revision, *theme, batched.clone()));
                batched
            }
        };
        let cursor = CursorCell::of(snapshot);
        let (columns, rows) = (snapshot.cols, snapshot.rows.len());
        self.cursor = cursor.as_ref().map(|cursor| cursor.at);
        Frame { batched, cursor, columns, rows, anchor_bottom }
    }

    /// The link to underline, if the pointer is on one.
    pub fn hovered_link(&self) -> Option<&ScreenLink> {
        self.pointer.hovered.as_ref().map(|hovered| &hovered.link)
    }

    /// Subscribes to the terminal's output; called again after a reconnect or a grant change.
    pub fn attach(&mut self, client: RpcClient, cx: &mut Context<Self>) {
        let (method, input) =
            self.launch.stream_request(&self.thread_id, &self.terminal_id, self.read_only, self.attached);
        self.client = Some(client.clone());
        self.sent_size = None;
        let stream = client.stream(method, input);
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
                self.shared_size = GridSize::shared(snapshot).or(self.shared_size);
                self.replay(snapshot["history"].as_str().unwrap_or_default(), cx);
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
                self.feed(event["data"].as_str().unwrap_or_default(), cx);
            }
            Some("cleared") => self.replay("", cx),
            Some("activity") => {
                if let Some(shared) = GridSize::shared(event) {
                    self.shared_size = Some(shared);
                    self.apply_grid();
                }
            }
            Some("exited") => self.on_exit("Process exited", cx),
            Some("closed") => self.on_exit("Terminal closed", cx),
            Some("error") => {
                let message = event["message"].as_str().unwrap_or("Terminal error").to_owned();
                self.system_message(&message);
            }
            _ => {}
        }
    }

    fn shown_size(&self) -> GridSize {
        GridSize::shown(self.size.unwrap_or(GridSize { columns: 80, rows: 24 }), self.shared_size)
    }

    fn apply_grid(&mut self) {
        let size = self.shown_size();
        if self.core.size() != (size.columns, size.rows) {
            let (width, height) = self.cell;
            self.core.resize(size.columns, size.rows, f32::from(width), f32::from(height));
        }
    }

    /// `resetAndWrite`: retained output from a fresh start. Replayed queries are not answered,
    /// or the shell would read stale replies as typed input.
    fn replay(&mut self, history: &str, cx: &mut Context<Self>) {
        self.apply_grid();
        self.core.reset_and_write(history.as_bytes());
        self.pointer.end_drag();
        self.after_output(cx);
    }

    /// Live output, whose queries the terminal answers through the PTY.
    fn feed(&mut self, data: &str, cx: &mut Context<Self>) {
        write_output(&mut self.core, data.as_bytes());
        if let Some(replies) = self.core.take_replies() {
            self.send_input(&replies, cx);
        }
        if let Some(text) = self.core.take_clipboard() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        self.after_output(cx);
    }

    fn after_output(&mut self, cx: &mut Context<Self>) {
        self.pointer.forget_hover();
        self.cursor_on = true;
        self.cursor_held = true;
        self.blink(cx);
        self.hold_synchronized_frame(cx);
    }

    /// `deferSynchronizedRender`: while a program draws a synchronized frame the last one stays
    /// up; one that never ends is shown after a timeout.
    fn hold_synchronized_frame(&mut self, cx: &mut Context<Self>) {
        if !self.core.synchronized_output() {
            self._synchronized = None;
            return;
        }
        if self._synchronized.is_none() {
            self._synchronized = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(SYNCHRONIZED_OUTPUT_TIMEOUT).await;
                let _ = this.update(cx, |this, cx| {
                    this._synchronized = None;
                    this.core.end_synchronized_output();
                    cx.notify();
                });
            }));
        }
    }

    fn blinks(&mut self) -> bool {
        self.focused && self.core.cursor_visible() && self.core.snapshot().cursor_blinking
    }

    /// Starts the focused cursor's blink (`scheduleCursorBlink`) unless it already runs.
    fn blink(&mut self, cx: &mut Context<Self>) {
        if self.blinking || !self.blinks() {
            return;
        }
        self.blinking = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(CURSOR_BLINK).await;
                if !matches!(this.update(cx, Self::blink_tick), Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Half a blink; whether the blink goes on.
    fn blink_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let held = std::mem::take(&mut self.cursor_held);
        self.blinking = self.blinks() && !crate::desktop::snap_shot::reduced_motion();
        let on = !self.blinking || held || !self.cursor_on;
        if on != self.cursor_on {
            self.cursor_on = on;
            cx.notify();
        }
        self.blinking
    }

    fn system_message(&mut self, message: &str) {
        self.core.write_quietly(format!("\r\n[terminal] {message}\r\n").as_bytes());
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
        let Some(client) = self.client.clone().filter(|_| !self.read_only) else { return };
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
        self.core.scroll_to_bottom();
        self.core.clear_selection();
        self.pointer.end_drag();
        self.send_input(data, cx);
        cx.notify();
    }

    /// The grid that fits the pane, from the grid element's layout.
    pub fn fit(&mut self, columns: usize, rows: usize, cell: (Pixels, Pixels), cx: &mut Context<Self>) {
        let size = GridSize { columns, rows };
        if self.size == Some(size) && self.cell == cell {
            return;
        }
        self.cell = cell;
        self.size = Some(size);
        self.apply_grid();
        self._resize = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_DEBOUNCE).await;
            let _ = this.update(cx, TerminalSession::send_size);
        }));
    }

    /// Follows the environment's `terminal:operate` grant, switching between observing and
    /// attaching; the new stream's snapshot sends the size when granted.
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        if std::mem::replace(&mut self.read_only, read_only) != read_only
            && let Some(client) = self.client.clone()
        {
            self.attach(client, cx);
        }
    }

    fn send_size(&mut self, cx: &mut Context<Self>) {
        let (Some(size), Some(client)) = (self.size, self.client.clone()) else { return };
        if !self.running || self.read_only || self.sent_size == Some(size) {
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
        self.pointer.end_drag();
        self.core.select_all();
        cx.notify();
    }

    /// `scrollToBottom`: back to the live screen.
    pub fn scroll_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.core.scroll_to_bottom();
        cx.notify();
    }

    /// The selection as chat context: 1-based buffer lines (scrollback counts) and its text
    /// without surrounding blank lines (`readSelectionAction`).
    pub fn selected(&self) -> Option<Selected> {
        let text = self.core.selection_text();
        let text = text.replace("\r\n", "\n").trim_matches('\n').to_owned();
        if text.is_empty() {
            return None;
        }
        let (start, end) = self.core.selection_range()?;
        let (start, end) = (start.min(end), start.max(end));
        Some(Selected { line_start: start.y + 1, line_end: end.y + 1, text })
    }

    /// `hasSelection`: Ghostty drops a selection once output replaces its cells.
    fn has_selection(&self) -> bool {
        !self.core.selection_text().is_empty()
    }

    fn copy(&self, cx: &mut Context<Self>) {
        let text = self.core.selection_text();
        if !text.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
        let data = self.core.encode_paste(&text);
        if !data.is_empty() {
            self.input(&data, cx);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        // Keys on the focused scrollbar never reach the shell.
        if !self.focus.is_focused(window) {
            return;
        }
        let keystroke = &event.keystroke;
        // A fresh press proves a release was missed.
        if !event.is_held {
            self.encoded_keys.remove(&keystroke.key);
        }
        self.pointer.cancel_selection_menu();
        // Formatting the selection is not free; only copy chords need to know it is there.
        let alt_screen = self.core.alt_screen();
        let action = match key_action(keystroke, event.is_held, alt_screen, true) {
            Some(KeyAction::Copy { .. }) if !self.has_selection() => {
                key_action(keystroke, event.is_held, alt_screen, false)
            }
            action => action,
        };
        match action {
            None => return,
            Some(KeyAction::SelectAll) => self.select_all(cx),
            Some(KeyAction::Scroll(history)) => {
                if let Some(delta) = self.core.scroll_state().key_delta(history.key()) {
                    self.scroll_rows(delta, cx);
                }
            }
            Some(KeyAction::Send(data)) => self.input(data, cx),
            Some(KeyAction::Encode(input)) => {
                let data = self.core.encode_key(&input);
                // Text the encoder only types back is the input handler's, so IME and dead keys work.
                if data.is_empty() || Some(data.as_str()) == input.text {
                    return;
                }
                self.encoded_keys.insert(keystroke.key.clone());
                self.input(&data, cx);
            }
            Some(KeyAction::Copy { keep_selection }) => {
                self.copy(cx);
                if !keep_selection {
                    self.core.clear_selection();
                }
                cx.notify();
            }
            Some(KeyAction::Paste) => self.paste(cx),
        }
        cx.stop_propagation();
    }

    /// Kitty sessions that asked for releases hear them, for the keys whose press they heard.
    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if !self.encoded_keys.remove(&keystroke.key) {
            return;
        }
        let data = self.core.encode_key(&keys::key_input(keystroke, Press::Release, cfg!(target_os = "macos")));
        if !data.is_empty() {
            self.send_input(&data, cx);
            cx.stop_propagation();
        }
    }
}

impl TerminalSession {
    /// `scrollViewport`: `delta` rows toward the end of the scrollback, clamped.
    pub(super) fn scroll_rows(&mut self, delta: i64, cx: &mut Context<Self>) {
        let delta = self.core.scroll_state().clamp_delta(delta);
        if delta == 0 {
            return;
        }
        self.core.scroll(isize::try_from(delta).unwrap_or(0));
        self.pointer.forget_hover();
        cx.notify();
    }

    /// `onScrollbarPointerDown`: a press on the thumb holds it there; one on the track centers it.
    fn scrollbar_press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.prevent_default();
        cx.stop_propagation();
        let track = self.scrollbar.track.get();
        let Some(thumb) = scrollbar::thumb(self.core.scroll_state(), f32::from(track.size.height)) else { return };
        let at = f32::from(event.position.y - track.top());
        let on_thumb = (thumb.top..thumb.top + thumb.height).contains(&at);
        self.scrollbar.grab = Some(if on_thumb { at - thumb.top } else { thumb.height / 2. });
        self.scrollbar_drag(event.position.y, cx);
    }

    fn scrollbar_drag(&mut self, y: Pixels, cx: &mut Context<Self>) {
        let Some(grab) = self.scrollbar.grab else { return };
        let track = self.scrollbar.track.get();
        let state = self.core.scroll_state();
        let offset =
            scrollbar::offset_at_pointer(state, f32::from(track.size.height), f32::from(y - track.top()), grab);
        let delta = i64::try_from(offset).unwrap_or(i64::MAX) - i64::try_from(state.offset).unwrap_or(i64::MAX);
        self.scroll_rows(delta, cx);
    }

    /// `onScrollbarKeyDown`, and Tab out: the terminal keeps Tab for the shell.
    fn scrollbar_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "tab" if keystroke.modifiers.shift => window.focus_prev(cx),
            "tab" => window.focus_next(cx),
            key => {
                let Some(delta) = self.core.scroll_state().key_delta(key) else { return };
                self.scroll_rows(delta, cx);
            }
        }
        cx.stop_propagation();
    }

    /// The thumb over the right edge while there is scrollback (`updateScrollbar`).
    fn render_scrollbar(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let state = self.core.scroll_state();
        if state.max_offset() == 0 {
            return None;
        }
        let tokens = crate::theme::tokens(cx);
        let (rest, lit) = (tokens.scrollbar_thumb, tokens.scrollbar_thumb_hover);
        let focused = self.scrollbar.focus.is_focused(window);
        let track = self.scrollbar.track.clone();
        let session = cx.entity();
        let paint = move |bounds: Bounds<Pixels>, (): (), window: &mut Window, _: &mut gpui_kit::App| {
            let Some(thumb) = scrollbar::thumb(state, f32::from(bounds.size.height)) else { return };
            let hovered = bounds.contains(&window.mouse_position());
            let thumb = Bounds::new(
                point(bounds.left() + px(1.), bounds.top() + px(thumb.top)),
                size(bounds.size.width - px(2.), px(thumb.height)),
            );
            window.paint_quad(fill(thumb, if hovered || focused { lit } else { rest }).corner_radii(px(3.)));
            let dragged = session.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase.bubble() && event.dragging() {
                    dragged.update(cx, |this, cx| this.scrollbar_drag(event.position.y, cx));
                }
            });
            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                if phase.bubble() {
                    session.update(cx, |this, _| this.scrollbar.grab = None);
                }
            });
        };
        Some(
            div()
                .id("terminal-scrollbar")
                .absolute()
                .top(px(4.))
                .right(px(1.))
                .bottom(px(4.))
                .w(px(6.))
                .occlude()
                .cursor_default()
                // A hover style re-renders the view as the pointer enters and leaves.
                .hover(gpui_kit::Styled::cursor_default)
                .track_focus(&self.scrollbar.focus)
                .on_mouse_down(MouseButton::Left, cx.listener(Self::scrollbar_press))
                .on_key_down(cx.listener(Self::scrollbar_key))
                .child(canvas(move |bounds, _, _| track.set(bounds), paint).size_full()),
        )
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
            .on_key_up(cx.listener(Self::key_up))
            // Edit › Copy, Paste and Select All; the keys reach `key_down` instead.
            .on_action(cx.listener(|this, _: &input::Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &input::Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &input::SelectAll, _, cx| this.select_all(cx)))
            .child(TerminalGrid { session: cx.entity(), theme, focused, cursor_on: self.cursor_on })
            .children(self.render_scrollbar(window, cx))
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
        let cursor = self.cursor.filter(|cursor| cursor.y < geometry.rows)?;
        let origin =
            geometry.origin + point(geometry.cell_width * cursor.x as f32, geometry.line_height * cursor.y as f32);
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
    fn a_selection_survives_a_redraw_until_its_text_changes() {
        let mut core = super::super::core::tests::core(20, 3);
        write_output(&mut core, b"* Working |");
        let start = core.viewport_to_screen(super::super::model::ViewportPoint { x: 0, y: 0 }).unwrap();
        core.set_selection(start, super::super::model::ScreenPoint { x: 10, ..start });
        assert_eq!(core.selection_text(), "* Working |");
        // A TUI repaints its spinner line in place, the way Claude Code redraws.
        write_output(&mut core, b"\r\x1b[2K* Working |");
        assert_eq!(core.selection_text(), "* Working |");
        write_output(&mut core, b"\r\x1b[2K* Working /");
        assert_eq!(core.selection_text(), "");
        assert_eq!(core.selection_cells(), 0);
    }

    #[test]
    fn output_under_a_selection_too_large_to_compare_ends_it() {
        let mut core = super::super::core::tests::core(100, 40);
        write_output(&mut core, "x\r\n".repeat(40).as_bytes());
        core.select_all();
        assert!(core.selection_cells() > MAX_COMPARED_SELECTION_CELLS);
        write_output(&mut core, b"y");
        assert_eq!(core.selection_text(), "");
    }

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
    fn panes_draw_the_shared_grid_capped_to_their_fit() {
        let fit = GridSize { columns: 100, rows: 30 };
        assert_eq!(GridSize::shown(fit, None), fit);
        let shared = GridSize::shared(&json!({"cols": 80, "rows": 40}));
        assert_eq!(shared, Some(GridSize { columns: 80, rows: 40 }));
        assert_eq!(GridSize::shown(fit, shared), GridSize { columns: 80, rows: 30 });
        assert_eq!(GridSize::shared(&json!({"cols": 80})), None, "older servers send no grid");
    }

    #[test]
    fn read_only_panes_observe_instead_of_attaching() {
        let launch =
            Launch { cwd: "/repo".into(), worktree_path: None, project_root: "/repo".into(), stop_on_settle: None };
        let (method, input) = launch.stream_request("t", "default", true, true);
        assert_eq!((method, input), ("terminal.observe", json!({"threadId": "t", "terminalId": "default"})));
        let (method, input) = launch.stream_request("t", "default", false, false);
        assert_eq!(method, "terminal.attach");
        assert_eq!(input["cwd"], "/repo");
        assert!(input.get("resume").is_none());
        assert_eq!(launch.stream_request("t", "default", false, true).1["resume"], true);
    }
}
