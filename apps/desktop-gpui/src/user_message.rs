//! A user message's attachments and context records (`ChatAttachment`, `OrchestrationMessageContext`),
//! and how its row lays them out (`UserTimelineRow` in `TimelineUserMessageRow.tsx`).

use std::ops::Range;

use serde_json::Value;

/// How an attachment shows (`isImageAttachment`, `isVideoAttachment`, `filePreviewKind === "audio"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Media {
    Image,
    Video,
    Audio,
    File,
    /// A type this client does not know, listed by name only.
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub media: Media,
    /// `source._tag === "pasted-text"`: a long paste folded into a file.
    pub pasted_text: bool,
    /// An image's `SnapShotSource`.
    pub snap_shot: Option<Value>,
}

/// `GENERIC_MIME_TYPES`, which say nothing about the bytes.
const GENERIC_MIME: [&str; 5] =
    ["application/octet-stream", "binary/octet-stream", "application/unknown", "video/mp2t", "video/vnd.dlna.mpeg-tts"];
const IMAGE_EXTENSIONS: [&str; 5] = ["gif", "jpeg", "jpg", "png", "webp"];
const IMAGE_MIME: [&str; 4] = ["image/gif", "image/jpeg", "image/png", "image/webp"];
const VIDEO_EXTENSIONS: [&str; 7] = ["avi", "m4v", "mkv", "mov", "mp4", "ogv", "webm"];
const AUDIO_EXTENSIONS: [&str; 11] =
    ["mp3", "wav", "ogg", "oga", "flac", "aac", "m4a", "opus", "aiff", "webm-audio", "weba"];

fn extension(name: &str) -> String {
    name.rsplit_once('.').map(|(_, ext)| ext.trim().to_ascii_lowercase()).unwrap_or_default()
}

/// The media a `file` attachment's MIME type names, or its extension when that type is missing
/// or generic (`imageMimeType`, `videoMimeType`, `filePreviewKind`).
fn file_media(name: &str, mime: &str) -> Media {
    let mime = mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    let generic = mime.is_empty() || GENERIC_MIME.contains(&mime.as_str());
    let ext = extension(name);
    if IMAGE_MIME.contains(&mime.as_str()) || (generic && IMAGE_EXTENSIONS.contains(&ext.as_str())) {
        Media::Image
    } else if (mime.starts_with("video/") && !generic) || (generic && VIDEO_EXTENSIONS.contains(&ext.as_str())) {
        Media::Video
    } else if mime.starts_with("audio/")
        || ((generic || mime == "text/plain") && AUDIO_EXTENSIONS.contains(&ext.as_str()))
    {
        Media::Audio
    } else {
        Media::File
    }
}

impl Attachment {
    fn parse(value: &Value) -> Option<Self> {
        let text = |key: &str| value[key].as_str().map(str::to_owned);
        let (id, name, mime) = (text("id")?, text("name")?, text("mimeType").unwrap_or_default());
        let media = match value["type"].as_str()? {
            "image" => Media::Image,
            "file" => file_media(&name, &mime),
            _ => Media::Unknown,
        };
        let source = value.get("source").filter(|source| source.is_object());
        Some(Self {
            pasted_text: media != Media::Image && source.is_some_and(|s| s["_tag"] == "pasted-text"),
            snap_shot: source.filter(|s| media == Media::Image && s["kind"] == "snap-shot").cloned(),
            size: value["sizeBytes"].as_u64().unwrap_or_default(),
            id,
            name,
            mime,
            media,
        })
    }

    /// `userMessageFileLabel`.
    pub fn label(&self) -> &str {
        if self.pasted_text { "Pasted text" } else { &self.name }
    }

    /// `UserFileCard`'s line under the name: "PDF · 2.1 MB", or only the size of pasted text.
    pub fn card_detail(&self) -> String {
        let size = format_bytes(self.size as f64);
        if self.pasted_text {
            return size;
        }
        let ext = self
            .name
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            .filter(|ext| (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()));
        format!("{} · {size}", ext.map_or_else(|| "File".to_owned(), str::to_ascii_uppercase))
    }

    /// A preview annotation's screenshot, shown with its annotation instead of as media.
    fn is_preview_annotation(&self) -> bool {
        self.media == Media::Image && self.name.starts_with("preview-annotation-")
    }
}

/// `resolveUserMessagePresentation`: an attachment-only message's bootstrap prompt, even after
/// `Ultrathink:`, shows as no words (`isAttachmentOnlyBootstrapPrompt`).
pub fn without_bootstrap_prompt(text: &str) -> &str {
    let rest = text
        .get(..11)
        .filter(|head| head.eq_ignore_ascii_case("ultrathink:"))
        .map_or(text, |_| text[11..].trim_start());
    if rest == crate::composer_text::ATTACHMENT_ONLY_PROMPT { "" } else { text }
}

/// `formatBytes` (`apps/web/src/lib/formatBytes.ts`).
pub fn format_bytes(value: f64) -> String {
    if value.round() < 1024. {
        return format!("{} B", value.round());
    }
    let scaled = |v: f64| {
        format!(
            "{v:.*}",
            if v >= 100. {
                0
            } else if v >= 10. {
                1
            } else {
                2
            }
        )
    };
    let units = ["KB", "MB", "GB", "TB"];
    let mut next = value / 1024.;
    let mut unit = 0;
    while scaled(next).parse::<f64>().unwrap_or(0.) >= 1024. && unit < units.len() - 1 {
        next /= 1024.;
        unit += 1;
    }
    format!("{} {}", scaled(next), units[unit])
}

/// `formatAttachmentSize`: whole kilobytes, or megabytes to one place.
pub fn attachment_size(bytes: u64) -> String {
    let kb = bytes.div_ceil(1024).max(1);
    if kb >= 1024 { format!("{:.1} MB", bytes as f64 / (1024. * 1024.)) } else { format!("{kb} KB") }
}

/// What a user message carried besides its words.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserContent {
    pub attachments: Vec<Attachment>,
    /// `context.records`, as sent; a chip names one by its `contextId`.
    pub records: Vec<Value>,
}

impl UserContent {
    /// A `user_message` item's (or a send command's) `attachments` and `context.records`.
    pub fn parse(message: &Value) -> Self {
        let list = |value: &Value| value.as_array().cloned().unwrap_or_default();
        Self {
            attachments: list(&message["attachments"]).iter().filter_map(Attachment::parse).collect(),
            records: list(&message["context"]["records"])
                .into_iter()
                .filter(|record| record["contextId"].is_string() && record["kind"].is_string())
                .collect(),
        }
    }

    pub fn record(&self, context_id: &str) -> Option<&Value> {
        self.records.iter().find(|record| record["contextId"].as_str() == Some(context_id))
    }

    /// The attachment an `image` or `file` record names.
    pub fn record_attachment(&self, record: &Value) -> Option<&Attachment> {
        let id = record["attachmentId"].as_str()?;
        let image = record["kind"] == "image";
        self.attachments.iter().find(|a| a.id == id && (a.media == Media::Image) == image && a.media != Media::Unknown)
    }

    /// `resolvePreviewAnnotationImage`: the screenshot record's image, else the image named for
    /// the annotation, else the annotation's place among the message's annotation images.
    pub fn annotation_image(&self, record: &Value) -> Option<&Attachment> {
        let screenshot = record["screenshotContextId"]
            .as_str()
            .and_then(|id| self.record(id))
            .filter(|r| r["kind"] == "image")
            .and_then(|r| self.record_attachment(r));
        let previews: Vec<&Attachment> = self.attachments.iter().filter(|a| a.is_preview_annotation()).collect();
        let named = record["annotationId"].as_str().and_then(|id| {
            let name = format!("preview-annotation-{id}.png");
            previews.iter().copied().find(|a| a.name == name)
        });
        let nth = || {
            let annotations = self.records.iter().filter(|r| r["kind"] == "preview-annotation");
            let index = annotations.map(|r| &r["contextId"]).position(|id| *id == record["contextId"])?;
            previews.get(index).copied()
        };
        screenshot.or(named).or_else(nth)
    }
}

/// A `[label](t3-context://v1/{kind}/{id})` chip in a message's text
/// (`collectComposerContextReferences`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub kind: String,
    pub id: String,
    pub label: String,
    pub image: bool,
    pub range: Range<usize>,
}

const CONTEXT_PREFIX: &str = "t3-context://v1/";

/// `sanitizeComposerContextLabel`.
pub fn sanitize_label(label: &str, kind: &str) -> String {
    let spaced: String =
        label.chars().map(|c| if matches!(c, '[' | ']' | '\\' | '\r' | '\n') { ' ' } else { c }).collect();
    let cleaned: String = spaced.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
    if cleaned.is_empty() { kind.to_owned() } else { cleaned }
}

/// `parseComposerContextHref`'s `kind/id`.
pub fn parse_href(href: &str) -> Option<(String, String)> {
    let (kind, id) = href.strip_prefix(CONTEXT_PREFIX)?.split_once('/')?;
    let kind_ok = kind.len() <= 40
        && kind.starts_with(|c: char| c.is_ascii_lowercase())
        && kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    let id_ok = (1..=128).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    (kind_ok && id_ok).then(|| (kind.to_owned(), id.to_owned()))
}

/// Every context chip in `text`, left to right and never overlapping (`CONTEXT_LINK`).
pub fn references(text: &str) -> Vec<Reference> {
    let mut found = Vec::new();
    if !text.contains("](t3-context:") {
        return found;
    }
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find('[') {
        let open = cursor + offset;
        cursor = open + 1;
        let Some(close) = text[open + 1..].find([']', '\n']).map(|i| open + 1 + i) else { break };
        if !text[close..].starts_with(']') || text[open + 1..close].chars().count() > 512 {
            continue;
        }
        let Some(target) = text[close + 1..].strip_prefix('(') else { continue };
        let href_len = target.find(|c: char| c == ')' || c.is_whitespace()).unwrap_or(target.len());
        let href = &target[..href_len];
        let rest_len = href.len().saturating_sub(CONTEXT_PREFIX.len());
        if !href.starts_with(CONTEXT_PREFIX) || !(1..=200).contains(&rest_len) || !target[href_len..].starts_with(')') {
            continue;
        }
        let Some((kind, id)) = parse_href(href) else { continue };
        let image =
            open > 0 && text[..open].ends_with('!') && found.last().is_none_or(|r: &Reference| r.range.end < open);
        let start = if image { open - 1 } else { open };
        let end = close + 2 + href_len + 1;
        found.push(Reference {
            label: sanitize_label(&text[open + 1..close], &kind),
            kind,
            id,
            image,
            range: start..end,
        });
        cursor = end;
    }
    found
}

/// `replaceComposerContextReferences`.
pub fn replace_references(text: &str, mut replace: impl FnMut(&Reference) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for reference in references(text) {
        out.push_str(&text[cursor..reference.range.start]);
        out.push_str(&replace(&reference));
        cursor = reference.range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// The text with each chip as its label, as copies and fold summaries read it.
pub fn plain_text(text: &str) -> String {
    replace_references(text, |reference| reference.label.clone())
}

/// `isPullRequestSummaryContext`: a review-comment record that names a whole change request.
pub fn is_pull_request_summary(record: &Value) -> bool {
    let text = |key: &str| record[key].as_str().unwrap_or_default();
    record["pullRequest"].is_object()
        || (text("sectionId").starts_with("pull-request:")
            && text("diff").trim().is_empty()
            && legacy_pull_request_number(text("filePath")).is_some())
}

/// `PR #12`, the file path an older pull request summary carried.
fn legacy_pull_request_number(path: &str) -> Option<u64> {
    path.strip_prefix("PR #").filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))?.parse().ok()
}

/// `reviewCommentContextLabel`: `#12` for a change request, else the file and its line range.
pub fn review_comment_label(record: &Value) -> String {
    let text = |key: &str| record[key].as_str().unwrap_or_default();
    let number = record["pullRequest"]["number"].as_u64().or_else(|| legacy_pull_request_number(text("filePath")));
    if let Some(number) = number.filter(|_| is_pull_request_summary(record)) {
        return format!("#{number}");
    }
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let range = text("rangeLabel");
    // `+12`, `-3`, `+4 to +9`: lines after (or before) the change.
    let diff_range = range.chars().next().filter(|c| matches!(c, '+' | '-')).and_then(|sign| {
        let rest = &range[1..];
        match rest.split_once(" to ") {
            None => digits(rest).then(|| format!("L{rest}")),
            Some((start, end)) => {
                let end = end.strip_prefix(sign)?;
                (digits(start) && digits(end)).then(|| format!("L{start} to L{end}"))
            }
        }
        .map(|label| if sign == '-' { format!("{label} (before)") } else { label })
    });
    let section = text("sectionId").strip_prefix("pull-request:").filter(|n| digits(n));
    let prefix = section.map(|n| format!("#{n} ")).unwrap_or_default();
    let path = text("filePath");
    let base = path.rsplit(['/', '\\']).next().unwrap_or(path);
    format!("{prefix}{base} {}", diff_range.as_deref().unwrap_or(range))
}

/// Where a user message's attachments go (`UserTimelineRow`).
#[derive(Debug, Default, PartialEq)]
pub struct Layout<'a> {
    /// Images that show as media; a preview annotation's screenshot shows with its chip instead.
    pub images: Vec<&'a Attachment>,
    pub videos: Vec<&'a Attachment>,
    pub audio: Vec<&'a Attachment>,
    /// Files listed in the bubble: every other file, less those a chip in the text stands for.
    pub listed_files: Vec<&'a Attachment>,
    pub unknown: Vec<&'a Attachment>,
    /// Every file that is not media, for the cards a files-only message shows.
    pub files: Vec<&'a Attachment>,
    /// Images or videos sent without words show as media, with no bubble around them.
    pub media_only: bool,
    /// Media sent with words sits above the bubble as its own shape.
    pub media_above_text: bool,
    /// Files sent without words show as cards.
    pub file_cards_only: bool,
    /// Audio sent without words shows as its player alone.
    pub audio_only: bool,
}

impl Layout<'_> {
    /// The media stands on its own beside (or instead of) the bubble.
    pub fn standalone_media(&self) -> bool {
        self.media_only || self.media_above_text
    }
}

/// `UserTimelineRow`'s attachment layout for `text` (chips as written); `own_shape` is false for a
/// folded agent message or a guest's, which keep everything inside their bubble.
pub fn layout<'a>(content: &'a UserContent, text: &str, own_shape: bool) -> Layout<'a> {
    let of = |media: Media| content.attachments.iter().filter(move |a| a.media == media);
    let images: Vec<_> = of(Media::Image).filter(|a| !a.is_preview_annotation()).collect();
    let videos: Vec<_> = of(Media::Video).collect();
    let audio: Vec<_> = of(Media::Audio).collect();
    let files: Vec<_> = of(Media::File).collect();
    let unknown: Vec<_> = of(Media::Unknown).collect();
    let references = references(text);
    // The attachment each file or image chip stands for.
    let chipped = |reference: &Reference| {
        content
            .record(&reference.id)
            .filter(|record| matches!(record["kind"].as_str(), Some("file" | "image")))
            .and_then(|record| content.record_attachment(record))
    };
    let chipped_ids: Vec<&str> = references.iter().filter_map(&chipped).map(|a| a.id.as_str()).collect();
    let listed_files: Vec<_> = files.iter().copied().filter(|f| !chipped_ids.contains(&f.id.as_str())).collect();
    // The words left once the chips of `media`'s attachments are gone.
    let words_without = |media: Media| {
        replace_references(text, |reference| match chipped(reference) {
            Some(attachment) if attachment.media == media => String::new(),
            _ => reference.label.clone(),
        })
    };
    let visual = images.len() + videos.len();
    let media_only = own_shape
        && text.trim().is_empty()
        && audio.is_empty()
        && listed_files.is_empty()
        && unknown.is_empty()
        && visual > 0;
    let media_above_text = own_shape && !media_only && visual > 0;
    let file_cards_only = own_shape
        && !files.is_empty()
        && visual + audio.len() + unknown.len() == 0
        && words_without(Media::File).trim().is_empty();
    let audio_only = own_shape
        && !audio.is_empty()
        && visual + files.len() + unknown.len() == 0
        && words_without(Media::Audio).trim().is_empty();
    Layout {
        images,
        videos,
        audio,
        listed_files,
        unknown,
        files,
        media_only,
        media_above_text,
        file_cards_only,
        audio_only,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn content(attachments: Value, records: Value) -> UserContent {
        UserContent::parse(&json!({"attachments": attachments, "context": {"version": 1, "records": records}}))
    }

    fn file(id: &str, name: &str, mime: &str) -> Value {
        json!({"type": "file", "id": id, "name": name, "mimeType": mime, "sizeBytes": 2048})
    }

    #[test]
    fn attachments_sort_into_their_media() {
        let message = content(
            json!([
                {"type": "image", "id": "i", "name": "a.png", "mimeType": "image/png", "sizeBytes": 1},
                file("legacy", "b.jpg", "application/octet-stream"),
                file("v", "clip.mov", "video/quicktime"),
                file("ts", "main.ts", "video/mp2t"),
                file("s", "song.mp3", ""),
                file("t", "notes.txt", "text/plain"),
                {"type": "sticker", "id": "x", "name": "x", "mimeType": "a/b", "sizeBytes": 1},
                {"type": "file", "name": "no id"}
            ]),
            json!([]),
        );
        let media: Vec<_> = message.attachments.iter().map(|a| (a.id.as_str(), a.media)).collect();
        assert_eq!(
            media,
            [
                ("i", Media::Image),
                ("legacy", Media::Image),
                ("v", Media::Video),
                ("ts", Media::File),
                ("s", Media::Audio),
                ("t", Media::File),
                ("x", Media::Unknown)
            ]
        );
    }

    #[test]
    fn chips_are_found_like_the_web_pattern() {
        let text = "see ![shot](t3-context://v1/image/img_1) and [a [b]](t3-context://v1/file/f-2) \
                    [bad](t3-context://v1/File/x) [sp](t3-context://v1/file/a b) [x](t3-context://v1/file/f3)";
        let found: Vec<_> = references(text).into_iter().map(|r| (r.kind, r.id, r.label, r.image)).collect();
        assert_eq!(
            found,
            [("image".into(), "img_1".into(), "shot".into(), true), ("file".into(), "f3".into(), "x".into(), false)],
            "a label stops at its first `]`; bad kinds and spaced ids are no chips"
        );
        assert_eq!(plain_text("look [main.rs](t3-context://v1/file/f) now"), "look main.rs now");
        assert_eq!(references("[](t3-context://v1/terminal/t)")[0].label, "terminal", "an empty label is its kind");
    }

    #[test]
    fn media_without_words_stands_alone_and_with_words_sits_above() {
        let message = content(
            json!([{"type": "image", "id": "i", "name": "a.png", "mimeType": "image/png", "sizeBytes": 1}]),
            json!([{"version": 1, "kind": "image", "contextId": "c1", "label": "a.png", "attachmentId": "i"}]),
        );
        let alone = layout(&message, "", true);
        assert!(alone.media_only && !alone.media_above_text && alone.images.len() == 1);
        let worded = layout(&message, "look ![a.png](t3-context://v1/image/c1)", true);
        assert!(!worded.media_only && worded.media_above_text);
        let folded = layout(&message, "", false);
        assert!(!folded.standalone_media(), "a folded or guest message keeps media in its bubble");
    }

    #[test]
    fn files_whose_chips_are_all_the_words_show_as_cards() {
        let message = content(
            json!([file("f", "report.pdf", "application/pdf"), file("g", "data.csv", "text/csv")]),
            json!([{"version": 1, "kind": "file", "contextId": "c1", "label": "report.pdf", "attachmentId": "f"}]),
        );
        let cards = layout(&message, " [report.pdf](t3-context://v1/file/c1) ", true);
        assert!(cards.file_cards_only);
        assert_eq!(cards.listed_files.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(), ["g"]);
        let worded = layout(&message, "read [report.pdf](t3-context://v1/file/c1)", true);
        assert!(!worded.file_cards_only, "words beside the chip keep the bubble");
        let unknown_record = layout(&message, "[report.pdf](t3-context://v1/file/gone)", true);
        assert!(!unknown_record.file_cards_only && unknown_record.listed_files.len() == 2);
        let audio = content(json!([file("s", "a.mp3", "audio/mpeg")]), json!([]));
        assert!(layout(&audio, "", true).audio_only);
        assert!(!layout(&audio, "listen", true).audio_only);
    }

    #[test]
    fn an_annotation_finds_its_screenshot() {
        let png = |id: &str, name: &str| json!({"type": "image", "id": id, "name": name, "mimeType": "image/png", "sizeBytes": 1});
        let message = content(
            json!([png("s", "shot.png"), png("p1", "preview-annotation-x.png"), png("p2", "preview-annotation-y.png")]),
            json!([
                {"kind": "image", "contextId": "img", "attachmentId": "s", "label": "shot"},
                {"kind": "preview-annotation", "contextId": "a1", "annotationId": "q", "label": "a", "screenshotContextId": "img"},
                {"kind": "preview-annotation", "contextId": "a2", "annotationId": "y", "label": "b"},
                {"kind": "preview-annotation", "contextId": "a3", "annotationId": "z", "label": "c"}
            ]),
        );
        let image = |id: &str| message.annotation_image(message.record(id).unwrap()).map(|a| a.id.as_str());
        assert_eq!((image("a1"), image("a2"), image("a3")), (Some("s"), Some("p2"), None));
        assert_eq!(layout(&message, "", true).images.len(), 1, "annotation screenshots are not media");
    }

    #[test]
    fn review_comments_are_labeled_by_file_and_lines() {
        let comment = |section: &str, path: &str, range: &str| json!({"kind": "review-comment", "sectionId": section, "filePath": path, "rangeLabel": range, "diff": "x"});
        assert_eq!(review_comment_label(&comment("s", "src/a.rs", "+12")), "a.rs L12");
        assert_eq!(review_comment_label(&comment("s", "src\\a.rs", "-3 to -9")), "a.rs L3 to L9 (before)");
        assert_eq!(review_comment_label(&comment("pull-request:7", "b.ts", "+1 to -2")), "#7 b.ts +1 to -2");
        assert_eq!(review_comment_label(&comment("s", "c.md", "Lines 1-3")), "c.md Lines 1-3");
        let pr = json!({"kind": "review-comment", "sectionId": "x", "filePath": "f", "rangeLabel": "r", "diff": "d",
            "pullRequest": {"number": 42}});
        assert!(is_pull_request_summary(&pr));
        assert_eq!(review_comment_label(&pr), "#42");
        let legacy = json!({"sectionId": "pull-request:5", "filePath": "PR #5", "rangeLabel": "", "diff": " "});
        assert_eq!((is_pull_request_summary(&legacy), review_comment_label(&legacy)), (true, "#5".to_owned()));
    }

    #[test]
    fn sizes_and_labels_read_like_the_web() {
        let pasted = Attachment::parse(&json!({"type": "file", "id": "p", "name": "pasted-1.txt",
            "mimeType": "text/plain", "sizeBytes": 3_500_000, "source": {"_tag": "pasted-text"}}))
        .unwrap();
        assert_eq!((pasted.label(), pasted.card_detail()), ("Pasted text", "3.34 MB".to_owned()));
        let pdf = Attachment::parse(&file("f", "Report.pdf", "application/pdf")).unwrap();
        assert_eq!((pdf.label(), pdf.card_detail()), ("Report.pdf", "PDF · 2.00 KB".to_owned()));
        assert_eq!(Attachment::parse(&file("n", "Makefile", "")).unwrap().card_detail(), "File · 2.00 KB");
        assert_eq!(
            (attachment_size(0), attachment_size(1536), attachment_size(5 << 20)),
            ("1 KB".into(), "2 KB".into(), "5.0 MB".into())
        );
    }
}
