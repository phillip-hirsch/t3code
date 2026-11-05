//! Composer attachments (`ComposerAttachmentStrip.tsx`, `useComposerAttachmentIntake.ts`,
//! `attachmentUploadQueue.ts`): files from the paperclip, paste or drop are
//! uploaded as soon as they are added (`attachments.createUploadUrl`, then a
//! POST of the raw bytes to the signed URL) and sent by their server ids.

use std::collections::HashMap;
use std::io::Read as _;
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use bytes::Bytes;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::InlineToken;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, ElementId, ExternalPaths, Image, ImageFormat, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, PathPromptOptions, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img, prelude::FluentBuilder as _, px,
};
use serde_json::{Value, json};

use super::composer::ComposerDraft;
use super::snap_shot::SnapShotSource;
use super::{Target, Workspace, new_id};
use crate::composer_text;
use crate::theme::Tokens;

pub(super) const MAX_ATTACHMENTS: usize = 100;
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 50 * 1024 * 1024;
/// How long one upload POST may take before the attachment shows as failed.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(120);

static UPLOADS: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);
pub(super) use crate::composer_text::ATTACHMENT_ONLY_PROMPT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Image,
    File,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::File => "file",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Upload {
    Uploading,
    Ready(String),
    Failed(String),
}

pub(super) struct Attachment {
    local_id: String,
    kind: Kind,
    name: String,
    mime: String,
    bytes: Bytes,
    image: Option<Arc<Image>>,
    upload: Upload,
    /// The environment `upload` went to; its id only works there.
    upload_env: Option<SharedString>,
    /// A SnapShot's source, sent with the image.
    source: Option<SnapShotSource>,
}

impl Attachment {
    /// A sent message's attachment, back in the composer as a new upload.
    pub(super) fn restored(kind: &str, name: String, mime: String, bytes: Vec<u8>) -> Self {
        Self::restored_as(new_id(), kind, name, mime, bytes)
    }

    /// Restored under the id its prompt's chips still name.
    pub(super) fn restored_as(local_id: String, kind: &str, name: String, mime: String, bytes: Vec<u8>) -> Self {
        let kind = if kind == "image" { Kind::Image } else { Kind::File };
        let image = image_format(&mime)
            .filter(|_| kind == Kind::Image)
            .map(|format| Arc::new(Image::from_bytes(format, bytes.clone())));
        Self {
            local_id,
            kind,
            name,
            mime,
            bytes: Bytes::from(bytes),
            image,
            upload: Upload::Uploading,
            upload_env: None,
            source: None,
        }
    }

    pub(super) fn local_id(&self) -> &str {
        &self.local_id
    }

    pub(super) fn kind_str(&self) -> &'static str {
        self.kind.as_str()
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn mime(&self) -> &str {
        &self.mime
    }

    pub(super) fn bytes(&self) -> &Bytes {
        &self.bytes
    }

    pub(super) fn image(&self) -> Option<Arc<Image>> {
        self.image.clone()
    }

    /// Starts a new upload to `environment`, handing back a finished one (with the
    /// environment that holds it) for deletion.
    fn begin_upload(&mut self, environment: Option<SharedString>) -> Option<(SharedString, String)> {
        let previous = mem::replace(&mut self.upload, Upload::Uploading);
        match (previous, mem::replace(&mut self.upload_env, environment)) {
            (Upload::Ready(id), Some(held_by)) => Some((held_by, id)),
            _ => None,
        }
    }

    /// Whether an upload to `environment` that just finished is the one this attachment waits for.
    fn awaits_upload_on(&self, environment: &str) -> bool {
        self.upload == Upload::Uploading && self.upload_env.as_deref() == Some(environment)
    }

    fn context_id(&self) -> String {
        format!("{}_{}", self.kind.as_str(), self.local_id)
    }

    /// The id of its inline chip, as [`composer_text::Chip::id`] spells it.
    fn chip_id(&self) -> String {
        composer_text::Chip::Context { kind: self.kind.as_str().into(), id: self.context_id(), label: String::new() }
            .id()
    }

    /// The message's attachment entry for the upload `id`, plus its `context.records` entry
    /// when `text` links its chip.
    fn wire(&self, id: &str, text: &str) -> (Value, Option<Value>) {
        let (kind, context_id, size) = (self.kind.as_str(), self.context_id(), self.bytes.len());
        let mut sent = json!({"type": kind, "id": id, "name": self.name, "mimeType": self.mime, "sizeBytes": size});
        if let Some(source) = &self.source {
            sent["source"] = source.value.clone();
        }
        let record = text.contains(&format!("/{kind}/{context_id})")).then(|| {
            json!({
                "version": 1, "kind": kind, "contextId": context_id, "label": self.name, "attachmentId": id,
                "name": self.name, "mimeType": self.mime, "sizeBytes": size,
            })
        });
        (sent, record)
    }
}

/// An attachment in the composer on screen (no target: the open one's) or in the draft it was
/// stashed with, beside that draft's target.
fn find_attachment<'a>(
    active: &'a mut [Attachment],
    drafts: &'a mut HashMap<Target, ComposerDraft>,
    local_id: &str,
) -> Option<(Option<&'a Target>, &'a mut Attachment)> {
    let stashed = drafts.iter_mut().filter_map(|(target, draft)| match draft {
        ComposerDraft::Prompt(_, kept) => Some((Some(target), kept.as_mut_slice())),
        ComposerDraft::Question(_) => None,
    });
    std::iter::once((None, active))
        .chain(stashed)
        .flat_map(|(target, attachments)| attachments.iter_mut().map(move |attachment| (target, attachment)))
        .find(|(_, attachment)| attachment.local_id == local_id)
}

/// A dropped file's bytes, or its size when it is over the limit and was not read.
#[derive(Debug, PartialEq)]
enum Read {
    Bytes(Vec<u8>),
    TooLarge(u64),
}

/// Reads at most `limit` bytes, so a huge file is refused without loading it.
fn read_at_most(path: &Path, limit: u64) -> std::io::Result<Read> {
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size > limit {
        return Ok(Read::TooLarge(size));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or_default());
    file.take(limit + 1).read_to_end(&mut bytes)?;
    match u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
        read if read > limit => Ok(Read::TooLarge(read)),
        _ => Ok(Read::Bytes(bytes)),
    }
}

/// `classifyComposerAttachmentFile` by extension: GIF, JPEG, PNG and WebP are
/// images; other image types are refused; everything else is a file.
fn classify(name: &str) -> Result<(Kind, &'static str), String> {
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
    let image = match extension.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    };
    if let Some(mime) = image {
        return Ok((Kind::Image, mime));
    }
    let mime = match extension.as_str() {
        // The web converts HEIC to JPEG first; there is no decoder here.
        "heic" | "heif" => return Err(format!("'{name}' could not be read as an image.")),
        "bmp" | "tif" | "tiff" | "avif" | "ico" => {
            return Err(format!(
                "'{name}' is not a supported image type. Attach GIF, HEIC, HEIF, JPEG, PNG, or WebP images."
            ));
        }
        "svg" => "image/svg+xml",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    };
    Ok((Kind::File, mime))
}

/// `formatAttachmentSize`: whole KB, or one decimal of MB from 1024 KB.
pub(super) fn format_size(bytes: u64) -> String {
    let kb = (bytes as f64 / 1024.).ceil();
    if kb >= 1024. { format!("{:.1} MB", bytes as f64 / 1024. / 1024.) } else { format!("{kb} KB") }
}

fn image_format(mime: &str) -> Option<ImageFormat> {
    match mime {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}

impl Workspace {
    /// The paperclip shows while the server takes file uploads.
    pub(super) fn can_attach(&self) -> bool {
        self.server_info().file_upload_limit().is_some() && self.question.is_none()
    }

    pub(super) fn attachments_busy(&self) -> bool {
        self.attachments.iter().any(|a| a.upload == Upload::Uploading)
    }

    pub(super) fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }

    pub(super) fn pick_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths =
            cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update_in(cx, |this, window, cx| this.add_paths(paths, window, cx));
            }
        })
        .detach();
    }

    pub(super) fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        self.dragging_files = false;
        if self.can_attach() && self.target.is_some() {
            self.add_paths(paths.paths().to_vec(), window, cx);
        }
    }

    pub(super) fn on_file_drag_move(
        &mut self,
        event: &gpui_kit::DragMoveEvent<ExternalPaths>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let over = self.can_attach() && self.target.is_some() && event.bounds.contains(&event.event.position);
        if over != self.dragging_files {
            self.dragging_files = over;
            cx.notify();
        }
    }

    /// The composer's paste hook: images and copied files attach, text pastes as usual.
    pub(super) fn paste_attachments(
        &mut self,
        item: &gpui_kit::ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.can_attach() {
            return false;
        }
        let mut handled = false;
        for entry in item.entries() {
            match entry {
                gpui_kit::ClipboardEntry::Image(image) => {
                    let (mime, extension) = match image.format() {
                        ImageFormat::Png => ("image/png", "png"),
                        ImageFormat::Jpeg => ("image/jpeg", "jpg"),
                        ImageFormat::Gif => ("image/gif", "gif"),
                        ImageFormat::Webp => ("image/webp", "webp"),
                        _ => continue,
                    };
                    self.add_attachment(
                        format!("image.{extension}"),
                        Kind::Image,
                        mime,
                        image.bytes().to_vec(),
                        window,
                        cx,
                    );
                    handled = true;
                }
                gpui_kit::ClipboardEntry::ExternalPaths(paths) => {
                    self.add_paths(paths.paths().to_vec(), window, cx);
                    handled = true;
                }
                gpui_kit::ClipboardEntry::String(text) if !handled => {
                    handled = self.fold_pasted_text(text.text(), window, cx);
                }
                gpui_kit::ClipboardEntry::String(_) => {}
            }
        }
        handled
    }

    /// `foldPastedText`: a large paste, or one past the input limit, attaches as a text
    /// file; one that can neither fit nor attach is refused.
    pub(super) fn fold_pasted_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let state = self.composer.read(cx);
        let selected = state.selected_range();
        let value = state.value();
        let kept = composer_text::js_length(&value) - composer_text::js_length(&value[selected]);
        let would_exceed = kept + composer_text::js_length(text) > composer_text::MAX_INPUT_CHARS;
        if !composer_text::paste_folds(text, would_exceed) {
            return false;
        }
        let limit = self.server_info().file_upload_limit().map(|l| l.min(MAX_FILE_BYTES));
        let can_stage = self.target.is_some() && self.can_attach() && self.attachments.len() < MAX_ATTACHMENTS;
        let refuse = |title: &'static str, description: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            let toast = gpui_kit::component::notification::Notification::error(description).title(title);
            gpui_kit::component::WindowExt::push_notification(window, toast, cx);
            true
        };
        let Some(limit) = limit.filter(|_| can_stage) else {
            return would_exceed
                && refuse(
                    "Pasted text is too large for this message",
                    "Remove some text or an attachment, then paste again.",
                    window,
                    cx,
                );
        };
        if text.len() as u64 > limit {
            return would_exceed
                && refuse(
                    "Pasted text is too large to attach",
                    "Reduce the clipboard contents or save a smaller excerpt as a file.",
                    window,
                    cx,
                );
        }
        let name = composer_text::next_pasted_text_name(self.attachments.iter().map(|a| a.name.as_str()));
        self.add_attachment(name, Kind::File, "text/plain", text.as_bytes().to_vec(), window, cx);
        true
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let files: Vec<PathBuf> = paths.into_iter().filter(|p| !p.is_dir()).collect();
        if files.is_empty() {
            return;
        }
        let read = cx.background_executor().spawn(async move {
            files
                .into_iter()
                .map(|path| {
                    let name =
                        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
                    let read = read_at_most(&path, MAX_FILE_BYTES);
                    (name, read)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn_in(window, async move |this, cx| {
            let files = read.await;
            let _ = this.update_in(cx, |this, window, cx| {
                for (name, read) in files {
                    let (kind, mime) = match classify(&name) {
                        Ok(classified) => classified,
                        Err(error) => {
                            this.error = Some(error.into());
                            continue;
                        }
                    };
                    match read {
                        Ok(Read::Bytes(bytes)) if !bytes.is_empty() => {
                            this.add_attachment(name, kind, mime, bytes, window, cx);
                        }
                        Ok(Read::TooLarge(size)) => {
                            this.error = this.attachment_error(&name, kind, size).map(Into::into)
                        }
                        Ok(Read::Bytes(_)) => {
                            this.error = Some(format!("'{name}' is empty or could not be read.").into())
                        }
                        Err(error) => {
                            log::warn!("could not read {name:?}: {error}");
                            this.error = Some(format!("'{name}' is empty or could not be read.").into());
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Why an attachment of `size` bytes can't join the composer, or `None` when it can.
    fn attachment_error(&self, name: &str, kind: Kind, size: u64) -> Option<String> {
        let file_limit = self.server_info().file_upload_limit().unwrap_or(MAX_FILE_BYTES).min(MAX_FILE_BYTES);
        if kind == Kind::Image && size > MAX_IMAGE_BYTES {
            Some(format!("'{name}' is too large to attach, even after compression."))
        } else if size > file_limit {
            Some(format!("'{name}' exceeds the {} MB attachment limit.", file_limit / 1024 / 1024))
        } else {
            None
        }
    }

    fn add_attachment(
        &mut self,
        name: String,
        kind: Kind,
        mime: &str,
        bytes: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let error = if self.attachments.len() >= MAX_ATTACHMENTS {
            Some(format!("You can attach up to {MAX_ATTACHMENTS} files per message."))
        } else {
            self.attachment_error(&name, kind, bytes.len() as u64)
        };
        if let Some(error) = error {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        let image = image_format(mime)
            .filter(|_| kind == Kind::Image)
            .map(|format| Arc::new(Image::from_bytes(format, bytes.clone())));
        let attachment = Attachment {
            local_id: new_id(),
            kind,
            name,
            mime: mime.to_owned(),
            bytes: Bytes::from(bytes),
            image,
            upload: Upload::Uploading,
            upload_env: None,
            source: None,
        };
        // Files always get an inline chip; images only once the prompt has text.
        if kind == Kind::File || !self.composer_empty {
            let link = composer_text::context_link(kind.as_str(), &attachment.context_id(), &attachment.name);
            let chip = composer_text::Chip::Context {
                kind: kind.as_str().into(),
                id: attachment.context_id(),
                label: attachment.name.clone(),
            };
            self.composer.update(cx, |state, cx| {
                let at = state.cursor();
                let needs_space = at > 0 && !state.value()[..at].ends_with(char::is_whitespace);
                if needs_space {
                    state.insert(" ", window, cx);
                }
                let at = state.cursor();
                let token = InlineToken::new(chip.id(), link).with_label(attachment.name.clone());
                if state.replace_range_with_token(at..at, token, window, cx).is_ok() {
                    state.insert(" ", window, cx);
                }
            });
            self.sync_composer_empty(cx);
        }
        let local_id = attachment.local_id.clone();
        self.attachments.push(attachment);
        self.upload(local_id, cx);
        cx.notify();
    }

    /// A SnapShot capture into the composer on screen, with its source.
    pub(super) fn attach_snap_shot(
        &mut self,
        name: String,
        mime: &str,
        bytes: Vec<u8>,
        source: SnapShotSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at = self.attachments.len();
        self.add_attachment(name, Kind::Image, mime, bytes, window, cx);
        if let Some(attachment) = self.attachments.get_mut(at) {
            attachment.source = Some(source);
        }
    }

    /// A SnapShot capture into `target`'s set-aside draft, which the user left while it was
    /// taken; `false` with the reason when that draft can't take it.
    pub(super) fn stash_snap_shot(
        &mut self,
        target: Target,
        name: String,
        mime: &str,
        bytes: Vec<u8>,
        source: SnapShotSource,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if let Some(error) = self.attachment_error(&name, Kind::Image, bytes.len() as u64) {
            return Err(error);
        }
        let draft =
            self.composer_drafts.entry(target).or_insert_with(|| ComposerDraft::Prompt(String::new(), Vec::new()));
        let full = || "Remove an attachment, then try this capture again.".to_owned();
        let ComposerDraft::Prompt(_, kept) = draft else { return Err(full()) };
        if kept.len() >= MAX_ATTACHMENTS {
            return Err(full());
        }
        let mut attachment = Attachment::restored(Kind::Image.as_str(), name, mime.to_owned(), bytes);
        attachment.source = Some(source);
        let local_id = attachment.local_id.clone();
        kept.push(attachment);
        self.upload(local_id, cx);
        Ok(())
    }

    /// Mints a signed upload URL on the environment of the attachment's draft, then POSTs the bytes to it.
    pub(super) fn upload(&mut self, local_id: String, cx: &mut Context<Self>) {
        let Some((draft, _)) = find_attachment(&mut self.attachments, &mut self.composer_drafts, &local_id) else {
            return;
        };
        let environment = draft.map(Target::environment).cloned();
        let environment = environment.or_else(|| self.active_environment().cloned());
        let client = environment.as_deref().and_then(|id| self.env_client(id));
        let http_base = environment.as_deref().and_then(|id| self.env_endpoint(id)).map(|e| e.http_base.clone());
        let Some((_, attachment)) = find_attachment(&mut self.attachments, &mut self.composer_drafts, &local_id) else {
            return;
        };
        let released = attachment.begin_upload(environment.clone());
        let mut input =
            json!({"name": attachment.name, "mimeType": attachment.mime, "sizeBytes": attachment.bytes.len()});
        if attachment.kind == Kind::File {
            input["type"] = "file".into();
        }
        let (bytes, mime) = (attachment.bytes.clone(), attachment.mime.clone());
        let connected = match (environment, client, http_base) {
            (Some(environment), Some(client), Some(http_base)) => Some((environment, client, http_base)),
            _ => {
                attachment.upload = Upload::Failed("Not connected".into());
                None
            }
        };
        if let Some((held_by, id)) = released {
            self.delete_upload(&held_by, id, cx);
        }
        let Some((environment, client, http_base)) = connected else { return };
        cx.spawn(async move |this, cx| {
            let result = async {
                let minted = client.request("attachments.createUploadUrl", input).await.map_err(|error| {
                    log::warn!("attachments.createUploadUrl failed: {error}");
                    "Upload could not start".to_owned()
                })?;
                let id = minted["attachmentId"].as_str().ok_or("Upload could not start")?.to_owned();
                let url = format!("{http_base}{}", minted["relativeUrl"].as_str().ok_or("Upload could not start")?);
                let sent = crate::runtime()
                    .spawn(async move {
                        UPLOADS
                            .post(url)
                            .header(reqwest::header::CONTENT_TYPE, mime)
                            .body(bytes)
                            .timeout(UPLOAD_TIMEOUT)
                            .send()
                            .await
                            .map(|response| response.status())
                    })
                    .await;
                match sent {
                    Ok(Ok(status)) if status.is_success() => Ok(id),
                    Ok(Ok(status)) => Err(format!("Upload rejected ({})", status.as_u16())),
                    Ok(Err(error)) => {
                        log::warn!("attachment upload failed: {error}");
                        Err("Upload failed".to_owned())
                    }
                    Err(error) => {
                        log::warn!("attachment upload task failed: {error}");
                        Err("Upload failed".to_owned())
                    }
                }
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                let waiting = find_attachment(&mut this.attachments, &mut this.composer_drafts, &local_id)
                    .map(|(_, attachment)| attachment)
                    .filter(|attachment| attachment.awaits_upload_on(&environment));
                if let Some(attachment) = waiting {
                    attachment.upload = match result {
                        Ok(id) => Upload::Ready(id),
                        Err(reason) => Upload::Failed(reason),
                    };
                } else if let Ok(id) = result {
                    // Removed, moved to another machine or uploaded again meanwhile: the server drops the pending file.
                    this.delete_upload(&environment, id, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The draft moved to another machine: what was uploaded elsewhere uploads its bytes again,
    /// since an upload id only works where it was taken (`startAttachmentUpload`).
    pub(super) fn sync_attachment_uploads(&mut self, cx: &mut Context<Self>) {
        let environment = self.active_environment().cloned();
        let moved: Vec<String> =
            self.attachments.iter().filter(|a| a.upload_env != environment).map(|a| a.local_id.clone()).collect();
        for local_id in moved {
            self.upload(local_id, cx);
        }
    }

    /// Lets the environment that took `attachment`'s finished upload drop it.
    pub(super) fn release_upload(&self, attachment: &Attachment, cx: &mut Context<Self>) {
        if let (Upload::Ready(id), Some(environment)) = (&attachment.upload, &attachment.upload_env) {
            self.delete_upload(environment, id.clone(), cx);
        }
    }

    fn delete_upload(&self, environment: &str, id: String, cx: &mut Context<Self>) {
        let Some(client) = self.env_client(environment) else { return };
        cx.spawn(async move |_, _| client.request("attachments.delete", json!({"attachmentId": id})).await).detach();
    }

    pub(super) fn remove_attachment(&mut self, local_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.attachments.iter().position(|a| a.local_id == local_id) else { return };
        let attachment = self.attachments.remove(index);
        self.release_upload(&attachment, cx);
        let chip_id = attachment.chip_id();
        self.composer.update(cx, |state, cx| {
            if let Some(span) = state.tokens().iter().find(|s| s.token().id().as_ref() == chip_id).map(|s| s.range()) {
                state.set_selected_range(span, cx);
                state.replace("", window, cx);
            }
        });
        self.sync_composer_empty(cx);
        cx.notify();
    }

    /// Drops attachments whose chip the user deleted from the text.
    pub(super) fn sync_attachment_chips(&mut self, previous_chips: &[String], cx: &mut Context<Self>) {
        let state = self.composer.read(cx);
        let live: Vec<String> = state.tokens().iter().map(|s| s.token().id().to_string()).collect();
        let dropped: Vec<String> = self
            .attachments
            .iter()
            .filter(|a| {
                let id = a.chip_id();
                previous_chips.contains(&id) && !live.contains(&id)
            })
            .map(|a| a.local_id.clone())
            .collect();
        if dropped.is_empty() {
            return;
        }
        self.attachments.retain(|a| !dropped.contains(&a.local_id));
        cx.notify();
    }

    /// The send payload's `attachments` and `context`, or why it cannot send yet.
    pub(super) fn attachment_payload(&self, text: &str) -> Result<(Vec<Value>, Option<Value>), String> {
        // An upload on another machine is re-uploading here (`sync_attachment_uploads`).
        let here = self.active_environment();
        if self.attachments_busy() || self.attachments.iter().any(|a| a.upload_env.as_ref() != here) {
            return Err("Attachment(s) still uploading".into());
        }
        if self.attachments.iter().any(|a| matches!(a.upload, Upload::Failed(_))) {
            return Err("Retry or remove failed uploads before sending.".into());
        }
        let mut attachments = Vec::new();
        let mut records = self.terminal_context_records(text);
        for attachment in &self.attachments {
            let Upload::Ready(id) = &attachment.upload else { continue };
            let (sent, record) = attachment.wire(id, text);
            attachments.push(sent);
            records.extend(record);
        }
        let context = (!records.is_empty()).then(|| json!({"version": 1, "records": records}));
        Ok((attachments, context))
    }

    /// Images not shown inline sit in the media row; files not shown inline in the list.
    pub(super) fn render_attachments(&self, t: &Tokens, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.attachments.is_empty() {
            return None;
        }
        let chips: Vec<String> = self.composer.read(cx).tokens().iter().map(|s| s.token().id().to_string()).collect();
        let inline = |a: &Attachment| chips.contains(&a.chip_id());
        let images: Vec<&Attachment> =
            self.attachments.iter().filter(|a| a.kind == Kind::Image && !inline(a)).collect();
        let files: Vec<&Attachment> = self.attachments.iter().filter(|a| a.kind == Kind::File && !inline(a)).collect();
        if images.is_empty() && files.is_empty() {
            return None;
        }
        Some(
            v_flex()
                .when(!images.is_empty(), |el| {
                    el.child(
                        h_flex()
                            .mb(px(12.))
                            .gap(px(8.))
                            .flex_wrap()
                            .children(images.into_iter().map(|a| self.render_image_tile(a, t, cx))),
                    )
                })
                .when(!files.is_empty(), |el| {
                    el.child(
                        v_flex()
                            .mb(px(12.))
                            .gap(px(4.))
                            .children(files.into_iter().map(|a| self.render_file_row(a, t, cx))),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_image_tile(&self, attachment: &Attachment, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let id = attachment.local_id.clone();
        let retry_id = id.clone();
        let failed = match &attachment.upload {
            Upload::Failed(reason) => Some(reason.clone()),
            _ => None,
        };
        div()
            .id(ElementId::Name(format!("attachment-{id}").into()))
            .relative()
            // `SNAP_SHOT_ATTACHMENT_FRAME_CLASS`.
            .map(|el| if attachment.source.is_some() { el.w(px(208.)).h(px(112.)) } else { el.size(px(64.)) })
            .flex_none()
            .overflow_hidden()
            .rounded(px(8.))
            .border_1()
            .border_color(t.border.opacity(0.8))
            .bg(t.background)
            .map(|el| match &attachment.image {
                Some(image) => el.child(img(image.clone()).size_full().object_fit(ObjectFit::Cover)),
                None => el.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .px(px(4.))
                        .text_size(px(10.))
                        .text_color(t.muted_foreground)
                        .child(attachment.name.clone()),
                ),
            })
            .children(attachment.source.as_ref().map(|source| source.render_details(&id, t)))
            .when(attachment.upload == Upload::Uploading, |el| {
                el.child(
                    div()
                        .absolute()
                        .bottom_0()
                        .left_0()
                        .right_0()
                        .px(px(4.))
                        .bg(gpui_kit::hsla(0., 0., 0., 0.85))
                        .flex()
                        .justify_center()
                        .text_size(px(10.))
                        .text_color(t.foreground)
                        .child("Uploading"),
                )
            })
            .children(failed.map(|reason| {
                overlay_button(ElementId::Name(format!("retry-{id}").into()), Lucide::RefreshCw)
                    .left(px(4.))
                    .bottom(px(4.))
                    .tooltip(move |window, cx| Tooltip::new(reason.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.upload(retry_id.clone(), cx)))
            }))
            .child(
                overlay_button(ElementId::Name(format!("remove-{id}").into()), Lucide::X)
                    .right(px(4.))
                    .top(px(4.))
                    .tooltip({
                        let label = format!("Remove {}", attachment.name);
                        move |window, cx| Tooltip::new(label.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.remove_attachment(&id, window, cx))),
            )
            .into_any_element()
    }

    fn render_file_row(&self, attachment: &Attachment, t: &Tokens, cx: &mut Context<Self>) -> AnyElement {
        let id = attachment.local_id.clone();
        let retry_id = id.clone();
        let status = match &attachment.upload {
            Upload::Uploading => "Uploading".to_owned(),
            Upload::Ready(_) => format_size(attachment.bytes.len() as u64),
            Upload::Failed(_) => "Attach again".to_owned(),
        };
        let failed = matches!(attachment.upload, Upload::Failed(_));
        let preview_id = id.clone();
        h_flex()
            .id(ElementId::Name(format!("attachment-{id}").into()))
            .min_w_0()
            .gap(px(8.))
            .py(px(4.))
            .text_sm()
            .child(Icon::new(Lucide::File).with_size(px(16.)).text_color(t.muted_foreground))
            .child(
                div()
                    .id(ElementId::Name(format!("preview-{id}").into()))
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .cursor_pointer()
                    .text_color(t.foreground)
                    .hover(|s| s.underline())
                    .child(attachment.name.clone())
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_attachment_preview(&preview_id, window, cx)),
                    ),
            )
            .child(div().text_xs().text_color(t.muted_foreground).child(status))
            .when(failed, |el| {
                el.child(
                    ghost_icon(ElementId::Name(format!("retry-{id}").into()), Lucide::RefreshCw, t)
                        .on_click(cx.listener(move |this, _, _, cx| this.upload(retry_id.clone(), cx))),
                )
            })
            .child(
                ghost_icon(ElementId::Name(format!("remove-{id}").into()), Lucide::X, t)
                    .on_click(cx.listener(move |this, _, window, cx| this.remove_attachment(&id, window, cx))),
            )
            .into_any_element()
    }

    /// The canvas overlay while files are dragged over it (`TimelineOverlays.tsx`).
    pub(super) fn render_drop_overlay(&self, t: &Tokens) -> AnyElement {
        div()
            .absolute()
            .inset(px(8.))
            .rounded(px(16.))
            .border_2()
            .border_dashed()
            .border_color(t.primary.opacity(0.6))
            .bg(t.primary.opacity(0.035))
            .flex()
            .items_center()
            .justify_center()
            .child(
                h_flex()
                    .gap(px(8.))
                    .px(px(16.))
                    .py(px(10.))
                    .rounded_full()
                    .border_1()
                    .border_color(t.primary.opacity(0.25))
                    .bg(gpui_kit::hsla(0., 0., 0., 0.95))
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(t.foreground)
                    .child(Icon::new(Lucide::Paperclip).with_size(px(16.)).text_color(t.primary))
                    .child("Drop files to attach"),
            )
            .into_any_element()
    }
}

fn overlay_button(id: ElementId, icon: Lucide) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .absolute()
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(gpui_kit::hsla(0., 0., 0., 0.65))
        .border_1()
        .border_color(gpui_kit::hsla(0., 0., 1., 0.2))
        .text_color(gpui_kit::white())
        .cursor_pointer()
        .hover(|s| s.bg(gpui_kit::hsla(0., 0., 0., 0.8)))
        .child(Icon::new(icon).with_size(px(14.)))
}

fn ghost_icon(id: ElementId, icon: Lucide, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .text_color(t.muted_foreground)
        .cursor_pointer()
        .hover(|s| s.bg(t.accent).text_color(t.foreground))
        .child(Icon::new(icon).with_size(px(14.)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_classify_by_extension() {
        assert_eq!(classify("shot.PNG"), Ok((Kind::Image, "image/png")));
        assert_eq!(classify("a.jpeg"), Ok((Kind::Image, "image/jpeg")));
        assert_eq!(classify("logo.svg"), Ok((Kind::File, "image/svg+xml")), "svg is a file");
        assert_eq!(classify("notes"), Ok((Kind::File, "application/octet-stream")));
        assert!(classify("photo.bmp").is_err());
    }

    #[test]
    fn an_attachment_is_sent_with_a_context_record_only_when_the_prompt_links_its_chip() {
        let file = Attachment::restored_as("l1".into(), "file", "notes.txt".into(), "text/plain".into(), vec![1, 2, 3]);
        let link = composer_text::context_link("file", &file.context_id(), "notes.txt");
        let (sent, record) = file.wire("up1", &format!("see {link}"));
        assert_eq!(
            sent,
            json!({"type": "file", "id": "up1", "name": "notes.txt", "mimeType": "text/plain", "sizeBytes": 3})
        );
        assert_eq!(
            record,
            Some(json!({
                "version": 1, "kind": "file", "contextId": "file_l1", "label": "notes.txt", "attachmentId": "up1",
                "name": "notes.txt", "mimeType": "text/plain", "sizeBytes": 3,
            }))
        );
        assert_eq!(file.wire("up1", "no chip here").1, None);
        assert_eq!(file.chip_id(), "context:file/file_l1");
    }

    #[test]
    fn a_file_over_the_limit_is_refused_without_reading_it() {
        let path = std::env::temp_dir().join(format!("t3-gpui-attachment-{}.bin", std::process::id()));
        std::fs::write(&path, [7u8; 11]).unwrap();
        assert_eq!(read_at_most(&path, 10).unwrap(), Read::TooLarge(11));
        assert_eq!(read_at_most(&path, 11).unwrap(), Read::Bytes(vec![7; 11]));
        std::fs::remove_file(&path).unwrap();
        assert!(read_at_most(&path, 11).is_err(), "a missing file is an error, not an empty file");
    }

    #[test]
    fn an_upload_finds_its_attachment_in_the_draft_it_was_stashed_with() {
        let attachment = |id: &str| Attachment {
            local_id: id.into(),
            kind: Kind::File,
            name: "a.txt".into(),
            mime: "text/plain".into(),
            bytes: Bytes::new(),
            image: None,
            upload: Upload::Uploading,
            upload_env: None,
            source: None,
        };
        let stashed_in = Target::Thread { env: "e".into(), id: "t1".into() };
        let mut active = vec![attachment("on-screen")];
        let mut drafts =
            HashMap::from([(stashed_in.clone(), ComposerDraft::Prompt("hi".into(), vec![attachment("stashed")]))]);
        let (target, found) = find_attachment(&mut active, &mut drafts, "stashed").unwrap();
        assert_eq!(target, Some(&stashed_in), "a stashed attachment uploads to its draft's environment");
        found.upload = Upload::Ready("srv".into());
        let Some(ComposerDraft::Prompt(_, kept)) = drafts.get(&stashed_in) else { panic!() };
        assert_eq!(kept[0].upload, Upload::Ready("srv".into()));
        assert!(find_attachment(&mut active, &mut drafts, "on-screen").is_some_and(|(target, _)| target.is_none()));
        assert!(find_attachment(&mut active, &mut drafts, "removed").is_none());
    }

    #[test]
    fn an_upload_lands_only_on_the_machine_its_attachment_waits_on() {
        let mut attachment = Attachment::restored("file", "a.txt".into(), "text/plain".into(), vec![1]);
        assert_eq!(attachment.begin_upload(Some("a".into())), None);
        assert!(attachment.awaits_upload_on("a"));
        attachment.upload = Upload::Ready("on-a".into());

        // The draft moved to b: a's finished upload goes back to a for deletion.
        assert_eq!(attachment.begin_upload(Some("b".into())), Some(("a".into(), "on-a".into())));
        assert!(!attachment.awaits_upload_on("a"), "a late upload to a must not land on b's draft");
        assert!(attachment.awaits_upload_on("b"));

        // A second upload finishing after the first landed is stale too.
        attachment.upload = Upload::Ready("on-b".into());
        assert!(!attachment.awaits_upload_on("b"));
    }

    #[test]
    fn sizes_read_like_the_web() {
        assert_eq!(format_size(1), "1 KB");
        assert_eq!(format_size(1536), "2 KB");
        assert_eq!(format_size(1024 * 1024 + 300_000), "1.3 MB");
    }
}
