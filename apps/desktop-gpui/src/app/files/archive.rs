//! Archive listings (`BinaryViewer.tsx`, `binaryPreview.worker.ts`, `boundedZip.ts`): zip, jar,
//! tar and gzip files list their entries by folder, and a text or image entry opens inside.

use std::collections::HashMap;
use std::io::Read as _;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, Image, ImageFormat, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img,
    px, uniform_list,
};

use super::chrome;
use super::logic::{self, FileKind};
use crate::app::Workspace;
use crate::app::attachments::format_size;
use crate::theme::Tokens;

/// `ARCHIVE_MAX_BYTES`, `ARCHIVE_ENTRY_MAX_BYTES`, `ARCHIVE_MAX_ENTRIES`, `ARCHIVE_MAX_RATIO`.
pub(super) const MAX_BYTES: u64 = 50 * 1024 * 1024;
const ENTRY_MAX_BYTES: u64 = 10 * 1024 * 1024;
const MAX_ENTRIES: usize = 2048;
const MAX_RATIO: u64 = 200;
const XML_MAX_BYTES: u64 = 8 * 1024 * 1024;
const TEXT_MAX_BYTES: usize = 1024 * 1024;
const ROW_HEIGHT: f32 = 33.;
const EXPANDED_TOO_LARGE: &str = "The expanded archive exceeds the preview size or compression ratio limit.";

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ArchiveEntry {
    pub path: String,
    pub size: u64,
    /// `toLocaleDateString()` of the entry's time.
    pub modified: Option<String>,
    pub directory: bool,
}

/// One unpacked zip part.
pub(super) struct ZipPart {
    pub name: String,
    pub bytes: Vec<u8>,
    pub modified: Option<String>,
    pub directory: bool,
}

fn date_label(year: i32, month: u32, day: u32) -> String {
    format!("{month}/{day}/{year}")
}

/// `boundedZip`: every part, refusing archives that expand past the preview's limits.
pub(super) fn read_zip(bytes: &[u8], entry_limit: u64) -> Result<Vec<ZipPart>, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Preview limited to files under 50 MB.".into());
    }
    let mut zip =
        zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|_| "This archive is damaged.".to_owned())?;
    if zip.len() > MAX_ENTRIES {
        return Err("This archive contains more than 2,048 entries.".into());
    }
    let ratio_limit = (bytes.len() as u64).max(1) * MAX_RATIO;
    let mut total = 0u64;
    let mut parts = Vec::with_capacity(zip.len());
    for index in 0..zip.len() {
        let mut file = zip.by_index(index).map_err(|_| "This archive is damaged.".to_owned())?;
        let name = file.name().map_err(|_| "This archive is damaged.".to_owned())?.into_owned();
        if name.len() > 1024 {
            return Err("An archive path is too long to preview.".into());
        }
        let limit = if name.ends_with(".xml") { entry_limit.min(XML_MAX_BYTES) } else { entry_limit };
        let mut content = Vec::new();
        (&mut file).take(limit + 1).read_to_end(&mut content).map_err(|_| "This archive is damaged.".to_owned())?;
        total += content.len() as u64;
        if content.len() as u64 > limit || total > MAX_BYTES || total > ratio_limit {
            return Err(EXPANDED_TOO_LARGE.into());
        }
        let modified = file.last_modified().map(|t| date_label(t.year().into(), t.month().into(), t.day().into()));
        parts.push(ZipPart { directory: file.is_dir(), name, bytes: content, modified });
    }
    Ok(parts)
}

fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let limit = MAX_BYTES.min((bytes.len() as u64).max(1) * MAX_RATIO);
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|_| "This archive is damaged.".to_owned())?;
    if out.len() as u64 > limit {
        return Err(EXPANDED_TOO_LARGE.into());
    }
    Ok(out)
}

fn epoch_date(seconds: i64) -> Option<String> {
    use chrono::Datelike as _;
    let date = chrono::DateTime::from_timestamp(seconds, 0)?.with_timezone(&chrono::Local).date_naive();
    Some(date_label(date.year(), date.month(), date.day()))
}

/// `tarEntries`: ustar, GNU long names and pax `path`/`size`/`mtime` records.
fn tar_entries(bytes: &[u8], files: &mut HashMap<String, Arc<Vec<u8>>>) -> Result<Vec<ArchiveEntry>, String> {
    let damaged = || "An archive entry is too large or damaged.".to_owned();
    let text = |offset: usize, length: usize| -> String {
        let slice = &bytes[offset..(offset + length).min(bytes.len())];
        let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
        String::from_utf8_lossy(&slice[..end]).into_owned()
    };
    let number = |offset: usize, length: usize| -> Result<u64, String> {
        let value = text(offset, length);
        let value = value.trim();
        if !value.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return Err("This TAR uses an unsupported numeric format.".into());
        }
        Ok(if value.is_empty() { 0 } else { u64::from_str_radix(value, 8).map_err(|_| damaged())? })
    };
    let mut entries = Vec::new();
    let mut long_path: Option<String> = None;
    let mut global: HashMap<String, String> = HashMap::new();
    let mut local: HashMap<String, String> = HashMap::new();
    let mut offset = 0usize;
    while offset + 512 <= bytes.len() {
        let header = &bytes[offset..offset + 512];
        if header.iter().all(|&b| b == 0) {
            break;
        }
        if entries.len() >= MAX_ENTRIES {
            return Err("This archive contains more than 2,048 entries.".into());
        }
        let checksum: u64 =
            header.iter().enumerate().map(|(i, &b)| if (148..156).contains(&i) { 32 } else { u64::from(b) }).sum();
        if checksum != number(offset + 148, 8)? {
            return Err("The TAR header is damaged.".into());
        }
        let kind = text(offset + 156, 1);
        let metadata = matches!(kind.as_str(), "L" | "x" | "g");
        let mut attributes = HashMap::new();
        if !metadata {
            attributes.extend(global.clone());
            attributes.extend(local.clone());
        }
        let size = match attributes.get("size") {
            Some(size) => size.parse::<u64>().map_err(|_| damaged())?,
            None => number(offset + 124, 12)?,
        };
        let prefix = if text(offset + 257, 6) == "ustar" && text(offset + 263, 2) == "00" {
            text(offset + 345, 155)
        } else {
            String::new()
        };
        let path = attributes.get("path").cloned().or(long_path.clone()).unwrap_or_else(|| {
            [prefix, text(offset, 100)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
        });
        if !metadata {
            long_path = None;
            local.clear();
        }
        let start = offset + 512;
        if path.len() > 1024 || size > MAX_BYTES || start as u64 + size > bytes.len() as u64 {
            return Err(damaged());
        }
        let content = &bytes[start..start + size as usize];
        match kind.as_str() {
            "L" => long_path = Some(String::from_utf8_lossy(content).split('\0').next().unwrap_or("").to_owned()),
            "x" | "g" => {
                if size > 65536 {
                    return Err("Archive metadata is too large.".into());
                }
                for (key, value) in pax_records(content).ok_or("The TAR metadata is damaged.")? {
                    if matches!(key.as_str(), "path" | "size" | "mtime") {
                        if kind == "x" {
                            local.insert(key, value);
                        } else if value.is_empty() {
                            global.remove(&key);
                        } else {
                            global.insert(key, value);
                        }
                    }
                }
            }
            "" | "0" | "5" => {
                let seconds = match attributes.get("mtime") {
                    Some(mtime) => mtime.parse::<f64>().map_err(|_| "The TAR metadata is damaged.".to_owned())? as i64,
                    None => number(offset + 136, 12)? as i64,
                };
                let directory = kind == "5";
                if !directory {
                    files.insert(path.clone(), Arc::new(content.to_vec()));
                }
                entries.push(ArchiveEntry { path, size, modified: epoch_date(seconds), directory });
            }
            _ => {}
        }
        offset = start + size.div_ceil(512) as usize * 512;
    }
    if entries.is_empty() {
        return Err("This archive has no readable entries.".into());
    }
    Ok(entries)
}

/// `length key=value\n` records.
fn pax_records(content: &[u8]) -> Option<Vec<(String, String)>> {
    let mut records = Vec::new();
    let mut cursor = 0;
    while cursor < content.len() {
        let space = cursor + content[cursor..].iter().position(|&b| b == b' ')?;
        let length: usize = std::str::from_utf8(&content[cursor..space]).ok()?.parse().ok()?;
        let end = cursor + length;
        if end > content.len() || end <= space + 1 || content[end - 1] != b'\n' {
            return None;
        }
        let record = String::from_utf8_lossy(&content[space + 1..end - 1]).into_owned();
        let (key, value) = record.split_once('=').filter(|(key, _)| !key.is_empty())?;
        records.push((key.to_owned(), value.to_owned()));
        cursor = end;
    }
    Some(records)
}

/// The parsed archive: its entries and the bytes of its files.
pub(super) struct Listing {
    pub entries: Vec<ArchiveEntry>,
    pub files: HashMap<String, Arc<Vec<u8>>>,
}

pub(super) fn open_archive(name: &str, bytes: &[u8]) -> Result<Listing, String> {
    let lower = name.to_lowercase();
    let mut files = HashMap::new();
    let magic = bytes.get(..4).map(|m| u32::from_le_bytes([m[0], m[1], m[2], m[3]]));
    let entries =
        if lower.ends_with(".zip") || lower.ends_with(".jar") || matches!(magic, Some(0x04034b50 | 0x06054b50)) {
            read_zip(bytes, MAX_BYTES)?
                .into_iter()
                .map(|part| {
                    let size = part.bytes.len() as u64;
                    if !part.directory {
                        files.insert(part.name.clone(), Arc::new(part.bytes));
                    }
                    ArchiveEntry { path: part.name, size, modified: part.modified, directory: part.directory }
                })
                .collect()
        } else if lower.ends_with(".gz") || lower.ends_with(".tgz") || bytes.first() == Some(&0x1f) {
            let expanded = gunzip(bytes)?;
            let inner = logic::file_name(name);
            let inner = inner.strip_suffix(".gz").or(inner.strip_suffix(".GZ")).unwrap_or("");
            if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") || inner.to_lowercase().ends_with(".tar") {
                tar_entries(&expanded, &mut files)?
            } else {
                let path = if inner.is_empty() { "contents".to_owned() } else { inner.to_owned() };
                let size = expanded.len() as u64;
                files.insert(path.clone(), Arc::new(expanded));
                vec![ArchiveEntry { path, size, modified: None, directory: false }]
            }
        } else {
            tar_entries(bytes, &mut files)?
        };
    Ok(Listing { entries, files })
}

/// One row of a folder: a file, or a folder with the size of everything below it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Child {
    pub path: String,
    pub directory: bool,
    pub size: u64,
    pub modified: Option<String>,
}

/// The children of `folder` (`""` or `a/b/`), folders first, then by path.
pub(super) fn folder_children(entries: &[ArchiveEntry], folder: &str) -> Vec<Child> {
    let mut children: Vec<Child> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for item in entries {
        let Some(rest) = item.path.strip_prefix(folder) else { continue };
        let rest = rest.trim_end_matches('/');
        if rest.is_empty() {
            continue;
        }
        let (path, directory) = match rest.find('/') {
            Some(slash) => (format!("{folder}{}", &rest[..slash]), true),
            None => (item.path.trim_end_matches('/').to_owned(), item.directory),
        };
        let key = format!("{path}{}", if directory { "/" } else { "" });
        match index.get(&key) {
            Some(&at) => children[at].size += item.size,
            None => {
                index.insert(key, children.len());
                children.push(Child { path, directory, size: item.size, modified: item.modified.clone() });
            }
        }
    }
    children.sort_by(|a, b| b.directory.cmp(&a.directory).then_with(|| a.path.cmp(&b.path)));
    children
}

/// An entry opened inside the archive (`ArchiveFile`).
enum Opened {
    Text { editor: Entity<EditorState>, truncated: bool },
    Image(Arc<Image>),
    Failed(&'static str),
}

const ONLY_TEXT_AND_IMAGES: &str = "Only UTF-8 text and images can be opened inside an archive.";

pub(super) struct Archive {
    listing: Listing,
    size: u64,
    folder: String,
    children: Rc<Vec<Child>>,
    selected: Option<(String, Opened)>,
}

impl Archive {
    pub(super) fn new(listing: Listing, size: u64) -> Self {
        let children = Rc::new(folder_children(&listing.entries, ""));
        Self { listing, size, folder: String::new(), children, selected: None }
    }

    fn set_folder(&mut self, folder: String) {
        self.children = Rc::new(folder_children(&self.listing.entries, &folder));
        self.folder = folder;
    }
}

fn image_format(path: &str) -> Option<ImageFormat> {
    match logic::file_name(path).rsplit_once('.')?.1.to_lowercase().as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        "bmp" => Some(ImageFormat::Bmp),
        "tif" | "tiff" => Some(ImageFormat::Tiff),
        "svg" => Some(ImageFormat::Svg),
        "ico" => Some(ImageFormat::Ico),
        _ => None,
    }
}

/// `decodeFilePreviewText`: the first 1 MB as UTF-8 without NULs.
pub(super) fn preview_text(bytes: &[u8]) -> Option<(String, bool)> {
    let truncated = bytes.len() > TEXT_MAX_BYTES;
    let head = &bytes[..bytes.len().min(TEXT_MAX_BYTES)];
    if head.contains(&0) {
        return None;
    }
    let text = match std::str::from_utf8(head) {
        Ok(text) => text,
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&head[..error.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };
    Some((text.to_owned(), truncated))
}

impl Workspace {
    fn archive_mut(&mut self, key: &(String, String)) -> Option<&mut Archive> {
        match self.files.assets.viewer_mut(key)? {
            super::asset::Viewer::Archive(archive) => Some(archive),
            _ => None,
        }
    }

    fn open_archive_entry(
        &mut self,
        key: &(String, String),
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(archive) = self.archive_mut(key) else { return };
        let bytes = archive.listing.files.get(&path).cloned();
        let opened = match bytes {
            None => Opened::Failed("This archive entry is unavailable."),
            Some(bytes) if bytes.len() as u64 > ENTRY_MAX_BYTES => {
                Opened::Failed("Entry previews are limited to 10 MB.")
            }
            Some(bytes) => match logic::file_kind(&path) {
                FileKind::Image => match image_format(&path) {
                    Some(format) => Opened::Image(Arc::new(Image::from_bytes(format, bytes.to_vec()))),
                    None => Opened::Failed(ONLY_TEXT_AND_IMAGES),
                },
                FileKind::Text | FileKind::Html => match preview_text(&bytes) {
                    Some((text, truncated)) => {
                        let language = logic::language_for_path(&path);
                        let editor = cx.new(|cx| EditorState::new(window, cx).language(language).default_value(text));
                        Opened::Text { editor, truncated }
                    }
                    None => Opened::Failed(ONLY_TEXT_AND_IMAGES),
                },
                _ => Opened::Failed(ONLY_TEXT_AND_IMAGES),
            },
        };
        if let Some(archive) = self.archive_mut(key) {
            archive.selected = Some((path, opened));
        }
        cx.notify();
    }

    pub(super) fn render_archive_listing(
        &mut self,
        key: &(String, String),
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(archive) = self.archive_mut(key) else { return div().into_any_element() };
        let muted = tokens.muted_foreground;
        let selected = archive.selected.as_ref().map(|(path, _)| path.clone());
        let can_go_back = selected.is_some() || !archive.folder.is_empty();
        let title = selected.clone().unwrap_or_else(|| {
            if archive.folder.is_empty() { "Archive contents".to_owned() } else { archive.folder.clone() }
        });
        let summary =
            format!("{} entries · {}", logic::grouped(archive.listing.entries.len()), format_size(archive.size));
        let back_key = key.clone();
        let header = h_flex()
            .flex_none()
            .h(px(40.))
            .gap(px(8.))
            .px(px(12.))
            .border_b_1()
            .border_color(tokens.border.opacity(0.6))
            .text_xs()
            .child(
                chrome::action(
                    "archive-back",
                    IconName::ChevronLeft,
                    if selected.is_some() { "Back to archive" } else { "Parent folder" },
                    tokens,
                )
                .when(!can_go_back, |el| el.opacity(0.5).cursor_default())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let Some(archive) = this.archive_mut(&back_key) else { return };
                    if archive.selected.take().is_none() {
                        let trimmed = archive.folder.trim_end_matches('/');
                        let parent = trimmed.rfind('/').map_or(String::new(), |at| trimmed[..=at].to_owned());
                        archive.set_folder(parent);
                    }
                    cx.notify();
                })),
            )
            .child(div().min_w_0().flex_1().truncate().child(title))
            .child(div().flex_none().text_color(muted).child(summary));
        let body = match &archive.selected {
            Some((_, Opened::Image(image))) => div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .p(px(16.))
                .child(img(image.clone()).max_w_full().max_h_full().object_fit(ObjectFit::Contain))
                .into_any_element(),
            Some((_, Opened::Failed(message))) => chrome::failure(*message, None, tokens),
            Some((_, Opened::Text { editor, truncated })) => v_flex()
                .flex_1()
                .min_h_0()
                .when(*truncated, |el| el.child(chrome::notice("Preview limited to the first 1 MB.", tokens)))
                .child(
                    Editor::new(editor)
                        .appearance(false)
                        .bordered(false)
                        .readonly(true)
                        .h_full()
                        .font_family(tokens.fonts.mono),
                )
                .into_any_element(),
            None if archive.children.is_empty() => {
                div().p(px(24.)).text_xs().text_color(muted).child("This folder is empty.").into_any_element()
            }
            None => {
                let children = archive.children.clone();
                let folder = archive.folder.clone();
                let list_key = key.clone();
                let column_header = h_flex()
                    .flex_none()
                    .text_xs()
                    .text_color(muted)
                    .child(div().flex_1().px(px(12.)).py(px(8.)).child("Name"))
                    .child(div().w(px(80.)).px(px(12.)).py(px(8.)).text_right().child("Size"))
                    .child(div().w(px(112.)).px(px(12.)).py(px(8.)).text_right().child("Modified"));
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(column_header)
                    .child(
                        uniform_list(
                            ElementId::Name(format!("archive:{}", key.1).into()),
                            children.len(),
                            cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                                let tokens = *crate::theme::tokens(cx);
                                range
                                    .map(|index| archive_row(&list_key, &children[index], &folder, &tokens, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .flex_1(),
                    )
                    .into_any_element()
            }
        };
        v_flex().flex_1().min_h_0().overflow_hidden().child(header).child(body).into_any_element()
    }
}

fn archive_row(
    key: &(String, String),
    child: &Child,
    folder: &str,
    tokens: &Tokens,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let muted = tokens.muted_foreground;
    let (click_key, click) = (key.clone(), child.clone());
    h_flex()
        .id(ElementId::Name(format!("archive-row:{}:{}", child.path, child.directory).into()))
        .h(px(ROW_HEIGHT))
        .w_full()
        .text_xs()
        .border_b_1()
        .border_color(tokens.border.opacity(0.6))
        .cursor_pointer()
        .hover(|s| s.bg(tokens.muted.opacity(0.4)))
        .on_click(cx.listener(move |this, _, window, cx| {
            if click.directory {
                if let Some(archive) = this.archive_mut(&click_key) {
                    archive.set_folder(format!("{}/", click.path));
                }
                cx.notify();
            } else {
                this.open_archive_entry(&click_key, click.path.clone(), window, cx);
            }
        }))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap(px(8.))
                .px(px(12.))
                .child(
                    Icon::new(if child.directory { IconName::Folder } else { IconName::File })
                        .size(px(16.))
                        .flex_none()
                        .text_color(muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(child.path[folder.len().min(child.path.len())..].to_owned()),
                ),
        )
        .child(div().w(px(80.)).flex_none().px(px(12.)).text_right().text_color(muted).child(format_size(child.size)))
        .child(
            div()
                .w(px(112.))
                .flex_none()
                .px(px(12.))
                .text_right()
                .text_color(muted)
                .child(child.modified.clone().unwrap_or_else(|| "-".into())),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_header(name: &str, kind: u8, size: usize) -> Vec<u8> {
        let mut header = vec![0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        let octal = |value: usize, width: usize| format!("{value:0>w$o}\0", w = width - 1);
        header[124..136].copy_from_slice(octal(size, 12).as_bytes());
        header[136..148].copy_from_slice(octal(0, 12).as_bytes());
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        header[148..156].copy_from_slice(b"        ");
        let sum: usize = header.iter().map(|&b| b as usize).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        header
    }

    fn tar(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, content) in files {
            let kind = if name.ends_with('/') { b'5' } else { b'0' };
            out.extend(tar_header(name, kind, content.len()));
            out.extend_from_slice(content);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out.extend([0u8; 1024]);
        out
    }

    #[test]
    fn tar_and_gzip_list_their_entries() {
        let bytes = tar(&[("src/", b""), ("src/a.rs", b"fn a() {}"), ("README", b"hi")]);
        let listing = open_archive("x.tar", &bytes).unwrap();
        let paths: Vec<(&str, u64, bool)> =
            listing.entries.iter().map(|e| (e.path.as_str(), e.size, e.directory)).collect();
        assert_eq!(paths, [("src/", 0, true), ("src/a.rs", 9, false), ("README", 2, false)]);
        assert_eq!(listing.files["README"].as_slice(), b"hi");
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, b"plain").unwrap();
        let single = open_archive("notes.txt.gz", &gz.finish().unwrap()).unwrap();
        assert_eq!(single.entries[0].path, "notes.txt");
        let mut damaged = bytes.clone();
        damaged[0] = b'x';
        assert_eq!(open_archive("x.tar", &damaged).err().unwrap(), "The TAR header is damaged.");
    }

    #[test]
    fn folders_group_their_descendants() {
        let entry = |path: &str, size, directory| ArchiveEntry { path: path.into(), size, modified: None, directory };
        let entries = [
            entry("b.txt", 1, false),
            entry("src/a.rs", 2, false),
            entry("src/x/y.rs", 3, false),
            entry("src/", 0, true),
        ];
        let root = folder_children(&entries, "");
        let rows: Vec<(&str, bool, u64)> = root.iter().map(|c| (c.path.as_str(), c.directory, c.size)).collect();
        assert_eq!(rows, [("src", true, 5), ("b.txt", false, 1)], "folders first, sizes summed");
        let src: Vec<String> = folder_children(&entries, "src/").into_iter().map(|c| c.path).collect();
        assert_eq!(src, ["src/x", "src/a.rs"]);
        assert_eq!(preview_text(b"a\0b"), None);
        assert_eq!(preview_text(b"ok"), Some(("ok".into(), false)));
    }
}
