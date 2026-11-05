//! Files the web hands to the browser engine (`BrowserDocumentFrame`, `WorkspaceVideoPreview`,
//! `WorkspaceAudioPreview`): PDFs, rendered HTML, video and audio play in a native WebView of
//! the file's signed asset URL. Also "Open file in preview browser".

use std::collections::HashMap;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Sizable as _, WindowExt as _, v_flex};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use gpui_webview::WebView;
use raw_window_handle::HasWindowHandle as _;
use serde_json::{Value, json};

use super::logic::{self, FileKind};
use crate::app::preview::ParentWindow;
use crate::app::{Target, Workspace};

type Key = (String, String);

/// Document views kept alive while not drawn, so switching tabs back keeps the page.
const KEPT_VIEWS: usize = 2;

struct DocView {
    view: Option<Entity<WebView>>,
    failed: Option<String>,
    /// The workspace mutation and `fileRevision` the page loaded.
    mutation: (Option<String>, Option<String>),
    revision: Option<String>,
    shown: bool,
    used: u64,
    _task: Option<Task<()>>,
    _links: Option<Task<()>>,
}

#[derive(Default)]
pub(super) struct Documents {
    views: HashMap<Key, DocView>,
    /// The document drawn this frame.
    drawn: Option<Key>,
    frame: u64,
}

/// A colour as CSS `#rrggbb`.
fn css_hex(color: Hsla) -> String {
    let rgba = gpui_kit::Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", byte(rgba.r), byte(rgba.g), byte(rgba.b))
}

fn escape_attribute(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

/// The page a document's view opens: PDFs load their URL in the engine's own viewer
/// (`#navpanes=0&view=FitH`, as `PdfDocumentRevision` does); HTML runs in a sandboxed frame with an
/// opaque origin (`FileDocument`); media get the browser's controls on the panel's background.
enum Page {
    Url(String),
    Html(String),
}

fn document_page(kind: FileKind, url: &str, background: &str, dark: bool) -> Page {
    let src = escape_attribute(url);
    let scheme = if dark { "dark" } else { "light" };
    let shell = |body_style: &str, body: String| {
        Page::Html(format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"{scheme}\">\
             <style>html,body{{margin:0;height:100%;background:{background}}}body{{{body_style}}}</style></head>\
             <body>{body}</body></html>"
        ))
    };
    match kind {
        FileKind::Pdf => Page::Url(format!("{}#navpanes=0&view=FitH", url.split('#').next().unwrap_or(url))),
        FileKind::Video => shell(
            "display:flex;align-items:center;justify-content:center;padding:16px;box-sizing:border-box",
            format!(
                "<video controls playsinline preload=\"metadata\" src=\"{src}\" \
                 style=\"max-width:min(100%,64rem);max-height:100%\"></video>"
            ),
        ),
        FileKind::Audio => shell(
            "display:flex;align-items:center;justify-content:center;padding:24px;box-sizing:border-box",
            format!("<audio controls preload=\"metadata\" src=\"{src}\" style=\"width:100%;max-width:36rem\"></audio>"),
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

const FONT_MAX_BYTES: u64 = 10 * 1024 * 1024;
const FONT_UNSUPPORTED: &str = "The font is damaged or unsupported.";
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
    let sizes: String = [12, 16, 24, 40, 64]
        .iter()
        .map(|size| {
            format!(
                "<p style=\"font-size:{size}px\"><small>{size} px</small>The quick brown fox jumps over the lazy dog.</p>"
            )
        })
        .collect();
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
    if relative.starts_with("http") {
        relative.to_owned()
    } else {
        format!("{}/{}", http_base.trim_end_matches('/'), relative.trim_start_matches('/'))
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
    pub(super) fn asset_resource(&self, key: &Key, kind: &'static str) -> Value {
        let absolute = logic::absolute_path(&key.0, &key.1);
        match &self.target {
            Some(Target::Thread(thread_id)) => {
                let inside = !logic::is_host_path(&key.1);
                json!({"_tag": if inside { kind } else { "media-file" }, "threadId": thread_id, "path": absolute})
            }
            _ => json!({"_tag": "draft-workspace-file", "cwd": key.0, "path": absolute}),
        }
    }

    /// The document's view, built on first draw and reloaded when a workspace mutation gives
    /// the file a new revision.
    pub(super) fn render_document(
        &mut self,
        key: &Key,
        kind: FileKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *crate::theme::tokens(cx);
        let mutation = self.files.mutation.clone();
        let documents = &mut self.files.documents;
        documents.drawn = Some(key.clone());
        let state = documents.views.entry(key.clone()).or_insert_with(|| DocView {
            view: None,
            failed: None,
            mutation: mutation.clone(),
            revision: None,
            shown: false,
            used: 0,
            _task: None,
            _links: None,
        });
        if state.view.is_none() && state._task.is_none() && state.failed.is_none() {
            self.start_document(key.clone(), kind, window, cx);
        } else if state.view.is_some() && state.mutation != mutation {
            state.mutation = mutation;
            self.refresh_document(key.clone(), kind, window, cx);
        }
        let Some(state) = self.files.documents.views.get(key) else { return div().into_any_element() };
        match (&state.view, &state.failed) {
            (Some(view), _) => div().flex_1().min_h_0().size_full().child(view.clone()).into_any_element(),
            (None, Some(error)) if kind == FileKind::Font => {
                super::chrome::no_preview(format!("{error} Save it to open in another app."), &tokens)
            }
            (None, Some(_)) => {
                let retry = key.clone();
                let message =
                    if kind == FileKind::Audio { "Unable to load audio." } else { "Unable to load file preview." };
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .px(px(24.))
                    .text_xs()
                    .child(div().text_color(tokens.error).child(message))
                    .child(
                        div()
                            .id("document-retry")
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

    fn document_resource(&self, key: &Key, kind: FileKind) -> Value {
        self.asset_resource(key, if kind == FileKind::Html { "workspace-file" } else { "media-file" })
    }

    fn start_document(&mut self, key: Key, kind: FileKind, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(endpoint)) = (self.client(), self.endpoint.clone()) else { return };
        let parent = match window.window_handle() {
            Ok(handle) => ParentWindow(handle.as_raw()),
            Err(_) => return,
        };
        let resource = self.document_resource(&key, kind);
        let tokens = *crate::theme::tokens(cx);
        let (background, dark) = (css_hex(tokens.background), self.dark_theme(cx));
        let foreground = css_hex(tokens.foreground);
        let request_key = key.clone();
        // A page's popup or `target=_blank` link opens in the system browser.
        let (links, opened) = async_channel::unbounded::<String>();
        let links_task = cx.spawn(async move |_, cx| {
            while let Ok(url) = opened.recv().await {
                if url.starts_with("http:") || url.starts_with("https:") {
                    let _ = cx.update(|cx| cx.open_url(&url));
                }
            }
        });
        let task = cx.spawn_in(window, async move |this, cx| {
            let minted =
                mint_asset_url(client, endpoint.http_base, resource).await.map_err(|e| logic::error_message(&e));
            let font = match (&minted, kind) {
                (Ok((url, _)), FileKind::Font) => Some(
                    super::asset::fetch_bounded(url.clone(), FONT_MAX_BYTES, "Font previews are limited to 10 MB.")
                        .await
                        .and_then(|bytes| {
                            font_mime(&bytes).map(|mime| (bytes, mime)).ok_or_else(|| FONT_UNSUPPORTED.to_owned())
                        }),
                ),
                _ => None,
            };
            let built = match (minted, font) {
                (Ok(_), Some(Err(error))) | (Err(error), _) => Err(error),
                (Ok((url, revision)), font) => {
                    let builder =
                        wry::WebViewBuilder::new().with_devtools(true).with_new_window_req_handler(move |url, _| {
                            let _ = links.try_send(url);
                            wry::NewWindowResponse::Deny
                        });
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
                        (_, Page::Url(url)) => builder.with_url(url),
                        (_, Page::Html(html)) => builder.with_html(html),
                    };
                    builder.build_as_child_async(&parent).await.map(|view| (view, revision)).map_err(|e| e.to_string())
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(state) = this.files.documents.views.get_mut(&request_key) else { return };
                state._task = None;
                match built {
                    Ok((webview, revision)) => {
                        let webview = WebView::new(webview, window, cx);
                        // Hidden until a frame lays it out, so it never flashes at the window's origin.
                        let _ = webview.set_visible(false);
                        state.view = Some(cx.new(|_| webview));
                        state.revision = revision;
                        state.shown = false;
                    }
                    Err(error) => {
                        log::warn!("file preview: {error}");
                        state.failed = Some(error);
                    }
                }
                cx.notify();
            });
        });
        if let Some(state) = self.files.documents.views.get_mut(&key) {
            state._task = Some(task);
            state._links = Some(links_task);
        }
    }

    /// Mints the URL again after a workspace mutation; a new `fileRevision` (or, for HTML on
    /// servers without one, any mutation) reloads the page.
    fn refresh_document(&mut self, key: Key, kind: FileKind, window: &mut Window, cx: &mut Context<Self>) {
        if kind == FileKind::Font {
            // The page holds the font's bytes: a changed font needs a new view.
            self.files.documents.views.remove(&key);
            return;
        }
        let (Some(client), Some(endpoint)) = (self.client(), self.endpoint.clone()) else { return };
        let resource = self.document_resource(&key, kind);
        let tokens = *crate::theme::tokens(cx);
        let (background, dark) = (css_hex(tokens.background), self.dark_theme(cx));
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
                    let _ = match document_page(kind, &url, &background, dark) {
                        Page::Url(url) => raw.load_url(&url),
                        Page::Html(html) => raw.load_html(&html),
                    };
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
        let covered = self.browser_covered(window, cx);
        let documents = &mut self.files.documents;
        documents.frame += 1;
        let frame = documents.frame;
        let drawn = documents.drawn.take();
        for (key, doc) in &mut documents.views {
            let drawn = drawn.as_ref() == Some(key);
            if drawn {
                doc.used = frame;
            }
            let shown = drawn && !covered;
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
    pub(super) fn open_file_in_browser(&mut self, key: &Key, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(endpoint)) = (self.client(), self.endpoint.clone()) else { return };
        let Some(thread_id) = self.preview_thread_id().map(str::to_owned) else { return };
        let resource = self.asset_resource(key, "workspace-file");
        cx.spawn_in(window, async move |this, cx| match mint_asset_url(client, endpoint.http_base, resource).await {
            Ok((url, _)) => {
                let _ = this.update_in(cx, |this, window, cx| this.open_preview_tab(thread_id, Some(url), window, cx));
            }
            Err(error) => {
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::error(logic::error_message(&error)).title("Unable to open file in browser"),
                        cx,
                    )
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
        let Page::Url(pdf) = document_page(FileKind::Pdf, "http://h/a.pdf#x", "#000", true) else { panic!() };
        assert_eq!(pdf, "http://h/a.pdf#navpanes=0&view=FitH");
        let Page::Html(html) = document_page(FileKind::Html, "http://h/a?x=1&y=\"2\"", "#000", false) else { panic!() };
        assert!(html.contains("sandbox=\"allow-scripts allow-forms allow-popups allow-downloads\""));
        assert!(html.contains("src=\"http://h/a?x=1&amp;y=&quot;2&quot;\""), "the URL is escaped");
        let Page::Html(audio) = document_page(FileKind::Audio, "u", "#123456", true) else { panic!() };
        assert!(audio.contains("<audio controls") && audio.contains("background:#123456"));
        assert!(audio.contains("content=\"dark\""));
        assert_eq!(resolve_asset_url("http://h:1/", "/api/assets/t/a.pdf"), "http://h:1/api/assets/t/a.pdf");
        assert_eq!(resolve_asset_url("http://h:1", "https://x/y"), "https://x/y");
        assert_eq!(css_hex(gpui_kit::rgb(0x0a0b0c).into()), "#0a0b0c");
        assert_eq!(font_mime(b"wOF2...."), Some("font/woff2"));
        assert_eq!(font_mime(&[0, 1, 0, 0, 9]), Some("font/ttf"));
        assert_eq!(font_mime(b"PK\x03\x04"), None);
        assert!(font_specimen("#000", "#fff", true).contains("src:url('/font')"));
    }
}
