//! A sent user message's attachments and context chips (`TimelineUserMessageRow.tsx`,
//! `UserMessageContextChips.tsx`, `contextChipParts.tsx`). Images and files open in the
//! attachment preview; videos and audio play in the system's player through their signed link.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::text::{MarkdownNode, MarkdownParseContext, MarkdownPlugin};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, ElementId, FontWeight, Hsla, Image, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, Pixels, Point, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, WeakEntity, Window, div, img, px, relative, rgb,
};
use serde_json::{Value, json};

use super::Workspace;
use super::chat::view_pane;
use super::side_chat::ChatPane;
use crate::rpc::RpcClient;
use crate::theme::Tokens;
use crate::user_message::{self as logic, Attachment, Layout, Media, UserContent};

/// `CONTEXT_CHIP_ACCENTS`, solved for the default surfaces.
const FILE_INK: u32 = 0x0090cd;
const VIDEO_INK: u32 = 0xd06217;
const MENTION_INK: u32 = 0x0096af;
const TERMINAL_INK: u32 = 0x009f6e;
const ELEMENT_INK: u32 = 0xb87501;
const REVIEW_INK: u32 = 0x8a70dd;
const SKILL_INK: u32 = 0xb261be;
const ENVIRONMENT_INK: u32 = 0x59985b;
const PR_DRAFT_INK: u32 = 0x7f8793;
const PR_CLOSED_INK: u32 = 0xd55665;
const UNAVAILABLE: &str = "This context is no longer available.";
const ATTACHMENT_TIMEOUT: Duration = Duration::from_secs(120);

/// The image's thumbnail once loaded; the first ask starts its load (`useAssetUrls`).
fn thumbnail(this: &WeakEntity<Workspace>, pane: ChatPane, id: &str, cx: &mut App) -> Option<Arc<Image>> {
    let workspace = this.upgrade()?;
    let w = workspace.read(cx);
    if let Some(image) = w.attachment_image(id) {
        return Some(image);
    }
    if !w.attachment_image_requested(id) {
        let (this, id) = (this.clone(), id.to_owned());
        cx.defer(move |cx| {
            let _ = this.update(cx, |w, cx| w.load_attachment_images(pane, [id.as_str()], cx));
        });
    }
    None
}

/// The signed link's bytes (`assets.createUrl`, then a GET).
async fn fetch_bytes(client: &RpcClient, base: &str, resource: Value) -> Result<Bytes, String> {
    let signed =
        client.request("assets.createUrl", json!({ "resource": resource })).await.map_err(|e| e.to_string())?;
    let relative = signed["relativeUrl"].as_str().ok_or_else(|| "The attachment is unavailable.".to_owned())?;
    let url = format!("{base}{relative}");
    let fetched = crate::runtime()
        .spawn(async move {
            let response = reqwest::Client::new().get(url).timeout(ATTACHMENT_TIMEOUT).send().await?;
            response.error_for_status()?.bytes().await
        })
        .await;
    match fetched {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(error.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

fn attachment_resource(attachment: &Attachment, disposition: &str) -> Value {
    json!({
        "_tag": "attachment",
        "attachmentId": attachment.id,
        "fileName": attachment.name,
        "mimeType": attachment.mime,
        "disposition": disposition,
    })
}

fn not_connected(window: &mut Window, cx: &mut App) {
    window.push_notification(Notification::error("").title("The environment is not connected."), cx);
}

impl Workspace {
    /// The socket and HTTP origin of the environment `pane`'s thread lives on.
    fn pane_connection(&self, pane: ChatPane) -> Option<(RpcClient, String)> {
        self.env_origin(&self.pane_thread(pane)?.0)
    }

    /// The socket and HTTP origin `env` has now.
    fn env_origin(&self, env: &str) -> Option<(RpcClient, String)> {
        self.env_connection(env).map(|(client, endpoint)| (client, endpoint.http_base))
    }

    /// `onFileOpen` and `onImageExpand`: a sent file opens in its right panel tab, an image in the
    /// image viewer over the pane it was sent in; a video or a recording plays in the system's
    /// player.
    pub(super) fn open_message_attachment(
        &mut self,
        pane: ChatPane,
        attachment: &Attachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(attachment.media, Media::File | Media::Unknown) {
            let file = super::files::AttachmentFile {
                id: attachment.id.clone(),
                name: attachment.name.clone(),
                mime: attachment.mime.clone(),
                size: attachment.size,
            };
            return self.open_attachment(file, window, cx);
        }
        let (Some(owner), Some((client, base))) = (self.pane_owner(pane), self.pane_connection(pane)) else {
            return not_connected(window, cx);
        };
        let resource = attachment_resource(attachment, "inline");
        if matches!(attachment.media, Media::Video | Media::Audio) {
            cx.spawn(async move |_, cx| {
                match client.request("assets.createUrl", json!({ "resource": resource })).await {
                    Ok(signed) => match signed["relativeUrl"].as_str() {
                        Some(url) => cx.update(|cx| cx.open_url(&format!("{base}{url}"))),
                        None => log::debug!("assets.createUrl for a message attachment returned no URL"),
                    },
                    Err(error) => log::debug!("assets.createUrl for a message attachment: {error}"),
                }
            })
            .detach();
            return;
        }
        // A preview for a thread its pane has left stays closed.
        let attachment = attachment.clone();
        cx.spawn_in(window, async move |this, cx| {
            let fetched = fetch_bytes(&client, &base, resource).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if !this.owner_shown(&owner) {
                    return;
                }
                match fetched {
                    Ok(bytes) => match super::markdown_images::sniff(&bytes) {
                        Some(format) => {
                            let image = Arc::new(Image::from_bytes(format, bytes.to_vec()));
                            let name = SharedString::from(attachment.name.clone());
                            let item = super::image_viewer::ViewerImage { image, name, reference: None };
                            this.open_owned_image_viewer(Some(owner), vec![item], 0, window, cx);
                        }
                        None => {
                            let title = format!("Could not open {}", attachment.label());
                            let note = Notification::error("The image could not be read.").title(title);
                            window.push_notification(note, cx);
                        }
                    },
                    Err(error) => {
                        let title = format!("Could not open {}", attachment.label());
                        window.push_notification(Notification::error(error).title(title), cx);
                    }
                }
            });
        })
        .detach();
    }

    /// `onFileDownload`: saves a sent file where the user picks.
    pub(super) fn download_message_attachment(
        &mut self,
        pane: ChatPane,
        attachment: &Attachment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(env) = self.pane_thread(pane).map(|(env, _)| env).filter(|env| self.env_origin(env).is_some()) else {
            return not_connected(window, cx);
        };
        let picked = cx.prompt_for_new_path(&super::attachment_preview::downloads_dir(), Some(&attachment.name));
        let resource = attachment_resource(attachment, "attachment");
        let label = attachment.label().to_owned();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            // Looked up after the dialog: a reconnect while it was open replaced the socket.
            let connection = this.read_with(cx, |this, _| this.env_origin(&env)).ok().flatten();
            let fetched = match &connection {
                Some((client, base)) => fetch_bytes(client, base, resource).await,
                None => Err("The environment is not connected.".to_owned()),
            };
            let saved = match fetched {
                Ok(bytes) => {
                    let write = async move { std::fs::write(path, bytes).map_err(|error| error.to_string()) };
                    cx.background_executor().spawn(write).await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = saved {
                let _ = cx.update(|window, cx| {
                    let toast = Notification::error(error).title(format!("Could not download {label}"));
                    window.push_notification(toast, cx);
                });
            }
        })
        .detach();
    }
}

fn open_handler(
    pane: ChatPane,
    this: &WeakEntity<Workspace>,
    attachment: &Attachment,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let (this, attachment) = (this.clone(), attachment.clone());
    move |_, window, cx| {
        cx.stop_propagation();
        let _ = this.update(cx, |w, cx| w.open_message_attachment(pane, &attachment, window, cx));
    }
}

/// A `<button>`: Tab reaches it, Enter or Space clicks it, keyboard focus shows the ring.
fn button(el: gpui_kit::Stateful<gpui_kit::Div>, t: &Tokens) -> gpui_kit::Stateful<gpui_kit::Div> {
    super::sidebar::keyboard_button(el.cursor_pointer(), t.ring)
}

/// The Pierre icon for a file name.
fn file_icon(name: &str, size: f32, t: &Tokens) -> Icon {
    let (asset, color) = crate::file_icons::file_icon(name, t.dark);
    Icon::empty().path(asset).text_color(color).size(px(size))
}

/// "Download {label}": a ghost-muted 24px icon button.
fn download_button(
    message: &str,
    attachment: &Attachment,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let pane = view_pane(message).0;
    let label: SharedString = format!("Download {}", attachment.label()).into();
    let (this, attachment_for_click) = (this.clone(), attachment.clone());
    button(
        div()
            .id(ElementId::Name(format!("download:{message}:{}", attachment.id).into()))
            .size(px(24.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .text_color(t.muted_foreground)
            .hover(|s| s.bg(t.accent).text_color(t.foreground)),
        t,
    )
    .child(Icon::new(IconName::Download).size(px(14.)))
    .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
    .on_click(move |_, window, cx| {
        cx.stop_propagation();
        let _ = this.update(cx, |w, cx| w.download_message_attachment(pane, &attachment_for_click, window, cx));
    })
}

/// How the media block's corners meet the bubble and the neighbouring messages.
#[derive(Clone, Copy)]
pub(super) struct Corners {
    pub top_right: bool,
    pub bottom_right: bool,
}

/// `MEDIA_EDGE_CLASS_NAME`: a hairline so dark screenshots keep an edge on a dark background.
fn media_edge(t: &Tokens) -> Hsla {
    if t.dark { gpui_kit::hsla(0., 0., 1., 0.12) } else { gpui_kit::hsla(0., 0., 0., 0.09) }
}

fn rounded_media(el: gpui_kit::Div, corners: Corners) -> gpui_kit::Div {
    el.rounded(px(16.))
        .when(corners.top_right, |el| el.rounded_tr(px(6.)))
        .when(corners.bottom_right, |el| el.rounded_br(px(6.)))
}

/// A video's poster: its name over black, with the play mark; a click plays it.
fn video_tile(message: &str, file: &Attachment, this: &WeakEntity<Workspace>, t: &Tokens) -> AnyElement {
    button(div().id(ElementId::Name(format!("video:{message}:{}", file.id).into())), t)
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .px(px(8.))
        .bg(gpui_kit::black())
        .text_color(gpui_kit::hsla(0., 0., 1., 0.7))
        .text_size(px(11.))
        .child(
            div()
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(gpui_kit::hsla(0., 0., 1., 0.2))
                .child(Icon::new(IconName::Play).size(px(16.)).text_color(gpui_kit::white())),
        )
        .child(div().max_w_full().truncate().child(file.name.clone()))
        .on_click(open_handler(view_pane(message).0, this, file))
        .into_any_element()
}

/// An image's tile: the picture, or its name while it loads; a click previews it.
fn image_tile(
    message: &str,
    image: &Attachment,
    fit: ObjectFit,
    lone: bool,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let picture = thumbnail(this, view_pane(message).0, &image.id, cx);
    let own_width = lone && picture.is_some();
    let body = match picture {
        // A lone image takes its width from its own shape.
        Some(picture) if own_width => img(picture).h_full().object_fit(fit).into_any_element(),
        Some(picture) => img(picture).size_full().object_fit(fit).into_any_element(),
        None => div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .px(px(8.))
            .text_size(px(11.))
            .text_color(t.muted_foreground.opacity(0.7))
            .child(image.name.clone())
            .into_any_element(),
    };
    let details = image.snap_shot.as_ref().map(|source| {
        let key = ElementId::Name(format!("snap-shot-source:{message}:{}", image.id).into());
        let source =
            window.use_keyed_state(key, cx, |_, _| super::snap_shot::SnapShotSource::from_value(source.clone()));
        source.read(cx).render_details(&format!("{message}-{}", image.id), t)
    });
    button(div().id(ElementId::Name(format!("image:{message}:{}", image.id).into())), t)
        .relative()
        .when(own_width, gpui_kit::Styled::h_full)
        .when(!own_width, gpui_kit::Styled::size_full)
        .overflow_hidden()
        .bg(t.background.opacity(0.7))
        .child(body)
        .children(details)
        .on_click(open_handler(view_pane(message).0, this, image))
        .into_any_element()
}

/// The images and videos (`mediaBlock`): inside the bubble as a small grid, or standing on their
/// own as a lone picture, a collage, or a column of tiles.
/// `message` here and in the other blocks is the message's view key (`chat::view_key`), so a
/// click acts on the pane it was sent in.
pub(super) fn media_block(
    message: &str,
    layout: &Layout,
    corners: Corners,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let (images, videos) = (&layout.images, &layout.videos);
    let count = images.len() + videos.len();
    if count == 0 {
        return None;
    }
    let edge = media_edge(t);
    let tile = |attachment: &Attachment, fit: ObjectFit, window: &mut Window, cx: &mut App| {
        if attachment.media == Media::Video {
            video_tile(message, attachment, this, t)
        } else {
            image_tile(message, attachment, fit, false, this, t, window, cx)
        }
    };
    let all: Vec<&Attachment> = images.iter().chain(videos.iter()).copied().collect();
    if !layout.standalone_media() {
        // `mb-2 max-w-[210px] grid-cols-2 gap-2`, each tile 4:3 in a rounded border.
        let tiles = all.iter().map(|attachment| {
            let frame = div().overflow_hidden().rounded(px(8.)).border_1().border_color(t.border.opacity(0.8));
            let frame = if attachment.snap_shot.is_some() {
                frame.w(px(208.)).h(px(112.))
            } else {
                frame.w(px(101.)).h(px(76.))
            };
            frame.child(tile(attachment, ObjectFit::Cover, window, cx)).into_any_element()
        });
        return Some(
            h_flex()
                .flex_wrap()
                .mb(px(8.))
                .max_w(px(210.))
                .gap(px(8.))
                .children(tiles.collect::<Vec<_>>())
                .into_any_element(),
        );
    }
    let framed = |el: gpui_kit::Div| rounded_media(el.overflow_hidden().border_1().border_color(edge), corners);
    let snap_shots = images.iter().any(|image| image.snap_shot.is_some());
    if videos.is_empty() && images.len() == 1 {
        // A lone image keeps its own shape; a 4:3 crop hides most of a wide screenshot.
        let image = images[0];
        let loaded = this.upgrade().and_then(|w| w.read(cx).attachment_image(&image.id)).is_some();
        let frame = framed(div().h(px(192.)).min_w(px(96.)).max_w_full()).when(!loaded, |el| el.w(px(256.)));
        let fit = if image.snap_shot.is_some() { ObjectFit::Contain } else { ObjectFit::Cover };
        return Some(frame.child(image_tile(message, image, fit, true, this, t, window, cx)).into_any_element());
    }
    if videos.is_empty() && !snap_shots {
        // A collage: one rounded shape split by hairlines.
        let collage = framed(v_flex().w(px(320.)).max_w_full().gap(px(1.)).bg(edge));
        let cell = |image: &Attachment, height: f32, window: &mut Window, cx: &mut App| {
            div().flex_1().min_w_0().h(px(height)).child(tile(image, ObjectFit::Cover, window, cx))
        };
        let collage = match images.len() {
            2 => collage.child(
                h_flex().gap(px(1.)).child(cell(images[0], 192., window, cx)).child(cell(images[1], 192., window, cx)),
            ),
            3 => collage.child(
                h_flex()
                    .gap(px(1.))
                    .child(div().w(relative(0.6)).h(px(192.)).child(tile(images[0], ObjectFit::Cover, window, cx)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(1.))
                            .child(cell(images[1], 95.5, window, cx))
                            .child(cell(images[2], 95.5, window, cx)),
                    ),
            ),
            _ => {
                let rows: Vec<AnyElement> = images
                    .chunks(2)
                    .map(|pair| {
                        h_flex()
                            .gap(px(1.))
                            .children(pair.iter().map(|image| cell(image, 96., window, cx)))
                            .into_any_element()
                    })
                    .collect();
                collage.children(rows)
            }
        };
        return Some(collage.into_any_element());
    }
    // Videos and captures: a lone one fills the 320px column, more share two columns.
    let lone = count == 1;
    let tiles: Vec<AnyElement> = all
        .iter()
        .map(|attachment| {
            let wide = lone || attachment.snap_shot.is_some();
            let (width, height) =
                if wide { (320., if attachment.snap_shot.is_some() { 192. } else { 240. }) } else { (156., 117.) };
            let fit = if attachment.snap_shot.is_some() { ObjectFit::Contain } else { ObjectFit::Cover };
            let frame = div().w(px(width)).max_w_full().h(px(height)).overflow_hidden().border_1().border_color(edge);
            let frame = if lone { rounded_media(frame, corners) } else { frame.rounded(px(16.)) };
            frame.child(tile(attachment, fit, window, cx)).into_any_element()
        })
        .collect();
    Some(h_flex().flex_wrap().justify_end().w(px(320.)).max_w_full().gap(px(8.)).children(tiles).into_any_element())
}

/// `InlineAudioPreview`: each recording by name, as a chip in the bubble or, sent alone, as its
/// own bubble; a click plays it.
pub(super) fn audio_block(
    message: &str,
    layout: &Layout,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> Option<AnyElement> {
    if layout.audio.is_empty() {
        return None;
    }
    let bubble = layout.audio_only;
    let rows = layout.audio.iter().map(|file| {
        button(div().id(ElementId::Name(format!("audio:{message}:{}", file.id).into())), t)
            .flex()
            .items_center()
            .min_w_0()
            .gap(px(8.))
            .when(bubble, |el| {
                el.w(px(256.)).max_w_full().px(px(12.)).py(px(8.)).rounded(px(16.)).bg(t.message_surface)
            })
            .when(!bubble, |el| el.py(px(2.)).rounded(px(6.)))
            .text_sm()
            .child(Icon::new(IconName::AudioLines).size(px(16.)).flex_none().text_color(t.muted_foreground))
            .child(div().min_w_0().flex_1().truncate().hover(gpui_kit::Styled::underline).child(file.name.clone()))
            .on_click(open_handler(view_pane(message).0, this, file))
            .into_any_element()
    });
    Some(
        v_flex()
            .max_w_full()
            .gap(px(4.))
            .when(!bubble, |el| el.mb(px(8.)))
            .children(rows.collect::<Vec<_>>())
            .into_any_element(),
    )
}

/// `UserFileCard`s: files sent without words, each a card that previews it, with a download.
pub(super) fn file_cards(message: &str, layout: &Layout, this: &WeakEntity<Workspace>, t: &Tokens) -> AnyElement {
    let cards = layout.files.iter().map(|file| {
        let identity = button(div().id(ElementId::Name(format!("file-card:{message}:{}", file.id).into())), t)
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(12.))
            .rounded(px(12.))
            .child(
                div()
                    .size(px(36.))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .bg(t.background.opacity(0.6))
                    .child(file_icon(&file.name, 16., t)),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .hover(gpui_kit::Styled::underline)
                            .child(file.label().to_owned()),
                    )
                    .child(div().text_xs().text_color(t.muted_foreground).child(file.card_detail())),
            )
            .on_click(open_handler(view_pane(message).0, this, file));
        h_flex()
            .w(px(256.))
            .max_w_full()
            .gap(px(4.))
            .rounded(px(16.))
            .bg(t.message_surface)
            .py(px(8.))
            .pl(px(8.))
            .pr(px(4.))
            .text_color(t.foreground)
            .child(identity)
            .child(download_button(message, file, this, t))
            .into_any_element()
    });
    v_flex().items_end().gap(px(4.)).children(cards.collect::<Vec<_>>()).into_any_element()
}

/// The files a chip does not stand for, listed in the bubble with preview and download, and
/// attachments of a kind this client does not know, by name.
pub(super) fn file_list(
    message: &str,
    layout: &Layout,
    this: &WeakEntity<Workspace>,
    t: &Tokens,
) -> Option<AnyElement> {
    if layout.listed_files.is_empty() && layout.unknown.is_empty() {
        return None;
    }
    let files = layout.listed_files.iter().map(|file| {
        let identity = button(div().id(ElementId::Name(format!("file-row:{message}:{}", file.id).into())), t)
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .text_sm()
            .hover(gpui_kit::Styled::underline)
            .child(file_icon(&file.name, 16., t).flex_none())
            .child(v_flex().min_w_0().flex_1().child(div().truncate().child(file.label().to_owned())).when(
                file.pasted_text,
                |el| {
                    el.child(
                        div().text_xs().text_color(t.muted_foreground).child(logic::format_bytes(file.size as f64)),
                    )
                },
            ))
            .child(Icon::new(IconName::Eye).size(px(16.)).flex_none())
            .on_click(open_handler(view_pane(message).0, this, file));
        h_flex().min_w_0().gap(px(4.)).child(identity).child(download_button(message, file, this, t)).into_any_element()
    });
    let unknown = layout.unknown.iter().map(|attachment| {
        h_flex()
            .min_w_0()
            .gap(px(8.))
            .py(px(4.))
            .text_sm()
            .child(file_icon(&attachment.name, 16., t).flex_none())
            .child(div().min_w_0().flex_1().truncate().child(attachment.name.clone()))
            .into_any_element()
    });
    Some(v_flex().mb(px(8.)).gap(px(4.)).children(files.chain(unknown).collect::<Vec<_>>()).into_any_element())
}

/// A context chip's identity, kept from parse to render.
#[derive(Clone)]
struct ContextRef {
    kind: String,
    id: String,
    label: String,
}

/// `renderContextReference`: a sent message's `t3-context:` links draw as the chip for the record
/// they name (`UserMessageContextReferenceChip`); one whose record is gone draws dashed.
pub(super) struct ContextChips {
    pub workspace: WeakEntity<Workspace>,
    pub content: Arc<UserContent>,
    /// The message's view key, which names the pane it shows in.
    pub message: SharedString,
}

impl MarkdownPlugin for ContextChips {
    fn name(&self) -> &'static str {
        "t3-context"
    }

    fn parse(
        &self,
        node: &gpui_kit::component::text::markdown_ast::Node,
        cx: &MarkdownParseContext<'_>,
    ) -> Option<MarkdownNode> {
        use gpui_kit::component::text::markdown_ast::Node;
        let (url, written) = match node {
            Node::Link(link) => (&link.url, super::markdown_ui::link_text(&link.children)),
            Node::Image(image) => (&image.url, image.alt.clone()),
            _ => return None,
        };
        let (kind, id) = logic::parse_href(url)?;
        let label = logic::sanitize_label(&written, &kind);
        let markdown = cx.node_source(node).unwrap_or_default().to_owned();
        Some(
            MarkdownNode::new("t3-context", ContextRef { kind, id, label: label.clone() })
                .text(label)
                .markdown(markdown),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(reference) = node.data::<ContextRef>() else { return div().into_any_element() };
        self.chip(reference, cx)
    }
}

/// `ContextChip`: the kind's icon and the label, in the kind's ink.
fn chip_shell(
    id: String,
    icon: AnyElement,
    label: SharedString,
    pressable: bool,
    t: &Tokens,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let ring = t.ring;
    h_flex()
        .id(ElementId::Name(id.into()))
        .max_w_full()
        .gap(px(3.))
        .rounded(px(4.))
        .font_weight(FontWeight::MEDIUM)
        .tab_index(0)
        .focus_visible(move |s| s.bg(ring.opacity(0.25)))
        .when(pressable, gpui_kit::Styled::cursor_pointer)
        .child(div().flex_none().child(icon))
        .child(div().min_w_0().truncate().when(pressable, |el| el.hover(gpui_kit::Styled::underline)).child(label))
}

fn lucide(name: IconName) -> AnyElement {
    Icon::new(name).size(px(14.)).into_any_element()
}

fn text_of<'a>(record: &'a Value, key: &str) -> &'a str {
    record[key].as_str().unwrap_or_default()
}

impl ContextChips {
    fn chip(&self, reference: &ContextRef, cx: &mut App) -> AnyElement {
        let t = *crate::theme::tokens(cx);
        let record = self.content.record(&reference.id).filter(|record| record["kind"] == reference.kind.as_str());
        let id = format!("context:{}:{}", self.message, reference.id);
        let label: SharedString = reference.label.clone().into();
        let Some(record) = record else { return unavailable(id, label, &t) };
        let (this, pane) = (self.workspace.clone(), view_pane(&self.message).0);
        match reference.kind.as_str() {
            "mention" => {
                let path = text_of(record, "path").to_owned();
                let (asset, color) = crate::file_icons::file_icon(&path, t.dark);
                let ink = super::markdown_ui::document_ink(asset).unwrap_or_else(|| rgb(MENTION_INK));
                let icon = Icon::empty().path(asset).text_color(color).size(px(14.)).into_any_element();
                let tooltip: SharedString = path.clone().into();
                chip_shell(id, icon, label, true, &t)
                    .text_color(ink)
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(move |_, window, cx| {
                        let _ = this.update(cx, |w, cx| w.open_file_chip(pane, &path, None, false, window, cx));
                    })
                    .into_any_element()
            }
            "skill" => {
                let name = text_of(record, "name").to_owned();
                let label = if label.is_empty() { name.clone().into() } else { label };
                chip_shell(id, lucide(IconName::Box), label, true, &t)
                    .text_color(rgb(SKILL_INK))
                    .on_click(move |event, window, cx| {
                        let position = event.position();
                        let _ = this.update(cx, |w, cx| w.open_skill_menu(&name, Some(pane), position, window, cx));
                    })
                    .into_any_element()
            }
            "thread" => {
                let link = crate::thread_links::ThreadLink {
                    environment_id: Some(text_of(record, "environmentId").to_owned()).filter(|e| !e.is_empty()),
                    thread_id: text_of(record, "threadId").to_owned(),
                };
                let title = Some(text_of(record, "title").trim()).filter(|t| !t.is_empty()).map(str::to_owned);
                super::markdown_ui::thread_chip(&link, title.or(Some(reference.label.clone())), &self.workspace, cx)
            }
            "environment" => self.environment_chip(id, label, record, &t, cx),
            "image" => {
                let Some(image) = self.content.record_attachment(record) else { return unavailable(id, label, &t) };
                let icon = match thumbnail(&self.workspace, view_pane(&self.message).0, &image.id, cx) {
                    Some(picture) => {
                        img(picture).size(px(14.)).rounded(px(2.)).object_fit(ObjectFit::Cover).into_any_element()
                    }
                    None => lucide(IconName::Image),
                };
                let name = h_flex()
                    .min_w_0()
                    .items_baseline()
                    .gap(px(4.))
                    .child(div().min_w_0().max_w(px(288.)).truncate().child(text_of(record, "name").to_owned()))
                    .child(div().flex_none().text_size(px(10.)).child(logic::attachment_size(image.size)));
                let ring = t.ring;
                h_flex()
                    .id(ElementId::Name(id.into()))
                    .max_w_full()
                    .gap(px(3.))
                    .rounded(px(4.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(FILE_INK))
                    .cursor_pointer()
                    .tab_index(0)
                    .focus_visible(move |s| s.bg(ring.opacity(0.25)))
                    .child(div().flex_none().child(icon))
                    .child(name)
                    .on_click(open_handler(view_pane(&self.message).0, &this, image))
                    .into_any_element()
            }
            "file" => {
                let Some(file) = self.content.record_attachment(record) else { return unavailable(id, label, &t) };
                if file.media == Media::Audio {
                    // The recording shows as its player above the words.
                    return div().into_any_element();
                }
                let (icon, ink) = if file.media == Media::Video {
                    (lucide(IconName::Film), rgb(VIDEO_INK))
                } else {
                    let (asset, color) = crate::file_icons::file_icon(&file.name, t.dark);
                    let ink = super::markdown_ui::document_ink(asset).unwrap_or_else(|| rgb(FILE_INK));
                    (Icon::empty().path(asset).text_color(color).size(px(14.)).into_any_element(), ink)
                };
                let tooltip: SharedString = format!("{}\n{}", file.label(), logic::attachment_size(file.size)).into();
                chip_shell(id, icon, file.label().to_owned().into(), true, &t)
                    .text_color(ink)
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(open_handler(view_pane(&self.message).0, &this, file))
                    .into_any_element()
            }
            "review-comment" if logic::is_pull_request_summary(record) => self
                .pull_request_chip(id.clone(), record, cx)
                .unwrap_or_else(|| self.details_chip(id, reference, record, &t)),
            "terminal" | "element" | "review-comment" | "preview-annotation" => {
                self.details_chip(id, reference, record, &t)
            }
            _ => unavailable(id, label, &t),
        }
    }

    /// `EnvironmentContextChip`: the machine by the name this app knows it by, with its icon.
    fn environment_chip(&self, id: String, label: SharedString, record: &Value, t: &Tokens, cx: &App) -> AnyElement {
        let environment = text_of(record, "environmentId");
        let known = self.workspace.upgrade().and_then(|w| {
            let w = w.read(cx);
            let session = w.sessions().iter().find(|s| s.id.as_deref() == Some(environment))?;
            let name = session.server_info.environment_label.as_deref().map(str::trim).filter(|l| !l.is_empty());
            Some((name.map(str::to_owned), super::host_picker::machine_icon(session.server_info.machine_kind())))
        });
        let icon = known.as_ref().map_or(IconName::Server, |(_, icon)| *icon);
        let label = known.as_ref().and_then(|(name, _)| name.clone()).map_or(label, SharedString::from);
        let tooltip = if known.is_some() { "T3 Code environment" } else { "Machine not in this app" };
        chip_shell(id, lucide(icon), label, false, t)
            .text_color(rgb(ENVIRONMENT_INK))
            .tooltip(move |window, cx| Tooltip::new(tooltip).build(window, cx))
            .into_any_element()
    }

    /// `PullRequestChip`: the change request's state glyph and `#12`, which opens it, with its
    /// details on hover.
    fn pull_request_chip(&self, id: String, record: &Value, cx: &App) -> Option<AnyElement> {
        let pr: super::composer::PullRequestContext = serde::Deserialize::deserialize(&record["pullRequest"]).ok()?;
        let t = *crate::theme::tokens(cx);
        let (glyph, state, ..) = pr.state();
        let ink = match state {
            "Merged" => rgb(REVIEW_INK),
            "Closed" => rgb(PR_CLOSED_INK),
            "Draft" => rgb(PR_DRAFT_INK),
            _ => rgb(TERMINAL_INK),
        };
        let (url, details, this) = (pr.url.clone(), pr.clone(), self.workspace.clone());
        Some(
            chip_shell(id, lucide(glyph), logic::review_comment_label(record).into(), true, &t)
                .text_color(ink)
                .tooltip(move |window, cx| {
                    let pr = details.clone();
                    Tooltip::element(move |_, cx| super::composer::pull_request_details(&pr, crate::theme::tokens(cx)))
                        .build(window, cx)
                })
                .on_click(move |event, window, cx| {
                    let _ = this.update(cx, |w, cx| {
                        if !w.open_change_request_link(&url, cx) {
                            w.open_link(&url, event, window, cx);
                        }
                    });
                })
                .into_any_element(),
        )
    }

    /// `ContextChipPopover`: a terminal excerpt, browser element, review comment or preview
    /// annotation, whose details open over the chip.
    fn details_chip(&self, id: String, reference: &ContextRef, record: &Value, t: &Tokens) -> AnyElement {
        let (icon, ink, label) = match reference.kind.as_str() {
            "terminal" => (IconName::Terminal, TERMINAL_INK, reference.label.clone()),
            "review-comment" => {
                let icon = if logic::is_pull_request_summary(record) {
                    IconName::GitPullRequestArrow
                } else {
                    IconName::MessageCircle
                };
                (icon, REVIEW_INK, logic::review_comment_label(record))
            }
            _ => (IconName::MousePointerClick, ELEMENT_INK, reference.label.clone()),
        };
        // An empty terminal excerpt has nothing to show.
        if reference.kind == "terminal" && text_of(record, "text").is_empty() {
            return chip_shell(id, lucide(icon), label.into(), false, t).text_color(rgb(ink)).into_any_element();
        }
        let (this, record, content) = (self.workspace.clone(), record.clone(), self.content.clone());
        let pane = view_pane(&self.message).0;
        let kind = reference.kind.clone();
        chip_shell(id, lucide(icon), label.into(), true, t)
            .text_color(rgb(ink))
            .on_click(move |event, window, cx| {
                let (record, content, kind) = (record.clone(), content.clone(), kind.clone());
                let position = event.position();
                let _ =
                    this.update(cx, |w, cx| w.open_context_details(pane, kind, record, content, position, window, cx));
            })
            .into_any_element()
    }
}

/// `UnresolvedChip`: the label, dashed, saying why it shows nothing.
fn unavailable(id: String, label: SharedString, t: &Tokens) -> AnyElement {
    chip_shell(id, lucide(IconName::CircleDashed), label, false, t)
        .tooltip(|window, cx| Tooltip::new(UNAVAILABLE).build(window, cx))
        .into_any_element()
}

impl Workspace {
    /// Opens a context chip's details over it, as the web's popover does.
    fn open_context_details(
        &mut self,
        pane: ChatPane,
        kind: String,
        record: Value,
        content: Arc<UserContent>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.git_cwd();
        let view = cx.weak_entity();
        self.open_chat_menu(Some(pane), position, window, cx, move |menu: PopupMenu, _, _| {
            menu.item(
                PopupMenuItem::element(move |_, cx| {
                    let t = *crate::theme::tokens(cx);
                    let body = match kind.as_str() {
                        "terminal" => terminal_details(&record, &t),
                        "element" => element_details(&record, &t),
                        "review-comment" => review_details(&record, cwd.as_deref(), &t),
                        _ => annotation_details(pane, &record, &content, &view, &t, cx),
                    };
                    div().w(px(384.)).max_w_full().whitespace_normal().child(body)
                })
                .disabled(true),
            )
        });
    }
}

fn details_frame(t: &Tokens) -> gpui_kit::Div {
    v_flex()
        .w_full()
        .overflow_hidden()
        .rounded(px(8.))
        .border_1()
        .border_color(t.border.opacity(0.7))
        .bg(t.background.opacity(0.7))
}

/// Text as a `<pre>`: a line per row in the mono font, scrolled past `max_height`.
fn pre(id: String, text: &str, max_height: f32, t: &Tokens) -> AnyElement {
    let lines: Vec<String> = text.lines().map(|line| line.replace('\t', "    ")).collect();
    div()
        .id(ElementId::Name(id.into()))
        .max_h(px(max_height))
        .overflow_y_scroll()
        .rounded(px(4.))
        .bg(t.muted.opacity(0.6))
        .px(px(8.))
        .py(px(6.))
        .font_family(t.fonts.mono)
        .text_size(px(10.))
        .line_height(relative(1.625))
        .text_color(t.foreground)
        .children(lines.into_iter().map(|line| div().whitespace_nowrap().child(line)))
        .into_any_element()
}

/// `TerminalContextInlineChip`'s popover: the terminal and its lines, then the captured output.
fn terminal_details(record: &Value, t: &Tokens) -> AnyElement {
    let (start, end) =
        (record["lineStart"].as_u64().unwrap_or_default(), record["lineEnd"].as_u64().unwrap_or_default());
    let lines = if start == end { format!("Line {start}") } else { format!("Lines {start}–{end}") };
    details_frame(t)
        .child(
            h_flex()
                .gap(px(8.))
                .px(px(12.))
                .py(px(8.))
                .border_b_1()
                .border_color(t.border.opacity(0.7))
                .child(Icon::new(IconName::Terminal).size(px(16.)).flex_none().text_color(rgb(TERMINAL_INK)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(text_of(record, "terminalLabel").to_owned()),
                )
                .child(div().ml_auto().flex_none().text_xs().text_color(t.muted_foreground).child(lines)),
        )
        .child(div().p(px(4.)).child(pre(
            format!("terminal-text:{}", text_of(record, "contextId")),
            text_of(record, "text"),
            320.,
            t,
        )))
        .into_any_element()
}

/// `fileName:line`, an element's source in the app's code.
fn source_label(source: &Value) -> Option<String> {
    let file = source["fileName"].as_str().filter(|f| !f.is_empty())?;
    Some(match source["lineNumber"].as_u64() {
        Some(line) => format!("{file}:{line}"),
        None => file.to_owned(),
    })
}

/// An element's selector (or tag) with its source on the right.
fn selector_row(element: &Value, t: &Tokens) -> gpui_kit::Div {
    let selector = Some(text_of(element, "selector"))
        .filter(|s| !s.is_empty())
        .map_or_else(|| format!("<{}>", text_of(element, "tagName")), str::to_owned);
    h_flex()
        .min_w_0()
        .gap(px(8.))
        .text_xs()
        .child(div().min_w_0().truncate().font_family(t.fonts.mono).child(selector))
        .children(
            source_label(&element["source"])
                .map(|source| div().ml_auto().flex_none().text_color(t.muted_foreground).child(source)),
        )
}

/// `UserMessageElementDetails`: the page, the element, its HTML and its styles.
fn element_details(record: &Value, t: &Tokens) -> AnyElement {
    let id = text_of(record, "contextId");
    let title =
        Some(text_of(record, "pageTitle").trim()).filter(|t| !t.is_empty()).unwrap_or(text_of(record, "pageUrl"));
    let html = text_of(record, "htmlPreview").trim();
    let styles = text_of(record, "styles").trim();
    details_frame(t)
        .child(
            v_flex()
                .px(px(12.))
                .py(px(10.))
                .border_b_1()
                .border_color(t.border.opacity(0.7))
                .child(div().truncate().text_xs().font_weight(FontWeight::MEDIUM).child(title.to_owned()))
                .child(
                    div()
                        .truncate()
                        .text_size(px(10.))
                        .text_color(t.muted_foreground)
                        .child(text_of(record, "pageUrl").to_owned()),
                ),
        )
        .child(
            v_flex()
                .gap(px(8.))
                .px(px(12.))
                .py(px(10.))
                .child(selector_row(record, t))
                .when(!html.is_empty(), |el| el.child(pre(format!("element-html:{id}"), html, 160., t)))
                .when(!styles.is_empty(), |el| el.child(pre(format!("element-styles:{id}"), styles, 128., t))),
        )
        .into_any_element()
}

/// `UserMessageReviewCommentCard`: the file, its section and lines, the comment, then the code
/// it was left on.
fn review_details(record: &Value, cwd: Option<&str>, t: &Tokens) -> AnyElement {
    let path = text_of(record, "filePath");
    let shown = cwd
        .and_then(|cwd| path.strip_prefix(cwd.trim_end_matches(['/', '\\'])))
        .and_then(|rest| rest.strip_prefix(['/', '\\']))
        .unwrap_or(path);
    let comment = text_of(record, "text");
    let diff = text_of(record, "diff").trim_end_matches('\n');
    let id = text_of(record, "contextId");
    details_frame(t)
        .gap(px(8.))
        .p(px(12.))
        .child(
            v_flex().gap(px(4.)).child(div().text_xs().font_weight(FontWeight::MEDIUM).child(shown.to_owned())).child(
                div().text_size(px(11.)).text_color(t.muted_foreground).child(format!(
                    "{} · {}",
                    text_of(record, "sectionTitle"),
                    text_of(record, "rangeLabel")
                )),
            ),
        )
        .when(!comment.is_empty(), |el| el.child(div().text_sm().child(comment.to_owned())))
        .when(!diff.trim().is_empty(), |el| el.child(pre(format!("review-diff:{id}"), diff, 240., t)))
        .into_any_element()
}

/// `UserMessagePreviewAnnotationDetails`: the annotated crop, the page, the comment, what it
/// targets and the elements it picked.
fn annotation_details(
    pane: ChatPane,
    record: &Value,
    content: &UserContent,
    view: &WeakEntity<Workspace>,
    t: &Tokens,
    cx: &mut App,
) -> AnyElement {
    let screenshot = content.annotation_image(record);
    let crop = match screenshot.and_then(|image| thumbnail(view, pane, &image.id, cx).map(|picture| (image, picture))) {
        Some((image, picture)) => button(div().id(ElementId::Name(format!("annotation-crop:{}", image.id).into())), t)
            .max_h(px(256.))
            .w_full()
            .overflow_hidden()
            .border_b_1()
            .border_color(t.border.opacity(0.7))
            .bg(t.muted)
            .child(img(picture).w_full().max_h(px(256.)).object_fit(ObjectFit::Contain))
            .on_click(open_handler(pane, view, image))
            .into_any_element(),
        None => div()
            .border_b_1()
            .border_color(t.border.opacity(0.7))
            .bg(t.muted.opacity(0.4))
            .px(px(12.))
            .py(px(8.))
            .text_xs()
            .text_color(t.muted_foreground)
            .child("Screenshot unavailable")
            .into_any_element(),
    };
    let title = [text_of(record, "pageTitle").trim(), text_of(record, "pageUrl")]
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or("Preview annotation")
        .to_owned();
    let comment = text_of(record, "comment");
    let target = text_of(record, "targetSummary");
    let changes = record["styleChanges"].as_array().map_or(0, Vec::len);
    let elements = record["elements"].as_array().cloned().unwrap_or_default();
    let id = text_of(record, "contextId").to_owned();
    let element_rows: Vec<AnyElement> = elements
        .iter()
        .enumerate()
        .map(|(index, element)| {
            let html = text_of(element, "htmlPreview").trim();
            v_flex()
                .min_w_0()
                .gap(px(4.))
                .child(selector_row(element, t))
                .when(!html.is_empty(), |el| el.child(pre(format!("annotation-html:{id}:{index}"), html, 96., t)))
                .into_any_element()
        })
        .collect();
    details_frame(t)
        .child(crop)
        .child(
            v_flex()
                .min_w_0()
                .px(px(12.))
                .py(px(10.))
                .child(div().text_xs().font_weight(FontWeight::MEDIUM).child(title))
                .when(!comment.is_empty(), |el| el.child(div().mt(px(4.)).text_sm().child(comment.to_owned())))
                .child(
                    h_flex()
                        .mt(px(4.))
                        .gap(px(8.))
                        .text_size(px(10.))
                        .text_color(t.muted_foreground)
                        .when(!target.is_empty(), |el| el.child(div().min_w_0().truncate().child(target.to_owned())))
                        .when(changes > 0, |el| {
                            el.child(
                                h_flex()
                                    .flex_none()
                                    .gap(px(4.))
                                    .child(Icon::new(IconName::Paintbrush).size(px(12.)))
                                    .child(changes.to_string()),
                            )
                        }),
                )
                .when(!element_rows.is_empty(), |el| {
                    el.child(
                        v_flex()
                            .mt(px(8.))
                            .gap(px(8.))
                            .pt(px(8.))
                            .border_t_1()
                            .border_color(t.border.opacity(0.6))
                            .children(element_rows),
                    )
                }),
        )
        .into_any_element()
}
