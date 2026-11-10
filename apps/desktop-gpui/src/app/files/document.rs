//! Files the web hands to the browser engine (`BrowserDocumentFrame`, `WorkspaceVideoPreview`,
//! `WorkspaceAudioPreview`): rendered HTML, video and audio play in a native WebView of the
//! file's signed asset URL, and PDFs and Office files in viewer pages (`super::viewer`). Also
//! "Open file in preview browser".

use std::collections::HashMap;
use std::fmt::Write as _;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use gpui_webview::WebView;
use serde_json::{Value, json};

use super::FileKey;
use super::logic::{self, FileKind, Office};
use super::media::MediaKind;
use crate::app::mermaid::{REFUSAL, Shown, css_color};
use crate::app::{Target, Workspace};

/// Document views kept alive while not drawn, so switching tabs back keeps the page.
const KEPT_VIEWS: usize = 2;

pub(super) struct DocView {
    kind: FileKind,
    view: Option<Entity<WebView>>,
    failed: Option<String>,
    /// The URL the page loaded, for a failed video's "Open in browser".
    url: Option<String>,
    /// Drawn last frame: a media file that stops drawing pauses, as the web's unmount does.
    drawn: bool,
    /// The workspace mutation and `fileRevision` the page loaded.
    mutation: (Option<String>, Option<String>),
    revision: Option<String>,
    shown: bool,
    used: u64,
    /// A viewer page's resource, signed again for each read of the file, and the theme it shows.
    resource: Option<Value>,
    theme: Option<super::viewer::ViewerTheme>,
    _task: Option<Task<()>>,
    _links: Option<Task<()>>,
    _media: Option<Task<()>>,
    /// A viewer page's protocol and report listeners.
    _viewer: Vec<Task<()>>,
}

impl DocView {
    fn new(kind: FileKind, mutation: (Option<String>, Option<String>)) -> Self {
        Self {
            kind,
            view: None,
            failed: None,
            url: None,
            drawn: false,
            mutation,
            revision: None,
            shown: false,
            used: 0,
            resource: None,
            theme: None,
            _task: None,
            _links: None,
            _media: None,
            _viewer: Vec::new(),
        }
    }
}

/// What a media page's script reports through `window.ipc`.
#[derive(Debug, PartialEq)]
enum MediaMessage {
    /// The element could not play its source (`onError`).
    Failed,
    /// A video's context menu, at a point of the page.
    Menu { x: f32, y: f32 },
}

fn media_message(body: &str) -> Option<MediaMessage> {
    let value: Value = serde_json::from_str(body).ok()?;
    #[expect(clippy::cast_possible_truncation, reason = "page coordinates are small")]
    let coordinate = |name: &str| value[name].as_f64().map(|v| v as f32).filter(|v| v.is_finite()).unwrap_or(0.);
    match value["type"].as_str()? {
        "error" => Some(MediaMessage::Failed),
        "menu" => Some(MediaMessage::Menu { x: coordinate("x"), y: coordinate("y") }),
        _ => None,
    }
}

/// The media element's bridge (`MediaVideoPlayer`, `AudioPreview`): a changed file loads once
/// playback pauses, and a failure the element can't recover from by loading a newer source is
/// reported.
const MEDIA_SCRIPT: &str = "const m=document.getElementById('m');let next=null;\
const post=(message)=>window.ipc.postMessage(JSON.stringify(message));\
const load=(u)=>{next=null;m.src=u;};const adopt=()=>{if(next!==null&&m.paused)load(next);};\
m.addEventListener('pause',adopt);m.addEventListener('ended',adopt);\
m.addEventListener('error',()=>{if(next!==null)load(next);else post({type:'error'});});\
window.t3Media={adopt:(u)=>{next=u;adopt();},pause:()=>m.pause()};";

/// `MediaActions` on a video: the app's menu replaces the engine's, at the pointer or, from the
/// keyboard, under the video.
const VIDEO_MENU_SCRIPT: &str = "const menu=(x,y)=>{if(x===0&&y===0){const r=m.getBoundingClientRect();x=r.left;y=r.bottom;}\
post({type:'menu',x,y});};\
document.addEventListener('contextmenu',(e)=>{e.preventDefault();menu(e.clientX,e.clientY);});\
document.addEventListener('keydown',(e)=>{if(e.key==='ContextMenu'||(e.shiftKey&&e.key==='F10')){\
e.preventDefault();menu(0,0);}});";

/// Hands a media page the file's new URL (see [`MEDIA_SCRIPT`]).
fn media_adopt_script(url: &str) -> String {
    format!("window.t3Media&&window.t3Media.adopt({})", serde_json::to_string(url).unwrap_or_default())
}

const MEDIA_PAUSE_SCRIPT: &str = "window.t3Media&&window.t3Media.pause()";

impl Documents {
    /// Records that `key`'s view is on screen this frame: in the panel, or a draft's in its dialog.
    fn mark_drawn(&mut self, key: &FileKey) {
        if matches!(key.origin, super::Origin::Draft { .. }) {
            self.dialog = Some((key.clone(), self.frame));
        } else {
            self.drawn = Some(key.clone());
        }
    }

    /// The draft document a dialog drew last frame, if it is still open.
    fn dialog_view(&self, dialog_open: bool) -> Option<&FileKey> {
        self.dialog.as_ref().filter(|(_, frame)| dialog_open && frame + 1 >= self.frame).map(|(key, _)| key)
    }

    /// Drops `key`'s view, so its next draw builds it again.
    pub(super) fn forget(&mut self, key: &FileKey) {
        self.views.remove(key);
    }

    /// Marks `key`'s viewer page as starting, with the resource it reads and its listeners.
    pub(super) fn begin_viewer(&mut self, key: &FileKey, resource: Option<Value>, tasks: Vec<Task<()>>) {
        if let Some(doc) = self.views.get_mut(key) {
            doc.resource = resource;
            doc._viewer = tasks;
            doc._task = Some(Task::ready(()));
        }
    }

    pub(super) fn viewer_resource(&self, key: &FileKey) -> Option<&Value> {
        self.views.get(key)?.resource.as_ref()
    }

    /// The `fileRevision` the viewer last read, which a workspace mutation compares against.
    pub(super) fn set_revision(&mut self, key: &FileKey, revision: Option<String>) {
        if let Some(doc) = self.views.get_mut(key) {
            doc.revision = revision;
        }
    }

    pub(super) fn set_theme(&mut self, key: &FileKey, theme: &super::viewer::ViewerTheme) {
        if let Some(doc) = self.views.get_mut(key) {
            doc.theme = Some(*theme);
        }
    }

    /// `key`'s view when its page shows a theme other than `theme`, which it now records.
    pub(super) fn theme_changed(
        &mut self,
        key: &FileKey,
        theme: &super::viewer::ViewerTheme,
    ) -> Option<Entity<WebView>> {
        let doc = self.views.get_mut(key)?;
        if doc.theme.is_none_or(|shown| shown == *theme) {
            return None;
        }
        doc.theme = Some(*theme);
        doc.view.clone()
    }
}

#[derive(Default)]
pub(super) struct Documents {
    views: HashMap<FileKey, DocView>,
    /// The document drawn this frame.
    drawn: Option<FileKey>,
    /// The draft attachment's document its preview dialog drew, and when. Dialogs draw after the
    /// workspace syncs its views, so this one shows from the next frame while the dialog stays.
    dialog: Option<(FileKey, u64)>,
    frame: u64,
    /// PDF reading positions, kept after a viewer closes.
    pub(super) pdf_positions: super::pdf::Positions,
}

/// The viewer page that shows `kind`, and its global API's name.
fn viewer_of(kind: FileKind) -> Option<(&'static super::viewer::ViewerSpec, &'static str)> {
    match kind {
        FileKind::Pdf => Some((&super::pdf::VIEWER, super::pdf::GLOBAL)),
        // Notebooks render natively (`super::notebook`).
        FileKind::Office(office) if office != Office::Ipynb => Some((&super::office::VIEWER, super::office::GLOBAL)),
        _ => None,
    }
}

fn escape_attribute(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

/// The page a document's view opens: HTML runs in a sandboxed frame with an opaque origin
/// (`FileDocument`); media get the browser's controls on the panel's background.
fn document_page(kind: FileKind, url: &str, background: &str, dark: bool) -> String {
    let src = escape_attribute(url);
    let scheme = if dark { "dark" } else { "light" };
    let shell = |body_style: &str, body: String| {
        format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"{scheme}\">\
             <style>html,body{{margin:0;height:100%;background:{background}}}body{{{body_style}}}</style></head>\
             <body>{body}</body></html>"
        )
    };
    match kind {
        FileKind::Video => shell(
            "display:flex;align-items:center;justify-content:center;padding:16px;box-sizing:border-box",
            format!(
                "<video id=\"m\" controls playsinline preload=\"metadata\" src=\"{src}\" \
                 style=\"max-width:min(100%,64rem);max-height:100%\"></video><script>{MEDIA_SCRIPT}{VIDEO_MENU_SCRIPT}</script>"
            ),
        ),
        FileKind::Audio => shell(
            "display:flex;align-items:center;justify-content:center;padding:24px;box-sizing:border-box",
            format!(
                "<audio id=\"m\" controls preload=\"metadata\" src=\"{src}\" \
                 style=\"width:100%;max-width:36rem\"></audio><script>{MEDIA_SCRIPT}</script>"
            ),
        ),
        _ => shell(
            "background:#fff",
            format!(
                "<iframe src=\"{src}\" sandbox=\"allow-scripts allow-forms allow-popups allow-downloads\" \
                 style=\"display:block;border:0;width:100%;height:100%\"></iframe>"
            ),
        ),
    }
}

/// A chat video's page (`ChatMarkdownVideo`): the same element and bridge as the file panel's,
/// filling an embed sized to the video (see [`crate::app::embed`]), rounded with a faint border on
/// `background`, letterboxed in `fill`. It reports the video's natural size and pauses when its
/// row unmounts.
pub(in crate::app) fn inline_video_page(url: &str, background: &str, border: &str, fill: &str, dark: bool) -> String {
    use crate::app::embed::{FRAME_CSS, FRAME_SCRIPT};
    let src = escape_attribute(url);
    let scheme = if dark { "dark" } else { "light" };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"{scheme}\">\
         <style>{FRAME_CSS}html{{background:{background}}}video{{display:block;width:100%;height:100%;\
         box-sizing:border-box;border:1px solid {border};border-radius:8px;background:{fill};object-fit:contain}}</style></head>\
         <body><div id=\"t3-embed\"><video id=\"m\" controls playsinline preload=\"metadata\" src=\"{src}\"></video></div>\
         <script>{FRAME_SCRIPT}{MEDIA_SCRIPT}m.addEventListener('loadedmetadata',()=>\
         post({{type:'size',w:m.videoWidth,h:m.videoHeight}}));window.t3Hidden=()=>m.pause();</script></body></html>"
    )
}

const FONT_MAX_BYTES: u64 = 10 * 1024 * 1024;
const FONT_UNSUPPORTED: &str = "The font is damaged or unsupported.";
const FONT_TOO_LARGE: &str = "Font previews are limited to 10 MB.";

const FONT_SCHEME: &str = "t3font";
/// wry serves a custom protocol at `http://<name>.localhost` on Windows and `<name>://localhost` elsewhere.
#[cfg(windows)]
const FONT_PAGE_URL: &str = "http://t3font.localhost/";
#[cfg(not(windows))]
const FONT_PAGE_URL: &str = "t3font://localhost/";

/// The font's MIME type by its signature: OpenType, TrueType, collections and WOFF.
fn font_mime(bytes: &[u8]) -> Option<&'static str> {
    match bytes.get(..4)? {
        [0, 1, 0, 0] | b"true" => Some("font/ttf"),
        b"OTTO" => Some("font/otf"),
        b"ttcf" => Some("font/collection"),
        b"wOFF" => Some("font/woff"),
        b"wOF2" => Some("font/woff2"),
        _ => None,
    }
}

/// `FontViewer`'s specimen, its font served beside it.
fn font_specimen(background: &str, foreground: &str, dark: bool) -> String {
    let scheme = if dark { "dark" } else { "light" };
    let mut sizes = String::new();
    for size in [12, 16, 24, 40, 64] {
        let _ = write!(
            sizes,
            "<p style=\"font-size:{size}px\"><small>{size} px</small>The quick brown fox jumps over the lazy dog.</p>"
        );
    }
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" \
         content=\"default-src 'none'; font-src 'self'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\
         <style>@font-face{{font-family:preview;src:url('/font')}}html{{color-scheme:{scheme};background:{background};\
         color:{foreground}}}body{{margin:0;padding:32px;font-family:preview;overflow-wrap:anywhere}}\
         h1{{font-size:40px;font-weight:normal}}p{{line-height:1.4}}small{{font:12px monospace;display:block;\
         opacity:.6;margin-bottom:8px}}</style></head><body><h1>Font preview</h1><p style=\"font-size:24px\">\
         ABCDEFGHIJKLMNOPQRSTUVWXYZ<br>abcdefghijklmnopqrstuvwxyz<br>0123456789 !?&amp;@#%(){{}}[]</p>{sizes}</body></html>"
    )
}

/// An asset URL as the client fetches it: relative URLs join the server's origin.
pub(super) fn resolve_asset_url(http_base: &str, relative: &str) -> String {
    if relative.starts_with("http://") || relative.starts_with("https://") {
        relative.to_owned()
    } else {
        format!("{}/{}", http_base.trim_end_matches('/'), relative.trim_start_matches('/'))
    }
}

/// Whether `url` is on the server at `http_base`: only those requests carry the bearer token.
pub(super) fn is_server_url(url: &str, http_base: &str) -> bool {
    match (url::Url::parse(url), url::Url::parse(http_base)) {
        (Ok(url), Ok(base)) => url.origin() == base.origin(),
        _ => false,
    }
}

/// `assets.createUrl`: the file's URL and the `fileRevision` the server reports for it.
pub(super) async fn mint_asset_url(
    client: crate::rpc::RpcClient,
    http_base: String,
    resource: Value,
) -> Result<(String, Option<String>), crate::rpc::RpcError> {
    let minted = client.request("assets.createUrl", json!({"resource": resource})).await?;
    let url = resolve_asset_url(&http_base, minted["relativeUrl"].as_str().unwrap_or_default());
    Ok((url, minted["fileRevision"].as_str().map(str::to_owned)))
}

impl Workspace {
    /// `workspaceAssetResource`: a draft reads `draft-workspace-file`; a thread reads `kind`
    /// inside the workspace, else the host `media-file`.
    /// An attachment reads its `attachment` asset, `None` for a draft not uploaded yet.
    pub(super) fn asset_resource(&self, key: &FileKey, kind: &'static str) -> Option<Value> {
        if !key.in_workspace() {
            return self.attachment_resource(key, false);
        }
        let absolute = logic::absolute_path(&key.checkout.cwd, &key.path);
        Some(match &self.target {
            Some(Target::Thread { id: thread_id, .. }) => {
                let inside = !logic::is_host_path(&key.path);
                json!({"_tag": if inside { kind } else { "media-file" }, "threadId": thread_id, "path": absolute})
            }
            _ => json!({"_tag": "draft-workspace-file", "cwd": key.checkout.cwd, "path": absolute}),
        })
    }

    /// The document's view, built on first draw and reloaded when a workspace mutation gives
    /// the file a new revision.
    pub(super) fn render_document(
        &mut self,
        key: &FileKey,
        kind: FileKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *crate::theme::tokens(cx);
        let mutation = self.key_mutation(key);
        let documents = &mut self.files.documents;
        documents.mark_drawn(key);
        let state = documents.views.entry(key.clone()).or_insert_with(|| DocView::new(kind, mutation.clone()));
        if state.view.is_none() && state._task.is_none() && state.failed.is_none() {
            match viewer_of(kind) {
                Some((spec, _)) => self.start_viewer(key.clone(), spec, window, cx),
                None => self.start_document(key.clone(), kind, window, cx),
            }
        } else if state.view.is_some() && state.mutation != mutation {
            state.mutation = mutation;
            self.refresh_document(key.clone(), kind, window, cx);
        }
        if let Some((_, global)) = viewer_of(kind) {
            self.sync_viewer_theme(key, global, cx);
        }
        let menu = (kind == FileKind::Video).then(|| self.render_media_menu()).flatten();
        let Some(state) = self.files.documents.views.get(key) else { return div().into_any_element() };
        match (&state.view, &state.failed) {
            (Some(view), _) => {
                div().flex_1().min_h_0().size_full().child(view.clone()).children(menu).into_any_element()
            }
            (None, Some(error)) if kind == FileKind::Font => {
                super::chrome::no_preview(format!("{error} Save it to open in another app."), &tokens)
            }
            (None, Some(_)) if kind == FileKind::Video && key.in_workspace() => {
                let url = state.url.clone();
                self.render_video_failure(key, url, menu, &tokens, cx)
            }
            (None, Some(_)) if matches!(kind, FileKind::Audio | FileKind::Video) => {
                let retry = key.clone();
                let button = super::chrome::retry_button("document-retry", &tokens).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.files.documents.views.remove(&retry);
                        cx.notify();
                    },
                ));
                let message = if kind == FileKind::Audio { "Unable to load audio." } else { "Unable to load video." };
                super::chrome::failure(message, Some(button), &tokens)
            }
            (None, Some(_)) => {
                let retry = key.clone();
                let message = "Unable to load file preview.";
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .px(px(24.))
                    .text_xs()
                    .child(div().text_color(tokens.error).child(message))
                    .child(
                        crate::app::chat::focusable(div().id("document-retry"), &tokens)
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(tokens.input)
                            .cursor_pointer()
                            .hover(|s| s.bg(tokens.accent))
                            .child("Retry")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.files.documents.views.remove(&retry);
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
            (None, None) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(Spinner::new().large().color(tokens.muted_foreground))
                .into_any_element(),
        }
    }

    fn document_resource(&self, key: &FileKey, kind: FileKind) -> Option<Value> {
        self.asset_resource(key, if kind == FileKind::Html { "workspace-file" } else { "media-file" })
    }

    /// `MediaVideoPlayer`'s failed state: the file's name, Retry video, and Open in browser once
    /// a URL was signed. The media actions menu still opens on it.
    fn render_video_failure(
        &self,
        key: &FileKey,
        url: Option<String>,
        menu: Option<AnyElement>,
        tokens: &crate::theme::Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = logic::file_name(&key.path).to_owned();
        let (retry, menu_key, keyboard_key) = (key.clone(), key.clone(), key.clone());
        let view = cx.weak_entity();
        let label =
            if name.is_empty() { "Video unavailable".to_owned() } else { format!("Video unavailable · {name}") };
        let open = url.filter(|url| url.starts_with("http:") || url.starts_with("https:")).map(|url| {
            Button::new("video-open-browser")
                .secondary()
                .small()
                .icon(IconName::ExternalLink)
                .label("Open in browser")
                .on_click(move |_, _, cx| cx.open_url(&url))
        });
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .p(px(16.))
            .child(
                v_flex()
                    .id("video-failure")
                    .w_full()
                    .max_w(px(1024.))
                    .aspect_ratio(16. / 9.)
                    .max_h_full()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(tokens.border.opacity(0.4))
                    .bg(tokens.muted.opacity(0.4))
                    .p(px(16.))
                    .text_center()
                    .text_sm()
                    .text_color(tokens.muted_foreground)
                    .on_mouse_down(
                        gpui_kit::MouseButton::Right,
                        cx.listener(move |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                            this.open_media_menu(&menu_key, MediaKind::Video, None, event.position, window, cx);
                        }),
                    )
                    // Retry and Open in browser take focus; the menu opens from them too.
                    .on_context_menu_key(move |position, window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.open_media_menu(&keyboard_key, MediaKind::Video, None, position, window, cx);
                        });
                    })
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(Icon::new(IconName::TriangleAlert).size(px(14.)).flex_none())
                            .child(label),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_center()
                            .gap(px(8.))
                            .child(
                                Button::new("video-retry")
                                    .secondary()
                                    .small()
                                    .icon(IconName::RotateCw)
                                    .label("Retry video")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.files.documents.views.remove(&retry);
                                        cx.notify();
                                    })),
                            )
                            .children(open),
                    ),
            )
            .children(menu)
            .into_any_element()
    }

    /// A media page's report: a playback failure shows the failed state, a video's context
    /// menu opens the media actions.
    fn on_media_message(&mut self, key: &FileKey, message: MediaMessage, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.files.documents.views.get_mut(key) else { return };
        match message {
            MediaMessage::Failed => {
                log::warn!("file preview: the media element could not play {}", key.path);
                state.view = None;
                state.failed = Some("playback".into());
                cx.notify();
            }
            // An attachment's video has no `MediaActions` menu (`AttachmentFilePreview`).
            MediaMessage::Menu { .. } if !key.in_workspace() => {}
            MediaMessage::Menu { x, y } => {
                let Some(view) = &state.view else { return };
                let origin = view.read(cx).bounds().origin;
                let position = gpui_kit::point(origin.x + px(x), origin.y + px(y));
                self.open_media_menu(key, MediaKind::Video, None, position, window, cx);
            }
        }
    }

    fn start_document(&mut self, key: FileKey, kind: FileKind, window: &mut Window, cx: &mut Context<Self>) {
        // A draft's font is on this machine; every other page loads a signed URL.
        let local = self.draft_bytes(&key).filter(|_| kind == FileKind::Font);
        let fetch = match local {
            Some(bytes) => super::asset::Fetch::Local(bytes),
            None => {
                let Some((client, endpoint)) = self.file_connection(&key) else { return };
                let Some(resource) = self.document_resource(&key, kind) else { return };
                super::asset::Fetch::Signed { client, http_base: endpoint.http_base, resource }
            }
        };
        let tokens = *crate::theme::tokens(cx);
        let (background, dark) = (css_color(tokens.background), self.dark_theme(cx));
        let foreground = css_color(tokens.foreground);
        let request_key = key.clone();
        // A page's popup or `target=_blank` link opens in the system browser.
        let (links, opened) = async_channel::unbounded::<String>();
        let links_task = cx.spawn(async move |_, cx| {
            while let Ok(url) = opened.recv().await {
                if url.starts_with("http:") || url.starts_with("https:") {
                    cx.update(|cx| cx.open_url(&url));
                }
            }
        });
        let (media, reports) = async_channel::unbounded::<String>();
        let media_key = key.clone();
        let media_task = matches!(kind, FileKind::Video | FileKind::Audio).then(|| {
            cx.spawn_in(window, async move |this, cx| {
                while let Ok(body) = reports.recv().await {
                    let Some(message) = media_message(&body) else { continue };
                    let _ =
                        this.update_in(cx, |this, window, cx| this.on_media_message(&media_key, message, window, cx));
                }
            })
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let (minted, font) = match fetch {
                super::asset::Fetch::Local(bytes) => {
                    let font = super::asset::Fetch::Local(bytes).read(FONT_MAX_BYTES, FONT_TOO_LARGE).await;
                    (Ok((String::new(), None)), Some(font.map(|(bytes, _)| bytes).map_err(|error| error.to_string())))
                }
                super::asset::Fetch::Signed { client, http_base, resource } => {
                    let minted =
                        mint_asset_url(client, http_base, resource).await.map_err(|e| logic::error_message(&e));
                    let font = match (&minted, kind) {
                        (Ok((url, _)), FileKind::Font) => {
                            Some(super::asset::fetch_bounded(url.clone(), FONT_MAX_BYTES, FONT_TOO_LARGE).await)
                        }
                        _ => None,
                    };
                    (minted, font)
                }
            };
            let font = font.map(|read| {
                read.and_then(|bytes| {
                    font_mime(&bytes).map(|mime| (bytes, mime)).ok_or_else(|| FONT_UNSUPPORTED.to_owned())
                })
            });
            let prepared = match (minted, font) {
                (Ok(_), Some(Err(error))) | (Err(error), _) => Err(error),
                (Ok((url, revision)), font) => {
                    let mut builder =
                        wry::WebViewBuilder::new().with_devtools(true).with_new_window_req_handler(move |url, _| {
                            let _ = links.try_send(url);
                            wry::NewWindowResponse::Deny
                        });
                    if matches!(kind, FileKind::Video | FileKind::Audio) {
                        builder = builder.with_ipc_handler(move |request| {
                            let _ = media.try_send(request.into_body());
                        });
                    }
                    let loaded = url.clone();
                    let builder = match (font, document_page(kind, &url, &background, dark)) {
                        (Some(Ok((bytes, mime))), _) => {
                            let page = font_specimen(&background, &foreground, dark);
                            builder
                                .with_custom_protocol(FONT_SCHEME.into(), move |_, request| {
                                    let (body, content_type): (Vec<u8>, &str) = if request.uri().path() == "/font" {
                                        (bytes.clone(), mime)
                                    } else {
                                        (page.clone().into_bytes(), "text/html")
                                    };
                                    wry::http::Response::builder()
                                        .header("content-type", content_type)
                                        .body(std::borrow::Cow::Owned(body))
                                        .unwrap_or_default()
                                })
                                .with_url(FONT_PAGE_URL)
                        }
                        (_, html) => builder.with_html(html),
                    };
                    Ok((builder, revision, loaded))
                }
            };
            let _ = this.update_in(cx, |this, window, cx| match prepared {
                Ok((builder, revision, loaded)) => {
                    if let Some(state) = this.files.documents.views.get_mut(&request_key) {
                        state.url = Some(loaded);
                    }
                    this.build_view(request_key, builder, revision, window, cx);
                }
                Err(error) => this.fail_view(&request_key, error, cx),
            });
        });
        if let Some(state) = self.files.documents.views.get_mut(&key) {
            state._task = Some(task);
            state._links = Some(links_task);
            state._media = media_task;
        }
    }

    /// Builds `key`'s native view from a task: see [`build_view`](crate::app::preview::build_view).
    pub(super) fn build_view(
        &mut self,
        key: FileKey,
        builder: wry::WebViewBuilder<'static>,
        revision: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                // The document closed while its view waited to start.
                if !this.files.documents.views.contains_key(&key) {
                    return;
                }
                let built = crate::app::preview::build_view(builder, window, cx);
                let Some(state) = this.files.documents.views.get_mut(&key) else { return };
                state._task = None;
                match built {
                    Ok(webview) => {
                        // Hidden until a frame lays it out, so it never flashes at the window's origin.
                        let _ = webview.set_visible(false);
                        state.view = Some(cx.new(|_| webview));
                        state.revision = revision;
                        state.shown = false;
                    }
                    Err(error) => {
                        log::warn!("file preview: {error}");
                        state.failed = Some(error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn fail_view(&mut self, key: &FileKey, error: String, cx: &mut Context<Self>) {
        let Some(state) = self.files.documents.views.get_mut(key) else { return };
        log::warn!("file preview: {error}");
        state._task = None;
        state.failed = Some(error);
        cx.notify();
    }

    /// A Mermaid file's diagram (`MermaidViewer`), or the refusal for a source that could load
    /// resources; `source` is given when `id` is new. A click expands it.
    pub(super) fn render_mermaid(
        &mut self,
        id: u64,
        source: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *crate::theme::tokens(cx);
        let (shown, source) = self.file_diagram(id, source, window, cx);
        if matches!(shown, Shown::Refused) {
            return div().p(px(24.)).text_xs().text_color(tokens.muted_foreground).child(REFUSAL).into_any_element();
        }
        let this = cx.entity().downgrade();
        match crate::app::mermaid::diagram_body(&this, id, &shown, &source, &tokens) {
            Some(body) => div()
                .id("mermaid-file")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p(px(24.))
                .child(body)
                .into_any_element(),
            None => super::chrome::loading(&tokens),
        }
    }

    /// Mints the URL again after a workspace mutation; a new `fileRevision` (or, for HTML on
    /// servers without one, any mutation) reloads the page.
    fn refresh_document(&mut self, key: FileKey, kind: FileKind, window: &mut Window, cx: &mut Context<Self>) {
        if kind == FileKind::Font {
            // The page holds the font's bytes: a changed font needs a new view.
            self.files.documents.views.remove(&key);
            return;
        }
        let Some((client, endpoint)) = self.file_connection(&key) else { return };
        let Some(resource) = self.document_resource(&key, kind) else { return };
        let tokens = *crate::theme::tokens(cx);
        let (background, dark) = (css_color(tokens.background), self.dark_theme(cx));
        let request_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let Ok((url, revision)) = mint_asset_url(client, endpoint.http_base, resource).await else { return };
            let _ = this.update_in(cx, |this, _, cx| {
                let Some(state) = this.files.documents.views.get_mut(&request_key) else { return };
                state._task = None;
                let changed = match (&revision, &state.revision) {
                    (Some(new), Some(old)) => new != old,
                    _ => kind == FileKind::Html || revision.is_some(),
                };
                if !changed {
                    return;
                }
                state.revision = revision;
                if let Some(view) = &state.view {
                    let raw = view.read(cx).raw();
                    // Media keep playing what they play; the new bytes load once playback pauses.
                    // A viewer page reads the new revision itself: a PDF from where the reader is.
                    let _ = match kind {
                        FileKind::Video | FileKind::Audio => raw.evaluate_script(&media_adopt_script(&url)),
                        FileKind::Pdf => raw.evaluate_script(super::pdf::RELOAD_SCRIPT),
                        FileKind::Office(_) => raw.evaluate_script(super::office::RELOAD_SCRIPT),
                        _ => raw.load_html(&document_page(kind, &url, &background, dark)),
                    };
                    state.url = Some(url);
                }
            });
        });
        if let Some(state) = self.files.documents.views.get_mut(&key) {
            state._task = Some(task);
        }
    }

    /// Shows the drawn document's view and hides the rest, after the panel recorded what it
    /// drew; like the browser's views, a GPUI overlay hides it. Views not drawn lately close.
    pub(in crate::app) fn sync_document_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settle_file_menu(window, cx);
        let covered = self.browser_covered(window, cx);
        let dialog_open = window.has_active_dialog(cx);
        let menu_open = self.files.menu_open;
        let documents = &mut self.files.documents;
        documents.frame += 1;
        let frame = documents.frame;
        let drawn = documents.drawn.take();
        let dialog = documents.dialog_view(dialog_open).cloned();
        for (key, doc) in &mut documents.views {
            let in_dialog = dialog.as_ref() == Some(key);
            let in_panel = drawn.as_ref() == Some(key);
            let drawn = in_panel || in_dialog;
            if drawn {
                doc.used = frame;
            }
            let media = matches!(doc.kind, FileKind::Video | FileKind::Audio);
            if let Some(view) = doc.view.as_ref().filter(|_| media && doc.drawn && !drawn) {
                let _ = view.read(cx).raw().evaluate_script(MEDIA_PAUSE_SCRIPT);
            }
            doc.drawn = drawn;
            // The dialog covers the panel, but its own document sits on top of it.
            let shown = (in_panel && !covered) || (in_dialog && !menu_open);
            if let Some(view) = doc.view.as_ref().filter(|_| doc.shown != shown) {
                doc.shown = shown;
                let _ = view.read(cx).set_visible(shown);
            }
        }
        if documents.views.len() > KEPT_VIEWS {
            let mut used: Vec<u64> = documents.views.values().map(|d| d.used).collect();
            used.sort_unstable_by(|a, b| b.cmp(a));
            let oldest_kept = used[KEPT_VIEWS - 1];
            documents.views.retain(|_, doc| doc.used >= oldest_kept || doc._task.is_some());
        }
    }

    /// `openFileInPreview`: the file's asset URL in a new tab of the integrated browser.
    pub(super) fn open_file_in_browser(&mut self, key: &FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some((client, endpoint)) = self.file_connection(key) else { return };
        let Some(thread) = self.preview_thread() else { return };
        let Some(resource) = self.asset_resource(key, "workspace-file") else { return };
        cx.spawn_in(window, async move |this, cx| match mint_asset_url(client, endpoint.http_base, resource).await {
            Ok((url, _)) => {
                let _ = this.update_in(cx, |this, window, cx| this.open_preview_tab(thread, Some(url), window, cx));
            }
            Err(error) => {
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(logic::error_message(&error)).title("Unable to open file in browser"),
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    /// `openFileInPreview` for a chat file link: a path on the thread's host, in a new tab of
    /// the integrated browser.
    pub(in crate::app) fn open_host_file_in_browser(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(client), Some(endpoint)) = (self.client(), self.endpoint().cloned()) else { return };
        let Some(thread) = self.preview_thread() else { return };
        let inside = crate::app::markdown_ui::workspace_relative(path, self.git_cwd().as_deref()).is_some();
        let resource = json!({"_tag": if inside { "workspace-file" } else { "media-file" }, "threadId": thread.thread_id, "path": path});
        cx.spawn_in(window, async move |this, cx| match mint_asset_url(client, endpoint.http_base, resource).await {
            Ok((url, _)) => {
                let _ = this.update_in(cx, |this, window, cx| this.open_preview_tab(thread, Some(url), window, cx));
            }
            Err(error) => {
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(logic::error_message(&error)).title("Unable to open file in browser"),
                        cx,
                    );
                });
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_pages_follow_the_web_frames() {
        let html = document_page(FileKind::Html, "http://h/a?x=1&y=\"2\"", "#000", false);
        assert!(html.contains("sandbox=\"allow-scripts allow-forms allow-popups allow-downloads\""));
        assert!(html.contains("src=\"http://h/a?x=1&amp;y=&quot;2&quot;\""), "the URL is escaped");
        let audio = document_page(FileKind::Audio, "u", "#123456", true);
        assert!(audio.contains("<audio id=\"m\" controls") && audio.contains("background:#123456"));
        assert!(audio.contains("window.t3Media") && !audio.contains("contextmenu"), "audio has no media actions");
        let video = document_page(FileKind::Video, "u", "#000", true);
        assert!(video.contains("window.t3Media") && video.contains("'contextmenu'"));
        assert_eq!(media_adopt_script("http://h/a\"b"), r#"window.t3Media&&window.t3Media.adopt("http://h/a\"b")"#);
        assert!(audio.contains("content=\"dark\""));
        let chat = inline_video_page("http://h/a\"b.mp4", "#fff", "#eee", "#000", false);
        assert!(chat.contains("src=\"http://h/a&quot;b.mp4\"") && chat.contains("id=\"t3-embed\""));
        assert!(chat.contains("window.t3Media") && chat.contains("type:'size'") && chat.contains("window.t3Hidden"));
        assert!(chat.contains("background:#000;object-fit:contain"), "the letterbox fill");
        assert_eq!(resolve_asset_url("http://h:1/", "/api/assets/t/a.pdf"), "http://h:1/api/assets/t/a.pdf");
        assert_eq!(resolve_asset_url("http://h:1", "https://x/y"), "https://x/y");
        assert_eq!(resolve_asset_url("http://h:1", "httpx/y"), "http://h:1/httpx/y", "a path, not a URL");
        assert_eq!(font_mime(b"wOF2...."), Some("font/woff2"));
        assert_eq!(font_mime(&[0, 1, 0, 0, 9]), Some("font/ttf"));
        assert_eq!(font_mime(b"PK\x03\x04"), None);
        assert!(font_specimen("#000", "#fff", true).contains("src:url('/font')"));
    }

    #[test]
    fn media_pages_report_failures_and_menus() {
        assert_eq!(media_message(r#"{"type":"error"}"#), Some(MediaMessage::Failed));
        assert_eq!(media_message(r#"{"type":"menu","x":12.5,"y":40}"#), Some(MediaMessage::Menu { x: 12.5, y: 40. }));
        assert_eq!(media_message(r#"{"type":"menu"}"#), Some(MediaMessage::Menu { x: 0., y: 0. }));
        assert_eq!(media_message(r#"{"type":"other"}"#), None);
        assert_eq!(media_message("not json"), None);
    }

    #[test]
    fn the_bearer_token_only_goes_to_the_server() {
        let base = "http://127.0.0.1:3999";
        for (url, server) in [
            ("http://127.0.0.1:3999/api/assets/a", true),
            ("http://127.0.0.1:3999", true),
            ("https://127.0.0.1:3999/api/assets/a", false),
            ("http://127.0.0.1:4000/api/assets/a", false),
            ("http://evil.example/api/assets/a", false),
            ("http://127.0.0.1:3999@evil.example/a", false),
            ("not a url", false),
        ] {
            assert_eq!(is_server_url(url, base), server, "{url}");
        }
    }
}
