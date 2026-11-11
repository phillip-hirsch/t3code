//! `BrowserViewportResizeHandles` and `useBrowserViewportResize`: the edge and corner handles
//! beside a fixed viewport, dragged with the pointer or stepped with the arrow keys.

use std::time::Duration;

use gpui_kit::{
    AnyElement, Context, CursorStyle, DispatchPhase, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Styled as _, Task,
    Window, canvas, div, px, svg,
};

use super::Workspace;
use super::viewport::{self, Direction, RAIL};
use crate::preview::{TabRef, Viewport};
use crate::theme;

/// `KEYBOARD_RESIZE_COMMIT_DELAY_MS`: arrow keys commit once they pause.
const KEY_COMMIT_DELAY: Duration = Duration::from_millis(150);

const HANDLES: [Direction; 5] =
    [Direction::West, Direction::East, Direction::South, Direction::SouthWest, Direction::SouthEast];

pub(super) struct ViewportResize {
    /// One tab stop per handle, in [`HANDLES`] order.
    focus: [FocusHandle; 5],
    active: Option<Active>,
    /// The tab whose handles this frame drew.
    drawn: Option<TabRef>,
}

impl ViewportResize {
    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        Self { focus: std::array::from_fn(|_| cx.focus_handle().tab_stop(true)), active: None, drawn: None }
    }
}

/// The size a drag or the arrow keys show until it commits (`dragViewport`).
struct Active {
    tab: TabRef,
    /// The setting it started from (`sourceKey`); once the tab's setting changes it is dropped.
    source: Viewport,
    direction: Direction,
    size: (u32, u32),
    drag: Option<Drag>,
    /// The arrow keys' pending commit.
    commit: Option<Task<()>>,
}

#[derive(Clone, Copy)]
struct Drag {
    origin: Point<Pixels>,
    start: (u32, u32),
    /// Host px per CSS px when the drag started: the page zoom times the fit's scale.
    zoom: f64,
    available: (f32, f32),
    ratio: Option<f64>,
}

fn cursor(direction: Direction) -> CursorStyle {
    match direction {
        Direction::West | Direction::East => CursorStyle::ResizeLeftRight,
        Direction::South => CursorStyle::ResizeUpDown,
        Direction::SouthWest => CursorStyle::ResizeUpRightDownLeft,
        Direction::SouthEast => CursorStyle::ResizeUpLeftDownRight,
    }
}

fn group(direction: Direction) -> &'static str {
    match direction {
        Direction::West => "viewport-resize-west",
        Direction::East => "viewport-resize-east",
        Direction::South => "viewport-resize-south",
        Direction::SouthWest => "viewport-resize-southwest",
        Direction::SouthEast => "viewport-resize-southeast",
    }
}

/// Where a handle sits around a `size` viewport box: left, top, width, height.
fn placement(direction: Direction, size: (f32, f32)) -> (f32, f32, f32, f32) {
    let (width, height) = size;
    match direction {
        Direction::West => (-RAIL, 0., RAIL, height),
        Direction::East => (width, 0., RAIL, height),
        Direction::South => (0., height, width, RAIL),
        Direction::SouthWest => (-RAIL, height, RAIL, RAIL),
        Direction::SouthEast => (width, height, RAIL, RAIL),
    }
}

impl Workspace {
    /// `effectiveViewport`: `tab`'s `setting`, or the size a resize in progress shows.
    pub(super) fn resized_viewport(&mut self, tab: &TabRef, setting: &Viewport) -> Viewport {
        let resize = &mut self.preview.resize;
        match &resize.active {
            Some(active) if active.tab == *tab && active.source == *setting => {
                let (width, height) = active.size;
                Viewport::Freeform { width, height }
            }
            Some(active) if active.tab == *tab => {
                resize.active = None;
                setting.clone()
            }
            _ => setting.clone(),
        }
    }

    /// A committed resize landed: the setting shows the size from here on.
    pub(super) fn settle_viewport_resize(&mut self, tab: &TabRef) {
        let resize = &mut self.preview.resize;
        if resize.active.as_ref().is_some_and(|a| a.tab == *tab && a.drag.is_none() && a.commit.is_none()) {
            resize.active = None;
        }
    }

    /// The handles around `tab`'s viewport box, `size` px, fitted into `available` at `scale`;
    /// `zoom` is the page's layout zoom and `setting` the tab's own viewport.
    #[expect(clippy::too_many_arguments, reason = "the layout the handles sit in")]
    pub(super) fn render_resize_handles(
        &mut self,
        tab: &TabRef,
        setting: &Viewport,
        size: (f32, f32),
        scale: f32,
        zoom: f64,
        available: (f32, f32),
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let t = *theme::tokens(cx);
        self.preview.resize.drawn = Some(tab.clone());
        let resize = &self.preview.resize;
        let active = resize.active.as_ref().filter(|a| a.tab == *tab);
        let mut handles: Vec<AnyElement> = HANDLES
            .into_iter()
            .zip(&resize.focus)
            .map(|(direction, focus)| {
                let bright = active.is_some_and(|a| a.direction == direction)
                    || (focus.is_focused(window) && window.last_input_was_keyboard());
                let grip_color = if bright { t.foreground } else { t.muted_foreground.opacity(0.55) };
                let hover = t.foreground.opacity(0.85);
                let line = |w: f32, h: f32| {
                    let line = div().w(px(w)).h(px(h)).rounded_full().bg(grip_color);
                    if bright { line } else { line.group_hover(group(direction), move |s| s.bg(hover)) }
                };
                let grip = match direction {
                    Direction::West | Direction::East => {
                        div().flex().gap(px(1.)).child(line(1., 24.)).child(line(1., 24.)).into_any_element()
                    }
                    Direction::South => {
                        div().flex().flex_col().gap(px(1.)).child(line(24., 1.)).child(line(24., 1.)).into_any_element()
                    }
                    Direction::SouthWest | Direction::SouthEast => {
                        let path = if direction == Direction::SouthWest {
                            "icons/app/viewport-resize-southwest.svg"
                        } else {
                            "icons/app/viewport-resize-southeast.svg"
                        };
                        let corner = svg().path(path).flex_none().size(px(12.)).text_color(grip_color);
                        (if bright {
                            corner
                        } else {
                            corner.group_hover(group(direction), move |s| s.text_color(hover))
                        })
                        .into_any_element()
                    }
                };
                let (left, top, width, height) = placement(direction, size);
                let (down_tab, down_setting, key_tab, key_setting) =
                    (tab.clone(), setting.clone(), tab.clone(), setting.clone());
                let down_focus = focus.clone();
                div()
                    .id(group(direction))
                    .group(group(direction))
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height))
                    .flex()
                    .items_center()
                    .justify_center()
                    .track_focus(focus)
                    .cursor(cursor(direction))
                    .focus_visible(move |s| s.bg(t.foreground.opacity(0.04)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                            window.focus(&down_focus, cx);
                            let drag = Drag {
                                origin: e.position,
                                start: (0, 0),
                                zoom: zoom * f64::from(scale),
                                available,
                                ratio: None,
                            };
                            this.start_viewport_drag(&down_tab, &down_setting, direction, drag, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .on_key_down(cx.listener(move |this, e: &KeyDownEvent, _, cx| {
                        this.viewport_key(&key_tab, &key_setting, direction, e, zoom, cx);
                    }))
                    .child(grip)
                    .into_any_element()
            })
            .collect();
        // A drag follows the pointer and its release anywhere in the window.
        if let Some(direction) = active.filter(|a| a.drag.is_some()).map(|a| a.direction) {
            let (view, up_view) = (cx.weak_entity(), cx.weak_entity());
            let listeners = canvas(
                |_, _, _| (),
                move |_, (), window, _| {
                    window.set_window_cursor_style(cursor(direction));
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            let _ = view.update(cx, |this, cx| this.drag_viewport(e, cx));
                        }
                    });
                    window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture && e.button == MouseButton::Left {
                            let _ = up_view.update(cx, Self::end_viewport_drag);
                        }
                    });
                },
            )
            .absolute()
            .size_full();
            handles.push(listeners.into_any_element());
        }
        handles
    }

    /// The size shown for `tab` now, and the ratio its lock holds.
    fn resize_base(&self, tab: &TabRef, setting: &Viewport) -> Option<((u32, u32), Option<f64>)> {
        let (width, height) = setting.size()?;
        let ratio = self.preview.toolbar.locked_ratio(tab, (width, height));
        let active = self.preview.resize.active.as_ref().filter(|a| a.tab == *tab && a.source == *setting);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "viewport sizes are whole pixels")]
        let size = active.map_or((width as u32, height as u32), |a| a.size);
        Some((size, ratio))
    }

    /// `handleResizePointerDown`: drops a pending key commit and starts dragging from the size shown.
    fn start_viewport_drag(
        &mut self,
        tab: &TabRef,
        setting: &Viewport,
        direction: Direction,
        drag: Drag,
        cx: &mut Context<Self>,
    ) {
        let Some((size, ratio)) = self.resize_base(tab, setting) else { return };
        let drag = Drag { start: size, ratio, ..drag };
        self.preview.resize.active =
            Some(Active { tab: tab.clone(), source: setting.clone(), direction, size, drag: Some(drag), commit: None });
        cx.notify();
    }

    fn drag_viewport(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let resize = &mut self.preview.resize;
        let Some(active) = resize.active.as_mut() else { return };
        let Some(drag) = active.drag else { return };
        // The release went somewhere this window didn't see: the drag ends like a `pointercancel`.
        if event.pressed_button != Some(MouseButton::Left) {
            resize.active = None;
            cx.notify();
            return;
        }
        let position = event.position;
        let pointer = (f32::from(position.x - drag.origin.x), f32::from(position.y - drag.origin.y));
        let size =
            viewport::resize_from_rail(drag.start, pointer, drag.available, drag.zoom, active.direction, drag.ratio);
        if size != active.size {
            active.size = size;
            cx.notify();
        }
    }

    /// A drag or arrow-key resize whose handles this frame didn't draw ends without committing,
    /// as the web's unmount does. Runs after the panel renders.
    pub(super) fn end_undrawn_viewport_resize(&mut self) {
        let resize = &mut self.preview.resize;
        let drawn = resize.drawn.take();
        let interacting = resize.active.as_ref().is_some_and(|a| a.drag.is_some() || a.commit.is_some());
        if interacting && resize.active.as_ref().map(|a| &a.tab) != drawn.as_ref() {
            resize.active = None;
        }
    }

    /// The release commits a changed size; an unchanged one just ends the drag.
    fn end_viewport_drag(&mut self, cx: &mut Context<Self>) {
        let resize = &mut self.preview.resize;
        let Some(active) = resize.active.as_mut() else { return };
        let Some(drag) = active.drag.take() else { return };
        cx.notify();
        if active.size == drag.start {
            resize.active = None;
            return;
        }
        let (tab, (width, height)) = (active.tab.clone(), active.size);
        self.commit_viewport_resize(tab, (width, height), cx);
    }

    /// Sends the size shown; when no request could start, the setting shows again.
    fn commit_viewport_resize(&mut self, tab: TabRef, (width, height): (u32, u32), cx: &mut Context<Self>) {
        self.resize_viewport(tab.clone(), Viewport::Freeform { width, height }, None, cx);
        if !self.preview.toolbar.resizing(&tab) {
            self.preview.resize.active = None;
        }
    }

    /// `handleResizeKeyDown`: an arrow key steps the size shown, committed once the keys pause.
    fn viewport_key(
        &mut self,
        tab: &TabRef,
        setting: &Viewport,
        direction: Direction,
        event: &KeyDownEvent,
        zoom: f64,
        cx: &mut Context<Self>,
    ) {
        let Some((size, ratio)) = self.resize_base(tab, setting) else { return };
        let keystroke = &event.keystroke;
        let Some(next) = viewport::key_resize(size, direction, &keystroke.key, keystroke.modifiers.shift, zoom, ratio)
        else {
            return;
        };
        cx.stop_propagation();
        if next == size {
            return;
        }
        let commit = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(KEY_COMMIT_DELAY).await;
            let _ = this.update(cx, Self::commit_key_resize);
        });
        self.preview.resize.active = Some(Active {
            tab: tab.clone(),
            source: setting.clone(),
            direction,
            size: next,
            drag: None,
            commit: Some(commit),
        });
        cx.notify();
    }

    fn commit_key_resize(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.preview.resize.active.as_mut() else { return };
        if active.commit.take().is_none() {
            return;
        }
        let (tab, source, (width, height)) = (active.tab.clone(), active.source.clone(), active.size);
        if self.preview.threads.session(&tab).map(|s| &s.viewport) != Some(&source) {
            self.preview.resize.active = None;
            return;
        }
        self.commit_viewport_resize(tab, (width, height), cx);
    }
}
