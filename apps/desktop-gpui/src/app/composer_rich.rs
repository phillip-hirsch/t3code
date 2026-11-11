//! The rich composer's Markdown presentation (`ComposerPromptEditorTiptap`, `index.css`
//! `.composer-tiptap`): styled runs that show their delimiters only while the selection touches
//! them (`composer-markdown-source.ts`); headings, quotes, rules, list markers and task
//! checkboxes drawn over their syntax; and fenced code in a highlighted frame with a language
//! picker (`ComposerCodeBlockNodeView`). The text stays Markdown source: all of this is what the
//! editor draws over it (`TextPresentation`), rebuilt when the text, selection or theme changes.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::input::{BlockBackdrop, InlineReplacement, InlineReplacementContext, LineStyle, TextPresentation};
use gpui_kit::component::highlighter::SyntaxHighlighter;
use gpui_kit::component::input::{Input, InputEvent, InputState, TextDecoration};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FocusHandle, FontStyle, FontWeight, HighlightStyle,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Pixels, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, StrikethroughStyle, Styled as _, Subscription, WeakEntity, Window,
    anchored, deferred, div, prelude::FluentBuilder as _, px,
};

use super::Workspace;
use crate::composer_code;
use crate::composer_markdown::{
    self as markdown, Block, BlockKind, Gutter, ItemKind, LineKind, Marker, Object, Parsed,
};
use crate::theme::{Tokens, tokens};

/// `ComboboxPopup className="w-56"`.
const PICKER_WIDTH: f32 = 224.;
/// A replaced range this wide fills the rest of its row.
const ROW: f32 = 100_000.;

#[derive(Default)]
pub(super) struct RichUi {
    /// The selection the shown presentation was built for; `None` when none is shown.
    shown_for: Option<Range<usize>>,
    /// The caret last seen, to tell a step left out of a hidden prefix from a click.
    caret: usize,
    /// Code blocks' highlights by block, with the language, code and appearance they are for.
    highlights: Vec<CodeHighlights>,
    highlighters: HashMap<String, SyntaxHighlighter>,
    /// Each code block's language trigger, by block.
    triggers: Vec<FocusHandle>,
    /// Each task's checkbox, by task.
    checkboxes: Vec<FocusHandle>,
    pub(super) picker: Option<LanguagePicker>,
    /// When a press outside last closed the picker, so that press on its trigger doesn't reopen it.
    closed_at: Option<Instant>,
    anchors: Anchors,
}

/// Where each code block's language trigger was drawn, by block.
type Anchors = Rc<RefCell<HashMap<usize, Bounds<Pixels>>>>;

/// What the presentation's controls take: their focus handles, and where triggers record
/// their bounds.
struct Focus {
    triggers: Vec<FocusHandle>,
    checkboxes: Vec<FocusHandle>,
    anchors: Anchors,
}

struct CodeHighlights {
    grammar: String,
    code: String,
    dark: bool,
    styles: Vec<(Range<usize>, HighlightStyle)>,
}

/// `ComposerCodeBlockLanguagePicker` open on a code block.
pub(super) struct LanguagePicker {
    /// The code block, counted from the text's start.
    block: usize,
    query: Entity<InputState>,
    highlight: Option<usize>,
    scroll: ScrollHandle,
    _subscription: Subscription,
}

impl LanguagePicker {
    fn highlight(&mut self, index: usize) {
        self.highlight = Some(index);
        self.scroll.scroll_to_item(index);
    }
}

/// The prompt's sizes the presentation is laid out in, in px.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    rem: f32,
    prompt: f32,
    code: f32,
    /// The prompt font's `0` advance, CSS's `ch`.
    ch: f32,
}

/// The indent one list adds to its items (`index.css`).
fn gutter_width(gutter: Gutter, m: Metrics) -> f32 {
    match gutter {
        // `.composer-tiptap ul`: `max(1.25rem, 2ch)`.
        Gutter::Bullet => (1.25 * m.rem).max(2. * m.ch),
        // `--list-gutter`: a `ch` past the widest marker, at least `3ch`.
        Gutter::Ordered(widest) => (1.25 * m.rem).max((widest.max(2) + 1) as f32 * m.ch),
        // `li[data-checked]`: the 0.95rem box and the 0.5rem gap; a task list in a task list
        // adds its 1.375rem padding.
        Gutter::Task { nested } => 1.45 * m.rem + if nested { 1.375 * m.rem } else { 0. },
    }
}

fn item_indent(gutters: &[Gutter], m: Metrics) -> f32 {
    gutters.iter().map(|gutter| gutter_width(*gutter, m)).sum()
}

/// `:is(.chat-markdown, .composer-tiptap) h1`..`h6`, in rem.
fn heading_rems(level: u8) -> f32 {
    match level {
        1 => 1.25,
        2 => 1.125,
        3 => 1.,
        _ => 0.875,
    }
}

/// The web's `strong`, `em`, `s` and inline `code` (`.composer-tiptap`), and the delimiters a
/// selection shows (`.composer-rich-marker`).
fn mark_style(style: u8, t: &Tokens) -> HighlightStyle {
    use crate::composer_markdown::{BOLD, CODE, HEADING, ITALIC, MARKER, STRIKE};
    if style & MARKER != 0 {
        return HighlightStyle {
            color: Some(t.muted_foreground),
            fade_out: Some(0.25),
            font_weight: Some(FontWeight::NORMAL),
            ..Default::default()
        };
    }
    let weight =
        if style & BOLD != 0 { Some(FontWeight::BOLD) } else { (style & HEADING != 0).then_some(FontWeight::SEMIBOLD) };
    HighlightStyle {
        font_weight: weight,
        font_style: (style & ITALIC != 0).then_some(FontStyle::Italic),
        strikethrough: (style & STRIKE != 0).then_some(StrikethroughStyle { thickness: px(1.), color: None }),
        background_color: (style & CODE != 0).then(|| t.muted_foreground.opacity(0.12)),
        ..Default::default()
    }
}

/// Each fenced block's language and code range, in order.
fn code_blocks(parsed: &Parsed) -> Vec<(String, Range<usize>)> {
    let mut blocks: Vec<(String, Range<usize>)> = Vec::new();
    let mut open = false;
    for line in &parsed.lines {
        match &line.block {
            Block::FenceOpen { language, .. } => {
                blocks.push((language.clone(), line.range.end + 1..line.range.end + 1));
                open = true;
            }
            Block::Code { .. } if open => {
                if let Some((_, range)) = blocks.last_mut() {
                    range.end = line.range.end;
                }
            }
            _ => open = false,
        }
    }
    // A fence on the text's last line has no code after it.
    let len = parsed.lines.last().map_or(0, |line| line.range.end);
    for (_, range) in &mut blocks {
        range.start = range.start.min(len);
        range.end = range.end.clamp(range.start, len);
    }
    blocks
}

/// The opening fence of code block `block`.
fn fence_start(parsed: &Parsed, block: usize) -> Option<usize> {
    parsed.lines.iter().filter(|line| matches!(line.block, Block::FenceOpen { .. })).nth(block).map(|l| l.range.start)
}

impl Workspace {
    fn rich_text_enabled(&self) -> bool {
        self.client_settings.values.bool("composerRichTextEnabled")
    }

    /// The composer's text and its Markdown, chips parsed as atoms.
    fn composer_markdown(&self, cx: &App) -> (SharedString, Parsed) {
        let state = self.composer.read(cx);
        let text = state.value();
        let chips: Vec<_> = state.tokens().iter().map(gpui_kit::base::input::InlineTokenSpan::range).collect();
        let parsed = markdown::parse(&text, &chips);
        (text, parsed)
    }

    /// Repaints the composer's Markdown (`composerRichTextEnabled`); off, the text is plain.
    /// Runs per text or theme change, never per frame; the editor slides ranges in between.
    pub(super) fn sync_composer_marks(&mut self, cx: &mut Context<Self>) {
        let enabled = self.rich_text_enabled();
        if !enabled && self.composer_marks.is_none() {
            self.sync_composer_look(true, cx);
            return;
        }
        let decorations = if enabled {
            let t = *tokens(cx);
            let (text, parsed) = self.composer_markdown(cx);
            let mut decorations: Vec<TextDecoration> = parsed
                .spans
                .iter()
                .map(|span| TextDecoration::new(span.range.clone(), mark_style(span.style, &t)))
                .collect();
            decorations.extend(self.code_highlights(&text, &parsed, t.dark));
            decorations.sort_by_key(|decoration| decoration.range.start);
            decorations
        } else {
            Vec::new()
        };
        match &self.composer_marks {
            Some(marks) => marks.set(decorations, cx),
            None => {
                let marks = self.composer.update(cx, |state, cx| state.create_decorations_collection(decorations, cx));
                self.composer_marks = Some(marks);
            }
        }
        self.sync_composer_look(true, cx);
    }

    /// Syntax colors for each fenced block's code (`composerCodeBlockHighlight`); a block keeps
    /// its colors until its language or code changes.
    fn code_highlights(&mut self, text: &str, parsed: &Parsed, dark: bool) -> Vec<TextDecoration> {
        let rich = &mut self.composer_ui.rich;
        let blocks = code_blocks(parsed);
        rich.highlights.truncate(blocks.len());
        let theme = super::chat::highlight_theme(dark);
        let mut decorations = Vec::new();
        for (index, (language, range)) in blocks.iter().enumerate() {
            let grammar = composer_code::grammar(language).unwrap_or_default();
            let code = &text[range.clone()];
            let cached = rich
                .highlights
                .get(index)
                .is_some_and(|entry| entry.grammar == grammar && entry.code == code && entry.dark == dark);
            if !cached {
                let styles = if grammar.is_empty() || code.is_empty() {
                    Vec::new()
                } else {
                    let highlighter =
                        rich.highlighters.entry(grammar.clone()).or_insert_with(|| SyntaxHighlighter::new(&grammar));
                    highlighter.update(None, &gpui_kit::base::input::Rope::from(code), None);
                    highlighter.styles(&(0..code.len()), theme.as_ref())
                };
                let entry = CodeHighlights { grammar, code: code.to_owned(), dark, styles };
                match rich.highlights.get_mut(index) {
                    Some(slot) => *slot = entry,
                    None => rich.highlights.push(entry),
                }
            }
            let offset = range.start;
            decorations.extend(
                rich.highlights[index]
                    .styles
                    .iter()
                    .map(|(range, style)| TextDecoration::new(range.start + offset..range.end + offset, *style)),
            );
        }
        decorations
    }

    /// What the editor draws over the Markdown for the current selection: called for every
    /// caret move, and with `force` after the text or theme changed. A caret that lands in a
    /// hidden block prefix moves out of it first.
    pub(super) fn sync_composer_look(&mut self, force: bool, cx: &mut Context<Self>) {
        if !self.rich_text_enabled() {
            self.composer_ui.rich.picker = None;
            if self.composer_ui.rich.shown_for.take().is_some() {
                self.composer.update(cx, |state, cx| state.set_presentation(TextPresentation::default(), cx));
            }
            return;
        }
        let mut selection = self.composer.read(cx).selected_range();
        if !force && self.composer_ui.rich.shown_for.as_ref() == Some(&selection) {
            return;
        }
        let disabled = self.target.is_none();
        let (_, parsed) = self.composer_markdown(cx);
        if selection.is_empty() {
            let caret = markdown::snap_caret(&parsed, self.composer_ui.rich.caret, selection.start);
            if caret != selection.start {
                selection = caret..caret;
                self.composer.update(cx, |state, cx| state.set_selected_range(caret..caret, cx));
            }
            self.composer_ui.rich.caret = caret;
        }
        let look = markdown::look(&parsed, &selection);
        let blocks = parsed.lines.iter().filter(|line| matches!(line.block, Block::FenceOpen { .. })).count();
        let tasks = parsed
            .lines
            .iter()
            .filter(|line| matches!(line.block, Block::Item { kind: ItemKind::Task { .. }, .. }))
            .count();
        let rich = &mut self.composer_ui.rich;
        for (handles, len) in [(&mut rich.triggers, blocks), (&mut rich.checkboxes, tasks)] {
            while handles.len() < len {
                handles.push(cx.focus_handle().tab_stop(true));
            }
            handles.truncate(len);
        }
        if rich.picker.as_ref().is_some_and(|picker| picker.block >= blocks) {
            rich.picker = None;
        }
        rich.shown_for = Some(selection);
        let t = *tokens(cx);
        let font = gpui_kit::font(t.fonts.composer);
        let text_system = cx.text_system();
        let ch = f32::from(
            text_system.ch_advance(text_system.resolve_font(&font), px(t.fonts.prompt_size)).unwrap_or(px(8.)),
        );
        let m = Metrics { rem: t.fonts.interface_size, prompt: t.fonts.prompt_size, code: t.fonts.code_size, ch };
        let view = cx.weak_entity();
        let rich = &self.composer_ui.rich;
        let focus = Focus {
            triggers: rich.triggers.clone(),
            checkboxes: rich.checkboxes.clone(),
            anchors: rich.anchors.clone(),
        };
        let presentation = presentation(&parsed, &look, m, &t, &view, &focus, disabled);
        self.composer.update(cx, |state, cx| state.set_presentation(presentation, cx));
    }

    /// Backspace at a rendered block's start removes its hidden syntax in one step
    /// (`unwrapCodeBlockAtStart`, and a heading, quote or list item lifted to a paragraph).
    pub(super) fn block_backspace_key(
        &mut self,
        keystroke: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let m = keystroke.modifiers;
        if keystroke.key != "backspace" || m.modified() || self.question.is_some() || !self.rich_text_enabled() {
            return false;
        }
        let (_, parsed) = self.composer_markdown(cx);
        let edit = crate::composer_edit::block_backspace(&parsed, &self.composer.read(cx).selected_range());
        edit.is_some_and(|edit| self.apply_composer_edit(edit, window, cx))
    }

    /// A task's checkbox pressed: its source checked or cleared (`TaskItem`'s checkbox).
    fn toggle_composer_task(&mut self, check: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.is_none() || !self.rich_text_enabled() {
            return;
        }
        let (text, parsed) = self.composer_markdown(cx);
        // The checkbox may be from before an edit; it must still be a task's.
        let is_task = parsed.lines.iter().any(
            |line| matches!(&line.block, Block::Item { kind: ItemKind::Task { check: at, .. }, .. } if *at == check),
        );
        let selection = self.composer.read(cx).selected_range();
        let edit = is_task.then(|| crate::composer_edit::toggle_task(&text, check, selection)).flatten();
        if let Some(edit) = edit {
            self.apply_composer_edit(edit, window, cx);
        }
    }

    /// A press on a trigger: it closes its open picker, or opens one, unless the press itself
    /// just closed the picker from outside it.
    fn toggle_language_picker(&mut self, block: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer_ui.rich.picker.as_ref().is_some_and(|picker| picker.block == block) {
            self.close_language_picker(window, cx);
        } else if self.composer_ui.rich.closed_at.is_none_or(|at| at.elapsed() >= Duration::from_millis(300)) {
            self.open_language_picker(block, window, cx);
        }
    }

    /// Closes the picker after focus or a press went elsewhere, leaving focus there.
    fn dismiss_language_picker(&mut self, cx: &mut Context<Self>) {
        if self.composer_ui.rich.picker.take().is_some() {
            self.composer_ui.rich.closed_at = Some(Instant::now());
            cx.notify();
        }
    }

    fn open_language_picker(&mut self, block: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.is_none() {
            return;
        }
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search languages"));
        let subscription = cx.subscribe_in(&query, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                // `autoHighlight`: the first match.
                if let Some(picker) = &mut this.composer_ui.rich.picker {
                    picker.highlight(0);
                }
                cx.notify();
            }
            // Focus left the combobox: it closes, and focus stays where it went.
            InputEvent::Blur if window.is_window_active() => this.dismiss_language_picker(cx),
            _ => {}
        });
        query.update(cx, |query, cx| query.focus(window, cx));
        let mut picker =
            LanguagePicker { block, query, highlight: None, scroll: ScrollHandle::new(), _subscription: subscription };
        // The block's language starts highlighted, as Base UI opens a combobox on its value.
        let current = self.code_block_language(block, cx).unwrap_or_default();
        let selected = composer_code::choices(&current, "").iter().position(|(id, _)| *id == current).unwrap_or(0);
        picker.highlight(selected);
        self.composer_ui.rich.picker = Some(picker);
        cx.notify();
    }

    /// Closes the picker, focus going back to its trigger.
    fn close_language_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = self.composer_ui.rich.picker.take() {
            if let Some(trigger) = self.composer_ui.rich.triggers.get(picker.block) {
                window.focus(trigger, cx);
            }
            cx.notify();
        }
    }

    /// The picker's listed id for code block `block`'s language.
    fn code_block_language(&self, block: usize, cx: &App) -> Option<String> {
        let (_, parsed) = self.composer_markdown(cx);
        let open = fence_start(&parsed, block)?;
        parsed.lines.iter().find_map(|line| match &line.block {
            Block::FenceOpen { language, .. } if line.range.start == open => Some(composer_code::current(language)),
            _ => None,
        })
    }

    fn language_picker_rows(&self, cx: &App) -> Vec<(String, String)> {
        let Some(picker) = &self.composer_ui.rich.picker else { return Vec::new() };
        let current = self.code_block_language(picker.block, cx).unwrap_or_default();
        composer_code::choices(&current, &picker.query.read(cx).value())
    }

    /// `changeLanguage`: the fence's info string takes the language and the caret goes to the
    /// end of the code. Picking the language it has only closes the picker.
    fn pick_code_language(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(block) = self.composer_ui.rich.picker.as_ref().map(|picker| picker.block) else { return };
        if self.code_block_language(block, cx).as_deref() == Some(id.as_str()) {
            self.close_language_picker(window, cx);
            return;
        }
        self.composer_ui.rich.picker = None;
        let (text, parsed) = self.composer_markdown(cx);
        let edit = fence_start(&parsed, block)
            .and_then(|open| crate::composer_edit::set_code_language(&text, &parsed, open, &id));
        if let Some(edit) = edit {
            self.apply_composer_edit(edit, window, cx);
        }
        self.focus_composer(window, cx);
        cx.notify();
    }

    fn language_picker_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.language_picker_rows(cx);
        let Some(picker) = &mut self.composer_ui.rich.picker else { return };
        match event.keystroke.key.as_str() {
            "escape" => self.close_language_picker(window, cx),
            key @ ("down" | "up") => {
                let Some(last) = rows.len().checked_sub(1) else { return cx.stop_propagation() };
                picker.highlight(match (key == "down", picker.highlight) {
                    (true, Some(i)) if i < last => i + 1,
                    (true, _) => 0,
                    (false, Some(i)) if i > 0 => i - 1,
                    (false, _) => last,
                });
                cx.notify();
            }
            "enter" => {
                if let Some((id, _)) = picker.highlight.and_then(|i| rows.get(i)).cloned() {
                    self.pick_code_language(id, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

/// The editor's presentation for `look`; see [`markdown::look`].
fn presentation(
    parsed: &Parsed,
    look: &markdown::Look,
    m: Metrics,
    t: &Tokens,
    view: &WeakEntity<Workspace>,
    focus: &Focus,
    disabled: bool,
) -> TextPresentation {
    let t = *t;
    let line_end = |start: usize| {
        let index = parsed.lines.partition_point(|line| line.range.start < start);
        parsed.lines.get(index).map_or(start, |line| line.range.end)
    };
    let mut replacements: Vec<InlineReplacement> =
        look.hidden.iter().map(|range| InlineReplacement::hidden(range.clone())).collect();
    let mut tasks = 0;
    for object in &look.objects {
        replacements.push(match object {
            Object::Marker { range, marker } => {
                let checkbox = if matches!(marker, Marker::Task { .. }) {
                    tasks += 1;
                    focus.checkboxes.get(tasks - 1).cloned()
                } else {
                    None
                };
                let (marker, view) = (marker.clone(), view.clone());
                InlineReplacement::gutter(range.clone(), move |context, window, _| {
                    render_marker(&marker, checkbox.as_ref(), context, &view, disabled, &t, window)
                })
            }
            Object::Rule(range) => InlineReplacement::inline(range.clone(), px(ROW), move |context, _, _| {
                // `hr`: a 1px top border; `--contrast-muted-foreground` in dark mode.
                let color = if t.dark { t.muted_foreground } else { t.border };
                div()
                    .w(context.width())
                    .h(context.line_height())
                    .flex()
                    .items_center()
                    .child(div().w_full().h(px(1.)).bg(color))
                    .into_any_element()
            }),
            Object::FenceHeader { range, language, .. } => {
                let block = parsed
                    .lines
                    .iter()
                    .filter(|line| matches!(line.block, Block::FenceOpen { .. }) && line.range.start < range.start)
                    .count();
                let (language, view) = (language.clone(), view.clone());
                let (trigger, anchors) = (focus.triggers.get(block).cloned(), focus.anchors.clone());
                InlineReplacement::inline(range.clone(), px(ROW), move |context, window, cx| {
                    render_code_header(
                        block,
                        &language,
                        trigger.as_ref(),
                        &anchors,
                        context,
                        &view,
                        disabled,
                        &t,
                        window,
                        cx,
                    )
                })
            }
        });
    }
    let line_styles = look
        .lines
        .iter()
        .map(|(start, kind)| {
            let style = LineStyle::new(*start..line_end(*start));
            match kind {
                LineKind::Heading(level) => style.scale(heading_rems(*level) * m.rem / m.prompt),
                // `blockquote`: a 2px border and 0.8rem of padding.
                LineKind::Quote => style.indent(px(0.8 * m.rem + 2.)),
                LineKind::Item(gutters) => style.indent(px(item_indent(gutters, m))),
                // `pre`: `px-3`, in the code font and size.
                LineKind::Code => style.family(t.fonts.mono).scale(m.code / m.prompt).indent(px(12.)),
            }
        })
        .collect();
    let backdrops = look
        .blocks
        .iter()
        .map(|(range, kind)| match kind {
            // `border-inline-start: 2px solid color-mix(currentcolor 35%)`.
            BlockKind::Quote => BlockBackdrop::new(range.clone(), t.foreground.opacity(0.35)).bar(px(0.), px(2.)),
            // `MarkdownCodeBlockFrame`: `rounded-lg border border-border/70 bg-secondary`,
            // `dark:border-transparent dark:bg-input/32`.
            BlockKind::Code => {
                let frame = BlockBackdrop::new(range.clone(), t.background.blend(super::chat::code_background(&t)))
                    .radius(px(8.));
                if t.dark { frame } else { frame.border(t.border.opacity(0.7)) }
            }
        })
        .collect();
    TextPresentation { replacements, line_styles, backdrops }
}

/// A list item's marker in the indent its lists leave: the browser's disc, circle or square, the
/// number as written (`::marker { content: attr(data-marker) }`), or a task's checkbox.
fn render_marker(
    marker: &Marker,
    focus: Option<&FocusHandle>,
    context: &InlineReplacementContext,
    view: &WeakEntity<Workspace>,
    disabled: bool,
    t: &Tokens,
    window: &Window,
) -> AnyElement {
    let row = h_flex().w(context.width()).h(context.line_height()).justify_end().items_center();
    match marker {
        Marker::Bullet(depth) => {
            let dot = div().size(px(5.)).flex_none();
            let dot = match depth {
                0 | 1 => dot.rounded_full().bg(t.foreground),
                2 => dot.rounded_full().border_1().border_color(t.foreground),
                _ => dot.bg(t.foreground),
            };
            row.pr(px(7.)).child(dot).into_any_element()
        }
        Marker::Number(number) => row.pr(px(4.)).text_color(t.foreground).child(number.clone()).into_any_element(),
        Marker::Task { checked, check } => {
            let (checked, check) = (*checked, *check);
            let (click_view, key_view) = (view.clone(), view.clone());
            let size = 0.95 * t.fonts.interface_size;
            let ring = focus.is_some_and(|focus| focus.is_focused(window) && window.last_input_was_keyboard());
            let checkbox = div()
                .id(("composer-task", check))
                .when_some(focus, gpui_kit::InteractiveElement::track_focus)
                .size(px(size))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .border_1()
                .when(!disabled, gpui_kit::Styled::cursor_pointer)
                .map(|el| {
                    if checked {
                        el.bg(t.primary)
                            .border_color(t.primary)
                            .child(Icon::new(Lucide::Check).size(px(size - 4.)).text_color(t.primary_foreground))
                    } else {
                        el.border_color(t.muted_foreground).bg(t.background)
                    }
                })
                // The press is the checkbox's, not a caret placement in the text.
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .when(ring, |el| el.border_2().border_color(t.ring))
                .on_click(move |event, window, cx| {
                    if !event.is_keyboard() {
                        let _ = click_view.update(cx, |this, cx| this.toggle_composer_task(check, window, cx));
                    }
                })
                // Space toggles a checkbox.
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "space" && !event.keystroke.modifiers.modified() {
                        let _ = key_view.update(cx, |this, cx| this.toggle_composer_task(check, window, cx));
                    }
                    keep_keys_from_editor(event, cx);
                });
            row.pr(px(0.5 * t.fonts.interface_size)).child(checkbox).into_any_element()
        }
    }
}

/// A control drawn in the editor sits in the editor's key context, where keys it doesn't take
/// would edit the text; only focus moves and Escape go on.
fn keep_keys_from_editor(event: &KeyDownEvent, cx: &mut App) {
    if !matches!(event.keystroke.key.as_str(), "tab" | "escape") {
        cx.stop_propagation();
    }
}

/// `MarkdownCodeBlockFrame`'s header with `ComposerCodeBlockLanguagePicker`'s trigger in it.
#[expect(clippy::too_many_arguments, reason = "a replacement renderer's inputs, captured per block")]
fn render_code_header(
    block: usize,
    language: &str,
    trigger: Option<&FocusHandle>,
    anchors: &Anchors,
    context: &InlineReplacementContext,
    view: &WeakEntity<Workspace>,
    disabled: bool,
    t: &Tokens,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let current = composer_code::current(language);
    let label = composer_code::label(language);
    let (asset, color) = crate::file_icons::file_icon(&composer_code::file_name(&current), t.dark);
    let open =
        view.upgrade().is_some_and(|w| w.read(cx).composer_ui.rich.picker.as_ref().is_some_and(|p| p.block == block));
    let ring = trigger.is_some_and(|focus| focus.is_focused(window) && window.last_input_was_keyboard());
    let (toggle_view, key_view) = (view.clone(), view.clone());
    // `Button variant="ghost-muted" size="xs"`, `-ml-1.5` from the header's `pl-3`.
    let button = h_flex()
        .id(("composer-code-language", block))
        .when_some(trigger, gpui_kit::InteractiveElement::track_focus)
        .h(px(24.))
        .ml(px(-6.))
        .px(px(8.))
        .gap(px(4.))
        .rounded(px(6.))
        .border_1()
        .border_color(if ring { t.ring } else { gpui_kit::transparent_black() })
        .font_family(t.fonts.sans)
        .text_size(px(12.))
        .line_height(px(16.))
        .text_color(t.muted_foreground)
        .when(open, |el| el.bg(t.accent).text_color(t.foreground))
        .when(disabled, |el| el.opacity(0.64))
        .when(!disabled, |el| {
            el.cursor_pointer()
                .hover(|style| style.bg(t.accent).text_color(t.foreground))
                // The press is the trigger's, not a caret placement in the text.
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(move |event, window, cx| {
                    if !event.is_keyboard() {
                        let _ = toggle_view.update(cx, |this, cx| this.toggle_language_picker(block, window, cx));
                    }
                })
                // Enter, Space and the arrows open it (`ComboboxTrigger`).
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    let key = event.keystroke.key.as_str();
                    if matches!(key, "enter" | "space" | "down" | "up") && !event.keystroke.modifiers.modified() {
                        let _ = key_view.update(cx, |this, cx| {
                            if this.composer_ui.rich.picker.is_none() {
                                this.open_language_picker(block, window, cx);
                            }
                        });
                    }
                    keep_keys_from_editor(event, cx);
                })
        })
        .child(Icon::empty().path(asset).size(px(14.)).text_color(color))
        .child(div().max_w(px(160.)).truncate().child(label))
        .child(Icon::new(Lucide::ChevronDown).size(px(14.)).opacity(0.6));
    let anchors = anchors.clone();
    h_flex()
        .w(context.width())
        .h(context.line_height())
        .items_center()
        // The picker opens under the trigger, outside the editor (`render_language_picker`).
        .child(div().child(button).on_children_prepainted(move |bounds, _, _| {
            if let Some(bounds) = bounds.first() {
                anchors.borrow_mut().insert(block, *bounds);
            }
        }))
        .into_any_element()
}

impl Workspace {
    /// `ComboboxPopup align="start"` under the open picker's trigger. It renders outside the
    /// editor, so the editor's keys never reach the text while the picker has focus.
    pub(super) fn render_language_picker(
        &self,
        t: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.composer_ui.rich.picker.as_ref()?;
        let anchor = self.composer_ui.rich.anchors.borrow().get(&picker.block).copied()?;
        let rows = self.language_picker_rows(cx);
        let current = self.code_block_language(picker.block, cx).unwrap_or_default();
        let (query, highlight, scroll) = (&picker.query, picker.highlight, &picker.scroll);
        let search_focused = gpui_kit::Focusable::focus_handle(query.read(cx), cx).is_focused(window);
        let (fg, muted) = (t.foreground, t.muted_foreground);
        let below_sm = window.viewport_size().width < px(640.);
        let list: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(i, (id, label))| {
                let (asset, color) = crate::file_icons::file_icon(&composer_code::file_name(&id), t.dark);
                let selected = id == current;
                h_flex()
                    .id(SharedString::from(format!(
                        "composer-code-language-{}",
                        if id.is_empty() { "plain" } else { &id }
                    )))
                    .flex_none()
                    .min_h(px(if below_sm { 32. } else { 28. }))
                    .px(px(8.))
                    .py(px(4.))
                    .gap(px(8.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(highlight == Some(i), |el| el.bg(fg.opacity(0.09)))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if let Some(picker) =
                            this.composer_ui.rich.picker.as_mut().filter(|p| *hovered && p.highlight != Some(i))
                        {
                            picker.highlight = Some(i);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick_code_language(id.clone(), window, cx)))
                    .child(Icon::empty().path(asset).size(px(14.)).text_color(color))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .when(selected, |el| el.child(Icon::new(Lucide::Check).size(px(16.)).ml_auto()))
                    .into_any_element()
            })
            .collect();
        let empty = list.is_empty();
        let popup = v_flex()
            .id("composer-code-language-popup")
            .key_context(super::keymap::COMBOBOX_CONTEXT)
            .occlude()
            .w(px(PICKER_WIDTH))
            .max_h(px(368.))
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(fg.opacity(0.1))
            .bg(t.popover)
            .shadow_xl()
            .cursor_default()
            .text_left()
            .font_family(t.fonts.sans)
            .text_sm()
            .line_height(px(20.))
            .font_weight(FontWeight::NORMAL)
            .text_color(fg)
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.dismiss_language_picker(cx)))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.language_picker_key(event, window, cx);
            }))
            // `ComboboxSearchInput`: a search glyph and an underline.
            .child(
                div().px(px(12.)).pt(px(10.)).child(
                    h_flex()
                        .relative()
                        .pb(px(6.))
                        .border_b_1()
                        .border_color(if search_focused { t.ring } else { t.border.opacity(0.7) })
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top(px(5.))
                                .child(Icon::new(Lucide::Search).size(px(16.)).text_color(muted.opacity(0.55))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .h(px(26.))
                                .flex()
                                .items_center()
                                .pl(px(20.))
                                .child(Input::new(query).appearance(false).small().px_0().text_sm()),
                        ),
                ),
            )
            .child(
                v_flex()
                    .id("composer-code-language-list")
                    .track_scroll(scroll)
                    .flex_1()
                    .min_h_0()
                    .p(px(4.))
                    .overflow_y_scroll()
                    .children(list)
                    .when(empty, |el| {
                        el.child(div().p(px(8.)).text_center().text_color(muted).child("No matching languages."))
                    }),
            );
        Some(
            deferred(
                anchored()
                    .position(anchor.bottom_left() + gpui_kit::point(px(0.), px(4.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(popup),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics = Metrics { rem: 16., prompt: 14., code: 13., ch: 8. };

    #[test]
    fn lists_indent_their_items_like_the_web_gutters() {
        assert_eq!(gutter_width(Gutter::Bullet, M), 20.);
        // A wide numbering widens its list: `10.` takes 4ch.
        assert_eq!(gutter_width(Gutter::Ordered(2), M), 24.);
        assert_eq!(gutter_width(Gutter::Ordered(3), M), 32.);
        assert_eq!(gutter_width(Gutter::Task { nested: false }, M), 23.2);
        assert_eq!(gutter_width(Gutter::Task { nested: true }, M), 45.2);
        assert_eq!(item_indent(&[Gutter::Bullet, Gutter::Task { nested: false }], M), 43.2);
        assert_eq!(item_indent(&[Gutter::Task { nested: false }, Gutter::Task { nested: true }], M), 23.2 + 45.2);
    }

    #[test]
    fn code_blocks_span_their_code_lines() {
        let text = "a\n```rust\nfn a() {}\nb()\n```\n```\n";
        let blocks = code_blocks(&markdown::parse(text, &[]));
        assert_eq!(blocks, [("rust".to_owned(), 10..23), (String::new(), 32..32)]);
        assert_eq!(&text[10..23], "fn a() {}\nb()");
        assert_eq!(fence_start(&markdown::parse(text, &[]), 1), Some(28));
        assert_eq!(code_blocks(&markdown::parse("x\n```", &[])), [(String::new(), 5..5)]);
    }
}
