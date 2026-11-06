//! The git diff's two-step read (`useReviewFilePatches`): the preview carries every file's stats,
//! and a large or `metadataOnly` comparison sends its patches one file per request.

use std::cmp::Ordering;

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

/// `reviewFileQueries`' request for one file of the preview's source.
pub(super) fn file_patch_request(
    cwd: &str,
    base_ref: Option<&str>,
    commit_sha: Option<&str>,
    source_kind: &str,
    ignore_whitespace: bool,
    file: &FileStat,
) -> Value {
    let mut input = json!({
        "cwd": cwd,
        "ignoreWhitespace": ignore_whitespace,
        "file": {"path": file.path, "previousPath": file.previous_path, "sourceKind": source_kind},
    });
    if let Some(base) = base_ref {
        input["baseRef"] = base.into();
    }
    if let Some(sha) = commit_sha {
        input["commitSha"] = sha.into();
    }
    input
}

/// The files of a one-file reply (`diffFilePatch`), from its source of the requested kind.
pub(super) fn file_patch(value: &Value, source_kind: &str) -> Vec<DiffFile> {
    let source = value["sources"].as_array().and_then(|s| s.iter().find(|s| s["kind"].as_str() == Some(source_kind)));
    source.map_or_else(Vec::new, |source| {
        parse_patch(source["diff"].as_str().unwrap_or_default(), source["truncated"].as_bool() == Some(true))
    })
}

/// `Intl.Collator(undefined, {numeric: true, sensitivity: "base"})`, for paths: case-insensitive,
/// with digit runs compared by value.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    fn digits(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
        let mut run = String::new();
        while let Some(c) = chars.next_if(char::is_ascii_digit) {
            run.push(c);
        }
        run.trim_start_matches('0').to_owned()
    }
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (x, y) = (digits(&mut a), digits(&mut b));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(&y));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
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
        assert_eq!(
            file_patch_request("/repo", Some("origin/main"), None, "branch-range", true, &file),
            json!({"cwd": "/repo", "baseRef": "origin/main", "ignoreWhitespace": true,
                "file": {"path": "b.ts", "previousPath": "a.ts", "sourceKind": "branch-range"}}),
        );
        let commit = file_patch_request("/repo", None, Some("abc"), "commit", false, &file);
        assert_eq!(commit["commitSha"], "abc");
        assert!(commit.get("baseRef").is_none());
    }

    #[test]
    fn one_file_replies_read_their_requested_source() {
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n";
        let reply = json!({"sources": [
            {"kind": "working-tree", "diff": "", "truncated": false},
            {"kind": "branch-range", "diff": patch, "truncated": false},
        ]});
        assert_eq!(file_patch(&reply, "branch-range").len(), 1);
        assert!(file_patch(&reply, "working-tree").is_empty());
    }
}
