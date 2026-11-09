//! The file surfaces: Go to file (`ProjectFilePicker.tsx`), Search project contents
//! (`ProjectContentSearchDialog.tsx`), and the right panel's Files tree (`FileBrowserPanel.tsx`)
//! and file preview (`FilePreviewPanel.tsx`).

mod archive;
mod asset;
mod chrome;
mod crumbs;
mod document;
mod logic;
mod media;
mod notebook;
mod office;
mod open_in;
mod overlay;
mod preview;
mod table;
mod tree;

use std::collections::HashMap;

use gpui_kit::{Entity, SharedString};

pub(super) use document::inline_video_page;
pub(super) use logic::is_image;
pub(super) use overlay::{FileOverlay, OverlayMode};
pub(super) use tree::MentionDrag;

use super::Workspace;

/// A checkout on one environment: two machines can hold the same path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Checkout {
    pub env: SharedString,
    pub cwd: String,
}

/// A file or folder of a checkout: what its preview and its reveal requests are kept by.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct FileKey {
    pub checkout: Checkout,
    pub path: String,
}

/// `useActiveProjectTarget`: the open thread's or draft's checkout and project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectTarget {
    pub checkout: Checkout,
    pub project_name: SharedString,
}

#[derive(Default)]
pub(super) struct FilesState {
    pub(super) overlay: Option<FileOverlay>,
    /// The file tree of each checkout, kept while the window lives like the web's query cache.
    trees: HashMap<Checkout, tree::TreeState>,
    /// Each opened file's preview.
    previews: HashMap<FileKey, preview::PreviewState>,
    /// Each opened file's last open request, with the line it asked for (`revealLine`).
    reveals: HashMap<FileKey, (Option<u32>, u64)>,
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
    /// A workspace image's or video's actions menu.
    media_menu: Option<media::MediaMenu>,
    /// The crumb menu that is open and the folder it lists.
    crumb_menu: Option<crumbs::CrumbMenu>,
    /// Workspace images, per environment: a draft's resource names only its path.
    images: HashMap<SharedString, Entity<super::pages::settings::code::projects::icon::Images>>,
    /// `filesystem:write` on each environment, `None` until its session answers; asked again
    /// when its socket returns.
    writable: HashMap<SharedString, (Option<bool>, gpui_kit::Task<()>)>,
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
        });
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

/// `panelPath` for a link outside the workspace: a host file or folder opens in the panel,
/// except media, which the web shows in its media viewer.
pub(super) fn opens_host_path_in_panel(path: &str) -> bool {
    logic::is_host_path(path)
        && !matches!(logic::file_kind(path), logic::FileKind::Image | logic::FileKind::Video | logic::FileKind::Audio)
}

impl Workspace {
    /// A tree rooted at a host folder (`hostFolder`), not at the open project's checkout: its
    /// rows open and mention their host paths.
    pub(super) fn is_host_tree(&self, root: &Checkout) -> bool {
        self.project_target().is_none_or(|target| target.checkout != *root)
    }

    /// Whether the dark theme is showing, for icons with a colour per theme.
    pub(super) fn dark_theme(&self, cx: &gpui_kit::App) -> bool {
        gpui_kit::component::ActiveTheme::theme(cx).is_dark()
    }

    pub(super) fn project_target(&self) -> Option<ProjectTarget> {
        let project = self.target_project()?;
        let cwd = self.composer_cwd().filter(|cwd| !cwd.is_empty())?;
        let env = self.target.as_ref()?.environment().clone();
        Some(ProjectTarget { checkout: Checkout { env, cwd }, project_name: super::project_name(project) })
    }
}

#[cfg(test)]
mod tests {
    use super::opens_host_path_in_panel;

    #[test]
    fn host_links_open_in_the_panel_except_media() {
        assert!(opens_host_path_in_panel("/tmp/report.md"));
        assert!(opens_host_path_in_panel(r"C:\Users\me\notes"));
        assert!(!opens_host_path_in_panel("/tmp/plot.png"));
        assert!(!opens_host_path_in_panel("/tmp/clip.mp4"));
        assert!(!opens_host_path_in_panel("src/main.rs"), "workspace paths are not host paths");
    }
}
