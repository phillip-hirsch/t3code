//! Below the web's `md` breakpoint the sidebar leaves the row and opens as a sheet over the
//! window (`ui/sidebar.tsx`: `isMobile`, `openMobile`). The sheet keeps its own open state, so
//! the docked sidebar's state survives a trip through a narrow window.

use gpui_kit::{
    AnyElement, AnyView, Context, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _, Pixels,
    Styled as _, Window, div,
};

use super::Workspace;

/// `SIDEBAR_MOBILE_QUERY` is `(max-width: 767px)`.
const SHEET_BELOW: f32 = 768.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SidebarOpen {
    /// `open`: the docked sidebar, from a wide window.
    docked: bool,
    /// `openMobile`: the sheet, from a narrow window.
    sheet: bool,
    narrow: bool,
}

impl Default for SidebarOpen {
    fn default() -> Self {
        Self { docked: true, sheet: false, narrow: false }
    }
}

impl SidebarOpen {
    /// Follows the window width; true when the sidebar switched between docked and sheet.
    pub(super) fn fit(&mut self, viewport_width: Pixels) -> bool {
        let narrow = f32::from(viewport_width) < SHEET_BELOW;
        std::mem::replace(&mut self.narrow, narrow) != narrow
    }

    /// `useSidebarVisibility`: what the toggle shows as pressed.
    pub(super) fn shown(self) -> bool {
        if self.narrow { self.sheet } else { self.docked }
    }

    /// The sidebar takes its column in the row.
    pub(super) fn docked(self) -> bool {
        self.docked && !self.narrow
    }

    /// The sidebar shows as a sheet over the window.
    pub(super) fn sheet(self) -> bool {
        self.sheet && self.narrow
    }

    /// `toggleSidebar`: the sheet in a narrow window, the docked sidebar otherwise.
    pub(super) fn toggle(&mut self) {
        if self.narrow {
            self.sheet = !self.sheet;
        } else {
            self.docked = !self.docked;
        }
    }

    /// `if (isMobile) setOpenMobile(false)`: navigating from the sheet closes it.
    pub(super) fn close_sheet(&mut self) {
        if self.narrow {
            self.sheet = false;
        }
    }
}

impl Workspace {
    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open.toggle();
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// The sidebar as a `Sheet` from the left over a dimmed backdrop that closes it on a press.
    /// Electron sizes the sheet to its content: `SIDEBAR_WIDTH_MOBILE` puts `--spacing(3)` in an
    /// inline style, where it is invalid, so the popup's width falls back to `auto`.
    pub(super) fn render_sidebar_sheet(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.sidebar_open.sheet() {
            return None;
        }
        let tokens = crate::theme::tokens(cx);
        let sheet = AnyView::from(self.sidebar_view.clone());
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    // `SheetBackdrop`: `bg-background/60` (the web also blurs it slightly).
                    div()
                        .id("sidebar-sheet-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(tokens.background.opacity(0.6))
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.sidebar_open.close_sheet();
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    div()
                        .id("sidebar-sheet")
                        .absolute()
                        .top_0()
                        .left_0()
                        .bottom_0()
                        .max_w(window.viewport_size().width)
                        .occlude()
                        .shadow_lg()
                        .child(sheet),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::px;

    fn at(width: f32) -> SidebarOpen {
        let mut open = SidebarOpen::default();
        open.fit(px(width));
        open
    }

    #[test]
    fn the_breakpoint_is_the_webs_md() {
        assert!(at(768.).docked());
        let narrow = at(767.);
        assert!(!narrow.docked() && !narrow.sheet() && !narrow.shown());
    }

    #[test]
    fn a_narrow_toggle_opens_the_sheet_and_keeps_the_docked_state() {
        let mut open = at(700.);
        open.toggle();
        assert!(open.sheet() && open.shown() && !open.docked());
        open.close_sheet();
        assert!(!open.sheet());

        // Widening brings the docked sidebar back; a wide toggle never touches the sheet.
        open.toggle();
        assert!(open.fit(px(1000.)));
        assert!(open.docked() && !open.sheet());
        open.toggle();
        assert!(!open.shown());
        assert!(!open.fit(px(1200.)));

        // Narrowing again shows the sheet the user left open, as the web's dialog does.
        open.fit(px(700.));
        assert!(open.sheet());
        open.fit(px(1000.));
        assert!(!open.docked(), "the docked sidebar stays closed");
    }
}
