//! `ExpandedImageDialog` with its `ZoomableImage`: a gallery of images over the window, with
//! previous/next, zoom, pan, and the image's `MediaActions` menu. Also the zoom math the file
//! panel's `PreviewImageContent` uses.

use std::cell::Cell;
use std::collections::HashMap;
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{Icon, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, CursorStyle, DismissEvent, Entity, FocusHandle,
    Focusable as _, Image, ImageFormat, InteractiveElement as _, IntoElement as _, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, Point, ScrollEvent, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, anchored, canvas, deferred, div, img, point,
    px, size,
};

use super::Workspace;

/// `MAX_ZOOM`: the viewer zooms up to eight times the fitted size.
const MAX_ZOOM: f32 = 8.;
/// How far an arrow key pans a zoomed image.
const PAN_STEP: f32 = 40.;
/// A press that moves further than this pans instead of clicking.
const DRAG_SLOP: f32 = 4.;

/// Where an image came from, as its menu copies it (`MediaReference`).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Reference {
    /// A file: its full path and, inside the workspace, the relative one, both as the host writes them.
    File {
        full: String,
        relative: Option<String>,
    },
    Url(String),
}

/// `mediaFileReference` for an absolute `path`: relative when it lies inside `root`, with
/// backslashes for a Windows host (`formatFilePathForClipboard`).
pub(super) fn file_reference(path: &str, root: Option<&str>) -> Reference {
    let windows = path.starts_with("\\\\") || path.as_bytes().get(1) == Some(&b':');
    // A Windows host takes either separator.
    let normalize = |text: &str| if windows { text.replace('\\', "/") } else { text.to_owned() };
    let format = |text: &str| if windows { text.replace('/', "\\") } else { text.to_owned() };
    let full = normalize(path);
    let relative = root
        .map(|root| normalize(root).trim_end_matches('/').to_owned())
        .filter(|root| !root.is_empty())
        .and_then(|root| full.strip_prefix(&root)?.strip_prefix('/').map(|rest| rest.trim_start_matches('/')))
        .filter(|rest| !rest.is_empty())
        .map(format);
    Reference::File { full: format(&full), relative }
}

/// One image of a gallery (`ExpandedImageItem`).
#[derive(Clone)]
pub(super) struct ViewerImage {
    pub image: Arc<Image>,
    pub name: SharedString,
    pub reference: Option<Reference>,
}

impl ViewerImage {
    fn svg(&self) -> bool {
        self.image.format == ImageFormat::Svg
    }

    /// The name Save offers (`mediaFileName`): the file's or URL's own name, else the image's
    /// with its format's extension.
    fn file_name(&self) -> String {
        let from_reference = match &self.reference {
            Some(Reference::File { full, .. }) => full.rsplit(['/', '\\']).next(),
            Some(Reference::Url(url)) => url.split(['?', '#']).next().and_then(|path| path.rsplit('/').next()),
            None => None,
        };
        match from_reference.filter(|name| !name.is_empty()) {
            Some(name) => name.replace("%20", " "),
            None if self.name.contains('.') => self.name.to_string(),
            None => format!("{}.{}", self.name, extension(self.image.format)),
        }
    }
}

fn extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
        ImageFormat::Ico => "ico",
        ImageFormat::Pnm => "pnm",
    }
}

/// The image's own width and height (`naturalWidth`), read from its header, or for an SVG from
/// its size attributes and view box.
fn natural_size(image: &Image) -> Option<Size<f32>> {
    let (width, height) = if image.format == ImageFormat::Svg {
        let tree = usvg::Tree::from_data(&image.bytes, &usvg::Options::default()).ok()?;
        (tree.size().width(), tree.size().height())
    } else {
        let (width, height) =
            image::ImageReader::new(Cursor::new(&image.bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
        #[expect(clippy::cast_precision_loss, reason = "image sides are far below f32's exact-integer limit")]
        (width as f32, height as f32)
    };
    (width > 0. && height > 0.).then_some(size(width, height))
}

/// The viewer's room for an image in a window of `window` size: `--media-width` across and
/// `ZoomableImage`'s `maxHeight` down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Stage {
    width: f32,
    height: f32,
}

impl Stage {
    pub(super) fn new(window: Size<f32>) -> Self {
        let gutter = if window.width >= 640. { 96. } else { 0. };
        Self {
            width: (window.width * 0.92 - gutter).max(1.),
            height: (window.height * 0.86).min(window.height - 160.).max(1.),
        }
    }

    /// The image's size at zoom 1: as large as fits, never past its own size unless it is an SVG,
    /// which scales without loss.
    pub(super) fn fitted(self, natural: Size<f32>, svg: bool) -> Size<f32> {
        let limit = if svg { f32::INFINITY } else { 1. };
        let fit = limit.min(self.width / natural.width.max(1.)).min(self.height / natural.height.max(1.));
        size(natural.width * fit, natural.height * fit)
    }
}

/// The zoom (1 to [`MAX_ZOOM`] times the fitted size) and the scroll of the viewport over it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Zoom {
    pub zoom: f32,
    pub scroll: Point<f32>,
}

impl Default for Zoom {
    fn default() -> Self {
        Self { zoom: 1., scroll: point(0., 0.) }
    }
}

/// An arrow key's pan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Pan {
    Left,
    Right,
    Up,
    Down,
}

impl Zoom {
    pub(super) fn content(self, fitted: Size<f32>) -> Size<f32> {
        size(fitted.width * self.zoom, fitted.height * self.zoom)
    }

    /// The visible box: the image, up to the stage.
    pub(super) fn viewport(self, fitted: Size<f32>, stage: Stage) -> Size<f32> {
        let content = self.content(fitted);
        size(content.width.min(stage.width), content.height.min(stage.height))
    }

    fn clamp_scroll(&mut self, fitted: Size<f32>, stage: Stage) {
        let (content, viewport) = (self.content(fitted), self.viewport(fitted, stage));
        self.scroll.x = self.scroll.x.clamp(0., (content.width - viewport.width).max(0.));
        self.scroll.y = self.scroll.y.clamp(0., (content.height - viewport.height).max(0.));
    }

    /// `changeZoom`: zooms to `next` keeping the image under `at` (in the viewport, its center by
    /// default) in place. The viewport stays centered, so it grows around its middle. Returns
    /// whether the zoom changed.
    pub(super) fn set(&mut self, next: f32, at: Option<Point<f32>>, fitted: Size<f32>, stage: Stage) -> bool {
        let next = next.clamp(1., MAX_ZOOM);
        if (next - self.zoom).abs() < f32::EPSILON {
            return false;
        }
        let before = self.viewport(fitted, stage);
        let at = at.unwrap_or(point(before.width / 2., before.height / 2.));
        let anchor = point((self.scroll.x + at.x) / self.zoom, (self.scroll.y + at.y) / self.zoom);
        self.zoom = next;
        let after = self.viewport(fitted, stage);
        // The point's place in the moved viewport.
        let at = point(at.x + (after.width - before.width) / 2., at.y + (after.height - before.height) / 2.);
        self.scroll = point(anchor.x * next - at.x, anchor.y * next - at.y);
        self.clamp_scroll(fitted, stage);
        true
    }

    /// Click or Enter: zooms to 2× (at `at`), or back to fit.
    pub(super) fn toggle(&mut self, at: Option<Point<f32>>, fitted: Size<f32>, stage: Stage) {
        let next = if self.zoom > 1. { 1. } else { 2. };
        self.set(next, at, fitted, stage);
    }

    /// `pan`: an arrow key scrolls a zoomed image. Left and Right pan only when the image is wider
    /// than its viewport, so they can still change images. Returns whether the key panned.
    pub(super) fn pan(&mut self, pan: Pan, fitted: Size<f32>, stage: Stage) -> bool {
        if self.zoom <= 1. {
            return false;
        }
        let horizontal = matches!(pan, Pan::Left | Pan::Right);
        if horizontal && self.content(fitted).width <= self.viewport(fitted, stage).width {
            return false;
        }
        match pan {
            Pan::Left => self.scroll.x -= PAN_STEP,
            Pan::Right => self.scroll.x += PAN_STEP,
            Pan::Up => self.scroll.y -= PAN_STEP,
            Pan::Down => self.scroll.y += PAN_STEP,
        }
        self.clamp_scroll(fitted, stage);
        true
    }

    /// A drag that started with the scroll at `from` and has moved `delta`.
    pub(super) fn drag(&mut self, from: Point<f32>, delta: Point<f32>, fitted: Size<f32>, stage: Stage) {
        self.scroll = point(from.x - delta.x, from.y - delta.y);
        self.clamp_scroll(fitted, stage);
    }
}

/// `PreviewImageContent`'s fit: the image within the panel less its 16px padding, never enlarged.
pub(in crate::app) fn preview_fit(natural: Size<f32>, viewport: Size<f32>) -> f32 {
    1f32.min((viewport.width - 32.).max(1.) / natural.width).min((viewport.height - 32.).max(1.) / natural.height)
}

/// `PreviewImageContent`'s zoom buttons: the scale times `factor`, kept between 5% and 800%.
pub(in crate::app) fn preview_zoom(scale: f32, factor: f32) -> f32 {
    (scale * factor).clamp(PREVIEW_MIN_ZOOM, PREVIEW_MAX_ZOOM)
}

pub(in crate::app) const PREVIEW_MIN_ZOOM: f32 = 0.05;
pub(in crate::app) const PREVIEW_MAX_ZOOM: f32 = MAX_ZOOM;
pub(in crate::app) const PREVIEW_ZOOM_STEP: f32 = 1.25;

/// A file panel image's zoom (`PreviewImageContent`): `None` fits it.
#[derive(Default)]
pub(in crate::app) struct PreviewZoom {
    pub zoom: Option<f32>,
    /// The scroll area's size, measured as it paints.
    pub viewport: Rc<Cell<Option<Size<f32>>>>,
}

/// An image's natural size: measuring, known, or unreadable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::app) enum Measured {
    Pending,
    Size(Size<f32>),
    Failed,
}

impl Measured {
    pub(in crate::app) fn size(self) -> Option<Size<f32>> {
        match self {
            Self::Size(size) => Some(size),
            Self::Pending | Self::Failed => None,
        }
    }
}

struct Drag {
    from_mouse: Point<Pixels>,
    from_scroll: Point<f32>,
    moved: bool,
}

/// The open viewer.
pub(super) struct ImageViewer {
    items: Vec<ViewerImage>,
    index: usize,
    zoom: Zoom,
    /// The window size the zoom was set at: a resize refits the image, as on the web.
    window: Size<f32>,
    drag: Option<Drag>,
    /// A drag just ended, so the click its release makes is not a click.
    swallow_click: bool,
    focus: FocusHandle,
    close_focus: FocusHandle,
    image_focus: FocusHandle,
    restore: Option<FocusHandle>,
    menu: Option<(Entity<PopupMenu>, Point<Pixels>, Subscription)>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// The viewer and the natural sizes measured for it and the file panel, by image id.
#[derive(Default)]
pub(super) struct ViewerUi {
    open: Option<ImageViewer>,
    sizes: HashMap<u64, Measured>,
}

impl ViewerUi {
    /// Closes the viewer without moving focus, for a view that is going away.
    pub(super) fn dismiss(&mut self) {
        self.open = None;
    }
}

impl Workspace {
    /// Opens the viewer on `items`, at `index` (`openExpandedImagePreview`).
    pub(super) fn open_image_viewer(
        &mut self,
        items: Vec<ViewerImage>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if index >= items.len() {
            return;
        }
        let restore = window.focused(cx).or_else(|| self.image_viewer.open.as_ref().and_then(|v| v.restore.clone()));
        let focus = cx.focus_handle();
        let close_focus = cx.focus_handle().tab_index(0).tab_stop(true);
        let image_focus = cx.focus_handle().tab_index(0).tab_stop(true);
        // `initialFocus={closeButtonRef}`.
        close_focus.focus(window, cx);
        self.image_viewer.open = Some(ImageViewer {
            items,
            index,
            zoom: Zoom::default(),
            window: Size::default(),
            drag: None,
            swallow_click: false,
            focus,
            close_focus,
            image_focus,
            restore,
            menu: None,
            bounds: Rc::default(),
        });
        cx.notify();
    }

    pub(super) fn image_viewer_open(&self) -> bool {
        self.image_viewer.open.is_some()
    }

    /// Closes the viewer and gives focus back to what opened it.
    pub(super) fn close_image_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewer) = self.image_viewer.open.take() else { return };
        if let Some(restore) = viewer.restore {
            restore.focus(window, cx);
        }
        cx.notify();
    }

    /// `image`'s natural size; the first ask measures it off the UI thread.
    pub(in crate::app) fn image_natural_size(&mut self, image: &Arc<Image>, cx: &mut Context<Self>) -> Measured {
        if let Some(measured) = self.image_viewer.sizes.get(&image.id) {
            return *measured;
        }
        self.image_viewer.sizes.insert(image.id, Measured::Pending);
        let (id, image) = (image.id, image.clone());
        cx.spawn(async move |this, cx| {
            let size = cx.background_executor().spawn(async move { natural_size(&image) }).await;
            let _ = this.update(cx, |this, cx| {
                this.image_viewer.sizes.insert(id, size.map_or(Measured::Failed, Measured::Size));
                cx.notify();
            });
        })
        .detach();
        Measured::Pending
    }

    fn navigate_image(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(viewer) = self.image_viewer.open.as_mut() else { return };
        let count = viewer.items.len();
        if count <= 1 {
            return;
        }
        viewer.index = (viewer.index + count).wrapping_add_signed(step) % count;
        viewer.zoom = Zoom::default();
        viewer.drag = None;
        cx.notify();
    }

    /// The current image's fitted size and stage once its size is known, or why it is not.
    fn viewer_frame(&mut self, cx: &mut Context<Self>) -> Result<(Size<f32>, Stage), Measured> {
        let Some(viewer) = self.image_viewer.open.as_ref() else { return Err(Measured::Pending) };
        let Some(item) = viewer.items.get(viewer.index).cloned() else { return Err(Measured::Failed) };
        let stage = Stage::new(viewer.window);
        let measured = self.image_natural_size(&item.image, cx);
        let natural = measured.size().ok_or(measured)?;
        Ok((stage.fitted(natural, item.svg()), stage))
    }

    fn zoom_viewer(
        &mut self,
        change: impl FnOnce(&mut Zoom, Size<f32>, Stage) -> bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Ok((fitted, stage)) = self.viewer_frame(cx) else { return false };
        let Some(viewer) = self.image_viewer.open.as_mut() else { return false };
        let changed = change(&mut viewer.zoom, fitted, stage);
        if changed {
            cx.notify();
        }
        changed
    }

    /// The point `position` (in the window) inside the image's viewport.
    fn viewer_point(&self, position: Point<Pixels>) -> Option<Point<f32>> {
        let bounds = self.image_viewer.open.as_ref()?.bounds.get()?;
        Some(point(f32::from(position.x - bounds.origin.x), f32::from(position.y - bounds.origin.y)))
    }

    /// Keys anywhere in the viewer (`onKeyDown`): Escape closes, arrows pan a zoomed image or
    /// change images, and with the image focused, plus, minus and 0 zoom.
    fn viewer_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewer) = self.image_viewer.open.as_ref() else { return };
        if viewer.menu.is_some() {
            return;
        }
        let image_focused = viewer.image_focus.is_focused(window);
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let plain = !modifiers.control && !modifiers.platform && !modifiers.alt;
        let pan = match keystroke.key.as_str() {
            "left" => Some(Pan::Left),
            "right" => Some(Pan::Right),
            "up" => Some(Pan::Up),
            "down" => Some(Pan::Down),
            _ => None,
        };
        let bounds = viewer.bounds.get();
        if let Some(pan) = pan.filter(|_| plain) {
            if !self.zoom_viewer(|zoom, fitted, stage| zoom.pan(pan, fitted, stage), cx) {
                match pan {
                    Pan::Left => self.navigate_image(-1, cx),
                    Pan::Right => self.navigate_image(1, cx),
                    Pan::Up | Pan::Down => return,
                }
            }
            cx.stop_propagation();
            return;
        }
        match keystroke.key.as_str() {
            "escape" => self.close_image_viewer(window, cx),
            "+" | "=" if image_focused && plain => {
                self.zoom_viewer(|zoom, fitted, stage| zoom.set(zoom.zoom * 1.5, None, fitted, stage), cx);
            }
            "-" if image_focused && plain => {
                self.zoom_viewer(|zoom, fitted, stage| zoom.set(zoom.zoom / 1.5, None, fitted, stage), cx);
            }
            "0" if image_focused && plain => {
                self.zoom_viewer(|zoom, fitted, stage| zoom.set(1., None, fitted, stage), cx);
            }
            "menu" | "f10" if image_focused && (keystroke.key == "menu" || modifiers.shift) => {
                let Some(bounds) = bounds else { return };
                self.open_viewer_menu(bounds.bottom_left(), window, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The image's `MediaActions` menu: copy its path or URL, save it, or copy the image.
    fn open_viewer_menu(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewer) = self.image_viewer.open.as_ref() else { return };
        if viewer.menu.is_some() {
            return;
        }
        let Some(item) = viewer.items.get(viewer.index).cloned() else { return };
        let menu = PopupMenu::build(window, cx, move |mut menu, _, _| {
            let copy = |text: String, title: &'static str| {
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    window.push_notification(Notification::success(text.clone()).title(title), cx);
                }
            };
            match item.reference.clone() {
                Some(Reference::File { full, relative }) => {
                    menu = menu.item(PopupMenuItem::new("Copy full path").on_click(copy(full, "Path copied")));
                    if let Some(relative) = relative {
                        menu =
                            menu.item(PopupMenuItem::new("Copy relative path").on_click(copy(relative, "Path copied")));
                    }
                }
                Some(Reference::Url(url)) => {
                    menu = menu.item(PopupMenuItem::new("Copy URL").on_click(copy(url, "URL copied")));
                }
                None => {}
            }
            let save = item.clone();
            menu = menu
                .item(PopupMenuItem::new("Save image").on_click(move |_, window, cx| save_image(&save, window, cx)));
            let image = item.image;
            menu.item(PopupMenuItem::new("Copy image").on_click(move |_, window, cx| {
                cx.write_to_clipboard(ClipboardItem::new_image(&image));
                window.push_notification(Notification::success("Image copied"), cx);
            }))
        });
        menu.focus_handle(cx).focus(window, cx);
        let dismiss = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, window, cx| {
            if let Some(viewer) = this.image_viewer.open.as_mut() {
                viewer.menu = None;
                viewer.image_focus.focus(window, cx);
            }
            cx.notify();
        });
        if let Some(viewer) = self.image_viewer.open.as_mut() {
            viewer.menu = Some((menu, position, dismiss));
        }
        cx.notify();
    }

    /// The viewer over the window, when one is open.
    pub(super) fn render_image_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let titlebar = f32::from(super::titlebar::TITLEBAR_HEIGHT);
        let viewport = window.viewport_size();
        let room = size(f32::from(viewport.width), (f32::from(viewport.height) - titlebar).max(1.));
        let viewer = self.image_viewer.open.as_mut()?;
        if viewer.window != room {
            viewer.window = room;
            viewer.zoom = Zoom::default();
        }
        let frame = self.viewer_frame(cx);
        let viewer = self.image_viewer.open.as_ref()?;
        let item = viewer.items.get(viewer.index)?.clone();
        let count = viewer.items.len();
        let (zoom, dragging, bounds) = (viewer.zoom, viewer.drag.is_some(), viewer.bounds.clone());
        let t = *crate::theme::tokens(cx);
        let white = gpui_kit::white();
        let wide = room.width >= 640.;

        let image = match frame {
            Ok((fitted, stage)) => {
                let (content, visible) = (zoom.content(fitted), zoom.viewport(fitted, stage));
                div()
                    .id("image-viewer-image")
                    .track_focus(&viewer.image_focus)
                    .relative()
                    .flex_none()
                    .w(px(visible.width))
                    .h(px(visible.height))
                    .overflow_hidden()
                    .rounded(px(8.))
                    .bg(t.background)
                    .border_1()
                    .border_color(t.border.opacity(0.7))
                    .shadow_2xl()
                    .focus_visible(move |style| style.border_2().border_color(t.ring))
                    .cursor(match (zoom.zoom > 1., dragging) {
                        (true, true) => CursorStyle::ClosedHand,
                        (true, false) => CursorStyle::OpenHand,
                        (false, _) => CursorStyle::PointingHand,
                    })
                    .child(
                        img(item.image.clone())
                            .absolute()
                            .left(px(-zoom.scroll.x))
                            .top(px(-zoom.scroll.y))
                            .w(px(content.width))
                            .h(px(content.height)),
                    )
                    .child(
                        canvas(move |element, _, _| bounds.set(Some(element)), |_, (), _, _| {}).absolute().size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            let Some(viewer) = this.image_viewer.open.as_mut() else { return };
                            viewer.image_focus.focus(window, cx);
                            viewer.swallow_click = false;
                            if viewer.zoom.zoom > 1. {
                                viewer.drag = Some(Drag {
                                    from_mouse: event.position,
                                    from_scroll: viewer.zoom.scroll,
                                    moved: false,
                                });
                                cx.notify();
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_viewer_menu(event.position, window, cx);
                        }),
                    )
                    .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                        cx.stop_propagation();
                        let delta = f32::from(event.delta.pixel_delta(px(16.)).y);
                        if delta == 0. {
                            return;
                        }
                        let rate = if event.modifiers.control { 0.01 } else { 0.002 };
                        let at = this.viewer_point(event.position);
                        this.zoom_viewer(
                            |zoom, fitted, stage| zoom.set(zoom.zoom * (-delta * rate).exp(), at, fitted, stage),
                            cx,
                        );
                    }))
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        let Some(viewer) = this.image_viewer.open.as_mut() else { return };
                        if std::mem::take(&mut viewer.swallow_click) {
                            return;
                        }
                        let at = match event {
                            ClickEvent::Mouse(click) if click.up.button != MouseButton::Left => return,
                            ClickEvent::Mouse(click) if click.down.click_count > 1 => return,
                            ClickEvent::Keyboard(_) => None,
                            _ => this.viewer_point(event.position()),
                        };
                        this.zoom_viewer(
                            |zoom, fitted, stage| {
                                zoom.toggle(at, fitted, stage);
                                true
                            },
                            cx,
                        );
                    }))
                    .into_any_element()
            }
            // `ExpandedMediaFailure`.
            Err(Measured::Failed) => div()
                .h(px(192.))
                .w(px(Stage::new(room).width.min(512.)))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .p(px(24.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.border.opacity(0.7))
                .bg(gpui_kit::black())
                .text_sm()
                .text_center()
                .text_color(white)
                .shadow_2xl()
                .child("Image unavailable. The file may have been moved or deleted.")
                .into_any_element(),
            Err(_) => div().size(px(32.)).into_any_element(),
        };

        let button = |id: &'static str, icon: IconName, icon_size: f32| {
            div()
                .id(id)
                .tab_index(0)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .child(Icon::new(icon).size(px(icon_size)))
        };
        let close = button("image-viewer-close", IconName::X, 16.)
            .track_focus(&viewer.close_focus)
            .absolute()
            .top_0()
            .right_0()
            .size(px(28.))
            .bg(gpui_kit::black().opacity(0.65))
            .text_color(white)
            .border_1()
            .border_color(white.opacity(0.2))
            .shadow_sm()
            .hover(|style| style.bg(gpui_kit::black().opacity(0.8)))
            .focus_visible(move |style| style.border_color(white))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.close_image_viewer(window, cx);
            }));
        let navigation = |id: &'static str, icon: IconName, step: isize| {
            button(id, icon, 20.)
                .absolute()
                .size(px(32.))
                .text_color(white.opacity(0.9))
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .hover(move |style| style.bg(white.opacity(0.1)).text_color(white))
                .focus_visible(move |style| style.border_color(white))
                .map(|el| if wide { el.top(gpui_kit::relative(0.5)).mt(px(-16.)) } else { el.bottom_0() })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.navigate_image(step, cx);
                }))
        };
        let caption =
            if count > 1 { format!("{} ({}/{count})", item.name, viewer.index + 1) } else { item.name.to_string() };
        let column = v_flex()
            .id("image-viewer-media")
            .relative()
            .max_w(px(Stage::new(room).width))
            .items_center()
            // A click on the image or its caption stays; one beside them closes the viewer.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(image)
            .child(
                h_flex()
                    .mt(px(8.))
                    .max_w_full()
                    .justify_center()
                    .text_xs()
                    .text_color(white.opacity(0.8))
                    .when(!wide, |el| el.px(px(40.)))
                    .child(div().min_w_0().truncate().child(caption)),
            );
        let popup = div()
            .id("image-viewer-popup")
            .relative()
            .w(px(room.width * 0.92))
            .h_full()
            .max_h(px(f32::from(viewport.height) * 0.92))
            .flex()
            .items_center()
            .justify_center()
            .child(column)
            .child(close)
            .when(count > 1, |el| {
                el.child(navigation("image-viewer-previous", IconName::ChevronLeft, -1).left_0())
                    .child(navigation("image-viewer-next", IconName::ChevronRight, 1).right_0())
            })
            .focus_trap("image-viewer-trap", &viewer.focus);
        let menu = viewer.menu.as_ref().map(|(menu, position, _)| {
            deferred(anchored().position(*position).snap_to_window_with_margin(px(8.)).child(menu.clone()))
                .with_priority(3)
        });
        Some(
            div()
                .id("image-viewer")
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
                .on_key_down(cx.listener(Self::viewer_key_down))
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    let Some(viewer) = this.image_viewer.open.as_mut() else { return };
                    let Some(drag) = viewer.drag.as_mut() else { return };
                    if event.pressed_button != Some(MouseButton::Left) {
                        viewer.drag = None;
                        cx.notify();
                        return;
                    }
                    let delta = event.position - drag.from_mouse;
                    let delta = point(f32::from(delta.x), f32::from(delta.y));
                    if delta.x.hypot(delta.y) > DRAG_SLOP {
                        drag.moved = true;
                    }
                    let from = drag.from_scroll;
                    this.zoom_viewer(
                        |zoom, fitted, stage| {
                            zoom.drag(from, delta, fitted, stage);
                            true
                        },
                        cx,
                    );
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        let Some(viewer) = this.image_viewer.open.as_mut() else { return };
                        if let Some(drag) = viewer.drag.take() {
                            viewer.swallow_click = drag.moved;
                            cx.notify();
                        }
                    }),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    let swallowed =
                        this.image_viewer.open.as_mut().is_some_and(|v| std::mem::take(&mut v.swallow_click));
                    if !swallowed {
                        this.close_image_viewer(window, cx);
                    }
                }))
                .child(popup)
                .children(menu)
                .into_any_element(),
        )
    }
}

/// Save image: writes the bytes where the user picks, in their Downloads folder by default.
fn save_image(item: &ViewerImage, window: &mut Window, cx: &mut App) {
    let directory = std::env::home_dir().map(|home| home.join("Downloads")).unwrap_or_default();
    let picked = cx.prompt_for_new_path(&directory, Some(&item.file_name()));
    let image = item.image.clone();
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(path))) = picked.await else { return };
            let written = cx.background_executor().spawn(async move { std::fs::write(path, &image.bytes) }).await;
            let _ = cx.update(|window, cx| {
                let toast = match written {
                    Ok(()) => Notification::success("Image saved"),
                    Err(error) => Notification::error(error.to_string()).title("Could not save image"),
                };
                window.push_notification(toast, cx);
            });
        })
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Size<f32> = Size { width: 1000., height: 800. };

    #[test]
    fn the_stage_leaves_the_web_margins() {
        let stage = Stage::new(WINDOW);
        assert_eq!((stage.width, stage.height), (1000. * 0.92 - 96., 640.));
        let narrow = Stage::new(size(500., 300.));
        assert_eq!((narrow.width, narrow.height), (460., 140.), "no gutter under 640px; the 160px margin wins");
    }

    #[test]
    fn images_fit_without_enlarging_unless_vector() {
        let stage = Stage::new(WINDOW);
        assert_eq!(stage.fitted(size(200., 100.), false), size(200., 100.), "a small raster keeps its size");
        assert_eq!(stage.fitted(size(1648., 400.), false), size(824., 200.), "a wide image fits the width");
        assert_eq!(stage.fitted(size(100., 1280.), false), size(50., 640.), "a tall image fits the height");
        assert_eq!(stage.fitted(size(32., 32.), true), size(640., 640.), "an SVG grows to fill");
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let stage = Stage::new(WINDOW);
        let fitted = stage.fitted(size(800., 600.), false);
        let mut zoom = Zoom::default();
        // At 2x the 800x600 image is 1600x1200 in an 824x640 viewport that grew around its middle.
        assert!(zoom.set(2., Some(point(200., 150.)), fitted, stage));
        let viewport = zoom.viewport(fitted, stage);
        assert_eq!(viewport, size(824., 640.));
        let at = point(200. + (824. - 800.) / 2., 150. + (640. - 600.) / 2.);
        assert_eq!(zoom.scroll, point(400. - at.x, 300. - at.y));
        assert!(!zoom.set(2., None, fitted, stage), "the same zoom changes nothing");
        zoom.set(1., None, fitted, stage);
        assert_eq!(zoom.scroll, point(0., 0.), "back to fit, nothing scrolls");
        assert!(!zoom.set(0.5, None, fitted, stage), "fit is the least zoom");
        zoom.set(100., None, fitted, stage);
        assert_eq!(zoom.zoom, MAX_ZOOM);
        let content = zoom.content(fitted);
        assert_eq!(zoom.scroll, point((content.width - 824.) / 2., (content.height - 640.) / 2.), "the center stays");
    }

    #[test]
    fn toggling_zooms_to_double_and_back() {
        let stage = Stage::new(WINDOW);
        let fitted = stage.fitted(size(800., 600.), false);
        let mut zoom = Zoom::default();
        zoom.toggle(None, fitted, stage);
        assert_eq!(zoom.zoom, 2.);
        zoom.toggle(None, fitted, stage);
        assert_eq!(zoom, Zoom::default());
    }

    #[test]
    fn arrows_pan_only_a_zoomed_image_that_overflows() {
        let stage = Stage::new(WINDOW);
        let fitted = stage.fitted(size(800., 600.), false);
        let mut zoom = Zoom::default();
        assert!(!zoom.pan(Pan::Right, fitted, stage), "at fit the arrows change images");
        zoom.set(2., Some(point(0., 0.)), fitted, stage);
        let scroll = zoom.scroll;
        assert!(zoom.pan(Pan::Right, fitted, stage));
        assert_eq!(zoom.scroll.x, scroll.x + PAN_STEP);
        for _ in 0..100 {
            zoom.pan(Pan::Up, fitted, stage);
        }
        assert_eq!(zoom.scroll.y, 0., "panning stops at the edge");
        // A tall image zoomed until it overflows only down: Left and Right still navigate.
        let tall = stage.fitted(size(100., 1280.), false);
        let mut zoom = Zoom::default();
        zoom.set(1.2, None, tall, stage);
        assert!(!zoom.pan(Pan::Left, tall, stage));
        assert!(zoom.pan(Pan::Down, tall, stage));
    }

    #[test]
    fn dragging_scrolls_against_the_pointer_within_the_image() {
        let stage = Stage::new(WINDOW);
        let fitted = stage.fitted(size(800., 600.), false);
        let mut zoom = Zoom::default();
        zoom.set(2., None, fitted, stage);
        let from = zoom.scroll;
        zoom.drag(from, point(10., -20.), fitted, stage);
        assert_eq!(zoom.scroll, point(from.x - 10., from.y + 20.));
        zoom.drag(from, point(10_000., 10_000.), fitted, stage);
        assert_eq!(zoom.scroll, point(0., 0.));
    }

    #[test]
    fn the_file_panel_fits_and_steps_its_zoom() {
        assert_eq!(preview_fit(size(200., 100.), size(600., 400.)), 1., "never enlarged");
        assert_eq!(preview_fit(size(1136., 100.), size(600., 400.)), 0.5, "fits inside the 16px padding");
        assert_eq!(preview_fit(size(100., 100.), size(10., 10.)), 0.01, "a collapsed panel still has a size");
        assert_eq!(preview_zoom(1., PREVIEW_ZOOM_STEP), 1.25);
        assert_eq!(preview_zoom(0.06, 1. / PREVIEW_ZOOM_STEP), PREVIEW_MIN_ZOOM);
        assert_eq!(preview_zoom(7., PREVIEW_ZOOM_STEP), PREVIEW_MAX_ZOOM);
    }

    #[test]
    fn references_copy_paths_as_the_host_writes_them() {
        assert_eq!(
            file_reference("/repo/docs/a.png", Some("/repo/")),
            Reference::File { full: "/repo/docs/a.png".into(), relative: Some("docs/a.png".into()) }
        );
        assert_eq!(
            file_reference("C:/repo/docs/a.png", Some("C:\\repo")),
            Reference::File { full: r"C:\repo\docs\a.png".into(), relative: Some(r"docs\a.png".into()) }
        );
        assert_eq!(
            file_reference("/repo-other/a.png", Some("/repo")),
            Reference::File { full: "/repo-other/a.png".into(), relative: None },
            "a sibling folder sharing the prefix is outside"
        );
    }

    #[test]
    fn natural_sizes_come_from_headers_and_svg_attributes() {
        let svg = Image::from_bytes(
            ImageFormat::Svg,
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"/>"#.to_vec(),
        );
        assert_eq!(natural_size(&svg), Some(size(40., 20.)));
        let mut png = Vec::new();
        image::RgbaImage::new(3, 2).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert_eq!(natural_size(&Image::from_bytes(ImageFormat::Png, png)), Some(size(3., 2.)));
        assert_eq!(natural_size(&Image::from_bytes(ImageFormat::Png, b"nope".to_vec())), None);
    }

    #[test]
    fn save_names_come_from_the_source() {
        let image = Arc::new(Image::from_bytes(ImageFormat::Png, Vec::new()));
        let item =
            |name: &str, reference| ViewerImage { image: image.clone(), name: name.to_owned().into(), reference };
        assert_eq!(
            item("Logo", Some(Reference::Url("https://x.dev/img/my%20logo.png?v=2".into()))).file_name(),
            "my logo.png"
        );
        assert_eq!(item("Logo", Some(file_reference("/repo/a.png", None))).file_name(), "a.png");
        assert_eq!(item("Logo", None).file_name(), "Logo.png");
        assert_eq!(item("shot.jpeg", None).file_name(), "shot.jpeg");
    }
}
