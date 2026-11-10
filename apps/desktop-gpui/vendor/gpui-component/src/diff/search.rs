//! Find within a diff, as Pierre's `CodeViewSearch` (T3 Code's patched
//! `@pierre/diffs`): the query runs over every file's line data rather than
//! the rendered rows, so lines in folded files, collapsed context and rows
//! never laid out are all found, and stepping to one reveals it.

use std::{collections::HashMap, ops::Range, rc::Rc};

use gpui::{
    AnyElement, App, ClickEvent, Entity, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    MouseButton, ParentElement as _, SharedString, Styled as _, Subscription, Window, div, px,
    rgba,
};
use rust_i18n::t;

use super::{DiffFile, DiffSide, DiffState};
use crate::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
};

/// Pierre's `MAX_SEARCH_MATCHES`.
pub(crate) const MAX_MATCHES: usize = 100_000;
/// Room the panel covers at the top of the viewport; a match under it is
/// scrolled clear (`SEARCH_PANEL_RESERVED_HEIGHT`).
pub(crate) const PANEL_RESERVED_HEIGHT: f32 = 48.;
/// Characters that end a word for Whole Word, besides ASCII controls and spaces.
const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

/// The panel's toggles; each starts off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SearchOptions {
    pub(crate) case_sensitive: bool,
    pub(crate) whole_word: bool,
    pub(crate) regex: bool,
}

/// One match, in the order the unified view shows its line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SearchMatch {
    pub(crate) file: usize,
    pub(crate) path: SharedString,
    pub(crate) side: DiffSide,
    /// The line's index on its side.
    pub(crate) line: usize,
    pub(crate) line_number: usize,
    /// Byte range in the line's text.
    pub(crate) range: Range<usize>,
    /// An unchanged line, which split mode paints on both sides.
    pub(crate) context: bool,
}

impl SearchMatch {
    /// Pierre's `findSameMatch`: the match a new search keeps as current.
    fn same_place(&self, other: &Self) -> bool {
        self.path == other.path
            && self.side == other.side
            && self.line_number == other.line_number
            && self.range.start == other.range.start
    }
}

/// Each file's searched lines in unified order: within a run of changes the
/// deletions, then the additions; an unchanged line once, by its modified side.
fn unified_lines(file: &DiffFile) -> Vec<(DiffSide, usize, bool)> {
    let mut hunk_starts = file
        .hunks()
        .iter()
        .map(|hunk| hunk.pairs().start)
        .peekable();
    let mut lines = Vec::with_capacity(file.lines_count(DiffSide::Modified));
    let mut added = Vec::new();
    for (ix, pair) in file.pairs().iter().enumerate() {
        while hunk_starts.next_if(|start| *start <= ix).is_some() {
            lines.append(&mut added);
        }
        if pair.is_changed() {
            lines.extend(
                pair.original()
                    .map(|line| (DiffSide::Original, line, false)),
            );
            added.extend(
                pair.modified()
                    .map(|line| (DiffSide::Modified, line, false)),
            );
            continue;
        }
        lines.append(&mut added);
        match (pair.modified(), pair.original()) {
            (Some(line), _) => lines.push((DiffSide::Modified, line, true)),
            (None, Some(line)) => lines.push((DiffSide::Original, line, true)),
            (None, None) => {}
        }
    }
    lines.append(&mut added);
    lines
}

fn is_separator(c: Option<char>) -> bool {
    c.is_none_or(|c| (c as u32) <= 32 || c as u32 == 127 || WORD_SEPARATORS.contains(c))
}

/// Pierre's `collectMatches`: every match of `query` across `files`, in render
/// order. An empty or multi-line query, or an invalid pattern, finds nothing.
/// `fancy_regex` gives the web's RegExp lookaround and backreferences, and
/// runs any pattern without them on the plain `regex` engine.
pub(crate) fn collect_matches(
    files: &[DiffFile],
    query: &str,
    options: SearchOptions,
) -> Vec<SearchMatch> {
    if query.is_empty() || query.contains('\n') {
        return Vec::new();
    }
    let pattern = if options.regex {
        query.into()
    } else {
        fancy_regex::escape(query)
    };
    let Ok(pattern) = fancy_regex::RegexBuilder::new(&pattern)
        .case_insensitive(!options.case_sensitive)
        .build()
    else {
        return Vec::new();
    };
    let mut matches = Vec::new();
    for (file_ix, file) in files.iter().enumerate() {
        for (side, line, context) in unified_lines(file) {
            let source = &file.lines(side)[line];
            let text = source.text().as_str();
            // A line that exceeds the backtracking limit keeps the matches before it.
            for found in pattern.find_iter(text).map_while(Result::ok) {
                if found.range().is_empty()
                    || options.whole_word
                        && !(is_separator(text[..found.start()].chars().next_back())
                            && is_separator(text[found.end()..].chars().next()))
                {
                    continue;
                }
                matches.push(SearchMatch {
                    file: file_ix,
                    path: file.path().clone(),
                    side,
                    line,
                    line_number: source.line_number(),
                    range: found.range(),
                    context,
                });
                if matches.len() >= MAX_MATCHES {
                    return matches;
                }
            }
        }
    }
    matches
}

/// The match after `current`, or before it, wrapping at both ends. With no
/// current match, next is the first and previous the last.
pub(crate) fn step(current: Option<usize>, total: usize, forward: bool) -> Option<usize> {
    let last = total.checked_sub(1)?;
    Some(match (current.map(|ix| ix.min(last)), forward) {
        (None, true) => 0,
        (None, false) => last,
        (Some(ix), true) if ix == last => 0,
        (Some(ix), true) => ix + 1,
        (Some(0), false) => last,
        (Some(ix), false) => ix - 1,
    })
}

/// The matches on each painted line. An unchanged line is painted on both
/// sides in split mode, so its match is listed under its counterpart too.
pub(crate) type LineMatches = HashMap<(usize, DiffSide, usize), Vec<usize>>;

fn index_lines(files: &[DiffFile], matches: &[SearchMatch]) -> LineMatches {
    let mut lines = LineMatches::new();
    for (ix, found) in matches.iter().enumerate() {
        lines
            .entry((found.file, found.side, found.line))
            .or_default()
            .push(ix);
        let file = &files[found.file];
        let source = &file.lines(found.side)[found.line];
        if found.context
            && let Some(counterpart) = source.counterpart()
            && file.lines(found.side.other())[counterpart].text() == source.text()
        {
            lines
                .entry((found.file, found.side.other(), counterpart))
                .or_default()
                .push(ix);
        }
    }
    lines
}

/// An open find panel and its matches, owned by [`DiffState`].
pub(crate) struct DiffSearch {
    pub(crate) input: Entity<InputState>,
    pub(crate) options: SearchOptions,
    pub(crate) matches: Rc<Vec<SearchMatch>>,
    pub(crate) lines: Rc<LineMatches>,
    pub(crate) current: Option<usize>,
    /// The match to bring into view horizontally once its row is laid out.
    pub(crate) reveal_x: Option<usize>,
    pub(crate) _subscription: Subscription,
}

impl DiffSearch {
    pub(crate) fn new(input: Entity<InputState>, subscription: Subscription) -> Self {
        Self {
            input,
            options: SearchOptions::default(),
            matches: Rc::default(),
            lines: Rc::default(),
            current: None,
            reveal_x: None,
            _subscription: subscription,
        }
    }

    /// Installs a new result, keeping the current match where it survives.
    pub(crate) fn set_matches(&mut self, files: &[DiffFile], matches: Vec<SearchMatch>) {
        let previous = self.current.and_then(|ix| self.matches.get(ix)).cloned();
        self.current = previous
            .and_then(|previous| matches.iter().position(|found| found.same_place(&previous)));
        self.lines = Rc::new(index_lines(files, &matches));
        self.matches = Rc::new(matches);
        self.reveal_x = None;
    }

    pub(crate) fn paint(&self) -> SearchPaint {
        SearchPaint {
            matches: self.matches.clone(),
            lines: self.lines.clone(),
            current: self.current,
        }
    }

    /// The panel's count: Pierre's "No results", "N results" or "I of N".
    fn label(&self) -> String {
        match (self.matches.len(), self.current) {
            (0, _) => t!("Diff.SearchNoResults").to_string(),
            (total, Some(current)) => {
                t!("Diff.SearchMatchOf", index = current + 1, total = total).to_string()
            }
            (total, None) => t!("Diff.SearchResults", count = total).to_string(),
        }
    }
}

/// What the rows read to paint matches.
#[derive(Clone)]
pub(crate) struct SearchPaint {
    matches: Rc<Vec<SearchMatch>>,
    lines: Rc<LineMatches>,
    current: Option<usize>,
}

impl SearchPaint {
    /// Each match on a line as a byte range of its text, and whether it is current.
    pub(crate) fn line(
        &self,
        file: usize,
        side: DiffSide,
        line: usize,
    ) -> impl Iterator<Item = (Range<usize>, bool)> + '_ {
        self.lines
            .get(&(file, side, line))
            .into_iter()
            .flatten()
            .map(|&ix| (self.matches[ix].range.clone(), self.current == Some(ix)))
    }
}

/// Pierre's match colours: `--diffs-search-match-bg` for every match and
/// `--diffs-search-focus-bg` (the modified colour at 45%) for the current one.
pub(crate) fn match_colors(cx: &App) -> (Hsla, Hsla) {
    if cx.theme().is_dark() {
        (rgba(0xff963266).into(), rgba(0x69b1ff73).into())
    } else {
        (rgba(0xff963288).into(), rgba(0x009fff73).into())
    }
}

/// The find panel, floating at the top right of the diff body.
pub(crate) fn render_panel(search: &DiffSearch, state: &Entity<DiffState>, cx: &App) -> AnyElement {
    let has_matches = !search.matches.is_empty();
    let theme = cx.theme();
    let muted = theme.muted_foreground;
    let accent = theme.primary;
    let icon_button = |id: &'static str, icon: IconName, label: SharedString| {
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(icon)
            .rounded(px(999.))
            .text_color(muted)
            .tooltip(label.clone())
            .accessibility_label(label)
    };
    let toggle = |id: &'static str,
                  icon: gpui_kit_assets::IconName,
                  label: String,
                  on: bool,
                  flip: fn(&mut SearchOptions)| {
        let state = state.clone();
        Button::new(id)
            .ghost()
            .xsmall()
            .compact()
            .icon(icon)
            .rounded(px(4.))
            .text_color(if on { accent } else { muted })
            .tooltip(label.clone())
            .accessibility_label(label)
            .toggled(on)
            .on_click(move |_, _, cx| {
                state.update(cx, |state, cx| state.update_search_options(flip, cx))
            })
    };
    let options = search.options;
    let step = |forward: bool| {
        let state = state.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            state.update(cx, |state, cx| state.step_search(forward, cx))
        }
    };
    let close = {
        let state = state.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            state.update(cx, |state, cx| state.close_search(window, cx))
        }
    };
    let escape = {
        let state = state.clone();
        move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
            if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                cx.stop_propagation();
                state.update(cx, |state, cx| state.close_search(window, cx));
            }
        }
    };
    h_flex()
        .id("diff-search")
        .absolute()
        .top(px(8.))
        .left(px(12.))
        .right(px(16.))
        .justify_end()
        .child(
            h_flex()
                .id("diff-search-panel")
                .occlude()
                .capture_key_down(escape)
                // Presses here never start a text or line selection.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .h(px(36.))
                .min_w_0()
                .gap(px(2.))
                .pl(px(4.))
                .pr(px(4.))
                .rounded(px(16.))
                .border_1()
                .border_color(theme.foreground.opacity(0.1))
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .font_family(theme.font_family.clone())
                .shadow_lg()
                .child(
                    div().w(px(200.)).min_w_0().child(
                        Input::new(&search.input).appearance(false).small().suffix(
                            h_flex()
                                .gap(px(1.))
                                .child(toggle(
                                    "diff-search-case",
                                    gpui_kit_assets::IconName::CaseSensitive,
                                    t!("Diff.SearchMatchCase").to_string(),
                                    options.case_sensitive,
                                    |o| o.case_sensitive = !o.case_sensitive,
                                ))
                                .child(toggle(
                                    "diff-search-word",
                                    gpui_kit_assets::IconName::WholeWord,
                                    t!("Diff.SearchWholeWord").to_string(),
                                    options.whole_word,
                                    |o| o.whole_word = !o.whole_word,
                                ))
                                .child(toggle(
                                    "diff-search-regex",
                                    gpui_kit_assets::IconName::Regex,
                                    t!("Diff.SearchRegex").to_string(),
                                    options.regex,
                                    |o| o.regex = !o.regex,
                                )),
                        ),
                    ),
                )
                .child(
                    div()
                        .flex_none()
                        .min_w(px(50.))
                        .px(px(4.))
                        .text_center()
                        .text_xs()
                        .whitespace_nowrap()
                        .text_color(if has_matches {
                            muted
                        } else {
                            muted.opacity(0.7)
                        })
                        .child(search.label()),
                )
                .child(
                    icon_button(
                        "diff-search-previous",
                        IconName::ArrowUp,
                        t!("Diff.SearchPrevious").into(),
                    )
                    .disabled(!has_matches)
                    .on_click(step(false)),
                )
                .child(
                    icon_button(
                        "diff-search-next",
                        IconName::ArrowDown,
                        t!("Diff.SearchNext").into(),
                    )
                    .disabled(!has_matches)
                    .on_click(step(true)),
                )
                .child(
                    icon_button(
                        "diff-search-close",
                        IconName::Close,
                        t!("Diff.SearchClose").into(),
                    )
                    .on_click(close),
                ),
        )
        .into_any_element()
}
