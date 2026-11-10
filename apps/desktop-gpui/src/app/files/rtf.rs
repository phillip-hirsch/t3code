//! Rich text documents (`openRtf` in `extendedDocumentPreview.ts`, shown by `DocumentPages`):
//! the text with its bold, italic and underline, line breaks and page breaks. Fonts, colors,
//! pictures, fields' instructions and other hidden destinations are left out.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use encoding_rs::Encoding;
use gpui_kit::assets::IconName;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, FontStyle, FontWeight, HighlightStyle, InteractiveElement as _, IntoElement,
    ListAlignment, ListState, ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    StyledText, UnderlineStyle, div, list, px, relative,
};

use super::{FileKey, chrome};
use crate::app::Workspace;
use crate::app::attachments::format_size;
use crate::theme::Tokens;

pub(super) const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CONTENT: usize = 8 * 1024 * 1024;
const MAX_PAGES: usize = 500;
const MAX_DEPTH: usize = 100;
const TOO_MUCH: &str = "This rich text document has too much content to preview.";
const DAMAGED: &str = "This rich text document is damaged.";

/// Destinations whose text is never shown.
const HIDDEN: &[&str] = &[
    "colortbl",
    "stylesheet",
    "info",
    "pict",
    "object",
    "fldinst",
    "header",
    "headerl",
    "headerr",
    "headerf",
    "footer",
    "footerl",
    "footerr",
    "footerf",
    "filetbl",
    "datastore",
    "themedata",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

#[derive(Debug, PartialEq)]
pub(super) struct Run {
    pub text: String,
    pub style: Style,
}

/// A page's lines, split where the document breaks the line (the web's `<br>`).
pub(super) type Page = Vec<Vec<Run>>;

/// The formatting a group inherits and restores when it closes.
#[derive(Clone, Copy)]
struct State {
    skip: bool,
    style: Style,
    uc: usize,
    font: i64,
    font_table: bool,
    encoding: &'static Encoding,
}

struct Parser {
    state: State,
    stack: Vec<State>,
    fonts: HashMap<i64, &'static Encoding>,
    default_font: i64,
    pages: Vec<Page>,
    lines: Page,
    run: String,
    /// The run's length once escaped, in UTF-16 units like the web's markup string.
    run_length: usize,
    /// Raw and `\'hh` bytes awaiting the current font's code page.
    encoded: Vec<u8>,
    /// `\uc` fallback characters still to drop after a `\u`.
    fallback: usize,
    /// A `\u` high surrogate awaiting its low half.
    high: Option<u16>,
    /// Markup kept so far (escaped text, style tags and `<br>`s), bounded like the web's.
    length: usize,
}

impl Parser {
    fn new() -> Self {
        Self {
            state: State {
                skip: false,
                style: Style::default(),
                uc: 1,
                font: 0,
                font_table: false,
                encoding: encoding_rs::WINDOWS_1252,
            },
            stack: Vec::new(),
            fonts: HashMap::new(),
            default_font: 0,
            pages: Vec::new(),
            lines: vec![Vec::new()],
            run: String::new(),
            run_length: 0,
            encoded: Vec::new(),
            fallback: 0,
            high: None,
            length: 0,
        }
    }

    fn bound(&self) -> Result<(), String> {
        if self.length + self.run_length > MAX_CONTENT { Err(TOO_MUCH.into()) } else { Ok(()) }
    }

    fn push(&mut self, text: &str) {
        self.run.push_str(text);
        self.run_length += text
            .chars()
            .map(|c| match c {
                '&' => 5,
                '<' | '>' => 4,
                '"' => 6,
                c => c.len_utf16(),
            })
            .sum::<usize>();
    }

    fn push_char(&mut self, c: char) {
        self.push(c.encode_utf8(&mut [0; 4]));
    }

    /// A high surrogate no low half followed shows as U+FFFD.
    fn settle(&mut self) {
        if self.high.take().is_some() {
            self.push_char(char::REPLACEMENT_CHARACTER);
        }
    }

    /// Ends the current run: pending bytes decode with the font's code page, and the text keeps
    /// the formatting it was written under.
    fn flush(&mut self) -> Result<(), String> {
        if !self.encoded.is_empty() {
            self.settle();
            let encoding = self.fonts.get(&self.state.font).copied().unwrap_or(self.state.encoding);
            let mut encoded = std::mem::take(&mut self.encoded);
            self.push(&encoding.decode_with_bom_removal(&encoded).0);
            encoded.clear();
            self.encoded = encoded;
        }
        self.bound()?;
        if self.run.is_empty() {
            return Ok(());
        }
        let style = self.state.style;
        let tags = [(style.bold, "<strong></strong>"), (style.italic, "<em></em>"), (style.underline, "<u></u>")];
        self.length += std::mem::take(&mut self.run_length)
            + tags.iter().filter(|(on, _)| *on).map(|(_, tag)| tag.len()).sum::<usize>();
        let text = std::mem::take(&mut self.run);
        let line = self.lines.last_mut().expect("a page has a line");
        line.push(Run { text, style });
        Ok(())
    }

    fn emit(&mut self, text: &str) -> Result<(), String> {
        if self.fallback > 0 {
            self.fallback -= 1;
            return Ok(());
        }
        if self.state.skip {
            return Ok(());
        }
        self.settle();
        self.push(text);
        self.bound()
    }

    /// `\u`: one UTF-16 unit, paired with the next when it is a surrogate.
    fn emit_unit(&mut self, unit: u16) -> Result<(), String> {
        if self.state.skip {
            return Ok(());
        }
        match unit {
            0xD800..=0xDBFF => {
                self.settle();
                self.high = Some(unit);
            }
            0xDC00..=0xDFFF => {
                let pair = self.high.take().map_or(vec![unit], |high| vec![high, unit]);
                for c in char::decode_utf16(pair) {
                    self.push_char(c.unwrap_or(char::REPLACEMENT_CHARACTER));
                }
            }
            _ => {
                self.settle();
                if let Some(c) = char::from_u32(unit.into()) {
                    self.push_char(c);
                }
            }
        }
        self.bound()
    }

    fn emit_byte(&mut self, byte: u8) {
        if self.fallback > 0 {
            self.fallback -= 1;
        } else if !self.state.skip {
            self.encoded.push(byte);
        }
    }

    /// `\par`, `\line`, `\row` and a backslash before a line break.
    fn line_break(&mut self) -> Result<(), String> {
        if self.state.skip {
            return Ok(());
        }
        self.settle();
        self.flush()?;
        self.length += 4;
        self.lines.push(Vec::new());
        self.bound()
    }

    fn finish_page(&mut self) -> Result<(), String> {
        self.settle();
        self.flush()?;
        if self.pages.len() >= MAX_PAGES {
            return Err("Rich text preview limited to 500 pages.".into());
        }
        let lines = std::mem::replace(&mut self.lines, vec![Vec::new()]);
        self.pages.push(lines);
        Ok(())
    }

    fn page_is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    fn control(&mut self, word: &str, arg: Option<i64>, source: &[u8], at: &mut usize) -> Result<(), String> {
        let value = arg.unwrap_or(1);
        match word {
            "fonttbl" => {
                self.state.font_table = true;
                self.state.skip = true;
            }
            "deff" => {
                self.state.font = value;
                self.default_font = value;
            }
            "f" => self.state.font = value,
            "fcharset" if self.state.font_table => {
                let label = match value {
                    0 | 1 => Some(self.state.encoding),
                    77 => Some(encoding_rs::MACINTOSH),
                    128 => Some(encoding_rs::SHIFT_JIS),
                    129 => Some(encoding_rs::EUC_KR),
                    134 => Some(encoding_rs::GBK),
                    136 => Some(encoding_rs::BIG5),
                    161 => Some(encoding_rs::WINDOWS_1253),
                    162 => Some(encoding_rs::WINDOWS_1254),
                    163 => Some(encoding_rs::WINDOWS_1258),
                    177 => Some(encoding_rs::WINDOWS_1255),
                    178 => Some(encoding_rs::WINDOWS_1256),
                    186 => Some(encoding_rs::WINDOWS_1257),
                    204 => Some(encoding_rs::WINDOWS_1251),
                    222 => Some(encoding_rs::WINDOWS_874),
                    238 => Some(encoding_rs::WINDOWS_1250),
                    255 => Some(encoding_rs::IBM866),
                    _ => None,
                };
                if let Some(encoding) = label {
                    self.fonts.insert(self.state.font, encoding);
                }
            }
            word if HIDDEN.contains(&word) => self.state.skip = true,
            "bin" => {
                let length = usize::try_from(value).ok().filter(|&n| n <= source.len() - *at);
                *at += length.ok_or("Invalid rich text binary data.")?;
            }
            "ansicpg" => {
                let label = match value {
                    65001 => "utf-8".to_owned(),
                    932 => "shift_jis".to_owned(),
                    936 => "gbk".to_owned(),
                    949 => "euc-kr".to_owned(),
                    950 => "big5".to_owned(),
                    874 => "windows-874".to_owned(),
                    other => format!("windows-{other}"),
                };
                self.state.encoding = Encoding::for_label_no_replacement(label.as_bytes())
                    .ok_or("This rich text code page is not supported.")?;
            }
            "b" => self.state.style.bold = value != 0,
            "i" => self.state.style.italic = value != 0,
            "ul" => self.state.style.underline = value != 0,
            "ulnone" => self.state.style.underline = false,
            "plain" => {
                self.state.style = Style::default();
                self.state.font = self.default_font;
            }
            "uc" => self.state.uc = usize::try_from(value.clamp(0, 10)).unwrap_or_default(),
            "u" => {
                self.fallback = 0;
                // `String.fromCharCode` keeps the low 16 bits.
                self.emit_unit(u16::try_from(value.rem_euclid(0x1_0000)).unwrap_or(0xFFFD))?;
                self.fallback = self.state.uc;
            }
            "page" if !self.state.skip => self.finish_page()?,
            "par" | "line" | "row" => self.line_break()?,
            "tab" | "cell" => self.emit("\t")?,
            "endash" => self.emit("\u{2013}")?,
            "emdash" => self.emit("\u{2014}")?,
            "bullet" => self.emit("\u{2022}")?,
            "lquote" => self.emit("\u{2018}")?,
            "rquote" => self.emit("\u{2019}")?,
            "ldblquote" => self.emit("\u{201c}")?,
            "rdblquote" => self.emit("\u{201d}")?,
            _ => {}
        }
        Ok(())
    }
}

/// A control word at `source[at..]` (`[a-z]+(-?\d+)? ?`): the word, its argument and the bytes it spans.
fn control_word(source: &[u8], at: usize) -> Option<(&str, Option<i64>, usize)> {
    let rest = &source[at..];
    let letters = rest.iter().take_while(|b| b.is_ascii_alphabetic()).count();
    if letters == 0 {
        return None;
    }
    let word = std::str::from_utf8(&rest[..letters]).ok()?;
    let mut end = letters;
    let negative = rest.get(end) == Some(&b'-') && rest.get(end + 1).is_some_and(u8::is_ascii_digit);
    if negative {
        end += 1;
    }
    let digits = rest[end..].iter().take_while(|b| b.is_ascii_digit()).count();
    let arg = (digits > 0).then(|| {
        let magnitude =
            rest[end..end + digits].iter().fold(0i64, |n, d| n.saturating_mul(10).saturating_add(i64::from(d - b'0')));
        if negative { -magnitude } else { magnitude }
    });
    end += digits;
    if rest.get(end) == Some(&b' ') {
        end += 1;
    }
    Some((word, arg, end))
}

/// `openRtf`: each page's lines of formatted text.
pub(super) fn parse(source: &[u8]) -> Result<Vec<Page>, String> {
    if source.len() as u64 > MAX_BYTES {
        return Err("Rich text preview limited to files under 8 MB.".into());
    }
    if !source.starts_with(b"{\\rtf") {
        return Err("This is not a valid rich text document.".into());
    }
    let mut parser = Parser::new();
    let mut at = 0;
    while at < source.len() {
        let byte = source[at];
        at += 1;
        match byte {
            b'{' => {
                parser.flush()?;
                parser.fallback = 0;
                if parser.stack.len() > MAX_DEPTH {
                    return Err("This rich text document is nested too deeply.".into());
                }
                parser.stack.push(parser.state);
            }
            b'}' => {
                parser.flush()?;
                parser.fallback = 0;
                parser.state = parser.stack.pop().ok_or(DAMAGED)?;
            }
            b'\\' => {
                let next = source.get(at).copied();
                if next == Some(b'\'') {
                    let hex = source.get(at + 1..at + 3).and_then(|h| std::str::from_utf8(h).ok());
                    let value = hex
                        .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                        .ok_or("Invalid rich text byte escape.")?;
                    at += 3;
                    parser.emit_byte(value);
                    continue;
                }
                parser.flush()?;
                match next {
                    Some(symbol @ (b'\\' | b'{' | b'}')) => {
                        at += 1;
                        parser.emit(char::from(symbol).encode_utf8(&mut [0; 4]))?;
                        continue;
                    }
                    Some(b'*') => {
                        parser.state.skip = true;
                        at += 1;
                        continue;
                    }
                    _ => {}
                }
                let Some((word, arg, length)) = control_word(source, at) else {
                    match next {
                        Some(b'~') => parser.emit("\u{a0}")?,
                        // A backslash before a line break is \par; TextEdit ends paragraphs this way.
                        Some(b'\n' | b'\r') => parser.line_break()?,
                        _ => {}
                    }
                    at += 1;
                    continue;
                };
                at += length;
                parser.control(word, arg, source, &mut at)?;
            }
            b'\r' | b'\n' => {}
            other => parser.emit_byte(other),
        }
    }
    if !parser.stack.is_empty() {
        return Err(DAMAGED.into());
    }
    parser.settle();
    parser.flush()?;
    if !parser.page_is_empty() || parser.pages.is_empty() {
        parser.finish_page()?;
    }
    Ok(parser.pages)
}

/// One displayed line: its text with HTML whitespace collapsed, and its formatted ranges.
#[derive(Debug, PartialEq)]
pub(super) struct Line {
    text: SharedString,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
}

fn highlight(style: Style) -> Option<HighlightStyle> {
    (style != Style::default()).then(|| HighlightStyle {
        font_weight: style.bold.then_some(FontWeight::BOLD),
        font_style: style.italic.then_some(FontStyle::Italic),
        underline: style.underline.then_some(UnderlineStyle { thickness: px(1.), color: None, wavy: false }),
        ..Default::default()
    })
}

/// A page's lines as the web's `<p>` lays them out: runs of spaces, tabs and line feeds show as
/// one space and none at a line's ends, a closing break adds no line, and an empty page has none.
fn layout(page: &Page) -> Vec<Line> {
    let mut lines: Vec<Line> = page
        .iter()
        .map(|runs| {
            let mut text = String::new();
            let mut highlights = Vec::new();
            let mut space = false;
            for run in runs {
                let mut start = None;
                for c in run.text.chars() {
                    if matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                        space = !text.is_empty();
                        continue;
                    }
                    if std::mem::take(&mut space) {
                        text.push(' ');
                    }
                    start.get_or_insert(text.len());
                    text.push(c);
                }
                if let (Some(start), Some(style)) = (start, highlight(run.style)) {
                    highlights.push((start..text.len(), style));
                }
            }
            Line { text: text.into(), highlights }
        })
        .collect();
    if lines.last().is_some_and(|line| line.text.is_empty()) {
        lines.pop();
    }
    lines
}

/// The document's pages, parsed and laid out off the UI thread.
pub(super) fn open_rtf(bytes: &[u8]) -> Result<Vec<Vec<Line>>, String> {
    Ok(parse(bytes)?.iter().map(layout).collect())
}

/// Reading size in quarters: 50% to 200%, as `DocumentPages` steps it.
const ZOOM_MIN: u8 = 2;
const ZOOM_DEFAULT: u8 = 4;
const ZOOM_MAX: u8 = 8;

pub(super) struct RtfDoc {
    pages: Vec<Rc<[Line]>>,
    size: u64,
    page: usize,
    zoom: u8,
    list: ListState,
}

impl RtfDoc {
    pub(super) fn new(pages: Vec<Vec<Line>>, size: u64) -> Self {
        let pages: Vec<Rc<[Line]>> = pages.into_iter().map(Rc::from).collect();
        let list = ListState::new(pages.first().map_or(0, |p| p.len()), ListAlignment::Top, px(600.));
        Self { pages, size, page: 0, zoom: ZOOM_DEFAULT, list }
    }

    fn select(&mut self, page: usize) {
        let page = page.min(self.pages.len().saturating_sub(1));
        if page != self.page {
            self.page = page;
            // A new page opens at its top, as the web's frame reloads.
            self.list.reset(self.pages[page].len());
        }
    }

    fn set_zoom(&mut self, zoom: u8) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        if zoom != self.zoom {
            self.zoom = zoom;
            self.list.remeasure();
        }
    }
}

/// `FileSurfaceAction`, a tab stop while enabled.
fn control(id: &'static str, icon: IconName, label: &'static str, enabled: bool, tokens: &Tokens) -> Stateful<Div> {
    let ring = tokens.ring;
    chrome::action(id, icon, label, tokens)
        .border_2()
        .border_color(gpui_kit::transparent_black())
        .when(enabled, |el| el.tab_index(0).focus_visible(move |s| s.border_color(ring)))
        .when(!enabled, |el| el.opacity(0.5).cursor_default())
}

impl Workspace {
    fn rtf_mut(&mut self, key: &FileKey) -> Option<&mut RtfDoc> {
        match self.files.assets.viewer_mut(key)? {
            super::asset::Viewer::Rtf(doc) => Some(doc),
            _ => None,
        }
    }

    /// `ExtendedOfficeViewer` with a `document` result and `DocumentPages`.
    pub(super) fn render_rtf(
        &mut self,
        key: &FileKey,
        width: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(doc) = self.rtf_mut(key) else { return div().into_any_element() };
        let count = doc.pages.len();
        let page = doc.page;
        let zoom = f32::from(doc.zoom) / 4.;
        let lines = doc.pages[page].clone();
        let size = if doc.size == 0 { "0 bytes".to_owned() } else { format_size(doc.size) };
        let details = format!("{count} page{} · {size}", if count == 1 { "" } else { "s" });
        let zoom_steps = doc.zoom;
        let state = doc.list.clone();
        let border = tokens.border.opacity(0.6);

        let step = |id: &'static str, icon: IconName, label: &'static str, enabled: bool, target: usize| {
            let key = key.clone();
            control(id, icon, label, enabled, tokens).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(doc) = this.rtf_mut(&key) {
                    doc.select(target);
                }
                cx.notify();
            }))
        };
        let navigation = (count > 1).then(|| {
            h_flex()
                .flex_none()
                .h(px(40.))
                .gap(px(8.))
                .px(px(12.))
                .border_b_1()
                .border_color(border)
                .text_xs()
                .child(step("rtf-previous", IconName::ChevronLeft, "Previous page", page > 0, page.saturating_sub(1)))
                .child(div().flex_1().text_center().child(format!("Page {} of {count}", page + 1)))
                .child(step("rtf-next", IconName::ChevronRight, "Next page", page + 1 < count, page + 1))
        });
        let zoom_control = |id: &'static str, icon: IconName, label: &'static str, enabled: bool, target: u8| {
            let key = key.clone();
            control(id, icon, label, enabled, tokens).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(doc) = this.rtf_mut(&key) {
                    doc.set_zoom(target);
                }
                cx.notify();
            }))
        };
        let toolbar = h_flex()
            .flex_none()
            .flex_wrap()
            .min_h(px(40.))
            .gap(px(4.))
            .px(px(12.))
            .py(px(4.))
            .border_b_1()
            .border_color(border)
            .text_xs()
            .text_color(tokens.muted_foreground)
            .child(div().flex_1().min_w_0().child(details))
            .child(zoom_control(
                "rtf-zoom-out",
                IconName::Minus,
                "Decrease reading size",
                zoom_steps > ZOOM_MIN,
                zoom_steps.saturating_sub(1),
            ))
            .child(div().min_w(px(40.)).text_center().child(format!("{}%", u32::from(zoom_steps) * 25)))
            .child(zoom_control(
                "rtf-zoom-in",
                IconName::Plus,
                "Increase reading size",
                zoom_steps < ZOOM_MAX,
                zoom_steps + 1,
            ))
            .child(zoom_control(
                "rtf-zoom-reset",
                IconName::RotateCcw,
                "Reset reading size",
                zoom_steps != ZOOM_DEFAULT,
                ZOOM_DEFAULT,
            ));

        // The frame's body: a card centred in the page colour, `calc(100% - 40px)` wide up to
        // 860px at 100%, with a `<p>` of 15px text and 1em margins inside 32px of padding.
        let (gutter, padding) = if width <= 420. { (12., 20.) } else { (20., 32.) };
        let font = 15. * zoom;
        let card = tokens.card;
        let max_width = px(860. * zoom);
        let last = lines.len().saturating_sub(1);
        let body = list(state, move |index, _, _| {
            let line = &lines[index];
            div()
                .w_full()
                .px(px(gutter))
                .when(index == 0, |el| el.pt(px(gutter)))
                .when(index == last, |el| el.pb(px(gutter)))
                .child(
                    div()
                        .w_full()
                        .max_w(max_width)
                        .mx_auto()
                        .px(px(padding))
                        .bg(card)
                        .when(index == 0, |el| el.pt(px(padding + font)))
                        .when(index == last, |el| el.pb(px(padding + font)))
                        .child(
                            div()
                                .min_h(px(font * 1.65))
                                .child(StyledText::new(line.text.clone()).with_highlights(line.highlights.clone())),
                        ),
                )
                .into_any_element()
        })
        .size_full();
        let page_view = div()
            .relative()
            .flex_1()
            .min_h_0()
            .bg(tokens.background)
            .text_color(tokens.foreground)
            .text_size(px(font))
            .line_height(relative(1.65))
            .child(
                div()
                    .absolute()
                    .top(px(gutter))
                    .bottom(px(gutter))
                    .left_0()
                    .right_0()
                    .px(px(gutter))
                    .child(div().size_full().max_w(max_width).mx_auto().bg(card)),
            )
            .child(body);
        v_flex()
            .flex_1()
            .min_h_0()
            .children(navigation)
            .child(chrome::notice("Text and images. Layout and embedded objects may differ from the original.", tokens))
            .child(toolbar)
            .child(page_view)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    /// The web's page markup, so its cases port as written.
    fn html(page: &Page) -> String {
        let lines: Vec<String> = page
            .iter()
            .map(|line| {
                line.iter()
                    .map(|run| {
                        let mut span = escape(&run.text);
                        if run.style.bold {
                            span = format!("<strong>{span}</strong>");
                        }
                        if run.style.italic {
                            span = format!("<em>{span}</em>");
                        }
                        if run.style.underline {
                            span = format!("<u>{span}</u>");
                        }
                        span
                    })
                    .collect()
            })
            .collect();
        format!("<p>{}</p>", lines.join("<br>"))
    }

    fn pages(rtf: &[u8]) -> Vec<String> {
        parse(rtf).unwrap().iter().map(html).collect()
    }

    fn strip_tags(html: &str) -> String {
        let mut out = String::new();
        let mut tag = false;
        for c in html.chars() {
            match c {
                '<' => tag = true,
                '>' => tag = false,
                c if !tag => out.push(c),
                _ => {}
            }
        }
        out
    }

    #[test]
    fn skipped_unicode_fallbacks_are_consumed() {
        for fallback in ["?", r"\'3f", r"\\"] {
            let rtf = format!(r"{{\rtf1{{\*\unknown\u945{fallback}}}Hello}}");
            assert_eq!(pages(rtf.as_bytes()), ["<p>Hello</p>"], "fallback {fallback}");
        }
    }

    #[test]
    fn formatting_and_unicode_decode_without_hidden_destinations() {
        let rtf = r#"{\rtf1\ansi{\fonttbl{\f0 Hidden font;}}Hello \b bold\b0\par Unicode: \u945?{\*\unknown hidden}{\field{\*\fldinst HYPERLINK "https://example.com"}{\fldrslt visible}}}"#;
        let html = &pages(rtf.as_bytes())[0];
        assert_eq!(strip_tags(html), "Hello boldUnicode: \u{3b1}visible");
        assert!(html.contains("<strong>"));
        assert!(!html.contains("https:"));
    }

    #[test]
    fn a_backslash_before_a_line_break_is_a_paragraph_break() {
        let text_edit =
            "{\\rtf1\\ansi{\\fonttbl\\f0\\fswiss Helvetica;}\n\\pard\\f0\\fs24 \\cf0 First\\\nSecond\\\n\\\nThird}";
        let expected =
            pages(br"{\rtf1\ansi{\fonttbl\f0\fswiss Helvetica;}\pard\f0\fs24 \cf0 First\par Second\par \par Third}");
        assert_eq!(pages(text_edit.as_bytes()), expected);
        assert_eq!(pages(text_edit.replace('\n', "\r\n").as_bytes()), expected);
        assert_eq!(expected, ["<p>First<br>Second<br><br>Third</p>"]);
    }

    #[test]
    fn page_breaks_keep_formatting_without_splitting_hidden_destinations() {
        assert_eq!(
            pages(br"{\rtf1\b First\page Second\b0{\*\unknown \page hidden}\page Third\page}"),
            ["<p><strong>First</strong></p>", "<p><strong>Second</strong></p>", "<p>Third</p>"]
        );
        let many = format!(r"{{\rtf1 {}}}", r"\page".repeat(501));
        assert!(parse(many.as_bytes()).unwrap_err().contains("500 pages"));
    }

    #[test]
    fn declared_code_pages_decode_raw_bytes_and_escaped_runs() {
        let mut input = br"{\rtf1\ansi\ansicpg1251 A \'c0\par\ansicpg932\'82\'a0}".to_vec();
        let first = input.iter().position(|&b| b == b'A').unwrap();
        input[first] = 0xc0;
        assert_eq!(pages(&input), ["<p>\u{410} \u{410}<br>\u{3042}</p>"]);
        assert!(parse(br"{\rtf1\ansicpg99999 text}").unwrap_err().contains("code page"));
    }

    #[test]
    fn the_content_limit_counts_escaped_utf16_markup() {
        // Each `&` is `&amp;` (5 units) and each Cyrillic letter one unit, as the web counts its markup.
        let document = |cyrillic: usize| {
            let mut rtf = br"{\rtf1\ansi\ansicpg1251 ".to_vec();
            rtf.resize(rtf.len() + MAX_CONTENT / 5, b'&');
            rtf.resize(rtf.len() + cyrillic, 0xc0);
            rtf.push(b'}');
            rtf
        };
        assert!(parse(&document(MAX_CONTENT % 5)).is_ok());
        assert_eq!(parse(&document(MAX_CONTENT % 5 + 1)).err().as_deref(), Some(TOO_MUCH));
        let mut cyrillic = br"{\rtf1\ansi\ansicpg1251 ".to_vec();
        cyrillic.resize(cyrillic.len() + 5 * 1024 * 1024, 0xc0);
        cyrillic.push(b'}');
        assert!(parse(&cyrillic).is_ok(), "5 MiB of one-byte Cyrillic is 5 Mi units");
        let mut tagged = br"{\rtf1\b ".to_vec();
        tagged.resize(tagged.len() + MAX_CONTENT - 16, b'a');
        tagged.extend_from_slice(br"\b0 b}");
        assert_eq!(parse(&tagged).err().as_deref(), Some(TOO_MUCH), "a closed run's tags count");
    }

    #[test]
    fn unicode_surrogate_pairs_join_and_damage_is_reported() {
        assert_eq!(pages(br"{\rtf1 a\u-10179?\u-8704?b}"), ["<p>a\u{1f600}b</p>"]);
        assert_eq!(pages(br"{\rtf1 \u-10179?x}"), ["<p>\u{fffd}x</p>"]);
        assert_eq!(parse(br"{\rtf1 {open}").unwrap_err(), DAMAGED);
        assert_eq!(parse(br"{\rtf1 x}}").unwrap_err(), DAMAGED);
        assert!(parse(b"plain text").unwrap_err().contains("not a valid"));
        assert!(parse(br"{\rtf1 \'zz}").unwrap_err().contains("byte escape"));
        assert_eq!(pages(br"{\rtf1 a\bin3 {}xb}"), ["<p>ab</p>"]);
        assert!(parse(br"{\rtf1 \bin9 x}").unwrap_err().contains("binary data"));
    }

    #[test]
    fn lines_lay_out_like_an_html_paragraph() {
        let run = |text: &str, bold| Run { text: text.into(), style: Style { bold, ..Style::default() } };
        let lines = layout(&vec![vec![run("  a \t", false), run(" b ", true)], vec![], vec![run(" ", false)], vec![]]);
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_ref()).collect();
        assert_eq!(texts, ["a b", "", ""]);
        let ranges: Vec<Range<usize>> = lines[0].highlights.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!((ranges.len(), ranges.first()), (1, Some(&(2..3))));
        assert!(layout(&vec![vec![]]).is_empty());
        assert_eq!(layout(&vec![vec![], vec![]]).len(), 1);
    }
}
