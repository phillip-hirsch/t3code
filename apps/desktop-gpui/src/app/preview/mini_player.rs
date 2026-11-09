//! `ThreadPreviewMiniPlayer`: the floated browser tab over the chat. A frame at its page's aspect
//! ratio that its handle moves and its edges resize, with the handle's hover pill of controls.
//!
//! A native page view covers whatever GPUI draws over it, so the handle can't see the pointer
//! there: the page reports its top-right corner instead ([`crate::preview::CORNER_SCRIPT`]), and
//! the view hides while the pill shows over it.

use gpui_kit::assets::IconName;
use gpui_kit::component::v_flex;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Bounds, Context, DragMoveEvent, Empty, FocusHandle, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, Point,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::mini_layout::{self, Composer, Direction, Frame, GAP, Obstacles, Position, Size};
use super::{Surface, Workspace, icon_button};
use crate::preview::{self, ThreadRef};
use crate::theme;

/// `pillInset` at the frame's 12px radius.
const PILL_INSET: f32 = 8.;
/// The handle's `size-3`, and the pill's `h-8` with its two `icon-xs` buttons.
const HANDLE: f32 = 12.;
const PILL: (f32, f32) = (56., 32.);
/// `HANDLE_TAP_SLOP_PX`: a press moves the player only once it travels this far.
const SLOP: f64 = 6.;

/// One thread's floating tab (`PreviewMiniPlayerState`).
#[derive(Clone, Debug, PartialEq)]
pub(in crate::app) struct MiniPlayer {
    pub tab_id: String,
    position: Option<Position>,
    width: Option<f64>,
    /// The panel page's size when the tab was floated from it: a fill tab keeps laying out at it
    /// (`fittedSourceContent`).
    fitted: Option<(f32, f32)>,
}

/// The drag a move or resize runs as, so its moves reach the frame wherever the pointer goes.
struct MiniDrag;

/// A move (no direction) or resize in progress (`PointerGesture`).
struct Gesture {
    thread: ThreadRef,
    tab_id: String,
    pointer: Point<Pixels>,
    frame: Frame,
    source: Size,
    direction: Option<Direction>,
    moved: bool,
}

/// What the mini player drew last frame, for a gesture to start from.
struct Shown {
    thread: ThreadRef,
    tab_id: String,
    frame: Frame,
    source: Size,
}

pub(super) struct MiniPlayerUi {
    /// The chat canvas the player floats in, as last laid out.
    canvas: Option<Bounds<Pixels>>,
    shown: Option<Shown>,
    gesture: Option<Gesture>,
    /// The pointer is on the handle or its open pill.
    hovered: bool,
    /// The page reported the pointer at its top-right corner, under the handle.
    corner: bool,
    /// The handle: a Tab stop that opens the pill, as `group-focus-within` does.
    handle: FocusHandle,
}

impl MiniPlayerUi {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        Self {
            canvas: None,
            shown: None,
            gesture: None,
            hovered: false,
            corner: false,
            handle: cx.focus_handle().tab_stop(true),
        }
    }
}

/// Whether `local` (in the frame) is on the handle, or, while the pill is open, on the pill or
/// the corner the page reports, so the pill stays open over the area that opened it.
fn on_handle(local: Position, frame_width: f64, open: bool) -> bool {
    let inset = f64::from(PILL_INSET);
    let (width, height) = if open {
        let corner = frame_width * 0.12;
        (corner.max(inset + f64::from(PILL.0)), corner.max(inset + f64::from(PILL.1)))
    } else {
        (inset + f64::from(HANDLE), inset + f64::from(HANDLE))
    };
    let from_right = frame_width - local.x;
    (0. ..=width).contains(&from_right) && (0. ..=height).contains(&local.y)
}

#[expect(clippy::cast_possible_truncation, reason = "layout px fit f32")]
fn px64(value: f64) -> Pixels {
    px(value as f32)
}

fn delta(from: Point<Pixels>, to: Point<Pixels>) -> Position {
    Position { x: f64::from(f32::from(to.x - from.x)), y: f64::from(f32::from(to.y - from.y)) }
}

impl Workspace {
    /// The tab floating over `thread`'s chat.
    pub(in crate::app) fn floating_tab(&self, thread: &ThreadRef) -> Option<&str> {
        self.preview.floating.get(thread).map(|mini| mini.tab_id.as_str())
    }

    /// `previewMiniPlayerStore.open`: floats `tab_id`, keeping the player's place and size when
    /// it swaps tabs. A fill tab the panel just showed keeps that page size.
    pub(in crate::app) fn float_tab(&mut self, thread: ThreadRef, tab_id: String) {
        let fitted = self.preview.panel_area.as_ref().filter(|(tab, _)| tab.thread == thread && tab.tab_id == tab_id);
        let fitted = fitted.map(|(_, size)| *size);
        let current = self.preview.floating.get(&thread);
        if current.is_some_and(|mini| mini.tab_id == tab_id) {
            return;
        }
        let (position, width) = current.map_or((None, None), |mini| (mini.position, mini.width));
        self.preview.floating.insert(thread, MiniPlayer { tab_id, position, width, fitted });
    }

    /// A page's corner report: it matters only from the tab the mini player shows.
    pub(super) fn page_corner(&mut self, thread: &ThreadRef, tab_id: &str, inside: bool, cx: &mut Context<Self>) {
        let ui = &mut self.preview.mini;
        let shown = ui.shown.as_ref().is_some_and(|shown| shown.thread == *thread && shown.tab_id == tab_id);
        if shown && ui.corner != inside {
            ui.corner = inside;
            cx.notify();
        }
    }

    /// The aspect the frame keeps: an agent tab's streamed viewport, else the tab's own size.
    fn mini_source(&self, thread: &ThreadRef, mini: &MiniPlayer, session: &preview::Session) -> Size {
        let tab = thread.tab(&mini.tab_id);
        let streamed = session.server_hosted().then(|| self.stream_viewport(&tab)).flatten();
        if let Some((width, height)) = streamed.filter(|(w, h)| *w > 0. && *h > 0.) {
            return Size { width: f64::from(width), height: f64::from(height) };
        }
        let (zoom, _) = self.tab_look(&tab, session);
        mini_layout::source_size(session.viewport.size(), mini.fitted, zoom)
    }

    /// The composer the player keeps clear of, in the canvas's coordinates.
    fn mini_obstacles(&self, window: &Window) -> Obstacles {
        let (lane, _) = self.chat_layout(window);
        let composer = (self.composer_height > 0.).then(|| Composer {
            left: f64::from(lane.left),
            right: f64::from(lane.left + lane.width),
            height: f64::from(self.composer_height),
        });
        Obstacles { composer, details_card: None }
    }

    fn begin_mini_gesture(&mut self, direction: Option<Direction>, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let ui = &mut self.preview.mini;
        let Some(shown) = &ui.shown else { return };
        ui.gesture = Some(Gesture {
            thread: shown.thread.clone(),
            tab_id: shown.tab_id.clone(),
            pointer: event.position,
            frame: shown.frame,
            source: shown.source,
            direction,
            moved: false,
        });
        cx.stop_propagation();
    }

    /// `handlePointerMove`: a move clamps into the canvas; a resize keeps the aspect ratio. The
    /// layout clears the composer when it next draws, without changing what is stored.
    fn mini_gesture_move(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let ui = &mut self.preview.mini;
        let (Some(gesture), Some(canvas)) = (ui.gesture.as_mut(), ui.canvas) else { return };
        let delta = delta(gesture.pointer, position);
        if !gesture.moved && delta.x.hypot(delta.y) < SLOP {
            return;
        }
        gesture.moved = true;
        let container =
            Size { width: f64::from(f32::from(canvas.size.width)), height: f64::from(f32::from(canvas.size.height)) };
        let frame = gesture.frame;
        let (position, width) = match gesture.direction {
            None => {
                let moved = Position { x: frame.x + delta.x, y: frame.y + delta.y };
                (mini_layout::clamp_position(moved, container, frame.size(), Obstacles::default(), GAP), None)
            }
            Some(direction) => {
                let next =
                    mini_layout::resize(frame, direction, delta, gesture.source, container, Obstacles::default());
                (Position { x: next.x, y: next.y }, Some(next.width))
            }
        };
        // A drag that outlives the tab it started on changes nothing.
        let Some(mini) = self.preview.floating.get_mut(&gesture.thread).filter(|m| m.tab_id == gesture.tab_id) else {
            return;
        };
        if mini.position != Some(position) || (width.is_some() && mini.width != width) {
            mini.position = Some(position);
            mini.width = width.or(mini.width);
            cx.notify();
        }
    }

    /// Whether the pointer at window `position` is on the handle, and the pill should show.
    fn mini_hover(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let ui = &mut self.preview.mini;
        let (Some(canvas), Some(shown)) = (ui.canvas, &ui.shown) else { return };
        let local = Position {
            x: f64::from(f32::from(position.x - canvas.origin.x)) - shown.frame.x,
            y: f64::from(f32::from(position.y - canvas.origin.y)) - shown.frame.y,
        };
        let hovered = on_handle(local, shown.frame.width, ui.hovered || ui.corner);
        if hovered != ui.hovered || (!hovered && ui.corner) {
            ui.hovered = hovered;
            ui.corner = false;
            cx.notify();
        }
    }

    /// `ThreadPreviewMiniPlayer`: the floated tab over the chat, unless the panel shows that same
    /// tab (`shouldRenderPreviewMiniPlayer`).
    pub(in crate::app) fn render_preview_mini_player(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        self.preview.mini.shown = None;
        let player = self.render_mini_player(window, cx);
        // A player that comes back starts with its pill closed.
        let ui = &mut self.preview.mini;
        if ui.shown.is_none() {
            (ui.hovered, ui.corner, ui.gesture) = (false, false, None);
        }
        player
    }

    fn render_mini_player(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let thread = self.preview_thread()?;
        let mini = self.preview.floating.get(&thread)?.clone();
        let tab = thread.tab(&mini.tab_id);
        if self.page.is_some() || self.active_surface() == Some(&Surface::Browser(tab.tab_id.clone())) {
            return None;
        }
        let session = self.preview.threads.session(&tab)?.clone();
        let tokens = *theme::tokens(cx);
        let source = self.mini_source(&thread, &mini, &session);
        let obstacles = self.mini_obstacles(window);
        let frame = self.preview.mini.canvas.map(|canvas| {
            let container = Size {
                width: f64::from(f32::from(canvas.size.width)),
                height: f64::from(f32::from(canvas.size.height)),
            };
            let frame = mini_layout::resolve_frame(mini.width, mini.position, source, container, obstacles);
            // A player not yet moved starts in the bottom-right corner (`resolveChatCanvasLayout`).
            let start = match mini.position {
                Some(_) => Position { x: frame.x, y: frame.y },
                None => Position { x: frame.x, y: container.height - frame.height - GAP },
            };
            let position = mini_layout::clamp_position(start, container, frame.size(), obstacles, GAP);
            Frame { x: position.x, y: position.y, ..frame }
        });
        let view = cx.weak_entity();
        let measure = div().absolute().inset_0().on_children_prepainted(move |bounds, _, cx| {
            let Some(bounds) = bounds.first().copied() else { return };
            let view = view.clone();
            cx.defer(move |cx| {
                let _ = view.update(cx, |this, cx| {
                    if this.preview.mini.canvas != Some(bounds) {
                        this.preview.mini.canvas = Some(bounds);
                        cx.notify();
                    }
                });
            });
        });
        let measure = measure.child(div().size_full());
        let Some(frame) = frame.filter(|frame| frame.width > 0. && frame.height > 0.) else {
            return Some(measure.into_any_element());
        };
        self.preview.mini.shown = Some(Shown { thread: thread.clone(), tab_id: mini.tab_id.clone(), frame, source });
        let open = self.preview.mini.hovered
            || self.preview.mini.corner
            || self.preview.mini.handle.contains_focused(window, cx);
        let server = session.server_hosted();
        // An agent's tab streams here as in the panel, with its control bar below the page.
        let content = if server {
            v_flex()
                .size_full()
                .child(self.render_server_page(&tab, false, true, &tokens, window, cx))
                .into_any_element()
        } else {
            let hosted = self.preview.tabs.get(&tab).map(|hosted| hosted.view.clone());
            match hosted {
                // The pill draws where the view would cover it.
                Some(_) if open => div().into_any_element(),
                Some(view) => {
                    #[expect(clippy::cast_possible_truncation, reason = "layout px fit f32")]
                    let ((width, height), scale) = preview::fit_viewport(
                        (source.width as f32, source.height as f32),
                        (frame.width as f32, frame.height as f32),
                    );
                    self.preview.mini_view = Some((tab.clone(), scale));
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().w(px(width)).h(px(height)).child(view))
                        .into_any_element()
                }
                // Showing the tab starts its view (`sync_preview_views`).
                None => {
                    self.preview.mini_view = Some((tab.clone(), 1.));
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .text_color(tokens.muted_foreground)
                        .child("Reconnecting preview\u{2026}")
                        .into_any_element()
                }
            }
        };
        let radius = px(mini_layout::CORNER_RADIUS);
        let resize_handles = Direction::ALL.into_iter().enumerate().map(|(index, direction)| {
            let edge = div().id(("mini-resize", index)).absolute();
            let edge = match direction {
                Direction::North => edge.left_0().right_0().top(px(-4.)).h(px(8.)).cursor_ns_resize(),
                Direction::South => edge.left_0().right_0().bottom(px(-4.)).h(px(8.)).cursor_ns_resize(),
                Direction::West => edge.top_0().bottom_0().left(px(-4.)).w(px(8.)).cursor_ew_resize(),
                Direction::East => edge.top_0().bottom_0().right(px(-4.)).w(px(8.)).cursor_ew_resize(),
                Direction::NorthWest => edge.left(px(-8.)).top(px(-8.)).size(px(16.)).cursor_nwse_resize(),
                Direction::NorthEast => edge.right(px(-8.)).top(px(-8.)).size(px(16.)).cursor_nesw_resize(),
                Direction::SouthWest => edge.left(px(-8.)).bottom(px(-8.)).size(px(16.)).cursor_nesw_resize(),
                Direction::SouthEast => edge.right(px(-8.)).bottom(px(-8.)).size(px(16.)).cursor_nwse_resize(),
            };
            edge.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.begin_mini_gesture(Some(direction), event, cx);
                }),
            )
            .on_drag(MiniDrag, |_, _, _, cx| cx.new(|_| Empty))
        });
        let resize_handles: Vec<_> = resize_handles.collect();
        let (expand_thread, expand_tab, close_thread) = (thread.clone(), mini.tab_id, thread);
        let pill = h_pill(&tokens)
            .id("mini-pill")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| this.begin_mini_gesture(None, event, cx)),
            )
            .on_drag(MiniDrag, |_, _, _, cx| cx.new(|_| Empty))
            .child(
                icon_button("mini-open-in-panel", IconName::PanelRight, true, &tokens, cx)
                    .tooltip("Open in right panel")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.preview.floating.remove(&expand_thread);
                        this.open_surface(Surface::Browser(expand_tab.clone()), window, cx);
                    })),
            )
            .child(
                icon_button("mini-close", IconName::X, true, &tokens, cx).tooltip("Close floating preview").on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.preview.floating.remove(&close_thread);
                        cx.notify();
                    }),
                ),
            );
        let dot = div()
            .absolute()
            .right_0()
            .top_0()
            .size(px(8.))
            .rounded_full()
            .border_1()
            .border_color(tokens.background.opacity(0.7))
            .bg(tokens.foreground.opacity(0.25))
            .shadow_sm();
        let handle = div()
            .id("mini-handle")
            .absolute()
            .right(px(PILL_INSET))
            .top(px(PILL_INSET))
            .size(px(HANDLE))
            .track_focus(&self.preview.mini.handle)
            .cursor_grab()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| this.begin_mini_gesture(None, event, cx)),
            )
            .on_drag(MiniDrag, |_, _, _, cx| cx.new(|_| Empty))
            .map(|handle| if open { handle.child(pill) } else { handle.child(dot) });
        let player = div()
            .id("preview-mini-player")
            .absolute()
            .left(px64(frame.x))
            .top(px64(frame.y))
            .w(px64(frame.width))
            .h(px64(frame.height))
            .rounded(radius)
            .occlude()
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<MiniDrag>, _, cx| {
                this.mini_gesture_move(event.event.position, cx);
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.preview.mini.gesture = None))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, _| this.preview.mini.gesture = None))
            .child(div().absolute().inset_0().rounded(radius).bg(tokens.muted).shadow_2xl())
            .child(div().absolute().inset_0().rounded(radius).overflow_hidden().child(content))
            .child(div().absolute().inset_0().rounded(radius).border_1().border_color(tokens.border.opacity(0.8)))
            // Over the page without blocking it: where the pointer is decides whether the pill shows.
            .child(
                div()
                    .id("mini-hover")
                    .absolute()
                    .inset_0()
                    .on_mouse_move(
                        cx.listener(|this, event: &MouseMoveEvent, _, cx| this.mini_hover(event.position, cx)),
                    )
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        let ui = &mut this.preview.mini;
                        if !*hovered && (ui.hovered || ui.corner) {
                            (ui.hovered, ui.corner) = (false, false);
                            cx.notify();
                        }
                    })),
            )
            .children(resize_handles)
            .child(handle);
        Some(measure.child(player).into_any_element())
    }
}

/// The pill: `h-8 rounded-lg border bg-popover/92 p-0.5 shadow-lg`, at the handle's corner.
fn h_pill(tokens: &theme::Tokens) -> gpui_kit::Div {
    gpui_kit::component::h_flex()
        .absolute()
        .right_0()
        .top_0()
        .h(px(PILL.1))
        .items_center()
        .gap(px(2.))
        .p(px(2.))
        .rounded(px(8.))
        .border_1()
        .border_color(tokens.border.opacity(0.8))
        .bg(tokens.popover.opacity(0.92))
        .shadow_lg()
        .cursor_grab()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handle_zone_grows_to_the_pill_and_the_page_corner_while_open() {
        let at = |x: f64, y: f64| Position { x, y };
        // A 320px frame: the 12px handle sits 8px in from its top-right corner.
        assert!(on_handle(at(305., 10.), 320., false), "on the handle");
        assert!(!on_handle(at(270., 10.), 320., false), "left of the handle");
        assert!(!on_handle(at(305., 30.), 320., false), "below the handle");
        assert!(on_handle(at(270., 30.), 320., true), "on the open pill");
        assert!(!on_handle(at(240., 30.), 320., true), "left of the open pill");
        assert!(!on_handle(at(305., 45.), 320., true), "below the open pill");
        // A wide frame keeps the pill open over the whole corner the page reports.
        assert!(on_handle(at(1000. - 110., 110.), 1000., true), "in the page's corner");
        assert!(!on_handle(at(1000. - 110., 130.), 1000., true), "past it");
        assert!(!on_handle(at(330., 10.), 320., true), "outside the frame");
    }
}
