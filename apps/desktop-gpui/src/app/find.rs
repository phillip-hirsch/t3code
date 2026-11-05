//! The thread find bar (`ThreadFindBar.tsx`, `useThreadFind.ts`, `threadFindStore.ts`): the open
//! thread is searched on the server (`orchestration.searchThread`), Enter and the arrows step
//! through its matches, and the timeline scrolls to each and paints them in the messages'
//! markdown (`threadFindHighlights.ts`).

use std::collections::HashMap;
use std::ops::Range;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::{RangeHighlight, RenderedText};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, Subscription, Task, Window, div, px, rgb,
};
use serde_json::{Value, json};

use super::{Target, Workspace};
use crate::model::ThreadProjection;
use crate::timeline::{self, Row};

const QUERY_DEBOUNCE: Duration = Duration::from_millis(100);
/// Streamed content re-runs an open search this long after it lands.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(300);
const MAX_QUERY: usize = 200;
/// The bar's height plus the details card gap (`THREAD_FIND_BAR_RESERVED_HEIGHT`).
pub(super) const RESERVED_HEIGHT: f32 = 36. + 12.;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct FindMatch {
    pub(super) entry_id: String,
    pub(super) occurrence: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct NavEntry {
    entry_id: String,
    start: usize,
    count: usize,
}

/// `OrchestrationV2SearchThreadResult`.
#[derive(Clone, Debug, PartialEq)]
struct FindResult {
    total: usize,
    active: usize,
    /// False while a progressive search is still counting.
    complete: bool,
    matched: Option<FindMatch>,
    navigation: Vec<NavEntry>,
}

impl FindResult {
    fn parse(value: &Value) -> Option<Self> {
        let index = |v: &Value, key: &str| v.get(key).and_then(Value::as_u64).map(|n| n as usize);
        Some(Self {
            total: index(value, "totalMatches")?,
            active: index(value, "activeIndex").unwrap_or(0),
            complete: value.get("complete").and_then(Value::as_bool) != Some(false),
            matched: value.get("match").and_then(|m| {
                Some(FindMatch {
                    entry_id: m.get("entryId")?.as_str()?.to_owned(),
                    occurrence: index(m, "occurrence")?,
                })
            }),
            navigation: value
                .get("navigation")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|e| {
                    Some(NavEntry {
                        entry_id: e.get("entryId")?.as_str()?.to_owned(),
                        start: index(e, "startIndex")?,
                        count: index(e, "count")?,
                    })
                })
                .collect(),
        })
    }

    /// The match `delta` away, when the navigation window holds it, so stepping needs no request.
    fn stepped(&self, delta: i64) -> Option<Self> {
        let next = step_index(self.active, self.total, delta);
        let entry =
            self.navigation.iter().find(|e| next.checked_sub(e.start).is_some_and(|offset| offset < e.count))?;
        let matched = FindMatch { entry_id: entry.entry_id.clone(), occurrence: next - entry.start };
        Some(Self { active: next, matched: Some(matched), ..self.clone() })
    }
}

/// `stepThreadFindIndex`: wraps around both ends.
/// The counts come from the server, so no step may overflow.
fn step_index(index: usize, total: usize, delta: i64) -> usize {
    if total == 0 {
        return 0;
    }
    let index = index.min(total - 1);
    let step = usize::try_from(delta.unsigned_abs()).unwrap_or(usize::MAX) % total;
    let room = total - index;
    match (delta >= 0, step) {
        (true, step) if step < room => index + step,
        (true, step) => step - room,
        (false, step) if step <= index => index - step,
        (false, step) => total - (step - index),
    }
}

/// `formatThreadFindCount`: "3/12", or "0/0".
fn count_label(index: usize, total: usize) -> String {
    if total == 0 { "0/0".into() } else { format!("{}/{total}", index.min(total - 1) + 1) }
}

/// `foldThreadSearchCase` per char: lowercased when that keeps one char, final sigma as
/// sigma, curly apostrophes straight.
fn fold_char(c: char) -> char {
    let mut lower = c.to_lowercase();
    let folded = match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    };
    match folded {
        'ς' => 'σ',
        '‘' | '’' => '\'',
        other => other,
    }
}

/// `findThreadSearchOccurrences`: non-overlapping byte ranges of `query` in `text`, ignoring case.
pub(super) fn occurrences(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle: Vec<char> = query.chars().map(fold_char).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let chars: Vec<(usize, char)> = text.char_indices().map(|(at, c)| (at, fold_char(c))).collect();
    let mut ranges = Vec::new();
    let mut at = 0;
    while at + needle.len() <= chars.len() {
        if chars[at..at + needle.len()].iter().map(|(_, c)| *c).eq(needle.iter().copied()) {
            let end = chars.get(at + needle.len()).map_or(text.len(), |(byte, _)| *byte);
            ranges.push(chars[at].0..end);
            at += needle.len();
        } else {
            at += 1;
        }
    }
    ranges
}

/// `findForMessageMatch`'s query: the whole query where the snippet holds it as typed, else its
/// longest word as the snippet writes it, preferring a whole word.
pub(super) fn query_for_message(snippet: &str, query: &str) -> String {
    let words: Vec<String> = query
        .trim_matches(['"', '\'', '“', '”'])
        .split_whitespace()
        .map(|w| w.replace('’', "'").to_lowercase())
        .collect();
    let chars: Vec<char> = snippet.chars().map(|c| if c == '’' || c == '‘' { '\'' } else { c }).collect();
    if words.len() > 1
        && let Some(range) = phrase_at(&chars, &words)
    {
        return chars[range].iter().collect();
    }
    let word = words
        .iter()
        .filter(|w| crate::search::has_long_token(w))
        .fold("", |longest, w| if w.chars().count() > longest.chars().count() { w } else { longest });
    let word = if word.is_empty() { query.trim() } else { word };
    let ranges = crate::search::token_ranges(snippet, word);
    let alnum = |at: usize| chars.get(at).is_some_and(|c| c.is_alphanumeric());
    match ranges.iter().find(|r| !alnum(r.end)).or(ranges.first()) {
        Some(range) => chars[range.start.min(chars.len())..range.end.min(chars.len())].iter().collect(),
        None => word.to_owned(),
    }
}

/// `messagePhrase`: the words in order, whitespace between, not inside other words.
fn phrase_at(chars: &[char], words: &[String]) -> Option<Range<usize>> {
    let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let boundary = |at: Option<usize>| at.and_then(|at| chars.get(at)).is_none_or(|c| !c.is_alphanumeric());
    'start: for start in 0..lower.len() {
        if !boundary(start.checked_sub(1)) {
            continue;
        }
        let mut at = start;
        for (index, word) in words.iter().enumerate() {
            if index > 0 {
                let gap = lower[at..].iter().take_while(|c| c.is_whitespace()).count();
                if gap == 0 {
                    continue 'start;
                }
                at += gap;
            }
            for w in word.chars() {
                if lower.get(at) != Some(&w) {
                    continue 'start;
                }
                at += 1;
            }
        }
        if boundary(Some(at)) {
            return Some(start..at);
        }
    }
    None
}

/// The turn item a search entry names: a message's `messageId`, else the item's own id.
fn entry_item<'a>(projection: &'a ThreadProjection, entry_id: &str) -> Option<&'a crate::model::TurnItem> {
    projection
        .visible_turn_items
        .iter()
        .map(|row| row.item.as_ref())
        .find(|item| item.raw.get("messageId").and_then(Value::as_str) == Some(entry_id) || item.id == entry_id)
}

fn item_entry(projection: &ThreadProjection, item_id: &str) -> Option<String> {
    let item = projection.visible_turn_items.iter().find(|row| row.item.id == item_id)?;
    Some(item.item.raw.get("messageId").and_then(Value::as_str).unwrap_or(&item.item.id).to_owned())
}

fn searchable_row(row: &Row) -> Option<&SharedString> {
    match row {
        Row::User { id, .. } | Row::Assistant { id, .. } | Row::Plan { id, .. } => Some(id),
        _ => None,
    }
}

pub(super) struct ThreadFind {
    thread_id: String,
    input: Entity<InputState>,
    /// The query searched: trimmed, at most 200 chars.
    query: String,
    result: Option<FindResult>,
    loading: bool,
    failed: bool,
    /// Bumped per request, so a late response for an older one is dropped.
    generation: u64,
    /// Where the next search starts, and how far past it.
    start: Option<FindMatch>,
    offset: i64,
    /// The entry the timeline last scrolled to.
    revealed: Option<String>,
    /// Per markdown view, the text, active occurrence and query it was last painted for.
    painted: HashMap<SharedString, (RenderedText, Option<usize>, String)>,
    _search: Option<Task<()>>,
    _refresh: Option<Task<()>>,
    _subscription: Subscription,
}

impl Workspace {
    /// The server can search threads (`serverConfig.threadFind`) and a thread is open.
    pub(super) fn thread_find_available(&self) -> bool {
        matches!(self.target, Some(Target::Thread { .. })) && self.server_info().config["threadFind"] == true
    }

    /// The open find, while it belongs to the thread on screen.
    fn open_find(&self) -> Option<&ThreadFind> {
        let find = self.find.as_ref()?;
        (self.open_thread_id() == Some(&*find.thread_id)).then_some(find)
    }

    /// `chat.find`: opens find, or refocuses it and restarts its search from the viewport.
    /// Returns false when this thread can't be searched.
    pub(super) fn open_thread_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(Target::Thread { id: thread_id, .. }) = self.target.clone().filter(|_| self.thread_find_available())
        else {
            return false;
        };
        if self.open_find().is_none() {
            self.create_find(thread_id, window, cx);
        }
        let start = self.viewport_start(cx);
        let Some(find) = &mut self.find else { return false };
        let input = find.input.clone();
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        if !find.query.is_empty() {
            find.start = start;
            find.offset = 0;
            self.search_find(Duration::ZERO, cx);
        }
        cx.notify();
        true
    }

    /// The palette's `chat.find` path: a toast says why find can't open (`CommandPalette.tsx`).
    pub(super) fn open_thread_find_or_explain(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open_thread_find(window, cx) && matches!(self.target, Some(Target::Thread { .. })) {
            let note = Notification::new()
                .title("Thread search is unavailable on this server.")
                .message("Update the server to enable it.");
            window.push_notification(note, cx);
        }
    }

    /// `openThreadFindOnQuery`: opens find on `query` in the open thread, starting at `entry_id`.
    pub(super) fn open_thread_find_on(
        &mut self,
        query: &str,
        entry_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.open_thread_find(window, cx) {
            return;
        }
        let Some(find) = &mut self.find else { return };
        find.start = entry_id.map(|entry_id| FindMatch { entry_id, occurrence: 0 });
        find.offset = 0;
        // The input's change event runs the search from `start`.
        find.query.clear();
        let input = find.input.clone();
        input.update(cx, |input, cx| {
            input.set_value(query.to_owned(), window, cx);
            input.select_all(window, cx);
        });
        self.find_query_changed(Some(Duration::ZERO), cx);
    }

    fn create_find(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_find_highlights(cx);
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in thread"));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.find_query_changed(None, cx),
            InputEvent::PressEnter { shift, .. } => this.step_find(if *shift { -1 } else { 1 }, window, cx),
            _ => {}
        });
        self.find = Some(ThreadFind {
            thread_id,
            input,
            query: String::new(),
            result: None,
            loading: false,
            failed: false,
            generation: 0,
            start: None,
            offset: 0,
            revealed: None,
            painted: HashMap::new(),
            _search: None,
            _refresh: None,
            _subscription: subscription,
        });
        // Opening find is manual navigation: it releases live follow (`announceOpening`).
        self.scroller.update(cx, |scroller, cx| {
            let top = scroller.logical_scroll_top().item_ix;
            scroller.scroll_to_item(top, cx);
        });
    }

    pub(super) fn close_thread_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_some() {
            self.clear_find_highlights(cx);
            self.find = None;
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    /// `setThreadFindQuery`: a new query searches from the first message on screen.
    fn find_query_changed(&mut self, delay: Option<Duration>, cx: &mut Context<Self>) {
        let start = self.viewport_start(cx);
        let Some(find) = &mut self.find else { return };
        let value = find.input.read(cx).value();
        let query: String = value.trim().chars().take(MAX_QUERY).collect::<String>().trim().to_owned();
        if query == find.query {
            return;
        }
        find.query = query;
        find.result = None;
        find.failed = false;
        find.revealed = None;
        if delay.is_none() || find.start.is_none() {
            find.start = start;
        }
        find.offset = 0;
        if find.query.is_empty() {
            find.loading = false;
            find._search = None;
            self.paint_all_find(cx);
            cx.notify();
            return;
        }
        self.search_find(delay.unwrap_or(QUERY_DEBOUNCE), cx);
    }

    /// Runs `orchestration.searchThread` for the find's query from its start and offset.
    fn search_find(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let client = self.client();
        let Some(find) = &mut self.find else { return };
        let Some(client) = client.filter(|_| !find.query.is_empty()) else { return };
        find.generation += 1;
        find.loading = true;
        find.failed = false;
        let generation = find.generation;
        let mut input = json!({"threadId": find.thread_id, "query": find.query, "skills": []});
        if let Some(start) = &find.start {
            input["start"] = json!({"entryId": start.entry_id, "occurrence": start.occurrence});
        }
        if find.offset != 0 {
            input["offset"] = find.offset.into();
        }
        find._search = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let response = client.request("orchestration.searchThread", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(find) = this.find.as_mut().filter(|f| f.generation == generation) else { return };
                find.loading = false;
                find.offset = 0;
                match response.ok().as_ref().and_then(FindResult::parse) {
                    Some(result) => {
                        find.start = result.matched.clone();
                        find.result = Some(result);
                    }
                    None => find.failed = true,
                }
                this.reveal_find_match(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Next (`1`) or previous (`-1`) match; locally when the navigation window holds it.
    pub(super) fn step_find(&mut self, delta: i64, _: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        let Some(result) = find.result.as_ref().filter(|r| r.total > 0 && r.complete) else { return };
        if !find.loading
            && let Some(next) = result.stepped(delta)
        {
            find.start = next.matched.clone();
            find.result = Some(next);
            self.reveal_find_match(cx);
            cx.notify();
            return;
        }
        if !find.loading {
            find.start = result.matched.clone();
        }
        find.offset += delta;
        self.search_find(Duration::ZERO, cx);
    }

    /// Re-runs an open search shortly after the thread's content changes, from the match shown.
    pub(super) fn refresh_thread_find(&mut self, cx: &mut Context<Self>) {
        let open = self.open_find().is_some_and(|f| !f.query.is_empty());
        let Some(find) = self.find.as_mut().filter(|f| open && f._refresh.is_none()) else { return };
        find._refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                let Some(find) = &mut this.find else { return };
                find._refresh = None;
                if find.loading {
                    return;
                }
                find.start = find.result.as_ref().and_then(|r| r.matched.clone()).or(find.start.take());
                find.offset = 0;
                this.search_find(Duration::ZERO, cx);
            });
        }));
    }

    /// The first message on screen, where a new search starts (`readThreadFindPosition`).
    fn viewport_start(&self, cx: &gpui_kit::App) -> Option<FindMatch> {
        let projection = self.thread.as_ref()?.state.projection.as_ref()?;
        let top = self.scroller.read(cx).logical_scroll_top().item_ix;
        let item_id = self.rows.iter().skip(top).find_map(searchable_row)?;
        Some(FindMatch { entry_id: item_entry(projection, item_id)?, occurrence: 0 })
    }

    /// Opens what hides the active match (turn folds, collapsed messages), scrolls to its row
    /// when it moved to another one, and repaints the highlights.
    fn reveal_find_match(&mut self, cx: &mut Context<Self>) {
        let matched = self.find.as_ref().and_then(|f| f.result.as_ref()?.matched.clone());
        let item_id = matched.as_ref().and_then(|m| {
            let projection = self.thread.as_ref()?.state.projection.as_ref()?;
            Some(entry_item(projection, &m.entry_id)?.id.clone())
        });
        if let Some(item_id) = &item_id {
            let mut opened = false;
            for key in self.entry_cache.folds_hiding(item_id).to_vec() {
                opened |= self.expanded.insert(key);
            }
            // A long user message or plan shows in full (`beforematch`).
            opened |= self.expanded.insert(timeline::entry_key(item_id));
            if opened {
                self.refresh_rows(cx);
            }
            let index = self.rows.iter().position(|row| searchable_row(row).is_some_and(|id| **id == *item_id));
            let moved = self.find.as_ref().is_some_and(|f| f.revealed.as_ref() != Some(item_id));
            if let (Some(index), true) = (index, moved) {
                self.scroller.update(cx, |scroller, cx| scroller.scroll_to_item(index, cx));
                if let Some(find) = &mut self.find {
                    find.revealed = Some(item_id.clone());
                }
            }
        }
        self.paint_all_find(cx);
        // Within a tall message, bring the match's line into view.
        let (Some(item_id), Some(matched)) = (item_id, matched) else { return };
        let query = self.find.as_ref().map(|f| f.query.clone()).unwrap_or_default();
        if let Some(view) = self.markdown_view(&item_id) {
            view.update(cx, |view, cx| {
                let text = view.rendered_text();
                if let Some(range) = occurrences(text.as_str(), &query).into_iter().nth(matched.occurrence) {
                    let _ = view.reveal_range(range, cx);
                }
            });
        }
    }

    fn paint_all_find(&mut self, cx: &mut Context<Self>) {
        for key in self.markdown_keys() {
            self.paint_find(&key, cx);
        }
    }

    /// Paints the find's matches in the markdown view under `key`; a no-op when nothing changed,
    /// so the view's own notifications don't repaint it.
    pub(super) fn paint_find(&mut self, key: &SharedString, cx: &mut Context<Self>) {
        let open = self.open_find().is_some();
        if !open {
            return;
        }
        let Some(view) = self.markdown_view(key) else { return };
        let projection = self.thread.as_ref().and_then(|t| t.state.projection.as_ref());
        let entry = projection.and_then(|p| item_entry(p, key));
        let dark = crate::theme::tokens(cx).dark;
        let Some(find) = self.find.as_mut().filter(|_| open) else { return };
        let matched = find.result.as_ref().and_then(|r| r.matched.as_ref());
        // Painted once the server confirms a match, as the web paints `findQuery`.
        let query = if matched.is_some() { find.query.clone() } else { String::new() };
        let active = matched.filter(|m| Some(&m.entry_id) == entry.as_ref()).map(|m| m.occurrence);
        let text = view.read(cx).rendered_text();
        let painted = (text.clone(), active, query.clone());
        if find.painted.get(key) == Some(&painted) {
            return;
        }
        let (inactive, current): (Hsla, Hsla) = if dark {
            (rgb(0x8c6c1f).into(), rgb(0xeea52b).into())
        } else {
            (rgb(0xffde8b).into(), rgb(0xeea52b).into())
        };
        let highlights: Vec<RangeHighlight> = occurrences(text.as_str(), &query)
            .into_iter()
            .enumerate()
            .map(|(index, range)| RangeHighlight::new(range, if Some(index) == active { current } else { inactive }))
            .collect();
        find.painted.insert(key.clone(), painted);
        view.update(cx, |view, cx| {
            if highlights.is_empty() {
                view.clear_range_highlights(cx);
            } else {
                let _ = view.set_range_highlights(highlights, cx);
            }
        });
    }

    fn clear_find_highlights(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        for key in std::mem::take(&mut find.painted).into_keys() {
            if let Some(view) = self.markdown_view(&key) {
                view.update(cx, gpui_kit::base::TextViewState::clear_range_highlights);
            }
        }
    }

    /// The bar, top right of the chat canvas, while find is open in the thread on screen.
    pub(super) fn render_find_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self.open_find()?;
        let t = crate::theme::tokens(cx);
        let result = find.result.as_ref();
        let total = result.map_or(0, |r| r.total);
        let counting = result.is_some_and(|r| !r.complete);
        let has_query = !find.query.is_empty();
        let label: SharedString = if find.failed {
            "Search failed".into()
        } else if find.loading && result.is_none() && has_query {
            "Searching…".into()
        } else if !has_query {
            "".into()
        } else if counting {
            "1/…".into()
        } else {
            count_label(result.map_or(0, |r| r.active), total).into()
        };
        let no_results = has_query && !find.loading && !find.failed && result.is_some_and(|r| r.total == 0);
        let disabled = total == 0 || (find.loading && result.is_none()) || counting;
        let icon_button = |id: &'static str, icon: IconName| {
            Button::new(id).ghost().xsmall().icon(icon).rounded(px(999.)).text_color(t.muted_foreground)
        };
        let border = t.foreground.opacity(0.1);
        Some(
            // `w-[min(var(--thread-details-panel-width),calc(100%-1.5rem))]`, right-aligned.
            h_flex()
                .id("thread-find-bar")
                .absolute()
                .top(px(12.))
                .left(px(12.))
                .right(px(12.))
                .justify_end()
                .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.close_thread_find(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    h_flex()
                        .occlude()
                        .h(px(36.))
                        .w(px(280.))
                        .min_w_0()
                        .rounded(px(16.))
                        .border_1()
                        .border_color(border)
                        .bg(t.background.blend(t.popover.opacity(t.glass)))
                        .shadow_lg()
                        .pl(px(4.))
                        .pr(px(4.))
                        .child(div().flex_1().min_w_0().child(Input::new(&find.input).appearance(false).small()))
                        .child(
                            h_flex()
                                .flex_none()
                                .gap(px(2.))
                                .child(
                                    div()
                                        .min_w(px(40.))
                                        .text_center()
                                        .text_xs()
                                        .text_color(if no_results { t.error } else { t.muted_foreground })
                                        .child(label),
                                )
                                .children(find.failed.then(|| {
                                    Button::new("thread-find-retry")
                                        .ghost()
                                        .xsmall()
                                        .label("Retry")
                                        .rounded(px(999.))
                                        .on_click(cx.listener(|this, _, _, cx| this.search_find(Duration::ZERO, cx)))
                                }))
                                .child(
                                    icon_button("thread-find-previous", IconName::ChevronUp)
                                        .tooltip("Previous match")
                                        .disabled(disabled)
                                        .on_click(cx.listener(|this, _, window, cx| this.step_find(-1, window, cx))),
                                )
                                .child(
                                    icon_button("thread-find-next", IconName::ChevronDown)
                                        .tooltip("Next match")
                                        .disabled(disabled)
                                        .on_click(cx.listener(|this, _, window, cx| this.step_find(1, window, cx))),
                                )
                                .child(
                                    icon_button("thread-find-close", IconName::X).tooltip("Close find").on_click(
                                        cx.listener(|this, _, window, cx| this.close_thread_find(window, cx)),
                                    ),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// How far the details card moves down for the bar.
    pub(super) fn find_bar_inset(&self) -> f32 {
        if self.open_find().is_some() { RESERVED_HEIGHT } else { 0. }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_wrap_and_counts_read_like_the_web() {
        assert_eq!(step_index(0, 3, -1), 2);
        assert_eq!(step_index(2, 3, 1), 0);
        assert_eq!(step_index(9, 3, 1), 0, "an index past the end clamps first");
        assert_eq!(step_index(0, 0, 1), 0);
        // Server counts near the top of the range used to overflow.
        assert_eq!(step_index(usize::MAX - 1, usize::MAX, 1), 0);
        assert_eq!(step_index(usize::MAX - 2, usize::MAX, i64::MIN), (usize::MAX - 2) - (1usize << 63));
        assert_eq!(step_index(1, 3, i64::MIN), 2);
        let wide = json!({"totalMatches": 5, "navigation": [{"entryId": "a", "startIndex": 1, "count": u64::MAX}]});
        let stepped = FindResult::parse(&wide).and_then(|result| result.stepped(1));
        assert_eq!(stepped.and_then(|result| result.matched).map(|m| m.occurrence), Some(0));
        assert_eq!(count_label(1, 12), "2/12");
        assert_eq!(count_label(0, 0), "0/0");
        assert_eq!(count_label(20, 3), "3/3");
    }

    #[test]
    fn results_step_inside_their_navigation_window() {
        let result = FindResult::parse(&json!({
            "snapshotSequence": 4, "totalMatches": 5, "activeIndex": 1,
            "match": {"entryId": "m1", "runId": null, "occurrence": 1},
            "navigation": [
                {"entryId": "m1", "runId": null, "startIndex": 0, "count": 2},
                {"entryId": "p2", "runId": "r", "startIndex": 2, "count": 1},
            ],
        }))
        .unwrap();
        assert!(result.complete);
        let next = result.stepped(1).unwrap();
        assert_eq!((next.active, next.matched.clone()), (2, Some(FindMatch { entry_id: "p2".into(), occurrence: 0 })));
        assert_eq!(result.stepped(-1).unwrap().matched.unwrap().occurrence, 0);
        assert!(next.stepped(1).is_none(), "match 4 is outside the window: ask the server");
        assert!(
            !FindResult::parse(&json!({"totalMatches": 0, "activeIndex": 0, "match": null, "complete": false}))
                .unwrap()
                .complete
        );
    }

    #[test]
    fn occurrences_fold_case_and_apostrophes_without_overlap() {
        assert_eq!(occurrences("Find the find, FIND", "find"), [0..4, 9..13, 15..19]);
        assert_eq!(occurrences("aaaa", "aa"), [0..2, 2..4]);
        assert_eq!(occurrences("it’s Café", "it's"), [0..6]);
        assert_eq!(occurrences("ΟΔΟΣ", "οδος"), [0..8]);
        assert!(occurrences("text", "").is_empty());
    }

    #[test]
    fn message_hits_open_find_on_the_phrase_or_its_longest_word() {
        assert_eq!(query_for_message("run `vp  run dev` here", "vp run dev"), "vp  run dev");
        assert_eq!(query_for_message("Fixed the Sidebar layout", "fix sidebar"), "Sidebar");
        assert_eq!(query_for_message("Cafeteria and Café", "cafe"), "Café", "a whole word first");
        assert_eq!(query_for_message("nothing here", "sidebar"), "sidebar");
    }
}
