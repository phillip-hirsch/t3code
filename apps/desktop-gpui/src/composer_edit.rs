//! Edits the composer's Enter and Tab make instead of sending or moving focus: Markdown list
//! continuation and nesting (`composer-list-continuation.ts`), and the rich editor's fenced
//! code keys (`composer-code-block.ts`, `convertCodeFenceOnEnter`) over the plain text the
//! composer keeps. Offsets are UTF-8 bytes; `tokens` are the editor's chip ranges.

use std::ops::Range;

use crate::composer_markdown::{is_closing_fence, opening_fence};

/// Replace `range` with `text`, then select `select`.
#[derive(Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
    pub select: Range<usize>,
}

impl Edit {
    fn caret(range: Range<usize>, text: String, caret: usize) -> Self {
        Self { range, text, select: caret..caret }
    }

    /// Consumes the key without changing the text.
    fn keep(select: Range<usize>) -> Self {
        Self { range: select.start..select.start, text: String::new(), select }
    }

    pub fn is_noop(&self) -> bool {
        self.range.is_empty() && self.text.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Ordered { number: String, delimiter: char },
    Task,
    Bullet(char),
}

#[derive(Clone, Debug)]
struct Marker {
    kind: Kind,
    indent: String,
    space: String,
}

struct Parsed {
    marker: Marker,
    end: usize,
    /// The column a nested item must reach to belong to this item.
    content_column: usize,
}

/// `[ \t]+`, else the whitespace-only rest of the line, else nothing.
fn marker_space(rest: &str) -> Option<&str> {
    let tabs = rest.bytes().take_while(|&b| b == b' ' || b == b'\t').count();
    if tabs > 0 { Some(&rest[..tabs]) } else { rest.chars().all(char::is_whitespace).then_some(rest) }
}

/// `parseListMarker`.
fn parse_marker(line: &str) -> Option<Parsed> {
    let indent = &line[..line.bytes().take_while(|&b| b == b' ' || b == b'\t').count()];
    let rest = &line[indent.len()..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0
        && let Some(delimiter @ ('.' | ')')) = rest[digits..].chars().next()
        && let Some(space) = marker_space(&rest[digits + 1..])
    {
        return Some(Parsed {
            marker: Marker {
                kind: Kind::Ordered { number: rest[..digits].to_owned(), delimiter },
                indent: indent.to_owned(),
                space: space.to_owned(),
            },
            end: indent.len() + digits + 1 + space.len(),
            content_column: content_column(indent, digits + 1, space),
        });
    }
    let mut chars = rest.chars();
    if chars.next() == Some('-')
        && let Some(gap) = chars.next().filter(|c| c.is_whitespace())
        && let Some(after) = rest[1 + gap.len_utf8()..].strip_prefix('[')
        && after.starts_with([' ', 'x', 'X'])
        && after[1..].starts_with(']')
        && let Some(space) = marker_space(&after[2..])
    {
        let head = 1 + gap.len_utf8() + 3;
        return Some(Parsed {
            marker: Marker { kind: Kind::Task, indent: indent.to_owned(), space: space.to_owned() },
            end: indent.len() + head + space.len(),
            content_column: content_column(indent, 1, &rest[1..=gap.len_utf8()]),
        });
    }
    let bullet = rest.chars().next().filter(|c| matches!(c, '-' | '*' | '+'))?;
    let space = marker_space(&rest[1..])?;
    Some(Parsed {
        marker: Marker { kind: Kind::Bullet(bullet), indent: indent.to_owned(), space: space.to_owned() },
        end: indent.len() + 1 + space.len(),
        content_column: content_column(indent, 1, space),
    })
}

/// `CommonMark` content follows 1-4 columns of padding; wider padding is code, so the content
/// column sits one column past the marker.
fn content_column(indent: &str, marker_width: usize, padding: &str) -> usize {
    let marker_end = indent_width(indent, 0) + marker_width;
    let space = indent_width(padding, marker_end);
    marker_end + if (1..=4).contains(&space) { space } else { 1 }
}

/// Markdown tabs advance to the next four-column stop.
fn indent_width(line: &str, from: usize) -> usize {
    let mut column = from;
    for byte in line.bytes() {
        match byte {
            b' ' => column += 1,
            b'\t' => column += 4 - column % 4,
            _ => break,
        }
    }
    column - from
}

/// `Number.isSafeInteger` bounds the count; past it the number stays as written.
fn next_number(number: &str) -> String {
    match number.parse::<u64>() {
        Ok(n) if n < (1 << 53) - 1 => format!("{:0width$}", n + 1, width = number.len()),
        _ => number.to_owned(),
    }
}

/// `nextOrderedMarkerText` without the delimiter.
fn next_marker_text(marker: &Marker) -> String {
    match &marker.kind {
        Kind::Ordered { number, delimiter } => format!("{}{}{delimiter} ", marker.indent, next_number(number)),
        Kind::Task => format!("{}- [ ] ", marker.indent),
        Kind::Bullet(bullet) => format!("{}{bullet} ", marker.indent),
    }
}

fn line_at(value: &str, cursor: usize) -> Range<usize> {
    let start = value[..cursor].rfind('\n').map_or(0, |i| i + 1);
    let end = value[cursor..].find('\n').map_or(value.len(), |i| cursor + i);
    start..end
}

fn inside_token(tokens: &[Range<usize>], cursor: usize) -> bool {
    tokens.iter().any(|token| cursor > token.start && cursor < token.end)
}

/// `listMarkersAbove`: the list items directly above the line starting at `line_start`, nearest
/// first; a newline inside a chip does not start a line.
fn markers_above(value: &str, mut line_start: usize, tokens: &[Range<usize>]) -> Vec<Parsed> {
    let mut remaining = tokens;
    let mut found = Vec::new();
    while line_start > 0 {
        let line_end = line_start - 1;
        line_start = value[..line_end].rfind('\n').map_or(0, |i| i + 1);
        while let Some((token, before)) = remaining.split_last() {
            if token.start >= line_start {
                remaining = before;
            } else if token.end > line_start {
                line_start = value[..token.start].rfind('\n').map_or(0, |i| i + 1);
            } else {
                break;
            }
        }
        let Some(parsed) = parse_marker(&value[line_start..line_end]) else { break };
        found.push(parsed);
    }
    found
}

fn parent_marker(value: &str, line_start: usize, marker: &Marker, tokens: &[Range<usize>]) -> Option<Marker> {
    let width = indent_width(&marker.indent, 0);
    markers_above(value, line_start, tokens)
        .into_iter()
        .find(|above| indent_width(&above.marker.indent, 0) < width)
        .map(|above| above.marker)
}

/// `nestedNumber`: (bytes of the number replaced, the number written).
fn nested_number(marker: &Marker, previous: Option<&Marker>) -> (usize, String) {
    let Kind::Ordered { number, delimiter } = &marker.kind else { return (0, String::new()) };
    let text = match previous.map(|p| &p.kind) {
        Some(Kind::Ordered { number: previous, delimiter: d }) if d == delimiter => next_number(previous),
        _ => "1".to_owned(),
    };
    (number.len(), text)
}

/// `renumberFollowingItems`: the siblings after `after_line_end`, counted on from `marker`'s
/// next number, as (end of the rewritten text, its replacement).
fn renumber_following(
    value: &str,
    after_line_end: usize,
    marker: &Marker,
    tokens: &[Range<usize>],
) -> Option<(usize, String)> {
    let Kind::Ordered { number, delimiter } = &marker.kind else { return None };
    let base = indent_width(&marker.indent, 0);
    let mut expected = number.parse::<u64>().ok()?.checked_add(2)?;
    let mut parts = String::new();
    let (mut position, mut copied) = (after_line_end, after_line_end);
    let mut changed = false;
    while position < value.len() && expected < 1 << 53 {
        let line_start = position + 1;
        let line_end = value[line_start..].find('\n').map_or(value.len(), |i| line_start + i);
        position = line_end;
        if tokens.iter().any(|token| token.start < line_start && token.end > line_start) {
            continue;
        }
        let line = &value[line_start..line_end];
        let width = indent_width(line, 0);
        if line.trim().is_empty() || width < base {
            break;
        }
        if width > base {
            continue;
        }
        let Some(Parsed { marker: current, .. }) = parse_marker(line) else { break };
        let Kind::Ordered { number: current_number, delimiter: d } = &current.kind else { break };
        if d != delimiter {
            break;
        }
        let number_start = line_start + current.indent.len();
        let replacement = if current_number.starts_with('0') {
            format!("{expected:0width$}", width = current_number.len())
        } else {
            expected.to_string()
        };
        if replacement != *current_number {
            parts.push_str(&value[copied..number_start]);
            parts.push_str(&replacement);
            copied = number_start + current_number.len();
            changed = true;
        }
        expected += 1;
    }
    changed.then_some((copied, parts))
}

/// `listContinuationForEnter`: a new item after this one, or an empty item lifted a level.
pub fn list_enter(value: &str, cursor: usize, tokens: &[Range<usize>]) -> Option<Edit> {
    let line = line_at(value, cursor);
    let parsed = parse_marker(&value[line.clone()])?;
    if parsed.marker.space.is_empty() {
        return None;
    }
    let marker_end = line.start + parsed.end;
    if cursor < marker_end || inside_token(tokens, cursor) {
        return None;
    }
    if value[marker_end..line.end].trim().is_empty() {
        let parent = parent_marker(value, line.start, &parsed.marker, tokens);
        let replacement = parent.as_ref().map(next_marker_text).unwrap_or_default();
        let caret = line.start + replacement.len();
        let end = cursor.max(marker_end);
        if let Some(parent) = &parent
            && let Some((until, renumbered)) = renumber_following(value, line.end, parent, tokens)
        {
            let text = format!("{replacement}{}{renumbered}", &value[end..line.end]);
            return Some(Edit::caret(line.start..until, text, caret));
        }
        return Some(Edit::caret(line.start..end, replacement, caret));
    }
    let insertion = format!("\n{}", next_marker_text(&parsed.marker));
    let caret = cursor + insertion.len();
    if let Some((until, renumbered)) = renumber_following(value, line.end, &parsed.marker, tokens) {
        let text = format!("{insertion}{}{renumbered}", &value[cursor..line.end]);
        return Some(Edit::caret(cursor..until, text, caret));
    }
    Some(Edit::caret(cursor..cursor, insertion, caret))
}

/// `listIndentForTab`: nests the item under the one above, at that item's content column.
pub fn list_indent(value: &str, cursor: usize, tokens: &[Range<usize>]) -> Option<Edit> {
    let line = line_at(value, cursor);
    let Parsed { marker, .. } = parse_marker(&value[line.clone()])?;
    if inside_token(tokens, cursor) {
        return None;
    }
    let width = indent_width(&marker.indent, 0);
    let mut previous_child: Option<Marker> = None;
    for above in markers_above(value, line.start, tokens) {
        let above_width = indent_width(&above.marker.indent, 0);
        if above_width < width {
            break;
        }
        if above_width > width {
            if previous_child.as_ref().is_none_or(|child| above_width < indent_width(&child.indent, 0)) {
                previous_child = Some(above.marker);
            }
            continue;
        }
        let indent = previous_child.as_ref().map_or_else(
            || format!("{}{}", above.marker.indent, " ".repeat(above.content_column - above_width)),
            |child| child.indent.clone(),
        );
        let (replaced, number) = nested_number(&marker, previous_child.as_ref());
        let replaced_end = line.start + marker.indent.len() + replaced;
        let replacement = format!("{indent}{number}");
        let caret = if cursor < replaced_end {
            line.start + indent.len()
        } else {
            cursor - replaced_end + line.start + replacement.len()
        };
        return Some(Edit::caret(line.start..replaced_end, replacement, caret));
    }
    Some(Edit::caret(line.start..line.start, "  ".to_owned(), cursor + 2))
}

/// `listOutdentForTab`: lifts a nested item to its parent's level; a top-level item is left to
/// Shift+Tab's Build/Plan switch.
pub fn list_outdent(value: &str, cursor: usize, tokens: &[Range<usize>]) -> Option<Edit> {
    let line = line_at(value, cursor);
    let Parsed { marker, .. } = parse_marker(&value[line.clone()])?;
    if marker.indent.is_empty() || inside_token(tokens, cursor) {
        return None;
    }
    let parent = parent_marker(value, line.start, &marker, tokens);
    // With no parent above, undo one step of Tab's two-space fallback.
    let indent = parent.as_ref().map_or_else(
        || {
            let trimmed = marker.indent.strip_prefix('\t').or_else(|| marker.indent.strip_prefix("  "));
            trimmed.or_else(|| marker.indent.strip_prefix(' ')).unwrap_or(&marker.indent).to_owned()
        },
        |parent| parent.indent.clone(),
    );
    let continues = matches!(
        (&parent.as_ref().map(|p| &p.kind), &marker.kind),
        (Some(Kind::Ordered { delimiter: a, .. }), Kind::Ordered { delimiter: b, .. }) if a == b
    );
    let (replaced, number) = if continues { nested_number(&marker, parent.as_ref()) } else { (0, String::new()) };
    let replaced_end = line.start + marker.indent.len() + replaced;
    let replacement = format!("{indent}{number}");
    let caret = if cursor < replaced_end {
        line.start + indent.len()
    } else {
        cursor - replaced_end + line.start + replacement.len()
    };
    if continues
        && let Some(parent) = &parent
        && let Some((until, renumbered)) = renumber_following(value, line.end, parent, tokens)
    {
        let text = format!("{replacement}{}{renumbered}", &value[replaced_end..line.end]);
        return Some(Edit::caret(line.start..until, text, caret));
    }
    Some(Edit::caret(line.start..replaced_end, replacement, caret))
}

const CODE_INDENT: &str = "  ";

/// A fenced block: its opening line, its code, and its closing line once there is one.
struct Fence {
    open: Range<usize>,
    code: Range<usize>,
    close: Option<Range<usize>>,
    fence: String,
}

/// The fenced blocks of `text`, as the composer's Markdown paints them.
fn fences(text: &str) -> Vec<Fence> {
    let mut blocks: Vec<Fence> = Vec::new();
    let mut open: Option<(Range<usize>, u8, usize)> = None;
    let mut offset = 0;
    for line in text.split('\n') {
        let range = offset..offset + line.len();
        offset = range.end + 1;
        match &open {
            Some((start, fence_char, fence_len)) if is_closing_fence(line, *fence_char, *fence_len) => {
                let code = (start.end + 1).min(range.start)..range.start.saturating_sub(1).max(start.end + 1);
                let fence = text[start.start..start.start + fence_len].to_owned();
                blocks.push(Fence { open: start.clone(), code, close: Some(range), fence });
                open = None;
            }
            Some(_) => {}
            None => {
                if let Some((fence_char, fence_len)) = opening_fence(line) {
                    open = Some((range, fence_char, fence_len));
                }
            }
        }
    }
    if let Some((start, _, fence_len)) = open {
        let fence = text[start.start..start.start + fence_len].to_owned();
        let code = (start.end + 1).min(text.len())..text.len();
        blocks.push(Fence { open: start, code, close: None, fence });
    }
    blocks
}

/// The block whose code holds all of `selection` (`selectionInOneCodeBlock`).
fn fence_around<'a>(blocks: &'a [Fence], selection: &Range<usize>) -> Option<&'a Fence> {
    blocks.iter().find(|block| {
        block.open.end < block.code.start && block.code.start <= selection.start && selection.end <= block.code.end
    })
}

/// Enter in rich text, ahead of sending: a new line in code (keeping its indent), out of the block
/// after a closing fence or two blank lines, and into a block after an opening fence.
pub fn code_enter(text: &str, selection: Range<usize>) -> Option<Edit> {
    let blocks = fences(text);
    let caret = selection.start;
    let collapsed = selection.is_empty();
    // `exitCodeBlockOnClosingFence` and `convertCodeFenceOnEnter`: the fence line just typed.
    if collapsed
        && blocks.iter().any(|block| block.open.end == caret || block.close.as_ref().is_some_and(|c| c.end == caret))
    {
        return Some(Edit::caret(caret..caret, "\n".to_owned(), caret + 1));
    }
    let block = fence_around(&blocks, &selection)?;
    if collapsed && caret == block.code.end {
        let code = &text[block.code.start..caret];
        let trimmed = code.trim_end_matches([' ', '\t']);
        if let Some(rest) = trimmed.strip_suffix('\n') {
            let rest = rest.trim_end_matches([' ', '\t']);
            if let Some(kept) = rest.strip_suffix('\n') {
                let from = block.code.start + kept.len();
                // Close the fence on exit so following prose is not read as code.
                return Some(match &block.close {
                    Some(close) => {
                        let replacement = format!("\n{}\n", &text[close.clone()]);
                        let end = from + replacement.len();
                        Edit::caret(from..close.end, replacement, end)
                    }
                    None => {
                        let replacement = format!("\n{}\n", block.fence);
                        let end = from + replacement.len();
                        Edit::caret(from..caret, replacement, end)
                    }
                });
            }
        }
    }
    let line_start = text[..caret].rfind('\n').map_or(0, |i| i + 1).max(block.code.start);
    let line = &text[line_start..caret];
    let indent = &line[..line.bytes().take_while(|&b| b == b' ' || b == b'\t').count()];
    let inserted = format!("\n{indent}");
    let end = caret + inserted.len();
    Some(Edit::caret(selection, inserted, end))
}

/// Tab and Shift+Tab in code (`indentCodeBlock`): two spaces at the caret, or the selected lines
/// indented or outdented.
pub fn code_tab(text: &str, selection: Range<usize>, outdent: bool) -> Option<Edit> {
    let blocks = fences(text);
    let block = fence_around(&blocks, &selection)?;
    if selection.is_empty() && !outdent {
        return Some(Edit::caret(selection.clone(), CODE_INDENT.to_owned(), selection.start + CODE_INDENT.len()));
    }
    let line_start =
        text[block.code.start..selection.start].rfind('\n').map_or(block.code.start, |i| block.code.start + i + 1);
    let search = selection.start.max(selection.end.saturating_sub(1)).min(block.code.end);
    let line_end = text[search..block.code.end].find('\n').map_or(block.code.end, |i| search + i);
    let original: Vec<&str> = text[line_start..line_end].split('\n').collect();
    let next: Vec<String> = original
        .iter()
        .map(|line| match (outdent, line.is_empty()) {
            (false, true) => String::new(),
            (false, false) => format!("{CODE_INDENT}{line}"),
            (true, _) => {
                let trimmed = line.strip_prefix(CODE_INDENT).or_else(|| line.strip_prefix(['\t', ' ']));
                trimmed.unwrap_or(line).to_owned()
            }
        })
        .collect();
    let replacement = next.join("\n");
    let before = &text[line_start..line_end];
    if replacement == before {
        return Some(Edit::keep(selection));
    }
    let delta = |a: usize, b: usize| a.cast_signed() - b.cast_signed();
    let first = delta(next[0].len(), original[0].len());
    let total = delta(replacement.len(), before.len());
    let from = (selection.start.cast_signed() + first).max(line_start.cast_signed()).cast_unsigned();
    let to = (selection.end.cast_signed() + total).max(from.cast_signed()).cast_unsigned();
    Some(Edit { range: line_start..line_end, text: replacement, select: from..to })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(value: &str, edit: &Edit) -> String {
        format!("{}{}{}", &value[..edit.range.start], edit.text, &value[edit.range.end..])
    }

    fn enter(value: &str, cursor: usize) -> String {
        apply(value, &list_enter(value, cursor, &[]).unwrap())
    }

    #[test]
    fn enter_continues_splits_and_leaves_lists() {
        assert_eq!(enter("- foo", 5), "- foo\n- ");
        assert_eq!(enter("  * foo", 7), "  * foo\n  * ");
        assert_eq!(enter("- [x] done", 10), "- [x] done\n- [ ] ");
        assert_eq!(enter("1. foobar", 5), "1. fo\n2. obar");
        assert_eq!(enter("1. foo\n2. ", 10), "1. foo\n");
        assert_eq!(enter("- ", 2), "");
        assert_eq!(enter("1)\t", 3), "");
        assert_eq!(enter("- [ ] ", 6), "");
        assert_eq!(list_enter("plain", 5, &[]), None);
        assert_eq!(list_enter("-x", 2, &[]), None);
        let mut value = "1. parent\n   1. child\n      1. ".to_owned();
        for expected in ["1. parent\n   1. child\n   2. ", "1. parent\n   1. child\n2. ", "1. parent\n   1. child\n"] {
            value = enter(&value, value.len());
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn enter_renumbers_the_items_after_it() {
        let value = "1. a\n   1. \n2. b";
        let edit = list_enter(value, value.find("\n2.").unwrap(), &[]).unwrap();
        assert_eq!(apply(value, &edit), "1. a\n2. \n3. b");
        assert_eq!(edit.select, 8..8);
        let value = "1. go\n2. two\n03. three\n10. four";
        assert_eq!(enter(value, 3), "1. \n2. go\n3. two\n04. three\n5. four");
        let value = "  1) a\n\t1) child\n  2) b\n1) parent\n  3) untouched";
        assert_eq!(enter(value, 6), "  1) a\n  2) \n\t1) child\n  3) b\n1) parent\n  3) untouched");
        for boundary in ["", "text", "1) other list"] {
            let value = format!("1. a\n2. b\n{boundary}\n3. untouched");
            assert_eq!(enter(&value, 4), format!("1. a\n2. \n3. b\n{boundary}\n3. untouched"));
        }
    }

    #[test]
    fn chips_are_never_split() {
        let value = "1. @README.md go";
        let chip = 3..13;
        assert_eq!(list_enter(value, 6, std::slice::from_ref(&chip)), None);
        // A newline inside a chip starts no item.
        let value = "1. a\n2. [f\n2. g](x) \n3. b";
        let chip = 8..19;
        let edit = list_enter(value, 4, &[chip]).unwrap();
        assert_eq!(apply(value, &edit), "1. a\n2. \n3. [f\n2. g](x) \n4. b");
    }

    #[test]
    fn tab_nests_at_the_parents_content_column_and_shift_tab_undoes_it() {
        for (value, expected) in [
            ("1. parent\n2. child", "1. parent\n   1. child"),
            ("10. parent\n11. child", "10. parent\n    1. child"),
            ("1.\tparent\n2. child", "1.\tparent\n    1. child"),
        ] {
            let cursor = value.rfind("child").unwrap() + 2;
            let edit = list_indent(value, cursor, &[]).unwrap();
            assert_eq!(apply(value, &edit), expected);
            assert_eq!(edit.select.start, expected.rfind("child").unwrap() + 2);
        }
        let value = "1. parent\n   1. first\n2. second";
        assert_eq!(
            apply(value, &list_indent(value, value.len(), &[]).unwrap()),
            "1. parent\n   1. first\n   2. second"
        );
        for value in ["- parent\n- child", "1. parent\n2. child", "  7. child"] {
            let cursor = value.rfind("child").unwrap() + 2;
            let nest = list_indent(value, cursor, &[]).unwrap();
            let nested = apply(value, &nest);
            let edit = list_outdent(&nested, nest.select.start, &[]).unwrap();
            assert_eq!(apply(&nested, &edit), value);
            assert_eq!(edit.select.start, cursor);
        }
        for (value, expected) in [("1. a\n   1. b\n2. c", "1. a\n2. b\n3. c"), ("- a\n  5. b", "- a\n5. b")] {
            let cursor = value.find('b').unwrap();
            assert_eq!(apply(value, &list_outdent(value, cursor, &[]).unwrap()), expected);
        }
        assert_eq!(list_outdent("- item", 6, &[]), None, "a top-level item leaves Shift+Tab to Build/Plan");
    }

    #[test]
    fn ordered_markers_count_up_with_their_padding() {
        for (number, expected) in
            [("1", "2"), ("09", "10"), ("001", "002"), ("99", "100"), ("99999999999999999999", "99999999999999999999")]
        {
            assert_eq!(next_number(number), expected);
        }
    }

    fn code(value: &str, caret: usize) -> Option<String> {
        code_enter(value, caret..caret).map(|edit| apply(value, &edit))
    }

    #[test]
    fn enter_writes_code_instead_of_sending() {
        assert_eq!(code("say hi", 6), None);
        assert_eq!(code("```ts", 5).as_deref(), Some("```ts\n"), "an opening fence starts the block");
        assert_eq!(code("```ts\nlet a", 11).as_deref(), Some("```ts\nlet a\n"));
        assert_eq!(code("```\n  if x {\n```", 12).as_deref(), Some("```\n  if x {\n  \n```"));
        assert_eq!(code("```\nx\n```", 9).as_deref(), Some("```\nx\n```\n"), "a closing fence leaves it");
        assert_eq!(code("```\nx\n```\nafter", 15), None, "prose after the block sends");
        assert_eq!(code("```\nx\n", 6).as_deref(), Some("```\nx\n\n"));
        assert_eq!(code("```\nx\n\n", 7).as_deref(), Some("```\nx\n```\n"), "two blank lines close it");
        assert_eq!(code("```\nx\n\n \n```\nmore", 8).as_deref(), Some("```\nx\n```\n\nmore"));
        let edit = code_enter("```\nab\n```", 5..6).unwrap();
        assert_eq!(apply("```\nab\n```", &edit), "```\na\n\n```");
    }

    #[test]
    fn tab_indents_code_lines() {
        let value = "```\na\n\nb\n```";
        let edit = code_tab(value, 4..4, false).unwrap();
        assert_eq!((apply(value, &edit), edit.select), ("```\n  a\n\nb\n```".to_owned(), 6..6));
        let edit = code_tab(value, 4..8, false).unwrap();
        assert_eq!((apply(value, &edit), edit.select), ("```\n  a\n\n  b\n```".to_owned(), 6..12));
        let indented = "```\n  a\n\tb\n```";
        let edit = code_tab(indented, 6..9, true).unwrap();
        assert_eq!((apply(indented, &edit), edit.select), ("```\na\nb\n```".to_owned(), 4..6));
        assert!(code_tab("```\na\n```", 4..4, true).unwrap().is_noop());
        assert_eq!(code_tab("- a", 3..3, false), None);
    }
}
