//! `PdfViewer`: PDFs read in PDF.js (`assets/vendor/pdfjs-*.zip`, Apache-2.0,
//! `assets/vendor/pdfjs-LICENSE`) inside a native WebView, with the web's toolbar, find and
//! virtualized pages (`assets/pdf-viewer`). The app serves the page, PDF.js and the file's bytes
//! over a custom protocol, and keeps each file's reading position while the window lives.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::Read as _;

use gpui_kit::{AppContext as _, Context, Hsla, Window};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::FileKey;
use super::document::{css_hex, mint_asset_url};
use crate::app::Workspace;
use crate::theme::Tokens;

/// PDF.js 6.4.299's legacy build, its character maps, standard fonts and image decoders.
const PDFJS_ZIP: &[u8] = include_bytes!("../../../assets/vendor/pdfjs-6.4.299.zip");
const VIEWER_HTML: &str = include_str!("../../../assets/pdf-viewer/viewer.html");
const VIEWER_JS: &str = include_str!("../../../assets/pdf-viewer/viewer.mjs");
const LAYOUT_JS: &str = include_str!("../../../assets/pdf-viewer/layout.mjs");
const VIEWER_CSS: &str = include_str!("../../../assets/pdf-viewer/viewer.css");

const PDF_SCHEME: &str = "t3pdf";
/// wry serves a custom protocol at `http://<name>.localhost` on Windows and `<name>://localhost` elsewhere.
#[cfg(windows)]
const PDF_PAGE_URL: &str = "http://t3pdf.localhost/";
#[cfg(not(windows))]
const PDF_PAGE_URL: &str = "t3pdf://localhost/";
const DOCUMENT_PATH: &str = "/document.pdf";

/// `readBoundedDocument`'s limit for PDFs.
const MAX_BYTES: u64 = 50 * 1024 * 1024;
const TOO_LARGE: &str = "This file is too large to preview.";
const UNAVAILABLE: &str = "This file is unavailable or too large to preview.";
/// What the web shows when the file's asset URL cannot be signed.
const UNSIGNED: &str = "Unable to load file preview.";

/// `PDF_MIN_ZOOM` and `PDF_MAX_ZOOM`, and the page count the viewer opens.
const MIN_ZOOM: f64 = 0.25;
const MAX_ZOOM: f64 = 3.;
const MAX_PAGES: usize = 2000;
/// `MAX_POSITIONS`: the reading positions kept, most recently read first out last.
const MAX_POSITIONS: usize = 50;

/// Where a reader was in a PDF (`PdfViewState`), with the page width and page shapes it was
/// read at so a reopened file lands on the same spot before its pages are measured again.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(super) struct PdfPosition {
    page: u32,
    offset: f64,
    zoom: Option<f64>,
    left: f64,
    /// Absent until the viewer reports a view: the restore then reads the offset at today's width.
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<f64>,
    ratios: Vec<f64>,
}

/// What the viewer reports through `window.ipc`.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ViewerMessage {
    View { page: u32, offset: f64, zoom: Option<f64>, left: f64, width: f64 },
    Ratios { ratios: Vec<f64> },
}

/// A report the store can take: positions are finite, zoom stays in the viewer's range and the
/// page shapes are positive.
fn viewer_message(body: &str) -> Option<ViewerMessage> {
    let message: ViewerMessage = serde_json::from_str(body).ok()?;
    let valid = match &message {
        ViewerMessage::View { offset, zoom, left, width, .. } => {
            [*offset, *left].iter().all(|value| value.is_finite() && *value >= 0.)
                && width.is_finite()
                && *width > 0.
                && zoom.is_none_or(|zoom| (MIN_ZOOM..=MAX_ZOOM).contains(&zoom))
        }
        ViewerMessage::Ratios { ratios } => {
            ratios.len() <= MAX_PAGES && ratios.iter().all(|ratio| ratio.is_finite() && *ratio > 0.)
        }
    };
    valid.then_some(message)
}

/// The reading positions of recently read PDFs, kept per file while the window lives.
#[derive(Default)]
pub(super) struct Positions {
    /// Least recently read first.
    entries: VecDeque<(FileKey, PdfPosition)>,
}

impl Positions {
    fn get(&self, key: &FileKey) -> Option<&PdfPosition> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, position)| position)
    }

    fn record(&mut self, key: &FileKey, message: ViewerMessage) {
        let mut position = match self.entries.iter().position(|(k, _)| k == key) {
            Some(index) => self.entries.remove(index).map(|(_, position)| position).unwrap_or_default(),
            None => PdfPosition::default(),
        };
        match message {
            ViewerMessage::View { page, offset, zoom, left, width } => {
                (position.page, position.offset, position.zoom, position.left) = (page, offset, zoom, left);
                position.width = Some(width);
            }
            ViewerMessage::Ratios { ratios } => position.ratios = ratios,
        }
        self.entries.push_back((key.clone(), position));
        if self.entries.len() > MAX_POSITIONS {
            self.entries.pop_front();
        }
    }
}

/// The tokens the viewer's chrome reads, compared each frame so a theme change restyles an open page.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct PdfTheme {
    dark: bool,
    background: Hsla,
    foreground: Hsla,
    muted: Hsla,
    muted_foreground: Hsla,
    accent: Hsla,
    border: Hsla,
    input: Hsla,
    ring: Hsla,
    popover: Hsla,
    destructive: Hsla,
    thumb: Hsla,
    thumb_hover: Hsla,
    sans: &'static str,
    root_size: f32,
}

impl PdfTheme {
    pub(super) fn of(tokens: &Tokens) -> Self {
        Self {
            dark: tokens.dark,
            background: tokens.background,
            foreground: tokens.foreground,
            muted: tokens.muted,
            muted_foreground: tokens.muted_foreground,
            accent: tokens.accent,
            border: tokens.border,
            input: tokens.input,
            ring: tokens.ring,
            popover: tokens.popover,
            destructive: tokens.error,
            thumb: tokens.scrollbar_thumb,
            thumb_hover: tokens.scrollbar_thumb_hover,
            sans: tokens.fonts.sans,
            root_size: tokens.fonts.interface_size,
        }
    }

    /// The page's custom properties. The app's bundled Inter is served beside the page; any
    /// other family is the system's, and macOS's system font is `system-ui`.
    fn css(&self) -> String {
        let bundled = self.sans == "Inter";
        let family: String =
            self.sans.chars().filter(|c| !matches!(c, '"' | '\\' | '<' | '>' | ';' | '{' | '}')).collect();
        let family = if family.starts_with('.') || family.is_empty() {
            "system-ui, sans-serif".to_owned()
        } else {
            format!("\"{family}\", system-ui, sans-serif")
        };
        let input_background = if self.dark { css_hex(self.input.opacity(0.32)) } else { css_hex(self.background) };
        format!(
            "{font}:root{{color-scheme:{scheme};font-size:{size}px;--font-sans:{family};--background:{background};\
             --foreground:{foreground};--muted:{muted};--muted-foreground:{muted_foreground};--placeholder:{muted_foreground};\
             --accent:{accent};--border:{border};--input:{input};--input-background:{input_background};--ring:{ring};\
             --popover:{popover};--destructive:{destructive};--scrollbar-thumb:{thumb};--scrollbar-thumb-hover:{thumb_hover}}}",
            font = if bundled { "@font-face{font-family:\"Inter\";src:url(\"/inter.ttf\")}" } else { "" },
            scheme = if self.dark { "dark" } else { "light" },
            size = self.root_size,
            background = css_hex(self.background),
            foreground = css_hex(self.foreground),
            muted = css_hex(self.muted),
            muted_foreground = css_hex(self.muted_foreground),
            accent = css_hex(self.accent),
            border = css_hex(self.border),
            input = css_hex(self.input),
            ring = css_hex(self.ring),
            popover = css_hex(self.popover),
            destructive = css_hex(self.destructive),
            thumb = css_hex(self.thumb),
            thumb_hover = css_hex(self.thumb_hover),
        )
    }
}

/// JSON as a script's text: `<` is escaped so it cannot close the element.
fn script_json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into()).replace('<', "\\u003c")
}

/// The viewer page, with the file's title and the position it was last read at.
fn viewer_page(title: &str, saved: Option<&PdfPosition>, theme: &PdfTheme) -> String {
    let state = script_json(&serde_json::json!({ "title": title, "saved": saved }));
    VIEWER_HTML.replace("{{theme}}", &theme.css().replace('<', "")).replace("{{state}}", &state)
}

/// The script that restyles an open page.
fn theme_script(theme: &PdfTheme) -> String {
    format!("window.t3Pdf&&window.t3Pdf.theme({})", script_json(&theme.css()))
}

/// Reads a changed file again, from where the reader is.
pub(super) const RELOAD_SCRIPT: &str = "window.t3Pdf&&window.t3Pdf.reload()";

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("mjs" | "js") => "text/javascript",
        Some("css") => "text/css",
        Some("wasm") => "application/wasm",
        Some("ttf") => "font/ttf",
        Some("pfb" | "bcmap") => "application/octet-stream",
        _ => "text/plain",
    }
}

/// One of the viewer's own files or PDF.js's, by its URL path.
fn static_file(path: &str) -> Option<Cow<'static, [u8]>> {
    let borrowed = |text: &'static str| Some(Cow::Borrowed(text.as_bytes()));
    match path {
        "/viewer.mjs" => borrowed(VIEWER_JS),
        "/layout.mjs" => borrowed(LAYOUT_JS),
        "/viewer.css" => borrowed(VIEWER_CSS),
        "/inter.ttf" => Some(Cow::Borrowed(crate::theme::FONTS[0])),
        _ => {
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(PDFJS_ZIP)).ok()?;
            let mut file = archive.by_name(path.strip_prefix('/')?).ok()?;
            let mut bytes = Vec::with_capacity(usize::try_from(file.size()).unwrap_or_default());
            file.read_to_end(&mut bytes).ok()?;
            Some(Cow::Owned(bytes))
        }
    }
}

fn respond(responder: wry::RequestAsyncResponder, status: u16, content_type: &str, body: Cow<'static, [u8]>) {
    let response = wry::http::Response::builder()
        .status(status)
        .header("content-type", content_type)
        .header("cache-control", "no-store")
        .body(body)
        .unwrap_or_default();
    responder.respond(response);
}

/// A request the page made over the custom protocol.
struct PageRequest {
    path: String,
    responder: wry::RequestAsyncResponder,
}

impl super::document::Documents {
    /// Records a viewer's report for `key`; the panel doesn't redraw for it.
    fn record_pdf(&mut self, key: &FileKey, body: &str) {
        if let Some(message) = viewer_message(body) {
            self.pdf_positions.record(key, message);
        }
    }
}

impl Workspace {
    /// Opens `key`'s viewer. The page asks for its pieces over the protocol: the page itself, built
    /// with the current theme and saved position, PDF.js's files, and the PDF, signed and fetched
    /// each time so a retry after a reconnect uses the environment's new connection.
    pub(super) fn start_pdf(&mut self, key: FileKey, window: &mut Window, cx: &mut Context<Self>) {
        let resource = self.asset_resource(&key, "media-file");
        let (requests, incoming) = async_channel::unbounded::<PageRequest>();
        let (reports, inbox) = async_channel::unbounded::<String>();
        let builder = wry::WebViewBuilder::new()
            .with_devtools(true)
            .with_asynchronous_custom_protocol(PDF_SCHEME.into(), move |_, request, responder| {
                let _ = requests.try_send(PageRequest { path: request.uri().path().to_owned(), responder });
            })
            .with_ipc_handler(move |request| {
                let _ = reports.try_send(request.into_body());
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_url(PDF_PAGE_URL);
        let serve_key = key.clone();
        let serve = cx.spawn(async move |this, cx| {
            while let Ok(PageRequest { path, responder }) = incoming.recv().await {
                match path.as_str() {
                    "/" => {
                        let page = this.update(cx, |this, cx| this.pdf_page(&serve_key, cx)).ok();
                        match page {
                            Some(page) => respond(responder, 200, "text/html", Cow::Owned(page.into_bytes())),
                            None => respond(responder, 404, "text/plain", Cow::Borrowed(b"")),
                        }
                    }
                    DOCUMENT_PATH => {
                        let source = this.update(cx, |this, _| this.pdf_source(&serve_key)).ok().flatten();
                        let (this, key) = (this.clone(), serve_key.clone());
                        cx.spawn(async move |cx| {
                            let fetched = fetch_pdf(source).await;
                            match fetched {
                                Ok((bytes, revision)) => {
                                    let _ =
                                        this.update(cx, |this, _| this.files.documents.set_revision(&key, revision));
                                    respond(responder, 200, "application/pdf", Cow::Owned(bytes));
                                }
                                Err(message) => {
                                    respond(responder, 502, "text/plain", Cow::Borrowed(message.as_bytes()));
                                }
                            }
                        })
                        .detach();
                    }
                    _ => {
                        let file = cx.background_spawn({
                            let path = path.clone();
                            async move { static_file(&path) }
                        });
                        cx.spawn(async move |_| match file.await {
                            Some(body) => respond(responder, 200, content_type(&path), body),
                            None => respond(responder, 404, "text/plain", Cow::Borrowed(b"")),
                        })
                        .detach();
                    }
                }
            }
        });
        let report_key = key.clone();
        let listen = cx.spawn(async move |this, cx| {
            while let Ok(body) = inbox.recv().await {
                let _ = this.update(cx, |this, _| this.files.documents.record_pdf(&report_key, &body));
            }
        });
        self.files.documents.begin_pdf(&key, resource, vec![serve, listen]);
        self.build_view(key, builder, None, window, cx);
    }

    /// The page, as the file's viewer opens or reloads it.
    fn pdf_page(&mut self, key: &FileKey, cx: &Context<Self>) -> String {
        let theme = PdfTheme::of(crate::theme::tokens(cx));
        let documents = &mut self.files.documents;
        documents.set_pdf_theme(key, theme);
        viewer_page(super::logic::file_name(&key.path), documents.pdf_positions.get(key), &theme)
    }

    /// What signs the file's URL: the environment's current connection and the resource the
    /// viewer opened with.
    fn pdf_source(&self, key: &FileKey) -> Option<(crate::rpc::RpcClient, String, Value)> {
        let resource = self.files.documents.pdf_resource(key)?.clone();
        let (client, endpoint) = self.file_connection(key)?;
        Some((client, endpoint.http_base, resource))
    }

    /// Restyles an open viewer after a theme change.
    pub(super) fn sync_pdf_theme(&mut self, key: &FileKey, cx: &Context<Self>) {
        let theme = PdfTheme::of(crate::theme::tokens(cx));
        if let Some(view) = self.files.documents.pdf_theme_changed(key, theme) {
            let _ = view.read(cx).raw().evaluate_script(&theme_script(&theme));
        }
    }
}

/// The PDF's bytes and the `fileRevision` they were signed at, or the message the viewer shows.
async fn fetch_pdf(
    source: Option<(crate::rpc::RpcClient, String, Value)>,
) -> Result<(Vec<u8>, Option<String>), &'static str> {
    let (client, http_base, resource) = source.ok_or(UNSIGNED)?;
    let (url, revision) = mint_asset_url(client, http_base, resource).await.map_err(|_| UNSIGNED)?;
    match super::asset::fetch_bounded(url, MAX_BYTES, TOO_LARGE).await {
        Ok(bytes) => Ok((bytes, revision)),
        Err(error) if error == TOO_LARGE => Err(TOO_LARGE),
        Err(_) => Err(UNAVAILABLE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::files::Checkout;

    fn key(path: &str) -> FileKey {
        FileKey { checkout: Checkout { env: "env".into(), cwd: "/repo".into() }, path: path.into() }
    }

    fn view(page: u32) -> ViewerMessage {
        ViewerMessage::View { page, offset: 12.5, zoom: None, left: 0., width: 600. }
    }

    #[test]
    fn the_viewer_reports_views_and_page_shapes() {
        assert_eq!(
            viewer_message(r#"{"type":"view","page":3,"offset":40,"zoom":1.5,"left":8,"width":918}"#),
            Some(ViewerMessage::View { page: 3, offset: 40., zoom: Some(1.5), left: 8., width: 918. })
        );
        assert_eq!(
            viewer_message(r#"{"type":"view","page":0,"offset":0,"zoom":null,"left":0,"width":468}"#),
            Some(ViewerMessage::View { page: 0, offset: 0., zoom: None, left: 0., width: 468. })
        );
        assert_eq!(
            viewer_message(r#"{"type":"ratios","ratios":[1.29,0.5]}"#),
            Some(ViewerMessage::Ratios { ratios: vec![1.29, 0.5] })
        );
        for rejected in [
            r#"{"type":"view","page":0,"offset":-1,"zoom":null,"left":0,"width":468}"#,
            r#"{"type":"view","page":0,"offset":0,"zoom":9,"left":0,"width":468}"#,
            r#"{"type":"view","page":0,"offset":0,"zoom":null,"left":0,"width":0}"#,
            r#"{"type":"view","page":-1,"offset":0,"zoom":null,"left":0,"width":468}"#,
            r#"{"type":"ratios","ratios":[1,0]}"#,
            r#"{"type":"other"}"#,
            "not json",
        ] {
            assert_eq!(viewer_message(rejected), None, "{rejected}");
        }
        let too_many = format!(r#"{{"type":"ratios","ratios":[{}]}}"#, vec!["1"; MAX_PAGES + 1].join(","));
        assert_eq!(viewer_message(&too_many), None);
    }

    #[test]
    fn positions_merge_reports_and_keep_the_most_recently_read_files() {
        let mut positions = Positions::default();
        positions.record(&key("a.pdf"), ViewerMessage::Ratios { ratios: vec![1.5] });
        positions.record(&key("a.pdf"), view(2));
        assert_eq!(
            positions.get(&key("a.pdf")),
            Some(&PdfPosition { page: 2, offset: 12.5, zoom: None, left: 0., width: Some(600.), ratios: vec![1.5] })
        );
        // The same path on another checkout is another file.
        let other = FileKey { checkout: Checkout { env: "other".into(), cwd: "/repo".into() }, path: "a.pdf".into() };
        assert_eq!(positions.get(&other), None);

        for index in 0..MAX_POSITIONS {
            positions.record(&key(&format!("{index}.pdf")), view(1));
            if index == 10 {
                // Reading a file again keeps it.
                positions.record(&key("a.pdf"), view(3));
            }
        }
        assert_eq!(positions.entries.len(), MAX_POSITIONS);
        assert_eq!(positions.get(&key("a.pdf")).map(|p| p.page), Some(3));
        assert_eq!(positions.get(&key("0.pdf")), None, "the least recently read file goes first");
        assert!(positions.get(&key("1.pdf")).is_some());
    }

    #[test]
    fn the_page_carries_its_state_and_theme_safely() {
        let tokens = crate::theme::Tokens::dark();
        let theme = PdfTheme::of(&tokens);
        let saved =
            PdfPosition { page: 4, offset: 9., zoom: Some(1.25), left: 3., width: Some(500.), ratios: vec![1.] };
        let page = viewer_page("a</script><b>.pdf", Some(&saved), &theme);
        // `<` as the JSON escape the page carries.
        let lt = format!("{}u{:04x}", char::from(92), u32::from('<'));
        assert!(page.contains(&format!(r#""title":"a{lt}/script>{lt}b>.pdf""#)), "the title cannot close the script");
        assert!(!page.contains("a</script>"));
        assert!(
            page.contains(r#""saved":{"page":4,"offset":9.0,"zoom":1.25,"left":3.0,"width":500.0,"ratios":[1.0]}"#)
        );
        assert!(page.contains("--background:") && !page.contains("{{"), "both placeholders are filled");
        assert!(viewer_page("a.pdf", None, &theme).contains(r#""saved":null"#));
        assert!(theme_script(&theme).starts_with("window.t3Pdf&&window.t3Pdf.theme(\""));
    }

    #[test]
    fn the_protocol_serves_the_viewer_and_pdfjs() {
        for (path, kind) in [
            ("/viewer.mjs", "text/javascript"),
            ("/layout.mjs", "text/javascript"),
            ("/viewer.css", "text/css"),
            ("/pdf.min.mjs", "text/javascript"),
            ("/pdf.worker.min.mjs", "text/javascript"),
            ("/wasm/openjpeg.wasm", "application/wasm"),
            ("/cmaps/UniJIS-UCS2-H.bcmap", "application/octet-stream"),
            ("/standard_fonts/LiberationSans-Regular.ttf", "font/ttf"),
            ("/inter.ttf", "font/ttf"),
        ] {
            assert!(static_file(path).is_some_and(|body| !body.is_empty()), "{path}");
            assert_eq!(content_type(path), kind, "{path}");
        }
        assert!(static_file("/LICENSE").is_some_and(|body| body.starts_with(b"\n") || body.contains(&b'A')));
        assert!(static_file("/missing.js").is_none());
        assert!(static_file("/").is_none());
    }
}
