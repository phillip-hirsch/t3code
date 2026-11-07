//! `DraftHeroHeadline`'s project picker: a combobox under the draft's project name that
//! searches the projects, pins them, and adds one.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, FontWeight, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, anchored, deferred, div, point, prelude::FluentBuilder as _, px, relative, svg,
};

use super::Workspace;
use super::palette::View;
use crate::theme::Tokens;

/// `ComboboxPopup className="w-72"`.
const POPUP_WIDTH: f32 = 288.;

pub(super) struct DraftPicker {
    open: Option<Open>,
    /// When an outside press last closed it, so that press on the trigger doesn't reopen it.
    closed_at: Option<Instant>,
    /// The project name that opens it, where Escape and a pick return focus.
    pub(super) trigger: FocusHandle,
}

impl DraftPicker {
    pub(super) fn new(cx: &mut App) -> Self {
        Self { open: None, closed_at: None, trigger: cx.focus_handle().tab_stop(true) }
    }
}

struct Open {
    query: Entity<InputState>,
    highlight: Option<usize>,
    /// `useOrderWhileOpen`: the group keys as the picker opened, so a pin doesn't move rows.
    order: Vec<String>,
    scroll: ScrollHandle,
    _subscription: Subscription,
}

impl Open {
    /// Highlights row `index` and scrolls the list to reveal it.
    fn highlight(&mut self, index: usize) {
        self.highlight = Some(index);
        self.scroll.scroll_to_item(index);
    }

    /// Moves the highlight one of `len` rows down or up, wrapping at the ends.
    fn step(&mut self, len: usize, down: bool) {
        let Some(last) = len.checked_sub(1) else { return };
        self.highlight(match (down, self.highlight) {
            (true, Some(i)) if i < last => i + 1,
            (true, _) => 0,
            (false, Some(i)) if i > 0 => i - 1,
            (false, _) => last,
        });
    }
}

/// A picker row.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    /// A project group, by key.
    Project(String),
    AddProject,
}

/// The rows for `groups` (`(key, name)` in picker order): the projects in the order they had
/// when the picker opened, then Add project; a query keeps the projects whose name contains it.
fn rows(groups: &[(String, String)], order: &[String], query: &str) -> Vec<Entry> {
    let mut projects: Vec<&(String, String)> = groups.iter().collect();
    projects.sort_by_key(|(key, _)| order.iter().position(|k| k == key).unwrap_or(usize::MAX));
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        return projects
            .into_iter()
            .filter(|(_, name)| name.to_lowercase().contains(&query))
            .map(|(key, _)| Entry::Project(key.clone()))
            .collect();
    }
    projects.into_iter().map(|(key, _)| Entry::Project(key.clone())).chain([Entry::AddProject]).collect()
}

impl Workspace {
    /// `menuEntries`: the pickable groups, the current project leading the unpinned ones,
    /// without the No project home, which has its own link.
    fn draft_picker_groups(&self) -> Vec<crate::project_groups::ProjectGroup> {
        let groups = self.project_groups();
        let current = self.current_project().map(|p| p.id.clone());
        crate::project_groups::picker(&groups, current.as_deref())
            .into_iter()
            .filter(|g| !g.members[0].is_scratch)
            .cloned()
            .collect()
    }

    fn draft_picker_rows(&self, cx: &App) -> Vec<Entry> {
        let Some(open) = &self.draft_picker.open else { return Vec::new() };
        let groups: Vec<(String, String)> =
            self.draft_picker_groups().into_iter().map(|g| (g.key, g.display_name)).collect();
        rows(&groups, &open.order, &open.query.read(cx).value())
    }

    /// `ComboboxTrigger`'s arrow keys: open the picker and move its highlight a row.
    pub(super) fn open_draft_picker_from_key(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft_picker.open.is_none() {
            self.toggle_draft_picker(window, cx);
        }
        let len = self.draft_picker_rows(cx).len();
        if let Some(open) = &mut self.draft_picker.open {
            open.step(len, down);
            cx.notify();
        }
    }

    pub(super) fn draft_picker_open(&self) -> bool {
        self.draft_picker.open.is_some()
    }

    pub(super) fn toggle_draft_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.draft_picker.open.is_some() {
            self.close_draft_picker(window, cx);
            return;
        }
        if self.draft_picker.closed_at.is_some_and(|at| at.elapsed() < Duration::from_millis(300)) {
            return;
        }
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects..."));
        let subscription = cx.subscribe_in(&query, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                // `autoHighlight`: typing highlights the first match.
                if let Some(open) = &mut this.draft_picker.open {
                    open.highlight(0);
                }
                cx.notify();
            }
            // Focus left the combobox (Tab, a click elsewhere): it closes and the focus stays there.
            // Switching to another window leaves it open, as in the browser.
            InputEvent::Blur if window.is_window_active() => {
                this.dismiss_draft_picker(cx);
            }
            _ => {}
        });
        query.update(cx, |query, cx| query.focus(window, cx));
        let groups = self.draft_picker_groups();
        let current = self.current_project().map(|p| p.id.clone());
        // The selected project starts highlighted, as Base UI opens a combobox on its value.
        let highlight = current.and_then(|id| groups.iter().position(|g| g.contains(&id))).unwrap_or(0);
        let order = groups.into_iter().map(|g| g.key).collect();
        let mut open = Open { query, highlight: None, order, scroll: ScrollHandle::new(), _subscription: subscription };
        open.highlight(highlight);
        self.draft_picker.open = Some(open);
        cx.notify();
    }

    /// Closes the picker and returns focus to its trigger, as a combobox does.
    fn close_draft_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dismiss_draft_picker(cx) {
            window.focus(&self.draft_picker.trigger, cx);
        }
    }

    /// Closes the picker without moving focus; whether it was open.
    fn dismiss_draft_picker(&mut self, cx: &mut Context<Self>) -> bool {
        if self.draft_picker.open.take().is_none() {
            return false;
        }
        self.draft_picker.closed_at = Some(Instant::now());
        cx.notify();
        true
    }

    fn pick_draft_entry(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        self.close_draft_picker(window, cx);
        let key = match entry {
            Entry::AddProject => return self.open_palette(View::AddProject, window, cx),
            Entry::Project(key) => key,
        };
        let current = self.current_project().map(|p| (p.id.clone(), p.environment_id.clone()));
        let Some(group) = self.draft_picker_groups().into_iter().find(|g| g.key == key) else { return };
        if current.as_ref().is_some_and(|(id, _)| group.contains(id)) {
            return;
        }
        let target = group.target(current.as_ref().map(|(_, env)| env.as_ref()));
        self.select_draft_project(target.id.clone(), window, cx);
    }

    /// `selectProject`: a draft with content moves to the project; an empty one opens the
    /// project's draft, since retargeting it would overwrite that one.
    fn select_draft_project(&mut self, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let composed = !self.composer.read(cx).value().trim().is_empty() || !self.attachments.is_empty();
        match self.project_environment(&project_id) {
            Some(env) if composed && matches!(self.target, Some(super::Target::Draft { .. })) => {
                self.move_draft_to_project(env, project_id, window, cx);
            }
            _ => self.start_draft(project_id, window, cx),
        }
    }

    fn toggle_draft_entry_pin(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(group) = self.draft_picker_groups().into_iter().find(|g| g.key == key) {
            self.set_projects_pinned(&group.project_ids, !group.pinned, cx);
        }
    }

    fn draft_picker_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.draft_picker_rows(cx);
        let pin = self.is_pin_shortcut(&event.keystroke);
        let Some(open) = &mut self.draft_picker.open else { return };
        let highlight = open.highlight;
        if pin {
            if let Some(Entry::Project(key)) = highlight.and_then(|i| rows.get(i)) {
                self.toggle_draft_entry_pin(key, cx);
            }
            cx.stop_propagation();
            return;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.close_draft_picker(window, cx),
            key @ ("down" | "up") => {
                open.step(rows.len(), key == "down");
                cx.notify();
            }
            "enter" => {
                if let Some(entry) = highlight.and_then(|i| rows.get(i)).cloned() {
                    self.pick_draft_entry(entry, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The open popup, centered under the trigger it is a child of (`ComboboxPopup align="center"`).
    pub(super) fn render_draft_picker(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.draft_picker.open.as_ref()?;
        let fg = t.foreground;
        let muted = t.muted_foreground;
        let groups = self.draft_picker_groups();
        let current = self.current_project().map(|p| p.id.clone());
        let rows = self.draft_picker_rows(cx);
        let pin_shortcut = self.shortcut_label("thread.pin");
        let show_environments = crate::project_groups::span_environments(&self.project_groups());
        let primary = self.primary_environment().map(ToString::to_string);
        let list = rows.iter().enumerate().map(|(i, entry)| {
            let highlighted = open.highlight == Some(i);
            let group = match entry {
                Entry::Project(key) => groups.iter().find(|g| &g.key == key),
                Entry::AddProject => None,
            };
            let selected = group.is_some_and(|g| current.as_ref().is_some_and(|id| g.contains(id)));
            let row_icon: AnyElement = match group {
                Some(g) => {
                    let project = g.representative();
                    super::sidebar::project_icon(project, t, false, 16., self.favicon(project))
                }
                None => Icon::new(IconName::FolderPlus).size(px(16.)).text_color(muted).into_any_element(),
            };
            let name: SharedString = group.map_or("Add project".into(), |g| g.display_name.clone().into());
            let tip = name.clone();
            // `ProjectPinButton`: a pinned project always shows its pin, which turns into PinOff
            // while hovered; an unpinned one shows it on the highlighted row and keeps its room.
            let pin = group.map(|g| {
                let (pinned, key) = (g.pinned, g.key.clone());
                let action = format!("{} {}", if pinned { "Unpin" } else { "Pin" }, g.display_name);
                let title: SharedString = match &pin_shortcut {
                    Some(shortcut) => format!("{action} ({shortcut})").into(),
                    None => action.into(),
                };
                let pin_group: SharedString = format!("draft-picker-pin-{key}").into();
                div()
                    .id(pin_group.clone())
                    .group(pin_group.clone())
                    .relative()
                    .ml_auto()
                    .size(px(24.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(!pinned && !highlighted, gpui_kit::Styled::invisible)
                    .text_color(muted)
                    .hover(|s| s.bg(t.accent).text_color(fg))
                    .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                    .child(
                        div()
                            .when(pinned, |el| el.group_hover(pin_group.clone(), gpui_kit::Styled::invisible))
                            .child(Icon::new(IconName::Pin).size(px(14.))),
                    )
                    .when(pinned, |el| {
                        el.child(
                            div()
                                .absolute()
                                .invisible()
                                .group_hover(pin_group, gpui_kit::Styled::visible)
                                .child(Icon::new(IconName::PinOff).size(px(14.))),
                        )
                    })
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_draft_entry_pin(&key, cx);
                    }))
            });
            let badge = group.filter(|_| show_environments).and_then(|g| {
                let (description, environment) = g.environment_badge(primary.as_deref(), |env| {
                    self.env_info(env).and_then(|info| info.environment_label.clone())
                })?;
                let machine = self.env_info(&environment).map_or("server", |info| info.machine_kind());
                let description = SharedString::from(description);
                Some(
                    div()
                        .id(SharedString::from(format!("draft-picker-env-{}", g.key)))
                        .flex_none()
                        .flex()
                        .items_center()
                        .child(
                            svg().path(super::pages::settings::machine_icon(machine)).size(px(14.)).text_color(muted),
                        )
                        .tooltip(move |window, cx| Tooltip::new(description.clone()).build(window, cx)),
                )
            });
            let pick = entry.clone();
            let row_id: SharedString = match entry {
                Entry::Project(key) => format!("draft-picker-row-{key}").into(),
                Entry::AddProject => "draft-picker-add-project".into(),
            };
            h_flex()
                .id(row_id.clone())
                .flex_none()
                .min_h(px(28.))
                .px(px(8.))
                .py(px(4.))
                .gap(px(8.))
                .rounded(px(4.))
                .cursor_pointer()
                .when(selected, |el| el.bg(fg.opacity(0.06)))
                .when(highlighted, |el| el.bg(fg.opacity(0.09)))
                .hover(|s| s.bg(fg.opacity(0.09)))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if let Some(open) = this.draft_picker.open.as_mut().filter(|o| *hovered && o.highlight != Some(i)) {
                        open.highlight = Some(i);
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| this.pick_draft_entry(pick.clone(), window, cx)))
                .child(div().flex_none().size(px(16.)).child(row_icon))
                .child(
                    div()
                        .id(SharedString::from(format!("{row_id}-name")))
                        .flex_1()
                        .min_w_0()
                        .line_clamp(2)
                        .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                        .child(name),
                )
                .children(pin)
                .children(badge)
        });
        let popup = v_flex()
            .id("draft-project-picker-popup")
            .key_context(super::keymap::COMBOBOX_CONTEXT)
            .occlude()
            .w(px(POPUP_WIDTH))
            .max_h(px(368.))
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(fg.opacity(0.1))
            .bg(t.popover)
            .shadow_xl()
            .cursor_default()
            .text_left()
            .text_sm()
            .line_height(px(20.))
            .font_weight(FontWeight::NORMAL)
            .text_color(fg)
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.dismiss_draft_picker(cx);
            }))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.draft_picker_key(event, window, cx);
            }))
            // `ComboboxSearchInput`: a search glyph and an underline in the ring color while focused.
            .child(
                div().px(px(12.)).pt(px(10.)).child(
                    h_flex()
                        .relative()
                        .pb(px(6.))
                        .border_b_1()
                        .border_color(t.ring)
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top(px(5.))
                                .child(Icon::new(IconName::Search).size(px(16.)).text_color(muted.opacity(0.55))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .h(px(26.))
                                .flex()
                                .items_center()
                                .pl(px(20.))
                                .child(Input::new(&open.query).appearance(false).small().px_0().text_sm()),
                        ),
                ),
            )
            // The rows are the list's direct children, so the scroll handle can reveal one by index.
            .child(
                v_flex()
                    .id("draft-project-picker-list")
                    .track_scroll(&open.scroll)
                    .flex_1()
                    .min_h_0()
                    .p(px(4.))
                    .overflow_y_scroll()
                    .children(list)
                    .when(rows.is_empty(), |el| {
                        el.child(div().p(px(8.)).text_center().text_color(muted).child("No matching projects."))
                    }),
            );
        Some(
            div()
                .absolute()
                .top_full()
                .left(relative(0.5))
                .mt(px(4.))
                .child(
                    deferred(
                        anchored()
                            .offset(point(px(-POPUP_WIDTH / 2.), px(0.)))
                            .snap_to_window_with_margin(px(8.))
                            .child(popup),
                    )
                    .with_priority(3),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(names: &[&str]) -> Vec<(String, String)> {
        names.iter().map(|n| (format!("key-{n}"), (*n).to_owned())).collect()
    }

    fn keys(rows: &[Entry]) -> Vec<&str> {
        rows.iter()
            .map(|row| match row {
                Entry::Project(key) => key.as_str(),
                Entry::AddProject => "add",
            })
            .collect()
    }

    #[test]
    fn rows_keep_their_opening_order_and_end_with_add_project() {
        let order: Vec<String> = ["key-a", "key-b", "key-c"].map(String::from).to_vec();
        // Pinning c moved it to the front of the groups; the open picker keeps its place.
        assert_eq!(keys(&rows(&groups(&["c", "a", "b"]), &order, "")), ["key-a", "key-b", "key-c", "add"]);
        // A project that appeared meanwhile goes last.
        assert_eq!(keys(&rows(&groups(&["d", "a"]), &order, "")), ["key-a", "key-d", "add"]);
    }

    #[test]
    fn a_query_keeps_matching_projects_and_drops_add_project() {
        let all = groups(&["t3code", "Phillip", "examples"]);
        assert_eq!(keys(&rows(&all, &[], " T3 ")), ["key-t3code"]);
        assert_eq!(keys(&rows(&all, &[], "zzz")), Vec::<&str>::new());
    }
}
