//! The Diff surface's file tree (`DiffFileTree` beside the patch) and its "Hide test files"
//! filter. Folders come in the order of their first file, as `orderDiffFilesLikeTree` keeps
//! the list and the tree in agreement.

use std::collections::HashSet;
use std::ops::Range;

use gpui_kit::assets::IconName;
use gpui_kit::component::diff::{DiffFile, DiffFileStatus};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, DragMoveEvent, ElementId, Empty, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, ScrollStrategy, SharedString, StatefulInteractiveElement as _,
    Styled as _, UniformListScrollHandle, Window, div, px, uniform_list,
};

use super::super::git;
use super::super::titlebar::icon;
use super::super::{Target, Workspace};
use super::DiffScope;
use crate::theme::Tokens;

/// `FileExplorerPane` as the diff uses it: 256px at most at first (40% of the row), never under
/// 160px, and leaving the patch 256px while it stays under 560px.
const TREE_DEFAULT: f32 = 256.;
const TREE_FRACTION: f32 = 0.4;
const TREE_MIN: f32 = 160.;
const TREE_MAX: f32 = 560.;
const PATCH_MIN: f32 = 256.;
/// How far one arrow key moves the resize handle.
const RESIZE_STEP: f32 = 16.;

/// The tree's width and its largest in a row `row` wide; a chosen width overrides the default.
fn pane_width(chosen: Option<f32>, row: f32) -> (f32, f32) {
    let initial = TREE_MIN.max(TREE_DEFAULT.min(row * TREE_FRACTION));
    let max = TREE_MIN.max(TREE_MAX.min((row - PATCH_MIN).max(initial)));
    (chosen.unwrap_or(initial).clamp(TREE_MIN, max), max)
}

/// The resize handle's keys: `ArrowLeft` widens, `ArrowRight` narrows, Home and End jump.
fn resized_width(width: f32, max: f32, key: &str) -> Option<f32> {
    let next = match key {
        "left" => width + RESIZE_STEP,
        "right" => width - RESIZE_STEP,
        "home" => TREE_MIN,
        "end" => max,
        _ => return None,
    };
    Some(next.clamp(TREE_MIN, max))
}

/// What a key does in the tree (Pierre's roving focus).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TreeKey {
    Focus(usize),
    Toggle(usize),
    Select(usize),
}

/// The tree's keys: the arrows move and open or close folders, Home and End jump, Enter and
/// Space open a folder or pick a file.
fn tree_key(rows: &[Row], folded: &HashSet<String>, at: Option<usize>, key: &str) -> Option<TreeKey> {
    let last = rows.len().checked_sub(1)?;
    let folder = |at: usize| rows[at].status.is_none();
    let open = |at: usize| folder(at) && !folded.contains(&rows[at].path);
    Some(match (key, at) {
        ("down", None) | ("home", _) => TreeKey::Focus(0),
        ("up", None) | ("end", _) => TreeKey::Focus(last),
        ("down", Some(at)) => TreeKey::Focus((at + 1).min(last)),
        ("up", Some(at)) => TreeKey::Focus(at.saturating_sub(1)),
        ("right", Some(at)) if folder(at) && !open(at) => TreeKey::Toggle(at),
        ("right", Some(at)) if open(at) => TreeKey::Focus((at + 1).min(last)),
        ("right", Some(at)) => TreeKey::Focus(at),
        ("left", Some(at)) if open(at) => TreeKey::Toggle(at),
        ("left", Some(at)) => {
            let depth = rows[at].depth;
            TreeKey::Focus((0..at).rev().find(|&i| rows[i].depth + 1 == depth).unwrap_or(at))
        }
        ("enter" | "space", Some(at)) if folder(at) => TreeKey::Toggle(at),
        ("enter" | "space", Some(at)) => TreeKey::Select(at),
        _ => return None,
    })
}

/// Every folder of the shown files, with its trailing `/`.
fn folders(files: &[(&str, Status)]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (path, _) in files {
        let Some((parent, _)) = path.rsplit_once('/') else { continue };
        let mut folder = String::new();
        for segment in parent.split('/') {
            folder.push_str(segment);
            folder.push('/');
            if seen.insert(folder.clone()) {
                out.push(folder.clone());
            }
        }
    }
    out
}

/// The tree pane's own state: its width, keyboard focus and scroll.
pub(super) struct TreePane {
    /// The width dragged or keyed to; unset, the pane takes its default.
    width: Option<f32>,
    focus: FocusHandle,
    focused: Option<String>,
    resize_focus: FocusHandle,
    toggle_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    folders: Vec<String>,
}

impl TreePane {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            width: None,
            focus: cx.focus_handle().tab_stop(true),
            focused: None,
            resize_focus: cx.focus_handle().tab_stop(true),
            toggle_focus: cx.focus_handle().tab_stop(true),
            scroll: UniformListScrollHandle::new(),
            folders: Vec::new(),
        }
    }
}

/// Drag payload for the tree's resize handle.
struct TreeResize;

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
pub(super) fn rows(files: &[(&str, Status)], folded: &HashSet<String>) -> Vec<Row> {
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
                path: (*path).to_owned(),
                status: Some(*status),
            });
        }
    }
    out
}

/// Whether the patch path `path` is `requested`, a `/`-separated path from the chat.
fn is_requested(path: &str, requested: &str) -> bool {
    path.len() == requested.len()
        && path.bytes().zip(requested.bytes()).all(|(a, b)| a == b || (a == b'\\' && b == b'/'))
}

/// The patch's path for `requested`, if the patch shows it.
pub(super) fn find_requested<'a>(files: &'a [DiffFile], requested: &str) -> Option<&'a SharedString> {
    files.iter().map(DiffFile::path).find(|path| is_requested(path, requested))
}

/// `isFileShown`: while test files hide, a test file shows only as the file the chat asked for.
fn shows_file(path: &str, hide_tests: bool, requested: Option<&str>) -> bool {
    !hide_tests
        || !crate::pull_request_diff::is_test_file(path)
        || requested.is_some_and(|requested| is_requested(path, requested))
}

impl Workspace {
    /// The loaded files the patch shows for `selection` (the one being installed, not the one shown
    /// before it): test files leave while "Hide test files" is on, except the one the chat asked for.
    pub(super) fn shown_diff_files(&self, selection: Option<&(Target, DiffScope)>) -> Vec<DiffFile> {
        let hide = self.right_panel.hide_test_files;
        let requested = selection.and_then(|selection| self.requested_diff_file(selection));
        self.right_panel
            .all_diff_files
            .iter()
            .filter(|file| shows_file(file.path(), hide, requested))
            .cloned()
            .collect()
    }

    /// Stores a loaded selection's files and counts its tests once, not every frame.
    pub(super) fn store_diff_files(&mut self, files: Vec<DiffFile>) {
        let panel = &mut self.right_panel;
        panel.diff_test_files = files.iter().filter(|f| crate::pull_request_diff::is_test_file(f.path())).count();
        panel.all_diff_files = files;
    }

    /// Rebuilds the tree's rows from the files the patch shows; run when they or the folds change.
    pub(super) fn rebuild_tree_rows(&mut self, cx: &App) {
        let panel = &mut self.right_panel;
        let state = panel.diff.read(cx);
        let files: Vec<(&str, Status)> = state.files().iter().map(|f| (f.path().as_ref(), Status::of(f))).collect();
        panel.tree_rows = rows(&files, &panel.tree_folded).into();
        panel.tree_pane.folders = folders(&files);
    }

    fn toggle_diff_tree_folder(&mut self, path: &str, cx: &mut Context<Self>) {
        let folded = &mut self.right_panel.tree_folded;
        if !folded.remove(path) {
            folded.insert(path.to_owned());
        }
        self.rebuild_tree_rows(cx);
        cx.notify();
    }

    fn select_diff_tree_file(&mut self, path: &str, cx: &mut Context<Self>) {
        self.right_panel.tree_selected = Some(path.to_owned());
        self.reveal_diff_file(path, cx);
        cx.notify();
    }

    fn diff_tree_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let rows = self.right_panel.tree_rows.clone();
        let focused = self.right_panel.tree_pane.focused.as_ref();
        let at = focused.and_then(|path| rows.iter().position(|r| &r.path == path));
        let Some(action) = tree_key(&rows, &self.right_panel.tree_folded, at, key) else { return false };
        match action {
            TreeKey::Focus(index) => {
                let pane = &mut self.right_panel.tree_pane;
                pane.focused = Some(rows[index].path.clone());
                pane.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
                cx.notify();
            }
            TreeKey::Toggle(index) => self.toggle_diff_tree_folder(&rows[index].path, cx),
            TreeKey::Select(index) => self.select_diff_tree_file(&rows[index].path, cx),
        }
        true
    }

    /// "Collapse all folders", or "Expand all folders" while any is folded.
    fn toggle_all_tree_folders(&mut self, cx: &mut Context<Self>) {
        let panel = &mut self.right_panel;
        if panel.tree_pane.folders.iter().any(|f| panel.tree_folded.contains(f)) {
            panel.tree_folded.clear();
        } else {
            panel.tree_folded.extend(panel.tree_pane.folders.iter().cloned());
        }
        self.rebuild_tree_rows(cx);
        cx.notify();
    }

    /// Puts the filtered files back into the patch, keeping their folds.
    pub(super) fn apply_diff_files(&mut self, cx: &mut Context<Self>) {
        let files = self.shown_diff_files(self.right_panel.shown.clone().as_ref());
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
        self.rebuild_tree_rows(cx);
        cx.notify();
    }

    /// The tree pane beside the patch (`FileExplorerPane` around `DiffFileTree`) in a row
    /// `row_width` wide: the Files header, the rows, and a handle on its left edge that drags
    /// or keys its width.
    pub(super) fn render_diff_tree(
        &self,
        row_width: f32,
        tokens: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = self.right_panel.tree_rows.clone();
        let pane = &self.right_panel.tree_pane;
        let (width, max) = pane_width(pane.width, row_width);
        let focused = pane.focused.clone().filter(|_| pane.focus.is_focused(window));
        let all_open = !pane.folders.iter().any(|f| self.right_panel.tree_folded.contains(f));
        let list = uniform_list(
            "diff-file-tree-rows",
            rows.len(),
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let tokens = *crate::theme::tokens(cx);
                range
                    .map(|index| {
                        let focused = focused.as_deref() == Some(rows[index].path.as_str());
                        this.diff_tree_row(&rows[index], focused, &tokens, cx)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&pane.scroll)
        .size_full();
        let ring = tokens.ring;
        let toggle_label = if all_open { "Collapse all folders" } else { "Expand all folders" };
        let header = h_flex()
            .flex_none()
            .h(px(28.))
            .mb_3()
            .px_2()
            .gap_1()
            .text_xs()
            .text_color(tokens.muted_foreground)
            .child(div().px_1().font_weight(gpui_kit::FontWeight::MEDIUM).text_color(tokens.foreground).child("Files"))
            .child(div().ml_auto().child(self.right_panel.diff.read(cx).files().len().to_string()))
            .when(!pane.folders.is_empty(), |el| {
                el.child(
                    div()
                        .id("diff-tree-toggle-folders")
                        .track_focus(&pane.toggle_focus)
                        .focus_visible(move |s| s.border_2().border_color(ring))
                        .size(px(24.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(tokens.accent).text_color(tokens.foreground))
                        .child(
                            Icon::new(if all_open { IconName::ChevronsDownUp } else { IconName::ChevronsUpDown })
                                .size(px(14.)),
                        )
                        .tooltip(move |window, cx| Tooltip::new(toggle_label).build(window, cx))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_all_tree_folders(cx))),
                )
            });
        let handle = div()
            .id("diff-file-tree-resize")
            .track_focus(&pane.resize_focus)
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(-3.))
            .w(px(6.))
            .cursor_col_resize()
            .focus_visible(move |s| s.bg(ring))
            .on_drag(TreeResize, |_, _, _, cx| cx.new(|_| Empty))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                let modifiers = &event.keystroke.modifiers;
                if modifiers.alt || modifiers.control || modifiers.platform {
                    return;
                }
                if let Some(next) = resized_width(width, max, &event.keystroke.key) {
                    this.right_panel.tree_pane.width = Some(next);
                    cx.stop_propagation();
                    cx.notify();
                }
            }));
        v_flex()
            .id("diff-file-tree")
            .relative()
            .w(px(width))
            .flex_none()
            .h_full()
            .bg(tokens.background)
            .border_l_1()
            .border_color(tokens.border.opacity(0.6))
            .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<TreeResize>, _, cx| {
                let right = f32::from(event.bounds.right());
                this.right_panel.tree_pane.width =
                    Some((right - f32::from(event.event.position.x)).clamp(TREE_MIN, max));
                cx.notify();
            }))
            .child(handle)
            .child(header)
            .child(
                div()
                    .id("diff-file-tree-keys")
                    .track_focus(&pane.focus)
                    .flex_1()
                    .min_h_0()
                    .px_1()
                    .pb_1()
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                        if !event.keystroke.modifiers.modified() && this.diff_tree_key(&event.keystroke.key, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .child(list),
            )
            .into_any_element()
    }

    fn diff_tree_row(&self, row: &Row, focused: bool, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let is_folder = row.status.is_none();
        let folded = is_folder && self.right_panel.tree_folded.contains(&row.path);
        let selected = self.right_panel.tree_selected.as_deref() == Some(row.path.as_str());
        let scheme = self.diff_color_scheme();
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
            .border_1()
            .border_color(if focused { tokens.ring } else { gpui_kit::transparent_black() })
            .text_xs()
            .cursor_pointer()
            .text_color(if is_folder { tokens.muted_foreground } else { tokens.foreground })
            .when(selected, |el| el.bg(tokens.accent))
            .hover(|s| s.bg(tokens.accent.opacity(0.6)))
            .child(icon(glyph).with_size(px(14.)).flex_none().text_color(tokens.muted_foreground))
            .child(div().flex_1().min_w_0().truncate().child(row.name.clone()))
            .children(row.status.map(|status| {
                div()
                    .flex_none()
                    .font_family(tokens.fonts.mono)
                    .text_size(px(11.))
                    .text_color(color)
                    .child(status.letter())
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                let pane = &mut this.right_panel.tree_pane;
                pane.focused = Some(path.clone());
                window.focus(&pane.focus.clone(), cx);
                if is_folder {
                    this.toggle_diff_tree_folder(&path, cx);
                } else {
                    this.select_diff_tree_file(&path, cx);
                }
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str], folded: &[&str]) -> Vec<(usize, String)> {
        let files: Vec<(&str, Status)> = paths.iter().map(|p| (*p, Status::Modified)).collect();
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

    #[test]
    fn a_requested_file_is_found_only_among_the_files_the_patch_shows() {
        let patch = "diff --git a/src\\app\\a.rs b/src\\app\\a.rs\n--- a/src\\app\\a.rs\n+++ b/src\\app\\a.rs\n@@ -1 +1 @@\n-a\n+b\n";
        let files = DiffFile::parse(patch).expect("a one-file patch parses");
        assert_eq!(find_requested(&files, "src/app/a.rs").map(SharedString::as_ref), Some("src\\app\\a.rs"));
        assert_eq!(find_requested(&files, "src/app/a.test.rs"), None);
    }

    #[test]
    fn hidden_test_files_still_show_the_requested_one() {
        assert!(shows_file("src/a.test.ts", false, None));
        assert!(!shows_file("src/a.test.ts", true, None));
        assert!(!shows_file("src/a.test.ts", true, Some("src/b.test.ts")));
        assert!(shows_file("src\\a.test.ts", true, Some("src/a.test.ts")));
        assert!(shows_file("src/a.ts", true, Some("src/b.test.ts")));
    }

    #[test]
    fn the_tree_starts_at_most_256px_and_resizes_within_its_limits() {
        // 40% of the row, between 160px and 256px.
        assert_eq!(pane_width(None, 1000.), (256., 560.));
        assert_eq!(pane_width(None, 500.), (200., 244.));
        assert_eq!(pane_width(None, 300.), (160., 160.));
        // A chosen width holds while the row has room for it.
        assert_eq!(pane_width(Some(400.), 1000.), (400., 560.));
        assert_eq!(pane_width(Some(400.), 500.), (244., 244.));
        assert_eq!(resized_width(256., 560., "left"), Some(272.));
        assert_eq!(resized_width(256., 560., "right"), Some(240.));
        assert_eq!(resized_width(170., 560., "right"), Some(160.));
        assert_eq!(resized_width(256., 560., "home"), Some(160.));
        assert_eq!(resized_width(256., 560., "end"), Some(560.));
        assert_eq!(resized_width(256., 560., "up"), None);
    }

    #[test]
    fn tree_keys_move_open_close_and_pick() {
        let paths = ["src/app/a.rs", "src/b.rs", "c.rs"];
        let files: Vec<(&str, Status)> = paths.iter().map(|p| (*p, Status::Modified)).collect();
        let open = HashSet::new();
        // src/, app/, a.rs, b.rs, c.rs
        let rows = rows(&files, &open);
        assert_eq!(tree_key(&rows, &open, None, "down"), Some(TreeKey::Focus(0)));
        assert_eq!(tree_key(&rows, &open, None, "up"), Some(TreeKey::Focus(4)));
        assert_eq!(tree_key(&rows, &open, Some(4), "down"), Some(TreeKey::Focus(4)));
        assert_eq!(tree_key(&rows, &open, Some(0), "right"), Some(TreeKey::Focus(1)));
        assert_eq!(tree_key(&rows, &open, Some(1), "left"), Some(TreeKey::Toggle(1)));
        // A file goes to its folder on Left, stays on Right, and is picked on Enter.
        assert_eq!(tree_key(&rows, &open, Some(2), "left"), Some(TreeKey::Focus(1)));
        assert_eq!(tree_key(&rows, &open, Some(4), "left"), Some(TreeKey::Focus(4)));
        assert_eq!(tree_key(&rows, &open, Some(2), "right"), Some(TreeKey::Focus(2)));
        assert_eq!(tree_key(&rows, &open, Some(3), "enter"), Some(TreeKey::Select(3)));
        assert_eq!(tree_key(&rows, &open, Some(0), "space"), Some(TreeKey::Toggle(0)));
        // A folded folder opens on Right and moves to its parent on Left.
        let folded: HashSet<String> = ["src/app/".to_owned()].into();
        let rows = super::rows(&files, &folded);
        assert_eq!(tree_key(&rows, &folded, Some(1), "right"), Some(TreeKey::Toggle(1)));
        assert_eq!(tree_key(&rows, &folded, Some(1), "left"), Some(TreeKey::Focus(0)));
        assert_eq!(tree_key(&[], &open, None, "down"), None);
        assert_eq!(tree_key(&rows, &open, Some(0), "x"), None);
    }

    #[test]
    fn every_folder_counts_for_collapse_all() {
        let files = [("src/app/a.rs", Status::Added), ("src/b.rs", Status::Added), ("c.rs", Status::Added)];
        assert_eq!(folders(&files), ["src/", "src/app/"]);
    }
}
