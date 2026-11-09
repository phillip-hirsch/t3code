//! One terminal's libghostty-vt handles (`GhosttyTerminalCore` in `apps/web/src/terminal/ghostty/core.ts`):
//! the emulator, its render state, the key and mouse encoders, the active selection and the
//! replies the terminal writes back to the PTY. The rest of the terminal sees plain data
//! ([`Snapshot`]) and never the crate's handles, which must stay on one thread.

use std::cell::{Cell as Flag, RefCell};
use std::rc::Rc;

use libghostty_vt::error::Error;
use libghostty_vt::fmt::Format;
use libghostty_vt::key::{self, Mods};
use libghostty_vt::render::{CellIterator, Colors, CursorVisualStyle, Dirty, RowIterator};
use libghostty_vt::screen::{CellWide, GridRef, Screen, TrackedGridRef};
use libghostty_vt::selection::{FormatOptions, SelectLineOptions, SelectWordOptions, Selection};
use libghostty_vt::style::{PaletteIndex, RgbColor, Underline};
use libghostty_vt::terminal::{Mode, Point, PointCoordinate, PointSpace, ScrollViewport, SizeReportSize};
use libghostty_vt::{RenderState, Terminal, TerminalOptions, mouse, paste};

use super::model::{CursorShape, Rgb, Row, ScreenPoint, Snapshot, ViewportPoint, Width};
use super::scrollbar::ScrollState;

/// `MAX_SCROLLBACK_ROWS`.
pub(super) const MAX_SCROLLBACK: usize = 10_000;

/// The colors the app theme gives the terminal (`GhosttyTheme`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Theme {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub palette: [Rgb; 16],
}

/// Held modifiers, as both keys and the mouse report them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Modifiers {
    fn mods(self) -> Mods {
        let mut mods = Mods::empty();
        mods.set(Mods::SHIFT, self.shift);
        mods.set(Mods::CTRL, self.ctrl);
        mods.set(Mods::ALT, self.alt);
        mods.set(Mods::SUPER, self.meta);
        mods
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyAction {
    Press,
    Repeat,
    Release,
}

/// One key event for the encoder (what `encodeKey` reads off a `KeyboardEvent`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct KeyInput<'a> {
    pub action: KeyAction,
    /// GPUI's key name: `"a"`, `"enter"`, `"f5"`, `"["`.
    pub key: &'a str,
    pub mods: Modifiers,
    /// Modifiers the layout used to produce `text` (`ghosttyConsumedMods`).
    pub consumed: Modifiers,
    /// The character the key types, without Ctrl applied.
    pub text: Option<std::borrow::Cow<'a, str>>,
    /// The key's character without Shift, for Kitty's alternate keys.
    pub unshifted: Option<char>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseAction {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

/// One pointer event for the encoder (`GhosttyMouseInput`); pixels are relative to the grid
/// element's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MouseInput {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    pub mods: Modifiers,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub padding_left: f32,
    pub padding_top: f32,
    pub any_button_pressed: bool,
}

/// A word or line selection, in both coordinate spaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SelectedRange {
    pub viewport_end: ViewportPoint,
    pub start: ScreenPoint,
    pub end: ScreenPoint,
}

/// What the terminal's effect callbacks hand back to the session.
#[derive(Default)]
struct Effects {
    /// Off while retained output replays, so old queries are not answered (`resetAndWrite`).
    live: Flag<bool>,
    replies: RefCell<Vec<u8>>,
    clipboard: RefCell<Option<String>>,
    cell_px: Flag<(u32, u32)>,
}

pub(super) struct Core {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    row_iterator: RowIterator<'static>,
    cell_iterator: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    effects: Rc<Effects>,
    snapshot: Snapshot,
    /// The active selection's ends; libghostty-vt 0.2.2 cannot read the selection back.
    selection: Option<(TrackedGridRef, TrackedGridRef)>,
    /// Redraw every row next frame: Ghostty does not mark rows dirty for selection or theme changes.
    force_full: bool,
    /// The colors the rows were last resolved with; Ghostty marks no row dirty when OSC 4, 10 or 11 changes them.
    colors: Option<Colors>,
    graphemes: Vec<char>,
}

fn rgb(color: RgbColor) -> Rgb {
    Rgb { r: color.r, g: color.g, b: color.b }
}

fn ghostty_rgb(color: Rgb) -> RgbColor {
    RgbColor { r: color.r, g: color.g, b: color.b }
}

/// `blend`: faint text mixes 155/255 of its color with the background.
pub(super) fn faint(fg: Rgb, bg: Rgb) -> Rgb {
    let channel =
        |front: u8, back: u8| u8::try_from((u32::from(front) * 155 + u32::from(back) * 100) / 255).unwrap_or(u8::MAX);
    Rgb { r: channel(fg.r, bg.r), g: channel(fg.g, bg.g), b: channel(fg.b, bg.b) }
}

fn clamp_u16(value: usize) -> u16 {
    u16::try_from(value.max(1)).unwrap_or(u16::MAX)
}

fn coordinate(x: usize, y: usize) -> PointCoordinate {
    PointCoordinate { x: u16::try_from(x).unwrap_or(u16::MAX), y: u32::try_from(y).unwrap_or(u32::MAX) }
}

/// Calls `fill` with a growing buffer until it fits (the C API's out-of-space protocol).
fn read_bytes(mut fill: impl FnMut(&mut [u8]) -> Result<usize, Error>) -> Option<Vec<u8>> {
    let mut buf = vec![0; 256];
    loop {
        match fill(&mut buf) {
            Ok(len) => {
                buf.truncate(len);
                return Some(buf);
            }
            Err(Error::OutOfSpace { required }) if required > buf.len() => buf.resize(required, 0),
            Err(_) => return None,
        }
    }
}

/// GPUI's key names as Ghostty's W3C key codes (`ghosttyKeyForEvent`), with shifted US
/// symbols on the key that types them.
fn ghostty_key(name: &str) -> key::Key {
    use key::Key as K;
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        const LETTERS: [K; 26] = [
            K::A,
            K::B,
            K::C,
            K::D,
            K::E,
            K::F,
            K::G,
            K::H,
            K::I,
            K::J,
            K::K,
            K::L,
            K::M,
            K::N,
            K::O,
            K::P,
            K::Q,
            K::R,
            K::S,
            K::T,
            K::U,
            K::V,
            K::W,
            K::X,
            K::Y,
            K::Z,
        ];
        const DIGITS: [K; 10] = [
            K::Digit0,
            K::Digit1,
            K::Digit2,
            K::Digit3,
            K::Digit4,
            K::Digit5,
            K::Digit6,
            K::Digit7,
            K::Digit8,
            K::Digit9,
        ];
        let offset = |from: char| usize::try_from(u32::from(c.to_ascii_lowercase()) - u32::from(from)).unwrap_or(0);
        return match c.to_ascii_lowercase() {
            'a'..='z' => LETTERS[offset('a')],
            '0'..='9' => DIGITS[offset('0')],
            ')' => K::Digit0,
            '!' => K::Digit1,
            '@' => K::Digit2,
            '#' => K::Digit3,
            '$' => K::Digit4,
            '%' => K::Digit5,
            '^' => K::Digit6,
            '&' => K::Digit7,
            '*' => K::Digit8,
            '(' => K::Digit9,
            '`' | '~' => K::Backquote,
            '\\' | '|' => K::Backslash,
            '[' | '{' => K::BracketLeft,
            ']' | '}' => K::BracketRight,
            ',' | '<' => K::Comma,
            '=' | '+' => K::Equal,
            '-' | '_' => K::Minus,
            '.' | '>' => K::Period,
            '\'' | '"' => K::Quote,
            ';' | ':' => K::Semicolon,
            '/' | '?' => K::Slash,
            ' ' => K::Space,
            _ => K::Unidentified,
        };
    }
    match name {
        "enter" => K::Enter,
        "tab" => K::Tab,
        "space" => K::Space,
        "escape" => K::Escape,
        "backspace" => K::Backspace,
        "delete" => K::Delete,
        "insert" => K::Insert,
        "home" => K::Home,
        "end" => K::End,
        "pageup" => K::PageUp,
        "pagedown" => K::PageDown,
        "up" => K::ArrowUp,
        "down" => K::ArrowDown,
        "left" => K::ArrowLeft,
        "right" => K::ArrowRight,
        "f1" => K::F1,
        "f2" => K::F2,
        "f3" => K::F3,
        "f4" => K::F4,
        "f5" => K::F5,
        "f6" => K::F6,
        "f7" => K::F7,
        "f8" => K::F8,
        "f9" => K::F9,
        "f10" => K::F10,
        "f11" => K::F11,
        "f12" => K::F12,
        "f13" => K::F13,
        "f14" => K::F14,
        "f15" => K::F15,
        "f16" => K::F16,
        "f17" => K::F17,
        "f18" => K::F18,
        "f19" => K::F19,
        "f20" => K::F20,
        "f21" => K::F21,
        "f22" => K::F22,
        "f23" => K::F23,
        "f24" => K::F24,
        "menu" => K::ContextMenu,
        _ => K::Unidentified,
    }
}

impl Core {
    pub fn new(cols: usize, rows: usize, theme: &Theme) -> Result<Self, Error> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: clamp_u16(cols),
            rows: clamp_u16(rows),
            max_scrollback: MAX_SCROLLBACK,
        })?;
        let effects = Rc::new(Effects { live: Flag::new(true), ..Effects::default() });
        terminal
            .on_pty_write({
                let effects = effects.clone();
                move |_, data| {
                    if effects.live.get() {
                        effects.replies.borrow_mut().extend_from_slice(data);
                    }
                }
            })?
            .on_size({
                let effects = effects.clone();
                move |terminal| {
                    let (cell_width, cell_height) = effects.cell_px.get();
                    Some(SizeReportSize {
                        rows: terminal.rows().ok()?,
                        columns: terminal.cols().ok()?,
                        cell_width,
                        cell_height,
                    })
                }
            })?
            .on_clipboard_write({
                let effects = effects.clone();
                move |_, write| {
                    if effects.live.get()
                        && let Some(text) = write.contents().find(|content| content.mime.starts_with("text/plain"))
                    {
                        *effects.clipboard.borrow_mut() = Some(text.data.to_owned());
                    }
                    Ok(())
                }
            })?;
        let mut core = Self {
            terminal,
            render: RenderState::new()?,
            row_iterator: RowIterator::new()?,
            cell_iterator: CellIterator::new()?,
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            mouse_encoder: mouse::Encoder::new()?,
            mouse_event: mouse::Event::new()?,
            effects,
            snapshot: Snapshot::default(),
            selection: None,
            force_full: true,
            colors: None,
            graphemes: Vec::new(),
        };
        core.default_cursor_blink();
        core.set_theme(theme);
        Ok(core)
    }

    /// `applyDefaultCursorBlink`: the cursor blinks from the start and after `DECSCUSR 0`;
    /// an explicit `DECSCUSR` or DEC mode 12 still wins.
    fn default_cursor_blink(&mut self) {
        let _ = self.terminal.set_default_cursor_blink(Some(true));
    }

    /// Live output; any replies wait in [`Core::take_replies`].
    pub fn write(&mut self, data: &[u8]) {
        self.terminal.vt_write(data);
    }

    /// `resetAndWrite`: a full reset, then `data` replayed without answering its queries.
    pub fn reset_and_write(&mut self, data: &[u8]) {
        self.terminal.reset();
        // RIS restores the built-in steady cursor; DECSCUSR 0 takes the blinking default back up.
        self.default_cursor_blink();
        self.selection = None;
        self.force_full = true;
        self.write_quietly(b"\x1b[0 q");
        self.write_quietly(data);
    }

    /// Writes without replying to the PTY or the clipboard: replays and the app's own notices.
    pub fn write_quietly(&mut self, data: &[u8]) {
        self.effects.live.set(false);
        self.terminal.vt_write(data);
        self.effects.live.set(true);
    }

    /// What the terminal answered since the last call, for the PTY.
    pub fn take_replies(&mut self) -> Option<String> {
        let replies = std::mem::take(&mut *self.effects.replies.borrow_mut());
        (!replies.is_empty()).then(|| String::from_utf8_lossy(&replies).into_owned())
    }

    /// Text a program copied with OSC 52.
    pub fn take_clipboard(&mut self) -> Option<String> {
        self.effects.clipboard.borrow_mut().take()
    }

    pub fn size(&self) -> (usize, usize) {
        let get = |value: Result<u16, Error>| usize::from(value.unwrap_or(1));
        (get(self.terminal.cols()), get(self.terminal.rows()))
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell_width: f32, cell_height: f32) {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "whole pixels; `as` saturates")]
        let px = |size: f32| (size.round() as u32).max(1);
        let (width, height) = (px(cell_width), px(cell_height));
        self.effects.cell_px.set((width, height));
        if self.terminal.resize(clamp_u16(cols), clamp_u16(rows), width, height).is_ok() {
            self.force_full = true;
        }
    }

    /// `setTheme`: default colors and the 16 ANSI colors; programs' OSC overrides still win.
    pub fn set_theme(&mut self, theme: &Theme) {
        let terminal = &mut self.terminal;
        let _ = terminal.set_default_fg_color(Some(ghostty_rgb(theme.fg)));
        let _ = terminal.set_default_bg_color(Some(ghostty_rgb(theme.bg)));
        let _ = terminal.set_default_cursor_color(Some(ghostty_rgb(theme.cursor)));
        if let Ok(mut palette) = terminal.default_color_palette() {
            for (index, color) in (0u8..).zip(theme.palette) {
                palette.set(PaletteIndex(index), ghostty_rgb(color));
            }
            let _ = terminal.set_default_color_palette(Some(palette));
        }
        self.force_full = true;
    }

    fn mode(&self, mode: Mode) -> bool {
        self.terminal.mode(mode).unwrap_or(false)
    }

    pub fn alt_screen(&self) -> bool {
        matches!(self.terminal.active_screen(), Ok(Screen::Alternate))
    }

    pub fn app_cursor(&self) -> bool {
        self.mode(Mode::DECCKM)
    }

    pub fn mouse_tracking(&self) -> bool {
        self.terminal.is_mouse_tracking().unwrap_or(false)
    }

    /// DEC 1003: motion reports without a button held.
    pub fn any_event_mouse(&self) -> bool {
        self.mode(Mode::ANY_MOUSE)
    }

    pub fn cursor_visible(&self) -> bool {
        self.terminal.is_cursor_visible().unwrap_or(true)
    }

    /// DEC 2026: the program is drawing a frame; it is not shown until the mode ends.
    pub fn synchronized_output(&self) -> bool {
        self.mode(Mode::SYNC_OUTPUT)
    }

    /// `endSynchronizedOutput`: a frame that never finished is shown anyway.
    pub fn end_synchronized_output(&mut self) {
        let _ = self.terminal.set_mode(Mode::SYNC_OUTPUT, false);
    }

    pub fn scroll_state(&self) -> ScrollState {
        let count = |value: u64| usize::try_from(value).unwrap_or(usize::MAX);
        self.terminal.scrollbar().map_or(ScrollState { total: 0, len: 0, offset: 0 }, |bar| ScrollState {
            total: count(bar.total),
            len: count(bar.len),
            offset: count(bar.offset),
        })
    }

    /// Moves the viewport `delta` rows toward the end of the scrollback.
    pub fn scroll(&mut self, delta: isize) {
        if delta != 0 {
            self.terminal.scroll_viewport(ScrollViewport::Delta(delta));
            self.force_full = true;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
        self.force_full = true;
    }

    /// `encodeKey`, with the encoder following the terminal's modes (cursor and keypad keys,
    /// Alt prefix, modifyOtherKeys and the Kitty keyboard flags).
    pub fn encode_key(&mut self, input: &KeyInput<'_>) -> String {
        self.key_encoder
            .set_options_from_terminal(&self.terminal)
            // Alt is Meta, as in the web's WebAssembly build; composed characters arrive as consumed Alt.
            .set_macos_option_as_alt(key::OptionAsAlt::True);
        let action = match input.action {
            KeyAction::Press => key::Action::Press,
            KeyAction::Repeat => key::Action::Repeat,
            KeyAction::Release => key::Action::Release,
        };
        self.key_event
            .set_action(action)
            .set_key(ghostty_key(input.key))
            .set_mods(input.mods.mods())
            .set_consumed_mods(input.consumed.mods())
            .set_composing(false)
            .set_unshifted_codepoint(input.unshifted.unwrap_or('\0'))
            .set_utf8(input.text.as_deref().filter(|text| !text.chars().any(char::is_control)));
        let mut out = Vec::new();
        match self.key_encoder.encode_to_vec(&self.key_event, &mut out) {
            Ok(()) => String::from_utf8_lossy(&out).into_owned(),
            Err(_) => String::new(),
        }
    }

    /// `encodePaste`: bracketed when the program asked for it, with unsafe controls removed.
    pub fn encode_paste(&self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let bracketed = self.mode(Mode::BRACKETED_PASTE);
        // Windows clipboards end lines with CRLF; one line break is one Enter.
        let text = text.replace("\r\n", "\n");
        read_bytes(|buf| paste::encode(&mut text.as_bytes().to_vec(), bracketed, buf))
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    }

    /// `encodeMouse`: a report in the program's tracking mode and format, or nothing.
    pub fn encode_mouse(&mut self, input: &MouseInput) -> String {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "whole pixels; `as` saturates")]
        let px = |value: f32| value.max(0.).round() as u32;
        let (cols, rows) = self.size();
        #[expect(clippy::cast_precision_loss, reason = "a few hundred cells")]
        let used = input.padding_top + rows as f32 * input.cell_height;
        #[expect(clippy::cast_precision_loss, reason = "a few hundred cells")]
        let used_width = input.padding_left + cols as f32 * input.cell_width;
        self.mouse_encoder
            .set_options_from_terminal(&self.terminal)
            .set_size(mouse::EncoderSize {
                screen_width: px(input.width),
                screen_height: px(input.height),
                cell_width: px(input.cell_width).max(1),
                cell_height: px(input.cell_height).max(1),
                padding_top: px(input.padding_top),
                padding_bottom: px(input.height - used),
                padding_right: px(input.width - used_width),
                padding_left: px(input.padding_left),
            })
            .set_any_button_pressed(input.any_button_pressed)
            .set_track_last_cell(true);
        let action = match input.action {
            MouseAction::Press => mouse::Action::Press,
            MouseAction::Release => mouse::Action::Release,
            MouseAction::Motion => mouse::Action::Motion,
        };
        let button = input.button.map(|button| match button {
            MouseButton::Left => mouse::Button::Left,
            MouseButton::Middle => mouse::Button::Middle,
            MouseButton::Right => mouse::Button::Right,
            MouseButton::WheelUp => mouse::Button::Four,
            MouseButton::WheelDown => mouse::Button::Five,
        });
        self.mouse_event
            .set_action(action)
            .set_button(button)
            .set_mods(input.mods.mods())
            .set_position(mouse::Position { x: input.x.max(0.), y: input.y.max(0.) });
        let mut out = Vec::new();
        match self.mouse_encoder.encode_to_vec(&self.mouse_event, &mut out) {
            Ok(()) => String::from_utf8_lossy(&out).into_owned(),
            Err(_) => String::new(),
        }
    }

    fn viewport_ref(&self, point: ViewportPoint) -> Option<GridRef<'_>> {
        self.terminal.grid_ref(Point::Viewport(coordinate(point.x, point.y))).ok()
    }

    fn screen_ref(&self, point: ScreenPoint) -> Option<GridRef<'_>> {
        self.terminal.grid_ref(Point::Screen(coordinate(point.x, point.y))).ok()
    }

    fn point_of(&self, grid_ref: &GridRef<'_>, space: PointSpace) -> Option<PointCoordinate> {
        self.terminal.point_from_grid_ref(grid_ref, space).ok().flatten()
    }

    pub fn viewport_to_screen(&self, point: ViewportPoint) -> Option<ScreenPoint> {
        let grid_ref = self.viewport_ref(point)?;
        let at = self.point_of(&grid_ref, PointSpace::Screen)?;
        Some(ScreenPoint { x: usize::from(at.x), y: usize::try_from(at.y).ok()? })
    }

    /// Installs `selection` as the terminal's and remembers its ends.
    fn install(&mut self, selection: Option<(ScreenPoint, ScreenPoint)>) {
        self.force_full = true;
        let Some((start, end)) = selection else {
            let _ = self.terminal.set_selection(None);
            self.selection = None;
            return;
        };
        let track = |point: ScreenPoint| self.terminal.track_grid_ref(Point::Screen(coordinate(point.x, point.y))).ok();
        let (Some(start_ref), Some(end_ref), Some(tracked_start), Some(tracked_end)) =
            (self.screen_ref(start), self.screen_ref(end), track(start), track(end))
        else {
            return;
        };
        let _ = self.terminal.set_selection(Some(&Selection::new(start_ref, end_ref, false)));
        self.selection = Some((tracked_start, tracked_end));
    }

    /// `setSelection`: from `anchor` to `end`, both inclusive, in screen rows.
    pub fn set_selection(&mut self, anchor: ScreenPoint, end: ScreenPoint) {
        self.install(Some((anchor, end)));
    }

    pub fn clear_selection(&mut self) {
        self.install(None);
    }

    /// The ends of a selection Ghostty derived, ordered as it gave them.
    fn ends(&self, selection: &Selection<'_>) -> Option<(ScreenPoint, ScreenPoint, ViewportPoint)> {
        let screen = |grid_ref: &GridRef<'_>| {
            let at = self.point_of(grid_ref, PointSpace::Screen)?;
            Some(ScreenPoint { x: usize::from(at.x), y: usize::try_from(at.y).ok()? })
        };
        let (start, end) = (selection.start(), selection.end());
        let viewport_end = self
            .point_of(&end, PointSpace::Viewport)
            .and_then(|at| Some(ViewportPoint { x: usize::from(at.x), y: usize::try_from(at.y).ok()? }))
            .unwrap_or_default();
        Some((screen(&start)?, screen(&end)?, viewport_end))
    }

    /// `selectWord` / `selectLine`: Ghostty's word or line around the cell, made active.
    pub fn select_around(&mut self, point: ViewportPoint, line: bool) -> Option<SelectedRange> {
        let grid_ref = self.viewport_ref(point)?;
        let selection = if line {
            self.terminal.select_line(SelectLineOptions::new(grid_ref))
        } else {
            self.terminal.select_word(SelectWordOptions::new(grid_ref))
        };
        let (start, end, viewport_end) = self.ends(selection.ok().flatten().as_ref()?)?;
        self.install(Some((start, end)));
        Some(SelectedRange { viewport_end, start, end })
    }

    /// `selectAll`: every line, scrollback included.
    pub fn select_all(&mut self) -> Option<(ScreenPoint, ScreenPoint)> {
        let selection = self.terminal.select_all().ok().flatten()?;
        let (start, end, _) = self.ends(&selection)?;
        self.install(Some((start, end)));
        Some((start, end))
    }

    /// `selectionText`: plain text with soft wraps joined and trailing blanks trimmed, as Ghostty
    /// copies it; empty once output replaced the selected cells.
    pub fn selection_text(&self) -> String {
        let options = FormatOptions::new().with_emit_format(Format::Plain).with_unwrap(true).with_trim(true);
        match self.terminal.format_selection_alloc(None, options) {
            Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
            _ => String::new(),
        }
    }

    /// `selectionRange`: the selection's ends in screen rows, following output that scrolled them.
    pub fn selection_range(&self) -> Option<(ScreenPoint, ScreenPoint)> {
        let (start, end) = self.selection.as_ref()?;
        let point = |tracked: &TrackedGridRef| {
            let at = tracked.point(PointSpace::Screen).ok().flatten()?;
            Some(ScreenPoint { x: usize::from(at.x), y: usize::try_from(at.y).ok()? })
        };
        Some((point(start)?, point(end)?))
    }

    /// `getSelectionCellCount`: an upper bound on the selection's size, without formatting it.
    pub fn selection_cells(&self) -> usize {
        self.selection_range().map_or(0, |(start, end)| (start.y.abs_diff(end.y) + 1) * self.size().0)
    }

    /// `hyperlinkAt`: the OSC 8 link on a visible cell.
    pub fn hyperlink_at(&self, point: ViewportPoint) -> Option<String> {
        let grid_ref = self.viewport_ref(point)?;
        let bytes = read_bytes(|buf| grid_ref.hyperlink_uri(buf))?;
        (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `snapshot`: the visible screen, re-reading only rows Ghostty marked dirty. A program's
    /// synchronized frame keeps the last one until it ends.
    pub fn snapshot(&mut self) -> &Snapshot {
        if !self.synchronized_output() {
            let _ = self.update_snapshot();
        }
        &self.snapshot
    }

    fn update_snapshot(&mut self) -> Result<(), Error> {
        let Self {
            terminal, render, row_iterator, cell_iterator, snapshot, force_full, colors: shown, graphemes, ..
        } = self;
        let frame = render.update(terminal)?;
        let dirty = frame.dirty()?;
        let colors = frame.colors()?;
        let (fg, bg) = (rgb(colors.foreground), rgb(colors.background));
        let recolored = shown.as_ref() != Some(&colors);
        snapshot.cursor_color = colors.cursor.map_or(fg, rgb);
        if recolored {
            *shown = Some(colors);
        }
        let cols = usize::from(frame.cols()?);
        let rows = usize::from(frame.rows()?);
        snapshot.fg = fg;
        snapshot.bg = bg;
        snapshot.cursor = if frame.cursor_visible()? {
            frame.cursor_viewport()?.map(|at| ViewportPoint { x: usize::from(at.x), y: usize::from(at.y) })
        } else {
            None
        };
        snapshot.cursor_blinking = frame.cursor_blinking()?;
        snapshot.cursor_shape = match frame.cursor_visual_style()? {
            CursorVisualStyle::Bar => CursorShape::Bar,
            CursorVisualStyle::Underline => CursorShape::Underline,
            CursorVisualStyle::BlockHollow => CursorShape::BlockHollow,
            _ => CursorShape::Block,
        };
        let mut full = std::mem::take(force_full) || recolored || dirty == Dirty::Full;
        if snapshot.cols != cols || snapshot.rows.len() != rows {
            snapshot.cols = cols;
            snapshot.rows.resize_with(rows, Row::default);
            full = true;
        }
        if full || dirty != Dirty::Clean {
            let mut iteration = row_iterator.update(&frame)?;
            let mut index = 0;
            while index < rows
                && let Some(row) = iteration.next()
            {
                if full || row.dirty()? {
                    let target = &mut snapshot.rows[index];
                    let raw = row.raw_row()?;
                    target.wrap_continuation = raw.is_wrap_continuation()?;
                    target.wraps_to_next = raw.is_wrapped()?;
                    let selected = row.selection()?.map(|range| (usize::from(range.start_x), usize::from(range.end_x)));
                    target.cells.resize_with(cols, Default::default);
                    let mut cells = cell_iterator.update(row)?;
                    let mut column = 0;
                    while column < cols
                        && let Some(cell) = cells.next()
                    {
                        let out = &mut target.cells[column];
                        let style = cell.style()?;
                        let mut cell_fg = cell.fg_color()?.map_or(fg, rgb);
                        let mut cell_bg = cell.bg_color()?.map_or(bg, rgb);
                        if style.inverse {
                            std::mem::swap(&mut cell_fg, &mut cell_bg);
                        }
                        if style.faint {
                            cell_fg = faint(cell_fg, cell_bg);
                        }
                        out.text.clear();
                        let len = cell.graphemes_len()?;
                        if len > 0 {
                            graphemes.resize(len, '\0');
                            cell.graphemes_buf(graphemes)?;
                            out.text.extend(graphemes.iter());
                        }
                        out.wide = match cell.raw_cell()?.wide()? {
                            CellWide::Wide => Width::Wide,
                            CellWide::SpacerTail => Width::SpacerTail,
                            CellWide::SpacerHead => Width::SpacerHead,
                            CellWide::Narrow => Width::Narrow,
                        };
                        out.fg = cell_fg;
                        out.bg = cell_bg;
                        out.bold = style.bold;
                        out.italic = style.italic;
                        out.invisible = style.invisible;
                        out.strikethrough = style.strikethrough;
                        out.overline = style.overline;
                        out.underline = style.underline != Underline::None;
                        out.selected = selected.is_some_and(|(first, last)| (first..=last).contains(&column));
                        column += 1;
                    }
                    for blank in &mut target.cells[column..] {
                        *blank = super::model::Cell { fg, bg, ..Default::default() };
                    }
                    row.set_dirty(false)?;
                    snapshot.revision += 1;
                }
                index += 1;
            }
            frame.set_dirty(Dirty::Clean)?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::app::terminal) fn theme() -> Theme {
        let palette = std::array::from_fn(|index| Rgb { r: u8::try_from(index).unwrap(), g: 1, b: 2 });
        Theme {
            fg: Rgb { r: 255, g: 255, b: 255 },
            bg: Rgb::default(),
            cursor: Rgb { r: 255, g: 255, b: 255 },
            palette,
        }
    }

    pub(in crate::app::terminal) fn core(cols: usize, rows: usize) -> Core {
        Core::new(cols, rows, &theme()).unwrap()
    }

    fn row_text(core: &mut Core, row: usize) -> String {
        core.snapshot().rows[row].cells.iter().map(|cell| if cell.text.is_empty() { " " } else { &cell.text }).collect()
    }

    #[test]
    fn styles_wide_cells_and_selection_reach_the_snapshot() {
        let mut core = core(20, 3);
        let grapheme = format!("e{}", "\u{301}".repeat(64));
        core.write(format!("\x1b[1;3;4;8;9;53;38;2;123;45;67;48;2;9;8;7m{grapheme}\x1b[0m界🙂").as_bytes());
        let cells = core.snapshot().rows[0].cells.clone();
        let first = &cells[0];
        assert_eq!(first.text, grapheme);
        assert_eq!((first.fg, first.bg), (Rgb { r: 123, g: 45, b: 67 }, Rgb { r: 9, g: 8, b: 7 }));
        assert!(first.bold && first.italic && first.invisible && first.strikethrough && first.overline);
        assert!(first.underline && !first.selected);
        let wides: Vec<_> = cells[1..5].iter().map(|cell| (cell.text.as_str(), cell.wide)).collect();
        assert_eq!(wides, [("界", Width::Wide), ("", Width::SpacerTail), ("🙂", Width::Wide), ("", Width::SpacerTail)]);

        let start = core.viewport_to_screen(ViewportPoint { x: 0, y: 0 }).unwrap();
        core.set_selection(start, ScreenPoint { x: 2, y: start.y });
        assert!(core.snapshot().rows[0].cells[0].selected);
        core.clear_selection();
        assert!(!core.snapshot().rows[0].cells[0].selected);

        // Faint blends toward the background after inverse swapped the two.
        core.reset_and_write(b"\x1b[2;7;38;2;40;100;200;48;2;12;34;56mC\x1b[0m");
        let cell = &core.snapshot().rows[0].cells[0];
        assert_eq!(
            (cell.text.as_str(), cell.fg, cell.bg),
            ("C", Rgb { r: 22, g: 59, b: 112 }, Rgb { r: 40, g: 100, b: 200 })
        );
        assert!(!cell.bold && !cell.underline);
    }

    #[test]
    fn foreground_and_background_resolve_independently() {
        let mut core = core(20, 3);
        core.write(b"\x1b[38;2;123;45;67mF\x1b[39;48;2;9;8;7mB\x1b[0mD\x1b[38;2;12;34;56;48;2;78;90;123mC\x1b[0mE");
        let (white, black) = (Rgb { r: 255, g: 255, b: 255 }, Rgb::default());
        let cells: Vec<_> = core.snapshot().rows[0].cells[..6].iter().map(|c| (c.text.clone(), c.fg, c.bg)).collect();
        assert_eq!(
            cells,
            [
                ("F".into(), Rgb { r: 123, g: 45, b: 67 }, black),
                ("B".into(), white, Rgb { r: 9, g: 8, b: 7 }),
                ("D".into(), white, black),
                ("C".into(), Rgb { r: 12, g: 34, b: 56 }, Rgb { r: 78, g: 90, b: 123 }),
                ("E".into(), white, black),
                (String::new(), white, black),
            ]
        );
    }

    #[test]
    fn ansi_colors_follow_the_theme_palette_and_repaint_on_change() {
        let mut core = core(20, 3);
        core.write(b"\x1b[31mR\x1b[38;5;196mX\x1b[0m");
        let fg = |core: &mut Core, column: usize| core.snapshot().rows[0].cells[column].fg;
        assert_eq!(fg(&mut core, 0), Rgb { r: 1, g: 1, b: 2 });
        assert_eq!(fg(&mut core, 1), Rgb { r: 255, g: 0, b: 0 });
        let mut changed = theme();
        changed.palette = changed.palette.map(|color| Rgb { g: 9, ..color });
        core.set_theme(&changed);
        assert_eq!(fg(&mut core, 0), Rgb { r: 1, g: 9, b: 2 });
        assert_eq!(fg(&mut core, 1), Rgb { r: 255, g: 0, b: 0 });
    }

    #[test]
    fn live_queries_reply_but_replayed_ones_do_not() {
        let mut core = core(20, 3);
        core.write(b"\x1b[5n");
        assert_eq!(core.take_replies().as_deref(), Some("\x1b[0n"));
        assert_eq!(core.take_replies(), None);
        core.reset_and_write(b"old\x1b[5n\x1b[c");
        assert_eq!(core.take_replies(), None);
        core.write_quietly(b"\x1b[6n");
        assert_eq!(core.take_replies(), None);
        core.write(b"\x1b[6n");
        assert!(core.take_replies().is_some_and(|reply| reply.ends_with('R')));
        assert_eq!(row_text(&mut core, 0).trim_end(), "old");
    }

    #[test]
    fn only_changed_rows_are_reread() {
        let mut core = core(10, 3);
        core.write(b"one\r\ntwo");
        let revision = core.snapshot().revision;
        assert_eq!(core.snapshot().revision, revision, "a clean frame rebuilds nothing");
        core.write(b"!");
        assert_eq!(core.snapshot().revision, revision + 1, "one row changed");
        assert_eq!(row_text(&mut core, 1).trim_end(), "two!");
    }

    #[test]
    fn a_synchronized_frame_shows_once_it_ends_or_times_out() {
        let mut core = core(10, 2);
        core.write(b"before");
        core.snapshot();
        core.write(b"\x1b[?2026h\rafter ");
        assert_eq!(row_text(&mut core, 0).trim_end(), "before");
        core.end_synchronized_output();
        assert_eq!(row_text(&mut core, 0).trim_end(), "after");
    }

    #[test]
    fn resize_keeps_scrollback_within_its_bound() {
        let mut core = core(10, 3);
        for line in 0..(MAX_SCROLLBACK + 50) {
            core.write(format!("{line}\r\n").as_bytes());
        }
        let state = core.scroll_state();
        assert_eq!(state.len, 3);
        assert!(state.total > MAX_SCROLLBACK / 2 && state.total <= MAX_SCROLLBACK + 3, "{state:?}");
        assert_eq!(state.offset, state.total - state.len, "the live screen is shown");
        core.scroll(-5);
        assert_eq!(core.scroll_state().offset, state.offset - 5);
        core.resize(20, 5, 8., 16.);
        assert_eq!(core.size(), (20, 5));
        assert_eq!(core.snapshot().rows.len(), 5);
        core.scroll_to_bottom();
        let state = core.scroll_state();
        assert_eq!(state.offset, state.total - state.len);
    }

    #[test]
    fn the_cursor_blinks_until_the_program_asks_for_a_steady_one() {
        let mut core = core(10, 2);
        assert!(core.snapshot().cursor_blinking);
        core.write(b"\x1b[2 q");
        assert!(!core.snapshot().cursor_blinking);
        core.write(b"\x1b[0 q");
        assert!(core.snapshot().cursor_blinking);
        core.write(b"\x1b[6 q");
        assert_eq!(core.snapshot().cursor_shape, CursorShape::Bar);
        core.reset_and_write(b"");
        assert!(core.snapshot().cursor_blinking, "a reset restores the blinking default");
    }

    #[test]
    fn a_selection_follows_output_that_scrolls_it() {
        let mut core = core(20, 5);
        let rows: Vec<String> = (0..10).map(|index| format!("row {index}")).collect();
        core.write(format!("\x1b[?1049h\x1b[H{}", rows.join("\r\n")).as_bytes());
        let start = core.viewport_to_screen(ViewportPoint { x: 0, y: 2 }).unwrap();
        core.set_selection(start, ScreenPoint { x: 4, y: start.y });
        let text = core.selection_text();
        assert!(text.starts_with("row "), "{text}");
        let before = core.selection_range().unwrap();
        core.write(b"\r\nrow 10\r\nrow 11");
        assert_eq!(core.selection_text(), text);
        assert_eq!(core.selection_range().unwrap().0.y + 2, before.0.y);
    }

    #[test]
    fn select_all_covers_the_alternate_screen_until_it_closes() {
        let mut core = core(20, 3);
        core.write(b"history\r\n\x1b[?1049happ");
        assert!(core.alt_screen());
        core.select_all();
        assert_eq!(core.selection_text(), "app");
        core.write(b"\x1b[?1049l");
        assert!(!core.alt_screen());
        assert_eq!(core.selection_text(), "");
        core.select_all();
        assert_eq!(core.selection_text(), "history");
    }

    #[test]
    fn words_and_lines_select_around_a_cell() {
        let mut core = core(30, 3);
        core.write(b"git status --short");
        let word = core.select_around(ViewportPoint { x: 6, y: 0 }, false).unwrap();
        assert_eq!(core.selection_text(), "status");
        assert_eq!((word.start.x, word.end.x, word.viewport_end), (4, 9, ViewportPoint { x: 9, y: 0 }));
        core.select_around(ViewportPoint { x: 1, y: 0 }, true);
        assert_eq!(core.selection_text(), "git status --short");
    }

    #[test]
    fn hyperlinks_are_read_from_their_cells() {
        let mut core = core(30, 2);
        core.write(b"see \x1b]8;;https://t3.codes\x1b\\link\x1b]8;;\x1b\\ end");
        assert_eq!(core.hyperlink_at(ViewportPoint { x: 5, y: 0 }).as_deref(), Some("https://t3.codes"));
        assert_eq!(core.hyperlink_at(ViewportPoint { x: 1, y: 0 }), None);
    }

    #[test]
    fn paste_brackets_when_asked_and_otherwise_sends_carriage_returns() {
        let mut core = core(20, 2);
        assert_eq!(core.encode_paste("a\r\nb\nc"), "a\rb\rc");
        core.write(b"\x1b[?2004h");
        let pasted = core.encode_paste("ls\x1b[201~\n");
        assert!(pasted.starts_with("\x1b[200~ls") && pasted.ends_with("\x1b[201~"), "{pasted:?}");
        assert_eq!(pasted.matches("\x1b[201~").count(), 1, "the payload cannot end the paste: {pasted:?}");
    }
}
