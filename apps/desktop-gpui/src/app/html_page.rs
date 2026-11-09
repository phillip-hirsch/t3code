//! Pages an agent rendered with `html_render` (`HtmlRenderFrame`, `HtmlRenderDialog`): the page in
//! the timeline at the height it measures, with "Open full screen" into a dialog that zooms it.
//!
//! The page runs in a sandboxed iframe of an embed's own shell page (`HtmlRenderDocument`), whose
//! script forwards the page's `postMessage` bridge over IPC. Links, images and the full screen
//! button need the reader's activation, as the web's frame does.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, FocusHandle, Image, InteractiveElement as _, IntoElement as _, KeyDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use serde_json::{Value, json};

use super::Workspace;
use super::embed::{EmbedView, FRAME_CSS, FRAME_SCRIPT, Layer};
use super::image_viewer::{Reference, ViewerImage};
use super::mermaid::css_color;
use crate::theme::Tokens;
use crate::timeline::{self, PAGE_MAX_HEIGHT, PAGE_MIN_HEIGHT, PageSize, clamp_page_height};

/// `HTML_RENDER_COLUMN_WIDTH`.
const COLUMN_WIDTH: f32 = 728.;
/// `HTML_RENDER_MAX_FIT_HEIGHT`: a page that grows with its frame stops here.
const MAX_FIT_HEIGHT: f32 = 20_000.;
/// `MIN_URL_LIFE_MS`: full screen re-signs a URL that expires sooner.
const MIN_URL_LIFE_MS: f64 = 5. * 60_000.;
/// Pages kept loaded off screen, beside the ones on it.
const KEPT_PAGES: usize = 4;
const ZOOM_STEP: f32 = 0.25;
const ZOOM_RANGE: (f32, f32) = (0.5, 2.);

/// `measuredHeight`: the taller of the measurements around `width`, since a breakpoint between
/// them can pick either.
fn measured_height(heights: &[(u32, f32)], width: f32) -> f32 {
    let Some(last) = heights.len().checked_sub(1) else { return 0. };
    #[expect(clippy::cast_precision_loss, reason = "widths are at most 10 000")]
    let at = |index: usize| heights[index].0 as f32;
    let high = heights.iter().position(|&(measured, _)| measured as f32 >= width).unwrap_or(last);
    let low = if (at(high) - width).abs() < f32::EPSILON { high } else { high.saturating_sub(1) };
    heights[low].1.max(heights[high].1)
}

/// `htmlRenderFrameHeight`: the live page's height once it reports one, else what the server
/// measured at this width, held under the agent's height when that is the smaller.
pub(super) fn frame_height(size: &PageSize, width: f32, content: Option<f32>) -> f32 {
    if let Some(content) = content {
        return content.ceil().clamp(PAGE_MIN_HEIGHT, MAX_FIT_HEIGHT);
    }
    if size.heights.is_empty() {
        return clamp_page_height(size.height);
    }
    let cap = if measured_height(&size.heights, COLUMN_WIDTH) > size.height { size.height } else { PAGE_MAX_HEIGHT };
    clamp_page_height(cap.min(measured_height(&size.heights, width)))
}

/// What a page's shell reports over IPC.
#[derive(Debug, PartialEq)]
enum PageMessage {
    /// The shell (re)loaded and listens for its box.
    Ready,
    /// `readHtmlRenderContentHeight`.
    Height(f32),
    /// `readHtmlRenderLinkRequest`; `external` for a Cmd/Ctrl click.
    Link { url: String, external: bool },
    /// `readHtmlRenderImageRequest`.
    Image { src: String, name: String },
    /// The shell's "Open full screen" button.
    Expand,
}

fn starts_with_ignore_case(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len()).is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

fn page_message(body: &str) -> Option<PageMessage> {
    let value: Value = serde_json::from_str(body).ok()?;
    match value["type"].as_str()? {
        "ready" => return Some(PageMessage::Ready),
        "expand" => return Some(PageMessage::Expand),
        "page" => {}
        _ => return None,
    }
    let data = &value["data"];
    if data["jsonrpc"] != "2.0" {
        return None;
    }
    let params = &data["params"];
    let method = data["method"].as_str()?;
    #[expect(clippy::cast_possible_truncation, reason = "a page height")]
    if method == "ui/notifications/size-changed" {
        return params["height"].as_f64().filter(|h| h.is_finite() && *h > 0.).map(|h| PageMessage::Height(h as f32));
    }
    // Opens need the reader's activation on the frame: a page can take focus by script.
    if value["active"] != true {
        return None;
    }
    match method {
        "ui/open-link" if data["id"].is_string() || data["id"].is_number() => {
            let url = params["url"].as_str()?;
            (starts_with_ignore_case(url, "http://") || starts_with_ignore_case(url, "https://")).then(|| {
                PageMessage::Link {
                    url: url.to_owned(),
                    external: params["metaKey"] == true || params["ctrlKey"] == true,
                }
            })
        }
        "t3/notifications/open-image" => {
            let src = params["src"].as_str()?;
            let allowed = ["http://", "https://", "data:image/"].iter().any(|p| starts_with_ignore_case(src, p));
            let name: String = params["name"].as_str().unwrap_or_default().trim().chars().take(200).collect();
            allowed.then(|| PageMessage::Image {
                src: src.to_owned(),
                name: if name.is_empty() { "Image".to_owned() } else { name },
            })
        }
        _ => None,
    }
}

/// Forwards the page's bridge (`HtmlRenderDocument`'s message listener): its height always, the
/// rest with the activation it carries, and answers a link request the way the web does.
const PAGE_SCRIPT: &str = "(()=>{const f=document.getElementById('f'),x=document.getElementById('x');\
const post=(m)=>window.ipc.postMessage(JSON.stringify(m));\
addEventListener('message',(e)=>{if(e.source!==f.contentWindow)return;const d=e.data;\
if(!d||typeof d!=='object'||d.jsonrpc!=='2.0')return;\
const active=document.activeElement===f&&!(navigator.userActivation&&navigator.userActivation.isActive===false);\
if(active&&d.method==='ui/open-link'&&(typeof d.id==='string'||typeof d.id==='number'))\
f.contentWindow.postMessage({jsonrpc:'2.0',id:d.id,result:{}},'*');\
try{post({type:'page',active,data:d});}catch(_){}});\
if(x)x.addEventListener('click',(e)=>{if(e.isTrusted)post({type:'expand'});});\
window.t3Zoom=(z)=>{document.getElementById('t3-embed').style.zoom=z;};\
window.t3Scheme=(s)=>{f.style.colorScheme=s;document.querySelector('meta[name=color-scheme]').content=s;};})();";

/// Lucide's `Maximize2`.
const EXPAND_ICON: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"14\" height=\"14\" viewBox=\"0 0 24 24\" \
fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\
<polyline points=\"15 3 21 3 21 9\"/><polyline points=\"9 21 3 21 3 15\"/><line x1=\"21\" x2=\"14\" y1=\"3\" y2=\"10\"/>\
<line x1=\"3\" x2=\"10\" y1=\"21\" y2=\"14\"/></svg>";

fn escape_attribute(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

/// The colours the shell draws itself in: the page's background and the glass button's.
struct ShellLook {
    dark: bool,
    background: String,
    glass: String,
    border: String,
    icon: String,
    hover_border: String,
    ring: String,
}

impl ShellLook {
    fn of(t: &Tokens) -> Self {
        Self {
            dark: t.dark,
            background: css_color(t.background),
            glass: css_color(t.background.opacity(t.glass)),
            border: css_color(t.border.opacity(0.6)),
            icon: css_color(t.muted_foreground),
            hover_border: css_color(t.border),
            ring: css_color(t.ring),
        }
    }
}

/// The embed's shell: the page in its sandboxed frame (`sandbox="allow-scripts allow-forms"`, never
/// `allow-same-origin`, so it stays out of the app's session), and inline, the hover-revealed
/// "Open full screen" button over its top-right corner. Full screen scrolls when zoomed past 100%.
fn page_shell(url: &str, title: &str, look: &ShellLook, inline: bool) -> String {
    let scheme = if look.dark { "dark" } else { "light" };
    let src = escape_attribute(url.split('#').next().unwrap_or(url));
    let title = escape_attribute(title);
    let ShellLook { background, glass, border, icon, hover_border, ring, .. } = look;
    let (button, overflow) = if inline {
        (
            format!(
                "<button id=\"x\" type=\"button\" aria-label=\"Open full screen\" title=\"Open full screen\">{EXPAND_ICON}</button>"
            ),
            "",
        )
    } else {
        (String::new(), "html,body{overflow:auto}")
    };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"color-scheme\" content=\"{scheme}\">\
         <style>{FRAME_CSS}{overflow}html{{background:{background}}}\
         iframe{{display:block;border:0;width:100%;height:100%;color-scheme:{scheme}}}\
         #x{{position:absolute;top:8px;right:8px;width:24px;height:24px;padding:0;display:flex;align-items:center;\
         justify-content:center;border-radius:9999px;border:1px solid {border};background:{glass};color:{icon};\
         -webkit-backdrop-filter:blur(12px) saturate(1.5);backdrop-filter:blur(12px) saturate(1.5);\
         box-shadow:0 1px 2px rgba(0,0,0,.05);cursor:pointer;opacity:0;transition:opacity .15s}}\
         body:hover #x,#x:focus-visible{{opacity:1}}#x:hover{{border-color:{hover_border}}}\
         #x:focus-visible{{outline:2px solid {ring};outline-offset:1px}}</style></head>\
         <body><div id=\"t3-embed\"><iframe id=\"f\" src=\"{src}\" title=\"{title}\" sandbox=\"allow-scripts allow-forms\" \
         allow=\"fullscreen\"></iframe>{button}</div><script>{FRAME_SCRIPT}{PAGE_SCRIPT}</script></body></html>"
    )
}

/// A page by environment and attachment.
type PageKey = (SharedString, SharedString);

struct Page {
    embed: Option<EmbedView>,
    /// The URL the frame loaded and when it expires (ms since the epoch); the frame keeps it.
    url: Option<(String, f64)>,
    failed: bool,
    /// The live page's height, once it reports one.
    content_height: Option<f32>,
    dark: bool,
    /// The frame this page last drew in.
    used: u64,
    _task: Option<Task<()>>,
    _messages: Option<Task<()>>,
}

struct Dialog {
    key: PageKey,
    title: SharedString,
    /// The thread it opened from: leaving the thread closes it, as unmounting the row does.
    thread: Option<String>,
    embed: Option<EmbedView>,
    zoom: f32,
    dark: bool,
    focus: FocusHandle,
    close_focus: FocusHandle,
    restore: Option<FocusHandle>,
    _task: Option<Task<()>>,
    _messages: Option<Task<()>>,
}

#[derive(Default)]
pub(super) struct HtmlPages {
    pages: HashMap<PageKey, Page>,
    dialog: Option<Dialog>,
}

/// Where a shell's report came from.
#[derive(Clone)]
enum Origin {
    Inline(PageKey),
    Dialog(PageKey),
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0., |d| d.as_secs_f64() * 1000.)
}

/// `assets.createUrl` for the page (`HtmlRenderFrame`'s resource): its URL and expiry.
async fn mint_page_url(
    client: crate::rpc::RpcClient,
    base: String,
    attachment_id: SharedString,
    file_name: String,
) -> Option<(String, f64)> {
    let resource = json!({
        "_tag": "attachment",
        "attachmentId": attachment_id,
        "fileName": file_name,
        "mimeType": "text/html",
        "disposition": "inline",
    });
    match client.request("assets.createUrl", json!({"resource": resource})).await {
        Ok(minted) => {
            let url = format!("{base}{}", minted["relativeUrl"].as_str()?);
            Some((url, minted["expiresAt"].as_f64().unwrap_or(0.)))
        }
        Err(error) => {
            log::debug!("assets.createUrl for a rendered page: {error}");
            None
        }
    }
}

impl Workspace {
    fn page_key(&self, embed: &timeline::Embed) -> Option<PageKey> {
        Some((self.active_environment()?.clone(), embed.attachment_id.clone()))
    }

    /// `HtmlRenderTimelineRow`: the page at its frame height, nothing while its URL is minted, and
    /// "Unable to load …" when it can't be.
    pub(super) fn render_html_page(
        &mut self,
        embed: &Rc<timeline::Embed>,
        size: &PageSize,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *crate::theme::tokens(cx);
        let Some(key) = self.page_key(embed) else { return div().into_any_element() };
        let frame = self.embeds.frame();
        if !self.html_pages.pages.contains_key(&key) {
            self.start_html_page(key.clone(), embed, window, cx);
        }
        let Some(page) = self.html_pages.pages.get_mut(&key) else { return div().into_any_element() };
        page.used = frame;
        let height = frame_height(size, width, page.content_height);
        let body = match (&page.embed, page.failed) {
            (Some(view), _) => view.frame().into_any_element(),
            (None, true) => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(t.muted_foreground)
                .child(format!("Unable to load {}", embed.title))
                .into_any_element(),
            (None, false) => div().into_any_element(),
        };
        div().w_full().h(px(height)).relative().child(body).into_any_element()
    }

    fn start_html_page(&mut self, key: PageKey, embed: &timeline::Embed, window: &mut Window, cx: &mut Context<Self>) {
        let look = ShellLook::of(crate::theme::tokens(cx));
        let page = Page {
            embed: None,
            url: None,
            failed: false,
            content_height: None,
            dark: look.dark,
            used: self.embeds.frame(),
            _task: None,
            _messages: None,
        };
        self.html_pages.pages.insert(key.clone(), page);
        let (Some(client), Some(base)) = (self.client(), self.http_base().map(str::to_owned)) else {
            self.html_page_failed(&key, cx);
            return;
        };
        let (attachment, file_name, title) = (embed.attachment_id.clone(), embed.file_name(), embed.title.to_string());
        let request = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let minted = mint_page_url(client, base, attachment, file_name).await;
            let _ = this.update_in(cx, |this, window, cx| match minted {
                Some((url, expires)) => {
                    let Some(page) = this.html_pages.pages.get_mut(&request) else { return };
                    page.url = Some((url.clone(), expires));
                    let shell = page_shell(&url, &title, &look, true);
                    this.build_page_view(Origin::Inline(request), shell, look.dark, window, cx);
                }
                None => this.html_page_failed(&request, cx),
            });
        });
        if let Some(page) = self.html_pages.pages.get_mut(&key) {
            page._task = Some(task);
        }
    }

    fn html_page_failed(&mut self, key: &PageKey, cx: &mut Context<Self>) {
        if let Some(page) = self.html_pages.pages.get_mut(key) {
            page.failed = true;
            page._task = None;
            cx.notify();
        }
    }

    /// Builds a shell's view in a task of its own: see [`build_view`](super::preview::build_view).
    fn build_page_view(
        &mut self,
        origin: Origin,
        shell: String,
        dark: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (sender, reports) = async_channel::unbounded::<String>();
        let report_origin = origin.clone();
        let messages = cx.spawn_in(window, async move |this, cx| {
            while let Ok(body) = reports.recv().await {
                let Some(message) = page_message(&body) else { continue };
                let origin = report_origin.clone();
                if this.update_in(cx, |this, window, cx| this.on_page_message(origin, message, window, cx)).is_err() {
                    break;
                }
            }
        });
        let background = crate::theme::tokens(cx).background;
        let rgba = gpui_kit::Rgba::from(background);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to 0..=255")]
        let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
        let builder = wry::WebViewBuilder::new()
            .with_devtools(true)
            .with_focused(false)
            .with_background_color((byte(rgba.r), byte(rgba.g), byte(rgba.b), 255))
            .with_ipc_handler(move |request| {
                let _ = sender.try_send(request.into_body());
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_html(shell);
        let built_origin = origin.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let origin = built_origin;
            let _ = this.update_in(cx, |this, window, cx| {
                let wanted = match &origin {
                    Origin::Inline(key) => this.html_pages.pages.contains_key(key),
                    Origin::Dialog(key) => this.html_pages.dialog.as_ref().is_some_and(|d| d.key == *key),
                };
                if !wanted {
                    return;
                }
                let built = super::preview::build_view(builder, window, cx);
                let layer = if matches!(origin, Origin::Dialog(_)) { Layer::Dialog } else { Layer::Page };
                let embed = built.map(|view| {
                    let _ = view.set_theme(if dark { wry::Theme::Dark } else { wry::Theme::Light });
                    this.embeds.adopt(view, layer, cx)
                });
                match origin {
                    Origin::Inline(key) => match embed {
                        Ok(embed) => {
                            if let Some(page) = this.html_pages.pages.get_mut(&key) {
                                page.embed = Some(embed);
                                page._task = None;
                            }
                        }
                        Err(error) => {
                            log::warn!("rendered page: {error}");
                            this.html_page_failed(&key, cx);
                        }
                    },
                    Origin::Dialog(_) => match embed {
                        Ok(embed) => {
                            if let Some(dialog) = this.html_pages.dialog.as_mut() {
                                dialog.embed = Some(embed);
                                dialog._task = None;
                            }
                        }
                        Err(error) => {
                            log::warn!("rendered page: {error}");
                            this.close_html_dialog(window, cx);
                            this.page_unavailable(window, cx);
                        }
                    },
                }
                cx.notify();
            });
        });
        match &origin {
            Origin::Inline(key) => {
                if let Some(page) = self.html_pages.pages.get_mut(key) {
                    page._task = Some(task);
                    page._messages = Some(messages);
                }
            }
            Origin::Dialog(_) => {
                if let Some(dialog) = self.html_pages.dialog.as_mut() {
                    dialog._task = Some(task);
                    dialog._messages = Some(messages);
                }
            }
        }
    }

    fn on_page_message(&mut self, origin: Origin, message: PageMessage, window: &mut Window, cx: &mut Context<Self>) {
        let embed = match &origin {
            Origin::Inline(key) => self.html_pages.pages.get(key).and_then(|page| page.embed.as_ref()),
            Origin::Dialog(key) => {
                self.html_pages.dialog.as_ref().filter(|d| d.key == *key).and_then(|d| d.embed.as_ref())
            }
        };
        match message {
            PageMessage::Ready => {
                if let Some(embed) = embed {
                    embed.retell();
                }
                if let Origin::Dialog(_) = origin
                    && let Some(dialog) = &self.html_pages.dialog
                    && let Some(embed) = &dialog.embed
                {
                    let _ = embed.raw(cx).evaluate_script(&format!("window.t3Zoom&&t3Zoom({})", dialog.zoom));
                }
                cx.notify();
            }
            PageMessage::Height(height) => {
                let Origin::Inline(key) = origin else { return };
                let Some(page) = self.html_pages.pages.get_mut(&key) else { return };
                if page.content_height.is_some_and(|h| (h - height).abs() < 0.5) {
                    return;
                }
                page.content_height = Some(height);
                self.remeasure_page_row(&key.1, cx);
            }
            PageMessage::Link { url, external } => {
                if external {
                    cx.open_url(&url);
                } else if !self.open_change_request_link(&url, cx) {
                    let in_app = crate::preview::link_opens_in_app(
                        &url,
                        false,
                        &self.client_settings.values,
                        self.browser_available(),
                    );
                    match self.preview_thread_id().map(str::to_owned).filter(|_| in_app) {
                        Some(thread_id) => self.request_preview_tab(thread_id, Some(url), true, window, cx),
                        None => cx.open_url(&url),
                    }
                }
            }
            PageMessage::Image { src, name } => self.open_page_image(src, name, window, cx),
            PageMessage::Expand => {
                if let Origin::Inline(key) = origin {
                    self.open_html_dialog(key, window, cx);
                }
            }
        }
    }

    /// Re-lays out the timeline row of the page `attachment_id`, whose height changed.
    fn remeasure_page_row(&mut self, attachment_id: &SharedString, cx: &mut Context<Self>) {
        let is_page =
            |row: &timeline::Row| matches!(row, timeline::Row::Embed(embed) if embed.attachment_id == *attachment_id);
        if let Some(index) = self.rows.iter().position(is_page) {
            self.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
        }
        if let Some(side) = &self.side_chat.view
            && let Some(index) = side.rows.iter().position(is_page)
        {
            side.scroller.update(cx, |scroller, cx| scroller.remeasure_items(index..index + 1, cx));
        }
        cx.notify();
    }

    /// An image the page asked to expand (`ExpandedImageDialog`): a published image's data URL, or
    /// a web image, loaded and opened in the image viewer.
    fn open_page_image(&mut self, src: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            let (image, reference) = if src.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("data:")) {
                let image = super::markdown_images::data_url(&src)
                    .map(|(format, bytes)| Arc::new(Image::from_bytes(format, bytes)));
                (image, None)
            } else {
                let image = super::markdown_images::fetch_image(src.clone()).await;
                (image, Some(Reference::Url(src)))
            };
            let _ = this.update_in(cx, |this, window, cx| match image {
                Some(image) => {
                    let item = ViewerImage { image, name: name.into(), reference };
                    this.open_image_viewer(vec![item], 0, window, cx);
                }
                None => window.push_notification(Notification::error("The image could not be loaded."), cx),
            });
        })
        .detach();
    }

    fn page_unavailable(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.push_notification(
            Notification::error("Reconnect to the environment and try again.").title("Page unavailable"),
            cx,
        );
    }

    pub(super) fn html_dialog_open(&self) -> bool {
        self.html_pages.dialog.is_some()
    }

    /// "Open full screen": the page in a dialog, from the frame's URL while it has five minutes
    /// left, else a fresh one.
    fn open_html_dialog(&mut self, key: PageKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.html_pages.dialog.is_some() {
            return;
        }
        let embed =
            self.rows.iter().chain(self.side_chat.view.iter().flat_map(|side| side.rows.iter())).find_map(|row| {
                match row {
                    timeline::Row::Embed(embed) if embed.attachment_id == key.1 => Some(embed.clone()),
                    _ => None,
                }
            });
        let Some(embed) = embed else { return };
        let look = ShellLook::of(crate::theme::tokens(cx));
        let restore = window.focused(cx);
        let close_focus = cx.focus_handle().tab_index(0).tab_stop(true);
        close_focus.focus(window, cx);
        self.html_pages.dialog = Some(Dialog {
            key: key.clone(),
            title: embed.title.clone(),
            thread: self.thread.as_ref().map(|thread| thread.id.clone()),
            embed: None,
            zoom: 1.,
            dark: look.dark,
            focus: cx.focus_handle(),
            close_focus,
            restore,
            _task: None,
            _messages: None,
        });
        let fresh = self
            .html_pages
            .pages
            .get(&key)
            .and_then(|page| page.url.clone())
            .filter(|(_, expires)| expires - now_ms() > MIN_URL_LIFE_MS);
        let title = embed.title.to_string();
        if let Some((url, _)) = fresh {
            self.build_page_view(Origin::Dialog(key), page_shell(&url, &title, &look, false), look.dark, window, cx);
            cx.notify();
            return;
        }
        let (Some(client), Some(base)) = (self.client(), self.http_base().map(str::to_owned)) else {
            self.close_html_dialog(window, cx);
            self.page_unavailable(window, cx);
            return;
        };
        let (attachment, file_name) = (embed.attachment_id.clone(), embed.file_name());
        let task = cx.spawn_in(window, async move |this, cx| {
            let minted = mint_page_url(client, base, attachment, file_name).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.html_pages.dialog.as_ref().is_none_or(|dialog| dialog.key != key) {
                    return;
                }
                match minted {
                    Some((url, _)) => {
                        let shell = page_shell(&url, &title, &look, false);
                        this.build_page_view(Origin::Dialog(key), shell, look.dark, window, cx);
                    }
                    None => {
                        this.close_html_dialog(window, cx);
                        this.page_unavailable(window, cx);
                    }
                }
            });
        });
        if let Some(dialog) = self.html_pages.dialog.as_mut() {
            dialog._task = Some(task);
        }
        cx.notify();
    }

    fn close_html_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.html_pages.dialog.take() else { return };
        if let Some(restore) = dialog.restore {
            restore.focus(window, cx);
        }
        cx.notify();
    }

    fn zoom_html_dialog(&mut self, zoom: Option<f32>, cx: &mut Context<Self>) {
        let Some(dialog) = self.html_pages.dialog.as_mut() else { return };
        let zoom = zoom.map_or(1., |zoom| zoom.clamp(ZOOM_RANGE.0, ZOOM_RANGE.1));
        dialog.zoom = zoom;
        if let Some(embed) = &dialog.embed {
            let _ = embed.raw(cx).evaluate_script(&format!("window.t3Zoom&&t3Zoom({zoom})"));
        }
        cx.notify();
    }

    /// Each frame, before anything draws: pages follow the theme, pages long off screen close, and
    /// the dialog closes with the thread it opened from.
    pub(super) fn sync_html_pages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dark = crate::theme::tokens(cx).dark;
        let scheme = if dark { "window.t3Scheme&&t3Scheme('dark')" } else { "window.t3Scheme&&t3Scheme('light')" };
        let theme = if dark { wry::Theme::Dark } else { wry::Theme::Light };
        let pages = &mut self.html_pages;
        let views = pages
            .pages
            .values_mut()
            .map(|page| (&mut page.dark, page.embed.as_ref()))
            .chain(pages.dialog.as_mut().map(|dialog| (&mut dialog.dark, dialog.embed.as_ref())));
        for (shown_dark, embed) in views {
            if let Some(embed) = embed.filter(|_| *shown_dark != dark) {
                *shown_dark = dark;
                let _ = embed.webview(cx).set_theme(theme);
                let _ = embed.raw(cx).evaluate_script(scheme);
            }
        }
        if pages.pages.len() > KEPT_PAGES {
            let last = self.embeds.frame();
            let mut used: Vec<u64> = pages.pages.values().map(|page| page.used).collect();
            used.sort_unstable_by(|a, b| b.cmp(a));
            let oldest_kept = used[KEPT_PAGES - 1];
            pages.pages.retain(|_, page| page.used >= oldest_kept || page.used + 1 >= last);
        }
        let thread = self.thread.as_ref().map(|thread| thread.id.as_str());
        if self.html_pages.dialog.as_ref().is_some_and(|dialog| dialog.thread.as_deref() != thread) {
            self.close_html_dialog(window, cx);
        }
    }

    fn html_dialog_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.close_html_dialog(window, cx);
        }
    }

    /// `HtmlRenderDialog` over the window: the page in a 92%-wide box, its title and the zoom
    /// controls under it, and Close above its corner.
    pub(super) fn render_html_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.html_pages.dialog.as_ref()?;
        let t = *crate::theme::tokens(cx);
        let white = gpui_kit::white();
        let viewport = window.viewport_size();
        let titlebar = f32::from(super::titlebar::TITLEBAR_HEIGHT);
        let (width, height) = (f32::from(viewport.width), f32::from(viewport.height));
        let box_height = (height * 0.86).min(height - 160.).max(120.);
        let zoom = dialog.zoom;
        let body = match &dialog.embed {
            Some(embed) => embed.frame().into_any_element(),
            None => div().into_any_element(),
        };
        let control = |id: &'static str, label: &'static str, disabled: bool| {
            h_flex()
                .id(id)
                .h(px(28.))
                .min_w(px(28.))
                .justify_center()
                .rounded(px(8.))
                .text_color(white.opacity(0.9))
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .when(!disabled, |el| {
                    el.tab_index(0)
                        .cursor_pointer()
                        .hover(move |style| style.bg(white.opacity(0.1)).text_color(white))
                        .focus_visible(move |style| style.border_color(white))
                })
                .when(disabled, |el| el.opacity(0.5))
                .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
        };
        let step =
            |id: &'static str, icon: IconName, label: &'static str, delta: f32, limit: f32, cx: &mut Context<Self>| {
                let disabled = (zoom - limit).abs() < f32::EPSILON;
                control(id, label, disabled).child(Icon::new(icon).size(px(16.))).when(!disabled, |el| {
                    el.on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.zoom_html_dialog(Some(zoom + delta), cx);
                    }))
                })
            };
        let zoom_out = step("html-dialog-zoom-out", IconName::Minus, "Zoom out", -ZOOM_STEP, ZOOM_RANGE.0, cx);
        let zoom_in = step("html-dialog-zoom-in", IconName::Plus, "Zoom in", ZOOM_STEP, ZOOM_RANGE.1, cx);
        #[expect(clippy::cast_possible_truncation, reason = "a percentage from 50 to 200")]
        let percent = (zoom * 100.).round() as i32;
        let reset = control("html-dialog-zoom-reset", "Reset zoom", false)
            .px(px(10.))
            .text_sm()
            .child(format!("{percent}%"))
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.zoom_html_dialog(None, cx);
            }));
        let close = div()
            .id("html-dialog-close")
            .track_focus(&dialog.close_focus)
            .absolute()
            .right_0()
            .top(px(-40.))
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .cursor_pointer()
            .bg(gpui_kit::black().opacity(0.65))
            .text_color(white)
            .border_1()
            .border_color(white.opacity(0.2))
            .shadow_sm()
            .hover(|style| style.bg(gpui_kit::black().opacity(0.8)))
            .focus_visible(move |style| style.border_color(white))
            .child(Icon::new(IconName::X).size(px(16.)))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.close_html_dialog(window, cx);
            }));
        let content = v_flex()
            .id("html-dialog-content")
            .relative()
            .w(px(width * 0.92))
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(close)
            .child(
                div()
                    .h(px(box_height))
                    .w_full()
                    .p(px(16.))
                    .rounded(px(8.))
                    .bg(t.background)
                    .border_1()
                    .border_color(t.border.opacity(0.7))
                    .shadow_2xl()
                    .child(body),
            )
            .child(
                h_flex()
                    .mt(px(8.))
                    .min_w_0()
                    .justify_center()
                    .gap(px(12.))
                    .text_xs()
                    .text_color(white.opacity(0.8))
                    .child(div().min_w_0().truncate().child(dialog.title.clone()))
                    .child(h_flex().flex_none().gap(px(4.)).child(zoom_out).child(reset).child(zoom_in)),
            )
            .focus_trap("html-dialog-trap", &dialog.focus);
        Some(
            div()
                .id("html-dialog")
                .absolute()
                .inset_0()
                .occlude()
                .bg(gpui_kit::black().opacity(0.75))
                .flex()
                .items_center()
                .justify_center()
                .px(px(16.))
                .pb(px(24.))
                .pt(px(titlebar + 24.))
                .on_key_down(cx.listener(Self::html_dialog_key_down))
                .on_click(cx.listener(|this, _, window, cx| this.close_html_dialog(window, cx)))
                .child(content)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_fit_the_page_like_html_render_frame_height() {
        let plain = PageSize { height: 640., heights: vec![] };
        assert_eq!(frame_height(&plain, 728., None), 640.);
        // The live page wins, within 80 and the fit cap.
        assert_eq!(frame_height(&plain, 728., Some(912.2)), 913.);
        assert_eq!(frame_height(&plain, 728., Some(10.)), 80.);
        assert_eq!(frame_height(&plain, 728., Some(1e9)), 20_000.);
        let measured = PageSize { height: 900., heights: vec![(320, 1400.), (640, 1000.), (728, 800.), (1000, 700.)] };
        // Between two measurements the taller one counts; an exact width uses its own.
        assert_eq!(frame_height(&measured, 700., None), 1000.);
        assert_eq!(frame_height(&measured, 728., None), 800.);
        // Past the widest, the two widest still count.
        assert_eq!(frame_height(&measured, 2000., None), 800.);
        assert_eq!(frame_height(&measured, 100., None), 1400.);
        // An agent height under the column's measurement caps the narrower measurements.
        let capped = PageSize { height: 600., heights: vec![(320, 1400.), (728, 800.)] };
        assert_eq!(frame_height(&capped, 320., None), 600.);
    }

    #[test]
    fn shells_forward_the_bridge_with_its_activation() {
        let page = |active: bool, data: Value| {
            page_message(&json!({"type": "page", "active": active, "data": data}).to_string())
        };
        let rpc = |method: &str, params: Value| json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        assert_eq!(
            page(false, rpc("ui/notifications/size-changed", json!({"height": 420.5}))),
            Some(PageMessage::Height(420.5))
        );
        assert_eq!(page(true, rpc("ui/notifications/size-changed", json!({"height": 0}))), None);
        let link = rpc("ui/open-link", json!({"url": "HTTPS://x.dev/a", "ctrlKey": true}));
        assert_eq!(page(false, link.clone()), None, "a page can't open links without the reader");
        assert_eq!(page(true, link), Some(PageMessage::Link { url: "HTTPS://x.dev/a".into(), external: true }));
        assert_eq!(page(true, rpc("ui/open-link", json!({"url": "javascript:alert(1)"}))), None);
        assert_eq!(
            page(true, json!({"jsonrpc": "2.0", "method": "ui/open-link", "params": {"url": "https://x.dev"}})),
            None,
            "a link request needs its id"
        );
        assert_eq!(
            page(true, rpc("t3/notifications/open-image", json!({"src": "data:image/png;base64,AA", "name": "  "}))),
            Some(PageMessage::Image { src: "data:image/png;base64,AA".into(), name: "Image".into() })
        );
        assert_eq!(page(true, rpc("t3/notifications/open-image", json!({"src": "file:///etc/passwd"}))), None);
        assert_eq!(page(true, json!({"method": "ui/notifications/size-changed", "params": {"height": 9}})), None);
        assert_eq!(page_message(r#"{"type":"ready"}"#), Some(PageMessage::Ready));
        assert_eq!(page_message(r#"{"type":"expand"}"#), Some(PageMessage::Expand));
        assert_eq!(page_message("nope"), None);
    }

    #[test]
    fn the_shell_sandboxes_the_page_without_its_fragment() {
        let look = ShellLook {
            dark: true,
            background: "#000".into(),
            glass: "#111".into(),
            border: "#222".into(),
            icon: "#333".into(),
            hover_border: "#444".into(),
            ring: "#555".into(),
        };
        let inline = page_shell("http://h/a?x=1&y=\"2\"#t3-theme=x", "A <b>", &look, true);
        assert!(inline.contains("src=\"http://h/a?x=1&amp;y=&quot;2&quot;\" title=\"A &lt;b>\""));
        assert!(inline.contains("sandbox=\"allow-scripts allow-forms\"") && !inline.contains("allow-same-origin"));
        assert!(inline.contains("aria-label=\"Open full screen\"") && inline.contains("content=\"dark\""));
        let full = page_shell("http://h/a", "A", &look, false);
        assert!(!full.contains("id=\"x\"") && full.contains("html,body{overflow:auto}"));
    }
}
