//! Attachments as files (`AttachmentFilePreview.tsx`): an uploaded attachment's right panel tab
//! (`RightPanelSurfaceContent`'s `attachment:<id>` file surface, which needs no workspace) and
//! a draft attachment's preview dialog. The kind comes from the name and MIME type
//! (`filePreviewKind`), and the file surfaces' own viewers show it, reading the attachment's
//! `attachment` asset or the draft's bytes.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, Selectable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClipboardItem, Context, FontWeight, Image, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use serde_json::{Value, json};

use super::logic::{self, FileKind, Office};
use super::preview::Rendered;
use super::{Checkout, FileKey, Origin, chrome};
use crate::app::Workspace;

/// `FILE_TEXT_PREVIEW_MAX_BYTES`, plus the byte that tells a longer file apart.
pub(super) const TEXT_HEAD_BYTES: usize = TEXT_PREVIEW_MAX_BYTES + 1;
const TEXT_PREVIEW_MAX_BYTES: usize = 1024 * 1024;
/// `useCopyToClipboard`'s default timeout.
const COPIED_FOR: Duration = Duration::from_millis(2000);

/// A file sent with a message (`ChatFileAttachment`): what its panel tab opens. Tabs are the
/// same attachment by id (`attachment:<id>`).
#[derive(Clone, Debug)]
pub(in crate::app) struct AttachmentFile {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

impl PartialEq for AttachmentFile {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for AttachmentFile {}

/// `GENERIC_MIME_TYPES`: types that say nothing about the file.
const GENERIC_MIME_TYPES: [&str; 5] =
    ["application/octet-stream", "binary/octet-stream", "application/unknown", "video/mp2t", "video/vnd.dlna.mpeg-tts"];

/// `OFFICE_MIME_TYPES`, in the web's order.
const OFFICE_KINDS: [(Office, &str, &str); 9] = [
    (Office::Docx, "docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
    (Office::Xlsx, "xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
    (Office::Pptx, "pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
    (Office::Odt, "odt", "application/vnd.oasis.opendocument.text"),
    (Office::Ods, "ods", "application/vnd.oasis.opendocument.spreadsheet"),
    (Office::Odp, "odp", "application/vnd.oasis.opendocument.presentation"),
    (Office::Rtf, "rtf", "application/rtf"),
    (Office::Epub, "epub", "application/epub+zip"),
    (Office::Ipynb, "ipynb", "application/x-ipynb+json"),
];

/// The generic branch's text extensions (`filePreviewKind`).
const TEXT_EXTENSIONS: &[&str] = &[
    "txt",
    "log",
    "json",
    "jsonc",
    "jsonl",
    "ndjson",
    "yaml",
    "yml",
    "toml",
    "ini",
    "conf",
    "config",
    "env",
    "csv",
    "tsv",
    "xml",
    "css",
    "scss",
    "sass",
    "less",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "ts",
    "tsx",
    "mts",
    "cts",
    "py",
    "pyi",
    "rb",
    "go",
    "rs",
    "swift",
    "kt",
    "kts",
    "java",
    "c",
    "h",
    "cc",
    "cpp",
    "hpp",
    "cs",
    "php",
    "sh",
    "bash",
    "zsh",
    "fish",
    "sql",
    "graphql",
    "gql",
    "vue",
    "svelte",
    "r",
    "lua",
    "ex",
    "exs",
    "erl",
    "hs",
    "clj",
    "dart",
    "diff",
    "patch",
    "lock",
    "properties",
    "gradle",
];

/// The generic branch's well-known text file names.
const TEXT_NAMES: [&str; 10] = [
    "dockerfile",
    "makefile",
    "gemfile",
    "rakefile",
    "license",
    "readme",
    ".gitignore",
    ".gitattributes",
    ".editorconfig",
    ".env",
];

/// How an attachment previews (`filePreviewKind` with rich documents).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Office(Office),
    /// An archive, by the extension its reader expects.
    Archive(&'static str),
    Sqlite,
    Pdf,
    Html,
    Markdown,
    Mermaid,
    Font,
    Image,
    Video,
    Audio,
    Text,
    Unsupported,
}

/// The MIME type without parameters, lowercased.
fn base_mime(mime: &str) -> String {
    mime.split(';').next().unwrap_or_default().trim().to_lowercase()
}

/// A type that leaves the kind to the name.
fn generic(mime: &str) -> bool {
    mime.is_empty() || mime == "text/plain" || GENERIC_MIME_TYPES.contains(&mime)
}

/// The lowercased extension after the last dot, `""` without one.
fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase()).unwrap_or_default()
}

/// `officePreviewKind`.
fn office_kind(name: &str, mime: &str) -> Option<Office> {
    if mime == "text/rtf" {
        return Some(Office::Rtf);
    }
    let name = name.to_lowercase();
    OFFICE_KINDS.iter().find_map(|&(office, ext, office_mime)| {
        let by_name = generic(mime) || (office == Office::Ipynb && mime == "application/json");
        (mime == office_mime || (by_name && name.ends_with(&format!(".{ext}")))).then_some(office)
    })
}

/// `binaryPreviewKind`.
fn binary_kind(name: &str, mime: &str) -> Option<Kind> {
    match mime {
        "application/vnd.sqlite3" | "application/x-sqlite3" | "application/x-sqlite" => return Some(Kind::Sqlite),
        "application/zip" | "application/x-zip-compressed" => return Some(Kind::Archive("zip")),
        "application/java-archive" | "application/x-java-archive" => return Some(Kind::Archive("jar")),
        "application/x-tar" => return Some(Kind::Archive("tar")),
        "application/gzip" | "application/x-gzip" => return Some(Kind::Archive("gz")),
        "application/x-compressed-tar" => return Some(Kind::Archive("tgz")),
        _ if !generic(mime) => return None,
        _ => {}
    }
    match extension(name).as_str() {
        "zip" => Some(Kind::Archive("zip")),
        "jar" => Some(Kind::Archive("jar")),
        "tar" => Some(Kind::Archive("tar")),
        "gz" => Some(Kind::Archive("gz")),
        "tgz" => Some(Kind::Archive("tgz")),
        "sqlite" | "sqlite3" | "db" => Some(Kind::Sqlite),
        _ => None,
    }
}

/// `filePreviewKind`.
fn preview_kind(name: &str, mime: &str) -> Kind {
    let mime = base_mime(mime);
    let mime = mime.as_str();
    let ext = extension(name);
    if let Some(office) = office_kind(name, mime) {
        return Kind::Office(office);
    }
    if let Some(binary) = binary_kind(name, mime) {
        return binary;
    }
    match mime {
        "application/pdf" => return Kind::Pdf,
        "text/html" => return Kind::Html,
        "text/markdown" | "text/x-markdown" => return Kind::Markdown,
        "text/vnd.mermaid" => return Kind::Mermaid,
        _ if mime.starts_with("font/") => return Kind::Font,
        // Linux's MIME database also assigns .otf to OpenDocument formula templates.
        "application/vnd.oasis.opendocument.formula-template" if ext == "otf" => return Kind::Font,
        _ if mime.starts_with("image/") => return Kind::Image,
        _ if mime.starts_with("video/") && !generic(mime) => return Kind::Video,
        _ if mime.starts_with("audio/") => return Kind::Audio,
        _ => {}
    }
    if generic(mime) {
        match ext.as_str() {
            "pdf" => return Kind::Pdf,
            "htm" | "html" => return Kind::Html,
            "md" | "markdown" | "mdown" | "mkd" | "mdx" => return Kind::Markdown,
            "mmd" | "mermaid" => return Kind::Mermaid,
            "ttf" | "otf" | "woff" | "woff2" => return Kind::Font,
            _ => {}
        }
        match logic::file_kind(&ext_path(&ext)) {
            FileKind::Image => return Kind::Image,
            FileKind::Video => return Kind::Video,
            FileKind::Audio => return Kind::Audio,
            _ => {}
        }
        let file_name = logic::file_name(name).to_lowercase();
        let known_name = TEXT_NAMES
            .iter()
            .any(|known| file_name.strip_prefix(known).is_some_and(|rest| rest.is_empty() || rest.starts_with('.')));
        if TEXT_EXTENSIONS.contains(&ext.as_str()) || known_name {
            return Kind::Text;
        }
    }
    let structured = mime.strip_prefix("application/").is_some_and(|sub| {
        matches!(sub, "json" | "xml" | "javascript" | "x-javascript" | "yaml" | "x-yaml" | "toml" | "sql")
            || sub.ends_with("+json")
            || sub.ends_with("+xml")
    });
    if mime.starts_with("text/") || structured { Kind::Text } else { Kind::Unsupported }
}

/// A file name that only has `ext`, to ask [`logic::file_kind`] about it.
fn ext_path(ext: &str) -> String {
    format!("file.{ext}")
}

/// `filePreviewDelimiter`.
fn table_delimiter(name: &str, mime: &str) -> Option<char> {
    match base_mime(mime).as_str() {
        "text/csv" => Some(','),
        "text/tab-separated-values" => Some('\t'),
        mime if !generic(mime) => None,
        _ => logic::table_delimiter(name),
    }
}

/// The extension a viewer reads `kind` from, by the MIME type where that picks one.
fn kind_extension(kind: Kind, delimiter: Option<char>, mime: &str) -> &'static str {
    let mime = base_mime(mime);
    match kind {
        Kind::Office(office) => OFFICE_KINDS.iter().find(|(o, _, _)| *o == office).map_or("docx", |(_, ext, _)| ext),
        Kind::Archive(ext) => ext,
        Kind::Sqlite => "sqlite",
        Kind::Pdf => "pdf",
        Kind::Html => "html",
        Kind::Markdown => "md",
        Kind::Mermaid => "mmd",
        Kind::Font => match mime.as_str() {
            "font/otf" | "application/vnd.oasis.opendocument.formula-template" => "otf",
            "font/woff" => "woff",
            "font/woff2" => "woff2",
            _ => "ttf",
        },
        Kind::Image => match mime.as_str() {
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            "image/svg+xml" => "svg",
            "image/bmp" => "bmp",
            "image/tiff" => "tiff",
            "image/x-icon" | "image/vnd.microsoft.icon" => "ico",
            "image/avif" => "avif",
            "image/heic" => "heic",
            "image/heif" => "heif",
            _ => "png",
        },
        Kind::Video => match mime.as_str() {
            "video/webm" => "webm",
            "video/quicktime" => "mov",
            "video/ogg" => "ogv",
            "video/x-matroska" => "mkv",
            "video/x-msvideo" => "avi",
            _ => "mp4",
        },
        Kind::Audio => match mime.as_str() {
            "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
            "audio/ogg" => "ogg",
            "audio/flac" => "flac",
            "audio/aac" => "aac",
            "audio/mp4" | "audio/x-m4a" => "m4a",
            "audio/webm" => "weba",
            "audio/aiff" | "audio/x-aiff" => "aiff",
            _ => "mp3",
        },
        Kind::Text => match delimiter {
            Some(',') => "csv",
            Some(_) => "tsv",
            None => "txt",
        },
        Kind::Unsupported => "",
    }
}

/// The path an attachment's viewers read its kind from: its name as a file name, with the kind's
/// extension added when the name alone would pick another viewer. `None` has no preview.
pub(super) fn preview_path(name: &str, mime: &str) -> Option<String> {
    let kind = preview_kind(name, mime);
    if kind == Kind::Unsupported {
        return None;
    }
    let delimiter = table_delimiter(name, mime).filter(|_| kind == Kind::Text);
    let safe = name.replace(['/', '\\'], "_");
    let path_kind = logic::file_kind(&safe);
    let plain_text = path_kind == FileKind::Text
        && !logic::is_markdown(&safe)
        && !logic::is_mermaid(&safe)
        && logic::table_delimiter(&safe).is_none();
    let fits = match kind {
        Kind::Office(office) => path_kind == FileKind::Office(office),
        Kind::Archive(ext) => {
            let lower = safe.to_lowercase();
            lower.ends_with(&format!(".{ext}")) || (ext == "gz" && lower.ends_with(".tgz"))
        }
        Kind::Sqlite => path_kind == FileKind::Sqlite,
        Kind::Pdf => path_kind == FileKind::Pdf,
        Kind::Html => path_kind == FileKind::Html,
        Kind::Markdown => path_kind == FileKind::Text && logic::is_markdown(&safe),
        Kind::Mermaid => path_kind == FileKind::Text && logic::is_mermaid(&safe),
        Kind::Font => path_kind == FileKind::Font,
        Kind::Image => path_kind == FileKind::Image,
        Kind::Video => path_kind == FileKind::Video,
        Kind::Audio => path_kind == FileKind::Audio,
        Kind::Text => match delimiter {
            Some(delimiter) => path_kind == FileKind::Text && logic::table_delimiter(&safe) == Some(delimiter),
            None => plain_text,
        },
        Kind::Unsupported => false,
    };
    Some(if fits { safe } else { format!("{safe}.{}", kind_extension(kind, delimiter, mime)) })
}

/// Whether an attachment cut at 1 MB reads its whole text (`streamsText`): plain text and
/// tables, not Markdown, Mermaid or HTML, which keep the first megabyte with a notice.
pub(super) fn streams_text(path: &str) -> bool {
    logic::file_kind(path) == FileKind::Text && !logic::is_markdown(path) && !logic::is_mermaid(path)
}

/// `decodeFilePreviewText`: the first megabyte as UTF-8 without NULs, and whether it was cut.
pub(super) fn decode_preview_text(bytes: &[u8]) -> Result<(String, bool), String> {
    if bytes[..bytes.len().min(TEXT_PREVIEW_MAX_BYTES)].contains(&0) {
        return Err("This file contains binary data and cannot be shown as text.".into());
    }
    super::archive::preview_text(bytes)
        .ok_or_else(|| "This file is not UTF-8 text. Open it in another app to view its contents.".into())
}

/// What frames an attachment's preview: its origin crumb, and in the draft dialog the full-height
/// subheader with Remove from draft and Close.
struct Look {
    origin: &'static str,
    draft: Option<SharedString>,
}

impl Workspace {
    /// The workspace mutation a file's views follow; an attachment never changes.
    pub(super) fn key_mutation(&self, key: &FileKey) -> (Option<String>, Option<String>) {
        if key.in_workspace() { self.files.mutation.clone() } else { (None, None) }
    }

    /// The environment that serves the file: a draft's is where it uploaded.
    pub(super) fn file_environment(&self, key: &FileKey) -> Option<SharedString> {
        match &key.origin {
            Origin::Draft { local_id } => Some(self.draft_attachment(local_id)?.uploaded()?.ok()?.0.clone()),
            _ => Some(key.checkout.env.clone()),
        }
    }

    fn draft_attachment(&self, local_id: &str) -> Option<&crate::app::attachments::Attachment> {
        self.attachments.iter().find(|attachment| attachment.local_id() == local_id)
    }

    /// A draft attachment's bytes.
    pub(super) fn draft_bytes(&self, key: &FileKey) -> Option<bytes::Bytes> {
        let Origin::Draft { local_id } = &key.origin else { return None };
        Some(self.draft_attachment(local_id)?.bytes().clone())
    }

    pub(super) fn draft_name(&self, key: &FileKey) -> Option<String> {
        let Origin::Draft { local_id } = &key.origin else { return None };
        Some(self.draft_attachment(local_id)?.name().to_owned())
    }

    /// A draft image, decoded as its type (the attachment's own image, else by `path`'s extension).
    pub(super) fn draft_image(&self, local_id: &str, path: &str) -> Option<Arc<Image>> {
        let attachment = self.draft_attachment(local_id)?;
        attachment.image().or_else(|| {
            let format = super::archive::image_format(path)?;
            Some(Arc::new(Image::from_bytes(format, attachment.bytes().to_vec())))
        })
    }

    /// The size its subheader and notice name.
    pub(super) fn attachment_size(&self, key: &FileKey) -> u64 {
        match &key.origin {
            Origin::Attachment { size, .. } => *size,
            Origin::Draft { .. } => self.draft_bytes(key).map_or(0, |bytes| bytes.len() as u64),
            Origin::Workspace => 0,
        }
    }

    /// The `attachment` asset an attachment reads, inline for viewers or as a download; a draft
    /// has one once uploaded.
    pub(super) fn attachment_resource(&self, key: &FileKey, download: bool) -> Option<Value> {
        let (id, name, mime) = match &key.origin {
            Origin::Workspace => return None,
            Origin::Attachment { id, name, mime, .. } => (id.to_string(), name.to_string(), mime.to_string()),
            Origin::Draft { local_id } => {
                let attachment = self.draft_attachment(local_id)?;
                let (_, id) = attachment.uploaded()?.ok()?;
                (id.to_owned(), attachment.name().to_owned(), attachment.mime().to_owned())
            }
        };
        let disposition = if download { "attachment" } else { "inline" };
        Some(json!({
            "_tag": "attachment",
            "attachmentId": id,
            "fileName": name,
            "mimeType": mime,
            "disposition": disposition,
        }))
    }

    /// An attachment tab's file, on the panel's environment.
    fn attachment_key(&self, file: &AttachmentFile) -> Option<FileKey> {
        let env = self.target.as_ref()?.environment().clone();
        let path = preview_path(&file.name, &file.mime).unwrap_or_else(|| file.name.clone());
        let origin = Origin::Attachment {
            id: file.id.clone().into(),
            name: file.name.clone().into(),
            mime: file.mime.clone().into(),
            size: file.size,
        };
        Some(FileKey { checkout: Checkout { env, cwd: String::new() }, path, origin })
    }

    /// The draft attachment `local_id` as a previewed file.
    pub(in crate::app) fn draft_preview_key(&self, local_id: &str) -> Option<FileKey> {
        let attachment = self.draft_attachment(local_id)?;
        let path = preview_path(attachment.name(), attachment.mime()).unwrap_or_else(|| attachment.name().to_owned());
        let checkout = Checkout { env: SharedString::default(), cwd: String::new() };
        Some(FileKey { checkout, path, origin: Origin::Draft { local_id: local_id.to_owned().into() } })
    }

    /// An attachment tab (`AttachmentFilePreview` in `FilePreviewPanel`).
    pub(in crate::app) fn render_attachment_surface(
        &mut self,
        file: &AttachmentFile,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(key) = self.attachment_key(file) else { return div().into_any_element() };
        self.render_attachment_preview(&key, &Look { origin: "Attachment", draft: None }, width, window, cx)
    }

    /// A draft attachment's preview, as `ComposerFilePreviewDialog` shows it.
    pub(in crate::app) fn render_draft_preview(
        &mut self,
        key: &FileKey,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Origin::Draft { local_id } = &key.origin else { return div().into_any_element() };
        let look = Look { origin: "Draft", draft: Some(local_id.clone()) };
        self.render_attachment_preview(key, &look, width, window, cx)
    }

    /// The draft dialog closed: its views and reads go, as the web's unmount drops them.
    pub(in crate::app) fn close_draft_preview(&mut self, key: &FileKey, cx: &mut Context<Self>) {
        self.forget_preview(key);
        cx.notify();
    }

    /// The name, mime type and size of an attachment's preview.
    fn attachment_details(&self, key: &FileKey) -> Option<(String, String, u64)> {
        match &key.origin {
            Origin::Workspace => None,
            Origin::Attachment { name, mime, size, .. } => Some((name.to_string(), mime.to_string(), *size)),
            Origin::Draft { local_id } => {
                let attachment = self.draft_attachment(local_id)?;
                Some((attachment.name().to_owned(), attachment.mime().to_owned(), attachment.bytes().len() as u64))
            }
        }
    }

    fn render_attachment_preview(
        &mut self,
        key: &FileKey,
        look: &Look,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *crate::theme::tokens(cx);
        let Some((name, mime, size)) = self.attachment_details(key) else { return div().into_any_element() };
        let supported = preview_path(&name, &mime).is_some();
        let path = key.path.clone();
        let file_kind = logic::file_kind(&path);
        let mode = supported.then(|| Rendered::of(&path)).flatten();
        let rendered = mode.is_some_and(|mode| self.files.shows_rendered(key, mode));
        // A draft's media and rendered page play from its upload's URL.
        let waits_for_upload = matches!(key.origin, Origin::Draft { .. })
            && (matches!(file_kind, FileKind::Video | FileKind::Audio) || (file_kind == FileKind::Html && rendered))
            && self.attachment_resource(key, false).is_none();
        let upload_failure = match &key.origin {
            Origin::Draft { local_id } => self
                .draft_attachment(local_id)
                .and_then(crate::app::attachments::Attachment::uploaded)
                .and_then(Result::err)
                .map(str::to_owned),
            _ => None,
        };
        let body = if !supported {
            let extension = name.rsplit('.').next().filter(|e| !e.is_empty()).unwrap_or("this format");
            chrome::no_preview(format!("Save it to open in an app that supports {extension} files."), &tokens)
        } else if waits_for_upload {
            match upload_failure {
                Some(reason) => chrome::failure(reason, None, &tokens),
                None => chrome::loading(&tokens),
            }
        } else {
            self.sync_preview(key, window, cx);
            self.render_file_content(key, &tokens, width, window, cx)
        };
        let status = self.attachment_text_status(key);
        let text_kind = supported && matches!(file_kind, FileKind::Text | FileKind::Html);
        let html_rendered = file_kind == FileKind::Html && rendered;
        let source_shown = mode.is_none() || !rendered;
        let shows_raw_text = text_kind && status.loaded && source_shown;
        let copyable = text_kind && status.loaded && !html_rendered;
        let saving = self.files.saving.contains(key);
        let savable = self.file_fetch(key, None, true).is_some();
        let identity = match &key.origin {
            Origin::Attachment { id, .. } => id.as_ref(),
            Origin::Draft { local_id } => local_id.as_ref(),
            Origin::Workspace => key.path.as_str(),
        };
        let copy_id: SharedString = format!("attachment-copy:{identity}").into();
        let copied = self.copy_feedback.id.as_ref() == Some(&copy_id);
        let wrap = status.wrap.unwrap_or_else(|| self.client_settings.values.bool("wordWrap"));
        let inline = look.draft.is_none();
        let button = |id: &'static str, icon: IconName, label: SharedString, enabled: bool, cx: &App| {
            chrome::action_button(id, icon, label, enabled, &tokens, cx).size(px(28.)).rounded(px(8.))
        };
        let crumbs = h_flex()
            .flex_1()
            .min_w_0()
            .text_xs()
            .child(div().flex_none().px(px(2.)).text_color(tokens.muted_foreground).child(look.origin))
            .child(
                Icon::new(IconName::ChevronRight)
                    .size(px(14.))
                    .mx(px(4.))
                    .flex_none()
                    .text_color(tokens.muted_foreground.opacity(0.6)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .px(px(2.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tokens.foreground)
                    .child(name),
            )
            .when(size > 0, |el| {
                el.child(
                    div()
                        .flex_none()
                        .ml(px(8.))
                        .text_color(tokens.muted_foreground)
                        .child(crate::app::attachments::format_size(size)),
                )
            });
        let subheader = h_flex()
            .flex_none()
            .gap(px(8.))
            .px(px(12.))
            .bg(tokens.background)
            .map(|el| {
                if inline {
                    el.h(px(28.)).mb(px(12.))
                } else {
                    el.h(px(40.)).border_b_1().border_color(tokens.border.opacity(0.6))
                }
            })
            .child(crumbs)
            .when_some(mode, |el, mode| {
                let icon = match (rendered, mode) {
                    (true, _) => IconName::CodeXml,
                    (false, Rendered::Table) => IconName::Table2,
                    (false, _) => IconName::Eye,
                };
                let key = key.clone();
                el.child(
                    button("attachment-rendered", icon, mode.toggle_label(rendered).into(), true, cx)
                        .selected(rendered)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.files.set_rendered(&key, mode, !rendered);
                            cx.notify();
                        })),
                )
            })
            .when(shows_raw_text, |el| {
                let key = key.clone();
                let label = if wrap { "Disable word wrap" } else { "Enable word wrap" };
                el.child(button("attachment-wrap", IconName::TextWrap, label.into(), true, cx).selected(wrap).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.set_preview_wrap(&key, !wrap);
                        cx.notify();
                    }),
                ))
            })
            .when(copyable, |el| {
                let key = key.clone();
                let label = match (copied, status.cut) {
                    (true, _) => "Copied",
                    (false, true) => "Copy preview",
                    (false, false) => "Copy contents",
                };
                let icon = if copied { IconName::Check } else { IconName::Copy };
                el.child(button("attachment-copy", icon, label.into(), true, cx).on_click(cx.listener(
                    move |this, _, window, cx| {
                        match this.attachment_text(&key, cx) {
                            Some(text) => {
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                                this.copied(copy_id.clone(), COPIED_FOR, cx);
                            }
                            None => window.push_notification(
                                Notification::error("Wait for the file to finish loading before copying.")
                                    .title("Could not copy file"),
                                cx,
                            ),
                        }
                    },
                )))
            })
            .when(savable, |el| {
                let key = key.clone();
                let label = if saving { "Preparing file…" } else { "Save file" };
                el.child(button("attachment-save", IconName::Download, label.into(), !saving, cx).on_click(
                    cx.listener(move |this, _, window, cx| {
                        if !this.files.saving.contains(&key) {
                            this.save_asset_file(&key, "Could not save file".into(), None, window, cx);
                        }
                    }),
                ))
            })
            .when_some(look.draft.clone(), |el, local_id| {
                let (remove_key, close_key) = (key.clone(), key.clone());
                el.child(button("attachment-remove", IconName::Trash, "Remove from draft".into(), true, cx).on_click(
                    cx.listener(move |this, _, window, cx| {
                        window.close_dialog(cx);
                        this.close_draft_preview(&remove_key, cx);
                        this.request_remove_attachment(&local_id, window, cx);
                    }),
                ))
                .child(button("attachment-close", IconName::X, "Close".into(), true, cx).on_click(
                    cx.listener(move |this, _, window, cx| {
                        window.close_dialog(cx);
                        this.close_draft_preview(&close_key, cx);
                    }),
                ))
            });
        let notice = status.cut.then(|| {
            let of = if size > 0 {
                format!(" of a {} byte file", logic::grouped(usize::try_from(size).unwrap_or(usize::MAX)))
            } else {
                String::new()
            };
            chrome::notice(format!("Preview limited to the first 1 MB{of}. Save the file to read it in full."), &tokens)
        });
        v_flex()
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .bg(tokens.background)
            .child(subheader)
            .children(notice)
            .child(div().flex_1().min_h_0().flex().flex_col().overflow_hidden().child(body))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_follow_the_web_by_type_then_name() {
        for (name, mime, path) in [
            ("report.pdf", "application/pdf", Some("report.pdf")),
            ("report", "application/pdf", Some("report.pdf")),
            ("notes", "text/markdown", Some("notes.md")),
            ("notes.md", "text/plain", Some("notes.md")),
            ("data.txt", "text/csv", Some("data.txt.csv")),
            ("data.csv", "application/octet-stream", Some("data.csv")),
            ("sheet", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", Some("sheet.xlsx")),
            ("story.rtf", "", Some("story.rtf")),
            ("nb.ipynb", "application/json", Some("nb.ipynb")),
            ("bundle", "application/gzip", Some("bundle.gz")),
            ("bundle.tgz", "application/gzip", Some("bundle.tgz")),
            ("app.db", "", Some("app.db")),
            ("clip", "video/quicktime", Some("clip.mov")),
            ("photo", "image/jpeg", Some("photo.jpg")),
            ("shot.png", "image/png", Some("shot.png")),
            ("song.mp3", "application/octet-stream", Some("song.mp3")),
            ("font", "font/woff2", Some("font.woff2")),
            ("page.htm", "text/plain", Some("page.htm")),
            ("diagram", "text/vnd.mermaid", Some("diagram.mmd")),
            ("config", "application/json", Some("config")),
            ("data.json", "application/json", Some("data.json")),
            ("main.ts", "video/mp2t", Some("main.ts")),
            ("Makefile", "", Some("Makefile")),
            ("a/b.txt", "text/plain", Some("a_b.txt")),
            ("x.md", "text/x-python", Some("x.md.txt")),
            ("setup.exe", "application/x-msdownload", None),
            ("blob.bin", "application/octet-stream", None),
            ("noext", "", None),
        ] {
            assert_eq!(preview_path(name, mime).as_deref(), path, "{name} ({mime})");
        }
    }

    #[test]
    fn text_streams_only_for_plain_text_and_tables() {
        assert!(streams_text("a.txt") && streams_text("a.csv") && streams_text("a.rs"));
        assert!(!streams_text("a.md") && !streams_text("a.mmd") && !streams_text("a.html"));
    }

    #[test]
    fn text_previews_need_utf8_without_nuls() {
        assert_eq!(decode_preview_text(b"a\tb"), Ok(("a\tb".into(), false)));
        assert!(decode_preview_text(b"PK\x03\x04\0\0").unwrap_err().contains("binary data"));
        assert!(decode_preview_text(&[0xff, 0xfe]).unwrap_err().contains("not UTF-8"));
        let mut big = "é".repeat(TEXT_HEAD_BYTES / 2).into_bytes();
        big.insert(0, b'x');
        let (text, truncated) = decode_preview_text(&big).unwrap();
        assert!(truncated && text.len() < TEXT_PREVIEW_MAX_BYTES, "a cut character is dropped");
    }

    #[test]
    fn attachment_tabs_are_the_same_attachment_by_id() {
        let file =
            |id: &str, name: &str| AttachmentFile { id: id.into(), name: name.into(), mime: String::new(), size: 1 };
        assert_eq!(file("a", "x.txt"), file("a", "renamed.txt"));
        assert_ne!(file("a", "x.txt"), file("b", "x.txt"));
    }
}
