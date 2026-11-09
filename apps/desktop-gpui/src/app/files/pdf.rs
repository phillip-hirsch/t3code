//! `PdfViewer`: PDFs read in PDF.js (`assets/vendor/pdfjs-*.zip`, Apache-2.0,
//! `assets/vendor/pdfjs-LICENSE`) in a viewer page (`super::viewer`), with the web's toolbar, find
//! and virtualized pages (`assets/pdf-viewer`). Each file's reading position is kept while the
//! window lives.

use std::borrow::Cow;
use std::collections::VecDeque;

use gpui_kit::Context;
use serde::{Deserialize, Serialize};

use super::FileKey;
use super::viewer::{ViewerSpec, archive_file, fill_page};
use crate::app::Workspace;

/// PDF.js 6.4.299's legacy build, its character maps, standard fonts and image decoders.
const PDFJS_ZIP: &[u8] = include_bytes!("../../../assets/vendor/pdfjs-6.4.299.zip");
const VIEWER_HTML: &str = include_str!("../../../assets/pdf-viewer/viewer.html");
const VIEWER_JS: &str = include_str!("../../../assets/pdf-viewer/viewer.mjs");
const LAYOUT_JS: &str = include_str!("../../../assets/pdf-viewer/layout.mjs");
const VIEWER_CSS: &str = include_str!("../../../assets/pdf-viewer/viewer.css");

pub(super) static VIEWER: ViewerSpec = ViewerSpec {
    scheme: "t3pdf",
    #[cfg(windows)]
    url: "http://t3pdf.localhost/",
    #[cfg(not(windows))]
    url: "t3pdf://localhost/",
    document: "/document.pdf",
    document_type: "application/pdf",
    // `readBoundedDocument`'s limit for PDFs.
    limit: 50 * 1024 * 1024,
    too_large: "This file is too large to preview.",
    unavailable: Some("This file is unavailable or too large to preview."),
    // What the web shows when the file's asset URL cannot be signed.
    unsigned: "Unable to load file preview.",
    page: pdf_page,
    static_file,
    report: |this, key, body, _| this.files.documents.record_pdf(key, body),
};

/// The page's global API, which restyles and reloads it.
pub(super) const GLOBAL: &str = "t3Pdf";

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

/// The viewer page, with the file's title and the position it was last read at.
fn viewer_page(title: &str, saved: Option<&PdfPosition>, theme: &super::viewer::ViewerTheme) -> String {
    fill_page(VIEWER_HTML, theme, &serde_json::json!({ "title": title, "saved": saved }))
}

/// The page, as the file's viewer opens or reloads it.
fn pdf_page(this: &mut Workspace, key: &FileKey, cx: &Context<Workspace>) -> String {
    let theme = this.viewer_theme(key, cx);
    viewer_page(super::logic::file_name(&key.path), this.files.documents.pdf_positions.get(key), &theme)
}

/// One of the viewer's own files or PDF.js's, by its URL path.
fn static_file(path: &str) -> Option<Cow<'static, [u8]>> {
    let borrowed = |text: &'static str| Some(Cow::Borrowed(text.as_bytes()));
    match path {
        "/viewer.mjs" => borrowed(VIEWER_JS),
        "/layout.mjs" => borrowed(LAYOUT_JS),
        "/viewer.css" => borrowed(VIEWER_CSS),
        _ => archive_file(PDFJS_ZIP, path),
    }
}

/// Reads a changed file again, from where the reader is.
pub(super) const RELOAD_SCRIPT: &str = "window.t3Pdf&&window.t3Pdf.reload()";

impl super::document::Documents {
    /// Records a viewer's report for `key`; the panel doesn't redraw for it.
    fn record_pdf(&mut self, key: &FileKey, body: &str) {
        if let Some(message) = viewer_message(body) {
            self.pdf_positions.record(key, message);
        }
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
        let theme = super::super::viewer::ViewerTheme::of(&tokens);
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
            assert_eq!(super::super::viewer::content_type(path), kind, "{path}");
        }
        assert!(static_file("/LICENSE").is_some_and(|body| body.starts_with(b"\n") || body.contains(&b'A')));
        assert!(static_file("/missing.js").is_none());
        assert!(static_file("/").is_none());
    }
}
