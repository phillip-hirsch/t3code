//! Settings sections in the code group; see `SETTINGS` in `mod.rs`.

use gpui_kit::{AnyElement, Context, IntoElement, ParentElement as _, Window, div};

use crate::app::Workspace;

#[path = "pending.rs"]
pub(super) mod pending;
#[path = "source_control.rs"]
mod source_control;
#[path = "worktrees.rs"]
mod worktrees;

impl Workspace {
    pub(super) fn render_settings_projects(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().child("Projects").into_any_element()
    }

    pub(super) fn render_settings_source_control(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_source_control_settings(window, cx)
    }

    pub(super) fn render_settings_worktrees(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.render_worktrees_settings(window, cx)
    }
}
