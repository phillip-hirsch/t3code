//! Settings sections in the general group; see `SETTINGS` in `mod.rs`.

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Window, div};

use crate::app::Workspace;

impl Workspace {
    pub(super) fn render_settings_general(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("General").into_any_element()
    }

    pub(super) fn render_settings_appearance(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Appearance").into_any_element()
    }

    pub(super) fn render_settings_chat(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_chat_section(cx)
    }

    pub(super) fn render_settings_diffs(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_diffs_section(cx)
    }

    pub(super) fn render_settings_keybindings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_keybindings_section(window, cx)
    }

    pub(super) fn render_settings_snap_shot(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_snap_shot_section(cx)
    }
}
