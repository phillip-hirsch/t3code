//! Settings sections in the system group; see `SETTINGS` in `mod.rs`.

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Window, div};

use crate::app::Workspace;

impl Workspace {
    pub(super) fn render_settings_maintenance(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Maintenance").into_any_element()
    }

    pub(super) fn render_settings_labs(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Labs").into_any_element()
    }

    pub(super) fn render_settings_about(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("About").into_any_element()
    }

    pub(super) fn render_settings_archived(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Archive").into_any_element()
    }
}
