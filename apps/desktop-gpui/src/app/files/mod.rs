//! The file surfaces: Go to file (`ProjectFilePicker.tsx`), Search project contents
//! (`ProjectContentSearchDialog.tsx`), and the right panel's Files tree (`FileBrowserPanel.tsx`)
//! and file preview (`FilePreviewPanel.tsx`).

mod archive;
mod asset;
mod attachment;
mod chrome;
mod comments;
mod crumbs;
mod database;
mod document;
mod logic;
mod media;
mod notebook;
mod office;
mod open_in;
mod overlay;
mod pdf;
mod preview;
mod table;
mod tree;
mod viewer;

use std::collections::HashMap;

use gpui_kit::{Entity, SharedString};

pub(super) use attachment::AttachmentFile;
pub(super) use document::inline_video_page;
pub(super) use logic::{absolute_path, is_image};
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
    pub origin: Origin,
}

impl FileKey {
    /// `path` in the checkout.
    pub(super) fn new(checkout: Checkout, path: String) -> Self {
        Self { checkout, path, origin: Origin::Workspace }
    }

    pub(super) fn in_workspace(&self) -> bool {
        self.origin == Origin::Workspace
    }
}

/// Where a previewed file's bytes come from. An attachment's key holds no checkout path: its
/// `path` is the name its viewers read the kind from (`attachment::preview_path`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Origin {
    Workspace,
    /// An uploaded attachment on the key's environment, read through its `attachment` asset.
    Attachment {
        id: SharedString,
        name: SharedString,
        mime: SharedString,
        size: u64,
    },
    /// A draft's attachment, by its local id: its bytes are on this machine.
    Draft {
        local_id: SharedString,
    },
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
    /// `t3code.fileExplorerWidth`, stored once a resize settles.
    explorer_width: Option<f32>,
    width_save: Option<gpui_kit::Task<()>>,
    /// `t3code.renderMarkdown`: Markdown files show rendered.
    render_markdown: bool,
    /// `t3code.renderBrowserFile`, `t3code.renderTable` and `t3code.renderMermaid`, which
    /// default to rendered: HTML, tables and diagrams show their source.
    html_source: bool,
    table_source: bool,
    mermaid_source: bool,
    /// Attachment previews showing their source: each starts rendered (`AttachmentFilePreview`'s
    /// own `rendered` state), apart from the preferences above.
    attachment_source: std::collections::HashSet<FileKey>,
    /// Files whose Save file is fetching them (`saving`).
    saving: std::collections::HashSet<FileKey>,
    /// PDFs, Office files, rendered HTML and media in native views, and PDF reading positions.
    documents: document::Documents,
    /// Parsed documents and archives.
    assets: asset::Assets,
    /// A menu of the file surfaces is open; native document views hide so it shows.
    pub(super) menu_open: bool,
    _menu: Vec<gpui_kit::Subscription>,
    /// The tracked menu was released, so `menu_open` clears next frame unless a menu replaced it.
    menu_released: bool,
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
    /// The line comment being written in a file.
    comment: Option<comments::FileDraft>,
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

/// Marks a file surface (context) menu open until it is dismissed or released. Called from the
/// menu's builder, which runs while the window draws, so the change lands after the frame.
pub(super) fn track_menu(view: &Entity<Workspace>, cx: &mut gpui_kit::Context<gpui_kit::component::menu::PopupMenu>) {
    track(view, true, cx);
}

/// Marks a file surface dropdown's menu open until it is released: its trigger can leave the tree
/// (navigating away) without `on_open_change(false)`. `menu_open_change` reports its dismissal.
pub(super) fn track_dropdown_menu(
    view: &Entity<Workspace>,
    cx: &mut gpui_kit::Context<gpui_kit::component::menu::PopupMenu>,
) {
    track(view, false, cx);
}

fn track(view: &Entity<Workspace>, dismiss: bool, cx: &mut gpui_kit::Context<gpui_kit::component::menu::PopupMenu>) {
    let menu = cx.entity().downgrade();
    let view = view.clone();
    cx.defer(move |cx| {
        let Some(menu) = menu.upgrade() else { return };
        view.update(cx, |this, cx| {
            this.files.menu_open = true;
            this.files.menu_released = false;
            this.files._menu = vec![cx.observe_release(&menu, |this, _, cx| {
                this.files.menu_released = true;
                cx.notify();
            })];
            if dismiss {
                this.files._menu.push(cx.subscribe(&menu, |this, _, _: &gpui_kit::DismissEvent, cx| {
                    this.files.menu_open = false;
                    this.files._menu.clear();
                    cx.notify();
                }));
            }
            cx.notify();
        });
    });
}

/// Clears `flag` once the dropdown menu being built is released: its trigger can leave the tree
/// (navigating away) while it is open, without `on_open_change(false)`.
pub(super) fn clear_on_release(
    view: &Entity<Workspace>,
    cx: &mut gpui_kit::Context<gpui_kit::component::menu::PopupMenu>,
    flag: fn(&mut Workspace) -> &mut bool,
) {
    let view = view.downgrade();
    cx.on_release(move |_, cx| {
        _ = view.update(cx, |this, cx| {
            *flag(this) = false;
            cx.notify();
        });
    })
    .detach();
}

impl Workspace {
    /// A released file menu (its owner left, or a crumb menu moved to another folder) clears
    /// `menu_open` after the frame drawn now, which builds any menu reopened in its place.
    pub(super) fn settle_file_menu(&mut self, window: &mut gpui_kit::Window, cx: &mut gpui_kit::Context<Self>) {
        if !self.files.menu_released {
            return;
        }
        cx.on_next_frame(window, |this, _, cx| {
            if std::mem::take(&mut this.files.menu_released) {
                this.files.menu_open = false;
                this.files._menu.clear();
                cx.notify();
            }
        });
    }
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

/// The web's `localStorage` keys for the file view, kept in this device's client settings.
const EXPLORER_OPEN_KEY: &str = "t3code.fileExplorerOpen";
const EXPLORER_WIDTH_KEY: &str = "t3code.fileExplorerWidth";
const RENDER_MARKDOWN_KEY: &str = "t3code.renderMarkdown";
const RENDER_BROWSER_FILE_KEY: &str = "t3code.renderBrowserFile";
const RENDER_TABLE_KEY: &str = "t3code.renderTable";
const RENDER_MERMAID_KEY: &str = "t3code.renderMermaid";

impl FilesState {
    /// The file view as the last session left it, else the web's defaults.
    #[expect(clippy::cast_possible_truncation, reason = "a pixel width")]
    pub(super) fn restored(stored: &crate::settings::Settings) -> Self {
        let flag = |key: &str, default: bool| stored.get(key).as_bool().unwrap_or(default);
        Self {
            explorer_hidden: !flag(EXPLORER_OPEN_KEY, true),
            explorer_width: stored.get(EXPLORER_WIDTH_KEY).as_f64().filter(|width| width.is_finite()).map(|w| w as f32),
            render_markdown: flag(RENDER_MARKDOWN_KEY, false),
            html_source: !flag(RENDER_BROWSER_FILE_KEY, true),
            table_source: !flag(RENDER_TABLE_KEY, true),
            mermaid_source: !flag(RENDER_MERMAID_KEY, true),
            ..Self::default()
        }
    }

    /// The preferences [`Self::restored`] reads back.
    fn stored_prefs(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut prefs = serde_json::Map::from_iter(
            [
                (EXPLORER_OPEN_KEY, !self.explorer_hidden),
                (RENDER_MARKDOWN_KEY, self.render_markdown),
                (RENDER_BROWSER_FILE_KEY, !self.html_source),
                (RENDER_TABLE_KEY, !self.table_source),
                (RENDER_MERMAID_KEY, !self.mermaid_source),
            ]
            .map(|(key, value)| (key.to_owned(), value.into())),
        );
        if let Some(width) = self.explorer_width {
            prefs.insert(EXPLORER_WIDTH_KEY.to_owned(), f64::from(width).into());
        }
        prefs
    }
}

impl Workspace {
    /// Stores the file view's preferences, as the web writes them to `localStorage`.
    fn save_file_prefs(&mut self) {
        let prefs = self.files.stored_prefs();
        self.client_settings.apply(&prefs);
    }

    /// Stores the explorer's width once a resize has settled (`commitWidth` on pointer up).
    fn save_explorer_width_later(&mut self, cx: &mut gpui_kit::Context<Self>) {
        self.files.width_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            let _ = this.update(cx, |this, _| this.save_file_prefs());
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::{FilesState, opens_host_path_in_panel};

    #[test]
    fn file_view_preferences_load_from_the_web_keys() {
        let defaults = FilesState::restored(&crate::settings::Settings::layered(&Default::default(), &().into()));
        assert!(!defaults.explorer_hidden && defaults.explorer_width.is_none() && !defaults.render_markdown);
        assert!(!defaults.html_source && !defaults.table_source && !defaults.mermaid_source);

        let stored = serde_json::json!({
            "t3code.fileExplorerOpen": false,
            "t3code.fileExplorerWidth": 312.5,
            "t3code.renderMarkdown": true,
            "t3code.renderBrowserFile": false,
            "t3code.renderTable": false,
            "t3code.renderMermaid": false,
        });
        let restored = FilesState::restored(&crate::settings::Settings::layered(&Default::default(), &stored));
        assert!(restored.explorer_hidden && restored.render_markdown);
        assert!(restored.html_source && restored.table_source && restored.mermaid_source);
        assert_eq!(restored.explorer_width, Some(312.5));
        assert_eq!(serde_json::Value::Object(restored.stored_prefs()), stored);
    }

    #[test]
    fn host_links_open_in_the_panel_except_media() {
        assert!(opens_host_path_in_panel("/tmp/report.md"));
        assert!(opens_host_path_in_panel(r"C:\Users\me\notes"));
        assert!(!opens_host_path_in_panel("/tmp/plot.png"));
        assert!(!opens_host_path_in_panel("/tmp/clip.mp4"));
        assert!(!opens_host_path_in_panel("src/main.rs"), "workspace paths are not host paths");
    }
}
