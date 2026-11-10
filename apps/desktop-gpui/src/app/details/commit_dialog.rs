//! The Commit changes dialog (`GitActionsControl.tsx`): the branch, the changed files to review
//! and pick, and an optional message.

use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, BoxShadow, Context, ElementId, Entity, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, div, hsla, px, rgb,
};
use serde_json::json;

use super::super::git::{self, GitRequest, WorkingTreeFile};
use super::super::titlebar::icon;
use super::super::{Workspace, files};
use crate::theme::{self, Tokens};

/// The files left out of the commit (`excludedFiles`). A path stays excluded after it stops
/// being changed, so the commit then names its files explicitly, as on the web.
#[derive(Debug, Default)]
pub(super) struct FileSelection {
    excluded: HashSet<String>,
}

/// A checkbox's state; the select-all box is mixed while only some files are picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum Check {
    Checked,
    Unchecked,
    Mixed,
}

impl FileSelection {
    fn is_excluded(&self, path: &str) -> bool {
        self.excluded.contains(path)
    }

    fn all_selected(&self) -> bool {
        self.excluded.is_empty()
    }

    fn toggle(&mut self, path: &str) {
        if !self.excluded.remove(path) {
            self.excluded.insert(path.to_owned());
        }
    }

    /// The select-all box: leaves every file out when all are picked, else picks them all.
    fn toggle_all(&mut self, files: &[WorkingTreeFile]) {
        if self.all_selected() {
            self.excluded = files.iter().map(|file| file.path.clone()).collect();
        } else {
            self.excluded.clear();
        }
    }

    fn selected<'a>(&'a self, files: &'a [WorkingTreeFile]) -> impl Iterator<Item = &'a WorkingTreeFile> + 'a {
        files.iter().filter(|file| !self.is_excluded(&file.path))
    }

    fn none_selected(&self, files: &[WorkingTreeFile]) -> bool {
        self.selected(files).next().is_none()
    }

    /// The picked files' insertions and deletions.
    fn totals(&self, files: &[WorkingTreeFile]) -> (u64, u64) {
        self.selected(files).fold((0, 0), |(add, del), file| (add + file.insertions, del + file.deletions))
    }

    /// `filePaths`: sent only when something is left out.
    fn file_paths(&self, files: &[WorkingTreeFile]) -> Option<Vec<String>> {
        (!self.all_selected()).then(|| self.selected(files).map(|file| file.path.clone()).collect())
    }

    fn check_all(&self, files: &[WorkingTreeFile]) -> Check {
        if self.all_selected() {
            Check::Checked
        } else if self.none_selected(files) {
            Check::Unchecked
        } else {
            Check::Mixed
        }
    }
}

/// What the dialog shows of the watched checkout's live status.
struct Snapshot {
    has_status: bool,
    branch: SharedString,
    is_default_ref: bool,
    files: Vec<WorkingTreeFile>,
    root: Option<String>,
    /// `canOperateThread`: opening a file in an editor runs on the environment.
    can_open: bool,
    scheme: git::DiffColorScheme,
}

pub(super) struct CommitDialog {
    workspace: WeakEntity<Workspace>,
    message: Entity<TextareaState>,
    selection: FileSelection,
    /// Edit shows the checkboxes; Done hides them again.
    editing: bool,
    scroll: ScrollHandle,
    _observe: Subscription,
}

impl CommitDialog {
    fn new(workspace: &Entity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message =
            cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 8).placeholder("Leave empty to auto-generate"));
        Self {
            workspace: workspace.downgrade(),
            message,
            selection: FileSelection::default(),
            editing: false,
            scroll: ScrollHandle::new(),
            // The file list follows the live status, as the web's does.
            _observe: cx.observe(workspace, |_, _, cx| cx.notify()),
        }
    }

    fn files<'a>(&self, cx: &'a App) -> &'a [WorkingTreeFile] {
        let Some(workspace) = self.workspace.upgrade() else { return &[] };
        workspace.read(cx).vcs.status.as_ref().map_or(&[], |status| status.local.working_tree.files.as_slice())
    }

    fn none_selected(&self, cx: &App) -> bool {
        self.selection.none_selected(self.files(cx))
    }

    /// The commit to run, or `None` with nothing picked.
    fn request(&self, feature_branch: bool, cx: &App) -> Option<GitRequest> {
        let files = self.files(cx);
        if self.selection.none_selected(files) {
            return None;
        }
        let message = self.message.read(cx).value().trim().to_owned();
        Some(GitRequest {
            commit_message: (!message.is_empty()).then_some(message),
            feature_branch,
            file_paths: self.selection.file_paths(files),
            ..GitRequest::new("commit")
        })
    }

    fn snapshot(&self, cx: &App) -> Option<Snapshot> {
        let workspace = self.workspace.upgrade()?;
        let workspace = workspace.read(cx);
        let status = workspace.vcs.status.as_ref();
        Some(Snapshot {
            has_status: status.is_some(),
            branch: status
                .and_then(|status| status.local.ref_name.clone())
                .unwrap_or_else(|| "(detached HEAD)".into())
                .into(),
            is_default_ref: status.is_some_and(|status| status.local.is_default_ref),
            files: status.map(|status| status.local.working_tree.files.clone()).unwrap_or_default(),
            root: status.and_then(|status| status.local.working_tree.root_path.clone()),
            can_open: workspace.vcs.env.as_deref().is_some_and(|env| workspace.can_operate(env)),
            scheme: git::DiffColorScheme::from_settings(&workspace.client_settings.values),
        })
    }

    fn open_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else { return };
        workspace.update(cx, |this, cx| this.open_changed_file(&path, window, cx));
    }
}

impl Render for CommitDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme::tokens(cx);
        let Some(snapshot) = self.snapshot(cx) else { return div().into_any_element() };
        let files = &snapshot.files;
        let (editing, all_selected) = (self.editing, self.selection.all_selected());
        let addition = git::addition_base_color(&t, snapshot.scheme);
        let deletion = git::deletion_base_color(&t, snapshot.scheme);
        let ring = |dark_alpha: f32| if t.dark { hsla(0., 0., 1., dark_alpha) } else { hsla(0., 0., 0., 0.05) };

        let branch = h_flex().gap_2().child(div().text_color(t.muted_foreground).child("Branch")).child(
            h_flex()
                .flex_1()
                .min_w_0()
                .justify_between()
                .gap_2()
                .child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).child(snapshot.branch.clone()))
                .when(snapshot.is_default_ref, |row| {
                    row.child(div().flex_none().text_color(t.warning).child("Default branch"))
                }),
        );

        let select_all = (editing && !files.is_empty()).then(|| {
            check_box("commit-files-all", self.selection.check_all(files), "Select all files", &t).on_click(
                cx.listener(|this, _, _, cx| {
                    let files = this.files(cx).to_vec();
                    this.selection.toggle_all(&files);
                    cx.notify();
                }),
            )
        });
        let counted = (!all_selected && !editing).then(|| {
            let picked = self.selection.selected(files).count();
            div().text_color(t.muted_foreground).child(format!("({picked} of {})", files.len()))
        });
        let edit = (!files.is_empty()).then(|| {
            Button::new("commit-files-edit").ghost().xsmall().label(if editing { "Done" } else { "Edit" }).on_click(
                cx.listener(|this, _, _, cx| {
                    this.editing = !this.editing;
                    cx.notify();
                }),
            )
        });
        let header = h_flex()
            .justify_between()
            .child(
                h_flex()
                    .gap_2()
                    .children(select_all)
                    .child(div().text_color(t.muted_foreground).child("Files"))
                    .children(counted),
            )
            .children(edit);

        let list = if !snapshot.has_status || files.is_empty() {
            div().font_weight(FontWeight::MEDIUM).child("none").into_any_element()
        } else {
            let can_open = snapshot.can_open && snapshot.root.is_some();
            let rows = files.iter().map(|file| {
                let excluded = self.selection.is_excluded(&file.path);
                let path = file.path.clone();
                let counts = if excluded {
                    div().flex_none().text_color(t.muted_foreground).child("Excluded")
                } else {
                    div()
                        .flex_none()
                        .flex()
                        .child(
                            div().text_color(addition).child(format!("+{}", git::compact_diff_count(file.insertions))),
                        )
                        .child(div().text_color(t.muted_foreground).child(" / "))
                        .child(
                            div().text_color(deletion).child(format!("-{}", git::compact_diff_count(file.deletions))),
                        )
                };
                let open = h_flex()
                    .id(ElementId::Name(format!("commit-file-{path}").into()))
                    .flex_1()
                    .min_w_0()
                    .justify_between()
                    .gap_3()
                    .rounded(px(4.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis_start()
                            .when(excluded, |el| el.text_color(t.muted_foreground))
                            .child(file.path.clone()),
                    )
                    .child(counts)
                    .when(can_open, |el| {
                        let ring = t.ring;
                        el.cursor_pointer()
                            .tab_index(0)
                            .focus_visible(move |style| style.shadow(vec![focus_ring(ring)]))
                            .tooltip({
                                let label: SharedString = format!("Open {path} in editor").into();
                                move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
                                }
                            })
                            .on_click(cx.listener(move |this, _, window, cx| this.open_file(path.clone(), window, cx)))
                    });
                let hover = t.accent.opacity(0.5);
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .hover(move |style| style.bg(hover))
                    .when(editing, |row| {
                        let toggled = file.path.clone();
                        row.child(
                            check_box(
                                ElementId::Name(format!("commit-file-check-{}", file.path).into()),
                                if excluded { Check::Unchecked } else { Check::Checked },
                                format!("Include {}", file.path),
                                &t,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selection.toggle(&toggled);
                                cx.notify();
                            })),
                        )
                    })
                    .child(open)
            });
            let (insertions, deletions) = self.selection.totals(files);
            v_flex()
                .gap_2()
                .child(
                    div()
                        .id("commit-files")
                        .h(px(176.))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .rounded(px(8.))
                        .bg(if t.dark { hsla(0., 0., 1., 0.025) } else { t.card })
                        .border_1()
                        .border_color(ring(0.05))
                        .child(v_flex().gap_1().p_1().children(rows)),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .child(div().text_color(addition).child(format!("+{}", git::compact_diff_count(insertions))))
                        .child(div().text_color(t.muted_foreground).child(" / "))
                        .child(div().text_color(deletion).child(format!("-{}", git::compact_diff_count(deletions)))),
                )
                .into_any_element()
        };

        v_flex()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(t.muted_foreground)
                    .child("Review and confirm your commit. Leave the message blank to auto-generate one."),
            )
            .child(
                v_flex()
                    .gap_3()
                    .p_3()
                    .rounded(px(12.))
                    .text_sm()
                    .bg(if t.dark { hsla(0., 0., 1., 0.035) } else { rgb(0xfcfcfc).into() })
                    .border_1()
                    .border_color(ring(0.05))
                    .child(branch)
                    .child(v_flex().gap_1().child(header).child(div().font_family(t.fonts.mono).child(list))),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Commit message (optional)"))
                    .child(Textarea::new(&self.message)),
            )
            .into_any_element()
    }
}

/// `focus-visible:ring-2`, drawn outside the control.
fn focus_ring(color: Hsla) -> BoxShadow {
    BoxShadow::new(px(0.), px(0.), color).spread_radius(px(2.))
}

/// The web's `Checkbox`: a 16px box, primary when checked or mixed; a keyboard tab stop that
/// Space and Enter toggle.
pub(in crate::app) fn check_box(
    id: impl Into<ElementId>,
    check: Check,
    label: impl Into<SharedString>,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let on = check != Check::Unchecked;
    let ring = t.ring;
    div()
        .id(id.into())
        .flex_none()
        .size(px(16.))
        .rounded(px(4.))
        .border_1()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .focus_visible(move |style| style.shadow(vec![focus_ring(ring)]))
        .map(|el| if on { el.bg(t.primary).border_color(t.primary) } else { el.bg(t.background).border_color(t.input) })
        .map(|el| match check {
            Check::Checked => el.child(icon("icons/check.svg").with_size(px(12.)).text_color(t.primary_foreground)),
            Check::Mixed => el.child(div().w(px(8.)).h(px(2.)).rounded(px(1.)).bg(t.primary_foreground)),
            Check::Unchecked => el,
        })
        .tooltip({
            let label: SharedString = label.into();
            move |window, cx| gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx)
        })
}

impl Workspace {
    /// The commit dialog over the watched checkout's live status.
    pub(super) fn open_commit_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::dialog::{DialogClose, DialogFooter};
        let workspace = cx.entity();
        let body = cx.new(|cx| CommitDialog::new(&workspace, window, cx));
        let focus = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if focus.replace(false) {
                body.read(cx).message.focus_handle(cx).focus(window, cx);
            }
            let (can_write, can_branch) = {
                let this = workspace.read(cx);
                (this.permits_git_action(&GitRequest::new("commit")), this.can_change_thread_branch())
            };
            let none_selected = body.read(cx).none_selected(cx);
            let submit = |feature_branch: bool| {
                let (body, workspace) = (body.clone(), workspace.clone());
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    let Some(request) = body.read(cx).request(feature_branch, cx) else { return };
                    workspace.update(cx, |this, cx| this.run_git_action(request, cx));
                    window.close_dialog(cx);
                }
            };
            dialog
                .title("Commit changes")
                .w(px(512.))
                // Enter belongs to the message; only the buttons commit.
                .on_ok(|_, _, _| false)
                .child(body.clone())
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().trigger(|button| button.outline().small().label("Cancel")))
                        .child(
                            Button::new("commit-new-branch")
                                .outline()
                                .small()
                                .label("Commit on new branch")
                                .disabled(!can_branch || none_selected)
                                .on_click(submit(true)),
                        )
                        .child(
                            Button::new("commit-confirm")
                                .primary()
                                .small()
                                .label("Commit")
                                .disabled(!can_write || none_selected)
                                .on_click(submit(false)),
                        ),
                )
        });
    }

    /// `openChangedFileInEditor`: a changed file, resolved against the status' worktree root, in
    /// the preferred editor.
    fn open_changed_file(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let failure = |message: String| Notification::error(message).title("Unable to open file");
        let root = self.vcs.status.as_ref().and_then(|status| status.local.working_tree.root_path.clone());
        let Some(root) = root else {
            window.push_notification(Notification::error("Editor opening is unavailable."), cx);
            return;
        };
        if !self.vcs.env.as_deref().is_some_and(|env| self.can_operate(env)) {
            return;
        }
        let Some(editor) = self.preferred_editor().map(str::to_owned) else {
            window.push_notification(failure("No available editor can open this file.".into()), cx);
            return;
        };
        let Some(client) = self.vcs_client() else { return };
        let target = files::absolute_path(&root, path);
        cx.spawn_in(window, async move |_, cx| {
            if let Err(error) = client.request("shell.openInEditor", json!({"cwd": target, "editor": editor})).await {
                let _ = cx.update(|window, cx| window.push_notification(failure(error.message()), cx));
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, insertions: u64, deletions: u64) -> WorkingTreeFile {
        WorkingTreeFile { path: path.into(), insertions, deletions }
    }

    #[test]
    fn every_file_is_committed_until_one_is_left_out() {
        let files = [file("src/a.rs", 3, 1), file("src/b.rs", 5, 0), file("README.md", 0, 2)];
        let mut selection = FileSelection::default();
        assert_eq!(selection.file_paths(&files), None, "all picked sends no filePaths");
        assert_eq!(selection.totals(&files), (8, 3));
        assert_eq!(selection.check_all(&files), Check::Checked);

        selection.toggle("src/b.rs");
        assert_eq!(selection.file_paths(&files), Some(vec!["src/a.rs".to_owned(), "README.md".to_owned()]));
        assert_eq!(selection.totals(&files), (3, 3));
        assert_eq!(selection.check_all(&files), Check::Mixed);

        selection.toggle("src/b.rs");
        assert_eq!(selection.file_paths(&files), None);
    }

    #[test]
    fn select_all_clears_a_full_selection_and_fills_a_partial_one() {
        let files = [file("a", 1, 0), file("b", 1, 0)];
        let mut selection = FileSelection::default();
        selection.toggle_all(&files);
        assert!(selection.none_selected(&files), "an empty selection disables Commit");
        assert_eq!(selection.check_all(&files), Check::Unchecked);
        assert_eq!(selection.file_paths(&files), Some(Vec::new()));

        selection.toggle("a");
        assert_eq!(selection.check_all(&files), Check::Mixed);
        selection.toggle_all(&files);
        assert_eq!(selection.check_all(&files), Check::Checked);
        assert_eq!(selection.file_paths(&files), None);
    }

    #[test]
    fn a_left_out_file_that_stops_changing_keeps_the_selection_explicit() {
        let mut selection = FileSelection::default();
        selection.toggle("gone.rs");
        let files = [file("a", 1, 0)];
        assert_eq!(selection.file_paths(&files), Some(vec!["a".to_owned()]));
        assert!(!selection.none_selected(&files));
    }
}
