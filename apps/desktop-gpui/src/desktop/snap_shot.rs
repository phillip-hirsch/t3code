//! The native half of Electron's `DesktopSnapShot`. Windows and macOS capture directly: the
//! global capture shortcut (`globalShortcut` for chords, a key-state poll for modifier pairs),
//! the foreground window capture (`ActiveWindow.ts`, `RegionSnapShot.ts`, `MacSnapShot.ts`) and
//! the flash over the captured window (`SnapShotFlash`); macOS also reports and requests its
//! Screen Recording and Accessibility permissions. Linux captures through the XDG portals on
//! Wayland (`LinuxSnapShot.ts`, `PortalCaptureShortcut.ts`), and not at all on X11, as Electron.

#[cfg(any(windows, target_os = "macos"))]
use crate::snap_shot::setup::{LinuxBackend, PortalShortcut};
#[cfg(not(target_os = "macos"))]
use crate::snap_shot::setup::{MacPermission, MacPermissions};

/// A registered shortcut; dropping it unregisters.
pub struct Registration {
    #[cfg(any(windows, target_os = "macos"))]
    _stop: hotkeys::Stop,
    #[cfg(not(any(windows, target_os = "macos")))]
    portal: portal::Session,
}

impl Registration {
    /// `retry-shortcut`: asks the desktop to show its shortcut settings for this session. `None`
    /// when there is no session to ask about, so the shortcut registers again instead.
    pub fn configure(&self) -> Option<Result<(), &'static str>> {
        #[cfg(any(windows, target_os = "macos"))]
        return None;
        #[cfg(not(any(windows, target_os = "macos")))]
        return self.portal.configure();
    }
}

/// The foreground window, captured: a PNG, or a JPEG when the PNG is over the image limit.
pub struct Capture {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub app_name: String,
    pub window_title: String,
    pub icon_png: Option<Vec<u8>>,
    /// The window's text, when the capture asked for it and the app exposes it.
    pub accessible_text: Option<String>,
}

impl Capture {
    pub fn extension(&self) -> &'static str {
        if self.mime == "image/png" { "png" } else { "jpg" }
    }
}

/// The captured window's screen pixels, handed over as soon as they are read so the flash can show.
#[cfg_attr(not(any(windows, target_os = "macos")), expect(dead_code, reason = "Linux has no direct capture"))]
pub struct Grab<'a> {
    /// The window's top-left corner on screen, where the flash goes: physical pixels on
    /// Windows, points on macOS.
    pub origin: (i32, i32),
    /// The window's size in the same units.
    pub size: (u32, u32),
    #[cfg_attr(target_os = "macos", expect(dead_code, reason = "macOS lights a see-through window instead"))]
    pub image: &'a image::RgbaImage,
}

#[cfg(any(windows, target_os = "macos"))]
pub use direct::{detect_backend, register_portal};
#[cfg(any(windows, target_os = "macos"))]
pub use hotkeys::{check_chord, register};
pub use media::play_sound;
#[cfg(not(target_os = "macos"))]
pub use no_mac::{mac_permissions, request_mac_permission, test_capture};
#[cfg(any(windows, target_os = "macos"))]
pub use os::{Flash, capture, pair_held, reduced_motion, retire_flash, session, show_flash};
#[cfg(target_os = "macos")]
pub use os::{mac_permissions, request_mac_permission, test_capture};
#[cfg(not(any(windows, target_os = "macos")))]
pub use portal::{
    Flash, capture, check_chord, detect_backend, pair_held, reduced_motion, register, register_portal, retire_flash,
    session, show_flash,
};

/// The portal half on platforms that capture directly, which never take it.
#[cfg(any(windows, target_os = "macos"))]
mod direct {
    use super::{LinuxBackend, PortalShortcut, Registration, hotkeys};
    use crate::keybindings::Shortcut as Chord;

    pub async fn detect_backend() -> Result<LinuxBackend, String> {
        Err("Portal capture is only used on Linux.".into())
    }

    pub fn register_portal(
        _: &Chord,
        _: async_channel::Sender<()>,
        updates: async_channel::Sender<PortalShortcut>,
    ) -> Registration {
        let _ = updates.try_send(PortalShortcut::failed("Could not connect to your desktop's shortcut service."));
        Registration { _stop: hotkeys::Stop(Box::new(|| {})) }
    }
}

/// Permissions only macOS asks for.
#[cfg(not(target_os = "macos"))]
mod no_mac {
    use super::{MacPermission, MacPermissions};

    pub fn mac_permissions() -> Option<MacPermissions> {
        None
    }

    pub fn request_mac_permission(_: MacPermission) {}

    pub fn test_capture() -> Result<(), String> {
        Err("Test capture requires macOS.".into())
    }
}

/// The capture shortcut on platforms that capture directly: `global-hotkey` for chords (Windows'
/// `RegisterHotKey`, macOS's Carbon hot keys) and a key-state poll for modifier pairs.
#[cfg(any(windows, target_os = "macos"))]
mod hotkeys {
    use std::cell::OnceCell;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

    use super::os::pair_held;
    use crate::keybindings::Shortcut as Chord;
    use crate::snap_shot::{self, Modifier, Shortcut};

    /// `GlobalShiftShortcutWorker`'s `POLL_INTERVAL_MS`.
    const PAIR_POLL: Duration = Duration::from_millis(50);

    pub struct Stop(pub(super) Box<dyn FnMut()>);

    impl Drop for Stop {
        fn drop(&mut self) {
            (self.0)();
        }
    }

    /// Registers `shortcut`; each press sends on `trigger`. Call it on the UI thread: its message
    /// loop delivers the hotkey.
    pub fn register(shortcut: &Shortcut, trigger: async_channel::Sender<()>) -> Option<super::Registration> {
        let stop = match shortcut {
            Shortcut::Pair(modifier) => watch_pair(*modifier, trigger),
            Shortcut::Chord(chord) => register_chord(chord, trigger)?,
        };
        Some(super::Registration { _stop: stop })
    }

    /// Where each registered hotkey's presses go, by hotkey id.
    static TRIGGERS: Mutex<Vec<(u32, async_channel::Sender<()>)>> = Mutex::new(Vec::new());

    thread_local! {
        /// The hotkeys' hidden window, on the UI thread.
        static MANAGER: OnceCell<Option<GlobalHotKeyManager>> = const { OnceCell::new() };
    }

    fn with_manager<R>(f: impl FnOnce(&GlobalHotKeyManager) -> R) -> Option<R> {
        MANAGER.with(|manager| {
            manager
                .get_or_init(|| {
                    GlobalHotKeyEvent::set_event_handler(Some(|event: GlobalHotKeyEvent| {
                        if event.state() != HotKeyState::Pressed {
                            return;
                        }
                        let triggers = TRIGGERS.lock().expect("snapshot triggers lock poisoned");
                        if let Some((_, trigger)) = triggers.iter().find(|(id, _)| *id == event.id()) {
                            let _ = trigger.try_send(());
                        }
                    }));
                    GlobalHotKeyManager::new()
                        .inspect_err(|error| log::warn!("could not set up global shortcuts: {error}"))
                        .ok()
                })
                .as_ref()
                .map(f)
        })
    }

    fn register_chord(chord: &Chord, trigger: async_channel::Sender<()>) -> Option<Stop> {
        let hotkey = hotkey(chord)?;
        with_manager(|manager| manager.register(hotkey)).and_then(Result::ok)?;
        TRIGGERS.lock().expect("snapshot triggers lock poisoned").push((hotkey.id(), trigger));
        Some(Stop(Box::new(move || {
            TRIGGERS.lock().expect("snapshot triggers lock poisoned").retain(|(id, _)| *id != hotkey.id());
            with_manager(|manager| manager.unregister(hotkey));
        })))
    }

    /// `probeGlobalShortcut`: registers and releases the chord.
    pub fn check_chord(chord: &Chord) -> bool {
        let Some(hotkey) = hotkey(chord) else { return false };
        with_manager(|manager| manager.register(hotkey).is_ok() && manager.unregister(hotkey).is_ok()).unwrap_or(false)
    }

    fn hotkey(chord: &Chord) -> Option<HotKey> {
        let (flags, vk) = snap_shot::hotkey(chord)?;
        let modifiers =
            [(0x1, Modifiers::ALT), (0x2, Modifiers::CONTROL), (0x4, Modifiers::SHIFT), (0x8, Modifiers::META)]
                .into_iter()
                .filter(|(flag, _)| flags & flag != 0)
                .fold(Modifiers::empty(), |all, (_, modifier)| all | modifier);
        Some(HotKey::new(Some(modifiers), code(vk)?))
    }

    /// The key for one of [`snap_shot::hotkey`]'s virtual keys.
    fn code(vk: u32) -> Option<Code> {
        use Code::*;
        const LETTERS: [Code; 26] = [
            KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR,
            KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
        ];
        const DIGITS: [Code; 10] = [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9];
        const FUNCTION: [Code; 24] = [
            F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23,
            F24,
        ];
        let index = |first: u32| (vk - first) as usize;
        Some(match vk {
            0x41..=0x5a => LETTERS[index(0x41)],
            0x30..=0x39 => DIGITS[index(0x30)],
            0x70..=0x87 => FUNCTION[index(0x70)],
            0x20 => Space,
            0x0d => Enter,
            0x09 => Tab,
            0x1b => Escape,
            0x08 => Backspace,
            0x2e => Delete,
            0x2d => Insert,
            0x24 => Home,
            0x23 => End,
            0x21 => PageUp,
            0x22 => PageDown,
            0x25 => ArrowLeft,
            0x26 => ArrowUp,
            0x27 => ArrowRight,
            0x28 => ArrowDown,
            0x13 => Pause,
            0x2c => PrintScreen,
            0xba => Semicolon,
            0xbb => Equal,
            0xbc => Comma,
            0xbd => Minus,
            0xbe => Period,
            0xbf => Slash,
            0xc0 => Backquote,
            0xdb => BracketLeft,
            0xdc => Backslash,
            0xdd => BracketRight,
            0xde => Quote,
            _ => return None,
        })
    }

    /// Polls both keys of `modifier`, sending once each time they go down together.
    fn watch_pair(modifier: Modifier, trigger: async_channel::Sender<()>) -> Stop {
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        let spawned = thread::Builder::new().name("snap-shot-pair".into()).spawn(move || {
            let mut active = false;
            while !flag.load(Ordering::Relaxed) {
                let pressed = pair_held(modifier);
                if pressed && !active {
                    let _ = trigger.try_send(());
                }
                active = pressed;
                thread::sleep(PAIR_POLL);
            }
        });
        if spawned.is_err() {
            log::warn!("could not start the snapshot shortcut watcher");
        }
        Stop(Box::new(move || stopped.store(true, Ordering::Relaxed)))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn every_bindable_key_has_a_hotkey_code() {
            let keys = [
                " ",
                "enter",
                "tab",
                "escape",
                "backspace",
                "delete",
                "insert",
                "home",
                "end",
                "pageup",
                "pagedown",
                "arrowleft",
                "arrowup",
                "arrowright",
                "arrowdown",
                "pause",
                "printscreen",
                ";",
                "=",
                ",",
                "-",
                ".",
                "/",
                "`",
                "[",
                "\\",
                "]",
                "'",
                "a",
                "z",
                "0",
                "9",
                "f1",
                "f24",
            ];
            for key in keys {
                let chord = Chord { key: key.into(), ctrl_key: true, ..Default::default() };
                let (_, vk) = snap_shot::hotkey(&chord).unwrap_or_else(|| panic!("{key:?} binds"));
                assert!(code(vk).is_some(), "{key:?} (vk {vk:#x}) has a code");
            }
            assert_eq!(code(0x4b), Some(Code::KeyK));
            assert_eq!(code(0x32), Some(Code::Digit2));
            assert_eq!(code(0x7b), Some(Code::F12));
        }
    }
}

/// Encoding, sound and app icons.
mod media {
    use std::io::Cursor;
    use std::thread;

    use image::{ImageFormat, RgbaImage, imageops::FilterType};

    use crate::snap_shot;

    pub fn play_sound(bytes: &'static [u8]) {
        thread::spawn(move || {
            let Ok(source) = rodio::Decoder::try_from(Cursor::new(bytes)) else { return };
            match rodio::DeviceSinkBuilder::open_default_sink() {
                Ok(mut sink) => {
                    sink.log_on_drop(false);
                    let player = rodio::Player::connect_new(sink.mixer());
                    player.append(source);
                    player.sleep_until_end();
                }
                Err(error) => log::warn!("could not play the snapshot sound: {error}"),
            }
        });
    }

    /// Scales `image` into the thumbnail bounds and encodes it within the attachment limit.
    pub fn fit_and_encode(mut image: RgbaImage) -> Result<(Vec<u8>, &'static str), &'static str> {
        let (width, height) = image.dimensions();
        let (fit_width, fit_height) = snap_shot::fit_capture(width, height);
        if (fit_width, fit_height) != (width, height) {
            image = image::imageops::resize(&image, fit_width, fit_height, FilterType::Triangle);
        }
        let (bytes, mime) = encode(&image).ok_or(snap_shot::CAPTURE_FAILED)?;
        if bytes.len() > snap_shot::MAX_IMAGE_BYTES {
            return Err(snap_shot::TOO_LARGE);
        }
        Ok((bytes, mime))
    }

    /// `compressImageToByteLimit`: PNG when it fits, else a JPEG.
    fn encode(image: &RgbaImage) -> Option<(Vec<u8>, &'static str)> {
        let png = encode_png(image)?;
        if png.len() <= snap_shot::MAX_IMAGE_BYTES {
            return Some((png, "image/png"));
        }
        let mut jpeg = Vec::new();
        let rgb = image::DynamicImage::ImageRgba8(image.clone()).to_rgb8();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85).encode_image(&rgb).ok()?;
        Some((jpeg, "image/jpeg"))
    }

    fn encode_png(image: &RgbaImage) -> Option<Vec<u8>> {
        let mut png = Vec::new();
        image.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).ok()?;
        Some(png)
    }

    /// `app.getFileIcon(path, { size: "normal" })` scaled to 64px, as PNG.
    #[cfg(any(windows, target_os = "macos"))]
    pub fn file_icon(path: &str) -> Option<Vec<u8>> {
        let icon = file_icon_provider::get_file_icon(path, 32).ok()?;
        let image = RgbaImage::from_raw(icon.width, icon.height, icon.pixels)?;
        encode_png(&image::imageops::resize(&image, 64, 64, FilterType::Lanczos3))
    }
}

/// Windows: the foreground window by HWND, its pixels from the screen under it (`RegionSnapShot`),
/// its UI Automation text, and the flash as an opaque popup over it.
#[cfg(windows)]
mod os {
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use gpui_kit::{
        Animation, AnimationExt as _, AnyWindowHandle, App, AppContext as _, Bounds, Context, IntoElement,
        ParentElement as _, Render, RenderImage, Styled as _, Task, Window, WindowBounds, WindowKind, WindowOptions,
        div, img, point, px, size, white,
    };
    use image::RgbaImage;
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    use uiautomation::types::{Handle, TreeScope, UIProperty};
    use winsafe::{self as w, co};

    use super::{Capture, Grab, media};
    use crate::snap_shot::setup::State;
    use crate::snap_shot::{self, Modifier};

    /// `FLASH_PEAK_OPACITY`, `FLASH_ANIMATION_DURATION_MS`, `FLASH_STATIC_DURATION_MS`.
    const FLASH_PEAK_OPACITY: f32 = 0.08;
    const FLASH_ANIMATED: Duration = Duration::from_millis(180);
    const FLASH_STATIC: Duration = Duration::from_millis(60);

    /// `ACCESSIBILITY_TIMEOUT_MS`.
    const ACCESSIBILITY_TIMEOUT: Duration = Duration::from_secs(3);

    /// Windows captures directly and needs no setup.
    pub fn session() -> State {
        State { windows: true, ..State::default() }
    }

    /// `WINDOWS_MODIFIER_PAIR_VIRTUAL_KEYS`: the left and the right key are both down.
    pub fn pair_held(modifier: Modifier) -> bool {
        let (left, right) = match modifier {
            Modifier::Shift => (co::VK::LSHIFT, co::VK::RSHIFT),
            Modifier::Control => (co::VK::LCONTROL, co::VK::RCONTROL),
            Modifier::Alt => (co::VK::LMENU, co::VK::RMENU),
            Modifier::Meta => (co::VK::LWIN, co::VK::RWIN),
        };
        w::GetAsyncKeyState(left) && w::GetAsyncKeyState(right)
    }

    /// Windows' "Animation effects" setting, which Electron reads as `prefersReducedMotion`.
    pub fn reduced_motion() -> bool {
        windows::UI::ViewManagement::UISettings::new()
            .and_then(|settings| settings.AnimationsEnabled())
            .is_ok_and(|enabled| !enabled)
    }

    /// Captures the foreground window as it shows on screen, handing its pixels to `grabbed`
    /// first, and reads its text too with `text`. Blocks; run it off the UI thread.
    pub fn capture(text: bool, grabbed: impl FnOnce(Grab)) -> Result<Capture, &'static str> {
        let hwnd = w::HWND::GetForegroundWindow().ok_or(snap_shot::WINDOW_UNAVAILABLE)?;
        let rect = window_rect(&hwnd).ok_or(snap_shot::WINDOW_UNAVAILABLE)?;
        let image = grab(rect).ok_or(snap_shot::CAPTURE_FAILED)?;
        grabbed(Grab { origin: (rect.left, rect.top), size: image.dimensions(), image: &image });
        let text = text.then(|| read_text(hwnd.ptr() as isize));
        let (bytes, mime) = media::fit_and_encode(image)?;
        let path = process_image_path(&hwnd);
        Ok(Capture {
            bytes,
            mime,
            app_name: snap_shot::app_name(&path),
            window_title: hwnd.GetWindowText().unwrap_or_default(),
            icon_png: (!path.is_empty()).then(|| media::file_icon(&path)).flatten(),
            accessible_text: text.and_then(|read| read.recv_timeout(ACCESSIBILITY_TIMEOUT).ok().flatten()),
        })
    }

    /// The window's visible frame: DWM's bounds leave out the invisible resize border.
    fn window_rect(hwnd: &w::HWND) -> Option<w::RECT> {
        let rect = match hwnd.DwmGetWindowAttribute(co::DWMWA::EXTENDED_FRAME_BOUNDS) {
            Ok(w::DwmAttr::ExtendedFrameBounds(rect)) => rect,
            _ => hwnd.GetWindowRect().ok()?,
        };
        (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
    }

    /// The screen's pixels under `rect`, opaque, stitched from each monitor it covers.
    fn grab(rect: w::RECT) -> Option<RgbaImage> {
        let (width, height) =
            (u32::try_from(rect.right - rect.left).ok()?, u32::try_from(rect.bottom - rect.top).ok()?);
        let mut image = RgbaImage::new(width, height);
        let mut covered = false;
        for monitor in xcap::Monitor::all().ok()? {
            let (Ok(x), Ok(y), Ok(monitor_width), Ok(monitor_height)) =
                (monitor.x(), monitor.y(), monitor.width(), monitor.height())
            else {
                continue;
            };
            let left = rect.left.max(x);
            let top = rect.top.max(y);
            let right = rect.right.min(x.saturating_add_unsigned(monitor_width));
            let bottom = rect.bottom.min(y.saturating_add_unsigned(monitor_height));
            if right <= left || bottom <= top {
                continue;
            }
            let part = monitor
                .capture_region(
                    (left - x).unsigned_abs(),
                    (top - y).unsigned_abs(),
                    (right - left).unsigned_abs(),
                    (bottom - top).unsigned_abs(),
                )
                .ok()?;
            image::imageops::replace(&mut image, &part, i64::from(left - rect.left), i64::from(top - rect.top));
            covered = true;
        }
        for pixel in image.pixels_mut() {
            pixel[3] = 255;
        }
        covered.then_some(image)
    }

    /// Reads the window's UI Automation text on its own thread; a read past the timeout is dropped.
    fn read_text(hwnd: isize) -> std::sync::mpsc::Receiver<Option<String>> {
        let (done, read) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let _ = done.send(uia_text(hwnd));
        });
        read
    }

    /// The names and values of the window's elements in tree order (`accessibleWindowText`).
    fn uia_text(hwnd: isize) -> Option<String> {
        // Bound first so COM is released after the automation objects.
        let _com = w::CoInitializeEx(co::COINIT::MULTITHREADED).ok();
        let automation = uiautomation::UIAutomation::new_direct().ok()?;
        let window = automation.element_from_handle(Handle::from(hwnd)).ok()?;
        let request = automation.create_cache_request().ok()?;
        request.add_property(UIProperty::Name).ok()?;
        request.add_property(UIProperty::ValueValue).ok()?;
        let all = automation.create_true_condition().ok()?;
        let elements = window.find_all_build_cache(TreeScope::Subtree, &all, &request).ok()?;
        let candidates = elements.iter().take(snap_shot::ACCESSIBILITY_MAX_NODES).flat_map(|element| {
            let name = element.get_cached_name().ok();
            let value = element
                .get_cached_property_value(UIProperty::ValueValue)
                .ok()
                .and_then(|value| value.get_string().ok());
            [name, value]
        });
        Some(snap_shot::accessible_text(candidates.flatten(), snap_shot::ACCESSIBLE_TEXT_MAX))
            .filter(|text| !text.is_empty())
    }

    fn process_image_path(hwnd: &w::HWND) -> String {
        let (_, pid) = hwnd.GetWindowThreadProcessId();
        w::HPROCESS::OpenProcess(co::PROCESS::QUERY_LIMITED_INFORMATION, false, pid)
            .and_then(|process| process.QueryFullProcessImageName(co::PROCESS_NAME::WIN32))
            .unwrap_or_default()
    }

    /// The captured pixels for the flash, as GPUI's BGRA texture.
    pub struct Flash {
        origin: (i32, i32),
        size: (u32, u32),
        image: Arc<RenderImage>,
    }

    impl Flash {
        pub fn new(grab: &Grab) -> Self {
            let mut bgra = grab.image.clone();
            for pixel in bgra.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            Self { origin: grab.origin, size: grab.size, image: Arc::new(RenderImage::new([image::Frame::new(bgra)])) }
        }
    }

    thread_local! {
        static FLASH: RefCell<Option<AnyWindowHandle>> = const { RefCell::new(None) };
    }

    /// Closes the flash still showing, so the next capture doesn't see it.
    pub fn retire_flash(cx: &mut App) {
        if let Some(window) = FLASH.with_borrow_mut(Option::take) {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// `SnapShotFlash`: the window lit white, fading out when `animated`. With DirectComposition off
    /// (for WebView2) GPUI can't draw a see-through window, so an opaque, never-focused popup shows
    /// the captured pixels under the white. The task ends when the flash has played.
    pub fn show_flash(flash: Flash, animated: bool, cx: &mut App) -> Task<()> {
        retire_flash(cx);
        let options = WindowOptions {
            titlebar: None,
            kind: WindowKind::PopUp,
            focus: false,
            show: false,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.), px(0.)),
                size(px(flash.size.0 as f32), px(flash.size.1 as f32)),
            ))),
            ..Default::default()
        };
        let (origin, extent) = (flash.origin, flash.size);
        let mut hwnd = None;
        let opened = cx.open_window(options, |window, cx| {
            hwnd = match window.window_handle().map(|handle| handle.as_raw()) {
                Ok(RawWindowHandle::Win32(handle)) => Some(handle.hwnd.get()),
                _ => None,
            };
            cx.new(|_| FlashView { image: flash.image, animated })
        });
        let (Ok(window), Some(hwnd)) = (opened, hwnd) else { return Task::ready(()) };
        let window: AnyWindowHandle = window.into();
        FLASH.set(Some(window));
        cx.spawn(async move |cx| {
            // Placed in physical pixels outside any GPUI update: moving a window re-enters GPUI.
            let placed =
                super::super::top_level_window(hwnd).ok_or(co::ERROR::INVALID_WINDOW_HANDLE).and_then(|popup| {
                    popup.SetWindowPos(
                        w::HwndPlace::Place(co::HWND_PLACE::TOPMOST),
                        w::POINT::with(origin.0, origin.1),
                        w::SIZE::with(
                            i32::try_from(extent.0).unwrap_or(i32::MAX),
                            i32::try_from(extent.1).unwrap_or(i32::MAX),
                        ),
                        co::SWP::NOACTIVATE | co::SWP::SHOWWINDOW,
                    )
                });
            if let Err(error) = placed {
                log::warn!("could not show the snapshot flash: {error}");
            }
            cx.background_executor().timer(if animated { FLASH_ANIMATED } else { FLASH_STATIC }).await;
            cx.update(|cx| {
                if FLASH.with_borrow(|current| *current == Some(window)) {
                    retire_flash(cx);
                }
            });
        })
    }

    struct FlashView {
        image: Arc<RenderImage>,
        animated: bool,
    }

    impl Render for FlashView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let light = div().absolute().inset_0().bg(white()).opacity(FLASH_PEAK_OPACITY);
            let light = if self.animated {
                light
                    .with_animation("snap-shot-flash", Animation::new(FLASH_ANIMATED), |light, delta| {
                        light.opacity(FLASH_PEAK_OPACITY * (1. - delta))
                    })
                    .into_any_element()
            } else {
                light.into_any_element()
            };
            div().relative().size_full().child(img(self.image.clone()).size_full()).child(light)
        }
    }
}

/// The frontmost window as `ActiveWindow.ts` reads it on macOS: WindowServer's on-screen,
/// layer-0 windows of the frontmost app, skipping untitled helper strips along another window.
#[cfg(any(target_os = "macos", test))]
mod mac_window {
    use serde::Deserialize;

    /// `MAC_HELPER_STRIP_MAX_THICKNESS_RATIO`.
    const HELPER_STRIP_RATIO: f64 = 0.25;

    #[derive(Debug, Deserialize)]
    struct Lookup {
        owner: Owner,
        windows: Vec<Window>,
    }

    #[derive(Clone, Debug, Deserialize, PartialEq)]
    pub struct Owner {
        pub name: String,
        pub path: String,
    }

    #[derive(Clone, Debug, Deserialize, PartialEq)]
    pub struct Window {
        pub id: u32,
        pub title: String,
        pub bounds: Rect,
        alpha: f64,
    }

    #[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
    pub struct Rect {
        pub x: f64,
        pub y: f64,
        pub width: f64,
        pub height: f64,
    }

    /// `macActiveWindow`: the lookup script's output, or `None` without a frontmost window.
    pub fn parse(output: &str) -> Option<(Owner, Window)> {
        let output = output.trim();
        if output.is_empty() {
            return None;
        }
        let lookup: Lookup = serde_json::from_str(output).ok()?;
        let visible: Vec<&Window> = lookup
            .windows
            .iter()
            .filter(|window| window.alpha > 0. && window.bounds.width > 0. && window.bounds.height > 0.)
            .collect();
        let window = visible
            .iter()
            .enumerate()
            .find(|(index, candidate)| {
                !candidate.title.trim().is_empty()
                    || !visible[index + 1..]
                        .iter()
                        .any(|reference| is_helper_strip(&candidate.bounds, &reference.bounds))
            })
            .map(|(_, window)| (*window).clone())?;
        Some((lookup.owner, window))
    }

    /// A thin strip sharing an edge with `reference` and lying along its top or left side.
    #[expect(clippy::float_cmp, reason = "window bounds are whole pixels, so shared edges compare exactly")]
    fn is_helper_strip(strip: &Rect, reference: &Rect) -> bool {
        let r = HELPER_STRIP_RATIO;
        (strip.x == reference.x
            && strip.width == reference.width
            && strip.y <= reference.y
            && reference.y - strip.y <= reference.height * r
            && strip.height <= strip.width * r
            && strip.height <= reference.height * r)
            || (strip.y == reference.y
                && strip.height == reference.height
                && strip.x <= reference.x
                && reference.x - strip.x <= reference.width * r
                && strip.width <= strip.height * r
                && strip.width <= reference.width * r)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn window(id: u32, title: &str, bounds: (f64, f64, f64, f64)) -> serde_json::Value {
            let (x, y, width, height) = bounds;
            serde_json::json!({ "id": id, "title": title, "bounds": { "x": x, "y": y, "width": width, "height": height }, "alpha": 1 })
        }

        fn lookup(windows: Vec<serde_json::Value>) -> String {
            serde_json::json!({
                "owner": { "name": "Safari", "processId": 7, "path": "/Applications/Safari.app", "bundleId": "com.apple.Safari" },
                "windows": windows,
            })
            .to_string()
        }

        #[test]
        fn picks_the_front_window_past_untitled_helper_strips() {
            let strip = window(1, "", (100., 80., 800., 30.));
            let main = window(2, "", (100., 100., 800., 600.));
            let (owner, picked) = parse(&lookup(vec![strip, main])).unwrap();
            assert_eq!((owner.name.as_str(), owner.path.as_str()), ("Safari", "/Applications/Safari.app"));
            assert_eq!(picked.id, 2, "the strip along the top belongs to the window behind it");

            let titled = window(3, "Toolbar", (100., 80., 800., 30.));
            assert_eq!(parse(&lookup(vec![titled, window(2, "", (100., 100., 800., 600.))])).unwrap().1.id, 3);

            let hidden = serde_json::json!({ "id": 4, "title": "x", "bounds": { "x": 0, "y": 0, "width": 10, "height": 10 }, "alpha": 0 });
            assert_eq!(parse(&lookup(vec![hidden, window(5, "", (0., 0., 10., 10.))])).unwrap().1.id, 5);
            assert_eq!(parse(&lookup(vec![])), None);
            assert_eq!(parse(""), None);
            assert_eq!(parse("not json"), None);
        }
    }
}

/// macOS, as Electron's `MacSnapShot`: the frontmost window from WindowServer through
/// `osascript` (`ActiveWindow.ts`), its pixels from `screencapture -l` (needs Screen Recording
/// permission), and a white flash over it in a see-through window. Window text needs the
/// Accessibility API, which this client does not read on macOS.
#[cfg(target_os = "macos")]
mod os {
    use std::cell::RefCell;
    use std::io::Read as _;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    use gpui_kit::{
        Animation, AnimationExt as _, AnyWindowHandle, App, AppContext as _, Bounds, Context, IntoElement, Render,
        Styled as _, Task, Window, WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions, div, point, px,
        size, white,
    };
    use objc2_app_kit::NSWorkspace;
    use objc2_core_graphics::{CGEventSource, CGEventSourceStateID};

    use super::{Capture, Grab, mac_window, media};
    use crate::snap_shot::setup::{MacPermission, MacPermissions, State};
    use crate::snap_shot::{self, Modifier};

    /// `FLASH_PEAK_OPACITY`, `FLASH_ANIMATION_DURATION_MS`, `FLASH_STATIC_DURATION_MS`.
    const FLASH_PEAK_OPACITY: f32 = 0.08;
    const FLASH_ANIMATED: Duration = Duration::from_millis(180);
    const FLASH_STATIC: Duration = Duration::from_millis(60);
    /// `MAC_LOOKUP_TIMEOUT_MS` and `MAC_SCREEN_CAPTURE_TIMEOUT_MS`.
    const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
    const CAPTURE_TIMEOUT: Duration = Duration::from_secs(15);

    /// `MAC_LOOKUP_SCRIPT`.
    const LOOKUP_SCRIPT: &str = r#"
ObjC.import("CoreGraphics");
ObjC.import("AppKit");
function run() {
  const app = $.NSWorkspace.sharedWorkspace.frontmostApplication;
  if (app.isNil()) return "";
  const pid = app.processIdentifier;
  let ownerName = String(app.localizedName.js || "");
  const list = $.CGWindowListCopyWindowInfo(
    $.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements,
    $.kCGNullWindowID,
  );
  $.CFMakeCollectable(list);
  const count = $.CFArrayGetCount(list);
  const windows = [];
  for (let i = 0; i < count; i++) {
    const w = ObjC.castRefToObject($.CFArrayGetValueAtIndex(list, i));
    if (w.objectForKey("kCGWindowOwnerPID").js !== pid) continue;
    if (w.objectForKey("kCGWindowLayer").js !== 0) continue;
    if (!ownerName) {
      ownerName = String(ObjC.unwrap(w.objectForKey("kCGWindowOwnerName")) || "");
    }
    const b = ObjC.deepUnwrap(w.objectForKey("kCGWindowBounds"));
    windows.push({
      id: w.objectForKey("kCGWindowNumber").js,
      title: String(ObjC.unwrap(w.objectForKey("kCGWindowName")) || ""),
      bounds: { x: b.X, y: b.Y, width: b.Width, height: b.Height },
      alpha: w.objectForKey("kCGWindowAlpha").js,
    });
  }
  return JSON.stringify({
    owner: {
      name: ownerName,
      processId: pid,
      path: String(app.bundleURL.path.js || ""),
      bundleId: String(app.bundleIdentifier.js || ""),
    },
    windows,
  });
}"#;

    /// `MAC_PERMISSION_SETTINGS_URLS`.
    const SCREEN_RECORDING_SETTINGS: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
    const ACCESSIBILITY_SETTINGS: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

    pub fn session() -> State {
        State { mac_permissions: mac_permissions(), ..State::default() }
    }

    /// `currentMacPermissions`.
    pub fn mac_permissions() -> Option<MacPermissions> {
        Some(MacPermissions {
            screen_recording: objc2_core_graphics::CGPreflightScreenCaptureAccess(),
            accessibility: macos_accessibility_client::accessibility::application_is_trusted(),
        })
    }

    /// `allow-screen-recording` and `allow-accessibility`: macOS asks once; after that the
    /// permission is granted in System Settings, which opens at its pane. Blocks.
    pub fn request_mac_permission(permission: MacPermission) {
        let (granted, pane) = match permission {
            MacPermission::ScreenRecording => (
                objc2_core_graphics::CGPreflightScreenCaptureAccess()
                    || objc2_core_graphics::CGRequestScreenCaptureAccess(),
                SCREEN_RECORDING_SETTINGS,
            ),
            MacPermission::Accessibility => (
                macos_accessibility_client::accessibility::application_is_trusted_with_prompt(),
                ACCESSIBILITY_SETTINGS,
            ),
        };
        if !granted && Command::new("/usr/bin/open").arg(pane).stdin(Stdio::null()).status().is_err() {
            log::warn!("could not open System Settings");
        }
    }

    /// `test-mac-capture`: a snapshot of the front window, thrown away, so macOS asks for any
    /// consent it still needs while setup is open. Blocks.
    pub fn test_capture() -> Result<(), String> {
        const SUMMARY: &str = "Could not capture a test snapshot.";
        match capture(false, |_| {}) {
            Ok(_) => Ok(()),
            Err(snap_shot::WINDOW_UNAVAILABLE) => Err(format!("{SUMMARY} No window is available to capture.")),
            Err(_) => Err(format!(
                "{SUMMARY} Capturing the window failed. macOS grants Screen Recording and Accessibility to each app separately. In System Settings, open Privacy & Security and check that \"T3 Code\" is allowed for both."
            )),
        }
    }

    /// `MAC_MODIFIER_PAIR_DEVICE_MASKS`: the left and the right key's device bits are both set.
    pub fn pair_held(modifier: Modifier) -> bool {
        let (left, right) = match modifier {
            Modifier::Shift => (0x2, 0x4),
            Modifier::Control => (0x1, 0x2000),
            Modifier::Alt => (0x20, 0x40),
            Modifier::Meta => (0x8, 0x10),
        };
        let both = left | right;
        CGEventSource::flags_state(CGEventSourceStateID::CombinedSessionState).0 & both == both
    }

    /// The "Reduce motion" accessibility setting, which Electron reads as `prefersReducedMotion`.
    pub fn reduced_motion() -> bool {
        NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
    }

    /// Runs `command` to completion within `timeout`, returning its stdout when it succeeds.
    fn output_within(command: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
        let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        let mut stdout = child.stdout.take()?;
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait().ok()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let bytes = reader.join().ok()?.ok()?;
        status.success().then_some(bytes)
    }

    /// Captures the frontmost window, handing its pixels to `grabbed` first. Blocks; run it off
    /// the UI thread.
    pub fn capture(text: bool, grabbed: impl FnOnce(Grab)) -> Result<Capture, &'static str> {
        // Reading another app's window text needs the Accessibility API.
        let _ = text;
        let lookup = output_within(
            Command::new("/usr/bin/osascript").args(["-l", "JavaScript", "-e", LOOKUP_SCRIPT]),
            LOOKUP_TIMEOUT,
        )
        .ok_or(snap_shot::WINDOW_UNAVAILABLE)?;
        let (owner, window) =
            mac_window::parse(&String::from_utf8_lossy(&lookup)).ok_or(snap_shot::WINDOW_UNAVAILABLE)?;
        let image = screencapture(window.id)?;
        let bounds = window.bounds;
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "rounded window bounds in points")]
        let (origin, size) = (
            (bounds.x.round() as i32, bounds.y.round() as i32),
            (bounds.width.round() as u32, bounds.height.round() as u32),
        );
        grabbed(Grab { origin, size, image: &image });
        let (bytes, mime) = media::fit_and_encode(image)?;
        Ok(Capture {
            bytes,
            mime,
            // `snapShotAppName` off Windows: the app's name.
            app_name: snap_shot::bounded(&owner.name, 255).unwrap_or_else(|| "Window".into()),
            window_title: window.title,
            icon_png: (!owner.path.is_empty()).then(|| media::file_icon(&owner.path)).flatten(),
            accessible_text: None,
        })
    }

    /// `runMacSnapShot`: the window's own pixels, without its shadow, silently.
    fn screencapture(window_id: u32) -> Result<image::RgbaImage, &'static str> {
        let path = std::env::temp_dir().join(format!("t3code-snapshot-{}-{window_id}.png", std::process::id()));
        let captured = output_within(
            Command::new("/usr/sbin/screencapture")
                .args(["-l", &window_id.to_string(), "-o", "-x", "-t", "png"])
                .arg(&path),
            CAPTURE_TIMEOUT,
        );
        let png = captured.and_then(|_| std::fs::read(&path).ok());
        remove(&path);
        let png = png.ok_or(snap_shot::CAPTURE_FAILED)?;
        image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .map(|image| image.to_rgba8())
            .map_err(|_| snap_shot::CAPTURE_FAILED)
    }

    fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    /// Where the flash goes, in points.
    pub struct Flash {
        origin: (i32, i32),
        size: (u32, u32),
    }

    impl Flash {
        pub fn new(grab: &Grab) -> Self {
            Self { origin: grab.origin, size: grab.size }
        }
    }

    thread_local! {
        static FLASH: RefCell<Option<AnyWindowHandle>> = const { RefCell::new(None) };
    }

    /// Closes the flash still showing, so the next capture doesn't see it.
    pub fn retire_flash(cx: &mut App) {
        if let Some(window) = FLASH.with_borrow_mut(Option::take) {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
    }

    /// `SnapShotFlash`: the window lit white through a see-through, never-focused popup,
    /// fading out when `animated`. The task ends when the flash has played.
    pub fn show_flash(flash: Flash, animated: bool, cx: &mut App) -> Task<()> {
        retire_flash(cx);
        let options = WindowOptions {
            titlebar: None,
            kind: WindowKind::PopUp,
            focus: false,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(flash.origin.0 as f32), px(flash.origin.1 as f32)),
                size(px(flash.size.0 as f32), px(flash.size.1 as f32)),
            ))),
            ..Default::default()
        };
        let Ok(window) = cx.open_window(options, |_, cx| cx.new(|_| FlashView { animated })) else {
            return Task::ready(());
        };
        let window: AnyWindowHandle = window.into();
        FLASH.set(Some(window));
        cx.spawn(async move |cx| {
            cx.background_executor().timer(if animated { FLASH_ANIMATED } else { FLASH_STATIC }).await;
            cx.update(|cx| {
                if FLASH.with_borrow(|current| *current == Some(window)) {
                    retire_flash(cx);
                }
            });
        })
    }

    struct FlashView {
        animated: bool,
    }

    impl Render for FlashView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let light = div().size_full().bg(white()).opacity(FLASH_PEAK_OPACITY);
            if self.animated {
                light
                    .with_animation("snap-shot-flash", Animation::new(FLASH_ANIMATED), |light, delta| {
                        light.opacity(FLASH_PEAK_OPACITY * (1. - delta))
                    })
                    .into_any_element()
            } else {
                light.into_any_element()
            }
        }
    }
}

/// Linux, as Electron's `LinuxSnapShot` and `PortalCaptureShortcut` for desktops without a capture
/// helper: on Wayland the Screenshot portal captures the active window where it offers that
/// target, else the desktop's own screenshot picker lets the user choose; the `GlobalShortcuts`
/// portal holds the shortcut. Nothing flashes, and window text is not available.
#[cfg(not(any(windows, target_os = "macos")))]
mod portal {
    use std::collections::HashMap;
    use std::fmt::Write as _;
    use std::io::Read as _;
    use std::pin::pin;
    use std::sync::{Arc, Mutex, PoisonError};

    use ashpd::desktop::global_shortcuts::{
        BindShortcutsOptions, ConfigureShortcutsOptions, GlobalShortcuts, NewShortcut, Shortcut as Bound,
    };
    use ashpd::desktop::screenshot::{AvailableTargets, Screenshot, ScreenshotProxy};
    use ashpd::desktop::{CreateSessionOptions, ResponseError};
    use futures::{FutureExt as _, StreamExt as _};
    use gpui_kit::App;
    use sha2::{Digest as _, Sha256};

    use super::{Capture, Grab, media};
    use crate::keybindings::Shortcut as Chord;
    use crate::snap_shot::setup::{self, LinuxBackend, Mode, PortalShortcut, State};
    use crate::snap_shot::{self, Modifier, Shortcut};

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
    const CHECK_FAILED: &str = "Could not check desktop capture support. Check your desktop session and try again.";
    const CANCELLED: &str = "Snapshot was cancelled.";
    const DENIED: &str = "Your desktop did not allow the snapshot.";
    const INVALID: &str = "Invalid screenshot file.";
    const SERVICE_FAILED: &str = "Could not connect to your desktop's shortcut service.";
    /// `MAX_PNG_BYTES`.
    const MAX_PNG_BYTES: u64 = 32 * 1024 * 1024;

    /// `captureMode` and the desktop's name; the backend comes from [`detect_backend`].
    pub fn session() -> State {
        let env = |key: &str| std::env::var(key).ok();
        let runtime: Vec<String> = env("XDG_RUNTIME_DIR")
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .map(|entries| entries.flatten().filter_map(|entry| entry.file_name().into_string().ok()).collect())
            .unwrap_or_default();
        let wayland = setup::is_wayland_session(
            env("XDG_SESSION_TYPE").as_deref(),
            env("WAYLAND_DISPLAY").as_deref(),
            runtime.iter().map(String::as_str),
        );
        if !wayland {
            return State { mode: Mode::Unavailable, message: Some(setup::X11_UNAVAILABLE.into()), ..State::default() };
        }
        State {
            mode: Mode::Portal,
            linux_desktop: env("XDG_CURRENT_DESKTOP").as_deref().and_then(setup::linux_desktop),
            ..State::default()
        }
    }

    /// A session bus connection the portals know as this app (`org.freedesktop.host.portal.Registry`).
    async fn connect() -> Result<zbus::Connection, String> {
        let connection = zbus::Connection::session().await.map_err(|_| CHECK_FAILED.to_owned())?;
        if std::env::var_os("FLATPAK_ID").is_none() && std::env::var_os("SNAP").is_none() {
            let options: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
            // Older portals have no registry and know the app from its desktop file.
            let _ = connection
                .call_method(
                    Some(PORTAL),
                    PORTAL_PATH,
                    Some("org.freedesktop.host.portal.Registry"),
                    "Register",
                    &(crate::desktop::APP_ID, options),
                )
                .await;
        }
        Ok(connection)
    }

    async fn backend_on(connection: &zbus::Connection) -> LinuxBackend {
        let Ok(proxy) = ScreenshotProxy::with_connection(connection.clone()).await else { return LinuxBackend::Picker };
        let window = proxy.version() >= 3
            && proxy.available_targets().await.is_ok_and(|targets| targets.contains(AvailableTargets::ActiveWindow));
        if window { LinuxBackend::ScreenshotPortal } else { LinuxBackend::Picker }
    }

    /// `getLinuxCaptureSupport`: the active-window screenshot, else the picker.
    pub async fn detect_backend() -> Result<LinuxBackend, String> {
        Ok(backend_on(&connect().await?).await)
    }

    /// Captures the active window, or the screenshot the user picks. Blocks; run it off the UI thread.
    pub fn capture(_text: bool, _grabbed: impl FnOnce(Grab)) -> Result<Capture, &'static str> {
        futures::executor::block_on(async {
            let connection = connect().await.map_err(|_| snap_shot::CAPTURE_FAILED)?;
            let backend = backend_on(&connection).await;
            let request = Screenshot::request().connection(Some(connection)).modal(false);
            let request = match backend {
                LinuxBackend::ScreenshotPortal => request.interactive(false).target(AvailableTargets::ActiveWindow),
                LinuxBackend::Picker => request.interactive(true),
            };
            let screenshot = match request.send().await.and_then(|request| request.response()) {
                Ok(screenshot) => screenshot,
                Err(ashpd::Error::Response(ResponseError::Cancelled)) => return Err(CANCELLED),
                Err(ashpd::Error::Response(_)) => return Err(DENIED),
                Err(_) => return Err(snap_shot::CAPTURE_FAILED),
            };
            let png = read_png(screenshot.uri().as_str())?;
            let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png).map_err(|_| INVALID)?;
            let (bytes, mime) = media::fit_and_encode(image.to_rgba8())?;
            // `source.name`: the portal names no window, so the active one is all Electron knows.
            let name = match backend {
                LinuxBackend::ScreenshotPortal => "Active window",
                LinuxBackend::Picker => "",
            };
            Ok(Capture {
                bytes,
                mime,
                app_name: snap_shot::bounded(name, 255).unwrap_or_else(|| "Window".into()),
                window_title: name.into(),
                icon_png: None,
                accessible_text: None,
            })
        })
    }

    /// `readPortalPng`: a local PNG within the size limit. The file is the portal's; it stays.
    fn read_png(uri: &str) -> Result<Vec<u8>, &'static str> {
        let path = url::Url::parse(uri)
            .ok()
            .filter(|url| url.scheme() == "file")
            .and_then(|url| url.to_file_path().ok())
            .ok_or(INVALID)?;
        let file = std::fs::File::open(path).map_err(|_| INVALID)?;
        let meta = file.metadata().map_err(|_| INVALID)?;
        if !meta.is_file() || meta.len() > MAX_PNG_BYTES {
            return Err(INVALID);
        }
        let mut png = Vec::new();
        file.take(MAX_PNG_BYTES).read_to_end(&mut png).map_err(|_| INVALID)?;
        png.starts_with(b"\x89PNG\r\n\x1a\n").then_some(png).ok_or(INVALID)
    }

    /// What the UI asks of a running shortcut session.
    #[derive(Default)]
    struct Info {
        session: bool,
        version: u32,
        pending: bool,
    }

    /// A `GlobalShortcuts` session on its own thread; dropping it closes the session.
    pub struct Session {
        info: Arc<Mutex<Info>>,
        configure: async_channel::Sender<()>,
    }

    impl Session {
        pub fn configure(&self) -> Option<Result<(), &'static str>> {
            let info = self.info.lock().unwrap_or_else(PoisonError::into_inner);
            if !info.session || info.pending {
                return None;
            }
            if info.version < 2 {
                return Some(Err("Open your desktop's shortcut settings and allow T3 Code's capture shortcut."));
            }
            Some(self.configure.try_send(()).map_err(|_| SERVICE_FAILED))
        }
    }

    /// `PortalCaptureShortcut`: binds `chord` through the `GlobalShortcuts` portal. Each activation
    /// sends on `trigger`; what the desktop says about the shortcut goes to `updates`.
    pub fn register_portal(
        chord: &Chord,
        trigger: async_channel::Sender<()>,
        updates: async_channel::Sender<PortalShortcut>,
    ) -> super::Registration {
        let info = Arc::new(Mutex::new(Info { pending: true, ..Info::default() }));
        let (configure, requests) = async_channel::unbounded();
        let registration = super::Registration { portal: Session { info: info.clone(), configure } };
        let preferred = match setup::portal_trigger(chord) {
            Ok(preferred) => preferred,
            Err(message) => {
                info.lock().unwrap_or_else(PoisonError::into_inner).pending = false;
                let _ = updates.try_send(PortalShortcut::failed(message));
                return registration;
            }
        };
        let report = updates.clone();
        let spawned = std::thread::Builder::new().name("snap-shot-portal".into()).spawn(move || {
            let run = run(&preferred, &trigger, &updates, &requests, &info);
            if let Err(message) = futures::executor::block_on(run) {
                info.lock().unwrap_or_else(PoisonError::into_inner).pending = false;
                let _ = updates.try_send(PortalShortcut::failed(message));
            }
        });
        if spawned.is_err() {
            let _ = report.try_send(PortalShortcut::failed(SERVICE_FAILED));
        }
        registration
    }

    /// The shortcut's id: stable per trigger, so the desktop remembers its approval.
    fn shortcut_id(preferred: &str) -> String {
        let digest = Sha256::digest(preferred.as_bytes());
        digest.iter().take(8).fold("t3-snap-shot-".to_owned(), |mut id, byte| {
            let _ = write!(id, "{byte:02x}");
            id
        })
    }

    fn label<'a>(shortcuts: &'a [Bound], id: &str) -> Option<&'a str> {
        shortcuts.iter().find(|shortcut| shortcut.id() == id).map(Bound::trigger_description)
    }

    /// Runs the session until the registration is dropped (`Ok`) or the desktop ends it (`Err`).
    async fn run(
        preferred: &str,
        trigger: &async_channel::Sender<()>,
        updates: &async_channel::Sender<PortalShortcut>,
        requests: &async_channel::Receiver<()>,
        info: &Mutex<Info>,
    ) -> Result<(), String> {
        let failed = |_| SERVICE_FAILED.to_owned();
        let portal = GlobalShortcuts::with_connection(connect().await?).await.map_err(failed)?;
        let version = portal.version();
        let session = portal
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(|_| "Your desktop could not create a capture shortcut session.".to_owned())?;
        {
            let mut info = info.lock().unwrap_or_else(PoisonError::into_inner);
            info.session = true;
            info.version = version;
        }
        let mut activated = pin!(portal.receive_activated().await.map_err(failed)?.fuse());
        let mut changed = pin!(portal.receive_shortcuts_changed().await.map_err(failed)?.fuse());
        let mut closed = pin!(session.receive_closed().await.map_err(failed)?.fuse());
        let id = shortcut_id(preferred);
        let shortcut = NewShortcut::new(id.as_str(), "Capture a window").preferred_trigger(preferred);
        // Every session binds, even when the desktop remembers this shortcut's approval.
        let shortcuts = [shortcut];
        let bind = portal.bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default());
        let bound = futures::select! {
            bound = pin!(bind.fuse()) => bound,
            _ = requests.recv().fuse() => {
                let _ = session.close().await;
                return Ok(());
            }
        };
        let mut state = match bound.and_then(|request| request.response()) {
            Ok(bound) => PortalShortcut::bound(label(bound.shortcuts(), &id), version),
            Err(ashpd::Error::Response(_)) => PortalShortcut::denied(version),
            Err(_) => return Err(SERVICE_FAILED.into()),
        };
        loop {
            info.lock().unwrap_or_else(PoisonError::into_inner).pending = state.pending;
            let registered = state.registered;
            let _ = updates.try_send(state.clone());
            futures::select! {
                event = activated.next() => match event {
                    Some(event) if registered && event.shortcut_id() == id => {
                        let _ = trigger.try_send(());
                    }
                    Some(_) => {}
                    None => return Err(SERVICE_FAILED.into()),
                },
                event = changed.next() => match event {
                    Some(event) => state = PortalShortcut::bound(label(event.shortcuts(), &id), version),
                    None => return Err(SERVICE_FAILED.into()),
                },
                _ = closed.next() => {
                    return Err("Your desktop closed the capture shortcut. Retry the shortcut request.".into());
                }
                request = requests.recv().fuse() => {
                    if request.is_err() {
                        let _ = session.close().await;
                        return Ok(());
                    }
                    if portal.configure_shortcuts(&session, None, ConfigureShortcutsOptions::default()).await.is_err() {
                        log::warn!("could not open the desktop's shortcut settings");
                    }
                }
            }
        }
    }

    pub struct Flash;

    impl Flash {
        pub fn new(_: &Grab) -> Self {
            Self
        }
    }

    pub fn register(_: &Shortcut, _: async_channel::Sender<()>) -> Option<super::Registration> {
        None
    }

    pub fn check_chord(_: &Chord) -> bool {
        false
    }

    pub fn pair_held(_: Modifier) -> bool {
        false
    }

    pub fn reduced_motion() -> bool {
        false
    }

    pub fn retire_flash(_: &mut App) {}

    pub fn show_flash(_: Flash, _: bool, _: &mut App) -> gpui_kit::Task<()> {
        gpui_kit::Task::ready(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn shortcut_ids_are_stable_per_trigger() {
            assert_eq!(shortcut_id("CTRL+SHIFT+2"), shortcut_id("CTRL+SHIFT+2"));
            assert_ne!(shortcut_id("CTRL+SHIFT+2"), shortcut_id("CTRL+SHIFT+3"));
            assert_eq!(shortcut_id("CTRL+SHIFT+2").len(), "t3-snap-shot-".len() + 16);
        }

        #[test]
        fn screenshots_must_be_local_pngs() {
            assert_eq!(read_png("https://example.com/shot.png"), Err(INVALID));
            assert_eq!(read_png("file:///nonexistent/t3-shot.png"), Err(INVALID));
            let dir = std::env::temp_dir().join(format!("t3-snap-shot-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("a temp dir");
            let file = dir.join("shot.png");
            std::fs::write(&file, b"not a png").expect("a file");
            let uri = url::Url::from_file_path(&file).expect("a file url").to_string();
            assert_eq!(read_png(&uri), Err(INVALID));
            std::fs::write(&file, b"\x89PNG\r\n\x1a\nrest").expect("a file");
            assert_eq!(read_png(&uri).map(|png| png.len()), Ok(12));
            std::fs::remove_file(&file).expect("removed");
            std::fs::remove_dir(&dir).expect("removed");
        }
    }
}
