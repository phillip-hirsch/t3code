//! Settings sections in the agents group; see `SETTINGS` in `mod.rs`.

use gpui_kit::{AnyElement, Context, Window};

use crate::app::Workspace;

#[path = "devices/mod.rs"]
mod devices;

#[path = "threads.rs"]
mod threads;

#[path = "scheduled_tasks/mod.rs"]
mod scheduled_tasks;

impl Workspace {
    pub(super) fn render_settings_providers(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_providers_page(window, cx)
    }

    pub(super) fn render_settings_threads(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_threads_settings(window, cx)
    }

    pub(super) fn render_settings_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_agent_browser_access(window, cx)
    }

    pub(super) fn render_settings_devices(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_devices_settings(window, cx)
    }

    pub(super) fn render_settings_scheduled_tasks(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        scheduled_tasks::render(self, window, cx)
    }
}
