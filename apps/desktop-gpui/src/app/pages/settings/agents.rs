//! Settings sections in the agents group; see `SETTINGS` in `mod.rs`.

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Window, div};

use crate::app::Workspace;

#[path = "scheduled_tasks/mod.rs"]
mod scheduled_tasks;

impl Workspace {
    pub(super) fn render_settings_providers(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Providers").into_any_element()
    }

    pub(super) fn render_settings_threads(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Threads").into_any_element()
    }

    pub(super) fn render_settings_browser(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Browser").into_any_element()
    }

    pub(super) fn render_settings_devices(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Devices").into_any_element()
    }

    pub(super) fn render_settings_scheduled_tasks(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        scheduled_tasks::render(self, window, cx)
    }
}
