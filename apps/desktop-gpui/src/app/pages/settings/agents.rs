//! Settings sections in the agents group; see `SETTINGS` in `mod.rs`.

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Window, div};

use crate::app::Workspace;

#[path = "devices/mod.rs"]
mod devices;

#[path = "threads.rs"]
mod threads;

impl Workspace {
    pub(super) fn render_settings_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_providers_page(window, cx)
    }

    pub(super) fn render_settings_threads(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_threads_settings(window, cx)
    }

    pub(super) fn render_settings_browser(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_agent_browser_access(cx)
    }

    pub(super) fn render_settings_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_devices_settings(window, cx)
    }

    pub(super) fn render_settings_scheduled_tasks(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Scheduled tasks").into_any_element()
    }
}
