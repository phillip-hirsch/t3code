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

    pub(super) fn render_settings_chat(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Chat").into_any_element()
    }

    pub(super) fn render_settings_diffs(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Diffs & pull requests").into_any_element()
    }

    pub(super) fn render_settings_keybindings(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Keybindings").into_any_element()
    }

    pub(super) fn render_settings_snap_shot(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("SnapShots").into_any_element()
    }
}
