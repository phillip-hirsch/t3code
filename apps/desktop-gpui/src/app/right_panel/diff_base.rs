//! The branch diff's comparison target (`DiffPanel.tsx` base ref combobox): Automatic, or a
//! local branch paired with its remote, or a remote-only ref (`buildBaseRefChoices`).

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable as _, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, Window, div, px,
};
use std::collections::HashMap;

use serde_json::{Value, json};

use super::super::Workspace;
use super::super::titlebar::icon;
use crate::theme::Tokens;

/// A `VcsRef` as the picker needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Ref {
    pub name: String,
    pub remote_name: Option<String>,
}

impl Ref {
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            name: value["name"].as_str()?.to_owned(),
            remote_name: value["remoteName"].as_str().map(str::to_owned),
        })
    }

    /// `remoteBranchName`: `origin/main` is `main`.
    fn branch_name(&self) -> &str {
        match &self.remote_name {
            Some(remote) => {
                self.name.strip_prefix(remote.as_str()).and_then(|r| r.strip_prefix('/')).unwrap_or(&self.name)
            }
            None => &self.name,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Choice {
    pub label: String,
    pub local: Option<Ref>,
    pub remote: Option<Ref>,
}

/// `buildBaseRefChoices`: each local branch with its remote (origin first), then the rest.
pub(super) fn choices(local: &[Ref], remote: &[Ref]) -> Vec<Choice> {
    let mut unused: Vec<bool> = vec![true; remote.len()];
    let mut out: Vec<Choice> = local
        .iter()
        .map(|local| {
            let matching: Vec<usize> =
                (0..remote.len()).filter(|i| unused[*i] && remote[*i].branch_name() == local.name).collect();
            let pick = matching
                .iter()
                .copied()
                .find(|i| remote[*i].remote_name.as_deref() == Some("origin"))
                .or(matching.first().copied());
            if let Some(i) = pick {
                unused[i] = false;
            }
            Choice { label: local.name.clone(), local: Some(local.clone()), remote: pick.map(|i| remote[i].clone()) }
        })
        .collect();
    out.extend(remote.iter().zip(&unused).filter(|(_, unused)| **unused).map(|(r, _)| Choice {
        label: r.name.clone(),
        local: None,
        remote: Some(r.clone()),
    }));
    out
}

/// `filterBaseRefChoices`.
pub(super) fn matches(choice: &Choice, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || choice.label.to_lowercase().contains(&query)
        || choice.local.as_ref().is_some_and(|r| r.name.to_lowercase().contains(&query))
        || choice.remote.as_ref().is_some_and(|r| r.name.to_lowercase().contains(&query))
}

/// The ref the "Use remote version" switch picks: the remote one unless that is the one picked.
fn switched(choice: &Choice, selected: Option<&str>) -> Option<String> {
    let (local, remote) = (choice.local.as_ref()?, choice.remote.as_ref()?);
    Some(if selected == Some(remote.name.as_str()) { local.name.clone() } else { remote.name.clone() })
}

/// `valueForBaseRefChoice`: the remote ref when that is the one picked, else the local one.
pub(super) fn value(choice: &Choice, selected: Option<&str>) -> String {
    match (&choice.remote, &choice.local) {
        (Some(remote), _) if selected == Some(remote.name.as_str()) => remote.name.clone(),
        (_, Some(local)) => local.name.clone(),
        (Some(remote), None) => remote.name.clone(),
        (None, None) => choice.label.clone(),
    }
}

/// The combobox's arrow keys over `len` items: past either end the highlight returns to the
/// search box (`loopFocus`), and the next press enters the list again.
fn step_highlight(len: usize, current: Option<usize>, down: bool) -> Option<usize> {
    let last = len.checked_sub(1)?;
    match (current, down) {
        (None, true) => Some(0),
        (None, false) => Some(last),
        (Some(i), true) => (i < last).then_some(i + 1),
        (Some(i), false) => i.checked_sub(1),
    }
}

/// The checkout a ref list answers: its environment and cwd.
type RefsSource = (Option<SharedString>, String);

#[derive(Default)]
pub(in crate::app) struct BasePicker {
    open: bool,
    query: String,
    /// The row the arrow keys reached (`data-highlighted`); Enter picks it.
    highlight: Option<usize>,
    list_scroll: ScrollHandle,
    search: Option<Entity<InputState>>,
    /// The checkout `local` and `remote` came from; another checkout's refs never show.
    source: Option<RefsSource>,
    local: Vec<Ref>,
    remote: Vec<Ref>,
    loading: bool,
    /// Each local-and-remote row's "Use remote version" switch, a Tab stop as the web's `Switch` is.
    switch_focus: HashMap<String, FocusHandle>,
    _task: Option<Task<()>>,
    _search_changes: Option<Subscription>,
}

impl BasePicker {
    /// The refs loaded for `source`'s checkout; none while only another checkout's are loaded.
    fn refs(&self, source: Option<&RefsSource>) -> (&[Ref], &[Ref]) {
        if source.is_some() && self.source.as_ref() == source { (&self.local, &self.remote) } else { (&[], &[]) }
    }
}

impl Workspace {
    /// The checkout the picker lists refs of: the active environment and the git cwd.
    fn base_refs_source(&self) -> Option<RefsSource> {
        Some((self.active_environment().cloned(), self.git_cwd()?))
    }

    /// The base ref picked for the open thread's branch diff (`selectBranchBaseRef`).
    pub(super) fn diff_base_ref(&self) -> Option<&str> {
        self.right_panel.base_refs.get(self.target.as_ref()?).map(String::as_str)
    }

    fn select_base_ref(&mut self, base: Option<String>, cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else { return };
        match base {
            Some(base) => self.right_panel.base_refs.insert(target, base),
            None => self.right_panel.base_refs.remove(&target),
        };
        self.right_panel.base_picker.open = false;
        cx.notify();
    }

    fn open_base_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let search = match self.right_panel.base_picker.search.clone() {
            Some(search) => search,
            None => {
                let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search refs..."));
                self.right_panel.base_picker._search_changes =
                    Some(cx.subscribe_in(&search, window, |this, search, event, _, cx| {
                        if let InputEvent::Change = event {
                            this.right_panel.base_picker.query = search.read(cx).value().trim().to_owned();
                            this.right_panel.base_picker.highlight = None;
                            this.fetch_base_refs(cx);
                        }
                    }));
                self.right_panel.base_picker.search = Some(search.clone());
                search
            }
        };
        search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            search.focus(window, cx);
        });
        self.right_panel.base_picker.query.clear();
        self.right_panel.base_picker.highlight = None;
        self.right_panel.base_picker.open = true;
        self.fetch_base_refs(cx);
        cx.notify();
    }

    /// `vcs.listRefs` twice, local and remote, for the query (the head branch is left out).
    fn fetch_base_refs(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(source)) = (self.client(), self.base_refs_source()) else { return };
        let cwd = source.1.clone();
        let query = self.right_panel.base_picker.query.clone();
        let input = |kind: &str| {
            let mut input = json!({"cwd": cwd, "includeMatchingRemoteRefs": true, "refKind": kind, "limit": 100});
            if !query.is_empty() {
                input["query"] = query.clone().into();
            }
            input
        };
        let (local, remote) = (input("local"), input("remote"));
        self.right_panel.base_picker.loading = true;
        self.right_panel.base_picker._task = Some(cx.spawn(async move |this, cx| {
            let local = client.request("vcs.listRefs", local).await;
            let remote = client.request("vcs.listRefs", remote).await;
            let parse = |result: Result<Value, _>| -> Vec<Ref> {
                result
                    .ok()
                    .and_then(|v| v["refs"].as_array().cloned())
                    .into_iter()
                    .flatten()
                    .filter_map(|r| Ref::parse(&r))
                    .collect()
            };
            let (local, remote) = (parse(local), parse(remote));
            let _ = this.update(cx, |this, cx| {
                let picker = &mut this.right_panel.base_picker;
                let mut focus = std::mem::take(&mut picker.switch_focus);
                picker.switch_focus = choices(&local, &remote)
                    .into_iter()
                    .filter(|choice| choice.local.is_some() && choice.remote.is_some())
                    .map(|choice| {
                        let handle = focus.remove(&choice.label).unwrap_or_else(|| cx.focus_handle().tab_stop(true));
                        (choice.label, handle)
                    })
                    .collect();
                (picker.source, picker.local, picker.remote, picker.loading) = (Some(source), local, remote, false);
                cx.notify();
            });
        }));
    }

    /// "{head} → {base}" with the base as the picker's trigger.
    pub(super) fn render_base_ref_picker(
        &self,
        head: &str,
        base: &str,
        narrow: bool,
        tokens: &Tokens,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let picker = &self.right_panel.base_picker;
        let comparison: SharedString = format!("{head} → {base}").into();
        let trigger = Button::new("diff-base-ref")
            .ghost()
            .xsmall()
            .h(px(24.))
            .px(px(6.))
            .gap_1()
            .min_w(px(36.))
            .max_w(px(192.))
            .text_xs()
            .text_color(tokens.muted_foreground)
            .child(div().min_w_0().truncate().child(SharedString::from(base.to_owned())))
            .child(icon("icons/chevron-down.svg").with_size(px(14.)).opacity(0.7))
            .tooltip(format!("Change comparison target. Currently {base}"));
        let view = cx.entity();
        let mut popover = Popover::new("diff-base-ref-popover")
            .anchor(gpui_kit::Anchor::TopLeft)
            .open(picker.open)
            .on_open_change(move |open, window, cx| {
                view.update(cx, |this, cx| {
                    if *open {
                        this.open_base_picker(window, cx);
                    } else {
                        this.right_panel.base_picker.open = false;
                        cx.notify();
                    }
                });
            })
            .trigger(trigger)
            .p_0()
            .rounded(px(8.));
        if let Some(search) = &picker.search {
            popover = popover.track_focus(&search.focus_handle(cx));
        }
        if picker.open {
            popover = popover.child(self.render_base_ref_list(head, tokens, window, cx));
        }
        // Too narrow for both refs, the target stays readable and the head drops out.
        let lead = if narrow {
            div().id("diff-refs-vs").flex_none().child("vs")
        } else {
            div()
                .id("diff-refs-head")
                .flex()
                .items_center()
                .gap_2()
                .min_w(px(48.))
                .child(div().min_w_0().max_w(px(192.)).truncate().child(SharedString::from(head.to_owned())))
                .child(icon("icons/arrow-right.svg").with_size(px(14.)).flex_none().opacity(0.7))
        };
        h_flex()
            .id("diff-refs")
            .min_w_0()
            .gap_2()
            .overflow_hidden()
            .text_xs()
            .text_color(tokens.muted_foreground)
            .child(lead.tooltip(move |window, cx| Tooltip::new(comparison.clone()).build(window, cx)))
            .child(div().min_w_0().overflow_hidden().child(popover))
            .into_any_element()
    }

    /// The rows the picker lists for `head`, Automatic (`None`) first while nothing is searched.
    fn base_ref_items(&self, head: &str) -> (bool, Vec<Choice>) {
        let picker = &self.right_panel.base_picker;
        let (local, remote) = picker.refs(self.base_refs_source().as_ref());
        let local: Vec<Ref> = local.iter().filter(|r| r.name != head).cloned().collect();
        let shown = choices(&local, remote).into_iter().filter(|c| matches(c, &picker.query)).collect();
        (picker.query.is_empty(), shown)
    }

    /// Up and Down move the highlight from the search box, Enter picks the highlighted row; on a
    /// focused remote switch, Enter and Space flip it instead.
    fn base_picker_key(&mut self, head: &str, event: &KeyDownEvent, window: &Window, cx: &mut Context<Self>) -> bool {
        if event.keystroke.modifiers.modified() {
            return false;
        }
        let (automatic, shown) = self.base_ref_items(head);
        let switch_focus = &self.right_panel.base_picker.switch_focus;
        if let Some(choice) =
            shown.iter().find(|c| switch_focus.get(&c.label).is_some_and(|handle| handle.is_focused(window)))
        {
            if !matches!(event.keystroke.key.as_str(), "enter" | "space") {
                return false;
            }
            let next = switched(choice, self.diff_base_ref());
            self.select_base_ref(next, cx);
            return true;
        }
        let len = usize::from(automatic) + shown.len();
        let picker = &mut self.right_panel.base_picker;
        match event.keystroke.key.as_str() {
            key @ ("down" | "up") => {
                picker.highlight = step_highlight(len, picker.highlight, key == "down");
                if let Some(index) = picker.highlight {
                    picker.list_scroll.scroll_to_item(index);
                }
                cx.notify();
            }
            "enter" => {
                let Some(index) = picker.highlight.filter(|i| *i < len) else { return false };
                let pick =
                    index.checked_sub(usize::from(automatic)).map(|row| value(&shown[row], self.diff_base_ref()));
                self.select_base_ref(pick, cx);
            }
            _ => return false,
        }
        true
    }

    fn render_base_ref_list(&self, head: &str, tokens: &Tokens, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let picker = &self.right_panel.base_picker;
        let (automatic, all) = self.base_ref_items(head);
        let selected = self.diff_base_ref().map(str::to_owned);
        let shown: Vec<&Choice> = all.iter().collect();
        let highlight = picker.highlight;
        let offset = usize::from(automatic);
        // `ComboboxItem`: the picked row keeps a faint fill; hover takes a stronger one.
        let (picked_bg, hover_bg) = (tokens.foreground.opacity(0.06), tokens.foreground.opacity(0.09));
        let row = |id: ElementId, picked: bool, index: usize| {
            h_flex()
                .id(id)
                .when(highlight == Some(index), |el| el.bg(hover_bg))
                .min_h(px(28.))
                .flex_none()
                .px(px(8.))
                .py(px(4.))
                .gap(px(8.))
                .rounded(px(4.))
                .text_sm()
                .cursor_pointer()
                .when(picked, |el| el.bg(picked_bg))
                .hover(move |s| s.bg(hover_bg))
        };
        let automatic = automatic.then(|| {
            row("diff-base-automatic".into(), selected.is_none(), 0)
                .child(div().flex_1().min_w_0().truncate().child("Automatic"))
                .on_click(cx.listener(|this, _, _, cx| this.select_base_ref(None, cx)))
        });
        let rows = shown.iter().enumerate().map(|(index, choice)| {
            let value = value(choice, selected.as_deref());
            let checked = selected.as_deref() == Some(value.as_str());
            let both = switched(choice, selected.as_deref()).zip(picker.switch_focus.get(&choice.label).cloned());
            let use_remote = choice.remote.as_ref().is_some_and(|r| r.name == value);
            let remote_only = choice.local.is_none();
            let pick = value;
            row(ElementId::Name(format!("diff-base-{}", choice.label).into()), checked, index + offset)
                .child(div().flex_1().min_w_0().truncate().child(choice.label.clone()))
                .child(div().w(px(32.)).flex_none().flex().justify_end().map(|el| {
                    match both {
                        Some((next, focus)) => el.child(
                            div()
                                .id(ElementId::Name(format!("diff-base-remote-{}", choice.label).into()))
                                .track_focus(&focus)
                                // The focus ring sits outside the switch, as `ring-offset` does, without moving it.
                                .m(px(-1.))
                                .rounded_full()
                                .border_1()
                                .border_color(if focus.is_focused(window) {
                                    tokens.ring
                                } else {
                                    gpui_kit::transparent_black()
                                })
                                .child(switch(use_remote, tokens))
                                .tooltip(move |window, cx| Tooltip::new("Use remote version").build(window, cx))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.select_base_ref(Some(next.clone()), cx);
                                })),
                        ),
                        None if remote_only => el.child(
                            div()
                                .id(ElementId::Name(format!("diff-base-remote-only-{}", choice.label).into()))
                                .text_color(tokens.muted_foreground)
                                .child(icon("icons/check.svg").with_size(px(12.)))
                                .tooltip(|window, cx| Tooltip::new("Remote only").build(window, cx)),
                        ),
                        None => el,
                    }
                }))
                .on_click(cx.listener(move |this, _, _, cx| this.select_base_ref(Some(pick.clone()), cx)))
        });
        let empty = automatic.is_none() && shown.is_empty() && !picker.loading;
        // `ComboboxSearchInput`: a search glyph and an underline that turns the ring color on focus.
        let focused = picker.search.as_ref().is_some_and(|search| search.focus_handle(cx).is_focused(window));
        let underline = if focused { tokens.ring } else { tokens.border.opacity(0.7) };
        let key_head = head.to_owned();
        v_flex()
            .id("diff-base-ref-body")
            .w(px(288.))
            .bg(tokens.popover)
            .capture_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                if this.base_picker_key(&key_head, event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(div().px(px(12.)).pt(px(10.)).children(picker.search.clone().map(|search| {
                h_flex()
                    .pb(px(6.))
                    .gap(px(4.))
                    .border_b_1()
                    .border_color(underline)
                    .child(
                        icon("icons/search.svg")
                            .with_size(px(16.))
                            .flex_none()
                            .text_color(tokens.muted_foreground.opacity(0.55)),
                    )
                    .child(div().flex_1().min_w_0().child(Input::new(&search).appearance(false).small().text_sm()))
            })))
            .child(
                h_flex()
                    .pl(px(12.))
                    .pr(px(26.))
                    .pt(px(8.))
                    .pb(px(6.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(tokens.border.opacity(0.7))
                    .text_size(px(10.))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(tokens.muted_foreground)
                    .child(div().w(px(16.)))
                    .child(div().flex_1().child("BRANCH"))
                    .child(div().w(px(32.)).flex().justify_end().whitespace_nowrap().child("REMOTE")),
            )
            .child(
                v_flex()
                    .id("diff-base-ref-list")
                    .track_scroll(&picker.list_scroll)
                    .max_h(px(256.))
                    .overflow_y_scrollbar()
                    .p(px(4.))
                    .when(empty, |el| {
                        el.child(
                            div().p(px(8.)).text_sm().text_color(tokens.muted_foreground).child("No matching refs."),
                        )
                    })
                    .children(automatic)
                    .children(rows),
            )
            .into_any_element()
    }
}

/// The small `Switch` the web shows for a branch with a remote.
fn switch(on: bool, t: &Tokens) -> impl IntoElement {
    div()
        .w(px(22.))
        .h(px(12.))
        .p(px(2.))
        .rounded_full()
        .flex()
        .when(on, |el| el.justify_end().bg(t.primary))
        .when(!on, |el| el.bg(t.input))
        .child(div().size(px(8.)).rounded_full().bg(if on { t.primary_foreground } else { t.muted_foreground }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(name: &str) -> Ref {
        Ref { name: name.into(), remote_name: None }
    }

    fn remote(name: &str, remote: &str) -> Ref {
        Ref { name: name.into(), remote_name: Some(remote.into()) }
    }

    #[test]
    fn locals_pair_with_origin_first_then_remote_only_refs_follow() {
        let all = choices(
            &[local("main"), local("feature")],
            &[remote("upstream/main", "upstream"), remote("origin/main", "origin"), remote("origin/old", "origin")],
        );
        assert_eq!(all[0].remote.as_ref().map(|r| r.name.as_str()), Some("origin/main"));
        assert_eq!(all[1].remote, None);
        let rest: Vec<&str> = all[2..].iter().map(|c| c.label.as_str()).collect();
        assert_eq!(rest, ["upstream/main", "origin/old"]);
        assert_eq!(value(&all[0], None), "main");
        assert_eq!(value(&all[0], Some("origin/main")), "origin/main");
        assert_eq!(value(&all[2], None), "upstream/main");
        assert_eq!(switched(&all[0], None).as_deref(), Some("origin/main"));
        assert_eq!(switched(&all[0], Some("origin/main")).as_deref(), Some("main"));
        assert_eq!((switched(&all[1], None), switched(&all[2], None)), (None, None));
        assert!(matches(&all[0], "ORIGIN/m") && !matches(&all[1], "origin"));
    }

    #[test]
    fn arrows_walk_the_refs_and_return_to_the_search_box() {
        assert_eq!(step_highlight(3, None, true), Some(0));
        assert_eq!(step_highlight(3, Some(1), true), Some(2));
        assert_eq!(step_highlight(3, Some(2), true), None);
        assert_eq!(step_highlight(3, None, false), Some(2));
        assert_eq!(step_highlight(3, Some(0), false), None);
        assert_eq!(step_highlight(0, None, true), None);
    }

    #[test]
    fn refs_show_only_for_the_checkout_they_came_from() {
        let source = |env: &str, cwd: &str| (Some(SharedString::from(env.to_owned())), cwd.to_owned());
        let picker = BasePicker {
            source: Some(source("a", "/repo")),
            local: vec![local("main")],
            remote: vec![remote("origin/main", "origin")],
            ..Default::default()
        };
        assert_eq!(picker.refs(Some(&source("a", "/repo"))).0, [local("main")]);
        for other in [Some(source("b", "/repo")), Some(source("a", "/other")), None] {
            assert_eq!(picker.refs(other.as_ref()), (&[][..], &[][..]));
        }
    }
}
