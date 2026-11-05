//! The composer (`ChatComposer.tsx`, `ComposerSurface.tsx`), the pending
//! approval rows above it (`TimelinePendingApprovalRow.tsx`) and the question
//! banner attached to it (`ComposerPendingUserInputPanel.tsx`).

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InlineToken, InlineTokenContext, InputContent, Textarea};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, BoxShadow, ClickEvent, Context, ElementId, Focusable as _, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, Keystroke, ModifiersChangedEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _, px, rgb, svg,
};
use serde_json::{Value, json};

use super::attachments::Attachment;
use super::send::Intent;
use super::{Target, Workspace, new_id};
use crate::composer_text::{self, Chip};
use crate::model::{ModelSelection, RuntimeRequest, TurnItem};
use crate::new_thread::UsageFallback;
use crate::requests::{self, Question, QuestionDraft};
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

/// A failed message first, then whatever was typed after it.
fn merge_prompt(failed: String, newer: &str) -> String {
    if newer.trim().is_empty() { failed } else { format!("{failed}\n\n{newer}") }
}

/// What a target keeps of the composer while another one is on screen, like
/// the web's `composerDraftStore` (in memory only).
pub(super) enum ComposerDraft {
    Prompt(String, Vec<Attachment>),
    /// An open question card; its prompt is the one it set aside.
    Question(QuestionDraft),
}

impl Workspace {
    /// A user edit. While a question is open the editor holds its custom answer;
    /// programmatic changes (`set_composer_text`) never reach the draft.
    pub(super) fn on_composer_change(&mut self, cx: &mut Context<Self>) {
        self.close_stash_menu(cx);
        if self.composer_ui.length_error.is_some() {
            let error = composer_text::prompt_length_error(&self.composer.read(cx).value());
            if error != self.composer_ui.length_error {
                self.composer_ui.length_error = error;
                cx.notify();
            }
        }
        self.save_custom_answer(cx);
        self.sync_composer_empty(cx);
        let state = self.composer.read(cx);
        if self.prompt_history.as_ref().is_some_and(|h| *h.recalled != *state.value()) {
            self.prompt_history = None;
        }
        let chips: Vec<String> = state.tokens().iter().map(|s| s.token().id().to_string()).collect();
        let previous = std::mem::replace(&mut self.chip_ids, chips);
        self.sync_attachment_chips(&previous, cx);
        self.sync_trigger(cx);
    }

    pub(super) fn sync_composer_empty(&mut self, cx: &mut Context<Self>) {
        let empty = self.composer.read(cx).text().len() == 0;
        if empty != self.composer_empty {
            self.composer_empty = empty;
            cx.notify();
        }
    }

    /// `set_value` emits no `Change`, so this keeps `composer_empty` in step.
    /// File links, known skills and context links in `text` come back as chips.
    pub(super) fn set_composer_text(
        &mut self,
        text: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text: SharedString = text.into();
        let skills = self.skill_names();
        let mut content = InputContent::new(text.clone());
        for (range, chip) in composer_text::chips(&text, |name| skills.iter().any(|s| s == name)) {
            let token = InlineToken::new(chip.id(), &text[range.clone()]).with_label(chip.label().to_owned());
            content = match content.clone().with_token(range, token) {
                Ok(next) => next,
                Err(_) => content,
            };
        }
        self.chip_ids = content.tokens().iter().map(|s| s.token().id().to_string()).collect();
        self.composer.update(cx, |composer, cx| composer.set_value(content, window, cx));
        self.sync_composer_empty(cx);
        self.sync_trigger(cx);
    }

    /// The user-invocable skills of the composer's provider, by name.
    fn skill_names(&self) -> Vec<String> {
        let Some(provider) = self.composer_model().and_then(|s| self.server_info.provider(&s.instance_id)) else {
            return Vec::new();
        };
        let cwd = self.composer_cwd();
        let (skills, _) = crate::composer_search::provider_lists(provider, cwd.as_deref());
        crate::composer_search::invocable(skills).into_iter().map(|s| s.name.clone()).collect()
    }

    /// Keeps the leaving target's composer; call before the target changes.
    pub(super) fn stash_composer(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        let attachments = std::mem::take(&mut self.attachments);
        let draft = match self.question.take() {
            Some(question) => ComposerDraft::Question(question),
            None => ComposerDraft::Prompt(self.composer.read(cx).value().to_string(), attachments),
        };
        self.prompt_history = None;
        self.menu = Default::default();
        self.prompt_stash.open = false;
        self.picker.dismiss();
        if matches!(&draft, ComposerDraft::Prompt(prompt, attachments) if prompt.is_empty() && attachments.is_empty()) {
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
        if self.target.as_ref() == Some(origin) {
            match &mut self.question {
                Some(question) => question.prompt = merge_prompt(text, &question.prompt),
                None => {
                    let merged = merge_prompt(text, &self.composer.read(cx).value());
                    self.set_composer_text(merged, window, cx);
                }
            }
            return;
        }
        let slot = match self.composer_drafts.get_mut(origin) {
            Some(ComposerDraft::Prompt(prompt, _)) => prompt,
            Some(ComposerDraft::Question(question)) => &mut question.prompt,
            None => {
                self.composer_drafts.insert(origin.clone(), ComposerDraft::Prompt(text, Vec::new()));
                return;
            }
        };
        *slot = merge_prompt(text, slot);
    }

    /// Brings back the new target's composer; call after the target changes.
    /// `sync_question` drops a restored question once the thread shows it resolved.
    pub(super) fn restore_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_draft_workspace(cx);
        match self.target.as_ref().and_then(|target| self.composer_drafts.remove(target)) {
            Some(ComposerDraft::Question(question)) => {
                self.question = Some(question);
                self.load_custom_answer(window, cx);
            }
            Some(ComposerDraft::Prompt(prompt, attachments)) => {
                self.question = None;
                self.attachments = attachments;
                self.set_composer_text(prompt, window, cx);
            }
            None => {
                self.question = None;
                self.set_composer_text("", window, cx);
            }
        }
    }

    /// Every keystroke, before key bindings run (the editor binds arrows, Tab and
    /// Enter itself); `true` consumes it.
    pub(super) fn intercept_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.model_picker_open() {
            return self.picker_key(keystroke, window, cx);
        }
        if self.stash_menu_key(keystroke, window, cx)
            || self.draft_workspace_key(keystroke, window, cx)
            || self.composer_shortcut(keystroke, window, cx)
        {
            return true;
        }
        // Keys in the editor: Shift+Tab, the open menu, then prompt history, then the send shortcuts.
        self.composer.focus_handle(cx).is_focused(window)
            && (self.plan_toggle_key(keystroke, window, cx)
                || self.menu_key(keystroke, window, cx)
                || self.history_key(keystroke, window, cx)
                || self.send_key(keystroke, window, cx))
    }

    /// Shift+Tab switches between Build and Plan (`composerCommandKey.ts`).
    fn plan_toggle_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = keystroke.modifiers;
        if keystroke.key != "tab" || !m.shift || m.control || m.alt || m.platform || !self.interaction_mode().0 {
            return false;
        }
        self.toggle_interaction_mode(window, cx);
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
        let messages =
            projection.visible_turn_items.iter().map(|row| &row.item).filter(|item| {
                item.item_type == "user_message" && item.raw.get("attribution").is_none_or(Value::is_null)
            });
        composer_text::history_entries(messages.filter_map(|item| item.str("text")))
    }

    /// Enter sends unless `sendShortcut` asks for Ctrl; Ctrl+Enter takes the alternate path.
    fn send_key(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let running = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()).is_some_and(|p| p.is_running());
        let draft = matches!(self.target, Some(Target::Draft { .. }));
        let shortcut = self.client_settings.values.get("sendShortcut").as_str().unwrap_or("enter");
        let prompt = self.composer.read(cx).value();
        let Some(intent) = super::send::key_intent(keystroke, shortcut, &prompt, running, draft) else { return false };
        self.submit_with(intent, window, cx);
        true
    }

    /// Shortcuts that work wherever focus is (`keybindings.ts`): the model picker,
    /// permissions and branch pickers, and 1-9 on an open question card.
    fn composer_shortcut(&mut self, keystroke: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let m = keystroke.modifiers;
        if (m.control || m.platform) && m.shift && !m.alt && self.page.is_none() && self.settings.is_none() {
            match keystroke.key.as_str() {
                "m" | "e" => {
                    if self.composer_picker_open() {
                        self.close_model_picker(window, cx);
                    } else {
                        self.open_model_picker(window, cx);
                    }
                }
                "a" if matches!(self.target, Some(Target::Draft { .. })) => {
                    self.draft_workspace_open_permissions(cx);
                }
                "a" => self.open_model_picker(window, cx),
                _ => return false,
            }
            return true;
        }
        if m.control || m.platform || m.alt {
            return false;
        }
        let Some(digit) = keystroke.key.parse::<usize>().ok().filter(|d| (1..=9).contains(d)) else { return false };
        if self.composer.focus_handle(cx).is_focused(window) {
            return false;
        }
        let Some(draft) = &self.question else { return false };
        if draft.collapsed
            || self.pending_question_request().is_none_or(|(r, _)| r.capability() == Some("not_resumable"))
        {
            return false;
        }
        let Some(option) = draft.questions[draft.index].options.get(digit - 1) else { return false };
        let value = option.value().to_owned();
        self.pick_option(value, window, cx);
        true
    }

    /// Opens a fresh draft when the first pending question changes. The editor
    /// then holds the question's answer, and the prompt waits in the draft
    /// until the last question closes.
    pub(super) fn sync_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else { return };
        let first = projection.pending_user_inputs().into_iter().next();
        if first.map(|(request, _)| &request.id) == self.question.as_ref().map(|q| &q.request_id) {
            return;
        }
        let next = first
            .map(|(request, item)| (request.id.clone(), requests::questions(item)))
            .filter(|(_, questions)| !questions.is_empty());
        match (self.question.take(), next) {
            (None, None) => {}
            (Some(previous), None) => self.set_composer_text(previous.prompt, window, cx),
            (previous, Some((request_id, questions))) => {
                let prompt = match previous {
                    Some(previous) => previous.prompt,
                    None => self.composer.read(cx).value().to_string(),
                };
                self.question = Some(QuestionDraft { prompt, ..QuestionDraft::new(request_id, questions) });
                self.load_custom_answer(window, cx);
            }
        }
    }

    fn pending_question_request(&self) -> Option<(&RuntimeRequest, &TurnItem)> {
        let id = &self.question.as_ref()?.request_id;
        let projection = self.thread.as_ref()?.state.projection.as_ref()?;
        projection.pending_user_inputs().into_iter().find(|(request, _)| &request.id == id)
    }

    fn save_custom_answer(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        let Some(draft) = &mut self.question else { return };
        let question = &draft.questions[draft.index];
        if text.is_empty() && !draft.drafts.contains_key(&question.id) {
            return;
        }
        let next = requests::set_custom(question, draft.drafts.get(&question.id), text);
        draft.drafts.insert(question.id.clone(), next);
    }

    fn load_custom_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.question else { return };
        let question = &draft.questions[draft.index];
        let text = draft.drafts.get(&question.id).and_then(|d| d.custom.clone()).unwrap_or_default();
        self.set_composer_text(text, window, cx);
    }

    /// Next question, or submit on the last; a no-op while the active one is unanswered.
    pub(super) fn advance_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &mut self.question else { return };
        let question = &draft.questions[draft.index];
        if requests::answer(question, draft.drafts.get(&question.id)).is_none() {
            return;
        }
        if draft.index + 1 < draft.questions.len() {
            draft.index += 1;
            self.load_custom_answer(window, cx);
            cx.notify();
            return;
        }
        let Some(answers) = requests::answers(&draft.questions, &draft.drafts) else { return };
        let request_id = draft.request_id.clone();
        if self.responding.is_some() {
            return;
        }
        self.respond(request_id, json!({"answers": answers}), cx);
    }

    fn previous_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &mut self.question else { return };
        draft.index = draft.index.saturating_sub(1);
        self.load_custom_answer(window, cx);
        cx.notify();
    }

    fn pick_option(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &mut self.question else { return };
        let question = &draft.questions[draft.index];
        let next = requests::toggle(question, draft.drafts.get(&question.id), &value);
        let advance = !question.multi_select;
        draft.drafts.insert(question.id.clone(), next);
        let at = (draft.request_id.clone(), draft.index);
        self.set_composer_text("", window, cx);
        cx.notify();
        if advance {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(AUTO_ADVANCE).await;
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.question.as_ref().is_some_and(|q| (q.request_id.clone(), q.index) == at) {
                        this.advance_question(window, cx);
                    }
                });
            })
            .detach();
        }
    }

    fn dismiss_question(&mut self, request_id: String, cx: &mut Context<Self>) {
        let Some(thread) = &self.thread else { return };
        let command = json!({"type": "thread.user-input.dismiss", "commandId": new_id(), "threadId": thread.id, "requestId": request_id});
        self.dispatch(command, cx);
    }

    /// Sending while an approval waits declines it: with the text as its
    /// feedback when the provider takes feedback (the text is then used up and
    /// `true` comes back), else every live approval is declined before the
    /// message goes out.
    pub(super) fn approval_declines(&self, text: &str) -> (Vec<Value>, bool) {
        let Some(projection) = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()) else {
            return (Vec::new(), false);
        };
        let live: Vec<_> =
            projection.pending_approvals().into_iter().map(|(request, _)| request).filter(|r| r.is_live()).collect();
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
        let mode = self.server_info.project_setting(self.target_project(), "defaultRuntimeMode");
        mode.as_str().unwrap_or(DEFAULT_RUNTIME_MODE).to_owned()
    }

    pub(super) fn runtime_mode(&self) -> String {
        let mode = match &self.target {
            Some(Target::Thread(_)) => self
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
            Some(Target::Thread(thread_id)) => {
                let command = json!({"type": "thread.runtime-mode.set", "commandId": new_id(), "threadId": thread_id, "runtimeMode": mode});
                self.dispatch(command, cx);
            }
            _ => self.runtime_mode_override = Some(mode.to_owned()),
        }
        cx.notify();
    }

    /// `ChatComposer.tsx` placeholder precedence.
    fn placeholder(&self, approval: Option<&RuntimeRequest>) -> &'static str {
        if let Some(approval) = approval {
            return if approval.supports_feedback() {
                "Send feedback to decline this request"
            } else {
                "Send a message to decline and redirect the agent"
            };
        }
        if let Some(draft) = &self.question {
            return if draft.questions[draft.index].allows_custom() {
                "Type your own answer, or leave this blank to use the selected option"
            } else {
                "Choose an option above"
            };
        }
        if self.target.is_none() {
            return "Choose a project above to start a thread";
        }
        if self.server_info.model_choices().is_empty() {
            return "Enable a provider in Settings to send a message";
        }
        DEFAULT_PLACEHOLDER
    }

    pub(super) fn render_composer(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (lane, _) = self.chat_layout(window);
        let t = *tokens(cx);
        let projection = self.thread.as_ref().and_then(|t| t.state.projection.as_ref());
        let approvals = projection.map(|p| p.pending_approvals()).unwrap_or_default();
        let first_approval = approvals.first().map(|(request, _)| *request);
        let placeholder = self.placeholder(first_approval);
        let outline = if t.dark { white(0.05) } else { black(0.08) };
        let view = cx.weak_entity();
        let editor = Textarea::new(&self.composer)
            .appearance(false)
            .xsmall()
            .font_family(t.fonts.composer)
            .text_size(px(t.fonts.prompt_size))
            .line_height(px(t.fonts.prompt_size * PROMPT_LEADING))
            .mx(px(-4.))
            .disabled(self.target.is_none())
            .token(move |token, _, _| render_chip(token, &t))
            .on_paste(move |item, window, cx| {
                view.update(cx, |this, cx| this.paste_attachments(item, window, cx)).unwrap_or(false)
            });
        let surface = v_flex()
            .w_full()
            .rounded(px(16.))
            .border_1()
            .border_color(outline)
            // The glass fill over the canvas, opaque so the banner tucked under it stays hidden.
            .bg(t.background.blend(t.card.opacity(t.glass)))
            .shadow(if t.dark {
                vec![BoxShadow::new(px(0.), px(1.), white(0.03)).inset()]
            } else {
                vec![BoxShadow::new(px(0.), px(1.), black(0.05)).blur_radius(px(2.))]
            })
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                let held = event.modifiers.control || event.modifiers.platform;
                if held != this.ctrl_held {
                    this.ctrl_held = held;
                    cx.notify();
                }
            }))
            .child(v_flex().px(px(16.)).pt(px(16.)).pb(px(8.)).children(self.render_attachments(&t, cx)).child(
                div().relative().child(editor).when(self.composer_empty, |el| {
                    el.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .font_family(t.fonts.composer)
                            .text_size(px(t.fonts.prompt_size))
                            .line_height(px(t.fonts.prompt_size * PROMPT_LEADING))
                            .text_color(t.muted_foreground)
                            .truncate()
                            .child(placeholder),
                    )
                }),
            ))
            .children(self.composer_ui.length_error.clone().map(|error| {
                div().px(px(16.)).pb(px(8.)).text_xs().line_height(px(16.)).text_color(t.error).child(error)
            }))
            .child(self.render_footer(&t, cx));
        div().w_full().flex_none().pt(px(8.)).pl(px(lane.left)).pr(px(lane.right)).pb(px(20.)).child(
            v_flex()
                .w_full()
                .children(approvals.iter().map(|(request, item)| self.render_approval(request, *item, &t, cx)))
                .children(self.render_banner_dock(&t, cx))
                .child(
                    div()
                        .relative()
                        .children(self.render_command_menu(&t, cx))
                        .children(self.render_stash_menu(f32::from(window.viewport_size().height) * 0.4, &t, cx))
                        .child(surface),
                )
                .children(self.render_context_strip(&t, cx)),
        )
    }

    fn render_footer(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = self.runtime_mode();
        // Drafts name their permissions in the row under the composer instead.
        let strip_owns_mode = matches!(self.target, Some(Target::Draft { .. }));
        let (plan_ui, interaction) = self.interaction_mode();
        h_flex()
            .items_center()
            .px(px(8.))
            .pb(px(8.))
            .when(self.question.is_some(), |el| el.pt(px(8.)))
            .when(self.can_attach(), |el| {
                el.child(
                    div().mr(px(8.)).child(
                        div()
                            .id("attach")
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
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .justify_end()
                            .gap(px(2.))
                            .child(self.render_model_picker(t, cx))
                            .when(!strip_owns_mode && mode != self.default_runtime_mode(), |el| {
                                let (_, label, icon) = runtime_modes()
                                    .into_iter()
                                    .find(|(id, ..)| *id == mode)
                                    .unwrap_or(runtime_modes()[3]);
                                el.child(self.render_permissions_control(
                                    "footer-permissions",
                                    icon,
                                    label,
                                    false,
                                    t,
                                    cx,
                                ))
                            })
                            .when(plan_ui, |el| el.child(self.render_interaction_toggle(interaction, t, cx))),
                    )
                    .children(self.render_context_meter(t, cx))
                    .child(self.render_primary_action(t, cx)),
            )
    }

    fn render_primary_action(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let running = self.thread.as_ref().and_then(|t| t.state.projection.as_ref()).is_some_and(|p| p.is_running());
        let stop = || {
            round_button("stop", t.error.opacity(0.9), t.error)
                .hover(|s| s.bg(t.error))
                .child(div().size(px(8.)).rounded(px(1.5)).bg(t.primary_foreground))
                .tooltip(|window, cx| Tooltip::new("Interrupt").build(window, cx))
                .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
        };
        if let Some(draft) = &self.question {
            let responding = self.responding.as_ref() == Some(&draft.request_id);
            let can_respond =
                self.pending_question_request().is_some_and(|(r, _)| r.capability() != Some("not_resumable"));
            let last = draft.index + 1 == draft.questions.len();
            let question = &draft.questions[draft.index];
            let answered = requests::answer(question, draft.drafts.get(&question.id)).is_some();
            let ready = if last {
                draft.questions.iter().enumerate().all(|(i, q)| {
                    (i == draft.index && answered) || requests::answer(q, draft.drafts.get(&q.id)).is_some()
                })
            } else {
                answered
            };
            let label = match (responding, last) {
                (true, _) => "Submitting...",
                (false, false) => "Next question",
                (false, true) if draft.index > 0 => "Submit answers",
                (false, true) => "Submit answer",
            };
            let enabled = ready && can_respond && !responding;
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
                            .on_click(cx.listener(|this, _, window, cx| this.previous_question(window, cx))),
                    )
                })
                .child(
                    div()
                        .id("submit-answer")
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
                            el.cursor_pointer()
                                .hover(|s| s.bg(t.primary_hover))
                                .on_click(cx.listener(|this, _, window, cx| this.advance_question(window, cx)))
                        }),
                )
                .into_any_element();
        }
        if self.plan_follow_up_target().is_some() {
            return self.render_plan_follow_up_action(t, cx);
        }
        let sendable = !self.composer_empty || self.has_attachments();
        if running && !sendable {
            return stop().into_any_element();
        }
        let disabled_reason = self.attachments_busy().then_some("Attachment(s) still uploading");
        let can_send = sendable
            && self.target.is_some()
            && self.client().is_some()
            && disabled_reason.is_none()
            && !self.target_rewinding();
        // `environmentIdentificationMode`: the stage art fills the button (`StageBackdropButtonArt`).
        let stage = self.stage_art();
        let (fill, icon) = if stage.is_some() {
            (gpui_kit::transparent_black(), gpui_kit::white())
        } else if can_send || self.sending {
            (t.primary, t.primary_foreground)
        } else {
            (t.foreground.opacity(0.12), t.muted_foreground)
        };
        let follow_up = self.follow_up_behavior();
        let alternate = if follow_up == "queue" { "steer" } else { "queue" };
        let held = if self.ctrl_held { Intent::Alternate } else { Intent::Foreground };
        let queue = running && super::send::queues(follow_up, held);
        let content: AnyElement = if self.sending {
            Spinner::new().small().color(icon).into_any_element()
        } else if running {
            let glyph = if queue { Lucide::ListPlus } else { Lucide::CornerUpRight };
            Icon::new(glyph).with_size(px(16.)).text_color(icon).into_any_element()
        } else {
            svg().path("icons/app/send.svg").size(px(14.)).text_color(icon).into_any_element()
        };
        let tooltip: Option<SharedString> = match (disabled_reason, running && can_send) {
            (Some(reason), _) => Some(reason.into()),
            (None, true) => {
                let shortcut = self.client_settings.values.get("sendShortcut").as_str().unwrap_or("enter");
                let keys = if super::send::requires_modifier(shortcut, &self.composer.read(cx).value()) {
                    "Ctrl+Shift+Enter"
                } else {
                    "Ctrl+Enter"
                };
                Some(format!("Click to {follow_up}, Ctrl-click or {keys} to {alternate}").into())
            }
            _ => None,
        };
        round_button("send", fill, t.primary)
            .when_some(stage, |el, art| {
                el.relative()
                    .when(!can_send, |el| el.opacity(0.64))
                    .child(super::sidebar::stage_button_art(art, t.dark))
            })
            .child(div().relative().child(content))
            .when(!can_send, |el| el.shadow(Vec::new()))
            .when(can_send, |el| {
                el.cursor_pointer().when(stage.is_none(), |el| el.hover(|s| s.bg(t.primary_hover))).on_click(
                    cx.listener(|this, event: &ClickEvent, window, cx| {
                        let modifiers = event.modifiers();
                        let intent = if modifiers.control || modifiers.platform {
                            Intent::Alternate
                        } else {
                            Intent::Foreground
                        };
                        this.submit_with(intent, window, cx)
                    }),
                )
            })
            .when_some(tooltip, |el, tooltip| {
                el.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .into_any_element()
    }

    fn render_approval(
        &self,
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
        let options: Vec<(String, String)> = item
            .and_then(|i| i.raw.get("options"))
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|o| {
                        Some((o.get("decision")?.as_str()?.to_owned(), o.get("label")?.as_str()?.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_else(|| {
                [
                    ("cancel", "Cancel"),
                    ("decline", "Decline"),
                    ("acceptForSession", "Always allow this session"),
                    ("accept", "Approve"),
                ]
                .map(|(d, l)| (d.to_owned(), l.to_owned()))
                .to_vec()
            });
        let enabled = request.is_live() && self.responding.as_ref() != Some(&request.id);
        let inline = |decision: &str, label: &str, tone: Hsla, cx: &mut Context<Self>| {
            let request_id = request.id.clone();
            let decision = decision.to_owned();
            div()
                .id(ElementId::Name(format!("approval-{}-{decision}", request.id).into()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(tone)
                .child(label.to_owned())
                .when(!enabled, |el| el.opacity(0.64))
                .when(enabled, |el| {
                    el.cursor_pointer().hover(|s| s.underline().text_color(t.foreground)).on_click(cx.listener(
                        move |this, _, _, cx| this.respond(request_id.clone(), json!({"decision": decision}), cx),
                    ))
                })
        };
        let primary: Vec<_> = options.iter().filter(|(d, _)| d == "decline" || d == "accept").collect();
        let more: Rc<Vec<(String, String)>> =
            Rc::new(options.iter().filter(|(d, _)| d != "decline" && d != "accept").cloned().collect());
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
            for (decision, label) in primary {
                let tone = if decision == "accept" { t.foreground } else { t.muted_foreground };
                actions = actions.child(inline(decision, label, tone, cx));
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
                            for (decision, label) in more.iter() {
                                let request_id = request_id.clone();
                                let decision = decision.clone();
                                menu = menu.item(PopupMenuItem::new(label.clone()).on_click(window.listener_for(
                                    &view,
                                    move |this, _, _, cx| {
                                        this.respond(request_id.clone(), json!({"decision": decision}), cx)
                                    },
                                )));
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
        let Some(Target::Draft { project_id }) = &self.target else { return None };
        if self.model_override.is_some() || self.server_info.settings.get("fallbackModelSelection").is_null() {
            return None;
        }
        crate::new_thread::usage_fallback(&self.server_info, default, Some(project_id), jiff::Timestamp::now())
    }

    /// `ComposerBanner.Dock`: the banners tucked under the composer, with the stash tab
    /// on their right.
    fn render_banner_dock(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let banners: Vec<AnyElement> =
            self.render_fallback_banner(t, cx).into_iter().chain(self.render_question(t, cx)).collect();
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
    fn render_fallback_banner(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Some(Target::Draft { project_id }) = &self.target else { return None };
        let project = self.shell.projects.iter().find(|p| &p.id == project_id);
        let default = self.server_info.new_thread_model(project)?;
        let fallback = self.draft_usage_fallback(&default)?;
        let window = format!("{}:{}", fallback.exhausted_instance, fallback.resets_at.as_deref().unwrap_or_default());
        if self.composer_ui.dismissed_fallback.as_ref() == Some(&window) {
            return None;
        }
        let model_name = |selection: &ModelSelection| {
            self.server_info
                .provider(&selection.instance_id)
                .and_then(|p| p.models.iter().find(|m| m.slug == selection.model))
                .and_then(|m| m.name.clone())
                .unwrap_or_else(|| selection.model.clone())
        };
        let exhausted = self.server_info.provider(&fallback.exhausted_instance)?;
        let provider_name = exhausted.display_name.clone().unwrap_or_else(|| exhausted.driver.clone());
        let hour12 =
            crate::timestamp::hour12(self.client_settings.values.get("timestampFormat").as_str().unwrap_or(""));
        let resets = fallback
            .resets_at
            .as_deref()
            .and_then(|at| crate::timestamp::upcoming(at, &jiff::Zoned::now(), hour12))
            .map(|at| format!(" · resets {at}"))
            .unwrap_or_default();
        let outline = if t.dark { white(0.05) } else { black(0.08) };
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
                        .rounded_t(px(16.))
                        .border_1()
                        .border_color(outline)
                        .bg(t.card.opacity(t.glass))
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
                                        cx.listener(move |this, _, _, cx| this.pick_model(default.clone(), cx)),
                                    ),
                                )
                                .child(
                                    Button::new("fallback-dismiss")
                                        .ghost()
                                        .xsmall()
                                        .icon(Icon::new(Lucide::X))
                                        .tooltip("Dismiss fallback model notice")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.composer_ui.dismissed_fallback = Some(window.clone());
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_question(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let draft = self.question.as_ref()?;
        let (request, item) = self.pending_question_request()?;
        let question = &draft.questions[draft.index];
        let disabled = request.capability() == Some("not_resumable") || self.responding.as_ref() == Some(&request.id);
        let dismissible = item.str("responseMode") == Some("message") || request.capability() == Some("message");
        let collapsed = draft.collapsed;
        let count = draft.questions.len();
        let header = h_flex()
            .id("question-toggle")
            .w_full()
            .min_h(px(24.))
            .px(px(16.))
            .pt(px(6.))
            .gap(px(4.))
            .rounded(px(12.))
            .cursor_pointer()
            .hover(|s| s.bg(t.foreground.opacity(0.05)))
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(draft) = &mut this.question {
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
                        el.child(
                            icon_box(t)
                                .id("question-dismiss")
                                .cursor_pointer()
                                .hover(|s| s.bg(t.accent))
                                .child(Icon::new(Lucide::X).with_size(px(14.)))
                                .tooltip(|window, cx| {
                                    Tooltip::new("Dismiss question without answering").build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_question(request_id.clone(), cx);
                                })),
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
                            el.on_click(
                                cx.listener(move |this, _, window, cx| this.pick_option(value.clone(), window, cx)),
                            )
                        })
                    },
                )))
        });
        let outline = if t.dark { white(0.05) } else { black(0.08) };
        Some(
            div()
                .mb(px(-17.))
                .child(
                    v_flex()
                        .px(px(4.))
                        .pt(px(4.))
                        // 4px padding plus the 17px the composer covers.
                        .pb(px(21.))
                        .rounded_t(px(16.))
                        .border_1()
                        .border_color(outline)
                        .bg(t.card.opacity(t.glass))
                        .text_xs()
                        .line_height(px(16.))
                        .shadow(if t.dark {
                            vec![
                                BoxShadow::new(px(0.), px(14.), black(0.75))
                                    .blur_radius(px(32.))
                                    .spread_radius(px(-18.)),
                            ]
                        } else {
                            vec![BoxShadow::new(px(0.), px(1.), black(0.05)).blur_radius(px(2.))]
                        })
                        .child(header)
                        .children(body),
                )
                .into_any_element(),
        )
    }
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
        .when(!disabled, |el| el.cursor_pointer())
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

/// A chip in the editor (`ContextChip`): icon and label in the kind's accent ink.
fn render_chip(token: &InlineTokenContext, t: &Tokens) -> AnyElement {
    let id = token.token().id();
    let (accent, icon) = match Chip::from_id(id) {
        Some(Chip::Skill { .. }) => (rgb(0xb261be), Lucide::Box),
        Some(Chip::Context { kind, .. }) if kind == "image" => (rgb(0x0090cd), Lucide::Image),
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
        .into_any_element()
}

/// `ComposerControl size="sm"`: 28px tall, 8px padding, 6px gap, text-sm medium.
pub(super) fn control_button(id: &'static str, t: &Tokens) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .h(px(28.))
        .px(px(8.))
        .gap(px(6.))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.foreground.opacity(0.8))
}

/// The 32px send/stop circle with its tinted shadow and inset highlight.
fn round_button(id: &'static str, fill: Hsla, shadow: Hsla) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .size(px(32.))
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
    fn a_failed_send_keeps_newer_text_after_it() {
        assert_eq!(merge_prompt("sent".into(), ""), "sent");
        assert_eq!(merge_prompt("sent".into(), "  "), "sent");
        assert_eq!(merge_prompt("sent".into(), "typed later"), "sent\n\ntyped later");
    }
}
