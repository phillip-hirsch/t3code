//! The Diff surface's file tree (`DiffFileTree` beside the patch) and its "Hide test files"
//! filter. Folders come in the order of their first file, as `orderDiffFilesLikeTree` keeps
//! the list and the tree in agreement.

use std::collections::HashSet;

use gpui_kit::component::diff::{DiffFile, DiffFileStatus};
use gpui_kit::component::{Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use super::super::Workspace;
use super::super::git;
use super::super::titlebar::icon;
use crate::theme::Tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Added,
    Deleted,
    Renamed,
    Modified,
}

impl Status {
    fn of(file: &DiffFile) -> Self {
        match file.status() {
            DiffFileStatus::Added => Self::Added,
            DiffFileStatus::Deleted => Self::Deleted,
            DiffFileStatus::Renamed | DiffFileStatus::Copied => Self::Renamed,
            _ => Self::Modified,
        }
    }

    fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Modified => "M",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Row {
    pub depth: usize,
    pub name: String,
    /// The file's path, or the folder's with a trailing `/`.
    pub path: String,
    pub status: Option<Status>,
}

/// The tree's visible rows: each folder before its contents, folders and files in the order
/// of their first file, folded folders without their contents.
pub(super) fn rows(files: &[(String, Status)], folded: &HashSet<String>) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    // `diffFileTreePositions`: a folder ranks where its first file does.
    let mut first: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (index, (path, _)) in files.iter().enumerate() {
        let mut folder = String::new();
        for segment in path.split('/').rev().skip(1).collect::<Vec<_>>().into_iter().rev() {
            folder.push_str(segment);
            folder.push('/');
            first.entry(folder.clone()).or_insert(index);
        }
    }
    let keys: Vec<Vec<usize>> = files
        .iter()
        .enumerate()
        .map(|(index, (path, _))| {
            let mut folder = String::new();
            let mut key: Vec<usize> = Vec::new();
            for segment in path.split('/').rev().skip(1).collect::<Vec<_>>().into_iter().rev() {
                folder.push_str(segment);
                folder.push('/');
                key.push(first[&folder]);
            }
            key.push(index);
            key
        })
        .collect();
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by(|a, b| keys[*a].cmp(&keys[*b]));
    for index in order {
        let (path, status) = &files[index];
        let segments: Vec<&str> = path.split('/').collect();
        let mut folder = String::new();
        let mut hidden = false;
        for (depth, segment) in segments[..segments.len() - 1].iter().enumerate() {
            folder.push_str(segment);
            folder.push('/');
            if !hidden && emitted.insert(folder.clone()) {
                out.push(Row { depth, name: (*segment).to_owned(), path: folder.clone(), status: None });
            }
            hidden |= folded.contains(&folder);
        }
        if !hidden {
            out.push(Row {
                depth: segments.len() - 1,
                name: segments.last().copied().unwrap_or_default().to_owned(),
                path: path.clone(),
                status: Some(*status),
            });
        }
    }
    out
}

impl Workspace {
    /// The loaded files the patch shows: test files leave while "Hide test files" is on.
    pub(super) fn shown_diff_files(&self) -> Vec<DiffFile> {
        let hide = self.right_panel.hide_test_files;
        self.right_panel
            .all_diff_files
            .iter()
            .filter(|file| !hide || !crate::pull_request_diff::is_test_file(file.path()))
            .cloned()
            .collect()
    }

    /// How many loaded files are tests or test support.
    pub(super) fn test_file_count(&self) -> usize {
        self.right_panel.all_diff_files.iter().filter(|f| crate::pull_request_diff::is_test_file(f.path())).count()
    }

    /// Puts the filtered files back into the patch, keeping their folds.
    pub(super) fn apply_diff_files(&mut self, cx: &mut Context<Self>) {
        let files = self.shown_diff_files();
        let folded: Vec<SharedString> = {
            let state = self.right_panel.diff.read(cx);
            state.files().iter().map(DiffFile::path).filter(|p| state.is_file_collapsed(p)).cloned().collect()
        };
        self.right_panel.diff.update(cx, |state, cx| {
            state.set_files(files, cx);
            for path in &folded {
                state.set_file_collapsed(path, true, cx);
            }
        });
        cx.notify();
    }

    /// The tree pane beside the patch.
    pub(super) fn render_diff_tree(&self, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let state = self.right_panel.diff.read(cx);
        let files: Vec<(String, Status)> =
            state.files().iter().map(|file| (file.path().to_string(), Status::of(file))).collect();
        let rows = rows(&files, &self.right_panel.tree_folded);
        let scheme = self.diff_color_scheme();
        let selected = self.right_panel.tree_selected.clone();
        let list = rows.into_iter().map(|row| {
            let is_folder = row.status.is_none();
            let folded = is_folder && self.right_panel.tree_folded.contains(&row.path);
            let color = match row.status {
                Some(Status::Added) => git::addition_color(tokens, scheme),
                Some(Status::Deleted) => git::deletion_color(tokens, scheme),
                Some(Status::Renamed) => tokens.info,
                Some(Status::Modified) => tokens.warning,
                None => tokens.muted_foreground,
            };
            let glyph = if is_folder {
                if folded { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" }
            } else {
                "icons/file.svg"
            };
            let path = row.path.clone();
            h_flex()
                .id(ElementId::Name(format!("diff-tree-{}", row.path).into()))
                .h(px(24.))
                .flex_none()
                .pl(px(8. + row.depth as f32 * 12.))
                .pr(px(8.))
                .gap(px(6.))
                .rounded(px(4.))
                .text_xs()
                .cursor_pointer()
                .text_color(if is_folder { tokens.muted_foreground } else { tokens.foreground })
                .when(selected.as_deref() == Some(row.path.as_str()), |el| el.bg(tokens.accent))
                .hover(|s| s.bg(tokens.accent.opacity(0.6)))
                .child(icon(glyph).with_size(px(14.)).flex_none().text_color(tokens.muted_foreground))
                .child(div().flex_1().min_w_0().truncate().child(row.name))
                .children(row.status.map(|status| {
                    div()
                        .flex_none()
                        .font_family(tokens.fonts.mono)
                        .text_size(px(11.))
                        .text_color(color)
                        .child(status.letter())
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if is_folder {
                        if !this.right_panel.tree_folded.remove(&path) {
                            this.right_panel.tree_folded.insert(path.clone());
                        }
                    } else {
                        this.right_panel.tree_selected = Some(path.clone());
                        this.right_panel.diff.update(cx, |state, cx| {
                            state.set_file_collapsed(&path, false, cx);
                            state.scroll_to_file(&path, cx);
                        });
                    }
                    cx.notify();
                }))
                .into_any_element()
        });
        div()
            .id("diff-file-tree")
            .w(gpui_kit::relative(0.4))
            .max_w(px(320.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .p_1()
            .border_l_1()
            .border_color(tokens.border)
            .children(list)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str], folded: &[&str]) -> Vec<(usize, String)> {
        let files: Vec<(String, Status)> = paths.iter().map(|p| ((*p).to_owned(), Status::Modified)).collect();
        let folded: HashSet<String> = folded.iter().map(|f| (*f).to_owned()).collect();
        rows(&files, &folded).into_iter().map(|r| (r.depth, r.name)).collect()
    }

    #[test]
    fn folders_follow_their_first_file_and_fold() {
        let paths = ["src/b.rs", "README.md", "src/app/a.rs", "src/c.rs"];
        assert_eq!(
            tree(&paths, &[]),
            [
                (0, "src".to_owned()),
                (1, "b.rs".to_owned()),
                (1, "app".to_owned()),
                (2, "a.rs".to_owned()),
                (1, "c.rs".to_owned()),
                (0, "README.md".to_owned()),
            ]
        );
        assert_eq!(tree(&paths, &["src/"]), [(0, "src".to_owned()), (0, "README.md".to_owned())]);
    }
}
