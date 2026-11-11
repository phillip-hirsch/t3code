//! The Filters menu's searchable Author and Labels pickers (`PullRequestAuthorChoices`,
//! `PullRequestLabelChoices` in `PullRequestListFilters.tsx`): the loaded rows' facets, then
//! the host's answers to `pullRequests.filterOptions` as the reader types.
//! The web nests the search box in the submenu; a GPUI menu holds no input, so the submenu's
//! "Search authors…" / "Search labels…" opens this picker under the Filters button.

use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    ScrollHandle, StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, anchored, deferred, div,
    prelude::FluentBuilder as _, px, rgb,
};
use serde_json::{Value, json};

use crate::app::Workspace;
use crate::pull_requests as logic;
use crate::theme::Tokens;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FacetKind {
    Authors,
    Labels,
}

pub(super) struct FacetSearch {
    kind: FacetKind,
    input: Entity<InputState>,
    authors: Vec<String>,
    labels: Vec<(String, Option<String>)>,
    /// The query the host last answered; while it differs from the typed one, its answers wait.
    answered: Option<String>,
    failed: bool,
    /// The row the arrow keys rest on, which Enter picks (the menu's highlighted item).
    highlight: Option<usize>,
    scroll: ScrollHandle,
    _search: Option<Task<()>>,
    _subscription: Subscription,
}

/// The rows the picker shows, in order.
enum Visible {
    Authors(Vec<logic::AuthorFacet>),
    Labels(Vec<logic::LabelFacet>),
}

impl Visible {
    /// How many rows the arrow keys move over: "Anyone" leads the authors.
    fn len(&self) -> usize {
        match self {
            Visible::Authors(facets) => facets.len() + 1,
            Visible::Labels(facets) => facets.len(),
        }
    }
}

/// `MenuRadioGroup`'s arrow keys: they loop past either end.
pub(in crate::app::pages) fn step_highlight(highlight: Option<usize>, len: usize, down: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match (highlight, down) {
        (None, true) => 0,
        (None, false) => len - 1,
        (Some(index), true) => (index + 1) % len,
        (Some(index), false) => (index + len - 1) % len,
    })
}

impl FacetSearch {
    fn typed(&self, cx: &gpui_kit::App) -> String {
        self.input.read(cx).value().trim().chars().take(100).collect()
    }
}

impl Workspace {
    pub(super) fn open_facet_search(&mut self, kind: FacetKind, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = match kind {
            FacetKind::Authors => "Search authors",
            FacetKind::Labels => "Search labels",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => {
                if let Some(search) = this.pull_requests.facet_search.as_mut() {
                    search.highlight = None;
                }
                this.search_facets(true, cx);
            }
            InputEvent::PressEnter { .. } => this.pick_highlighted_facet(cx),
            _ => {}
        });
        let focus = input.clone();
        // The menu that opened this hands focus back as it closes; take it after that.
        cx.defer_in(window, move |_, window, cx| focus.update(cx, |input, cx| input.focus(window, cx)));
        self.pull_requests.facet_search = Some(FacetSearch {
            kind,
            input,
            authors: Vec::new(),
            labels: Vec::new(),
            answered: None,
            failed: false,
            highlight: None,
            scroll: ScrollHandle::new(),
            _search: None,
            _subscription: subscription,
        });
        self.search_facets(false, cx);
        cx.notify();
    }

    fn close_facet_search(&mut self, cx: &mut Context<Self>) {
        if self.pull_requests.facet_search.take().is_some() {
            cx.notify();
        }
    }

    /// Asks the host for options matching the typed text, within the listing's host and project.
    fn search_facets(&mut self, debounce: bool, cx: &mut Context<Self>) {
        let targets = self.pull_request_targets();
        if targets.is_empty() {
            return;
        }
        let (host, project) = (self.pull_requests.host.clone(), self.pull_requests.project.clone());
        let Some(search) = self.pull_requests.facet_search.as_mut() else { return };
        let typed = search.typed(cx);
        let mut input =
            json!({"kind": match search.kind { FacetKind::Authors => "authors", FacetKind::Labels => "labels" }});
        if let Some(host) = host {
            input["host"] = json!(host);
        }
        if let Some(project) = project {
            input["projectId"] = json!(project);
        }
        if !typed.is_empty() {
            input["query"] = json!(typed);
        }
        search._search = Some(cx.spawn(async move |this, cx| {
            if debounce {
                cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            }
            // Each machine answers for its own projects; the options are their union.
            let reads = targets.into_iter().map(|(_, client, project_ids)| {
                let mut input = input.clone();
                if let Some(project_ids) = project_ids {
                    input["projectIds"] = json!(project_ids);
                }
                async move { client.request("pullRequests.filterOptions", input).await }
            });
            let answers: Vec<Value> =
                futures::future::join_all(reads).await.into_iter().filter_map(Result::ok).collect();
            let _ = this.update(cx, |this, cx| {
                let Some(search) = this.pull_requests.facet_search.as_mut() else { return };
                let text = |v: &Value, key: &str| v[key].as_str().map(str::to_owned);
                search.failed = answers.is_empty();
                search.authors.clear();
                search.labels.clear();
                for options in &answers {
                    let list = |key: &str| options[key].as_array().cloned().unwrap_or_default();
                    for login in list("authors").iter().filter_map(|a| text(a, "login")) {
                        if !search.authors.contains(&login) {
                            search.authors.push(login);
                        }
                    }
                    for (name, color) in
                        list("labels").iter().filter_map(|l| Some((text(l, "name")?, text(l, "color"))))
                    {
                        if !search.labels.iter().any(|(held, _)| *held == name) {
                            search.labels.push((name, color));
                        }
                    }
                }
                search.answered = Some(typed);
                cx.notify();
            });
        }));
    }

    /// The rows for what is typed: the picked value first, then the loaded and hosted choices.
    fn visible_facets(&self, search: &FacetSearch, cx: &gpui_kit::App) -> Visible {
        let state = &self.pull_requests;
        let typed = search.typed(cx);
        let pending = search.answered.as_deref() != Some(typed.as_str());
        let (author_facets, label_facets) =
            state.list.as_ref().map(|l| logic::facets(&l.entries, state.state)).unwrap_or_default();
        match search.kind {
            FacetKind::Authors => {
                let hosted: &[String] = if pending { &[] } else { &search.authors };
                let value = state.menu.author.clone();
                let login = value.as_deref().map(str::to_lowercase);
                let selected = value.as_ref().map(|value| {
                    author_facets
                        .iter()
                        .find(|f| f.login.eq_ignore_ascii_case(value))
                        .cloned()
                        .unwrap_or(logic::AuthorFacet { login: value.clone(), count: 0, merged: 0 })
                });
                Visible::Authors(
                    selected
                        .into_iter()
                        .chain(
                            logic::author_choices(&author_facets, hosted, &typed)
                                .into_iter()
                                .filter(|f| Some(f.login.to_lowercase()) != login),
                        )
                        .take(30)
                        .collect(),
                )
            }
            FacetKind::Labels => {
                let hosted: &[(String, Option<String>)] = if pending { &[] } else { &search.labels };
                let value: Vec<String> = state.menu.labels.iter().flatten().cloned().collect();
                let choices = logic::label_choices(&label_facets, hosted, &typed);
                // Picked labels the choices no longer list stay up, so they can be unpicked.
                let mut visible: Vec<logic::LabelFacet> = value
                    .iter()
                    .filter(|name| !choices.iter().any(|c| c.name.eq_ignore_ascii_case(name)))
                    .map(|name| logic::LabelFacet { name: name.clone(), color: None, count: 0 })
                    .collect();
                visible.extend(choices);
                Visible::Labels(visible)
            }
        }
    }

    /// Picks row `index`: an author closes the picker, a label toggles and leaves it open.
    fn pick_facet(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(search) = self.pull_requests.facet_search.as_ref() else { return };
        match self.visible_facets(search, cx) {
            Visible::Authors(_) if index == 0 => {
                self.set_pull_request_scope(cx, |state| state.menu.author = None);
                self.close_facet_search(cx);
            }
            Visible::Authors(facets) => {
                let Some(facet) = facets.into_iter().nth(index - 1) else { return };
                let current = self.pull_requests.menu.author.as_deref().map(str::to_lowercase);
                if current != Some(facet.login.to_lowercase()) {
                    self.set_pull_request_scope(cx, |state| state.menu.author = Some(facet.login));
                }
                self.close_facet_search(cx);
            }
            Visible::Labels(facets) => {
                if let Some(facet) = facets.into_iter().nth(index) {
                    self.toggle_label_filter(facet.name, cx);
                }
            }
        }
    }

    fn pick_highlighted_facet(&mut self, cx: &mut Context<Self>) {
        if let Some(index) = self.pull_requests.facet_search.as_ref().and_then(|s| s.highlight) {
            self.pick_facet(index, cx);
        }
    }

    fn move_facet_highlight(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(search) = self.pull_requests.facet_search.as_ref() else { return };
        let len = self.visible_facets(search, cx).len();
        let Some(search) = self.pull_requests.facet_search.as_mut() else { return };
        search.highlight = step_highlight(search.highlight, len, down);
        if let Some(index) = search.highlight {
            search.scroll.scroll_to_item(index);
        }
        cx.notify();
    }

    fn toggle_label_filter(&mut self, label: String, cx: &mut Context<Self>) {
        self.set_pull_request_scope(cx, |state| {
            let mut picked: Vec<String> = state.menu.labels.iter().flatten().cloned().collect();
            match picked.iter().position(|l| l.eq_ignore_ascii_case(&label)) {
                Some(index) => {
                    picked.remove(index);
                }
                None => picked.push(label),
            }
            state.menu.labels = picked.into_iter().take(10).map(|l| vec![l]).collect();
        });
    }

    /// The open picker, under the Filters button.
    pub(super) fn render_facet_search(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let search = self.pull_requests.facet_search.as_ref()?;
        let state = &self.pull_requests;
        let typed = search.typed(cx);
        let pending = search.answered.as_deref() != Some(typed.as_str());
        let highlight = search.highlight;
        let visible = self.visible_facets(search, cx);
        let row = |id: String, index: usize, checked: bool| {
            h_flex()
                .id(ElementId::Name(id.into()))
                .when(highlight == Some(index), |el| el.bg(t.accent))
                .h(px(28.))
                .px(px(8.))
                .gap(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_sm()
                .hover(|s| s.bg(t.accent))
                .on_click(cx.listener(move |this, _, _, cx| this.pick_facet(index, cx)))
                .child(
                    div()
                        .size(px(14.))
                        .flex_none()
                        .when(checked, |el| el.child(Icon::new(Lucide::Check).size(px(14.)).text_color(t.foreground))),
                )
        };
        let empty =
            |label: &'static str| div().px(px(8.)).py(px(6.)).text_sm().text_color(t.muted_foreground).child(label);
        let mut rows: Vec<AnyElement> = Vec::new();
        match visible {
            Visible::Authors(visible) => {
                let login = state.menu.author.as_deref().map(str::to_lowercase);
                rows.push(
                    row("pr-author-anyone".into(), 0, login.is_none())
                        .child(Icon::new(Lucide::Layers).size(px(14.)).text_color(t.muted_foreground))
                        .child("Anyone")
                        .into_any_element(),
                );
                if visible.is_empty() && !pending {
                    rows.push(
                        empty(if search.failed { "Couldn't search authors on the host" } else { "No authors found" })
                            .into_any_element(),
                    );
                }
                for (index, facet) in visible.into_iter().enumerate() {
                    let checked = Some(facet.login.to_lowercase()) == login;
                    rows.push(
                        row(format!("pr-author-{}", facet.login.to_lowercase()), index + 1, checked)
                            .child(div().flex_1().min_w_0().truncate().child(facet.login.clone()))
                            .when(facet.merged > 0, |el| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(t.muted_foreground)
                                        .child(format!("{} merges loaded", facet.merged)),
                                )
                            })
                            .into_any_element(),
                    );
                }
            }
            Visible::Labels(visible) => {
                let value: Vec<String> = state.menu.labels.iter().flatten().cloned().collect();
                if visible.is_empty() && !pending {
                    rows.push(
                        empty(if search.failed { "Couldn't search labels on the host" } else { "No labels found" })
                            .into_any_element(),
                    );
                }
                for (index, facet) in visible.into_iter().enumerate() {
                    let checked = value.iter().any(|v| v.eq_ignore_ascii_case(&facet.name));
                    let dot =
                        logic::label_color(facet.color.as_deref()).map_or(t.muted_foreground, |hex| rgb(hex).into());
                    rows.push(
                        row(format!("pr-label-{}", facet.name.to_lowercase()), index, checked)
                            .child(div().size(px(10.)).flex_none().rounded_full().bg(dot))
                            .child(div().flex_1().min_w_0().truncate().child(facet.name.clone()))
                            .when(facet.count > 0, |el| {
                                el.child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(t.muted_foreground)
                                        .child(facet.count.to_string()),
                                )
                            })
                            .into_any_element(),
                    );
                }
            }
        }
        let card = v_flex()
            .id("pr-facet-search")
            .w(px(256.))
            .p(px(4.))
            .gap(px(2.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_lg()
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_facet_search(cx)))
            .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.close_facet_search(cx),
                    "down" | "up" => this.move_facet_highlight(event.keystroke.key == "down", cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(
                h_flex()
                    .h(px(32.))
                    .px(px(8.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(t.border)
                    .child(Icon::new(Lucide::Search).size(px(14.)).text_color(t.muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&search.input).appearance(false).px_0().py_0().text_sm()),
                    )
                    .when(pending, |el| el.child(div().text_xs().text_color(t.muted_foreground).child("…"))),
            )
            .child(
                v_flex()
                    .id("pr-facet-search-list")
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .track_scroll(&search.scroll)
                    .gap(px(1.))
                    .children(rows),
            );
        Some(
            div()
                .absolute()
                .top_full()
                .right_0()
                .mt(px(4.))
                .child(
                    deferred(
                        anchored().anchor(gpui_kit::Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(card),
                    )
                    .with_priority(2),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::step_highlight;

    #[test]
    fn the_arrow_keys_loop_over_the_rows() {
        assert_eq!(step_highlight(None, 3, true), Some(0));
        assert_eq!(step_highlight(None, 3, false), Some(2));
        assert_eq!(step_highlight(Some(2), 3, true), Some(0));
        assert_eq!(step_highlight(Some(0), 3, false), Some(2));
        assert_eq!(step_highlight(Some(1), 0, true), None);
    }
}
