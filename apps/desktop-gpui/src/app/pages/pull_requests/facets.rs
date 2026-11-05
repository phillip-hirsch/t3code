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
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, anchored, deferred, div,
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
    _search: Option<Task<()>>,
    _subscription: Subscription,
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
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.search_facets(true, cx);
            }
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
        let Some(client) = self.client() else { return };
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
            let answer = client.request("pullRequests.filterOptions", input).await;
            let _ = this.update(cx, |this, cx| {
                let Some(search) = this.pull_requests.facet_search.as_mut() else { return };
                let text = |v: &Value, key: &str| v[key].as_str().map(str::to_owned);
                match answer {
                    Ok(options) => {
                        let list = |key: &str| options[key].as_array().cloned().unwrap_or_default();
                        search.authors = list("authors").iter().filter_map(|a| text(a, "login")).collect();
                        search.labels =
                            list("labels").iter().filter_map(|l| Some((text(l, "name")?, text(l, "color")))).collect();
                        search.failed = false;
                    }
                    Err(_) => search.failed = true,
                }
                search.answered = Some(typed);
                cx.notify();
            });
        }));
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
        let (author_facets, label_facets) =
            state.list.as_ref().map(|l| logic::facets(&l.entries, state.state)).unwrap_or_default();
        let row = |id: String, checked: bool| {
            h_flex()
                .id(ElementId::Name(id.into()))
                .h(px(28.))
                .px(px(8.))
                .gap(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_sm()
                .hover(|s| s.bg(t.accent))
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
                let visible: Vec<logic::AuthorFacet> = selected
                    .into_iter()
                    .chain(
                        logic::author_choices(&author_facets, hosted, &typed)
                            .into_iter()
                            .filter(|f| Some(f.login.to_lowercase()) != login),
                    )
                    .take(30)
                    .collect();
                rows.push(
                    row("pr-author-anyone".into(), value.is_none())
                        .child(Icon::new(Lucide::Layers).size(px(14.)).text_color(t.muted_foreground))
                        .child("Anyone")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_pull_request_scope(cx, |state| state.menu.author = None);
                            this.close_facet_search(cx);
                        }))
                        .into_any_element(),
                );
                if visible.is_empty() && !pending {
                    rows.push(
                        empty(if search.failed { "Couldn't search authors on the host" } else { "No authors found" })
                            .into_any_element(),
                    );
                }
                for facet in visible {
                    let checked = Some(facet.login.to_lowercase()) == login;
                    let picked = facet.login.clone();
                    rows.push(
                        row(format!("pr-author-{}", facet.login.to_lowercase()), checked)
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
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let author = picked.clone();
                                if !checked {
                                    this.set_pull_request_scope(cx, |state| state.menu.author = Some(author));
                                }
                                this.close_facet_search(cx);
                            }))
                            .into_any_element(),
                    );
                }
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
                if visible.is_empty() && !pending {
                    rows.push(
                        empty(if search.failed { "Couldn't search labels on the host" } else { "No labels found" })
                            .into_any_element(),
                    );
                }
                for facet in visible {
                    let checked = value.iter().any(|v| v.eq_ignore_ascii_case(&facet.name));
                    let dot =
                        logic::label_color(facet.color.as_deref()).map_or(t.muted_foreground, |hex| rgb(hex).into());
                    let picked = facet.name.clone();
                    rows.push(
                        row(format!("pr-label-{}", facet.name.to_lowercase()), checked)
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
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_label_filter(picked.clone(), cx)))
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
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_facet_search(cx);
                }
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
            .child(v_flex().id("pr-facet-search-list").max_h(px(320.)).overflow_y_scroll().gap(px(1.)).children(rows));
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
