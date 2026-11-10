//! How the rich-text composer paints Markdown while the text stays plain. A port of
//! the web's inline parser (`parseInlineMarkdownPieces` in `apps/web/src/composer-rich-text.ts`),
//! the block syntax `buildTiptapContent` (`composer-rich-text-doc.ts`) recognizes around it, and
//! the source the selection reveals (`composer-markdown-source.ts`). Offsets are UTF-8 bytes.

use std::ops::Range;

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const STRIKE: u8 = 4;
pub const CODE: u8 = 8;
pub const HEADING: u8 = 16;
/// Delimiters and block syntax: the web hides them away from the caret.
pub const MARKER: u8 = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub range: Range<usize>,
    pub style: u8,
}

/// A styled run with its delimiters (`collectStyledRanges`): the selection touching it shows
/// the delimiters, and leaving it hides them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub range: Range<usize>,
    pub delimiters: Vec<Range<usize>>,
}

/// A list a line's item sits in, from the outermost: what it indents the item by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gutter {
    Bullet,
    /// The widest marker in the list, in characters.
    Ordered(usize),
    /// A task list nested in another task list is indented; any other is not.
    Task {
        nested: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Bullet,
    /// The marker as written, `3)` or `7.`.
    Ordered(String),
    /// `check` is the byte between the brackets.
    Task {
        checked: bool,
        check: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Paragraph,
    /// `prefix` is the hashes and the spaces after them.
    Heading {
        level: u8,
        prefix: usize,
    },
    Quote {
        prefix: usize,
    },
    Rule,
    /// `gutters` runs from the outermost list to the item's own.
    Item {
        prefix: usize,
        kind: ItemKind,
        gutters: Vec<Gutter>,
    },
    /// `info` is the fence's info string, absolute.
    FenceOpen {
        info: Range<usize>,
        language: String,
    },
    Code {
        language: String,
    },
    FenceClose,
}

/// A source line (without its newline) and what it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub range: Range<usize>,
    pub block: Block,
}

impl Line {
    /// The block syntax at the line's start the editor never shows: a heading's hashes, a
    /// quote's `>`, a list item's marker.
    pub fn prefix(&self) -> Option<Range<usize>> {
        let len = match &self.block {
            Block::Heading { prefix, .. } | Block::Quote { prefix } | Block::Item { prefix, .. } => *prefix,
            _ => return None,
        };
        (len > 0).then(|| self.range.start..self.range.start + len)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub spans: Vec<Span>,
    pub groups: Vec<Group>,
    pub lines: Vec<Line>,
}

/// The styles, styled groups and block lines of `text`. Spans are in order and never overlap.
/// Token ranges are atoms, like the web's chips: they parse as one opaque character and get no
/// span of their own.
pub fn parse(text: &str, tokens: &[Range<usize>]) -> Parsed {
    let mut source = text.as_bytes().to_vec();
    for token in tokens {
        // Same length, so offsets hold: not whitespace, not a word character, not a delimiter.
        if let Some(bytes) = source.get_mut(token.clone()) {
            bytes.fill(1);
        }
    }
    let source = String::from_utf8(source).unwrap_or_else(|_| text.to_owned());
    let mut styles = vec![0u8; text.len()];
    let mut groups = Vec::new();
    let mut lines = Vec::new();
    let mut lists = Lists::default();
    let mut fence: Option<(u8, usize, String)> = None;
    let mut offset = 0;
    for line in source.split('\n') {
        let start = offset;
        let range = start..start + line.len();
        let out = &mut styles[range.clone()];
        offset += line.len() + 1;
        let block = if let Some((fence_char, fence_len, language)) = &fence {
            if is_closing_fence(line, *fence_char, *fence_len) {
                fence = None;
                out.fill(MARKER);
                Block::FenceClose
            } else {
                Block::Code { language: language.clone() }
            }
        } else if let Some((fence_char, fence_len)) = opening_fence(line) {
            out.fill(MARKER);
            let info = start + fence_len..range.end;
            let language = language_of_info(&text[info.clone()]).to_owned();
            fence = Some((fence_char, fence_len, language.clone()));
            Block::FenceOpen { info, language }
        } else if is_rule(line) {
            out.fill(MARKER);
            Block::Rule
        } else if let Some(prefix) = heading_prefix(line) {
            out[..prefix].fill(MARKER);
            style_inline(&line[prefix..], &mut out[prefix..], HEADING, start + prefix, &mut groups);
            Block::Heading { level: u8::try_from(run_of(line, b'#')).unwrap_or(1), prefix }
        } else if let Some(prefix) = quote_prefix(line) {
            style_inline(&line[prefix..], &mut out[prefix..], 0, start + prefix, &mut groups);
            Block::Quote { prefix }
        } else if let Some(item) = list_item(line) {
            style_inline(&line[item.prefix..], &mut out[item.prefix..], 0, start + item.prefix, &mut groups);
            lists.push(&item, lines.len());
            let kind = match item.kind {
                ItemKind::Task { checked, check } => ItemKind::Task { checked, check: start + check },
                kind => kind,
            };
            Block::Item { prefix: item.prefix, kind, gutters: Vec::new() }
        } else {
            style_inline(line, out, 0, start, &mut groups);
            Block::Paragraph
        };
        if !matches!(block, Block::Item { .. }) {
            lists.close();
        }
        lines.push(Line { range, block });
    }
    lists.finish(&mut lines);
    for token in tokens {
        if let Some(bytes) = styles.get_mut(token.clone()) {
            bytes.fill(0);
        }
    }
    let mut spans: Vec<Span> = Vec::new();
    for (index, &style) in styles.iter().enumerate() {
        match spans.last_mut() {
            Some(last) if last.range.end == index && last.style == style => last.range.end += 1,
            _ if style != 0 => spans.push(Span { range: index..index + 1, style }),
            _ => {}
        }
    }
    Parsed { spans, groups, lines }
}

/// Text gets its marks plus `extra`; the delimiters parsing drops become markers,
/// except backticks beside code, which the web paints as part of the code. Each styled run's
/// group lands in `groups`, offset by `base`.
fn style_inline(line: &str, out: &mut [u8], extra: u8, base: usize, groups: &mut Vec<Group>) {
    out.fill(MARKER);
    let mut covered = vec![false; line.len()];
    let (pieces, spans) = inline_pieces(line);
    for piece in pieces {
        out[piece.start..piece.end].fill(piece.marks | extra);
        covered[piece.start..piece.end].fill(true);
        if piece.marks & CODE != 0 {
            let style = piece.marks | extra;
            for tick in [piece.start.checked_sub(1), Some(piece.end)].into_iter().flatten() {
                if line.as_bytes().get(tick) == Some(&b'`') && out[tick] == MARKER {
                    out[tick] = style;
                }
            }
        }
    }
    // Nested styles share the outermost run's group.
    let mut merged: Vec<Range<usize>> = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start < last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    for range in merged {
        let mut delimiters: Vec<Range<usize>> = Vec::new();
        for index in range.clone() {
            if covered[index] {
                continue;
            }
            match delimiters.last_mut() {
                Some(last) if last.end == base + index => last.end += 1,
                _ => delimiters.push(base + index..base + index + 1),
            }
        }
        groups.push(Group { range: base + range.start..base + range.end, delimiters });
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Piece {
    start: usize,
    end: usize,
    marks: u8,
}

struct Frame {
    delimiter: u8,
    len: usize,
    mark: u8,
    start: usize,
    pieces: Vec<Piece>,
}

fn push(pieces: &mut Vec<Piece>, start: usize, end: usize, marks: u8) {
    if end <= start {
        return;
    }
    match pieces.last_mut() {
        Some(last) if last.end == start && last.marks == marks => last.end = end,
        _ => pieces.push(Piece { start, end, marks }),
    }
}

/// Where the next piece goes: the innermost open delimiter, else the line.
fn target<'a>(stack: &'a mut [Frame], root: &'a mut Vec<Piece>) -> &'a mut Vec<Piece> {
    stack.last_mut().map_or(root, |frame| &mut frame.pieces)
}

/// The runs of `text` that survive parsing, with their marks (parsing only drops
/// delimiters), and each matched style's extent with its delimiters, in source order.
fn inline_pieces(text: &str) -> (Vec<Piece>, Vec<Range<usize>>) {
    let bytes = text.as_bytes();
    let mut root = Vec::new();
    let mut spans = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\\' {
            let end = (index + 1 + text[index + 1..].chars().next().map_or(0, char::len_utf8)).min(bytes.len());
            push(target(&mut stack, &mut root), index, end, 0);
            index = end;
            continue;
        }
        if byte == b'`' {
            let run = bytes[index..].iter().take_while(|&&b| b == b'`').count();
            let close = text[index + run..].find(&text[index..index + run]).map(|at| at + index + run);
            match close {
                Some(close)
                    if run == 1
                        && close > index + 1
                        && !text[index..close].contains('\n')
                        && bytes.get(close + 1) != Some(&b'`') =>
                {
                    push(target(&mut stack, &mut root), index + 1, close, CODE);
                    spans.push(index..close + 1);
                    index = close + 1;
                }
                _ => {
                    push(target(&mut stack, &mut root), index, index + run, 0);
                    index += run;
                }
            }
            continue;
        }
        if !matches!(byte, b'*' | b'_' | b'~') {
            let end = bytes[index..]
                .iter()
                .position(|b| matches!(b, b'\\' | b'`' | b'*' | b'_' | b'~'))
                .map_or(bytes.len(), |at| index + at);
            push(target(&mut stack, &mut root), index, end, 0);
            index = end;
            continue;
        }
        let end = index + bytes[index..].iter().take_while(|&&b| b == byte).count();
        let before = text[..index].chars().next_back();
        let after = text[end..].chars().next();
        let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        let can_close = before.is_some_and(|c| !c.is_whitespace()) && (byte != b'_' || !word(after));
        let can_open = after.is_some_and(|c| !c.is_whitespace()) && (byte != b'_' || !word(before));
        while index < end {
            let open = |mark: u8| stack.iter().any(|frame| frame.mark == mark);
            let top = stack.last();
            let opens_nested_bold = top.is_some_and(|frame| frame.mark == ITALIC)
                && end - index == 2
                && can_open
                && byte != b'~'
                && !open(BOLD);
            if !opens_nested_bold
                && can_close
                && top.is_some_and(|frame| frame.delimiter == byte && index + frame.len <= end)
            {
                let frame = stack.pop().expect("a top frame");
                for piece in frame.pieces {
                    push(target(&mut stack, &mut root), piece.start, piece.end, frame.mark | piece.marks);
                }
                index += frame.len;
                spans.push(frame.start..index);
            } else if can_open && (byte != b'~' || end - index >= 2) {
                let len = if byte == b'~' || end - index >= 2 { 2 } else { 1 };
                let mark = match (byte, len) {
                    (b'~', _) => STRIKE,
                    (_, 2) => BOLD,
                    _ => ITALIC,
                };
                // No self-nesting, even through the alternate delimiter: bounds the stack.
                if open(mark) {
                    push(target(&mut stack, &mut root), index, end, 0);
                    index = end;
                    continue;
                }
                stack.push(Frame { delimiter: byte, len, mark, start: index, pieces: Vec::new() });
                index += len;
            } else {
                push(target(&mut stack, &mut root), index, end, 0);
                index = end;
            }
        }
    }
    while let Some(frame) = stack.pop() {
        let pieces = target(&mut stack, &mut root);
        push(pieces, frame.start, frame.start + frame.len, 0);
        for piece in frame.pieces {
            push(pieces, piece.start, piece.end, piece.marks);
        }
    }
    spans.sort_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
    (root, spans)
}

fn run_of(line: &str, byte: u8) -> usize {
    line.bytes().take_while(|&b| b == byte).count()
}

fn blank(rest: &str) -> bool {
    rest.trim_end_matches('\r').bytes().all(|b| b == b' ' || b == b'\t')
}

fn spaces(text: &str) -> usize {
    text.bytes().take_while(|&b| b == b' ' || b == b'\t').count()
}

/// `parseOpeningFence`: three or more backticks or tildes, then the language.
pub(crate) fn opening_fence(line: &str) -> Option<(u8, usize)> {
    let fence_char = *line.as_bytes().first()?;
    let len = run_of(line, fence_char);
    let valid = matches!(fence_char, b'`' | b'~') && len >= 3;
    (valid && (fence_char == b'~' || !line[len..].contains('`'))).then_some((fence_char, len))
}

/// `isClosingFence`: the opening character, at least as many times, then only spaces.
pub(crate) fn is_closing_fence(line: &str, fence_char: u8, fence_len: usize) -> bool {
    let len = run_of(line, fence_char);
    len >= fence_len && blank(&line[len..])
}

/// `languageOfInfoString`: the info string's first word.
pub fn language_of_info(info: &str) -> &str {
    info.split_whitespace().next().unwrap_or_default()
}

/// A thematic break: one of `-*_` three or more times, spaces between.
fn is_rule(line: &str) -> bool {
    let Some(&rule) = line.as_bytes().first().filter(|b| matches!(b, b'-' | b'*' | b'_')) else {
        return false;
    };
    let body = line.trim_end_matches('\r');
    body.bytes().filter(|&b| b == rule).count() >= 3 && body.bytes().all(|b| b == rule || b == b' ' || b == b'\t')
}

/// `#` to `######` and the spaces after them.
fn heading_prefix(line: &str) -> Option<usize> {
    let hashes = run_of(line, b'#');
    let spaces = spaces(&line[hashes..]);
    ((1..=6).contains(&hashes) && spaces > 0).then_some(hashes + spaces)
}

fn quote_prefix(line: &str) -> Option<usize> {
    line.starts_with('>').then(|| 1 + spaces(&line[1..]))
}

struct ListLine {
    indent: String,
    prefix: usize,
    kind: ItemKind,
    /// `listKey`: items of another kind start another list.
    key: String,
}

/// `parseListPrefix`: a task, else `1.`/`1)`, else a bullet, after an indent and before
/// spaces or the line's end. A task's `check` is relative to the line.
fn list_item(line: &str) -> Option<ListLine> {
    let indent = spaces(line);
    let rest = &line[indent..];
    if let Some(after_dash) = rest.strip_prefix('-') {
        let gap = spaces(after_dash);
        let boxed = &after_dash[gap..];
        if gap > 0
            && let [b'[', check @ (b' ' | b'x' | b'X'), b']', ..] = boxed.as_bytes()
        {
            let content_space = spaces(&boxed[3..]);
            if boxed.len() == 3 + content_space || content_space > 0 {
                return Some(ListLine {
                    indent: line[..indent].to_owned(),
                    prefix: indent + 1 + gap + 3 + content_space,
                    kind: ItemKind::Task { checked: *check != b' ', check: indent + 1 + gap + 1 },
                    key: "task".to_owned(),
                });
            }
        }
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (marker, kind, key) = match rest.as_bytes().get(digits) {
        Some(&delimiter @ (b'.' | b')')) if digits > 0 => {
            (digits + 1, ItemKind::Ordered(rest[..=digits].to_owned()), format!("ordered:{}", char::from(delimiter)))
        }
        Some(&bullet @ (b'-' | b'*' | b'+')) if digits == 0 => {
            (1, ItemKind::Bullet, format!("bullet:{}", char::from(bullet)))
        }
        _ => return None,
    };
    let gap = spaces(&rest[marker..]);
    (gap > 0 || marker == rest.len()).then(|| ListLine {
        indent: line[..indent].to_owned(),
        prefix: indent + marker + gap,
        kind,
        key,
    })
}

struct ListInfo {
    key: String,
    parent: Option<usize>,
    widest: usize,
}

struct Level {
    indent: String,
    list: usize,
}

/// `buildTiptapContent`'s list stack: which list each item joins, and which item a deeper
/// indent nests under. Any other line ends every open list.
#[derive(Default)]
struct Lists {
    lists: Vec<ListInfo>,
    stack: Vec<Level>,
    /// Line index and list of each item.
    items: Vec<(usize, usize)>,
}

impl Lists {
    fn open(&mut self, key: &str, parent: Option<usize>) -> usize {
        self.lists.push(ListInfo { key: key.to_owned(), parent, widest: 0 });
        self.lists.len() - 1
    }

    /// The top level's list for an item of `key`: its own, or a sibling list of that kind.
    fn join(&mut self, key: &str) -> usize {
        let level = self.stack.len() - 1;
        let list = self.stack[level].list;
        if self.lists[list].key == key {
            return list;
        }
        let sibling = self.open(key, self.lists[list].parent);
        self.stack[level].list = sibling;
        sibling
    }

    /// Places the item on line `line`; `finish` fills in its gutters.
    fn push(&mut self, item: &ListLine, line: usize) {
        let indent = &item.indent;
        let list = loop {
            let depth = self.stack.len();
            let Some(top) = self.stack.last_mut() else {
                let list = self.open(&item.key, None);
                self.stack.push(Level { indent: indent.clone(), list });
                continue;
            };
            if top.indent == *indent {
                break self.join(&item.key);
            }
            if !top.indent.is_empty() && !indent.starts_with(top.indent.as_str()) {
                if depth > 1 {
                    self.stack.pop();
                    continue;
                }
                top.indent.clone_from(indent);
                break self.join(&item.key);
            }
            let top_list = top.list;
            let parent_item = self.items.iter().rev().find(|(_, list)| *list == top_list).map(|(line, _)| *line);
            if parent_item.is_none() {
                break top_list;
            }
            let list = self.open(&item.key, Some(top_list));
            self.stack.push(Level { indent: indent.clone(), list });
            break list;
        };
        if let ItemKind::Ordered(marker) = &item.kind {
            self.lists[list].widest = self.lists[list].widest.max(marker.chars().count().min(10));
        }
        self.items.push((line, list));
    }

    fn close(&mut self) {
        self.stack.clear();
    }

    fn gutter(&self, list: usize) -> Gutter {
        let info = &self.lists[list];
        match info.key.as_str() {
            "task" => Gutter::Task { nested: info.parent.is_some_and(|parent| self.lists[parent].key == "task") },
            key if key.starts_with("ordered") => Gutter::Ordered(info.widest),
            _ => Gutter::Bullet,
        }
    }

    fn finish(&self, lines: &mut [Line]) {
        for &(line, list) in &self.items {
            let mut gutters = Vec::new();
            let mut next = Some(list);
            while let Some(list) = next {
                gutters.push(self.gutter(list));
                next = self.lists[list].parent;
            }
            gutters.reverse();
            if let Block::Item { gutters: slot, .. } = &mut lines[line].block {
                *slot = gutters;
            }
        }
    }
}

/// What a line's marker draws in its indent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marker {
    /// Disc, circle, then square, by how many bullet lists hold it.
    Bullet(usize),
    Number(String),
    /// `check` is the byte between the brackets.
    Task {
        checked: bool,
        check: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Object {
    /// A list item's marker, drawn in the indent its `range` leaves.
    Marker { range: Range<usize>, marker: Marker },
    /// A thematic break across the row.
    Rule(Range<usize>),
    /// A code block's opening fence: its header, with the language picker.
    FenceHeader { range: Range<usize>, info: Range<usize>, language: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineKind {
    Heading(u8),
    Quote,
    Item(Vec<Gutter>),
    Code,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Quote,
    /// A fence's lines; `open` is whether its closing fence is missing.
    Code,
}

/// What the editor draws for `text` with `selection`: the source it hides, the markers and
/// blocks it draws instead, and the lines it sets in another size, font or indent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub hidden: Vec<Range<usize>>,
    pub objects: Vec<Object>,
    /// Line start and kind.
    pub lines: Vec<(usize, LineKind)>,
    pub blocks: Vec<(Range<usize>, BlockKind)>,
}

/// `touches`: a caret at either end touches a range; a selection must overlap it.
fn touches(selection: &Range<usize>, range: &Range<usize>) -> bool {
    if selection.is_empty() {
        range.start <= selection.start && selection.start <= range.end
    } else {
        selection.start < range.end && selection.end > range.start
    }
}

/// Whether the selection is on `line`, newline aside.
fn on_line(selection: &Range<usize>, line: &Range<usize>) -> bool {
    selection.start <= line.end && selection.end >= line.start
}

/// The look of `parsed` with `selection`: styled runs the selection touches and fence and rule
/// lines it sits on show their source; everything else shows rendered.
pub fn look(parsed: &Parsed, selection: &Range<usize>) -> Look {
    let mut look = Look::default();
    for group in &parsed.groups {
        if !touches(selection, &group.range) {
            look.hidden.extend(group.delimiters.iter().cloned());
        }
    }
    let mut quote: Option<Range<usize>> = None;
    let mut code: Option<Range<usize>> = None;
    for line in &parsed.lines {
        let range = line.range.clone();
        if !matches!(line.block, Block::Quote { .. })
            && let Some(block) = quote.take()
        {
            look.blocks.push((block, BlockKind::Quote));
        }
        match &line.block {
            Block::Paragraph => {}
            Block::Heading { level, prefix } => {
                look.hidden.push(range.start..range.start + prefix);
                look.lines.push((range.start, LineKind::Heading(*level)));
            }
            Block::Quote { prefix } => {
                if *prefix > 0 {
                    look.hidden.push(range.start..range.start + prefix);
                }
                look.lines.push((range.start, LineKind::Quote));
                quote = Some(quote.map_or(range.clone(), |block| block.start..range.end));
            }
            Block::Rule => {
                if !on_line(selection, &range) {
                    look.objects.push(Object::Rule(range));
                }
            }
            Block::Item { prefix, kind, gutters } => {
                let marker = match kind {
                    ItemKind::Bullet => {
                        Marker::Bullet(gutters.iter().filter(|g| !matches!(g, Gutter::Ordered(_))).count())
                    }
                    ItemKind::Ordered(marker) => Marker::Number(marker.clone()),
                    ItemKind::Task { checked, check } => Marker::Task { checked: *checked, check: *check },
                };
                look.objects.push(Object::Marker { range: range.start..range.start + prefix, marker });
                look.lines.push((range.start, LineKind::Item(gutters.clone())));
            }
            Block::FenceOpen { info, language } => {
                code = Some(range.clone());
                if !on_line(selection, &range) {
                    look.objects.push(Object::FenceHeader {
                        range: range.clone(),
                        info: info.clone(),
                        language: language.clone(),
                    });
                }
                look.lines.push((range.start, LineKind::Code));
            }
            Block::Code { .. } => {
                if let Some(block) = &mut code {
                    block.end = range.end;
                }
                look.lines.push((range.start, LineKind::Code));
            }
            Block::FenceClose => {
                if let Some(block) = code.take() {
                    look.blocks.push((block.start..range.end, BlockKind::Code));
                }
                if !on_line(selection, &range) && !range.is_empty() {
                    look.hidden.push(range.clone());
                }
                look.lines.push((range.start, LineKind::Code));
            }
        }
    }
    if let Some(block) = quote {
        look.blocks.push((block, BlockKind::Quote));
    }
    if let Some(block) = code {
        look.blocks.push((block, BlockKind::Code));
    }
    look.hidden.retain(|range| !range.is_empty());
    look.hidden.sort_by_key(|range| range.start);
    look
}

/// Where a caret that moved from `previous` to `caret` rests: never inside or before a line's
/// hidden prefix, where nothing marks its place. One step left from the prefix's end goes on to
/// the line before, as the web's caret leaves a block's start; a click or Home there stays.
/// The prefixes are ASCII, so one step is one byte.
pub fn snap_caret(parsed: &Parsed, previous: usize, caret: usize) -> usize {
    let Some(prefix) =
        parsed.lines.iter().filter_map(Line::prefix).find(|prefix| prefix.start <= caret && caret < prefix.end)
    else {
        return caret;
    };
    if previous == prefix.end && caret + 1 == prefix.end && prefix.start > 0 { prefix.start - 1 } else { prefix.end }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parseInlineMarkdown`: the surviving text, merged by marks.
    fn parse_inline(text: &str) -> Vec<(String, u8)> {
        let mut out: Vec<(String, u8)> = Vec::new();
        for piece in inline_pieces(text).0 {
            match out.last_mut() {
                Some((last, marks)) if *marks == piece.marks => last.push_str(&text[piece.start..piece.end]),
                _ => out.push((text[piece.start..piece.end].to_owned(), piece.marks)),
            }
        }
        out
    }

    fn styled<'a>(text: &'a str, tokens: &[Range<usize>]) -> Vec<(&'a str, u8)> {
        parse(text, tokens).spans.into_iter().map(|span| (&text[span.range], span.style)).collect()
    }

    fn hidden(text: &str, caret: usize) -> Vec<&str> {
        look(&parse(text, &[]), &(caret..caret)).hidden.into_iter().map(|range| &text[range]).collect()
    }

    fn blocks(text: &str) -> Vec<Block> {
        parse(text, &[]).lines.into_iter().map(|line| line.block).collect()
    }

    #[test]
    fn preserves_unmatched_markers_escapes_and_identifiers() {
        for text in ["plain text", "snake_case", "unmatched **", "** spaced **", "\\*literal\\*"] {
            assert_eq!(parse_inline(text), [(text.to_owned(), 0)], "{text}");
        }
    }

    #[test]
    fn renders_triple_markers_and_nested_styles() {
        assert_eq!(parse_inline("***both***"), [("both".to_owned(), BOLD | ITALIC)]);
        assert_eq!(
            parse_inline("*a **b** c*"),
            [("a ".to_owned(), ITALIC), ("b".to_owned(), ITALIC | BOLD), (" c".to_owned(), ITALIC)]
        );
        assert_eq!(
            parse_inline("**a `code` c**"),
            [("a ".to_owned(), BOLD), ("code".to_owned(), BOLD | CODE), (" c".to_owned(), BOLD)]
        );
    }

    #[test]
    fn preserves_crossing_italic_and_bold_spans() {
        assert_eq!(
            parse_inline("*a**b*****c**"),
            [("a".to_owned(), ITALIC), ("b".to_owned(), ITALIC | BOLD), ("c".to_owned(), BOLD)]
        );
    }

    #[test]
    fn keeps_repeated_and_alternate_delimiters_literal_inside_an_active_mark() {
        let repeated = "*".repeat(10_000);
        assert_eq!(
            parse_inline(&format!("{repeated}text{repeated}")),
            [(format!("{}text", &repeated[2..]), BOLD), (repeated[2..].to_owned(), 0)]
        );
        let alternate = "__".repeat(5_000);
        assert_eq!(
            parse_inline(&format!("**~~{alternate}text{alternate}~~**")),
            [(format!("{alternate}text{alternate}"), BOLD | STRIKE)]
        );
    }

    #[test]
    fn keeps_code_span_contents_literal() {
        assert_eq!(parse_inline("`**not bold**`"), [("**not bold**".to_owned(), CODE)]);
    }

    #[test]
    fn dims_dropped_delimiters_and_paints_ticks_as_code() {
        assert_eq!(styled("**`*` w** \\*", &[]), [("**", MARKER), ("`*`", BOLD | CODE), (" w", BOLD), ("**", MARKER)]);
        assert_eq!(styled("~~gone~~ snake_case_name", &[]), [("~~", MARKER), ("gone", STRIKE), ("~~", MARKER)]);
    }

    #[test]
    fn styles_headings_and_skips_fenced_code() {
        assert_eq!(
            styled("## Big *idea*", &[]),
            [("## ", MARKER), ("Big ", HEADING), ("*", MARKER), ("idea", HEADING | ITALIC), ("*", MARKER),]
        );
        assert_eq!(styled("#tag", &[]), []);
        assert_eq!(
            styled("```rs\nlet **x** = 1;\n```\n**y**", &[]),
            [("```rs", MARKER), ("```", MARKER), ("**", MARKER), ("y", BOLD), ("**", MARKER),]
        );
        assert_eq!(styled("---", &[]), [("---", MARKER)]);
    }

    #[test]
    fn parses_list_and_quote_content_after_their_prefix() {
        assert_eq!(styled("* **a**", &[]), [("**", MARKER), ("a", BOLD), ("**", MARKER)]);
        assert_eq!(styled("> _b_", &[]), [("_", MARKER), ("b", ITALIC), ("_", MARKER)]);
    }

    #[test]
    #[expect(clippy::single_range_in_vec_init, reason = "a list of one range is the expected value")]
    fn treats_tokens_as_atoms() {
        // A chip's own underscores never open or close a style, and it keeps its own look.
        let text = "_see @src/a_b.rs now_";
        let token = 5..16;
        assert_eq!(
            styled(text, std::slice::from_ref(&token)),
            [("_", MARKER), ("see ", ITALIC), (" now", ITALIC), ("_", MARKER)]
        );
        assert_eq!(styled("@a_*b*", &[0..3]), [("*", MARKER), ("b", ITALIC), ("*", MARKER)]);
    }

    #[test]
    fn delimiters_show_only_while_the_selection_touches_their_run() {
        let text = "a **bold** and `code` x";
        // Away from both runs: every delimiter is hidden, the code's ticks too.
        assert_eq!(hidden(text, 0), ["**", "**", "`", "`"]);
        // A caret at either edge or inside a run shows that run's delimiters only.
        for caret in [2, 5, 10] {
            assert_eq!(hidden(text, caret), ["`", "`"], "caret {caret}");
        }
        assert_eq!(hidden(text, 15), ["**", "**"]);
        // A selection shows the runs it overlaps, not the ones it only meets.
        let parsed = parse(text, &[]);
        let shown = |selection: Range<usize>| look(&parsed, &selection).hidden.len();
        assert_eq!(shown(0..2), 4);
        assert_eq!(shown(0..3), 2);
        assert_eq!(shown(9..16), 0);
    }

    #[test]
    fn nested_styles_reveal_as_one_run_and_unmatched_markers_stay_text() {
        let text = "**a *b* c** **open";
        let parsed = parse(text, &[]);
        assert_eq!(parsed.groups.len(), 1, "the unmatched `**` opens no run");
        assert_eq!(parsed.groups[0].range, 0..11);
        assert_eq!(hidden(text, 5), Vec::<&str>::new());
        assert_eq!(hidden(text, 15), ["**", "*", "*", "**"]);
    }

    #[test]
    fn classifies_headings_quotes_rules_and_fences() {
        assert_eq!(
            blocks("# Title\n####### seven\n> quoted\n>\n- - -\n```rust title\nfn a() {}\n```\nafter"),
            [
                Block::Heading { level: 1, prefix: 2 },
                Block::Paragraph,
                Block::Quote { prefix: 2 },
                Block::Quote { prefix: 1 },
                Block::Rule,
                Block::FenceOpen { info: 42..52, language: "rust".to_owned() },
                Block::Code { language: "rust".to_owned() },
                Block::FenceClose,
                Block::Paragraph,
            ]
        );
        // An unclosed fence runs to the end; a shorter run inside does not close it.
        assert_eq!(
            blocks("````\n```\nx"),
            [
                Block::FenceOpen { info: 4..4, language: String::new() },
                Block::Code { language: String::new() },
                Block::Code { language: String::new() },
            ]
        );
    }

    #[test]
    fn nests_list_items_by_indent_and_splits_lists_by_kind() {
        let lines = blocks("- a\n  1. b\n  10. c\n    - [x] d\n- e\n* f\npara\n- [ ] g");
        let gutters: Vec<_> = lines
            .iter()
            .map(|block| match block {
                Block::Item { gutters, .. } => gutters.clone(),
                other => panic!("not an item: {other:?}"),
            })
            .take(6)
            .collect();
        assert_eq!(gutters[0], [Gutter::Bullet]);
        // Siblings share their list's widest marker.
        assert_eq!(gutters[1], [Gutter::Bullet, Gutter::Ordered(3)]);
        assert_eq!(gutters[2], [Gutter::Bullet, Gutter::Ordered(3)]);
        assert_eq!(gutters[3], [Gutter::Bullet, Gutter::Ordered(3), Gutter::Task { nested: false }]);
        assert_eq!(gutters[4], [Gutter::Bullet]);
        assert_eq!(gutters[5], [Gutter::Bullet], "another bullet character starts a sibling list");
        assert_eq!(lines[6], Block::Paragraph);
        assert_eq!(
            lines[7],
            Block::Item {
                prefix: 6,
                kind: ItemKind::Task { checked: false, check: 47 },
                gutters: vec![Gutter::Task { nested: false }],
            }
        );
        assert!(
            matches!(&blocks("- [x] a\n  - [ ] b")[1], Block::Item { gutters, .. } if gutters[1] == Gutter::Task { nested: true })
        );
        // A task needs a space before its content; `- [ ]x` is a bullet.
        assert!(matches!(&blocks("- [ ]x")[0], Block::Item { kind: ItemKind::Bullet, prefix: 2, .. }));
        assert!(matches!(&blocks("-x")[0], Block::Paragraph));
    }

    #[test]
    fn fence_and_rule_lines_show_their_source_under_the_caret() {
        let text = "```rust\ncode\n```\n---";
        let parsed = parse(text, &[]);
        let away = look(&parsed, &(9..9));
        assert_eq!(
            away.objects,
            [Object::FenceHeader { range: 0..7, info: 3..7, language: "rust".to_owned() }, Object::Rule(17..20),]
        );
        assert_eq!(away.hidden, vec![13..16]);
        assert_eq!(away.blocks, [(0..16, BlockKind::Code)]);
        let on_fence = look(&parsed, &(7..7));
        assert_eq!(on_fence.objects, [Object::Rule(17..20)]);
        assert_eq!(look(&parsed, &(14..14)).hidden, Vec::<Range<usize>>::new());
        assert_eq!(look(&parsed, &(18..18)).objects.len(), 1);
    }

    #[test]
    fn caret_never_rests_inside_a_hidden_prefix() {
        let text = "intro\n## Head\n- item";
        let parsed = parse(text, &[]);
        // Arriving from the line above, from a click or Home: the text's start.
        assert_eq!(snap_caret(&parsed, 5, 6), 9);
        assert_eq!(snap_caret(&parsed, 12, 6), 9);
        assert_eq!(snap_caret(&parsed, 13, 7), 9);
        // One step left from the text's start goes on to the line above.
        assert_eq!(snap_caret(&parsed, 9, 8), 5);
        assert_eq!(snap_caret(&parsed, 16, 15), 13);
        // Home or a click at the gutter from the text's start stays on the line.
        assert_eq!(snap_caret(&parsed, 9, 6), 9);
        // Outside prefixes, and at a first line's start, nothing moves.
        assert_eq!(snap_caret(&parsed, 0, 3), 3);
        assert_eq!(snap_caret(&parse("# a", &[]), 2, 1), 2);
    }
}
