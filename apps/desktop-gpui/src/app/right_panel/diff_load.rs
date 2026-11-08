//! The git diff's two-step read (`useReviewFilePatches`): the preview carries every file's stats,
//! and a large or `metadataOnly` comparison sends its patches one file per request.

use std::collections::VecDeque;

use crate::turn_diff_tree::natural_cmp;

use gpui_kit::component::diff::{DiffFile, DiffParseOptions};
use serde_json::{Value, json};

/// `OUTPUT_TRUNCATED_MARKER`, which the server appends to a patch cut at its byte cap.
const TRUNCATED_MARKER: &str = "\n\n[truncated]";
/// Patch reads in flight at once (`patchReads` in client-runtime's review state).
pub(super) const PATCH_READS: usize = 4;
/// Files read before anything asks (`INITIAL_FILE_COUNT`), and each later batch's size.
const BATCH: usize = 4;
/// How far below the viewport a pending file's header asks for its patch (`rootMargin`).
pub(super) const DEMAND_MARGIN: f32 = 600.;

/// Where one file's patch read is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Read {
    Unasked,
    Queued,
    Reading,
    Done,
}

/// Which files' patches to read, and when (`useReviewFilePatches`' requested paths under
/// client-runtime's `patchReads` limit): the first batch at once, then the files something asks
/// for, each read once, at most [`PATCH_READS`] at a time, in the order asked.
#[derive(Debug)]
pub(super) struct PatchDemand {
    reads: Vec<Read>,
    queue: VecDeque<usize>,
    in_flight: usize,
}

impl PatchDemand {
    pub(super) fn new(len: usize) -> Self {
        let mut demand = Self { reads: vec![Read::Unasked; len], queue: VecDeque::new(), in_flight: 0 };
        demand.request(0..BATCH);
        demand
    }

    /// `requestFiles`: asks for files not asked for yet.
    pub(super) fn request(&mut self, indices: impl IntoIterator<Item = usize>) {
        for index in indices {
            if self.reads.get(index) == Some(&Read::Unasked) {
                self.reads[index] = Read::Queued;
                self.queue.push_back(index);
            }
        }
    }

    /// `loadNextFiles`: the batch after the files that settled in order, unless its first file
    /// was already asked for and is on its way.
    pub(super) fn load_next(&mut self, first_unsettled: usize) {
        if self.reads.get(first_unsettled) == Some(&Read::Unasked) {
            self.request(first_unsettled..first_unsettled + BATCH);
        }
    }

    /// `retry`: reads a settled file again, ahead of the rest.
    pub(super) fn retry(&mut self, index: usize) {
        if self.reads.get(index) == Some(&Read::Done) {
            self.reads[index] = Read::Queued;
            self.queue.push_front(index);
        }
    }

    /// Reads asked for that have not settled: the patch is still loading while any remain.
    pub(super) fn outstanding(&self) -> usize {
        self.reads.iter().filter(|read| matches!(read, Read::Queued | Read::Reading)).count()
    }

    pub(super) fn is_requested(&self, index: usize) -> bool {
        self.reads.get(index).is_some_and(|read| *read != Read::Unasked)
    }

    /// The reads to start now.
    pub(super) fn start(&mut self) -> Vec<usize> {
        let mut started = Vec::new();
        while self.in_flight < PATCH_READS {
            let Some(index) = self.queue.pop_front() else { break };
            self.reads[index] = Read::Reading;
            self.in_flight += 1;
            started.push(index);
        }
        started
    }

    pub(super) fn settle(&mut self, index: usize) {
        if self.reads.get(index) == Some(&Read::Reading) {
            self.reads[index] = Read::Done;
            self.in_flight -= 1;
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct FileStat {
    pub(super) path: String,
    pub(super) previous_path: Option<String>,
    pub(super) additions: usize,
    pub(super) deletions: usize,
}

/// Parses a patch; one cut at the size limit keeps the files and lines it holds.
pub(super) fn parse_patch(patch: &str, truncated: bool) -> Vec<DiffFile> {
    let (patch, options) = if truncated {
        (patch.strip_suffix(TRUNCATED_MARKER).unwrap_or(patch), DiffParseOptions::new().truncated_hunks(true))
    } else {
        (patch, DiffParseOptions::new())
    };
    DiffFile::parse_with(patch, options).unwrap_or_else(|error| {
        log::warn!("unparseable diff: {error}");
        Vec::new()
    })
}

/// A source's file stats, when it has them.
pub(super) fn file_stats(source: &Value) -> Option<Vec<FileStat>> {
    let files = source["files"].as_array()?;
    let count = |value: &Value| value.as_u64().and_then(|n| usize::try_from(n).ok()).unwrap_or(0);
    Some(
        files
            .iter()
            .filter_map(|file| {
                Some(FileStat {
                    path: file["path"].as_str()?.to_owned(),
                    previous_path: file["previousPath"].as_str().map(str::to_owned),
                    additions: count(&file["additions"]),
                    deletions: count(&file["deletions"]),
                })
            })
            .collect(),
    )
}

/// The files whose patches load one by one (`lazySource`): a truncated or deferred source with
/// stats, sorted as the web's `filePathCollator` lists them.
pub(super) fn deferred_files(source: &Value) -> Vec<FileStat> {
    let lazy = source["truncated"].as_bool() == Some(true) || source["patchesDeferred"].as_bool() == Some(true);
    let mut files = if lazy { file_stats(source).unwrap_or_default() } else { Vec::new() };
    files.sort_by(|a, b| natural_cmp(&a.path, &b.path));
    files
}

/// What reads one file's patch of the preview's source (`reviewFileQueries`).
#[derive(Clone, Debug)]
pub(super) struct PatchRequest {
    pub(super) cwd: String,
    pub(super) base_ref: Option<String>,
    pub(super) commit_sha: Option<String>,
    pub(super) source_kind: &'static str,
    pub(super) ignore_whitespace: bool,
}

impl PatchRequest {
    /// The `review.getDiffPreview` input for `file`.
    pub(super) fn input(&self, file: &FileStat) -> Value {
        let mut input = json!({
            "cwd": self.cwd,
            "ignoreWhitespace": self.ignore_whitespace,
            "file": {"path": file.path, "previousPath": file.previous_path, "sourceKind": self.source_kind},
        });
        if let Some(base) = &self.base_ref {
            input["baseRef"] = base.as_str().into();
        }
        if let Some(sha) = &self.commit_sha {
            input["commitSha"] = sha.as_str().into();
        }
        input
    }
}

/// How one file's patch read settled (`fileStates`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileState {
    Loaded,
    /// The server cut the patch at its size limit; the counts still cover every change.
    Truncated,
    /// The read failed or left the file out; its row stays, with a retry.
    Failed,
}

/// One file's reply (`diffFilePatch`, `None` when the request failed) as the rows it shows:
/// the file from the requested source, or a placeholder row when it did not load.
pub(super) fn settle_file(file: &FileStat, reply: Option<&Value>, source_kind: &str) -> (Vec<DiffFile>, FileState) {
    let source =
        reply.and_then(|value| value["sources"].as_array()?.iter().find(|s| s["kind"].as_str() == Some(source_kind)));
    let Some(source) = source else { return (placeholder(file), FileState::Failed) };
    let truncated = source["truncated"].as_bool() == Some(true);
    let files: Vec<DiffFile> = parse_patch(source["diff"].as_str().unwrap_or_default(), truncated)
        .into_iter()
        .filter(|parsed| parsed.path().as_ref() == file.path)
        .collect();
    match (files.is_empty(), truncated) {
        (true, _) => (placeholder(file), FileState::Failed),
        (false, true) => (files, FileState::Truncated),
        (false, false) => (files, FileState::Loaded),
    }
}

/// The settled rows a refresh keeps: each file listed again with the same counts keeps the
/// patch it showed until its new read settles.
pub(super) fn carried_rows<R: Clone>(previous: &[FileStat], rows: &[Option<R>], files: &[FileStat]) -> Vec<Option<R>> {
    let by_path: std::collections::HashMap<&str, usize> =
        previous.iter().enumerate().map(|(index, file)| (file.path.as_str(), index)).collect();
    files
        .iter()
        .map(|file| {
            let index = *by_path.get(file.path.as_str())?;
            if previous[index] == *file { rows.get(index).cloned().flatten() } else { None }
        })
        .collect()
}

/// A header-only row that names the file, for a patch that did not load or is still on its way.
pub(super) fn placeholder(file: &FileStat) -> Vec<DiffFile> {
    let previous = file.previous_path.as_deref().unwrap_or(&file.path);
    parse_patch(
        &format!(
            "--- a/{previous}
+++ b/{}
",
            file.path
        ),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deferred_sources_list_every_file_from_their_stats_in_path_order() {
        let source = json!({"kind": "branch-range", "diff": "", "truncated": false, "patchesDeferred": true, "files": [
            {"path": "src/b10.ts", "previousPath": null, "additions": 3, "deletions": 0},
            {"path": "src/B2.ts", "previousPath": "src/old.ts", "additions": 1, "deletions": 9},
            {"path": "README.md", "previousPath": null, "additions": 0, "deletions": 1},
        ]});
        let paths: Vec<_> = deferred_files(&source).into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["README.md", "src/B2.ts", "src/b10.ts"]);
        // A complete patch renders as it is, and a truncated one without stats parses what it holds.
        assert!(deferred_files(&json!({"diff": "x", "truncated": false, "files": source["files"]})).is_empty());
        assert!(deferred_files(&json!({"diff": "x", "truncated": true})).is_empty());
    }

    #[test]
    fn patches_load_the_first_batch_then_what_is_asked_for_a_few_at_a_time() {
        let mut demand = PatchDemand::new(10);
        assert_eq!(demand.start(), [0, 1, 2, 3]);
        assert!(!demand.is_requested(4));
        // A header scrolled near asks for its file; it waits for a free read.
        demand.request([7, 7, 2]);
        assert!(demand.is_requested(7));
        assert_eq!(demand.start(), Vec::<usize>::new());
        assert_eq!(demand.outstanding(), 5);
        demand.settle(1);
        assert_eq!(demand.start(), [7]);
        // The next batch starts after the files that settled in order; while its first file is
        // on its way, asking again adds nothing.
        demand.load_next(4);
        demand.load_next(4);
        for index in [0, 2, 3, 7] {
            demand.settle(index);
        }
        assert_eq!(demand.start(), [4, 5, 6]);
        assert!(!demand.is_requested(8));
        assert_eq!(demand.outstanding(), 3, "files nobody asked for are not loading");
        // A retry jumps the queue; past the end nothing is asked for.
        demand.request([8, 9, 12]);
        demand.settle(4);
        demand.retry(4);
        demand.retry(9);
        demand.settle(5);
        assert_eq!(demand.start(), [4, 8, 9]);
        demand.load_next(12);
        assert!(!demand.is_requested(12));
    }

    #[test]
    fn a_refresh_keeps_the_patches_of_files_listed_again_unchanged() {
        let file =
            |path: &str, additions: usize| FileStat { path: path.into(), previous_path: None, additions, deletions: 0 };
        let previous = [file("a", 1), file("b", 2), file("c", 3)];
        let rows = [Some("a"), Some("b"), None];
        let files = [file("0", 1), file("a", 1), file("b", 5), file("c", 3)];
        assert_eq!(carried_rows(&previous, &rows, &files), [None, Some("a"), None, None]);
    }

    #[test]
    fn file_requests_name_the_file_and_its_source() {
        let file = FileStat { path: "b.ts".into(), previous_path: Some("a.ts".into()), additions: 1, deletions: 1 };
        let mut request = PatchRequest {
            cwd: "/repo".into(),
            base_ref: Some("origin/main".into()),
            commit_sha: None,
            source_kind: "branch-range",
            ignore_whitespace: true,
        };
        assert_eq!(
            request.input(&file),
            json!({"cwd": "/repo", "baseRef": "origin/main", "ignoreWhitespace": true,
                "file": {"path": "b.ts", "previousPath": "a.ts", "sourceKind": "branch-range"}}),
        );
        (request.base_ref, request.commit_sha, request.source_kind) = (None, Some("abc".into()), "commit");
        let commit = request.input(&file);
        assert_eq!(commit["commitSha"], "abc");
        assert!(commit.get("baseRef").is_none());
    }

    #[test]
    fn every_requested_file_keeps_a_row_with_how_its_read_went() {
        let patch = "diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
";
        let file = |path: &str| FileStat { path: path.into(), previous_path: None, additions: 1, deletions: 1 };
        let reply = |truncated: bool| {
            json!({"sources": [
                {"kind": "working-tree", "diff": "", "truncated": false},
                {"kind": "branch-range", "diff": patch, "truncated": truncated},
            ]})
        };
        let settled = |file: &FileStat, reply: Option<&Value>, kind: &str| {
            let (files, state) = settle_file(file, reply, kind);
            (
                files.iter().map(|f| f.path().to_string()).collect::<Vec<_>>(),
                files.iter().any(DiffFile::has_changes),
                state,
            )
        };
        let a = file("a.txt");
        assert_eq!(settled(&a, Some(&reply(false)), "branch-range"), (vec!["a.txt".into()], true, FileState::Loaded));
        assert_eq!(settled(&a, Some(&reply(true)), "branch-range").2, FileState::Truncated);
        // A failed request, a missing source, or a reply without the file each keep a bare row to retry.
        assert_eq!(settled(&a, None, "branch-range"), (vec!["a.txt".into()], false, FileState::Failed));
        assert_eq!(settled(&a, Some(&reply(false)), "working-tree").2, FileState::Failed);
        assert_eq!(settled(&file("dir/b c.txt"), Some(&reply(false)), "branch-range").0, ["dir/b c.txt"]);
        let renamed = FileStat { previous_path: Some("old.txt".into()), ..file("new.txt") };
        let (rows, _) = settle_file(&renamed, None, "branch-range");
        assert_eq!(
            rows.iter().map(|f| (f.original_path().cloned(), f.path().clone())).collect::<Vec<_>>(),
            [(Some("old.txt".into()), "new.txt".into())]
        );
    }
}
