//! A chat file reference that names no real path, found among the workspace's files
//! (`workspaceBasenameLookup.ts` and `findWorkspaceFileMatch` in `ChatMarkdown.tsx`): a bare
//! basename (`README.md`), or a path that repeats the project folder's own tail
//! (`apps/web/src/a.ts` written inside `apps/web`).

use gpui_kit::{Context, Task, Window};
use serde_json::{Value, json};

use super::Workspace;
use crate::file_links;

/// `WORKSPACE_BASENAME_LOOKUP_LIMIT`.
const LOOKUP_LIMIT: u32 = 25;

/// `needsWorkspaceBasenameLookup`.
fn needs_basename_lookup(path: &str) -> bool {
    let trimmed = path.trim();
    !matches!(trimmed, "" | "." | "..") && !trimmed.contains(['/', '\\'])
}

fn is_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with(['/', '\\'])
        || path.starts_with('~')
        || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/'))
}

fn windows_cwd(cwd: &str) -> bool {
    let b = cwd.as_bytes();
    cwd.starts_with("\\\\") || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// `stripRepeatedWorkspacePrefix`: `path` without the leading folders that repeat `cwd`'s tail.
fn strip_repeated_prefix(path: &str, cwd: &str) -> Option<String> {
    if is_absolute(path) {
        return None;
    }
    let segments: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let cwd_segments: Vec<&str> = cwd.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let fold = |segment: &str| if windows_cwd(cwd) { segment.to_lowercase() } else { segment.to_owned() };
    (1..=cwd_segments.len().min(segments.len().saturating_sub(1))).rev().find_map(|count| {
        let prefix = &cwd_segments[cwd_segments.len() - count..];
        prefix
            .iter()
            .zip(&segments)
            .all(|(a, b)| fold(a) == fold(b))
            .then(|| segments[count..].join("/"))
            .filter(|rest| !rest.is_empty())
    })
}

/// The files among a `projects.searchEntries` answer.
fn file_entries(answer: &Value) -> Vec<&str> {
    let entries = answer.get("entries").and_then(Value::as_array);
    entries
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("kind").and_then(Value::as_str) == Some("file"))
        .filter_map(|entry| entry.get("path")?.as_str())
        .collect()
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `pickWorkspaceBasenameMatch`: the file with that exact name, else the only one with it in
/// another case.
fn pick_basename_match(name: &str, files: &[&str]) -> Option<String> {
    let target = name.trim();
    if target.is_empty() {
        return None;
    }
    if let Some(exact) = files.iter().find(|path| basename(path) == target) {
        return Some((*exact).to_owned());
    }
    let folded = target.to_lowercase();
    let mut matches = files.iter().filter(|path| basename(path).to_lowercase() == folded);
    match (matches.next(), matches.next()) {
        (Some(only), None) => Some((*only).to_owned()),
        _ => None,
    }
}

/// `pickStrippedWorkspaceMatch`.
fn pick_stripped_match(stripped: &str, cwd: &str, files: &[&str]) -> Option<String> {
    let key = |path: &str| {
        let normalized = path.replace('\\', "/");
        if windows_cwd(cwd) { normalized.to_lowercase() } else { normalized }
    };
    files.iter().find(|path| key(path) == key(stripped)).map(|path| (*path).to_owned())
}

impl Workspace {
    /// Runs `then` with the absolute target of the file reference `authored` (as the message wrote
    /// it, maybe with `:line:col`): the workspace file it names when it names no real path, else
    /// `target`. A newer lookup cancels the one before it, so the latest click wins, and one that
    /// finishes after the open thread or its folder changed is dropped.
    pub(super) fn with_workspace_match(
        &mut self,
        authored: &str,
        target: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, String, &mut Window, &mut Context<Self>) + 'static,
    ) {
        self.file_lookup = None;
        let path = file_links::without_position(authored);
        let suffix = authored[path.len()..].to_owned();
        let (Some(cwd), Some(client)) = (self.git_cwd(), self.client()) else { return then(self, target, window, cx) };
        let basename = needs_basename_lookup(path);
        let stripped = if basename { None } else { strip_repeated_prefix(path, &cwd) };
        if !basename && stripped.is_none() {
            return then(self, target, window, cx);
        }
        let query = stripped.clone().unwrap_or_else(|| path.trim().to_owned());
        let literal = file_links::resolve_target(path, Some(&cwd));
        let search = json!({"cwd": cwd, "query": query, "limit": LOOKUP_LIMIT, "kind": "file"});
        let scope = self.target.clone();
        let lookup: Task<()> = cx.spawn_in(window, async move |this, cx| {
            let answer = client.request("projects.searchEntries", search).await.ok();
            let files = answer.as_ref().map(file_entries).unwrap_or_default();
            let found = match &stripped {
                None => pick_basename_match(&query, &files),
                Some(stripped) => match pick_stripped_match(stripped, &cwd, &files) {
                    // Search can be truncated or still scanning, so it cannot rule out the literal
                    // path: redirect only when the host says that path is missing.
                    Some(found) => {
                        let metadata = client.request("filesystem.getMetadata", json!({"paths": [literal]})).await;
                        metadata.ok().filter(|m| m["entries"][0].is_null() && m["entries"].is_array()).map(|_| found)
                    }
                    None => None,
                },
            };
            let resolved = found
                .and_then(|found| file_links::resolve_target(&found, Some(&cwd)))
                .map_or(target, |found| format!("{found}{suffix}"));
            let _ = this.update_in(cx, |this, window, cx| {
                this.file_lookup = None;
                if this.target != scope || this.git_cwd().as_deref() != Some(cwd.as_str()) {
                    return;
                }
                then(this, resolved, window, cx);
            });
        });
        self.file_lookup = Some(lookup);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_names_and_repeated_project_folders_need_a_lookup() {
        assert!(needs_basename_lookup("README.md"));
        assert!(!needs_basename_lookup("src/a.ts"));
        assert!(!needs_basename_lookup(".."));
        assert_eq!(strip_repeated_prefix("apps/web/src/a.ts", "/repo/apps/web").as_deref(), Some("src/a.ts"));
        assert_eq!(strip_repeated_prefix("web/src/a.ts", "/repo/apps/web").as_deref(), Some("src/a.ts"));
        assert_eq!(strip_repeated_prefix("src/a.ts", "/repo/apps/web"), None);
        assert_eq!(strip_repeated_prefix("Web\\a.ts", "C:\\repo\\web").as_deref(), Some("a.ts"));
        assert_eq!(strip_repeated_prefix("web", "/repo/web"), None);
        assert_eq!(strip_repeated_prefix("/repo/web/a.ts", "/repo/web"), None);
    }

    #[test]
    fn matches_pick_the_file_the_reference_names() {
        let answer = json!({"entries": [
            {"path": "docs", "kind": "directory"},
            {"path": "docs/Readme.md", "kind": "file"},
            {"path": "src/a.ts", "kind": "file"},
        ]});
        let files = file_entries(&answer);
        assert_eq!(files, ["docs/Readme.md", "src/a.ts"]);
        assert_eq!(pick_basename_match("README.md", &files).as_deref(), Some("docs/Readme.md"));
        assert_eq!(pick_basename_match("a.ts", &files).as_deref(), Some("src/a.ts"));
        assert_eq!(pick_basename_match("b.ts", &files), None);
        assert_eq!(pick_basename_match("x.md", &["a/X.md", "b/x.MD"]), None);
        assert_eq!(pick_stripped_match("SRC/a.ts", "C:\\repo", &files).as_deref(), Some("src/a.ts"));
        assert_eq!(pick_stripped_match("SRC/a.ts", "/repo", &files), None);
    }
}
