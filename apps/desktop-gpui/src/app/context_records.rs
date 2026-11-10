//! Each draft's context records (`composerDraftStore`'s terminal contexts, review comments,
//! annotations and machines; `useComposerContextImport.ts`): one record per `t3-context://`
//! chip, sent while its chip is in the text, copied with it and brought back on paste.

use bytes::Bytes;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::input::InlineToken;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, ClipboardEntry, ClipboardItem, Context, SharedString, WeakEntity, Window};
use serde_json::{Value, json};

use super::attachments::{Attachment, MAX_ATTACHMENTS};
use super::composer::ComposerDraft;
use super::{Target, Workspace, new_id};
use crate::composer_context::{self, ContextRecords, Fragment, Imported};
use crate::composer_text::{self, Chip};

#[derive(Default)]
pub(super) struct ContextState {
    /// The open composer's records.
    pub(super) records: ContextRecords,
    /// Records whose chip was deleted, kept until the editor's undo history ends.
    retained: ContextRecords,
    /// The last copy's attachments, so a paste can attach their bytes again.
    copied: Vec<CopiedAttachment>,
}

impl ContextState {
    /// The records leave with the composer's text (a send, a question card taking the editor).
    pub(super) fn set_aside(&mut self) -> ContextRecords {
        self.retained = ContextRecords::default();
        std::mem::take(&mut self.records)
    }

    /// The composer's text was replaced, which ends its undo history: only the records its new
    /// chips name stay.
    fn reset(&mut self, text: &str) {
        self.retained = ContextRecords::default();
        self.records.retain_referenced(text);
    }
}

struct CopiedAttachment {
    environment: SharedString,
    /// The id the copied record names: its upload's, or its local id before that finished.
    attachment_id: String,
    bytes: Bytes,
}

/// The composer's `set_copy_metadata` hook: a copy holding context chips carries their records.
pub(super) fn copy_metadata(view: WeakEntity<Workspace>) -> impl Fn(&str, &mut Window, &mut App) -> Option<String> {
    move |text, _, cx| view.update(cx, |this, _| this.context_fragment(text)).ok().flatten()
}

/// `environmentContextRecord` for each machine the prompt mentions, once; a machine's chip
/// carries all its record holds.
fn environment_records(text: &str) -> Vec<Value> {
    let mut records: Vec<Value> = Vec::new();
    for (_, chip) in composer_text::chips(text, |_| false) {
        let Chip::Context { kind, id, label } = chip else { continue };
        let Some(environment_id) = id.strip_prefix("environment_").filter(|_| kind == "environment") else { continue };
        if records.iter().all(|record| record["contextId"] != id.as_str()) {
            let record = json!({"version": 1, "kind": "environment", "contextId": id, "label": label, "environmentId": environment_id});
            records.push(record);
        }
    }
    records
}

/// `buildMessageContext`'s records besides attachments': each held record `text` references
/// and each machine it mentions, grouped by kind in the web's order, then as the text names them.
fn message_records(held: &ContextRecords, text: &str) -> Vec<Value> {
    const ORDER: [&str; 5] = ["terminal", "review-comment", "thread", "environment", "preview-annotation"];
    let mut records = held.referenced(text);
    let sent = |record: &Value| records.iter().any(|r| r["contextId"] == record["contextId"]);
    let machines: Vec<Value> = environment_records(text).into_iter().filter(|r| !sent(r)).collect();
    records.extend(machines);
    records.sort_by_key(|record| ORDER.iter().position(|kind| record["kind"] == *kind).unwrap_or(ORDER.len()));
    records
}

/// Where a chip goes into a set-aside prompt: after its text, a space apart.
fn append_chip(prompt: &mut String, link: &str) {
    if !prompt.is_empty() && !prompt.ends_with(char::is_whitespace) {
        prompt.push(' ');
    }
    prompt.push_str(link);
    prompt.push(' ');
}

impl Workspace {
    /// Adds `record` to `target`'s draft with its chip: at the caret of the composer on screen,
    /// else after the set-aside prompt. The chip's text, or `None` when the draft holds an open
    /// question, which takes no context.
    pub(super) fn add_context_record(
        &mut self,
        target: &Target,
        record: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let (Some(kind), Some(id)) = (record["kind"].as_str(), record["contextId"].as_str()) else { return None };
        let label = record["label"].as_str().unwrap_or_default().to_owned();
        let link = composer_text::context_link(kind, id, &label);
        let chip = Chip::Context { kind: kind.to_owned(), id: id.to_owned(), label };
        if self.target.as_ref() == Some(target) {
            if self.question.is_some() {
                return None;
            }
            self.context.records.insert(record);
            self.insert_chip_at_caret(&chip, link.clone(), window, cx);
            return Some(link);
        }
        match self.composer_drafts.entry(target.clone()).or_insert_with(ComposerDraft::empty) {
            ComposerDraft::Prompt(prompt, _, records) => {
                records.insert(record);
                append_chip(prompt, &link);
            }
            ComposerDraft::Question(_) => return None,
        }
        cx.notify();
        Some(link)
    }

    /// Whether `target`'s draft takes new context now: not while a question card holds it.
    pub(super) fn takes_context(&self, target: &Target) -> bool {
        if self.target.as_ref() == Some(target) {
            return self.question.is_none();
        }
        matches!(self.composer_drafts.get(target), Some(ComposerDraft::Prompt(..)))
    }

    /// The records of `target`'s prompt: the composer's on screen, else the set-aside prompt's
    /// (a draft not on screen, or the prompt a question card holds).
    pub(super) fn draft_records(&self, target: &Target) -> Option<&ContextRecords> {
        if self.target.as_ref() == Some(target) {
            return Some(self.question.as_ref().map_or(&self.context.records, |question| &question.prompt_records));
        }
        match self.composer_drafts.get(target)? {
            ComposerDraft::Prompt(_, _, records) => Some(records),
            ComposerDraft::Question(question) => Some(&question.prompt_records),
        }
    }

    /// `target`'s prompt and records when the editor doesn't hold them: a draft not on screen,
    /// or the prompt a question card set aside.
    fn set_aside_prompt(&mut self, target: &Target) -> Option<(&mut String, &mut ContextRecords)> {
        if self.target.as_ref() == Some(target) {
            return self.question.as_mut().map(|question| (&mut question.prompt, &mut question.prompt_records));
        }
        match self.composer_drafts.get_mut(target)? {
            ComposerDraft::Prompt(prompt, _, records) => Some((prompt, records)),
            ComposerDraft::Question(question) => Some((&mut question.prompt, &mut question.prompt_records)),
        }
    }

    /// Replaces a record `target`'s draft holds (a comment whose lines moved), keeping its chip.
    pub(super) fn replace_context_record(&mut self, target: &Target, record: Value, cx: &mut Context<Self>) {
        let replaced = if self.target.as_ref() == Some(target) && self.question.is_none() {
            self.context.records.replace(record)
        } else {
            self.set_aside_prompt(target).is_some_and(|(_, records)| records.replace(record))
        };
        if replaced {
            cx.notify();
        }
    }

    /// Removes a record from `target`'s draft with its chips (`removeReviewComment`). On screen
    /// the edit is one undo step, which brings both back.
    pub(super) fn remove_context_record(
        &mut self,
        target: &Target,
        kind: &str,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chip = Chip::Context { kind: kind.to_owned(), id: id.to_owned(), label: String::new() }.id();
        if self.target.as_ref() == Some(target) && self.question.is_none() {
            // Every reference, chip or not (rich text shows a fenced one as code), in one edit.
            let removals = composer_text::context_reference_removals(&self.composer.read(cx).value(), &chip);
            if let Some(first) = removals.last().map(|range| range.start) {
                let edits: Vec<_> = removals.into_iter().map(|range| (range, String::new())).collect();
                self.composer.update(cx, |state, cx| state.replace_ranges(&edits, first..first, window, cx));
                self.sync_context_chips(cx);
            }
            self.context.records.remove(id);
            self.sync_composer_empty(cx);
        } else if let Some((prompt, records)) = self.set_aside_prompt(target) {
            for range in composer_text::context_reference_removals(prompt, &chip) {
                prompt.replace_range(range, "");
            }
            records.remove(id);
        }
        cx.notify();
    }

    /// A chip at the caret, a space apart from the text around it.
    pub(super) fn insert_chip_at_caret(
        &mut self,
        chip: &Chip,
        link: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.composer.update(cx, |state, cx| {
            let at = state.cursor();
            if at > 0 && !state.value()[..at].ends_with(char::is_whitespace) {
                state.insert(" ", window, cx);
            }
            let at = state.cursor();
            let token = InlineToken::new(chip.id(), link).with_label(chip.label().to_owned());
            if state.replace_range_with_token(at..at, token, window, cx).is_ok() {
                state.insert(" ", window, cx);
            }
        });
        self.sync_composer_empty(cx);
        cx.notify();
    }

    /// The records a message with `text` sends besides its attachments' (`buildMessageContext`).
    pub(super) fn message_context_records(&self, text: &str) -> Vec<Value> {
        message_records(&self.context.records, text)
    }

    /// The open composer's records, which leave with its text.
    pub(super) fn take_context_records(&mut self) -> ContextRecords {
        self.context.set_aside()
    }

    /// Records going back to the draft they were taken from (a failed send, a cancelled edit),
    /// ahead of its text so the chips find them.
    pub(super) fn give_back_context_records(&mut self, origin: &Target, records: ContextRecords) {
        if records.is_empty() {
            return;
        }
        if self.target.as_ref() == Some(origin) && self.question.is_none() {
            self.context.records.extend(records.iter().cloned());
            return;
        }
        if self.target.as_ref() != Some(origin) {
            self.composer_drafts.entry(origin.clone()).or_insert_with(ComposerDraft::empty);
        }
        if let Some((_, held)) = self.set_aside_prompt(origin) {
            held.extend(records.iter().cloned());
        }
    }

    /// After a user edit: a record whose chip left is set aside, one whose chip an undo
    /// brought back returns.
    pub(super) fn sync_context_chips(&mut self, cx: &App) {
        let ContextState { records, retained, .. } = &mut self.context;
        if !records.is_empty() || !retained.is_empty() {
            records.reconcile(retained, &self.composer.read(cx).value());
        }
    }

    /// The composer's text was replaced, which ends its undo history: only the records its new
    /// chips name stay.
    pub(super) fn reset_context_chips(&mut self, text: &str) {
        self.context.reset(text);
    }

    /// The clipboard fragment for copied `text` (`buildContextClipboardFragment`), remembering
    /// its attachments' bytes for a paste; `None` when it holds no context.
    fn context_fragment(&mut self, text: &str) -> Option<String> {
        let ids = composer_context::referenced_ids(text);
        if ids.is_empty() || self.question.is_some() {
            return None;
        }
        let environment = self.active_environment()?.clone();
        let mut records = self.message_context_records(text);
        // An annotation's screenshot rides along with it.
        let screenshots: Vec<String> =
            records.iter().filter_map(|r| r["screenshotContextId"].as_str().map(str::to_owned)).collect();
        let mut copied = Vec::new();
        for attachment in &self.attachments {
            let context_id = attachment.context_id();
            if !ids.contains(&context_id.as_str()) && !screenshots.contains(&context_id) {
                continue;
            }
            let attachment_id = attachment.upload_id().unwrap_or(attachment.local_id()).to_owned();
            records.push(attachment.context_record(&attachment_id));
            let bytes = attachment.bytes().clone();
            copied.push(CopiedAttachment { environment: environment.clone(), attachment_id, bytes });
        }
        if records.is_empty() {
            return None;
        }
        let thread_id = match &self.target {
            Some(Target::Thread { id, .. }) => Some(id.clone()),
            _ => None,
        };
        let encoded = composer_context::encode_fragment(&Fragment {
            environment_id: environment.to_string(),
            thread_id,
            records,
        })?;
        self.context.copied = copied;
        Some(encoded)
    }

    /// The open draft's record for a context id, an attachment's included.
    fn held_context_record(&self, id: &str) -> Option<Value> {
        self.context.records.get(id).cloned().or_else(|| {
            let attachment = self.attachments.iter().find(|a| a.context_id() == id)?;
            Some(attachment.context_record(attachment.upload_id().unwrap_or(attachment.local_id())))
        })
    }

    /// A paste whose clipboard entry carries copied context (`importPastedComposerText`): its
    /// records join the draft under ids that don't collide, its chips follow them, and its
    /// attachments arrive as new ones. `false` leaves the paste to the other handlers.
    pub(super) fn paste_context(&mut self, item: &ClipboardItem, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let pasted = item.entries().iter().find_map(|entry| match entry {
            ClipboardEntry::String(text) => {
                let fragment = composer_context::decode_fragment(text.metadata.as_deref()?)?;
                Some((text.text().to_owned(), fragment))
            }
            _ => None,
        });
        let Some((text, fragment)) = pasted.filter(|(_, fragment)| !fragment.records.is_empty()) else { return false };
        if self.question.is_some() || self.target.is_none() {
            return false;
        }
        let here = self.active_environment().cloned();
        let can_attach = self.can_attach();
        let import = composer_context::plan_import(
            &text,
            &fragment.records,
            |id| self.held_context_record(id),
            |record| match record["kind"].as_str() {
                // An agent reads threads only on its own machine.
                Some("thread") => record["environmentId"].as_str() == here.as_deref(),
                Some("image" | "file") => can_attach,
                _ => true,
            },
            new_id,
        );
        let text = composer_context::rewrite_ids(&text, &import.ids);
        let mut attachments = Vec::new();
        for item in import.items {
            match item {
                Imported::Record(record) => self.context.records.insert(record),
                Imported::Attachment { record, local_id } => attachments.push((record, local_id)),
            }
        }
        if !self.paste_chips(&text, window, cx) {
            self.composer.update(cx, |state, cx| state.replace(text, window, cx));
        }
        let source: SharedString = fragment.environment_id.into();
        for (record, local_id) in attachments {
            self.import_attachment(&source, &record, local_id, window, cx);
        }
        true
    }

    /// A pasted attachment record's bytes: the copy's own, else downloaded from the machine it
    /// came from (`runAttachmentImport`). They land in the draft the paste went to.
    fn import_attachment(
        &mut self,
        source: &SharedString,
        record: &Value,
        local_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(origin) = self.target.clone() else { return };
        let field = |key: &str| record[key].as_str().unwrap_or_default().to_owned();
        let (kind, attachment_id, name, mime) =
            (field("kind"), field("attachmentId"), field("name"), field("mimeType"));
        let copied = self.context.copied.iter().find(|c| c.environment == *source && c.attachment_id == attachment_id);
        if let Some(bytes) = copied.map(|c| c.bytes.to_vec()) {
            let imported = ImportedFile { kind, name, mime, local_id };
            return self.attach_imported(&origin, imported, bytes, window, cx);
        }
        let connected = self.env_client(source).zip(self.env_endpoint(source).map(|e| e.http_base.clone()));
        let Some((client, http_base)) = connected else {
            return import_failed(&name, "The environment it came from is not connected.", window, cx);
        };
        let Some(reading) = self.attachment_origin() else { return };
        self.reading_attachments.push(reading.clone());
        cx.notify();
        let file = json!({"type": kind, "id": attachment_id, "name": name, "mimeType": mime});
        let imported = ImportedFile { kind, name, mime, local_id };
        cx.spawn_in(window, async move |this, cx| {
            let downloaded = super::message_actions::download_attachments(&client, &http_base, vec![file]).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(at) = this.reading_attachments.iter().position(|busy| *busy == reading) {
                    this.reading_attachments.swap_remove(at);
                }
                match downloaded.ok().and_then(|mut files| files.pop()) {
                    Some(file) => this.attach_imported(&origin, imported, file.bytes().to_vec(), window, cx),
                    None => import_failed(&imported.name, "Downloading it from the source failed.", window, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Attaches imported bytes to `origin`'s draft, on screen or set aside, while its chip is
    /// still there; bytes for a chip the user deleted meanwhile are dropped.
    fn attach_imported(
        &mut self,
        origin: &Target,
        file: ImportedFile,
        bytes: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let context_id = format!("{}_{}", file.kind, file.local_id);
        let text = self.composer.read(cx).value().to_string();
        let on_screen = (self.target.as_ref(), text.as_str(), &self.context.records, &mut self.attachments);
        let Some(slot) = import_slot(origin, &context_id, on_screen, &mut self.composer_drafts) else { return };
        if slot.len() >= MAX_ATTACHMENTS {
            return import_failed(&file.name, "The attachment limit has been reached.", window, cx);
        }
        slot.push(Attachment::restored_as(file.local_id.clone(), &file.kind, file.name, file.mime, bytes));
        self.upload(file.local_id, cx);
        cx.notify();
    }
}

/// The attachments of `origin`'s draft, on screen or set aside, while it still wants attachment
/// `context_id`; `None` once the chip is gone.
fn import_slot<'a>(
    origin: &Target,
    context_id: &str,
    (active, text, records, attachments): (Option<&Target>, &str, &ContextRecords, &'a mut Vec<Attachment>),
    drafts: &'a mut std::collections::HashMap<Target, ComposerDraft>,
) -> Option<&'a mut Vec<Attachment>> {
    if active == Some(origin) {
        return wants_attachment(text, records, context_id).then_some(attachments);
    }
    match drafts.get_mut(origin) {
        Some(ComposerDraft::Prompt(prompt, kept, records)) if wants_attachment(prompt, records, context_id) => {
            Some(kept)
        }
        _ => None,
    }
}

/// Whether `text` still references attachment `context_id`: its own chip, or the chip of an
/// annotation that owns it as its screenshot, which has no chip of its own.
fn wants_attachment(text: &str, records: &ContextRecords, context_id: &str) -> bool {
    let live = composer_context::referenced_ids(text);
    live.contains(&context_id)
        || records.iter().any(|record| {
            record["kind"] == "preview-annotation"
                && record["screenshotContextId"] == context_id
                && record["contextId"].as_str().is_some_and(|id| live.contains(&id))
        })
}

/// What a pasted attachment record becomes once its bytes arrive.
struct ImportedFile {
    kind: String,
    name: String,
    mime: String,
    local_id: String,
}

fn import_failed(name: &str, reason: &str, window: &mut Window, cx: &mut Context<Workspace>) {
    let toast = Notification::error(format!("{reason} Remove the chip or attach the file again."))
        .title(format!("Couldn't bring {name} into this message"));
    window.push_notification(toast, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_card_keeps_the_set_aside_prompts_records() {
        let link = composer_text::context_link("terminal", "terminal_t1", "Shell");
        let prompt = format!("see {link}");
        let mut context = ContextState::default();
        context.records.insert(json!({"version": 1, "kind": "terminal", "contextId": "terminal_t1", "label": "Shell"}));
        // The card takes the editor, which then shows the answer.
        let parked = context.set_aside();
        context.reset("the answer");
        // It closes: the prompt and its records come back.
        context.records = parked;
        context.reset(&prompt);
        assert!(context.records.get("terminal_t1").is_some(), "the restored chip still sends its terminal output");
    }

    #[test]
    fn a_machine_mention_sends_its_environment_record_once() {
        let link = composer_text::context_link("environment", "environment_e2", "Build box");
        let records = environment_records(&format!("run on {link} and {link}"));
        assert_eq!(
            records,
            [
                json!({"version": 1, "kind": "environment", "contextId": "environment_e2", "label": "Build box", "environmentId": "e2"})
            ]
        );
        let terminal = composer_text::context_link("terminal", "terminal_t1", "Shell");
        assert!(environment_records(&format!("see {terminal}")).is_empty());
    }

    #[test]
    fn a_message_sends_every_kind_whose_chip_is_in_its_text() {
        let record = |kind: &str, id: &str| json!({"version": 1, "kind": kind, "contextId": id, "label": id});
        let mut held = ContextRecords::default();
        held.extend([
            record("preview-annotation", "preview-annotation_a1"),
            record("review-comment", "review-comment_c1"),
            record("terminal", "terminal_t1"),
            record("review-comment", "review-comment_gone"),
        ]);
        let link = |kind: &str, id: &str| composer_text::context_link(kind, id, id);
        let text = [
            link("preview-annotation", "preview-annotation_a1"),
            link("environment", "environment_e1"),
            link("review-comment", "review-comment_c1"),
            link("terminal", "terminal_t1"),
            link("terminal", "terminal_unknown"),
        ]
        .join(" ");
        let sent: Vec<_> = message_records(&held, &text).iter().map(|r| r["contextId"].clone()).collect();
        assert_eq!(
            sent,
            ["terminal_t1", "review-comment_c1", "environment_e1", "preview-annotation_a1"],
            "in `buildMessageContext`'s kind order; a pruned or unknown record sends nothing"
        );
        assert!(message_records(&ContextRecords::default(), "plain").is_empty());
    }

    #[test]
    fn pasted_bytes_land_in_the_draft_the_paste_went_to() {
        let (a, b) =
            (Target::Thread { env: "e".into(), id: "a".into() }, Target::Thread { env: "e".into(), id: "b".into() });
        let link = composer_text::context_link("file", "file_l1", "x");
        let none = ContextRecords::default();
        let mut drafts = std::collections::HashMap::from([(
            a.clone(),
            ComposerDraft::Prompt(format!("see {link} "), Vec::new(), ContextRecords::default()),
        )]);
        let mut on_screen = Vec::new();
        // The user moved from a to b while the bytes downloaded.
        let slot = import_slot(&a, "file_l1", (Some(&b), "b's text", &none, &mut on_screen), &mut drafts).unwrap();
        slot.push(Attachment::restored_as("l1".into(), "file", "x".into(), "text/plain".into(), vec![1]));
        assert!(on_screen.is_empty(), "nothing lands in the draft on screen");
        let Some(ComposerDraft::Prompt(_, kept, _)) = drafts.get(&a) else { panic!() };
        assert_eq!(kept.len(), 1);

        assert!(import_slot(&b, "file_l1", (Some(&b), "chip deleted", &none, &mut on_screen), &mut drafts).is_none());
        assert!(import_slot(&b, "file_l1", (Some(&b), &link, &none, &mut on_screen), &mut drafts).is_some());
        assert!(
            import_slot(&b, "file_l1", (Some(&a), "", &none, &mut on_screen), &mut drafts).is_none(),
            "a draft left with nothing"
        );

        // An annotation's screenshot has no chip of its own; the annotation's chip keeps it wanted.
        let annotation = composer_text::context_link("preview-annotation", "preview-annotation_p1", "Button");
        let mut records = ContextRecords::default();
        records.insert(json!({"version": 1, "kind": "preview-annotation", "contextId": "preview-annotation_p1",
            "label": "Button", "screenshotContextId": "image_n2"}));
        let on = |text: &str, records: &ContextRecords, on_screen: &mut Vec<Attachment>, drafts: &mut _| {
            import_slot(&b, "image_n2", (Some(&b), text, records, on_screen), drafts).is_some()
        };
        assert!(on(&annotation, &records, &mut on_screen, &mut drafts), "the screenshot joins its annotation");
        assert!(!on("annotation deleted", &records, &mut on_screen, &mut drafts));
        assert!(!on(&annotation, &none, &mut on_screen, &mut drafts), "only an annotation that owns it");
        drafts.insert(a.clone(), ComposerDraft::Prompt(annotation, Vec::new(), records));
        assert!(
            import_slot(&a, "image_n2", (Some(&b), "", &none, &mut on_screen), &mut drafts).is_some(),
            "a set-aside draft's annotation keeps it too"
        );
    }

    #[test]
    fn a_chip_for_a_set_aside_draft_goes_after_its_text() {
        let mut prompt = "fix".to_owned();
        append_chip(&mut prompt, "[#7](t3-context://v1/review-comment/rc)");
        assert_eq!(prompt, "fix [#7](t3-context://v1/review-comment/rc) ");
        let mut empty = String::new();
        append_chip(&mut empty, "[a](t3-context://v1/terminal/t)");
        assert_eq!(empty, "[a](t3-context://v1/terminal/t) ");
    }
}
