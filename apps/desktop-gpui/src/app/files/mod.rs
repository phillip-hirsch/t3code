//! The file surfaces: Go to file (`ProjectFilePicker.tsx`), Search project contents
//! (`ProjectContentSearchDialog.tsx`), and the right panel's Files tree (`FileBrowserPanel.tsx`)
//! and file preview (`FilePreviewPanel.tsx`).

mod logic;
mod overlay;
mod preview;
mod tree;

use std::collections::HashMap;

use gpui_kit::{Entity, SharedString};

pub(super) use overlay::{FileOverlay, OverlayMode};

use super::Workspace;

/// `useActiveProjectTarget`: the open thread's or draft's checkout and project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectTarget {
    pub cwd: String,
    pub project_name: SharedString,
}

#[derive(Default)]
pub(super) struct FilesState {
    pub(super) overlay: Option<FileOverlay>,
    /// The file tree of each checkout, kept while the window lives like the web's query cache.
    trees: HashMap<String, tree::TreeState>,
    /// Each opened file's preview, by checkout and path.
    previews: HashMap<(String, String), preview::PreviewState>,
    /// A line to scroll each opened file to, with the request it came from (`revealLine`).
    reveals: HashMap<(String, String), (u32, u64)>,
    reveal_requests: u64,
    /// `t3code.fileExplorerOpen`: the explorer beside a previewed file.
    explorer_hidden: bool,
    explorer_width: Option<f32>,
    /// `t3code.renderMarkdown`: Markdown files show rendered.
    render_markdown: bool,
    images: Option<Entity<super::pages::settings::code::projects::icon::Images>>,
    /// The open thread's workspace mutation, read once per frame for the tree and preview.
    mutation: (Option<String>, Option<String>),
}

impl Workspace {
    pub(super) fn project_target(&self) -> Option<ProjectTarget> {
        let project = self.target_project()?;
        let cwd = self.composer_cwd().filter(|cwd| !cwd.is_empty())?;
        Some(ProjectTarget { cwd, project_name: super::project_name(project) })
    }
}
