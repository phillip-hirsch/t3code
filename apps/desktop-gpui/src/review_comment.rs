//! Review comments on diff and file lines (`reviewCommentContext.ts`, `fileCommentAnnotations.ts`):
//! the `review-comment` context record a comment becomes, where a held one sits again, and how a
//! file comment follows its lines through edits.

use std::ops::Range;

use gpui_kit::component::diff::DiffFile;
use gpui_kit::component::input::{Rope, RopeExt as _};
use serde_json::{Value, json};

use crate::composer_context::{clamp_text, sanitize_label};
use crate::pull_request_diff::{Row, Side};

pub const KIND: &str = "review-comment";
/// `COMPOSER_CONTEXT_REVIEW_TEXT_MAX_CHARS`, `COMPOSER_CONTEXT_REVIEW_DIFF_MAX_CHARS`.
const TEXT_MAX: usize = 16_000;
const DIFF_MAX: usize = 32_000;

/// One end of a diff selection: its side and one-based line.
pub type Point = (Side, u64);

/// Where a comment belongs: a diff selection's (`turn:…`, `branch`, …) or a file's.
pub struct Section<'a> {
    pub id: &'a str,
    pub title: &'a str,
}

/// A viewer file's patch body as rows, in patch order.
pub fn rows(file: &DiffFile) -> Vec<Row<'_>> {
    let line = |n: Option<usize>| n.and_then(|n| u64::try_from(n).ok());
    file.patch_body()
        .map(|body| {
            let (old, new) = (line(body.original()), line(body.modified()));
            let mark = match (old, new) {
                (Some(_), None) => '-',
                (None, Some(_)) => '+',
                _ => ' ',
            };
            Row { mark, old, new, text: body.text(), hunk: 0 }
        })
        .collect()
}

/// `findDiffReviewLineIndex` over the patch's rows: the line on its side, else on the other.
fn row_index(rows: &[Row], (side, line): Point) -> Option<usize> {
    let on = |side| {
        rows.iter().position(|row| match side {
            Side::Left => row.old == Some(line),
            Side::Right => row.new == Some(line),
        })
    };
    on(side).or_else(|| on(if side == Side::Left { Side::Right } else { Side::Left }))
}

/// `formatDiffReviewRangeLabel`: `+3`, `-3 to -5`, `3 to 9`.
fn diff_range_label(rows: &[Row]) -> String {
    let (Some(first), Some(last)) = (rows.first(), rows.last()) else { return "line".into() };
    let (Some(from), Some(to)) = (first.new.or(first.old), last.new.or(last.old)) else {
        return if rows.len() == 1 { "line".into() } else { format!("{} lines", rows.len()) };
    };
    let marker = if first.mark != ' ' && rows.iter().all(|row| row.mark == first.mark) {
        first.mark.to_string()
    } else {
        String::new()
    };
    if from == to { format!("{marker}{from}") } else { format!("{marker}{from} to {marker}{to}") }
}

/// `buildDiffReviewComment` as its record: the rows from `start` to `end` quoted as a hunk;
/// `None` when either end is not in the patch.
pub fn diff_comment(
    context_id: &str,
    section: &Section,
    file_path: &str,
    rows: &[Row],
    start: Point,
    end: Point,
    text: &str,
) -> Option<Value> {
    let (a, b) = (row_index(rows, start)?, row_index(rows, end)?);
    let (first, last) = (a.min(b), a.max(b));
    let selected = &rows[first..=last];
    let range = |line: fn(&Row) -> Option<u64>| {
        let numbered: Vec<u64> = selected.iter().filter_map(line).collect();
        (numbered.first().copied().unwrap_or(0), numbered.len())
    };
    let ((old_start, old_count), (new_start, new_count)) = (range(|row| row.old), range(|row| row.new));
    let mut diff = format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@");
    for row in selected {
        diff.push('\n');
        diff.push(row.mark);
        diff.push_str(row.text);
    }
    let range_label = diff_range_label(selected);
    Some(record(context_id, section, file_path, (first, last), &range_label, text, &diff, "diff"))
}

/// `restoreDiffReviewCommentRange` for a held record: the selection its rows cover.
pub fn diff_range(rows: &[Row], record: &Value) -> Option<(Point, Point)> {
    let index = |key: &str| usize::try_from(record[key].as_u64()?).ok();
    let point = |row: &Row| match (row.mark, row.old, row.new) {
        ('-', Some(old), _) => Some((Side::Left, old)),
        (_, _, Some(new)) => Some((Side::Right, new)),
        (_, old, None) => old.map(|old| (Side::Left, old)),
    };
    let (start, end) = (rows.get(index("startIndex")?)?, rows.get(index("endIndex")?)?);
    Some((point(start)?, point(end)?))
}

/// The section id of `path`'s file comments.
pub fn file_section(path: &str) -> String {
    format!("file:{path}")
}

/// `buildFileReviewComment` as its record: lines `start` to `end` (one-based, either order) of
/// `contents`.
pub fn file_comment(context_id: &str, file_path: &str, start: u64, end: u64, text: &str, contents: &str) -> Value {
    let first = start.min(end).max(1);
    let last = start.max(end).max(first);
    let skip = usize::try_from(first - 1).unwrap_or(usize::MAX);
    let take = usize::try_from(last - first + 1).unwrap_or(usize::MAX);
    let quoted = contents.split('\n').skip(skip).take(take).collect::<Vec<_>>().join("\n");
    let section_id = file_section(file_path);
    let section = Section { id: &section_id, title: "File comment" };
    let indices = (skip, usize::try_from(last - 1).unwrap_or(usize::MAX));
    let language = fence_language(file_path);
    record(context_id, &section, file_path, indices, &file_range_label(first, last), text, &quoted, &language)
}

/// `formatFileCommentRange`.
pub fn file_range_label(start: u64, end: u64) -> String {
    if start == end { format!("L{start}") } else { format!("L{start} to L{end}") }
}

/// A held file comment's one-based lines.
pub fn file_lines(record: &Value) -> Option<(u64, u64)> {
    Some((record["startIndex"].as_u64()? + 1, record["endIndex"].as_u64()? + 1))
}

/// Whether `record` is a comment on `path` in the diff selection `section_id`.
pub fn in_diff(record: &Value, section_id: &str, path: &str) -> bool {
    record["kind"] == KIND
        && record["sectionId"] == section_id
        && record["filePath"] == path
        && record["fenceLanguage"].as_str().unwrap_or("diff") == "diff"
}

/// Whether `record` is a comment on the lines of the file at `path`.
pub fn in_file(record: &Value, path: &str) -> bool {
    record["kind"] == KIND && record["filePath"] == path && record["sectionId"].as_str() == Some(&file_section(path))
}

/// `inferReviewCommentFenceLanguage`.
pub fn fence_language(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let name = normalized.rsplit('/').next().unwrap_or_default().to_lowercase();
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot < name.len() - 1 => name[dot + 1..].to_owned(),
        _ if name.len() > 1 && name.starts_with('.') => name[1..].to_owned(),
        _ => "text".into(),
    }
}

/// `+3 to +5` as its sign and lines.
fn signed_range(label: &str) -> Option<(char, &str, Option<&str>)> {
    let sign = label.chars().next().filter(|c| matches!(c, '+' | '-'))?;
    let digits = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then_some(());
    let rest = &label[1..];
    match rest.split_once(" to ") {
        Some((from, to)) => {
            let to = to.strip_prefix(sign)?;
            digits(from).and(digits(to)).map(|()| (sign, from, Some(to)))
        }
        None => digits(rest).map(|()| (sign, rest, None)),
    }
}

/// `reviewCommentContextLabel` for a line comment: `#7 a.ts L3 to L5 (before)`.
fn label(section_id: &str, file_path: &str, range_label: &str) -> String {
    let range = match signed_range(range_label) {
        Some((sign, from, to)) => format!(
            "L{from}{}{}",
            to.map(|to| format!(" to L{to}")).unwrap_or_default(),
            if sign == '-' { " (before)" } else { "" }
        ),
        None => range_label.to_owned(),
    };
    let prefix = section_id
        .strip_prefix("pull-request:")
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .map(|n| format!("#{n} "))
        .unwrap_or_default();
    let base = file_path.rsplit(['/', '\\']).next().unwrap_or(file_path);
    format!("{prefix}{base} {range}")
}

/// `reviewCommentContextRecord`.
fn record(
    context_id: &str,
    section: &Section,
    file_path: &str,
    (start_index, end_index): (usize, usize),
    range_label: &str,
    text: &str,
    diff: &str,
    fence_language: &str,
) -> Value {
    json!({
        "version": 1,
        "contextId": context_id,
        "kind": KIND,
        "label": sanitize_label(&label(section.id, file_path, range_label), KIND),
        "sectionId": section.id,
        "sectionTitle": section.title,
        "filePath": file_path,
        "startIndex": start_index,
        "endIndex": end_index,
        "rangeLabel": range_label,
        "text": clamp_text(text.trim(), TEXT_MAX),
        "diff": clamp_text(diff, DIFF_MAX),
        "fenceLanguage": fence_language,
    })
}

/// The bytes one-based lines `start` to `end` cover, up to the next line's start (the text's
/// end past its last line).
pub fn line_span(text: &Rope, start: u64, end: u64) -> Range<usize> {
    let row = |line: u64| usize::try_from(line.max(1) - 1).unwrap_or(usize::MAX);
    text.line_start_offset(row(start))..text.line_start_offset(row(end).saturating_add(1))
}

/// The one-based lines a byte range touches; a range ending at a line's start leaves that line out.
pub fn lines_of(text: &Rope, range: Range<usize>) -> (u64, u64) {
    let start = text.offset_to_point(range.start).row;
    let mut end = text.offset_to_point(range.end).row.max(start);
    if end > start && text.line_start_offset(end) == range.end {
        end -= 1;
    }
    let line = |row: usize| u64::try_from(row).unwrap_or(u64::MAX).saturating_add(1);
    (line(start), line(end))
}

/// `remapFileCommentAnnotations`: a comment spanning `span` lines past its first stays on the
/// line its tracked range now ends on, as many lines tall.
pub fn remapped_lines(text: &Rope, range: Range<usize>, span: u64) -> (u64, u64) {
    let (_, end) = lines_of(text, range);
    (end.saturating_sub(span).max(1), end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull_request_diff::{file_rows, index_patch};

    const PATCH: &str = "diff --git a/src/a.ts b/src/a.ts\n--- a/src/a.ts\n+++ b/src/a.ts\n@@ -10,3 +10,4 @@\n keep\n-old\n+new\n+extra\n tail\n";

    #[test]
    fn a_diff_selection_quotes_its_rows_and_sits_on_them_again() {
        let files = index_patch(PATCH);
        let rows = file_rows(&files[0]);
        let section = Section { id: "turn:r1", title: "Turn 2" };
        let added =
            diff_comment("rc1", &section, "src/a.ts", &rows, (Side::Right, 11), (Side::Right, 12), " fix ").unwrap();
        assert_eq!(added["rangeLabel"], "+11 to +12");
        assert_eq!(added["label"], "a.ts L11 to L12");
        assert_eq!(added["diff"], "@@ -0,0 +11,2 @@\n+new\n+extra");
        assert_eq!((added["startIndex"].as_u64(), added["endIndex"].as_u64()), (Some(2), Some(3)));
        assert_eq!((added["text"].as_str(), added["sectionTitle"].as_str()), (Some("fix"), Some("Turn 2")));
        assert!(in_diff(&added, "turn:r1", "src/a.ts") && !in_diff(&added, "branch", "src/a.ts"));
        assert_eq!(diff_range(&rows, &added), Some(((Side::Right, 11), (Side::Right, 12))));

        let removed = diff_comment("rc2", &section, "src/a.ts", &rows, (Side::Left, 11), (Side::Left, 11), "").unwrap();
        assert_eq!(
            (removed["rangeLabel"].as_str(), removed["label"].as_str()),
            (Some("-11"), Some("a.ts L11 (before)"))
        );
        assert_eq!(diff_range(&rows, &removed), Some(((Side::Left, 11), (Side::Left, 11))));

        // Bottom-up across both sides: the rows between, unsigned.
        let mixed = diff_comment("rc3", &section, "src/a.ts", &rows, (Side::Right, 13), (Side::Left, 10), "").unwrap();
        assert_eq!(mixed["rangeLabel"], "10 to 13");
        assert_eq!(mixed["diff"], "@@ -10,3 +10,4 @@\n keep\n-old\n+new\n+extra\n tail");
        assert_eq!(diff_range(&rows, &mixed), Some(((Side::Right, 10), (Side::Right, 13))));

        // A line only on the other side is found there; one outside the patch is not.
        assert!(diff_comment("rc4", &section, "src/a.ts", &rows, (Side::Left, 13), (Side::Left, 13), "").is_some());
        assert!(diff_comment("rc5", &section, "src/a.ts", &rows, (Side::Right, 99), (Side::Right, 99), "").is_none());
        let mut moved = added;
        moved["startIndex"] = 40.into();
        assert_eq!(diff_range(&rows, &moved), None, "a record whose rows are gone has no place");
    }

    #[test]
    fn a_file_comment_quotes_its_lines() {
        let record = file_comment("rc1", "src/x.RS", 3, 2, "  look ", "a\nb\nc\nd");
        assert_eq!(record["diff"], "b\nc");
        assert_eq!(record["rangeLabel"], "L2 to L3");
        assert_eq!(record["label"], "x.RS L2 to L3");
        assert_eq!(record["sectionId"], "file:src/x.RS");
        assert_eq!(record["fenceLanguage"], "rs");
        assert_eq!(record["text"], "look");
        assert_eq!(file_lines(&record), Some((2, 3)));
        assert!(in_file(&record, "src/x.RS") && !in_file(&record, "src/y.rs"));
        assert!(!in_diff(&record, "file:src/x.RS", "src/x.RS"), "a file comment is not a diff comment");
        assert_eq!(file_comment("rc2", "a.txt", 0, 0, "", "one")["rangeLabel"], "L1");
        assert_eq!(fence_language(".env"), "env");
        assert_eq!(fence_language("dir.d\\Makefile"), "text");
        assert_eq!(fence_language("archive.tar.gz"), "gz");
    }

    #[test]
    fn labels_read_like_the_web() {
        assert_eq!(label("pull-request:7", "src/a.ts", "-3 to -5"), "#7 a.ts L3 to L5 (before)");
        assert_eq!(label("branch", "a.ts", "3 to 9"), "a.ts 3 to 9");
        assert_eq!(label("branch", "a.ts", "+3 to -5"), "a.ts +3 to -5");
        let record = file_comment("rc", "[odd]\\name.ts", 1, 1, "", "x");
        assert_eq!(record["label"], "name.ts L1");
    }

    #[test]
    fn a_file_comment_follows_its_lines_through_edits() {
        let text = &Rope::from("one\ntwo\nthree\n");
        let span = line_span(text, 2, 3);
        assert_eq!(span, 4..14);
        assert_eq!(lines_of(text, span), (2, 3));
        assert_eq!(lines_of(text, 5..5), (2, 2), "a caret is its line");
        assert_eq!(lines_of(text, 4..8), (2, 2), "a whole line with its newline");
        assert_eq!(line_span(&Rope::from("a\nb"), 2, 2), 2..3);

        // "zero\n" typed above slides the tracked range down a line.
        let edited = Rope::from("zero\none\ntwo\nthree\n");
        assert_eq!(remapped_lines(&edited, 9..19, 1), (3, 4));
        // Its lines deleted, the comment keeps its height above where they were.
        assert_eq!(remapped_lines(&Rope::from("one\n"), 4..4, 1), (1, 2));
        assert_eq!(remapped_lines(&Rope::from("one\ntwo\n"), 4..4, 3), (1, 2));
    }
}
