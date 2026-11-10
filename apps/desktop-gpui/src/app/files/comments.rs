//! Line comments in the file editor (`EditableFileSurface`, `fileCommentAnnotations.ts`): Comment
//! opens a draft on the editor's selected lines, a saved comment is a review-comment chip in the
//! composer, the gutter marks commented lines, and an edit moves each comment with its lines.
//! The editor draws no blocks between lines, so the comments list under it.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{GutterDecoration, GutterDecorationCollection, Position};
use gpui_kit::component::{Icon, Sizable as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, ElementId, Focusable as _, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::FileKey;
use crate::app::review_comments::{self, CommentDraft};
use crate::app::{Target, Workspace};
use crate::review_comment::{self, KIND};
use crate::theme::Tokens;

/// A comment being written on a file's lines, for the draft it goes to.
pub(super) struct FileDraft {
    key: FileKey,
    target: Target,
    start: u64,
    end: u64,
    draft: CommentDraft,
}

/// A file's gutter marks and what each tracks, in mark order: a held comment's id and lines,
/// or the draft's (`None`).
#[derive(Default)]
pub(super) struct Marks {
    collection: Option<GutterDecorationCollection>,
    tracked: Vec<(Option<String>, u64, u64)>,
}

impl Marks {
    /// The editor's text was replaced wholesale, which moves no comment: the marks redraw from
    /// the comments' lines.
    pub(super) fn reset(&mut self) {
        self.tracked.clear();
    }
}

/// A held comment on a file's lines.
struct FileComment {
    id: String,
    start: u64,
    end: u64,
    text: String,
}

impl Workspace {
    /// The open draft's comments on `key`'s file, top to bottom.
    fn file_comments(&self, key: &FileKey) -> Vec<FileComment> {
        let Some(records) = self.target.as_ref().and_then(|target| self.draft_records(target)) else {
            return Vec::new();
        };
        let mut comments: Vec<FileComment> = records
            .iter()
            .filter(|record| review_comment::in_file(record, &key.path))
            .filter_map(|record| {
                let (start, end) = review_comment::file_lines(record)?;
                let id = record["contextId"].as_str()?.to_owned();
                Some(FileComment { id, start, end, text: record["text"].as_str().unwrap_or_default().to_owned() })
            })
            .collect();
        comments.sort_by_key(|comment| (comment.start, comment.end));
        comments
    }

    /// The draft on `key`'s file, while the composer it goes to is on screen.
    fn file_draft(&self, key: &FileKey) -> Option<&FileDraft> {
        self.files.comment.as_ref().filter(|draft| draft.key == *key && self.target.as_ref() == Some(&draft.target))
    }

    /// Whether the open composer takes a comment on `key`'s file now.
    pub(super) fn can_comment_on_file(&self, key: &FileKey) -> bool {
        self.target.as_ref().is_some_and(|target| self.takes_context(target)) && self.file_draft(key).is_none()
    }

    /// Marks the commented lines in the gutter, redrawing only when the comments changed.
    fn sync_file_comment_marks(&mut self, key: &FileKey, comments: &[FileComment], t: &Tokens, cx: &mut Context<Self>) {
        let draft = self.file_draft(key).map(|draft| (None, draft.start, draft.end));
        let wanted: Vec<_> = comments
            .iter()
            .map(|comment| (Some(comment.id.clone()), comment.start, comment.end))
            .chain(draft)
            .collect();
        let Some(state) = self.files.previews.get_mut(key) else { return };
        let Some(editor) = state.editor.clone() else { return };
        if state.marks.tracked == wanted || (wanted.is_empty() && state.marks.collection.is_none()) {
            return;
        }
        let text = editor.read(cx).text();
        let marks: Vec<GutterDecoration> = wanted
            .iter()
            .map(|(_, start, end)| {
                GutterDecoration::new(review_comment::line_span(text, *start, *end)).with_color(t.primary)
            })
            .collect();
        let collection = state.marks.collection.get_or_insert_with(|| {
            editor.update(cx, |editor, cx| editor.create_gutter_decorations_collection(Vec::new(), cx))
        });
        collection.set(marks, cx);
        state.marks.tracked = wanted;
    }

    /// After an edit: each comment ends on the line its mark now ends on, as tall as it was
    /// (`remapFileCommentAnnotations`), and quotes those lines again.
    pub(super) fn remap_file_comments(&mut self, key: &FileKey, cx: &mut Context<Self>) {
        let Some(state) = self.files.previews.get_mut(key) else { return };
        let (Some(editor), Some(collection)) = (state.editor.clone(), &state.marks.collection) else { return };
        let ranges = collection.get_ranges(cx);
        if ranges.len() != state.marks.tracked.len() {
            return;
        }
        let text = editor.read(cx).text();
        let mut moved = Vec::new();
        for ((id, start, end), range) in state.marks.tracked.iter_mut().zip(ranges) {
            let lines = review_comment::remapped_lines(text, range, *end - *start);
            if lines != (*start, *end) {
                (*start, *end) = lines;
                moved.push((id.clone(), lines));
            }
        }
        let Some(target) = self.target.clone().filter(|_| !moved.is_empty()) else { return };
        let contents = editor.read(cx).value();
        for (id, (start, end)) in moved {
            let Some(id) = id else {
                if let Some(draft) = self.files.comment.as_mut().filter(|draft| draft.key == *key) {
                    (draft.start, draft.end) = (start, end);
                }
                continue;
            };
            let held = self.draft_records(&target).and_then(|records| records.get(&id));
            let Some(comment) = held.and_then(|record| record["text"].as_str()).map(str::to_owned) else { continue };
            let record = review_comment::file_comment(&id, &key.path, start, end, &comment, &contents);
            self.replace_context_record(&target, record, cx);
        }
    }

    /// `beginComment`: a draft on the lines the editor's selection or caret is on.
    pub(super) fn begin_file_comment(&mut self, key: &FileKey, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_comment_on_file(key) {
            return;
        }
        let Some(target) = self.target.clone() else { return };
        let Some(editor) = self.files.previews.get(key).and_then(|state| state.editor.clone()) else { return };
        let (start, end) = {
            let editor = editor.read(cx);
            review_comment::lines_of(editor.text(), editor.selected_range())
        };
        let draft = CommentDraft::new(Self::submit_file_comment, window, cx);
        self.files.comment = Some(FileDraft { key: key.clone(), target, start, end, draft });
        cx.notify();
    }

    /// `submitAnnotationEntry`: the drafted lines as a review-comment chip in the composer.
    fn submit_file_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.files.comment.take() else { return };
        let text = draft.draft.text(cx);
        if text.is_empty() {
            self.files.comment = Some(draft);
            return;
        }
        let editor = self.files.previews.get(&draft.key).and_then(|state| state.editor.clone());
        if let Some(editor) = editor.filter(|_| self.target.as_ref() == Some(&draft.target)) {
            let producer = format!("file-comment-{}", crate::app::new_id());
            let id = crate::composer_context::kind_scoped_id(KIND, &producer);
            let contents = editor.read(cx).value().to_string();
            let record = review_comment::file_comment(&id, &draft.key.path, draft.start, draft.end, &text, &contents);
            self.add_context_record(&draft.target, record, window, cx);
        }
        self.focus_file_editor(&draft.key, window, cx);
    }

    fn cancel_file_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(draft) = self.files.comment.take() {
            self.focus_file_editor(&draft.key, window, cx);
        }
    }

    /// Focus returns to the editor.
    fn focus_file_editor(&mut self, key: &FileKey, window: &mut Window, cx: &mut Context<Self>) {
        cx.notify();
        let Some(editor) = self.files.previews.get(key).and_then(|state| state.editor.clone()) else { return };
        window.focus(&editor.focus_handle(cx), cx);
    }

    /// The subheader's Comment button.
    pub(super) fn render_file_comment_button(&self, key: &FileKey, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let key = key.clone();
        Button::new("file-comment")
            .ghost()
            .xsmall()
            .flex_none()
            .w(px(24.))
            .h(px(24.))
            .rounded(px(6.))
            .text_color(t.muted_foreground)
            .tooltip("Comment on the selected lines")
            .child(Icon::new(Lucide::MessageSquarePlus).with_size(px(14.)))
            .on_click(cx.listener(move |this, _, window, cx| this.begin_file_comment(&key, window, cx)))
            .into_any_element()
    }

    /// The editor, with the file's comments and draft under it and their lines marked.
    pub(super) fn with_file_comments(
        &mut self,
        key: &FileKey,
        editor: AnyElement,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let comments = self.file_comments(key);
        self.sync_file_comment_marks(key, &comments, t, cx);
        let draft = self.file_draft(key).map(|draft| {
            let label = format!("Comment on {}", review_comment::file_range_label(draft.start, draft.end));
            v_flex().child(div().px(px(12.)).pt(px(6.)).text_xs().text_color(t.muted_foreground).child(label)).child(
                review_comments::render_draft(
                    &draft.draft,
                    "file-comment",
                    Self::cancel_file_comment,
                    Self::submit_file_comment,
                    t,
                    cx,
                ),
            )
        });
        if comments.is_empty() && draft.is_none() {
            return editor;
        }
        let Some(target) = self.target.clone() else { return editor };
        let editor_state = self.files.previews.get(key).and_then(|state| state.editor.clone());
        let rule = t.border.opacity(0.3);
        let drafting = draft.is_some();
        let mut list = v_flex()
            .id("file-comments")
            .flex_none()
            .max_h(px(240.))
            .overflow_y_scroll()
            .border_t_1()
            .border_color(t.border)
            .children(draft);
        for (index, comment) in comments.into_iter().enumerate() {
            let label = review_comment::file_range_label(comment.start, comment.end);
            let line = u32::try_from(comment.start.saturating_sub(1)).unwrap_or(u32::MAX);
            let reveal = editor_state.clone().map(|editor| {
                Button::new(ElementId::Name(format!("file-comment-{}-lines", comment.id).into()))
                    .ghost()
                    .xsmall()
                    .self_start()
                    .mx(px(-6.))
                    .text_xs()
                    .text_color(t.muted_foreground)
                    .font_family(t.fonts.mono)
                    .tooltip("Show these lines")
                    .child(label)
                    .on_click(move |_, window, cx| {
                        editor.update(cx, |editor, cx| {
                            editor.set_cursor_position(Position::new(line, 0), window, cx);
                            editor.focus(window, cx);
                        });
                    })
                    .into_any_element()
            });
            let (target, id) = (target.clone(), comment.id.clone());
            let delete = move |this: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                this.remove_context_record(&target, KIND, &id, window, cx);
            };
            let entry = review_comments::render_comment(
                &format!("file-comment-{}", comment.id),
                &comment.text,
                reveal,
                delete,
                t,
                cx,
            );
            let ruled = index > 0 || drafting;
            list = list.child(div().when(ruled, |el| el.border_t_1().border_color(rule)).child(entry));
        }
        v_flex().size_full().min_h_0().child(div().flex_1().min_h_0().child(editor)).child(list).into_any_element()
    }
}
