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
            match draft {
                ComposerDraft::Prompt(_, kept, _) => lists.push((Some(target), kept.as_mut_slice())),
                ComposerDraft::Question(question) => lists.extend(question.lists().map(|list| (Some(target), list))),
            }
        }
        lists
            .into_iter()
            .flat_map(|(target, attachments)| attachments.iter_mut().map(move |attachment| (target, attachment)))
            .find(|(_, attachment)| attachment.local_id() == local_id)
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
            PromptHolder::QueueEdit => {
                self.queue.edit_draft_mut().map(|(prompt, kept, records)| (prompt, kept, records))
            }
            PromptHolder::Question => self.question.as_mut().map(QuestionDraft::prompt_mut),
            PromptHolder::OffScreen => match self.composer_drafts.get_mut(target)? {
                ComposerDraft::Prompt(prompt, kept, records) => Some((prompt, kept, records)),
                ComposerDraft::Question(question) => Some(question.prompt_mut()),
            },
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
            PromptHolder::OffScreen => match self.composer_drafts.get(target)? {
                ComposerDraft::Prompt(prompt, kept, records) => Some((prompt.as_str(), kept.as_slice(), records)),
                ComposerDraft::Question(question) => Some(parked(question)),
            },
        }
    }

    /// Every set-aside prompt of `target`'s that can hold a chip: the editor's content a card
    /// parked, the prompt an edit set aside, or the draft not on screen.
    pub(super) fn set_aside_prompts(&mut self, target: &Target) -> Vec<DraftMut<'_>> {
        if self.target.as_ref() != Some(target) {
            return self.set_aside_prompt(target).into_iter().collect();
        }
        let parked = self.question.as_mut().map(QuestionDraft::prompt_mut);
        let saved = self.queue.edit_draft_mut().map(|(prompt, kept, records)| (prompt, kept, records));
        parked.into_iter().chain(saved).collect()
    }

    /// Where `draft`'s content waits once the editor no longer shows it: the prompt set aside
    /// (see [`Self::set_aside_prompt`]), or the edit's text a question card parked; `None` once
    /// its edit ended.
    pub(super) fn left_draft_mut(&mut self, draft: &EditorDraft) -> Option<DraftMut<'_>> {
        match &draft.edit {
            None => self.set_aside_prompt(&draft.target),
            Some(_) if self.editor_draft().as_ref() == Some(draft) => {
                self.question.as_mut().map(QuestionDraft::prompt_mut)
            }
            Some(_) => None,
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
}
