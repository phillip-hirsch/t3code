//! Mermaid diagrams as images (`MermaidDiagram`). One hidden WebView per window runs the
//! bundled pele renderer and hands back each diagram as a standalone SVG, its colours and fonts
//! written out so GPUI's SVG renderer draws it as the browser would. Results are cached by
//! source and theme, so the chat and the file panel never hold a WebView per diagram. The
//! same page typesets TeX formulas (`super::math`).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, Hsla, Image, ImageFormat, InteractiveElement as _, IntoElement as _,
    ParentElement as _, SharedString, Size, StatefulInteractiveElement as _, Styled as _, Task, WeakEntity, Window,
    div, img, px, relative, size,
};
use gpui_webview::WebView;
use serde_json::{Value, json};

use super::Workspace;
use super::image_viewer::ViewerImage;
use super::message_actions;
use crate::theme::Tokens;

/// `pele` 1.8.0's global build (MIT, `assets/vendor/pele-LICENSE`), the web's Mermaid renderer.
const PELE_GZ: &[u8] = include_bytes!("../../assets/vendor/pele-1.8.0.global.js.gz");
const SCHEME: &str = "t3mermaid";
/// wry serves a custom protocol at `http://<name>.localhost` on Windows and `<name>://localhost` elsewhere.
#[cfg(windows)]
const PAGE_URL: &str = "http://t3mermaid.localhost/";
#[cfg(not(windows))]
const PAGE_URL: &str = "t3mermaid://localhost/";

/// Diagrams kept: an unbounded session would otherwise keep every theme of every diagram.
const CACHE_LIMIT: usize = 128;
/// A page that has not said it is ready by then counts as failed, so diagrams never wait forever.
const LOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// The page may run only its own scripts and load nothing else.
const PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" \
content=\"default-src 'none'; script-src 'self'; style-src 'unsafe-inline'\"></head>\
<body><div id=\"d\" class=\"pele\"></div><script src=\"/pele.js\"></script><script src=\"/render.js\"></script>\
<script src=\"/math.js\"></script></body></html>";

/// `window.t3Render(id, source, colours, refuse)`: renders with links and images off, removes
/// what the web's sanitizer forbids (links, images, `foreignObject`, styles, `href`s, handlers),
/// then writes each element's computed paint and font as attributes, since an SVG drawn outside
/// the page cannot resolve pele's CSS variables. `refuse` is `MermaidViewer`'s check for sources
/// that could load resources. Fonts are families both the WebView and GPUI find installed.
const RENDER_SCRIPT: &str = r##""use strict";
(() => {
  const post = (message) => window.ipc.postMessage(JSON.stringify(message));
  if (typeof pele === "undefined") { post({ type: "unavailable" }); return; }
  const host = document.getElementById("d");
  const REFUSE = /%%\s*\{|^\s*---|<\/?[A-Za-z]|<[!?]|@\s*\{|\b(?:img|image)\s*:|url\s*\(|@import|(?:https?|data|blob|file):/im;
  const FONT = '"Segoe UI", "Helvetica Neue", "Noto Sans", "DejaVu Sans", "Liberation Sans", Arial, sans-serif';
  const MONO = 'Consolas, Menlo, "DejaVu Sans Mono", "Liberation Mono", monospace';
  const INHERITED = ["fill", "fill-opacity", "fill-rule", "stroke", "stroke-width", "stroke-dasharray",
    "stroke-dashoffset", "stroke-linecap", "stroke-linejoin", "stroke-miterlimit", "stroke-opacity", "font-family",
    "font-size", "font-weight", "font-style", "text-anchor", "dominant-baseline", "letter-spacing", "visibility",
    "marker-start", "marker-mid", "marker-end", "paint-order", "color"];
  const OWN = { opacity: "1", display: "inline", rx: "auto", ry: "auto", "stop-color": "rgb(0, 0, 0)",
    "stop-opacity": "1", "clip-path": "none", mask: "none", filter: "none" };
  const COLORS = new Set(["fill", "stroke", "color", "stop-color"]);
  const probe = document.createElement("canvas").getContext("2d", { willReadFrequently: true });
  const known = new Map();
  // Computed colours can be `color(srgb ...)` (from `color-mix`), which SVG renderers don't read.
  const rgba = (value) => {
    if (!probe || /^(?:none|transparent|currentcolor|rgba?\(|#)/i.test(value)) return value;
    let color = known.get(value);
    if (color === undefined) {
      probe.clearRect(0, 0, 1, 1);
      probe.fillStyle = "#000";
      probe.fillStyle = value;
      probe.fillRect(0, 0, 1, 1);
      const [r, g, b, a] = probe.getImageData(0, 0, 1, 1).data;
      color = `rgba(${r},${g},${b},${(a / 255).toFixed(3)})`;
      known.set(value, color);
    }
    return color;
  };
  const read = (style, name) => {
    const raw = style.getPropertyValue(name).trim();
    if (raw.includes("url(")) return raw.replace(/url\(\s*["']?[^"')#]*(#[^"')]*)["']?\s*\)/g, "url($1)");
    return raw && COLORS.has(name) ? rgba(raw) : raw;
  };
  const clean = (root) => {
    for (const link of root.querySelectorAll("a")) link.replaceWith(...link.childNodes);
    for (const element of root.querySelectorAll("image, foreignObject, style, script, use")) element.remove();
    for (const element of [root, ...root.querySelectorAll("*")]) {
      for (const { name } of [...element.attributes]) {
        const lower = name.toLowerCase();
        if (lower.startsWith("on") || ["href", "xlink:href", "src", "srcset"].includes(lower)) {
          element.removeAttribute(name);
        }
      }
    }
  };
  const inline = (svg) => {
    const elements = [svg, ...svg.querySelectorAll("*")];
    const values = new Map();
    for (const element of elements) {
      const style = getComputedStyle(element);
      const own = {};
      for (const name of INHERITED) own[name] = read(style, name);
      for (const name of Object.keys(OWN)) own[name] = read(style, name);
      values.set(element, own);
    }
    const writes = elements.map((element) => {
      const own = values.get(element);
      const parent = values.get(element.parentElement);
      const set = [];
      for (const name of INHERITED) {
        if (!parent || own[name] !== parent[name] || element.hasAttribute(name)) set.push([name, own[name]]);
      }
      for (const [name, initial] of Object.entries(OWN)) {
        if (own[name] !== initial || element.hasAttribute(name)) set.push([name, own[name]]);
      }
      return set;
    });
    elements.forEach((element, index) => {
      element.removeAttribute("style");
      element.removeAttribute("class");
      for (const [name, value] of writes[index]) if (value) element.setAttribute(name, value);
      for (const { name, value } of [...element.attributes]) if (value.includes("var(")) element.removeAttribute(name);
    });
  };
  window.t3Render = (id, source, colors, refuse) => {
    try {
      if (refuse && REFUSE.test(source)) { post({ type: "refused", id }); return; }
      for (const [name, value] of Object.entries(colors)) host.style.setProperty(name, value);
      const result = pele.render(source.trim(), { links: false, images: false, fontFamily: FONT, fontFamilyMono: MONO });
      const parsed = new DOMParser().parseFromString(result.svg, "image/svg+xml").documentElement;
      if (parsed.localName !== "svg") throw new Error("The diagram could not be rendered.");
      clean(parsed);
      const svg = document.importNode(parsed, true);
      host.replaceChildren(svg);
      inline(svg);
      const width = Number(svg.getAttribute("width")) || 0;
      const height = Number(svg.getAttribute("height")) || 0;
      post({ type: "rendered", id, svg: new XMLSerializer().serializeToString(svg), width, height });
    } catch (error) {
      post({ type: "failed", id, message: (error && error.message) || "The diagram could not be rendered." });
    } finally {
      host.replaceChildren();
    }
  };
  post({ type: "ready" });
})();
"##;

const RENDER_FAILED: &str = "The diagram could not be rendered.";
const RENDERER_FAILED: &str = "The diagram renderer failed to load.";
/// `MermaidViewer`'s refusal.
pub(super) const REFUSAL: &str = "This diagram contains resource URLs, markup, node metadata, or custom configuration. \
Show Mermaid source to inspect it safely.";

fn pele_script() -> &'static [u8] {
    static SCRIPT: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    SCRIPT.get_or_init(|| {
        let mut out = Vec::new();
        let _ = std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(PELE_GZ), &mut out);
        out
    })
}

/// A colour as CSS: `#rrggbb`, or `rgba(...)` when translucent like many dark tokens.
pub(super) fn css_color(color: Hsla) -> String {
    let rgba = gpui_kit::Rgba::from(color);
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255 before the cast")]
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    let (r, g, b) = (byte(rgba.r), byte(rgba.g), byte(rgba.b));
    if rgba.a < 1. { format!("rgba({r},{g},{b},{:.3})", rgba.a) } else { format!("#{r:02x}{g:02x}{b:02x}") }
}

/// The colours pele reads (`.pele` in the web's CSS), as the page's custom properties.
fn palette(t: &Tokens) -> SharedString {
    json!({
        "--pele-bg": css_color(t.background),
        "--pele-fg": css_color(t.foreground),
        "--pele-muted": css_color(t.muted_foreground),
        "--pele-line": css_color(t.muted_foreground),
        "--pele-surface": css_color(t.secondary),
        "--pele-surface-alt": css_color(t.accent),
        // `color-mix(in srgb, var(--foreground) 28%, transparent)`.
        "--pele-border": css_color(t.foreground.opacity(0.28)),
        "--pele-accent": css_color(t.primary),
        "--pele-critical": css_color(t.error),
    })
    .to_string()
    .into()
}

/// The cache key of a diagram: its source in a theme, refused or not.
pub(super) fn diagram_key(source: &str, palette: &str, refuse: bool) -> u64 {
    let mut hasher = DefaultHasher::new();
    (source, palette, refuse).hash(&mut hasher);
    hasher.finish()
}

fn render_call(id: u64, source: &str, palette: &str, refuse: bool) -> String {
    let source = serde_json::to_string(source).unwrap_or_default();
    format!("window.t3Render(\"{id:x}\", {source}, {palette}, {refuse})")
}

/// What the page reports through `window.ipc`.
#[derive(Debug, PartialEq)]
enum Message {
    Ready,
    /// The renderer script did not load.
    Unavailable,
    Rendered {
        id: u64,
        svg: String,
        size: Size<f32>,
    },
    Failed {
        id: u64,
        message: String,
    },
    Refused {
        id: u64,
    },
    /// A formula typeset by `window.t3Math`.
    Typeset {
        id: u64,
        svg: String,
        metrics: super::math::Metrics,
    },
    /// A formula that does not parse, or the typesetter that did not load.
    Untypeset {
        id: u64,
    },
}

fn message(body: &str) -> Option<Message> {
    let value: Value = serde_json::from_str(body).ok()?;
    let id = || value["id"].as_str().and_then(|id| u64::from_str_radix(id, 16).ok());
    #[expect(clippy::cast_possible_truncation, reason = "diagram sides are small")]
    let side = |name: &str| value[name].as_f64().map(|v| v as f32).filter(|v| v.is_finite() && *v > 0.);
    match value["type"].as_str()? {
        "ready" => Some(Message::Ready),
        "unavailable" => Some(Message::Unavailable),
        "rendered" => {
            let id = id()?;
            match (value["svg"].as_str(), side("width"), side("height")) {
                (Some(svg), Some(width), Some(height)) => {
                    Some(Message::Rendered { id, svg: svg.to_owned(), size: size(width, height) })
                }
                _ => Some(Message::Failed { id, message: RENDER_FAILED.to_owned() }),
            }
        }
        "failed" => Some(Message::Failed {
            id: id()?,
            message: value["message"].as_str().filter(|m| !m.is_empty()).unwrap_or(RENDER_FAILED).to_owned(),
        }),
        "refused" => Some(Message::Refused { id: id()? }),
        "typeset" => {
            let id = id()?;
            #[expect(clippy::cast_possible_truncation, reason = "formula lengths are a few em")]
            let ascent = value["ascent"].as_f64().map(|v| v as f32).filter(|v| v.is_finite());
            match (value["svg"].as_str(), side("width"), side("height"), ascent) {
                (Some(svg), Some(width), Some(height), Some(ascent)) => Some(Message::Typeset {
                    id,
                    svg: svg.to_owned(),
                    metrics: super::math::Metrics { width, height, ascent },
                }),
                _ => Some(Message::Untypeset { id }),
            }
        }
        "untypeset" => Some(Message::Untypeset { id: id()? }),
        _ => None,
    }
}

/// `isClosedCodeFence`: whether `source`, a fence as written, ends with a real closer. `code` is
/// the fence's content as parsed, without the newline the web's code text ends with.
pub(super) fn fence_closed(source: &str, code: &str) -> bool {
    let fence = |text: &str| {
        let marker = text.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let length = text.chars().take_while(|&c| c == marker).count();
        (length >= 3).then_some((marker, length))
    };
    let Some((marker, opening)) = fence(source) else { return false };
    let last = source.rsplit('\n').next().unwrap_or_default().trim_start_matches([' ', '\t', '>']);
    let Some((closer, closing)) = fence(last) else { return false };
    let rest = &last[closing * closer.len_utf8()..];
    let lines = |text: &str| {
        let bytes = text.as_bytes();
        1 + bytes
            .iter()
            .enumerate()
            .filter(|&(i, &b)| b == b'\n' || (b == b'\r' && bytes.get(i + 1) != Some(&b'\n')))
            .count()
    };
    let code_lines = if code.is_empty() { 1 } else { lines(code) + 1 };
    closer == marker
        && closing >= opening
        && rest.chars().all(|c| matches!(c, ' ' | '\t' | '\r'))
        && lines(source.strip_suffix('\r').unwrap_or(source)) > code_lines
}

/// The SVG with a backdrop of `color`, as the web's expanded diagram paints the page's
/// background behind it.
fn with_background(svg: &str, color: &str) -> String {
    // The serializer escapes `>` inside attributes, so the first one closes the tag.
    let tag_end = svg.find("<svg").and_then(|open| svg[open..].find('>').map(|i| open + i));
    match tag_end {
        Some(end) if !svg[..end].ends_with('/') => format!(
            "{}<rect x=\"0\" y=\"0\" width=\"100%\" height=\"100%\" fill=\"{color}\"/>{}",
            &svg[..=end],
            &svg[end + 1..]
        ),
        _ => svg.to_owned(),
    }
}

/// A rendered diagram.
#[derive(Clone)]
pub(super) struct Rendered {
    pub image: Arc<Image>,
    pub size: Size<f32>,
}

#[derive(Clone)]
enum Diagram {
    Pending,
    Ready(Rendered),
    Failed(SharedString),
    Refused,
}

/// What a diagram shows now.
pub(super) enum Shown {
    Loading,
    Ready(Rendered),
    Failed(SharedString),
    Refused,
    /// The renderer itself failed: Retry starts it again.
    RendererFailed,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Phase {
    #[default]
    Idle,
    Starting,
    Loaded,
    Failed,
}

#[derive(Default)]
struct Renderer {
    phase: Phase,
    view: Option<WebView>,
    /// Render calls waiting for the page to load, by diagram.
    queue: Vec<(u64, String)>,
    _messages: Option<Task<()>>,
    /// Building the view, then waiting out [`LOAD_TIMEOUT`]; dropped with the renderer.
    _start: Option<Task<()>>,
}

/// Rendered diagrams by [`diagram_key`], the renderer, and which chat blocks show their source.
#[derive(Default)]
pub(super) struct Diagrams {
    cache: HashMap<u64, Diagram>,
    renderer: Renderer,
    /// The markdown views drawing a pending diagram, which re-lay out once it lands.
    waiting: HashMap<u64, Vec<SharedString>>,
    /// Mermaid fences toggled to their source, by message and the block's place in it.
    sources: HashSet<(SharedString, usize)>,
    palette: Option<([Hsla; 7], SharedString)>,
    /// The file panel's diagram source, by its key.
    file: Option<(u64, SharedString)>,
}

impl Diagrams {
    /// The theme's colours for pele, built again only when the theme changes.
    pub(super) fn palette(&mut self, t: &Tokens) -> SharedString {
        let colors = [t.background, t.foreground, t.muted_foreground, t.secondary, t.accent, t.primary, t.error];
        match &self.palette {
            Some((cached, palette)) if *cached == colors => palette.clone(),
            _ => {
                let palette = palette(t);
                self.palette = Some((colors, palette.clone()));
                palette
            }
        }
    }

    pub(super) fn shown(&self, id: u64) -> Shown {
        match self.cache.get(&id) {
            Some(Diagram::Ready(rendered)) => Shown::Ready(rendered.clone()),
            Some(Diagram::Failed(message)) => Shown::Failed(message.clone()),
            Some(Diagram::Refused) => Shown::Refused,
            Some(Diagram::Pending) | None if self.renderer.phase == Phase::Failed => Shown::RendererFailed,
            Some(Diagram::Pending) | None => Shown::Loading,
        }
    }

    pub(super) fn shows_source(&self, key: &SharedString, at: usize) -> bool {
        self.sources.contains(&(key.clone(), at))
    }

    /// The file panel's diagram source for `id`, when it is the one recorded.
    pub(super) fn file_source(&self, id: u64) -> Option<SharedString> {
        self.file.as_ref().filter(|(file, _)| *file == id).map(|(_, source)| source.clone())
    }

    fn settle(&mut self, id: u64, diagram: Diagram) -> Vec<SharedString> {
        if let Some(entry) = self.cache.get_mut(&id).filter(|entry| matches!(entry, Diagram::Pending)) {
            *entry = diagram;
        }
        self.waiting.remove(&id).unwrap_or_default()
    }
}

impl Workspace {
    /// Queues `source`'s render under `id` unless it is cached or on its way.
    pub(super) fn request_diagram(
        &mut self,
        id: u64,
        source: &str,
        palette: &str,
        refuse: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let diagrams = &mut self.diagrams;
        if diagrams.cache.contains_key(&id) {
            return;
        }
        if diagrams.cache.len() >= CACHE_LIMIT {
            diagrams.cache.retain(|_, diagram| matches!(diagram, Diagram::Pending));
        }
        diagrams.cache.insert(id, Diagram::Pending);
        self.render_in_page(id, render_call(id, source, palette, refuse), window, cx);
    }

    /// Runs `call`, which answers for `id`, in the renderer page, starting the page if needed.
    pub(super) fn render_in_page(&mut self, id: u64, call: String, window: &mut Window, cx: &mut Context<Self>) {
        let renderer = &mut self.diagrams.renderer;
        match renderer.phase {
            Phase::Loaded => self.evaluate_render(id, &call, cx),
            Phase::Failed => {}
            Phase::Starting => renderer.queue.push((id, call)),
            Phase::Idle => {
                renderer.queue.push((id, call));
                self.start_diagram_renderer(window, cx);
            }
        }
    }

    fn evaluate_render(&mut self, id: u64, call: &str, cx: &mut Context<Self>) {
        let Some(view) = &self.diagrams.renderer.view else { return };
        if let Err(error) = view.raw().evaluate_script(call) {
            log::warn!("mermaid: {error}");
            let mut keys = self.diagrams.settle(id, Diagram::Failed(RENDER_FAILED.into()));
            keys.extend(self.formulas.settle(id, None));
            self.diagram_landed(&keys, cx);
        }
    }

    /// Builds the hidden renderer page, outside of a draw.
    fn start_diagram_renderer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.diagrams.renderer.phase = Phase::Starting;
        let (sender, messages) = async_channel::unbounded::<String>();
        let builder = wry::WebViewBuilder::new()
            .with_custom_protocol(SCHEME.into(), |_, request| {
                let (body, content_type): (&'static [u8], &str) = match request.uri().path() {
                    "/pele.js" => (pele_script(), "text/javascript"),
                    "/render.js" => (RENDER_SCRIPT.as_bytes(), "text/javascript"),
                    "/math.js" => (super::math::MATH_SCRIPT.as_bytes(), "text/javascript"),
                    "/mathjax.js" => (super::math::mathjax_script(), "text/javascript"),
                    _ => (PAGE.as_bytes(), "text/html"),
                };
                wry::http::Response::builder()
                    .header("content-type", content_type)
                    .body(Cow::Borrowed(body))
                    .unwrap_or_default()
            })
            .with_ipc_handler(move |request| {
                let _ = sender.try_send(request.into_body());
            })
            .with_navigation_handler(|url| url.starts_with(PAGE_URL))
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_url(PAGE_URL);
        self.diagrams.renderer._messages = Some(cx.spawn(async move |this, cx| {
            while let Ok(body) = messages.recv().await {
                let Some(message) = message(&body) else { continue };
                if this.update(cx, |this, cx| this.on_diagram_message(message, cx)).is_err() {
                    break;
                }
            }
        }));
        self.diagrams.renderer._start = Some(cx.spawn_in(window, async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| match super::preview::build_view(builder, window, cx) {
                Ok(view) => {
                    let _ = view.set_visible(false);
                    this.diagrams.renderer.view = Some(view);
                }
                Err(error) => {
                    log::warn!("mermaid: {error}");
                    this.fail_diagram_renderer(cx);
                }
            });
            cx.background_executor().timer(LOAD_TIMEOUT).await;
            let _ = this.update(cx, |this, cx| {
                if this.diagrams.renderer.phase == Phase::Starting {
                    log::warn!("mermaid: the renderer page did not load");
                    this.fail_diagram_renderer(cx);
                }
            });
        }));
    }

    fn fail_diagram_renderer(&mut self, cx: &mut Context<Self>) {
        let renderer = &mut self.diagrams.renderer;
        renderer.phase = Phase::Failed;
        renderer.view = None;
        renderer.queue.clear();
        // Formulas keep showing their source, as the web's do when KaTeX fails to load.
        self.formulas.abandon();
        let keys: Vec<SharedString> = self.diagrams.waiting.drain().flat_map(|(_, keys)| keys).collect();
        self.diagram_landed(&keys, cx);
    }

    fn on_diagram_message(&mut self, message: Message, cx: &mut Context<Self>) {
        let (id, diagram) = match message {
            Message::Ready => {
                self.diagrams.renderer.phase = Phase::Loaded;
                for (id, call) in std::mem::take(&mut self.diagrams.renderer.queue) {
                    self.evaluate_render(id, &call, cx);
                }
                return;
            }
            Message::Unavailable => {
                log::warn!("mermaid: the renderer script did not load");
                self.fail_diagram_renderer(cx);
                return;
            }
            Message::Rendered { id, svg, size } => {
                let image = Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()));
                (id, Diagram::Ready(Rendered { image, size }))
            }
            Message::Failed { id, message } => (id, Diagram::Failed(message.into())),
            Message::Refused { id } => (id, Diagram::Refused),
            Message::Typeset { id, svg, metrics } => {
                let keys = self.formulas.settle(id, Some(super::math::Typeset::new(svg, metrics)));
                self.diagram_landed(&keys, cx);
                return;
            }
            Message::Untypeset { id } => {
                let keys = self.formulas.settle(id, None);
                self.diagram_landed(&keys, cx);
                return;
            }
        };
        let keys = self.diagrams.settle(id, diagram);
        self.diagram_landed(&keys, cx);
    }

    /// A diagram changed what it draws: the views showing it re-lay out.
    fn diagram_landed(&mut self, keys: &[SharedString], cx: &mut Context<Self>) {
        for key in keys {
            self.relayout_markdown(key, cx);
        }
        cx.notify();
    }

    /// Retry after the renderer failed to load: a fresh page renders every waiting diagram.
    pub(super) fn retry_diagrams(&mut self, cx: &mut Context<Self>) {
        self.diagrams.renderer = Renderer::default();
        self.diagrams.cache.retain(|_, diagram| !matches!(diagram, Diagram::Pending));
        self.formulas.abandon();
        cx.notify();
    }

    /// Expands a rendered diagram into the image viewer, on the page's background.
    pub(super) fn expand_diagram(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Shown::Ready(rendered) = self.diagrams.shown(id) else { return };
        let background = css_color(crate::theme::tokens(cx).background);
        let svg = String::from_utf8_lossy(&rendered.image.bytes);
        let image = Arc::new(Image::from_bytes(ImageFormat::Svg, with_background(&svg, &background).into_bytes()));
        let item = ViewerImage { image, name: "Mermaid diagram".into(), reference: None };
        self.open_image_viewer(vec![item], 0, window, cx);
    }

    fn toggle_diagram_source(&mut self, key: SharedString, at: usize, cx: &mut Context<Self>) {
        let block = (key.clone(), at);
        if !self.diagrams.sources.remove(&block) {
            self.diagrams.sources.insert(block);
        }
        self.relayout_markdown(&key, cx);
        cx.notify();
    }

    /// The file panel's diagram (`MermaidViewer`); `source` is given when `id` is new.
    pub(super) fn file_diagram(
        &mut self,
        id: u64,
        source: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Shown, SharedString) {
        if let Some(source) = source {
            let palette = self.diagrams.palette(crate::theme::tokens(cx));
            self.request_diagram(id, &source, &palette, true, window, cx);
            self.diagrams.file = Some((id, source.into()));
        }
        (self.diagrams.shown(id), self.diagrams.file_source(id).unwrap_or_default())
    }
}

/// `MarkdownMermaidCodeBlock`'s view toggle: "Show code" on the diagram, "Show diagram" on the source.
pub(super) fn view_toggle(
    this: &WeakEntity<Workspace>,
    key: &SharedString,
    at: usize,
    showing_code: bool,
    t: &Tokens,
) -> AnyElement {
    let label = if showing_code { "Show diagram" } else { "Show code" };
    let (this, key) = (this.clone(), key.clone());
    div()
        .id("view-toggle")
        .tab_index(0)
        .size(px(24.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .text_color(t.foreground.opacity(0.72))
        .hover(|style| style.bg(t.accent).text_color(t.foreground))
        .focus_visible(|style| style.bg(t.accent).text_color(t.foreground))
        .child(Icon::new(if showing_code { IconName::Workflow } else { IconName::Code }).size(px(12.)))
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        .on_click(move |_, _, cx| {
            let _ = this.update(cx, |workspace, cx| workspace.toggle_diagram_source(key.clone(), at, cx));
        })
        .into_any_element()
}

/// A rendered diagram, fitted to its column and centered; a click, Enter or Space expands it.
pub(super) fn diagram_button(this: &WeakEntity<Workspace>, id: u64, rendered: &Rendered, t: &Tokens) -> AnyElement {
    let Size { width, height } = rendered.size;
    let (this, ring) = (this.clone(), t.ring);
    div()
        .id("diagram")
        .tab_index(0)
        .w_full()
        .rounded(px(6.))
        .border_2()
        .border_color(gpui_kit::transparent_black())
        .cursor_pointer()
        .focus_visible(move |style| style.border_color(ring))
        .on_click(move |_, window, cx| {
            let _ = this.update(cx, |workspace, cx| workspace.expand_diagram(id, window, cx));
        })
        .child(v_flex().w_full().items_center().child(
            div().w(px(width)).max_w_full().aspect_ratio(width / height).child(img(rendered.image.clone()).size_full()),
        ))
        .into_any_element()
}

/// `DiagramError`: the message, Retry when the renderer itself failed, and the source.
pub(super) fn diagram_error(
    this: &WeakEntity<Workspace>,
    message: &str,
    source: SharedString,
    retry: bool,
    t: &Tokens,
) -> AnyElement {
    let retry = retry.then(|| {
        let this = this.clone();
        Button::new("diagram-retry").ghost().xsmall().label("Retry").on_click(move |_, _, cx| {
            let _ = this.update(cx, Workspace::retry_diagrams);
        })
    });
    v_flex()
        .w_full()
        .child(
            h_flex()
                .justify_between()
                .items_center()
                .gap(px(8.))
                .child(div().text_xs().text_color(t.error).child(format!("Unable to render diagram: {message}")))
                .children(retry),
        )
        .child(div().mt(px(8.)).font_family(t.fonts.mono).text_xs().child(source))
        .into_any_element()
}

/// What a shown diagram draws, failures included; `None` while it loads.
pub(super) fn diagram_body(
    this: &WeakEntity<Workspace>,
    id: u64,
    shown: &Shown,
    source: &SharedString,
    t: &Tokens,
) -> Option<AnyElement> {
    Some(match shown {
        Shown::Ready(rendered) => diagram_button(this, id, rendered, t),
        Shown::Failed(message) => diagram_error(this, message, source.clone(), false, t),
        Shown::RendererFailed => diagram_error(this, RENDERER_FAILED, source.clone(), true, t),
        Shown::Refused => div().text_xs().text_color(t.muted_foreground).child(REFUSAL).into_any_element(),
        Shown::Loading => return None,
    })
}

/// A chat Mermaid fence as its diagram (`MarkdownMermaidCodeBlock` with `diagram`): the view
/// toggle and copy over the diagram, which expands on a click. `None` keeps the code block,
/// as when the block is toggled to its source.
pub(super) fn chat_diagram(
    this: &WeakEntity<Workspace>,
    key: &SharedString,
    at: usize,
    code: SharedString,
    t: &Tokens,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let (id, shown) = this
        .update(cx, |workspace, cx| {
            if workspace.diagrams.shows_source(key, at) {
                return None;
            }
            let palette = workspace.diagrams.palette(t);
            let id = diagram_key(&code, &palette, false);
            workspace.request_diagram(id, &code, &palette, false, window, cx);
            let shown = workspace.diagrams.shown(id);
            if matches!(shown, Shown::Loading) {
                let waiting = workspace.diagrams.waiting.entry(id).or_default();
                if !waiting.contains(key) {
                    waiting.push(key.clone());
                }
            }
            Some((id, shown))
        })
        .ok()
        .flatten()?;
    let copy = message_actions::code_copy_button(this, format!("code:{key}:{at:x}").into(), code.clone(), t, cx);
    // Suspense's fallback: the source, invisible, holds the block's height.
    let body = diagram_body(this, id, &shown, &code, t).unwrap_or_else(|| {
        div()
            .invisible()
            .font_family(t.fonts.mono)
            .text_size(px(t.fonts.code_size))
            .line_height(relative(1.375))
            .child(code)
            .into_any_element()
    });
    Some(
        v_flex()
            .id(ElementId::Name(format!("mermaid:{key}:{at:x}").into()))
            .w_full()
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .child(
                h_flex()
                    .justify_end()
                    .items_center()
                    .gap(px(2.))
                    .pt(px(6.))
                    .pr(px(6.))
                    .child(view_toggle(this, key, at, false, t))
                    .child(copy),
            )
            .child(body)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_close_only_with_a_real_closer() {
        assert!(fence_closed("```mermaid\ngraph TD\n```", "graph TD"));
        assert!(fence_closed("```mermaid\n```", ""), "an empty closed fence");
        assert!(fence_closed("~~~~mermaid\nA\n  > ~~~~~ \r", "A"), "a longer closer, quoted and padded");
        assert!(!fence_closed("```mermaid\ngraph TD\n", "graph TD"), "still streaming");
        assert!(!fence_closed("```mermaid\ngraph TD", "graph TD"));
        assert!(!fence_closed("````mermaid\nA\n```", "A\n```"), "a shorter closer is code");
        assert!(!fence_closed("```mermaid\nA\n~~~", "A\n~~~"), "a different marker is code");
        assert!(!fence_closed("```mermaid\nA\n``` x", "A\n``` x"), "a closer takes no info string");
        assert!(!fence_closed("```mermaid\nA\n    ```", "A\n    ```"), "an indented closer that parsed as code");
        assert!(!fence_closed("    code", "code"), "not a fence");
        assert!(fence_closed("```\r\nA\r\n```", "A"), "CRLF lines count once");
    }

    #[test]
    fn page_messages_parse() {
        assert_eq!(message(r#"{"type":"ready"}"#), Some(Message::Ready));
        assert_eq!(message(r#"{"type":"unavailable"}"#), Some(Message::Unavailable));
        assert_eq!(
            message(r#"{"type":"rendered","id":"ff","svg":"<svg/>","width":40,"height":20.5}"#),
            Some(Message::Rendered { id: 255, svg: "<svg/>".into(), size: size(40., 20.5) })
        );
        assert_eq!(
            message(r#"{"type":"rendered","id":"1","svg":"<svg/>","width":0,"height":20}"#),
            Some(Message::Failed { id: 1, message: RENDER_FAILED.into() }),
            "an empty diagram cannot be drawn"
        );
        assert_eq!(
            message(r#"{"type":"failed","id":"a","message":"Parse error on line 2"}"#),
            Some(Message::Failed { id: 10, message: "Parse error on line 2".into() })
        );
        assert_eq!(
            message(r#"{"type":"failed","id":"a"}"#),
            Some(Message::Failed { id: 10, message: RENDER_FAILED.into() })
        );
        assert_eq!(message(r#"{"type":"refused","id":"2"}"#), Some(Message::Refused { id: 2 }));
        assert_eq!(
            message(r#"{"type":"typeset","id":"b","svg":"<svg/>","width":1.5,"height":1.2,"ascent":-0.1}"#),
            Some(Message::Typeset {
                id: 11,
                svg: "<svg/>".into(),
                metrics: crate::app::math::Metrics { width: 1.5, height: 1.2, ascent: -0.1 },
            }),
            "a formula can sit wholly below the baseline"
        );
        assert_eq!(
            message(r#"{"type":"typeset","id":"b","svg":"<svg/>","width":1.5,"height":1.2}"#),
            Some(Message::Untypeset { id: 11 }),
            "a formula without a baseline cannot be placed"
        );
        assert_eq!(message(r#"{"type":"untypeset","id":"c"}"#), Some(Message::Untypeset { id: 12 }));
        assert_eq!(message(r#"{"type":"rendered","id":42}"#), None, "ids travel as hex strings");
        assert_eq!(message("nope"), None);
    }

    #[test]
    fn render_calls_carry_the_source_as_a_string() {
        let id = diagram_key("graph TD", "{}", false);
        assert_ne!(id, diagram_key("graph TD", "{}", true), "a refusal check is its own result");
        assert_ne!(id, diagram_key("graph TD", "{\"x\":1}", false), "each theme renders apart");
        assert_eq!(
            render_call(0xab, "A\"</script>\nB", "{\"--pele-bg\":\"#000\"}", true),
            r##"window.t3Render("ab", "A\"</script>\nB", {"--pele-bg":"#000"}, true)"##
        );
    }

    #[test]
    fn the_expanded_diagram_gets_a_backdrop() {
        assert_eq!(
            with_background(r#"<svg width="4" height="2"><g/></svg>"#, "#123"),
            r##"<svg width="4" height="2"><rect x="0" y="0" width="100%" height="100%" fill="#123"/><g/></svg>"##
        );
        assert_eq!(with_background("<svg/>", "#123"), "<svg/>", "nothing to put it behind");
        assert_eq!(with_background("text", "#123"), "text");
    }

    #[test]
    fn the_palette_follows_the_theme() {
        assert_eq!(css_color(gpui_kit::rgb(0x0a0b0c).into()), "#0a0b0c");
        assert_eq!(css_color(gpui_kit::rgba(0xffffff80).into()), "rgba(255,255,255,0.502)");
        let mut diagrams = Diagrams::default();
        let dark = diagrams.palette(&Tokens::dark());
        assert!(dark.contains(&format!("\"--pele-bg\":\"{}\"", css_color(Tokens::dark().background))));
        assert_eq!(diagrams.palette(&Tokens::dark()), dark, "the same theme keeps its palette");
        assert_ne!(diagrams.palette(&Tokens::light()), dark, "a theme change recolours diagrams");
        assert!(pele_script().starts_with(b"/*! Pele 1.8.0"), "the vendored renderer decompresses");
    }
}
