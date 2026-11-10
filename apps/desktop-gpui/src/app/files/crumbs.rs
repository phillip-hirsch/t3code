//! `FolderContentsMenu` (`FileBreadcrumbs.tsx`): the chevron after a folder crumb lists that
//! folder. Files open; a folder lists its own entries in the same menu, which "Back" (or
//! `ArrowLeft`) climbs again, never above the crumb's folder.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::base::actions::SelectLeft;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, DismissEvent, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, WeakEntity, Window, div, px,
};

use super::Checkout;
use super::logic::{self, Entry, EntryKind};
use crate::app::Workspace;
use crate::theme::Tokens;

/// The open crumb menu: its checkout, the crumb's folder, the folder it lists now, and the menu
/// showing it.
pub(super) struct CrumbMenu {
    root: Checkout,
    crumb: String,
    /// The file the panel showed when it opened; another file's crumbs have their own menus.
    current: String,
    directory: String,
    /// Set while a row moves the menu to another folder, so the close that follows reopens it.
    moving: bool,
    menu: Option<WeakEntity<PopupMenu>>,
}

impl CrumbMenu {
    fn is_for(&self, root: &Checkout, crumb: &str) -> bool {
        self.root == *root && self.crumb == crumb
    }
}

/// Whether `crumb`'s menu shows open for `current` in `root`: a menu opened on another checkout,
/// or for another file, stays closed here.
fn shows(state: Option<&CrumbMenu>, root: &Checkout, crumb: &str, current: &str) -> bool {
    state.is_some_and(|menu| menu.is_for(root, crumb) && menu.current == current)
}

/// `fileBreadcrumbParent`, offered as "Back" only within the crumb's folder `root`.
fn back_target(root: &str, directory: &str) -> Option<String> {
    if directory.is_empty() || directory == root {
        return None;
    }
    let parent = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
    let inside = root.is_empty() || parent == root || parent.starts_with(&format!("{root}/"));
    inside.then(|| parent.to_owned())
}

/// The rows of a folder's menu: folders first, then by name.
fn sorted(entries: &[Entry]) -> Vec<Entry> {
    let mut refs: Vec<&Entry> = entries.iter().collect();
    logic::sort_entries(&mut refs);
    refs.into_iter().cloned().collect()
}

/// How far a folder's listing got, so a menu showing it rebuilds once that changes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Listing {
    Pending,
    Failed,
    Ready,
}

/// What a crumb's menu acts on.
#[derive(Clone)]
struct FolderMenu {
    view: Entity<Workspace>,
    root: Checkout,
    /// The crumb's folder, above which the menu does not climb.
    crumb: String,
    project_name: SharedString,
    /// The file the panel shows, checked in its folder's list.
    current: String,
}

impl FolderMenu {
    fn load(&self, folder: &str, window: &mut Window, cx: &mut App) {
        self.view.update(cx, |this, cx| {
            this.ensure_tree(&self.root, window, cx);
            this.load_folder(&self.root, folder.to_owned(), false, cx);
        });
    }

    fn listing(&self, folder: &str, cx: &App) -> Listing {
        match self.view.read(cx).tree_state(&self.root).map(|tree| tree.folder(folder)) {
            Some(Ok(Some(_))) => Listing::Ready,
            Some(Err(_)) => Listing::Failed,
            _ => Listing::Pending,
        }
    }

    /// The folder the menu lists now: where it moved to, else the crumb's own.
    fn directory(&self, cx: &App) -> String {
        match &self.view.read(cx).files.crumb_menu {
            Some(open) if open.is_for(&self.root, &self.crumb) => open.directory.clone(),
            _ => self.crumb.clone(),
        }
    }

    /// Lists `folder` next. A row's click closes the menu after it runs, and the menu reopens
    /// on the new folder.
    fn navigate(&self, folder: String, window: &mut Window, cx: &mut App) {
        self.load(&folder, window, cx);
        self.view.update(cx, |this, cx| {
            if let Some(open) = this.files.crumb_menu.as_mut().filter(|open| open.is_for(&self.root, &self.crumb)) {
                open.directory = folder;
                open.moving = true;
            }
            cx.notify();
        });
    }

    fn move_to(&self, folder: String) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        let menu = self.clone();
        move |_, window, cx| menu.navigate(folder.clone(), window, cx)
    }

    /// `ArrowLeft` in the menu: lists the folder above, as "Back" does.
    fn climb(&self, window: &mut Window, cx: &mut App) {
        let Some(parent) = back_target(&self.crumb, &self.directory(cx)) else { return };
        let menu = self.view.read(cx).files.crumb_menu.as_ref().and_then(|open| open.menu.clone());
        self.navigate(parent, window, cx);
        // Closing the menu lets the reopened one list the new folder.
        if let Some(menu) = menu.and_then(|menu| menu.upgrade()) {
            menu.update(cx, |_, cx| cx.emit(DismissEvent));
        }
    }

    /// `folder`'s entries as menu items, after "Back" when the menu can climb.
    fn items(&self, folder: &str, mut menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        let view = &self.view;
        if let Some(parent) = back_target(&self.crumb, folder) {
            let label =
                if parent.is_empty() { self.project_name.to_string() } else { logic::file_name(&parent).to_owned() };
            menu = menu
                .item(
                    PopupMenuItem::new(format!("Back to {label}"))
                        .icon(IconName::ArrowLeft)
                        .on_click(self.move_to(parent)),
                )
                .separator();
        }
        let dark = view.read(cx).dark_theme(cx);
        let listing = view.read(cx).tree_state(&self.root).map(|tree| tree.folder(folder).map(|e| e.map(sorted)));
        let entries = match listing {
            Some(Ok(Some(entries))) => entries,
            Some(Err(_)) => {
                let (retry, folder) = (self.clone(), folder.to_owned());
                // The menu stays open on the folder (`closeOnClick={false}`).
                return menu.item(PopupMenuItem::new("Retry loading folder").icon(IconName::RefreshCw).on_click(
                    move |_, window, cx| {
                        let root = retry.root.clone();
                        retry.view.update(cx, |this, cx| this.load_folder(&root, folder.clone(), true, cx));
                        retry.navigate(folder.clone(), window, cx);
                    },
                ));
            }
            _ => return menu.item(PopupMenuItem::new("Loading folder…").disabled(true)),
        };
        if entries.is_empty() {
            return menu.item(PopupMenuItem::new("This folder is empty.").disabled(true));
        }
        for entry in entries {
            let label: SharedString = logic::file_name(&entry.path).to_owned().into();
            let path = entry.path;
            menu = match entry.kind {
                EntryKind::Directory => menu.item(
                    PopupMenuItem::element(move |_, cx| {
                        h_flex().w_full().gap_2().child(div().min_w_0().truncate().child(label.clone())).child(
                            Icon::new(IconName::ChevronRight)
                                .size(px(16.))
                                .ml_auto()
                                .flex_none()
                                .text_color(crate::theme::tokens(cx).muted_foreground),
                        )
                    })
                    .icon(IconName::Folder)
                    .on_click(self.move_to(path)),
                ),
                EntryKind::File => {
                    let (asset, color) = crate::file_icons::file_icon(&path, dark);
                    let current = path == self.current;
                    menu.item(
                        PopupMenuItem::new(label)
                            .icon(Icon::empty().path(asset).text_color(color))
                            .checked(current)
                            .on_click(window.listener_for(view, move |this: &mut Workspace, _, window, cx| {
                                this.open_file(&path, None, window, cx);
                            })),
                    )
                }
            };
        }
        menu
    }

    /// The menu for the folder it lists now, rebuilt when that folder's listing lands or fails.
    fn build(&self, menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>) -> PopupMenu {
        super::track_dropdown_menu(&self.view, cx);
        let folder = self.directory(cx);
        let (handle, view, root, crumb) =
            (cx.entity().downgrade(), self.view.clone(), self.root.clone(), self.crumb.clone());
        // The builder runs while the window draws, so the handle lands after the frame.
        cx.defer(move |cx| {
            view.update(cx, |this, _| {
                if let Some(open) = this.files.crumb_menu.as_mut().filter(|open| open.is_for(&root, &crumb)) {
                    open.menu = Some(handle);
                }
            });
        });
        let shown = Rc::new(Cell::new(self.listing(&folder, cx)));
        let (watch, watched) = (self.clone(), folder.clone());
        cx.observe_in(&self.view, window, move |menu, _, window, cx| {
            let now = watch.listing(&watched, cx);
            if now != shown.replace(now) {
                menu.rebuild(window, cx, |menu, window, cx| watch.items(&watched, menu, window, cx));
            }
        })
        .detach();
        self.items(&folder, menu.max_h(px(384.)), window, cx)
    }
}

impl Workspace {
    /// The chevron after the crumb for `folder` (`""` is the workspace root).
    pub(super) fn crumb_menu(
        &self,
        root: &Checkout,
        folder: &str,
        current: &str,
        project_name: &SharedString,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let menu = FolderMenu {
            view: view.clone(),
            root: root.clone(),
            crumb: folder.to_owned(),
            project_name: project_name.clone(),
            current: current.to_owned(),
        };
        let label = if folder.is_empty() { project_name.as_ref() } else { logic::file_name(folder) };
        let tooltip = format!("Browse {label}");
        let id = gpui_kit::ElementId::Name(format!("crumb-menu:{folder}").into());
        let open = shows(self.files.crumb_menu.as_ref(), root, folder, current);
        let open_change = super::menu_open_change(view.clone());
        let (opener, back, builder) = (menu.clone(), menu.clone(), menu);
        let trigger = Button::new(id)
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
            .dropdown_menu(move |popup, window, cx| builder.build(popup, window, cx))
            .open(open)
            .on_open_change(move |open, window, cx| {
                let stays = view.update(cx, |this, cx| {
                    let stays =
                        open_changed(&mut this.files.crumb_menu, &opener.root, &opener.crumb, &opener.current, *open);
                    cx.notify();
                    stays
                });
                if *open {
                    opener.load(&opener.crumb, window, cx);
                }
                open_change(&stays, window, cx);
            });
        // With no submenu to close, the menu passes ArrowLeft on: it climbs one folder.
        div()
            .flex_none()
            .on_action(move |_: &SelectLeft, window, cx| back.climb(window, cx))
            .child(trigger)
            .into_any_element()
    }
}

/// Whether `crumb`'s menu stays open after its popover opened or closed: the close that
/// follows a move to another folder reopens it there, and opening starts at the crumb's folder.
fn open_changed(state: &mut Option<CrumbMenu>, root: &Checkout, crumb: &str, current: &str, open: bool) -> bool {
    match state {
        Some(menu) if !open && menu.is_for(root, crumb) && menu.moving => {
            menu.moving = false;
            true
        }
        _ if open => {
            *state = Some(CrumbMenu {
                root: root.clone(),
                crumb: crumb.to_owned(),
                current: current.to_owned(),
                directory: crumb.to_owned(),
                moving: false,
                menu: None,
            });
            true
        }
        _ => {
            *state = None;
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Checkout, back_target, open_changed, shows};

    fn checkout(env: &str) -> Checkout {
        Checkout { env: env.to_owned().into(), cwd: "/repo".into() }
    }

    #[test]
    fn moving_to_another_folder_keeps_the_menu_open_there() {
        let (a, b) = (checkout("a"), checkout("b"));
        let mut state = None;
        assert!(open_changed(&mut state, &a, "src", "src/a.rs", true));
        assert!(shows(state.as_ref(), &a, "src", "src/a.rs"));
        // The same file and crumb on another environment's checkout has its own menu.
        assert!(!shows(state.as_ref(), &b, "src", "src/a.rs"));
        let menu = state.as_mut().expect("opened");
        assert_eq!(menu.directory, "src");
        // A folder row moves the menu, then its click closes it: it reopens on that folder.
        (menu.directory, menu.moving) = ("src/app".into(), true);
        assert!(open_changed(&mut state, &a, "src", "src/a.rs", false));
        assert_eq!(state.as_ref().map(|m| (m.directory.as_str(), m.moving)), Some(("src/app", false)));
        // Any other close closes it, and the next open starts at the crumb's folder again.
        assert!(!open_changed(&mut state, &a, "src", "src/a.rs", false));
        assert!(state.is_none());
        assert!(open_changed(&mut state, &a, "src", "src/a.rs", true));
        assert_eq!(state.map(|m| m.directory), Some("src".into()));
    }

    #[test]
    fn back_climbs_only_within_the_crumbs_folder() {
        assert_eq!(back_target("", ""), None);
        assert_eq!(back_target("", "src").as_deref(), Some(""));
        assert_eq!(back_target("", "src/app").as_deref(), Some("src"));
        assert_eq!(back_target("src", "src"), None);
        assert_eq!(back_target("src", "src/app").as_deref(), Some("src"));
        assert_eq!(back_target("src", "src/app/ui").as_deref(), Some("src/app"));
        // A folder outside the crumb's never climbs back into it.
        assert_eq!(back_target("src", "srcs/app"), None);
    }
}
