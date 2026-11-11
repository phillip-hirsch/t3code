//! Assistant citations in the chat: the Cite button over text selected in a reply
//! (`AssistantSelectionToolbar.tsx`), the composer chip's comment editor
//! (`AssistantCitationChip.tsx`, `AssistantCitationCommentEditor.tsx`) and going to a cited
//! reply (`AssistantCitationSource.tsx`, `citationProcedure.ts`).

use std::ops::Range;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InlineToken, InlineTokenContext, InputContent, InputEvent, Textarea, TextareaState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::{RangeHighlight, RenderedText};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    Anchor, AnyElement, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, FocusHandle, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, ListOffset, ParentElement as _, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, anchored, deferred, div, point,
    prelude::FluentBuilder as _, px, rgb,
};
use serde_json::Value;

use super::chat::view_pane;
use super::send::Intent;
use super::side_chat::ChatPane;
use super::{Target, Workspace};
use crate::assistant_citations::{self, AssistantCitation, Dismissal, MAX_COMMENT_LENGTH, MAX_TEXT_LENGTH, js_len};
use crate::composer_text::{self, Chip};
use crate::model::ThreadState;
use crate::theme::{Tokens, tokens};
use crate::timeline::Row;

/// `--context-chip-citation`: `oklch(0.62 0.16 259)`.
pub(super) const CITATION_ACCENT: u32 = 0x4684e5;
/// How long a reached citation stays highlighted (`CITATION_HIGHLIGHT_TOTAL_MS`).
const REVEAL_HIGHLIGHT: Duration = Duration::from_millis(3_000);

#[derive(Default)]
pub(super) struct CitationUi {
    toolbar: Option<CiteSelection>,
    toolbar_focus: Option<FocusHandle>,
    comment: Option<CommentEditor>,
    /// A citation whose reply is to be shown once its thread's rows are in.
    reveal: Option<(AssistantCitation, AnyWindowHandle)>,
    highlight: Option<SourceHighlight>,
    _highlight_timer: Option<Task<()>>,
}

/// A selection in one reply that the Cite button would quote.
struct CiteSelection {
    /// The chat the reply is in, whose composer Cite fills.
    pane: ChatPane,
    citation: AssistantCitation,
    /// Where the button sits, in window coordinates.
    position: Point<Pixels>,
    /// The reply's markdown key and the quote's bytes in its rendered text.
    key: SharedString,
    range: Range<usize>,
    /// The timeline's top when the selection was made; a scroll hides the button.
    scroll_top: ListOffset,
}

struct CommentEditor {
    /// The chat whose composer holds the chip.
    pane: ChatPane,
    /// The chip's token id and its start in the composer, when the editor opened.
    chip_id: String,
    chip_start: usize,
    input: Entity<TextareaState>,
    anchor: CommentAnchor,
    /// Cancel removes a chip Cite just inserted, with the spaces it added before and after.
    remove_on_cancel: Option<(bool, bool)>,
    _events: Subscription,
}

/// Where the editor opens: below the quoted text, or above its chip (`side`, `align="end"`).
#[derive(Clone)]
enum CommentAnchor {
    /// The quote's end, as an offset from its reply's view, so the editor moves with the timeline.
    Source {
        key: SharedString,
        offset: Point<Pixels>,
    },
    Chip(Bounds<Pixels>),
}

/// Quoted text painted in its reply: while its comment is edited, or once it is reached.
struct SourceHighlight {
    key: SharedString,
    kind: HighlightKind,
    /// The rendered text last painted, so a view's own notifications don't repaint it.
    painted: Option<RenderedText>,
}

enum HighlightKind {
    /// The selection a comment is being written about, as rendered when it was cited.
    Comment(Range<usize>),
    /// A reached citation, found again by its quote and context.
    Reveal { citation: Box<AssistantCitation>, window: AnyWindowHandle, revealed: bool },
}

/// A toast from code that holds no window, shown once the current update ends.
fn toast_later(handle: AnyWindowHandle, title: &'static str, message: &'static str, cx: &mut App) {
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, cx| {
            window.push_notification(Notification::warning(message).title(title), cx);
        });
    });
}

impl Workspace {
    /// A press in the timeline drops the Cite button, as `observeSelectionActions` does.
    pub(super) fn chat_mouse_down(&mut self, cx: &mut Context<Self>) {
        if self.citations.toolbar.take().is_some() {
            cx.notify();
        }
    }

    /// The pointer let go over the timeline: once the selection settles, offer to cite it.
    pub(super) fn chat_mouse_up(
        &mut self,
        pane: ChatPane,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.on_next_frame(window, move |this, _, cx| this.capture_cite_selection(pane, Some(position), cx));
    }

    /// Select All in a reply, the text views' one keyboard selection: once it applies, offer to
    /// cite it (`selectionchange`).
    pub(super) fn chat_select_all(&mut self, pane: ChatPane, window: &mut Window, cx: &mut Context<Self>) {
        cx.on_next_frame(window, move |this, _, cx| this.capture_cite_selection(pane, None, cx));
    }

    /// `captureAssistantTextSelection`: a selection inside one reply of the pane's thread, its
    /// button at the pointer that made it, else below the selection's end.
    fn capture_cite_selection(&mut self, pane: ChatPane, pointer: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let captured = self.cite_selection_at(pane, pointer, cx);
        if captured.is_some() {
            self.citations.toolbar_focus.get_or_insert_with(|| cx.focus_handle());
        }
        if captured.is_some() || self.citations.toolbar.is_some() {
            self.citations.toolbar = captured;
            cx.notify();
        }
    }

    fn cite_selection_at(&self, pane: ChatPane, pointer: Option<Point<Pixels>>, cx: &App) -> Option<CiteSelection> {
        let (env, thread_id) = self.pane_thread(pane)?;
        let rows = self.pane_rows(pane);
        let mut selected = self
            .markdown_keys(pane)
            .into_iter()
            .filter_map(|key| {
                let view = self.markdown_view(&key)?;
                let state = view.read(cx);
                let source = state.selected_source_range()?;
                Some((key, state.is_selecting(), state.rendered_text(), source))
            })
            .filter(|(key, ..)| rows.iter().any(|row| super::chat::shows_markdown(row, view_pane(key).1)));
        // A selection that spans replies quotes neither.
        let (Some((key, false, rendered, source)), None) = (selected.next(), selected.next()) else { return None };
        let item = view_pane(&key).1;
        if !rows.iter().any(|row| matches!(row, Row::Assistant { id, .. } if **id == *item)) {
            return None;
        }
        let range = rendered.range_for_source(source)?;
        let selector = assistant_citations::create_selector(rendered.as_str(), range.clone())?;
        let citation = AssistantCitation {
            environment_id: env.to_string(),
            thread_id,
            message_id: self.message_id_of(pane, item)?,
            text: selector.text,
            comment: None,
            start: selector.start,
            end: selector.end,
            prefix: selector.prefix,
            suffix: selector.suffix,
        };
        let scroller = self.pane_scroller(pane)?.read(cx);
        // Select All, the only selection the keyboard makes, ends where the reply does.
        let reply = self.markdown_view(&key)?.read(cx).bounds();
        let position = selection_action_position(scroller.viewport_bounds(), reply.bottom_right(), pointer);
        let scroll_top = scroller.logical_scroll_top();
        Some(CiteSelection { pane, citation, position, key, range, scroll_top })
    }

    /// The `messageId` of the pane's timeline item `item_id`.
    fn message_id_of(&self, pane: ChatPane, item_id: &str) -> Option<String> {
        let projection = self.pane_projection(pane)?;
        let item = projection.visible_turn_items.iter().find(|row| row.item.id == item_id)?;
        Some(item.item.raw.get("messageId").and_then(Value::as_str).unwrap_or(&item.item.id).to_owned())
    }

    /// The Cite button at the pointer, while the timeline hasn't scrolled since the selection.
    pub(super) fn render_cite_toolbar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toolbar = self.citations.toolbar.as_ref()?;
        if self.cite_toolbar_hidden(cx) {
            return None;
        }
        let t = *tokens(cx);
        let too_long = js_len(&toolbar.citation.text) > MAX_TEXT_LENGTH;
        let focus = self.citations.toolbar_focus.clone();
        let button = Button::new("cite-selection")
            .outline()
            .xsmall()
            .h(px(24.))
            .px(px(8.))
            .gap(px(4.))
            .rounded(px(6.))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            // `variant="glass"`; with no backdrop blur, an opaque fill stands in.
            .bg(t.background)
            .text_color(t.foreground)
            .border_color(t.border)
            .shadow_sm()
            .disabled(too_long)
            .icon(Icon::new(Lucide::Quote).with_size(px(14.)))
            .label(if too_long { "Shorten selection" } else { "Cite" })
            .when_some(focus, |button, focus| button.track_focus(&focus))
            .on_click(cx.listener(|this, _, window, cx| this.cite_selection(window, cx)));
        Some(
            deferred(
                anchored()
                    .position(toolbar.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(div().occlude().child(button)),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// The Cite button's keys, ahead of the keymap: Tab reaches it (`focusActions`), Enter or
    /// Space cites, and Escape dismisses it. In the comment editor, Escape cancels.
    pub(super) fn citation_key(
        &mut self,
        keystroke: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let m = keystroke.modifiers;
        let plain = !(m.control || m.platform || m.alt || m.shift);
        if let Some(editor) = &self.citations.comment
            && editor.input.focus_handle(cx).is_focused(window)
        {
            // Escape is Cancel: it drops the draft, and the chip too when Cite just added it.
            if keystroke.key == "escape" && !m.modified() {
                self.cancel_citation_comment(window, cx);
                return true;
            }
            return false;
        }
        if keystroke.key == "tab" && m.shift && !(m.control || m.platform || m.alt) {
            return self.edit_citation_before_caret(window, cx);
        }
        let too_long = self.citations.toolbar.as_ref().is_some_and(|t| js_len(&t.citation.text) > MAX_TEXT_LENGTH);
        if self.citations.toolbar.is_none() || self.cite_toolbar_hidden(cx) {
            return false;
        }
        let focused = self.citations.toolbar_focus.as_ref().is_some_and(|focus| focus.is_focused(window));
        match keystroke.key.as_str() {
            // Tab belongs to the foreground chat; the side chat only follows its selection.
            "tab"
                if plain
                    && !focused
                    && !too_long
                    && !self.citations.toolbar.as_ref().is_some_and(|t| t.pane == ChatPane::Side) =>
            {
                let Some(focus) = self.citations.toolbar_focus.clone() else { return false };
                focus.focus(window, cx);
                cx.notify();
                true
            }
            "enter" | "space" if plain && focused => {
                self.cite_selection(window, cx);
                true
            }
            "escape" if !m.modified() => {
                let pane = self.citations.toolbar.take().map_or(ChatPane::Main, |toolbar| toolbar.pane);
                if focused {
                    self.focus_pane_composer(pane, window, cx);
                }
                cx.notify();
                true
            }
            _ => false,
        }
    }

    /// Shift+Tab from just after a composer citation edits its comment, as it reaches the chip's
    /// pencil on the web.
    fn edit_citation_before_caret(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let state = self.composer.read(cx);
        if !self.composer.focus_handle(cx).is_focused(window) || !state.selected_range().is_empty() {
            return false;
        }
        let caret = state.cursor();
        let Some(span) = state.tokens().iter().find(|span| span.range().end == caret) else { return false };
        if !matches!(Chip::from_id(span.token().id()), Some(Chip::Citation { .. })) {
            return false;
        }
        let (chip_id, range) = (span.token().id().to_string(), span.range());
        let Some(bounds) = state.range_to_bounds(&range) else { return false };
        self.open_citation_comment(ChatPane::Main, chip_id, range.start, CommentAnchor::Chip(bounds), None, window, cx);
        true
    }

    /// The button hides once its timeline scrolls or its pane shows another thread.
    fn cite_toolbar_hidden(&self, cx: &App) -> bool {
        self.citations.toolbar.as_ref().is_some_and(|toolbar| {
            let citation = &toolbar.citation;
            !self.pane_shows(toolbar.pane, &citation.environment_id, &citation.thread_id)
                || self
                    .pane_scroller(toolbar.pane)
                    .is_none_or(|scroller| scroller.read(cx).logical_scroll_top() != toolbar.scroll_top)
        })
    }

    /// Cite: the selection joins the composer as a chip whose comment editor opens at the quote.
    fn cite_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(toolbar) = self.citations.toolbar.take() else { return };
        cx.notify();
        if js_len(&toolbar.citation.text) > MAX_TEXT_LENGTH {
            return;
        }
        let origin = self.markdown_view(&toolbar.key).map_or(Point::default(), |view| view.read(cx).bounds().origin);
        let anchor = CommentAnchor::Source { key: toolbar.key.clone(), offset: toolbar.position - origin };
        if !self.cite_into_composer(toolbar.pane, &toolbar.citation, anchor, window, cx) {
            let message = "Try citing the selection after the connection or pending input is resolved.";
            window.push_notification(Notification::warning(message).title("The composer is not ready"), cx);
            return;
        }
        if let Some(view) = self.markdown_view(&toolbar.key) {
            view.update(cx, gpui_kit::base::TextViewState::clear_selection);
        }
        gpui_kit::base::TextSelection::clear(window, cx);
        self.set_citation_highlight(toolbar.key, HighlightKind::Comment(toolbar.range), cx);
    }

    /// The pane's composer takes text: its chat is open, no answer or message is being sent, and
    /// an open question takes a typed answer (`insertComposerText`).
    pub(super) fn composer_takes_citations(&self, pane: ChatPane) -> bool {
        let open = match pane {
            ChatPane::Main => self.target.is_some(),
            ChatPane::Side => self.pane_thread(ChatPane::Side).is_some() && !self.pane_sending(ChatPane::Side),
        };
        open && self.question_takes_text(pane)
    }

    /// `citeAssistantText`: the chip at the caret, after a space unless one is there, then a
    /// space (or the one already after the caret); its comment editor opens.
    fn cite_into_composer(
        &mut self,
        pane: ChatPane,
        citation: &AssistantCitation,
        anchor: CommentAnchor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(composer) = self.pane_composer(pane).filter(|_| self.composer_takes_citations(pane)) else {
            return false;
        };
        let chip = Chip::citation(citation.clone());
        let chip_id = chip.id();
        let markdown = citation.markdown();
        let (start, spaces) = composer.update(cx, |state, cx| {
            let value = state.value();
            let at = state.cursor();
            let leading = at > 0 && !value[..at].ends_with(char::is_whitespace);
            let text = format!("{}{markdown} ", if leading { " " } else { "" });
            let end = composer_text::extend_for_trailing_space(&value, at..at, &text).end;
            state.set_selected_range(at..end, cx);
            state.replace(text.clone(), window, cx);
            let start = at + usize::from(leading);
            let token = InlineToken::new(chip_id.clone(), markdown.clone()).with_label(chip.label().to_owned());
            let _ = state.replace_range_with_token(start..start + markdown.len(), token, window, cx);
            let caret = at + text.len();
            state.set_selected_range(caret..caret, cx);
            (start, (leading, end == at))
        });
        if pane == ChatPane::Main {
            self.sync_composer_empty(cx);
        }
        self.open_citation_comment(pane, chip_id, start, anchor, Some(spaces), window, cx);
        true
    }

    /// A composer chip was clicked: its pencil (or the keyboard) edits the comment, the rest
    /// goes to the quoted reply.
    pub(super) fn citation_chip_clicked(
        &mut self,
        pane: ChatPane,
        chip_id: String,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        click: &gpui_kit::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Chip::Citation { citation, .. }) = Chip::from_id(&chip_id) else { return };
        let pencil = px(tokens(cx).fonts.prompt_size * 1.3);
        let on_pencil = match click {
            gpui_kit::ClickEvent::Mouse(mouse) => mouse.up.position.x >= bounds.right() - pencil,
            _ => true,
        };
        if on_pencil {
            if self
                .citations
                .comment
                .as_ref()
                .is_some_and(|c| c.pane == pane && c.chip_id == chip_id && c.chip_start == range.start)
            {
                self.close_citation_comment(false, window, cx);
            } else if self.target.is_some() {
                self.open_citation_comment(pane, chip_id, range.start, CommentAnchor::Chip(bounds), None, window, cx);
            }
        } else {
            self.go_to_citation(*citation, window, cx);
        }
    }

    fn open_citation_comment(
        &mut self,
        pane: ChatPane,
        chip_id: String,
        chip_start: usize,
        anchor: CommentAnchor,
        remove_on_cancel: Option<(bool, bool)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.citations.comment.is_some() {
            self.close_citation_comment(false, window, cx);
        }
        let saved = match Chip::from_id(&chip_id) {
            Some(Chip::Citation { citation, .. }) => citation.comment.unwrap_or_default(),
            _ => return,
        };
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 7)
                .submit_on_enter(true)
                .placeholder("Add an optional comment...")
        });
        input.update(cx, |input, cx| {
            input.set_value(SharedString::from(saved.clone()), window, cx);
            let end = saved.len();
            input.set_selected_range(end..end, cx);
            input.focus(window, cx);
        });
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            // Enter saves, Mod+Enter saves and sends; Shift+Enter writes a line.
            InputEvent::PressEnter { secondary, shift: false } => this.save_citation_comment(*secondary, window, cx),
            InputEvent::Change => cx.notify(),
            _ => {}
        });
        self.citations.comment =
            Some(CommentEditor { pane, chip_id, chip_start, input, anchor, remove_on_cancel, _events: events });
        cx.notify();
    }

    /// The span of the chip the comment editor edits, if the composer still holds it.
    fn edited_chip(&self, cx: &App) -> Option<(Range<usize>, AssistantCitation)> {
        let editor = self.citations.comment.as_ref()?;
        let state = self.pane_composer(editor.pane)?.read(cx);
        let spans = state.tokens().iter().filter(|span| span.token().id().as_ref() == editor.chip_id.as_str());
        let mut spans: Vec<Range<usize>> = spans.map(gpui_kit::base::input::InlineTokenSpan::range).collect();
        spans.sort_by_key(|range| range.start.abs_diff(editor.chip_start));
        let Some(Chip::Citation { citation, .. }) = Chip::from_id(&editor.chip_id) else { return None };
        Some((spans.into_iter().next()?, *citation))
    }

    /// Writes `comment` into the edited chip; returns where the chip now ends.
    fn write_citation_comment(&mut self, comment: &str, window: &mut Window, cx: &mut Context<Self>) -> Option<usize> {
        let (range, citation) = self.edited_chip(cx)?;
        let citation = citation.with_comment(comment);
        let markdown = citation.markdown();
        let chip = Chip::citation(citation);
        let end = range.start + markdown.len();
        let token = InlineToken::new(chip.id(), markdown).with_label(chip.label().to_owned());
        let composer = self.pane_composer(self.citations.comment.as_ref()?.pane)?;
        composer.update(cx, |state, cx| state.replace_range_with_token(range, token, window, cx)).ok()?;
        Some(end)
    }

    /// Save (and with `send`, send the prompt too). A comment over the limit stays open.
    fn save_citation_comment(&mut self, send: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.citations.comment else { return };
        let (pane, draft) = (editor.pane, editor.input.read(cx).value().to_string());
        if js_len(&draft) > MAX_COMMENT_LENGTH {
            return;
        }
        let end = self.write_citation_comment(&draft, window, cx);
        self.finish_citation_comment(end, true, window, cx);
        if send && end.is_some() {
            match pane {
                ChatPane::Main => self.submit_with(Intent::Foreground, window, cx),
                ChatPane::Side => self.send_side_chat(window, cx),
            }
        }
    }

    /// Cancel: a chip Cite just inserted goes again; otherwise the editor closes unchanged.
    fn cancel_citation_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.citations.comment else { return };
        let (pane, remove) = (editor.pane, editor.remove_on_cancel);
        let composer = self.pane_composer(pane);
        let Some((((before, after), (range, _)), composer)) = remove.zip(self.edited_chip(cx)).zip(composer) else {
            self.close_citation_comment(true, window, cx);
            return;
        };
        composer.update(cx, |state, cx| {
            let value = state.value();
            let from = if before && value[..range.start].ends_with(' ') { range.start - 1 } else { range.start };
            let to = if after && value[range.end..].starts_with(' ') { range.end + 1 } else { range.end };
            state.set_selected_range(from..to, cx);
            state.replace("", window, cx);
        });
        if pane == ChatPane::Main {
            self.sync_composer_empty(cx);
        }
        self.finish_citation_comment(None, false, window, cx);
        self.focus_pane_composer(pane, window, cx);
    }

    /// Closes the editor: Escape drops the draft, any other way out saves a changed draft that
    /// fits (`resolveAssistantCitationCommentDismissal`), and a draft too long keeps it open.
    /// `restore` returns focus to the composer, after the chip.
    pub(super) fn close_citation_comment(&mut self, escape: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.settle_citation_comment(escape, true, window, cx);
    }

    fn settle_citation_comment(&mut self, escape: bool, restore: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = &self.citations.comment else { return };
        let draft = editor.input.read(cx).value().to_string();
        let saved = self.edited_chip(cx).and_then(|(_, citation)| citation.comment);
        let end = match assistant_citations::resolve_dismissal(escape, &draft, saved.as_deref()) {
            Dismissal::KeepOpen => return,
            Dismissal::Close => self.edited_chip(cx).map(|(range, _)| range.end),
            Dismissal::Commit(comment) => self.write_citation_comment(&comment, window, cx),
        };
        self.finish_citation_comment(end, restore, window, cx);
    }

    /// The composer's text with the open comment's draft saved into its chip, closing the editor:
    /// a target switch keeps an unsaved comment in the draft it stashes (`onCitationDraftSave`).
    /// A side chat's editor stays for its view, which saves it when it goes.
    pub(super) fn stash_citation_comment(&mut self, cx: &mut Context<Self>) -> Option<String> {
        let editor = self.citations.comment.as_ref().filter(|editor| editor.pane == ChatPane::Main)?;
        let draft = editor.input.read(cx).value().to_string();
        let chip = self.edited_chip(cx);
        self.drop_citation_comment(cx);
        let (range, citation) = chip?;
        let Dismissal::Commit(comment) =
            assistant_citations::resolve_dismissal(false, &draft, citation.comment.as_deref())
        else {
            return None;
        };
        let mut prompt = self.composer.read(cx).value().to_string();
        prompt.replace_range(range, &citation.with_comment(&comment).markdown());
        Some(prompt)
    }

    /// Saves a side chat's open comment into its chip and closes the editor, before the side
    /// chat's view goes (`onCitationDraftSave`).
    pub(super) fn stash_side_citation_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.citations.comment.as_ref().filter(|editor| editor.pane == ChatPane::Side) else {
            return;
        };
        let draft = editor.input.read(cx).value().to_string();
        if let Some((_, citation)) = self.edited_chip(cx)
            && let Dismissal::Commit(comment) =
                assistant_citations::resolve_dismissal(false, &draft, citation.comment.as_deref())
        {
            self.write_citation_comment(&comment, window, cx);
        }
        self.drop_citation_comment(cx);
    }

    /// A comment editor whose chip left the composer (a send, a recalled prompt) closes.
    pub(super) fn drop_orphan_citation_comment(&mut self, cx: &mut Context<Self>) {
        if self.citations.comment.is_some() && self.edited_chip(cx).is_none() {
            self.drop_citation_comment(cx);
        }
    }

    fn drop_citation_comment(&mut self, cx: &mut Context<Self>) {
        self.citations.comment = None;
        if matches!(self.citations.highlight.as_ref().map(|h| &h.kind), Some(HighlightKind::Comment(_))) {
            self.clear_citation_highlight(cx);
        }
        cx.notify();
    }

    fn finish_citation_comment(
        &mut self,
        chip_end: Option<usize>,
        restore: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &self.citations.comment else { return };
        let (pane, focused) = (editor.pane, editor.input.focus_handle(cx).contains_focused(window, cx));
        self.drop_citation_comment(cx);
        if restore && focused {
            self.focus_pane_composer(pane, window, cx);
            if let Some((end, composer)) = chip_end.zip(self.pane_composer(pane)) {
                composer.update(cx, |state, cx| state.set_selected_range(end..end, cx));
            }
        }
        cx.notify();
    }

    /// The comment editor, below the quoted text or above its chip (`PopoverPopup width="md"`).
    pub(super) fn render_citation_comment(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editor = self.citations.comment.as_ref()?;
        // A side chat's editor goes with its tab.
        self.pane_composer(editor.pane)?;
        let t = *tokens(cx);
        let (position, corner) = match &editor.anchor {
            CommentAnchor::Source { key, offset } => {
                let origin = self.markdown_view(key).map_or(Point::default(), |view| view.read(cx).bounds().origin);
                let at = origin + *offset;
                (point(at.x, at.y + px(14.)), Anchor::TopRight)
            }
            CommentAnchor::Chip(bounds) => (point(bounds.right(), bounds.top() - px(4.)), Anchor::BottomRight),
        };
        let too_long = js_len(&editor.input.read(cx).value()) > MAX_COMMENT_LENGTH;
        let cancel = Button::new("citation-comment-cancel")
            .outline()
            .xsmall()
            .h(px(24.))
            .px(px(8.))
            .rounded(px(6.))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .border_color(t.input)
            .label("Cancel")
            .on_click(cx.listener(|this, _, window, cx| this.cancel_citation_comment(window, cx)));
        let save = Button::new("citation-comment-save")
            .primary()
            .xsmall()
            .h(px(24.))
            .px(px(8.))
            .rounded(px(6.))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .disabled(too_long)
            .label(if too_long { "Shorten comment" } else { "Save" })
            .on_click(cx.listener(|this, _, window, cx| this.save_citation_comment(false, window, cx)));
        let popup =
            v_flex()
                .id("citation-comment")
                .occlude()
                .w(px(320.))
                .max_w(px(320.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.border)
                .bg(t.popover)
                .shadow_lg()
                .text_color(t.foreground)
                .on_mouse_down_out(
                    cx.listener(|this, _, window, cx| this.settle_citation_comment(false, false, window, cx)),
                )
                .child(div().px(px(4.)).py(px(6.)).min_h(px(64.)).child(
                    Textarea::new(&editor.input).appearance(false).xsmall().text_size(px(14.)).line_height(px(20.)),
                ))
                .when(too_long, |popup| {
                    popup.child(
                        div()
                            .pt(px(4.))
                            .text_xs()
                            .line_height(px(16.))
                            .text_color(t.error)
                            .child("Comments can contain up to 8,000 characters."),
                    )
                })
                .child(h_flex().mt(px(8.)).flex_wrap().justify_end().gap(px(8.)).child(cancel).child(save));
        Some(
            deferred(anchored().anchor(corner).position(position).snap_to_window_with_margin(px(8.)).child(popup))
                .with_priority(3)
                .into_any_element(),
        )
    }

    /// `assistantCitationNavigation`: opens the cited reply's thread, then shows the reply.
    pub(super) fn go_to_citation(&mut self, citation: AssistantCitation, window: &mut Window, cx: &mut Context<Self>) {
        let env = SharedString::from(citation.environment_id.clone());
        let here =
            matches!(&self.target, Some(Target::Thread { env: e, id }) if *e == env && *id == citation.thread_id);
        // The shell lists only some threads; whether the reply exists is known once its thread loads.
        let thread_id = citation.thread_id.clone();
        self.citations.reveal = Some((citation, window.window_handle()));
        if !here {
            self.select_thread_in(env, thread_id, window, cx);
        }
        self.reveal_pending_citation(cx);
    }

    /// Shows the reply a pending citation names once its thread's timeline is in: opens what
    /// folds it, scrolls to it and highlights the quote.
    pub(super) fn reveal_pending_citation(&mut self, cx: &mut Context<Self>) {
        self.settle_pending_citation(false, cx);
    }

    /// `reveal_pending_citation`, told whether the cited thread could not be read.
    pub(super) fn settle_pending_citation(&mut self, load_failed: bool, cx: &mut Context<Self>) {
        let Some((citation, _)) = &self.citations.reveal else { return };
        let here = matches!(&self.target, Some(Target::Thread { env, id })
            if **env == *citation.environment_id && *id == citation.thread_id);
        let Some(thread) = self.thread.as_ref().filter(|_| here) else {
            self.citations.reveal = None;
            return;
        };
        let found = cited_reply(&thread.state, load_failed, &citation.message_id);
        let Some((citation, window)) = self.citations.reveal.take() else { return };
        let item_id = match found {
            CitedReply::Found(item_id) => item_id,
            CitedReply::Loading => {
                self.citations.reveal = Some((citation, window));
                return;
            }
            CitedReply::Failed((title, message)) => {
                toast_later(window, title, message, cx);
                return;
            }
        };
        let mut opened = false;
        for key in self.entry_cache.folds_hiding(&item_id).to_vec() {
            opened |= self.expanded.insert(key);
        }
        if opened {
            self.refresh_rows(cx);
        }
        let row = self.rows.iter().position(|row| matches!(row, Row::Assistant { id, .. } if id.as_ref() == item_id));
        if let Some(index) = row {
            self.pending_position = None;
            self.scroller.update(cx, |scroller, cx| scroller.scroll_to_item(index, cx));
        }
        let kind = HighlightKind::Reveal { citation: Box::new(citation), window, revealed: false };
        self.set_citation_highlight(item_id.into(), kind, cx);
        self.citations._highlight_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REVEAL_HIGHLIGHT).await;
            let _ = this.update(cx, |this, cx| {
                if matches!(this.citations.highlight.as_ref().map(|h| &h.kind), Some(HighlightKind::Reveal { .. })) {
                    this.clear_citation_highlight(cx);
                }
            });
        }));
    }

    fn set_citation_highlight(&mut self, key: SharedString, kind: HighlightKind, cx: &mut Context<Self>) {
        self.clear_citation_highlight(cx);
        self.citations.highlight = Some(SourceHighlight { key: key.clone(), kind, painted: None });
        self.paint_citation(&key, cx);
    }

    fn clear_citation_highlight(&mut self, cx: &mut Context<Self>) {
        self.citations._highlight_timer = None;
        let Some(highlight) = self.citations.highlight.take() else { return };
        if highlight.painted.is_some()
            && self.find.is_none()
            && let Some(view) = self.markdown_view(&highlight.key)
        {
            view.update(cx, gpui_kit::base::TextViewState::clear_range_highlights);
        }
    }

    /// Paints the citation highlight in the markdown under `key` once its text is parsed; the
    /// find bar's matches take the view's highlights while it is open.
    pub(super) fn paint_citation(&mut self, key: &SharedString, cx: &mut Context<Self>) {
        if self.find.is_some() {
            return;
        }
        let Some(view) = self.markdown_view(key) else { return };
        let Some(highlight) = self.citations.highlight.as_mut().filter(|h| h.key == *key) else { return };
        let text = view.read(cx).rendered_text();
        if text.is_empty() || highlight.painted.as_ref() == Some(&text) {
            return;
        }
        let range = match &highlight.kind {
            // Painted once; the view carries it through later updates.
            HighlightKind::Comment(_) if highlight.painted.is_some() => return,
            HighlightKind::Comment(range) => Some(range.clone()).filter(|r| r.end <= text.len()),
            HighlightKind::Reveal { citation, .. } => assistant_citations::locate(text.as_str(), citation),
        };
        highlight.painted = Some(text);
        let reveal = match &mut highlight.kind {
            HighlightKind::Reveal { revealed, .. } => !std::mem::replace(revealed, true),
            HighlightKind::Comment(_) => false,
        };
        let Some(range) = range else {
            if let Some(SourceHighlight { kind: HighlightKind::Reveal { window, .. }, .. }) =
                self.citations.highlight.take()
            {
                let message = "Showing the source response. The saved quote is unchanged.";
                toast_later(window, "The quoted text has changed", message, cx);
            }
            return;
        };
        let color = tokens(cx).primary.opacity(0.45);
        view.update(cx, |view, cx| {
            let _ = view.set_range_highlights([RangeHighlight::new(range.clone(), color)], cx);
            if reveal {
                let _ = view.reveal_range(range, cx);
            }
        });
    }

    /// A sent citation's popover (`ContextChipPopover`): the quote, its comment and Go to source.
    pub(super) fn open_citation_menu(
        &mut self,
        citation: AssistantCitation,
        pane: Option<ChatPane>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let t = *tokens(cx);
        let view = cx.weak_entity();
        self.open_chat_menu(pane, position, window, cx, move |menu, _, _| {
            let (text, comment) = (citation.text.clone(), citation.comment.clone());
            menu.item(PopupMenuItem::element(move |_, _| quote_preview(&text, comment.as_deref(), &t)).disabled(true))
                .separator()
                .item(PopupMenuItem::new("Go to source").icon(Icon::new(Lucide::ArrowUpRight)).on_click(
                    move |_, window, cx| {
                        let citation = citation.clone();
                        let _ = view.update(cx, |this, cx| this.go_to_citation(citation, window, cx));
                    },
                ))
        });
    }
}

const UNAVAILABLE: (&str, &str) = (
    "The cited response is unavailable",
    "It may have been removed. The selected text is still saved in your citation.",
);
const COULD_NOT_LOAD: (&str, &str) = (
    "Could not load the cited response",
    "Load earlier turns, then click the citation to try again. Your saved quote is unchanged.",
);
const NOT_ASSISTANT: (&str, &str) =
    ("The citation does not refer to an assistant response", "The selected text is still saved in your citation.");

/// Where a citation stands against its thread as loaded so far (`loadCitedHistory`).
#[derive(Debug, PartialEq)]
enum CitedReply {
    Loading,
    /// The reply's timeline item.
    Found(String),
    /// The toast's title and message.
    Failed((&'static str, &'static str)),
}

fn cited_reply(state: &ThreadState, load_failed: bool, message_id: &str) -> CitedReply {
    if state.deleted {
        return CitedReply::Failed(UNAVAILABLE);
    }
    let Some(projection) = &state.projection else {
        return if load_failed { CitedReply::Failed(COULD_NOT_LOAD) } else { CitedReply::Loading };
    };
    let item = projection
        .visible_turn_items
        .iter()
        .map(|row| row.item.as_ref())
        .find(|item| item.raw.get("messageId").and_then(Value::as_str) == Some(message_id));
    match item {
        Some(item) if item.item_type == "assistant_message" => CitedReply::Found(item.id.clone()),
        Some(_) => CitedReply::Failed(NOT_ASSISTANT),
        None => CitedReply::Failed(UNAVAILABLE),
    }
}

/// `AssistantCitationQuote`: the quote as a block quote, then the comment.
fn quote_preview(text: &str, comment: Option<&str>, t: &Tokens) -> AnyElement {
    v_flex()
        .id("citation-quote")
        .w(px(360.))
        .max_h(px(256.))
        .overflow_y_scroll()
        .gap(px(12.))
        .p(px(4.))
        .text_sm()
        .whitespace_normal()
        .child(
            div().border_l_2().border_color(t.border).pl(px(12.)).text_color(t.muted_foreground).child(text.to_owned()),
        )
        .children(comment.map(|comment| div().text_color(t.foreground).child(comment.to_owned())))
        .into_any_element()
}

/// `resolveSelectionActionPosition`: the pointer, else just below the selection's end, kept
/// inside the timeline once it has laid out.
fn selection_action_position(
    timeline: Bounds<Pixels>,
    selection_end: Point<Pixels>,
    pointer: Option<Point<Pixels>>,
) -> Point<Pixels> {
    let at = pointer.unwrap_or(point(selection_end.x, selection_end.y + px(4.)));
    if timeline.size.width <= px(0.) || timeline.size.height <= px(0.) {
        return at;
    }
    let clamp = |value: Pixels, low: Pixels, high: Pixels| value.max(low).min(high);
    point(clamp(at.x, timeline.left(), timeline.right()), clamp(at.y, timeline.top(), timeline.bottom()))
}

/// `text` for a plain-text composer, its citations as chips.
pub(super) fn citation_content(text: &str) -> InputContent {
    let mut content = InputContent::new(text.to_owned());
    for (range, chip) in composer_text::chips(text, |_| false) {
        if !matches!(chip, Chip::Citation { .. }) {
            continue;
        }
        let token = InlineToken::new(chip.id(), &text[range.clone()]).with_label(chip.label().to_owned());
        content = match content.clone().with_token(range, token) {
            Ok(next) => next,
            Err(_) => content,
        };
    }
    content
}

/// A citation token as the composer draws it; another kind of token shows its label.
pub(super) fn citation_token(token: &InlineTokenContext, t: &Tokens) -> AnyElement {
    let label = token.token().label().clone();
    if !matches!(Chip::from_id(token.token().id()), Some(Chip::Citation { .. })) {
        return div().child(label).into_any_element();
    }
    citation_chip(label, t.fonts.prompt_size, token.is_selected(), t)
        .h(token.line_height())
        .max_w(token.available_width())
        .into_any_element()
}

/// The citation chip's look in the composer (`ContextChip kind="citation"`): the quote glyph, the
/// label up to 16em, and the pencil that edits the comment (`ContextChipAction`).
pub(super) fn citation_chip(label: SharedString, size: f32, selected: bool, t: &Tokens) -> gpui_kit::Div {
    let accent = rgb(CITATION_ACCENT);
    h_flex()
        .gap(px(size * 0.2))
        .rounded(px(4.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(accent)
        .when(selected, |el| el.bg(t.primary.opacity(0.25)))
        .child(Icon::new(Lucide::Quote).with_size(px(size)).flex_none())
        .child(div().min_w_0().max_w(px(size * 16.)).truncate().child(label))
        .child(
            div()
                .ml(px(size * 0.17))
                .size(px(size))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(2.))
                .hover(move |s| s.bg(gpui_kit::Hsla::from(accent).opacity(0.17)))
                .child(Icon::new(Lucide::Pencil).with_size(px(size * 0.85))),
        )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn loaded(items: &[(&str, &str, &str)]) -> ThreadState {
        let items: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, (id, item_type, message_id))| json!({"id": id, "type": item_type, "threadId": "t", "runId": "r1", "nodeId": null, "ordinal": i, "status": "completed", "updatedAt": "x", "messageId": message_id}))
            .collect();
        let rows: Vec<_> = items
            .iter()
            .enumerate()
            .map(|(i, item)| json!({"position": i, "visibility": "local", "sourceThreadId": "t", "sourceItemId": item["id"], "item": item}))
            .collect();
        let projection = json!({"thread": {"id": "t", "projectId": "p", "title": "t"}, "runs": [{"id": "r1", "status": "completed", "ordinal": 1}], "turnItems": items, "visibleTurnItems": rows});
        let mut state = ThreadState::default();
        state.apply(&json!({"kind": "snapshot", "snapshotSequence": 1, "projection": projection})).unwrap();
        state
    }

    #[test]
    fn a_citation_waits_for_its_thread_before_calling_the_reply_unavailable() {
        let unloaded = ThreadState::default();
        assert_eq!(cited_reply(&unloaded, false, "m1"), CitedReply::Loading);
        assert_eq!(cited_reply(&unloaded, true, "m1"), CitedReply::Failed(COULD_NOT_LOAD));

        let mut state = loaded(&[("u", "user_message", "m0"), ("a", "assistant_message", "m1")]);
        assert_eq!(cited_reply(&state, false, "m1"), CitedReply::Found("a".into()));
        assert_eq!(cited_reply(&state, false, "m0"), CitedReply::Failed(NOT_ASSISTANT));
        assert_eq!(cited_reply(&state, false, "gone"), CitedReply::Failed(UNAVAILABLE));

        state.deleted = true;
        assert_eq!(
            cited_reply(&state, false, "m1"),
            CitedReply::Failed(UNAVAILABLE),
            "a deleted thread keeps its rows"
        );
    }

    #[test]
    fn a_plain_text_composer_restores_citations_as_chips() {
        let citation = AssistantCitation {
            environment_id: "e".into(),
            thread_id: "t".into(),
            message_id: "m".into(),
            text: "quoted".into(),
            comment: None,
            start: 0,
            end: 6,
            prefix: String::new(),
            suffix: String::new(),
        };
        let markdown = citation.markdown();
        let text = format!("see {markdown} and [a](https://example.com)");
        let content = citation_content(&text);
        let ranges: Vec<_> = content.tokens().iter().map(gpui_kit::base::input::InlineTokenSpan::range).collect();
        assert_eq!(ranges, vec![4..4 + markdown.len()], "only the citation is a chip");
    }

    #[test]
    fn the_cite_button_sits_below_a_keyboard_selection_inside_the_timeline() {
        let timeline = Bounds::new(point(px(100.), px(50.)), gpui_kit::size(px(400.), px(300.)));
        let end = point(px(300.), px(200.));
        assert_eq!(selection_action_position(timeline, end, None), point(px(300.), px(204.)));
        let below = point(px(600.), px(500.));
        assert_eq!(selection_action_position(timeline, below, None), point(px(500.), px(350.)), "a long reply's end");
        let pointer = point(px(120.), px(60.));
        assert_eq!(selection_action_position(timeline, end, Some(pointer)), pointer);
        let unlaid = Bounds::default();
        assert_eq!(selection_action_position(unlaid, end, None), point(px(300.), px(204.)));
    }
}
