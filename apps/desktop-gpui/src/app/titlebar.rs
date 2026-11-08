//! Window chrome on the top row: the sidebar toggle, the panel layout toggles
//! and, on Windows and Linux, the caption buttons Electron's titlebar overlay
//! draws (macOS keeps its traffic lights, as Electron's `hiddenInset` does).
//! Windows maps each `WindowControlArea` to its native hit-test code, so drag,
//! double-click maximize, snap layouts and the resize edges stay native; macOS
//! and Linux get the same through [`drag_region`] and [`render_resize_edges`].
//!
//! A button inside a drag area must `occlude()`: otherwise the drag area is
//! still under the pointer and Windows treats the press as a caption drag.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use std::cell::Cell;

use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, Context, CursorStyle, Decorations, Div, Element, ElementId, FocusHandle,
    FontFallbacks, GlobalElementId, Hsla, InspectorElementId, InteractiveElement, IntoElement, LayoutId, MouseButton,
    ParentElement as _, Pixels, ResizeEdge, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    TextRenderingMode, Tiling, Window, WindowControlArea, div, font, hsla, px, rgb, svg,
};

use super::{ToggleRightPanel, ToggleSidebar, Workspace};
use crate::theme;

/// `--workspace-topbar-height`: 52px beside macOS's traffic lights, else Electron's 40px titlebar overlay.
pub(super) const TITLEBAR_HEIGHT: Pixels = px(if cfg!(target_os = "macos") { 52. } else { 40. });
/// `#root` keeps this much free at the right so the resize edge stays grabbable; only
/// `.electron-windows` sets `--desktop-window-right-resize-inset` (`index.css`).
pub(super) const RIGHT_RESIZE_INSET: Pixels = px(if cfg!(windows) { 6. } else { 0. });
/// The three caption buttons, 46px each; macOS draws traffic lights at the left instead.
const CAPTION_BUTTONS: f32 = if cfg!(target_os = "macos") { 0. } else { 138. };
pub(super) const CAPTION_BUTTONS_WIDTH: Pixels = px(CAPTION_BUTTONS);

const CONTROL_SIZE: Pixels = px(28.);
/// `--workspace-controls-left` (0.75rem) plus `ml-px`.
const CONTROLS_LEFT: Pixels = px(13.);
/// `MACOS_TRAFFIC_LIGHTS_LEFT_INSET` (90px) plus `ml-px`: the traffic lights' room.
const MACOS_CONTROLS_LEFT: Pixels = px(91.);
/// Where macOS's traffic lights sit, as Electron's `syncMacosWindowButtons` places them: 16px in
/// and centered on the 52px top row (14px buttons).
pub const TRAFFIC_LIGHTS: (f32, f32) = (16., 19.);
/// Linux client-side decorations: how far in from each edge a press resizes the window.
const RESIZE_EDGE: Pixels = px(5.);
/// The panel toggles end 12px + 1px left of the caption buttons.
const CONTROLS_RIGHT: Pixels = px(CAPTION_BUTTONS + 13.);

/// The sidebar toggle's left edge: past the traffic lights on macOS, except in fullscreen where they hide.
fn controls_left(window: &Window) -> Pixels {
    if cfg!(target_os = "macos") && !window.is_fullscreen() { MACOS_CONTROLS_LEFT } else { CONTROLS_LEFT }
}

/// `useNativeTitlebarHeight`: the caption strip drawn over the window's top (Electron's title bar
/// overlay, or macOS's traffic lights), or 0 where the window manager draws its own titlebar.
pub(super) fn native_titlebar_height(window: &Window) -> Pixels {
    if cfg!(target_os = "linux") && !matches!(window.window_decorations(), Decorations::Client { .. }) {
        return px(0.);
    }
    TITLEBAR_HEIGHT
}

/// Header content starts here while the sidebar is collapsed, clearing its toggle.
pub(super) fn collapsed_content_left(window: &Window) -> Pixels {
    controls_left(window) + CONTROL_SIZE + px(11.)
}

thread_local! {
    /// A press in a drag region, waiting for the pointer to move before it drags the window.
    static DRAG_ARMED: Cell<bool> = const { Cell::new(false) };
}

/// Makes `element` the window's drag region (`-webkit-app-region: drag`). Windows hit-tests
/// [`WindowControlArea::Drag`] itself; macOS and Linux drag on a press-and-move and zoom on a
/// double click, as their native titlebars do.
pub(super) fn drag_region<E: InteractiveElement>(element: E) -> E {
    let element = element.window_control_area(WindowControlArea::Drag);
    if cfg!(windows) {
        return element;
    }
    element
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            let double = event.click_count == 2;
            DRAG_ARMED.set(!double);
            if double {
                // macOS follows the user's "double-click a window's title bar to" setting.
                if cfg!(target_os = "macos") { window.titlebar_double_click() } else { window.zoom_window() }
            }
        })
        .on_mouse_up(MouseButton::Left, |_, _, _| DRAG_ARMED.set(false))
        .on_mouse_move(|event, window, _| {
            if DRAG_ARMED.replace(false) && event.pressed_button == Some(MouseButton::Left) {
                window.start_window_move();
            }
        })
}

/// Linux client-side decorations: the resize edges a native frame has, while the window floats
/// (a maximized or tiled edge doesn't resize). Nothing on Windows and macOS.
pub(super) fn render_resize_edges(window: &Window, cx: &App) -> Option<AnyElement> {
    let Decorations::Client { mut tiling } = window.window_decorations() else { return None };
    if window.is_maximized() || window.is_fullscreen() {
        return None;
    }
    // An X11 window manager that can't show a shadow outside the frame reports a floating window
    // as tiled on every side (see `vendor/README.md`); its edges still resize.
    if cx.compositor_name() == "X11" && tiling == Tiling::tiled() {
        tiling = Tiling::default();
    }
    let corner = RESIZE_EDGE * 2.;
    let edge = |id: &'static str, resize: ResizeEdge, cursor: CursorStyle| {
        div().id(id).absolute().occlude().cursor(cursor).on_mouse_down(MouseButton::Left, move |_, window, cx| {
            cx.stop_propagation();
            window.start_window_resize(resize);
        })
    };
    let (vertical, horizontal) = (CursorStyle::ResizeUpDown, CursorStyle::ResizeLeftRight);
    let (falling, rising) = (CursorStyle::ResizeUpLeftDownRight, CursorStyle::ResizeUpRightDownLeft);
    let sides = [
        (edge("resize-top", ResizeEdge::Top, vertical).top_0().left(corner).right(corner).h(RESIZE_EDGE), tiling.top),
        (
            edge("resize-bottom", ResizeEdge::Bottom, vertical).bottom_0().left(corner).right(corner).h(RESIZE_EDGE),
            tiling.bottom,
        ),
        (
            edge("resize-left", ResizeEdge::Left, horizontal).left_0().top(corner).bottom(corner).w(RESIZE_EDGE),
            tiling.left,
        ),
        (
            edge("resize-right", ResizeEdge::Right, horizontal).right_0().top(corner).bottom(corner).w(RESIZE_EDGE),
            tiling.right,
        ),
        (
            edge("resize-top-left", ResizeEdge::TopLeft, falling).top_0().left_0().size(corner),
            tiling.top || tiling.left,
        ),
        (
            edge("resize-top-right", ResizeEdge::TopRight, rising).top_0().right_0().size(corner),
            tiling.top || tiling.right,
        ),
        (
            edge("resize-bottom-left", ResizeEdge::BottomLeft, rising).bottom_0().left_0().size(corner),
            tiling.bottom || tiling.left,
        ),
        (
            edge("resize-bottom-right", ResizeEdge::BottomRight, falling).bottom_0().right_0().size(corner),
            tiling.bottom || tiling.right,
        ),
    ];
    let edges = sides.into_iter().filter(|(_, tiled)| !tiled).map(|(edge, _)| edge);
    Some(div().absolute().inset_0().children(edges).into_any_element())
}

pub(super) fn icon(path: &'static str) -> Icon {
    Icon::empty().path(path)
}

fn white(alpha: f32) -> Hsla {
    hsla(0., 0., 1., alpha)
}

fn black(alpha: f32) -> Hsla {
    hsla(0., 0., 0., alpha)
}

/// A 28px square titlebar control (`Toggle variant=ghost size=sm` and `SidebarTrigger`).
fn control(id: &'static str) -> Stateful<Div> {
    div().id(id).flex_none().size(CONTROL_SIZE).rounded(px(8.)).flex().items_center().justify_center().occlude()
}

/// A tab stop with the `focus-visible` ring that Enter and Space activate. A press leaves focus
/// where it was, so clicking a toggle never takes it from the composer.
pub(super) fn keyboard_control(el: Stateful<Div>, focus: &FocusHandle, ring: Hsla) -> Stateful<Div> {
    el.track_focus(focus)
        .focus_visible(move |style| style.border_2().border_color(ring))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
}

fn is_keyboard(event: &ClickEvent) -> bool {
    matches!(event, ClickEvent::Keyboard(_))
}

/// The titlebar toggles' tab stops.
pub(super) struct TitlebarFocus {
    sidebar: FocusHandle,
    details: FocusHandle,
    maximize: FocusHandle,
    terminal: FocusHandle,
    right_panel: FocusHandle,
}

impl TitlebarFocus {
    pub(super) fn new(cx: &mut App) -> Self {
        let stop = || cx.focus_handle().tab_stop(true);
        Self { sidebar: stop(), details: stop(), maximize: stop(), terminal: stop(), right_panel: stop() }
    }
}

fn tooltip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView {
    let text = text.into();
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

impl Workspace {
    /// Everything drawn over the top row, above the sidebar and the header.
    pub(super) fn render_titlebar_overlay(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(TITLEBAR_HEIGHT)
            .when(!self.sidebar_open.sheet(), |el| el.child(self.render_sidebar_toggle(window, cx)))
            .map(|el| {
                if self.page == Some(super::pages::Page::PullRequests) && self.settings.is_none() {
                    el.child(self.render_pull_request_page_toggles(cx))
                } else {
                    el.when(self.target.is_some(), |el| el.child(self.render_panel_toggles(window, cx)))
                }
            })
    }

    /// The drawn window buttons, above Settings, dialogs and toasts as Electron's native ones are;
    /// only popups and tooltips, which close on the click, draw over them.
    pub(super) fn render_caption_overlay(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let bar = div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(TITLEBAR_HEIGHT)
            .when(cfg!(windows), |el| el.child(render_caption_buttons(window, cx)))
            .when(
                cfg!(target_os = "linux") && matches!(window.window_decorations(), Decorations::Client { .. }),
                |el| el.child(render_linux_caption_buttons(window, cx)),
            );
        gpui_kit::deferred(bar).with_priority(gpui_kit::base::POPUP_PRIORITY - 1).into_any_element()
    }

    /// `SidebarTrigger` in its `ghost` variant: we draw no stage art behind it.
    pub(super) fn render_sidebar_toggle(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::tokens(cx);
        let (color, hover_bg, hover_color, ring) =
            (tokens.muted_foreground, tokens.accent, tokens.foreground, tokens.ring);
        let path = if self.sidebar_open.shown() { "icons/panel-left-close.svg" } else { "icons/panel-left.svg" };
        div().absolute().left(controls_left(window)).top_0().h(TITLEBAR_HEIGHT).flex().items_center().child(
            keyboard_control(
                control("toggle-sidebar").group("toggle-sidebar").hover(|style| style.bg(hover_bg)),
                // In the sheet it is the sheet's first tab stop.
                if self.sidebar_open.sheet() { &self.sidebar.sheet_toggle } else { &self.titlebar_focus.sidebar },
                ring,
            )
            .child(
                svg()
                    .path(path)
                    .size_4()
                    .text_color(color)
                    .group_hover("toggle-sidebar", |style| style.text_color(hover_color)),
            )
            .tooltip(tooltip(self.with_shortcut("Toggle main sidebar", "sidebar.toggle")))
            .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx)),
        )
    }

    /// The Pull requests page's `openPanelControls`: maximize while its panel shows, and the
    /// right panel toggle, unavailable until a pull request is picked.
    fn render_pull_request_page_toggles(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::tokens(cx);
        let (shown, maximized) = (self.pull_request_panel_shown(), self.pull_request_panel_maximized());
        let available = self.pull_request_panel_open();
        let focus = &self.titlebar_focus;
        // These act on press; Enter and Space reach them as keyboard clicks.
        let toggle = |id: &'static str, path: &'static str, pressed: bool, enabled: bool, stop: &FocusHandle| {
            let color = if enabled { tokens.foreground.opacity(0.8) } else { tokens.muted_foreground };
            control(id)
                .when(pressed, |el| el.bg(tokens.accent))
                .when(enabled, |el| {
                    keyboard_control(el.cursor_pointer().hover(|style| style.bg(tokens.accent)), stop, tokens.ring)
                })
                .child(icon(path).with_size(px(16.)).text_color(color))
        };
        let maximize = shown.then(|| {
            let label = if maximized { "Restore panel size" } else { "Maximize panel" };
            let path = if maximized { "icons/minimize-2.svg" } else { "icons/maximize-2.svg" };
            toggle("toggle-pr-panel-maximized", path, maximized, true, &focus.maximize)
                .tooltip(tooltip(label))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.toggle_pull_request_panel_maximized(cx);
                    }),
                )
                .on_click(cx.listener(|this, event, _, cx| {
                    if is_keyboard(event) {
                        this.toggle_pull_request_panel_maximized(cx);
                    }
                }))
        });
        let panel = toggle("toggle-pr-panel", "icons/panel-right.svg", shown, available, &focus.right_panel)
            .tooltip(tooltip(if available {
                self.with_shortcut("Toggle right panel", "rightPanel.toggle")
            } else {
                "Select a pull request first".into()
            }))
            .when(available, |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.toggle_pull_request_panel(cx);
                    }),
                )
                .on_click(cx.listener(|this, event, _, cx| {
                    if is_keyboard(event) {
                        this.toggle_pull_request_panel(cx);
                    }
                }))
            });
        div()
            .absolute()
            .top_0()
            .right(CONTROLS_RIGHT)
            .h(TITLEBAR_HEIGHT)
            .flex()
            .items_center()
            .gap_1()
            .children(maximize)
            .child(panel)
    }

    /// `WorkspaceTitlebarControls` + `PanelLayoutControls`: thread details, terminal, right panel.
    /// With the panel open, maximize takes the details toggle's place and the details toggle
    /// moves to the chat header's right edge (`ThreadPanelHeaderControl`).
    fn render_panel_toggles(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::tokens(cx);
        let details = self.details_shown(window);
        let panel_width = self.right_panel_width(window);
        let maximized = self.right_panel_maximized();
        let focus = &self.titlebar_focus;
        // These act on press; Enter and Space reach them as keyboard clicks.
        let toggle = |id: &'static str, path: &'static str, pressed: bool, enabled: bool, stop: &FocusHandle| {
            let color = if enabled { tokens.foreground.opacity(0.8) } else { tokens.muted_foreground };
            control(id)
                .when(pressed, |el| el.bg(tokens.accent))
                .when(enabled, |el| {
                    keyboard_control(el.cursor_pointer().hover(|style| style.bg(tokens.accent)), stop, tokens.ring)
                })
                .child(icon(path).with_size(px(16.)).text_color(color))
        };
        let details_toggle = toggle("toggle-thread-details", "icons/square-menu.svg", details, true, &focus.details)
            .tooltip(tooltip(self.with_shortcut("Toggle thread details", "threadPanel.toggle")))
            // Acts on press, like a popover trigger: the popover's outside-press
            // dismissal runs first, so a press that already closed it must not reopen it.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    if this.details_shown(window) == details {
                        this.toggle_details(window, cx);
                    }
                }),
            )
            .on_click(cx.listener(|this, event, window, cx| {
                if is_keyboard(event) {
                    this.toggle_details(window, cx);
                }
            }));
        let (header_details, bar_details) = match panel_width {
            Some(_) if maximized => (None, None),
            Some(_) => (Some(details_toggle), None),
            None => (None, Some(details_toggle)),
        };
        let maximize = panel_width.map(|_| {
            let label = if maximized { "Restore panel size" } else { "Maximize panel" };
            let path = if maximized { "icons/minimize-2.svg" } else { "icons/maximize-2.svg" };
            toggle("toggle-right-panel-maximized", path, maximized, true, &focus.maximize)
                .tooltip(tooltip(label))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.toggle_right_panel_maximized(cx);
                    }),
                )
                .on_click(cx.listener(|this, event, _, cx| {
                    if is_keyboard(event) {
                        this.toggle_right_panel_maximized(cx);
                    }
                }))
        });
        let bar = div()
            .absolute()
            .top_0()
            .right(CONTROLS_RIGHT)
            .h(TITLEBAR_HEIGHT)
            .flex()
            .items_center()
            .gap_1()
            .children(maximize)
            .children(bar_details)
            .child({
                let available = self.terminal_available();
                toggle("toggle-terminal", "icons/panel-bottom.svg", self.terminal_open(), available, &focus.terminal)
                    .tooltip(tooltip(if available {
                        self.with_shortcut("Toggle terminal drawer", "terminal.toggle")
                    } else {
                        "Terminal drawer is unavailable".into()
                    }))
                    // On press, like the details toggle beside it.
                    .when(available, |el| {
                        el.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| this.toggle_terminal(window, cx)),
                        )
                        .on_click(cx.listener(|this, event, window, cx| {
                            if is_keyboard(event) {
                                this.toggle_terminal(window, cx);
                            }
                        }))
                    })
            })
            .child(
                toggle(
                    "toggle-right-panel",
                    "icons/panel-right.svg",
                    self.right_panel_open(),
                    true,
                    &focus.right_panel,
                )
                .tooltip(tooltip(self.with_shortcut("Toggle right panel", "rightPanel.toggle")))
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.dispatch_action(Box::new(ToggleRightPanel), cx);
                })
                .on_click(|event, window, cx| {
                    if is_keyboard(event) {
                        window.dispatch_action(Box::new(ToggleRightPanel), cx);
                    }
                }),
            );
        // `right-[calc(env(safe-area-inset-right)+0.75rem)]` of the chat header, left of the panel.
        let header = header_details.zip(panel_width).map(|(toggle, width)| {
            div()
                .absolute()
                .top_0()
                .right(px(width) + RIGHT_RESIZE_INSET + px(12.))
                .h(TITLEBAR_HEIGHT)
                .flex()
                .items_center()
                .child(toggle)
        });
        div().children(header).child(bar)
    }
}

/// Minimize, maximize/restore and close, drawn like Windows 11's own caption
/// buttons: Segoe Fluent Icons glyphs (Segoe MDL2 Assets on Windows 10), which
/// are hinted to stay crisp at every scale.
fn render_caption_buttons(window: &Window, cx: &App) -> impl IntoElement {
    let dark = theme::tokens(cx).dark;
    // `titleBarOverlay.symbolColor` in `apps/desktop/src/window/DesktopWindow.ts`.
    let glyph: Hsla = rgb(if dark { 0xf8fafc } else { 0x1f2937 }).into();
    let (hover, pressed) = if dark { (white(0.0605), white(0.0419)) } else { (black(0.0373), black(0.0241)) };
    let close_hover: Hsla = rgb(0xc42b1c).into();
    let close_pressed: Hsla = rgb(0xc83c31).into();
    let mut caption_font = font("Segoe Fluent Icons");
    caption_font.fallbacks = Some(FontFallbacks::from_fonts(vec!["Segoe MDL2 Assets".into()]));
    let button = |id: &'static str, glyph_char: &'static str, area, hover: Hsla, pressed: Hsla, hover_glyph: Hsla| {
        div()
            .id(id)
            .w(px(46.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .window_control_area(area)
            .occlude()
            .font(caption_font.clone())
            .text_size(px(10.))
            .line_height(px(10.))
            .text_color(glyph)
            .hover(|style| style.bg(hover).text_color(hover_glyph))
            .active(|style| style.bg(pressed).text_color(hover_glyph))
            .child(Grayscale(div().child(glyph_char).into_any_element()))
    };
    let maximize = if window.is_maximized() { ("restore", "\u{E923}") } else { ("maximize", "\u{E922}") };
    div()
        .absolute()
        .top_0()
        .right_0()
        .h(TITLEBAR_HEIGHT)
        .flex()
        .child(button("minimize", "\u{E921}", WindowControlArea::Min, hover, pressed, glyph))
        .child(button(maximize.0, maximize.1, WindowControlArea::Max, hover, pressed, glyph))
        .child(button("close", "\u{E8BB}", WindowControlArea::Close, close_hover, close_pressed, white(1.)))
}

/// Paints its child's text with grayscale antialiasing, as Windows draws its caption glyphs;
/// `ClearType`'s color fringes show on their hairline strokes. The mode is app-wide, so it is
/// set only around this paint.
struct Grayscale(AnyElement);

impl IntoElement for Grayscale {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Grayscale {
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
        (self.0.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.prepaint(window, cx);
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
        let previous = cx.text_rendering_mode();
        cx.set_text_rendering_mode(TextRenderingMode::Grayscale);
        self.0.paint(window, cx);
        cx.set_text_rendering_mode(previous);
    }
}

/// Minimize, maximize/restore and close as Electron's titlebar overlay draws them on Linux, with
/// Lucide glyphs since Linux has no system caption font. Shown whenever the window draws its own
/// decorations, which on X11 includes running without a compositor.
fn render_linux_caption_buttons(window: &Window, cx: &App) -> impl IntoElement {
    let dark = theme::tokens(cx).dark;
    let glyph: Hsla = rgb(if dark { 0xf8fafc } else { 0x1f2937 }).into();
    let hover = if dark { white(0.0605) } else { black(0.0373) };
    let close_hover: Hsla = rgb(0xc42b1c).into();
    let button = |id: &'static str, path: &'static str, hover: Hsla, hover_glyph: Hsla| {
        div()
            .id(id)
            .group(id)
            .w(px(46.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .hover(|style| style.bg(hover))
            .child(
                svg().path(path).size(px(14.)).text_color(glyph).group_hover(id, |style| style.text_color(hover_glyph)),
            )
    };
    let maximize = if window.is_maximized() { "icons/copy.svg" } else { "icons/square.svg" };
    div()
        .absolute()
        .top_0()
        .right_0()
        .h(TITLEBAR_HEIGHT)
        .flex()
        .child(button("minimize", "icons/minus.svg", hover, glyph).on_click(|_, window, _| window.minimize_window()))
        .child(button("maximize", maximize, hover, glyph).on_click(|_, window, _| window.zoom_window()))
        .child(button("close", "icons/x.svg", close_hover, white(1.)).on_click(|_, window, _| window.remove_window()))
}
