//! The changed-files card under a run's last reply (`ChangedFilesCard` and `ChangedFilesTree`
//! in `ChangedFilesTree.tsx`): the run's checkpointed files as a folder tree that opens the
//! turn's diff at a file.

use std::collections::HashMap;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, div, px, rems,
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
    changes: &ChangedFiles,
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
    let list = v_flex().p(px(8.)).children(rows.into_iter().map(|(node, depth)| {
        tree_row(node, depth, has_directory_nodes, all_expanded, overrides, &run, this, tokens, scheme)
    }));

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

#[expect(clippy::too_many_arguments, reason = "one row of the card, drawn from its shared state")]
fn tree_row(
    node: &Node,
    depth: usize,
    has_directory_nodes: bool,
    all_expanded: bool,
    overrides: &HashMap<SharedString, bool>,
    run: &SharedString,
    this: &WeakEntity<Workspace>,
    tokens: &Tokens,
    scheme: DiffColorScheme,
) -> AnyElement {
    #[expect(clippy::cast_precision_loss, reason = "a tree depth")]
    let indent = 8. + depth as f32 * 14.;
    let accent = tokens.accent.opacity(0.6);
    let row = |id: ElementId| {
        keyboard_button(
            h_flex()
                .id(id)
                .h(px(28.))
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
                .into_any_element()
        }
    }
}
