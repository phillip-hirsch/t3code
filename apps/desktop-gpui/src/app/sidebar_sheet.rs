//! Below the web's `md` breakpoint the sidebar leaves the row and opens as a sheet over the
//! window (`ui/sidebar.tsx`: `isMobile`, `openMobile`). The sheet keeps its own open state, so
//! the docked sidebar's state survives a trip through a narrow window. The docked state also
//! survives a relaunch, as the web keeps it in `localStorage` (`chat_thread_sidebar_open`).

use std::path::{Path, PathBuf};

use gpui_kit::base::FocusTrapElement as _;
use gpui_kit::{
    AnyElement, AnyView, Context, InteractiveElement as _, IntoElement as _, MouseButton, ParentElement as _, Pixels,
    Styled as _, Window, div, px,
};

use super::Workspace;

/// `SIDEBAR_MOBILE_QUERY` is `(max-width: 767px)`.
const SHEET_BELOW: f32 = 768.;

/// Where the sidebar's rows are drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowsSurface {
    Hidden,
    Docked,
    Sheet,
}

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
    /// The docked state the last session left, else open (`readStoredSidebarOpen ?? defaultOpen`).
    pub(super) fn restored(stored: Option<bool>) -> Self {
        Self { docked: stored.unwrap_or(true), ..Self::default() }
    }

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

    /// Where the rows are drawn; rows on another surface are new rows, as the web remounts them.
    pub(super) fn surface(self) -> RowsSurface {
        match (self.docked(), self.sheet()) {
            (true, _) => RowsSurface::Docked,
            (false, true) => RowsSurface::Sheet,
            (false, false) => RowsSurface::Hidden,
        }
    }

    /// `toggleSidebar`: the sheet in a narrow window, the docked sidebar otherwise. Returns the
    /// docked state to store when that is what changed; the sheet's state is never stored.
    pub(super) fn toggle(&mut self) -> Option<bool> {
        if self.narrow {
            self.sheet = !self.sheet;
            None
        } else {
            self.docked = !self.docked;
            Some(self.docked)
        }
    }

    /// `if (isMobile) setOpenMobile(false)`: navigating from the sheet closes it.
    pub(super) fn close_sheet(&mut self) {
        if self.narrow {
            self.sheet = false;
        }
    }
}

/// Where the docked state is kept between launches.
pub(super) fn docked_path() -> Option<PathBuf> {
    Some(crate::settings::userdata_dir()?.join("chat-thread-sidebar-open.json"))
}

/// A missing or unreadable file leaves the default.
pub(super) fn read_docked(path: &Path) -> Option<bool> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

pub(super) fn write_docked(path: &Path, docked: bool) {
    if let Err(error) = crate::settings::write_atomically(path, &docked.into()) {
        log::warn!("could not save {}: {error}", path.display());
    }
}

impl RowsSurface {
    /// Moves to `now`; true when the rows left their surface. Rows that stop drawing never see
    /// the pointer leave, so whatever they tracked of it (the hovered row, its hover card) is stale.
    pub(super) fn follow(&mut self, now: Self) -> bool {
        std::mem::replace(self, now) != now
    }
}

/// The web's sheet is a modal dialog: focus moves into it when it shows, to its first tab stop (the
/// "Toggle main sidebar" button), and returns to where it was when it hides (Base UI's
/// `initialFocus` and `finalFocus`), so keys never reach the composer it covers.
#[derive(Debug)]
pub(super) struct SheetFocus<F> {
    /// While the sheet shows: what had focus when it opened.
    opened_from: Option<Option<F>>,
}

impl<F> Default for SheetFocus<F> {
    fn default() -> Self {
        Self { opened_from: None }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum FocusMove<F> {
    IntoSheet,
    /// Back to what had focus, or to the window when nothing did.
    Back(Option<F>),
}

impl<F> SheetFocus<F> {
    /// Follows the sheet showing or hiding. A pick that already moved focus out of the sheet,
    /// such as a thread's composer or the palette, keeps it there.
    pub(super) fn follow(
        &mut self,
        shown: bool,
        focused: impl FnOnce() -> Option<F>,
        focus_in_sheet: impl FnOnce() -> bool,
    ) -> Option<FocusMove<F>> {
        match (shown, self.opened_from.is_some()) {
            (true, false) => {
                self.opened_from = Some(focused());
                Some(FocusMove::IntoSheet)
            }
            (false, true) => {
                let back = self.opened_from.take().flatten();
                focus_in_sheet().then_some(FocusMove::Back(back))
            }
            _ => None,
        }
    }
}

impl Workspace {
    /// Moves focus into the sheet as it shows and back out as it hides, whichever way it closed:
    /// the backdrop, Escape, a pick, or a window widened past the breakpoint.
    pub(super) fn sync_sheet_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A press on the sheet's bare areas focuses the sheet itself, or leaves focus with the
        // window; the list takes it so the arrows keep working.
        let on_no_control =
            window.focused(cx).is_none() || self.focus.is_focused(window) || self.sheet_trap.is_focused(window);
        let trap = &self.sheet_trap;
        let shown = self.sidebar_open.sheet();
        let moved = self.sheet_focus.follow(
            shown,
            || window.focused(cx),
            // The last frame drew the sheet, so its focus path still tells.
            || on_no_control || trap.contains_focused(window, cx),
        );
        match moved {
            Some(FocusMove::IntoSheet) => self.sidebar.sheet_toggle.focus(window, cx),
            Some(FocusMove::Back(Some(handle))) => handle.focus(window, cx),
            Some(FocusMove::Back(None)) => self.focus.focus(window, cx),
            None if shown && on_no_control => self.sidebar.list_focus.focus(window, cx),
            None => {}
        }
    }

    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        // A hidden sidebar would still save the toggle, so the shortcut does nothing without one.
        if self.sidebar_hidden() {
            return;
        }
        if let (Some(docked), Some(path)) = (self.sidebar_open.toggle(), docked_path()) {
            write_docked(&path, docked);
        }
        self.notify_sidebar(cx);
        cx.notify();
    }

    /// The sidebar as a `Sheet` from the left; a press anywhere else closes it. It is the
    /// window's width less 12 px (`SIDEBAR_WIDTH_MOBILE`), whatever its content.
    /// Setup starts with an empty workspace, so the Welcome page has no thread list.
    pub(super) fn sidebar_hidden(&self) -> bool {
        self.page == Some(super::pages::Page::Welcome)
    }

    pub(super) fn render_sidebar_sheet(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.sidebar_open.sheet() || self.sidebar_hidden() {
            return None;
        }
        let sheet = AnyView::from(self.sidebar_view.clone());
        Some(
            div()
                .absolute()
                .inset_0()
                .child(
                    // Base UI draws no `SheetBackdrop` for this sheet (it counts as a nested dialog),
                    // so the window behind stays undimmed; the full-window `SheetViewport` still
                    // takes the press that closes it.
                    div().id("sidebar-sheet-backdrop").absolute().inset_0().occlude().on_mouse_down(
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
                        .w(px((f32::from(window.viewport_size().width) - 12.).max(0.)))
                        .occlude()
                        .shadow_lg()
                        .focus_trap("sidebar-sheet-trap", &self.sheet_trap)
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

    #[test]
    fn only_the_docked_state_is_stored_and_restored() {
        let mut open = at(1000.);
        assert_eq!(open.toggle(), Some(false));
        open.fit(px(700.));
        assert_eq!(open.toggle(), None, "the sheet's state is not stored");

        let dir = std::env::temp_dir().join(format!("t3-gpui-sidebar-open-{}", std::process::id()));
        let path = dir.join("chat-thread-sidebar-open.json");
        assert_eq!(read_docked(&path), None);
        write_docked(&path, false);
        let mut restored = SidebarOpen::restored(read_docked(&path));
        restored.fit(px(1000.));
        assert!(!restored.docked() && !restored.shown());
        assert!(SidebarOpen::restored(None).docked());
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn rows_hidden_by_a_resize_and_shown_in_the_sheet_are_new_rows() {
        let mut open = at(1000.);
        let mut rows = open.surface();
        assert_eq!(rows, RowsSurface::Docked);
        assert!(!rows.follow(open.surface()), "a frame with no change keeps the rows");
        open.fit(px(700.));
        assert!(rows.follow(open.surface()), "narrowing hides the docked rows");
        open.toggle();
        assert!(rows.follow(open.surface()), "the sheet draws its own rows");
        assert_eq!(rows, RowsSurface::Sheet);
        open.close_sheet();
        assert!(rows.follow(open.surface()));
    }

    #[test]
    fn the_sheet_takes_focus_and_hands_it_back() {
        let mut focus = SheetFocus::default();
        let in_sheet = || true;
        assert_eq!(focus.follow(false, || Some("composer"), in_sheet), None);
        assert_eq!(focus.follow(true, || Some("composer"), in_sheet), Some(FocusMove::IntoSheet));
        assert_eq!(focus.follow(true, || Some("list"), in_sheet), None, "focus moves in once");
        assert_eq!(focus.follow(false, || Some("list"), in_sheet), Some(FocusMove::Back(Some("composer"))));
        assert_eq!(focus.follow(false, || None, in_sheet), None);

        // Opened over nothing focused, it hands focus to the window.
        focus.follow(true, || None, in_sheet);
        assert_eq!(focus.follow(false, || None, in_sheet), Some(FocusMove::Back(None)));
    }

    #[test]
    fn a_pick_that_moved_focus_out_keeps_it() {
        let mut focus = SheetFocus::default();
        focus.follow(true, || Some("composer"), || true);
        assert_eq!(focus.follow(false, || Some("palette"), || false), None);
        // The next opening remembers afresh.
        assert_eq!(focus.follow(true, || Some("palette"), || true), Some(FocusMove::IntoSheet));
        assert_eq!(focus.follow(false, || None, || true), Some(FocusMove::Back(Some("palette"))));
    }
}
