//! Who holds a composer draft. The editor holds a target's own prompt until something takes
//! it: a queued message's edit sets the prompt aside in its saved draft, and a question card
//! parks whatever the editor held (the prompt, or the edit's text) as its prompt. Off screen a
//! draft waits in `composer_drafts` (a side chat's in `SideChats`). Every take, give-back and
//! upload lookup asks here, so none of them mistakes an edit's text for the prompt it set aside.

use std::collections::HashMap;

use gpui_kit::{Context, Window};

use super::attachments::{Attachment, QuestionDraft};
use super::composer::ComposerDraft;
use super::side_chat::ChatPane;
use super::{Target, Workspace};
use crate::composer_context::ContextRecords;

/// A prompt with its attachments and the records its chips name.
pub(super) type Draft = (String, Vec<Attachment>, ContextRecords);
pub(super) type DraftMut<'a> = (&'a mut String, &'a mut Vec<Attachment>, &'a mut ContextRecords);
type DraftRef<'a> = (&'a str, &'a [Attachment], &'a ContextRecords);

/// Where a pane's own prompt waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PromptHolder {
    /// The editor on screen.
    Editor,
    /// Set aside by the queued message being edited.
    QueueEdit,
    /// Parked by the open question card.
    Question,
    /// A draft not on screen.
    OffScreen,
}

impl PromptHolder {
    /// The edit outranks the card: a question asked during an edit parks the edit's text.
    pub(super) fn of(on_screen: bool, editing: bool, asking: bool) -> Self {
        match (on_screen, editing, asking) {
            (false, ..) => Self::OffScreen,
            (true, true, _) => Self::QueueEdit,
            (true, false, true) => Self::Question,
            (true, false, false) => Self::Editor,
        }
    }

    /// Where the editor's content (the prompt, or a queued message's edit) waits: context
    /// records join and are found there, as the web's `composerDraftTarget` is the edit's.
    pub(super) fn of_editor_content(on_screen: bool, asking: bool) -> Self {
        match (on_screen, asking) {
            (false, _) => Self::OffScreen,
            (true, true) => Self::Question,
            (true, false) => Self::Editor,
        }
    }
}

/// What the editor's content belongs to: a target's prompt, or the queued message's edit begun
/// as `edit` over it. Async work for the editor captures it and lands only on the same one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct EditorDraft {
    pub(super) target: Target,
    pub(super) edit: Option<String>,
}

fn parked(question: &QuestionDraft) -> DraftRef<'_> {
    (question.prompt.as_str(), question.prompt_attachments.as_slice(), &question.prompt_records)
}

/// Every list a composer attachment can wait in, borrowed apart so an upload finds its own.
pub(super) struct AttachmentLists<'a> {
    pub(super) editor: &'a mut [Attachment],
    pub(super) question: Option<&'a mut QuestionDraft>,
    pub(super) queue_edit: Option<&'a mut Vec<Attachment>>,
    pub(super) drafts: &'a mut HashMap<Target, ComposerDraft>,
}

impl<'a> AttachmentLists<'a> {
    /// The attachment and, for one in a draft not on screen, that draft's target.
    pub(super) fn find(self, local_id: &str) -> Option<(Option<&'a Target>, &'a mut Attachment)> {
        let mut lists: Vec<(Option<&'a Target>, &'a mut [Attachment])> = vec![(None, self.editor)];
        lists.extend(self.question.into_iter().flat_map(QuestionDraft::lists).map(|list| (None, list)));
        lists.extend(self.queue_edit.map(|list| (None, list.as_mut_slice())));
        for (target, draft) in self.drafts.iter_mut() {
            lists.extend(draft.lists().into_iter().map(|list| (Some(target), list)));
        }
        lists
            .into_iter()
            .flat_map(|(target, attachments)| attachments.iter_mut().map(move |attachment| (target, attachment)))
            .find(|(_, attachment)| attachment.local_id() == local_id)
    }
}

impl ComposerDraft {
    /// The target's own prompt: the one a card parked, or the one a queued message's edit set aside.
    pub(super) fn own_prompt_mut(&mut self) -> DraftMut<'_> {
        match self {
            Self::Prompt(prompt, kept, records) => (prompt, kept, records),
            Self::Question(question) => question.prompt_mut(),
            Self::QueueEdit(edit, _) => edit.saved_mut(),
        }
    }

    pub(super) fn own_prompt(&self) -> DraftRef<'_> {
        match self {
            Self::Prompt(prompt, kept, records) => (prompt.as_str(), kept.as_slice(), records),
            Self::Question(question) => parked(question),
            Self::QueueEdit(edit, _) => {
                let (prompt, kept, records) = edit.saved();
                (prompt.as_str(), kept.as_slice(), records)
            }
        }
    }

    /// What the editor shows once the target is back on screen: a queued message's edit's
    /// composer, else the draft itself.
    pub(super) fn editor_mut(&mut self) -> &mut ComposerDraft {
        match self {
            Self::QueueEdit(_, editor) => editor.editor_mut(),
            draft => draft,
        }
    }

    pub(super) fn editor(&self) -> &ComposerDraft {
        match self {
            Self::QueueEdit(_, editor) => editor.editor(),
            draft => draft,
        }
    }

    /// The content of the queued message's edit begun as `id` that the target was left in: its
    /// prompt, or the edit's text a card parked.
    pub(super) fn edit_content_mut(&mut self, id: &str) -> Option<DraftMut<'_>> {
        match self {
            Self::QueueEdit(edit, editor) if edit.id() == id => Some(editor.own_prompt_mut()),
            _ => None,
        }
    }

    /// What the editor shows once back on screen, as a draft: a queued message's edit left open,
    /// else the target's own prompt (its text a card parked, if one is open).
    pub(super) fn editor_content_mut(&mut self) -> DraftMut<'_> {
        self.editor_mut().own_prompt_mut()
    }

    fn editor_content(&self) -> DraftRef<'_> {
        self.editor().own_prompt()
    }

    /// Every prompt it keeps that can hold a chip: its own, and a queued message's edit left open.
    fn prompts_mut(&mut self) -> Vec<DraftMut<'_>> {
        match self {
            Self::QueueEdit(edit, editor) => vec![edit.saved_mut(), editor.own_prompt_mut()],
            draft => vec![draft.own_prompt_mut()],
        }
    }

    /// Ends a queued message's edit left open, as its cancel does: the editor gets the target's
    /// own prompt back, or the card it showed parks it.
    pub(super) fn end_edit(self) -> ComposerDraft {
        let Self::QueueEdit(edit, editor) = self else { return self };
        match *editor {
            Self::Question(mut question) => {
                question.set_parked(edit.into_saved());
                Self::Question(question)
            }
            _ => {
                let (prompt, kept, records) = edit.into_saved();
                Self::Prompt(prompt, kept, records)
            }
        }
    }

    fn lists(&mut self) -> Vec<&mut [Attachment]> {
        match self {
            Self::Prompt(_, kept, _) => vec![kept.as_mut_slice()],
            Self::Question(question) => question.lists().collect(),
            Self::QueueEdit(edit, editor) => {
                let mut lists = vec![edit.saved_mut().1.as_mut_slice()];
                lists.extend(editor.lists());
                lists
            }
        }
    }

    /// Every attachment it keeps for the prompt, to let their uploads go.
    pub(super) fn into_attachments(self) -> Vec<Attachment> {
        match self {
            Self::Prompt(_, attachments, _) => attachments,
            Self::Question(question) => question.prompt_attachments,
            Self::QueueEdit(edit, editor) => {
                let mut attachments = edit.into_saved().1;
                attachments.extend(editor.into_attachments());
                attachments
            }
        }
    }
}

impl Workspace {
    /// Where `target`'s own prompt waits.
    pub(super) fn prompt_holder(&self, target: &Target) -> PromptHolder {
        PromptHolder::of(self.target.as_ref() == Some(target), self.editing_queued(), self.question.is_some())
    }

    /// Whether the editor holds `target`'s own prompt.
    pub(super) fn composer_holds_prompt(&self, target: &Target) -> bool {
        self.prompt_holder(target) == PromptHolder::Editor
    }

    /// `target`'s prompt, attachments and records when the editor doesn't hold them.
    pub(super) fn set_aside_prompt(&mut self, target: &Target) -> Option<DraftMut<'_>> {
        match self.prompt_holder(target) {
            PromptHolder::Editor => None,
            PromptHolder::QueueEdit => self.queue.edit_draft_mut(),
            PromptHolder::Question => self.question.as_mut().map(QuestionDraft::prompt_mut),
            PromptHolder::OffScreen => self.composer_drafts.get_mut(target).map(ComposerDraft::own_prompt_mut),
        }
    }

    /// [`Self::set_aside_prompt`], to read.
    pub(super) fn set_aside_prompt_ref(&self, target: &Target) -> Option<DraftRef<'_>> {
        match self.prompt_holder(target) {
            PromptHolder::Editor => None,
            PromptHolder::QueueEdit => {
                self.queue.edit_draft().map(|(prompt, kept, records)| (prompt.as_str(), kept.as_slice(), records))
            }
            PromptHolder::Question => self.question.as_ref().map(parked),
            PromptHolder::OffScreen => self.composer_drafts.get(target).map(ComposerDraft::own_prompt),
        }
    }

    /// Where `target`'s editor content waits (see [`PromptHolder::of_editor_content`]).
    pub(super) fn editor_content_holder(&self, target: &Target) -> PromptHolder {
        PromptHolder::of_editor_content(self.target.as_ref() == Some(target), self.question.is_some())
    }

    /// `target`'s editor content when the editor doesn't show it: parked under the question
    /// card, or the draft off screen.
    pub(super) fn parked_editor_content(&mut self, target: &Target) -> Option<DraftMut<'_>> {
        match self.editor_content_holder(target) {
            PromptHolder::Editor | PromptHolder::QueueEdit => None,
            PromptHolder::Question => self.question.as_mut().map(QuestionDraft::prompt_mut),
            PromptHolder::OffScreen => self.composer_drafts.get_mut(target).map(ComposerDraft::editor_content_mut),
        }
    }

    /// [`Self::parked_editor_content`], to read.
    pub(super) fn parked_editor_content_ref(&self, target: &Target) -> Option<DraftRef<'_>> {
        match self.editor_content_holder(target) {
            PromptHolder::Editor | PromptHolder::QueueEdit => None,
            PromptHolder::Question => self.question.as_ref().map(parked),
            PromptHolder::OffScreen => self.composer_drafts.get(target).map(ComposerDraft::editor_content),
        }
    }

    /// Every set-aside prompt of `target`'s that can hold a chip: the editor's content a card
    /// parked, the prompt an edit set aside, or the draft not on screen (with an edit left open).
    pub(super) fn set_aside_prompts(&mut self, target: &Target) -> Vec<DraftMut<'_>> {
        if self.target.as_ref() != Some(target) {
            return self.composer_drafts.get_mut(target).map(ComposerDraft::prompts_mut).unwrap_or_default();
        }
        let parked = self.question.as_mut().map(QuestionDraft::prompt_mut);
        let saved = self.queue.edit_draft_mut();
        parked.into_iter().chain(saved).collect()
    }

    /// Where `draft`'s content waits once the editor no longer shows it: the prompt set aside
    /// (see [`Self::set_aside_prompt`]), the edit's text a question card parked, or the edit left
    /// open in its target's draft; `None` once its edit ended.
    pub(super) fn left_draft_mut(&mut self, draft: &EditorDraft) -> Option<DraftMut<'_>> {
        match &draft.edit {
            None => self.set_aside_prompt(&draft.target),
            Some(_) if self.editor_draft().as_ref() == Some(draft) => {
                self.question.as_mut().map(QuestionDraft::prompt_mut)
            }
            Some(_) if self.target.as_ref() == Some(&draft.target) => None,
            Some(edit) => self.composer_drafts.get_mut(&draft.target)?.edit_content_mut(edit),
        }
    }

    /// What the editor's content belongs to now.
    pub(super) fn editor_draft(&self) -> Option<EditorDraft> {
        let target = self.target.clone()?;
        Some(EditorDraft { target, edit: self.queue.edit_id().map(str::to_owned) })
    }

    pub(super) fn attachment_lists(&mut self) -> AttachmentLists<'_> {
        AttachmentLists {
            editor: &mut self.attachments,
            question: self.question.as_mut(),
            queue_edit: self.queue.edit_draft_mut().map(|(_, kept, _)| kept),
            drafts: &mut self.composer_drafts,
        }
    }

    /// The pane's editor content, wherever it waits: parked under the open question card, which
    /// then shows its answer in the editor, or in the editor. Its text, and whether it has
    /// attachments (side chats take none).
    pub(super) fn editor_content(&self, pane: ChatPane, cx: &gpui_kit::App) -> (String, bool) {
        let main = pane == ChatPane::Main;
        match self.question(pane) {
            Some(question) => (question.prompt.clone(), main && !question.prompt_attachments.is_empty()),
            None => (self.pane_text(pane, cx), main && self.has_attachments()),
        }
    }

    /// Puts `draft` where the pane's editor content waits (see [`Self::editor_content`]).
    pub(super) fn put_editor_content(
        &mut self,
        pane: ChatPane,
        (text, attachments, records): Draft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(question) = self.question_mut(pane) {
            question.set_parked((text, attachments, records));
            return;
        }
        if pane == ChatPane::Main {
            self.attachments = attachments;
            // Ahead of the text, so its chips find them.
            self.context.records = records;
        }
        self.set_pane_text(pane, text, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::super::queue::QueueEdit;
    use super::*;

    #[test]
    fn a_queued_edit_holds_the_prompt_even_under_a_question() {
        use PromptHolder::*;
        let holder = |on_screen, editing, asking| PromptHolder::of(on_screen, editing, asking);
        assert_eq!(holder(true, false, false), Editor);
        assert_eq!(holder(true, false, true), Question);
        assert_eq!(holder(true, true, false), QueueEdit);
        assert_eq!(holder(true, true, true), QueueEdit, "a card asked during an edit parked the edit's text");
        assert_eq!(holder(false, true, true), OffScreen);
    }

    fn records() -> ContextRecords {
        ContextRecords::default()
    }

    /// A thread left mid-edit: its own prompt `own` set aside in the edit `edit`, over `editor`.
    fn left_mid_edit(editor: ComposerDraft) -> ComposerDraft {
        let edit = QueueEdit::test("edit", "e", ("own".into(), Vec::new(), records()));
        ComposerDraft::QueueEdit(Box::new(edit), Box::new(editor))
    }

    #[test]
    fn a_thread_left_mid_edit_keeps_the_edit_and_its_own_prompt() {
        let mut draft = left_mid_edit(ComposerDraft::Prompt("edited".into(), Vec::new(), records()));
        assert_eq!(draft.content(), ("own", 0), "its sidebar row and saved draft are its own prompt");
        assert_eq!(draft.own_prompt_mut().0, "own");
        assert_eq!(draft.edit_content_mut("edit").map(|(text, ..)| text.clone()).as_deref(), Some("edited"));
        assert!(draft.edit_content_mut("a later edit").is_none());
        assert_eq!(draft.editor_content().0, "edited", "a comment added off screen joins the edit");
        let prompts: Vec<String> = draft.prompts_mut().into_iter().map(|(text, ..)| text.clone()).collect();
        assert_eq!(prompts, ["own", "edited"], "an imported chip is found in either");
        let ComposerDraft::Prompt(text, ..) = draft.end_edit() else { panic!("the edit's composer was a prompt") };
        assert_eq!(text, "own", "a save landing while away gives the composer back its own prompt");
    }

    #[test]
    fn a_thread_left_mid_edit_under_a_question_keeps_the_edits_text_parked() {
        let question = |id: &str| -> crate::requests::Question {
            serde_json::from_value(serde_json::json!({"id": id, "header": "H", "question": "?", "options": []}))
                .unwrap()
        };
        let mut card = QuestionDraft::new("r".into(), vec![question("a")]).unwrap();
        card.prompt = "edited".into();
        let mut draft = left_mid_edit(ComposerDraft::Question(card));
        assert_eq!(draft.content(), ("own", 0));
        assert_eq!(draft.edit_content_mut("edit").map(|(text, ..)| text.clone()).as_deref(), Some("edited"));
        let ComposerDraft::Question(card) = draft.end_edit() else { panic!("the card stays") };
        assert_eq!(card.prompt, "own", "the card parks the thread's own prompt once the edit ends");
    }

    #[test]
    fn context_follows_the_editor_content_not_the_prompt_an_edit_set_aside() {
        use PromptHolder::*;
        // During an edit the prompt waits with it, but a comment joins and is found in the edit.
        assert_eq!(PromptHolder::of(true, true, false), QueueEdit);
        assert_eq!(PromptHolder::of_editor_content(true, false), Editor);
        assert_eq!(PromptHolder::of_editor_content(true, true), Question, "the card parked the editor's content");
        assert_eq!(PromptHolder::of_editor_content(false, false), OffScreen);
    }
}
