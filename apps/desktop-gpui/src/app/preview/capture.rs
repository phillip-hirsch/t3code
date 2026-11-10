//! The chrome row's camera: a screenshot of this computer's browser tab, or with Shift a
//! recording (`PreviewChromeRow`'s capture button, `handleCapture` in `PreviewView.tsx` and
//! `browserRecording.ts`).
//!
//! Native WebViews have no DevTools protocol and wry has no snapshot API, so both read the tab's
//! pixels off the screen with xcap. Recordings are encoded to AV1 `WebM` on their own thread
//! (`encode.rs`); the page draws the cursor and the key and mouse press overlays itself
//! (`recording.js`, after `RecordingCursor.ts`), since a screen capture has neither. Files land in
//! `<T3 home>/userdata/browser-artifacts`, as Electron's do.

mod encode;
mod webm;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::notification::{Notification, NotificationType};
use gpui_kit::component::{Disableable as _, Sizable as _, WindowExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, ClickEvent, ClipboardItem, Context, Image, ImageFormat, IntoElement as _,
    ParentElement as _, Pixels, Point, SharedString, Styled as _, Task, Window, div, px,
};
use image::RgbaImage;
use serde_json::json;

use super::icon_button;
use crate::app::Workspace;
use crate::theme::Tokens;

use encode::Encoder;

/// Defines `window.__t3Recording`, which a recording starts in its tab after every page load.
pub(super) const RECORDING_SCRIPT: &str = include_str!("capture/recording.js");

/// How long a toast's "Copied!" shows.
const COPIED_FOR: Duration = Duration::from_secs(2);

/// `revealInFileExplorerLabel` for this computer.
const REVEAL_LABEL: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else if cfg!(windows) {
    "Reveal in File Explorer"
} else {
    "Reveal in Files"
};

/// The recording in progress; this client records one tab at a time.
#[derive(Default)]
pub(super) struct Captures {
    recording: Option<Recording>,
    /// Numbers recordings, so a finished one's events can't reach the next.
    started: u64,
}

struct Recording {
    id: u64,
    thread_id: String,
    tab_id: String,
    /// `__t3Recording.start`'s options, sent again after each page load.
    overlay: String,
    /// Frame requests to the recording thread; dropping it stops the recording.
    frames: Option<SyncSender<FrameRequest>>,
    started: Instant,
    _pump: Task<()>,
}

impl Captures {
    fn recording(&self, thread_id: &str, tab_id: &str) -> Option<&Recording> {
        self.recording.as_ref().filter(|recording| recording.thread_id == thread_id && recording.tab_id == tab_id)
    }
}

/// A rectangle on screen in xcap's units: physical pixels on Windows and X11, points on macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ScreenRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

/// The screen rectangle of a view at `view` in a window whose content starts at `origin`, both
/// in GPUI pixels, at `units` screen units per pixel.
fn screen_rect(origin: Point<Pixels>, view: Bounds<Pixels>, units: f32) -> Option<ScreenRect> {
    #[expect(clippy::cast_possible_truncation, reason = "screen coordinates fit in an i32")]
    let to_screen = |value: f32| (value * units).round() as i32;
    let x = to_screen(f32::from(origin.x) + f32::from(view.origin.x));
    let y = to_screen(f32::from(origin.y) + f32::from(view.origin.y));
    let right = to_screen(f32::from(origin.x) + f32::from(view.origin.x) + f32::from(view.size.width));
    let bottom = to_screen(f32::from(origin.y) + f32::from(view.origin.y) + f32::from(view.size.height));
    let (width, height) = (u32::try_from(right - x).ok()?, u32::try_from(bottom - y).ok()?);
    (width > 0 && height > 0).then_some(ScreenRect { x, y, width, height })
}

/// The screen's pixels under `rect`, opaque, from the display under its center and clipped to it.
fn grab(rect: ScreenRect) -> Result<RgbaImage, String> {
    let center_x = rect.x.saturating_add_unsigned(rect.width / 2);
    let center_y = rect.y.saturating_add_unsigned(rect.height / 2);
    let unavailable = |error: xcap::XCapError| format!("Unable to read the screen: {error}");
    let monitor = xcap::Monitor::from_point(center_x, center_y).map_err(unavailable)?;
    let (x, y) = (monitor.x().map_err(unavailable)?, monitor.y().map_err(unavailable)?);
    let (width, height) = (monitor.width().map_err(unavailable)?, monitor.height().map_err(unavailable)?);
    let left = rect.x.max(x);
    let top = rect.y.max(y);
    let right = rect.x.saturating_add_unsigned(rect.width).min(x.saturating_add_unsigned(width));
    let bottom = rect.y.saturating_add_unsigned(rect.height).min(y.saturating_add_unsigned(height));
    if right <= left || bottom <= top {
        return Err("The browser is not on screen.".to_owned());
    }
    let mut image = monitor
        .capture_region(
            (left - x).unsigned_abs(),
            (top - y).unsigned_abs(),
            (right - left).unsigned_abs(),
            (bottom - top).unsigned_abs(),
        )
        .map_err(unavailable)?;
    for pixel in image.pixels_mut() {
        pixel[3] = 255;
    }
    Ok(image)
}

/// `artifactSiteSlug`: the page's host as a file name part.
fn site_slug(url: &str) -> String {
    const MAX: usize = 80;
    let host = url::Url::parse(url).ok().and_then(|url| url.host_str().map(str::to_lowercase)).unwrap_or_default();
    let mut slug = String::new();
    for part in host.split(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit()).filter(|part| !part.is_empty()) {
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(part);
    }
    slug.truncate(MAX);
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() { "site".to_owned() } else { slug.to_owned() }
}

/// `Number.prototype.toString(36)`.
fn base36(mut value: u128) -> String {
    let mut digits = Vec::new();
    loop {
        digits.push(char::from_digit(u32::try_from(value % 36).unwrap_or(0), 36).unwrap_or('0'));
        value /= 36;
        if value == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

fn now_millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis())
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Writes an artifact file; the name is ASCII and the folder is created on first use.
fn save_artifact(name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let folder = crate::settings::userdata_dir()
        .ok_or_else(|| "There is no T3 home folder to save into.".to_owned())?
        .join("browser-artifacts");
    std::fs::create_dir_all(&folder).map_err(|error| format!("Unable to create {}: {error}", folder.display()))?;
    let path = folder.join(name);
    std::fs::write(&path, bytes).map_err(|error| format!("Unable to write {}: {error}", path.display()))?;
    Ok(path)
}

/// A screenshot's file and PNG bytes.
struct Screenshot {
    path: PathBuf,
    png: Arc<[u8]>,
}

/// `captureScreenshot`: `browser-screenshot-<site>-<time>-<id>.png`.
fn save_screenshot(rect: ScreenRect, url: &str) -> Result<Screenshot, String> {
    let image = grab(rect)?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("Unable to encode the screenshot: {error}"))?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("browser-screenshot-{}-{}-{}.png", site_slug(url), base36(now_millis()), &id[..8]);
    let path = save_artifact(&name, &png)?;
    Ok(Screenshot { path, png: png.into() })
}

/// What the recording thread is asked to capture, and when, from the recording's start.
struct FrameRequest {
    rect: ScreenRect,
    at: Duration,
}

enum RecorderEvent {
    /// The first frame was captured and the encoder started, or why not; a failed start ends there.
    Started(Result<(), String>),
    /// The saved file, or nothing when no frame was captured.
    Finished(Result<Option<PathBuf>, String>),
}

/// Starts the recording thread. It captures each requested frame and encodes it; a request that
/// arrives while it is busy is dropped by the sender, so a slow encode lowers the frame rate
/// instead of the timing. It saves the file once the sender is dropped.
fn spawn_recorder(
    frame_rate: u32,
) -> Result<(SyncSender<FrameRequest>, async_channel::Receiver<RecorderEvent>), String> {
    let (frames, requests) = std::sync::mpsc::sync_channel(1);
    let (events, received) = async_channel::unbounded();
    std::thread::Builder::new()
        .name("browser-recording".into())
        .spawn(move || {
            let event = record(frame_rate, &requests, &events);
            let _ = events.try_send(event);
        })
        .map_err(|error| format!("Unable to start the recording thread: {error}"))?;
    Ok((frames, received))
}

/// Records until the requests end; returns the last event.
fn record(
    frame_rate: u32,
    requests: &Receiver<FrameRequest>,
    events: &async_channel::Sender<RecorderEvent>,
) -> RecorderEvent {
    let mut encoder: Option<Encoder> = None;
    let mut last = Duration::ZERO;
    while let Ok(request) = requests.recv() {
        let image = match grab(request.rect) {
            Ok(image) => image,
            Err(error) if encoder.is_none() => return RecorderEvent::Started(Err(error)),
            Err(error) => {
                log::debug!("skipped a recording frame: {error}");
                continue;
            }
        };
        if encoder.is_none() {
            let started = encode::encode_size(image.width(), image.height())
                .ok_or_else(|| "The browser is too small to record.".to_owned())
                .and_then(|(width, height)| Encoder::new(width, height, frame_rate));
            match started {
                Ok(started) => encoder = Some(started),
                Err(error) => return RecorderEvent::Started(Err(error)),
            }
            let _ = events.try_send(RecorderEvent::Started(Ok(())));
        }
        let Some(active) = encoder.as_mut() else { continue };
        last = request.at;
        if let Err(error) = active.push(&image, millis(request.at)) {
            return RecorderEvent::Finished(Err(error));
        }
    }
    let Some(encoder) = encoder.filter(|encoder| !encoder.is_empty()) else { return RecorderEvent::Finished(Ok(None)) };
    let frame = Duration::from_secs(1) / frame_rate.max(1);
    let saved = encoder
        .finish(millis(last + frame))
        .and_then(|webm| save_artifact(&format!("browser-recording-{}.webm", base36(now_millis())), &webm));
    RecorderEvent::Finished(saved.map(Some))
}

/// The theme's primary colour for the page's press overlay (`readPreviewAnnotationTheme`).
fn css_color(color: gpui_kit::Hsla) -> String {
    let rgb = color.to_rgb();
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "channels are 0–1")]
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    format!("rgb({} {} {} / {})", channel(rgb.r), channel(rgb.g), channel(rgb.b), rgb.a.clamp(0., 1.))
}

fn error_toast(window: &mut Window, cx: &mut App, title: &'static str, description: impl Into<SharedString>) {
    window.push_notification(Notification::error(description).title(title), cx);
}

/// Shows "Copied!" on a toast's button for a moment.
fn flash_copied(flag: &Rc<Cell<bool>>, cx: &mut Context<Notification>) {
    flag.set(true);
    cx.notify();
    let flag = flag.clone();
    cx.spawn(async move |toast, cx| {
        cx.background_executor().timer(COPIED_FOR).await;
        flag.set(false);
        let _ = toast.update(cx, |_, cx| cx.notify());
    })
    .detach();
}

/// A toast's xs button.
fn toast_button(id: &'static str, label: &'static str, copied: bool) -> Button {
    Button::new(id).label(if copied { "Copied!" } else { label }).xsmall().disabled(copied)
}

/// "Screenshot saved": Copy path, Reveal and Copy image.
fn screenshot_toast(shot: Screenshot, window: &mut Window, cx: &mut App) {
    let (image_copied, path_copied) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
    let path: Arc<Path> = shot.path.into();
    let png = shot.png;
    let toast =
        Notification::new().with_type(NotificationType::Success).title("Screenshot saved").content(move |_, _, cx| {
            let (path, reveal, png) = (path.clone(), path.clone(), png.clone());
            let (image_flag, path_flag) = (image_copied.clone(), path_copied.clone());
            h_flex()
                .mt_2()
                .gap(px(6.))
                .flex_wrap()
                .child(toast_button("copy-path", "Copy path", path_copied.get()).outline().on_click(cx.listener(
                    move |_, _: &ClickEvent, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                        flash_copied(&path_flag, cx);
                    },
                )))
                .child(Button::new("reveal").label(REVEAL_LABEL).xsmall().outline().on_click(move |_, _, cx| {
                    cx.reveal_path(&reveal);
                }))
                .child(toast_button("copy-image", "Copy image", image_copied.get()).primary().on_click(cx.listener(
                    move |_, _: &ClickEvent, _, cx| {
                        let image = Image::from_bytes(ImageFormat::Png, png.to_vec());
                        cx.write_to_clipboard(ClipboardItem::new_image(&image));
                        flash_copied(&image_flag, cx);
                    },
                )))
                .into_any_element()
        });
    window.push_notification(toast, cx);
}

/// "Recording saved": Copy path and Reveal.
fn recording_toast(path: PathBuf, window: &mut Window, cx: &mut App) {
    let copied = Rc::new(Cell::new(false));
    let path: Arc<Path> = path.into();
    let toast =
        Notification::new().with_type(NotificationType::Success).title("Recording saved").content(move |_, _, cx| {
            let (path, reveal, flag) = (path.clone(), path.clone(), copied.clone());
            h_flex()
                .mt_2()
                .gap(px(6.))
                .flex_wrap()
                .child(toast_button("copy-path", "Copy path", copied.get()).outline().on_click(cx.listener(
                    move |_, _: &ClickEvent, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                        flash_copied(&flag, cx);
                    },
                )))
                .child(Button::new("reveal").label(REVEAL_LABEL).xsmall().primary().on_click(move |_, _, cx| {
                    cx.reveal_path(&reveal);
                }))
                .into_any_element()
        });
    window.push_notification(toast, cx);
}

impl Workspace {
    /// The camera button for a tab this computer draws; `page_ready` is a loaded, reachable page.
    pub(super) fn render_capture_button(
        &self,
        thread_id: &str,
        tab_id: &str,
        page_ready: bool,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let recording = self.preview.capture.recording(thread_id, tab_id).is_some();
        // A recording can always be stopped.
        let enabled = page_ready || recording;
        let (thread, tab) = (thread_id.to_owned(), tab_id.to_owned());
        let button = icon_button("preview-capture", IconName::Camera, enabled, tokens, cx)
            // `variant="secondary"` with a destructive icon while recording.
            .when(recording, |el| {
                el.custom(
                    ButtonCustomVariant::new(cx)
                        .color(tokens.secondary)
                        .foreground(tokens.error)
                        .hover(tokens.secondary)
                        .active(tokens.secondary),
                )
            })
            .tooltip(if recording { "Stop recording" } else { "Screenshot · Shift-click to record" })
            .when(enabled, |el| {
                el.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                    this.capture_preview(&thread, &tab, event.modifiers().shift, window, cx);
                }))
            });
        // The web's dot pulses; a static one keeps the panel from repainting every frame.
        div()
            .relative()
            .flex_none()
            .child(button)
            .when(recording, |el| {
                el.child(div().absolute().top(px(2.)).right(px(2.)).size(px(6.)).rounded_full().bg(tokens.error))
            })
            .into_any_element()
    }

    /// `handleCapture`: stops this tab's recording, else starts one (`record`) or takes a screenshot.
    fn capture_preview(
        &mut self,
        thread_id: &str,
        tab_id: &str,
        record: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.preview.capture.recording(thread_id, tab_id).is_some() {
            self.stop_recording(cx);
        } else if record {
            self.start_recording(thread_id, tab_id, window, cx);
        } else {
            self.take_screenshot(tab_id, window, cx);
        }
    }

    /// The native view of one of this computer's tabs; every lookup here goes through it.
    fn capture_tab(&self, tab_id: &str) -> Option<&super::HostedTab> {
        self.preview.tabs.get(tab_id)
    }

    /// Where a shown tab's page is on screen.
    fn capture_rect(&self, tab_id: &str, window: &Window, cx: &App) -> Option<ScreenRect> {
        let tab = self.capture_tab(tab_id).filter(|tab| tab.shown)?;
        let units = if cfg!(target_os = "macos") { 1. } else { window.scale_factor() };
        screen_rect(window.bounds().origin, tab.view.read(cx).bounds(), units)
    }

    fn take_screenshot(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rect) = self.capture_rect(tab_id, window, cx) else {
            return error_toast(window, cx, "Unable to capture screenshot", "Show the browser tab to capture it.");
        };
        let url = self.capture_tab(tab_id).and_then(|tab| tab.status.url()).unwrap_or_default().to_owned();
        let saved = cx.background_executor().spawn(async move { save_screenshot(rect, &url) });
        cx.spawn_in(window, async move |_, cx| {
            let saved = saved.await;
            let _ = cx.update(|window, cx| match saved {
                Ok(shot) => screenshot_toast(shot, window, cx),
                Err(error) => error_toast(window, cx, "Unable to capture screenshot", error),
            });
        })
        .detach();
    }

    /// `startBrowserRecording` with the frame rate and overlays set for new recordings.
    fn start_recording(&mut self, thread_id: &str, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.preview.capture.recording.is_some() {
            let description = "Another browser tab is being recorded. Stop that recording first.";
            return error_toast(window, cx, "Unable to start recording", description);
        }
        if self.capture_rect(tab_id, window, cx).is_none() {
            return error_toast(window, cx, "Unable to start recording", "Show the browser tab to record it.");
        }
        let settings = &self.client_settings.values;
        let frame_rate = if settings.get("browserRecordingFrameRate").as_u64() == Some(60) { 60 } else { 30 };
        let overlay = json!({
            "keys": settings.bool("browserRecordingShowKeyPresses"),
            "mouse": settings.bool("browserRecordingShowMousePresses"),
            "mac": cfg!(target_os = "macos"),
            "primary": css_color(crate::theme::tokens(cx).primary),
        })
        .to_string();
        let (frames, events) = match spawn_recorder(frame_rate) {
            Ok(started) => started,
            Err(error) => return error_toast(window, cx, "Unable to start recording", error),
        };
        self.preview.capture.started += 1;
        let id = self.preview.capture.started;
        let pump = cx.spawn_in(window, async move |this, cx| {
            let interval = Duration::from_secs(1) / frame_rate;
            loop {
                cx.background_executor().timer(interval).await;
                if !matches!(this.update_in(cx, Self::feed_recording), Ok(true)) {
                    break;
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update_in(cx, |this, window, cx| this.on_recorder_event(id, event, window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        self.preview.capture.recording = Some(Recording {
            id,
            thread_id: thread_id.to_owned(),
            tab_id: tab_id.to_owned(),
            overlay,
            frames: Some(frames),
            started: Instant::now(),
            _pump: pump,
        });
        self.recording_page_loaded(tab_id, cx);
        cx.notify();
    }

    /// Asks for the next frame, unless the recording stopped; a closed tab stops it.
    fn feed_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(recording) = &self.preview.capture.recording else { return false };
        let Some(frames) = recording.frames.clone() else { return false };
        let (thread_id, tab_id, started) = (recording.thread_id.clone(), recording.tab_id.clone(), recording.started);
        if self.capture_tab(&tab_id).is_none_or(|tab| tab.thread_id != thread_id) {
            self.stop_recording(cx);
            return false;
        }
        // A hidden or covered tab skips frames, and so does an inactive window, whose page other
        // windows may cover on screen; the video holds its last frame.
        if window.is_window_active()
            && let Some(rect) = self.capture_rect(&tab_id, window, cx)
            && let Err(TrySendError::Disconnected(_)) = frames.try_send(FrameRequest { rect, at: started.elapsed() })
        {
            return false;
        }
        true
    }

    /// `stopBrowserRecording`: the thread finishes the file and reports it.
    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        let Some(recording) = self.preview.capture.recording.as_mut() else { return };
        if recording.frames.take().is_some() {
            let tab_id = recording.tab_id.clone();
            self.run_recording_overlay(&tab_id, None, cx);
            cx.notify();
        }
    }

    fn on_recorder_event(&mut self, id: u64, event: RecorderEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(recording) = self.preview.capture.recording.as_ref().filter(|recording| recording.id == id) else {
            return;
        };
        let stopping = recording.frames.is_none();
        let finished = match event {
            RecorderEvent::Started(Ok(())) => return,
            RecorderEvent::Started(Err(error)) => Err(("Unable to start recording", error)),
            RecorderEvent::Finished(Ok(path)) => Ok(path),
            RecorderEvent::Finished(Err(error)) => {
                Err((if stopping { "Unable to stop recording" } else { "Recording stopped" }, error))
            }
        };
        if let Some(recording) = self.preview.capture.recording.take()
            && recording.frames.is_some()
        {
            self.run_recording_overlay(&recording.tab_id, None, cx);
        }
        match finished {
            Ok(Some(path)) => recording_toast(path, window, cx),
            Ok(None) => {}
            Err((title, error)) => error_toast(window, cx, title, error),
        }
        cx.notify();
    }

    /// Starts the overlays again in a recorded tab's newly loaded page.
    pub(super) fn recording_page_loaded(&self, tab_id: &str, cx: &App) {
        let Some(recording) = self.preview.capture.recording.as_ref() else { return };
        if recording.tab_id == tab_id && recording.frames.is_some() {
            self.run_recording_overlay(tab_id, Some(&recording.overlay), cx);
        }
    }

    /// Starts the page's overlays with these options, or stops them.
    fn run_recording_overlay(&self, tab_id: &str, options: Option<&str>, cx: &App) {
        let Some(tab) = self.capture_tab(tab_id) else { return };
        let script = match options {
            Some(options) => format!("window.__t3Recording&&window.__t3Recording.start({options})"),
            None => "window.__t3Recording&&window.__t3Recording.stop()".to_owned(),
        };
        if let Err(error) = tab.view.read(cx).raw().evaluate_script(&script) {
            log::warn!("could not update the recording overlay: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, size};

    #[test]
    fn screen_rects_scale_the_window_and_view_offsets() {
        let view = Bounds { origin: point(px(10.), px(40.5)), size: size(px(300.), px(200.)) };
        let rect = screen_rect(point(px(100.), px(50.)), view, 2.).expect("on screen");
        assert_eq!(rect, ScreenRect { x: 220, y: 181, width: 600, height: 400 });
        let at_points = screen_rect(point(px(-1200.), px(0.)), view, 1.).expect("on screen");
        assert_eq!(at_points, ScreenRect { x: -1190, y: 41, width: 300, height: 200 });
        let empty = Bounds { origin: point(px(0.), px(0.)), size: size(px(0.), px(20.)) };
        assert_eq!(screen_rect(point(px(0.), px(0.)), empty, 1.), None);
    }

    #[test]
    fn artifact_names_match_electron() {
        assert_eq!(site_slug("https://Docs.Example.co.uk:8080/a?b"), "docs-example-co-uk");
        assert_eq!(site_slug("http://[::1]:3000/"), "1");
        assert_eq!(site_slug("about:blank"), "site");
        assert_eq!(site_slug("not a url"), "site");
        assert_eq!(site_slug(&format!("https://{}.com/", "a".repeat(79))), "a".repeat(79));
        assert_eq!(base36(0), "0");
        assert_eq!(base36(1_700_000_000_000), "loyw3v28");
    }
}
