//! The file surfaces: Go to file (`ProjectFilePicker.tsx`), Search project contents
//! (`ProjectContentSearchDialog.tsx`), and the right panel's Files tree (`FileBrowserPanel.tsx`)
//! and file preview (`FilePreviewPanel.tsx`).

mod archive;
mod asset;
mod chrome;
mod crumbs;
mod document;
mod logic;
mod notebook;
mod office;
mod open_in;
mod overlay;
mod preview;
mod table;
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
    /// `t3code.renderBrowserFile`, `t3code.renderTable` and `t3code.renderMermaid`, which
    /// default to rendered: HTML, tables and diagrams show their source.
    html_source: bool,
    table_source: bool,
    mermaid_source: bool,
    /// PDFs, rendered HTML and media in native views.
    documents: document::Documents,
    /// Parsed documents and archives.
    assets: asset::Assets,
    /// A menu of the file surfaces is open; native document views hide so it shows.
    pub(super) menu_open: bool,
    _menu: Option<gpui_kit::Subscription>,
    images: Option<Entity<super::pages::settings::code::projects::icon::Images>>,
    /// The open thread's workspace mutation, read once per frame for the tree and preview.
    mutation: (Option<String>, Option<String>),
}

/// A dropdown's `on_open_change`: its popover closes on outside clicks without the menu's
/// `DismissEvent`, so dropdowns report their state here.
pub(super) fn menu_open_change(view: Entity<Workspace>) -> impl Fn(&bool, &mut gpui_kit::Window, &mut gpui_kit::App) {
    move |open, _, cx| {
        view.update(cx, |this, cx| {
            this.files.menu_open = *open;
            cx.notify();
        })
    }
}

/// Marks a file surface (context) menu open until it is dismissed. Called from the menu's builder, which
/// runs while the window draws, so the change lands after the frame.
pub(super) fn track_menu(view: &Entity<Workspace>, cx: &mut gpui_kit::Context<gpui_kit::component::menu::PopupMenu>) {
    let menu = cx.entity().downgrade();
    let view = view.clone();
    cx.defer(move |cx| {
        let Some(menu) = menu.upgrade() else { return };
        view.update(cx, |this, cx| {
            this.files.menu_open = true;
            this.files._menu = Some(cx.subscribe(&menu, |this, _, _: &gpui_kit::DismissEvent, cx| {
                this.files.menu_open = false;
                this.files._menu = None;
                cx.notify();
            }));
            cx.notify();
        });
    });
}

impl Workspace {
    /// Whether the dark theme is showing, for icons with a colour per theme.
    pub(super) fn dark_theme(&self, cx: &gpui_kit::App) -> bool {
        gpui_kit::component::ActiveTheme::theme(cx).is_dark()
    }

    pub(super) fn project_target(&self) -> Option<ProjectTarget> {
        let project = self.target_project()?;
        let cwd = self.composer_cwd().filter(|cwd| !cwd.is_empty())?;
        Some(ProjectTarget { cwd, project_name: super::project_name(project) })
    }
}
