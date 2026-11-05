//! The Pull requests page (`apps/web/src/routes/_chat.pull-requests.tsx`: `PullRequestsPage`,
//! `PullRequestsColumn`, `PullRequestListGroups`, `PullRequestRow`). The list reads
//! `pullRequests.list` (plus `pullRequests.listStats` for diff sizes the listing leaves out),
//! and Refresh asks the host again through `pullRequests.invalidate`.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, ElementId, Entity, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ListAlignment, ListState, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, list, px, relative, rgb, svg,
};
use serde_json::{Value, json};

use super::super::Workspace;
use super::super::sidebar::now_ms;
use crate::pull_request_detail::Reference;
use crate::pull_requests::{
    self as logic, CHECKS, DRAFTS, Entry, Filters, ListResult, MAX_PAGE_SIZE, PAGE_SIZE, REVIEWS, SORTS, STATES, Stats,
    View,
};
use crate::theme::{self, FONT_MONO, Tokens};

/// `PullRequestRef` for a listed row.
fn reference_of(entry: &Entry) -> Reference {
    Reference {
        project_id: entry.project_id.clone(),
        host: Some(entry.host.clone()),
        repository: entry.repository.clone(),
        number: entry.number,
    }
}

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
/// `WorkspacePageContainer width="expanded"`: `max-w-6xl px-6 pt-6 pb-12`.
const PAGE_MAX_WIDTH: f32 = 1152.;
const PAGE_GUTTER: f32 = 24.;

/// One line of the virtualized list.
enum Row {
    Header {
        key: &'static str,
        label: &'static str,
        count: usize,
        first: bool,
    },
    Entry {
        entry: Rc<Entry>,
        stat: Option<(u64, u64)>,
        elsewhere: bool,
        first: bool,
    },
    /// The read failed but earlier rows are still shown.
    Stale(SharedString),
    Footer(Footer),
}

#[derive(Clone, Copy)]
enum Footer {
    Loading { continuing: bool },
    More,
    Narrow,
}

/// The page's own state, kept while it is closed so reopening shows the last answer at once.
pub(in crate::app) struct PullRequestsState {
    involvement: &'static str,
    state: &'static str,
    sort: &'static str,
    host: Option<String>,
    project: Option<String>,
    /// Draft, review, checks, author and labels picked from the menus.
    menu: Filters,
    search: Entity<InputState>,
    /// What the search box holds now, and what was last sent to the host.
    typed: String,
    sent: String,
    list: Option<ListResult>,
    page_size: u32,
    /// Bumped per read, so an answer to an older question is dropped.
    generation: u64,
    loading: bool,
    loading_more: bool,
    refreshing: bool,
    error: Option<SharedString>,
    stats: Stats,
    rows: Rc<Vec<Row>>,
    list_state: ListState,
    _debounce: Option<Task<()>>,
    _search_subscription: Subscription,
    /// The pull request open beside the list.
    pub(super) panel: Option<super::pull_request_detail::Panel>,
    /// The merge method last picked in any panel (`uiStateStore.pullRequestMergeMethod`).
    pub(super) last_merge_method: &'static str,
    /// Unsent reviews by pull request, kept across panels (`usePullRequestReviewStore`).
    pub(super) reviews: std::collections::HashMap<String, super::pull_request_detail::write::ReviewDraft>,
}

impl PullRequestsState {
    pub(in crate::app) fn new(window: &mut Window, cx: &mut Context<Workspace>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search pull requests, or label:bug"));
        let subscription = cx.subscribe(&search, |this, search, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                this.pull_requests.typed = search.read(cx).value().trim().to_owned();
                this.rebuild_pull_request_rows();
                this.debounce_pull_request_search(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.send_pull_request_search(cx),
            _ => {}
        });
        Self {
            involvement: "all",
            state: "open",
            sort: "ready",
            host: None,
            project: None,
            menu: Filters::default(),
            search,
            typed: String::new(),
            sent: String::new(),
            list: None,
            page_size: PAGE_SIZE,
            generation: 0,
            loading: false,
            loading_more: false,
            refreshing: false,
            error: None,
            stats: Stats::new(),
            rows: Rc::default(),
            list_state: ListState::new(0, ListAlignment::Top, px(480.)),
            _debounce: None,
            _search_subscription: subscription,
            panel: None,
            last_merge_method: "merge",
            reviews: std::collections::HashMap::new(),
        }
    }

    /// The listed row for a pull request, which the panel shows while its detail loads.
    pub(super) fn entry_for(&self, reference: &Reference) -> Option<Rc<Entry>> {
        self.list.as_ref()?.entries.iter().find(|e| reference_of(e) == *reference).cloned()
    }

    fn filter_count(&self) -> usize {
        let menu = &self.menu;
        [self.state != "open", self.involvement != "all", self.host.is_some(), self.project.is_some()]
            .into_iter()
            .chain([menu.draft.is_some(), menu.review.is_some(), menu.checks.is_some(), menu.author.is_some()])
            .filter(|on| *on)
            .count()
            + menu.labels.iter().map(Vec::len).sum::<usize>()
    }

    fn filtered(&self) -> bool {
        !self.menu.is_empty()
            || self.state != "open"
            || self.involvement != "all"
            || self.project.is_some()
            || self.host.is_some()
    }

    fn searching(&self) -> bool {
        !self.typed.is_empty() && (self.typed != self.sent || self.loading)
    }

    fn can_continue(&self) -> bool {
        self.list.as_ref().is_some_and(|list| !list.next_cursors.is_empty())
    }

    fn can_load_more(&self) -> bool {
        self.list.as_ref().is_some_and(|list| list.truncated) && (self.can_continue() || self.page_size < MAX_PAGE_SIZE)
    }
}

impl Workspace {
    /// Reads the list the first time the page opens; later opens show what was read.
    pub(in crate::app) fn pull_requests_opened(&mut self, cx: &mut Context<Self>) {
        if self.pull_requests.list.is_none() && !self.pull_requests.loading {
            self.read_pull_requests(false, cx);
        }
    }

    /// Changes what the list asks for and reads it again from the first page.
    fn set_pull_request_scope(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut PullRequestsState)) {
        change(&mut self.pull_requests);
        self.pull_requests.page_size = PAGE_SIZE;
        self.read_pull_requests(false, cx);
        self.rebuild_pull_request_rows();
        cx.notify();
    }

    fn debounce_pull_request_search(&mut self, cx: &mut Context<Self>) {
        self.pull_requests._debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| this.send_pull_request_search(cx));
        }));
    }

    fn send_pull_request_search(&mut self, cx: &mut Context<Self>) {
        self.pull_requests._debounce = None;
        if self.pull_requests.sent == self.pull_requests.typed {
            return;
        }
        let typed = self.pull_requests.typed.clone();
        self.set_pull_request_scope(cx, |state| state.sent = typed);
    }

    fn clear_pull_request_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pull_requests.search.update(cx, |search, cx| search.set_value("", window, cx));
    }

    /// `PullRequestListInput` for the current scope; `cursors` continues a truncated read.
    fn pull_request_input(&self, cursors: Option<&serde_json::Map<String, Value>>) -> Value {
        let state = &self.pull_requests;
        let (text, typed_filters) = logic::parse_query(&state.sent);
        let filters = state.menu.merged(&typed_filters);
        let mut input = json!({"state": state.state, "involvement": state.involvement, "limit": state.page_size});
        if let Some(host) = &state.host {
            input["host"] = json!(host);
        }
        if let Some(project) = &state.project {
            input["projectId"] = json!(project);
        }
        if !filters.is_empty() {
            input["filters"] = filters.to_json();
        }
        if !text.is_empty() {
            input["query"] = json!(text.chars().take(200).collect::<String>());
        }
        if let Some(cursors) = cursors {
            input["cursors"] = Value::Object(cursors.clone());
        }
        input
    }

    /// Reads the list; `more` continues from the last answer's cursors, or asks for a longer
    /// page when the host gave none.
    pub(super) fn read_pull_requests(&mut self, more: bool, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let state = &mut self.pull_requests;
        let cursors =
            more.then(|| state.list.as_ref().map(|l| l.next_cursors.clone())).flatten().filter(|c| !c.is_empty());
        if more && cursors.is_none() {
            state.page_size = (state.page_size + PAGE_SIZE).min(MAX_PAGE_SIZE);
        }
        state.generation += 1;
        let generation = state.generation;
        state.loading = true;
        state.loading_more = more;
        let continuing = cursors.is_some();
        let input = self.pull_request_input(cursors.as_ref());
        cx.spawn(async move |this, cx| {
            let result = client.request("pullRequests.list", input).await;
            let _ = this.update(cx, |this, cx| {
                let state = &mut this.pull_requests;
                if state.generation != generation {
                    return;
                }
                state.loading = false;
                state.loading_more = false;
                match result.map(serde_json::from_value::<ListResult>) {
                    Ok(Ok(mut answer)) => {
                        state.error = None;
                        let (text, _) = logic::parse_query(&state.sent);
                        match (continuing, state.list.as_mut()) {
                            (true, Some(held)) => logic::append_page(held, answer, &text),
                            _ => {
                                logic::rank_matches(&mut answer.entries, &text);
                                state.list = Some(answer);
                            }
                        }
                        this.read_pull_request_stats(cx);
                    }
                    Ok(Err(error)) => state.error = Some(format!("Unreadable pull request list: {error}").into()),
                    Err(error) => state.error = Some(logic::error_message(&error).into()),
                }
                this.rebuild_pull_request_rows();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Asks the host for the diff sizes the listing left at zero.
    fn read_pull_request_stats(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(list)) = (self.client(), self.pull_requests.list.as_ref()) else { return };
        let stats = &self.pull_requests.stats;
        let refs: Vec<Value> = list
            .entries
            .iter()
            .filter(|e| e.additions + e.deletions == 0 && !stats.contains_key(&e.stat_key()))
            .take(500)
            .map(|e| e.reference())
            .collect();
        if refs.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let Ok(answer) = client.request("pullRequests.listStats", json!({"refs": refs})).await else { return };
            let _ = this.update(cx, |this, cx| {
                for stat in answer.get("stats").and_then(Value::as_array).into_iter().flatten() {
                    let field = |key: &str| stat.get(key).and_then(Value::as_str).unwrap_or_default();
                    let count = |key: &str| stat.get(key).and_then(Value::as_u64).unwrap_or_default();
                    let key = logic::stat_key(field("projectId"), field("repository"), count("number"));
                    this.pull_requests.stats.insert(key, (count("additions"), count("deletions")));
                }
                this.rebuild_pull_request_rows();
                cx.notify();
            });
        })
        .detach();
    }

    /// Refresh: the host is asked again rather than the server's cache.
    fn refresh_pull_requests(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        self.pull_requests.refreshing = true;
        cx.spawn(async move |this, cx| {
            let _ = client.request("pullRequests.invalidate", json!({})).await;
            let _ = this.update(cx, |this, cx| {
                this.pull_requests.refreshing = false;
                this.pull_requests.stats.clear();
                this.pull_requests.page_size = PAGE_SIZE;
                this.read_pull_requests(false, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn rebuild_pull_request_rows(&mut self) {
        let state = &mut self.pull_requests;
        let mut rows = Vec::new();
        if let Some(list) = &state.list {
            let (text, typed_filters) = logic::parse_query(&state.typed);
            let local_filters = state.menu.merged(&typed_filters);
            let view = View {
                state: state.state,
                project: state.project.as_deref(),
                host: state.host.as_deref(),
                involvement: state.involvement,
                sort: state.sort,
                local_filters: &local_filters,
                text: &text,
                answered_locally: state.typed == state.sent && !state.loading,
            };
            for (index, group) in logic::display_groups(list, &view, &state.stats).into_iter().enumerate() {
                if !group.label.is_empty() {
                    rows.push(Row::Header {
                        key: group.key,
                        label: group.label,
                        count: group.entries.len(),
                        first: index == 0,
                    });
                }
                for (position, entry) in group.entries.into_iter().enumerate() {
                    rows.push(Row::Entry {
                        stat: logic::diff_stat(&entry, &state.stats),
                        elsewhere: logic::matched_elsewhere(&entry, &text),
                        first: position == 0 && (index == 0 || !group.label.is_empty()),
                        entry,
                    });
                }
            }
            let shown = rows.iter().any(|row| matches!(row, Row::Entry { .. }));
            if let (Some(error), true) = (&state.error, shown) {
                rows.push(Row::Stale(error.clone()));
            }
            if list.truncated && shown {
                rows.push(Row::Footer(if state.loading_more {
                    Footer::Loading { continuing: state.can_continue() }
                } else if state.can_load_more() {
                    Footer::More
                } else {
                    Footer::Narrow
                }));
            }
        }
        state.list_state.reset(rows.len());
        state.rows = Rc::new(rows);
    }

    pub(super) fn render_pull_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let state = &self.pull_requests;
        let shown = state.rows.iter().any(|row| matches!(row, Row::Entry { .. }));
        // Nothing held fits the new scope yet: the read is still the first load (`firstLoad`).
        let first_load = state.loading && !shown && state.typed.is_empty();
        let body = if (state.list.is_none() && state.error.is_none()) || first_load {
            ghost_list(7, Some("Loading pull requests…"), &tokens).into_any_element()
        } else if let (Some(error), false) = (&state.error, shown) {
            self.render_pull_requests_unavailable(error.clone(), &tokens, cx)
        } else if !shown {
            self.render_pull_requests_empty(&tokens, cx)
        } else {
            self.render_pull_request_list(&tokens, cx)
        };
        let column = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .child(page_frame().pt(px(24.)).pb(px(16.)).child(self.render_pull_request_toolbar(&tokens, cx)))
            .child(div().flex_1().min_h_0().child(body));
        if self.pull_requests.panel.is_none() {
            return column.into_any_element();
        }
        // `RightPanelTabs mode="inline"` opens at half the window.
        let width = (f32::from(window.viewport_size().width) / 2.).max(360.);
        h_flex()
            .size_full()
            .min_h_0()
            .child(column)
            .child(
                div()
                    .flex_none()
                    .w(px(width))
                    .h_full()
                    .min_h_0()
                    .border_l_1()
                    .border_color(tokens.border)
                    .child(self.render_pull_request_panel(window, cx)),
            )
            .into_any_element()
    }

    fn render_pull_request_toolbar(&self, t: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let state = &self.pull_requests;
        let view = cx.entity();
        let search = h_flex()
            .id("pr-search")
            .flex_1()
            .min_w(px(96.))
            .h(px(32.))
            .px(px(10.))
            .gap(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.input)
            .bg(control_background(t))
            .child(Icon::new(Lucide::Search).with_size(px(16.)).text_color(t.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&state.search).appearance(false).px_0().py_0().h(px(20.)).text_size(px(14.))),
            );

        let sort = state.sort;
        let sort_menu = outline_button("pr-sort", t)
            .child(Icon::new(Lucide::ArrowDownUp).with_size(px(16.)).text_color(t.muted_foreground))
            .child("Sort")
            .dropdown_menu(radio_menu(SORTS, sort, sort_icon, view.clone(), |this, value, cx| {
                this.pull_requests.sort = value;
                this.rebuild_pull_request_rows();
                cx.notify();
            }));

        let count = state.filter_count();
        let filters = outline_button("pr-filters", t)
            .child(Icon::new(Lucide::ListFilter).with_size(px(16.)).text_color(t.muted_foreground))
            .child("Filters")
            .when(count > 0, |el| {
                el.child(
                    div()
                        .rounded_full()
                        .bg(t.muted)
                        .px(px(6.))
                        .text_xs()
                        .text_color(t.muted_foreground)
                        .child(count.to_string()),
                )
            })
            .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, self.filters_menu(view.clone()));

        let hosts: Vec<(String, String, Option<String>)> = state
            .list
            .as_ref()
            .map(|l| {
                l.providers
                    .iter()
                    .map(|p| {
                        let unavailable = (!p.configured)
                            .then(|| p.detail.clone().unwrap_or_else(|| "This host could not be read.".into()));
                        (p.host.clone(), p.kind.clone(), unavailable)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let host = state.host.clone();
        let provider = outline_button("pr-provider", t)
            .when(host.is_some(), |b| b.w(px(32.)).px_0())
            .child(Icon::new(Lucide::Plug2).with_size(px(16.)).text_color(t.muted_foreground))
            .when(host.is_none(), |b| b.child("All"))
            .dropdown_menu({
                let view = view.clone();
                move |mut menu, window, _| {
                    menu = menu.item(PopupMenuItem::new("All").icon(Lucide::Plug2).checked(host.is_none()).on_click(
                        window.listener_for(&view, |this, _, _, cx| {
                            this.set_pull_request_scope(cx, |state| state.host = None)
                        }),
                    ));
                    for (name, kind, unavailable) in &hosts {
                        let picked = name.clone();
                        let mut label =
                            if hosts.len() > 1 { name.clone() } else { logic::provider_name(kind).to_owned() };
                        if unavailable.is_some() {
                            label.push_str(" · Unavailable");
                        }
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(host.as_ref() == Some(name))
                                .disabled(unavailable.is_some())
                                .on_click(window.listener_for(&view, move |this, _, _, cx| {
                                    let host = picked.clone();
                                    this.set_pull_request_scope(cx, |state| state.host = Some(host))
                                })),
                        );
                    }
                    menu
                }
            });

        let busy = state.refreshing || state.loading;
        let refresh = outline_button("pr-refresh", t)
            .w(px(32.))
            .px_0()
            .disabled(busy)
            .tooltip("Refresh pull requests")
            .child(Icon::new(Lucide::RefreshCw).with_size(px(16.)).text_color(t.muted_foreground))
            .on_click(cx.listener(|this, _, _, cx| this.refresh_pull_requests(cx)));

        h_flex()
            .w_full()
            .flex_wrap()
            .gap(px(8.))
            .child(search)
            .child(sort_menu)
            .child(filters)
            .child(provider)
            .child(refresh)
    }

    /// The Filters menu: state and involvement, then author, labels, draft, review and checks,
    /// then project (`PullRequestFiltersMenu`).
    fn filters_menu(
        &self,
        view: Entity<Workspace>,
    ) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let state = &self.pull_requests;
        let (authors, labels) = state.list.as_ref().map(|l| logic::facets(&l.entries, state.state)).unwrap_or_default();
        let authors: Rc<Vec<String>> = Rc::new(authors.into_iter().take(15).map(|a| a.login).collect());
        let labels: Rc<Vec<String>> = Rc::new(labels.into_iter().take(30).map(|l| l.name).collect());
        let unreadable: Vec<String> =
            state.list.as_ref().map(|l| l.errors.iter().map(|e| e.project_id.clone()).collect()).unwrap_or_default();
        let mut projects: Vec<(String, String, bool)> = self
            .shell
            .projects
            .iter()
            .filter(|p| p.archived_at.is_none())
            .map(|p| (p.id.clone(), p.title.clone(), unreadable.contains(&p.id)))
            .collect();
        projects.sort_by_key(|p| p.2);
        let projects = Rc::new(projects);
        let menu = state.menu.clone();
        let (current_state, involvement, project) = (state.state, state.involvement, state.project.clone());
        move |menu_root, window, cx| {
            let view = view.clone();
            let submenu = |menu_root: PopupMenu,
                           label: &str,
                           options: &'static [(&'static str, &'static str)],
                           current: &'static str,
                           window: &mut Window,
                           cx: &mut Context<PopupMenu>,
                           apply: fn(&mut PullRequestsState, &'static str)| {
                let view = view.clone();
                let value = options.iter().find(|o| o.0 == current).map_or("All", |o| o.1);
                menu_root.submenu(format!("{label}  {value}"), window, cx, move |mut menu, window, _| {
                    for (value, option) in options {
                        let value: &'static str = value;
                        menu = menu.item(
                            PopupMenuItem::new(*option).icon(option_icon(value)).checked(value == current).on_click(
                                window.listener_for(&view, move |this, _, _, cx| {
                                    this.set_pull_request_scope(cx, |state| apply(state, value))
                                }),
                            ),
                        );
                    }
                    menu
                })
            };
            let as_static = |value: Option<&String>, options: &'static [(&'static str, &'static str)]| {
                value.and_then(|v| options.iter().find(|o| o.0 == v)).map_or("all", |o| o.0)
            };
            let mut root = submenu(menu_root, "State", STATES, current_state, window, cx, |s, v| s.state = v);
            root = submenu(root, "Involvement", logic::INVOLVEMENTS, involvement, window, cx, |s, v| s.involvement = v);
            root = root.separator();
            let author_view = view.clone();
            let picked_author = menu.author.clone();
            let author_choices = authors.clone();
            root = root.submenu(
                format!("Author  {}", menu.author.as_deref().unwrap_or("Anyone")),
                window,
                cx,
                move |mut menu, window, _| {
                    menu = menu.item(PopupMenuItem::new("Anyone").checked(picked_author.is_none()).on_click(
                        window.listener_for(&author_view, |this, _, _, cx| {
                            this.set_pull_request_scope(cx, |state| state.menu.author = None)
                        }),
                    ));
                    for login in author_choices.iter() {
                        let value = login.clone();
                        menu = menu.item(
                            PopupMenuItem::new(login.clone())
                                .checked(picked_author.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(login)))
                                .on_click(window.listener_for(&author_view, move |this, _, _, cx| {
                                    let author = value.clone();
                                    this.set_pull_request_scope(cx, |state| state.menu.author = Some(author))
                                })),
                        );
                    }
                    menu
                },
            );
            let label_view = view.clone();
            let selected: Vec<String> = menu.labels.iter().flatten().cloned().collect();
            let label_choices = labels.clone();
            let label_summary =
                if selected.is_empty() { "Any".to_owned() } else { format!("{} selected", selected.len()) };
            root = root.submenu(format!("Labels  {label_summary}"), window, cx, move |mut menu, window, _| {
                if label_choices.is_empty() && selected.is_empty() {
                    return menu.label("No labels found");
                }
                let mut names: Vec<String> = selected.clone();
                names.extend(
                    label_choices.iter().filter(|l| !selected.iter().any(|s| s.eq_ignore_ascii_case(l))).cloned(),
                );
                for name in names {
                    let checked = selected.iter().any(|s| s.eq_ignore_ascii_case(&name));
                    let value = name.clone();
                    menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(window.listener_for(
                        &label_view,
                        move |this, _, _, cx| {
                            let label = value.clone();
                            this.set_pull_request_scope(cx, |state| {
                                let mut picked: Vec<String> = state.menu.labels.iter().flatten().cloned().collect();
                                match picked.iter().position(|l| l.eq_ignore_ascii_case(&label)) {
                                    Some(index) => {
                                        picked.remove(index);
                                    }
                                    None => picked.push(label),
                                }
                                state.menu.labels = picked.into_iter().take(10).map(|l| vec![l]).collect();
                            })
                        },
                    )));
                }
                menu
            });
            let draft = as_static(menu.draft.as_ref(), DRAFTS);
            let draft_view = view.clone();
            root = root.submenu(
                format!("Draft  {}", DRAFTS.iter().find(|o| o.0 == draft).map_or("All", |o| o.1)),
                window,
                cx,
                move |mut menu, window, _| {
                    for (value, option) in DRAFTS {
                        let value: &'static str = value;
                        menu = menu.item(
                            PopupMenuItem::new(*option).icon(option_icon(value)).checked(value == draft).on_click(
                                window.listener_for(&draft_view, move |this, _, window, cx| {
                                    this.pick_draft_filter(value, window, cx)
                                }),
                            ),
                        );
                    }
                    menu
                },
            );
            root = submenu(root, "Review", REVIEWS, as_static(menu.review.as_ref(), REVIEWS), window, cx, |s, v| {
                s.menu.review = (v != "all").then(|| v.to_owned())
            });
            root = submenu(root, "Checks", CHECKS, as_static(menu.checks.as_ref(), CHECKS), window, cx, |s, v| {
                s.menu.checks = (v != "all").then(|| v.to_owned())
            });
            root = root.separator();
            let project_view = view.clone();
            let project_choices = projects.clone();
            let picked_project = project.clone();
            let project_label = picked_project
                .as_ref()
                .and_then(|id| project_choices.iter().find(|p| &p.0 == id))
                .map_or("All projects".to_owned(), |p| p.1.clone());
            root.submenu(format!("Project  {project_label}"), window, cx, move |mut menu, window, _| {
                menu = menu.item(PopupMenuItem::new("All projects").checked(picked_project.is_none()).on_click(
                    window.listener_for(&project_view, |this, _, _, cx| {
                        this.set_pull_request_scope(cx, |state| state.project = None)
                    }),
                ));
                for (id, title, unavailable) in project_choices.iter() {
                    let value = id.clone();
                    let label = if *unavailable { format!("{title} · Unavailable") } else { title.clone() };
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .icon(Lucide::FolderGit2)
                            .disabled(*unavailable)
                            .checked(picked_project.as_ref() == Some(id))
                            .on_click(window.listener_for(&project_view, move |this, _, _, cx| {
                                let project = value.clone();
                                this.set_pull_request_scope(cx, |state| state.project = Some(project))
                            })),
                    );
                }
                menu.max_h(px(360.)).scrollable(true)
            })
        }
    }

    /// `applyPullRequestDraftFilterChange`: a typed `draft:` qualifier leaves the query when the
    /// menu picks another value.
    fn pick_draft_filter(&mut self, value: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        let typed = logic::parse_query(&self.pull_requests.typed).1.draft;
        if typed.is_some() {
            let query = logic::drop_draft_qualifier(&self.pull_requests.typed);
            self.pull_requests.search.update(cx, |search, cx| search.set_value(query, window, cx));
        }
        self.set_pull_request_scope(cx, |state| {
            state.menu.draft = (value != "all").then(|| value.to_owned());
        });
    }

    fn render_pull_request_list(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let rows = self.pull_requests.rows.clone();
        let view = cx.entity();
        let t = *t;
        let now = now_ms();
        let selected = self.pull_requests.panel.as_ref().map(|p| p.reference().clone());
        // Labels and author names show as the row's meta line widens (`@xs`, `@xl`, `@3xl`).
        let meta_width = f32::from(self.pull_requests.list_state.viewport_bounds().size.width).min(PAGE_MAX_WIDTH)
            - 2. * PAGE_GUTTER
            - 48.;
        list(self.pull_requests.list_state.clone(), move |index, window, cx| {
            let content = match &rows[index] {
                Row::Header { key, label, count, first } => group_header(key, label, *count, *first, &t),
                Row::Entry { entry, stat, elsewhere, first } => {
                    let selected = selected.as_ref().is_some_and(|r| *r == reference_of(entry));
                    pull_request_row(entry, *stat, *elsewhere, *first, selected, meta_width, now, &view, &t, window, cx)
                }
                Row::Stale(error) => stale_banner(error.clone(), &view, &t, window),
                Row::Footer(footer) => list_footer(*footer, &view, &t, window),
            };
            page_frame().pb(px(if index + 1 == rows.len() { 48. } else { 0. })).child(content).into_any_element()
        })
        .size_full()
        .into_any_element()
    }

    fn render_pull_requests_unavailable(&self, error: SharedString, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.pull_requests.loading || self.pull_requests.refreshing;
        empty_state(t)
            .child(
                div()
                    .mb(px(24.))
                    .size(px(36.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.card)
                    .child(Icon::new(Lucide::GitPullRequestArrow).with_size(px(18.)).text_color(t.foreground)),
            )
            .child(empty_header("Could not load pull requests", error, t))
            .child(
                outline_button("pr-retry", t)
                    .disabled(busy)
                    .child(Icon::new(Lucide::RefreshCw).with_size(px(14.)).text_color(t.muted_foreground))
                    .child("Retry")
                    .on_click(cx.listener(|this, _, _, cx| this.read_pull_requests(false, cx))),
            )
            .into_any_element()
    }

    fn render_pull_requests_empty(&self, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let state = &self.pull_requests;
        if self.shell.projects.is_empty() && self.shell.live {
            return empty_state(t)
                .child(branch_mark(t))
                .child(empty_header(
                    "No projects in this workspace",
                    "Add a project, and the pull requests from its repository appear here.".into(),
                    t,
                ))
                .into_any_element();
        }
        let quoted = || {
            let query: String = state.typed.chars().take(48).collect();
            if state.typed.chars().count() > 48 { format!("{query}…") } else { query }
        };
        if state.searching() {
            return ghost_list(5, Some(&format!("Searching every host for “{}”", quoted())), t).into_any_element();
        }
        let busy = state.refreshing || state.loading;
        let check_again = outline_button("pr-check-again", t)
            .disabled(busy)
            .child(Icon::new(Lucide::RefreshCw).with_size(px(14.)).text_color(t.muted_foreground))
            .child(if busy { "Checking..." } else { "Check again" })
            .on_click(cx.listener(|this, _, _, cx| this.refresh_pull_requests(cx)));
        let buttons = h_flex().flex_wrap().justify_center().gap(px(8.));
        if !state.typed.is_empty() {
            return empty_state(t)
                .child(branch_mark(t))
                .child(empty_header(
                    &format!("Nothing matches “{}”", quoted()),
                    "The hosts were searched for it. Try fewer words, or search by number, author or branch.".into(),
                    t,
                ))
                .child(
                    buttons
                        .child(
                            outline_button("pr-clear-search", t)
                                .child(Icon::new(Lucide::Search).with_size(px(14.)).text_color(t.muted_foreground))
                                .child("Clear search")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.clear_pull_request_search(window, cx)),
                                ),
                        )
                        .child(check_again),
                )
                .into_any_element();
        }
        let filtered = state.filtered();
        empty_state(t)
            .child(branch_mark(t))
            .child(empty_header(
                if filtered { "Nothing under these filters" } else { "No pull requests" },
                if filtered {
                    "Widen the state, involvement or project filter to see more.".into()
                } else {
                    "Pull requests from every project in this workspace appear here.".into()
                },
                t,
            ))
            .child(
                buttons
                    .when(state.can_load_more(), |el| {
                        el.child(
                            outline_button("pr-load-more-empty", t)
                                .disabled(state.loading_more)
                                .child(if state.loading_more { "Loading..." } else { "Load more pull requests" })
                                .on_click(cx.listener(|this, _, _, cx| this.read_pull_requests(true, cx))),
                        )
                    })
                    .child(check_again),
            )
            .into_any_element()
    }
}

/// The centered `max-w-6xl` column every part of the page sits in.
fn page_frame() -> gpui_kit::Div {
    div().w_full().max_w(px(PAGE_MAX_WIDTH)).mx_auto().px(px(PAGE_GUTTER))
}

/// `dark:bg-input/32` over the popover surface, as the web's outline controls.
fn control_background(t: &Tokens) -> Hsla {
    if t.dark { Hsla { a: t.input.a * 0.32, ..t.input } } else { t.popover }
}

/// `Button variant="outline"` at the desktop's `sm:h-8`.
pub(super) fn outline_button(id: &'static str, t: &Tokens) -> Button {
    Button::new(id)
        .outline()
        .small()
        .h(px(32.))
        .px(px(11.))
        .gap(px(8.))
        .rounded(px(8.))
        .border_color(t.input)
        .bg(control_background(t))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .text_color(t.foreground)
}

/// `PullRequestFilterOptionIcon` for the state, involvement, draft, review and checks options.
fn option_icon(value: &str) -> Lucide {
    match value {
        "open" => Lucide::GitPullRequestArrow,
        "closed" => Lucide::GitPullRequestClosed,
        "merged" => Lucide::GitMerge,
        "reviewing" => Lucide::Eye,
        "authored" => Lucide::PenLine,
        "only" => Lucide::GitPullRequestDraft,
        "hide" => Lucide::EyeOff,
        "approved" | "passing" => Lucide::CircleCheck,
        "changes-requested" | "failing" => Lucide::CircleX,
        "review-required" => Lucide::CircleDashed,
        "none" => Lucide::CircleSlash,
        _ => Lucide::Layers,
    }
}

fn sort_icon(value: &str) -> Option<Lucide> {
    Some(match value {
        "ready" => Lucide::ListChecks,
        "blocked" => Lucide::UserLock,
        "updated" => Lucide::Clock,
        "newest" => Lucide::CalendarArrowDown,
        "oldest" => Lucide::CalendarArrowUp,
        "largest" => Lucide::Maximize2,
        "smallest" => Lucide::Minimize2,
        _ => return None,
    })
}

/// A radio menu over `options`, as `PullRequestCompactFilterMenu`.
fn radio_menu(
    options: &'static [(&'static str, &'static str)],
    current: &'static str,
    icon: fn(&str) -> Option<Lucide>,
    view: Entity<Workspace>,
    apply: fn(&mut Workspace, &'static str, &mut Context<Workspace>),
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    move |mut menu, window, _| {
        for (value, label) in options {
            let value: &'static str = value;
            let mut item = PopupMenuItem::new(*label).checked(value == current);
            if let Some(icon) = icon(value) {
                item = item.icon(icon);
            }
            menu = menu.item(item.on_click(window.listener_for(&view, move |this, _, _, cx| apply(this, value, cx))));
        }
        menu
    }
}

fn empty_state(_: &Tokens) -> gpui_kit::Div {
    v_flex().size_full().min_h_0().items_center().justify_center().gap(px(24.)).p(px(48.)).text_center()
}

fn empty_header(title: &str, description: SharedString, t: &Tokens) -> impl IntoElement {
    v_flex()
        .max_w(px(384.))
        .items_center()
        .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).text_color(t.foreground).child(title.to_owned()))
        .child(div().mt(px(4.)).text_sm().text_color(t.muted_foreground).child(description))
}

fn branch_mark(t: &Tokens) -> impl IntoElement {
    svg().path("icons/app/pr-branch-mark.svg").w(px(128.)).h(px(80.)).text_color(t.muted_foreground)
}

/// `PullRequestListGhost`: placeholder rows while the list loads.
fn ghost_list(rows: usize, caption: Option<&str>, t: &Tokens) -> impl IntoElement {
    const TITLE: [f32; 7] = [0.6, 0.4, 0.5, 0.667, 0.4, 0.6, 0.5];
    const META: [f32; 7] = [0.4, 0.333, 0.4, 0.25, 0.333, 0.4, 0.333];
    let bar = |w: gpui_kit::DefiniteLength, h: f32| {
        div().w(w).h(px(h)).rounded(px(4.)).bg(Hsla { a: 0.15, ..t.muted_foreground })
    };
    page_frame().child(
        v_flex()
            .gap(px(2.))
            .children(caption.map(|caption| {
                div()
                    .px(px(12.))
                    .pb(px(4.))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.muted_foreground)
                    .child(caption.to_owned())
            }))
            .children((0..rows).map(|index| {
                h_flex()
                    .px(px(12.))
                    .py(px(10.))
                    .gap(px(8.))
                    .child(bar(px(16.).into(), 16.).rounded_full())
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.))
                            .child(bar(relative(TITLE[index % 7]), 16.))
                            .child(bar(relative(META[index % 7]), 14.)),
                    )
                    .child(
                        v_flex()
                            .items_end()
                            .gap(px(6.))
                            .child(bar(px(48.).into(), 12.))
                            .child(bar(px(64.).into(), 12.)),
                    )
            })),
    )
}

fn group_header(key: &str, label: &'static str, count: usize, first: bool, t: &Tokens) -> AnyElement {
    let icon = match key {
        "authored" => Lucide::PenLine,
        "reviewRequested" => Lucide::Eye,
        "others" => Lucide::Users,
        _ => Lucide::Layers,
    };
    let color = Hsla { a: 0.7, ..t.muted_foreground };
    h_flex()
        .when(!first, |el| el.mt(px(12.)))
        .px(px(12.))
        .pb(px(4.))
        .mb(px(2.))
        .gap(px(8.))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(Icon::new(icon).with_size(px(14.)).text_color(color))
        .child(label)
        .child(div().text_color(Hsla { a: 0.5, ..t.muted_foreground }).child(count.to_string()))
        .child(div().flex_1().min_w(px(8.)).h(px(1.)).bg(t.border))
        .into_any_element()
}

/// Tailwind's tones the row glyphs use, light and dark (`pullRequestIcons.tsx`, `pullRequestPresentation.tsx`).
pub(super) fn tone(t: &Tokens, light: u32, dark: u32, dark_alpha: f32) -> Hsla {
    if t.dark { Hsla { a: dark_alpha, ..rgb(dark).into() } } else { rgb(light).into() }
}

fn state_glyph(entry: &Entry, t: &Tokens) -> (Lucide, Hsla, &'static str) {
    match (entry.state.as_str(), entry.is_draft) {
        ("merged", _) => (Lucide::GitMerge, tone(t, 0x7f22fe, 0xc4b4ff, 0.9), "Merged"),
        ("closed", _) => (Lucide::GitPullRequestClosed, tone(t, 0xe7000b, 0xffa2a2, 0.9), "Closed"),
        (_, true) => (Lucide::GitPullRequestDraft, tone(t, 0x71717b, 0x9f9fa9, 0.8), "Draft"),
        _ => (Lucide::GitPullRequestArrow, tone(t, 0x009966, 0x5ee9b5, 0.9), "Open"),
    }
}

fn checks_glyph(state: &str, t: &Tokens) -> Option<(Lucide, Hsla, &'static str)> {
    Some(match state {
        "passing" => (Lucide::CircleCheck, tone(t, 0x009966, 0x5ee9b5, 0.9), "All checks have passed"),
        "failing" => (Lucide::CircleX, t.error, "Some checks were not successful"),
        "pending" => (Lucide::CircleDot, tone(t, 0xe17100, 0xffb900, 0.9), "Some checks haven't completed yet"),
        _ => return None,
    })
}

fn review_glyph(decision: &str, t: &Tokens) -> Option<(Lucide, Hsla, &'static str)> {
    Some(match decision {
        "approved" => (Lucide::UserCheck, tone(t, 0x009966, 0x5ee9b5, 0.9), "Approved"),
        "changes-requested" => (Lucide::UserRoundX, tone(t, 0xe17100, 0xffb900, 0.8), "Changes requested"),
        "review-required" => (Lucide::UserRound, Hsla { a: 0.6, ..t.muted_foreground }, "Awaiting review"),
        _ => return None,
    })
}

fn glyph(id: String, icon: Lucide, size: f32, color: Hsla, label: impl Into<SharedString>) -> impl IntoElement {
    let label: SharedString = label.into();
    div()
        .id(ElementId::Name(id.into()))
        .flex_none()
        .flex()
        .child(Icon::new(icon).with_size(px(size)).text_color(color))
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
}

/// `formatRelativeElapsed`.
pub(super) fn relative_time(elapsed_ms: i64) -> String {
    let minutes = elapsed_ms.max(0) / 60_000;
    match minutes {
        0 => "just now".into(),
        1..60 => format!("{minutes}m ago"),
        60..1440 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

/// A label chip, tinted by the label's color (`Badge variant="label" size="sm"`).
pub(super) fn label_chip(name: &str, color: Option<&str>, more: usize, t: &Tokens) -> impl IntoElement {
    let (bg, fg) = match logic::label_color(color) {
        Some(hex) => {
            let label: Hsla = rgb(hex).into();
            let mix = |share: f32| mix(label, t.foreground, share);
            if t.dark { (Hsla { a: 0.12, ..label }, mix(0.45)) } else { (Hsla { a: 0.08, ..label }, mix(0.3)) }
        }
        None => (t.secondary, t.foreground),
    };
    h_flex()
        .flex_shrink(1.)
        .min_w_0()
        .max_w(px(160.))
        .h(px(16.))
        .px(px(3.))
        .gap(px(4.))
        .rounded(px(4.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(10.))
        .line_height(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .child(div().min_w_0().truncate().child(name.to_owned()))
        .when(more > 0, |el| el.child(div().flex_none().child(format!("+{more}"))))
}

/// `color-mix(in srgb, a share, b)`.
fn mix(a: Hsla, b: Hsla, share: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let lerp = |x: f32, y: f32| x * share + y * (1. - share);
    gpui_kit::Rgba { r: lerp(a.r, b.r), g: lerp(a.g, b.g), b: lerp(a.b, b.b), a: 1. }.into()
}

#[allow(clippy::too_many_arguments)]
fn pull_request_row(
    entry: &Rc<Entry>,
    stat: Option<(u64, u64)>,
    elsewhere: bool,
    first: bool,
    selected: bool,
    meta_width: f32,
    now: i64,
    view: &Entity<Workspace>,
    t: &Tokens,
    window: &mut Window,
    _: &mut App,
) -> AnyElement {
    let key = entry.key();
    let id = |part: &str| format!("pr-{key}-{part}");
    let (state_icon, state_color, state_label) = state_glyph(entry, t);
    let conflict = entry.state == "open" && !entry.is_draft && entry.mergeability == "conflicting";
    let conflict_label = format!("Conflicts with {}", entry.base_branch);
    let url = entry.url.clone();
    let open_label = logic::open_on_host_label(&entry.provider);
    let number = div()
        .id(ElementId::Name(id("number").into()))
        .flex_none()
        .font_family(FONT_MONO)
        .text_xs()
        .text_color(t.muted_foreground)
        .child(format!("#{}", entry.number))
        .context_menu({
            let url = url.clone();
            move |menu, _, _| {
                let (copy, open) = (url.clone(), url.clone());
                menu.item(
                    PopupMenuItem::new("Copy link")
                        .icon(Lucide::Copy)
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))),
                )
                .item(PopupMenuItem::new(open_label).on_click(move |_, _, cx| cx.open_url(&open)))
            }
        });
    let mut signals = h_flex().flex_none().gap(px(4.));
    if let Some((icon, color, label)) = entry.checks_state.as_deref().and_then(|s| checks_glyph(s, t)) {
        signals = signals.child(glyph(id("checks"), icon, 14., color, label));
    }
    if let Some((icon, color, label)) = entry.review_decision.as_deref().and_then(|d| review_glyph(d, t)) {
        signals = signals.child(glyph(id("review"), icon, 14., color, label));
    }
    let stat = stat.filter(|(a, d)| a + d > 0).map(|(additions, deletions)| {
        let addition: Hsla = if t.dark { rgb(0x00d492).into() } else { rgb(0x007a55).into() };
        h_flex()
            .ml_auto()
            .flex_none()
            .gap(px(4.))
            .font_family(FONT_MONO)
            .text_size(px(11.))
            .child(div().text_color(addition).child(format!("+{additions}")))
            .child(div().text_color(t.error).child(format!("-{deletions}")))
    });
    let title_line = h_flex()
        .min_w_0()
        .gap(px(6.))
        .child(number)
        .child(div().min_w_0().truncate().text_sm().text_color(t.foreground).child(entry.title.clone()))
        .child(signals)
        .children(stat);

    let login = entry.author.as_ref().map_or("ghost", |a| a.login.as_str());
    let author_tooltip: SharedString = match entry.author.as_ref().and_then(|a| a.name.as_deref()) {
        Some(name) if name != login => format!("{name} (@{login})").into(),
        _ => login.to_owned().into(),
    };
    let label_slots = if meta_width >= 768. {
        3
    } else if meta_width >= 576. {
        2
    } else {
        1
    };
    let shown_labels = entry.labels.len().min(label_slots);
    let meta_line = h_flex()
        .min_w_0()
        .gap(px(6.))
        .overflow_hidden()
        .text_size(px(11.))
        .text_color(t.muted_foreground)
        .when(elsewhere, |el| {
            el.child(
                h_flex()
                    .id(ElementId::Name(id("elsewhere").into()))
                    .flex_none()
                    .gap(px(4.))
                    .px(px(4.))
                    .rounded_full()
                    .border_1()
                    .border_color(Hsla { a: t.border.a * 0.6, ..t.border })
                    .text_size(px(10.))
                    .child(Icon::new(Lucide::Search).with_size(px(12.)))
                    .when(meta_width >= 320., |el| el.child("matched in the description"))
                    .tooltip(|window, cx| Tooltip::new("Matched in the description").build(window, cx)),
            )
        })
        .child(
            h_flex()
                .id(ElementId::Name(id("author").into()))
                .flex_none()
                .max_w(px(160.))
                .gap(px(4.))
                .child(
                    div()
                        .size(px(14.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(t.muted)
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .child(login.chars().next().unwrap_or('?').to_uppercase().to_string()),
                )
                .when(meta_width >= 320., |el| el.child(div().truncate().child(login.to_owned())))
                .tooltip(move |window, cx| Tooltip::new(author_tooltip.clone()).build(window, cx)),
        )
        .child(div().min_w_0().truncate().child(entry.repository.clone()))
        .when(shown_labels > 0, |el| {
            el.child(h_flex().min_w_0().gap(px(4.)).children(entry.labels.iter().take(shown_labels).enumerate().map(
                |(index, label)| {
                    let more = if index + 1 == shown_labels { entry.labels.len() - shown_labels } else { 0 };
                    label_chip(&label.name, label.color.as_deref(), more, t)
                },
            )))
        })
        .children(
            entry
                .updated_ms
                .map(|updated| div().ml_auto().flex_none().whitespace_nowrap().child(relative_time(now - updated))),
        );

    let picked = entry.clone();
    h_flex()
        .id(ElementId::Name(id("row").into()))
        .when(!first, |el| el.mt(px(2.)))
        .w_full()
        .px(px(12.))
        .py(px(10.))
        .gap(px(8.))
        .rounded(px(6.))
        .cursor_pointer()
        .map(|el| if selected { el.bg(t.accent) } else { el.hover(|s| s.bg(Hsla { a: t.accent.a * 0.6, ..t.accent })) })
        .child(
            div()
                .relative()
                .flex_none()
                .w(px(16.))
                .mt(px(3.))
                .self_start()
                .child(glyph(id("state"), state_icon, 16., state_color, state_label))
                .when(conflict, |el| {
                    el.child(
                        div().absolute().right(px(-4.)).bottom(px(-4.)).rounded_full().bg(t.background).child(glyph(
                            id("conflict"),
                            Lucide::TriangleAlert,
                            12.,
                            t.error,
                            conflict_label,
                        )),
                    )
                }),
        )
        .child(v_flex().flex_1().min_w_0().child(title_line).child(meta_line))
        .on_click(window.listener_for(view, move |this, _, _, cx| {
            this.open_pull_request_panel(reference_of(&picked), Some(picked.clone()), cx)
        }))
        .into_any_element()
}

fn stale_banner(error: SharedString, view: &Entity<Workspace>, t: &Tokens, window: &mut Window) -> AnyElement {
    h_flex()
        .mt(px(16.))
        .justify_between()
        .gap(px(12.))
        .rounded(px(8.))
        .border_1()
        .border_color(Hsla { a: 0.3, ..t.warning })
        .bg(Hsla { a: 0.08, ..t.warning })
        .px(px(12.))
        .py(px(8.))
        .text_xs()
        .text_color(t.foreground)
        .child(div().min_w_0().child(format!("{error} Showing the last pull requests loaded.")))
        .child(
            Button::new("pr-stale-retry")
                .outline()
                .xsmall()
                .label("Retry")
                .on_click(window.listener_for(view, |this, _, _, cx| this.read_pull_requests(false, cx))),
        )
        .into_any_element()
}

fn list_footer(footer: Footer, view: &Entity<Workspace>, t: &Tokens, window: &mut Window) -> AnyElement {
    let row = h_flex().justify_center().py(px(12.)).text_xs().text_color(t.muted_foreground);
    match footer {
        Footer::Loading { continuing } => {
            row.gap(px(8.)).child(if continuing { "Loading more" } else { "Updating pull requests" }).into_any_element()
        }
        Footer::More => row
            .child(
                outline_button("pr-load-more", t)
                    .child("Load more pull requests")
                    .on_click(window.listener_for(view, |this, _, _, cx| this.read_pull_requests(true, cx))),
            )
            .into_any_element(),
        Footer::Narrow => row.child("Narrow your search to find more pull requests.").into_any_element(),
    }
}
