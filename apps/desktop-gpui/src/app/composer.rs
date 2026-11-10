//! The composer (`ChatComposer.tsx`, `ComposerSurface.tsx`), the pending
//! approval rows above it (`TimelinePendingApprovalRow.tsx`) and the question
//! banner attached to it (`ComposerPendingUserInputPanel.tsx`).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InlineToken, InlineTokenContext, InputContent, Textarea};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, BoxShadow, ClickEvent, Context, ElementId, Focusable as _, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, Keystroke, ModifiersChangedEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, px, rgb, svg,
};
use serde_json::{Value, json};

use super::attachments::{Attachment, QuestionDraft};
use super::composer_fit::{ControlsWidths, ModeFit, resolve_mode_fit};
use super::context_strip::PermissionsForm;
use super::queue::QueueEdit;
use super::send::Intent;
use super::side_chat::ChatPane;
use super::{Target, Workspace, new_id};
use crate::composer_context::ContextRecords;
use crate::composer_text::{self, Chip};
use crate::model::{ModelSelection, RuntimeRequest, TurnItem};
use crate::new_thread::UsageFallback;
use crate::requests::{self, Attached, Question};
use crate::theme::{Tokens, tokens};

/// `max-h-52` (208px) over the 22.75px line.
pub(super) const EDITOR_MAX_ROWS: usize = 9;
const LINE_HEIGHT: f32 = 22.75;
/// `leading-relaxed` over the Prompt font size (`--font-size-prompt`).
const PROMPT_LEADING: f32 = 1.625;
const DEFAULT_RUNTIME_MODE: &str = "full-access";
const DEFAULT_PLACEHOLDER: &str = "Ask anything. @ files, $ skills, / commands";
/// Single-select picks advance after this, so the pick is seen first.
const AUTO_ADVANCE: Duration = Duration::from_millis(200);

/// `runtimeModeConfig.ts`.
pub(super) fn runtime_modes() -> [(&'static str, &'static str, Lucide); 4] {
    [
        ("approval-required", "Supervised", Lucide::Lock),
        ("auto-accept-edits", "Auto-accept edits", Lucide::PenLine),
        ("auto", "Auto", Lucide::Sparkles),
        ("full-access", "Full access", Lucide::ShieldOff),
    ]
}

/// The editor's scrollbar, drawn only once the text overflows the 208px cap.
pub(super) fn editor_scrollbar() -> gpui_kit::base::input::EditorScrollbarRenderer {
    use gpui_kit::base::ScrollbarHandle as _;
    Rc::new(|context, _, _| {
        if context.content_size().height <= context.viewport_bounds().size.height + px(0.5) {
            return div().into_any_element();
        }
        gpui_kit::base::Scrollbar::vertical(&context).into_any_element()
    })
}

/// What the footer controls' natural widths depend on: the picker's text, the mode block's
/// labels, and the font they are shaped in.
#[derive(Debug, PartialEq)]
pub(super) struct FooterText {
    picker: super::model_picker::TriggerText,
    chip: Option<&'static str>,
    font: gpui_kit::Font,
    size: gpui_kit::Pixels,
    scale: f32,
}

impl FooterText {
    fn widths(&self, window: &Window) -> ControlsWidths {
        let measure = |text: &str| {
            let run = gpui_kit::TextRun {
                len: text.len(),
                font: self.font.clone(),
                color: Hsla::default(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(window.text_system().shape_line(text.to_owned().into(), self.size, &[run], None).width)
        };
        // The chip: `ps-1.5`/8px sides, 1px borders, a 16px icon and a 6px gap.
        let (block, block_icon_only) = self.chip.map_or((0., 0.), |label| (32. + 6. + measure(label), 32.));
        ControlsWidths { picker: self.picker.natural_width(measure), block, block_icon_only, overflow: 34., gap: 2. }
    }
}

/// Which composer chips a catch-up may change; a later kind covers every earlier one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ChipSync {
    /// After an undo or redo: only the chips it brought back for a skill or model that is gone.
    Restored,
    /// The skills or models chips resolve against changed (`skillChipReplacements`).
    Lists,
    /// Rich text turned on or off: every chip is rebuilt from the text, as the web's remount does.
    Remount,
}

/// The catch-up to run now: `sync`, or the wider one still waiting in `pending`.
fn take_chip_sync(pending: &mut Option<ChipSync>, sync: ChipSync) -> ChipSync {
    pending.take().map_or(sync, |waiting| waiting.max(sync))
}

/// Whether the editor took a catch-up. One it refused during an IME composition waits in
/// `pending` for the change that ends the composition.
fn settle_chip_sync(
    pending: &mut Option<ChipSync>,
    sync: ChipSync,
    result: Result<(), gpui_kit::base::input::InlineTokenError>,
) -> bool {
    match result {
        Ok(()) => true,
        Err(gpui_kit::base::input::InlineTokenError::CompositionActive) => {
            *pending = Some(sync);
            false
        }
        Err(error) => {
            log::warn!("composer chips not reconciled: {error:?}");
            false
        }
    }
}

/// Records `now` as the provider chips resolve against; whether it differs from the one before.
fn note_provider(seen: &mut Option<String>, now: Option<&str>) -> bool {
    if seen.as_deref() == now {
        return false;
    }
    *seen = now.map(str::to_owned);
    true
}

/// Whether `now` holds a chip `before` did not, counting each occurrence of an id.
fn adds_chip(before: &[String], now: &[String]) -> bool {
    let count = |ids: &[String], id: &String| ids.iter().filter(|other| *other == id).count();
    now.iter().any(|id| count(now, id) > count(before, id))
}

/// A failed message first, then whatever was typed after it.
pub(super) fn merge_prompt(failed: String, newer: &str) -> String {
    if newer.trim().is_empty() { failed } else { format!("{failed}\n\n{newer}") }
}

/// What a target keeps of the composer while another one is on screen, like
/// the web's `composerDraftStore`; [`super::draft_store`] keeps it across restarts.
pub(super) enum ComposerDraft {
    /// The prompt, its attachments and the context records its chips name.
    Prompt(String, Vec<Attachment>, ContextRecords),
    /// An open question card; its prompt is the one it set aside.
    Question(QuestionDraft),
    /// A queued message's edit left open: the composer as it was, while the target's own prompt
    /// waits in the edit.
    QueueEdit(Box<QueueEdit>, Box<ComposerDraft>),
}

impl ComposerDraft {
    pub(super) fn empty() -> Self {
        ComposerDraft::Prompt(String::new(), Vec::new(), ContextRecords::default())
    }

    /// A prompt with nothing in it, which needs no keeping.
    pub(super) fn is_empty(&self) -> bool {
        matches!(self, Self::Prompt(prompt, attachments, _) if prompt.is_empty() && attachments.is_empty())
    }

    /// Its own prompt and attachment count.
    pub(super) fn content(&self) -> (&str, usize) {
        let (prompt, attachments, _) = self.own_prompt();
        (prompt, attachments.len())
    }

    /// `composerDraftHasUserContent`.
    pub(super) fn has_content(&self) -> bool {
        let (prompt, attachments) = self.content();
        !prompt.trim().is_empty() || attachments > 0
    }
}

/// A target's model and permissions picks while another one is on screen.
pub(super) struct ComposerSelection {
    pub(super) model: Option<ModelSelection>,
    pub(super) explicit: bool,
    pub(super) runtime_mode: Option<String>,
}

impl Workspace {
    /// A user edit. While a question is open the editor holds its custom answer;
    /// programmatic changes (`set_composer_text`) never reach the draft.
    pub(super) fn on_composer_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.typing_guard.typed(std::time::Instant::now());
        self.schedule_typing_idle(window, cx);
        self.close_stash_menu(cx);
        if self.composer_ui.length_error.is_some() {
            let error = composer_text::prompt_length_error(&self.composer.read(cx).value());
            if error != self.composer_ui.length_error {
                self.composer_ui.length_error = error;
                cx.notify();
            }
        }
        self.save_custom_answer(ChatPane::Main, cx);
        self.sync_composer_empty(cx);
        self.drop_orphan_citation_comment(cx);
        self.reveal_branch_banner();
        let state = self.composer.read(cx);
        if self.prompt_history.as_ref().is_some_and(|h| *h.recalled != *state.value()) {
            self.prompt_history = None;
        }
        let chips: Vec<String> = state.tokens().iter().map(|s| s.token().id().to_string()).collect();
        // A chip an undo or redo brings back may name a skill or model that has gone since.
        let restored = adds_chip(&self.chip_ids, &chips);
        self.chip_ids = chips;
        let references = composer_text::context_reference_ids(&state.value());
        let previous = std::mem::replace(&mut self.context_refs, references);
        self.sync_attachment_chips(&previous, cx);
        self.sync_context_chips(cx);
        // A catch-up refused while an IME composition was active runs once a change ends it.
        if restored || self.chip_sync_pending.is_some() {
            self.reconcile_composer_chips(ChipSync::Restored, cx);
        }
        self.sync_trigger(cx);
        self.sync_composer_marks(cx);
        self.drafts_changed(cx);
    }

    pub(super) fn sync_composer_empty(&mut self, cx: &mut Context<Self>) {
        let empty = self.composer.read(cx).text().len() == 0;
        let triage = !empty && super::palette_actions::is_triage_prompt(&self.composer.read(cx).value());
        let ultrathink = if empty {
            Default::default()
        } else {
            crate::model_label::prompt_ultrathink(&self.composer.read(cx).value())
        };
        if empty != self.composer_empty || triage != self.composer_triage || ultrathink != self.composer_ultrathink {
            self.composer_empty = empty;
            self.composer_triage = triage;
            self.composer_ultrathink = ultrathink;
            cx.notify();
        }
    }

    /// `composerDraftHasUserContent`: the open composer holds text or attachments.
    pub(super) fn composer_has_user_content(&self, cx: &gpui_kit::App) -> bool {
        !self.composer.read(cx).value().trim().is_empty() || self.has_attachments()
    }

    /// `set_value` emits no `Change`, so this keeps `composer_empty` in step.
    /// File links, known skills and models, and context links in `text` come back as chips.
    pub(super) fn set_composer_text(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text: SharedString = text.into();
        let skills = self.skill_names();
        let mut content = InputContent::new(text.clone());
        let chips = composer_text::chips_with_models(
            &text,
            self.client_settings.values.bool("composerRichTextEnabled"),
            |name| skills.iter().any(|s| s == name),
            |instance_id, model| self.model_mention(instance_id, model).map(|m| m.label),
        );
        for (range, chip) in chips {
            let token = InlineToken::new(chip.id(), &text[range.clone()]).with_label(chip.label().to_owned());
            content = match content.clone().with_token(range, token) {
                Ok(next) => next,
                Err(_) => content,
            };
        }
        self.chip_ids = content.tokens().iter().map(|s| s.token().id().to_string()).collect();
        self.context_refs = composer_text::context_reference_ids(&text);
        self.chip_sync_pending = None;
        // `set_value` clears the undo history that could bring a deleted chip back.
        self.release_retained_attachments(cx);
        self.reset_context_chips(&text);
        self.composer.update(cx, |composer, cx| composer.set_value(content, window, cx));
        self.sync_composer_empty(cx);
        self.drop_orphan_citation_comment(cx);
        self.sync_trigger(cx);
        self.sync_composer_marks(cx);
        self.drafts_changed(cx);
    }

    /// The skills or models chips resolve against may have changed: the composer's chips and
    /// the messages that name one catch up.
    pub(super) fn chip_lists_changed(&mut self, cx: &mut Context<Self>) {
        let provider = self.composer_provider().map(|p| p.instance_id.clone());
        note_provider(&mut self.chip_provider, provider.as_deref());
        self.reconcile_composer_chips(ChipSync::Lists, cx);
        self.markdown.chips_changed();
        cx.notify();
    }

    /// The composer's provider, whose skills chips name, changed from outside a pick (another
    /// client's model change on the open thread): chips catch up. A model change within the
    /// provider leaves them, since a model chip names any provider's model.
    pub(super) fn sync_chip_provider(&mut self, cx: &mut Context<Self>) {
        let provider = self.composer_provider().map(|p| p.instance_id.clone());
        if note_provider(&mut self.chip_provider, provider.as_deref()) {
            self.refresh_menu_items(cx);
            self.chip_lists_changed(cx);
        }
    }

    /// Brings the composer's chips in line with the text and the lists it knows, outside the undo
    /// history and keeping the selection; `sync` says which chips may change. Attachments keep
    /// their records: like the web's, a send reads their references from the text.
    pub(super) fn reconcile_composer_chips(&mut self, sync: ChipSync, cx: &mut Context<Self>) {
        let sync = take_chip_sync(&mut self.chip_sync_pending, sync);
        let state = self.composer.read(cx);
        let (text, spans) = (state.value(), state.tokens().to_vec());
        if sync != ChipSync::Remount
            && spans.is_empty()
            && !text.contains(composer_text::is_currency)
            && !text.contains("model:")
        {
            return;
        }
        let skills = self.skill_names();
        let tokens: Vec<_> =
            spans.iter().map(|s| (s.range(), s.token().id().as_ref(), s.token().label().as_ref())).collect();
        let rich = self.client_settings.values.bool("composerRichTextEnabled");
        let is_skill = |name: &str| skills.iter().any(|s| s == name);
        let model_label = |instance_id: &str, model: &str| self.model_mention(instance_id, model).map(|m| m.label);
        let (stale, fresh) = match sync {
            ChipSync::Restored => composer_text::reconcile_restored_chips(&text, rich, &tokens, is_skill, model_label),
            ChipSync::Lists => composer_text::reconcile_chips(&text, rich, &tokens, is_skill, model_label),
            ChipSync::Remount => composer_text::remount_chips(&text, rich, &tokens, is_skill, model_label),
        };
        if stale.is_empty() && fresh.is_empty() {
            return;
        }
        let mut next: Vec<_> = spans
            .iter()
            .filter(|span| !stale.contains(&span.range()))
            .map(|span| (span.range(), span.token().clone()))
            .chain(fresh.into_iter().map(|(range, chip)| {
                let token = InlineToken::new(chip.id(), &text[range.clone()]).with_label(chip.label().to_owned());
                (range, token)
            }))
            .collect();
        next.sort_by_key(|(range, _)| range.start);
        let mut content = InputContent::new(text);
        for (range, token) in next {
            content = match content.clone().with_token(range, token) {
                Ok(next) => next,
                Err(_) => content,
            };
        }
        let result = self.composer.update(cx, |composer, cx| composer.retokenize(content, cx));
        if settle_chip_sync(&mut self.chip_sync_pending, sync, result) {
            let state = self.composer.read(cx);
            self.chip_ids = state.tokens().iter().map(|s| s.token().id().to_string()).collect();
            self.sync_composer_marks(cx);
        }
    }

    /// Pasted file links, known skills and models, and context links arrive as chips (`handlePaste`);
    /// `false` leaves a paste without any, or one into code, to the editor.
    pub(super) fn paste_chips(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let state = self.composer.read(cx);
        let (value, selection) = (state.value(), state.selected_range());
        let rich = self.client_settings.values.bool("composerRichTextEnabled");
        if rich && crate::composer_edit::in_code_block(&value, selection.clone()) {
            return false;
        }
        let after_space = value[..selection.start].chars().next_back().is_none_or(char::is_whitespace);
        let skills = self.skill_names();
        let Some((text, found)) = composer_text::pasted_chips(
            text,
            after_space,
            rich,
            |name| skills.iter().any(|s| s == name),
            |instance_id, model| self.model_mention(instance_id, model).map(|m| m.label),
        ) else {
            return false;
        };
        let at = selection.start;
        self.composer.update(cx, |state, cx| {
            state.replace(text.clone(), window, cx);
            // Same-length swaps, back to front, so every range stays put.
            for (range, chip) in found.into_iter().rev() {
                let token = InlineToken::new(chip.id(), &text[range.clone()]).with_label(chip.label().to_owned());
                let _ = state.replace_range_with_token(at + range.start..at + range.end, token, window, cx);
            }
            let end = at + text.len();
            state.set_selected_range(end..end, cx);
        });
        true
    }

    /// The model a `model:` reference names, as its chip shows it (`modelMentionPresentationAtom`):
    /// any provider of this environment that offers it, ready or not.
    pub(super) fn model_mention(&self, instance_id: &str, model: &str) -> Option<ModelMention> {
        let provider = self.server_info().provider(instance_id)?;
        let option = provider.models.iter().find(|m| m.slug == model)?;
        Some(ModelMention {
            label: crate::model_label::display_model_name(option),
            provider_label: (option.sub_provider.clone())
                .unwrap_or_else(|| crate::model_label::provider_name(provider).to_owned()),
            driver: provider.driver.clone(),
        })
    }

    /// The user-invocable skills of the composer's provider, by name.
    fn skill_names(&self) -> Vec<String> {
        let Some(provider) = self.composer_provider() else {
            return Vec::new();
        };
        let cwd = self.composer_cwd();
        let (skills, _) = crate::composer_search::provider_lists(provider, cwd.as_deref());
        crate::composer_search::invocable(skills).into_iter().map(|s| s.name.clone()).collect()
    }

    /// Keeps the leaving target's composer; call before the target changes.
    pub(super) fn stash_composer(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        if self.model_override.is_some() || self.runtime_mode_override.is_some() {
            let selection = ComposerSelection {
                model: self.model_override.clone(),
                explicit: self.model_explicit,
                runtime_mode: self.runtime_mode_override.clone(),
            };
            self.composer_selections.insert(target.clone(), selection);
        } else {
            self.composer_selections.remove(&target);
        }
        let attachments = std::mem::take(&mut self.attachments);
        let records = self.take_context_records();
        let commented = self.stash_citation_comment(cx);
        let draft = match self.question.take() {
            Some(mut question) => {
                question.park(attachments);
                ComposerDraft::Question(question)
            }
            None => ComposerDraft::Prompt(
                commented.unwrap_or_else(|| self.composer.read(cx).value().to_string()),
                attachments,
                records,
            ),
        };
        // A queued message's edit stays open with the composer it holds, the thread's own draft in it.
        let draft = match self.queue.park_edit() {
            Some(edit) => ComposerDraft::QueueEdit(Box::new(edit), Box::new(draft)),
            None => draft,
        };
        self.prompt_history = None;
        self.menu = Default::default();
        self.composer_ui.rich.picker = None;
        self.prompt_stash.open = false;
        self.picker.dismiss();
        if draft.is_empty() {
            self.composer_drafts.remove(&target);
        } else {
            self.composer_drafts.insert(target, draft);
        }
    }

    /// Returns a failed send's text to the target it came from, ahead of anything
    /// typed there since, so it never lands in another thread or gets dropped.
    pub(super) fn give_back_prompt(
        &mut self,
        origin: &Target,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drafts_changed(cx);
        if self.composer_holds_prompt(origin) {
            let merged = merge_prompt(text, &self.composer.read(cx).value());
            self.set_composer_text(merged, window, cx);
            return;
        }
        if self.target.as_ref() != Some(origin) {
            self.composer_drafts.entry(origin.clone()).or_insert_with(ComposerDraft::empty);
        }
        if let Some((prompt, ..)) = self.set_aside_prompt(origin) {
            *prompt = merge_prompt(text, prompt);
        }
    }

    /// A failed send's attachments, back with `origin`'s prompt wherever it waits: the composer,
    /// or set aside under an open question or in a draft not on screen.
    pub(super) fn give_back_attachments(
        &mut self,
        origin: &Target,
        attachments: Vec<Attachment>,
        cx: &mut Context<Self>,
    ) {
        if attachments.is_empty() {
            return;
        }
        if self.composer_holds_prompt(origin) {
            self.attachments.extend(attachments);
        } else {
            if self.target.as_ref() != Some(origin) {
                self.composer_drafts.entry(origin.clone()).or_insert_with(ComposerDraft::empty);
            }
            if let Some((_, kept, _)) = self.set_aside_prompt(origin) {
                kept.extend(attachments);
            }
        }
        self.drafts_changed(cx);
    }

    /// Brings back the new target's composer; call after the target changes.
    /// `sync_question` drops a restored question once the thread shows it resolved.
    pub(super) fn restore_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_draft_workspace(cx);
        // The question coming back is not the one a pick on another target was advancing.
        self.composer_ui.question_advance = None;
        self.typing_guard.reset();
        self._typing_idle = None;
        self.sync_typing_guard(window, cx);
        let draft = self.target.as_ref().and_then(|target| self.composer_drafts.remove(target));
        self.restore_editor(draft, window, cx);
    }

    /// Puts a draft that was set aside back in the editor.
    fn restore_editor(&mut self, draft: Option<ComposerDraft>, window: &mut Window, cx: &mut Context<Self>) {
        match draft {
            Some(ComposerDraft::QueueEdit(edit, editor)) => {
                self.queue.resume_edit(*edit);
                self.restore_editor(Some(*editor), window, cx);
            }
            Some(ComposerDraft::Question(mut question)) => {
                self.attachments = question.unpark();
                self.question = Some(question);
                self.load_custom_answer(ChatPane::Main, window, cx);
            }
            Some(ComposerDraft::Prompt(prompt, attachments, records)) => {
                self.question = None;
                self.attachments = attachments;
                self.context.records = records;
                self.set_composer_text(prompt, window, cx);
                self.start_restored_uploads(cx);
            }
            None => {
                self.question = None;
                self.context.records = ContextRecords::default();
                self.set_composer_text("", window, cx);
            }
        }
    }

    /// Keys the composer's open menus take ahead of the keymap (the editor binds
    /// arrows, Tab and Enter itself); `true` consumes it.
    pub(super) fn composer_menu_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // The permissions popover nests in the model picker's footer, so it takes the keys first.
        if self.permissions_key(keystroke, window, cx) {
            return true;
        }
        if self.model_picker_open() {
            return self.picker_key(keystroke, window, cx);
        }
        self.stash_menu_key(keystroke, window, cx)
            || self.draft_workspace_key(keystroke, window, cx)
            || (self.composer.focus_handle(cx).is_focused(window)
                && (self.model_chip_key(keystroke, window, cx)
                    || self.code_key(keystroke, window, cx)
                    || self.block_backspace_key(keystroke, window, cx)
                    || self.list_outdent_key(keystroke, window, cx)
                    || self.menu_key(keystroke, window, cx)
                    || self.prompt_suggestion_key(keystroke, window, cx)))
    }

    /// Enter on a selected model chip opens its details, as the web chip's button does.
    fn model_chip_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if keystroke.key != "enter" || keystroke.modifiers.modified() {
            return false;
        }
        let state = self.composer.read(cx);
        let selection = state.selected_range();
        let Some(Chip::Model { instance_id, model, .. }) = selected_chip(state.tokens(), &selection) else {
            return false;
        };
        let Some(bounds) = state.range_to_bounds(&selection) else { return false };
        self.open_model_menu(&instance_id, &model, None, model_chip_menu_at(bounds), window, cx);
        true
    }

    /// Keys left after the keymap: 1-9 on a question card, prompt history, then a plain send.
    pub(super) fn composer_key(
        &mut self,
        keystroke: &Keystroke,
        context: &super::keymap::ShortcutContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.model_picker_open() {
            return false;
        }
        if self.redirect_to_composer(keystroke, window, cx) {
            return true;
        }
        self.question_option_key(keystroke, window, cx)
            || (context.composer_focus
                && (self.history_key(keystroke, window, cx)
                    || self.send_key(keystroke, context, window, cx)
                    || self.composer_list_key(keystroke, window, cx)))
    }

    /// The rich editor's code-block keys, ahead of menus and sending (`ComposerPromptEditorTiptap`):
    /// Enter writes a line of code or leaves the block, Tab and Shift+Tab indent.
    fn code_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = keystroke.modifiers;
        if m.control || m.platform || self.question.is_some() {
            return false;
        }
        if !self.client_settings.values.bool("composerRichTextEnabled") {
            return false;
        }
        let state = self.composer.read(cx);
        let (text, selection) = (state.value(), state.selected_range());
        let edit = match keystroke.key.as_str() {
            "enter" => crate::composer_edit::code_enter(&text, selection),
            "tab" => crate::composer_edit::code_tab(&text, selection, m.shift),
            _ => None,
        };
        edit.is_some_and(|edit| self.apply_composer_edit(edit, window, cx))
    }

    /// Shift+Tab lifts a nested list item before it switches Build and Plan (`composerCommandKey.ts`).
    fn list_outdent_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = keystroke.modifiers;
        if keystroke.key != "tab" || !m.shift || m.control || m.alt || m.platform {
            return false;
        }
        self.list_edit(crate::composer_edit::list_outdent, window, cx)
    }

    /// Enter that doesn't send continues a list; Tab nests its item (`applyListEdit`).
    fn composer_list_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = keystroke.modifiers;
        if m.control || m.platform {
            return false;
        }
        match keystroke.key.as_str() {
            "enter" => self.list_edit(crate::composer_edit::list_enter, window, cx),
            "tab" if !m.shift && !m.alt => self.list_edit(crate::composer_edit::list_indent, window, cx),
            _ => false,
        }
    }

    fn list_edit(
        &mut self,
        edit: fn(&str, usize, &[std::ops::Range<usize>]) -> Option<crate::composer_edit::Edit>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.question.is_some() {
            return false;
        }
        let state = self.composer.read(cx);
        let selection = state.selected_range();
        if !selection.is_empty() {
            return false;
        }
        let tokens: Vec<_> = state.tokens().iter().map(gpui_kit::base::input::InlineTokenSpan::range).collect();
        let edit = edit(&state.value(), selection.start, &tokens);
        edit.is_some_and(|edit| self.apply_composer_edit(edit, window, cx))
    }

    pub(super) fn apply_composer_edit(
        &mut self,
        edit: crate::composer_edit::Edit,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.composer.update(cx, |state, cx| {
            if edit.is_noop() {
                state.set_selected_range(edit.select, cx);
            } else {
                state.replace_ranges(&edit.changes, edit.select, window, cx);
            }
        });
        true
    }

    /// `shouldTypeToFocusComposer` and `usePasteToFocusComposer`: with nothing focused (a
    /// click on the timeline blurs the editor), typing and pasting land at the composer's end.
    fn redirect_to_composer(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.focus.is_focused(window)
            || self.target.is_none()
            || self.palette.is_some()
            || self.page.is_some()
            || self.settings.is_some()
        {
            return false;
        }
        let m = keystroke.modifiers;
        let paste = keystroke.key == "v" && (m.control || m.platform) && !m.alt;
        let text = if paste {
            let item = cx.read_from_clipboard();
            let only_text = item
                .as_ref()
                .is_some_and(|item| item.entries().iter().all(|e| matches!(e, gpui_kit::ClipboardEntry::String(_))));
            match item.and_then(|item| item.text()).filter(|text| only_text && !text.is_empty()) {
                Some(text) => text,
                None => return false,
            }
        } else if !m.control && !m.alt && !m.platform && !m.function {
            match keystroke.key_char.as_ref().filter(|c| c.chars().count() == 1 && !c.chars().any(char::is_control)) {
                Some(c) => c.clone(),
                None => return false,
            }
        } else {
            return false;
        };
        self.composer.update(cx, |state, cx| {
            let end = state.value().len();
            state.set_selected_range(end..end, cx);
        });
        self.composer.read(cx).focus_handle(cx).focus(window, cx);
        // Ctrl+Shift+V pastes as text even when it is long (`isPasteAsTextShortcut`).
        if paste && !m.shift && self.fold_pasted_text(&text, window, cx) {
            return true;
        }
        self.composer.update(cx, |state, cx| state.insert(text, window, cx));
        true
    }

    /// Up/Down recall this thread's earlier messages (`composerCommandKey.ts`).
    fn history_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let back = match keystroke.key.as_str() {
            "up" => true,
            "down" => false,
            _ => return false,
        };
        let m = keystroke.modifiers;
        if m.shift || m.alt || m.control || m.platform || self.question.is_some() || self.has_attachments() {
            return false;
        }
        let state = self.composer.read(cx);
        let text = state.value();
        let cursor = state.cursor().min(text.len());
        let on_edge = if back { !text[..cursor].contains('\n') } else { !text[cursor..].contains('\n') };
        if !on_edge || (self.prompt_history.is_none() && !text.is_empty()) {
            return false;
        }
        let entries = self.history_entries();
        let Some((prompt, position)) = composer_text::history_step(&entries, self.prompt_history.as_ref(), &text, back)
        else {
            return false;
        };
        self.set_composer_text(prompt, window, cx);
        self.composer.update(cx, |state, cx| {
            let end = state.value().len();
            state.set_selected_range(end..end, cx);
        });
        self.prompt_history = position;
        self.hide_menu(cx);
        true
    }

    fn history_entries(&self) -> Vec<String> {
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else {
            return Vec::new();
        };
        let messages = (projection.visible_turn_items.iter().map(|row| &row.item))
            .filter(|item| item.item_type == "user_message" && crate::queue::sent_by_user(item));
        composer_text::history_entries(messages.filter_map(|item| item.str("text")))
    }

    /// Enter sends unless `sendShortcut` asks for Ctrl (`composerSubmissionIntentForKey`);
    /// the alternate sends are keybinding commands.
    fn send_key(
        &mut self,
        keystroke: &Keystroke,
        context: &super::keymap::ShortcutContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !super::send::sends_on(keystroke, context.composer_requires_modifier) {
            return false;
        }
        self.submit_with(super::send::Intent::Foreground, window, cx);
        true
    }

    /// 1-9 pick an option on an open question card while no text field, terminal or preview has focus.
    fn question_option_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = keystroke.modifiers;
        if m.control || m.platform || m.alt || self.page.is_some() || self.settings.is_some() {
            return false;
        }
        let Some(digit) = keystroke.key.parse::<usize>().ok().filter(|d| (1..=9).contains(d)) else { return false };
        // The web skips keys aimed at any text field; here that is any focus but the workspace's own
        // (composer, find bar, side chat, terminal, preview), and an open palette or dialog.
        let resting = self.focus.is_focused(window) || window.focused(cx).is_none();
        if !resting || self.palette.is_some() || window.has_active_dialog(cx) {
            return false;
        }
        // The open thread's card first, else the side chat's.
        let pane = if self.question.is_some() { ChatPane::Main } else { ChatPane::Side };
        let Some(draft) = self.question(pane) else { return false };
        if draft.collapsed || !self.question_answerable(pane) {
            return false;
        }
        let Some(option) = draft.questions[draft.index].options.get(digit - 1) else { return false };
        let value = option.value().to_owned();
        self.pick_option(pane, value, window, cx);
        true
    }

    /// `useComposerTypingGuard`: the open thread's pending requests, minding the ones new mid-typing.
    pub(super) fn sync_typing_guard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else { return };
        let approvals = projection.pending_approvals().into_iter().map(|(request, _)| request.id.as_str());
        let questions = projection.pending_user_inputs().into_iter().map(|(request, _)| request.id.as_str());
        let ids: Vec<&str> = approvals.chain(questions).collect();
        if self.typing_guard.requests(ids, std::time::Instant::now()) {
            self.schedule_typing_idle(window, cx);
            cx.notify();
        }
    }

    /// Shows what the guard held once typing has rested.
    fn schedule_typing_idle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.typing_guard.idle_at() else {
            self._typing_idle = None;
            return;
        };
        self._typing_idle = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(at.saturating_duration_since(std::time::Instant::now())).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.typing_guard.idle_at() == Some(at) {
                    this.release_typing_hold(false, window, cx);
                }
            });
        }));
    }

    /// Blur, a send or rested typing shows the held requests.
    pub(super) fn release_typing_hold(&mut self, blur: bool, window: &mut Window, cx: &mut Context<Self>) {
        self._typing_idle = None;
        if self.typing_guard.release(blur) {
            self.sync_question(ChatPane::Main, window, cx);
            cx.notify();
        }
    }

    /// Pending approvals the composer shows: those the typing guard isn't holding.
    pub(super) fn shown_approvals(&self) -> Vec<(&RuntimeRequest, Option<&TurnItem>)> {
        let Some(projection) = self.open_projection() else { return Vec::new() };
        let mut approvals = projection.pending_approvals();
        approvals.retain(|(request, _)| !self.typing_guard.is_held(&request.id));
        approvals
    }

    pub(super) fn question(&self, pane: ChatPane) -> Option<&QuestionDraft> {
        match pane {
            ChatPane::Main => self.question.as_ref(),
            ChatPane::Side => self.side_chat_question(),
        }
    }

    fn question_slot(&mut self, pane: ChatPane) -> Option<&mut Option<QuestionDraft>> {
        match pane {
            ChatPane::Main => Some(&mut self.question),
            ChatPane::Side => self.side_chat_question_slot(),
        }
    }

    pub(super) fn question_mut(&mut self, pane: ChatPane) -> Option<&mut QuestionDraft> {
        self.question_slot(pane)?.as_mut()
    }

    /// Replaces the pane's pending auto-advance; `None` cancels it, like the web clearing its
    /// timer when the active question changes or the card unmounts.
    fn set_question_advance(&mut self, pane: ChatPane, task: Option<gpui_kit::Task<()>>) {
        let slot = match pane {
            ChatPane::Main => Some(&mut self.composer_ui.question_advance),
            ChatPane::Side => self.side_chat_advance_slot(),
        };
        if let Some(slot) = slot {
            *slot = task;
        }
    }

    pub(super) fn cancel_question_advances(&mut self) {
        self.set_question_advance(ChatPane::Main, None);
        self.set_question_advance(ChatPane::Side, None);
    }

    /// Opens a fresh draft when the pane's first pending question changes. The editor
    /// then holds the question's answer, and the prompt waits in the draft
    /// until the last question closes.
    pub(super) fn sync_question(&mut self, pane: ChatPane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(projection) = self.pane_projection(pane) else { return };
        let first = projection.pending_user_inputs().into_iter().next();
        // A question that arrived mid-typing waits; the composer keeps the prompt meanwhile.
        let first = first.filter(|(request, _)| pane == ChatPane::Side || !self.typing_guard.is_held(&request.id));
        if first.map(|(request, _)| &request.id) == self.question(pane).map(|q| &q.request_id) {
            return;
        }
        let next = first.and_then(|(request, item)| QuestionDraft::new(request.id.clone(), requests::questions(item)));
        self.set_question_advance(pane, None);
        let Some(previous) = self.question_slot(pane).map(Option::take) else { return };
        // The open thread's composer lists the question on screen's attachments; side chats take none.
        let main = pane == ChatPane::Main;
        match (previous, next) {
            (None, None) => {}
            (Some(previous), None) => {
                if main {
                    self.close_question_attachments(previous.attachments, previous.prompt_attachments, cx);
                    // Ahead of the prompt's text, so its chips find them.
                    self.context.records = previous.prompt_records;
                }
                self.set_pane_text(pane, previous.prompt, window, cx);
            }
            (previous, Some(draft)) => {
                let (prompt, prompt_attachments, prompt_records) = match previous {
                    Some(previous) if main => {
                        self.close_question_attachments(previous.attachments, Vec::new(), cx);
                        (previous.prompt, previous.prompt_attachments, previous.prompt_records)
                    }
                    Some(previous) => (previous.prompt, previous.prompt_attachments, previous.prompt_records),
                    // Out of reach of the answer's text, which would prune them.
                    None if main => {
                        (self.pane_text(pane, cx), std::mem::take(&mut self.attachments), self.take_context_records())
                    }
                    None => (self.pane_text(pane, cx), Vec::new(), ContextRecords::default()),
                };
                if let Some(slot) = self.question_slot(pane) {
                    *slot = Some(QuestionDraft { prompt, prompt_attachments, prompt_records, ..draft });
                }
                self.load_custom_answer(pane, window, cx);
            }
        }
    }

    /// Shows question `index` of the pane's card, with its attachments in the composer.
    fn show_question(&mut self, pane: ChatPane, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.set_question_advance(pane, None);
        match pane {
            ChatPane::Main => {
                let Some(question) = self.question.as_mut() else { return };
                question.park(std::mem::take(&mut self.attachments));
                question.index = index;
                self.attachments = question.unpark();
            }
            ChatPane::Side => {
                let Some(draft) = self.question_mut(pane) else { return };
                draft.index = index;
            }
        }
        self.load_custom_answer(pane, window, cx);
        cx.notify();
    }

    pub(super) fn pending_question_request(&self, pane: ChatPane) -> Option<(&RuntimeRequest, &TurnItem)> {
        let id = &self.question(pane)?.request_id;
        self.pane_projection(pane)?.pending_user_inputs().into_iter().find(|(request, _)| &request.id == id)
    }

    /// A user edit in the pane's composer, while a question holds it, is the custom answer.
    pub(super) fn save_custom_answer(&mut self, pane: ChatPane, cx: &mut Context<Self>) {
        if self.question(pane).is_none() {
            return;
        }
        let text = self.pane_text(pane, cx);
        let Some(draft) = self.question_mut(pane) else { return };
        let question = &draft.questions[draft.index];
        if text.is_empty() && !draft.drafts.contains_key(&question.id) {
            return;
        }
        let next = requests::set_custom(question, draft.drafts.get(&question.id), text);
        draft.drafts.insert(question.id.clone(), next);
    }

    fn load_custom_answer(&mut self, pane: ChatPane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.question(pane) else { return };
        let question = &draft.questions[draft.index];
        let text = draft.drafts.get(&question.id).and_then(|d| d.custom.clone()).unwrap_or_default();
        self.set_pane_text(pane, text, window, cx);
    }

    /// The pane's question takes answers now: its request still waits and can resume, no answer
    /// is on its way, and the connection may operate the thread (`responseDisabled`).
    pub(super) fn question_answerable(&self, pane: ChatPane) -> bool {
        !self.question_responding(pane)
            && self.pane_can_operate(pane)
            && self.pending_question_request(pane).is_some_and(|(r, _)| r.capability() != Some("not_resumable"))
    }

    /// Next question, or submit on the last; a no-op while the active one is unanswered.
    pub(super) fn advance_question(&mut self, pane: ChatPane, window: &mut Window, cx: &mut Context<Self>) {
        if !self.question_answerable(pane) {
            return;
        }
        let Some(draft) = self.question(pane) else { return };
        let question = draft.active();
        let attached = self.question_attached(pane, &question.id);
        if requests::answer(question, draft.drafts.get(&question.id), attached).is_none() {
            return;
        }
        if draft.index + 1 < draft.questions.len() {
            let next = draft.index + 1;
            self.show_question(pane, next, window, cx);
            return;
        }
        let attached = |id: &str| self.question_attached(pane, id);
        let Some(answers) = requests::answers(&draft.questions, &draft.drafts, attached) else { return };
        let request_id = draft.request_id.clone();
        let mut response = json!({"answers": answers});
        if pane == ChatPane::Main {
            match self.question_attachments_payload() {
                Ok(by_question) => {
                    if let Some(by_question) = by_question {
                        response["attachmentsByQuestionId"] = by_question;
                    }
                }
                Err(error) => {
                    self.error = Some(error.into());
                    cx.notify();
                    return;
                }
            }
        }
        self.respond(pane, request_id, response, cx);
    }

    pub(super) fn previous_question(&mut self, pane: ChatPane, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.question(pane) else { return };
        let previous = draft.index.saturating_sub(1);
        self.show_question(pane, previous, window, cx);
    }

    fn pick_option(&mut self, pane: ChatPane, value: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.question_answerable(pane) {
            return;
        }
        let shown = self.pane_thread(pane);
        let Some(draft) = self.question_mut(pane) else { return };
        let question = &draft.questions[draft.index];
        let next = requests::toggle(question, draft.drafts.get(&question.id), &value);
        let advance = !question.multi_select;
        draft.drafts.insert(question.id.clone(), next);
        // Request ids repeat across environments, so the pane's thread is part of where it was.
        let at = (shown, draft.request_id.clone(), draft.index);
        self.set_pane_text(pane, String::new(), window, cx);
        cx.notify();
        // A pick replaces the previous one's timer, as the web's does.
        let task = advance.then(|| {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(AUTO_ADVANCE).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    let now = this.question(pane).map(|q| (this.pane_thread(pane), q.request_id.clone(), q.index));
                    if now.as_ref() == Some(&at) {
                        this.advance_question(pane, window, cx);
                    }
                });
            })
        });
        self.set_question_advance(pane, task);
    }

    fn dismiss_question(&mut self, pane: ChatPane, request_id: String, cx: &mut Context<Self>) {
        self.dispatch_response(
            pane,
            request_id,
            |thread_id, request_id| {
                json!({"type": "thread.user-input.dismiss", "commandId": new_id(), "threadId": thread_id, "requestId": request_id})
            },
            cx,
        );
    }

    /// Sending while an approval waits declines it: with the text as its
    /// feedback when the provider takes feedback (the text is then used up and
    /// `true` comes back), else every live approval is declined before the
    /// message goes out.
    pub(super) fn approval_declines(&self, text: &str) -> (Vec<Value>, bool) {
        // Approvals the typing guard holds stay unseen and pending.
        let live: Vec<_> =
            self.shown_approvals().into_iter().map(|(request, _)| request).filter(|r| r.is_live()).collect();
        match live.first() {
            None => (Vec::new(), false),
            Some(first) if first.supports_feedback() => {
                let decline = self.respond_command(&first.id, json!({"decision": "decline", "feedback": text}));
                (decline.into_iter().collect(), true)
            }
            Some(_) => (
                live.iter().filter_map(|r| self.respond_command(&r.id, json!({"decision": "decline"}))).collect(),
                false,
            ),
        }
    }

    /// `defaultRuntimeMode` for the open thread's or draft's project (`ChatView.tsx`).
    pub(super) fn default_runtime_mode(&self) -> String {
        let mode = self.server_info().project_setting(self.target_project(), "defaultRuntimeMode");
        mode.as_str().unwrap_or(DEFAULT_RUNTIME_MODE).to_owned()
    }

    pub(super) fn runtime_mode(&self) -> String {
        let mode = match &self.target {
            Some(Target::Thread { .. }) => self
                .thread
                .as_ref()
                .and_then(|t| t.state.projection.as_ref())
                .and_then(|p| p.thread.runtime_mode.clone()),
            _ => self.runtime_mode_override.clone(),
        };
        mode.unwrap_or_else(|| self.default_runtime_mode())
    }

    pub(super) fn set_runtime_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        match &self.target {
            Some(Target::Thread { .. }) => {
                // Changing a started thread needs `orchestration:operate` on its environment.
                let Some((client, _, thread_id)) = self.pane_client(ChatPane::Main, cx) else { return };
                let command = json!({"type": "thread.runtime-mode.set", "commandId": new_id(), "threadId": thread_id, "runtimeMode": mode});
                self.dispatch_in_order(client, vec![command], cx);
            }
            _ => self.runtime_mode_override = Some(mode.to_owned()),
        }
        self.drafts_changed(cx);
        cx.notify();
    }

    /// `ChatComposer.tsx` placeholder precedence; an offered prompt suggestion replaces only the default.
    fn placeholder(&self, approval: Option<&RuntimeRequest>) -> &'static str {
        if let Some(approval) = approval {
            return if approval.supports_feedback() {
                "Send feedback to decline this request"
            } else {
                "Send a message to decline and redirect the agent"
            };
        }
        if let Some(draft) = &self.question {
            return question_placeholder(draft);
        }
        if self.target.is_none() {
            return "Choose a project above to start a thread";
        }
        if self.server_info().model_choices().is_empty() {
            return "Enable a provider in Settings to send a message";
        }
        DEFAULT_PLACEHOLDER
    }

    /// `data-chat-composer-overlay`: the composer floats over the transcript's end, which scrolls
    /// on beneath it; its measured height becomes the transcript's end inset.
    pub(super) fn render_floating_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .on_children_prepainted(move |bounds, _, cx| {
                let Some(height) = bounds.first().map(|b| f32::from(b.size.height).ceil()) else { return };
                let view = view.clone();
                cx.defer(move |cx| {
                    let _ = view.update(cx, |this, cx| {
                        if (this.composer_height - height).abs() >= 1. {
                            this.composer_height = height;
                            cx.notify();
                        }
                    });
                });
            })
            .child(self.render_composer(window, cx))
    }

    pub(super) fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (lane, _) = self.chat_layout(window);
        let t = *tokens(cx);
        let approvals = self.shown_approvals();
        let first_approval = approvals.first().map(|(request, _)| *request);
        let placeholder = self.placeholder(first_approval);
        let suggestion = self
            .offered_prompt_suggestion(first_approval.is_some())
            .map(|text| super::composer_suggestion::suggested_placeholder(text, &t, window));
        let outline = t.composer_outline();
        let view = cx.weak_entity();
        let chip_view = view.clone();
        let hover_view = view.clone();
        let pull_requests = Rc::new(pull_request_contexts(self.context.records.iter()));
        let chip_tooltip = self.render_chip_tooltip(&pull_requests, &t);
        let machines: Rc<[(String, Lucide)]> = (self.sessions().iter())
            .filter_map(|s| {
                Some((s.id.as_ref()?.to_string(), super::host_picker::machine_icon(s.server_info.machine_kind())))
            })
            .collect();
        let held: Rc<HashSet<String>> =
            Rc::new(self.context.records.iter().filter_map(|r| r["contextId"].as_str().map(str::to_owned)).collect());
        let models: Rc<HashMap<String, ModelMention>> = Rc::new(
            (self.composer.read(cx).tokens().iter())
                .filter_map(|span| {
                    let Some(Chip::Model { instance_id, model, .. }) = Chip::from_id(span.token().id()) else {
                        return None;
                    };
                    Some((span.token().id().to_string(), self.model_mention(&instance_id, &model)?))
                })
                .collect(),
        );
        // The web's composer host turns `inert` while a queued edit saves.
        let saving = self.saving_queue_edit();
        let editor = Textarea::new(&self.composer)
            .appearance(false)
            .xsmall()
            .font_family(t.fonts.composer)
            .text_size(px(t.fonts.prompt_size))
            .line_height(px(t.fonts.prompt_size * PROMPT_LEADING))
            .mx(px(-4.))
            .disabled(self.target.is_none())
            .readonly(saving)
            .token(move |token, _, _| {
                let records =
                    ChipRecords { pull_requests: &pull_requests, held: &held, machines: &machines, models: &models };
                render_chip(token, &records, &t)
            })
            // A pull request chip shows its details while hovered (`PullRequestContextDetails`).
            .on_token_hover(move |event, _, cx| {
                let hovered = event.is_hovered().then(|| (event.token().id().to_string(), event.bounds()));
                let _ = hover_view.update(cx, |this, cx| {
                    if this.composer_ui.chip_hover != hovered {
                        this.composer_ui.chip_hover = hovered;
                        cx.notify();
                    }
                });
            })
            // A file or image chip opens its preview: an uploaded attachment's (`composerContextActions.openFile`), or
            // the workspace file a mention names in the panel (`openMention`).
            .on_token_click(move |event, window, cx| match Chip::from_id(event.token().id()) {
                Some(Chip::Context { kind, id, .. }) if kind == "file" || kind == "image" => {
                    if let Some(local_id) = id.strip_prefix("file_").or_else(|| id.strip_prefix("image_")) {
                        let _ = chip_view.update(cx, |this, cx| this.open_attachment_preview(local_id, window, cx));
                    }
                }
                Some(Chip::File { path }) => {
                    // A folder mention's trailing slash goes, except a drive root's (`openFile`).
                    let trimmed = path.trim_end_matches('/');
                    let drive_root =
                        trimmed.len() == 2 && trimmed.ends_with(':') && trimmed.as_bytes()[0].is_ascii_alphabetic();
                    let path = if trimmed.is_empty() || drive_root { path.clone() } else { trimmed.to_owned() };
                    let _ = chip_view.update(cx, |this, cx| this.open_file_surface(path, window, cx));
                }
                Some(Chip::Citation { .. }) => {
                    let (id, range, bounds) = (event.token().id().to_string(), event.range(), event.bounds());
                    let _ = chip_view.update(cx, |this, cx| {
                        this.citation_chip_clicked(ChatPane::Main, id, range, bounds, event.click(), window, cx);
                    });
                }
                // `ModelMentionChip`: a click or Enter opens its details popover.
                Some(Chip::Model { instance_id, model, .. }) => {
                    let at = model_chip_menu_at(event.bounds());
                    let _ = chip_view
                        .update(cx, |this, cx| this.open_model_menu(&instance_id, &model, None, at, window, cx));
                }
                _ => {}
            })
            .on_paste(move |item, window, cx| {
                view.update(cx, |this, cx| this.paste_attachments(item, window, cx)).unwrap_or(false)
            });
        // A provider's own subagent runs on its own: its bar stands in for the composer.
        let subagent_bar = self.render_provider_subagent_bar(&t, cx);
        let subagent = subagent_bar.is_some();
        let surface = v_flex()
            .w_full()
            .relative()
            .children(super::theme_inspect::probe(&["surfaceRaised", "toolbarBorder", "input"], cx))
            .rounded(px(16.))
            .border_1()
            .border_color(outline)
            // The glass fill over the canvas, opaque so the banner tucked under it stays hidden.
            .bg(t.background.blend(t.composer_glass().opacity(t.glass)))
            .shadow(if t.dark {
                vec![BoxShadow::new(px(0.), px(1.), t.composer_highlight()).inset()]
            } else {
                vec![BoxShadow::new(px(0.), px(1.), black(0.05)).blur_radius(px(2.))]
            })
            // A Files row dropped here joins the prompt as a mention (`isDragOverComposer`).
            .drag_over::<super::files::MentionDrag>(move |style, _, _, _| {
                style
                    .bg(t.background.blend(t.composer_glass().opacity(t.glass)).blend(t.accent.opacity(0.45)))
                    .border_color(t.primary.opacity(0.7))
            })
            .on_drop(cx.listener(|this, drag: &super::files::MentionDrag, window, cx| {
                if this.target.is_some() && !this.saving_queue_edit() {
                    this.add_file_to_chat(&drag.path, window, cx);
                    this.focus_composer(window, cx);
                }
            }))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                let held = event.modifiers.control || event.modifiers.platform;
                if held != this.ctrl_held {
                    this.ctrl_held = held;
                    cx.notify();
                }
            }))
            .children(subagent_bar)
            .when(!subagent, |surface| {
                surface
                    .child(
                        v_flex()
                            .px(px(16.))
                            .pt(px(16.))
                            .pb(px(8.))
                            .children(self.render_queue_edit_attachments(&t, cx))
                            .children(self.render_attachments(&t, cx))
                            .child(
                                // The placeholder wraps over the editor's two reserved lines without
                                // growing it, as Tiptap's zero-height `::before` does.
                                div().relative().child(editor).children(chip_tooltip).when(self.composer_empty, |el| {
                                    el.child(
                                        div()
                                            .absolute()
                                            .top_0()
                                            .left_0()
                                            .right_0()
                                            .font_family(t.fonts.composer)
                                            .text_size(px(t.fonts.prompt_size))
                                            .line_height(px(t.fonts.prompt_size * PROMPT_LEADING))
                                            .text_color(t.placeholder)
                                            .children(super::theme_inspect::probe(&["placeholder"], cx))
                                            .map(|el| match suggestion {
                                                Some(suggested) => el.child(suggested),
                                                None => el.child(placeholder),
                                            }),
                                    )
                                }),
                            ),
                    )
                    .children(self.composer_ui.length_error.clone().map(|error| {
                        div().px(px(16.)).pb(px(8.)).text_xs().line_height(px(16.)).text_color(t.error).child(error)
                    }))
                    .child(self.render_footer(window, &t, cx))
            })
            .when(saving, |el| el.child(inert_cover("composer-inert")));
        // The draft hero centers the bare stack; under a thread the overlay keeps `sm:pt-2`.
        let top = if matches!(self.target, Some(Target::Draft { .. })) { 0. } else { 8. };
        div().w_full().flex_none().pt(px(top)).pl(px(lane.left)).pr(px(lane.right)).pb(px(20.)).child(
            v_flex()
                .id("composer-stack")
                .w_full()
                // Only the stack takes the pointer; the transcript shows and scrolls around it.
                .occlude()
                // Approvals the timeline shows where they were asked stay there.
                .children(
                    approvals
                        .iter()
                        .filter(|(request, _)| !self.timeline_shows_approval(&request.id))
                        .map(|(request, item)| self.render_approval(ChatPane::Main, request, *item, &t, cx)),
                )
                .children(self.render_banner_dock(&t, cx))
                .child(
                    div()
                        .relative()
                        .children(self.render_command_menu(&t, cx))
                        .children(self.render_stash_menu(f32::from(window.viewport_size().height) * 0.4, &t, cx))
                        .children(self.render_language_picker(&t, window, cx))
                        .child(surface),
                )
                .children(self.render_context_strip(&t, cx))
                // Message and composer chips share one menu; both surfaces draw the composer.
                .children(self.render_chat_menu()),
        )
    }

    /// `resolveComposerControlsOwnership` plus the width check: the permissions chip (off the
    /// default only), labelled, icon only, or in the overflow menu.
    fn render_mode_block(&self, window: &Window, t: &Tokens, cx: &mut Context<Self>) -> (Option<AnyElement>, ModeFit) {
        let mode = self.runtime_mode();
        // Drafts name their permissions in the row under the composer instead.
        let strip_owns_mode = matches!(self.target, Some(Target::Draft { .. }));
        if strip_owns_mode || mode == self.default_runtime_mode() {
            return (None, ModeFit::Label);
        }
        let (_, label, icon) = runtime_modes().into_iter().find(|(id, ..)| *id == mode).unwrap_or(runtime_modes()[3]);
        let text = FooterText {
            picker: self.model_trigger_text(),
            chip: Some(label),
            font: gpui_kit::Font { weight: FontWeight::MEDIUM, ..window.text_style().font() },
            size: gpui_kit::rems(0.875).to_pixels(window.rem_size()),
            scale: window.scale_factor(),
        };
        let widths = {
            let mut cached = self.composer_ui.footer_widths.borrow_mut();
            match &*cached {
                Some((measured, widths)) if *measured == text => *widths,
                _ => {
                    let widths = text.widths(window);
                    *cached = Some((text, widths));
                    widths
                }
            }
        };
        let fit = self
            .composer_ui
            .footer_host
            .map_or(ModeFit::Label, |host| resolve_mode_fit(widths, host, self.composer_ui.mode_fit));
        let element = match fit {
            // The overflow menu holds Permissions alone.
            ModeFit::Hidden => {
                self.render_permissions_control("footer-permissions", icon, label, PermissionsForm::Overflow, t, cx)
            }
            ModeFit::Label | ModeFit::Icon => {
                let form = PermissionsForm::Chip { label: fit == ModeFit::Label };
                let chip = self.render_permissions_control("footer-permissions", icon, label, form, t, cx);
                h_flex().flex_none().child(chip).into_any_element()
            }
        };
        (Some(element), fit)
    }

    fn render_footer(&self, window: &Window, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let (mode_block, fit) = self.render_mode_block(window, t, cx);
        let view = cx.weak_entity();
        h_flex()
            .items_center()
            .px(px(8.))
            .pb(px(8.))
            .when(self.question.is_some(), |el| el.pt(px(8.)))
            .when(self.can_attach(), |el| {
                el.child(
                    div().mr(px(8.)).child(
                        action_control(div().id("attach"), &self.composer_action_focus[ATTACH], t)
                            .size(px(28.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .cursor_pointer()
                            .hover(|s| s.bg(t.accent))
                            .child(
                                Icon::new(Lucide::Paperclip)
                                    .with_size(px(16.))
                                    .text_color(t.muted_foreground)
                                    .mt(px(1.)),
                            )
                            .tooltip(|window, cx| Tooltip::new("Attach files").build(window, cx))
                            .on_click(cx.listener(|this, _, window, cx| this.pick_attachments(window, cx))),
                    ),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .justify_end()
                    .gap(px(8.))
                    // The controls row's width decides what fits next frame, like the web's observer.
                    .on_children_prepainted(move |bounds, _, cx| {
                        let Some(host) = bounds.first().map(|b| f32::from(b.size.width)) else { return };
                        let view = view.clone();
                        cx.defer(move |cx| {
                            let _ = view.update(cx, |this, cx| {
                                let ui = &mut this.composer_ui;
                                if ui.footer_host.is_none_or(|old| (old - host).abs() >= 0.5) || ui.mode_fit != fit {
                                    ui.footer_host = Some(host);
                                    ui.mode_fit = fit;
                                    cx.notify();
                                }
                            });
                        });
                    })
                    .child(h_flex().flex_1().min_w_0().justify_end().gap(px(2.)).map(|el| {
                        match self.render_provider_unavailable(t, cx) {
                            Some(unavailable) => el.child(unavailable),
                            None => el.child(self.render_model_picker(t, cx)).children(mode_block),
                        }
                    }))
                    // `@max-[480px]/composer-surface:hidden` on the toggle's label.
                    .children(self.render_compact_toggle(self.chat_layout(window).0.width < 480., t, cx))
                    .child(self.render_primary_action(t, cx)),
            )
    }

    /// `canOperateThread`: this connection may change the open target's environment.
    pub(super) fn target_can_operate(&self) -> bool {
        self.target.as_ref().is_some_and(|target| self.can_operate(target.environment()))
    }

    fn render_primary_action(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let operate = self.target_can_operate();
        let running = self
            .thread
            .as_ref()
            .and_then(|t| t.state.projection.as_ref())
            .is_some_and(super::super::model::ThreadProjection::is_running);
        let stop = || {
            action_control(round_button("stop", t.error.opacity(0.9), t.error, t), &self.composer_action_focus[STOP], t)
                .hover(|s| s.bg(t.error))
                .child(div().size(px(8.)).rounded(px(1.5)).bg(t.primary_foreground))
                .tooltip(|window, cx| Tooltip::new("Interrupt").build(window, cx))
                .when(operate, |el| el.on_click(cx.listener(|this, _, _, cx| this.stop(cx))))
        };
        if let Some(draft) = &self.question {
            let responding = self.pane_responding(ChatPane::Main, &draft.request_id);
            let can_respond = self
                .pending_question_request(ChatPane::Main)
                .is_some_and(|(r, _)| r.capability() != Some("not_resumable"));
            let attached = |id: &str| self.question_attached(ChatPane::Main, id);
            let (label, enabled) = question_submit(draft, attached, responding, can_respond);
            let enabled = enabled && operate;
            let submit = div().id("submit-answer");
            let submit = if enabled { action_control(submit, &self.composer_action_focus[PRIMARY], t) } else { submit };
            return h_flex()
                .gap(px(8.))
                .when(running, |el| el.child(stop()))
                .when(draft.index > 0, |el| {
                    el.child(
                        Button::new("previous-question")
                            .outline()
                            .small()
                            .label("Previous")
                            .disabled(responding)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.previous_question(ChatPane::Main, window, cx)),
                            ),
                    )
                })
                .child(
                    submit
                        .h(px(28.))
                        .px(px(16.))
                        .flex()
                        .items_center()
                        .rounded_full()
                        .bg(t.primary)
                        .text_color(t.primary_foreground)
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .shadow(action_shadow(t.primary))
                        .child(label)
                        .when(!enabled, |el| el.opacity(0.64))
                        .when(enabled, |el| {
                            el.cursor_pointer().hover(|s| s.bg(t.primary_hover)).on_click(
                                cx.listener(|this, _, window, cx| this.advance_question(ChatPane::Main, window, cx)),
                            )
                        }),
                )
                .into_any_element();
        }
        let sendable = !self.composer_empty || self.has_attachments();
        let can_resume = self.can_resume();
        let editing = self.editing_queued();
        if running && !sendable && !editing {
            return stop().into_any_element();
        }
        let sending = self.target_sending();
        let busy = sending || self.saving_queue_edit();
        if can_resume && !sendable {
            let enabled = operate && !sending && !self.target_rewinding() && self.client().is_some();
            // The web's send button in its resume state: the same stage art, a filled play.
            let stage = self.stage_art();
            let (fill, icon) = if stage.is_some() {
                (gpui_kit::transparent_black(), gpui_kit::white())
            } else {
                (t.primary, t.primary_foreground)
            };
            let content = if sending {
                Spinner::new().small().color(icon).into_any_element()
            } else {
                Icon::empty().path("icons/app/play-filled.svg").with_size(px(16.)).text_color(icon).into_any_element()
            };
            let resume = round_button("resume", fill, t.primary, t);
            let resume = if enabled { action_control(resume, &self.composer_action_focus[PRIMARY], t) } else { resume };
            return resume
                .when_some(stage, |el, art| el.relative().child(super::sidebar::stage_button_art(art, t.dark)))
                .child(div().relative().child(content))
                .when(!enabled, |el| el.opacity(0.64).shadow(Vec::new()))
                .when(enabled, |el| {
                    el.cursor_pointer()
                        .when(stage.is_none(), |el| el.hover(|s| s.bg(t.primary_hover)))
                        .on_click(cx.listener(|this, _, window, cx| this.resume_thread(window, cx)))
                })
                .tooltip(|window, cx| Tooltip::new("Resume thread").build(window, cx))
                .into_any_element();
        }
        let disabled_reason: Option<SharedString> = if !operate {
            Some(super::side_chat::NOT_OPERATE.into())
        } else if self.model_fanout().is_some_and(<[_]>::is_empty) {
            Some("Select at least one model.".into())
        } else if self.attachments_busy() {
            Some("Attachment(s) still uploading".into())
        } else if let Some(reason) = self.clone_send_block() {
            Some(reason.into())
        } else {
            // An answer to an open question still goes through.
            self.runtime_mode_ceiling().filter(|_| self.question.is_none()).map(|(denial, _)| denial.into())
        };
        let can_send = sendable
            && self.target.is_some()
            && self.client().is_some()
            && disabled_reason.is_none()
            && !self.target_rewinding()
            && !self.saving_queue_edit();
        // `environmentIdentificationMode`: the stage art fills the button (`StageBackdropButtonArt`).
        let stage = self.stage_art();
        let (fill, icon) = if stage.is_some() {
            (gpui_kit::transparent_black(), gpui_kit::white())
        } else if can_send || busy {
            (t.primary, t.primary_foreground)
        } else {
            (t.foreground.opacity(0.12), t.muted_foreground)
        };
        let follow_up = self.follow_up_behavior();
        let alternate = if follow_up == "queue" { "steer" } else { "queue" };
        let held = if self.ctrl_held { Intent::Alternate } else { Intent::Foreground };
        let can_steer = !self.open_projection().is_some_and(crate::model::ThreadProjection::is_compacting);
        let queue = running && super::send::queues(follow_up, held, can_steer);
        let content: AnyElement = if busy {
            Spinner::new().small().color(icon).into_any_element()
        } else if editing {
            Icon::new(Lucide::Check).with_size(px(16.)).text_color(icon).into_any_element()
        } else if running {
            let glyph = if queue { Lucide::ListPlus } else { Lucide::CornerUpRight };
            Icon::new(glyph).with_size(px(16.)).text_color(icon).into_any_element()
        } else {
            svg().path("icons/app/send.svg").size(px(14.)).text_color(icon).into_any_element()
        };
        let compact_tokens = self.compact_send_tokens().map(super::details::format_context_tokens);
        let tooltip: Option<SharedString> = match (disabled_reason, running && can_send) {
            (Some(reason), _) => Some(reason),
            (None, _) if editing => Some(if busy { "Updating queued message" } else { "Update queued message" }.into()),
            (None, _) if compact_tokens.is_some() && !busy => {
                compact_tokens.map(|tokens| format!("Summarize {tokens} tokens of history, then send").into())
            }
            (None, true) if !can_steer => Some("Queue message".into()),
            (None, true) => {
                let shortcut = self.client_settings.values.get("sendShortcut").as_str().unwrap_or("enter");
                let context = super::keymap::ShortcutContext {
                    composer_focus: true,
                    turn_running: true,
                    composer_requires_modifier: super::send::requires_modifier(
                        shortcut,
                        &self.composer.read(cx).value(),
                    ),
                    ..Default::default()
                };
                let keys = super::keymap::label_for(&self.keymap, "composer.sendAlternate", &context)
                    .map(|keys| format!(" or {keys}"))
                    .unwrap_or_default();
                let modifier = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl" };
                Some(format!("Click to {follow_up}, {modifier}-click{keys} to {alternate}").into())
            }
            _ => None,
        };
        let send = round_button("send", fill, t.primary, t)
            .children(super::theme_inspect::probe(&["messageAction", "messageActionForeground"], cx));
        let send = if can_send { action_control(send, &self.composer_action_focus[PRIMARY], t) } else { send };
        send.when_some(stage, |el, art| {
            el.relative().when(!can_send, |el| el.opacity(0.64)).child(super::sidebar::stage_button_art(art, t.dark))
        })
        .child(div().relative().child(content))
        .when(!can_send, |el| el.shadow(Vec::new()))
        .when(can_send, |el| {
            el.cursor_pointer().when(stage.is_none(), |el| el.hover(|s| s.bg(t.primary_hover))).on_click(cx.listener(
                |this, event: &ClickEvent, window, cx| {
                    let modifiers = event.modifiers();
                    let intent =
                        if modifiers.control || modifiers.platform { Intent::Alternate } else { Intent::Foreground };
                    this.submit_with(intent, window, cx);
                },
            ))
        })
        .when_some(tooltip, |el, tooltip| el.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)))
        .into_any_element()
    }

    fn timeline_shows_approval(&self, request_id: &str) -> bool {
        self.rows
            .iter()
            .any(|row| matches!(row, crate::timeline::Row::Approval { request_id: id, .. } if id == request_id))
    }

    pub(super) fn render_approval(
        &self,
        pane: ChatPane,
        request: &RuntimeRequest,
        item: Option<&TurnItem>,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (icon, label) = match request.kind.as_str() {
            "command" => (Lucide::Terminal, "Command approval"),
            "file-change" => (Lucide::SquarePen, "File change approval"),
            "file-read" => (Lucide::Eye, "File read approval"),
            "permission" => (Lucide::Wrench, "App permission approval"),
            _ => (Lucide::Hammer, "App access approval"),
        };
        let detail = item.and_then(|i| i.str("prompt")).filter(|p| !p.is_empty()).unwrap_or(label).to_owned();
        let app_name = item.and_then(|i| i.str("appName")).map(str::to_owned);
        let reason = item.and_then(|i| i.str("reason")).map(str::to_owned);
        // (decision, label, warning)
        let options: Vec<(String, String, Option<SharedString>)> =
            item.and_then(|i| i.raw.get("options")).and_then(Value::as_array).map_or_else(
                || {
                    [
                        ("cancel", "Cancel"),
                        ("decline", "Decline"),
                        ("acceptForSession", "Always allow this session"),
                        ("accept", "Approve"),
                    ]
                    .map(|(d, l)| (d.to_owned(), l.to_owned(), None))
                    .to_vec()
                },
                |options| {
                    options
                        .iter()
                        .filter_map(|o| {
                            let warning =
                                o.get("warning").and_then(Value::as_str).map(|w| SharedString::from(w.to_owned()));
                            Some((
                                o.get("decision")?.as_str()?.to_owned(),
                                o.get("label")?.as_str()?.to_owned(),
                                warning,
                            ))
                        })
                        .collect()
                },
            );
        let enabled = request.is_live() && !self.pane_responding(pane, &request.id) && self.pane_can_operate(pane);
        let inline =
            |decision: &str, label: &str, warning: Option<SharedString>, tone: Hsla, cx: &mut Context<Self>| {
                let request_id = request.id.clone();
                let decision = decision.to_owned();
                // A button, so Tab reaches it and Enter or Space answers (`InlineButton`).
                Button::new(ElementId::Name(format!("approval-{}-{decision}", request.id).into()))
                    .text()
                    .xsmall()
                    .h(px(16.))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tone)
                    .when(warning.is_some(), |el| el.icon(Icon::new(Lucide::TriangleAlert).with_size(px(12.))))
                    .label(label.to_owned())
                    .when_some(warning, Button::tooltip)
                    .disabled(!enabled)
                    .when(!enabled, |el| el.opacity(0.64))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.respond(pane, request_id.clone(), json!({"decision": decision}), cx);
                    }))
            };
        let primary: Vec<_> = options.iter().filter(|(d, ..)| d == "decline" || d == "accept").collect();
        let more: Rc<Vec<_>> =
            Rc::new(options.iter().filter(|(d, ..)| d != "decline" && d != "accept").cloned().collect());
        let mut actions = h_flex().flex_wrap().items_center().gap_x(px(8.)).gap_y(px(4.));
        if request.is_live() {
            actions = actions
                .child(
                    h_flex()
                        .gap(px(4.))
                        .text_color(t.warning)
                        .child(Icon::new(Lucide::Shield).with_size(px(12.)))
                        .child("Waiting for approval"),
                )
                .child(div().text_color(t.muted_foreground).child("·"));
            for (decision, label, warning) in primary {
                let tone = if decision == "accept" { t.foreground } else { t.muted_foreground };
                actions = actions.child(inline(decision, label, warning.clone(), tone, cx));
            }
            if !more.is_empty() {
                let view = cx.entity();
                let request_id = request.id.clone();
                actions = actions.child(
                    Button::new(ElementId::Name(format!("approval-more-{}", request.id).into()))
                        .ghost()
                        .xsmall()
                        .label("More")
                        .h(px(16.))
                        .px(px(0.))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.muted_foreground)
                        .disabled(!enabled)
                        .dropdown_menu(move |mut menu, window, _| {
                            for (decision, label, warning) in more.iter() {
                                let request_id = request_id.clone();
                                let decision = decision.clone();
                                let item = PopupMenuItem::new(label.clone());
                                let item =
                                    if warning.is_some() { item.icon(Icon::new(Lucide::TriangleAlert)) } else { item };
                                menu = menu.item(item.on_click(window.listener_for(&view, move |this, _, _, cx| {
                                    this.respond(pane, request_id.clone(), json!({"decision": decision}), cx);
                                })));
                            }
                            menu
                        }),
                );
            }
        } else {
            actions = actions.child(
                div().text_color(t.warning).child("Provider process is gone. Interrupt or restart the run to respond."),
            );
        }
        v_flex()
            .px(px(2.))
            .py(px(2.))
            .mb(px(8.))
            .child(
                h_flex()
                    .items_start()
                    .gap(px(6.))
                    .min_h(px(24.))
                    .child(
                        div()
                            .w(px(24.))
                            .h(px(20.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(icon).with_size(px(16.)).text_color(t.muted_foreground.opacity(0.7))),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(format!("approval-detail-{}", request.id).into()))
                            .flex_1()
                            .min_w_0()
                            .max_h(px(160.))
                            .overflow_y_scroll()
                            .font_family(t.fonts.mono)
                            .text_xs()
                            .line_height(px(20.))
                            .text_color(t.foreground)
                            .child(detail),
                    ),
            )
            .child(
                v_flex()
                    .ml(px(30.))
                    .gap(px(4.))
                    .pb(px(4.))
                    .text_xs()
                    .children(app_name.map(|a| div().text_color(t.muted_foreground).child(a)))
                    .children(reason.map(|r| div().text_color(t.muted_foreground).child(r)))
                    .child(actions),
            )
            .into_any_element()
    }

    /// A draft's model moved to `fallbackModelSelection` while the default's provider
    /// is out of usage (`resolveNewThreadModelSelection`); an explicit pick wins.
    pub(super) fn draft_usage_fallback(&self, default: &ModelSelection) -> Option<UsageFallback> {
        let Some(Target::Draft { project_id, .. }) = &self.target else { return None };
        if self.model_override.is_some() || self.server_info().settings.get("fallbackModelSelection").is_null() {
            return None;
        }
        crate::new_thread::usage_fallback(self.server_info(), default, Some(project_id), crate::timestamp::now_ms())
    }

    /// `ComposerBanner.Dock`: the banners tucked under the composer, with the stash tab
    /// on their right.
    fn render_banner_dock(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Each card after the first joins the one above it.
        let mut banners: Vec<AnyElement> = self.render_environment_banner(t, cx).into_iter().collect();
        banners.extend(self.render_fallback_banner(!banners.is_empty(), t, cx));
        banners.extend(self.render_recovery_banner(!banners.is_empty(), t, cx));
        banners.extend(self.render_extension_status_banner(!banners.is_empty(), t));
        banners.extend(self.render_usage_limits_banner(!banners.is_empty(), t, cx));
        banners.extend(self.render_clone_banner(!banners.is_empty(), t, cx));
        banners.extend(self.render_fresh_session_banner(!banners.is_empty(), t, cx));
        banners.extend(self.render_branch_banner(!banners.is_empty(), t, cx));
        banners.extend(self.render_runtime_mode_ceiling_banner(!banners.is_empty(), t, cx));
        let joined = !banners.is_empty();
        banners.extend(self.render_question(ChatPane::Main, joined, t, cx));
        banners.extend(self.render_child_approvals(t, cx));
        let badge = self.render_stash_badge(t, cx);
        if banners.is_empty() && badge.is_none() {
            return None;
        }
        Some(
            h_flex()
                .mx(px(22.))
                .items_end()
                .gap(px(4.))
                .child(v_flex().flex_1().min_w_0().children(banners))
                .children(badge)
                .into_any_element(),
        )
    }

    /// `modelFallbackBannerItem`: "Using X", why, and a way back to the default.
    fn render_fallback_banner(&self, joined: bool, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Target::Draft { env, project_id, .. }) = &self.target else { return None };
        let project = self.shell.projects.iter().find(|p| &p.environment_id == env && &p.id == project_id);
        let default = self.server_info().new_thread_model(project)?;
        let fallback = self.draft_usage_fallback(&default)?;
        let window = format!("{}:{}", fallback.exhausted_instance, fallback.resets_at.as_deref().unwrap_or_default());
        if self.composer_ui.dismissed_fallback.as_ref() == Some(&window) {
            return None;
        }
        let model_name = |selection: &ModelSelection| {
            self.server_info()
                .provider(&selection.instance_id)
                .and_then(|p| p.models.iter().find(|m| m.slug == selection.model))
                .and_then(|m| m.name.clone())
                .unwrap_or_else(|| selection.model.clone())
        };
        let exhausted = self.server_info().provider(&fallback.exhausted_instance)?;
        let provider_name = exhausted.display_name.clone().unwrap_or_else(|| exhausted.driver.clone());
        let hour12 =
            crate::timestamp::hour12(self.client_settings.values.get("timestampFormat").as_str().unwrap_or(""));
        let resets = fallback
            .resets_at
            .as_deref()
            .and_then(|at| crate::timestamp::upcoming_local(at, hour12))
            .map(|at| format!(" · resets {at}"))
            .unwrap_or_default();
        let outline = t.composer_outline();
        let use_default = format!("Use {}", model_name(&default));
        Some(
            div()
                .mb(px(-17.))
                .child(
                    h_flex()
                        .px(px(4.))
                        .pt(px(5.))
                        // 5px padding plus the 17px the composer covers.
                        .pb(px(22.))
                        .gap(px(4.))
                        .border_1()
                        .map(|el| if joined { el.border_t_0() } else { el.rounded_t(px(16.)) })
                        .border_color(outline)
                        .bg(t.composer_glass().opacity(t.glass))
                        .text_xs()
                        .child(
                            div()
                                .w(px(24.))
                                .h(px(24.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(Lucide::Gauge).with_size(px(12.)).text_color(t.muted_foreground)),
                        )
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(px(4.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .line_height(px(24.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(format!("Using {}", model_name(&fallback.selection))),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(t.muted_foreground)
                                        .child(format!("{provider_name} is out of usage{resets}")),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap(px(2.))
                                .child(
                                    Button::new("fallback-use-default").ghost().xsmall().label(use_default).on_click(
                                        cx.listener(move |this, _, window, cx| {
                                            this.pick_model(default.clone(), window, cx);
                                        }),
                                    ),
                                )
                                .child(super::banners::corner_action(
                                    Button::new("fallback-dismiss")
                                        .ghost()
                                        .xsmall()
                                        .icon(Icon::new(Lucide::X))
                                        .tooltip("Dismiss fallback model notice")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.composer_ui.dismissed_fallback = Some(window.clone());
                                            cx.notify();
                                        })),
                                    joined,
                                )),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_question(
        &self,
        pane: ChatPane,
        joined: bool,
        t: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let draft = self.question(pane)?;
        let (request, item) = self.pending_question_request(pane)?;
        let question = &draft.questions[draft.index];
        let disabled = !self.question_answerable(pane);
        let dismissible = item.str("responseMode") == Some("message") || request.capability() == Some("message");
        let collapsed = draft.collapsed;
        let count = draft.questions.len();
        // The toggle, Dismiss and each option are buttons on the web, so Tab reaches them.
        let header = super::chat::focusable(h_flex().id("question-toggle"), t)
            .w_full()
            .min_h(px(24.))
            .px(px(16.))
            .pt(px(6.))
            .gap(px(4.))
            .rounded(px(12.))
            .cursor_pointer()
            .hover(|s| s.bg(t.foreground.opacity(0.05)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(draft) = this.question_mut(pane) {
                    draft.collapsed = !draft.collapsed;
                    cx.notify();
                }
            }))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .pl(px(6.))
                    .gap(px(4.))
                    .child(
                        div()
                            .min_w_0()
                            .when(collapsed, |el| el.max_w(gpui_kit::relative(0.5)))
                            .truncate()
                            .text_color(t.muted_foreground)
                            .child(question.header.clone()),
                    )
                    .when(collapsed, |el| {
                        el.child(div().flex_none().text_color(t.muted_foreground.opacity(0.4)).child("·")).child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(t.muted_foreground)
                                .child(question.question.clone()),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .when(count > 1, |el| {
                        el.child(
                            div()
                                .min_w(px(24.))
                                .flex()
                                .justify_center()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(t.muted_foreground)
                                .child(format!("{}/{count}", draft.index + 1)),
                        )
                    })
                    .child(icon_box(t).child(
                        Icon::new(if collapsed { Lucide::ChevronUp } else { Lucide::ChevronDown }).with_size(px(14.)),
                    ))
                    .when(dismissible, |el| {
                        let request_id = request.id.clone();
                        // Off while an answer or dismissal is on its way, as the web's `isResponding`.
                        let responding = self.pane_responding(pane, &request.id);
                        el.child(
                            super::chat::focusable(icon_box(t).id("question-dismiss"), t)
                                .child(Icon::new(Lucide::X).with_size(px(14.)))
                                .tooltip(|window, cx| {
                                    Tooltip::new("Dismiss question without answering").build(window, cx)
                                })
                                .when(responding, |el| el.opacity(0.64))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_question(pane, request_id.clone(), cx);
                                }))
                                .when(!responding, |el| el.cursor_pointer().hover(|s| s.bg(t.accent))),
                        )
                    }),
            );
        let body = (!collapsed).then(|| {
            let draft_answer = draft.drafts.get(&question.id);
            v_flex()
                .pl(px(22.))
                .pr(px(16.))
                .pb(px(4.))
                .child(
                    div()
                        .pt(px(8.))
                        .text_sm()
                        .line_height(px(LINE_HEIGHT))
                        .text_color(t.foreground.opacity(0.85))
                        .child(TextView::markdown(
                            ElementId::Name(format!("question-{}-{}", request.id, question.id).into()),
                            question.question.clone(),
                        )),
                )
                .child(v_flex().ml(px(-6.)).mt(px(6.)).children(question.options.iter().enumerate().map(
                    |(index, option)| {
                        let value = option.value().to_owned();
                        let selected = requests::is_selected(question, draft_answer, &value);
                        render_option(question, option, index, selected, disabled, t).when(!disabled, |el| {
                            super::chat::focusable(el, t).on_click(cx.listener(move |this, _, window, cx| {
                                this.pick_option(pane, value.clone(), window, cx);
                            }))
                        })
                    },
                )))
        });
        Some(top_drawer(v_flex().child(header).children(body), joined, t))
    }
}

/// The editor's placeholder while a question card holds it.
pub(super) fn question_placeholder(draft: &QuestionDraft) -> &'static str {
    if draft.questions[draft.index].allows_custom() {
        "Type your own answer, or leave this blank to use the selected option"
    } else {
        "Choose an option above"
    }
}

/// The question card's submit (`ComposerPrimaryActions`): its label, and whether it can go. The
/// last question submits once every question has an answer; an earlier one moves on once it has.
pub(super) fn question_submit(
    draft: &QuestionDraft,
    attached: impl Fn(&str) -> Attached,
    responding: bool,
    can_respond: bool,
) -> (&'static str, bool) {
    let last = draft.index + 1 == draft.questions.len();
    let answered = |index: usize| {
        let question = &draft.questions[index];
        requests::answer(question, draft.drafts.get(&question.id), attached(&question.id)).is_some()
    };
    let ready = if last { (0..draft.questions.len()).all(answered) } else { answered(draft.index) };
    let label = match (responding, last) {
        (true, _) => "Submitting...",
        (false, false) => "Next question",
        (false, true) if draft.index > 0 => "Submit answers",
        (false, true) => "Submit answer",
    };
    (label, ready && can_respond && !responding)
}

/// `ComposerBanner.Root variant="info"` in the top drawer, its bottom tucked under the composer.
/// `inert` over a composer card: it takes the pointer, so nothing under it is clicked or selected.
pub(super) fn inert_cover(id: &'static str) -> gpui_kit::Stateful<gpui_kit::Div> {
    div().id(id).absolute().inset_0().block_mouse_except_scroll()
}

pub(super) fn top_drawer(content: impl IntoElement, joined: bool, t: &Tokens) -> AnyElement {
    let outline = t.composer_outline();
    div()
        .mb(px(-17.))
        .child(
            v_flex()
                .px(px(4.))
                .pt(px(4.))
                // 4px padding plus the 17px the composer covers.
                .pb(px(21.))
                .border_1()
                .map(|el| if joined { el.border_t_0() } else { el.rounded_t(px(16.)) })
                .border_color(outline)
                .bg(t.composer_glass().opacity(t.glass))
                .text_xs()
                .line_height(px(16.))
                .shadow(if t.dark {
                    vec![BoxShadow::new(px(0.), px(14.), black(0.75)).blur_radius(px(32.)).spread_radius(px(-18.))]
                } else {
                    vec![BoxShadow::new(px(0.), px(1.), black(0.05)).blur_radius(px(2.))]
                })
                .child(content),
        )
        .into_any_element()
}

fn render_option(
    question: &Question,
    option: &requests::QuestionOption,
    index: usize,
    selected: bool,
    disabled: bool,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let indicator = if question.multi_select {
        div()
            .size(px(16.))
            .rounded(px(4.))
            .border_1()
            .flex()
            .items_center()
            .justify_center()
            .when(selected, |el| {
                el.bg(t.primary)
                    .border_color(t.primary)
                    .child(Icon::new(Lucide::Check).with_size(px(12.)).text_color(t.primary_foreground))
            })
            .when(!selected, |el| {
                el.border_color(t.input).bg(if t.dark { t.input.opacity(0.32) } else { t.background })
            })
    } else {
        div()
            .size(px(16.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .when(selected, |el| el.bg(t.primary).child(div().size(px(6.)).rounded_full().bg(t.primary_foreground)))
            .when(!selected, |el| {
                el.border_1().border_color(t.input).bg(if t.dark { t.input.opacity(0.32) } else { t.background })
            })
    };
    h_flex()
        .id(ElementId::Name(format!("option-{}-{}", question.id, option.value()).into()))
        .w_full()
        .items_start()
        .gap(px(10.))
        .rounded(px(6.))
        .py(px(6.))
        .pl(px(6.))
        .text_color(if selected { t.foreground } else { t.foreground.opacity(0.7) })
        .when(!selected && !disabled, |el| el.hover(|s| s.bg(t.muted.opacity(0.3))))
        .when(disabled, |el| el.opacity(0.5))
        .when(!disabled, gpui_kit::Styled::cursor_pointer)
        .child(div().h(px(20.)).flex().items_center().flex_none().child(indicator))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_sm().line_height(px(20.)).font_weight(FontWeight::MEDIUM).child(option.label.clone()))
                .children(
                    option
                        .description
                        .clone()
                        .filter(|d| *d != option.label)
                        .map(|d| div().text_size(px(11.)).line_height(px(16.)).text_color(t.muted_foreground).child(d)),
                ),
        )
        .when(index < 9, |el| {
            el.child(
                div().w(px(24.)).h(px(20.)).flex_none().flex().items_center().justify_center().child(
                    div()
                        .h(px(20.))
                        .min_w(px(20.))
                        .px(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .bg(t.foreground.opacity(0.07))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(t.muted_foreground)
                        .child(format!("{}", index + 1)),
                ),
            )
        })
}

/// What a chip shows of the record behind it.
struct ChipRecords<'a> {
    pull_requests: &'a HashMap<String, PullRequestContext>,
    /// The context ids the draft holds records for.
    held: &'a HashSet<String>,
    machines: &'a [(String, Lucide)],
    /// What each `model:` chip names.
    models: &'a HashMap<String, ModelMention>,
}

/// What a `model:` chip shows of the model it names (`ModelMentionPresentation`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ModelMention {
    pub label: String,
    /// Its sub-provider, else its provider.
    pub provider_label: String,
    pub driver: String,
}

/// `--context-chip-model`: `oklch(0.62 0.13 105)`.
const MODEL_CHIP_ACCENT: crate::theme_color::Oklch = crate::theme_color::Oklch { l: 0.62, c: 0.13, h: 105. };

/// `logoAccent`: the model chip's lightness with the hue and chroma of the provider logo's own
/// color, for logos that keep one color in both themes; else the model chip's own accent.
#[expect(clippy::cast_possible_truncation, reason = "colour channels; f32 is plenty")]
pub(super) fn model_chip_accent(driver: &str) -> Hsla {
    use crate::theme_color::{Oklch, Rgb, oklch_to_rgb, rgb_to_oklch};
    // `BUNDLED_LOGO_COLOR_BY_PROVIDER` also colors Antigravity, whose logo isn't bundled here.
    let logo = match driver {
        "antigravity" => Some(Hsla::from(rgb(0x5b87bf))),
        _ => crate::assets::provider_icon(driver).and_then(|(_, tint)| tint),
    };
    let color = logo.map_or(MODEL_CHIP_ACCENT, |logo| {
        let logo = logo.to_rgb();
        let [r, g, b] = [logo.r, logo.g, logo.b].map(|channel| f64::from(channel) * 255.);
        Oklch { l: MODEL_CHIP_ACCENT.l, ..rgb_to_oklch(Rgb { r, g, b }) }
    });
    let rgb = oklch_to_rgb(color, false);
    gpui_kit::Rgba { r: (rgb.r / 255.) as f32, g: (rgb.g / 255.) as f32, b: (rgb.b / 255.) as f32, a: 1. }.into()
}

/// The provider logo a model chip leads with, in the logo's own color or else `accent`.
pub(super) fn model_chip_icon(driver: &str, accent: Hsla) -> Icon {
    match crate::assets::provider_icon(driver) {
        Some((asset, tint)) => Icon::empty().path(asset).text_color(tint.unwrap_or(accent)),
        None => Icon::new(Lucide::Bot).text_color(accent),
    }
}

/// `ModelMentionChip`'s details: the model, its provider, and the reference the prompt holds.
pub(super) fn model_mention_details(mention: &ModelMention, source: &str, t: &Tokens) -> gpui_kit::Div {
    v_flex()
        .gap(px(4.))
        .max_w(px(320.))
        .text_sm()
        .child(div().font_weight(FontWeight::MEDIUM).text_color(t.foreground).child(mention.label.clone()))
        .child(div().text_color(t.muted_foreground).child(mention.provider_label.clone()))
        .child(div().font_family(t.fonts.mono).text_xs().text_color(t.muted_foreground).child(source.to_owned()))
}

/// A chip in the editor (`ContextChip`): icon and label in the kind's accent ink.
fn render_chip(token: &InlineTokenContext, records: &ChipRecords, t: &Tokens) -> AnyElement {
    let id = token.token().id();
    let chip = Chip::from_id(id);
    if let Some(Chip::Citation { .. }) = chip {
        let label = token.token().label().clone();
        return super::citations::citation_chip(label, t.fonts.prompt_size, token.is_selected(), t)
            .h(token.line_height())
            .max_w(token.available_width())
            .into_any_element();
    }
    // A record-backed chip whose record is gone sends nothing (`UnresolvedContextChip`);
    // attachments and machines carry their own.
    let unresolved = matches!(&chip, Some(Chip::Context { kind, id, .. })
        if !matches!(kind.as_str(), "image" | "file" | "environment") && !records.held.contains(id));
    // `WorkspaceEntryTooltip`: a mention names its workspace path.
    let tooltip = match &chip {
        _ if unresolved => Some(SharedString::from(UNRESOLVED_CONTEXT)),
        Some(Chip::File { path }) => Some(SharedString::from(path.clone())),
        _ => None,
    };
    if matches!(chip, Some(Chip::Model { .. })) {
        return render_model_chip(token, records.models.get(id.as_ref()), t);
    }
    let (accent, icon) = match chip {
        _ if unresolved => (t.foreground.into(), Lucide::CircleDashed),
        Some(Chip::Skill { .. }) => (rgb(0xb261be), Lucide::Box),
        Some(Chip::Context { kind, id, .. })
            if let Some(pr) = records.pull_requests.get(&id).filter(|_| kind == REVIEW_COMMENT) =>
        {
            (rgb(0x0090cd), pr.state().0)
        }
        Some(Chip::Context { kind, .. }) if kind == REVIEW_COMMENT => (rgb(0x0090cd), Lucide::MessageCircle),
        Some(Chip::Context { kind, .. }) if kind == "preview-annotation" => (rgb(0x0090cd), Lucide::MousePointerClick),
        Some(Chip::Context { kind, .. }) if kind == "image" => (rgb(0x0090cd), Lucide::Image),
        Some(Chip::Context { kind, .. }) if kind == "terminal" => (rgb(0x0090cd), Lucide::Terminal),
        // `EnvironmentContextChip`: the machine's own icon.
        Some(Chip::Context { kind, id, .. }) if kind == "environment" => {
            let environment = id.strip_prefix("environment_").unwrap_or_default();
            let icon =
                records.machines.iter().find(|(id, _)| id == environment).map_or(Lucide::Server, |(_, icon)| *icon);
            (rgb(0x0090cd), icon)
        }
        Some(Chip::Context { .. }) => (rgb(0x0090cd), Lucide::File),
        _ => (rgb(0x0096af), Lucide::File),
    };
    h_flex()
        .h(token.line_height())
        .max_w(token.available_width())
        .gap(px(3.))
        .rounded(px(4.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(accent)
        .when(token.is_selected(), |el| el.bg(t.primary.opacity(0.25)))
        .child(Icon::new(icon).with_size(px(14.)).flex_none())
        .child(div().min_w_0().truncate().child(token.token().label().clone()))
        .id(ElementId::Name(format!("chip-{}-{id}", token.range().start).into()))
        .when_some(tooltip, |el, tip| el.tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx)))
        .into_any_element()
}

const UNRESOLVED_CONTEXT: &str = "This context is no longer available. Remove it or attach it again.";

/// The chip a selection covers exactly, as a click or Shift+Arrow selects one.
fn selected_chip(spans: &[gpui_kit::base::input::InlineTokenSpan], selection: &std::ops::Range<usize>) -> Option<Chip> {
    let span = spans.iter().find(|span| span.range() == *selection)?;
    Chip::from_id(span.token().id())
}

/// Where a composer model chip's popover opens: under the chip.
fn model_chip_menu_at(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> gpui_kit::Point<gpui_kit::Pixels> {
    gpui_kit::point(bounds.left(), bounds.bottom() + px(4.))
}

/// A `model:` chip: the provider's logo and the model's name, its details while hovered. A model
/// the environment no longer offers keeps the name it was picked with.
fn render_model_chip(token: &InlineTokenContext, mention: Option<&ModelMention>, t: &Tokens) -> AnyElement {
    let id = token.token().id();
    let accent = mention.map_or_else(|| model_chip_accent(""), |m| model_chip_accent(&m.driver));
    let icon = model_chip_icon(mention.map_or("", |m| m.driver.as_str()), accent);
    h_flex()
        .h(token.line_height())
        .max_w(token.available_width())
        .gap(px(3.))
        .rounded(px(4.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(accent)
        .when(token.is_selected(), |el| el.bg(t.primary.opacity(0.25)))
        .child(icon.with_size(px(14.)).flex_none())
        .child(div().min_w_0().truncate().child(token.token().label().clone()))
        .id(ElementId::Name(format!("chip-{}-{id}", token.range().start).into()))
        .cursor_pointer()
        .into_any_element()
}

/// The kind of a context record that can carry a change request (`ReviewCommentContextRecord`).
const REVIEW_COMMENT: &str = "review-comment";

/// `PullRequestContextMetadata`: the change request a review-comment chip names.
#[derive(Clone, Debug, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct PullRequestContext {
    pub(super) number: u64,
    title: String,
    pub(super) url: String,
    head_branch: String,
    base_branch: String,
    state: String,
    is_draft: bool,
}

impl PullRequestContext {
    pub(super) fn state(&self) -> (Lucide, &'static str, u32, u32, f32) {
        pull_request_state(&self.state, self.is_draft)
    }

    /// `capitalizeTerm(singular)` for the host the URL names (`findByChangeRequestUrl`).
    fn noun(&self) -> &'static str {
        let path = self.url.split(['?', '#']).next().unwrap_or_default();
        if path.contains("/-/merge_requests/") {
            "Merge request"
        } else if ["/pull/", "/pulls/", "/pull-requests/", "/pullrequest/"].iter().any(|tail| path.contains(tail)) {
            "Pull request"
        } else {
            "Change request"
        }
    }
}

/// `resolvePullRequestState`: the glyph, the label, and the tone as light, dark and dark alpha.
pub(super) fn pull_request_state(state: &str, is_draft: bool) -> (Lucide, &'static str, u32, u32, f32) {
    match state {
        "merged" => (Lucide::GitMerge, "Merged", 0x7f22fe, 0xc4b4ff, 0.9),
        "closed" => (Lucide::GitPullRequestClosed, "Closed", 0xe7000b, 0xffa2a2, 0.9),
        _ if is_draft => (Lucide::GitPullRequestDraft, "Draft", 0x71717b, 0x9f9fa9, 0.8),
        _ => (Lucide::GitPullRequestArrow, "Open", 0x009966, 0x5ee9b5, 0.9),
    }
}

/// [`pull_request_state`]'s tone for the theme.
pub(super) fn pull_request_tone(light: u32, dark: u32, alpha: f32, t: &Tokens) -> Hsla {
    if t.dark { Hsla::from(rgb(dark)).opacity(alpha) } else { rgb(light).into() }
}

/// `PullRequestContextDetails`: the change request's number, state, title and branches.
pub(super) fn pull_request_details(pr: &PullRequestContext, t: &Tokens) -> gpui_kit::Div {
    let (glyph, label, light, dark, alpha) = pr.state();
    let tone = pull_request_tone(light, dark, alpha, t);
    let branch = |name: &str| div().min_w_0().truncate().font_family(t.fonts.mono).child(name.to_owned());
    v_flex()
        .max_w(px(320.))
        .gap(px(4.))
        .py(px(2.))
        .overflow_hidden()
        .child(
            h_flex()
                .gap(px(6.))
                .font_weight(FontWeight::MEDIUM)
                .child(Icon::new(glyph).with_size(px(14.)).flex_none().text_color(tone))
                .child(div().text_color(t.foreground).child(format!("{} #{}", pr.noun(), pr.number)))
                .child(div().text_color(tone).child(label)),
        )
        .child(div().text_color(t.foreground).child(pr.title.clone()))
        .child(
            h_flex()
                .min_w_0()
                .gap(px(4.))
                .text_size(px(10.))
                .text_color(t.muted_foreground)
                .child(branch(&pr.head_branch))
                .child(Icon::new(Lucide::ArrowRight).with_size(px(12.)).flex_none())
                .child(branch(&pr.base_branch)),
        )
}

/// The review-comment records' change requests, by the context id their chips carry.
fn pull_request_contexts<'a>(records: impl IntoIterator<Item = &'a Value>) -> HashMap<String, PullRequestContext> {
    records
        .into_iter()
        .filter(|record| record["kind"].as_str() == Some(REVIEW_COMMENT))
        .filter_map(|record| {
            let id = record["contextId"].as_str()?.to_owned();
            Some((id, serde::Deserialize::deserialize(&record["pullRequest"]).ok()?))
        })
        .collect()
}

impl Workspace {
    /// `PullRequestContextDetails` in a tooltip above the hovered pull request chip.
    fn render_chip_tooltip(
        &self,
        pull_requests: &HashMap<String, PullRequestContext>,
        t: &Tokens,
    ) -> Option<AnyElement> {
        let (chip, bounds) = self.composer_ui.chip_hover.as_ref()?;
        let Some(Chip::Context { kind, id, .. }) = Chip::from_id(chip) else { return None };
        let pr = pull_requests.get(&id).filter(|_| kind == REVIEW_COMMENT)?;
        let details = pull_request_details(pr, t);
        // `TooltipPopup side="top"`, placed in window coordinates over the chip.
        let at = gpui_kit::point(bounds.left(), bounds.top() - px(4.));
        Some(
            gpui_kit::deferred(
                gpui_kit::anchored().anchor(gpui_kit::Anchor::BottomLeft).position(at).child(
                    div()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(t.border)
                        .bg(t.popover)
                        .shadow_md()
                        .px(px(8.))
                        .py(px(4.))
                        .text_xs()
                        .line_height(gpui_kit::relative(1.375))
                        .child(details),
                ),
            )
            .into_any_element(),
        )
    }
}

/// `ComposerControl size="sm"`: 28px tall, 8px padding inside a transparent 1px border,
/// `text-base sm:text-sm` medium. The kit spaces children 4px apart, so a control wanting the
/// web's `gap-1.5` wraps them in its own 6px row.
pub(super) fn control_button(id: &'static str, t: &Tokens) -> Button {
    let button =
        Button::new(id).ghost().small().h(px(28.)).px(px(8.)).border_1().border_color(gpui_kit::transparent_black());
    t.breakpoint.control_text(button).font_weight(FontWeight::MEDIUM).text_color(t.foreground.opacity(0.8))
}

const ATTACH: usize = 0;
const STOP: usize = 1;
pub(super) const PRIMARY: usize = 2;
pub(super) const COMPACT: usize = 3;

/// A `<button>`: a tab stop that Enter and Space press, ringed while keyboard-focused, which a
/// click leaves the composer focused past (`preventPointerFocus`).
pub(super) fn action_control(
    control: gpui_kit::Stateful<gpui_kit::Div>,
    focus: &gpui_kit::FocusHandle,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ring = t.ring;
    control
        .track_focus(focus)
        .focus_visible(move |s| s.border_2().border_color(ring))
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| window.prevent_default())
}

/// The send/stop circle (`h-9 w-9 sm:h-8 sm:w-8`) with its tinted shadow and inset highlight.
fn round_button(id: &'static str, fill: Hsla, shadow: Hsla, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .size(px(t.breakpoint.pick(36., 32.)))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(fill)
        .text_color(gpui_kit::white())
        .shadow(action_shadow(shadow))
}

fn action_shadow(tint: Hsla) -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.), px(1.), tint.opacity(0.24)).blur_radius(px(2.)),
        BoxShadow::new(px(0.), px(1.), white(0.16)).inset(),
    ]
}

fn icon_box(t: &Tokens) -> gpui_kit::Div {
    div().size(px(24.)).flex().items_center().justify_center().rounded(px(8.)).text_color(t.foreground)
}

fn white(alpha: f32) -> Hsla {
    gpui_kit::hsla(0., 0., 1., alpha)
}

fn black(alpha: f32) -> Hsla {
    gpui_kit::hsla(0., 0., 0., alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_submits_once_every_answer_is_in() {
        let question = |id: &str| -> Question {
            serde_json::from_value(json!({"id": id, "header": "H", "question": "?", "options": [{"label": "Red"}]}))
                .unwrap()
        };
        let none = |_: &str| Attached::default();
        let mut draft = QuestionDraft::new("r".into(), vec![question("a"), question("b")]).unwrap();
        assert_eq!(question_submit(&draft, none, false, true), ("Next question", false), "unanswered");
        let ready = |id: &str| Attached { count: usize::from(id == "a"), blocked: false };
        assert_eq!(question_submit(&draft, ready, false, true), ("Next question", true), "attachments answer it");
        let uploading = |id: &str| Attached { count: usize::from(id == "a"), blocked: id == "a" };
        assert_eq!(question_submit(&draft, uploading, false, true), ("Next question", false), "until they upload");
        draft.drafts.insert("a".into(), requests::toggle(&draft.questions[0], None, "Red"));
        assert_eq!(question_submit(&draft, none, false, true), ("Next question", true));
        draft.index = 1;
        assert_eq!(question_submit(&draft, none, false, true), ("Submit answers", false), "the last needs its answer");
        draft.drafts.insert("b".into(), requests::toggle(&draft.questions[1], None, "Red"));
        assert_eq!(question_submit(&draft, none, false, true), ("Submit answers", true));
        assert_eq!(question_submit(&draft, none, true, true), ("Submitting...", false));
        assert_eq!(question_submit(&draft, none, false, false), ("Submit answers", false), "not resumable");
    }

    #[test]
    fn chips_catch_up_once_the_composer_provider_changes() {
        let mut seen = None;
        assert!(note_provider(&mut seen, Some("codex")), "the thread's provider arrives");
        assert!(!note_provider(&mut seen, Some("codex")), "a model change within it");
        assert!(note_provider(&mut seen, Some("claudeAgent")), "another client moved the thread");
        assert_eq!(seen.as_deref(), Some("claudeAgent"));
        assert!(note_provider(&mut seen, None), "its provider went");
        assert!(!note_provider(&mut seen, None));
    }

    #[test]
    fn a_chip_catch_up_refused_mid_composition_runs_once_it_ends() {
        use gpui_kit::base::input::InlineTokenError::{CompositionActive, TextMismatch};
        let mut pending = None;
        let sync = take_chip_sync(&mut pending, ChipSync::Remount);
        assert!(!settle_chip_sync(&mut pending, sync, Err(CompositionActive)), "rich text turned on mid-composition");
        assert_eq!(pending, Some(ChipSync::Remount));
        let sync = take_chip_sync(&mut pending, ChipSync::Lists);
        assert_eq!(sync, ChipSync::Remount, "a narrower catch-up still runs the waiting one");
        assert!(!settle_chip_sync(&mut pending, sync, Err(CompositionActive)), "still composing");
        let sync = take_chip_sync(&mut pending, ChipSync::Restored);
        assert!(settle_chip_sync(&mut pending, sync, Ok(())), "the commit's change runs it");
        assert_eq!((sync, pending), (ChipSync::Remount, None));
        let sync = take_chip_sync(&mut pending, ChipSync::Lists);
        assert!(!settle_chip_sync(&mut pending, sync, Err(TextMismatch)));
        assert_eq!(pending, None, "only a composition defers one");
    }

    #[test]
    fn an_undo_that_brings_back_a_chip_is_noticed() {
        let ids = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
        let gpt = "model:codex/gpt-6.1";
        assert!(adds_chip(&ids(&["skill:review"]), &ids(&["skill:review", gpt])));
        assert!(adds_chip(&ids(&[gpt]), &ids(&[gpt, gpt])), "a second chip for one model");
        assert!(!adds_chip(&ids(&[gpt, gpt]), &ids(&[gpt])));
        assert!(!adds_chip(&ids(&[gpt, "skill:review"]), &ids(&["skill:review", gpt])));
    }

    #[test]
    fn enter_opens_a_model_chip_only_while_the_selection_is_that_chip() {
        let text = "ask model:codex/gpt-6.1 or $review now";
        let content = InputContent::new(text)
            .with_token(4..23, InlineToken::new("model:codex/gpt-6.1", "model:codex/gpt-6.1"))
            .and_then(|content| content.with_token(27..34, InlineToken::new("skill:review", "$review")))
            .unwrap();
        let spans = content.tokens();
        assert!(matches!(selected_chip(spans, &(4..23)), Some(Chip::Model { model, .. }) if model == "gpt-6.1"));
        assert_eq!(selected_chip(spans, &(23..23)), None, "a caret beside it sends");
        assert_eq!(selected_chip(spans, &(0..23)), None, "a wider selection");
        assert_eq!(selected_chip(spans, &(27..34)), Some(Chip::Skill { name: "review".into() }));
    }

    #[test]
    fn review_comment_chips_keep_their_change_request() {
        let pr = |url: &str, state: &str, draft: bool| {
            json!({"number": 7, "title": "Fix login", "url": url, "headBranch": "fix", "baseBranch": "main",
                "state": state, "isDraft": draft})
        };
        let records = [
            json!({"version": 1, "kind": "review-comment", "contextId": "rc-1", "label": "#7",
                "pullRequest": pr("https://github.com/o/r/pull/7", "open", true)}),
            json!({"version": 1, "kind": "review-comment", "contextId": "rc-2", "label": "a.ts:3"}),
            json!({"version": 1, "kind": "terminal", "contextId": "t-1", "label": "x",
                "pullRequest": pr("https://github.com/o/r/pull/7", "open", false)}),
            json!({"version": 1, "kind": "review-comment", "contextId": "rc-3", "label": "!7",
                "pullRequest": pr("https://gitlab.com/g/p/-/merge_requests/7", "merged", false)}),
        ];
        let contexts = pull_request_contexts(&records);
        assert_eq!(contexts.len(), 2, "only review comments that carry one");
        let draft = &contexts["rc-1"];
        assert_eq!((draft.noun(), draft.state().1), ("Pull request", "Draft"));
        let merged = &contexts["rc-3"];
        assert_eq!((merged.noun(), merged.state().1), ("Merge request", "Merged"));
    }

    #[test]
    fn a_failed_send_keeps_newer_text_after_it() {
        assert_eq!(merge_prompt("sent".into(), ""), "sent");
        assert_eq!(merge_prompt("sent".into(), "  "), "sent");
        assert_eq!(merge_prompt("sent".into(), "typed later"), "sent\n\ntyped later");
    }
}
