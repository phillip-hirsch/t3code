//! The file surface (`FilePreviewPanel.tsx`): breadcrumbs and view toggles over the file, with
//! the Files tree beside it. Text opens in a highlighted editor that saves as you type
//! (`projects.writeFile`, 500 ms after the last edit), Markdown can render, images show, and a
//! folder path shows the tree with that folder selected.

use std::time::Duration;

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

use super::logic;
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

/// The whole file behind a read cut at 1 MB, read-only and unhighlighted like `LargeTextViewer`.
enum FullFile {
    Loading,
    Ready(Entity<EditorState>),
    Failed,
}

enum Load {
    Loading,
    Text { truncated: bool, byte_length: u64 },
    Folder,
    Binary,
    Image,
    Failed { message: String, attempted: Option<String>, cwd: Option<String> },
}

pub(super) struct PreviewState {
    load: Load,
    editor: Option<Entity<EditorState>>,
    /// The contents as last read or written; edits that match it don't save.
    saved: String,
    /// The editor holds edits not yet written.
    dirty: bool,
    /// A write is waiting or in flight.
    saving: bool,
    markdown: Option<Entity<TextViewState>>,
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
    fn new() -> Self {
        Self {
            load: Load::Loading,
            editor: None,
            saved: String::new(),
            dirty: false,
            saving: false,
            markdown: None,
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

type Key = (String, String);

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
            self.files.reveals.insert((target.cwd, path.clone()), (line.max(1), self.files.reveal_requests));
        }
        self.open_file_surface(path, window, cx);
    }

    /// Reads the file once, and again after a workspace mutation unless edits are unsaved; then
    /// applies the wrap toggle and a pending line reveal.
    fn sync_preview(&mut self, key: &Key, window: &mut Window, cx: &mut Context<Self>) {
        let mutation = self.files.mutation.clone();
        let wrap_default = self.client_settings.values.bool("wordWrap");
        let reveal = self.files.reveals.get(key).copied();
        let state = self.files.previews.entry(key.clone()).or_insert_with(PreviewState::new);
        let fresh = state.mutation.is_none();
        let changed = state.mutation.as_ref().is_some_and(|m| *m != mutation);
        let dirty = state.saving || state.dirty;
        state.mutation = Some(mutation);
        if logic::is_image(&key.1) {
            state.load = Load::Image;
        } else if fresh || (changed && !dirty) {
            self.read_file(key.clone(), window, cx);
        }
        let draft = !matches!(self.target, Some(Target::Thread(_)));
        let Some(state) = self.files.previews.get_mut(key) else { return };
        if matches!(state.load, Load::Text { truncated: true, .. })
            && state.full.is_none()
            && logic::loads_full_file(&key.1, draft)
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

    fn read_file(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client() else { return };
        let payload = json!({"cwd": key.0, "relativePath": key.1});
        let request_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = client.request("projects.readFile", payload).await;
            let _ = this.update_in(cx, |this, window, cx| this.apply_read(&request_key, result, window, cx));
        });
        if let Some(state) = self.files.previews.get_mut(&key) {
            state._read = Some(task);
        }
    }

    fn apply_read(&mut self, key: &Key, result: Result<Value, RpcError>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.files.previews.get_mut(key) else { return };
        state._read = None;
        match result {
            Ok(value) if value["exists"] == false => {
                state.load =
                    Load::Failed { message: format!("'{}' does not exist.", key.1), attempted: None, cwd: None };
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
                        let language = logic::language_for_path(&key.1);
                        let text = contents.clone();
                        let editor = cx.new(|cx| EditorState::new(window, cx).language(language).default_value(text));
                        let save_key = key.clone();
                        state._subscription =
                            Some(cx.subscribe_in(&editor, window, move |this, _, event: &InputEvent, _, cx| {
                                if matches!(event, InputEvent::Change) {
                                    this.schedule_save(&save_key, cx);
                                }
                            }));
                        state.editor = Some(editor);
                        state.applied_wrap = None;
                    }
                }
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
    fn media_resource(&self, key: &Key) -> Option<Value> {
        let Some(Target::Thread(thread_id)) = &self.target else { return None };
        let path = logic::absolute_path(&key.0, &key.1);
        Some(json!({"_tag": "media-file", "threadId": thread_id, "path": path, "disposition": "attachment"}))
    }

    fn load_full_file(&mut self, key: Key, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(endpoint), Some(resource)) =
            (self.client(), self.endpoint.clone(), self.media_resource(&key))
        else {
            return;
        };
        let Some(state) = self.files.previews.get_mut(&key) else { return };
        state.full = Some(FullFile::Loading);
        let request_key = key.clone();
        state._full = Some(cx.spawn_in(window, async move |this, cx| {
            let bytes = fetch_asset(client, endpoint, resource).await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(state) = this.files.previews.get_mut(&request_key) else { return };
                state.full = Some(match bytes {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        let editor = cx.new(|cx| EditorState::new(window, cx).language("text").default_value(text));
                        FullFile::Ready(editor)
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
    fn save_full_file(&mut self, key: &Key, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(endpoint), Some(resource)) =
            (self.client(), self.endpoint.clone(), self.media_resource(key))
        else {
            return;
        };
        let directory = std::env::home_dir().map(|home| home.join("Downloads")).unwrap_or_default();
        let picked = cx.prompt_for_new_path(&directory, Some(logic::file_name(&key.1)));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let written = match fetch_asset(client, endpoint, resource).await {
                Ok(bytes) => std::fs::write(&path, bytes).map_err(anyhow::Error::from),
                Err(error) => Err(error),
            };
            if let Err(error) = written {
                let _ = cx.update(|window, cx| {
                    window.push_notification(Notification::error(error.to_string()).title("Could not read file"), cx)
                });
            }
        })
        .detach();
    }

    /// Saves the editor's text 500 ms after the last edit (`FileSaveCoordinator`).
    fn schedule_save(&mut self, key: &Key, cx: &mut Context<Self>) {
        let client = self.client();
        let Some(state) = self.files.previews.get_mut(key) else { return };
        let Some(editor) = &state.editor else { return };
        if matches!(state.load, Load::Text { truncated: true, .. }) {
            return;
        }
        let contents = editor.read(cx).value().to_string();
        state.dirty = contents != state.saved;
        if !state.dirty {
            state._save = None;
            state.saving = false;
            return;
        }
        let Some(client) = client else { return };
        state.saving = true;
        let key = key.clone();
        let payload = json!({"cwd": key.0, "relativePath": key.1, "contents": contents});
        state._save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let result = client.request("projects.writeFile", payload).await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.files.previews.get_mut(&key) else { return };
                state.saving = false;
                match result {
                    Ok(_) => {
                        state.dirty = state.editor.as_ref().is_some_and(|e| *e.read(cx).value() != *contents);
                        state.saved = contents;
                    }
                    Err(error) => {
                        this.error = Some(format!("Could not save {}: {}", key.1, logic::error_message(&error)).into())
                    }
                }
                cx.notify();
            });
        }));
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
        let cwd = target.cwd.clone();
        self.files.mutation = self.current_workspace_mutation();
        let Some(path) = path else {
            return self.render_tree(&cwd, None, 0, window, cx);
        };
        let key = (cwd.clone(), path.clone());
        self.sync_preview(&key, window, cx);
        let reveal = self.files.reveals.get(&key).map_or(0, |(_, request)| *request);
        let folder = self.files.previews.get(&key).is_some_and(|s| matches!(s.load, Load::Folder));
        let subheader = self.render_file_subheader(&key, &target.project_name, folder, &tokens, cx);
        let draft = !matches!(self.target, Some(Target::Thread(_)));
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
            self.render_tree(&cwd, Some(&path), reveal, window, cx)
        } else {
            let content = self.render_file_content(&key, &tokens, cx);
            let explorer = (!self.files.explorer_hidden).then(|| {
                let max = EXPLORER_MAX.min(width - CONTENT_MIN).max(EXPLORER_MIN);
                let explorer_width = self.files.explorer_width.unwrap_or(EXPLORER_WIDTH).clamp(EXPLORER_MIN, max);
                let tree = self.render_tree(&cwd, Some(&path), reveal, window, cx);
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
        key: &Key,
        project_name: &SharedString,
        folder: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = key.1.clone();
        let state = self.files.previews.get(key);
        let source = Surface::File(path.clone());
        let crumbs = logic::breadcrumbs(&path, project_name);
        let last = crumbs.len() - 1;
        let mut trail = h_flex().min_w_0().flex_1().overflow_hidden().text_xs();
        for (index, (label, crumb)) in crumbs.into_iter().enumerate() {
            let current = index == last;
            if index > 0 {
                trail = trail.child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(12.))
                        .flex_none()
                        .text_color(tokens.muted_foreground.opacity(0.6)),
                );
            }
            let tooltip: SharedString = if crumb.is_empty() { project_name.clone() } else { crumb.clone().into() };
            let source = source.clone();
            let keep = !folder;
            trail = trail.child(
                div()
                    .id(ElementId::Name(format!("crumb:{crumb}").into()))
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
        let markdown = text && logic::is_markdown(&path);
        let rendered = markdown && self.files.render_markdown;
        let wrap = state.and_then(|s| s.wrap).unwrap_or_else(|| self.client_settings.values.bool("wordWrap"));
        let editor = self.preferred_editor_id();
        let absolute = logic::absolute_path(&key.0, &path);
        h_flex()
            .flex_none()
            .h(px(28.))
            .mb(px(12.))
            .gap(px(8.))
            .px(px(12.))
            .child(trail)
            .when_some(editor, |el, editor| {
                let label: SharedString = format!("Open in {}", crate::app::details::editor_label(&editor)).into();
                let title = format!("Could not open in {}", crate::app::details::editor_label(&editor));
                el.child(action("file-open-in", IconName::SquareArrowOutUpRight, label, false).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.open_path_in_editor(absolute.clone(), editor.clone(), false, title.clone(), window, cx)
                    },
                )))
            })
            .when(markdown, |el| {
                let label = if rendered { "Show markdown source" } else { "Show rendered markdown" };
                el.child(
                    action(
                        "file-rendered",
                        if rendered { IconName::Code } else { IconName::Eye },
                        label.into(),
                        rendered,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.files.render_markdown = !this.files.render_markdown;
                        cx.notify();
                    })),
                )
            })
            .when(text && !rendered, |el| {
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
            .when(!folder, |el| {
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

    fn render_file_content(&mut self, key: &Key, tokens: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let render_markdown = self.files.render_markdown;
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
            Load::Binary => centered(
                v_flex()
                    .gap(px(8.))
                    .items_center()
                    .text_xs()
                    .text_color(tokens.muted_foreground)
                    .child(Icon::new(IconName::File).size(px(24.)))
                    .child("The file is binary and cannot be displayed as text.")
                    .into_any_element(),
            ),
            Load::Failed { message, attempted, cwd } => {
                let workspace = cwd.clone().unwrap_or_else(|| key.0.clone());
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
                        FullFile::Ready(editor) => Some(Ok(editor.clone())),
                        FullFile::Failed => Some(Err(())),
                    };
                    return self.render_full_file(key, full, tokens, cx);
                }
                if render_markdown && logic::is_markdown(&key.1) {
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
        &self,
        key: &Key,
        full: Option<Result<Entity<EditorState>, ()>>,
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
        let copied = full.clone().and_then(Result::ok);
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
            Some(Ok(editor)) => Editor::new(&editor)
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
    fn workspace_image(&mut self, key: &Key, cx: &mut Context<Self>) -> Option<std::sync::Arc<gpui_kit::Image>> {
        let absolute = logic::absolute_path(&key.0, &key.1);
        let resource = match &self.target {
            Some(Target::Thread(thread_id)) => json!({"_tag": "media-file", "threadId": thread_id, "path": absolute}),
            _ => json!({"_tag": "draft-workspace-file", "cwd": key.0, "path": absolute}),
        };
        let (client, endpoint) = (self.client(), self.endpoint.clone());
        let images = self.files.images.get_or_insert_with(|| cx.new(|_| Default::default())).clone();
        images.update(cx, |images, cx| images.get(resource, client, endpoint, cx))
    }

    /// The editor the details card's Open in launches.
    pub(in crate::app) fn preferred_editor_id(&self) -> Option<String> {
        let available = &self.server_info.available_editors;
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
    let relative = minted["relativeUrl"].as_str().unwrap_or_default().to_owned();
    let url = if relative.starts_with("http") {
        relative
    } else {
        format!("{}/{}", endpoint.http_base, relative.trim_start_matches('/'))
    };
    crate::runtime()
        .spawn(async move {
            let response = reqwest::Client::new().get(url).bearer_auth(&endpoint.bearer_token).send().await?;
            let response = response.error_for_status()?;
            if response.content_length().is_some_and(|length| length > FULL_FILE_MAX_BYTES) {
                anyhow::bail!("Text previews support files up to 256 MB.");
            }
            anyhow::Ok(response.bytes().await?.to_vec())
        })
        .await?
}
