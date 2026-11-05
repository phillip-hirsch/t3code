//! `FolderContentsMenu` (`FileBreadcrumbs.tsx`): the chevron after a folder crumb lists that
//! folder. Files open; a folder opens its own entries beside it, whose folders open in the panel.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::{AnyElement, Context, Entity, IntoElement, Styled as _, Window, px};

use super::logic::{self, Entry, EntryKind};
use crate::app::Workspace;
use crate::theme::Tokens;

/// The rows of a folder's menu: folders first, then by name.
fn sorted(entries: &[Entry]) -> Vec<Entry> {
    let mut refs: Vec<&Entry> = entries.iter().collect();
    logic::sort_entries(&mut refs);
    refs.into_iter().cloned().collect()
}

/// A folder's entries as menu items; `nested` folders open in the panel instead of a submenu.
fn folder_items(
    view: &Entity<Workspace>,
    cwd: &str,
    folder: &str,
    current: &str,
    nested: bool,
    mut menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let dark = view.read(cx).dark_theme(cx);
    let listing = view.read(cx).tree_state(cwd).map(|tree| tree.folder(folder).map(|e| e.map(sorted)));
    let entries = match listing {
        Some(Ok(Some(entries))) => entries,
        Some(Err(_)) => {
            let (cwd, folder) = (cwd.to_owned(), folder.to_owned());
            return menu.item(PopupMenuItem::new("Retry loading folder").icon(IconName::RefreshCw).on_click(
                window.listener_for(view, move |this: &mut Workspace, _, _, cx| {
                    this.load_folder(&cwd, folder.clone(), true, cx)
                }),
            ));
        }
        _ => return menu.item(PopupMenuItem::new("Loading folder…").disabled(true)),
    };
    if entries.is_empty() {
        return menu.item(PopupMenuItem::new("This folder is empty.").disabled(true));
    }
    for entry in entries {
        let label = logic::file_name(&entry.path).to_owned();
        let path = entry.path.clone();
        if entry.kind == EntryKind::Directory {
            if nested {
                menu = menu.item(PopupMenuItem::new(label).icon(IconName::Folder).on_click(
                    window.listener_for(view, move |this: &mut Workspace, _, window, cx| {
                        this.open_file(&path, None, window, cx)
                    }),
                ));
            } else {
                let (view, cwd, current) = (view.clone(), cwd.to_owned(), current.to_owned());
                menu = menu.submenu_with_icon(
                    Some(Icon::new(IconName::Folder)),
                    label,
                    window,
                    cx,
                    move |sub, window, cx| folder_items(&view, &cwd, &path, &current, true, sub, window, cx),
                );
            }
            continue;
        }
        let (asset, color) = crate::file_icons::file_icon(&entry.path, dark);
        menu = menu.item(
            PopupMenuItem::new(label)
                .icon(Icon::empty().path(asset).text_color(color))
                .checked(entry.path == current)
                .on_click(window.listener_for(view, move |this: &mut Workspace, _, window, cx| {
                    this.open_file(&path, None, window, cx)
                })),
        );
    }
    menu
}

impl Workspace {
    /// The chevron after the crumb for `folder` (`""` is the workspace root).
    pub(super) fn crumb_menu(
        &self,
        cwd: &str,
        folder: &str,
        current: &str,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let (cwd, folder, current) = (cwd.to_owned(), folder.to_owned(), current.to_owned());
        let label = if folder.is_empty() { "the project".to_owned() } else { logic::file_name(&folder).to_owned() };
        Button::new(gpui_kit::ElementId::Name(format!("crumb-menu:{folder}").into()))
            .custom(
                ButtonCustomVariant::new(cx)
                    .foreground(tokens.muted_foreground.opacity(0.6))
                    .hover(tokens.accent)
                    .active(tokens.accent),
            )
            .w(px(16.))
            .h(px(20.))
            .mx(px(2.))
            .rounded(px(4.))
            .icon(Icon::new(IconName::ChevronRight).size(px(14.)))
            .tooltip(format!("Browse {label}"))
            .dropdown_menu(move |menu, window, cx| {
                // Lists the folder and the folders in it, so their submenus fill on the next open.
                view.update(cx, |this, cx| {
                    this.ensure_tree(&cwd, window, cx);
                    this.load_folder(&cwd, folder.clone(), false, cx);
                    let children: Vec<String> = this
                        .tree_state(&cwd)
                        .and_then(|tree| tree.folder(&folder).ok().flatten())
                        .map(|entries| {
                            entries.iter().filter(|e| e.kind == EntryKind::Directory).map(|e| e.path.clone()).collect()
                        })
                        .unwrap_or_default();
                    for child in children {
                        this.load_folder(&cwd, child, false, cx);
                    }
                });
                folder_items(&view, &cwd, &folder, &current, false, menu.max_h(px(384.)), window, cx)
            })
            .into_any_element()
    }
}
