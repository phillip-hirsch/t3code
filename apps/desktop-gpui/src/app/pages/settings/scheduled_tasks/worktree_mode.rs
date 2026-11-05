//! The editor's "Create a new worktree" workspace mode: the web's `<Field label="Base branch">`
//! with `WorktreeBaseBranchPicker` (branch list for the project's checkout, plus the
//! "start from origin" toggle). Seam for that port; the editor calls it while the mode is picked.
//!
//! It edits `editor.draft.base_ref` and `editor.draft.start_from_origin`; the editor already
//! saves them as `{type: "worktree", baseRef, startFromOrigin}`. The project's checkout is
//! `editor.selected_project(cx)`'s `workspace_root`; `editor.client` reaches the server.
//! Disable it while `editor.saving` or disconnected, as the web does.

use gpui_kit::{AnyElement, Context, Window};

use super::editor::TaskEditor;

/// The base-branch field; `None` until it is ported (saving then uses `main`, from origin).
pub(super) fn render_new_worktree_mode(
    _editor: &mut TaskEditor,
    _window: &mut Window,
    _cx: &mut Context<TaskEditor>,
) -> Option<AnyElement> {
    None
}
