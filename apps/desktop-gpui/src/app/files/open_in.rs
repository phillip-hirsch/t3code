//! The file surface's compact `OpenInPicker`: the preferred editor's button opens the file, and
//! the chevron picks the preferred editor. A remote environment's file opens in this machine's
//! editor over SSH (`crate::app::remote_open`).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Disableable as _, Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::FileKey;
use crate::app::Workspace;
use crate::app::details::{editor_icon, editor_label};
use crate::app::right_panel::Surface;
use crate::remote_open::RemoteOpenState;
use crate::theme::Tokens;

/// What the file manager does with a file (`revealFileManager`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FileManager {
    Open,
    /// Selects the file in its folder.
    Reveal,
}

/// What the file tab's Open in opens.
#[derive(Clone)]
pub(super) struct FileOpenTarget {
    pub environment: SharedString,
    pub absolute: String,
    /// `file.isFile`: a read file, which remote VS Code links open as a file.
    pub is_file: bool,
    pub file_manager: FileManager,
}

impl Workspace {
    /// The Open in button: the file in the preferred editor. Returns whether it opened.
    fn open_file_in_preferred_editor(
        &mut self,
        target: FileOpenTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let open_in = self.open_in(&target.environment, cx);
        let can_open = open_in.can_open();
        let Some(editor) = open_in.preferred.filter(|_| can_open) else { return false };
        let reveal = target.file_manager == FileManager::Reveal && editor == "file-manager";
        let FileOpenTarget { environment, absolute, is_file, .. } = target;
        self.open_in_editor_at(&environment, absolute, is_file, editor, reveal, window, cx)
    }

    /// `editor.openFavorite` while a file tab shows: it presses that tab's Open in button
    /// (`registerOpenFavoriteEditorCommand`). `None` when no file tab shows.
    pub(in crate::app) fn open_favorite_in_file_surface(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        let Some(Surface::File(path)) = self.active_surface().cloned() else { return None };
        let checkout = self.project_target()?.checkout;
        let target = self.file_open_in_target(&FileKey::new(checkout, path));
        Some(self.file_open_in_shows(&target.environment, cx) && self.open_file_in_preferred_editor(target, window, cx))
    }

    /// The tab shows Open in for the desktop's own environment and for a remote one.
    fn file_open_in_shows(&self, environment: &str, cx: &mut Context<Self>) -> bool {
        self.primary_environment().is_some_and(|primary| primary.as_ref() == environment)
            || self.remote_open_state(environment, cx) != RemoteOpenState::LocalExec
    }

    pub(super) fn render_file_open_in(
        &self,
        target: FileOpenTarget,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.file_open_in_shows(&target.environment, cx) {
            return None;
        }
        let open_in = self.open_in(&target.environment, cx);
        let (can_open, host_denied) = (open_in.can_open(), open_in.host_denied);
        let preferred = open_in.preferred.clone();
        let shortcut = self.shortcut_label("editor.openFavorite");
        let view = cx.entity();
        let view_for_menu = view.clone();
        let menu_tokens = *tokens;
        let build = move |mut menu: PopupMenu, window: &mut Window, _: &mut Context<PopupMenu>| {
            for editor in open_in.listed() {
                let (picked, item_editor) = (editor.clone(), editor.clone());
                let label = SharedString::from(editor_label(editor).to_owned());
                let current = open_in.preferred.as_deref() == Some(editor.as_str());
                let shortcut = shortcut.clone().filter(|_| current);
                menu = menu.item(
                    PopupMenuItem::element(move |_, _| {
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(editor_icon(&item_editor, &menu_tokens))
                            .child(div().flex_1().child(label.clone()))
                            .children(
                                shortcut
                                    .clone()
                                    .map(|s| div().text_xs().text_color(menu_tokens.muted_foreground).child(s)),
                            )
                    })
                    .disabled(!can_open)
                    .on_click(window.listener_for(
                        &view,
                        move |this: &mut Workspace, _, _, cx| {
                            this.preferred_editor = Some(picked.clone());
                            cx.notify();
                        },
                    )),
                );
            }
            open_in.menu_notes(menu)
        };
        let border = tokens.input;
        let enabled = preferred.is_some() && can_open;
        let primary = div()
            .id("file-open-in")
            .h_full()
            .px(px(5.))
            .flex()
            .items_center()
            .when(enabled, |el| el.cursor_pointer().hover(|s| s.bg(tokens.accent)))
            .when(!enabled, |el| el.opacity(0.5))
            .map(|el| match &preferred {
                Some(editor) => el.child(div().size(px(14.)).flex().items_center().child(editor_icon(editor, tokens))),
                None => el.child(Icon::new(IconName::SquareArrowOutUpRight).size(px(14.))),
            })
            .tooltip(|window, cx| Tooltip::new("Open file in preferred editor").build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_file_in_preferred_editor(target.clone(), window, cx);
            }));
        Some(
            h_flex()
                .flex_none()
                .h(px(24.))
                .rounded(px(6.))
                .border_1()
                .border_color(border)
                .overflow_hidden()
                .text_color(tokens.foreground)
                .child(primary)
                .child(div().w(px(1.)).h_full().bg(border))
                .child(
                    Button::new("file-open-in-menu")
                        .custom(
                            ButtonCustomVariant::new(cx)
                                .foreground(tokens.foreground)
                                .hover(tokens.accent)
                                .active(tokens.accent),
                        )
                        .w(px(22.))
                        .h_full()
                        .rounded(px(0.))
                        .icon(Icon::new(IconName::ChevronDown).size(px(16.)))
                        .tooltip("Choose editor")
                        .disabled(host_denied)
                        .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| build(menu, window, cx))
                        .on_open_change(super::menu_open_change(view_for_menu)),
                )
                .into_any_element(),
        )
    }
}
