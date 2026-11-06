//! The sidebar's header buttons: the project filter combobox (`Sidebar.tsx`
//! project scope), Add project, and New thread (`SidebarThreadHeader.tsx`).

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, KeyDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, anchored,
    deferred, div, prelude::FluentBuilder as _, px,
};
use serde_json::json;

use super::{icon, project_icon, tooltip};
use crate::app::palette::View;
use crate::app::{Workspace, new_id};
use crate::inbox;
use crate::project_groups::{self, ProjectGroup};
use crate::theme::Tokens;

/// The open filter popup.
pub(super) struct FilterMenu {
    query: Entity<InputState>,
    highlight: usize,
    _subscription: Subscription,
}

impl Workspace {
    /// The project groups the filter lists: "All projects" (`None`) while the query
    /// is empty, then the pickable groups whose name contains it.
    fn filter_rows<'g>(groups: &'g [ProjectGroup], query: &str) -> Vec<Option<&'g ProjectGroup>> {
        let query = query.trim().to_lowercase();
        let mut rows: Vec<Option<&ProjectGroup>> = if query.is_empty() { vec![None] } else { Vec::new() };
        rows.extend(
            project_groups::pickable(groups)
                .into_iter()
                .filter(|g| query.is_empty() || g.display_name.to_lowercase().contains(&query))
                .map(Some),
        );
        rows
    }

    /// Open project groups in view: the scoped ones, else all of them.
    fn groups_in_view(&self) -> usize {
        let groups = self.project_groups();
        match super::scoped_groups(&groups, &self.sidebar.scope).len() {
            0 => project_groups::pickable(&groups).len(),
            scoped => scoped,
        }
    }

    fn open_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects..."));
        let subscription = cx.subscribe(&query, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                if let Some(filter) = &mut this.sidebar.filter {
                    filter.highlight = 0;
                }
                this.notify_sidebar(cx);
            }
        });
        query.update(cx, |query, cx| query.focus(window, cx));
        self.sidebar.filter = Some(FilterMenu { query, highlight: 0, _subscription: subscription });
        self.notify_sidebar(cx);
    }

    fn close_filter(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.filter.take().is_some() {
            self.sidebar.filter_closed_at = Some(Instant::now());
            self.notify_sidebar(cx);
        }
    }

    /// A plain pick replaces the scope and closes; Shift toggles and stays open.
    fn pick_scope(&mut self, group: Option<String>, toggle: bool, cx: &mut Context<Self>) {
        match (group, toggle) {
            (None, _) => self.set_scope(Vec::new(), cx),
            (Some(id), false) => self.set_scope(vec![id], cx),
            (Some(id), true) => {
                let mut scope = self.sidebar.scope.clone();
                match scope.iter().position(|s| *s == id) {
                    Some(at) => {
                        scope.remove(at);
                    }
                    None => scope.push(id),
                }
                self.set_scope(scope, cx);
                return;
            }
        }
        self.close_filter(cx);
    }

    fn filter_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(filter) = &self.sidebar.filter else { return };
        let query = filter.query.read(cx).value().to_string();
        let groups = self.project_groups();
        let rows: Vec<Option<String>> =
            Self::filter_rows(&groups, &query).into_iter().map(|g| g.map(|g| g.key.clone())).collect();
        let highlight = filter.highlight;
        match event.keystroke.key.as_str() {
            "escape" => self.close_filter(cx),
            "down" | "up" if !rows.is_empty() => {
                let last = rows.len() - 1;
                let next = match event.keystroke.key.as_str() {
                    "down" => {
                        if highlight >= last {
                            0
                        } else {
                            highlight + 1
                        }
                    }
                    _ => {
                        if highlight == 0 {
                            last
                        } else {
                            highlight - 1
                        }
                    }
                };
                if let Some(filter) = &mut self.sidebar.filter {
                    filter.highlight = next;
                }
                self.notify_sidebar(cx);
            }
            "enter" => {
                if let Some(row) = rows.get(highlight).cloned() {
                    self.pick_scope(row, event.keystroke.modifiers.shift, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// `setProjectGroupPinned`: `projects.mutate project.update {pinned}` for every member.
    pub(in crate::app) fn set_projects_pinned(&mut self, project_ids: &[String], pinned: bool, cx: &mut Context<Self>) {
        for project_id in project_ids {
            let mutation =
                json!({"type": "project.update", "commandId": new_id(), "projectId": project_id, "pinned": pinned});
            self.mutate_project(mutation, cx);
        }
    }

    /// `chat.new` and the New thread button: a draft in the current project when
    /// `in_project` (Shift, `chat.newLocal`) or only one project is in view,
    /// else "New thread in…" in the palette.
    pub(in crate::app) fn new_thread(&mut self, in_project: bool, window: &mut Window, cx: &mut Context<Self>) {
        if in_project || self.groups_in_view() <= 1 {
            let groups = self.project_groups();
            let scoped =
                super::scoped_groups(&groups, &self.sidebar.scope).first().map(|g| g.representative().id.clone());
            let project = self.new_thread_project().or(scoped).or_else(|| {
                inbox::most_recent_project(&self.shell.projects, &self.shell.threads).map(|p| p.id.clone())
            });
            if let Some(project_id) = project {
                self.start_draft(project_id, window, cx);
                return;
            }
        }
        self.open_palette(View::NewThreadIn, window, cx);
    }

    pub(super) fn render_header_buttons(&self, tokens: &Tokens, cx: &mut Context<Self>) -> impl IntoElement {
        let sb = tokens.sidebar;
        let header_button = |id: &'static str| {
            div()
                .id(id)
                .relative()
                .size(px(28.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(sb.row_hover))
        };
        let has_projects = !self.shell.projects.is_empty();
        let show_hint = self.groups_in_view() > 1;
        let new_thread_label = self.with_shortcut("New thread", "chat.new");
        let in_project_hint = format!(
            "New thread in current project: Shift+click{}",
            self.shortcut_label("chat.newLocal").map(|s| format!(" ({s})")).unwrap_or_default()
        );
        let muted = tokens.muted_foreground;
        h_flex()
            .flex_none()
            .when(has_projects, |el| {
                el.child(self.render_filter(tokens, cx)).child(
                    header_button("sidebar-add-project")
                        .tooltip(tooltip("Add project"))
                        .on_click(cx.listener(|this, _, window, cx| this.open_palette(View::AddProject, window, cx)))
                        .child(icon(IconName::FolderPlus, 16., sb.icon)),
                )
            })
            .child(
                header_button("sidebar-new-thread")
                    .when(!has_projects, |el| el.opacity(0.64).cursor_default())
                    .tooltip(move |window, cx| {
                        let (label, hint) = (new_thread_label.clone(), in_project_hint.clone());
                        gpui_kit::component::tooltip::Tooltip::element(move |_, _| {
                            v_flex()
                                .gap(px(2.))
                                .child(label.clone())
                                .when(show_hint, |el| el.child(div().text_color(muted).child(hint.clone())))
                        })
                        .build(window, cx)
                    })
                    .when(has_projects, |el| {
                        el.on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                            this.new_thread(event.modifiers().shift, window, cx);
                        }))
                    })
                    .child(icon(IconName::SquarePen, 16., sb.icon)),
            )
    }

    /// The `ListFilter` trigger (or the scoped projects' icons) and its popup.
    fn render_filter(&self, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let sb = tokens.sidebar;
        let groups = self.project_groups();
        let scoped_groups = super::scoped_groups(&groups, &self.sidebar.scope);
        let scoped: Vec<_> = scoped_groups.iter().map(|g| g.representative()).collect();
        let label: SharedString = if scoped.is_empty() {
            "Filter threads by project".into()
        } else {
            let names: Vec<&str> = scoped_groups.iter().map(|g| g.display_name.as_str()).collect();
            format!("Filter threads by project: {}", names.join(", ")).into()
        };
        let trigger_icon: AnyElement = match scoped.as_slice() {
            [] => icon(IconName::ListFilter, 16., sb.icon).into_any_element(),
            [one] => project_icon(one, tokens, false, 16., self.favicon(one)),
            [first, second, ..] => h_flex()
                .child(project_icon(first, tokens, false, 14., self.favicon(first)))
                .child(div().ml(px(-6.)).child(project_icon(second, tokens, false, 14., self.favicon(second))))
                .into_any_element(),
        };
        let open = self.sidebar.filter.is_some();
        div()
            .id("sidebar-filter")
            .relative()
            .size(px(28.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|s| s.bg(sb.row_hover))
            .when(open, |el| el.bg(sb.row_active))
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(label.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| {
                // The press that closed the popup by clicking outside must not reopen it.
                let just_closed =
                    this.sidebar.filter_closed_at.is_some_and(|at| at.elapsed() < Duration::from_millis(300));
                if this.sidebar.filter.is_some() {
                    this.close_filter(cx);
                } else if !just_closed {
                    this.open_filter(window, cx);
                }
            }))
            .child(trigger_icon)
            .children(self.render_filter_popup(tokens, cx))
            .into_any_element()
    }

    fn render_filter_popup(&self, tokens: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let filter = self.sidebar.filter.as_ref()?;
        let query = filter.query.read(cx).value().to_string();
        let groups = self.project_groups();
        let rows = Self::filter_rows(&groups, &query);
        let scope_shown = !super::scoped_groups(&groups, &self.sidebar.scope).is_empty();
        let fg = tokens.foreground;
        let list = v_flex().px(px(4.)).py(px(4.)).children(rows.iter().enumerate().map(|(i, group)| {
            let highlighted = i == filter.highlight;
            let selected = match group {
                None => !scope_shown,
                Some(g) => self.sidebar.scope.contains(&g.key),
            };
            let row_icon: AnyElement = match group {
                None => icon(IconName::Folder, 16., tokens.muted_foreground).into_any_element(),
                Some(g) => project_icon(g.representative(), tokens, false, 16., self.favicon(g.representative())),
            };
            let badge = selected.then(|| {
                div()
                    .absolute()
                    .right(px(-4.))
                    .bottom(px(-4.))
                    .size(px(12.))
                    .rounded_full()
                    .border_2()
                    .border_color(tokens.popover)
                    .bg(tokens.primary)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(IconName::Check, 8., tokens.primary_foreground))
            });
            let pick = group.map(|g| g.key.clone());
            let small_button = |id: SharedString| {
                div()
                    .id(id)
                    .size(px(24.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(tokens.muted_foreground)
                    .hover(|s| s.bg(fg.opacity(0.06)).text_color(fg))
            };
            let controls = group.map(|g| {
                let pinned = g.pinned;
                let members: Vec<String> = g.members.iter().map(|m| m.project.id.clone()).collect();
                let name = &g.display_name;
                let pin_title: SharedString = format!("{} {name}", if pinned { "Unpin" } else { "Pin" }).into();
                let gear_title: SharedString = format!("Project settings for {name}").into();
                h_flex()
                    .ml_auto()
                    .child(
                        small_button(format!("filter-pin-{}", g.key).into())
                            .when(!pinned && !highlighted, gpui_kit::Styled::invisible)
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(pin_title.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.set_projects_pinned(&members, !pinned, cx);
                            }))
                            .child(
                                gpui_kit::component::Icon::new(if pinned { IconName::PinOff } else { IconName::Pin })
                                    .size(px(14.)),
                            ),
                    )
                    .child(
                        small_button(format!("filter-gear-{}", g.key).into())
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(gear_title.clone()).build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_filter(cx);
                                this.open_settings("projects", cx);
                            }))
                            .child(gpui_kit::component::Icon::new(IconName::Settings).size(px(14.))),
                    )
            });
            h_flex()
                .id(SharedString::from(format!("filter-row-{}", group.map_or("all", |g| g.key.as_str()))))
                .min_h(px(28.))
                .px(px(8.))
                .py(px(4.))
                .gap(px(8.))
                .rounded(px(4.))
                .text_sm()
                .line_height(px(20.))
                .cursor_pointer()
                .when(highlighted, |el| el.bg(fg.opacity(0.09)))
                .hover(|s| s.bg(fg.opacity(0.09)))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if let Some(filter) = this.sidebar.filter.as_mut().filter(|f| *hovered && f.highlight != i) {
                        filter.highlight = i;
                        this.notify_sidebar(cx);
                    }
                }))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.pick_scope(pick.clone(), event.modifiers().shift, cx);
                }))
                .child(div().relative().flex_none().size(px(16.)).child(row_icon).children(badge))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(group.map_or(SharedString::from("All projects"), |g| g.display_name.clone().into())),
                )
                .children(controls)
        }));
        let popup = v_flex()
            .id("sidebar-filter-popup")
            .occlude()
            .w(px(288.))
            .max_h(px(368.))
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(fg.opacity(0.1))
            .bg(tokens.popover)
            .shadow_xl()
            .cursor_default()
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_filter(cx)))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.filter_key(event, cx)))
            .child(
                div().px(px(12.)).pt(px(10.)).child(
                    h_flex()
                        .relative()
                        .pb(px(6.))
                        .border_b_1()
                        .border_color(tokens.ring)
                        .child(div().absolute().left_0().top(px(5.)).child(icon(
                            IconName::Search,
                            16.,
                            tokens.muted_foreground.opacity(0.55),
                        )))
                        .child(
                            div()
                                .flex_1()
                                .pl(px(20.))
                                .child(Input::new(&filter.query).appearance(false).h(px(26.)).px_0().text_sm()),
                        ),
                ),
            )
            .child(div().id("sidebar-filter-list").flex_1().min_h_0().overflow_y_scroll().child(list).when(
                rows.is_empty(),
                |el| {
                    el.child(
                        div()
                            .p(px(8.))
                            .text_center()
                            .text_sm()
                            .text_color(tokens.muted_foreground)
                            .child("No matching projects."),
                    )
                },
            ));
        Some(
            div()
                .absolute()
                .top_full()
                .left_0()
                .mt(px(4.))
                .child(deferred(anchored().snap_to_window_with_margin(px(8.)).child(popup)).with_priority(3))
                .into_any_element(),
        )
    }
}
