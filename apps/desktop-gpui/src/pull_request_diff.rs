//! The pull request Code tab's diff rules: where review threads sit, what a selection anchors
//! a comment to, whitespace hiding, test files and file order (`pullRequestDiff.logic.ts`,
//! `reviewCommentContext.ts`, `diffRendering.ts`, `pullRequestFileOrder.logic.ts`,
//! `changedFileKind.ts`). The viewer keeps its line data private, so the patch is indexed here.

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn parse(side: &str) -> Self {
        if side == "left" { Side::Left } else { Side::Right }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Side::Left => "left",
            Side::Right => "right",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Added,
    Deleted,
    Context,
}

#[derive(Clone, Debug)]
struct Line {
    change: Change,
    old: u64,
    new: u64,
    text: String,
}

#[derive(Clone, Debug)]
struct Hunk {
    old_start: u64,
    old_count: u64,
    new_start: u64,
    new_count: u64,
    lines: Vec<Line>,
}

/// One file of a patch, as far as review anchoring needs it.
#[derive(Clone, Debug)]
pub struct FilePatch {
    /// The new path, or the old one of a deleted file: the viewer's `DiffFile::path`.
    pub path: String,
    pub old_path: Option<String>,
    hunks: Vec<Hunk>,
}

fn strip_side(path: &str, prefix: &str) -> Option<String> {
    let path = path.split('\t').next().unwrap_or(path).trim_end();
    (path != "/dev/null").then(|| path.strip_prefix(prefix).unwrap_or(path).to_owned())
}

fn hunk_header(line: &str) -> Option<(u64, u64, u64, u64)> {
    let ranges = line.strip_prefix("@@ -")?.split(" @@").next()?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |text: &str| -> Option<(u64, u64)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    let ((old_start, old_count), (new_start, new_count)) = (range(old)?, range(new)?);
    Some((old_start, old_count, new_start, new_count))
}

/// Indexes every file of a unified or git patch.
pub fn index_patch(patch: &str) -> Vec<FilePatch> {
    let mut files: Vec<FilePatch> = Vec::new();
    let mut old: Option<String> = None;
    let mut remaining = (0u64, 0u64);
    let mut at = (0u64, 0u64);
    for line in patch.lines() {
        if remaining != (0, 0) {
            let Some(file) = files.last_mut() else { break };
            let Some(hunk) = file.hunks.last_mut() else { break };
            let change = match line.as_bytes().first() {
                Some(b'+') => Change::Added,
                Some(b'-') => Change::Deleted,
                Some(b'\\') => continue,
                _ => Change::Context,
            };
            hunk.lines.push(Line { change, old: at.0, new: at.1, text: line.get(1..).unwrap_or_default().to_owned() });
            if change != Change::Added {
                at.0 += 1;
                remaining.0 = remaining.0.saturating_sub(1);
            }
            if change != Change::Deleted {
                at.1 += 1;
                remaining.1 = remaining.1.saturating_sub(1);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b).to_owned();
            files.push(FilePatch { path, old_path: None, hunks: Vec::new() });
            old = None;
        } else if let Some(rest) = line.strip_prefix("--- ") {
            old = strip_side(rest, "a/");
            if files.last().is_none_or(|f| !f.hunks.is_empty()) {
                files.push(FilePatch { path: String::new(), old_path: None, hunks: Vec::new() });
            }
            if let Some(file) = files.last_mut() {
                file.old_path = old.clone();
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if let Some(file) = files.last_mut() {
                file.path = strip_side(rest, "b/").or_else(|| old.clone()).unwrap_or_default();
            }
        } else if let Some((old_start, old_count, new_start, new_count)) = hunk_header(line) {
            let Some(file) = files.last_mut() else { continue };
            file.hunks.push(Hunk { old_start, old_count, new_start, new_count, lines: Vec::new() });
            remaining = (old_count, new_count);
            at = (old_start, new_start);
        }
    }
    files
}

/// `isLineInFileDiff`: only a line inside a hunk can anchor a comment.
pub fn line_in_diff(file: &FilePatch, side: Side, line: u64) -> bool {
    file.hunks.iter().any(|hunk| match side {
        Side::Left => line >= hunk.old_start && line < hunk.old_start + hunk.old_count,
        Side::Right => line >= hunk.new_start && line < hunk.new_start + hunk.new_count,
    })
}

/// `PullRequestReviewPosition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Added { new: u64 },
    Deleted { old: u64 },
    Context { old: u64, new: u64, side: Side },
}

impl Position {
    /// `getReviewPositionAnchor`: the side and line it is shown at.
    pub fn anchor(self) -> (Side, u64) {
        match self {
            Position::Added { new } => (Side::Right, new),
            Position::Deleted { old } => (Side::Left, old),
            Position::Context { old, new, side } => (side, if side == Side::Left { old } else { new }),
        }
    }

    pub fn to_json(self) -> Value {
        match self {
            Position::Added { new } => json!({"kind": "added", "newLine": new}),
            Position::Deleted { old } => json!({"kind": "deleted", "oldLine": old}),
            Position::Context { old, new, side } => {
                json!({"kind": "context", "oldLine": old, "newLine": new, "side": side.as_str()})
            }
        }
    }
}

/// The line's place in diff order and its hunk.
fn find_line(file: &FilePatch, side: Side, line: u64) -> Option<(usize, usize, Line)> {
    let mut index = 0;
    for (hunk_index, hunk) in file.hunks.iter().enumerate() {
        for entry in &hunk.lines {
            let hit = match side {
                Side::Left => entry.change != Change::Added && entry.old == line,
                Side::Right => entry.change != Change::Deleted && entry.new == line,
            };
            if hit {
                return Some((index, hunk_index, entry.clone()));
            }
            index += 1;
        }
    }
    None
}

/// `resolveDiffReviewPosition`.
pub fn review_position(file: &FilePatch, side: Side, line: u64) -> Option<Position> {
    let (_, _, entry) = find_line(file, side, line)?;
    Some(match entry.change {
        Change::Added => Position::Added { new: entry.new },
        Change::Deleted => Position::Deleted { old: entry.old },
        Change::Context => Position::Context { old: entry.old, new: entry.new, side },
    })
}

/// `resolveDiffReviewAnchor`: where a comment on a selection sits. With `ranged`, a selection
/// inside one hunk keeps its first line as the start; hosts refuse a range across hunks.
pub fn review_anchor(
    file: &FilePatch,
    start: (Side, u64),
    end: (Side, u64),
    ranged: bool,
) -> Option<(Position, Option<Position>)> {
    let last = review_position(file, end.0, end.1)?;
    let first = if ranged { review_position(file, start.0, start.1) } else { None };
    let Some(first) = first else { return Some((last, None)) };
    let (start_index, start_hunk, _) = find_line(file, start.0, start.1)?;
    let (end_index, end_hunk, _) = find_line(file, end.0, end.1)?;
    if start_index == end_index || start_hunk != end_hunk {
        return Some((last, None));
    }
    Some(if start_index < end_index { (last, Some(first)) } else { (first, Some(last)) })
}

/// One line of a file's diff (`DiffRow`), for showing a few lines of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// `+`, `-` or a space.
    pub mark: char,
    pub old: Option<u64>,
    pub new: Option<u64>,
    pub text: String,
    pub hunk: usize,
}

/// `fileDiffRows`.
pub fn file_rows(file: &FilePatch) -> Vec<Row> {
    file.hunks
        .iter()
        .enumerate()
        .flat_map(|(hunk, h)| {
            h.lines.iter().map(move |line| Row {
                mark: match line.change {
                    Change::Added => '+',
                    Change::Deleted => '-',
                    Change::Context => ' ',
                },
                old: (line.change != Change::Added).then_some(line.old),
                new: (line.change != Change::Deleted).then_some(line.new),
                text: line.text.clone(),
                hunk,
            })
        })
        .collect()
}

/// `excerptRows`: the lines a guide excerpt names, with the removed lines an added run replaced
/// (or the added lines a removed run became) kept beside them.
pub fn excerpt_rows(rows: &[Row], new_side: bool, start: u64, end: u64) -> Vec<Row> {
    let line_of = |row: &Row| if new_side { row.new } else { row.old };
    let in_range = |row: &Row| line_of(row).is_some_and(|line| line >= start && line <= end);
    let Some(mut first) = rows.iter().position(in_range) else { return Vec::new() };
    let mut last = rows.iter().rposition(in_range).unwrap_or(first);
    if new_side && rows[first].mark == '+' {
        while first > 0 && rows[first - 1].mark == '-' && rows[first - 1].hunk == rows[first].hunk {
            first -= 1;
        }
    }
    if !new_side && rows[last].mark == '-' {
        while last + 1 < rows.len() && rows[last + 1].mark == '+' && rows[last + 1].hunk == rows[last].hunk {
            last += 1;
        }
    }
    rows[first..=last].to_vec()
}

/// `hideWhitespaceChanges`: within each run of removed and added lines, the pairs that differ
/// only in whitespace become context. Line counts are unchanged, so hunk headers stay valid.
pub fn hide_whitespace(patch: &str) -> String {
    let mut out = String::with_capacity(patch.len());
    let mut deleted: Vec<&str> = Vec::new();
    let mut added: Vec<&str> = Vec::new();
    let mut remaining = (0u64, 0u64);
    for line in patch.split_inclusive('\n') {
        let in_hunk = remaining != (0, 0);
        let kind = line.as_bytes().first().copied();
        if in_hunk && matches!(kind, Some(b'-' | b'+')) {
            if kind == Some(b'-') {
                deleted.push(line);
                remaining.0 = remaining.0.saturating_sub(1);
            } else {
                added.push(line);
                remaining.1 = remaining.1.saturating_sub(1);
            }
            continue;
        }
        flush_run(&mut out, &mut deleted, &mut added);
        if in_hunk && kind != Some(b'\\') {
            remaining = (remaining.0.saturating_sub(1), remaining.1.saturating_sub(1));
        } else if let Some((_, old_count, _, new_count)) = hunk_header(line.trim_end()) {
            remaining = (old_count, new_count);
        }
        out.push_str(line);
    }
    flush_run(&mut out, &mut deleted, &mut added);
    out
}

/// Larger runs are left as they are rather than spend quadratic time on them.
const WHITESPACE_RUN_LIMIT: usize = 4_000_000;

fn flush_run(out: &mut String, deleted: &mut Vec<&str>, added: &mut Vec<&str>) {
    if deleted.is_empty() || added.is_empty() || deleted.len() * added.len() > WHITESPACE_RUN_LIMIT {
        deleted.iter().chain(added.iter()).for_each(|line| out.push_str(line));
        deleted.clear();
        added.clear();
        return;
    }
    let squash = |line: &str| line[1..].chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let (old, new): (Vec<String>, Vec<String>) =
        (deleted.iter().map(|l| squash(l)).collect(), added.iter().map(|l| squash(l)).collect());
    // Longest common subsequence, filled from the end so the walk below goes forward.
    let (n, m) = (old.len(), new.len());
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i * (m + 1) + j] = if old[i] == new[j] {
                table[(i + 1) * (m + 1) + j + 1] + 1
            } else {
                table[(i + 1) * (m + 1) + j].max(table[i * (m + 1) + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && old[i] == new[j] {
            out.push(' ');
            out.push_str(&added[j][1..]);
            i += 1;
            j += 1;
        } else if j >= m || (i < n && table[(i + 1) * (m + 1) + j] >= table[i * (m + 1) + j + 1]) {
            out.push_str(deleted[i]);
            i += 1;
        } else {
            out.push_str(added[j]);
            j += 1;
        }
    }
    deleted.clear();
    added.clear();
}

const GENERATED_FILES: [&str; 20] = [
    "bun.lock",
    "bun.lockb",
    "cargo.lock",
    "composer.lock",
    "deno.lock",
    "flake.lock",
    "gemfile.lock",
    "go.sum",
    "gradle.lockfile",
    "mix.lock",
    "npm-shrinkwrap.json",
    "package-lock.json",
    "packages.lock.json",
    "pipfile.lock",
    "pnpm-lock.yaml",
    "podfile.lock",
    "poetry.lock",
    "pubspec.lock",
    "uv.lock",
    "yarn.lock",
];
const GENERATED_DIRS: [&str; 3] = ["__generated__", "_generated", "generated"];
const MOCK_DIRS: [&str; 10] = [
    "__fixtures__",
    "__mocks__",
    "__snapshots__",
    "fakes",
    "fixture",
    "fixtures",
    "mock",
    "mocks",
    "stubs",
    "testdata",
];
const TEST_DIRS: [&str; 9] = ["__tests__", "__test__", "cypress", "e2e", "evals", "spec", "specs", "test", "tests"];
const MOCK_WORDS: [&str; 8] = ["mock", "mocks", "fixture", "fixtures", "stub", "stubs", "fake", "fakes"];

fn is_generated_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".generated.", ".gen."].iter().any(|m| lower.contains(m))
        || [
            ".g.dart",
            ".pb.go",
            "_pb2.py",
            "_pb2_grpc.py",
            ".pb.cc",
            ".pb.h",
            ".min.js",
            ".min.css",
            ".js.map",
            ".css.map",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

/// The extension-free stem's last dotted part, e.g. `a.mock` of `a.mock.ts`.
fn dotted_kind(name: &str) -> Option<&str> {
    let stem = &name[..name.rfind('.')?];
    stem.rsplit('.').next().filter(|_| stem.contains('.'))
}

fn is_mock_name(name: &str) -> bool {
    if name.ends_with(".snap") || dotted_kind(name).is_some_and(|kind| MOCK_WORDS.contains(&kind)) {
        return true;
    }
    MOCK_WORDS.iter().any(|word| {
        name.strip_prefix(word).is_some_and(|rest| {
            rest.is_empty() || rest.starts_with(['-', '_', '.']) || rest.starts_with(char::is_uppercase)
        })
    })
}

fn is_test_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    let stem_kind = dotted_kind(&lower);
    let underscore = lower
        .rfind('.')
        .map(|dot| &lower[..dot])
        .is_some_and(|stem| ["_test", "_spec"].iter().any(|suffix| stem.ends_with(suffix)));
    let class_test = ["Tests", "Test", "Spec"].iter().any(|word| {
        ["java", "kt", "kts", "scala", "groovy", "cs", "php", "swift", "m"]
            .iter()
            .any(|ext| name.ends_with(&format!("{word}.{ext}")))
    });
    stem_kind.is_some_and(|kind| ["test", "spec", "e2e", "eval"].contains(&kind))
        || underscore
        || (lower.starts_with("test_") && lower.ends_with(".py"))
        || lower == "conftest.py"
        || ["vitest.setup.", "jest.setup."].iter().any(|prefix| lower.starts_with(prefix))
        || class_test
}

/// `isTestChangedFile`: tests and their support files, judged from the path.
pub fn is_test_file(path: &str) -> bool {
    let mut segments: Vec<&str> = path.split(['/', '\\']).collect();
    let name = segments.pop().unwrap_or_default();
    let dirs: Vec<String> = segments.iter().map(|s| s.to_lowercase()).collect();
    let in_dir = |set: &[&str]| dirs.iter().any(|d| set.contains(&d.as_str()));
    if GENERATED_FILES.contains(&name.to_lowercase().as_str()) || is_generated_name(name) || in_dir(&GENERATED_DIRS) {
        return false;
    }
    is_mock_name(name) || in_dir(&MOCK_DIRS) || is_test_name(name) || in_dir(&TEST_DIRS)
}

/// `diffFileTier`: source first, then tests, then generated files.
fn file_tier(path: &str) -> u8 {
    let segments: Vec<&str> = path.split('/').collect();
    let (name, dirs) = segments.split_last().map_or(("", &[][..]), |(n, d)| (*n, d));
    let generated = [
        "pnpm-lock.yaml",
        "yarn.lock",
        "package-lock.json",
        "bun.lockb",
        "Cargo.lock",
        "go.sum",
        "composer.lock",
        "Gemfile.lock",
    ]
    .contains(&name)
        || [".snap", ".min.js", ".min.css"].iter().any(|s| name.ends_with(s))
        || name.contains(".generated.")
        || dirs.iter().any(|d| ["__snapshots__", "__generated__", "dist", "build", "vendor"].contains(d));
    if generated {
        2
    } else if name.contains(".test.")
        || name.contains(".spec.")
        || dirs.iter().any(|d| ["__tests__", "tests", "test"].contains(d))
    {
        1
    } else {
        0
    }
}

/// `orderDiffFiles` then `orderDiffFilesLikeTree`: tiers, alphabetical within each, then each
/// directory kept together at its first file's place.
pub fn order_files(paths: &[String]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..paths.len()).collect();
    order.sort_by(|&a, &b| file_tier(&paths[a]).cmp(&file_tier(&paths[b])).then_with(|| paths[a].cmp(&paths[b])));
    let mut positions: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (rank, &index) in order.iter().enumerate() {
        let path = &paths[index];
        positions.entry(path.clone()).or_insert(rank);
        let mut directory = String::new();
        for segment in path.split('/').collect::<Vec<_>>().split_last().map_or(&[][..], |(_, d)| d) {
            directory.push_str(segment);
            directory.push('/');
            positions.entry(directory.clone()).or_insert(rank);
        }
    }
    let key = |path: &str| -> Vec<usize> {
        let mut key = Vec::new();
        let mut directory = String::new();
        let segments: Vec<&str> = path.split('/').collect();
        for segment in &segments[..segments.len() - 1] {
            directory.push_str(segment);
            directory.push('/');
            key.push(positions[&directory]);
        }
        key.push(positions[path]);
        key
    };
    order.sort_by_key(|&index| key(&paths[index]));
    order
}

/// `getFileViewedRange`: a shift-click marks every file between it and the last one marked.
pub fn viewed_range(paths: &[String], path: &str, anchor: Option<&str>) -> Vec<String> {
    let Some(index) = paths.iter().position(|p| p == path) else { return Vec::new() };
    match anchor.and_then(|anchor| paths.iter().position(|p| p == anchor)) {
        Some(other) => paths[index.min(other)..=index.max(other)].to_vec(),
        None => vec![path.to_owned()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,4 +10,4 @@ fn main() {
 keep
-old
+new
+more
 tail
-- comment
@@ -40,2 +41,2 @@
 x
-y
+y2
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
";

    #[test]
    fn a_patch_indexes_each_file_and_where_its_hunks_sit() {
        let files = index_patch(PATCH);
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["src/a.rs", "gone.txt"], "a deleted file keeps its old path");
        let a = &files[0];
        assert!(line_in_diff(a, Side::Right, 13) && !line_in_diff(a, Side::Right, 14));
        assert!(line_in_diff(a, Side::Left, 13), "a removed line that looks like a header stays in the hunk");
        assert_eq!(review_position(a, Side::Right, 11), Some(Position::Added { new: 11 }));
        assert_eq!(review_position(a, Side::Left, 11), Some(Position::Deleted { old: 11 }));
        assert_eq!(review_position(a, Side::Left, 13), Some(Position::Deleted { old: 13 }));
        assert_eq!(review_position(a, Side::Left, 10), Some(Position::Context { old: 10, new: 10, side: Side::Left }));
        assert_eq!(review_position(a, Side::Right, 30), None, "a line outside every hunk anchors nothing");
    }

    #[test]
    fn a_selection_anchors_at_its_end_and_keeps_its_start_only_within_a_hunk() {
        let file = &index_patch(PATCH)[0];
        let ranged = review_anchor(file, (Side::Right, 10), (Side::Right, 12), true);
        assert_eq!(
            ranged,
            Some((Position::Added { new: 12 }, Some(Position::Context { old: 10, new: 10, side: Side::Right })))
        );
        assert_eq!(
            review_anchor(file, (Side::Right, 12), (Side::Right, 10), true),
            Some((Position::Added { new: 12 }, Some(Position::Context { old: 10, new: 10, side: Side::Right }))),
            "a selection made upwards is put in diff order"
        );
        assert_eq!(
            review_anchor(file, (Side::Right, 10), (Side::Right, 12), false),
            Some((Position::Added { new: 12 }, None))
        );
        assert_eq!(
            review_anchor(file, (Side::Right, 10), (Side::Right, 42), true),
            Some((Position::Added { new: 42 }, None)),
            "a range across hunks keeps only its last line"
        );
        assert_eq!(Position::Added { new: 12 }.to_json(), json!({"kind": "added", "newLine": 12}));
        assert_eq!(Position::Context { old: 3, new: 4, side: Side::Left }.anchor(), (Side::Left, 3));
    }

    #[test]
    fn an_excerpt_keeps_the_lines_its_change_replaced() {
        let rows = file_rows(&index_patch(PATCH)[0]);
        let shape = |rows: &[Row]| rows.iter().map(|r| format!("{}{}", r.mark, r.text)).collect::<Vec<_>>();
        assert_eq!(
            shape(&excerpt_rows(&rows, true, 11, 12)),
            ["-old", "+new", "+more"],
            "the removed line it replaced comes too"
        );
        assert_eq!(shape(&excerpt_rows(&rows, false, 11, 11)), ["-old", "+new", "+more"]);
        assert_eq!(shape(&excerpt_rows(&rows, true, 41, 42)), [" x", "-y", "+y2"], "a removed line between them stays");
        assert!(excerpt_rows(&rows, true, 90, 99).is_empty());
    }

    #[test]
    fn hiding_whitespace_turns_respaced_lines_into_context() {
        let patch = "@@ -1,3 +1,3 @@\n-a  b\n-c\n+ab\n+d\n e\n";
        assert_eq!(hide_whitespace(patch), "@@ -1,3 +1,3 @@\n ab\n-c\n+d\n e\n");
        let unchanged = "@@ -1 +1 @@\n-x\n+y\n";
        assert_eq!(hide_whitespace(unchanged), unchanged);
    }

    #[test]
    fn tests_and_their_support_files_are_told_apart_from_code() {
        for path in [
            "src/a.test.ts",
            "pkg/foo_test.go",
            "tests/x.rs",
            "app/FooTest.java",
            "src/__mocks__/a.ts",
            "x.snap",
            "fixtures/a.json",
            "test_api.py",
            "conftest.py",
        ] {
            assert!(is_test_file(path), "{path}");
        }
        for path in
            ["src/latest.ts", "app/Latest.java", "src/contest.ts", "pnpm-lock.yaml", "generated/test.ts", "README.md"]
        {
            assert!(!is_test_file(path), "{path}");
        }
    }

    #[test]
    fn files_order_by_tier_then_keep_directories_together() {
        let paths: Vec<String> =
            ["src/b.test.ts", "pnpm-lock.yaml", "src/z.ts", "docs/a.md", "src/a/x.ts"].map(String::from).to_vec();
        let ordered: Vec<&str> = order_files(&paths).into_iter().map(|i| paths[i].as_str()).collect();
        assert_eq!(ordered, ["docs/a.md", "src/a/x.ts", "src/z.ts", "src/b.test.ts", "pnpm-lock.yaml"]);
    }

    #[test]
    fn a_viewed_range_runs_between_the_two_clicks() {
        let paths: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        assert_eq!(viewed_range(&paths, "c", Some("a")), ["a", "b", "c"]);
        assert_eq!(viewed_range(&paths, "b", None), ["b"]);
        assert!(viewed_range(&paths, "x", None).is_empty());
    }
}
