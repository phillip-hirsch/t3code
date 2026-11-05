//! The file surface (`FilePreviewPanel.tsx`): breadcrumbs and view toggles over the file, with
//! the Files tree beside it. Text opens in a highlighted editor that saves as you type
//! (`projects.writeFile`, 500 ms after the last edit), Markdown can render, images show, and a
//! folder path shows the tree with that folder selected.

use std::time::{Duration, Instant};

use anyhow::Context as _;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, InputEvent, Position};
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

/// `LARGE_TEXT_MAX_BYTES`.
const FULL_FILE_MAX_BYTES: u64 = 256 * 1024 * 1024;

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
    editor: Option<Entity<EditorState>>,
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
    revealed: Option<u64>,
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
            revealed: None,
            _read: None,
            _full: None,
            _save: None,
            _subscription: None,
        }
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

impl super::FilesState {
    pub(super) fn shows_rendered(&self, mode: Rendered) -> bool {
        match mode {
            Rendered::Markdown => self.render_markdown,
            Rendered::Html => !self.html_source,
            Rendered::Table => !self.table_source,
            Rendered::Mermaid => !self.mermaid_source,
        }
    }

    pub(super) fn set_rendered(&mut self, mode: Rendered, rendered: bool) {
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
        if let (Some(line), Some(target)) = (line, self.project_target()) {
            self.files.reveal_requests += 1;
            let key = FileKey { checkout: target.checkout, path: path.clone() };
            self.files.reveals.insert(key, (line.max(1), self.files.reveal_requests));
        }
        self.open_file_surface(path, window, cx);
    }

    /// Reads the file once, and again after a workspace mutation unless edits are unsaved; then
    /// applies the wrap toggle and a pending line reveal.
    fn sync_preview(&mut self, key: &FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let mutation = self.files.mutation.clone();
        let wrap_default = self.client_settings.values.bool("wordWrap");
        let reveal = self.files.reveals.get(key).copied();
        let state = self.files.previews.entry(key.clone()).or_insert_with(|| PreviewState::new(&key.path));
        let fresh = state.mutation.is_none();
        let changed = state.mutation.as_ref().is_some_and(|m| *m != mutation);
        let dirty = state.save.pending();
        state.mutation = Some(mutation);
        if state.kind == logic::FileKind::Image {
            state.load = Load::Image;
        } else if state.kind.is_asset() {
            state.load = Load::Asset;
        } else if fresh || (changed && !dirty) {
            self.read_file(key.clone(), window, cx);
        }
        let draft = !matches!(self.target, Some(Target::Thread { .. }));
        let Some(state) = self.files.previews.get_mut(key) else { return };
        if matches!(state.load, Load::Text { truncated: true, .. })
            && state.full.is_none()
            && logic::loads_full_file(&key.path, draft)
        {
            self.load_full_file(key.clone(), window, cx);
        }
        let Some(state) = self.files.previews.get_mut(key) else { return };
        let Some(editor) = state.editor.clone() else { return };
        let wrap = state.wrap.unwrap_or(wrap_default);
        if state.applied_wrap != Some(wrap) {
            state.applied_wrap = Some(wrap);
            editor.update(cx, |editor, cx| editor.set_soft_wrap(wrap, window, cx));
        }
        if let Some((line, request)) = reveal.filter(|(_, request)| state.revealed != Some(*request)) {
            state.revealed = Some(request);
            editor.update(cx, |editor, cx| editor.set_cursor_position(Position::new(line - 1, 0), window, cx));
            // The caret moves without taking focus: the composer keeps it, as when the web's palette closes.
            self.focus_composer(window, cx);
        }
    }

    fn read_file(&mut self, key: FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(&key.checkout.env) else { return };
        let Some(state) = self.files.previews.get_mut(&key) else { return };
        let revision = state.save.revision();
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
                // A new read (after a workspace mutation) loads the whole file again.
                state.full = None;
                state.load = Load::Text { truncated, byte_length: value["byteLength"].as_u64().unwrap_or(0) };
                if let Some(markdown) = &state.markdown {
                    markdown.update(cx, |view, cx| view.set_text(&contents, cx));
                }
                match &state.editor {
                    Some(editor) if *editor.read(cx).value() != *contents => {
                        let text = contents.clone();
                        editor.update(cx, |editor, cx| editor.set_value(text, window, cx));
                    }
                    Some(_) => {}
                    None => {
                        let language = logic::language_for_path(&key.path);
                        let text = contents.clone();
                        let editor = cx.new(|cx| EditorState::new(window, cx).language(language).default_value(text));
                        let save_key = key.clone();
                        state._subscription =
                            Some(cx.subscribe_in(&editor, window, move |this, _, event: &InputEvent, _, cx| {
                                if matches!(event, InputEvent::Change) {
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
        let env = &key.checkout.env;
        Some((self.env_client(env)?, self.env_endpoint(env)?.clone()))
    }

    fn load_full_file(&mut self, key: FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((client, endpoint)), Some(resource)) = (self.file_connection(&key), self.media_resource(&key)) else {
            return;
        };
        let Some(state) = self.files.previews.get_mut(&key) else { return };
        state.full = Some(FullFile::Loading);
        let request_key = key.clone();
        let delimiter = logic::table_delimiter(&key.path);
        state._full = Some(cx.spawn_in(window, async move |this, cx| {
            let fetched = fetch_asset(client, endpoint, resource).await;
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
                state.full = Some(match text {
                    Ok((text, cells)) => {
                        let editor = cx.new(|cx| EditorState::new(window, cx).language("text").default_value(text));
                        let table = cells.map(|cells| std::rc::Rc::new(super::table::Table::new(cells)));
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
        let (Some((client, endpoint)), Some(resource)) = (self.file_connection(key), self.media_resource(key)) else {
            return;
        };
        let directory = std::env::home_dir().map(|home| home.join("Downloads")).unwrap_or_default();
        let picked = cx.prompt_for_new_path(&directory, Some(logic::file_name(&key.path)));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let written = match fetch_asset(client, endpoint, resource).await {
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
        if !state.save.change() {
            return;
        }
        let key = key.clone();
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

    /// The write's socket is the environment the file was opened on, whatever is open now.
    fn start_write(&mut self, key: &FileKey, cx: &mut Context<Self>) -> Option<(crate::rpc::RpcClient, u64, String)> {
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
        let key = FileKey { checkout: checkout.clone(), path: path.clone() };
        self.sync_preview(&key, window, cx);
        let reveal = self.files.reveals.get(&key).map_or(0, |(_, request)| *request);
        let folder = self.files.previews.get(&key).is_some_and(|s| matches!(s.load, Load::Folder));
        let subheader = self.render_file_subheader(&key, &target.project_name, folder, &tokens, cx);
        let draft = !matches!(self.target, Some(Target::Thread { .. }));
        let notice = self.files.previews.get(&key).and_then(|state| match state.load {
            Load::Text { truncated: true, byte_length } if !logic::loads_full_file(&path, draft) => Some(
                div()
                    .flex_none()
                    .px(px(12.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(tokens.warning.opacity(0.2))
                    .bg(tokens.warning.opacity(0.08))
                    .text_size(gpui_kit::rems(0.625))
                    .text_color(tokens.warning_foreground)
                    .child(format!(
                        "Preview limited to the first 1 MB of a {} byte file.",
                        logic::grouped(byte_length as usize)
                    )),
            ),
            _ => None,
        });
        let body = if folder {
            self.render_tree(&checkout, Some(&path), reveal, window, cx)
        } else {
            let max = EXPLORER_MAX.min(width - CONTENT_MIN).max(EXPLORER_MIN);
            let explorer_width = self.files.explorer_width.unwrap_or(EXPLORER_WIDTH).clamp(EXPLORER_MIN, max);
            // `shouldShowFileExplorer`: a host file outside the workspace shows alone.
            let explorer_shown = !self.files.explorer_hidden && !logic::is_host_path(&path);
            let content_width = if explorer_shown { width - explorer_width } else { width };
            let content = self.render_file_content(&key, &tokens, content_width, window, cx);
            let explorer = explorer_shown.then(|| {
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
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .left(px(-3.))
                            .w(px(6.))
                            .cursor_col_resize()
                            .on_drag(ExplorerResize, |_, _, _, cx| cx.new(|_| Empty)),
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
            .children(notice)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .into_any_element()
    }

    /// The crumbs, then Open in, the rendered toggle, word wrap and the explorer toggle.
    fn render_file_subheader(
        &self,
        key: &FileKey,
        project_name: &SharedString,
        folder: bool,
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
                    self.crumb_menu(&key.checkout, &parent, &path, tokens, cx)
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
                        el.cursor_pointer().hover(|s| s.text_color(tokens.foreground)).on_click(cx.listener(
                            move |this, _, _, cx| this.navigate_file_folder(source.clone(), &crumb, keep, cx),
                        ))
                    })
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .child(label),
            );
        }
        let action = |id: &'static str, icon: IconName, label: SharedString, pressed: bool| {
            div()
                .id(id)
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
        let rendered = mode.is_some_and(|mode| self.files.shows_rendered(mode));
        let shows_raw_text = text && !rendered && kind != logic::FileKind::Pdf;
        let open_in_browser =
            !folder && matches!(kind, logic::FileKind::Html | logic::FileKind::Pdf) && self.browser_available();
        let wrap = state.and_then(|s| s.wrap).unwrap_or_else(|| self.client_settings.values.bool("wordWrap"));
        let absolute = logic::absolute_path(&key.checkout.cwd, &path);
        // `revealsInFileManager`: a read file (not a folder) reveals in the file manager.
        let file_manager = if !folder
            && state.is_some_and(|s| !matches!(s.load, Load::Loading))
            && self.server_info().config["shellRevealInFileManager"] == true
        {
            super::open_in::FileManager::Reveal
        } else {
            super::open_in::FileManager::Open
        };
        let open_in = self.render_file_open_in(absolute, file_manager, tokens, cx);
        h_flex()
            .flex_none()
            .h(px(28.))
            .mb(px(12.))
            .gap(px(8.))
            .px(px(12.))
            .child(h_flex().min_w_0().flex_1().justify_end().overflow_hidden().child(trail))
            .child(open_in)
            .when_some(mode, |el, mode| {
                let icon = match (rendered, mode) {
                    (true, _) => IconName::CodeXml,
                    (false, Rendered::Table) => IconName::Table2,
                    (false, _) => IconName::Eye,
                };
                el.child(action("file-rendered", icon, mode.toggle_label(rendered).into(), rendered).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.files.set_rendered(mode, !rendered);
                        cx.notify();
                    }),
                ))
            })
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

    fn render_file_content(
        &mut self,
        key: &FileKey,
        tokens: &Tokens,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let render_markdown = self.files.render_markdown;
        let delimiter = logic::table_delimiter(&key.path).filter(|_| self.files.shows_rendered(Rendered::Table));
        let render_mermaid = self.files.shows_rendered(Rendered::Mermaid);
        let kind = self.files.previews.get(key).map_or(logic::FileKind::Text, |s| s.kind);
        match self.files.previews.get(key).map(|s| &s.load) {
            Some(Load::Asset) => {
                return match kind {
                    logic::FileKind::Pdf | logic::FileKind::Video | logic::FileKind::Audio => {
                        self.render_document(key, kind, window, cx)
                    }
                    _ => self.render_asset_file(key, kind, width, window, cx),
                };
            }
            Some(Load::Binary) => return self.render_asset_file(key, kind, width, window, cx),
            Some(Load::Text { .. }) if kind == logic::FileKind::Html && self.files.shows_rendered(Rendered::Html) => {
                return self.render_document(key, kind, window, cx);
            }
            _ => {}
        }
        let image = matches!(self.files.previews.get(key).map(|s| &s.load), Some(Load::Image))
            .then(|| self.workspace_image(key, cx));
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
            Load::Image => centered(match image.flatten() {
                Some(image) => img(image).max_w_full().max_h_full().object_fit(ObjectFit::Contain).into_any_element(),
                None => Spinner::new().large().color(tokens.muted_foreground).into_any_element(),
            }),
            Load::Binary | Load::Asset => div().into_any_element(),
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
                    return self.render_full_file(key, full, width, tokens, cx);
                }
                if logic::is_mermaid(&key.path) && render_mermaid {
                    let revision = state.revision;
                    let source = self.files.documents.stale(key, revision).then(|| state.saved.clone());
                    return self.render_mermaid(key, revision, source, window, cx);
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
                    return div()
                        .id("file-markdown")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px(px(24.))
                        .py(px(16.))
                        .child(TextView::new(&view))
                        .into_any_element();
                }
                let Some(editor) = state.editor.clone() else { return div().into_any_element() };
                Editor::new(&editor)
                    .appearance(false)
                    .bordered(false)
                    .readonly(*truncated)
                    .h_full()
                    .font_family(tokens.fonts.mono)
                    .into_any_element()
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
            div()
                .id(id)
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
        let retry_key = key.clone();
        let body = match full {
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
                    div()
                        .id("full-file-retry")
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
        };
        v_flex().flex_1().min_h_0().child(header).child(body).into_any_element()
    }

    /// An image file, fetched through `assets.createUrl` like the web's `WorkspaceImagePreview`.
    fn workspace_image(&mut self, key: &FileKey, cx: &mut Context<Self>) -> Option<std::sync::Arc<gpui_kit::Image>> {
        let absolute = logic::absolute_path(&key.checkout.cwd, &key.path);
        let resource = match &self.target {
            Some(Target::Thread { id: thread_id, .. }) => {
                json!({"_tag": "media-file", "threadId": thread_id, "path": absolute})
            }
            _ => json!({"_tag": "draft-workspace-file", "cwd": key.checkout.cwd, "path": absolute}),
        };
        let env = &key.checkout.env;
        let (client, endpoint) = (self.env_client(env), self.env_endpoint(env).cloned());
        let images = self.files.images.entry(env.clone()).or_insert_with(|| cx.new(|_| Default::default())).clone();
        images.update(cx, |images, cx| images.get(resource, client, endpoint, cx))
    }

    /// The editor the details card's Open in launches.
    pub(in crate::app) fn preferred_editor_id(&self) -> Option<String> {
        let available = &self.server_info().available_editors;
        self.preferred_editor.clone().filter(|editor| available.contains(editor)).or_else(|| available.first().cloned())
    }
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
            asset::read_capped(response, FULL_FILE_MAX_BYTES)
                .await?
                .context("Text previews support files up to 256 MB.")
        })
        .await?
}
