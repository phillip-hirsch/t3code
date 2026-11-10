//! The separate preview window (Electron's picture-in-picture `BrowserWindow`, toggled from the
//! More menu and the mini player): a tab's own page moves into a small window that floats above
//! the app, and back into the panel when that window closes.
//!
//! Electron mirrors the page into its window as frames. On Windows the native view itself moves,
//! so the page stays live and interactive there. `WKWebView` and `WebKitGTK` views can't move
//! without `unsafe`, so on macOS and Linux the page is built again in the other window at its URL,
//! on the same profile, so cookies and logins stay (not an incognito tab's), losing its scroll
//! position, in-page state and back/forward history.
//! The panel and mini player show where it went.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Icon, Sizable as _, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowKind, WindowOptions, div, px,
    rgb, size,
};
use gpui_webview::WebView;

use super::{PageEvents, PageSource, Workspace};
use crate::preview::{self, Appearance, TabRef};
use crate::theme::Tokens;

/// `PICTURE_IN_PICTURE_INITIAL_WIDTH` / `_HEIGHT` and `_MIN_WIDTH` / `_MIN_HEIGHT`.
const INITIAL_SIZE: (f32, f32) = (480., 320.);
const MIN_SIZE: (f32, f32) = (240., 160.);
/// The window's `backgroundColor`, around a fixed viewport.
const BACKGROUND: u32 = 0x11_11_11;

/// A tab's open separate window, and the view it holds.
pub(super) struct Separate {
    window: WindowHandle<SeparatePreview>,
    view: Entity<WebView>,
}

/// What the window shows of its tab.
#[derive(Clone, Debug, PartialEq)]
struct Look {
    /// The page's title.
    title: String,
    /// A fixed viewport's size; `None` fills the window.
    viewport: Option<(f32, f32)>,
    zoom: f64,
    appearance: Appearance,
}

/// The window's root: the tab's page, fit like the panel fits a fixed viewport.
pub(super) struct SeparatePreview {
    tab: TabRef,
    view: Entity<WebView>,
    look: Look,
    /// The zoom and colour scheme last applied to the native view, and the title last shown.
    applied: Option<(f64, Appearance)>,
    titled: Option<SharedString>,
    _workspace: Subscription,
    _released: Subscription,
}

/// `Preview · <title>`, else `Browser preview`.
fn window_title(page_title: &str) -> SharedString {
    match page_title.trim() {
        "" => "Browser preview".into(),
        title => format!("Preview \u{b7} {title}").into(),
    }
}

/// `fitPictureInPictureContentSize`: `current`'s area at `aspect`, grown to the minimum size.
fn fit_content_size(current: (f32, f32), aspect: f32) -> (f32, f32) {
    let aspect = if aspect.is_finite() && aspect > 0. { aspect } else { INITIAL_SIZE.0 / INITIAL_SIZE.1 };
    let area = current.0.max(1.) * current.1.max(1.);
    let width = (area * aspect).sqrt();
    let height = width / aspect;
    let scale = (MIN_SIZE.0 / width).max(MIN_SIZE.1 / height).max(1.);
    ((width * scale).round(), (height * scale).round())
}

/// Keeps the window above other apps' windows, as Electron's `alwaysOnTop` does; GPUI's
/// floating windows on Windows are ordinary ones. Runs outside GPUI updates: it re-enters GPUI.
#[cfg(windows)]
fn keep_on_top(hwnd: isize) {
    use winsafe::{self as w, co};
    let placed = crate::desktop::top_level_window(hwnd).ok_or(co::ERROR::INVALID_WINDOW_HANDLE).and_then(|window| {
        window.SetWindowPos(
            w::HwndPlace::Place(co::HWND_PLACE::TOPMOST),
            w::POINT::with(0, 0),
            w::SIZE::with(0, 0),
            co::SWP::NOMOVE | co::SWP::NOSIZE | co::SWP::NOACTIVATE,
        )
    });
    if let Err(error) = placed {
        log::warn!("could not keep the separate preview window on top: {error}");
    }
}

/// Puts `view`'s page in `window`: the view itself where it can move (WebView2), else a new view
/// built from `source`, which `None` means nothing needs (the tab closed).
fn place_page(
    view: &Entity<WebView>,
    source: Option<&PageSource>,
    window: &mut Window,
    cx: &mut gpui_kit::App,
) -> wry::Result<Option<(Entity<WebView>, PageEvents)>> {
    #[cfg(windows)]
    {
        let _ = source;
        view.read(cx).raw().reparent_as_child(window).map(|()| None)
    }
    #[cfg(not(windows))]
    {
        let _ = view;
        let Some(source) = source else { return Ok(None) };
        let (webview, events) = source.build(window, cx)?;
        Ok(Some((cx.new(|_| webview), events)))
    }
}

impl SeparatePreview {
    fn new(
        workspace: &Entity<Workspace>,
        tab: TabRef,
        view: Entity<WebView>,
        look: Look,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The workspace notifies often; only a change to this tab's look redraws the window.
        let observe = cx.observe(workspace, |this, workspace, cx| {
            let Some(look) = workspace.read(cx).separate_look(&this.tab) else { return };
            if look != this.look {
                this.look = look;
                cx.notify();
            }
        });
        // The main window closed: nothing would bring the page back.
        let released = cx.observe_release_in(workspace, window, |_, _, window, _| window.remove_window());
        Self { tab, view, look, applied: None, titled: None, _workspace: observe, _released: released }
    }
}

impl Render for SeparatePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = window_title(&self.look.title);
        if self.titled.as_ref() != Some(&title) {
            window.set_window_title(&title);
            self.titled = Some(title);
        }
        let available = window.viewport_size();
        let fit = self
            .look
            .viewport
            .map(|viewport| preview::fit_viewport(viewport, (f32::from(available.width), f32::from(available.height))));
        let look = (self.look.zoom * f64::from(fit.map_or(1., |(_, scale)| scale)), self.look.appearance);
        if self.applied.is_none_or(|(zoom, appearance)| (zoom - look.0).abs() > 1e-6 || appearance != look.1) {
            let view = self.view.read(cx);
            let _ = view.set_zoom(look.0);
            let _ = view.set_theme(super::wry_theme(look.1));
            self.applied = Some(look);
        }
        let page = match fit {
            Some(((width, height), _)) => div().w(px(width)).h(px(height)),
            None => div().size_full(),
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(BACKGROUND))
            .child(page.child(self.view.clone()))
    }
}

impl Workspace {
    /// Whether `tab`'s page is in its separate window.
    pub(super) fn separated(&self, tab: &TabRef) -> bool {
        self.preview.separate.contains_key(tab)
    }

    fn separate_look(&self, tab: &TabRef) -> Option<Look> {
        let session = self.preview.threads.session(tab)?;
        let hosted = self.preview.tabs.get(tab)?;
        let (zoom, appearance) = self.tab_look(tab, session);
        Some(Look {
            title: hosted.status.title().unwrap_or_default().to_owned(),
            viewport: session.viewport.size(),
            zoom,
            appearance,
        })
    }

    /// `toggleNativePictureInPicture`. `window` is the main window.
    pub(super) fn toggle_separate_window(&mut self, tab: TabRef, window: &mut Window, cx: &mut Context<Self>) {
        if self.separated(&tab) {
            self.close_separate_window(&tab, window, cx);
        } else {
            self.open_separate_window(tab, window, cx);
        }
    }

    /// Moves `tab`'s page into a new separate window at the page's aspect ratio.
    fn open_separate_window(&mut self, tab: TabRef, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(look), Some(hosted)) = (self.separate_look(&tab), self.preview.tabs.get(&tab)) else { return };
        let view = hosted.view.clone();
        let page = self.page_source(&tab, cx);
        let panel = self.preview.panel_area.as_ref().filter(|(shown, _)| *shown == tab).map(|(_, area)| *area);
        let source = look.viewport.or(panel).unwrap_or(INITIAL_SIZE);
        let (width, height) = fit_content_size(INITIAL_SIZE, source.0 / source.1.max(1.));
        let options = WindowOptions {
            titlebar: Some(TitlebarOptions {
                title: Some(window_title(&look.title)),
                appears_transparent: false,
                traffic_light_position: None,
            }),
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                window.display(cx).map(|display| display.id()),
                size(px(width), px(height)),
                cx,
            ))),
            window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
            // An `NSPanel` at the floating level on macOS; on X11, kept above the main window.
            kind: WindowKind::Floating,
            is_minimizable: false,
            app_id: Some(crate::desktop::APP_ID.to_owned()),
            ..Default::default()
        };
        let workspace = cx.entity();
        let main = window.window_handle();
        let mut placed = Ok(None);
        #[cfg(windows)]
        let mut hwnd = None;
        let opened = cx.open_window(options, |separate, cx| {
            placed = place_page(&view, page.as_ref(), separate, cx);
            let shown = match &placed {
                Ok(Some((rebuilt, _))) => rebuilt.clone(),
                _ => view.clone(),
            };
            #[cfg(windows)]
            {
                use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
                hwnd = match separate.window_handle().map(|handle| handle.as_raw()) {
                    Ok(RawWindowHandle::Win32(handle)) => Some(handle.hwnd.get()),
                    _ => None,
                };
            }
            // Closing the window hands the page back first: closing it with the page inside
            // would destroy the page with it.
            let (closing, owner) = (tab.clone(), workspace.downgrade());
            separate.on_window_should_close(cx, move |_, cx| {
                if owner.upgrade().is_none() {
                    return true;
                }
                let (tab, owner) = (closing.clone(), owner.clone());
                cx.defer(move |cx| {
                    let _ = main.update(cx, |_, window, cx| {
                        owner.update(cx, |this, cx| this.close_separate_window(&tab, window, cx))
                    });
                });
                false
            });
            cx.new(|cx| SeparatePreview::new(&workspace, tab.clone(), shown, look, separate, cx))
        });
        let failure = match (opened, placed) {
            (Ok(handle), Ok(rebuilt)) => {
                let view = match rebuilt {
                    Some((rebuilt, events)) => {
                        self.replace_view(&tab, rebuilt.clone(), events, window, cx);
                        rebuilt
                    }
                    None => view,
                };
                self.preview.separate.insert(tab.clone(), Separate { window: handle, view });
                if let Some(hosted) = self.preview.tabs.get_mut(&tab) {
                    // The separate window owns the view's look and visibility until it closes.
                    hosted.shown = true;
                    let _ = hosted.view.read(cx).set_visible(true);
                }
                #[cfg(windows)]
                if let Some(hwnd) = hwnd {
                    cx.spawn(async move |_, _| keep_on_top(hwnd)).detach();
                }
                None
            }
            (Ok(handle), Err(error)) => {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
                Some(error.to_string())
            }
            (Err(error), _) => Some(format!("{error:#}")),
        };
        if let Some(error) = failure {
            self.error = Some(format!("Unable to update popped-out preview: {error}").into());
        }
        cx.notify();
    }

    /// Moves `tab`'s page back into the main window, `window`, and closes its separate window.
    pub(super) fn close_separate_window(&mut self, tab: &TabRef, window: &mut Window, cx: &mut Context<Self>) {
        let Some(separate) = self.preview.separate.remove(tab) else { return };
        // Hidden until the panel or the mini player lays it out again.
        let _ = separate.view.read(cx).set_visible(false);
        let source = self.page_source(tab, cx);
        match place_page(&separate.view, source.as_ref(), window, cx) {
            Ok(Some((rebuilt, events))) => self.replace_view(tab, rebuilt, events, window, cx),
            Ok(None) => {}
            Err(error) => log::warn!("could not move the page back from its separate window: {error}"),
        }
        if let Some(hosted) = self.preview.tabs.get_mut(tab) {
            hosted.shown = false;
            hosted.zoom = 0.;
            hosted.appearance = None;
        }
        let _ = separate.window.update(cx, |_, window, _| window.remove_window());
        cx.notify();
    }

    /// Closes the separate windows of tabs that closed.
    pub(super) fn prune_separate_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let closed: Vec<TabRef> =
            self.preview.separate.keys().filter(|tab| !self.preview.tabs.contains_key(*tab)).cloned().collect();
        for tab in closed {
            self.close_separate_window(&tab, window, cx);
        }
    }

    /// What the panel (or, `compact`, the mini player) shows of a tab whose page is in its
    /// separate window.
    pub(super) fn render_separated(
        &self,
        tab: &TabRef,
        compact: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let back = tab.clone();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .child(Icon::new(IconName::PictureInPicture2).size(px(20.)).text_color(tokens.muted_foreground))
            .child(
                div()
                    .text_center()
                    .map(|el| if compact { el.text_xs() } else { el.text_sm() })
                    .text_color(tokens.muted_foreground)
                    .child("Open in a separate window"),
            )
            .when(!compact, |el| {
                el.child(
                    Button::new("preview-separate-return").outline().small().label("Return to panel").on_click(
                        cx.listener(move |this, _, window, cx| this.close_separate_window(&back, window, cx)),
                    ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_keeps_its_area_at_the_pages_aspect_ratio() {
        // A 480x320 start at 3:2 is unchanged.
        assert_eq!(fit_content_size(INITIAL_SIZE, 1.5), (480., 320.));
        // A wide page keeps the area: 16:9 is 523x294.
        assert_eq!(fit_content_size(INITIAL_SIZE, 16. / 9.), (523., 294.));
        // A phone held upright: 9:19.5 keeps the area at 266x577.
        assert_eq!(fit_content_size(INITIAL_SIZE, 9. / 19.5), (266., 577.));
        // A sliver grows to the minimum width or height, keeping its ratio.
        assert_eq!(fit_content_size((240., 160.), 10.), (1600., 160.));
        // No usable ratio falls back to the initial one.
        assert_eq!(fit_content_size(INITIAL_SIZE, f32::NAN), (480., 320.));
        assert_eq!(fit_content_size(INITIAL_SIZE, 0.), (480., 320.));
    }

    #[test]
    fn the_title_names_the_page() {
        assert_eq!(window_title("Docs"), SharedString::from("Preview \u{b7} Docs"));
        assert_eq!(window_title("  "), SharedString::from("Browser preview"));
    }
}
