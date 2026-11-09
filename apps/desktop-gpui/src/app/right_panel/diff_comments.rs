//! Line comments on the Diff surface (`AnnotatableCodeView`): the gutter's add button opens a
//! draft under the picked lines, Comment puts a review-comment chip in the thread's composer, and
//! the comments its draft holds sit under their lines again whenever the selection shows.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::diff::{Diff, DiffAnnotation, DiffLinePosition, DiffLineRange, DiffSide};
use gpui_kit::component::v_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, Focusable as _, IntoElement, ParentElement as _, SharedString, Styled as _, Window,
    div, px,
};

use super::super::review_comments::{self, CommentDraft};
use super::super::{Target, Workspace};
use super::{DiffScope, TurnSummary};
use crate::pull_request_diff::Side;
use crate::review_comment::{self, Point, Section};
use crate::theme::Tokens;

/// A comment being written under the shown selection's lines.
pub(super) struct DiffComment {
    selection: (Target, DiffScope),
    section: (String, String),
    path: SharedString,
    start: Point,
    end: Point,
    draft: CommentDraft,
}

/// The comments under one line, then the draft when it sits there.
#[derive(Default)]
struct Group {
    comments: Vec<(String, String)>,
    draft: bool,
}

fn to_viewer(side: Side) -> DiffSide {
    match side {
        Side::Left => DiffSide::Original,
        Side::Right => DiffSide::Modified,
    }
}

fn from_viewer(side: DiffSide) -> Side {
    match side {
        DiffSide::Original => Side::Left,
        DiffSide::Modified => Side::Right,
    }
}

/// `reviewSectionId` and `reviewSectionTitle`: a turn is titled by its number, a git selection
/// by the menu's label.
pub(super) fn section(scope: &DiffScope, turns: &[TurnSummary], label: &str) -> (String, String) {
    match scope {
        DiffScope::Branch => ("branch".into(), label.into()),
        DiffScope::Unstaged => ("unstaged".into(), label.into()),
        DiffScope::Commit(sha) => (format!("commit:{sha}"), label.into()),
        DiffScope::Turn(run) => {
            let number = turns.iter().find(|turn| &turn.run_id == run).map(|turn| turn.turn_count.to_string());
            (format!("turn:{run}"), format!("Turn {}", number.as_deref().unwrap_or("?")))
        }
    }
}

/// Each annotated line's group, by key, with the line and its file.
type Groups = HashMap<SharedString, (Point, SharedString, Group)>;

/// The group of `path`'s line `at`, keyed by path, side and number.
fn place<'a>(groups: &'a mut Groups, path: &str, at: Point) -> &'a mut Group {
    let key: SharedString = format!("{path}\u{0}{}\u{0}{}", at.0.as_str(), at.1).into();
    &mut groups.entry(key).or_insert_with(|| (at, path.to_owned().into(), Group::default())).2
}

impl Workspace {
    /// The draft of the shown selection, while its file is still in the patch.
    fn shown_diff_comment(&self, cx: &gpui_kit::App) -> Option<&DiffComment> {
        let comment = self.right_panel.comment.as_ref()?;
        let shown = self.right_panel.diff.read(cx).files().iter().any(|file| *file.path() == comment.path);
        (self.right_panel.shown.as_ref() == Some(&comment.selection) && shown).then_some(comment)
    }

    /// Puts the shown selection's comments and draft under their lines, and lets the gutter
    /// begin one while no draft is open.
    pub(super) fn with_diff_comments(
        &self,
        diff: Diff,
        section: (String, String),
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Diff {
        let Some((target, _)) = self.right_panel.shown.clone() else { return diff };
        let Some(records) = self.draft_records(&target) else { return diff };
        let mut groups: Groups = HashMap::new();
        let files = self.right_panel.diff.read(cx).files();
        let mut rows = HashMap::new();
        for record in records.iter().filter(|record| record["sectionId"] == section.0.as_str()) {
            let path = record["filePath"].as_str().unwrap_or_default();
            let Some(file) = files.iter().find(|file| *file.path() == path) else { continue };
            if !review_comment::in_diff(record, &section.0, path) {
                continue;
            }
            let rows = rows.entry(path).or_insert_with(|| review_comment::rows(file));
            let Some((_, end)) = review_comment::diff_range(rows, record) else { continue };
            let (Some(id), Some(text)) = (record["contextId"].as_str(), record["text"].as_str()) else { continue };
            place(&mut groups, path, end).comments.push((id.to_owned(), text.to_owned()));
        }
        let drafting = self.shown_diff_comment(cx).map(|comment| (comment.path.clone(), comment.end));
        if let Some((path, end)) = &drafting {
            place(&mut groups, path, *end).draft = true;
        }
        let annotations: Vec<DiffAnnotation> = groups
            .iter()
            .map(|(key, ((side, line), path, _))| {
                let line = usize::try_from(*line).unwrap_or(usize::MAX);
                DiffAnnotation::line(
                    ElementId::Name(key.clone()),
                    DiffLinePosition::new(path.clone(), to_viewer(*side), line),
                )
            })
            .collect();
        let groups: Rc<HashMap<SharedString, Group>> =
            Rc::new(groups.into_iter().map(|(key, (_, _, group))| (key, group)).collect());
        let (view, tokens) = (cx.entity(), *tokens);
        let diff = diff.annotations(annotations).render_annotation(move |annotation, _, cx| {
            let ElementId::Name(key) = annotation.id() else { return div().into_any_element() };
            let Some(group) = groups.get(key) else { return div().into_any_element() };
            view.update(cx, |this, cx| this.render_diff_comments(&target, group, &tokens, cx))
        });
        let add_view = cx.entity();
        diff.when(drafting.is_none(), |diff| {
            diff.on_add_annotation(move |range, window, cx| {
                let section = section.clone();
                let () = add_view.update(cx, |this, cx| this.begin_diff_comment(range, section, window, cx));
            })
        })
    }

    /// `renderAnnotation`: a line's comments between rules, or a looser column with the draft.
    fn render_diff_comments(&self, target: &Target, group: &Group, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let rule = t.border.opacity(0.3);
        let mut column = v_flex()
            .when(group.draft, |el| el.py(px(4.)))
            .when(!group.draft, |el| el.border_t_1().border_b_1().border_color(rule));
        for (index, (id, text)) in group.comments.iter().enumerate() {
            let (target, id_owned) = (target.clone(), id.clone());
            let delete = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                this.right_panel.diff.update(cx, |diff, cx| diff.set_selected_lines(None, cx));
                this.remove_context_record(&target, review_comment::KIND, &id_owned, window, cx);
            };
            let entry = review_comments::render_comment(&format!("diff-comment-{id}"), text, None, delete, t, cx);
            column = column
                .child(div().when(index > 0 && !group.draft, |el| el.border_t_1().border_color(rule)).child(entry));
        }
        if group.draft
            && let Some(comment) = &self.right_panel.comment
        {
            column = column.child(review_comments::render_draft(
                &comment.draft,
                "diff-comment",
                Self::cancel_diff_comment,
                Self::submit_diff_comment,
                t,
                cx,
            ));
        }
        column.into_any_element()
    }

    /// `beginComment`: a draft under the picked lines, when they are in the patch.
    fn begin_diff_comment(
        &mut self,
        range: &DiffLineRange,
        section: (String, String),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = self.right_panel.shown.clone() else { return };
        if self.shown_diff_comment(cx).is_some() || self.draft_records(&selection.0).is_none() {
            return;
        }
        let path = range.path().clone();
        let to_line = |line: usize| u64::try_from(line).unwrap_or(u64::MAX);
        let start = (from_viewer(range.side()), to_line(range.start()));
        let end = (from_viewer(range.end_side()), to_line(range.end()));
        let files = self.right_panel.diff.read(cx).files();
        let Some(file) = files.iter().find(|file| *file.path() == path) else { return };
        let probe = Section { id: &section.0, title: &section.1 };
        if review_comment::diff_comment("draft", &probe, &path, &review_comment::rows(file), start, end, "").is_none() {
            return;
        }
        let draft = CommentDraft::new(Self::submit_diff_comment, window, cx);
        self.right_panel.comment = Some(DiffComment { selection, section, path, start, end, draft });
        cx.notify();
    }

    /// `submitEntry`: the drafted lines as a review-comment chip in the composer.
    fn submit_diff_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(comment) = self.right_panel.comment.take() else { return };
        let text = comment.draft.text(cx);
        if text.is_empty() {
            self.right_panel.comment = Some(comment);
            return;
        }
        let files = self.right_panel.diff.read(cx).files();
        let record = files.iter().find(|file| *file.path() == comment.path).and_then(|file| {
            let producer = format!("file-comment-{}", super::super::new_id());
            let id = crate::composer_context::kind_scoped_id(review_comment::KIND, &producer);
            let section = Section { id: &comment.section.0, title: &comment.section.1 };
            let rows = review_comment::rows(file);
            review_comment::diff_comment(&id, &section, &comment.path, &rows, comment.start, comment.end, &text)
        });
        if let Some(record) = record.filter(|_| self.right_panel.shown.as_ref() == Some(&comment.selection)) {
            self.add_context_record(&comment.selection.0, record, window, cx);
        }
        self.close_diff_comment(window, cx);
    }

    fn cancel_diff_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.right_panel.comment = None;
        self.close_diff_comment(window, cx);
    }

    /// The selection clears and focus returns to the diff.
    fn close_diff_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let diff = self.right_panel.diff.clone();
        diff.update(cx, |diff, cx| diff.set_selected_lines(None, cx));
        window.focus(&diff.focus_handle(cx), cx);
        cx.notify();
    }
}
