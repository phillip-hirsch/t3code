//! Office files (`OfficeViewer.tsx`, `OpenXmlOfficeViewer.tsx` and `office/*`) in a viewer page
//! (`super::viewer`): Word documents, slide decks and Excel workbooks laid out and painted by the
//! web's own engines, and `OpenDocument`, rich text and EPUB files read by its preparation worker
//! into pages and sheets. `assets/vendor/office-engine.zip` holds the web's Office workers with the
//! @betteroffice engines (Apache-2.0), their wasm modules and fonts, built by
//! `tools/office_engine.mjs`; `assets/office-viewer` ports the viewers' pages, selection and
//! navigation around them. The extended documents' markup passes `DOMPurify` 3.4.17 (Apache-2.0,
//! `assets/vendor/dompurify-LICENSE`), as `DocumentPages` does.

use std::borrow::Cow;

use gpui_kit::{ClipboardItem, Context};
use serde::Deserialize;

use super::FileKey;
use super::logic::{FileKind, Office};
use super::viewer::{ViewerSpec, archive_file, fill_page};
use crate::app::Workspace;

const ENGINE_ZIP: &[u8] = include_bytes!("../../../assets/vendor/office-engine.zip");
const VIEWER_HTML: &str = include_str!("../../../assets/office-viewer/viewer.html");
const VIEWER_JS: &str = include_str!("../../../assets/office-viewer/viewer.mjs");
const PAGES_JS: &str = include_str!("../../../assets/office-viewer/pages.mjs");
const SPREADSHEET_JS: &str = include_str!("../../../assets/office-viewer/spreadsheet.mjs");
const EXTENDED_JS: &str = include_str!("../../../assets/office-viewer/extended.mjs");
const PURIFY_GZ: &[u8] = include_bytes!("../../../assets/vendor/purify-3.4.17.es.mjs.gz");
const VIEWER_CSS: &str = include_str!("../../../assets/office-viewer/viewer.css");

pub(super) static VIEWER: ViewerSpec = ViewerSpec {
    scheme: "t3office",
    #[cfg(windows)]
    url: "http://t3office.localhost/",
    #[cfg(not(windows))]
    url: "t3office://localhost/",
    document: "/document",
    document_type: "application/octet-stream",
    // `OFFICE_PREVIEW_MAX_BYTES`.
    limit: 50 * 1024 * 1024,
    too_large: "Preview limited to files under 50 MB.",
    unavailable: None,
    unsigned: "Reconnect to the environment and try again.",
    page: office_page,
    static_file,
    report: |_, _, body, cx| match office_message(body) {
        Some(OfficeMessage::Open { url }) => cx.open_url(&url),
        Some(OfficeMessage::Copy { text }) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
        None => {}
    },
};

/// The page's global API, which restyles and reloads it.
pub(super) const GLOBAL: &str = "t3Office";
/// A new revision of the file opens again, as the web's viewer remounts for a new source.
pub(super) const RELOAD_SCRIPT: &str = "window.t3Office&&window.t3Office.reload()";

/// The most text one copy hands the clipboard: up to 200,000 cells, as the sheet allows.
const MAX_COPY_BYTES: usize = 64 * 1024 * 1024;

/// What the viewer asks the app to do through `window.ipc`.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum OfficeMessage {
    /// A document's link, opened in the system browser (`window.open` in the web).
    Open { url: String },
    /// A sheet's selected cells as tab-separated text (`navigator.clipboard.writeText`).
    Copy { text: String },
}

/// A request the app may act on: links only to the schemes the web's `externalUrl` opens.
fn office_message(body: &str) -> Option<OfficeMessage> {
    let message: OfficeMessage = serde_json::from_str(body).ok()?;
    let valid = match &message {
        OfficeMessage::Open { url } => {
            url::Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "mailto"))
        }
        OfficeMessage::Copy { text } => text.len() <= MAX_COPY_BYTES,
    };
    valid.then_some(message)
}

/// The kind the page opens the file as (`officePreviewKind`).
fn office_kind(path: &str) -> &'static str {
    match super::logic::file_kind(path) {
        FileKind::Office(Office::Docx) => "docx",
        FileKind::Office(Office::Xlsx) => "xlsx",
        FileKind::Office(Office::Odt) => "odt",
        FileKind::Office(Office::Ods) => "ods",
        FileKind::Office(Office::Odp) => "odp",
        FileKind::Office(Office::Rtf) => "rtf",
        FileKind::Office(Office::Epub) => "epub",
        _ => "pptx",
    }
}

fn office_page(this: &mut Workspace, key: &FileKey, cx: &Context<Workspace>) -> String {
    let theme = this.viewer_theme(key, cx);
    let state = serde_json::json!({ "title": super::logic::file_name(&key.path), "kind": office_kind(&key.path) });
    fill_page(VIEWER_HTML, &theme, &state)
}

/// One of the viewer's own files or the engine bundle's, by its URL path.
fn static_file(path: &str) -> Option<Cow<'static, [u8]>> {
    let borrowed = |text: &'static str| Some(Cow::Borrowed(text.as_bytes()));
    match path {
        "/viewer.mjs" => borrowed(VIEWER_JS),
        "/pages.mjs" => borrowed(PAGES_JS),
        "/spreadsheet.mjs" => borrowed(SPREADSHEET_JS),
        "/extended.mjs" => borrowed(EXTENDED_JS),
        "/purify.mjs" => {
            let mut script = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(PURIFY_GZ), &mut script).ok()?;
            Some(Cow::Owned(script))
        }
        "/viewer.css" => borrowed(VIEWER_CSS),
        _ => archive_file(ENGINE_ZIP, path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_viewer_may_open_web_and_mail_links_and_copy_cells() {
        assert_eq!(
            office_message(r#"{"type":"open","url":"https://example.com/a?b=1"}"#),
            Some(OfficeMessage::Open { url: "https://example.com/a?b=1".into() })
        );
        assert_eq!(
            office_message(r#"{"type":"open","url":"mailto:a@example.com"}"#),
            Some(OfficeMessage::Open { url: "mailto:a@example.com".into() })
        );
        assert_eq!(
            office_message(r#"{"type":"copy","text":"a\tb\n1\t2"}"#),
            Some(OfficeMessage::Copy { text: "a\tb\n1\t2".into() })
        );
        for rejected in [
            r#"{"type":"open","url":"javascript:alert(1)"}"#,
            r#"{"type":"open","url":"file:///etc/passwd"}"#,
            r#"{"type":"open","url":"tel:123"}"#,
            r#"{"type":"open","url":"not a url"}"#,
            r#"{"type":"copy"}"#,
            r#"{"type":"other"}"#,
            "not json",
        ] {
            assert_eq!(office_message(rejected), None, "{rejected}");
        }
    }

    #[test]
    fn each_file_opens_as_its_kind() {
        assert_eq!(office_kind("a/Report.docx"), "docx");
        assert_eq!(office_kind("Book.XLSX"), "xlsx");
        assert_eq!(office_kind("deck.pptx"), "pptx");
        for kind in ["odt", "ods", "odp", "rtf", "epub"] {
            assert_eq!(office_kind(&format!("a/Notes.{}", kind.to_uppercase())), kind);
        }
    }

    /// The bundle's scripts name their workers, wasm modules and fonts as `` `/assets/<file>` ``.
    fn named_assets(script: &str) -> Vec<String> {
        script.split("`/assets/").skip(1).map(|piece| piece.split('`').next().unwrap_or_default().to_owned()).collect()
    }

    #[test]
    fn the_protocol_serves_the_viewer_and_every_engine_file() {
        for path in [
            "/viewer.mjs",
            "/pages.mjs",
            "/spreadsheet.mjs",
            "/extended.mjs",
            "/viewer.css",
            "/engine.js",
            "/inter.ttf",
        ] {
            assert!(static_file(path).is_some_and(|body| !body.is_empty()), "{path}");
        }
        let purify = String::from_utf8(static_file("/purify.mjs").expect("purify").into_owned()).expect("utf-8");
        assert!(
            purify.starts_with("/*! @license DOMPurify 3.4.17")
                && purify.contains("export { purify_default as default }")
        );
        let engine = String::from_utf8(static_file("/engine.js").expect("engine").into_owned()).expect("utf-8");
        let workers = named_assets(&engine);
        for worker in ["officePreview.worker", "document.worker", "presentation.worker", "spreadsheet.worker"] {
            assert!(workers.iter().any(|file| file.starts_with(worker)), "{worker}");
        }
        let archive = zip::ZipArchive::new(std::io::Cursor::new(ENGINE_ZIP)).expect("archive");
        let scripts: Vec<String> = archive
            .file_names()
            .filter_map(Result::ok)
            .filter(|name| name.ends_with(".js"))
            .map(Cow::into_owned)
            .collect();
        for name in scripts {
            let script = static_file(&format!("/{name}")).expect("listed");
            for file in named_assets(&String::from_utf8_lossy(&script)) {
                assert!(static_file(&format!("/assets/{file}")).is_some(), "{name} names {file}");
            }
        }
        assert!(static_file("/LICENSE-betteroffice").is_some_and(|body| !body.is_empty()));
        assert!(static_file("/missing.js").is_none());
        assert!(static_file("/").is_none());
    }
}
