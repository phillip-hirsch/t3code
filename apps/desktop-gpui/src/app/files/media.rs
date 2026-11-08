//! `MediaActions` for a workspace image or video: its context menu copies the file's paths,
//! saves the file, or copies the image.

use std::sync::Arc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, DismissEvent, Entity, Focusable as _, Image, IntoElement as _,
    ParentElement as _, Pixels, Point, Subscription, Window, anchored, deferred, px,
};

use super::{FileKey, logic};
use crate::app::Workspace;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MediaKind {
    Image,
}

impl MediaKind {
    fn noun(self) -> &'static str {
        match self {
            Self::Image => "image",
        }
    }
}

pub(super) struct MediaMenu {
    view: Entity<PopupMenu>,
    position: Point<Pixels>,
    _dismiss: Subscription,
}

/// `isWindowsAbsolutePath`: a drive path or a UNC share.
fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with("\\\\")
        || (bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes.get(2).is_none_or(|c| matches!(c, b'/' | b'\\')))
}

/// `mediaFileReference` as the menu copies it (`formatFilePathForClipboard`): the full path and,
/// inside the workspace, the relative one, with backslashes for a Windows host.
fn clipboard_paths(cwd: &str, path: &str) -> (String, Option<String>) {
    let absolute = logic::absolute_path(cwd, path);
    let format = |text: &str| if is_windows_absolute(&absolute) { text.replace('/', "\\") } else { text.to_owned() };
    let relative = (!logic::is_host_path(path)).then(|| format(path));
    (format(&absolute), relative)
}

impl Workspace {
    /// The menu `MediaActions` shows on a right click (`showMenu`). `image` is the loaded
    /// picture Copy image writes; without it, Copy image is unavailable.
    pub(super) fn open_media_menu(
        &mut self,
        key: &FileKey,
        kind: MediaKind,
        image: Option<Arc<Image>>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.files.media_menu.is_some() {
            return;
        }
        let (full, relative) = clipboard_paths(&key.checkout.cwd, &key.path);
        let view = cx.weak_entity();
        let key = key.clone();
        let noun = kind.noun();
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            let copy = |text: String| {
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    window.push_notification(Notification::success(text.clone()).title("Path copied"), cx);
                }
            };
            menu = menu.item(PopupMenuItem::new("Copy full path").on_click(copy(full.clone())));
            if let Some(relative) = relative.clone() {
                menu = menu.item(PopupMenuItem::new("Copy relative path").on_click(copy(relative)));
            }
            let (save_view, save_key) = (view.clone(), key.clone());
            menu = menu.item(PopupMenuItem::new(format!("Save {noun}")).on_click(move |_, window, cx| {
                let _ = save_view.update(cx, |this, cx| {
                    let title = format!("Could not save {noun}");
                    this.save_asset_file(&save_key, title, Some("Download started"), window, cx);
                });
            }));
            if kind == MediaKind::Image {
                menu = menu.item(PopupMenuItem::new("Copy image").disabled(image.is_none()).on_click(
                    move |_, window, cx| {
                        let Some(image) = &image else { return };
                        cx.write_to_clipboard(ClipboardItem::new_image(image));
                        window.push_notification(Notification::success("Image copied"), cx);
                    },
                ));
            }
            menu
        });
        let previous = window.focused(cx);
        menu.focus_handle(cx).focus(window, cx);
        let dismiss = cx.subscribe_in(&menu, window, move |this, _, _: &DismissEvent, window, cx| {
            this.files.media_menu = None;
            this.files.menu_open = false;
            if let Some(previous) = &previous {
                previous.focus(window, cx);
            }
            cx.notify();
        });
        // Native document views hide so the menu shows over them.
        self.files.menu_open = true;
        self.files.media_menu = Some(MediaMenu { view: menu, position, _dismiss: dismiss });
        cx.notify();
    }

    pub(super) fn render_media_menu(&self) -> Option<AnyElement> {
        let menu = self.files.media_menu.as_ref()?;
        Some(
            deferred(anchored().position(menu.position).snap_to_window_with_margin(px(8.)).child(menu.view.clone()))
                .with_priority(3)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::clipboard_paths;

    #[test]
    fn media_paths_copy_like_the_host_writes_them() {
        assert_eq!(clipboard_paths("/repo", "img/a.png"), ("/repo/img/a.png".into(), Some("img/a.png".into())));
        assert_eq!(
            clipboard_paths("C:/Users/me/repo", "img/a.png"),
            (r"C:\Users\me\repo\img\a.png".into(), Some(r"img\a.png".into()))
        );
        assert_eq!(
            clipboard_paths("/repo", "/tmp/shot.png"),
            ("/tmp/shot.png".into(), None),
            "a host file has no relative path"
        );
    }
}
