//! Settings sections in the connections group; see `SETTINGS` in `mod.rs`.

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Window, div};

use crate::app::Workspace;

impl Workspace {
    pub(super) fn render_settings_connections(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Environments").into_any_element()
    }

    pub(super) fn render_settings_access(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Sharing & access").into_any_element()
    }

    pub(super) fn render_settings_secrets(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Secrets").into_any_element()
    }
}
