//! Window chrome on the 40px top row: the sidebar toggle, the panel layout
//! toggles and, on Windows, the caption buttons Electron's titlebar overlay
//! draws. Windows maps each `WindowControlArea` to its native hit-test code,
//! so drag, double-click maximize, snap layouts and the resize edges stay native.
//!
//! A button inside a drag area must `occlude()`: otherwise the drag area is
//! still under the pointer and Windows treats the press as a caption drag.

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Context, Div, FontFallbacks, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Pixels, Stateful, StatefulInteractiveElement as _, Styled as _, Window, WindowControlArea, div, font, hsla, px,
    rgb, svg,
};

use super::{ToggleSidebar, Workspace};
use crate::theme;

/// `--workspace-topbar-height` with Electron's 40px titlebar overlay on Windows.
pub(super) const TITLEBAR_HEIGHT: Pixels = px(40.);
/// `#root` keeps this much free at the right so the resize edge stays grabbable (`index.css:1764`).
pub(super) const RIGHT_RESIZE_INSET: Pixels = px(6.);
/// Header content starts here while the sidebar is collapsed, clearing its toggle.
pub(super) const COLLAPSED_CONTENT_LEFT: Pixels = px(52.);
/// The three caption buttons, 46px each.
const CAPTION_BUTTONS: f32 = if cfg!(target_os = "windows") { 138. } else { 0. };
pub(super) const CAPTION_BUTTONS_WIDTH: Pixels = px(CAPTION_BUTTONS);

const CONTROL_SIZE: Pixels = px(28.);
const CONTROLS_LEFT: Pixels = px(13.);
/// The panel toggles end 12px + 1px left of the caption buttons.
const CONTROLS_RIGHT: Pixels = px(CAPTION_BUTTONS + 13.);

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

fn tooltip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> gpui_kit::AnyView {
    move |window, cx| Tooltip::new(text).build(window, cx)
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
            .child(self.render_sidebar_toggle(cx))
            .when(self.target.is_some(), |el| el.child(self.render_panel_toggles(window, cx)))
            .when(cfg!(target_os = "windows"), |el| el.child(render_caption_buttons(window, cx)))
    }

    /// `SidebarTrigger` in its `ghost` variant: we draw no stage art behind it.
    fn render_sidebar_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::tokens(cx);
        let (color, hover_bg, hover_color) = (tokens.muted_foreground, tokens.accent, tokens.foreground);
        let path = if self.sidebar_open { "icons/panel-left-close.svg" } else { "icons/panel-left.svg" };
        div().absolute().left(CONTROLS_LEFT).top_0().h(TITLEBAR_HEIGHT).flex().items_center().child(
            control("toggle-sidebar")
                .group("toggle-sidebar")
                .hover(|style| style.bg(hover_bg))
                .child(
                    svg()
                        .path(path)
                        .size_4()
                        .text_color(color)
                        .group_hover("toggle-sidebar", |style| style.text_color(hover_color)),
                )
                .tooltip(tooltip("Toggle main sidebar (Ctrl+B)"))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(ToggleSidebar), cx)),
        )
    }

    /// `WorkspaceTitlebarControls` + `PanelLayoutControls`: thread details, terminal, right panel.
    fn render_panel_toggles(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = theme::tokens(cx);
        let details = self.details_shown(window);
        let toggle = |id: &'static str, path: &'static str, pressed: bool, enabled: bool| {
            let color = if enabled { tokens.foreground.opacity(0.8) } else { tokens.muted_foreground };
            control(id)
                .when(pressed, |el| el.bg(tokens.accent))
                .when(enabled, |el| el.cursor_pointer().hover(|style| style.bg(tokens.accent)))
                .child(icon(path).with_size(px(16.)).text_color(color))
        };
        div()
            .absolute()
            .top_0()
            .right(CONTROLS_RIGHT)
            .h(TITLEBAR_HEIGHT)
            .flex()
            .items_center()
            .gap_1()
            .child(
                toggle("toggle-thread-details", "icons/square-menu.svg", details, true)
                    .tooltip(tooltip("Toggle thread details (Ctrl+Shift+B)"))
                    // Acts on press, like a popover trigger: the popover's outside-press
                    // dismissal runs first, so a press that already closed it must not reopen it.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            if this.details_shown(window) == details {
                                this.toggle_details(window, cx);
                            }
                        }),
                    ),
            )
            .child(
                toggle("toggle-terminal", "icons/panel-bottom.svg", false, false)
                    .tooltip(tooltip("Terminal drawer is unavailable")),
            )
            .child({
                // The browser is this client's only right-panel surface so far.
                let browser = matches!(self.target, Some(super::Target::Thread(_)));
                toggle("toggle-right-panel", "icons/panel-right.svg", self.preview_panel_open(), browser)
                    .tooltip(tooltip(if browser {
                        "Toggle browser (Ctrl+Shift+J)"
                    } else {
                        "Start the thread to open a browser"
                    }))
                    .when(browser, |el| el.on_click(cx.listener(|this, _, window, cx| this.toggle_preview(window, cx))))
            })
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
            .child(glyph_char)
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
