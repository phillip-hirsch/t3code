//! Project defaults for the picked project (`ProjectDefaultsSettings category="project"`).
//! Built by the project-defaults agent; this stub keeps the panel's call site in place.

use gpui_kit::{AnyElement, Context, Window};

use super::groups::ProjectGroup;
use crate::app::Workspace;

impl Workspace {
    /// The section the web shows under the project overview. `group.members` are the
    /// picked checkouts (each `Member` has the `ProjectShell` and its physical key);
    /// `None` renders nothing.
    pub(super) fn render_project_defaults(
        &mut self,
        _group: &ProjectGroup,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        None
    }
}
