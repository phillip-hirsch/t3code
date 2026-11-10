//! The file surface (`FilePreviewPanel.tsx`): breadcrumbs and view toggles over the file, with
//! the Files tree beside it. Text opens in a highlighted editor that saves as you type
//! (`projects.writeFile`, 500 ms after the last edit), Markdown can render, images show, and a
//! folder path shows the tree with that folder selected.

use std::time::{Duration, Instant};

use anyhow::Context as _;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, InputEvent, Position};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::{TextView, TextViewState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, DragMoveEvent, ElementId, Empty, Entity, InteractiveElement as _,
    IntoElement, ObjectFit, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Subscription, Task, Window, div, img, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::{FileKey, asset, document, logic};
use crate::app::right_panel::Surface;
use crate::app::{Target, Workspace};
use crate::rpc::RpcError;
use crate::theme::Tokens;

/// `AuthFilesystemWriteScope`.
const FILESYSTEM_WRITE: &str = "filesystem:write";

/// `LARGE_TEXT_MAX_BYTES`.
const FULL_FILE_MAX_BYTES: u64 = 256 * 1024 * 1024;
const FULL_FILE_TOO_LARGE: &str = "Text previews support files up to 256 MB.";

/// `fileSaveSessions` debounce.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// `FileExplorerPane`: default, bounds, and the room the file keeps beside it.
const EXPLORER_WIDTH: f32 = 256.;
const EXPLORER_MIN: f32 = 160.;
const EXPLORER_MAX: f32 = 560.;
const CONTENT_MIN: f32 = 256.;

/// The whole file behind a read cut at 1 MB, read-only and unhighlighted like `LargeTextViewer`;
/// a CSV or TSV file's table too, which shows in table mode.
enum FullFile {
    Loading,
    Ready { editor: Entity<EditorState>, table: Option<std::rc::Rc<super::table::Table>> },
    Failed,
}

/// A workspace image's fetch (`useAssetUrlState` and the image's own load).
enum WorkspaceImage {
    Loading,
    Ready(std::sync::Arc<gpui_kit::Image>),
    Failed,
}

/// A loaded full file as drawn: its text, and its table in table mode.
type FullView = (Entity<EditorState>, Option<std::rc::Rc<super::table::Table>>);

enum Load {
    Loading,
    Text {
        truncated: bool,
        byte_length: u64,
    },
    Folder,
    Binary,
    Image,
    /// Read from its asset URL by a viewer of its kind.
    Asset,
    Failed {
        message: String,
        attempted: Option<String>,
        cwd: Option<String>,
    },
}

pub(super) struct PreviewState {
    kind: logic::FileKind,
    load: Load,
    pub(super) editor: Option<Entity<EditorState>>,
    /// The contents as last read or written.
    saved: String,
    save: logic::SaveQueue,
    last_change: Instant,
    markdown: Option<Entity<TextViewState>>,
    /// The table of a CSV or TSV file's contents, parsed when first shown.
    table: Option<std::rc::Rc<super::table::Table>>,
    /// Bumped whenever `saved` changes, so rendered views rebuild without comparing text.
    revision: u64,
    full: Option<FullFile>,
    /// The word wrap toggle; unset, it follows `wordWrap`.
    wrap: Option<bool>,
    applied_wrap: Option<bool>,
    mutation: Option<(Option<String>, Option<String>)>,
    /// The socket a workspace read was last sent on.
    read_on: Option<crate::rpc::SocketId>,
    revealed: Option<u64>,
    /// The open request the rendered view was picked over (`handledReveal`).
    rendered_reveal: Option<u64>,
    /// The gutter marks of the comments on its lines.
    pub(super) marks: super::comments::Marks,
    /// An image's zoom in the panel.
    image_zoom: crate::app::image_viewer::PreviewZoom,
    _read: Option<Task<()>>,
    _full: Option<Task<()>>,
    _save: Option<Task<()>>,
    _subscription: Option<Subscription>,
}

impl PreviewState {
    fn new(path: &str) -> Self {
        Self {
            kind: logic::file_kind(path),
            load: Load::Loading,
            editor: None,
            saved: String::new(),
            save: logic::SaveQueue::default(),
            last_change: Instant::now(),
            markdown: None,
            table: None,
            revision: 0,
            full: None,
            wrap: None,
            applied_wrap: None,
            mutation: None,
            read_on: None,
            revealed: None,
            rendered_reveal: None,
            marks: Default::default(),
            image_zoom: Default::default(),
            _read: None,
            _full: None,
            _save: None,
            _subscription: None,
        }
    }

    /// Whether to read the file on `socket`: no read landed and none runs, and a failure stands
    /// until a new socket, so a file opened offline reads once its environment reconnects.
    fn needs_read(&self, socket: &crate::rpc::SocketId) -> bool {
        matches!(self.load, Load::Loading | Load::Failed { .. })
            && self._read.is_none()
            && self.read_on.as_ref() != Some(socket)
    }
}

/// Drag payload for the explorer's resize handle.
struct ExplorerResize;

/// `filePreviewReadErrorMessage`.
fn read_error_message(error: &Value) -> String {
    match error["failure"].as_str() {
        Some("path_not_file") => "The path is a directory or special file, not a regular file.".into(),
        Some("binary_file") => "The file is binary and cannot be displayed as text.".into(),
        Some("workspace_path_outside_root") => "The requested path is outside the workspace.".into(),
        Some("resolved_path_outside_root") => "The path resolves to a location outside the workspace.".into(),
        Some("operation_failed") if error["operation"] == "realpath-workspace-root" => {
            "The workspace folder could not be accessed.".into()
        }
        Some("operation_failed") => "The file could not be accessed or read. It may be missing or inaccessible.".into(),
        _ => error["message"].as_str().unwrap_or("Workspace query failed.").to_owned(),
    }
}

/// A file with a rendered view (`renderedMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rendered {
    Mermaid,
    Markdown,
    Table,
    Html,
}

impl Rendered {
    pub(super) fn of(path: &str) -> Option<Self> {
        if logic::is_mermaid(path) {
            Some(Self::Mermaid)
        } else if logic::is_markdown(path) {
            Some(Self::Markdown)
        } else if logic::table_delimiter(path).is_some() {
            Some(Self::Table)
        } else if logic::file_kind(path) == logic::FileKind::Html {
            Some(Self::Html)
        } else {
            None
        }
    }

    /// `renderedToggleLabel`.
    pub(super) fn toggle_label(self, rendered: bool) -> &'static str {
        match (self, rendered) {
            (Self::Mermaid, true) => "Show Mermaid source",
            (Self::Mermaid, false) => "Show rendered diagram",
            (Self::Markdown, true) => "Show markdown source",
            (Self::Markdown, false) => "Show rendered markdown",
            (Self::Table, true) => "Show source",
            (Self::Table, false) => "Show table",
            (Self::Html, true) => "Show HTML source",
            (Self::Html, false) => "Show rendered page",
        }
    }
}

/// `revealHandled`: a line reveal shows the source until the rendered view is picked over it.
fn reveal_handled(reveal: Option<(Option<u32>, u64)>, rendered_reveal: Option<u64>) -> bool {
    match reveal {
        Some((Some(_), request)) => rendered_reveal == Some(request),
        _ => true,
    }
}

impl super::FilesState {
    /// The rendered view `key` shows in `mode`: the preference, unless a line reveal holds the source.
    pub(super) fn shows_rendered(&self, key: &FileKey, mode: Rendered) -> bool {
        if !key.in_workspace() {
            return !self.attachment_source.contains(key);
        }
        let preferred = match mode {
            Rendered::Markdown => self.render_markdown,
            Rendered::Html => !self.html_source,
            Rendered::Table => !self.table_source,
            Rendered::Mermaid => !self.mermaid_source,
        };
        preferred
            && reveal_handled(
                self.reveals.get(key).copied(),
                self.previews.get(key).and_then(|state| state.rendered_reveal),
            )
    }

    pub(super) fn set_rendered(&mut self, key: &FileKey, mode: Rendered, rendered: bool) {
        if !key.in_workspace() {
            if rendered {
                self.attachment_source.remove(key);
            } else {
                self.attachment_source.insert(key.clone());
            }
            return;
        }
        match mode {
            Rendered::Markdown => self.render_markdown = rendered,
            Rendered::Html => self.html_source = !rendered,
            Rendered::Table => self.table_source = !rendered,
            Rendered::Mermaid => self.mermaid_source = !rendered,
        }
    }
}

impl Workspace {
    /// `openFile`: opens `path` in the panel, scrolled to `line` when given.
    pub(in crate::app) fn open_file(
        &mut self,
        path: &str,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if path == "." {
            self.open_surface(Surface::Files, window, cx);
            return;
        }
        let path = logic::normalize_open_path(path);
        if let Some(target) = self.project_target() {
            self.files.reveal_requests += 1;
            let key = FileKey::new(target.checkout, path.clone());
            self.files.reveals.insert(key, (line.map(|line| line.max(1)), self.files.reveal_requests));
        }
        self.open_file_surface(path, window, cx);
    }

    /// Reads the file once, and again after a workspace mutation unless edits are unsaved; then
    /// applies the wrap toggle and a pending line reveal.
    pub(super) fn sync_preview(&mut self, key: &FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let mutation = self.key_mutation(key);
        let socket = key.in_workspace().then(|| self.env_client(&key.checkout.env)).flatten().map(|c| c.socket_id());
        let wrap_default = self.client_settings.values.bool("wordWrap");
        let reveal = self.files.reveals.get(key).copied();
        let state = self.files.previews.entry(key.clone()).or_insert_with(|| PreviewState::new(&key.path));
        let fresh = state.mutation.is_none();
        let changed = state.mutation.as_ref().is_some_and(|m| *m != mutation);
        let dirty = state.save.pending();
        state.mutation = Some(mutation);
        if state.kind == logic::FileKind::Image {
            state.load = Load::Image;
            // `workspace-revision`: a mutation fetches the image again, at its fitted size.
            if changed {
                state.image_zoom.zoom = None;
                if let Some(resource) = self.image_resource(key)
                    && let Some(images) = self.files.images.get(&key.checkout.env)
                {
                    images.update(cx, |images, _| images.forget(&resource));
                }
            }
        } else if state.kind.is_asset() {
            state.load = Load::Asset;
        } else if fresh || (changed && !dirty) || socket.is_some_and(|socket| state.needs_read(&socket)) {
            self.read_file(key.clone(), window, cx);
        }
        let draft = !matches!(self.target, Some(Target::Thread { .. }));
        let Some(state) = self.files.previews.get_mut(key) else { return };
        let loads_full = if key.in_workspace() {
            logic::loads_full_file(&key.path, draft)
        } else {
            super::attachment::streams_text(&key.path)
        };
        if matches!(state.load, Load::Text { truncated: true, .. }) && state.full.is_none() && loads_full {
            self.load_full_file(key.clone(), window, cx);
        }
        let Some(state) = self.files.previews.get_mut(key) else { return };
        let Some(editor) = state.editor.clone() else { return };
        let wrap = state.wrap.unwrap_or(wrap_default);
        if state.applied_wrap != Some(wrap) {
            state.applied_wrap = Some(wrap);
            editor.update(cx, |editor, cx| editor.set_soft_wrap(wrap, window, cx));
            // An attachment's whole text wraps too (`LargeTextViewer`'s `wordWrap`).
            if let Some(FullFile::Ready { editor, .. }) = state.full.as_ref().filter(|_| !key.in_workspace()) {
                editor.update(cx, |editor, cx| editor.set_soft_wrap(wrap, window, cx));
            }
        }
        // The full file shows in place of the first megabyte once it loads, so the line waits for it.
        let shown = match &state.full {
            None => Some(editor),
            Some(FullFile::Ready { editor, .. }) => Some(editor.clone()),
            Some(FullFile::Loading | FullFile::Failed) => None,
        };
        if let (Some(editor), Some((Some(line), request))) = (shown, reveal)
            && state.revealed != Some(request)
        {
            state.revealed = Some(request);
            editor.update(cx, |editor, cx| editor.set_cursor_position(Position::new(line - 1, 0), window, cx));
            // The caret moves without taking focus: the composer keeps it, as when the web's palette closes.
            self.focus_composer(window, cx);
        }
    }

    /// `onRefreshSelectedFile`: the tree's Refresh reads the open file again (unless edits are
    /// unsaved) and reloads its document view; media and PDFs keep what they show.
    pub(super) fn refresh_selected_file(&mut self, key: &FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.files.previews.get(key) else { return };
        let (kind, folder, dirty) = (state.kind, matches!(state.load, Load::Folder), state.save.pending());
        match kind {
            logic::FileKind::Image | logic::FileKind::Video | logic::FileKind::Audio | logic::FileKind::Pdf => {}
            _ if folder => {}
            kind @ (logic::FileKind::Text | logic::FileKind::Html) => {
                if !dirty {
                    self.read_file(key.clone(), window, cx);
                }
                if kind == logic::FileKind::Html {
                    self.files.documents.forget(key);
                }
            }
            logic::FileKind::Font => self.files.documents.forget(key),
            _ => self.files.assets.forget(key),
        }
        cx.notify();
    }

    fn read_file(&mut self, key: FileKey, window: &mut Window, cx: &mut Context<Self>) {
        if !key.in_workspace() {
            return self.read_attachment_text(key, window, cx);
        }
        let Some(client) = self.env_client(&key.checkout.env) else { return };
        let Some(state) = self.files.previews.get_mut(&key) else { return };
        let revision = state.save.revision();
        state.read_on = Some(client.socket_id());
        let payload = json!({"cwd": key.checkout.cwd, "relativePath": key.path});
        let request_key = key.clone();
        state._read = Some(cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.readFile", payload).await;
            let _ = this.update_in(cx, |this, window, cx| this.apply_read(&request_key, revision, result, window, cx));
        }));
    }

    /// Applies a read requested at edit `revision`; edits made since win, like the web's unsaved draft.
    fn apply_read(
        &mut self,
        key: &FileKey,
        revision: u64,
        result: Result<Value, RpcError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.files.previews.get_mut(key) else { return };
        state._read = None;
        if state.save.revision() != revision {
            return;
        }
        match result {
            Ok(value) if value["exists"] == false => {
                state.load =
                    Load::Failed { message: format!("'{}' does not exist.", key.path), attempted: None, cwd: None };
            }
            Ok(value) => {
                let contents = value["contents"].as_str().unwrap_or_default().to_owned();
                let truncated = value["truncated"].as_bool().unwrap_or(false);
                let byte_length = value["byteLength"].as_u64().unwrap_or(0);
                show_text(state, key, contents, truncated, byte_length, window, cx);
            }
            Err(RpcError::Failure(error)) => {
                state.load = match error["failure"].as_str() {
                    Some("path_not_file") => Load::Folder,
                    Some("binary_file") => Load::Binary,
                    _ => Load::Failed {
                        message: read_error_message(&error),
                        attempted: error["resolvedPath"]
                            .as_str()
                            .or(error["operationPath"].as_str())
                            .map(str::to_owned),
                        cwd: error["cwd"].as_str().map(str::to_owned),
                    },
                };
            }
            Err(error) => {
                state.load = Load::Failed { message: logic::error_message(&error), attempted: None, cwd: None };
            }
        }
        cx.notify();
    }

    /// The `media-file` resource for a workspace file of the open thread.
    fn media_resource(&self, key: &FileKey) -> Option<Value> {
        let Some(Target::Thread { id: thread_id, .. }) = &self.target else { return None };
        let path = logic::absolute_path(&key.checkout.cwd, &key.path);
        Some(json!({"_tag": "media-file", "threadId": thread_id, "path": path, "disposition": "attachment"}))
    }

    /// The socket and address of the environment `key`'s checkout is on.
    pub(super) fn file_connection(&self, key: &FileKey) -> Option<(crate::rpc::RpcClient, crate::backend::Endpoint)> {
        self.env_connection(&self.file_environment(key)?)
    }

    fn load_full_file(&mut self, key: FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let fetched = if key.in_workspace() {
            let (Some((client, endpoint)), Some(resource)) = (self.file_connection(&key), self.media_resource(&key))
            else {
                return;
            };
            futures::future::Either::Left(fetch_asset(client, endpoint, resource))
        } else {
            let Some(fetch) = self.file_fetch(&key, None, false) else { return };
            futures::future::Either::Right(async move {
                match fetch.read(FULL_FILE_MAX_BYTES, FULL_FILE_TOO_LARGE).await {
                    Ok((bytes, _)) => Ok(bytes),
                    Err(error) => Err(anyhow::anyhow!("{error}")),
                }
            })
        };
        let Some(state) = self.files.previews.get_mut(&key) else { return };
        state.full = Some(FullFile::Loading);
        let request_key = key.clone();
        let delimiter = logic::table_delimiter(&key.path);
        state._full = Some(cx.spawn_in(window, async move |this, cx| {
            let fetched = fetched.await;
            let text = cx
                .background_spawn(async move {
                    fetched.map(|bytes| {
                        let text = String::from_utf8(bytes)
                            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
                        let cells = delimiter.map(|delimiter| super::table::Cells::parse(&text, delimiter));
                        (text, cells)
                    })
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(state) = this.files.previews.get_mut(&request_key) else { return };
                // A new viewer takes the open request's line again, like the web's remounted one.
                state.revealed = None;
                state.full = Some(match text {
                    Ok((text, cells)) => {
                        let editor = cx.new(|cx| EditorState::new(window, cx).language("text").default_value(text));
                        let table = cells.map(|cells| std::rc::Rc::new(super::table::Table::new(cells)));
                        // The next sync wraps the new text as the toggle says.
                        state.applied_wrap = None;
                        FullFile::Ready { editor, table }
                    }
                    Err(error) => {
                        log::warn!("full file preview: {error:#}");
                        FullFile::Failed
                    }
                });
                cx.notify();
            });
        }));
    }

    /// Save file: fetches the file again and writes it where the user picks.
    fn save_full_file(&mut self, key: &FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.file_environment(key).filter(|env| self.env_connection(env).is_some());
        let (Some(env), Some(resource)) = (env, self.media_resource(key)) else {
            return;
        };
        let directory = std::env::home_dir().map(|home| home.join("Downloads")).unwrap_or_default();
        let picked = cx.prompt_for_new_path(&directory, Some(logic::file_name(&key.path)));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            // Looked up after the dialog: a reconnect while it was open replaced the socket.
            let connection = this.read_with(cx, |this, _| this.env_connection(&env)).ok().flatten();
            let fetched = match connection {
                Some((client, endpoint)) => fetch_asset(client, endpoint, resource).await,
                None => Err(anyhow::anyhow!("Reconnect to the environment and try again.")),
            };
            let written = match fetched {
                Ok(bytes) => {
                    cx.background_spawn(async move {
                        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))
                    })
                    .await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = written {
                let _ = cx.update(|window, cx| {
                    window
                        .push_notification(Notification::error(format!("{error:#}")).title("Could not save file"), cx);
                });
            }
        })
        .detach();
    }

    /// An edit: writes the editor's text 500 ms after the last edit, one write at a time
    /// (`FileSaveCoordinator`). While a write is in flight the loop running it picks the edit up.
    fn change_file(&mut self, key: &FileKey, cx: &mut Context<Self>) {
        let Some(state) = self.files.previews.get_mut(key) else { return };
        if matches!(state.load, Load::Text { truncated: true, .. }) {
            return;
        }
        state.last_change = Instant::now();
        if state.save.change() {
            Self::schedule_save(state, key.clone(), cx);
        }
    }

    fn toggle_markdown_task(
        &mut self,
        key: &FileKey,
        item_start: usize,
        checked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.files.previews.get_mut(key) else { return };
        let (Some(editor), markdown) = (state.editor.clone(), state.markdown.clone()) else { return };
        let Some(next) = logic::set_markdown_task(&editor.read(cx).value(), item_start, checked) else { return };
        if let Some(markdown) = markdown {
            markdown.update(cx, |view, cx| view.set_text(&next, cx));
        }
        editor.update(cx, |editor, cx| editor.set_value(next, window, cx));
        state.marks.reset();
        self.change_file(key, cx);
    }

    /// An environment's socket is back: its grants are asked again, and its files' unsaved
    /// edits write again once it may write, like the web's `subscribeWritable` recovery.
    pub(in crate::app) fn resume_file_saves(&mut self, env: &SharedString, cx: &mut Context<Self>) {
        self.files.writable.remove(env);
        let pending = self.files.previews.iter().any(|(key, state)| key.checkout.env == *env && state.save.pending());
        if pending {
            self.can_write_files(env, cx);
        }
    }

    /// `useEnvironmentScope(environmentId, AuthFilesystemWriteScope)`: whether files on `env`
    /// are editable, `None` while its session is being asked.
    fn can_write_files(&mut self, env: &SharedString, cx: &mut Context<Self>) -> Option<bool> {
        if let Some((granted, _)) = self.files.writable.get(env) {
            return *granted;
        }
        let endpoint = self.env_endpoint(env)?.clone();
        let answered = env.clone();
        let task =
            crate::app::pages::settings::check_scope(endpoint, FILESYSTEM_WRITE, cx, move |this, granted, cx| {
                if let Some((writable, _)) = this.files.writable.get_mut(&answered) {
                    *writable = Some(granted);
                }
                if granted {
                    this.resume_pending_saves(&answered, cx);
                }
            });
        self.files.writable.insert(env.clone(), (None, task));
        None
    }

    fn resume_pending_saves(&mut self, env: &str, cx: &mut Context<Self>) {
        for (key, state) in &mut self.files.previews {
            if key.checkout.env != env || !state.save.pending() {
                continue;
            }
            state.last_change = Instant::now();
            if state.save.resume() {
                Self::schedule_save(state, key.clone(), cx);
            }
        }
    }

    fn schedule_save(state: &mut PreviewState, key: FileKey, cx: &mut Context<Self>) {
        state._save = Some(cx.spawn(async move |this, cx| {
            let mut delay = SAVE_DEBOUNCE;
            loop {
                cx.background_executor().timer(delay).await;
                let Ok(Some((client, revision, contents))) = this.update(cx, |this, cx| this.start_write(&key, cx))
                else {
                    return;
                };
                let payload = json!({"cwd": key.checkout.cwd, "relativePath": key.path, "contents": contents});
                let result = client.request("projects.writeFile", payload).await;
                let finished = this.update(cx, |this, cx| this.finish_write(&key, revision, contents, result, cx));
                let Ok(Some(next)) = finished else { return };
                delay = next;
            }
        }));
    }

    /// The write's socket is the environment the file was opened on, whatever is open now. Without
    /// write access the edits stay unsaved until it returns.
    fn start_write(&mut self, key: &FileKey, cx: &mut Context<Self>) -> Option<(crate::rpc::RpcClient, u64, String)> {
        if self.files.writable.get(&key.checkout.env).and_then(|(granted, _)| *granted) != Some(true) {
            return None;
        }
        let client = self.env_client(&key.checkout.env)?;
        let state = self.files.previews.get_mut(key)?;
        let contents = state.editor.as_ref()?.read(cx).value().to_string();
        Some((client, state.save.start()?, contents))
    }

    /// Settles a write; the delay before the next one when edits arrived meanwhile.
    fn finish_write(
        &mut self,
        key: &FileKey,
        revision: u64,
        contents: String,
        result: Result<Value, RpcError>,
        cx: &mut Context<Self>,
    ) -> Option<Duration> {
        let state = self.files.previews.get_mut(key)?;
        let again = if result.is_ok() { state.save.written(revision) } else { state.save.failed(revision) };
        let next = again.then(|| SAVE_DEBOUNCE.saturating_sub(state.last_change.elapsed()));
        match result {
            Ok(_) => {
                if let Some(markdown) = &state.markdown {
                    markdown.update(cx, |view, cx| view.set_text(&contents, cx));
                }
                state.table = None;
                state.revision += 1;
                state.saved = contents;
            }
            Err(error) => {
                self.error = Some(format!("Could not save {}: {}", key.path, logic::error_message(&error)).into());
            }
        }
        cx.notify();
        next
    }

    /// The Files tab (`path` = None) or a file or folder tab.
    pub(in crate::app) fn render_files_surface(
        &mut self,
        path: Option<String>,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *crate::theme::tokens(cx);
        let Some(target) = self.project_target() else {
            return div()
                .p(px(16.))
                .text_sm()
                .text_color(tokens.muted_foreground)
                .child("Files are only available when a project is open.")
                .into_any_element();
        };
        let checkout = target.checkout;
        self.files.mutation = self.current_workspace_mutation();
        let Some(path) = path else {
            return self.render_tree(&checkout, None, 0, window, cx);
        };
        let key = FileKey::new(checkout.clone(), path.clone());
        self.sync_preview(&key, window, cx);
        let reveal = self.files.reveals.get(&key).map_or(0, |(_, request)| *request);
        let folder = self.files.previews.get(&key).is_some_and(|s| matches!(s.load, Load::Folder));
        let editable = self.can_write_files(&checkout.env, cx) == Some(true) && !logic::is_host_path(&path);
        let subheader = self.render_file_subheader(&key, &target.project_name, folder, editable, &tokens, cx);
        let draft = !matches!(self.target, Some(Target::Thread { .. }));
        let read_only =
            (self.can_write_files(&checkout.env, cx) == Some(false) && !logic::is_host_path(&path)).then(|| {
                div()
                    .flex_none()
                    .px(px(12.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(tokens.border)
                    .text_size(gpui_kit::rems(0.625))
                    .text_color(tokens.muted_foreground)
                    .child("Read-only connection. Unsaved edits are kept until write access returns.")
            });
        let notice = self.files.previews.get(&key).and_then(|state| match state.load {
            Load::Text { truncated: true, byte_length } if !logic::loads_full_file(&path, draft) => Some(
                div()
                    .flex_none()
                    .px(px(12.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(tokens.warning.opacity(0.2))
                    .bg(tokens.warning_surface)
                    .text_size(gpui_kit::rems(0.625))
                    .text_color(tokens.warning_foreground)
                    .child(format!(
                        "Preview limited to the first 1 MB of a {} byte file.",
                        logic::grouped(usize::try_from(byte_length).unwrap_or(usize::MAX))
                    )),
            ),
            _ => None,
        });
        let body = if folder && logic::is_host_path(&path) {
            // A host folder roots its own tree (`hostFolderPath`).
            let root = super::Checkout { env: checkout.env.clone(), cwd: path.clone() };
            self.render_tree(&root, None, reveal, window, cx)
        } else if folder {
            self.render_tree(&checkout, Some(&path), reveal, window, cx)
        } else {
            let max = EXPLORER_MAX.min(width - CONTENT_MIN).max(EXPLORER_MIN);
            let explorer_width = self.files.explorer_width.unwrap_or(EXPLORER_WIDTH).clamp(EXPLORER_MIN, max);
            // `shouldShowFileExplorer`: a host file outside the workspace shows alone.
            let explorer_shown = !self.files.explorer_hidden && !logic::is_host_path(&path);
            let content_width = if explorer_shown { width - explorer_width } else { width };
            let content = self.render_file_content(&key, &tokens, content_width, window, cx);
            let explorer = explorer_shown.then(|| {
                let ring = tokens.ring;
                let tree = self.render_tree(&checkout, Some(&path), reveal, window, cx);
                div()
                    .id("file-explorer")
                    .relative()
                    .flex_none()
                    .w(px(explorer_width))
                    .h_full()
                    .border_l_1()
                    .border_color(tokens.border.opacity(0.6))
                    .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<ExplorerResize>, _, cx| {
                        let right = f32::from(event.bounds.right());
                        let width = (right - f32::from(event.event.position.x)).clamp(EXPLORER_MIN, max);
                        this.files.explorer_width = Some(width);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .id("file-explorer-resize")
                            .tab_index(0)
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(-3.))
                            .w(px(6.))
                            .cursor_col_resize()
                            .focus_visible(move |s| s.bg(ring))
                            .on_drag(ExplorerResize, |_, _, _, cx| cx.new(|_| Empty))
                            .on_key_down(cx.listener(move |this, event: &gpui_kit::KeyDownEvent, _, cx| {
                                let modifiers = &event.keystroke.modifiers;
                                if modifiers.alt || modifiers.control || modifiers.platform {
                                    return;
                                }
                                let key = event.keystroke.key.as_str();
                                if let Some(next) = logic::explorer_key_width(explorer_width, EXPLORER_MIN, max, key) {
                                    this.files.explorer_width = Some(next);
                                    cx.stop_propagation();
                                    cx.notify();
                                }
                            })),
                    )
                    .child(tree)
            });
            h_flex()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(v_flex().flex_1().min_w_0().h_full().overflow_hidden().child(content))
                .children(explorer)
                .into_any_element()
        };
        v_flex()
            .size_full()
            .min_h_0()
            .bg(tokens.background)
            .child(subheader)
            .children(read_only)
            .children(notice)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .into_any_element()
    }

    /// What the file tab's Open in opens: the absolute path, whether it was read as a file, and
    /// whether the file manager reveals it (`revealsInFileManager`: a settled read, not a folder).
    pub(super) fn file_open_in_target(&self, key: &FileKey) -> super::open_in::FileOpenTarget {
        let state = self.files.previews.get(key);
        let settled = state.is_some_and(|s| !matches!(s.load, Load::Loading | Load::Folder));
        let reveals =
            self.env_info(&key.checkout.env).is_some_and(|info| info.config["shellRevealInFileManager"] == true);
        let file_manager =
            if settled && reveals { super::open_in::FileManager::Reveal } else { super::open_in::FileManager::Open };
        super::open_in::FileOpenTarget {
            environment: key.checkout.env.clone(),
            absolute: logic::absolute_path(&key.checkout.cwd, &key.path),
            is_file: state.is_some_and(|s| !matches!(s.load, Load::Loading | Load::Folder | Load::Failed { .. })),
            file_manager,
        }
    }

    /// The crumbs, then Open in, the rendered toggle, word wrap and the explorer toggle.
    fn render_file_subheader(
        &self,
        key: &FileKey,
        project_name: &SharedString,
        folder: bool,
        editable: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = key.path.clone();
        let state = self.files.previews.get(key);
        let source = Surface::File(path.clone());
        let crumbs = logic::breadcrumbs(&path, project_name);
        let last = crumbs.len() - 1;
        // Crumbs keep their width and the deepest stay in view, as the web scrolls them to the end.
        let mut trail = h_flex().flex_none().min_w_full().text_xs();
        let host = logic::is_host_path(&path);
        let mut parent = String::new();
        for (index, (label, crumb)) in crumbs.into_iter().enumerate() {
            let current = index == last;
            if index > 0 {
                // Host paths outside the workspace have no contents menus.
                trail = trail.child(if host {
                    Icon::new(IconName::ChevronRight)
                        .size(px(14.))
                        .mx(px(4.))
                        .flex_none()
                        .text_color(tokens.muted_foreground.opacity(0.6))
                        .into_any_element()
                } else {
                    self.crumb_menu(&key.checkout, &parent, &path, project_name, tokens, cx)
                });
            }
            parent = crumb.clone();
            let tooltip: SharedString = if crumb.is_empty() { project_name.clone() } else { crumb.clone().into() };
            let source = source.clone();
            let keep = !folder;
            trail = trail.child(
                div()
                    .id(ElementId::Name(format!("crumb:{crumb}").into()))
                    .flex_none()
                    .max_w(px(160.))
                    .truncate()
                    .px(px(2.))
                    .rounded(px(4.))
                    .map(|el| {
                        if current {
                            el.text_color(tokens.foreground).font_weight(gpui_kit::FontWeight::MEDIUM).flex_none()
                        } else {
                            el.text_color(tokens.muted_foreground)
                        }
                    })
                    .when(!current, |el| {
                        crate::app::chat::focusable(el, tokens)
                            .cursor_pointer()
                            .hover(|s| s.text_color(tokens.foreground))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.navigate_file_folder(source.clone(), &crumb, keep, cx);
                            }))
                    })
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .child(label),
            );
        }
        let action = |id: &'static str, icon: IconName, label: SharedString, pressed: bool| {
            crate::app::chat::focusable(div().id(id), tokens)
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(if pressed { tokens.foreground } else { tokens.muted_foreground })
                .when(pressed, |el| el.bg(tokens.accent))
                .hover(|s| s.bg(tokens.accent).text_color(tokens.foreground))
                .child(Icon::new(icon).size(px(14.)))
                .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        };
        let text = state.is_some_and(|s| matches!(s.load, Load::Text { .. }));
        let kind = state.map_or(logic::FileKind::Text, |s| s.kind);
        let mode = (!folder).then(|| Rendered::of(&path)).flatten();
        let rendered = mode.is_some_and(|mode| self.files.shows_rendered(key, mode));
        let shows_raw_text = text && !rendered && kind != logic::FileKind::Pdf;
        // The editor takes comments while it edits the whole file (`EditableFileSurface`).
        let whole = state.is_some_and(|s| matches!(s.load, Load::Text { truncated: false, .. }));
        let commentable = editable && whole && shows_raw_text && self.can_comment_on_file(key);
        let open_in_browser =
            !folder && matches!(kind, logic::FileKind::Html | logic::FileKind::Pdf) && self.browser_available();
        let wrap = state.and_then(|s| s.wrap).unwrap_or_else(|| self.client_settings.values.bool("wordWrap"));
        let open_in = self.render_file_open_in(self.file_open_in_target(key), tokens, cx);
        h_flex()
            .flex_none()
            .h(px(28.))
            .mb(px(12.))
            .gap(px(8.))
            .px(px(12.))
            .child(h_flex().min_w_0().flex_1().justify_end().overflow_hidden().child(trail))
            .children(open_in)
            .when_some(mode, |el, mode| {
                let icon = match (rendered, mode) {
                    (true, _) => IconName::CodeXml,
                    (false, Rendered::Table) => IconName::Table2,
                    (false, _) => IconName::Eye,
                };
                let key = key.clone();
                el.child(action("file-rendered", icon, mode.toggle_label(rendered).into(), rendered).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.files.set_rendered(&key, mode, !rendered);
                        let request = this.files.reveals.get(&key).map(|(_, request)| *request);
                        if let Some(state) = this.files.previews.get_mut(&key) {
                            state.rendered_reveal = if rendered { None } else { request };
                        }
                        cx.notify();
                    }),
                ))
            })
            .when(commentable, |el| el.child(self.render_file_comment_button(key, tokens, cx)))
            .when(shows_raw_text, |el| {
                let key = key.clone();
                let label = if wrap { "Disable word wrap" } else { "Enable word wrap" };
                el.child(action("file-wrap", IconName::TextWrap, label.into(), wrap).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if let Some(state) = this.files.previews.get_mut(&key) {
                            state.wrap = Some(!wrap);
                            cx.notify();
                        }
                    },
                )))
            })
            .when(open_in_browser, |el| {
                let key = key.clone();
                el.child(
                    action("file-open-browser", IconName::Globe, "Open file in preview browser".into(), false)
                        .on_click(cx.listener(move |this, _, window, cx| this.open_file_in_browser(&key, window, cx))),
                )
            })
            .when(!folder && !logic::is_host_path(&path), |el| {
                let open = !self.files.explorer_hidden;
                let label = if open { "Hide file explorer" } else { "Show file explorer" };
                el.child(action("file-explorer-toggle", IconName::FolderTree, label.into(), open).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.files.explorer_hidden = !this.files.explorer_hidden;
                        cx.notify();
                    }),
                ))
            })
            .into_any_element()
    }

    pub(super) fn render_file_content(
        &mut self,
        key: &FileKey,
        tokens: &Tokens,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let render_markdown = self.files.shows_rendered(key, Rendered::Markdown);
        let delimiter = logic::table_delimiter(&key.path).filter(|_| self.files.shows_rendered(key, Rendered::Table));
        let render_mermaid = self.files.shows_rendered(key, Rendered::Mermaid);
        let kind = self.files.previews.get(key).map_or(logic::FileKind::Text, |s| s.kind);
        match self.files.previews.get(key).map(|s| &s.load) {
            Some(Load::Asset) => {
                return match kind {
                    logic::FileKind::Pdf | logic::FileKind::Video | logic::FileKind::Audio => {
                        self.render_document(key, kind, window, cx)
                    }
                    // An attachment's own subheader is already above its viewer.
                    _ if !key.in_workspace() => self.render_asset_body(key, kind, tokens, window, cx),
                    _ => self.render_asset_file(key, kind, window, cx),
                };
            }
            Some(Load::Binary) => return self.render_asset_file(key, kind, window, cx),
            Some(Load::Text { .. })
                if kind == logic::FileKind::Html && self.files.shows_rendered(key, Rendered::Html) =>
            {
                return self.render_document(key, kind, window, cx);
            }
            _ => {}
        }
        let image = matches!(self.files.previews.get(key).map(|s| &s.load), Some(Load::Image))
            .then(|| self.workspace_image(key, cx));
        let media_menu = image.as_ref().and_then(|_| self.render_media_menu(key, window, cx));
        // A truncated read, a host file, an attachment or a connection without write access shows
        // the source read-only.
        let editable = key.in_workspace()
            && self.can_write_files(&key.checkout.env, cx) == Some(true)
            && !logic::is_host_path(&key.path);
        let Some(state) = self.files.previews.get_mut(key) else { return div().into_any_element() };
        let centered = |child: AnyElement| {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px(px(24.))
                .child(child)
                .into_any_element()
        };
        match &state.load {
            Load::Loading => centered(Spinner::new().large().color(tokens.muted_foreground).into_any_element()),
            Load::Image => match image.unwrap_or(WorkspaceImage::Loading) {
                WorkspaceImage::Loading => {
                    centered(Spinner::new().large().color(tokens.muted_foreground).into_any_element())
                }
                // `PreviewImage`: an attachment's image has no media menu.
                WorkspaceImage::Ready(image) if !key.in_workspace() => div()
                    .flex_1()
                    .min_h_0()
                    .p(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(img(image).max_w_full().max_h_full().object_fit(ObjectFit::Contain))
                    .into_any_element(),
                WorkspaceImage::Failed if !key.in_workspace() => {
                    let retry = key.clone();
                    let button = super::chrome::retry_button("image-retry", tokens).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if let Some(resource) = this.image_resource(&retry)
                                && let Some(images) = this.files.images.get(&retry.checkout.env)
                            {
                                images.update(cx, |images, _| images.forget(&resource));
                            }
                            cx.notify();
                        },
                    ));
                    super::chrome::failure("Unable to load image.", Some(button), tokens)
                }
                // `MediaActions`: a right click opens the image's menu; hovering names its path.
                loaded => {
                    let ready = match &loaded {
                        WorkspaceImage::Ready(image) => Some(image.clone()),
                        _ => None,
                    };
                    let tooltip: SharedString = logic::absolute_path(&key.checkout.cwd, &key.path).into();
                    let menu_key = key.clone();
                    let menu_image = ready.clone();
                    let (keyboard_key, keyboard_image, view) = (key.clone(), ready.clone(), cx.weak_entity());
                    let failed = ready.is_none();
                    // A tab stop, so the keyboard opens the image's menu (`MediaActions`).
                    crate::app::chat::focusable(div().id("workspace-image"), tokens)
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .when(failed, |el| el.items_center().justify_center().px(px(24.)))
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .on_mouse_down(
                            gpui_kit::MouseButton::Right,
                            cx.listener(move |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                                let kind = super::media::MediaKind::Image;
                                let image = menu_image.clone();
                                this.open_media_menu(&menu_key, kind, image, event.position, window, cx);
                            }),
                        )
                        .on_context_menu_key(move |position, window, cx| {
                            let image = keyboard_image.clone();
                            let _ = view.update(cx, |this, cx| {
                                let kind = super::media::MediaKind::Image;
                                this.open_media_menu(&keyboard_key, kind, image, position, window, cx);
                            });
                        })
                        .child(match ready {
                            Some(image) => self.render_preview_image(key, image, tokens, cx),
                            None => div()
                                .text_center()
                                .text_xs()
                                .line_height(gpui_kit::relative(1.625))
                                .text_color(tokens.error)
                                .child("Unable to load workspace image.")
                                .into_any_element(),
                        })
                        .children(media_menu)
                        .into_any_element()
                }
            },
            Load::Binary | Load::Asset => div().into_any_element(),
            Load::Failed { message, .. } if !key.in_workspace() => {
                let retry = key.clone();
                let button = super::chrome::retry_button("attachment-retry", tokens).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.files.previews.remove(&retry);
                        cx.notify();
                    },
                ));
                super::chrome::failure(message.clone(), Some(button), tokens)
            }
            Load::Failed { message, attempted, cwd } => {
                let workspace = cwd.clone().unwrap_or_else(|| key.checkout.cwd.clone());
                centered(
                    v_flex()
                        .gap(px(12.))
                        .text_center()
                        .text_xs()
                        .line_height(gpui_kit::relative(1.625))
                        .child(div().text_color(tokens.error).child(message.clone()))
                        .children(attempted.clone().map(|attempted| {
                            v_flex().text_color(tokens.muted_foreground).child("Attempted path").child(
                                div().font_family(tokens.fonts.mono).text_color(tokens.foreground).child(attempted),
                            )
                        }))
                        .child(div().text_color(tokens.muted_foreground).child(format!(
                            "Workspace folder: {workspace}. Check the link's path or locate the file in Files."
                        )))
                        .into_any_element(),
                )
            }
            Load::Folder => div().into_any_element(),
            Load::Text { truncated, .. } => {
                if let Some(full) = &state.full {
                    let full = match full {
                        FullFile::Loading => None,
                        FullFile::Ready { editor, table } => {
                            // `LargeTextViewer` with the delimiter: the table, or the source.
                            let table = table.clone().filter(|_| delimiter.is_some());
                            Some(Ok((editor.clone(), table)))
                        }
                        FullFile::Failed => Some(Err(())),
                    };
                    if !key.in_workspace() {
                        return self.render_full_body(key, full, width, tokens, cx);
                    }
                    return self.render_full_file(key, full, width, tokens, cx);
                }
                if logic::is_mermaid(&key.path) && render_mermaid {
                    let palette = self.diagrams.palette(tokens);
                    let id = crate::app::mermaid::diagram_key(&state.saved, &palette, true);
                    let source = self.diagrams.file_source(id).is_none().then(|| state.saved.clone());
                    return self.render_mermaid(id, source, window, cx);
                }
                if let Some(delimiter) = delimiter {
                    let saved = &state.saved;
                    let table = state
                        .table
                        .get_or_insert_with(|| std::rc::Rc::new(super::table::Table::parse(saved, delimiter)))
                        .clone();
                    return self.render_table(&key.path, table, width, tokens, cx);
                }
                if render_markdown && logic::is_markdown(&key.path) {
                    let saved = state.saved.clone();
                    let view =
                        state.markdown.get_or_insert_with(|| cx.new(|cx| TextViewState::markdown(&saved, cx))).clone();
                    // `onTaskListChange`: a task's checkbox edits the file, which saves as typing does.
                    let toggle = (editable && !*truncated).then(|| {
                        let (this, key) = (cx.entity().downgrade(), key.clone());
                        move |item_start: usize, checked: bool, window: &mut Window, cx: &mut gpui_kit::App| {
                            let _ = this.update(cx, |this, cx| {
                                this.toggle_markdown_task(&key, item_start, checked, window, cx);
                            });
                        }
                    });
                    return div()
                        .id("file-markdown")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(px(24.))
                        .py(px(16.))
                        .child(TextView::new(&view).when_some(toggle, TextView::on_task_toggle))
                        .into_any_element();
                }
                let Some(editor) = state.editor.clone() else { return div().into_any_element() };
                let read_only = *truncated || !editable;
                let editor = Editor::new(&editor)
                    .appearance(false)
                    .bordered(false)
                    .readonly(read_only)
                    .h_full()
                    .font_family(tokens.fonts.mono)
                    .into_any_element();
                // `EditableFileSurface` takes line comments; the read-only source does not.
                if read_only { editor } else { self.with_file_comments(key, editor, tokens, cx) }
            }
        }
    }

    /// `WorkspaceLargeTextPreview`: "Full file" with Copy contents and Save file, then the text.
    fn render_full_file(
        &mut self,
        key: &FileKey,
        full: Option<Result<FullView, ()>>,
        width: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = tokens.muted_foreground;
        let action = |id: &'static str, icon: IconName, label: &'static str| {
            crate::app::chat::focusable(div().id(id), tokens)
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(muted)
                .hover(|s| s.bg(tokens.accent).text_color(tokens.foreground))
                .child(Icon::new(icon).size(px(14.)))
                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        };
        let copied = full.clone().and_then(Result::ok).map(|(editor, _)| editor);
        let save_key = key.clone();
        let header = h_flex()
            .flex_none()
            .h(px(28.))
            .mb(px(12.))
            .gap(px(8.))
            .px(px(12.))
            .child(div().flex_1().min_w_0().text_xs().text_color(muted).child("Full file"))
            .child(action("full-file-copy", IconName::Copy, "Copy contents").on_click(move |_, _, cx| {
                if let Some(editor) = &copied {
                    let text = editor.read(cx).value().to_string();
                    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text));
                }
            }))
            .child(
                action("full-file-save", IconName::Download, "Save file")
                    .on_click(cx.listener(move |this, _, window, cx| this.save_full_file(&save_key, window, cx))),
            );
        let body = self.render_full_body(key, full, width, tokens, cx);
        v_flex().flex_1().min_h_0().child(header).child(body).into_any_element()
    }

    /// The whole text of [`Self::render_full_file`], or its table, without the "Full file" row.
    fn render_full_body(
        &mut self,
        key: &FileKey,
        full: Option<Result<FullView, ()>>,
        width: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = tokens.muted_foreground;
        let retry_key = key.clone();
        match full {
            None => div().p(px(24.)).text_xs().text_color(muted).child("Loading…").into_any_element(),
            Some(Err(())) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(px(12.))
                .px(px(24.))
                .text_xs()
                .child(div().text_color(tokens.error).child("Unable to load file preview."))
                .child(
                    crate::app::chat::focusable(div().id("full-file-retry"), tokens)
                        .px(px(10.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(tokens.input)
                        .cursor_pointer()
                        .hover(|s| s.bg(tokens.accent))
                        .child("Retry")
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.load_full_file(retry_key.clone(), window, cx)),
                        ),
                )
                .into_any_element(),
            Some(Ok((_, Some(table)))) => self.render_table(&key.path, table, width, tokens, cx),
            Some(Ok((editor, None))) => Editor::new(&editor)
                .appearance(false)
                .bordered(false)
                .readonly(true)
                .h_full()
                .font_family(tokens.fonts.mono)
                .into_any_element(),
        }
    }

    /// `PreviewImageContent`: the image's size and zoom controls over the image, which scrolls
    /// once it outgrows the panel and opens the image viewer on a click.
    fn render_preview_image(
        &mut self,
        key: &FileKey,
        image: std::sync::Arc<gpui_kit::Image>,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::app::image_viewer::{self as viewer, Measured};
        let measured = self.image_natural_size(&image, cx);
        if measured == Measured::Failed {
            return super::chrome::failure("Unable to load image.", None, tokens);
        }
        let Some(state) = self.files.previews.get(key) else { return div().into_any_element() };
        let (zoom, viewport) = (state.image_zoom.zoom, state.image_zoom.viewport.clone());
        // Sized once both the image and the panel are measured; until then it is contained.
        let natural = measured.size();
        let sized = natural.zip(viewport.get());
        let scale =
            zoom.unwrap_or_else(|| sized.map_or(1., |(natural, viewport)| viewer::preview_fit(natural, viewport)));
        let label = match natural {
            Some(natural) => {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "natural sides are whole pixels"
                )]
                let side = |value: f32| crate::app::git::group_thousands(value as u64);
                format!("{} × {}", side(natural.width), side(natural.height))
            }
            None => "Loading image…".to_owned(),
        };
        let set_zoom = |id: &'static str,
                        icon: IconName,
                        label: &'static str,
                        next: Option<f32>,
                        disabled: bool,
                        pressed: bool| {
            let key = key.clone();
            super::chrome::action(id, icon, label, tokens)
                .when(pressed, |el| el.bg(tokens.accent).text_color(tokens.foreground))
                .when(disabled, |el| el.opacity(0.64).cursor_default())
                .on_click(cx.listener(move |this, _, _, cx| {
                    if disabled {
                        return;
                    }
                    if let Some(state) = this.files.previews.get_mut(&key) {
                        state.image_zoom.zoom = next;
                        cx.notify();
                    }
                }))
        };
        let known = natural.is_some();
        let out = viewer::preview_zoom(scale, 1. / viewer::PREVIEW_ZOOM_STEP);
        let r#in = viewer::preview_zoom(scale, viewer::PREVIEW_ZOOM_STEP);
        let header = h_flex()
            .flex_none()
            .min_h(px(40.))
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .border_b_1()
            .border_color(tokens.border.opacity(0.6))
            .px(px(12.))
            .py(px(4.))
            .text_xs()
            .text_color(tokens.muted_foreground)
            .child(div().child(label))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(4.))
                    .child(set_zoom(
                        "image-zoom-out",
                        IconName::Minus,
                        "Zoom out",
                        Some(out),
                        !known || scale <= viewer::PREVIEW_MIN_ZOOM,
                        false,
                    ))
                    .child(div().min_w(px(40.)).text_center().child(format!("{}%", (scale * 100.).round())))
                    .child(set_zoom(
                        "image-zoom-in",
                        IconName::Plus,
                        "Zoom in",
                        Some(r#in),
                        !known || scale >= viewer::PREVIEW_MAX_ZOOM,
                        false,
                    ))
                    .child(set_zoom("image-fit", IconName::Scan, "Fit image", None, false, zoom.is_none()))
                    .child(set_zoom(
                        "image-actual-size",
                        IconName::Maximize,
                        "Actual image size",
                        Some(1.),
                        false,
                        zoom == Some(1.),
                    )),
            );
        let item = viewer::ViewerImage {
            image: image.clone(),
            name: logic::file_name(&key.path).to_owned().into(),
            reference: Some(viewer::file_reference(
                &logic::absolute_path(&key.checkout.cwd, &key.path),
                Some(&key.checkout.cwd),
            )),
        };
        let ring = tokens.ring;
        let picture = img(image)
            .id("workspace-image-zoom")
            .flex_none()
            .tab_index(0)
            .cursor_pointer()
            .focus_visible(move |style| style.border_2().border_color(ring))
            .map(|el| match sized {
                Some((natural, _)) => el.w(px(natural.width * scale)).h(px(natural.height * scale)),
                None => el.max_w_full().object_fit(ObjectFit::Contain),
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.open_image_viewer(vec![item.clone()], 0, window, cx);
            }));
        let entity = cx.entity_id();
        let measure = gpui_kit::canvas(
            move |bounds, _, cx| {
                let measured = gpui_kit::size(f32::from(bounds.size.width), f32::from(bounds.size.height));
                if viewport.get() != Some(measured) {
                    viewport.set(Some(measured));
                    cx.notify(entity);
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_full();
        v_flex()
            .flex_1()
            .min_h_0()
            .w_full()
            .child(header)
            .child(
                div().relative().flex_1().min_h_0().child(measure).child(
                    div().id("workspace-image-scroll").absolute().size_full().overflow_scroll().child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .p(px(16.))
                            .min_w(gpui_kit::relative(1.))
                            .min_h(gpui_kit::relative(1.))
                            .when_some(sized, |el, (natural, _)| {
                                el.w(px(natural.width * scale + 32.)).h(px(natural.height * scale + 32.))
                            })
                            .child(picture),
                    ),
                ),
            )
            .into_any_element()
    }

    /// An image file, fetched through `assets.createUrl` like the web's `WorkspaceImagePreview`.
    fn workspace_image(&mut self, key: &FileKey, cx: &mut Context<Self>) -> WorkspaceImage {
        if let super::Origin::Draft { local_id } = &key.origin {
            return match self.draft_image(local_id, &key.path) {
                Some(image) => WorkspaceImage::Ready(image),
                None => WorkspaceImage::Failed,
            };
        }
        let Some(resource) = self.image_resource(key) else { return WorkspaceImage::Loading };
        let env = &key.checkout.env;
        let (client, endpoint) = (self.env_client(env), self.env_endpoint(env).cloned());
        let images = self.files.images.entry(env.clone()).or_insert_with(|| cx.new(|_| Default::default())).clone();
        images.update(cx, |images, cx| match images.get(resource.clone(), client, endpoint, cx) {
            Some(image) => WorkspaceImage::Ready(image),
            None if images.failed(&resource) => WorkspaceImage::Failed,
            None => WorkspaceImage::Loading,
        })
    }

    fn image_resource(&self, key: &FileKey) -> Option<Value> {
        if !key.in_workspace() {
            return self.attachment_resource(key, false);
        }
        let absolute = logic::absolute_path(&key.checkout.cwd, &key.path);
        Some(match &self.target {
            Some(Target::Thread { id: thread_id, .. }) => {
                json!({"_tag": "media-file", "threadId": thread_id, "path": absolute})
            }
            _ => json!({"_tag": "draft-workspace-file", "cwd": key.checkout.cwd, "path": absolute}),
        })
    }

    /// The editor the details card's Open in launches.
    pub(in crate::app) fn preferred_editor_id(&self) -> Option<String> {
        let available = &self.server_info().available_editors;
        self.preferred_editor.clone().filter(|editor| available.contains(editor)).or_else(|| available.first().cloned())
    }
}

/// What an attachment's subheader shows of its text (`content`, `streamsText`).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TextStatus {
    /// The first megabyte, or the whole file, has loaded.
    pub loaded: bool,
    /// Only the first megabyte shows, with its notice; Copy copies that preview.
    pub cut: bool,
    /// The word wrap toggle; unset, it follows `wordWrap`.
    pub wrap: Option<bool>,
}

impl Workspace {
    /// Reads an attachment's first megabyte as text (`AttachmentFilePreview`'s `readFilePreviewResponse`).
    fn read_attachment_text(&mut self, key: FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let fetch = self.file_fetch(&key, None, false);
        let byte_length = self.attachment_size(&key);
        let Some(state) = self.files.previews.get_mut(&key) else { return };
        let Some(fetch) = fetch else {
            let message = super::asset::FetchError::Unsigned.to_string();
            state.load = Load::Failed { message, attempted: None, cwd: None };
            return;
        };
        let request_key = key.clone();
        state._read = Some(cx.spawn_in(window, async move |this, cx| {
            let read = match fetch.head(super::attachment::TEXT_HEAD_BYTES).await {
                Ok(bytes) => cx.background_spawn(async move { super::attachment::decode_preview_text(&bytes) }).await,
                Err(error) => Err(error.to_string()),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(state) = this.files.previews.get_mut(&request_key) else { return };
                state._read = None;
                match read {
                    Ok((text, truncated)) => show_text(state, &request_key, text, truncated, byte_length, window, cx),
                    Err(message) => state.load = Load::Failed { message, attempted: None, cwd: None },
                }
                cx.notify();
            });
        }));
    }

    pub(super) fn attachment_text_status(&self, key: &FileKey) -> TextStatus {
        let Some(state) = self.files.previews.get(key) else { return TextStatus::default() };
        let Load::Text { truncated, .. } = state.load else { return TextStatus::default() };
        let full = match &state.full {
            None => None,
            Some(FullFile::Ready { .. }) => Some(true),
            Some(FullFile::Loading | FullFile::Failed) => Some(false),
        };
        TextStatus { loaded: full != Some(false), cut: truncated && full.is_none(), wrap: state.wrap }
    }

    /// The text Copy contents copies: the whole file once it loaded, else the preview.
    pub(super) fn attachment_text(&self, key: &FileKey, cx: &gpui_kit::App) -> Option<String> {
        let state = self.files.previews.get(key)?;
        match &state.full {
            Some(FullFile::Ready { editor, .. }) => Some(editor.read(cx).value().to_string()),
            Some(FullFile::Loading | FullFile::Failed) => None,
            None => matches!(state.load, Load::Text { .. }).then(|| state.saved.clone()),
        }
    }

    pub(super) fn set_preview_wrap(&mut self, key: &FileKey, wrap: bool) {
        if let Some(state) = self.files.previews.get_mut(key) {
            state.wrap = Some(wrap);
        }
    }

    /// Forgets everything a closed draft preview kept.
    pub(super) fn forget_preview(&mut self, key: &FileKey) {
        self.files.previews.remove(key);
        self.files.assets.forget(key);
        self.files.documents.forget(key);
        self.files.attachment_source.remove(key);
    }
}

/// Shows a read's text: in the editor (made on the first read), the rendered Markdown and, as
/// rebuilt from `saved`, the table. A new read (after a workspace mutation) loads the whole
/// file again.
pub(super) fn show_text(
    state: &mut PreviewState,
    key: &FileKey,
    contents: String,
    truncated: bool,
    byte_length: u64,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    state.full = None;
    state.load = Load::Text { truncated, byte_length };
    if let Some(markdown) = &state.markdown {
        markdown.update(cx, |view, cx| view.set_text(&contents, cx));
    }
    match &state.editor {
        Some(editor) if *editor.read(cx).value() != *contents => {
            let text = contents.clone();
            editor.update(cx, |editor, cx| editor.set_value(text, window, cx));
            state.marks.reset();
        }
        Some(_) => {}
        None => {
            let language = logic::language_for_path(&key.path);
            let text = contents.clone();
            let editor = cx.new(|cx| EditorState::new(window, cx).language(language).default_value(text));
            let save_key = key.clone();
            state._subscription = Some(cx.subscribe_in(&editor, window, move |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.remap_file_comments(&save_key, cx);
                    this.change_file(&save_key, cx);
                }
            }));
            state.editor = Some(editor);
            state.applied_wrap = None;
        }
    }
    state.table = None;
    state.revision += 1;
    state.saved = contents;
}

/// `assets.createUrl`, then the file's bytes over HTTP; refused past 256 MB like the web's viewer.
async fn fetch_asset(
    client: crate::rpc::RpcClient,
    endpoint: crate::backend::Endpoint,
    resource: Value,
) -> anyhow::Result<Vec<u8>> {
    let minted = client.request("assets.createUrl", json!({"resource": resource})).await?;
    let url = document::resolve_asset_url(&endpoint.http_base, minted["relativeUrl"].as_str().unwrap_or_default());
    let token = document::is_server_url(&url, &endpoint.http_base).then_some(endpoint.bearer_token);
    crate::runtime()
        .spawn(async move {
            let mut request = asset::http().get(url);
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            let response = request.send().await?.error_for_status()?;
            asset::read_capped(response, FULL_FILE_MAX_BYTES).await?.context(FULL_FILE_TOO_LARGE)
        })
        .await?
}

#[cfg(test)]
mod tests {
    use super::{Load, PreviewState, reveal_handled};

    #[test]
    fn a_text_read_runs_again_on_a_new_socket_until_it_lands() {
        let (first, second) = (crate::rpc::RpcClient::unopened(), crate::rpc::RpcClient::unopened());
        let mut state = PreviewState::new("src/main.rs");
        assert!(state.needs_read(&first.socket_id()), "opened offline: nothing read yet");
        state.read_on = Some(first.socket_id());
        state._read = Some(gpui_kit::Task::ready(()));
        assert!(!state.needs_read(&first.socket_id()), "a read is running");
        state._read = None;
        state.load = Load::Failed { message: "closed".into(), attempted: None, cwd: None };
        assert!(!state.needs_read(&first.socket_id()), "the failure stands on that socket");
        assert!(state.needs_read(&second.socket_id()), "a reconnect reads again");
        state.load = Load::Text { truncated: false, byte_length: 0 };
        assert!(!state.needs_read(&second.socket_id()), "read");
    }

    #[test]
    fn a_line_reveal_holds_the_source_until_the_rendered_view_is_picked() {
        assert!(reveal_handled(None, None));
        assert!(reveal_handled(Some((None, 3)), None), "an open without a line keeps the rendered view");
        assert!(!reveal_handled(Some((Some(12), 3)), None));
        assert!(reveal_handled(Some((Some(12), 3)), Some(3)), "picking the rendered view handles the reveal");
        assert!(!reveal_handled(Some((Some(12), 4)), Some(3)), "a newer line request shows the source again");
    }
}
