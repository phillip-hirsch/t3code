//! Go to file (`ProjectFilePicker.tsx`) and Search project contents (`ProjectContentSearchDialog.tsx`):
//! the command palette's two child searches. Esc goes back to the palette with its query restored.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ListAlignment, ListState, ParentElement as _, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, UniformListScrollHandle, Window, div, list,
    prelude::FluentBuilder as _, px, uniform_list,
};
use serde_json::json;

use super::ProjectTarget;
use super::logic::{self, ContentMatch, PickerMatch};
use crate::app::{Workspace, palette};
use crate::theme::Tokens;

/// `PROJECT_PATH_SEARCH_DEBOUNCE_MS` and `PROJECT_CONTENT_SEARCH_DEBOUNCE_MS`.
const DEBOUNCE: Duration = Duration::from_millis(120);
/// `PROJECT_CONTENT_SEARCH_LIMIT`.
const CONTENT_LIMIT: usize = 500;
const PICKER_ROW: f32 = 48.;
/// `max-h-[min(34rem,76vh)]` for the tall list.
const PICKER_LIST_MAX: f32 = 544.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::app) enum OverlayMode {
    Files,
    Content,
}

/// A row of the content results: a file header, a hit, or the gap closing a file's section.
#[derive(Clone, Copy)]
enum ContentRow {
    Header(usize),
    Match(usize),
    Gap,
}

#[derive(Default)]
struct Search {
    /// The query the results answer.
    searched: String,
    pending: bool,
    error: Option<SharedString>,
    generation: u64,
    _task: Option<Task<()>>,
}

pub(in crate::app) struct FileOverlay {
    pub(in crate::app) mode: OverlayMode,
    input: Entity<InputState>,
    /// The palette's query when it opened this; Esc restores it.
    parent_query: Option<String>,
    target: Option<ProjectTarget>,
    search: Search,
    highlight: usize,
    picks: Rc<Vec<PickerMatch>>,
    picker_scroll: UniformListScrollHandle,
    case_sensitive: bool,
    whole_word: bool,
    use_regex: bool,
    matches: Rc<Vec<ContentMatch>>,
    groups: Rc<Vec<(String, Vec<usize>)>>,
    rows: Rc<Vec<ContentRow>>,
    truncated: bool,
    invalid_regex: bool,
    content_list: ListState,
    _subscription: Subscription,
}

impl FileOverlay {
    fn query(&self, cx: &gpui_kit::App) -> String {
        self.input.read(cx).value().to_string()
    }

    fn set_matches(&mut self, matches: Vec<ContentMatch>) {
        let groups = logic::group_matches(&matches);
        let mut rows = Vec::new();
        for (index, (_, hits)) in groups.iter().enumerate() {
            rows.push(ContentRow::Header(index));
            rows.extend(hits.iter().map(|&hit| ContentRow::Match(hit)));
            rows.push(ContentRow::Gap);
        }
        self.content_list.reset(rows.len());
        self.matches = Rc::new(matches);
        self.groups = Rc::new(groups);
        self.rows = Rc::new(rows);
        self.highlight = 0;
    }

    /// The list row showing match `index`.
    fn row_of_match(&self, index: usize) -> Option<usize> {
        self.rows.iter().position(|row| matches!(row, ContentRow::Match(m) if *m == index))
    }
}

impl Workspace {
    /// `filePicker.toggle` / `projectSearch.toggle`: opens the search, or closes it when it already shows.
    pub(in crate::app) fn toggle_file_overlay(
        &mut self,
        mode: OverlayMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.files.overlay.as_ref().is_some_and(|o| o.mode == mode) {
            self.close_file_overlay(window, cx);
        } else {
            self.open_file_overlay(mode, None, window, cx);
        }
    }

    /// Opens a child search, closing the palette; `parent_query` is the palette's, for Back.
    pub(in crate::app) fn open_file_overlay(
        &mut self,
        mode: OverlayMode,
        parent_query: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette = None;
        let target = self.project_target();
        let placeholder: SharedString = match (&target, mode) {
            (_, OverlayMode::Files) => "Search files…".into(),
            (Some(target), OverlayMode::Content) => format!("Search in {}", target.project_name).into(),
            (None, OverlayMode::Content) => "Search project contents…".into(),
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.run_file_search(cx);
            }
        });
        if target.is_some() {
            input.update(cx, |input, cx| input.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
        self.files.overlay = Some(FileOverlay {
            mode,
            input,
            parent_query,
            target,
            search: Search::default(),
            highlight: 0,
            picks: Rc::default(),
            picker_scroll: UniformListScrollHandle::new(),
            case_sensitive: false,
            whole_word: false,
            use_regex: false,
            matches: Rc::default(),
            groups: Rc::default(),
            rows: Rc::default(),
            truncated: false,
            invalid_regex: false,
            content_list: ListState::new(0, ListAlignment::Top, px(200.)),
            _subscription: subscription,
        });
        self.run_file_search(cx);
        cx.notify();
    }

    pub(in crate::app) fn close_file_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.files.overlay.take().is_some() {
            self.focus_composer(window, cx);
            cx.notify();
        }
    }

    /// Esc: back to the command palette (`BackToCommand`), with the query it had.
    fn file_overlay_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(overlay) = self.files.overlay.take() else { return };
        self.open_palette(palette::View::Root, window, cx);
        if let Some(query) = overlay.parent_query.filter(|q| !q.is_empty()) {
            self.restore_palette_query(&query, window, cx);
        }
        cx.notify();
    }

    /// Starts the debounced search for the current query and options.
    fn run_file_search(&mut self, cx: &mut Context<Self>) {
        let client = self.client();
        let Some(overlay) = &mut self.files.overlay else { return };
        let Some(target) = overlay.target.clone() else { return };
        let raw = overlay.query(cx);
        overlay.search.generation += 1;
        let generation = overlay.search.generation;
        let (method, payload) = match overlay.mode {
            OverlayMode::Files => {
                let query: String = raw.trim().to_owned();
                overlay.picks = Rc::default();
                overlay.highlight = 0;
                let payload = json!({"cwd": target.cwd, "query": query, "limit": logic::PICKER_LIMIT, "kind": "file"});
                ("projects.searchEntries", payload)
            }
            OverlayMode::Content => {
                if raw.trim().is_empty() {
                    overlay.search = Search { generation, ..Search::default() };
                    overlay.set_matches(Vec::new());
                    overlay.truncated = false;
                    overlay.invalid_regex = false;
                    cx.notify();
                    return;
                }
                let payload = json!({
                    "cwd": target.cwd,
                    "query": raw.chars().take(256).collect::<String>(),
                    "limit": CONTENT_LIMIT,
                    "caseSensitive": overlay.case_sensitive,
                    "wholeWord": overlay.whole_word,
                    "useRegex": overlay.use_regex,
                });
                ("projects.searchContents", payload)
            }
        };
        overlay.search.pending = true;
        let Some(client) = client else {
            overlay.search.pending = false;
            overlay.search.error = Some("Not connected.".into());
            cx.notify();
            return;
        };
        overlay.search._task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;
            let result = client.request(method, payload).await;
            let _ = this.update(cx, |this, cx| {
                let Some(overlay) = this.files.overlay.as_mut().filter(|o| o.search.generation == generation) else {
                    return;
                };
                overlay.search.pending = false;
                overlay.search.searched = raw.clone();
                match result {
                    Ok(value) => {
                        overlay.search.error = None;
                        match overlay.mode {
                            OverlayMode::Files => {
                                overlay.picks = Rc::new(logic::picker_matches(&logic::entries_from_json(&value), &raw));
                                overlay.highlight = 0;
                                overlay.picker_scroll.scroll_to_item(0, ScrollStrategy::Top);
                            }
                            OverlayMode::Content => {
                                let matches = value["matches"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(ContentMatch::from_json)
                                    .collect();
                                overlay.set_matches(matches);
                                overlay.truncated = value["truncated"].as_bool().unwrap_or(false);
                                overlay.invalid_regex = overlay.use_regex && value.get("regexFallbackError").is_some();
                            }
                        }
                    }
                    Err(error) => {
                        overlay.search.error = Some(logic::error_message(&error).into());
                        if overlay.mode == OverlayMode::Content {
                            overlay.set_matches(Vec::new());
                        }
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn toggle_content_option(
        &mut self,
        option: fn(&mut FileOverlay) -> &mut bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(overlay) = &mut self.files.overlay else { return };
        let flag = option(overlay);
        *flag = !*flag;
        let input = overlay.input.clone();
        input.update(cx, |input, cx| input.focus(window, cx));
        self.run_file_search(cx);
    }

    /// Opens the highlighted row: a file, or a hit at its line.
    fn open_overlay_selection(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(overlay) = &self.files.overlay else { return };
        let opened = match overlay.mode {
            OverlayMode::Files => overlay.picks.get(index).map(|pick| (pick.path.clone(), None)),
            OverlayMode::Content => {
                let can_open = !overlay.search.pending && overlay.search.error.is_none();
                overlay.matches.get(index).filter(|_| can_open).map(|m| (m.path.clone(), Some(m.line)))
            }
        };
        let Some((path, line)) = opened else { return };
        self.files.overlay = None;
        self.open_file(&path, line, window, cx);
        cx.notify();
    }

    fn file_overlay_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(overlay) = &mut self.files.overlay else { return };
        let count = match overlay.mode {
            OverlayMode::Files => overlay.picks.len(),
            OverlayMode::Content => overlay.matches.len(),
        };
        match event.keystroke.key.as_str() {
            "escape" => self.file_overlay_back(window, cx),
            "up" | "down" if count > 0 => {
                overlay.highlight = if event.keystroke.key == "down" {
                    (overlay.highlight + 1) % count
                } else {
                    (overlay.highlight + count - 1) % count
                };
                match overlay.mode {
                    OverlayMode::Files => {
                        overlay.picker_scroll.scroll_to_item(overlay.highlight, ScrollStrategy::Nearest)
                    }
                    OverlayMode::Content => {
                        if let Some(row) = overlay.row_of_match(overlay.highlight) {
                            overlay.content_list.scroll_to_reveal_item(row);
                        }
                    }
                }
                cx.notify();
            }
            "enter" => {
                let index = overlay.highlight;
                self.open_overlay_selection(index, window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(in crate::app) fn render_file_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let overlay = self.files.overlay.as_ref()?;
        let tokens = *crate::theme::tokens(cx);
        let (fg, muted) = (tokens.foreground, tokens.muted_foreground);
        let mode = overlay.mode;
        let has_target = overlay.target.is_some();
        let toggles = (mode == OverlayMode::Content && has_target).then(|| self.render_content_toggles(&tokens, cx));
        let input_row =
            h_flex().flex_none().h(px(46.)).px(px(8.)).py(px(6.)).child(
                h_flex()
                    .size_full()
                    .relative()
                    .child(
                        div()
                            .absolute()
                            .left(px(11.))
                            .child(Icon::new(IconName::Search).size(px(16.)).text_color(muted.opacity(0.8))),
                    )
                    .child(
                        div().flex_1().pl(px(32.)).when(toggles.is_some(), |el| el.pr(px(120.))).child(
                            Input::new(&overlay.input).appearance(false).text_sm().h(px(34.)).disabled(!has_target),
                        ),
                    )
                    .children(toggles),
            );
        let body = match (mode, has_target) {
            (_, false) => div()
                .py(px(40.))
                .text_center()
                .text_sm()
                .text_color(muted)
                .child("Open a project to search its files.")
                .into_any_element(),
            (OverlayMode::Files, true) => self.render_picker_results(&tokens, window, cx),
            (OverlayMode::Content, true) => self.render_content_results(&tokens, cx),
        };
        let kbd = |label: &'static str| {
            div()
                .h(px(20.))
                .min_w(px(20.))
                .px(px(4.))
                .rounded(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .bg(fg.opacity(0.08))
                .text_color(fg)
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .child(label)
        };
        let hint = |keys: &[&'static str], label: &'static str| {
            h_flex().gap(px(4.)).children(keys.iter().map(|k| kbd(k))).child(label)
        };
        let footer = h_flex()
            .flex_none()
            .gap(px(12.))
            .px(px(16.))
            .py(px(10.))
            .border_t_1()
            .border_color(fg.opacity(0.07))
            .bg(fg.opacity(0.025))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(muted)
            .child(hint(&["↑", "↓"], "Navigate"))
            .child(hint(&["Enter"], "Open file"))
            .child(hint(&["Esc"], "Back"));
        let height = window.viewport_size().height;
        let panel = v_flex()
            .id("file-overlay")
            .w_full()
            .max_w(px(576.))
            .overflow_hidden()
            .rounded(px(16.))
            .border_1()
            .border_color(fg.opacity(0.08))
            .bg(tokens.popover)
            .shadow_2xl()
            .map(|el| match mode {
                // `h-105`: the content search keeps its height as results come and go.
                OverlayMode::Content => el.h(px(420.)),
                OverlayMode::Files => el,
            })
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .capture_key_down(
                cx.listener(|this, event: &KeyDownEvent, window, cx| this.file_overlay_key(event, window, cx)),
            )
            .child(input_row)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .child(footer);
        Some(
            div()
                .id("file-overlay-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .bg(gpui_kit::black().opacity(0.64))
                .flex()
                .flex_col()
                .items_center()
                .px(px(16.))
                .pt(height * 0.1)
                .on_mouse_down(
                    gpui_kit::MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_file_overlay(window, cx)),
                )
                .child(panel)
                .into_any_element(),
        )
    }

    fn render_picker_results(&self, tokens: &Tokens, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(overlay) = &self.files.overlay else { return div().into_any_element() };
        let picks = overlay.picks.clone();
        let muted = tokens.muted_foreground;
        if picks.is_empty() {
            let searching = !overlay.query(cx).trim().is_empty();
            let text: SharedString = match (&overlay.search.error, overlay.search.pending, searching) {
                (Some(error), _, _) => error.clone(),
                (None, true, true) => "Searching workspace files…".into(),
                (None, true, false) => "Indexing workspace files…".into(),
                (None, false, true) => "No matching files.".into(),
                (None, false, false) => "No files found.".into(),
            };
            return div().py(px(40.)).text_center().text_sm().text_color(muted).child(text).into_any_element();
        }
        let label = overlay.target.as_ref().map(|t| t.project_name.clone()).unwrap_or_default();
        let highlighted = !overlay.search.searched.trim().is_empty();
        let max = PICKER_LIST_MAX.min(f32::from(window.viewport_size().height) * 0.76);
        let list_height = (picks.len() as f32 * PICKER_ROW).min(max - 44.);
        v_flex()
            .p(px(8.))
            .child(
                div()
                    .px(px(8.))
                    .py(px(6.))
                    .text_xs()
                    .line_height(px(16.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(muted)
                    .child(label),
            )
            .child(
                uniform_list(
                    "file-picker-results",
                    picks.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let tokens = *crate::theme::tokens(cx);
                        let current = this.files.overlay.as_ref().map_or(0, |o| o.highlight);
                        range
                            .map(|index| picker_row(&picks[index], index, index == current, highlighted, &tokens, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&overlay.picker_scroll)
                .h(px(list_height)),
            )
            .into_any_element()
    }

    fn render_content_toggles(&self, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(overlay) = &self.files.overlay else { return div().into_any_element() };
        let toggle = |id: &'static str,
                      label: &'static str,
                      active: bool,
                      glyph: AnyElement,
                      option: fn(&mut FileOverlay) -> &mut bool| {
            div()
                .id(id)
                .h(px(24.))
                .min_w(px(28.))
                .px(px(6.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_xs()
                .font_family(tokens.fonts.mono)
                .map(|el| {
                    if active {
                        el.bg(tokens.background).text_color(tokens.foreground).shadow_xs()
                    } else {
                        el.text_color(tokens.muted_foreground).hover(|s| s.text_color(tokens.foreground))
                    }
                })
                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, window, cx| this.toggle_content_option(option, window, cx)))
                .child(glyph)
        };
        h_flex()
            .absolute()
            .right(px(10.))
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(8.))
            .border_1()
            .border_color(tokens.border)
            .bg(tokens.muted.opacity(0.3))
            .child(toggle(
                "content-case",
                "Match case",
                overlay.case_sensitive,
                div().child("Aa").into_any_element(),
                |o| &mut o.case_sensitive,
            ))
            .child(toggle(
                "content-word",
                "Match whole word",
                overlay.whole_word,
                div().underline().text_decoration_2().child("ab").into_any_element(),
                |o| &mut o.whole_word,
            ))
            .child(toggle(
                "content-regex",
                "Use regular expression",
                overlay.use_regex,
                div().child(".*").into_any_element(),
                |o| &mut o.use_regex,
            ))
            .into_any_element()
    }

    fn render_content_results(&self, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let Some(overlay) = &self.files.overlay else { return div().into_any_element() };
        let muted = tokens.muted_foreground;
        let has_query = !overlay.query(cx).trim().is_empty();
        let search = &overlay.search;
        let status: Option<AnyElement> =
            (has_query || search.pending || search.error.is_some() || overlay.invalid_regex).then(|| {
                let content: AnyElement = if search.pending {
                    h_flex().gap(px(8.)).child(Spinner::new().xsmall()).child("Searching…").into_any_element()
                } else if let Some(error) = &search.error {
                    div().text_color(tokens.error).child(error.clone()).into_any_element()
                } else if overlay.invalid_regex {
                    div().text_color(tokens.error).child("Invalid regular expression").into_any_element()
                } else {
                    div().child(logic::results_summary(&overlay.matches, overlay.truncated)).into_any_element()
                };
                h_flex()
                    .flex_none()
                    .h(px(36.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(tokens.border)
                    .text_xs()
                    .text_color(muted)
                    .child(content)
                    .into_any_element()
            });
        let body = if overlay.matches.is_empty() {
            let text = if has_query && !search.pending && search.error.is_none() {
                "No results found."
            } else {
                "Type to search across your project."
            };
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .px(px(24.))
                .text_center()
                .text_sm()
                .text_color(muted)
                .child(text)
                .into_any_element()
        } else {
            let (rows, groups, matches) = (overlay.rows.clone(), overlay.groups.clone(), overlay.matches.clone());
            let can_open = !search.pending && search.error.is_none();
            let view = cx.entity().downgrade();
            list(overlay.content_list.clone(), move |index, _, cx| {
                let Some(view) = view.upgrade() else { return div().into_any_element() };
                let tokens = *crate::theme::tokens(cx);
                let row = rows[index];
                match row {
                    ContentRow::Header(group) => {
                        let (path, hits) = &groups[group];
                        content_header(path, hits.len(), &tokens).into_any_element()
                    }
                    ContentRow::Gap => div().h(px(8.)).into_any_element(),
                    ContentRow::Match(hit) => {
                        let selected = view.read(cx).files.overlay.as_ref().is_some_and(|o| o.highlight == hit);
                        content_row(&matches[hit], hit, selected, can_open, &tokens, view, cx)
                    }
                }
            })
            .flex_1()
            .py(px(8.))
            .into_any_element()
        };
        v_flex().flex_1().min_h_0().children(status).child(body).into_any_element()
    }
}

/// A Go to file row: the name and path, matched characters in bold.
fn picker_row(
    pick: &PickerMatch,
    index: usize,
    selected: bool,
    highlighted: bool,
    tokens: &Tokens,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let name = logic::file_name(&pick.path).to_owned();
    let fuzzy = |value: &str, indices: &[usize], base: gpui_kit::Hsla| -> AnyElement {
        if !highlighted {
            return div().truncate().text_color(base).child(value.to_owned()).into_any_element();
        }
        h_flex()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_color(tokens.muted_foreground)
            .children(logic::highlight_runs(value, indices).into_iter().map(|(text, bold)| {
                div()
                    .flex_none()
                    .when(bold, |el| el.font_weight(FontWeight::SEMIBOLD).text_color(tokens.foreground))
                    .child(text)
            }))
            .into_any_element()
    };
    h_flex()
        .id(ElementId::Name(format!("file-pick:{}", pick.path).into()))
        .h(px(PICKER_ROW))
        .w_full()
        .items_start()
        .gap(px(8.))
        .px(px(8.))
        .py(px(6.))
        .rounded(px(4.))
        .cursor_pointer()
        .when(selected, |el| el.bg(tokens.foreground.opacity(0.04)))
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            if let Some(overlay) = this.files.overlay.as_mut().filter(|o| *hovered && o.highlight != index) {
                overlay.highlight = index;
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |this, _, window, cx| this.open_overlay_selection(index, window, cx)))
        .child(
            div()
                .flex_none()
                .h(px(20.))
                .pt(px(2.))
                .child(Icon::new(IconName::File).size(px(16.)).text_color(tokens.muted_foreground)),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().h(px(20.)).min_w_0().overflow_hidden().text_sm().line_height(px(20.)).child(fuzzy(
                    &name,
                    &pick.name_indices,
                    tokens.foreground,
                )))
                .child(div().min_w_0().overflow_hidden().text_xs().line_height(px(16.)).child(fuzzy(
                    &pick.path,
                    &pick.path_indices,
                    tokens.muted_foreground.opacity(0.7),
                ))),
        )
        .into_any_element()
}

/// A file's header in the content results: name, folder and hit count.
fn content_header(path: &str, count: usize, tokens: &Tokens) -> impl IntoElement {
    let (name, directory) = logic::split_path(path);
    h_flex()
        .h(px(32.))
        .gap(px(8.))
        .px(px(12.))
        .text_xs()
        .child(Icon::new(IconName::File).size(px(14.)).text_color(tokens.muted_foreground))
        .child(div().flex_none().font_weight(FontWeight::MEDIUM).text_color(tokens.foreground).child(name.to_owned()))
        .when(!directory.is_empty(), |el| {
            el.child(div().min_w_0().truncate().text_color(tokens.muted_foreground).child(directory.to_owned()))
        })
        .child(
            div()
                .ml_auto()
                .flex_none()
                .px(px(6.))
                .py(px(2.))
                .rounded_full()
                .bg(tokens.muted)
                .text_size(gpui_kit::rems(0.625))
                .text_color(tokens.muted_foreground)
                .child(count.to_string()),
        )
}

/// A hit: its line number and the line, matches marked.
fn content_row(
    hit: &ContentMatch,
    index: usize,
    selected: bool,
    can_open: bool,
    tokens: &Tokens,
    view: Entity<Workspace>,
    cx: &mut gpui_kit::App,
) -> AnyElement {
    let _ = cx;
    let hover_view = view.clone();
    h_flex()
        .id(ElementId::Name(format!("content-hit:{}:{}:{index}", hit.path, hit.line).into()))
        .h(px(28.))
        .w_full()
        .min_w_0()
        .gap(px(12.))
        .px(px(12.))
        .text_xs()
        .font_family(tokens.fonts.mono)
        .when(can_open, |el| el.cursor_pointer().hover(|s| s.bg(tokens.accent.opacity(0.6))))
        .when(selected, |el| el.bg(tokens.accent).text_color(tokens.foreground))
        .on_hover(move |hovered, _, cx| {
            if *hovered {
                hover_view.update(cx, |this, cx| {
                    if let Some(overlay) = this.files.overlay.as_mut().filter(|o| o.highlight != index) {
                        overlay.highlight = index;
                        cx.notify();
                    }
                });
            }
        })
        .on_click(move |_, window, cx| {
            view.update(cx, |this, cx| this.open_overlay_selection(index, window, cx));
        })
        .child(
            div()
                .w(px(40.))
                .flex_none()
                .text_right()
                .text_color(tokens.muted_foreground.opacity(0.7))
                .child(hit.line.to_string()),
        )
        .child(h_flex().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().children(
            logic::match_runs(&hit.text, &hit.ranges).into_iter().map(|(text, matched)| {
                div().flex_none().when(matched, |el| el.rounded(px(2.)).bg(tokens.primary.opacity(0.25))).child(text)
            }),
        ))
        .into_any_element()
}
