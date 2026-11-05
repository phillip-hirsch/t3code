//! Settings sections in the code group; see `SETTINGS` in `mod.rs`.

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Window, div};

use crate::app::Workspace;

// `settings/projects/`, declared here so `settings/mod.rs` stays untouched until merge.
#[path = "projects/mod.rs"]
mod projects;

impl Workspace {
    pub(super) fn render_settings_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_settings_projects_panel(window, cx)
    }

    pub(super) fn render_settings_source_control(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Source control").into_any_element()
    }

    pub(super) fn render_settings_worktrees(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Worktrees").into_any_element()
    }
}
