//! Audio and video in chat markdown (`ChatMarkdown`'s `img` for a media destination): a video
//! plays in the browser's own player (`ChatMarkdownVideo`), sized to the video; audio is
//! `InlineAudioPreview`'s chip, with its play button, progress and time. Both run in an embedded
//! page ([`super::embed`]) so the reader's click starts playback, as autoplay rules require.
//! Workspace files play through a signed `media-file` URL; other sources show "unavailable".

use std::collections::HashMap;
use std::fmt::Write as _;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::text::{
    InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
};
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Font, FontWeight, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, Task,
    TextRun, WeakEntity, Window, div, px, rgb,
};
use serde_json::{Value, json};

use super::Workspace;
use super::embed::{EmbedView, FRAME_CSS, FRAME_SCRIPT, Layer};
use super::markdown_images::{Source, classify};
use super::mermaid::css_color;
use super::side_chat::PaneOwner;

/// `max-w-[min(100%,30rem)]` and `max-h-[30rem]`, in rem.
const MEDIA_MAX_REM: f32 = 30.;
/// Media kept loaded off screen, beside what is on it.
const KEPT_MEDIA: usize = 4;
/// `--context-chip-file`, as the composer's file chips draw it.
const CHIP_ACCENT: u32 = 0x0096af;
/// `middleTruncateAttachmentName`'s default.
const CHIP_NAME_CHARACTERS: usize = 36;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MediaKind {
    Audio,
    Video,
}

impl MediaKind {
    fn unavailable(self) -> &'static str {
        match self {
            Self::Audio => "Audio unavailable",
            Self::Video => "Video unavailable",
        }
    }
}

const VIDEO_EXTENSIONS: [&str; 7] = ["avi", "m4v", "mkv", "mov", "mp4", "ogv", "webm"];
const AUDIO_EXTENSIONS: [&str; 11] =
    ["mp3", "wav", "ogg", "oga", "flac", "aac", "m4a", "opus", "aiff", "webm-audio", "weba"];

/// `mediaKindFromPath` for audio and video: a `data:` URL by its type, anything else by its file
/// name's extension (a URL's decoded path).
pub(super) fn media_kind(destination: &str) -> Option<MediaKind> {
    let trimmed = destination.trim();
    let source = trimmed.strip_prefix('<').and_then(|s| s.strip_suffix('>')).unwrap_or(trimmed);
    let lower = source.to_ascii_lowercase();
    if let Some(data) = lower.strip_prefix("data:") {
        let mime = data.split([';', ',']).next().unwrap_or_default();
        return if mime.starts_with("video/") {
            Some(MediaKind::Video)
        } else if mime.starts_with("audio/") {
            Some(MediaKind::Audio)
        } else {
            None
        };
    }
    let path = if ["http:", "https:", "file:", "//"].iter().any(|scheme| lower.starts_with(scheme)) {
        let base = url::Url::parse("https://media.invalid").ok()?;
        base.join(source).ok()?.path().to_owned()
    } else {
        source.split(['?', '#']).next().unwrap_or_default().to_owned()
    };
    let decoded = percent_decode(&path);
    let name = decoded.rsplit(['/', '\\']).next().unwrap_or_default();
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    let simple = extension.bytes().all(|b| b.is_ascii_alphanumeric());
    if simple && VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        return Some(MediaKind::Video);
    }
    // Image extensions win over the audio ones only by not being listed here.
    let audio = !extension.is_empty() && extension.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    (audio && AUDIO_EXTENSIONS.contains(&extension.as_str())).then_some(MediaKind::Audio)
}

/// `decodeURIComponent`, keeping the text when it doesn't decode.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|b| char::from(*b).to_digit(16));
        match (bytes[index], hex(index + 1), hex(index + 2)) {
            (b'%', Some(high), Some(low)) => {
                #[expect(clippy::cast_possible_truncation, reason = "two hex digits")]
                out.push((high * 16 + low) as u8);
                index += 3;
            }
            (byte, ..) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

/// `middleTruncateAttachmentName`.
fn middle_truncate(name: &str, max: usize) -> String {
    let characters: Vec<char> = name.chars().collect();
    if characters.len() <= max {
        return name.to_owned();
    }
    let available = max.saturating_sub(1);
    let suffix = (available.saturating_sub(1)).min(if available >= 18 { 14 } else { available.div_ceil(2) });
    let prefix = available - suffix;
    let mut out: String = characters[..prefix].iter().collect();
    out.push('…');
    out.extend(&characters[characters.len() - suffix..]);
    out
}

/// The video's box: its natural size, shrunk to fit `max_width` and `max_height`; the browser's
/// 300×150 until the video reports one.
fn video_box(natural: Option<(f32, f32)>, max_width: f32, max_height: f32) -> (f32, f32) {
    let (width, height) = natural.filter(|(w, h)| *w > 0. && *h > 0.).unwrap_or((300., 150.));
    let scale = (max_width.max(1.) / width).min(max_height / height).min(1.);
    (width * scale, height * scale)
}

/// What a media page reports over IPC.
#[derive(Debug, PartialEq)]
enum MediaMessage {
    Ready,
    /// A video's natural size.
    Size(f32, f32),
    /// The video could not play its source.
    Failed,
    /// The chip's label or Open, for a workspace file.
    Open,
    /// The chip's Retry: sign the URL again.
    Retry,
}

#[expect(clippy::cast_possible_truncation, reason = "video dimensions")]
fn media_message(body: &str) -> Option<MediaMessage> {
    let value: Value = serde_json::from_str(body).ok()?;
    Some(match value["type"].as_str()? {
        "ready" => MediaMessage::Ready,
        "size" => {
            let side = |name: &str| value[name].as_f64().filter(|v| v.is_finite() && *v > 0.).map(|v| v as f32);
            MediaMessage::Size(side("w")?, side("h")?)
        }
        "error" => MediaMessage::Failed,
        "open" => MediaMessage::Open,
        "retry" => MediaMessage::Retry,
        _ => return None,
    })
}

/// Lucide's icons the chip swaps between.
const PLAY_ICON: &str = "<polygon points=\"6 3 20 12 6 21 6 3\"/>";
const PAUSE_ICON: &str = "<rect x=\"14\" y=\"4\" width=\"4\" height=\"16\" rx=\"1\"/><rect x=\"6\" y=\"4\" width=\"4\" height=\"16\" rx=\"1\"/>";
const LOADER_ICON: &str = "<path d=\"M21 12a9 9 0 1 1-6.219-8.56\"/>";
const ALERT_ICON: &str = "<path d=\"m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3\"/>\
<path d=\"M12 9v4\"/><path d=\"M12 17h.01\"/>";

/// `AudioChip`'s states, as the page paints them.
const CHIP_SCRIPT: &str = "(()=>{const $=(i)=>document.getElementById(i);\
const m=$('m'),a=$('a'),t=$('t'),p=$('p'),c=$('c'),n=$('n');\
const post=(x)=>window.ipc.postMessage(JSON.stringify(x));\
const svg=(d,spin)=>'<svg viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" \
stroke-linecap=\"round\" stroke-linejoin=\"round\"'+(spin?' class=\"spin\"':'')+'>'+d+'</svg>';\
let failed=!m.getAttribute('src'),ready=false,playing=false,duration=0,elapsed=0;\
const fmt=(s)=>Math.floor(s/60)+':'+String(Math.floor(s%60)).padStart(2,'0');\
const paint=()=>{const loading=!failed&&!ready;\
const action=failed?(OPEN?'Open':'Retry'):loading?'Loading':playing?'Pause':'Play';\
a.innerHTML=svg(failed?ICONS.alert:loading?ICONS.load:playing?ICONS.pause:ICONS.play,loading);\
a.disabled=loading;a.setAttribute('aria-label',(failed?'Audio unavailable. ':'')+action+' '+NAME);\
c.classList.toggle('bad',failed);c.title=failed?'Audio unavailable. Open the file or retry playback.':NAME;\
n.disabled=!(OPEN||failed);n.setAttribute('aria-label',(failed&&!OPEN?'Retry':'Preview')+' '+NAME);\
t.textContent=failed?'Unavailable':loading?'...':duration>0?(elapsed>0||playing?fmt(elapsed)+' / '+fmt(duration):fmt(duration)):fmt(elapsed);\
p.style.width=(duration>0?Math.min(100,elapsed/duration*100):0)+'%';};\
const dur=()=>Number.isFinite(m.duration)?m.duration:0;\
m.addEventListener('loadedmetadata',()=>{ready=true;duration=dur();paint();});\
m.addEventListener('loadeddata',()=>{ready=true;paint();});m.addEventListener('canplay',()=>{ready=true;paint();});\
m.addEventListener('durationchange',()=>{duration=dur();paint();});\
m.addEventListener('timeupdate',()=>{elapsed=m.currentTime;paint();});\
m.addEventListener('play',()=>{playing=true;paint();});\
m.addEventListener('pause',()=>{playing=false;paint();});m.addEventListener('ended',()=>{playing=false;paint();});\
m.addEventListener('error',()=>{if(!m.getAttribute('src'))return;failed=true;playing=false;paint();});\
const retry=()=>{failed=false;ready=false;playing=false;duration=0;elapsed=0;paint();post({type:'retry'});};\
const open=()=>{if(OPEN)post({type:'open'});else retry();};\
a.addEventListener('click',()=>{if(failed){open();return;}if(!m.paused)m.pause();\
else m.play().catch((e)=>{if(!(e&&e.name==='AbortError')){failed=true;playing=false;paint();}});});\
n.addEventListener('click',open);\
window.t3Audio=(u)=>{ready=false;playing=false;duration=0;elapsed=0;failed=u===null;\
if(u===null)m.removeAttribute('src');else{m.src=u;m.load();}paint();};\
window.t3Hidden=()=>m.pause();paint();})();";

/// The colours and type the chip page paints with.
struct ChipLook {
    dark: bool,
    background: String,
    accent: String,
    invalid: String,
    font: String,
    font_size: f32,
    line_height: f32,
}

fn escape_html(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The chip's page: `AudioChip` on the message's background, its `<audio>` hidden.
fn audio_chip_page(src: Option<&str>, name: &str, can_open: bool, look: &ChipLook) -> String {
    let scheme = if look.dark { "dark" } else { "light" };
    let ChipLook { background, accent, invalid, font, font_size, line_height, .. } = look;
    let src = src.map(|src| format!(" src=\"{}\"", escape_html(src))).unwrap_or_default();
    let label = escape_html(&middle_truncate(name, CHIP_NAME_CHARACTERS));
    let icons = json!({"play": PLAY_ICON, "pause": PAUSE_ICON, "load": LOADER_ICON, "alert": ALERT_ICON});
    // `<` escaped, so no name closes the script; the name goes in last, so nothing in it is replaced.
    let literal = |value: String| value.replace('<', r"\u003c");
    let script = CHIP_SCRIPT
        .replace("ICONS", &literal(icons.to_string()))
        .replace("OPEN", if can_open { "true" } else { "false" })
        .replace("NAME", &literal(serde_json::to_string(name).unwrap_or_default()));
    let mut page = String::new();
    let _ = write!(
        page,
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"{scheme}\"><style>\
         {FRAME_CSS}html{{background:{background};font:500 {font_size}px/{line_height}px {font};color:{accent}}}\
         #t3-embed{{display:flex;align-items:center}}\
         .c{{position:relative;isolation:isolate;display:inline-flex;align-items:center;gap:.2em;max-width:100%;\
         border-radius:4px;overflow:hidden;white-space:nowrap}}.c.bad{{color:{invalid}}}\
         .p{{position:absolute;left:0;top:0;bottom:0;z-index:-1;width:0;\
         background:color-mix(in srgb,currentColor 17%,transparent)}}\
         button{{all:unset;display:inline-flex;align-items:center;cursor:pointer;border-radius:2px;min-width:0}}\
         button:disabled{{cursor:default}}button:focus-visible{{outline:2px solid currentColor}}\
         #n{{display:block;max-width:288px;overflow:hidden;text-overflow:ellipsis}}\
         .t{{flex:none;font-size:10px;line-height:14px;align-self:baseline}}\
         svg{{display:block;width:1em;height:1em;flex:none}}@keyframes s{{to{{transform:rotate(360deg)}}}}\
         .spin{{animation:s 1s linear infinite}}</style></head><body><div id=\"t3-embed\">\
         <span class=\"c\" id=\"c\"><span class=\"p\" id=\"p\"></span><button id=\"a\" type=\"button\"></button>\
         <button id=\"n\" type=\"button\">{label}</button><span class=\"t\" id=\"t\" dir=\"ltr\"></span></span>\
         <audio id=\"m\" hidden preload=\"metadata\"{src}></audio></div><script>{FRAME_SCRIPT}{script}</script>\
         </body></html>"
    );
    page
}

/// The width the chip's page needs: its icon, the (truncated) name and the longest time.
fn chip_width(font_size: f32, name_width: f32, time_width: f32) -> f32 {
    let gap = font_size * 0.2;
    font_size + gap + name_width.min(288.) + gap + time_width + 6.
}

/// A media destination in a message, as the plugin parsed it.
#[derive(Clone, Debug)]
struct MediaNode {
    kind: MediaKind,
    destination: String,
    alt: String,
}

/// A chat media by environment, message and the destination's place in it.
type MediaKey = (SharedString, SharedString, usize);

/// What a media plays: a web URL as written, or a host file through a signed URL.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Playable {
    Url(String),
    File {
        thread_id: String,
        path: String,
    },
    /// An `assets.createUrl` resource such as a change request's host media (`github-media`),
    /// played from `fallback` when the URL can't be signed; `fragment` is the link's `#...`.
    Asset {
        resource: Value,
        fallback: String,
        fragment: String,
    },
}

/// What a media's size change re-lays out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// A chat message's markdown.
    Chat,
    /// The open pull request's panel.
    PullRequest,
}

struct ChatMedia {
    kind: MediaKind,
    playable: Playable,
    owner: Owner,
    /// A chat media's pane and thread: it closes once that pane shows another thread.
    shown_in: Option<PaneOwner>,
    /// The page's name and look until it starts loading (see [`Workspace::sync_chat_media`]).
    pending: Option<(String, ChipLook)>,
    embed: Option<EmbedView>,
    failed: bool,
    /// A video's natural size, once it reports one.
    natural: Option<(f32, f32)>,
    dark: bool,
    used: u64,
    _task: Option<Task<()>>,
    _messages: Option<Task<()>>,
}

#[derive(Default)]
pub(super) struct ChatMediaUi {
    media: HashMap<MediaKey, ChatMedia>,
}

/// Draws a media image destination (`![clip](demo.mp4)`) as its player.
pub(super) struct MarkdownMedia {
    pub workspace: WeakEntity<Workspace>,
    /// The message, whose view re-lays out once a video knows its size.
    pub key: SharedString,
    /// What the message is drawn on: the chip's page paints it around the chip.
    pub background: Hsla,
}

impl MarkdownPlugin for MarkdownMedia {
    fn name(&self) -> &'static str {
        "t3-media"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        use gpui_kit::component::text::markdown_ast::Node;
        let (url, alt) = match node {
            Node::Image(image) => (image.url.as_str(), image.alt.clone()),
            // `![clip][v]` plays its definition's destination, as the web's resolved `<img>` does.
            Node::ImageReference(image) => (cx.definition(&image.identifier)?, image.alt.clone()),
            _ => return None,
        };
        let kind = media_kind(url)?;
        // `markdownImageCopy`.
        let copy = format!("![{}]({url})", alt.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]"));
        let media = MediaNode { kind, destination: url.to_owned(), alt: alt.clone() };
        Some(MarkdownNode::new("t3-media", media).text(alt).markdown(copy))
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let media = node.data::<MediaNode>()?.clone();
        let at = node.source_range().map_or(0, |range| range.start);
        let (key, background) = (self.key.clone(), self.background);
        self.workspace
            .update(cx, |workspace, cx| workspace.render_chat_media(key, at, &media, background, context, window, cx))
            .ok()
    }
}

/// `ChatMarkdownImageFallback`: the kind's "unavailable" label with the alt text.
fn unavailable(kind: MediaKind, alt: &str, t: &crate::theme::Tokens) -> InlineElement {
    let label = if alt.is_empty() { kind.unavailable().to_owned() } else { format!("{} · {alt}", kind.unavailable()) };
    InlineElement::new(
        h_flex()
            .flex_none()
            .gap(px(6.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .border_1()
            .border_color(t.border.opacity(0.4))
            .bg(t.muted.opacity(0.4))
            .text_xs()
            .text_color(t.muted_foreground)
            .child(Icon::new(IconName::TriangleAlert).size(px(14.)).flex_none())
            .child(label),
    )
}

fn text_width(text: &str, font: &Font, size: f32, window: &Window) -> f32 {
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    f32::from(window.text_system().shape_line(text.to_owned().into(), px(size), &[run], None).width)
}

/// A file's name for the chip (`InlineAudioPreview`'s `name`).
fn media_name(destination: &str, path: Option<&str>, alt: &str, kind: MediaKind) -> String {
    let source = path.unwrap_or_else(|| destination.split(['?', '#']).next().unwrap_or_default());
    let name = source.rsplit(['/', '\\']).next().unwrap_or_default();
    match (name, alt) {
        ("", "") => match kind {
            MediaKind::Audio => "Audio".into(),
            MediaKind::Video => "video".into(),
        },
        ("", alt) => alt.to_owned(),
        (name, _) => name.to_owned(),
    }
}

/// What a media destination plays (`classifyMarkdownImageSource`): a web or data URL as it is, a
/// host file through a signed URL, nothing for the rest.
fn media_playable(destination: &str, cwd: Option<&str>, thread_id: &str) -> Option<Playable> {
    let source = destination.trim();
    // `classify` decodes data URLs as images; a media one plays from the URL itself.
    if source.get(..5).is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:")) {
        return Some(Playable::Url(source.to_owned()));
    }
    match classify(destination, cwd) {
        Source::Direct(url) => Some(Playable::Url(url)),
        Source::WorkspaceFile(path) => Some(Playable::File { thread_id: thread_id.to_owned(), path }),
        _ => None,
    }
}

impl Workspace {

    #[expect(clippy::too_many_arguments, reason = "the plugin's render context, passed through")]
    fn render_chat_media(
        &mut self,
        message: SharedString,
        at: usize,
        media: &MediaNode,
        background: Hsla,
        context: &InlineRenderContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> InlineElement {
        let t = *crate::theme::tokens(cx);
        // `message` is the view key, which names the pane showing it.
        let Some(owner) = self.pane_owner(super::chat::view_pane(&message).0) else {
            return unavailable(media.kind, &media.alt, &t);
        };
        let Some(playable) = media_playable(&media.destination, self.git_cwd().as_deref(), &owner.thread) else {
            return unavailable(media.kind, &media.alt, &t);
        };
        let key = (owner.env.clone(), message, at);
        let frame = self.embeds.frame();
        let path = match &playable {
            Playable::File { path, .. } => Some(path.clone()),
            Playable::Url(_) | Playable::Asset { .. } => None,
        };
        let name = media_name(&media.destination, path.as_deref(), &media.alt, media.kind);
        let style = context.text_style();
        let font_size = f32::from(context.font_size());
        let line_height = f32::from(context.line_height());
        if self.chat_media.media.get(&key).is_none_or(|existing| existing.playable != playable) {
            let look = ChipLook {
                dark: t.dark,
                background: css_color(background),
                accent: css_color(rgb(CHIP_ACCENT).into()),
                invalid: css_color(t.error_foreground),
                font: format!("\"{}\",system-ui,sans-serif", style.font_family),
                font_size,
                line_height,
            };
            self.start_chat_media(key.clone(), owner, media.kind, playable, name.clone(), look, window, cx);
        }
        let Some(state) = self.chat_media.media.get_mut(&key) else {
            return unavailable(media.kind, &media.alt, &t);
        };
        state.used = frame;
        match media.kind {
            MediaKind::Video => {
                let rem = f32::from(window.rem_size());
                let lane = self.timeline_lane_width.get();
                let max_width = if lane > 0. { (lane - 16.).min(MEDIA_MAX_REM * rem) } else { MEDIA_MAX_REM * rem };
                if state.failed {
                    let url = match &state.playable {
                        Playable::Url(url) => Some(url.clone()),
                        Playable::File { .. } | Playable::Asset { .. } => None,
                    };
                    let failure = self.video_failure(&key, Owner::Chat, &media.alt, url, Some(max_width), &t, cx);
                    return InlineElement::new(failure);
                }
                let (width, height) = video_box(state.natural, max_width, MEDIA_MAX_REM * rem);
                let body = state.embed.as_ref().map(|embed| embed.frame().into_any_element());
                InlineElement::new(div().flex_none().w(px(width)).h(px(height)).children(body))
            }
            MediaKind::Audio => {
                let font = Font { weight: FontWeight::MEDIUM, ..style.font() };
                let shown = middle_truncate(&name, CHIP_NAME_CHARACTERS);
                let width = chip_width(
                    font_size,
                    text_width(&shown, &font, font_size, window),
                    text_width("00:00 / 00:00", &font, 10., window),
                );
                let body = state.embed.as_ref().map(|embed| embed.frame().into_any_element());
                let ascent =
                    f32::from(window.text_system().ascent(window.text_system().resolve_font(&font), px(font_size)));
                let descent =
                    f32::from(window.text_system().descent(window.text_system().resolve_font(&font), px(font_size)));
                let baseline = (line_height - (ascent.abs() + descent.abs())) / 2. + ascent.abs();
                InlineElement::new(div().flex_none().w(px(width)).h(px(line_height)).children(body))
                    .with_baseline(px(baseline))
            }
        }
    }

    /// `MediaVideoPlayer`'s failed state: Retry video, and Open in browser for a web video. It is
    /// `width` wide at 16:9, or fills its column without one.
    #[expect(clippy::too_many_arguments, reason = "the failed video and where it sits")]
    fn video_failure(
        &self,
        key: &MediaKey,
        owner: Owner,
        alt: &str,
        url: Option<String>,
        width: Option<f32>,
        t: &crate::theme::Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = if alt.is_empty() { "Video unavailable".to_owned() } else { format!("Video unavailable · {alt}") };
        let retry = key.clone();
        let id = format!("{}:{}", key.1, key.2);
        let open = url.filter(|url| url.starts_with("http:") || url.starts_with("https:")).map(|url| {
            Button::new(SharedString::from(format!("chat-video-open:{id}")))
                .secondary()
                .small()
                .icon(IconName::ExternalLink)
                .label("Open in browser")
                .on_click(move |_, _, cx| cx.open_url(&url))
        });
        v_flex()
            .flex_none()
            .map(|el| match width {
                Some(width) => el.w(px(width)).h(px(width * 9. / 16.)),
                None => el.w_full().aspect_ratio(16. / 9.),
            })
            .items_center()
            .justify_center()
            .gap(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.border.opacity(0.4))
            .bg(t.muted.opacity(0.4))
            .p(px(16.))
            .text_center()
            .text_sm()
            .text_color(t.muted_foreground)
            .child(
                h_flex().gap(px(6.)).child(Icon::new(IconName::TriangleAlert).size(px(14.)).flex_none()).child(label),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(8.))
                    .child(
                        Button::new(SharedString::from(format!("chat-video-retry:{id}")))
                            .secondary()
                            .small()
                            .icon(IconName::RotateCw)
                            .label("Retry video")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.chat_media.media.remove(&retry);
                                this.relayout_media(&retry, owner, cx);
                            })),
                    )
                    .children(open),
            )
            .into_any_element()
    }

    #[expect(clippy::too_many_arguments, reason = "what the page is built from")]
    fn start_chat_media(
        &mut self,
        key: MediaKey,
        shown_in: PaneOwner,
        kind: MediaKind,
        playable: Playable,
        name: String,
        look: ChipLook,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_chat_media(key.clone(), kind, playable, Owner::Chat, name, look);
        if let Some(state) = self.chat_media.media.get_mut(&key) {
            state.shown_in = Some(shown_in);
        }
        self.load_chat_media(&key, window, cx);
    }

    fn insert_chat_media(
        &mut self,
        key: MediaKey,
        kind: MediaKind,
        playable: Playable,
        owner: Owner,
        name: String,
        look: ChipLook,
    ) {
        let state = ChatMedia {
            kind,
            playable,
            owner,
            shown_in: None,
            dark: look.dark,
            pending: Some((name, look)),
            embed: None,
            failed: false,
            natural: None,
            used: self.embeds.frame(),
            _task: None,
            _messages: None,
        };
        self.chat_media.media.insert(key, state);
    }

    /// The socket and HTTP origin of the environment `key` plays on.
    fn media_connection(&self, key: &MediaKey) -> (Option<crate::rpc::RpcClient>, Option<String>) {
        (self.env_client(&key.0), self.env_endpoint(&key.0).map(|endpoint| endpoint.http_base.clone()))
    }

    /// Starts building `key`'s page, once.
    fn load_chat_media(&mut self, key: &MediaKey, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self.media_connection(key);
        let t = *crate::theme::tokens(cx);
        let Some(state) = self.chat_media.media.get_mut(key) else { return };
        let Some((name, look)) = state.pending.take() else { return };
        let (kind, playable, owner) = (state.kind, state.playable.clone(), state.owner);
        // `MediaVideoPlayer`'s pull request look: a black letterbox inside `border-border/60`.
        let (border, fill) = match owner {
            Owner::Chat => (css_color(t.border.opacity(0.4)), "transparent"),
            Owner::PullRequest => (css_color(t.border.opacity(0.6)), "#000"),
        };
        let request = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let can_open = matches!(playable, Playable::File { .. });
            let url = resolve_playable(playable, connection).await;
            let page = match kind {
                MediaKind::Video => {
                    url.map(|url| super::files::inline_video_page(&url, &look.background, &border, fill, look.dark))
                }
                MediaKind::Audio => Some(audio_chip_page(url.as_deref(), &name, can_open, &look)),
            };
            let _ = this.update_in(cx, |this, window, cx| match page {
                Some(page) => this.build_media_view(request, page, look.dark, window, cx),
                None => this.chat_media_failed(&request, cx),
            });
        });
        state._task = Some(task);
    }

    /// Re-lays out what shows `key`, as its size or state changed.
    fn relayout_media(&mut self, key: &MediaKey, owner: Owner, cx: &mut Context<Self>) {
        match owner {
            Owner::Chat => self.relayout_markdown(&key.1, cx),
            Owner::PullRequest => self.remeasure_pr_panel(cx),
        }
    }

    fn chat_media_failed(&mut self, key: &MediaKey, cx: &mut Context<Self>) {
        let Some(state) = self.chat_media.media.get_mut(key) else { return };
        state.failed = true;
        state.embed = None;
        state._task = None;
        let owner = state.owner;
        self.relayout_media(key, owner, cx);
    }

    fn build_media_view(
        &mut self,
        key: MediaKey,
        page: String,
        dark: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (sender, reports) = async_channel::unbounded::<String>();
        let report_key = key.clone();
        let messages = cx.spawn_in(window, async move |this, cx| {
            while let Ok(body) = reports.recv().await {
                let Some(message) = media_message(&body) else { continue };
                let key = report_key.clone();
                if this.update_in(cx, |this, window, cx| this.on_chat_media_message(key, message, window, cx)).is_err()
                {
                    break;
                }
            }
        });
        let builder = wry::WebViewBuilder::new()
            .with_devtools(true)
            .with_focused(false)
            .with_ipc_handler(move |request| {
                let _ = sender.try_send(request.into_body());
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_html(page);
        let built_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.chat_media.media.contains_key(&built_key) {
                    return;
                }
                match super::preview::build_view(builder, window, cx) {
                    Ok(view) => {
                        let _ = view.set_theme(if dark { wry::Theme::Dark } else { wry::Theme::Light });
                        let embed = this.embeds.adopt(view, Layer::Page, cx);
                        let Some(state) = this.chat_media.media.get_mut(&built_key) else { return };
                        state.embed = Some(embed);
                        state._task = None;
                        let owner = state.owner;
                        this.relayout_media(&built_key, owner, cx);
                    }
                    Err(error) => {
                        log::warn!("chat media: {error}");
                        this.chat_media_failed(&built_key, cx);
                    }
                }
            });
        });
        if let Some(state) = self.chat_media.media.get_mut(&key) {
            state._task = Some(task);
            state._messages = Some(messages);
        }
    }

    fn on_chat_media_message(
        &mut self,
        key: MediaKey,
        message: MediaMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.chat_media.media.get_mut(&key) else { return };
        match message {
            MediaMessage::Ready => {
                if let Some(embed) = &state.embed {
                    embed.retell();
                }
                cx.notify();
            }
            MediaMessage::Size(width, height) => {
                // A pull request's video keeps its 16:9 box, whatever the video's own size.
                if state.natural != Some((width, height)) && state.owner == Owner::Chat {
                    state.natural = Some((width, height));
                    self.relayout_markdown(&key.1, cx);
                }
            }
            MediaMessage::Failed if state.kind == MediaKind::Video => {
                log::warn!("chat media: the video could not play");
                self.chat_media_failed(&key, cx);
            }
            MediaMessage::Failed => {}
            MediaMessage::Open => {
                if let Playable::File { path, .. } = state.playable.clone() {
                    let path = super::markdown_ui::workspace_relative(&path, self.git_cwd().as_deref()).unwrap_or(path);
                    self.open_file(&path, None, window, cx);
                }
            }
            MediaMessage::Retry => {
                let playable = state.playable.clone();
                let connection = self.media_connection(&key);
                let request = key.clone();
                let task = cx.spawn(async move |this, cx| {
                    let url = resolve_playable(playable, connection).await;
                    let _ = this.update(cx, |this, cx| {
                        let Some(embed) = this.chat_media.media.get(&request).and_then(|state| state.embed.as_ref())
                        else {
                            return;
                        };
                        let argument = url.map_or_else(|| "null".to_owned(), |url| json!(url).to_string());
                        let _ = embed.raw(cx).evaluate_script(&format!("window.t3Audio&&t3Audio({argument})"));
                    });
                });
                if let Some(state) = self.chat_media.media.get_mut(&key) {
                    state._task = Some(task);
                }
            }
        }
    }

    /// Before the frame draws: a chat's media close once its pane shows another thread, as
    /// unmounting stops them; media long off screen close, a theme change repaints the pages, and
    /// media a view asked for without a window start loading.
    pub(super) fn sync_chat_media(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let left: Vec<MediaKey> = self
            .chat_media
            .media
            .iter()
            .filter(|(_, state)| state.shown_in.as_ref().is_some_and(|owner| !self.owner_shown(owner)))
            .map(|(key, _)| key.clone())
            .collect();
        for key in left {
            self.chat_media.media.remove(&key);
        }
        let dark = crate::theme::tokens(cx).dark;
        let media = &mut self.chat_media.media;
        let stale: Vec<(MediaKey, Owner)> = media
            .iter()
            .filter(|(_, state)| state.dark != dark)
            .map(|(key, state)| (key.clone(), state.owner))
            .collect();
        media.retain(|_, state| state.dark == dark);
        if media.len() > KEPT_MEDIA {
            let last = self.embeds.frame();
            let mut used: Vec<u64> = media.values().map(|state| state.used).collect();
            used.sort_unstable_by(|a, b| b.cmp(a));
            let oldest_kept = used[KEPT_MEDIA - 1];
            media.retain(|_, state| state.used >= oldest_kept || state.used + 1 >= last);
        }
        let pending: Vec<MediaKey> =
            media.iter().filter(|(_, state)| state.pending.is_some()).map(|(key, _)| key.clone()).collect();
        for key in pending {
            self.load_chat_media(&key, window, cx);
        }
        for (key, owner) in stale {
            self.relayout_media(&key, owner, cx);
        }
    }

    /// `MediaVideoPlayer` filling its column at 16:9 (`PullRequestHostVideo`), in the open pull
    /// request's panel. It loads from the next frame, which starts it with the window.
    pub(super) fn render_pull_request_video(
        &mut self,
        environment: SharedString,
        message: SharedString,
        playable: Playable,
        original_url: &str,
        t: &crate::theme::Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        const LABEL: &str = "Pull request video";
        let key = (environment, message, 0);
        if self.chat_media.media.get(&key).is_none_or(|existing| existing.playable != playable) {
            let look = ChipLook {
                dark: t.dark,
                background: css_color(t.background),
                accent: String::new(),
                invalid: String::new(),
                font: String::new(),
                font_size: 0.,
                line_height: 0.,
            };
            self.insert_chat_media(key.clone(), MediaKind::Video, playable, Owner::PullRequest, LABEL.into(), look);
            cx.notify();
        }
        // A page over the layout holds the panel, so only what covers everything covers its video.
        let layer = if self.page.is_some() { Layer::Dialog } else { Layer::Page };
        let frame = self.embeds.frame();
        let Some(state) = self.chat_media.media.get_mut(&key) else { return div().into_any_element() };
        state.used = frame;
        if state.failed {
            let url = Some(original_url.to_owned());
            return self.video_failure(&key, Owner::PullRequest, LABEL, url, None, t, cx);
        }
        let body = state.embed.as_ref().map(|embed| {
            embed.set_layer(layer);
            embed.frame().into_any_element()
        });
        div()
            .w_full()
            .aspect_ratio(16. / 9.)
            .rounded(px(8.))
            .when(body.is_none(), |el| el.bg(t.muted.opacity(0.6)))
            .children(body)
            .into_any_element()
    }
}

/// The URL a media page plays: a signed one for a host file or asset.
async fn resolve_playable(
    playable: Playable,
    (client, base): (Option<crate::rpc::RpcClient>, Option<String>),
) -> Option<String> {
    let connection = client.zip(base);
    match playable {
        Playable::Url(url) => Some(url),
        Playable::File { thread_id, path } => {
            let (client, base) = connection?;
            let resource = json!({"_tag": "media-file", "threadId": thread_id, "path": path});
            mint_url(client, base, resource).await
        }
        Playable::Asset { resource, fallback, fragment } => {
            let minted = match connection {
                Some((client, base)) => mint_url(client, base, resource).await,
                None => None,
            };
            Some(format!("{}{fragment}", minted.unwrap_or(fallback)))
        }
    }
}

/// `assets.createUrl` for what a media plays.
async fn mint_url(client: crate::rpc::RpcClient, base: String, resource: Value) -> Option<String> {
    match client.request("assets.createUrl", json!({"resource": resource})).await {
        Ok(minted) => Some(format!("{base}{}", minted["relativeUrl"].as_str()?)),
        Err(error) => {
            log::debug!("assets.createUrl for chat media: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_destinations_classify_like_media_kind_from_path() {
        for (destination, kind) in [
            ("demo.mp4", Some(MediaKind::Video)),
            ("https://x.dev/clips/a.WEBM?t=3#x", Some(MediaKind::Video)),
            ("<docs/my clip.mov>", Some(MediaKind::Video)),
            ("https://x.dev/a%20b.mp3", Some(MediaKind::Audio)),
            ("voice.webm-audio", Some(MediaKind::Audio)),
            ("C:\\work\\note.M4A", Some(MediaKind::Audio)),
            ("data:video/mp4;base64,AA", Some(MediaKind::Video)),
            ("data:audio/mpeg,AA", Some(MediaKind::Audio)),
            ("data:image/png;base64,AA", None),
            ("shot.png", None),
            ("https://x.dev/watch?v=a.mp4", None),
            ("README", None),
            ("clip.mp4.txt", None),
        ] {
            assert_eq!(media_kind(destination), kind, "{destination}");
        }
    }

    #[test]
    fn media_image_references_play_their_definitions() {
        use gpui_base::text::editor_host::{BlockNode, parse_markdown};
        use gpui_kit::component::text::MarkdownExtensions;
        let plugin = MarkdownMedia { workspace: WeakEntity::new_invalid(), key: "m".into(), background: Hsla::default() };
        let extensions = std::sync::Arc::new(MarkdownExtensions::default().plugin(plugin));
        let source = "![clip][V] ![shot][p]\n\n[v]: https://example.com/clip.mp4\n[p]: shot.png";
        let blocks = parse_markdown(source, extensions).unwrap();
        let BlockNode::Paragraph(paragraph) = &blocks[0] else { panic!("a paragraph") };
        let media: Vec<_> = paragraph
            .children()
            .iter()
            .filter_map(|node| node.as_custom()?.data::<MediaNode>())
            .map(|media| (media.kind, media.destination.as_str(), media.alt.as_str()))
            .collect();
        assert_eq!(media, [(MediaKind::Video, "https://example.com/clip.mp4", "clip")]);
    }

    #[test]
    fn media_data_urls_play_as_they_are() {
        for url in ["data:audio/wav;base64,UklGRg==", "data:video/mp4;base64,AAAA", "https://x.dev/a.mp4"] {
            assert_eq!(media_playable(url, None, "t"), Some(Playable::Url(url.to_owned())), "{url}");
        }
        assert_eq!(
            media_playable("clip.mp4", Some("/work"), "t"),
            Some(Playable::File { thread_id: "t".into(), path: "/work/clip.mp4".into() })
        );
        assert_eq!(media_playable("javascript:alert(1)", Some("/work"), "t"), None);
    }

    #[test]
    fn videos_fit_their_natural_size_into_the_column() {
        assert_eq!(video_box(None, 480., 480.), (300., 150.), "the browser's default before metadata");
        assert_eq!(video_box(Some((1920., 1080.)), 480., 480.), (480., 270.));
        assert_eq!(video_box(Some((1080., 1920.)), 480., 480.), (270., 480.), "a tall video fits the height");
        assert_eq!(video_box(Some((320., 240.)), 480., 480.), (320., 240.), "never enlarged");
        assert_eq!(video_box(Some((1920., 1080.)), 240., 480.), (240., 135.));
    }

    #[test]
    fn chip_names_truncate_in_the_middle() {
        assert_eq!(middle_truncate("short.mp3", 36), "short.mp3");
        let long = "a-very-long-recording-name-from-the-meeting.mp3";
        let truncated = middle_truncate(long, 36);
        assert_eq!(truncated.chars().count(), 36);
        assert_eq!(truncated, "a-very-long-recording…he-meeting.mp3");
        assert_eq!(media_name("https://x.dev/a/b.mp3?x", None, "", MediaKind::Audio), "b.mp3");
        assert_eq!(media_name("x", Some("/repo/clip.wav"), "alt", MediaKind::Audio), "clip.wav");
        assert_eq!(media_name("https://x.dev/", None, "Intro", MediaKind::Audio), "Intro");
        assert_eq!(media_name("https://x.dev/", None, "", MediaKind::Audio), "Audio");
    }

    #[test]
    fn media_pages_report_their_state() {
        assert_eq!(media_message(r#"{"type":"size","w":1280,"h":720}"#), Some(MediaMessage::Size(1280., 720.)));
        assert_eq!(media_message(r#"{"type":"size","w":0,"h":720}"#), None);
        assert_eq!(media_message(r#"{"type":"error"}"#), Some(MediaMessage::Failed));
        assert_eq!(media_message(r#"{"type":"retry"}"#), Some(MediaMessage::Retry));
        assert_eq!(media_message(r#"{"type":"open"}"#), Some(MediaMessage::Open));
        assert_eq!(media_message("{}"), None);
        let look = ChipLook {
            dark: false,
            background: "#fff".into(),
            accent: "#0096af".into(),
            invalid: "#f00".into(),
            font: "Inter".into(),
            font_size: 14.,
            line_height: 20.,
        };
        let page = audio_chip_page(Some("http://h/a\".mp3"), "a<b>.mp3", false, &look);
        assert!(page.contains("src=\"http://h/a&quot;.mp3\"") && page.contains(">a&lt;b&gt;.mp3</button>"));
        assert!(page.contains("const action=failed?(false?") && page.contains(r#"' '+"a\u003cb>.mp3""#));
        let page = audio_chip_page(None, "OPEN</script>.mp3", true, &look);
        assert!(page.contains(r#""OPEN\u003c/script>.mp3""#) && page.contains("const action=failed?(true?"));
        assert!(!audio_chip_page(None, "a", true, &look).contains(" src="), "no source starts failed");
    }
}
