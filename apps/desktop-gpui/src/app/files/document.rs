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
use serde_json::{Value, json};

use super::FileKey;
use super::logic::{self, FileKind};
use crate::app::{Target, Workspace};

/// Document views kept alive while not drawn, so switching tabs back keeps the page.
const KEPT_VIEWS: usize = 2;

struct DocView {
    view: Option<Entity<WebView>>,
    failed: Option<String>,
    /// The workspace mutation and `fileRevision` the page loaded.
    mutation: (Option<String>, Option<String>),
    revision: Option<String>,
    /// A rendered diagram's source revision.
    content: u64,
    shown: bool,
    used: u64,
    _task: Option<Task<()>>,
    _links: Option<Task<()>>,
}

impl Documents {
    /// Whether `key`'s diagram must be built for source `revision`.
    pub(super) fn stale(&self, key: &FileKey, revision: u64) -> bool {
        self.views.get(key).is_none_or(|doc| doc.content != revision)
    }
}

#[derive(Default)]
pub(super) struct Documents {
    views: HashMap<FileKey, DocView>,
    /// The document drawn this frame.
    drawn: Option<FileKey>,
    frame: u64,
}

/// A colour as CSS: `#rrggbb`, or `rgba(...)` when translucent like many dark tokens.
fn css_hex(color: Hsla) -> String {
    let rgba = gpui_kit::Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    let (r, g, b) = (byte(rgba.r), byte(rgba.g), byte(rgba.b));
    if rgba.a < 1. { format!("rgba({r},{g},{b},{:.3})", rgba.a) } else { format!("#{r:02x}{g:02x}{b:02x}") }
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

/// `pele` 1.8.0's global build (MIT, `assets/vendor/pele-LICENSE`), the web's Mermaid renderer.
const PELE_GZ: &[u8] = include_bytes!("../../../assets/vendor/pele-1.8.0.global.js.gz");
const MERMAID_SCHEME: &str = "t3mermaid";
#[cfg(windows)]
const MERMAID_PAGE_URL: &str = "http://t3mermaid.localhost/";
#[cfg(not(windows))]
const MERMAID_PAGE_URL: &str = "t3mermaid://localhost/";

fn pele_script() -> &'static [u8] {
    static SCRIPT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    SCRIPT.get_or_init(|| {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(PELE_GZ), &mut out);
        out
    })
}

/// The colours pele reads (`.pele` in the web's CSS).
pub(super) struct DiagramColors {
    pub background: String,
    pub foreground: String,
    pub muted: String,
    pub surface: String,
    pub surface_alt: String,
    pub accent: String,
    pub critical: String,
    pub dark: bool,
}

/// `MermaidViewer`: the diagram, or the web's refusal for sources that could load resources.
/// The page may run only its own script and load nothing else.
fn mermaid_page(source: &str, colors: &DiagramColors) -> String {
    // JSON is valid JavaScript; `<` is escaped so the source cannot close the script.
    let source = serde_json::to_string(source).unwrap_or_default().replace('<', "\\u003c");
    let DiagramColors { background, foreground, muted, surface, surface_alt, accent, critical, dark } = colors;
    let scheme = if *dark { "dark" } else { "light" };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" \
         content=\"default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:\">\
         <style>html{{color-scheme:{scheme};background:{background};color:{foreground}}}\
         body{{margin:0;padding:24px;font:12px system-ui,sans-serif}}#d{{display:flex;justify-content:center}}\
         #d svg{{max-width:100%;height:auto}}.note{{color:{muted};line-height:1.6}}.error{{color:{critical}}}\
         .pele{{--pele-bg:{background};--pele-fg:{foreground};--pele-muted:{muted};--pele-line:{muted};\
         --pele-surface:{surface};--pele-surface-alt:{surface_alt};\
         --pele-border:color-mix(in srgb,{foreground} 28%,transparent);--pele-accent:{accent};--pele-critical:{critical}}}\
         </style><script src=\"/pele.js\"></script></head><body><div id=\"d\" class=\"pele\"></div><script>\
         const source = {source}; const root = document.getElementById('d');\
         const note = (text, cls) => {{ const p = document.createElement('p'); p.className = cls; p.textContent = text; root.replaceChildren(p); }};\
         if (/%%\\s*\\{{|^\\s*---|<\\/?[A-Za-z]|<[!?]|@\\s*\\{{|\\b(?:img|image)\\s*:|url\\s*\\(|@import|(?:https?|data|blob|file):/im.test(source)) {{\
           note('This diagram contains resource URLs, markup, node metadata, or custom configuration. Show Mermaid source to inspect it safely.', 'note');\
         }} else {{ try {{ pele.mount(root, source.trim(), {{ links: false, images: false, zoom: false, autoDirection: false,\
           onError: (e) => note('Unable to render diagram: ' + (e && e.message || 'The diagram could not be rendered.'), 'error') }}); }}\
           catch (e) {{ note('Unable to render diagram: ' + (e && e.message || 'The diagram could not be rendered.'), 'error'); }} }}\
         </script></body></html>"
    )
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
    pub(super) fn asset_resource(&self, key: &FileKey, kind: &'static str) -> Value {
        let absolute = logic::absolute_path(&key.checkout.cwd, &key.path);
        match &self.target {
            Some(Target::Thread { id: thread_id, .. }) => {
                let inside = !logic::is_host_path(&key.path);
                json!({"_tag": if inside { kind } else { "media-file" }, "threadId": thread_id, "path": absolute})
            }
            _ => json!({"_tag": "draft-workspace-file", "cwd": key.checkout.cwd, "path": absolute}),
        }
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
        let mutation = self.files.mutation.clone();
        let documents = &mut self.files.documents;
        documents.drawn = Some(key.clone());
        let state = documents.views.entry(key.clone()).or_insert_with(|| DocView {
            view: None,
            failed: None,
            mutation: mutation.clone(),
            revision: None,
            content: 0,
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

    fn document_resource(&self, key: &FileKey, kind: FileKind) -> Value {
        self.asset_resource(key, if kind == FileKind::Html { "workspace-file" } else { "media-file" })
    }

    fn start_document(&mut self, key: FileKey, kind: FileKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some((client, endpoint)) = self.file_connection(&key) else { return };
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
                    cx.update(|cx| cx.open_url(&url));
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
            let prepared = match (minted, font) {
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
                    Ok((builder, revision))
                }
            };
            let _ = this.update_in(cx, |this, window, cx| match prepared {
                Ok((builder, revision)) => this.build_view(request_key, builder, revision, window, cx),
                Err(error) => this.fail_view(&request_key, error, cx),
            });
        });
        if let Some(state) = self.files.documents.views.get_mut(&key) {
            state._task = Some(task);
            state._links = Some(links_task);
        }
    }

    /// Builds `key`'s native view from a task: see [`build_view`](crate::app::preview::build_view).
    fn build_view(
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

    /// A Mermaid file's diagram (`MermaidViewer`), rebuilt when its source changes.
    pub(super) fn render_mermaid(
        &mut self,
        key: &FileKey,
        revision: u64,
        source: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *crate::theme::tokens(cx);
        let dark = self.dark_theme(cx);
        let mutation = self.files.mutation.clone();
        let documents = &mut self.files.documents;
        documents.drawn = Some(key.clone());
        if let Some(source) = source {
            documents.views.remove(key);
            let colors = DiagramColors {
                background: css_hex(tokens.background),
                foreground: css_hex(tokens.foreground),
                muted: css_hex(tokens.muted_foreground),
                surface: css_hex(tokens.secondary),
                surface_alt: css_hex(tokens.accent),
                accent: css_hex(tokens.primary),
                critical: css_hex(tokens.error),
                dark,
            };
            let page = mermaid_page(&source, &colors);
            let builder = wry::WebViewBuilder::new()
                .with_custom_protocol(MERMAID_SCHEME.into(), move |_, request| {
                    let (body, content_type): (Vec<u8>, &str) = if request.uri().path() == "/pele.js" {
                        (pele_script().to_vec(), "text/javascript")
                    } else {
                        (page.clone().into_bytes(), "text/html")
                    };
                    wry::http::Response::builder()
                        .header("content-type", content_type)
                        .body(std::borrow::Cow::Owned(body))
                        .unwrap_or_default()
                })
                .with_url(MERMAID_PAGE_URL);
            documents.views.insert(
                key.clone(),
                DocView {
                    view: None,
                    failed: None,
                    mutation,
                    revision: None,
                    content: revision,
                    shown: false,
                    used: 0,
                    _task: None,
                    _links: None,
                },
            );
            self.build_view(key.clone(), builder, None, window, cx);
        }
        match self.files.documents.views.get(key).map(|doc| (&doc.view, &doc.failed)) {
            Some((Some(view), _)) => div().flex_1().min_h_0().size_full().child(view.clone()).into_any_element(),
            Some((None, Some(error))) => {
                super::chrome::failure(format!("Unable to render diagram: {error}"), None, &tokens)
            }
            _ => super::chrome::loading(&tokens),
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
    pub(super) fn open_file_in_browser(&mut self, key: &FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some((client, endpoint)) = self.file_connection(key) else { return };
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
        assert_eq!(resolve_asset_url("http://h:1", "httpx/y"), "http://h:1/httpx/y", "a path, not a URL");
        assert_eq!(css_hex(gpui_kit::rgb(0x0a0b0c).into()), "#0a0b0c");
        assert_eq!(css_hex(gpui_kit::rgba(0xffffff80).into()), "rgba(255,255,255,0.502)");
        assert_eq!(font_mime(b"wOF2...."), Some("font/woff2"));
        assert_eq!(font_mime(&[0, 1, 0, 0, 9]), Some("font/ttf"));
        assert_eq!(font_mime(b"PK\x03\x04"), None);
        assert!(font_specimen("#000", "#fff", true).contains("src:url('/font')"));
        let colors = DiagramColors {
            background: "#000".into(),
            foreground: "#fff".into(),
            muted: "#888".into(),
            surface: "#111".into(),
            surface_alt: "#222".into(),
            accent: "#00f".into(),
            critical: "#f00".into(),
            dark: true,
        };
        let page = mermaid_page("graph TD\n A</script><b>x</b>", &colors);
        assert!(page.contains("const source = \"graph TD\\n A\\u003c/script>\\u003cb>x\\u003c/b>\";"));
        assert!(page.contains("default-src 'none'") && page.contains("--pele-bg:#000"));
        assert!(pele_script().starts_with(b"/*! Pele 1.8.0"), "the vendored renderer decompresses");
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
