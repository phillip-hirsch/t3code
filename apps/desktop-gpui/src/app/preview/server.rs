//! Agent browser tabs: tabs in the server's own Chromium (`runtime: "server"`), streamed over
//! `/api/preview-stream/ws` as `ServerBrowserSurface.tsx` and `serverBrowserStream.ts` do. JPEG
//! frames are decoded off the UI thread and only the latest is painted; input reaches the page
//! once this viewer has taken control.

use std::cell::Cell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, ClickEvent, Context, DispatchPhase, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent, Keystroke,
    Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _,
    PathPromptOptions, Pixels, Point, RenderImage, ScrollDelta, ScrollEvent, SharedString, Styled as _,
    StyledImage as _, Task, UTF16Selection, Window, canvas, div, img, px, size,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

use super::super::Workspace;
use super::super::right_panel::Surface;
use crate::preview::{self, NavStatus, Session, TabRef, ThreadRef, Viewport};
use crate::theme::Tokens;

const TAB_GONE: u16 = 4404;
/// `PREVIEW_STREAM_HOST_SETUP_CLOSE_CODE`.
const HOST_SETUP: u16 = 4503;
const WHEEL_LINE_PX: f32 = 16.;

/// `PreviewStreamControl`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Control {
    can_operate: bool,
    controller: String,
    dialog: Option<Dialog>,
}

#[derive(Clone, Debug, PartialEq)]
struct Dialog {
    kind: String,
    message: String,
    default_value: String,
}

/// The text a prompt's response field resets to when the page's dialog changes
/// (`setPromptText(next.dialog?.defaultValue ?? "")`), else `None` to keep the user's edit.
fn prompt_reset(previous: Option<&Dialog>, next: Option<&Dialog>) -> Option<String> {
    let key = |dialog: Option<&Dialog>| dialog.map(|d| (d.message.clone(), d.default_value.clone()));
    (key(previous) != key(next)).then(|| next.map(|d| d.default_value.clone()).unwrap_or_default())
}

impl Control {
    fn parse(value: &Value) -> Option<Self> {
        let controller = value["controller"].as_str()?;
        if !matches!(controller, "agent" | "you" | "another-viewer" | "unclaimed") {
            return None;
        }
        let dialog = match &value["dialog"] {
            Value::Null => None,
            dialog => Some(Dialog {
                kind: dialog["type"].as_str()?.to_owned(),
                message: dialog["message"].as_str()?.to_owned(),
                default_value: dialog["defaultValue"].as_str()?.to_owned(),
            }),
        };
        Some(Self { can_operate: value["canOperate"].as_bool()?, controller: controller.to_owned(), dialog })
    }

    fn mine(&self) -> bool {
        self.controller == "you"
    }
}

/// `previewStreamControlLabel`.
fn control_label(control: Option<&Control>) -> &'static str {
    match control {
        None => "Connecting...",
        Some(control) if !control.can_operate => "Read-only",
        Some(control) => match control.controller.as_str() {
            "you" => "You have control",
            "agent" => "Agent has control",
            "another-viewer" => "Another viewer has control",
            _ => "Watching",
        },
    }
}

/// The page's open file picker, offered to this viewer (`PreviewStreamFileChooser`).
#[derive(Clone, Debug, PartialEq)]
struct FileChooser {
    id: String,
    multiple: bool,
}

#[derive(Debug, PartialEq)]
enum ChooserMessage {
    Offered(FileChooser),
    Closed(String),
}

impl ChooserMessage {
    /// `fileChooser` and `fileChooserClosed`; anything else, or a malformed one, is `None`.
    fn parse(value: &Value) -> Option<Self> {
        let id = value["id"].as_str()?.to_owned();
        match value["type"].as_str()? {
            // GPUI's system picker has no type filter for the input's `accept`; the page checks
            // the files it gets, as it would a drop.
            "fileChooser" if value["accept"].is_string() => {
                Some(Self::Offered(FileChooser { id, multiple: value["multiple"].as_bool()? }))
            }
            "fileChooserClosed" => Some(Self::Closed(id)),
            _ => None,
        }
    }

    /// A closed picker withdraws only the offer it names; a newer one stays.
    fn apply(self, chooser: &mut Option<FileChooser>) {
        match self {
            Self::Offered(offer) => *chooser = Some(offer),
            Self::Closed(id) => {
                if chooser.as_ref().is_some_and(|open| open.id == id) {
                    *chooser = None;
                }
            }
        }
    }
}

/// The page's text-input bridge (the web's hidden textarea): an IME composition stays here
/// until it commits, then reaches the page as one `text` input.
#[derive(Debug, Default)]
struct Composition {
    marked: String,
    /// A Windows dead key went to the platform, which composes it with the next key that types.
    dead_key: bool,
}

impl Composition {
    fn mark(&mut self, text: &str) {
        text.clone_into(&mut self.marked);
    }

    /// Committed text replaces the composition; the page types it unless it is empty. Typed
    /// text means no dead key waits: Windows sends a dead key no character.
    fn commit(&mut self, text: &str) -> Option<String> {
        self.marked.clear();
        self.dead_key = false;
        (!text.is_empty()).then(|| text.to_owned())
    }

    /// The platform ended the composition without a commit (a click, focus moving): what it
    /// showed is kept, as a browser's `compositionend` keeps it.
    fn unmark(&mut self) -> Option<String> {
        let text = std::mem::take(&mut self.marked);
        (!text.is_empty()).then_some(text)
    }

    fn marked_range(&self) -> Option<Range<usize>> {
        (!self.marked.is_empty()).then(|| 0..self.marked.encode_utf16().count())
    }

    /// Whether a key down belongs to the platform's composition, so it types through the input
    /// handler instead of reaching the page as a key (the terminal's `composing`). A Windows dead
    /// key, the key that completes it, and text a modifier types (`AltGr`) need the
    /// `TranslateMessage` a handled key skips. Until that text commits, the key may be a dead key.
    fn composing(&mut self, keystroke: &Keystroke, prefer_character_input: bool) -> bool {
        let m = &keystroke.modifiers;
        let plain = !(m.control || m.alt || m.platform || m.function);
        let completes_dead_key = keystroke.key_char.is_some() && std::mem::take(&mut self.dead_key);
        if prefer_character_input {
            self.dead_key = !completes_dead_key;
            return true;
        }
        !self.marked.is_empty() || (plain && completes_dead_key)
    }
}

/// Typed and composed text for the page, while this viewer drives it. Keys the key listener
/// forwards never reach it: a handled key down sends no text on any platform.
pub(super) struct PageInput {
    outgoing: mpsc::UnboundedSender<String>,
    /// Connected, operable and controlled by this viewer, as `ServerStream::send` requires.
    live: bool,
    composition: Composition,
}

impl PageInput {
    fn set_live(&mut self, live: bool) {
        self.live = live;
        if !live {
            self.composition = Composition::default();
        }
    }

    fn type_text(&self, text: Option<String>) {
        if let Some(text) = text.filter(|_| self.live) {
            let _ = self.outgoing.send(json!({"type": "text", "text": text}).to_string());
        }
    }
}

impl EntityInputHandler for PageInput {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let end = self.composition.marked_range().map_or(0, |range| range.end);
        Some(UTF16Selection { range: end..end, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.composition.marked_range()
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        let text = self.composition.unmark();
        self.type_text(text);
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, _: &mut Context<Self>) {
        let text = self.composition.commit(text);
        self.type_text(text);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        if self.live {
            self.composition.mark(text);
        }
    }

    /// The frame's top-left corner, where the web pins its textarea, so the IME window opens there.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds::new(element_bounds.origin, size(px(1.), px(16.))))
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        self.live
    }
}

/// A key event for the page: a release, or a press that may ask for character input
/// (`KeyDownEvent::prefer_character_input`, a Windows dead key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyAction {
    Down,
    DownComposing,
    Up,
}

/// Shift+Escape takes the keyboard back from the page (`aria-description` on the web's textarea).
fn leaves_page(keystroke: &Keystroke) -> bool {
    let m = &keystroke.modifiers;
    keystroke.key == "escape" && m.shift && !(m.control || m.alt || m.platform || m.function)
}

/// The server's `UPLOAD_MAX_FILE_BYTES` and `UPLOAD_MAX_FILES`: a larger answer is refused
/// there anyway, so it is never read into memory here.
const UPLOAD_MAX_BYTES: u64 = 100 * 1024 * 1024;
const UPLOAD_MAX_FILES: usize = 20;
const UPLOAD_REFUSED: &str = "The upload was refused.";

/// `previewStreamUploadUrl`.
fn upload_url(http_base: &str, tab: &TabRef, chooser: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("threadId", &tab.thread.thread_id)
        .append_pair("tabId", &tab.tab_id)
        .append_pair("chooser", chooser)
        .finish();
    format!("{http_base}/api/preview-stream/upload?{query}")
}

struct UploadFile {
    name: String,
    mime: String,
    bytes: Vec<u8>,
}

/// A `multipart/form-data` body of `file` parts, as the web's `FormData`; no parts cancels the
/// page's picker. Quotes and line breaks in names are percent-encoded, as browsers do.
fn multipart_body(boundary: &str, files: &[UploadFile]) -> Vec<u8> {
    let mut body = Vec::new();
    for file in files {
        let name = file.name.replace('"', "%22").replace('\r', "%0D").replace('\n', "%0A");
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: {}\r\n\r\n",
                file.mime
            )
            .as_bytes(),
        );
        body.extend_from_slice(&file.bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

fn read_upload_files(paths: Vec<PathBuf>) -> Result<Vec<UploadFile>, String> {
    if paths.len() > UPLOAD_MAX_FILES {
        return Err(UPLOAD_REFUSED.to_owned());
    }
    let mut total = 0;
    for path in &paths {
        total += std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?.len();
    }
    if total > UPLOAD_MAX_BYTES {
        return Err(UPLOAD_REFUSED.to_owned());
    }
    paths
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            let name = path.file_name().map_or_else(|| "file".into(), |name| name.to_string_lossy().into_owned());
            let mime = mime_guess::from_path(&path).first_or_octet_stream().to_string();
            Ok(UploadFile { name, mime, bytes })
        })
        .collect()
}

/// `uploadPreviewStreamFiles`: answers the page's picker with `paths`, authenticated as the
/// stream is, on the environment that owns the tab.
async fn upload_files(url: String, token: String, paths: Vec<PathBuf>) -> Result<(), String> {
    static UPLOADS: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);
    let files =
        tokio::task::spawn_blocking(move || read_upload_files(paths)).await.map_err(|error| error.to_string())??;
    let boundary = format!("t3-{}", uuid::Uuid::new_v4().simple());
    let response = UPLOADS
        .post(url)
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
        .body(multipart_body(&boundary, &files))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        return Ok(());
    }
    let text = response.text().await.unwrap_or_default();
    Err(if text.is_empty() { UPLOAD_REFUSED.to_owned() } else { text })
}

/// What the stream's socket last told the view.
enum Incoming {
    Connected(bool),
    Frame(Arc<RenderImage>, (u32, u32)),
    Message(Value),
    /// The socket stopped for good: the tab closed, the host needs setup, or access was refused.
    Stopped(Stop),
}

#[derive(Clone, Debug, PartialEq)]
enum Stop {
    Gone,
    HostSetup { message: &'static str, command: String },
    Refused,
}

/// `decodeHostSetup` + `previewStreamHostSetupMessage`.
fn host_setup(reason: &str) -> Stop {
    let value: Value = serde_json::from_str(reason).unwrap_or_default();
    let (need, command) = match (value["need"].as_str(), value["command"].as_str()) {
        (Some(need @ ("sandbox" | "libraries")), Some(command)) => (need, command),
        _ => ("sandbox", "sudo npx t3 browser setup"),
    };
    let message = if need == "sandbox" {
        "This server's host blocks the sandbox its browser runs in. Run this once on the host, then try again:"
    } else {
        "This server's host is missing libraries its browser needs. Run this once on the host, then try again:"
    };
    Stop::HostSetup { message, command: command.to_owned() }
}

/// One agent tab's stream, live while the panel shows it.
pub(super) struct ServerStream {
    frame: Option<Arc<RenderImage>>,
    /// The frame's size in device px.
    frame_size: (u32, u32),
    /// Frames replaced since the last paint, whose textures the window still holds.
    stale: Vec<Arc<RenderImage>>,
    /// The page's CSS viewport.
    viewport: Option<(f32, f32)>,
    control: Option<Control>,
    connected: bool,
    stopped: Option<Stop>,
    /// The agent's pointer in page px, while the agent drives the page.
    agent_pointer: Option<(f32, f32)>,
    outgoing: mpsc::UnboundedSender<String>,
    /// Where the frame area was laid out last frame.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    sent_size: Option<(u32, u32)>,
    buttons: u8,
    focus: FocusHandle,
    input: Entity<PageInput>,
    file_chooser: Option<FileChooser>,
    /// A `prompt` dialog's response.
    prompt: Entity<InputState>,
    _task: Task<()>,
}

impl ServerStream {
    fn mine(&self) -> bool {
        self.control.as_ref().is_some_and(Control::mine)
    }

    /// Whether `send` would carry this viewer's input.
    fn live(&self) -> bool {
        self.connected && self.control.as_ref().is_some_and(|control| control.can_operate && control.mine())
    }

    /// `PreviewStreamClient.send`: only the controller sends, except the claim itself.
    fn send(&self, input: Value) -> bool {
        let Some(control) = self.control.as_ref().filter(|c| self.connected && c.can_operate) else { return false };
        if input["type"] != "takeControl" && !control.mine() {
            return false;
        }
        self.outgoing.send(input.to_string()).is_ok()
    }
}

/// A server tab's PDF (`pdfUrl`), shown in a native view over its stream as the web's `<embed>`.
struct PdfView {
    url: String,
    view: Option<Entity<gpui_webview::WebView>>,
    shown: bool,
}

#[derive(Default)]
pub(super) struct ServerStreams {
    streams: HashMap<TabRef, ServerStream>,
    pdfs: HashMap<TabRef, PdfView>,
    /// The agent tabs the panel and the mini player drew this frame, and those drawn as PDFs.
    pub(super) drawn: Vec<TabRef>,
    pdf_drawn: Vec<TabRef>,
}

impl Workspace {
    /// Keeps the streams of the agent tabs on screen and closes the others; shows their PDFs
    /// unless GPUI draws over them (`covered`). Frames replaced since the last paint are freed
    /// here too, since a stream kept under a failure or download page never paints them.
    pub(super) fn retain_server_streams(&mut self, covered: bool, window: &mut Window, cx: &mut App) {
        let drawn = std::mem::take(&mut self.preview.server.drawn);
        let pdf_drawn = std::mem::take(&mut self.preview.server.pdf_drawn);
        self.preview.server.pdfs.retain(|tab, _| pdf_drawn.contains(tab));
        for pdf in self.preview.server.pdfs.values_mut() {
            if let Some(view) = &pdf.view
                && pdf.shown == covered
            {
                pdf.shown = !covered;
                let _ = view.read(cx).set_visible(pdf.shown);
            }
        }
        self.preview.server.streams.retain(|tab, stream| {
            let keep = drawn.contains(tab);
            let frame = if keep { None } else { stream.frame.take() };
            for image in stream.stale.drain(..).chain(frame) {
                cx.drop_image(image, Some(window));
            }
            keep
        });
    }

    /// The CSS viewport `tab`'s stream last reported (`onViewport`).
    pub(super) fn stream_viewport(&self, tab: &TabRef) -> Option<(f32, f32)> {
        self.preview.server.streams.get(tab)?.viewport
    }

    /// Connects `tab`'s stream, or keeps it connected through this frame.
    pub(super) fn keep_server_stream(&mut self, tab: &TabRef, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_server_stream(tab, window, cx);
        if !self.preview.server.drawn.contains(tab) {
            self.preview.server.drawn.push(tab.clone());
        }
    }

    fn ensure_server_stream(&mut self, key: &TabRef, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.server.streams.contains_key(key) {
            return;
        }
        // The tab streams from its thread's environment.
        let Some(endpoint) = self.env_endpoint(&key.thread.environment).cloned() else { return };
        let scale = window.scale_factor();
        let size = window.viewport_size();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "pixel math; f32 is what GPUI draws with"
        )]
        let max = |px: Pixels| ((f32::from(px) * scale).round() as u32).max(1).to_string();
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("threadId", &key.thread.thread_id)
            .append_pair("tabId", &key.tab_id)
            .append_pair("maxWidth", &max(size.width))
            .append_pair("maxHeight", &max(size.height))
            .finish();
        let ws_base = endpoint.http_base.replacen("http", "ws", 1);
        let url = format!("{ws_base}/api/preview-stream/ws?{query}");
        let token = endpoint.bearer_token;
        let (outgoing, outgoing_rx) = mpsc::unbounded_channel();
        let (events_tx, events) = async_channel::unbounded();
        crate::runtime().spawn(run_stream(url, token, events_tx, outgoing_rx));
        let tab = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(first) = events.recv().await {
                // The latest frame wins; older ones in the batch are never painted.
                let mut batch = vec![first];
                batch.extend(std::iter::from_fn(|| events.try_recv().ok()));
                let alive = this.update_in(cx, |this, window, cx| {
                    for event in batch {
                        this.on_server_stream(&tab, event, window, cx);
                    }
                    this.sync_page_input(&tab, window, cx);
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        let input =
            cx.new(|_| PageInput { outgoing: outgoing.clone(), live: false, composition: Composition::default() });
        let stream = ServerStream {
            frame: None,
            frame_size: (0, 0),
            stale: Vec::new(),
            viewport: None,
            control: None,
            connected: false,
            stopped: None,
            agent_pointer: None,
            outgoing,
            bounds: Rc::default(),
            sent_size: None,
            buttons: 0,
            focus: cx.focus_handle(),
            input,
            file_chooser: None,
            prompt: cx.new(|cx| InputState::new(window, cx)),
            _task: task,
        };
        self.preview.server.streams.insert(key.clone(), stream);
    }

    fn on_server_stream(&mut self, key: &TabRef, event: Incoming, window: &mut Window, cx: &mut Context<Self>) {
        let Some(stream) = self.preview.server.streams.get_mut(key) else { return };
        match event {
            Incoming::Connected(connected) => {
                stream.connected = connected;
                if !connected {
                    stream.control = None;
                    stream.buttons = 0;
                    // The server offers a picker still open again, under a new id, on reconnect.
                    stream.file_chooser = None;
                }
            }
            Incoming::Frame(image, size) => {
                stream.stale.extend(stream.frame.replace(image));
                stream.frame_size = size;
                let _ = stream.outgoing.send(ACK.to_owned());
            }
            Incoming::Stopped(stop) => {
                stream.stopped = Some(stop);
                stream.connected = false;
                stream.control = None;
                stream.file_chooser = None;
            }
            Incoming::Message(message) => match message["type"].as_str() {
                Some("fileChooser" | "fileChooserClosed") => {
                    if let Some(chooser) = ChooserMessage::parse(&message) {
                        chooser.apply(&mut stream.file_chooser);
                    }
                }
                Some("control") => {
                    let Some(control) = Control::parse(&message) else { return };
                    let previous = stream.control.as_ref().and_then(|c| c.dialog.as_ref());
                    if let Some(text) = prompt_reset(previous, control.dialog.as_ref()) {
                        stream.prompt.update(cx, |input, cx| input.set_value(text, window, cx));
                    }
                    let gained = control.mine() && !stream.control.as_ref().is_some_and(Control::mine);
                    stream.control = Some(control);
                    if gained {
                        stream.sent_size = None;
                    }
                    if !stream.control.as_ref().is_some_and(Control::mine) {
                        stream.buttons = 0;
                    }
                }
                Some("viewport") => {
                    if let (Some(width), Some(height)) = (message["width"].as_f64(), message["height"].as_f64()) {
                        #[expect(clippy::cast_possible_truncation, reason = "page sizes; f32 is what GPUI draws with")]
                        let point = (width as f32, height as f32);
                        stream.viewport = Some(point);
                    }
                }
                Some("pointer") => {
                    if let (Some(x), Some(y)) = (message["x"].as_f64(), message["y"].as_f64()) {
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "page coordinates; f32 is what GPUI draws with"
                        )]
                        let point = (x as f32, y as f32);
                        stream.agent_pointer = Some(point);
                    }
                }
                Some("clipboard") => {
                    if let Some(text) = message["text"].as_str() {
                        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.to_owned()));
                    }
                }
                // `showPreviewPopup`: a page's new window opens as its own tab, on its thread's panel.
                Some("popup") => {
                    if let Some(popup) = message["tabId"].as_str() {
                        let target = super::panel_target(&key.thread);
                        self.open_surface_on(&target, Surface::Browser(popup.to_owned()), window, cx);
                    }
                }
                _ => {}
            },
        }
    }

    /// The tab's PDF view while it shows a signed PDF asset: `None` otherwise, `Some(None)` while
    /// its view starts.
    fn server_pdf_view(
        &mut self,
        key: &TabRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Option<Entity<gpui_webview::WebView>>> {
        let session = self.preview.threads.session(key)?;
        let NavStatus::Success { url, .. } = &session.nav_status else { return None };
        let endpoint = self.env_endpoint(&key.thread.environment)?;
        let url = preview::pdf_asset_url(url, &endpoint.http_base)?;
        self.preview.server.pdf_drawn.push(key.clone());
        if let Some(pdf) = self.preview.server.pdfs.get_mut(key) {
            if pdf.url != url {
                if let Some(view) = &pdf.view {
                    view.update(cx, |view, _| view.load_url(&url));
                }
                pdf.url = url;
            }
            return Some(pdf.view.clone());
        }
        self.preview.server.pdfs.insert(key.clone(), PdfView { url, view: None, shown: false });
        let tab = key.clone();
        // `build_view` runs only from a task.
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(url) = this.preview.server.pdfs.get(&tab).map(|pdf| pdf.url.clone()) else { return };
                let builder = wry::WebViewBuilder::new().with_url(url).with_visible(false).with_focused(false);
                match super::build_view(builder, window, cx) {
                    Ok(view) => {
                        if let Some(pdf) = this.preview.server.pdfs.get_mut(&tab) {
                            pdf.view = Some(cx.new(|_| view));
                        }
                    }
                    Err(error) => log::warn!("could not show the PDF: {error}"),
                }
                cx.notify();
            });
        })
        .detach();
        Some(None)
    }

    /// Sends `input` to the agent tab on screen, if this viewer may.
    pub(super) fn send_server_input(&self, tab: &TabRef, input: Value) -> bool {
        self.preview.server.streams.get(tab).is_some_and(|stream| stream.send(input))
    }

    /// Keeps the text bridge in step with control; a viewer that loses control loses the
    /// page's keyboard too, as the web blurs its textarea (`clearInput`).
    fn sync_page_input(&mut self, tab: &TabRef, window: &mut Window, cx: &mut Context<Self>) {
        let Some(stream) = self.preview.server.streams.get(tab) else { return };
        let live = stream.live();
        stream.input.update(cx, |input, _| input.set_live(live));
        if !live && stream.focus.is_focused(window) {
            self.leave_server_page(tab, window, cx);
        }
    }

    /// Moves focus off the page to the area that shows it: the panel, or the chat under the
    /// mini player.
    fn leave_server_page(&self, tab: &TabRef, window: &mut Window, cx: &mut App) {
        let in_panel = self.preview_thread().as_ref() == Some(&tab.thread)
            && self.active_surface() == Some(&Surface::Browser(tab.tab_id.clone()));
        let area = if in_panel { &self.areas.panel } else { &self.focus };
        area.focus(window, cx);
    }

    /// "Choose files": the system picker, then the upload, if the page still asks by then.
    fn choose_server_files(&mut self, tab: &TabRef, window: &mut Window, cx: &mut Context<Self>) {
        let Some(stream) = self.preview.server.streams.get(tab) else { return };
        let Some(chooser) = stream.file_chooser.clone().filter(|_| stream.live()) else { return };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: chooser.multiple,
            prompt: None,
        });
        let tab = tab.clone();
        cx.spawn_in(window, async move |this, cx| {
            // A cancelled system picker leaves the page's picker waiting, as the web's does.
            let Ok(Ok(Some(paths))) = paths.await else { return };
            if paths.is_empty() {
                return;
            }
            let _ =
                this.update_in(cx, |this, window, cx| this.answer_file_chooser(&tab, &chooser.id, paths, window, cx));
        })
        .detach();
    }

    /// `answerFileChooser`: sends `paths` (none cancels) to the picker `id`, unless it closed or
    /// control moved since it was offered.
    fn answer_file_chooser(
        &mut self,
        tab: &TabRef,
        id: &str,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(stream) = self.preview.server.streams.get_mut(tab) else { return };
        if !stream.live() || stream.file_chooser.as_ref().is_none_or(|chooser| chooser.id != id) {
            return;
        }
        stream.file_chooser = None;
        cx.notify();
        // The tab's own environment holds the picker.
        let Some(endpoint) = self.env_endpoint(&tab.thread.environment) else { return };
        let url = upload_url(&endpoint.http_base, tab, id);
        let upload = crate::runtime().spawn(upload_files(url, endpoint.bearer_token.clone(), paths));
        cx.spawn_in(window, async move |this, cx| {
            let error = match upload.await {
                Ok(Ok(())) => return,
                Ok(Err(error)) => error,
                Err(error) => error.to_string(),
            };
            log::warn!("server browser upload failed: {error}");
            let _ = this.update_in(cx, |_, window, cx| {
                window.push_notification(Notification::error(error).title("Could not send the files to the page"), cx);
            });
        })
        .detach();
    }

    /// The agent tab's page: its latest frame with the control bar above it, or below it
    /// (`controlPosition="bottom"`) in the mini player.
    pub(super) fn render_server_page(
        &mut self,
        key: &TabRef,
        follow_size: bool,
        bar_below: bool,
        tokens: &Tokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.keep_server_stream(key, window, cx);
        let Some(stream) = self.preview.server.streams.get_mut(key) else {
            return div().flex_1().into_any_element();
        };
        for image in stream.stale.drain(..) {
            cx.drop_image(image, Some(window));
        }
        // The page follows the panel's size while this viewer drives it (`followSize`).
        let bounds = stream.bounds.get();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "pixel math; f32 is what GPUI draws with"
        )]
        let size = (f32::from(bounds.size.width).round() as u32, f32::from(bounds.size.height).round() as u32);
        if follow_size
            && size.0 > 0
            && stream.control.as_ref().is_some_and(Control::mine)
            && stream.sent_size != Some(size)
        {
            stream.sent_size = Some(size);
            stream.send(json!({"type": "resize", "width": size.0, "height": size.1}));
        }
        // A press the window stopped seeing ends like the web's `pointercancel`.
        if stream.buttons != 0 && !window.is_window_active() {
            self.cancel_server_buttons(key, window.mouse_position());
        }
        let stream = &self.preview.server.streams[key];
        let control = stream.control.clone();
        let mine = control.as_ref().is_some_and(Control::mine);
        let bar = h_flex()
            .flex_none()
            .justify_between()
            .gap_2()
            .px_2()
            .py_1()
            .map(|bar| if bar_below { bar.border_t_1() } else { bar.border_b_1() })
            .border_color(tokens.border)
            .bg(tokens.background)
            .child(div().text_xs().text_color(tokens.muted_foreground).child(control_label(control.as_ref())))
            .children(control.as_ref().filter(|c| c.can_operate).map(|control| {
                let tab = key.clone();
                let claim = if mine { "releaseControl" } else { "takeControl" };
                Button::new("server-browser-control")
                    .outline()
                    .xsmall()
                    .label(if mine { "Release control" } else { "Take control" })
                    .disabled(control.controller == "another-viewer")
                    .on_click(cx.listener(move |this, _, _, _| {
                        this.send_server_input(&tab, json!({"type": claim}));
                    }))
            }));
        // The native view would cover GPUI's dialog and stop overlays.
        let overlay = self.render_server_overlay(key, tokens, cx);
        let pdf = if overlay.is_empty() { self.server_pdf_view(key, window, cx) } else { None };
        let stream = &self.preview.server.streams[key];
        let fit = page_fit(stream.bounds.get(), stream.frame_size, stream.viewport);
        let cursor = stream
            .agent_pointer
            .filter(|_| !mine && pdf.is_none())
            .zip(fit)
            .map(|((x, y), fit)| fit.to_local(x, y))
            .map(|(left, top)| {
                div()
                    .absolute()
                    .left(px(left - 6.))
                    .top(px(top - 6.))
                    .size(px(12.))
                    .rounded_full()
                    .border_2()
                    .border_color(tokens.background)
                    .bg(tokens.primary)
            });
        let frame = stream.frame.clone().map(|frame| img(frame).size_full().object_fit(ObjectFit::Contain));
        let bounds_cell = stream.bounds.clone();
        // The web disables its textarea over a PDF.
        let text_input = pdf.is_none().then(|| (stream.focus.clone(), stream.input.clone()));
        let tab = key.clone();
        let (down_tab, move_tab, wheel_tab, key_tab, key_up_tab) =
            (tab.clone(), tab.clone(), tab.clone(), tab.clone(), tab.clone());
        let (drag_view, drag_tab) = (cx.weak_entity(), tab.clone());
        let body = div()
            .id("server-browser-page")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .track_focus(&stream.focus)
            .child(
                canvas(
                    move |bounds, _, _| bounds_cell.set(bounds),
                    move |bounds, (), window, cx| {
                        if let Some((focus, input)) = text_input {
                            window.handle_input(&focus, ElementInputHandler::new(bounds, input), cx);
                        }
                        // A press on the page follows the pointer and its release anywhere in the window.
                        let (up_view, up_tab) = (drag_view.clone(), drag_tab.clone());
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                let _ = drag_view.update(cx, |this, _| this.server_mouse_move(&drag_tab, e, true));
                            }
                        });
                        window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Capture {
                                let _ = up_view.update(cx, |this, cx| this.server_mouse_up(&up_tab, e, cx));
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(frame)
            .children(pdf.flatten().map(|view| div().absolute().inset_0().child(view)))
            .children(cursor)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| this.server_mouse_down(&down_tab, e, w, cx)),
            )
            .on_mouse_down(MouseButton::Right, {
                let tab = tab.clone();
                cx.listener(move |this, e: &MouseDownEvent, w, cx| this.server_mouse_down(&tab, e, w, cx))
            })
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, e: &MouseDownEvent, w, cx| this.server_mouse_down(&tab, e, w, cx)),
            )
            .on_mouse_move(
                cx.listener(move |this, e: &MouseMoveEvent, _, _| this.server_mouse_move(&move_tab, e, false)),
            )
            .on_scroll(cx.listener(move |this, e: &ScrollEvent, _, cx| {
                if this.server_wheel(&wheel_tab, e) {
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                let action = if e.prefer_character_input { KeyAction::DownComposing } else { KeyAction::Down };
                if this.server_key(&key_tab, action, &e.keystroke, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_key_up(cx.listener(move |this, e: &KeyUpEvent, window, cx| {
                if this.server_key(&key_up_tab, KeyAction::Up, &e.keystroke, window, cx) {
                    cx.stop_propagation();
                }
            }));
        let body = body.children(overlay);
        let page = v_flex().flex_1().min_h_0().relative();
        if bar_below { page.child(body).child(bar) } else { page.child(bar).child(body) }.into_any_element()
    }

    /// The page's file picker and dialog (the dialog on top), or the stop that ended the stream.
    fn render_server_overlay(&self, key: &TabRef, tokens: &Tokens, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(stream) = self.preview.server.streams.get(key) else { return Vec::new() };
        let card = || {
            v_flex()
                .absolute()
                .left_2()
                .right_2()
                .top_2()
                .gap_2()
                .p_3()
                .rounded(px(8.))
                .border_1()
                .border_color(tokens.border)
                .bg(tokens.background)
                .shadow_lg()
                // Presses on the dialog reach its controls, not the page under it.
                .occlude()
        };
        let cover = || {
            v_flex()
                .absolute()
                .inset_0()
                .items_center()
                .justify_center()
                .gap_3()
                .p_4()
                .bg(tokens.background)
                .text_center()
        };
        let tab = key.clone();
        match &stream.stopped {
            Some(Stop::HostSetup { message, command }) => {
                return vec![
                    cover()
                        .child(div().max_w(px(384.)).text_sm().text_color(tokens.muted_foreground).child(*message))
                        .child(
                            div()
                                .w_full()
                                .max_w(px(448.))
                                .p_2()
                                .rounded(px(6.))
                                .bg(tokens.muted)
                                .font_family(tokens.fonts.mono)
                                .text_xs()
                                .text_left()
                                .child(command.clone()),
                        )
                        .child(
                            Button::new("server-browser-retry")
                                .outline()
                                .small()
                                .label("Try again")
                                .on_click(cx.listener(move |this, _, _, cx| this.restart_server_stream(&tab, cx))),
                        )
                        .into_any_element(),
                ];
            }
            Some(Stop::Refused) => {
                return vec![
                    cover()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(tokens.muted_foreground)
                                .child("Browser connection was refused."),
                        )
                        .child(
                            Button::new("server-browser-reconnect")
                                .outline()
                                .small()
                                .label("Reconnect")
                                .on_click(cx.listener(move |this, _, _, cx| this.restart_server_stream(&tab, cx))),
                        )
                        .into_any_element(),
                ];
            }
            Some(Stop::Gone) | None => {}
        }
        let mut overlays = Vec::new();
        if let Some(chooser) = stream.file_chooser.as_ref().filter(|_| stream.mine()) {
            let id = chooser.id.clone();
            let (cancel_tab, choose_tab) = (tab.clone(), tab.clone());
            let what = if chooser.multiple { "files" } else { "a file" };
            overlays.push(
                card()
                    .child(div().text_sm().child(format!("The page asks for {what}.")))
                    .child(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .child(Button::new("server-chooser-cancel").outline().small().label("Cancel").on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.answer_file_chooser(&cancel_tab, &id, Vec::new(), window, cx);
                                }),
                            ))
                            .child(
                                Button::new("server-chooser-choose")
                                    .primary()
                                    .small()
                                    .label(if chooser.multiple { "Choose files" } else { "Choose file" })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.choose_server_files(&choose_tab, window, cx);
                                    })),
                            ),
                    )
                    .into_any_element(),
            );
        }
        let Some(dialog) = stream.control.as_ref().and_then(|control| control.dialog.as_ref()) else { return overlays };
        let is_prompt = dialog.kind == "prompt";
        let respond = |id: &'static str, label: &'static str, accept: bool, cx: &mut Context<Self>| {
            let (tab, prompt) = (tab.clone(), stream.prompt.clone());
            let button = Button::new(id).small().label(label);
            let button = if accept { button.primary() } else { button.outline() };
            button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let mut input = json!({"type": "dialog", "accept": accept});
                if accept && is_prompt {
                    input["promptText"] = prompt.read(cx).value().to_string().into();
                }
                this.send_server_input(&tab, input);
            }))
        };
        overlays.push(
            card()
                .child(div().text_sm().child(SharedString::from(dialog.message.clone())))
                .map(|el| {
                    if stream.mine() {
                        el.when(is_prompt, |el| el.child(Input::new(&stream.prompt))).child(
                            h_flex()
                                .justify_end()
                                .gap_2()
                                .child(respond("server-dialog-dismiss", "Dismiss", false, cx))
                                .child(respond("server-dialog-accept", "Accept", true, cx)),
                        )
                    } else {
                        el.child(div().text_xs().text_color(tokens.muted_foreground).child("Take control to respond."))
                    }
                })
                .into_any_element(),
        );
        overlays
    }

    fn restart_server_stream(&mut self, tab: &TabRef, cx: &mut Context<Self>) {
        if let Some(stream) = self.preview.server.streams.remove(tab) {
            for image in stream.stale.into_iter().chain(stream.frame) {
                cx.drop_image(image, None);
            }
        }
        cx.notify();
    }

    fn server_point(&self, tab: &TabRef, position: Point<Pixels>, captured: bool) -> Option<(f32, f32)> {
        let stream = self.preview.server.streams.get(tab)?;
        page_fit(stream.bounds.get(), stream.frame_size, stream.viewport)?.to_page(position, captured)
    }

    fn server_mouse_down(&mut self, tab: &TabRef, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(stream) = self.preview.server.streams.get(tab) else { return };
        if !stream.control.as_ref().is_some_and(Control::mine) {
            return;
        }
        window.focus(&stream.focus.clone(), cx);
        let Some((button, bit)) = mouse_button(event.button) else { return };
        let Some((x, y)) = self.server_point(tab, event.position, false) else { return };
        let Some(stream) = self.preview.server.streams.get_mut(tab) else { return };
        stream.buttons |= bit;
        stream.send(json!({
            "type": "mouse", "action": "down", "x": x, "y": y, "button": button, "buttons": stream.buttons,
            "clickCount": event.click_count, "modifiers": modifier_bits(event.modifiers),
        }));
        cx.stop_propagation();
    }

    fn server_mouse_up(&mut self, tab: &TabRef, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some((button, bit)) = mouse_button(event.button) else { return };
        if self.preview.server.streams.get(tab).is_none_or(|s| s.buttons & bit == 0) {
            return;
        }
        let Some((x, y)) = self.server_point(tab, event.position, true) else { return };
        let Some(stream) = self.preview.server.streams.get_mut(tab) else { return };
        stream.buttons &= !bit;
        stream.send(json!({
            "type": "mouse", "action": "up", "x": x, "y": y, "button": button, "buttons": stream.buttons,
            "clickCount": event.click_count, "modifiers": modifier_bits(event.modifiers),
        }));
        cx.stop_propagation();
    }

    /// A hover over the page, or (`dragging`) a held press moving anywhere in the window.
    fn server_mouse_move(&mut self, tab: &TabRef, event: &MouseMoveEvent, dragging: bool) {
        let Some(stream) = self.preview.server.streams.get(tab) else { return };
        // A release this window didn't see: the page gets its `up` before any more moves.
        if dragging && stream.buttons != 0 && event.pressed_button.is_none() {
            self.cancel_server_buttons(tab, event.position);
            return;
        }
        if !stream.control.as_ref().is_some_and(Control::mine) || (stream.buttons != 0) != dragging {
            return;
        }
        let held = stream.buttons;
        let Some((x, y)) = self.server_point(tab, event.position, held != 0) else { return };
        let button = match held {
            0 => "none",
            b if b & 1 != 0 => "left",
            b if b & 2 != 0 => "right",
            _ => "middle",
        };
        stream.send(json!({
            "type": "mouse", "action": "move", "x": x, "y": y, "button": button, "buttons": held,
            "clickCount": 0, "modifiers": modifier_bits(event.modifiers),
        }));
    }

    /// Releases every held button at `position`, clamped to the page.
    fn cancel_server_buttons(&mut self, tab: &TabRef, position: Point<Pixels>) {
        let point = self.server_point(tab, position, true);
        let Some(stream) = self.preview.server.streams.get_mut(tab) else { return };
        if let Some((x, y)) = point {
            for (button, buttons) in released_buttons(stream.buttons) {
                stream.send(json!({
                    "type": "mouse", "action": "up", "x": x, "y": y, "button": button, "buttons": buttons,
                    "clickCount": 0, "modifiers": 0,
                }));
            }
        }
        stream.buttons = 0;
    }

    fn server_wheel(&self, tab: &TabRef, event: &ScrollEvent) -> bool {
        let Some((x, y)) = self.server_point(tab, event.position, false) else { return false };
        let (dx, dy) = match event.delta {
            ScrollDelta::Pixels(delta) => (f32::from(delta.x), f32::from(delta.y)),
            ScrollDelta::Lines(delta) => (delta.x * WHEEL_LINE_PX, delta.y * WHEEL_LINE_PX),
        };
        // GPUI's deltas point where the content moves; the DOM's where the page scrolls.
        self.send_server_input(
            tab,
            json!({"type": "wheel", "x": x, "y": y, "deltaX": -dx, "deltaY": -dy, "modifiers": modifier_bits(event.modifiers)}),
        )
    }

    fn server_key(
        &mut self,
        tab: &TabRef,
        key_action: KeyAction,
        keystroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let action = if key_action == KeyAction::Up { "up" } else { "down" };
        let Some(stream) = self.preview.server.streams.get(tab) else { return false };
        // Keys typed into the page's own overlays, such as a prompt's response, stay there.
        if !stream.focus.is_focused(window) {
            return false;
        }
        if leaves_page(keystroke) {
            if action == "down" {
                self.leave_server_page(tab, window, cx);
            }
            return true;
        }
        if !stream.mine() {
            return false;
        }
        if key_action != KeyAction::Up {
            let prefer_character_input = key_action == KeyAction::DownComposing;
            if stream.input.update(cx, |input, _| input.composition.composing(keystroke, prefer_character_input)) {
                return false;
            }
        }
        let m = &keystroke.modifiers;
        // Paste types the clipboard's text, as the page's own paste would.
        if action == "down" && (m.control || m.platform) && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                return stream.send(json!({"type": "text", "text": text}));
            }
            return true;
        }
        let Some(key) = dom_key(keystroke) else { return false };
        let mut input = json!({
            "type": "key", "action": action, "key": key.key, "code": key.code, "keyCode": key.key_code,
            "modifiers": modifier_bits(*m),
        });
        if action == "down"
            && let Some(text) = key.text
        {
            input["text"] = text.into();
        }
        stream.send(input)
    }
}

impl Workspace {
    /// `alternatePreviewRuntime`'s label: a remote environment's tab can move between this
    /// computer and the environment's own browser. `None` for this app's own server.
    pub(super) fn move_tab_label(&self, thread: &ThreadRef, session: &Session) -> Option<SharedString> {
        let environment = &thread.environment;
        let remote = self.environment_remote || self.primary_environment() != Some(environment);
        let info = &self.session(environment)?.server_info;
        if !remote || !info.capability("serverBrowser") {
            return None;
        }
        Some(if session.server_hosted() {
            "Open on this computer".into()
        } else {
            let label = info.environment_label.as_deref().unwrap_or("the environment");
            format!("Open in {label}'s browser").into()
        })
    }

    /// `handleMoveTab`: opens the page in the other runtime, then closes this tab.
    pub(super) fn move_preview_tab(
        &mut self,
        thread: ThreadRef,
        session: Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.env_client(&thread.environment) else { return };
        let mut input = json!({"threadId": thread.thread_id, "viewport": viewport_json(&session.viewport)});
        if let Some(url) = session.nav_status.url() {
            // A page moving to this computer reaches the environment's loopback servers on its host.
            input["url"] =
                if session.server_hosted() { self.discovered_url(&thread, url) } else { url.to_owned() }.into();
        }
        if !session.server_hosted() {
            input["runtime"] = "server".into();
        }
        if let Some(profile) = &session.profile_id {
            input["profileId"] = profile.clone().into();
        }
        // The moved tab opens on its thread's panel, even if another thread shows by then.
        let origin = super::panel_target(&thread);
        cx.spawn_in(window, async move |this, cx| {
            let result = client.request("preview.open", input).await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result.map_err(|e| e.to_string()).and_then(|v| {
                    serde_json::from_value::<Session>(v).map_err(|e| format!("Unexpected preview.open result: {e}"))
                }) {
                    Ok(moved) => {
                        let tab = moved.tab_id.clone();
                        this.preview.threads.entry(&thread).apply_snapshot(moved, true);
                        this.open_surface_on(&origin, Surface::Browser(tab), window, cx);
                        this.close_preview_tab(thread.tab(&session.tab_id), window, cx);
                    }
                    Err(error) => window.push_notification(
                        gpui_kit::component::notification::Notification::error(error)
                            .title("Unable to move the browser tab"),
                        cx,
                    ),
                }
            });
        })
        .detach();
    }
}

fn viewport_json(viewport: &Viewport) -> Value {
    match viewport {
        Viewport::Fill => json!({"_tag": "fill"}),
        Viewport::Freeform { width, height } => json!({"_tag": "freeform", "width": width, "height": height}),
        Viewport::Preset { width, height, preset_id } => {
            json!({"_tag": "preset", "presetId": preset_id, "width": width, "height": height})
        }
    }
}

/// `previewStreamModifiers`: alt 1, ctrl 2, meta 4, shift 8.
fn modifier_bits(m: Modifiers) -> u8 {
    u8::from(m.alt) | u8::from(m.control) << 1 | u8::from(m.platform) << 2 | u8::from(m.shift) << 3
}

fn mouse_button(button: MouseButton) -> Option<(&'static str, u8)> {
    match button {
        MouseButton::Left => Some(("left", 1)),
        MouseButton::Right => Some(("right", 2)),
        MouseButton::Middle => Some(("middle", 4)),
        MouseButton::Navigate(_) => None,
    }
}

/// Cancellation releases every held button, one `up` each, with the buttons still held after it.
fn released_buttons(held: u8) -> Vec<(&'static str, u8)> {
    let mut buttons = held;
    [("left", 1), ("right", 2), ("middle", 4)]
        .into_iter()
        .filter(|(_, bit)| held & bit != 0)
        .map(|(button, bit)| {
            buttons &= !bit;
            (button, buttons)
        })
        .collect()
}

/// How the frame sits in its area: `object-fit: contain`, mapped to page CSS px.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PageFit {
    origin: (f32, f32),
    size: (f32, f32),
    viewport: (f32, f32),
    area: Bounds<Pixels>,
}

fn page_fit(area: Bounds<Pixels>, frame: (u32, u32), viewport: Option<(f32, f32)>) -> Option<PageFit> {
    let (fw, fh) = (frame.0 as f32, frame.1 as f32);
    let (aw, ah) = (f32::from(area.size.width), f32::from(area.size.height));
    if fw < 1. || fh < 1. || aw < 1. || ah < 1. {
        return None;
    }
    let fit = (aw / fw).min(ah / fh);
    let size = (fw * fit, fh * fit);
    let origin = (f32::from(area.origin.x) + (aw - size.0) / 2., f32::from(area.origin.y) + (ah - size.1) / 2.);
    // The page's viewport, else the frame's own size, which a device scale of 1 matches.
    Some(PageFit { origin, size, viewport: viewport.unwrap_or((fw, fh)), area })
}

impl PageFit {
    /// A window point as page px; outside the frame only while a button is held (`captured`).
    fn to_page(self, point: Point<Pixels>, captured: bool) -> Option<(f32, f32)> {
        let x = (f32::from(point.x) - self.origin.0) / self.size.0;
        let y = (f32::from(point.y) - self.origin.1) / self.size.1;
        if !(captured || ((0. ..=1.).contains(&x) && (0. ..=1.).contains(&y))) {
            return None;
        }
        Some((x.clamp(0., 1.) * self.viewport.0, y.clamp(0., 1.) * self.viewport.1))
    }

    /// A page point relative to the frame area, for overlays drawn in it.
    fn to_local(self, x: f32, y: f32) -> (f32, f32) {
        (
            self.origin.0 - f32::from(self.area.origin.x) + x / self.viewport.0 * self.size.0,
            self.origin.1 - f32::from(self.area.origin.y) + y / self.viewport.1 * self.size.1,
        )
    }
}

#[derive(Debug, PartialEq)]
struct DomKey {
    key: String,
    code: String,
    key_code: u32,
    /// Typed text: printable keys without Ctrl or Meta, and Enter's `"\r"`.
    text: Option<String>,
}

/// A GPUI keystroke as the DOM `KeyboardEvent` fields Puppeteer replays.
fn dom_key(keystroke: &Keystroke) -> Option<DomKey> {
    let m = &keystroke.modifiers;
    let shortcut = m.control || m.platform;
    let named = |key: &str, code: &str, key_code: u32| (key.to_owned(), code.to_owned(), key_code);
    let (key, code, key_code) = match keystroke.key.as_str() {
        "enter" => named("Enter", "Enter", 13),
        "backspace" => named("Backspace", "Backspace", 8),
        "tab" => named("Tab", "Tab", 9),
        "escape" => named("Escape", "Escape", 27),
        "delete" => named("Delete", "Delete", 46),
        "left" => named("ArrowLeft", "ArrowLeft", 37),
        "up" => named("ArrowUp", "ArrowUp", 38),
        "right" => named("ArrowRight", "ArrowRight", 39),
        "down" => named("ArrowDown", "ArrowDown", 40),
        "home" => named("Home", "Home", 36),
        "end" => named("End", "End", 35),
        "pageup" => named("PageUp", "PageUp", 33),
        "pagedown" => named("PageDown", "PageDown", 34),
        "space" => named(" ", "Space", 32),
        key if key.chars().count() == 1 => {
            let c = key.chars().next()?;
            let typed = keystroke.key_char.clone().filter(|t| t.chars().count() == 1).unwrap_or_else(|| key.to_owned());
            let code = match c {
                'a'..='z' => format!("Key{}", c.to_ascii_uppercase()),
                '0'..='9' => format!("Digit{c}"),
                _ => String::new(),
            };
            let key_code = if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() as u32 } else { 0 };
            (typed, code, key_code)
        }
        _ => return None,
    };
    let text = match key.as_str() {
        "Enter" if !shortcut => Some("\r".to_owned()),
        k if !shortcut && k.chars().count() == 1 => Some(k.to_owned()),
        _ => None,
    };
    Some(DomKey { key, code, key_code, text })
}

/// The frame acknowledgement; the server sends the next frame after it.
const ACK: &str = r#"{"type":"ack"}"#;

/// The socket's life: (re)connects with backoff until the view drops `outgoing` or the server
/// says to stop. The view acknowledges each frame it takes, so a slow viewer gets fewer of them.
async fn run_stream(
    url: String,
    token: String,
    events: async_channel::Sender<Incoming>,
    mut outgoing: mpsc::UnboundedReceiver<String>,
) {
    let mut failures = 0u32;
    loop {
        let Ok(mut request) = url.as_str().into_client_request() else { return };
        if let Ok(value) = HeaderValue::from_str(&format!("Bearer {token}")) {
            request.headers_mut().insert("Authorization", value);
        }
        let socket = match tokio_tungstenite::connect_async(request).await {
            Ok((socket, _)) => socket,
            Err(WsError::Http(response)) if matches!(response.status().as_u16(), 401 | 403) => {
                let _ = events.send(Incoming::Stopped(Stop::Refused)).await;
                return;
            }
            Err(WsError::Http(response)) if response.status().as_u16() == 404 => {
                let _ = events.send(Incoming::Stopped(Stop::Gone)).await;
                return;
            }
            Err(error) => {
                log::warn!("preview stream connect failed: {error}");
                if !backoff(&mut failures, &events).await {
                    return;
                }
                continue;
            }
        };
        if events.send(Incoming::Connected(true)).await.is_err() {
            return;
        }
        let (mut sink, mut source) = socket.split();
        let stop = loop {
            tokio::select! {
                message = source.next() => match message {
                    Some(Ok(Message::Binary(jpeg))) => {
                        failures = 0;
                        // Off the IO workers, which also carry the RPC socket.
                        let decoded = tokio::task::spawn_blocking(move || decode_jpeg(&jpeg)).await.ok().flatten();
                        match decoded {
                            Some((image, size)) => {
                                if events.send(Incoming::Frame(image, size)).await.is_err() {
                                    return;
                                }
                            }
                            // The view never sees an undecodable frame, so it is acknowledged here.
                            None => {
                                if sink.send(Message::text(ACK)).await.is_err() {
                                    break None;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        let Ok(value) = serde_json::from_str::<Value>(&text) else { continue };
                        if value["type"] == "viewport" {
                            failures = 0;
                        }
                        if events.send(Incoming::Message(value)).await.is_err() {
                            return;
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        break frame.and_then(|frame| match u16::from(frame.code) {
                            TAB_GONE => Some(Stop::Gone),
                            HOST_SETUP => Some(host_setup(&frame.reason)),
                            4401 => Some(Stop::Refused),
                            _ if frame.code == CloseCode::Policy => Some(Stop::Refused),
                            _ => None,
                        });
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) | None => break None,
                },
                input = outgoing.recv() => match input {
                    Some(text) => {
                        if sink.send(Message::text(text)).await.is_err() {
                            break None;
                        }
                    }
                    // The view closed the tab.
                    None => {
                        let _ = sink.close().await;
                        return;
                    }
                },
            }
        };
        if events.send(Incoming::Connected(false)).await.is_err() {
            return;
        }
        if let Some(stop) = stop {
            let _ = events.send(Incoming::Stopped(stop)).await;
            return;
        }
        if !backoff(&mut failures, &events).await {
            return;
        }
    }
}

/// Waits `min(500ms * 2^failures, 10s)`; false once the view is gone.
async fn backoff(failures: &mut u32, events: &async_channel::Sender<Incoming>) -> bool {
    let delay = Duration::from_millis((500u64 << (*failures).min(5)).min(10_000));
    *failures += 1;
    tokio::time::sleep(delay).await;
    !events.is_closed()
}

fn decode_jpeg(bytes: &[u8]) -> Option<(Arc<RenderImage>, (u32, u32))> {
    let decoded = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?;
    let mut rgba = decoded.into_rgba8();
    let size = rgba.dimensions();
    // GPUI textures are BGRA.
    for pixel in rgba.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Some((Arc::new(RenderImage::new([image::Frame::new(rgba)])), size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::point;

    #[test]
    fn control_labels_follow_the_controller() {
        let control = |can_operate: bool, controller: &str| {
            Control::parse(
                &json!({"canOperate": can_operate, "controller": controller, "generation": 1, "dialog": null}),
            )
        };
        assert_eq!(control_label(None), "Connecting...");
        assert_eq!(control_label(control(false, "agent").as_ref()), "Read-only");
        assert_eq!(control_label(control(true, "you").as_ref()), "You have control");
        assert_eq!(control_label(control(true, "agent").as_ref()), "Agent has control");
        assert_eq!(control_label(control(true, "unclaimed").as_ref()), "Watching");
        assert!(control(true, "robot").is_none());
        let dialog = Control::parse(&json!({"canOperate": true, "controller": "you", "generation": 2,
            "dialog": {"type": "prompt", "message": "Name?", "defaultValue": "x"}}))
        .unwrap();
        let prompt = dialog.dialog.unwrap();
        assert_eq!(prompt.default_value, "x");
        assert_eq!(prompt_reset(None, Some(&prompt)).as_deref(), Some("x"), "a new prompt shows its default");
        assert_eq!(prompt_reset(Some(&prompt), Some(&prompt.clone())), None, "a repeated control keeps the edit");
        let next = Dialog { message: "Age?".into(), ..prompt.clone() };
        assert_eq!(prompt_reset(Some(&prompt), Some(&next)).as_deref(), Some("x"), "another prompt resets");
        assert_eq!(prompt_reset(Some(&prompt), None).as_deref(), Some(""), "a closed dialog clears it");
    }

    #[test]
    fn a_cancelled_press_releases_each_held_button() {
        assert_eq!(released_buttons(0b101), vec![("left", 0b100), ("middle", 0)]);
        assert_eq!(released_buttons(0b010), vec![("right", 0)]);
        assert!(released_buttons(0).is_empty());
    }

    #[test]
    fn points_map_through_the_contained_frame() {
        // A 1000x500 frame of a 500x250 page in a 400x400 area at (100, 0): 400x200, centered.
        let area = Bounds { origin: point(px(100.), px(0.)), size: size(px(400.), px(400.)) };
        let fit = page_fit(area, (1000, 500), Some((500., 250.))).unwrap();
        assert_eq!(fit.to_page(point(px(100.), px(100.)), false), Some((0., 0.)));
        assert_eq!(fit.to_page(point(px(300.), px(200.)), false), Some((250., 125.)));
        assert_eq!(fit.to_page(point(px(300.), px(50.)), false), None, "above the frame");
        assert_eq!(fit.to_page(point(px(300.), px(50.)), true), Some((250., 0.)), "a drag clamps to the edge");
        assert_eq!(fit.to_local(250., 125.), (200., 200.));
        assert!(page_fit(area, (0, 0), None).is_none());
    }

    #[test]
    fn keystrokes_become_dom_keys() {
        let key = |text: &'static str| dom_key(&Keystroke::parse(text).ok().unwrap()).unwrap();
        assert_eq!(
            key("enter"),
            DomKey { key: "Enter".into(), code: "Enter".into(), key_code: 13, text: Some("\r".into()) }
        );
        assert_eq!(key("a").code, "KeyA");
        assert_eq!((key("a").key_code, key("a").text.as_deref()), (65, Some("a")));
        assert_eq!(key("ctrl-a").text, None, "shortcuts type nothing");
        assert_eq!(key("5").code, "Digit5");
        assert_eq!(key("left").key, "ArrowLeft");
        assert_eq!(key("space").text.as_deref(), Some(" "));
        assert!(dom_key(&Keystroke::parse("f13").ok().unwrap()).is_none());
        assert_eq!(modifier_bits(Modifiers { shift: true, control: true, ..Default::default() }), 10);
    }

    #[test]
    fn file_chooser_messages_offer_and_withdraw_the_named_picker() {
        let offer = |id: &str| json!({"type": "fileChooser", "id": id, "multiple": true, "accept": "image/*"});
        let mut chooser = None;
        ChooserMessage::parse(&offer("a")).unwrap().apply(&mut chooser);
        assert_eq!(chooser, Some(FileChooser { id: "a".into(), multiple: true }));
        ChooserMessage::parse(&offer("b")).unwrap().apply(&mut chooser);
        ChooserMessage::parse(&json!({"type": "fileChooserClosed", "id": "a"})).unwrap().apply(&mut chooser);
        assert_eq!(chooser.as_ref().map(|c| c.id.as_str()), Some("b"), "an older picker's close keeps the new one");
        ChooserMessage::parse(&json!({"type": "fileChooserClosed", "id": "b"})).unwrap().apply(&mut chooser);
        assert_eq!(chooser, None);
        assert_eq!(ChooserMessage::parse(&json!({"type": "fileChooser", "id": "c", "multiple": false})), None);
        assert_eq!(ChooserMessage::parse(&json!({"type": "fileChooserClosed"})), None);
        assert_eq!(ChooserMessage::parse(&json!({"type": "popup", "id": "d"})), None);
    }

    #[test]
    fn uploads_post_named_file_parts_to_the_tabs_picker() {
        let tab = ThreadRef::new("env", "thread 1").tab("tab&1");
        assert_eq!(
            upload_url("http://host:3773", &tab, "c1"),
            "http://host:3773/api/preview-stream/upload?threadId=thread+1&tabId=tab%261&chooser=c1"
        );
        let file = UploadFile { name: "a\"b\r\n.png".into(), mime: "image/png".into(), bytes: b"PNG".to_vec() };
        assert_eq!(
            String::from_utf8(multipart_body("X", &[file])).unwrap(),
            "--X\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a%22b%0D%0A.png\"\r\n\
             Content-Type: image/png\r\n\r\nPNG\r\n--X--\r\n"
        );
        assert_eq!(multipart_body("X", &[]), b"--X--\r\n", "no files cancels the picker");
    }

    #[test]
    fn compositions_reach_the_page_only_when_they_commit() {
        let mut composition = Composition::default();
        composition.mark("に");
        assert_eq!(composition.marked_range(), Some(0..1));
        composition.mark("日本");
        assert_eq!(composition.commit("日本"), Some("日本".into()));
        assert_eq!(composition.marked_range(), None);
        composition.mark("ka");
        assert_eq!(composition.commit(""), None, "a cancelled composition types nothing");
        composition.mark("😀");
        assert_eq!(composition.marked_range(), Some(0..2), "ranges count UTF-16 units");
        assert_eq!(composition.unmark(), Some("😀".into()), "an ended composition keeps its text");
        assert_eq!(composition.unmark(), None);
    }

    #[test]
    fn dead_keys_and_compositions_type_through_the_input_handler() {
        let key = |text: &'static str, key_char: Option<&str>| Keystroke {
            key_char: key_char.map(str::to_owned),
            ..Keystroke::parse(text).ok().unwrap()
        };
        let mut composition = Composition::default();
        // Windows: the dead key asks for character input; the key that completes it does not.
        assert!(composition.composing(&key("´", Some("´")), true), "the dead key");
        assert!(composition.composing(&key("e", Some("é")), false), "the key it composes with");
        assert!(!composition.composing(&key("e", Some("e")), false), "the dead key is spent");
        // A dead key pressed twice types it, and a shortcut never composes.
        assert!(composition.composing(&key("´", Some("´")), true));
        assert!(composition.composing(&key("´", Some("´")), true));
        assert!(!composition.composing(&key("e", Some("e")), false));
        assert!(!composition.composing(&key("ctrl-c", Some("c")), false));
        // AltGr is Ctrl+Alt on Windows: the text it types asks for character input too.
        assert!(composition.composing(&key("ctrl-alt-q", Some("@")), true), "German AltGr+Q types @");
        composition.commit("@");
        assert!(!composition.composing(&key("e", Some("e")), false), "typed text leaves no dead key");
        // An AltGr dead key composes with the next key that types.
        assert!(composition.composing(&key("ctrl-alt-]", Some("~")), true));
        assert!(composition.composing(&key("a", Some("ã")), false));
        // Keys while an IME shows marked text belong to it.
        composition.mark("に");
        assert!(composition.composing(&key("enter", None), false));
        composition.commit("に");
        assert!(!composition.composing(&key("enter", None), false));
    }

    #[test]
    fn shift_escape_alone_leaves_the_page() {
        let leaves = |text: &'static str| leaves_page(&Keystroke::parse(text).ok().unwrap());
        assert!(leaves("shift-escape"));
        assert!(!leaves("escape"), "Escape reaches the page");
        assert!(!leaves("ctrl-shift-escape"));
    }

    #[test]
    fn host_setup_reasons_fall_back_to_the_sandbox() {
        let Stop::HostSetup { message, command } = host_setup(r#"{"need":"libraries","command":"apt install x"}"#)
        else {
            panic!()
        };
        assert!(message.contains("missing libraries"));
        assert_eq!(command, "apt install x");
        assert_eq!(host_setup("garbage"), host_setup(r#"{"need":"sandbox","command":"sudo npx t3 browser setup"}"#));
    }
}
