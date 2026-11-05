//! Sidebar search: title and branch matches first, then message hits from
//! `orchestration.searchThreads` (`Sidebar.tsx` search, `SidebarSearchResultRow`).

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use super::{now_ms, project_icon, text_sm, text_xs};
use crate::app::Workspace;
use crate::app::palette::MessageSearch;
use crate::inbox;
use crate::model::ThreadShell;
use crate::search::{self, Field, Weighted};
use crate::theme::Tokens;

/// One search result: a title match, or a message hit with its snippet.
pub(super) struct SearchResult<'a> {
    pub(super) thread: &'a ThreadShell,
    snippet: Option<(bool, Vec<(String, bool)>)>,
}

impl Workspace {
    pub(super) fn sidebar_messages(this: &mut Workspace) -> Option<&mut MessageSearch> {
        Some(&mut this.sidebar.messages)
    }

    /// After a keystroke: no row is highlighted and the message search restarts.
    pub(super) fn on_search_changed(&mut self, cx: &mut Context<Self>) {
        self.sidebar.search_highlight = None;
        let query = self.sidebar.search.read(cx).value().to_string();
        self.search_messages(query, Self::sidebar_messages, cx);
        self.notify_sidebar(cx);
    }

    /// `searchSidebarThreads` over the scoped threads, then the message-only hits.
    pub(super) fn search_results(&self, query: &str, now: i64) -> Vec<SearchResult<'_>> {
        let tokens = search::fold_query(query);
        let visible: Vec<&ThreadShell> = self
            .shell
            .threads
            .iter()
            .filter(|t| t.is_listed() && t.archived_at.is_none() && t.deleted_at.is_none())
            .filter(|t| self.sidebar.scope.is_empty() || self.sidebar.scope.contains(&t.project_id))
            .collect();
        let score = |typos: bool| {
            let mut found: Vec<(&ThreadShell, f32, bool)> = visible
                .iter()
                .filter_map(|thread| {
                    let title = Field::new(&thread.title);
                    let branch = thread.branch.as_deref().unwrap_or_default();
                    let branch = Field::new(branch.trim_start_matches("t3code/").trim_start_matches("t3/"));
                    let mut title_field = Weighted::new(&title, 1.);
                    title_field.mid_word = true;
                    let found = search::match_fields(&tokens, &[title_field, Weighted::new(&branch, 0.6)], typos)?;
                    let used = inbox::parse_ms(
                        thread
                            .latest_user_authored_message_at
                            .clone()
                            .flatten()
                            .as_deref()
                            .or(thread.latest_user_message_at.as_deref())
                            .unwrap_or(&thread.updated_at),
                    )
                    .unwrap_or(0);
                    Some((*thread, found.quality * search::usage_boost(now - used), found.strong))
                })
                .collect();
            found.sort_by(|a, b| {
                let class = |q: f32| search::class(q).min(2);
                class(b.1).cmp(&class(a.1)).then(b.1.total_cmp(&a.1))
            });
            found
        };
        let mut found = score(false);
        if !found.iter().any(|(_, _, strong)| *strong) {
            found = score(true);
        }
        let mut results: Vec<SearchResult> =
            found.into_iter().map(|(thread, ..)| SearchResult { thread, snippet: None }).collect();
        for hit in &self.sidebar.messages.matches {
            if results.iter().any(|r| r.thread.id == hit.thread_id) {
                continue;
            }
            let Some(thread) = visible.iter().find(|t| t.id == hit.thread_id) else { continue };
            let ranges = search::token_ranges(&hit.snippet, query);
            results
                .push(SearchResult { thread, snippet: Some((hit.user, search::split_ranges(&hit.snippet, &ranges))) });
        }
        results
    }

    /// Up/Down wrap through the results, Enter opens one, Escape clears.
    pub(super) fn search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_query(cx);
        if query.is_empty() {
            return;
        }
        let ids: Vec<String> = self.search_results(&query, now_ms()).iter().map(|r| r.thread.id.clone()).collect();
        let at = self.sidebar.search_highlight.as_ref().and_then(|id| ids.iter().position(|i| i == id));
        match event.keystroke.key.as_str() {
            "escape" => self.clear_search(window, cx),
            "down" | "up" if !ids.is_empty() => {
                let last = ids.len() - 1;
                let next = match (event.keystroke.key.as_str(), at) {
                    ("down", None) => 0,
                    ("down", Some(i)) => {
                        if i >= last {
                            0
                        } else {
                            i + 1
                        }
                    }
                    (_, None) | (_, Some(0)) => last,
                    (_, Some(i)) => i - 1,
                };
                self.sidebar.search_highlight = Some(ids[next].clone());
                self.notify_sidebar(cx);
            }
            "enter" => {
                if let Some(id) = ids.get(at.unwrap_or(0)).cloned() {
                    self.open_search_result(id, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.search.update(cx, |search, cx| search.set_value("", window, cx));
        self.sidebar.search_highlight = None;
    }

    fn open_search_result(&mut self, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.clear_search(window, cx);
        self.select_thread(thread_id, window, cx);
    }

    pub(super) fn render_search_results(
        &self,
        query: &str,
        now: i64,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sb = tokens.sidebar;
        let results = self.search_results(query, now);
        if results.is_empty() {
            let text = if self.sidebar.messages.pending { "Searching thread messages…" } else { "No threads found" };
            return text_xs(div())
                .px(px(8.))
                .py(px(24.))
                .text_center()
                .text_color(sb.muted_foreground)
                .child(text)
                .into_any_element();
        }
        let route = self.route_thread();
        let highlight = self.sidebar.search_highlight.as_deref().or(results.first().map(|r| r.thread.id.as_str()));
        let highlight = highlight.filter(|_| self.sidebar.search_highlight.is_some());
        v_flex()
            .gap(px(1.))
            .children(results.into_iter().map(|result| {
                let thread = result.thread;
                let active = highlight == Some(thread.id.as_str()) || route == Some(thread.id.as_str());
                let settled = thread.settled_override.as_deref() == Some("settled");
                let muted_icon = settled && !active;
                let hovered = self.sidebar.hovered.as_deref() == Some(thread.id.as_str());
                let project = self.shell.projects.iter().find(|p| p.id == thread.project_id);
                let time = inbox::parse_ms(thread.latest_user_message_at.as_deref().unwrap_or(&thread.updated_at))
                    .map(|at| inbox::format_relative(now - at));
                let title_color = if active {
                    sb.foreground
                } else if settled {
                    sb.secondary_label.opacity(0.7)
                } else {
                    sb.foreground
                };
                let (open_id, hover_id) = (thread.id.clone(), thread.id.clone());
                let snippet = result.snippet.map(|(user, parts)| {
                    text_xs(h_flex())
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_color(sb.muted_foreground.opacity(0.85))
                        .child(
                            div()
                                .flex_none()
                                .mr(px(4.))
                                .text_color(if user { tokens.info_foreground } else { tokens.success_foreground })
                                .child(if user { "You:" } else { "Agent:" }),
                        )
                        .children(parts.into_iter().map(|(text, bold)| {
                            div()
                                .flex_none()
                                .when(bold, |el| el.font_weight(FontWeight::BOLD).text_color(sb.foreground))
                                .child(text)
                        }))
                });
                text_sm(h_flex())
                    .id(SharedString::from(format!("search-{}", thread.id)))
                    .min_h(px(36.))
                    .px(px(10.))
                    .py(px(4.))
                    .gap(px(10.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .map(|el| if active { el.bg(sb.row_active) } else { el.hover(|s| s.bg(sb.row_hover)) })
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered && this.sidebar.search_highlight.as_ref() != Some(&hover_id) {
                            this.sidebar.search_highlight = Some(hover_id.clone());
                            this.notify_sidebar(cx);
                        }
                    }))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_search_result(open_id.clone(), window, cx)),
                    )
                    .children(project.map(|p| {
                        div().flex_none().when(muted_icon && !hovered, |el| el.opacity(0.4)).child(project_icon(
                            p,
                            tokens,
                            muted_icon && !hovered,
                            16.,
                            self.favicon(p),
                        ))
                    }))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap(px(10.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(if settled { FontWeight::NORMAL } else { FontWeight::MEDIUM })
                                            .text_color(title_color)
                                            .when(thread.title_regeneration.is_some(), |el| el.opacity(0.55))
                                            .child(thread.title.clone()),
                                    )
                                    .child(
                                        text_xs(div())
                                            .w(px(32.))
                                            .flex_none()
                                            .text_right()
                                            .text_color(tokens.muted_foreground.opacity(0.55))
                                            .children(time),
                                    ),
                            )
                            .children(snippet),
                    )
                    .into_any_element()
            }))
            .into_any_element()
    }
}
