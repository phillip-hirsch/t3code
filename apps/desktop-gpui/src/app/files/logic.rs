//! Pure logic behind the file surfaces: picker matching (`ProjectFilePicker.logic.ts`),
//! content-search grouping (`ProjectContentSearchDialog.tsx`, `HighlightedSearchLine.tsx`),
//! path helpers (`filePreviewMode.ts`, `rightPanelStore.ts` `openFile`) and the file tree's rows.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

/// `PROJECT_FILE_PICKER_RESULT_LIMIT`.
pub(super) const PICKER_LIMIT: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EntryKind {
    File,
    Directory,
}

/// A `ProjectEntry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub path: String,
    pub kind: EntryKind,
    pub ignored: bool,
}

impl Entry {
    pub(super) fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            path: value["path"].as_str()?.to_owned(),
            kind: if value["kind"] == "directory" { EntryKind::Directory } else { EntryKind::File },
            ignored: value["ignored"].as_bool().unwrap_or(false),
        })
    }
}

pub(super) fn entries_from_json(value: &Value) -> Vec<Entry> {
    value["entries"].as_array().into_iter().flatten().filter_map(Entry::from_json).collect()
}

/// The message of a typed failure, as the web shows `error.message`.
pub(super) fn error_message(error: &crate::rpc::RpcError) -> String {
    match error {
        crate::rpc::RpcError::Failure(value) => {
            value["message"].as_str().map(str::to_owned).unwrap_or_else(|| error.to_string())
        }
        other => other.to_string(),
    }
}

pub(super) fn file_name(path: &str) -> &str {
    &path[path.rfind(['/', '\\']).map_or(0, |at| at + 1)..]
}

/// The file name and the folder it sits in (`splitPath`).
pub(super) fn split_path(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(at) => (&path[at + 1..], &path[..at]),
        None => (path, ""),
    }
}

/// `normalizeSearchQuery(raw, { trimLeadingPattern: /^[@./]+/ })` without whitespace.
fn picker_query(raw: &str) -> Vec<char> {
    raw.trim().trim_start_matches(['@', '.', '/']).to_lowercase().chars().filter(|c| !c.is_whitespace()).collect()
}

/// Char indices of `query` as a subsequence of `value`, or `None` when it is not one.
fn match_indices(value: &str, query: &[char]) -> Option<Vec<usize>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let mut indices = Vec::new();
    for (index, c) in value.chars().enumerate() {
        if c.to_lowercase().eq(std::iter::once(query[indices.len()])) {
            indices.push(index);
            if indices.len() == query.len() {
                return Some(indices);
            }
        }
    }
    None
}

/// One Go to file row (`ProjectFilePickerMatch`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PickerMatch {
    pub path: String,
    pub name_indices: Vec<usize>,
    pub path_indices: Vec<usize>,
}

/// `getProjectFilePickerMatches`: files in server rank order, with highlights where the query
/// is a subsequence (server-matched entries still show, unhighlighted).
pub(super) fn picker_matches(entries: &[Entry], raw_query: &str) -> Vec<PickerMatch> {
    let query = picker_query(raw_query);
    entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .take(PICKER_LIMIT)
        .map(|entry| PickerMatch {
            name_indices: match_indices(file_name(&entry.path), &query).unwrap_or_default(),
            path_indices: match_indices(&entry.path, &query).unwrap_or_default(),
            path: entry.path.clone(),
        })
        .collect()
}

/// Splits `value` into runs, bold where `indices` (char positions) fall.
pub(super) fn highlight_runs(value: &str, indices: &[usize]) -> Vec<(String, bool)> {
    let mut runs: Vec<(String, bool)> = Vec::new();
    for (index, c) in value.chars().enumerate() {
        let bold = indices.contains(&index);
        match runs.last_mut() {
            Some((text, last)) if *last == bold => text.push(c),
            _ => runs.push((c.to_string(), bold)),
        }
    }
    runs
}

/// One `projects.searchContents` hit.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ContentMatch {
    pub path: String,
    pub line: u32,
    pub text: String,
    /// UTF-16 offsets, as the server sends them.
    pub ranges: Vec<(usize, usize)>,
}

impl ContentMatch {
    pub(super) fn from_json(value: &Value) -> Option<Self> {
        Some(Self {
            path: value["path"].as_str()?.to_owned(),
            line: u32::try_from(value["lineNumber"].as_u64()?).ok()?,
            text: value["lineContent"].as_str().unwrap_or_default().to_owned(),
            ranges: value["matchRanges"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| Some((r["start"].as_u64()? as usize, r["end"].as_u64()? as usize)))
                .collect(),
        })
    }
}

/// `groupMatches`: indices into `matches` per path, in first-seen order.
pub(super) fn group_matches(matches: &[ContentMatch]) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    let mut by_path: HashMap<&str, usize> = HashMap::new();
    for (index, m) in matches.iter().enumerate() {
        match by_path.get(m.path.as_str()) {
            Some(&group) => groups[group].1.push(index),
            None => {
                by_path.insert(&m.path, groups.len());
                groups.push((m.path.clone(), vec![index]));
            }
        }
    }
    groups
}

/// `normalizeRanges` plus the split into plain and matched runs. Ranges are UTF-16 offsets,
/// clamped to the line, sorted and merged.
pub(super) fn match_runs(line: &str, ranges: &[(usize, usize)]) -> Vec<(String, bool)> {
    let units = line.encode_utf16().count();
    let mut sorted: Vec<(usize, usize)> =
        ranges.iter().map(|&(s, e)| (s.min(units), e.min(units))).filter(|(s, e)| e > s).collect();
    sorted.sort();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in sorted {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let mut runs: Vec<(String, bool)> = Vec::new();
    let mut offset = 0;
    for c in line.chars() {
        let matched = merged.iter().any(|&(s, e)| offset >= s && offset < e);
        offset += c.len_utf16();
        match runs.last_mut() {
            Some((text, last)) if *last == matched => text.push(c),
            _ => runs.push((c.to_string(), matched)),
        }
    }
    runs
}

/// `toLocaleString()` for counts: thousands separated by commas.
pub(super) fn grouped(count: usize) -> String {
    let digits = count.to_string();
    let mut out = String::new();
    for (index, c) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The content search status line.
pub(super) fn results_summary(matches: &[ContentMatch], truncated: bool) -> String {
    let files: HashSet<&str> = matches.iter().map(|m| m.path.as_str()).collect();
    format!("{}{} results in {} files", grouped(matches.len()), if truncated { "+" } else { "" }, grouped(files.len()))
}

/// `openFile`'s path normalization: workspace paths use `/`; trailing slashes go, except on a drive root.
pub(super) fn normalize_open_path(requested: &str) -> String {
    let bytes = requested.as_bytes();
    let drive_root = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && requested[2..].chars().all(|c| c == '/');
    if drive_root {
        return requested.to_owned();
    }
    let trimmed = requested.trim_end_matches('/');
    if trimmed.is_empty() { requested.to_owned() } else { trimmed.to_owned() }
}

/// `isMarkdownPreviewFile`.
pub(super) fn is_markdown(path: &str) -> bool {
    let lower = path.to_lowercase();
    [".md", ".mdx", ".markdown", ".mdown", ".mkd"].iter().any(|ext| lower.ends_with(ext))
}

/// Whether a read cut at 1 MB loads the whole file instead (`WorkspaceLargeTextPreview`): not for
/// drafts, Markdown or HTML, which keep the first 1 MB with a notice.
pub(super) fn loads_full_file(path: &str, draft: bool) -> bool {
    let lower = path.to_lowercase();
    !draft && !is_markdown(path) && !lower.ends_with(".html") && !lower.ends_with(".htm")
}

/// `isWorkspaceImagePreviewPath`; the formats GPUI decodes render, the rest fall back to "binary".
pub(super) fn is_image(path: &str) -> bool {
    let lower = path.to_lowercase();
    [".avif", ".bmp", ".gif", ".ico", ".heic", ".heif", ".jpeg", ".jpg", ".png", ".svg", ".tif", ".tiff", ".webp"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// The extension after the last dot of the file name, lowercased (`""` without one).
fn extension(path: &str) -> String {
    file_name(path).rsplit_once('.').map(|(_, ext)| ext.to_lowercase()).unwrap_or_default()
}

/// `officePreviewKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Office {
    Docx,
    Xlsx,
    Pptx,
    Odt,
    Ods,
    Odp,
    Rtf,
    Epub,
    Ipynb,
}

/// How the panel shows a workspace file (`FilePreviewPanel`'s `isVideo`, `isAudio`, `isImage`,
/// `isPdf`, `isDocument` and `isHtml`, in that order of precedence).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileKind {
    Video,
    Audio,
    Image,
    Pdf,
    /// `binaryPreviewKind`: zip, jar, tar and gzip archives.
    Archive,
    Sqlite,
    Office(Office),
    Font,
    Html,
    /// Read with `projects.readFile`: source, Markdown, Mermaid and tables.
    Text,
}

impl FileKind {
    /// Read through an asset URL, never as text (`isMedia || isPdf || isDocument`).
    pub(super) fn is_asset(self) -> bool {
        !matches!(self, Self::Html | Self::Text)
    }
}

pub(super) fn file_kind(path: &str) -> FileKind {
    let ext = extension(path);
    match ext.as_str() {
        "avi" | "m4v" | "mkv" | "mov" | "mp4" | "ogv" | "webm" => FileKind::Video,
        "mp3" | "wav" | "ogg" | "oga" | "flac" | "aac" | "m4a" | "opus" | "aiff" | "webm-audio" | "weba" => {
            FileKind::Audio
        }
        _ if is_image(path) => FileKind::Image,
        "pdf" => FileKind::Pdf,
        "zip" | "jar" | "tar" | "gz" | "tgz" => FileKind::Archive,
        "sqlite" | "sqlite3" | "db" => FileKind::Sqlite,
        "docx" => FileKind::Office(Office::Docx),
        "xlsx" => FileKind::Office(Office::Xlsx),
        "pptx" => FileKind::Office(Office::Pptx),
        "odt" => FileKind::Office(Office::Odt),
        "ods" => FileKind::Office(Office::Ods),
        "odp" => FileKind::Office(Office::Odp),
        "rtf" => FileKind::Office(Office::Rtf),
        "epub" => FileKind::Office(Office::Epub),
        "ipynb" => FileKind::Office(Office::Ipynb),
        "ttf" | "otf" | "woff" | "woff2" => FileKind::Font,
        "htm" | "html" => FileKind::Html,
        _ => FileKind::Text,
    }
}

/// `filePreviewDelimiter` by name.
pub(super) fn table_delimiter(path: &str) -> Option<char> {
    match extension(path).as_str() {
        "csv" => Some(','),
        "tsv" => Some('\t'),
        _ => None,
    }
}

pub(super) fn is_mermaid(path: &str) -> bool {
    matches!(extension(path).as_str(), "mmd" | "mermaid")
}

/// The highlighter language for a file, by extension or well-known name.
pub(super) fn language_for_path(path: &str) -> &'static str {
    let name = file_name(path).to_lowercase();
    if matches!(name.as_str(), "dockerfile" | "makefile") {
        return "text";
    }
    if name.starts_with(".env") || matches!(name.as_str(), ".bashrc" | ".zshrc" | ".profile") {
        return "bash";
    }
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match extension {
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" | "jsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "json" | "jsonc" | "json5" => "json",
        "py" | "pyi" => "python",
        "go" => "go",
        "sh" | "bash" | "zsh" => "bash",
        "css" | "scss" => "css",
        "html" | "htm" => "html",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "mdx" | "markdown" | "mdown" | "mkd" => "markdown",
        "diff" | "patch" => "diff",
        _ => "text",
    }
}

/// One crumb: its label and the folder (or file) path it names; `""` is the workspace root.
pub(super) fn breadcrumbs(path: &str, project_name: &str) -> Vec<(String, String)> {
    let mut crumbs = vec![(project_name.to_owned(), String::new())];
    let mut current = String::new();
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(segment);
        crumbs.push((segment.to_owned(), current.clone()));
    }
    crumbs
}

/// One visible row of the file tree.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TreeRow {
    pub path: String,
    /// The first folder of a flattened chain (else `path`).
    pub top: String,
    /// The label: the name, or `a/b/c` for a chain of folders that each hold one folder.
    pub label: String,
    pub depth: usize,
    pub kind: EntryKind,
    pub expanded: bool,
    pub ignored: bool,
}

/// The folder a workspace path sits in (`""` for the root).
pub(super) fn parent_of(path: &str) -> &str {
    path.rfind('/').map_or("", |at| &path[..at])
}

/// Folders before files, then by name, case-insensitively (the tree's sort).
pub(super) fn sort_entries(entries: &mut [&Entry]) {
    entries.sort_by(|a, b| {
        (a.kind == EntryKind::File)
            .cmp(&(b.kind == EntryKind::File))
            .then_with(|| file_name(&a.path).to_lowercase().cmp(&file_name(&b.path).to_lowercase()))
            .then_with(|| a.path.cmp(&b.path))
    });
}

/// The tree's rows for these entries (folders and files anywhere in the workspace), with
/// `expanded` folders open. `flattenEmptyDirectories`: a folder whose only child is a folder
/// shows as one `a/b` row. With `filter`, only those paths and their ancestors show, all open.
pub(super) fn tree_rows(
    entries: &[Entry],
    expanded: &HashSet<String>,
    filter: Option<&HashSet<String>>,
) -> Vec<TreeRow> {
    let mut children: HashMap<&str, Vec<&Entry>> = HashMap::new();
    for entry in entries {
        children.entry(parent_of(&entry.path)).or_default().push(entry);
    }
    for list in children.values_mut() {
        sort_entries(list);
    }
    let mut rows = Vec::new();
    walk(&children, "", 0, expanded, filter, &mut rows);
    rows
}

fn walk(
    children: &HashMap<&str, Vec<&Entry>>,
    parent: &str,
    depth: usize,
    expanded: &HashSet<String>,
    filter: Option<&HashSet<String>>,
    rows: &mut Vec<TreeRow>,
) {
    let shown = |path: &str| filter.is_none_or(|f| f.contains(path));
    for &entry in children.get(parent).into_iter().flatten() {
        if !shown(&entry.path) {
            continue;
        }
        let mut node = entry;
        let mut label = file_name(&entry.path).to_owned();
        let mut open = expanded.contains(&node.path);
        while node.kind == EntryKind::Directory {
            let Some([only]) = children.get(node.path.as_str()).map(Vec::as_slice) else { break };
            if only.kind != EntryKind::Directory || !shown(&only.path) {
                break;
            }
            label.push('/');
            label.push_str(file_name(&only.path));
            node = only;
            open |= expanded.contains(&node.path);
        }
        let open = node.kind == EntryKind::Directory && (filter.is_some() || open);
        rows.push(TreeRow {
            path: node.path.clone(),
            top: entry.path.clone(),
            label,
            depth,
            kind: node.kind,
            expanded: open,
            ignored: node.ignored,
        });
        if open {
            walk(children, &node.path, depth + 1, expanded, filter, rows);
        }
    }
}

/// The row holding the folder above `rows[at]`: the nearest earlier row one level up.
pub(super) fn parent_row(rows: &[TreeRow], at: usize) -> Option<usize> {
    let depth = rows[at].depth.checked_sub(1)?;
    rows[..at].iter().rposition(|row| row.depth == depth)
}

/// The paths a tree search shows: each match and every folder above it.
pub(super) fn search_paths(matches: &[Entry]) -> HashSet<String> {
    let mut paths = HashSet::new();
    for entry in matches {
        let mut path = entry.path.as_str();
        paths.insert(path.to_owned());
        while !path.is_empty() {
            path = parent_of(path);
            if !path.is_empty() {
                paths.insert(path.to_owned());
            }
        }
    }
    paths
}

/// The tree's own search (Pierre `hide-non-matches`): every known path that contains the
/// typed text, ignoring case.
pub(super) fn tree_search_matches(entries: &[Entry], typed: &str) -> Vec<Entry> {
    let needle = typed.to_lowercase();
    entries.iter().filter(|e| e.path.to_lowercase().contains(&needle)).cloned().collect()
}

/// `buildTreeEntries`: listed entries plus search hits and the folders above them.
pub(super) fn merge_search_entries(listed: &[Entry], hits: &[Entry]) -> Vec<Entry> {
    let mut known: HashSet<String> = listed.iter().map(|e| e.path.clone()).collect();
    let mut merged = listed.to_vec();
    for hit in hits {
        if known.insert(hit.path.clone()) {
            merged.push(hit.clone());
        }
        let mut path = parent_of(&hit.path);
        while !path.is_empty() {
            if known.insert(path.to_owned()) {
                merged.push(Entry { path: path.to_owned(), kind: EntryKind::Directory, ignored: false });
            }
            path = parent_of(path);
        }
    }
    merged
}

/// `isHostPath`: an absolute path rather than one in the workspace.
pub(super) fn is_host_path(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('\\') || path.as_bytes().get(1) == Some(&b':')
}

/// `resolvePathLinkTarget` for a workspace path: joined onto `cwd` with its separator.
pub(super) fn absolute_path(cwd: &str, path: &str) -> String {
    if is_host_path(path) {
        return path.to_owned();
    }
    let windows = cwd.as_bytes().get(1) == Some(&b':') || cwd.starts_with("\\\\");
    let separator = if windows { '\\' } else { '/' };
    let relative = if windows { path.replace('/', "\\") } else { path.to_owned() };
    format!("{}{separator}{relative}", cwd.trim_end_matches(['/', '\\']))
}

/// `revealInFileExplorerLabelForKind` / `ForOs`.
pub(super) fn reveal_label(kind: Option<&str>, os: Option<&str>) -> &'static str {
    match (kind, os) {
        (Some("finder"), _) | (None, Some("darwin")) => "Reveal in Finder",
        (Some("file-explorer"), _) | (None, Some("windows")) => "Reveal in File Explorer",
        _ => "Reveal in Files",
    }
}

/// The folders above `path`, outermost first (`loadFileAncestors`).
pub(super) fn ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = parent_of(path);
    while !current.is_empty() {
        out.push(current.to_owned());
        current = parent_of(current);
    }
    out.reverse();
    out
}

/// One ordered write queue per file (`FileSaveCoordinator`): every edit is a new revision, at
/// most one write is in flight, and the newest revision is written after it settles, so a
/// revert made while a write is in flight is still written.
#[derive(Debug, Default)]
pub(super) struct SaveQueue {
    revision: u64,
    confirmed: u64,
    writing: bool,
}

impl SaveQueue {
    /// An edit; true when a debounced write should be scheduled (none in flight).
    pub(super) fn change(&mut self) -> bool {
        self.revision += 1;
        !self.writing
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    /// Edits not yet confirmed on disk, or a write in flight.
    pub(super) fn pending(&self) -> bool {
        self.writing || self.revision != self.confirmed
    }

    /// Starts a write of the newest revision unless one is in flight or nothing is unwritten.
    pub(super) fn start(&mut self) -> Option<u64> {
        if self.writing || self.revision == self.confirmed {
            return None;
        }
        self.writing = true;
        Some(self.revision)
    }

    /// Confirms the write of `revision`; true when newer edits arrived meanwhile and need their own write.
    pub(super) fn written(&mut self, revision: u64) -> bool {
        self.confirmed = revision;
        self.settle(revision)
    }

    /// Settles a failed write of `revision`; its edits stay unsaved, and only newer edits write again.
    pub(super) fn failed(&mut self, revision: u64) -> bool {
        self.settle(revision)
    }

    fn settle(&mut self, revision: u64) -> bool {
        self.writing = false;
        self.revision != revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> Entry {
        Entry { path: path.into(), kind: EntryKind::File, ignored: false }
    }
    fn dir(path: &str) -> Entry {
        Entry { path: path.into(), kind: EntryKind::Directory, ignored: false }
    }

    #[test]
    fn picker_keeps_server_order_and_highlights_subsequences() {
        let entries = [file("src/app/main.rs"), dir("src"), file("README.md")];
        let matches = picker_matches(&entries, " @./MaIn ");
        assert_eq!(matches.len(), 2, "folders are left out");
        assert_eq!(matches[0].path, "src/app/main.rs");
        assert_eq!(matches[0].name_indices, [0, 1, 2, 3]);
        assert_eq!(matches[0].path_indices, [8, 9, 10, 11]);
        assert_eq!(matches[1].name_indices, Vec::<usize>::new(), "unmatched rows show without highlights");
        assert_eq!(picker_matches(&entries, "s m")[0].path_indices, [0, 8], "whitespace is dropped");
        assert_eq!(
            highlight_runs("main", &[0, 2]),
            [("m".into(), true), ("a".into(), false), ("i".into(), true), ("n".into(), false)]
        );
    }

    #[test]
    fn content_matches_group_by_file_in_first_seen_order() {
        let hit = |path: &str, line| ContentMatch { path: path.into(), line, text: String::new(), ranges: vec![] };
        let matches = [hit("a.rs", 1), hit("b.rs", 4), hit("a.rs", 9)];
        assert_eq!(group_matches(&matches), [("a.rs".into(), vec![0, 2]), ("b.rs".into(), vec![1])]);
        assert_eq!(results_summary(&matches, true), "3+ results in 2 files");
        assert_eq!(grouped(1234567), "1,234,567");
        assert_eq!(grouped(999), "999");
    }

    #[test]
    fn match_ranges_clamp_merge_and_count_utf16() {
        assert_eq!(
            match_runs("let foo = foo;", &[(10, 13), (4, 6), (5, 7), (40, 50)]),
            [
                ("let ".into(), false),
                ("foo".into(), true),
                (" = ".into(), false),
                ("foo".into(), true),
                (";".into(), false)
            ]
        );
        assert_eq!(match_runs("😀ab", &[(2, 3)]), [("😀".into(), false), ("a".into(), true), ("b".into(), false)]);
    }

    #[test]
    fn open_paths_lose_trailing_slashes_except_drive_roots() {
        assert_eq!(normalize_open_path("src/"), "src");
        assert_eq!(normalize_open_path("C:/"), "C:/");
        assert_eq!(normalize_open_path("/"), "/");
        assert_eq!(normalize_open_path("a/b.rs"), "a/b.rs");
    }

    #[test]
    fn tree_sorts_folders_first_and_flattens_single_folder_chains() {
        let entries = [
            file("b.txt"),
            dir("src"),
            dir("a"),
            dir("a/b"),
            dir("a/b/c"),
            file("a/b/c/x.rs"),
            file("src/main.rs"),
            file("src/lib.rs"),
        ];
        let expanded: HashSet<String> = ["a/b/c".to_owned()].into();
        let rows = tree_rows(&entries, &expanded, None);
        let labels: Vec<(&str, usize)> = rows.iter().map(|r| (r.label.as_str(), r.depth)).collect();
        assert_eq!(labels, [("a/b/c", 0), ("x.rs", 1), ("src", 0), ("b.txt", 0)]);
        assert_eq!(
            (rows[0].path.as_str(), rows[0].top.as_str()),
            ("a/b/c", "a"),
            "the chain acts as its deepest folder"
        );
        let top_open: HashSet<String> = ["a".to_owned()].into();
        assert!(tree_rows(&entries, &top_open, None)[0].expanded, "opening any folder of a chain opens it");
        let open: HashSet<String> = ["src".to_owned()].into();
        let labels: Vec<String> = tree_rows(&entries, &open, None).into_iter().map(|r| r.label).collect();
        assert_eq!(labels, ["a/b/c", "src", "lib.rs", "main.rs", "b.txt"]);
    }

    #[test]
    fn tree_search_shows_hits_with_their_folders_open() {
        let listed = [dir("src"), file("README.md")];
        let hits = [file("src/app/files.rs")];
        let merged = merge_search_entries(&listed, &hits);
        let matches = tree_search_matches(&merged, "FILES");
        assert_eq!(matches, [file("src/app/files.rs")], "server hits pass through the tree's own filter");
        let filter = search_paths(&matches);
        let rows = tree_rows(&merged, &HashSet::new(), Some(&filter));
        let labels: Vec<(&str, bool)> = rows.iter().map(|r| (r.label.as_str(), r.expanded)).collect();
        assert_eq!(labels, [("src/app", true), ("files.rs", false)]);
        assert!(tree_search_matches(&merged, "rail").is_empty());
        assert_eq!((parent_row(&rows, 1), parent_row(&rows, 0)), (Some(0), None), "left goes to the folder row");
        assert_eq!(ancestors("a/b/c.rs"), ["a", "a/b"]);
    }

    #[test]
    fn file_kinds_follow_the_web_precedence() {
        assert_eq!(file_kind("clip.webm"), FileKind::Video, "webm is video before audio");
        assert_eq!(file_kind("a/song.MP3"), FileKind::Audio);
        assert_eq!(file_kind("logo.svg"), FileKind::Image);
        assert_eq!(file_kind("doc.pdf"), FileKind::Pdf);
        assert_eq!(file_kind("x.tgz"), FileKind::Archive);
        assert_eq!(file_kind("app.db"), FileKind::Sqlite);
        assert_eq!(file_kind("r.docx"), FileKind::Office(Office::Docx));
        assert_eq!(file_kind("n.ipynb"), FileKind::Office(Office::Ipynb));
        assert_eq!(file_kind("Inter.woff2"), FileKind::Font);
        assert_eq!(file_kind("index.HTM"), FileKind::Html);
        assert_eq!(file_kind("main.rs"), FileKind::Text);
        assert!(!FileKind::Html.is_asset() && FileKind::Font.is_asset());
        assert_eq!(table_delimiter("a.TSV"), Some('\t'));
        assert_eq!((table_delimiter("a.csv"), table_delimiter("a.txt")), (Some(','), None));
        assert!(is_mermaid("flow.mmd") && is_host_path(r"C:\x") && !is_host_path("src/x"));
    }

    #[test]
    fn languages_and_crumbs_follow_the_path() {
        assert_eq!(language_for_path("src/App.TSX"), "tsx");
        assert_eq!(language_for_path("Cargo.toml"), "toml");
        assert_eq!(language_for_path(".env.local"), "bash");
        assert_eq!(language_for_path("LICENSE"), "text");
        assert_eq!(
            breadcrumbs("src/app/x.rs", "t3"),
            [
                ("t3".into(), "".into()),
                ("src".into(), "src".into()),
                ("app".into(), "src/app".into()),
                ("x.rs".into(), "src/app/x.rs".into())
            ]
        );
        assert!(is_markdown("docs/README.MD") && is_image("a/logo.Svg") && !is_image("a.rs"));
        assert_eq!(absolute_path("C:\\repo\\", "src/a.rs"), "C:\\repo\\src\\a.rs");
        assert_eq!(absolute_path("/home/me/repo", "src/a.rs"), "/home/me/repo/src/a.rs");
        assert_eq!(absolute_path("/repo", "/etc/hosts"), "/etc/hosts");
        assert_eq!(split_path("a/b/c.rs"), ("c.rs", "a/b"));
    }

    #[test]
    fn save_queue_writes_a_revert_made_during_a_write() {
        let mut queue = SaveQueue::default();
        assert!(queue.change(), "an edit schedules a write");
        let first = queue.start().unwrap();
        assert!(!queue.change(), "the revert waits for the write in flight");
        assert_eq!(queue.start(), None, "writes are serialized");
        assert!(queue.written(first), "the revert still needs its own write");
        let revert = queue.start().unwrap();
        assert!(!queue.written(revert));
        assert!(!queue.pending());

        queue.change();
        let failed = queue.start().unwrap();
        assert!(!queue.failed(failed), "a failed write with no newer edits is not retried");
        assert!(queue.pending(), "but its edits stay unsaved");
    }
}
