//! Sidebar search: title, pull request and branch matches first, then message hits from
//! `orchestration.searchThreads` (`Sidebar.tsx` search, `SidebarSearchResultRow`).

use std::cell::OnceCell;
use std::rc::Rc;

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
};

use super::{now_ms, project_icon, row_id, text_sm, text_xs};
use crate::app::Workspace;
use crate::app::palette::{MessageMatch, MessageSearch, SearchScope};
use crate::inbox;
use crate::model::{ScopedId, ThreadShell};
use crate::search::{self, ThreadSubject};
use crate::theme::Tokens;
use crate::thread_pull_requests::SearchPullRequest;

/// The title and branch matches for a query, as indexes into `shell.threads`, ranked once
/// per query, shell change, scope and minute instead of on every render, key and hover.
pub(super) struct Ranking {
    key: RankingKey,
    threads: Rc<[usize]>,
}

#[derive(PartialEq)]
struct RankingKey {
    query: String,
    revision: u64,
    scope: Vec<String>,
    /// Recency boosts the score.
    minute: i64,
}

/// One search result: a title match, or a message hit with its snippet.
pub(super) struct SearchResult<'a> {
    pub(super) thread: &'a ThreadShell,
    snippet: Option<(bool, Vec<(String, bool)>)>,
}

impl Workspace {
    #[expect(clippy::unnecessary_wraps, reason = "a `search_messages` slot, which can be missing elsewhere")]
    pub(super) fn sidebar_messages(this: &mut Workspace) -> Option<&mut MessageSearch> {
        Some(&mut this.sidebar.messages)
    }

    /// After a keystroke: no row is highlighted and the message search restarts.
    pub(super) fn on_search_changed(&mut self, cx: &mut Context<Self>) {
        self.sidebar.search_highlight = None;
        self.refresh_sidebar_search(cx);
        self.notify_sidebar(cx);
    }

    /// Searches the server for the box's query in the filter's projects, unless that search
    /// already ran.
    pub(super) fn refresh_sidebar_search(&mut self, cx: &mut Context<Self>) {
        let query = self.sidebar.search.read(cx).value().to_string();
        let projects = self.scoped_projects().map(|scoped| super::older_settled::search_projects(&scoped));
        self.search_messages(query, SearchScope { projects, labels: None }, Self::sidebar_messages, cx);
    }

    /// `searchSidebarThreads` over the scoped threads, then the message-only hits.
    pub(super) fn search_results(&self, query: &str, now: i64) -> Vec<SearchResult<'_>> {
        // Built only when a cache miss or a message hit needs it.
        let scope = OnceCell::new();
        let visible = |t: &ThreadShell| {
            t.is_listed()
                && t.archived_at.is_none()
                && t.deleted_at.is_none()
                && scope
                    .get_or_init(|| self.scoped_projects())
                    .as_ref()
                    .is_none_or(|projects| projects.contains(&ScopedId::thread_project(t)))
        };
        let threads = self.ranked_search(query, now, &visible);
        let mut results: Vec<SearchResult> =
            threads.iter().map(|&at| SearchResult { thread: &self.shell.threads[at], snippet: None }).collect();
        for hit in &self.sidebar.messages.matches {
            if results.iter().any(|r| is_hit_thread(hit, r.thread)) {
                continue;
            }
            let Some(thread) = self.shell.threads.iter().find(|t| is_hit_thread(hit, t) && visible(t)) else {
                continue;
            };
            let ranges = search::token_ranges(&hit.snippet, query);
            results
                .push(SearchResult { thread, snippet: Some((hit.user, search::split_ranges(&hit.snippet, &ranges))) });
        }
        results
    }

    /// `searchSidebarThreads`' ranking of the visible threads, cached in [`Ranking`].
    fn ranked_search(&self, query: &str, now: i64, visible: &dyn Fn(&ThreadShell) -> bool) -> Rc<[usize]> {
        let key = RankingKey {
            query: query.to_owned(),
            revision: self.shell.revision,
            scope: self.sidebar.scope.clone(),
            minute: now / 60_000,
        };
        if let Some(ranking) = self.sidebar.search_ranking.borrow().as_ref().filter(|r| r.key == key) {
            return ranking.threads.clone();
        }
        let tokens = search::fold_query(query);
        let candidates: Vec<(usize, &ThreadShell, Vec<SearchPullRequest>)> = self
            .shell
            .threads
            .iter()
            .enumerate()
            .filter(|(_, t)| visible(t))
            .map(|(at, t)| (at, t, crate::thread_pull_requests::search_entries(t)))
            .collect();
        let score = |typos: bool| {
            let mut found: Vec<(usize, f32, bool)> = candidates
                .iter()
                .filter_map(|(at, thread, pull_requests)| {
                    let subject = ThreadSubject {
                        id: &thread.id,
                        title: &thread.title,
                        branch: thread.branch.as_deref(),
                        project: None,
                        environment: None,
                        pull_requests,
                    };
                    let found = search::match_thread(&tokens, &subject, typos)?;
                    let used = search::ThreadUse::of(thread, self.local_opened_at(&thread.id));
                    Some((*at, found.quality * used.boost(now), found.strong))
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
        let threads: Rc<[usize]> = found.into_iter().map(|(at, ..)| at).collect();
        *self.sidebar.search_ranking.borrow_mut() = Some(Ranking { key, threads: threads.clone() });
        threads
    }

    /// Up/Down wrap through the results, Enter opens one, Escape clears.
    pub(super) fn search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.search_query(cx);
        if query.is_empty() {
            return;
        }
        let ids: Vec<ScopedId> =
            self.search_results(&query, now_ms()).iter().map(|r| ScopedId::of_thread(r.thread)).collect();
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
                    (_, None | Some(0)) => last,
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

    /// A row listed for a message match opens find on it, as the palette does.
    fn open_search_result(&mut self, key: ScopedId, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.sidebar.search.read(cx).value().trim().to_owned();
        let message_row = self
            .search_results(&self.search_query(cx), now_ms())
            .iter()
            .any(|r| key.is_thread(r.thread) && r.snippet.is_some());
        let find = message_row
            .then(|| {
                self.sidebar.messages.matches.iter().find(|m| m.environment_id == key.env && m.thread_id == key.id)
            })
            .flatten()
            .map(|m| (crate::app::find::query_for_message(&m.snippet, &query), m.message_id.clone()));
        self.clear_search(window, cx);
        self.select_thread_in(key.env, key.id, window, cx);
        if let Some((query, message_id)) = find {
            self.open_thread_find_on(&query, message_id, window, cx);
        }
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
        let route = self.route_key();
        let highlight = self.sidebar.search_highlight.as_ref();
        v_flex()
            .gap(px(1.))
            .children(results.into_iter().map(|result| {
                let thread = result.thread;
                let active = highlight.is_some_and(|key| key.is_thread(thread)) || route == Some(thread.thread_ref());
                let settled = thread.settled_override.as_deref() == Some("settled");
                let muted_icon = settled && !active;
                let hovered = self.sidebar.hovered.as_ref().is_some_and(|key| key.is_thread(thread));
                let project = self.shell.projects.iter().find(|p| p.project_ref() == thread.project_ref());
                let time = inbox::Facts { thread, child_request: None }
                    .activity_timestamp()
                    .map(|at| inbox::format_relative(now - at));
                let title_color = if active {
                    sb.foreground
                } else if settled {
                    sb.secondary_label.opacity(0.7)
                } else {
                    sb.foreground
                };
                let open_id = ScopedId::of_thread(thread);
                let hover_id = open_id.clone();
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
                    .id(row_id("search", &thread.environment_id, &thread.id))
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

/// Whether a message hit is from `thread`, on its environment.
fn is_hit_thread(hit: &MessageMatch, thread: &ThreadShell) -> bool {
    hit.environment_id == thread.environment_id && hit.thread_id == thread.id
}
