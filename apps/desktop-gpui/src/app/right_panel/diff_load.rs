//! The git diff's two-step read (`useReviewFilePatches`): the preview carries every file's stats,
//! and a large or `metadataOnly` comparison sends its patches one file per request.

use crate::turn_diff_tree::natural_cmp;

use gpui_kit::component::diff::{DiffFile, DiffParseOptions};
use serde_json::{Value, json};

/// `OUTPUT_TRUNCATED_MARKER`, which the server appends to a patch cut at its byte cap.
const TRUNCATED_MARKER: &str = "\n\n[truncated]";
/// Patch reads in flight at once (`patchReads` in client-runtime's review state).
pub(super) const PATCH_READS: usize = 4;

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

/// A header-only row that names the file, for a patch that did not load.
fn placeholder(file: &FileStat) -> Vec<DiffFile> {
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
