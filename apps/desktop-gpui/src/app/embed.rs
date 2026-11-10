//! Native pages laid into GPUI's layout, the way the web lays an iframe or a `<video>` into the
//! DOM (`HtmlRenderFrame`, `MediaVideoPlayer`). A WebView is a child window over the GPUI
//! window, so [`EmbedFrame`] places it from its element's bounds after layout, every frame:
//!
//! - Clipped by its scroller (and by the composer floating over the chat's bottom, see
//!   [`ClipScope`]), the native view takes only the visible part, and the page shifts its content
//!   by the clipped amount (`t3Frame`), so it scrolls with the timeline instead of drawing over it.
//! - A GPUI overlay can't draw over a native view, so a covered layer hides its pages
//!   ([`Embeds::begin_frame`]).
//! - An embed no frame laid out (scrolled out of a virtual list, or its view went away) is hidden
//!   by [`EmbedSweep`] at the end of the frame and its media pause (`t3Hidden`), as the web's
//!   unmount does. Dropping its [`EmbedView`] releases the page.
//!
//! The page loaded into an embed puts its content in `#t3-embed` and runs [`FRAME_SCRIPT`].

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::rc::{Rc, Weak};

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, AvailableSpace, Bounds, Element, ElementId, Entity, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, Size, Style, WeakEntity, Window, px,
};
use gpui_webview::WebView;

/// The page side of an embed, after its `#t3-embed` element: `t3Frame(x, y, w, h)` sizes the
/// content to the element and shifts it by the part its scroller clips; `ready` asks to be told
/// again once the page can listen.
pub(super) const FRAME_SCRIPT: &str = "(()=>{const e=document.getElementById('t3-embed');\
window.t3Frame=(x,y,w,h)=>{e.style.left=-x+'px';e.style.top=-y+'px';e.style.width=w+'px';e.style.height=h+'px';};\
window.ipc.postMessage(JSON.stringify({type:'ready'}));})();";

/// The CSS an embed page needs for [`FRAME_SCRIPT`]: the content box, positioned by the host.
pub(super) const FRAME_CSS: &str =
    "html,body{margin:0;overflow:hidden}#t3-embed{position:absolute;left:0;top:0;width:100vw;height:100vh}";

const HIDDEN_SCRIPT: &str = "window.t3Hidden&&window.t3Hidden()";

/// Which overlays an embed sits under: the app's layout (the chat, the panels), or a dialog drawn
/// over it, which the layout's overlays don't cover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layer {
    Page,
    Dialog,
}

/// The window's embeds and what this frame lays them out under.
#[derive(Default)]
pub(super) struct Embeds {
    shared: Rc<Shared>,
}

#[derive(Default)]
struct Shared {
    frame: Cell<u64>,
    /// Per [`Layer`]: a GPUI overlay is over it this frame.
    covered: Cell<[bool; 2]>,
    /// The part of the current [`ClipScope`] no overlay of its own covers.
    clip: Cell<Option<Bounds<Pixels>>>,
    members: RefCell<Vec<Weak<Placement>>>,
}

struct Placement {
    view: WeakEntity<WebView>,
    layer: Cell<Layer>,
    /// The frame an [`EmbedFrame`] last laid this embed out in.
    drawn: Cell<u64>,
    /// Laid out by the last sweep's frame, so the next one that misses it pauses it once.
    was_drawn: Cell<bool>,
    shown: Cell<bool>,
    /// What `t3Frame` last told the page; `None` until it can listen.
    told: Cell<Option<(Point<Pixels>, Size<Pixels>)>>,
    /// The `t3Frame` call, kept so scrolling doesn't allocate one per frame.
    script: RefCell<String>,
}

impl Placement {
    fn show(&self, view: &WebView) {
        if !self.shown.replace(true) {
            let _ = view.set_visible(true);
        }
    }

    fn hide(&self, view: &WebView, pause: bool) {
        if self.shown.replace(false) {
            let _ = view.set_visible(false);
        }
        if pause {
            let _ = view.raw().evaluate_script(HIDDEN_SCRIPT);
        }
    }

    /// Tells the page its content box: `size`, shifted by `offset` where its scroller clips it.
    fn tell(&self, view: &WebView, offset: Point<Pixels>, size: Size<Pixels>) {
        if self.told.get() == Some((offset, size)) {
            return;
        }
        self.told.set(Some((offset, size)));
        let mut script = self.script.borrow_mut();
        script.clear();
        let _ = write!(
            script,
            "window.t3Frame&&t3Frame({},{},{},{})",
            f32::from(offset.x),
            f32::from(offset.y),
            f32::from(size.width),
            f32::from(size.height)
        );
        let _ = view.raw().evaluate_script(&script);
    }
}

/// A native page owned by a feature; dropping it releases the page.
pub(super) struct EmbedView {
    view: Entity<WebView>,
    placement: Rc<Placement>,
    shared: Rc<Shared>,
}

impl EmbedView {
    /// The element that lays the page out in its bounds: give it a sized parent.
    pub(super) fn frame(&self) -> EmbedFrame {
        EmbedFrame { placement: self.placement.clone(), shared: self.shared.clone(), child: None }
    }

    pub(super) fn raw<'a>(&self, cx: &'a App) -> &'a wry::WebView {
        self.view.read(cx).raw()
    }

    pub(super) fn webview<'a>(&self, cx: &'a App) -> &'a WebView {
        self.view.read(cx)
    }

    /// The page (re)loaded and listens again: the next frame tells it its box.
    pub(super) fn retell(&self) {
        self.placement.told.set(None);
    }

    /// Moves the page under another layer's overlays, as a full screen app leaves the layout.
    pub(super) fn set_layer(&self, layer: Layer) {
        self.placement.layer.set(layer);
    }
}

impl Embeds {
    /// Takes a freshly built page; it stays hidden until a frame lays it out.
    pub(super) fn adopt(&self, webview: WebView, layer: Layer, cx: &mut App) -> EmbedView {
        let _ = webview.set_visible(false);
        let view = cx.new(|_| webview);
        let placement = Rc::new(Placement {
            view: view.downgrade(),
            layer: Cell::new(layer),
            drawn: Cell::new(0),
            was_drawn: Cell::new(false),
            shown: Cell::new(false),
            told: Cell::new(None),
            script: RefCell::new(String::new()),
        });
        let mut members = self.shared.members.borrow_mut();
        members.retain(|member| member.strong_count() > 0);
        members.push(Rc::downgrade(&placement));
        EmbedView { view, placement, shared: self.shared.clone() }
    }

    /// Starts a frame: `covered` says which layers a GPUI overlay is over.
    pub(super) fn begin_frame(&self, covered: [bool; 2]) {
        self.shared.frame.set(self.shared.frame.get() + 1);
        self.shared.covered.set(covered);
    }

    /// The current frame's number, which [`EmbedView::drawn`] compares against.
    pub(super) fn frame(&self) -> u64 {
        self.shared.frame.get()
    }

    /// Hides, at the end of the frame, every embed the frame did not lay out. Put it after
    /// everything that draws embeds.
    pub(super) fn sweep(&self) -> EmbedSweep {
        EmbedSweep { shared: self.shared.clone() }
    }

    /// Wraps a scroller of embeds whose bottom `inset` an overlay covers (the floating composer).
    pub(super) fn clip_scope(&self, child: impl IntoElement, inset: Pixels) -> ClipScope {
        ClipScope { shared: self.shared.clone(), child: child.into_any_element(), inset }
    }
}

/// Lays an [`EmbedView`]'s native page out in this element's bounds.
pub(super) struct EmbedFrame {
    placement: Rc<Placement>,
    shared: Rc<Shared>,
    child: Option<AnyElement>,
}

impl IntoElement for EmbedFrame {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// The part of `bounds` drawn on screen, and the content's offset into it.
fn visible_part(bounds: Bounds<Pixels>, clip: Bounds<Pixels>) -> Option<(Bounds<Pixels>, Point<Pixels>)> {
    let visible = bounds.intersect(&clip);
    if visible.size.width < px(1.) || visible.size.height < px(1.) {
        return None;
    }
    Some((visible, Point::new(visible.origin.x - bounds.origin.x, visible.origin.y - bounds.origin.y)))
}

impl Element for EmbedFrame {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style { size: Size::full(), ..Style::default() };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let placement = &self.placement;
        placement.drawn.set(self.shared.frame.get());
        let Some(view) = placement.view.upgrade() else { return };
        let mut clip = window.content_mask().bounds;
        let layer = placement.layer.get();
        if layer == Layer::Page
            && let Some(scope) = self.shared.clip.get()
        {
            clip = clip.intersect(&scope);
        }
        let covered = self.shared.covered.get()[layer as usize];
        let Some((visible, offset)) = visible_part(bounds, clip).filter(|_| !covered) else {
            placement.hide(view.read(cx), false);
            return;
        };
        placement.show(view.read(cx));
        placement.tell(view.read(cx), offset, bounds.size);
        // gpui-webview's element places the native view at its own bounds: the visible part.
        let mut child = view.into_any_element();
        child.layout_as_root(visible.size.map(AvailableSpace::Definite), window, cx);
        child.prepaint_at(visible.origin, window, cx);
        self.child = Some(child);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = &mut self.child {
            child.paint(window, cx);
        }
    }
}

/// Hides the embeds the frame did not lay out; see [`Embeds::sweep`].
pub(super) struct EmbedSweep {
    shared: Rc<Shared>,
}

impl IntoElement for EmbedSweep {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EmbedSweep {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style { position: gpui_kit::Position::Absolute, ..Style::default() };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        _: &mut Window,
        cx: &mut App,
    ) {
        let frame = self.shared.frame.get();
        let mut members = self.shared.members.borrow_mut();
        members.retain(|member| member.strong_count() > 0);
        for placement in members.iter().filter_map(Weak::upgrade) {
            let drawn = placement.drawn.get() == frame;
            let unmounted = placement.was_drawn.replace(drawn) && !drawn;
            if let Some(view) = placement.view.upgrade().filter(|_| !drawn) {
                placement.hide(view.read(cx), unmounted);
            }
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }
}

/// A scroller whose embeds stop `inset` above its bottom edge; see [`Embeds::clip_scope`].
pub(super) struct ClipScope {
    shared: Rc<Shared>,
    child: AnyElement,
    inset: Pixels,
}

impl IntoElement for ClipScope {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ClipScope {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut clip = bounds;
        clip.size.height = (clip.size.height - self.inset).max(px(0.));
        let outer = self.shared.clip.replace(Some(clip));
        self.child.prepaint(window, cx);
        self.shared.clip.set(outer);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

impl super::Workspace {
    /// Starts the frame's embeds before anything draws: what covers each layer, and the pages'
    /// own upkeep.
    pub(super) fn begin_embed_frame(&mut self, window: &mut Window, cx: &mut gpui_kit::Context<Self>) {
        let over_everything = self.settings.is_some()
            || self.palette.is_some()
            || self.image_viewer_open()
            || window.has_active_dialog(cx);
        let over_layout = over_everything
            || self.browser_covered(window, cx)
            || self.html_dialog_open()
            || self.mcp_app_fullscreen()
            || self.chat_menu.is_some()
            || self.composer_menu_open()
            || self.model_picker_open()
            || self.draft_picker_open()
            || self.find.is_some()
            || (self.details_visibility().popover_open && !self.details_inline(window));
        self.embeds.begin_frame([over_layout, over_everything]);
        self.sync_html_pages(window, cx);
        self.sync_mcp_apps(window, cx);
        self.sync_chat_media(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, size};

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    #[test]
    fn a_clipped_embed_shows_its_visible_part_shifted_by_the_clip() {
        let scroller = rect(0., 100., 800., 600.);
        // Whole: no shift.
        assert_eq!(
            visible_part(rect(10., 200., 700., 300.), scroller),
            Some((rect(10., 200., 700., 300.), point(px(0.), px(0.))))
        );
        // Scrolled past the top: the page shows its lower part.
        assert_eq!(
            visible_part(rect(10., 40., 700., 300.), scroller),
            Some((rect(10., 100., 700., 240.), point(px(0.), px(60.))))
        );
        // Under the bottom edge: the top part, unshifted.
        assert_eq!(
            visible_part(rect(10., 600., 700., 300.), scroller),
            Some((rect(10., 600., 700., 100.), point(px(0.), px(0.))))
        );
        // Out of view, or under a pixel of it.
        assert_eq!(visible_part(rect(10., 900., 700., 300.), scroller), None);
        assert_eq!(visible_part(rect(10., 699.5, 700., 300.), scroller), None);
    }
}
