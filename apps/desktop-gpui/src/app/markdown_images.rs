//! Images in chat markdown (`ChatMarkdown`'s `img`, `classifyMarkdownImageSource`): web
//! images load directly, workspace files through a signed `assets.createUrl` URL, and
//! anything else stays blank. `TextView` asks for each image synchronously, so loads are
//! started when a message parses and the view reads them from a shared cache.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use gpui_kit::{Context, Image, ImageFormat, ImageSource, SharedString, SharedUri};
use serde_json::json;

use super::Workspace;

/// A 1×1 transparent PNG, drawn while an image loads or when it cannot.
static BLANK: LazyLock<Arc<Image>> = LazyLock::new(|| {
    const PNG: [u8; 67] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
        0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00,
        0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D,
        0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    Arc::new(Image::from_bytes(ImageFormat::Png, PNG.to_vec()))
});

/// Loaded images by the destination the markdown wrote, for one thread: a relative
/// destination resolves against that thread's checkout. `None` is loading or failed.
#[derive(Clone, Default)]
pub(super) struct ImageCache(Arc<Mutex<HashMap<String, Option<Arc<Image>>>>>);

impl ImageCache {
    /// `TextView`'s `image_source`: the loaded image, else a blank one.
    pub(super) fn resolver(&self) -> impl Fn(&SharedUri) -> ImageSource + Send + Sync + 'static {
        let cache = self.0.clone();
        move |uri| {
            let image = cache.lock().ok().and_then(|cache| cache.get(uri.as_ref()).cloned().flatten());
            ImageSource::Image(image.unwrap_or_else(|| BLANK.clone()))
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Source {
    Direct(String),
    Data(ImageFormat, Vec<u8>),
    WorkspaceFile(String),
    Blocked,
}

/// The destinations of the `![alt](dest)` images in `markdown`, outside code fences.
pub(super) fn image_destinations(markdown: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut fenced = false;
    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find("![") {
            rest = &rest[start + 2..];
            let Some(close) = rest.find("](") else { break };
            let after = &rest[close + 2..];
            let Some(end) = after.find(')') else { break };
            let inner = after[..end].trim();
            let dest = match inner.strip_prefix('<') {
                Some(angled) => angled.split('>').next().unwrap_or_default(),
                None => inner.split_whitespace().next().unwrap_or_default(),
            };
            if !dest.is_empty() && !found.iter().any(|d| d == dest) {
                found.push(dest.to_owned());
            }
            rest = &after[end + 1..];
        }
    }
    found
}

fn is_windows_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() > 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'\\' | b'/')
}

/// `classifyMarkdownImageSource`, plus decoding data URLs for the native view.
pub(super) fn classify(destination: &str, workspace_root: Option<&str>) -> Source {
    let source = destination.trim();
    if source.is_empty() || source.starts_with('#') || source.starts_with('?') {
        return Source::Blocked;
    }
    let lower = source.to_ascii_lowercase();
    if lower.starts_with("http:") || lower.starts_with("https:") {
        return Source::Direct(source.to_owned());
    }
    if let Some(rest) = source.strip_prefix("//") {
        return Source::Direct(format!("https://{rest}"));
    }
    if lower.starts_with("data:") {
        return data_url(source).map_or(Source::Blocked, |(format, bytes)| Source::Data(format, bytes));
    }
    let path = source.strip_prefix("file://").unwrap_or(source);
    let path = path.split(['?', '#']).next().unwrap_or_default();
    // A network share loads only from inside the project's own.
    if path.starts_with("\\\\") || path.starts_with("//") {
        return match workspace_root {
            Some(root) if path.strip_prefix(root).is_some_and(|rest| rest.starts_with(['\\', '/'])) => {
                Source::WorkspaceFile(path.to_owned())
            }
            _ => Source::Blocked,
        };
    }
    if path.starts_with('/') || is_windows_absolute(path) {
        let path = if lower.starts_with("file:///") && is_windows_absolute(&path[1..]) { &path[1..] } else { path };
        return Source::WorkspaceFile(path.to_owned());
    }
    let has_scheme = path.find(':').is_some_and(|colon| {
        colon > 0 && path[..colon].chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
    });
    if has_scheme || path.starts_with("~/") || path.starts_with("~\\") || path.is_empty() {
        return Source::Blocked;
    }
    let Some(root) = workspace_root else { return Source::Blocked };
    let separator = if is_windows_absolute(root) { '\\' } else { '/' };
    let relative = path.trim_start_matches(['/', '\\']).replace(['/', '\\'], &separator.to_string());
    Source::WorkspaceFile(format!("{}{separator}{relative}", root.trim_end_matches(['/', '\\'])))
}

fn format_for(mime: &str) -> Option<ImageFormat> {
    ImageFormat::from_mime_type(mime)
}

/// `data:image/png;base64,...`.
fn data_url(url: &str) -> Option<(ImageFormat, Vec<u8>)> {
    let (meta, payload) = url.strip_prefix("data:")?.split_once(',')?;
    let mime = meta.split(';').next()?;
    let format = format_for(mime)?;
    if !meta.ends_with(";base64") {
        return None;
    }
    Some((format, base64(payload)?))
}

/// Padding is optional and whitespace is ignored, as browsers do.
fn base64(text: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    let compact: String = text.split_ascii_whitespace().collect();
    base64::engine::general_purpose::STANDARD_NO_PAD.decode(compact.trim_end_matches('=')).ok()
}

impl Workspace {
    /// Starts loading the images `markdown` shows that the cache has not seen yet.
    pub(super) fn load_markdown_images(&mut self, key: &SharedString, markdown: &str, cx: &mut Context<Self>) {
        if !markdown.contains("![") {
            return;
        }
        let root = self.git_cwd();
        let thread_id = self.thread.as_ref().map(|t| t.id.clone());
        let cache = self.markdown_images.clone();
        for destination in image_destinations(markdown) {
            {
                let Ok(mut cache) = cache.0.lock() else { return };
                if cache.contains_key(&destination) {
                    continue;
                }
                cache.insert(destination.clone(), None);
            }
            let source = classify(&destination, root.as_deref());
            let (client, base, thread_id) = (self.client(), self.http_base().map(str::to_owned), thread_id.clone());
            let (key, cache) = (key.clone(), cache.clone());
            cx.spawn(async move |this, cx| {
                let fetched: Option<(Option<String>, Vec<u8>)> = match source {
                    Source::Blocked => None,
                    Source::Data(format, bytes) => {
                        let image = Arc::new(Image::from_bytes(format, bytes));
                        let _ =
                            this.update(cx, |this, cx| this.image_loaded(&cache, destination, Some(image), &key, cx));
                        return;
                    }
                    Source::Direct(url) => fetch(url).await,
                    Source::WorkspaceFile(path) => match (client, base, thread_id) {
                        (Some(client), Some(base), Some(thread_id)) => {
                            let resource = json!({"_tag": "media-file", "threadId": thread_id, "path": path});
                            match client.request("assets.createUrl", json!({"resource": resource})).await {
                                Ok(signed) => match signed["relativeUrl"].as_str() {
                                    Some(relative) => fetch(format!("{base}{relative}")).await,
                                    None => None,
                                },
                                Err(error) => {
                                    log::debug!("assets.createUrl for a markdown image: {error}");
                                    None
                                }
                            }
                        }
                        _ => None,
                    },
                };
                let image = fetched.and_then(|(mime, bytes)| {
                    let format = mime.as_deref().and_then(format_for).or_else(|| sniff(&bytes))?;
                    Some(Arc::new(Image::from_bytes(format, bytes)))
                });
                let _ = this.update(cx, |this, cx| this.image_loaded(&cache, destination, image, &key, cx));
            })
            .detach();
        }
    }

    /// Stores a finished load in the cache it started from; a load that outlived its
    /// thread changes nothing on screen.
    fn image_loaded(
        &mut self,
        cache: &ImageCache,
        destination: String,
        image: Option<Arc<Image>>,
        key: &SharedString,
        cx: &mut Context<Self>,
    ) {
        let Some(image) = image else { return };
        if let Ok(mut cache) = cache.0.lock() {
            cache.insert(destination, Some(image));
        }
        if !Arc::ptr_eq(&cache.0, &self.markdown_images.0) {
            return;
        }
        if let Some(index) = self.rows.iter().rposition(|row| super::chat::shows_markdown(row, key)) {
            self.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
        }
        cx.notify();
    }
}

/// The bytes' format when the server named none.
fn sniff(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xFF, 0xD8, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', ..] => Some(ImageFormat::Gif),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        _ if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") => Some(ImageFormat::Svg),
        _ => None,
    }
}

/// The largest image a message loads: an agent can link any URL.
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// The image's MIME type and bytes, or `None` when it fails or passes [`MAX_IMAGE_BYTES`].
async fn fetch(url: String) -> Option<(Option<String>, Vec<u8>)> {
    crate::runtime()
        .spawn(async move {
            let mut response = reqwest::get(url).await?.error_for_status()?;
            if response.content_length().is_some_and(|length| length > MAX_IMAGE_BYTES as u64) {
                return Ok(None);
            }
            let mime = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next().unwrap_or_default().trim().to_owned());
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
                    return Ok(None);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, reqwest::Error>(Some((mime, bytes)))
        })
        .await
        .ok()?
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_outside_code_are_found_once() {
        let markdown = "![a](shot.png) and ![b](<my shot.png> \"t\")\n```\n![c](x.png)\n```\n![a](shot.png)";
        assert_eq!(image_destinations(markdown), vec!["shot.png", "my shot.png"]);
    }

    #[test]
    fn sources_classify_like_the_web() {
        assert_eq!(classify("https://x.dev/a.png", None), Source::Direct("https://x.dev/a.png".into()));
        assert_eq!(classify("//x.dev/a.png", None), Source::Direct("https://x.dev/a.png".into()));
        assert_eq!(classify("docs/a.png", Some("/repo")), Source::WorkspaceFile("/repo/docs/a.png".into()));
        assert_eq!(classify("docs/a.png", Some("C:\\repo")), Source::WorkspaceFile("C:\\repo\\docs\\a.png".into()));
        assert_eq!(classify("/tmp/a.png", None), Source::WorkspaceFile("/tmp/a.png".into()));
        assert_eq!(classify("docs/a.png", None), Source::Blocked);
        assert_eq!(classify("~/a.png", Some("/repo")), Source::Blocked);
        assert_eq!(classify("javascript:alert(1)", Some("/repo")), Source::Blocked);
        let share = Some(r"\\srv\proj");
        assert_eq!(classify(r"\\srv\proj\a.png", share), Source::WorkspaceFile(r"\\srv\proj\a.png".into()));
        assert_eq!(classify(r"\\srv\proj-secrets\a.png", share), Source::Blocked);
        assert_eq!(
            classify("data:image/png;base64,iVBO", None),
            Source::Data(ImageFormat::Png, vec![0x89, 0x50, 0x4e])
        );
    }
}
