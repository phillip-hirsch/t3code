//! Settings sections in the code group; see `SETTINGS` in `mod.rs`.

use gpui_kit::{AnyElement, Context, Window};

use crate::app::Workspace;
use crate::app::pages::settings::widgets::SettingsView;

#[path = "source_control.rs"]
mod source_control;
#[path = "worktrees.rs"]
mod worktrees;

// `settings/projects/`, declared here so `settings/mod.rs` stays untouched until merge.
#[path = "projects/mod.rs"]
pub(in crate::app) mod projects;

impl Workspace {
    pub(super) fn render_settings_projects(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_settings_projects_panel(view, window, cx)
    }

    pub(super) fn render_settings_source_control(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_source_control_settings(view, window, cx)
    }

    pub(super) fn render_settings_worktrees(
        &mut self,
        view: &SettingsView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_worktrees_settings(view, window, cx)
    }
}
