//! The file viewers the web runs as pages of its own (`PdfViewer`, `OpenXmlOfficeViewer`), hosted
//! in a native WebView. The app serves each viewer's page, its engine's files and the file's bytes
//! over a custom protocol, themes the page from the app's tokens, and hears the page's reports
//! through `window.ipc`.

use std::borrow::Cow;
use std::io::Read as _;

use gpui_kit::{AppContext as _, Context, Hsla, Window};
use serde::Serialize;
use serde_json::Value;

use super::FileKey;
use super::document::{css_hex, mint_asset_url};
use crate::app::Workspace;
use crate::theme::Tokens;

/// One viewer kind: where its page lives, how it reads the file and what it does with reports.
pub(super) struct ViewerSpec {
    /// The custom protocol's name; wry serves it at `http://<name>.localhost` on Windows and
    /// `<name>://localhost` elsewhere (`url`).
    pub(super) scheme: &'static str,
    pub(super) url: &'static str,
    /// The path the page reads the file's bytes from, and their content type.
    pub(super) document: &'static str,
    pub(super) document_type: &'static str,
    pub(super) limit: u64,
    pub(super) too_large: &'static str,
    /// What any other failed read shows; `None` shows the read's own message.
    pub(super) unavailable: Option<&'static str>,
    /// What the page shows when the file's URL cannot be signed.
    pub(super) unsigned: &'static str,
    /// The page, built as the view opens or reloads.
    pub(super) page: fn(&mut Workspace, &FileKey, &Context<Workspace>) -> String,
    /// The viewer's own files and its engine's, by URL path.
    pub(super) static_file: fn(&str) -> Option<Cow<'static, [u8]>>,
    /// A report the page sent through `window.ipc`.
    pub(super) report: fn(&mut Workspace, &FileKey, &str, &mut Context<Workspace>),
}

/// The tokens a viewer's chrome reads, compared each frame so a theme change restyles an open page.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct ViewerTheme {
    dark: bool,
    background: Hsla,
    foreground: Hsla,
    muted: Hsla,
    muted_foreground: Hsla,
    accent: Hsla,
    border: Hsla,
    card: Hsla,
    input: Hsla,
    ring: Hsla,
    popover: Hsla,
    primary: Hsla,
    destructive: Hsla,
    warning: Hsla,
    warning_foreground: Hsla,
    thumb: Hsla,
    thumb_hover: Hsla,
    sans: &'static str,
    root_size: f32,
}

impl ViewerTheme {
    pub(super) fn of(tokens: &Tokens) -> Self {
        Self {
            dark: tokens.dark,
            background: tokens.background,
            foreground: tokens.foreground,
            muted: tokens.muted,
            muted_foreground: tokens.muted_foreground,
            accent: tokens.accent,
            border: tokens.border,
            card: tokens.card,
            input: tokens.input,
            ring: tokens.ring,
            popover: tokens.popover,
            primary: tokens.primary,
            destructive: tokens.error,
            warning: tokens.warning,
            warning_foreground: tokens.warning_foreground,
            thumb: tokens.scrollbar_thumb,
            thumb_hover: tokens.scrollbar_thumb_hover,
            sans: tokens.fonts.sans,
            root_size: tokens.fonts.interface_size,
        }
    }

    /// The page's custom properties. The app's bundled Inter is served beside the page; any
    /// other family is the system's, and macOS's system font is `system-ui`.
    pub(super) fn css(&self) -> String {
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
             --accent:{accent};--border:{border};--card:{card};--input:{input};--input-background:{input_background};--ring:{ring};\
             --popover:{popover};--primary:{primary};--destructive:{destructive};--warning:{warning};\
             --warning-foreground:{warning_foreground};--scrollbar-thumb:{thumb};--scrollbar-thumb-hover:{thumb_hover}}}",
            font = if bundled { "@font-face{font-family:\"Inter\";src:url(\"/inter.ttf\")}" } else { "" },
            scheme = if self.dark { "dark" } else { "light" },
            size = self.root_size,
            background = css_hex(self.background),
            foreground = css_hex(self.foreground),
            muted = css_hex(self.muted),
            muted_foreground = css_hex(self.muted_foreground),
            accent = css_hex(self.accent),
            border = css_hex(self.border),
            card = css_hex(self.card),
            input = css_hex(self.input),
            ring = css_hex(self.ring),
            popover = css_hex(self.popover),
            primary = css_hex(self.primary),
            destructive = css_hex(self.destructive),
            warning = css_hex(self.warning),
            warning_foreground = css_hex(self.warning_foreground),
            thumb = css_hex(self.thumb),
            thumb_hover = css_hex(self.thumb_hover),
        )
    }
}

/// JSON as a script's text: `<` is escaped so it cannot close the element.
pub(super) fn script_json(value: &impl Serialize) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into()).replace('<', "\\u003c")
}

/// A viewer page with its theme and state filled in.
pub(super) fn fill_page(html: &str, theme: &ViewerTheme, state: &impl Serialize) -> String {
    html.replace("{{theme}}", &theme.css().replace('<', "")).replace("{{state}}", &script_json(state))
}

/// The script that restyles an open page whose API is `window.<global>`.
pub(super) fn theme_script(global: &str, theme: &ViewerTheme) -> String {
    format!("window.{global}&&window.{global}.theme({})", script_json(&theme.css()))
}

pub(super) fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("mjs" | "js") => "text/javascript",
        Some("css") => "text/css",
        Some("wasm") => "application/wasm",
        Some("ttf") => "font/ttf",
        Some("pfb" | "bcmap") => "application/octet-stream",
        _ => "text/plain",
    }
}

/// A file of an embedded archive, by URL path; the app's Inter is served beside every viewer.
pub(super) fn archive_file(archive: &'static [u8], path: &str) -> Option<Cow<'static, [u8]>> {
    if path == "/inter.ttf" {
        return Some(Cow::Borrowed(crate::theme::FONTS[0]));
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(archive)).ok()?;
    let mut file = archive.by_name(path.strip_prefix('/')?).ok()?;
    let mut bytes = Vec::with_capacity(usize::try_from(file.size()).unwrap_or_default());
    file.read_to_end(&mut bytes).ok()?;
    Some(Cow::Owned(bytes))
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

impl Workspace {
    /// Opens `key`'s viewer. The page asks for its pieces over the protocol: the page itself, built
    /// with the current theme, the engine's files, and the file, signed and fetched each time so a
    /// retry after a reconnect uses the environment's new connection.
    pub(super) fn start_viewer(
        &mut self,
        key: FileKey,
        spec: &'static ViewerSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let resource = self.asset_resource(&key, "media-file");
        let (requests, incoming) = async_channel::unbounded::<PageRequest>();
        let (reports, inbox) = async_channel::unbounded::<String>();
        let builder = wry::WebViewBuilder::new()
            .with_devtools(true)
            .with_asynchronous_custom_protocol(spec.scheme.into(), move |_, request, responder| {
                let _ = requests.try_send(PageRequest { path: request.uri().path().to_owned(), responder });
            })
            .with_ipc_handler(move |request| {
                let _ = reports.try_send(request.into_body());
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_url(spec.url);
        let serve_key = key.clone();
        let serve = cx.spawn(async move |this, cx| {
            while let Ok(PageRequest { path, responder }) = incoming.recv().await {
                if path == "/" {
                    match this.update(cx, |this, cx| (spec.page)(this, &serve_key, cx)).ok() {
                        Some(page) => respond(responder, 200, "text/html", Cow::Owned(page.into_bytes())),
                        None => respond(responder, 404, "text/plain", Cow::Borrowed(b"")),
                    }
                } else if path == spec.document {
                    let source = this.update(cx, |this, _| this.viewer_source(&serve_key)).ok().flatten();
                    let (this, key) = (this.clone(), serve_key.clone());
                    cx.spawn(async move |cx| match fetch_document(spec, source).await {
                        Ok((bytes, revision)) => {
                            let _ = this.update(cx, |this, _| this.files.documents.set_revision(&key, revision));
                            respond(responder, 200, spec.document_type, Cow::Owned(bytes));
                        }
                        Err(message) => respond(responder, 502, "text/plain", Cow::Owned(message.into_bytes())),
                    })
                    .detach();
                } else {
                    let file = cx.background_spawn({
                        let path = path.clone();
                        async move { (spec.static_file)(&path) }
                    });
                    cx.spawn(async move |_| match file.await {
                        Some(body) => respond(responder, 200, content_type(&path), body),
                        None => respond(responder, 404, "text/plain", Cow::Borrowed(b"")),
                    })
                    .detach();
                }
            }
        });
        let report_key = key.clone();
        let listen = cx.spawn(async move |this, cx| {
            while let Ok(body) = inbox.recv().await {
                let _ = this.update(cx, |this, cx| (spec.report)(this, &report_key, &body, cx));
            }
        });
        self.files.documents.begin_viewer(&key, resource, vec![serve, listen]);
        self.build_view(key, builder, None, window, cx);
    }

    /// What signs the file's URL: the environment's current connection and the resource the
    /// viewer opened with.
    fn viewer_source(&self, key: &FileKey) -> Option<(crate::rpc::RpcClient, String, Value)> {
        let resource = self.files.documents.viewer_resource(key)?.clone();
        let (client, endpoint) = self.file_connection(key)?;
        Some((client, endpoint.http_base, resource))
    }

    /// The theme for `key`'s page as it is built, which later frames compare against.
    pub(super) fn viewer_theme(&mut self, key: &FileKey, cx: &Context<Self>) -> ViewerTheme {
        let theme = ViewerTheme::of(crate::theme::tokens(cx));
        self.files.documents.set_theme(key, &theme);
        theme
    }

    /// Restyles an open viewer, whose API is `window.<global>`, after a theme change.
    pub(super) fn sync_viewer_theme(&mut self, key: &FileKey, global: &str, cx: &Context<Self>) {
        let theme = ViewerTheme::of(crate::theme::tokens(cx));
        if let Some(view) = self.files.documents.theme_changed(key, &theme) {
            let _ = view.read(cx).raw().evaluate_script(&theme_script(global, &theme));
        }
    }
}

/// The file's bytes and the `fileRevision` they were signed at, or the message the page shows.
async fn fetch_document(
    spec: &ViewerSpec,
    source: Option<(crate::rpc::RpcClient, String, Value)>,
) -> Result<(Vec<u8>, Option<String>), String> {
    let (client, http_base, resource) = source.ok_or(spec.unsigned)?;
    let (url, revision) = mint_asset_url(client, http_base, resource).await.map_err(|_| spec.unsigned)?;
    match super::asset::fetch_bounded(url, spec.limit, spec.too_large).await {
        Ok(bytes) => Ok((bytes, revision)),
        Err(error) if error == spec.too_large => Err(error),
        Err(error) => Err(spec.unavailable.map_or(error, str::to_owned)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_carry_their_state_and_theme_safely() {
        let theme = ViewerTheme::of(&crate::theme::Tokens::dark());
        let page = fill_page("<style>{{theme}}</style><script>{{state}}</script>", &theme, &"a</script>");
        // `<` as the JSON escape the page carries.
        let lt = format!("{}u{:04x}", char::from(92), u32::from('<'));
        assert!(page.contains(&format!("\"a{lt}/script>\"")), "the state cannot close the script");
        assert!(page.contains("--background:") && page.contains("--warning-foreground:") && !page.contains("{{"));
        assert!(theme_script("t3Office", &theme).starts_with("window.t3Office&&window.t3Office.theme(\""));
    }

    #[test]
    fn content_types_follow_extensions() {
        for (path, kind) in [
            ("/a.mjs", "text/javascript"),
            ("/assets/w.js", "text/javascript"),
            ("/v.css", "text/css"),
            ("/e.wasm", "application/wasm"),
            ("/f.ttf", "font/ttf"),
            ("/c.bcmap", "application/octet-stream"),
            ("/LICENSE", "text/plain"),
        ] {
            assert_eq!(content_type(path), kind, "{path}");
        }
    }
}
