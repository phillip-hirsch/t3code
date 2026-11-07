//! Opening a diff's file from its header (`openDiffFilePrimaryAction`): the path opens in the
//! panel's Files surface; the rest of the header folds the file, as the chevron does.

use gpui_kit::component::diff::{DiffFile, DiffFileStatus, DiffState};
use gpui_kit::component::h_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, svg,
};

use super::super::Workspace;
use super::super::git::{self, DiffColorScheme};
use super::super::markdown_ui::workspace_relative;
use super::diff_load::FileStat;
use crate::theme::Tokens;

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with("\\\\") || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// `normalizedRelativePathSegments`: `None` for absolute paths and paths that climb out.
fn relative_segments(path: &str) -> Option<Vec<&str>> {
    if path.starts_with('/') || is_windows_absolute(path) {
        return None;
    }
    let segments: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".").collect();
    (!segments.is_empty() && !segments.contains(&"..")).then_some(segments)
}

/// `normalizeProjectPathForComparison`.
fn comparable_root(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if is_windows_absolute(trimmed) { trimmed.replace('/', "\\").to_lowercase() } else { trimmed.to_owned() }
}

/// `repositoryRelativeWorkspaceSegments`: where the workspace sits inside the repository.
fn workspace_segments(workspace_root: Option<&str>, repository_root: Option<&str>) -> Option<Vec<String>> {
    let (workspace, repository) = (comparable_root(workspace_root?), comparable_root(repository_root?));
    if workspace == repository {
        return Some(Vec::new());
    }
    let separator = if repository.contains('\\') { '\\' } else { '/' };
    let rest = workspace.strip_prefix(&format!("{repository}{separator}"))?;
    Some(rest.split(['/', '\\']).filter(|s| !s.is_empty()).map(str::to_owned).collect())
}

/// `resolveDiffPathForWorkspace`: a diff file's path relative to the workspace, or `None`
/// when it lies outside it. Diffs name files from the repository root, or by absolute path.
pub(super) fn workspace_path(
    file_path: &str,
    workspace_root: Option<&str>,
    repository_root: Option<&str>,
) -> Option<String> {
    if file_path.starts_with('/') || is_windows_absolute(file_path) {
        return Some(relative_segments(&workspace_relative(file_path, workspace_root)?)?.join("/"));
    }
    let file = relative_segments(file_path)?;
    let Some(workspace) = workspace_segments(workspace_root, repository_root).filter(|s| !s.is_empty()) else {
        return Some(file.join("/"));
    };
    let case_insensitive = repository_root.is_some_and(is_windows_absolute);
    let inside = workspace.iter().enumerate().all(|(index, segment)| {
        file.get(index).is_some_and(|candidate| {
            if case_insensitive { candidate.to_lowercase() == *segment } else { candidate == segment }
        })
    });
    let rest = file.get(workspace.len()..).filter(|rest| inside && !rest.is_empty())?;
    Some(rest.join("/"))
}

impl Workspace {
    /// `openDiffFile`: the open checkout's file, in the panel.
    pub(super) fn open_diff_file(&mut self, file_path: &str, window: &mut Window, cx: &mut gpui_kit::Context<Self>) {
        let worktree = match &self.target {
            Some(super::super::Target::Thread { id, .. }) => {
                self.shell.threads.iter().find(|thread| &thread.id == id).and_then(|t| t.worktree_path.clone())
            }
            _ => None,
        };
        let project = self.target_project();
        let workspace_root = worktree.clone().or_else(|| project.map(|p| p.workspace_root.clone()));
        let repository_root = worktree
            .is_none()
            .then(|| project.and_then(|p| p.repository_identity.as_ref()?.root_path.clone()))
            .flatten();
        if let Some(path) = workspace_path(file_path, workspace_root.as_deref(), repository_root.as_deref()) {
            self.open_file(&path, None, window, cx);
        }
    }
}

/// The header's `+n −n`: a lazily read file's metadata counts, which cover changes a cut or
/// failed patch is missing (`renderHeaderMetadata`), else the parsed patch's.
fn header_counts(file: &DiffFile, stat: Option<&FileStat>) -> Option<(usize, usize)> {
    if let Some(stat) = stat {
        return Some((stat.additions, stat.deletions));
    }
    let counted = !matches!(file.status(), DiffFileStatus::Unchanged | DiffFileStatus::Conflicted);
    counted.then(|| (file.additions(), file.deletions()))
}

/// Pierre's `getIconForType` icon and its colour (`diffRendering.ts`, `--diff-modified-icon`).
fn change_icon(status: DiffFileStatus, tokens: &Tokens, scheme: DiffColorScheme) -> (&'static str, Hsla) {
    let blue_orange = scheme == DiffColorScheme::BlueOrange;
    match status {
        DiffFileStatus::Added => (
            "icons/app/diffs/symbol-added.svg",
            if tokens.dark && !blue_orange { tokens.success_foreground } else { git::addition_color(tokens, scheme) },
        ),
        DiffFileStatus::Deleted => ("icons/app/diffs/symbol-deleted.svg", git::deletion_color(tokens, scheme)),
        DiffFileStatus::Modified | DiffFileStatus::Renamed | DiffFileStatus::Copied => {
            let icon = if status == DiffFileStatus::Modified {
                "icons/app/diffs/symbol-modified.svg"
            } else {
                "icons/app/diffs/symbol-moved.svg"
            };
            let tint = if blue_orange { tokens.muted_foreground } else { tokens.info_foreground };
            let color = if tokens.dark {
                tint.opacity(0.65)
            } else if blue_orange {
                tint
            } else {
                tint.blend(tokens.muted_foreground.opacity(0.5))
            };
            (icon, color)
        }
        _ => ("icons/app/diffs/file-code.svg", tokens.foreground.opacity(0.6)),
    }
}

/// The default diff header (change icon, path, counts, status) with the path opening the file
/// and the rest of the row folding it. `stat` is the file's metadata when its patch loads on
/// its own; it shows as `DiffStatLabel`, the patch's own counts as Pierre's `-n +n`.
#[expect(clippy::too_many_arguments, reason = "the kit's header callback hands these over one by one")]
pub(super) fn header(
    file: &DiffFile,
    stat: Option<&FileStat>,
    state: Entity<DiffState>,
    view: Entity<Workspace>,
    tokens: &Tokens,
    scheme: DiffColorScheme,
    suffix: Option<AnyElement>,
    cx: &App,
) -> AnyElement {
    let name: SharedString = match (file.original_path(), file.modified_path()) {
        (Some(old), Some(new)) if old != new => format!("{old} → {new}").into(),
        _ => file.path().clone(),
    };
    let status = match file.status() {
        DiffFileStatus::Added => Some("Added file"),
        DiffFileStatus::Deleted => Some("Deleted file"),
        DiffFileStatus::Renamed => Some("Renamed"),
        DiffFileStatus::Copied => Some("Copied"),
        DiffFileStatus::Conflicted => Some("Conflicts"),
        _ => None,
    };
    let (icon, icon_color) = change_icon(file.status(), tokens, scheme);
    let counts = match (stat, header_counts(file, None)) {
        (Some(stat), _) => Some(
            git::diff_stat_label(stat.additions as u64, stat.deletions as u64, 11., tokens, scheme).into_any_element(),
        ),
        (None, Some((additions, deletions))) => Some(pierre_counts(additions, deletions, tokens, scheme)),
        (None, None) => None,
    };
    let path = file.path().clone();
    let collapsed = state.read(cx).is_file_collapsed(&path);
    let (open_path, fold_path, fold_view) = (path.clone(), path, view.clone());
    // `[data-title]:hover`: the code foreground mixed 16% toward primary.
    let hover = tokens.foreground.blend(tokens.primary.opacity(0.16));
    let muted = tokens.muted_foreground;
    h_flex()
        .id("diff-file-header-content")
        .w_full()
        .min_w_0()
        .gap_2()
        .child(svg().path(icon).size(px(16.)).flex_none().text_color(icon_color))
        .on_click(move |_, _, cx| {
            state.update(cx, |state, cx| state.set_file_collapsed(&fold_path, !collapsed, cx));
            fold_view.update(cx, Workspace::remember_collapsed);
        })
        .child(
            div()
                .id("diff-file-title")
                .min_w_0()
                .truncate()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .cursor_pointer()
                .hover(move |style| style.text_color(hover))
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    view.update(cx, |this, cx| this.open_diff_file(&open_path, window, cx));
                })
                .child(name),
        )
        .child(div().flex_1())
        .children(counts)
        .when_some(status, |el, status| el.child(div().text_xs().text_color(muted).child(status)))
        .children(suffix)
        .into_any_element()
}

/// Pierre's `createMetadataElement`: `-n` unless only lines were added, then `+n` unless only
/// lines were removed, in the code font at 11px, 1ch apart.
fn pierre_counts(additions: usize, deletions: usize, tokens: &Tokens, scheme: DiffColorScheme) -> AnyElement {
    let count = |n: usize| git::group_thousands(n as u64);
    h_flex()
        .flex_none()
        .gap(px(6.6))
        .font_family(tokens.fonts.mono)
        .text_size(px(11.))
        .when(deletions > 0 || additions == 0, |el| {
            el.child(div().text_color(git::deletion_color(tokens, scheme)).child(format!("-{}", count(deletions))))
        })
        .when(additions > 0 || deletions == 0, |el| {
            el.child(div().text_color(git::addition_color(tokens, scheme)).child(format!("+{}", count(additions))))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::diff_load::{FileStat, settle_file};
    use super::{header_counts, workspace_path};

    #[test]
    fn lazily_read_files_count_every_change_from_their_metadata() {
        let stat = FileStat { path: "a.txt".into(), previous_path: None, additions: 40, deletions: 7 };
        let patch = "--- a/a.txt
+++ b/a.txt
@@ -1,1 +1,2 @@
-old
+new
+more


[truncated]";
        let reply = json!({"sources": [{"kind": "branch-range", "diff": patch, "truncated": true}]});
        // A patch cut at the size limit holds only some of the changes.
        let (files, _) = settle_file(&stat, Some(&reply), "branch-range");
        assert_eq!(header_counts(&files[0], Some(&stat)), Some((40, 7)));
        assert_eq!(header_counts(&files[0], None), Some((2, 1)));
        // A failed read shows a placeholder row with no lines.
        let (failed, _) = settle_file(&stat, None, "branch-range");
        assert_eq!(header_counts(&failed[0], Some(&stat)), Some((40, 7)));
    }

    #[test]
    fn diff_paths_resolve_from_the_repository_into_the_workspace() {
        // The workspace is the repository: repository paths open as they are.
        assert_eq!(workspace_path("src/a.rs", Some("/repo"), Some("/repo")).as_deref(), Some("src/a.rs"));
        assert_eq!(workspace_path("./src//a.rs", Some("/repo"), None).as_deref(), Some("src/a.rs"));
        // A workspace inside the repository drops its own prefix and refuses files outside it.
        assert_eq!(
            workspace_path("apps/web/main.ts", Some("/repo/apps/web"), Some("/repo")).as_deref(),
            Some("main.ts")
        );
        assert_eq!(workspace_path("apps/server/main.ts", Some("/repo/apps/web"), Some("/repo")), None);
        assert_eq!(workspace_path("apps/web", Some("/repo/apps/web"), Some("/repo")), None);
        assert_eq!(
            workspace_path("Apps/Web/main.ts", Some("C:\\Repo\\apps\\web"), Some("C:\\Repo")).as_deref(),
            Some("main.ts")
        );
        // Absolute paths (Claude's tool diffs) open only under the workspace.
        assert_eq!(workspace_path("/repo/src/a.rs", Some("/repo"), None).as_deref(), Some("src/a.rs"));
        assert_eq!(workspace_path("/other/a.rs", Some("/repo"), None), None);
        assert_eq!(workspace_path("../a.rs", Some("/repo"), None), None);
    }
}
