//! At 980px or narrower the right panel leaves the row and opens as a sheet over the chat
//! (`rightPanelLayout.ts`, `RightPanelSheet.tsx`), unless the user maximized it. The sheet is a
//! modal dialog: a press outside it or Escape closes the panel, and focus moves into it as it
//! shows and back to where it was as it hides.

use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::{
    AnyElement, BoxShadow, Context, Hsla, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _,
    Styled as _, Window, div, hsla, px,
};

use crate::app::sidebar_sheet::FocusMove;
use crate::app::{Workspace, titlebar};
use crate::theme;

/// `RIGHT_PANEL_INLINE_LAYOUT_MEDIA_QUERY` is `(max-width: 980px)`.
const SHEET_AT_OR_BELOW: f32 = 980.;
/// `max-[760px]:` is Tailwind's `(width < 760px)`.
const NARROW_BELOW: f32 = 760.;

/// `shouldUsePlanSidebarSheet`: a window at the breakpoint shows the panel as a sheet unless the
/// user maximized it, which keeps it in the row.
pub(super) fn uses_sheet(viewport: f32, maximized: bool) -> bool {
    viewport <= SHEET_AT_OR_BELOW && !maximized
}

/// `w-[min(42vw,28rem)] min-w-80 max-w-[28rem]`, and below 760px `w-[min(88vw,24rem)] min-w-0`.
pub(super) fn sheet_width(viewport: f32) -> f32 {
    if viewport < NARROW_BELOW { (viewport * 0.88).min(384.) } else { (viewport * 0.42).clamp(320., 448.) }
}

fn black(alpha: f32) -> Hsla {
    hsla(0., 0., 0., alpha)
}

impl Workspace {
    fn panel_maximized_flag(&self) -> bool {
        self.panel_state().is_some_and(|panel| panel.maximized)
    }

    /// The open panel shows as a sheet over the chat.
    pub(in crate::app) fn right_panel_sheet(&self, window: &Window) -> bool {
        self.right_panel_open() && uses_sheet(f32::from(window.viewport_size().width), self.panel_maximized_flag())
    }

    /// The `enabled` flag of `useProactivePanels`: off while the window would show the panel as a
    /// sheet, so a finished turn never covers the chat.
    pub(super) fn proactive_panels_enabled(&self) -> bool {
        self.client_settings.values.bool("proactivePanelsEnabled")
            && !uses_sheet(self.right_panel.viewport_width, self.panel_maximized_flag())
    }

    /// Moves focus into the sheet as it shows and back out as it hides, whichever way it closed:
    /// the backdrop, Escape, or the toggle. A sheet that widens or maximizes into the row keeps
    /// focus where it is.
    pub(super) fn sync_right_panel_sheet_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.right_panel_sheet(window);
        let focused = window.focused(cx);
        // The last frame drew the sheet, so its focus path still tells.
        let in_sheet = !self.right_panel_open()
            && (focused.is_none()
                || self.focus.is_focused(window)
                || self.right_panel.sheet_trap.contains_focused(window, cx));
        let moved = self.right_panel.sheet_focus.follow(shown, || focused, || in_sheet);
        let entering = std::mem::take(&mut self.right_panel.sheet_entering);
        match moved {
            // The opening action may have focused something the sheet holds, which only its
            // first frame tells; focus moves in on the next one if it did not.
            Some(FocusMove::IntoSheet) => {
                self.right_panel.sheet_entering = true;
                window.request_animation_frame();
            }
            Some(FocusMove::Back(Some(handle))) => handle.focus(window, cx),
            Some(FocusMove::Back(None)) => self.focus.focus(window, cx),
            None if shown && entering && !self.right_panel.sheet_trap.contains_focused(window, cx) => {
                self.areas.panel.focus(window, cx);
            }
            None => {}
        }
    }

    /// `RightPanelSheet`: the backdrop over the window and the panel at its right edge. Under
    /// Electron's titlebar overlay (`wco:mt-(--workspace-topbar-height)`) it starts below the top row.
    pub(in crate::app) fn render_right_panel_sheet(
        &self,
        panel: AnyElement,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *theme::tokens(cx);
        let top = if cfg!(target_os = "macos") { px(0.) } else { titlebar::native_titlebar_height(window) };
        div()
            .absolute()
            .inset_0()
            .child(
                // `SheetBackdrop`'s `bg-background/60`; its `backdrop-blur-xs` has no GPUI equivalent.
                div()
                    .id("right-panel-sheet-backdrop")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(tokens.background.opacity(0.6))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.close_right_panel(cx);
                        }),
                    ),
            )
            .child(
                div()
                    .id("right-panel-sheet")
                    .absolute()
                    .top(top)
                    .right_0()
                    .bottom_0()
                    .occlude()
                    // `bg-popover border-s shadow-lg/5`; the panel inside paints `bg-background`.
                    .bg(tokens.popover)
                    .shadow(vec![
                        BoxShadow::new(px(0.), px(10.), black(0.05)).blur_radius(px(15.)).spread_radius(px(-3.)),
                        BoxShadow::new(px(0.), px(4.), black(0.05)).blur_radius(px(6.)).spread_radius(px(-4.)),
                    ])
                    .focus_trap("right-panel-sheet-trap", &self.right_panel.sheet_trap)
                    .child(panel),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width_at(viewport: f32) -> f32 {
        (sheet_width(viewport) * 10.).round() / 10.
    }

    #[test]
    fn the_sheet_breakpoints_match_the_web() {
        assert!(uses_sheet(980., false));
        assert!(!uses_sheet(981., false));
        assert!(!uses_sheet(600., true), "maximized, the panel takes the row instead");
        // `min(42vw, 28rem)`, held at `min-w-80`.
        assert_eq!(width_at(980.), 411.6);
        assert_eq!(width_at(760.), 320.);
        assert_eq!(width_at(1200.), 448.);
        // Below 760px: `min(88vw, 24rem)` with no minimum.
        assert_eq!(width_at(759.), 384.);
        assert_eq!(width_at(400.), 352.);
    }
}
