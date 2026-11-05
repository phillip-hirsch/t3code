//! Project actions for the picked project (`ProjectActionsSettings`).
//! Built by the project-actions agent; this stub keeps the panel's call site in place.

use gpui_kit::{AnyElement, Context, Window};

use super::groups::ProjectGroup;
use crate::app::Workspace;

impl Workspace {
    /// The section the web shows after the project defaults. `group.members` are the
    /// picked checkouts (each `Member` has the `ProjectShell` and its physical key);
    /// `None` renders nothing.
    pub(super) fn render_project_actions(
        &mut self,
        _group: &ProjectGroup,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        None
    }
}
