//! The changed-files card under a run's last reply (`ChangedFilesCard` and `ChangedFilesTree`
//! in `ChangedFilesTree.tsx`): the run's checkpointed files as a folder tree that opens the
//! turn's diff at a file. A long tree draws only the rows near the viewport.

use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, FocusHandle, GlobalElementId, Hsla,
    InspectorElementId, InteractiveElement as _, IntoElement, LayoutId, MouseButton, ParentElement as _, Pixels,
    SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div, point, px, rems, size,
};

use super::Workspace;
use super::git::{self, DiffColorScheme};
use super::titlebar::icon;
use crate::theme::Tokens;
use crate::timeline::ChangedFiles;
use crate::turn_diff_tree::{self, Node};

/// The cards' folder state per run: "expand all" (`threadChangedFilesExpandedById`), and the
/// folders toggled away from it since.
#[derive(Default)]
pub(super) struct ChangedFilesUi {
    expanded: HashMap<SharedString, bool>,
    overrides: HashMap<SharedString, HashMap<SharedString, bool>>,
}

/// `@[24rem]/changed-files`: narrower cards keep "Open diff" to its icon.
const LABELLED_OPEN_DIFF: f32 = 384.;
/// `VIRTUALIZE_AFTER_ROWS` and `ROW_OVERSCAN`: a tree with more rows draws only those in view
/// and this many either side.
const VIRTUALIZE_AFTER: usize = 250;
const OVERSCAN: usize = 8;
/// A row's `h-7`.
const ROW: f32 = 28.;

fn stat_label(additions: u64, deletions: u64, size: f32, tokens: &Tokens, scheme: DiffColorScheme) -> AnyElement {
    git::diff_stat_label(additions, deletions, size, tokens, scheme).into_any_element()
}

/// `Button variant="ghost-muted"`: muted until hovered, the accent fill on hover.
fn ghost_muted(id: impl Into<ElementId>, tokens: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    let (accent, foreground) = (tokens.accent, tokens.foreground);
    keyboard_button(
        h_flex()
            .id(id)
            .flex_none()
            .justify_center()
            .rounded(px(6.))
            .text_color(tokens.muted_foreground)
            .cursor_pointer()
            .hover(move |s| s.bg(accent).text_color(foreground)),
        tokens,
    )
}

/// A `<button>`: a tab stop that Enter and Space click, with `focus-visible:ring-2 ring-inset`.
/// The ring's 2px come out of the control's padding.
fn keyboard_button(element: gpui_kit::Stateful<gpui_kit::Div>, tokens: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ring = tokens.ring;
    element
        .tab_index(0)
        .border_2()
        .border_color(gpui_kit::transparent_black())
        .focus_visible(move |s| s.border_color(ring))
}

pub(super) fn card(
    changes: &Rc<ChangedFiles>,
    this: &WeakEntity<Workspace>,
    width: f32,
    tokens: &Tokens,
    cx: &App,
) -> AnyElement {
    let Some(workspace) = this.upgrade() else { return div().into_any_element() };
    let workspace = workspace.read(cx);
    let scheme = DiffColorScheme::from_settings(&workspace.client_settings.values);
    let run = changes.run_id.clone();
    let all_expanded = workspace.changed_files.expanded.get(&run).copied().unwrap_or(false);
    let empty = HashMap::new();
    let overrides = workspace.changed_files.overrides.get(&run).unwrap_or(&empty);
    let (count, additions, deletions) = changes.totals;
    let hidden = count.saturating_sub(changes.files.len() as u64);
    let has_directories = changes.files.iter().any(|file| file.path.contains(['/', '\\']));
    let first = changes.files.first().map(|file| SharedString::from(file.path.clone()));
    let open_diff = {
        let (this, run, first) = (this.clone(), run.clone(), first);
        move |_: &gpui_kit::ClickEvent, window: &mut gpui_kit::Window, cx: &mut App| {
            let _ = this.update(cx, |w, cx| w.open_turn_diff(run.clone(), first.clone(), window, cx));
        }
    };

    let toggle_all = has_directories.then(|| {
        let (this, run) = (this.clone(), run.clone());
        let label = if all_expanded { "Collapse all folders" } else { "Expand all folders" };
        ghost_muted(ElementId::Name(format!("changed-files-toggle-all-{run}").into()), tokens)
            .size(px(24.))
            .items_center()
            .child(
                icon(if all_expanded { "icons/chevrons-down-up.svg" } else { "icons/chevrons-up-down.svg" })
                    .with_size(px(12.)),
            )
            .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
            .on_click(move |_, _, cx| {
                let _ = this.update(cx, |w, cx| {
                    let ui = &mut w.changed_files;
                    let next = !ui.expanded.get(&run).copied().unwrap_or(false);
                    ui.expanded.insert(run.clone(), next);
                    // The tree remounts with the new default, dropping per-folder toggles.
                    ui.overrides.remove(&run);
                    cx.notify();
                });
            })
    });
    let header = h_flex()
        .justify_between()
        .gap(px(8.))
        .px(px(12.))
        .py(px(8.))
        .child(
            h_flex()
                .min_w_0()
                .flex_wrap()
                .gap_x(px(12.))
                .gap_y(px(4.))
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(tokens.foreground)
                .child(format!("{} changed file{}", git::group_thousands(count), if count == 1 { "" } else { "s" }))
                .when(additions > 0 || deletions > 0, |el| {
                    // `layout="inline"`: the two counts 4px apart.
                    el.child(
                        h_flex()
                            .gap(px(4.))
                            .font_family(tokens.fonts.mono)
                            .line_height(px(16.))
                            .child(
                                div()
                                    .text_color(git::addition_color(tokens, scheme))
                                    .child(format!("+{}", git::compact_diff_count(additions))),
                            )
                            .child(
                                div()
                                    .text_color(git::deletion_color(tokens, scheme))
                                    .child(format!("-{}", git::compact_diff_count(deletions))),
                            ),
                    )
                }),
        )
        .child(
            h_flex().flex_none().gap(px(4.)).children(toggle_all).child(
                ghost_muted(ElementId::Name(format!("changed-files-open-diff-{run}").into()), tokens)
                    .h(px(24.))
                    .px(px(5.))
                    .gap(px(4.))
                    .items_center()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(icon("icons/file-diff.svg").with_size(px(12.)))
                    .when(width >= LABELLED_OPEN_DIFF, |el| el.child("Open diff"))
                    .tooltip(|window, cx| Tooltip::new("Open the full diff").build(window, cx))
                    .on_click(open_diff.clone()),
            ),
        );

    let has_directory_nodes = changes.tree.iter().any(|node| matches!(node, Node::Directory { .. }));
    let rows = turn_diff_tree::rows(&changes.tree, overrides, all_expanded);
    let list = if rows.len() > VIRTUALIZE_AFTER {
        let overrides = Rc::new(overrides.clone());
        let (row_overrides, this, run, tokens) = (overrides.clone(), this.clone(), run.clone(), *tokens);
        div().p(px(8.)).child(TreeRows {
            id: ElementId::Name(format!("changed-files-rows-{run}").into()),
            changes: changes.clone(),
            overrides,
            all_expanded,
            count: rows.len(),
            row: Box::new(move |node, depth, focus| {
                let look = RowLook { has_directory_nodes, all_expanded, focus: Some(focus) };
                tree_row(node, depth, look, &row_overrides, &run, &this, &tokens, scheme)
            }),
        })
    } else {
        let look = RowLook { has_directory_nodes, all_expanded, focus: None };
        div().p(px(8.)).child(v_flex().children(
            rows.into_iter().map(|(node, depth)| tree_row(node, depth, look, overrides, &run, this, tokens, scheme)),
        ))
    };

    v_flex()
        .mt(px(16.))
        .rounded(px(8.))
        .bg(if tokens.dark { tokens.input.opacity(0.2) } else { tokens.secondary })
        .child(header)
        .child(list)
        .when(hidden > 0, |el| {
            el.child(
                div().px(px(12.)).pb(px(8.)).child(
                    ghost_muted(ElementId::Name(format!("changed-files-more-{run}").into()), tokens)
                        .h(px(24.))
                        .px(px(5.))
                        .items_center()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(format!(
                            "{} more file{} in the full diff",
                            git::group_thousands(hidden),
                            if hidden == 1 { "" } else { "s" }
                        ))
                        .on_click(open_diff),
                ),
            )
        })
        .into_any_element()
}

/// The rows a virtualized tree of `count` rows draws when its top is at `top` and the viewport
/// spans `viewport`: those in view and `OVERSCAN` either side, plus the focused row and its
/// neighbours, so focus survives scrolling away and Tab keeps walking the tree.
fn drawn_rows(count: usize, top: f32, viewport: (f32, f32), focused: Option<usize>) -> Vec<usize> {
    #[expect(clippy::cast_precision_loss, reason = "a row count")]
    let (rows, overscan) = (count as f32, OVERSCAN as f32);
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to the rows")]
    let row_at = |rows_down: f32| rows_down.clamp(0., rows) as usize;
    let start = row_at(((viewport.0 - top) / ROW).floor() - overscan);
    let end = row_at(((viewport.1 - top) / ROW).ceil() + overscan).max(start);
    let mut rows: Vec<usize> = (start..end).collect();
    if let Some(at) = focused.filter(|&at| at < count) {
        let around = at.saturating_sub(1)..(at + 2).min(count);
        rows.extend(around.filter(|i| !(start..end).contains(i)));
        rows.sort_unstable();
    }
    rows
}

/// Draws one row at its depth with its focus handle.
type RowFn = Box<dyn Fn(&Node, usize, &FocusHandle) -> AnyElement>;

/// The tree's rows past `VIRTUALIZE_AFTER`: as tall as every row, drawing only `drawn_rows`.
/// Each drawn row's focus handle is kept by its path, so focus follows the row across frames.
struct TreeRows {
    id: ElementId,
    changes: Rc<ChangedFiles>,
    overrides: Rc<HashMap<SharedString, bool>>,
    all_expanded: bool,
    count: usize,
    row: RowFn,
}

impl IntoElement for TreeRows {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for TreeRows {
    /// The box as tall as every row.
    type RequestLayoutState = AnyElement;
    type PrepaintState = Vec<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, AnyElement) {
        #[expect(clippy::cast_precision_loss, reason = "a row count")]
        let mut spacer = div().w_full().h(px(self.count as f32 * ROW)).into_any_element();
        (spacer.request_layout(window, cx), spacer)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        spacer: &mut AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        spacer.prepaint(window, cx);
        let id = id.expect("TreeRows has an id");
        let viewport = window.content_mask().bounds;
        let rows = turn_diff_tree::rows(&self.changes.tree, &self.overrides, self.all_expanded);
        window.with_element_state(id, |focus: Option<HashMap<SharedString, FocusHandle>>, window| {
            let mut focus = focus.unwrap_or_default();
            let focused = focus
                .iter()
                .find(|(_, handle)| handle.is_focused(window))
                .and_then(|(path, _)| rows.iter().position(|(node, _)| node_path(node) == path));
            let drawn = drawn_rows(
                rows.len(),
                f32::from(bounds.top()),
                (f32::from(viewport.top()), f32::from(viewport.bottom())),
                focused,
            );
            let mut kept = HashMap::with_capacity(drawn.len());
            let mut elements = Vec::with_capacity(drawn.len());
            for index in drawn {
                let (node, depth) = rows[index];
                let path = node_path(node);
                let handle = focus.remove(path).unwrap_or_else(|| cx.focus_handle().tab_stop(true));
                let mut element = (self.row)(node, depth, &handle);
                let space = size(AvailableSpace::Definite(bounds.size.width), AvailableSpace::Definite(px(ROW)));
                element.layout_as_root(space, window, cx);
                #[expect(clippy::cast_precision_loss, reason = "a row index")]
                element.prepaint_at(point(bounds.left(), bounds.top() + px(index as f32 * ROW)), window, cx);
                kept.insert(path.clone(), handle);
                elements.push(element);
            }
            (elements, kept)
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        spacer: &mut AnyElement,
        rows: &mut Vec<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        spacer.paint(window, cx);
        for row in rows {
            row.paint(window, cx);
        }
    }
}

fn node_path(node: &Node) -> &SharedString {
    match node {
        Node::Directory { path, .. } | Node::File { path, .. } => path,
    }
}

/// What every row of one card shares, and a virtualized row's focus handle.
#[derive(Clone, Copy)]
struct RowLook<'a> {
    has_directory_nodes: bool,
    all_expanded: bool,
    focus: Option<&'a FocusHandle>,
}

#[expect(clippy::too_many_arguments, reason = "one row of the card, drawn from its shared state")]
fn tree_row(
    node: &Node,
    depth: usize,
    look: RowLook<'_>,
    overrides: &HashMap<SharedString, bool>,
    run: &SharedString,
    this: &WeakEntity<Workspace>,
    tokens: &Tokens,
    scheme: DiffColorScheme,
) -> AnyElement {
    let RowLook { has_directory_nodes, all_expanded, focus } = look;
    #[expect(clippy::cast_precision_loss, reason = "a tree depth")]
    let indent = 8. + depth as f32 * 14.;
    let accent = tokens.accent.opacity(0.6);
    let row = |id: ElementId| {
        keyboard_button(
            h_flex()
                .id(id)
                .when_some(focus, gpui_kit::InteractiveElement::track_focus)
                .h(px(ROW))
                .w_full()
                .gap(px(8.))
                .pl(px(indent - 2.))
                .pr(px(6.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(move |s| s.bg(accent)),
            tokens,
        )
    };
    let muted = |alpha: f32| -> Hsla { tokens.muted_foreground.opacity(alpha) };
    match node {
        Node::Directory { name, path, stat, .. } => {
            let open = overrides.get(path).copied().unwrap_or(all_expanded);
            let (this, run, path) = (this.clone(), run.clone(), path.clone());
            row(ElementId::Name(format!("changed-dir-{run}-{path}").into()))
                .child(
                    icon(if open { "icons/chevron-down.svg" } else { "icons/chevron-right.svg" })
                        .with_size(px(14.))
                        .text_color(muted(0.7)),
                )
                .child(
                    icon(if open { "icons/folder.svg" } else { "icons/folder-closed.svg" })
                        .with_size(px(14.))
                        .text_color(muted(0.75)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(tokens.fonts.mono)
                        .text_size(rems(0.6875))
                        .text_color(muted(0.9))
                        .child(name.clone()),
                )
                .when(stat.additions > 0 || stat.deletions > 0, |el| {
                    el.child(div().ml_auto().child(stat_label(stat.additions, stat.deletions, 10., tokens, scheme)))
                })
                .on_click(move |_, _, cx| {
                    let _ = this.update(cx, |w, cx| {
                        let ui = &mut w.changed_files;
                        let all = ui.expanded.get(&run).copied().unwrap_or(false);
                        let folders = ui.overrides.entry(run.clone()).or_default();
                        let next = !folders.get(&path).copied().unwrap_or(all);
                        folders.insert(path.clone(), next);
                        cx.notify();
                    });
                })
                .into_any_element()
        }
        Node::File { name, path, stat } => {
            let (asset, color) = crate::file_icons::file_icon(path, tokens.dark);
            let (open_this, open_run, open_path) = (this.clone(), run.clone(), path.clone());
            let (menu_this, menu_path) = (this.clone(), path.clone());
            let (key_this, key_path) = (this.clone(), path.clone());
            row(ElementId::Name(format!("changed-file-{run}-{path}").into()))
                .when(has_directory_nodes || depth > 0, |el| el.child(div().size(px(14.)).flex_none()))
                .child(Icon::empty().path(asset).with_size(px(14.)).text_color(color))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(tokens.fonts.mono)
                        .text_xs()
                        .text_color(tokens.foreground.opacity(0.85))
                        .child(name.clone()),
                )
                .child(div().ml_auto().child(stat_label(stat.additions, stat.deletions, 10., tokens, scheme)))
                .on_click(move |_, window, cx| {
                    let _ = open_this.update(cx, |w, cx| {
                        w.open_turn_diff(open_run.clone(), Some(open_path.clone()), window, cx);
                    });
                })
                .on_mouse_down(MouseButton::Right, move |event, window, cx| {
                    cx.stop_propagation();
                    let _ =
                        menu_this.update(cx, |w, cx| w.open_changed_file_menu(&menu_path, event.position, window, cx));
                })
                .on_context_menu_key(move |position, window, cx| {
                    let _ = key_this.update(cx, |w, cx| w.open_changed_file_menu(&key_path, position, window, cx));
                })
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_tree_draws_the_rows_in_view_and_eight_either_side() {
        // The list starts 100px above a 280px viewport: rows 3 to 13 show.
        let rows = drawn_rows(1000, -100., (0., 280.), None);
        assert_eq!(rows, (0..22).collect::<Vec<_>>());
        let rows = drawn_rows(1000, -2800., (0., 280.), None);
        assert_eq!((rows[0], rows[rows.len() - 1]), (92, 117));
        // Below the viewport, the list draws the rows the overscan reaches; past its end, the last.
        assert_eq!(drawn_rows(1000, 400., (0., 280.), None), [0, 1, 2, 3]);
        assert_eq!(drawn_rows(1000, -28_100., (0., 280.), None), [995, 996, 997, 998, 999]);
    }

    #[test]
    fn the_focused_row_and_its_neighbours_stay_drawn() {
        let rows = drawn_rows(1000, -2800., (0., 280.), Some(10));
        assert_eq!(&rows[..3], &[9, 10, 11]);
        assert_eq!(rows[3], 92);
        // In view already, nothing repeats; the last row has no next one.
        let rows = drawn_rows(1000, -2800., (0., 280.), Some(100));
        assert_eq!(rows, (92..118).collect::<Vec<_>>());
        assert_eq!(drawn_rows(300, 400., (0., 280.), Some(299)), [0, 1, 2, 3, 298, 299]);
    }
}
