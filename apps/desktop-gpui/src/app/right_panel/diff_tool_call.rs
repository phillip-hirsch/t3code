//! A tool call's recorded file change as the Diff panel shows it (`toolCallDiff.ts` and the
//! tool-call arms of `DiffPanel`): a provider may record a complete patch, bare hunks, an added
//! or deleted file's contents, or only the contents before and after.

use gpui_kit::component::diff::{DiffFile, DiffParseError, DiffParseOptions, DiffSide};
use serde_json::Value;

use crate::pull_request_diff::hide_whitespace;

/// `recordedText`'s suffix: the server cut the value to fit the transport.
const TRANSPORT_CUT: &str = "\n… output truncated for transport";
const UNSUPPORTED: &str = "Unsupported diff format. Showing raw patch.";
/// The parser's error for text with no file in it.
const NOT_A_DIFF: &str = "expected a unified or Git diff";

/// What a tool call's diff shows.
pub(super) enum Body {
    /// Not a file change, or nothing recorded.
    NoPatch,
    /// A recorded patch with nothing in it.
    NoNetChanges,
    /// The patch's files, and `(recorded, omitted)` when the patch leaves recorded files out.
    Files { files: Vec<DiffFile>, omitted: Option<(usize, usize)> },
    /// A patch the viewer can't read, shown as text with why.
    Raw { reason: &'static str, text: String },
    /// File contents recorded without a diff, each under its label.
    Recorded(Vec<(&'static str, String)>),
}

pub(super) struct ToolCallView {
    pub(super) body: Body,
    /// The transport cut the diff or the contents short.
    pub(super) truncated: bool,
}

/// `orchestration.getTurnItem`'s reply as the Diff panel shows it, hiding whitespace-only
/// changes when asked.
pub(super) fn view(reply: &Value, ignore_whitespace: bool) -> ToolCallView {
    let item = &reply["item"];
    if text(item, "type") != Some("file_change") {
        return ToolCallView { body: Body::NoPatch, truncated: false };
    }
    let diff = tool_call_diff(item);
    let body = match diff.patch {
        Some(patch) if !js_trim(&patch).is_empty() => {
            let shown = if ignore_whitespace { hide_whitespace(&patch) } else { patch.clone() };
            match parse(&shown) {
                Ok(files) if !files.is_empty() => {
                    let recorded = changes(item).len();
                    let omitted = recorded.saturating_sub(files.len());
                    Body::Files { files, omitted: (omitted > 0).then_some((recorded, omitted)) }
                }
                // Pierre reads text without a diff in it as no files; this parser refuses it.
                Ok(_) => Body::Raw { reason: UNSUPPORTED, text: patch },
                Err(error) if error.message().as_ref() == NOT_A_DIFF => Body::Raw { reason: UNSUPPORTED, text: patch },
                Err(error) => {
                    log::warn!("unparseable tool call diff: {}", error.message());
                    Body::Raw { reason: "Failed to parse patch. Showing raw patch.", text: patch }
                }
            }
        }
        _ if !diff.content.is_empty() => Body::Recorded(diff.content),
        Some(_) => Body::NoNetChanges,
        None => Body::NoPatch,
    };
    ToolCallView { body, truncated: diff.truncated }
}

struct ToolCallDiff {
    patch: Option<String>,
    content: Vec<(&'static str, String)>,
    truncated: bool,
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn changes(item: &Value) -> &[Value] {
    item.get("changes").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// JS `trimStart`, which also drops a byte order mark.
fn js_trim_start(text: &str) -> &str {
    text.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// JS `trim`.
fn js_trim(text: &str) -> &str {
    js_trim_start(text).trim_end_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// `JSON.stringify` of a path, which Git's quoted header form reads back.
fn quoted(path: &str) -> String {
    serde_json::to_string(path).unwrap_or_default()
}

fn parse(patch: &str) -> Result<Vec<DiffFile>, DiffParseError> {
    DiffFile::parse_with(patch, DiffParseOptions::new().truncated_hunks(true))
}

/// `recordedText`: the value without the transport's cut marker, and whether it had one.
fn recorded_text(value: Option<&str>) -> (Option<&str>, bool) {
    match value.and_then(|value| value.strip_suffix(TRANSPORT_CUT)) {
        Some(text) => (Some(text), true),
        None => (value, false),
    }
}

/// `toolCallDiff`.
fn tool_call_diff(item: &Value) -> ToolCallDiff {
    let (diff, diff_cut) = recorded_text(text(item, "diffStr"));
    let operation = changes(item).first().and_then(|change| text(change, "operation"));
    let empty = (diff == Some("")).then_some("");
    let (before, before_cut) =
        recorded_text(text(item, "oldStr").or(if operation == Some("delete") { empty } else { None }));
    let (after, after_cut) =
        recorded_text(text(item, "newStr").or(if operation == Some("add") { empty } else { None }));
    let content = if diff.is_some_and(|diff| !js_trim(diff).is_empty()) {
        Vec::new()
    } else {
        [("Recorded content before", before), ("Recorded content after", after)]
            .into_iter()
            .filter_map(|(label, text)| Some((label, text?.to_owned())))
            .collect()
    };
    ToolCallDiff { patch: tool_call_patch(item, diff), content, truncated: diff_cut || before_cut || after_cut }
}

/// `toolCallPatch`: the recorded diff as a patch with file headers.
fn tool_call_patch(item: &Value, diff: Option<&str>) -> Option<String> {
    let patch = diff?;
    if patch.is_empty() {
        return Some(String::new());
    }
    let recorded = changes(item);
    let change = recorded.first();
    let operation = change.and_then(|change| text(change, "operation"));
    let added = operation == Some("add");
    let raw_operation = added || operation == Some("delete");
    let headed = patch.starts_with("diff --git ") || patch.starts_with("--- ");
    let body = if raw_operation {
        if headed && complete_patch(patch, recorded) {
            return Some(patch.to_owned());
        }
        // Codex can record added/deleted file contents or bare hunks instead of a complete patch.
        contents_hunk(patch, added)
    } else if headed || !js_trim_start(patch).starts_with("@@ ") {
        return Some(patch.to_owned());
    } else {
        patch.to_owned()
    };
    let file_name = text(item, "fileName").unwrap_or_default();
    let old_path = change.and_then(|change| text(change, "oldPath")).unwrap_or(file_name);
    let new_path = change.and_then(|change| text(change, "path")).unwrap_or(file_name);
    let (old, new) = (quoted(&format!("a/{old_path}")), quoted(&format!("b/{new_path}")));
    let before = if added { "/dev/null".to_owned() } else { old.clone() };
    let after = if operation == Some("delete") { "/dev/null".to_owned() } else { new.clone() };
    Some(format!("diff --git {old} {new}\n--- {before}\n+++ {after}\n{}", js_trim_start(&body)))
}

/// Whether a headed patch already is the recorded add or delete: every file has hunks, only
/// the side the operation keeps, and a recorded path.
fn complete_patch(patch: &str, recorded: &[Value]) -> bool {
    let Ok(files) = parse(patch) else { return false };
    !files.is_empty()
        && files.iter().all(|file| {
            let path = file.path().replace('\\', "/");
            file.hunk_count() > 0
                && recorded.iter().any(|change| {
                    let sides = match text(change, "operation") {
                        Some("add") => file.lines_count(DiffSide::Original) == 0,
                        Some("delete") => file.lines_count(DiffSide::Modified) == 0,
                        _ => true,
                    };
                    let recorded_path = text(change, "path").unwrap_or_default().replace('\\', "/");
                    sides && (recorded_path == path || recorded_path.ends_with(&format!("/{path}")))
                })
        })
}

/// An added or deleted file's contents as its one hunk.
fn contents_hunk(contents: &str, added: bool) -> String {
    let final_newline = contents.ends_with('\n');
    let mut lines: Vec<&str> = contents.split('\n').collect();
    if final_newline {
        lines.pop();
    }
    let range = format!("1,{}", lines.len());
    let (old, new) = if added { ("0,0", range.as_str()) } else { (range.as_str(), "0,0") };
    let mark = if added { '+' } else { '-' };
    let body: Vec<String> = lines.iter().map(|line| format!("{mark}{line}")).collect();
    let ending = if final_newline { "" } else { "\\ No newline at end of file\n" };
    format!("@@ -{old} +{new} @@\n{}\n{ending}", body.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn files(patch: Option<&str>) -> Vec<DiffFile> {
        parse(patch.expect("a patch")).expect("a parseable patch")
    }

    #[test]
    fn recorded_hunks_get_headers_including_renamed_paths() {
        for (changes, path) in [
            (None, "old.ts"),
            (Some(json!([{"operation": "update", "oldPath": "old.ts", "path": "new.ts"}])), "new.ts"),
        ] {
            let mut item = json!({"fileName": "old.ts", "diffStr": "@@ -1 +1 @@\n-before\n+after\n"});
            if let Some(changes) = changes {
                item["changes"] = changes;
            }
            let diff = tool_call_diff(&item);
            let files = files(diff.patch.as_deref());
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path().as_ref(), path);
            assert_eq!((files[0].additions(), files[0].deletions()), (1, 1));
        }
    }

    #[test]
    fn added_and_deleted_contents_become_one_hunk() {
        let contents = [
            ("first\nsecond", ""),
            ("@@ -0,0 +1 @@\n+content", "\n"),
            ("@@ -1 +0,0 @@\n-content", "\n"),
            ("first\nsecond", "\n"),
            ("--- notes\nsecond", "\n"),
            ("diff --git notes\nsecond", "\n"),
            ("--- a/embedded.ts\n+++ b/embedded.ts\n@@ -1 +1 @@\n-before\n+after", "\n"),
            (
                "diff --git a/embedded.ts b/embedded.ts\n--- a/embedded.ts\n+++ b/embedded.ts\n@@ -1 +1 @@\n-before\n+after",
                "\n",
            ),
        ];
        for operation in ["add", "delete"] {
            for (content, ending) in contents {
                let recorded = format!("{content}{ending}");
                let item = json!({
                    "fileName": "file with spaces.ts",
                    "diffStr": recorded,
                    "changes": [{"operation": operation, "path": "file with spaces.ts"}],
                });
                let files = files(tool_call_diff(&item).patch.as_deref());
                assert_eq!(files.len(), 1, "{operation} {content:?}");
                let file = &files[0];
                assert_eq!(file.path().as_ref(), "file with spaces.ts");
                let lines = content.split('\n').count();
                let expected = if operation == "add" { (lines, 0) } else { (0, lines) };
                assert_eq!((file.additions(), file.deletions()), expected, "{operation} {content:?}");
            }
        }
        let item =
            json!({"fileName": "a.ts", "diffStr": "first\nsecond", "changes": [{"operation": "add", "path": "a.ts"}]});
        assert_eq!(
            tool_call_diff(&item).patch.as_deref(),
            Some(
                "diff --git \"a/a.ts\" \"b/a.ts\"\n--- /dev/null\n+++ \"b/a.ts\"\n@@ -0,0 +1,2 @@\n+first\n+second\n\\ No newline at end of file\n"
            )
        );
    }

    #[test]
    fn contents_without_a_diff_show_as_recorded_and_empty_contents_stay() {
        for (operation, label) in [("add", "Recorded content after"), ("delete", "Recorded content before")] {
            let item = json!({"fileName": "empty.ts", "diffStr": "", "changes": [{"operation": operation, "path": "empty.ts"}]});
            assert_eq!(tool_call_diff(&item).content, [(label, String::new())]);
        }
        let diff = tool_call_diff(&json!({"fileName": "a.ts", "oldStr": "before", "newStr": ""}));
        assert_eq!(diff.patch, None);
        assert_eq!(
            diff.content,
            [("Recorded content before", "before".to_owned()), ("Recorded content after", String::new())]
        );
        assert!(!diff.truncated);
        assert_eq!(
            tool_call_diff(&json!({"fileName": "a.ts", "newStr": "after"})).content,
            [("Recorded content after", "after".to_owned())]
        );
    }

    #[test]
    fn transport_cuts_are_removed_and_reported() {
        let item = json!({
            "fileName": "a.ts",
            "diffStr": format!("first\nsecond{TRANSPORT_CUT}"),
            "changes": [{"operation": "add", "path": "a.ts"}],
        });
        let diff = tool_call_diff(&item);
        assert!(diff.truncated);
        assert!(!diff.patch.as_deref().unwrap_or_default().contains("output truncated"));
        assert_eq!(files(diff.patch.as_deref()).len(), 1);
        let diff = tool_call_diff(&json!({"fileName": "a.ts", "newStr": format!("content{TRANSPORT_CUT}")}));
        assert_eq!(diff.content, [("Recorded content after", "content".to_owned())]);
        assert!(diff.truncated);
    }

    #[test]
    fn complete_patches_and_unsupported_output_pass_through() {
        for operation in ["add", "delete"] {
            let added = operation == "add";
            let diff = [
                "diff --git a/a.ts b/a.ts",
                if added { "--- /dev/null" } else { "--- a/a.ts" },
                if added { "+++ b/a.ts" } else { "+++ /dev/null" },
                if added { "@@ -0,0 +1 @@" } else { "@@ -1 +0,0 @@" },
                if added { "+content" } else { "-content" },
                "",
            ]
            .join("\n");
            let item =
                json!({"fileName": "a.ts", "diffStr": diff, "changes": [{"operation": operation, "path": "a.ts"}]});
            assert_eq!(tool_call_diff(&item).patch.as_deref(), Some(diff.as_str()));
        }
        for diff in [None, Some(""), Some("provider output"), Some("--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n-a\n+b\n")] {
            let mut item = json!({"fileName": "a.ts"});
            if let Some(diff) = diff {
                item["diffStr"] = diff.into();
            }
            assert_eq!(tool_call_diff(&item).patch.as_deref(), diff);
        }
    }

    #[test]
    fn the_panel_shows_files_raw_text_contents_or_an_empty_state() {
        let reply = |item: Value| json!({"item": item});
        let change = |path: &str| json!({"operation": "update", "path": path});
        let one = json!({"type": "file_change", "fileName": "a.ts", "diffStr": "@@ -1 +1 @@\n-a\n+b\n",
            "changes": [change("a.ts"), change("b.ts"), change("c.ts")]});
        let ToolCallView { body: Body::Files { files, omitted }, truncated: false } = view(&reply(one), false) else {
            panic!("files");
        };
        assert_eq!((files.len(), omitted), (1, Some((3, 2))));

        let spaced = json!({"type": "file_change", "fileName": "a.ts", "diffStr": "@@ -1 +1 @@\n-a  b\n+a b\n"});
        let Body::Files { files, omitted: None } = view(&reply(spaced), true).body else { panic!("files") };
        assert_eq!((files[0].additions(), files[0].deletions()), (0, 0));

        let raw = json!({"type": "file_change", "fileName": "a.ts", "diffStr": "provider output"});
        let Body::Raw { reason, text } = view(&reply(raw), false).body else { panic!("raw") };
        assert_eq!((reason, text.as_str()), ("Unsupported diff format. Showing raw patch.", "provider output"));

        let contents = json!({"type": "file_change", "fileName": "a.ts", "newStr": "after"});
        assert!(matches!(view(&reply(contents), false).body, Body::Recorded(content) if content.len() == 1));
        let blank = json!({"type": "file_change", "fileName": "a.ts", "diffStr": "  \n"});
        assert!(matches!(view(&reply(blank), false).body, Body::NoNetChanges));
        let nothing = json!({"type": "file_change", "fileName": "a.ts"});
        assert!(matches!(view(&reply(nothing), false).body, Body::NoPatch));
        assert!(matches!(view(&json!({"item": null}), false).body, Body::NoPatch));
        let command = json!({"type": "command_execution", "diffStr": "@@ -1 +1 @@\n-a\n+b\n"});
        assert!(matches!(view(&reply(command), false).body, Body::NoPatch));
    }
}
