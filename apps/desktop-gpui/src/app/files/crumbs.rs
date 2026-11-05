//! `FolderContentsMenu` (`FileBreadcrumbs.tsx`): the chevron after a folder crumb lists that
//! folder. Files open; a folder opens its own entries beside it, whose folders open in the panel.

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::{AnyElement, App, Context, Entity, IntoElement, Styled as _, Window, px};

use super::Checkout;
use super::logic::{self, Entry, EntryKind};
use crate::app::Workspace;
use crate::theme::Tokens;

/// Which menu lists a folder: the crumb's own menu, whose folders open submenus, or one of
/// those submenus, whose folders open in the panel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Crumb,
    Submenu,
}

/// What every row of a crumb's menu acts on.
#[derive(Clone)]
struct FolderMenu {
    view: Entity<Workspace>,
    root: Checkout,
    /// The file the panel shows, checked in its folder's list.
    current: String,
}

/// The rows of a folder's menu: folders first, then by name.
fn sorted(entries: &[Entry]) -> Vec<Entry> {
    let mut refs: Vec<&Entry> = entries.iter().collect();
    logic::sort_entries(&mut refs);
    refs.into_iter().cloned().collect()
}

impl FolderMenu {
    /// Lists `folder` and the folders in it, so their submenus fill on the next open.
    fn load(&self, folder: &str, window: &mut Window, cx: &mut App) {
        self.view.update(cx, |this, cx| {
            this.ensure_tree(&self.root, window, cx);
            this.load_folder(&self.root, folder.to_owned(), false, cx);
            let children: Vec<String> = this
                .tree_state(&self.root)
                .and_then(|tree| tree.folder(folder).ok().flatten())
                .map(|entries| {
                    entries.iter().filter(|e| e.kind == EntryKind::Directory).map(|e| e.path.clone()).collect()
                })
                .unwrap_or_default();
            for child in children {
                this.load_folder(&self.root, child, false, cx);
            }
        });
    }

    /// `folder`'s entries as menu items.
    fn items(
        &self,
        folder: &str,
        level: Level,
        mut menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let view = &self.view;
        let dark = view.read(cx).dark_theme(cx);
        let listing = view.read(cx).tree_state(&self.root).map(|tree| tree.folder(folder).map(|e| e.map(sorted)));
        let entries = match listing {
            Some(Ok(Some(entries))) => entries,
            Some(Err(_)) => {
                let (root, folder) = (self.root.clone(), folder.to_owned());
                return menu.item(PopupMenuItem::new("Retry loading folder").icon(IconName::RefreshCw).on_click(
                    window.listener_for(view, move |this: &mut Workspace, _, _, cx| {
                        this.load_folder(&root, folder.clone(), true, cx)
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
            let path = entry.path;
            match (entry.kind, level) {
                (EntryKind::Directory, Level::Crumb) => {
                    let submenu = self.clone();
                    menu = menu.submenu_with_icon(
                        Some(Icon::new(IconName::Folder)),
                        label,
                        window,
                        cx,
                        move |sub, window, cx| submenu.items(&path, Level::Submenu, sub, window, cx),
                    );
                }
                (EntryKind::Directory, Level::Submenu) => {
                    menu = menu.item(PopupMenuItem::new(label).icon(IconName::Folder).on_click(
                        window.listener_for(view, move |this: &mut Workspace, _, window, cx| {
                            this.open_file(&path, None, window, cx)
                        }),
                    ));
                }
                (EntryKind::File, _) => {
                    let (asset, color) = crate::file_icons::file_icon(&path, dark);
                    let current = path == self.current;
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .icon(Icon::empty().path(asset).text_color(color))
                            .checked(current)
                            .on_click(window.listener_for(view, move |this: &mut Workspace, _, window, cx| {
                                this.open_file(&path, None, window, cx)
                            })),
                    );
                }
            }
        }
        menu
    }
}

impl Workspace {
    /// The chevron after the crumb for `folder` (`""` is the workspace root).
    pub(super) fn crumb_menu(
        &self,
        root: &Checkout,
        folder: &str,
        current: &str,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let menu = FolderMenu { view: view.clone(), root: root.clone(), current: current.to_owned() };
        let loader = menu.clone();
        let folder = folder.to_owned();
        let label = if folder.is_empty() { "the project" } else { logic::file_name(&folder) };
        let tooltip = format!("Browse {label}");
        let id = gpui_kit::ElementId::Name(format!("crumb-menu:{folder}").into());
        let open_change = super::menu_open_change(view);
        let load_folder = folder.clone();
        Button::new(id)
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
            .tooltip(tooltip)
            .dropdown_menu(move |popup, window, cx| {
                menu.items(&folder, Level::Crumb, popup.max_h(px(384.)), window, cx)
            })
            .on_open_change(move |open, window, cx| {
                if *open {
                    loader.load(&load_folder, window, cx);
                }
                open_change(open, window, cx);
            })
            .into_any_element()
    }
}
