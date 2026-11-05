//! How the rich-text composer paints Markdown while the text stays plain. A port of
//! the web's inline parser (`parseInlineMarkdownPieces` in `apps/web/src/composer-rich-text.ts`)
//! and the block syntax `composer-rich-text-doc.ts` recognizes around it. Offsets are UTF-8 bytes.

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

/// Styled runs of `text`, in order and never overlapping. Token ranges are atoms, like
/// the web's chips: they parse as one opaque character and get no span of their own.
pub fn spans(text: &str, tokens: &[Range<usize>]) -> Vec<Span> {
    let mut source = text.as_bytes().to_vec();
    for token in tokens {
        // Same length, so offsets hold: not whitespace, not a word character, not a delimiter.
        if let Some(bytes) = source.get_mut(token.clone()) {
            bytes.fill(1);
        }
    }
    let source = String::from_utf8(source).unwrap_or_else(|_| text.to_owned());
    let mut styles = vec![0u8; text.len()];
    let mut fence: Option<(u8, usize)> = None;
    let mut offset = 0;
    for line in source.split('\n') {
        let out = &mut styles[offset..offset + line.len()];
        offset += line.len() + 1;
        if let Some((fence_char, fence_len)) = fence {
            if is_closing_fence(line, fence_char, fence_len) {
                fence = None;
                out.fill(MARKER);
            }
            continue;
        }
        if let Some(opening) = opening_fence(line) {
            fence = Some(opening);
            out.fill(MARKER);
            continue;
        }
        if is_rule(line) {
            out.fill(MARKER);
            continue;
        }
        if let Some(prefix) = heading_prefix(line) {
            out[..prefix].fill(MARKER);
            style_inline(&line[prefix..], &mut out[prefix..], HEADING);
            continue;
        }
        let prefix = quote_prefix(line).or_else(|| list_prefix(line)).unwrap_or(0);
        style_inline(&line[prefix..], &mut out[prefix..], 0);
    }
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
    spans
}

/// Text gets its marks plus `extra`; the delimiters parsing drops become markers,
/// except backticks beside code, which the web paints as part of the code.
fn style_inline(line: &str, out: &mut [u8], extra: u8) {
    out.fill(MARKER);
    for piece in inline_pieces(line) {
        out[piece.start..piece.end].fill(piece.marks | extra);
        if piece.marks & CODE != 0 {
            let style = piece.marks | extra;
            for tick in [piece.start.checked_sub(1), Some(piece.end)].into_iter().flatten() {
                if line.as_bytes().get(tick) == Some(&b'`') && out[tick] == MARKER {
                    out[tick] = style;
                }
            }
        }
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

/// The runs of `text` that survive parsing, with their marks; parsing only drops delimiters.
fn inline_pieces(text: &str) -> Vec<Piece> {
    let bytes = text.as_bytes();
    let mut root = Vec::new();
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
    root
}

fn run_of(line: &str, byte: u8) -> usize {
    line.bytes().take_while(|&b| b == byte).count()
}

fn blank(rest: &str) -> bool {
    rest.trim_end_matches('\r').bytes().all(|b| b == b' ' || b == b'\t')
}

/// `parseOpeningFence`: three or more backticks or tildes, then the language.
fn opening_fence(line: &str) -> Option<(u8, usize)> {
    let fence_char = *line.as_bytes().first()?;
    let len = run_of(line, fence_char);
    let valid = matches!(fence_char, b'`' | b'~') && len >= 3;
    (valid && (fence_char == b'~' || !line[len..].contains('`'))).then_some((fence_char, len))
}

/// `isClosingFence`: the opening character, at least as many times, then only spaces.
fn is_closing_fence(line: &str, fence_char: u8, fence_len: usize) -> bool {
    let len = run_of(line, fence_char);
    len >= fence_len && blank(&line[len..])
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
    let spaces = line[hashes..].bytes().take_while(|&b| b == b' ' || b == b'\t').count();
    ((1..=6).contains(&hashes) && spaces > 0).then_some(hashes + spaces)
}

fn quote_prefix(line: &str) -> Option<usize> {
    line.starts_with('>').then(|| 1 + line[1..].bytes().take_while(|&b| b == b' ' || b == b'\t').count())
}

/// `parseListPrefix`: indent, a bullet or `1.`/`1)`, then spaces or the line's end.
fn list_prefix(line: &str) -> Option<usize> {
    let indent = line.bytes().take_while(|&b| b == b' ' || b == b'\t').count();
    let rest = &line[indent..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let marker = match rest.as_bytes().get(digits) {
        Some(b'.' | b')') if digits > 0 => digits + 1,
        Some(b'-' | b'*' | b'+') if digits == 0 => 1,
        _ => return None,
    };
    let spaces = rest[marker..].bytes().take_while(|&b| b == b' ' || b == b'\t').count();
    (spaces > 0 || marker == rest.len()).then_some(indent + marker + spaces)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `parseInlineMarkdown`: the surviving text, merged by marks.
    fn parse(text: &str) -> Vec<(String, u8)> {
        let mut out: Vec<(String, u8)> = Vec::new();
        for piece in inline_pieces(text) {
            match out.last_mut() {
                Some((last, marks)) if *marks == piece.marks => last.push_str(&text[piece.start..piece.end]),
                _ => out.push((text[piece.start..piece.end].to_owned(), piece.marks)),
            }
        }
        out
    }

    fn styled<'a>(text: &'a str, tokens: &[Range<usize>]) -> Vec<(&'a str, u8)> {
        spans(text, tokens).into_iter().map(|span| (&text[span.range], span.style)).collect()
    }

    #[test]
    fn preserves_unmatched_markers_escapes_and_identifiers() {
        for text in ["plain text", "snake_case", "unmatched **", "** spaced **", "\\*literal\\*"] {
            assert_eq!(parse(text), [(text.to_owned(), 0)], "{text}");
        }
    }

    #[test]
    fn renders_triple_markers_and_nested_styles() {
        assert_eq!(parse("***both***"), [("both".to_owned(), BOLD | ITALIC)]);
        assert_eq!(
            parse("*a **b** c*"),
            [("a ".to_owned(), ITALIC), ("b".to_owned(), ITALIC | BOLD), (" c".to_owned(), ITALIC)]
        );
        assert_eq!(
            parse("**a `code` c**"),
            [("a ".to_owned(), BOLD), ("code".to_owned(), BOLD | CODE), (" c".to_owned(), BOLD)]
        );
    }

    #[test]
    fn preserves_crossing_italic_and_bold_spans() {
        assert_eq!(
            parse("*a**b*****c**"),
            [("a".to_owned(), ITALIC), ("b".to_owned(), ITALIC | BOLD), ("c".to_owned(), BOLD)]
        );
    }

    #[test]
    fn keeps_repeated_and_alternate_delimiters_literal_inside_an_active_mark() {
        let repeated = "*".repeat(10_000);
        assert_eq!(
            parse(&format!("{repeated}text{repeated}")),
            [(format!("{}text", &repeated[2..]), BOLD), (repeated[2..].to_owned(), 0)]
        );
        let alternate = "__".repeat(5_000);
        assert_eq!(
            parse(&format!("**~~{alternate}text{alternate}~~**")),
            [(format!("{alternate}text{alternate}"), BOLD | STRIKE)]
        );
    }

    #[test]
    fn keeps_code_span_contents_literal() {
        assert_eq!(parse("`**not bold**`"), [("**not bold**".to_owned(), CODE)]);
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
}
