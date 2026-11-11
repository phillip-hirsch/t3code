//! Presentation a textarea draws over its source text without changing it: ranges drawn as
//! elements or hidden, whole lines in another font or indented, and blocks painted behind
//! lines. None of it enters the document, its history, or the caret's movement; the caret can
//! sit inside a replaced range. Ranges are UTF-8 bytes; an edit drops what it touches and
//! shifts what follows, so callers set the presentation again after each change.
use std::{ops::Range, rc::Rc};

use gpui::{AnyElement, App, Context, Hsla, Pixels, SharedString, Window, px};
use ropey::Rope;
use sum_tree::Bias;

use super::{InputBaseState, RopeExt as _, TextareaMode, display_map::WrapLineStyle};

/// Draws a replaced range. Called each frame the range is visible.
pub type InlineReplacementRenderer =
    Rc<dyn Fn(&InlineReplacementContext, &mut Window, &mut App) -> AnyElement>;

/// What a replacement's element is laid out in.
#[derive(Clone, Debug)]
pub struct InlineReplacementContext {
    range: Range<usize>,
    width: Pixels,
    line_height: Pixels,
}

impl InlineReplacementContext {
    pub(crate) fn new(range: Range<usize>, width: Pixels, line_height: Pixels) -> Self {
        Self {
            range,
            width,
            line_height,
        }
    }
    /// The replaced source range.
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
    /// The element's width: its inline width, or the line's indent for a gutter element.
    pub fn width(&self) -> Pixels {
        self.width
    }
    pub fn line_height(&self) -> Pixels {
        self.line_height
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ReplacementPlacement {
    /// In the text, `width` wide (clamped to the row).
    Inline(Pixels),
    /// In the indent of the line's first row, taking no inline space.
    Gutter,
}

/// A source range drawn as something else.
#[derive(Clone)]
pub struct InlineReplacement {
    pub(crate) range: Range<usize>,
    pub(crate) placement: ReplacementPlacement,
    pub(crate) render: Option<InlineReplacementRenderer>,
}

impl InlineReplacement {
    /// Takes no space and draws nothing.
    pub fn hidden(range: Range<usize>) -> Self {
        Self {
            range,
            placement: ReplacementPlacement::Inline(px(0.)),
            render: None,
        }
    }

    /// Draws `render` in the text, `width` wide; a width past the row fills the row.
    pub fn inline(
        range: Range<usize>,
        width: Pixels,
        render: impl Fn(&InlineReplacementContext, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            range,
            placement: ReplacementPlacement::Inline(width),
            render: Some(Rc::new(render)),
        }
    }

    /// Draws `render` in the indent [`LineStyle::indent`] gives the line it starts on, as a
    /// list marker sits beside its item; the range itself takes no space.
    pub fn gutter(
        range: Range<usize>,
        render: impl Fn(&InlineReplacementContext, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            range,
            placement: ReplacementPlacement::Gutter,
            render: Some(Rc::new(render)),
        }
    }

    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }

    pub(crate) fn is_gutter(&self) -> bool {
        self.placement == ReplacementPlacement::Gutter
    }

    pub(crate) fn renderer(&self) -> Option<&InlineReplacementRenderer> {
        self.render.as_ref()
    }
}

impl std::fmt::Debug for InlineReplacement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineReplacement")
            .field("range", &self.range)
            .field("placement", &self.placement)
            .field("drawn", &self.render.is_some())
            .finish()
    }
}

impl PartialEq for InlineReplacement {
    fn eq(&self, other: &Self) -> bool {
        self.range == other.range
            && self.placement == other.placement
            && match (&self.render, &other.render) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

/// One buffer line set in another font or size, or indented on every row.
#[derive(Clone, Debug, PartialEq)]
pub struct LineStyle {
    /// The line's text, without its newline.
    pub(crate) range: Range<usize>,
    pub(crate) family: Option<SharedString>,
    pub(crate) scale: f32,
    pub(crate) indent: Pixels,
}

impl LineStyle {
    /// `range` is the line's text from its first byte, without its newline.
    pub fn new(range: Range<usize>) -> Self {
        Self {
            range,
            family: None,
            scale: 1.,
            indent: px(0.),
        }
    }
    pub fn family(mut self, family: impl Into<SharedString>) -> Self {
        self.family = Some(family.into());
        self
    }
    /// The font size relative to the input's.
    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }
    /// Space before the text on every row; soft wraps narrow by it.
    pub fn indent(mut self, indent: Pixels) -> Self {
        self.indent = indent;
        self
    }
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
}

/// A rectangle behind whole lines: a framed block, or a bar beside them.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockBackdrop {
    /// Bytes on the first and last lines it spans.
    pub(crate) range: Range<usize>,
    pub(crate) left: Pixels,
    /// `None` reaches the text's right edge.
    pub(crate) width: Option<Pixels>,
    pub(crate) background: Hsla,
    pub(crate) border: Option<Hsla>,
    pub(crate) radius: Pixels,
}

impl BlockBackdrop {
    /// Fills the lines `range` touches, across the text.
    pub fn new(range: Range<usize>, background: Hsla) -> Self {
        Self {
            range,
            left: px(0.),
            width: None,
            background,
            border: None,
            radius: px(0.),
        }
    }
    /// A `width` wide strip `left` of the text's start.
    pub fn bar(mut self, left: Pixels, width: Pixels) -> Self {
        self.left = left;
        self.width = Some(width);
        self
    }
    pub fn border(mut self, color: Hsla) -> Self {
        self.border = Some(color);
        self
    }
    pub fn radius(mut self, radius: Pixels) -> Self {
        self.radius = radius;
        self
    }
}

/// Everything a textarea draws over its source text; see the module docs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextPresentation {
    /// Ascending and disjoint, and clear of inline tokens.
    pub replacements: Vec<InlineReplacement>,
    /// Ascending, one per line.
    pub line_styles: Vec<LineStyle>,
    pub backdrops: Vec<BlockBackdrop>,
}

impl TextPresentation {
    pub fn is_empty(&self) -> bool {
        self.replacements.is_empty() && self.line_styles.is_empty() && self.backdrops.is_empty()
    }

    /// Drops what an edit of `range` into `new_len` bytes touches and shifts what follows.
    pub(crate) fn adjust_for_edit(&mut self, range: &Range<usize>, new_len: usize) {
        if self.is_empty() {
            return;
        }
        let shift = |r: &Range<usize>| -> Option<Range<usize>> {
            if r.end <= range.start && r.start < range.start {
                return Some(r.clone());
            }
            if r.start >= range.end {
                let delta = new_len as isize - range.len() as isize;
                return Some(
                    r.start.checked_add_signed(delta)?..r.end.checked_add_signed(delta)?,
                );
            }
            None
        };
        self.replacements.retain_mut(|item| match shift(&item.range) {
            Some(range) => {
                item.range = range;
                true
            }
            None => false,
        });
        self.line_styles.retain_mut(|item| match shift(&item.range) {
            Some(range) => {
                item.range = range;
                true
            }
            None => false,
        });
        self.backdrops.retain_mut(|item| match shift(&item.range) {
            Some(range) => {
                item.range = range;
                true
            }
            None => false,
        });
    }

    /// The lines the wrapper sets in another font or narrows.
    pub(crate) fn wrap_styles(&self) -> Rc<[WrapLineStyle]> {
        self.line_styles
            .iter()
            .map(|style| WrapLineStyle {
                start: style.range.start,
                family: style.family.clone(),
                scale: style.scale,
                indent: style.indent,
            })
            .collect()
    }

    /// The width the row gives `item`: its own, at most what its line leaves of `max_width`.
    pub(crate) fn replacement_width(
        &self,
        item: &InlineReplacement,
        text: &Rope,
        max_width: Pixels,
    ) -> Pixels {
        match item.placement {
            ReplacementPlacement::Gutter => px(0.),
            ReplacementPlacement::Inline(width) => {
                let row = text.offset_to_point(item.range.start).row;
                let indent = self
                    .line_style(text.line_start_offset(row))
                    .map_or(px(0.), |style| style.indent);
                width.min((max_width - indent).max(px(0.)))
            }
        }
    }

    /// Each replaced range with the inline width it takes, for wrapping and layout.
    pub(crate) fn inline_metrics(
        &self,
        text: &Rope,
        max_width: Pixels,
    ) -> Vec<(Range<usize>, Pixels)> {
        self.replacements
            .iter()
            .map(|item| (item.range(), self.replacement_width(item, text, max_width)))
            .collect()
    }

    /// The style of the line starting at `line_start`.
    pub(crate) fn line_style(&self, line_start: usize) -> Option<&LineStyle> {
        let ix = self
            .line_styles
            .partition_point(|style| style.range.start < line_start);
        self.line_styles
            .get(ix)
            .filter(|style| style.range.start == line_start)
    }
}

impl InputBaseState<TextareaMode> {
    /// Replaces what the textarea draws over its text. See [`TextPresentation`].
    pub fn set_presentation(&mut self, presentation: TextPresentation, cx: &mut Context<Self>) {
        let len = self.text.len();
        let in_text = |range: &Range<usize>| {
            range.start <= range.end
                && range.end <= len
                && self.text.clip_offset(range.start, Bias::Left) == range.start
                && self.text.clip_offset(range.end, Bias::Left) == range.end
        };
        let tokens: Vec<Range<usize>> = self.token_spans().iter().map(|s| s.range()).collect();
        let mut presentation = presentation;
        // A replacement stays on one line, clear of tokens and of the one before it.
        presentation
            .replacements
            .sort_by_key(|item| (item.range.start, item.range.end));
        let mut end = 0;
        presentation.replacements.retain(|item| {
            let keep = in_text(&item.range)
                && item.range.start >= end
                && !self.text.slice(item.range.clone()).to_string().contains('\n')
                && !tokens
                    .iter()
                    .any(|token| token.start < item.range.end && item.range.start < token.end);
            if keep {
                end = item.range.end.max(item.range.start + 1);
            }
            keep
        });
        presentation
            .line_styles
            .retain(|item| in_text(&item.range) && self.text.line_start_offset(self.text.offset_to_point(item.range.start).row) == item.range.start);
        presentation.line_styles.sort_by_key(|item| item.range.start);
        presentation
            .line_styles
            .dedup_by_key(|item| item.range.start);
        presentation.backdrops.retain(|item| in_text(&item.range));
        if self.extras.presentation != presentation {
            self.extras.presentation = presentation;
            self.extras.presentation_revision += 1;
            cx.notify();
        }
    }

    pub fn text_presentation(&self) -> &TextPresentation {
        &self.extras.presentation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_drop_what_they_touch_and_shift_what_follows() {
        let mut presentation = TextPresentation {
            replacements: vec![
                InlineReplacement::hidden(0..2),
                InlineReplacement::hidden(5..7),
                InlineReplacement::hidden(10..12),
            ],
            line_styles: vec![LineStyle::new(0..4), LineStyle::new(9..14)],
            backdrops: vec![BlockBackdrop::new(9..14, gpui::black())],
        };
        presentation.adjust_for_edit(&(6..6), 3);
        let ranges: Vec<_> = presentation.replacements.iter().map(|r| r.range()).collect();
        assert_eq!(ranges, [0..2, 13..15]);
        assert_eq!(presentation.line_styles[0].range, 0..4);
        assert_eq!(presentation.line_styles[1].range, 12..17);
        assert_eq!(presentation.backdrops[0].range, 12..17);
        // Typing at a range's end keeps it; typing inside drops it.
        presentation.adjust_for_edit(&(4..4), 1);
        assert_eq!(presentation.line_styles.len(), 2);
        presentation.adjust_for_edit(&(2..2), 1);
        assert_eq!(presentation.line_styles.len(), 1);
        assert_eq!(presentation.line_style(14).map(LineStyle::range), Some(14..19));
    }
}
