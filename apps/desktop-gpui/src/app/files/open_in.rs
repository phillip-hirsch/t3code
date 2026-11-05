//! The file surface's compact `OpenInPicker`: the preferred editor's button opens the file, and
//! the chevron picks the preferred editor.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::app::Workspace;
use crate::app::details::{editor_icon, editor_label};
use crate::theme::Tokens;

impl Workspace {
    /// `reveal`: the file manager reveals the file instead of opening it (`revealFileManager`).
    pub(super) fn render_file_open_in(
        &self,
        absolute: String,
        reveal: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editors = self.server_info.available_editors.clone();
        let preferred = self.preferred_editor_id();
        let shortcut = self.shortcut_label("editor.openFavorite");
        let view = cx.entity();
        let menu_tokens = *tokens;
        let current = preferred.clone();
        let build = move |mut menu: PopupMenu, window: &mut Window, _: &mut Context<PopupMenu>| {
            if editors.is_empty() {
                return menu.item(PopupMenuItem::new("No installed editors found").disabled(true));
            }
            for editor in &editors {
                let (picked, item_editor) = (editor.clone(), editor.clone());
                let label = SharedString::from(editor_label(editor).to_owned());
                let shortcut = shortcut.clone().filter(|_| current.as_deref() == Some(editor.as_str()));
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
                    .on_click(window.listener_for(
                        &view,
                        move |this: &mut Workspace, _, _, cx| {
                            this.preferred_editor = Some(picked.clone());
                            cx.notify();
                        },
                    )),
                );
            }
            menu
        };
        let border = tokens.input;
        let enabled = preferred.is_some();
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
                let Some(editor) = this.preferred_editor_id() else { return };
                let title = format!("Could not open in {}", editor_label(&editor));
                let reveal = reveal && editor == "file-manager";
                this.preferred_editor = Some(editor.clone());
                this.open_path_in_editor(absolute.clone(), editor, reveal, title, window, cx);
            }));
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
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| build(menu, window, cx)),
            )
            .into_any_element()
    }
}
