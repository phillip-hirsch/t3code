//! The Windows half of Electron's `DesktopSnapShot`: the global capture shortcut
//! (`globalShortcut` for chords, `GlobalShiftShortcutWorker`'s key-state poll for modifier
//! pairs), the foreground window capture (`ActiveWindow.ts`, `RegionSnapShot.ts`) and the
//! flash over the captured window (`SnapShotFlash`). Other platforms have no capture.

/// A registered shortcut; dropping it unregisters.
pub struct Registration {
    _stop: imp::Stop,
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
pub struct Grab<'a> {
    /// The window's top-left corner on screen, in physical pixels.
    pub origin: (i32, i32),
    pub image: &'a image::RgbaImage,
}

pub use imp::{
    Flash, available, capture, check_chord, pair_held, play_sound, reduced_motion, register, retire_flash, show_flash,
};

#[cfg(windows)]
mod imp {
    use std::cell::{OnceCell, RefCell};
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use gpui_kit::{
        Animation, AnimationExt as _, AnyWindowHandle, App, AppContext as _, Bounds, Context, IntoElement,
        ParentElement as _, Render, RenderImage, Styled as _, Window, WindowBounds, WindowKind, WindowOptions, div,
        img, point, px, size, white,
    };
    use image::{ImageFormat, RgbaImage, imageops::FilterType};
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
    use uiautomation::types::{Handle, TreeScope, UIProperty};
    use winsafe::{self as w, co};

    use super::{Capture, Grab};
    use crate::keybindings::Shortcut as Chord;
    use crate::snap_shot::{self, Modifier, Shortcut};

    /// `GlobalShiftShortcutWorker`'s `POLL_INTERVAL_MS`.
    const PAIR_POLL: Duration = Duration::from_millis(50);
    /// `FLASH_PEAK_OPACITY`, `FLASH_ANIMATION_DURATION_MS`, `FLASH_STATIC_DURATION_MS`.
    const FLASH_PEAK_OPACITY: f32 = 0.08;
    const FLASH_ANIMATED: Duration = Duration::from_millis(180);
    const FLASH_STATIC: Duration = Duration::from_millis(60);

    /// `ACCESSIBILITY_TIMEOUT_MS`.
    const ACCESSIBILITY_TIMEOUT: Duration = Duration::from_secs(3);

    pub struct Stop(Box<dyn FnMut()>);

    impl Drop for Stop {
        fn drop(&mut self) {
            (self.0)();
        }
    }

    pub fn available() -> bool {
        true
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

    /// Captures the foreground window as it shows on screen, handing its pixels to `grabbed`
    /// first, and reads its text too with `text`. Blocks; run it off the UI thread.
    pub fn capture(text: bool, grabbed: impl FnOnce(Grab)) -> Result<Capture, &'static str> {
        let hwnd = w::HWND::GetForegroundWindow().ok_or(snap_shot::WINDOW_UNAVAILABLE)?;
        let rect = window_rect(&hwnd).ok_or(snap_shot::WINDOW_UNAVAILABLE)?;
        let mut image = grab(rect).ok_or(snap_shot::CAPTURE_FAILED)?;
        grabbed(Grab { origin: (rect.left, rect.top), image: &image });
        let text = text.then(|| read_text(hwnd.ptr() as isize));
        let (width, height) = image.dimensions();
        let (fit_width, fit_height) = snap_shot::fit_capture(width, height);
        if (fit_width, fit_height) != (width, height) {
            image = image::imageops::resize(&image, fit_width, fit_height, FilterType::Triangle);
        }
        let (bytes, mime) = encode(&image).ok_or(snap_shot::CAPTURE_FAILED)?;
        if bytes.len() > snap_shot::MAX_IMAGE_BYTES {
            return Err(snap_shot::TOO_LARGE);
        }
        let path = process_image_path(&hwnd);
        Ok(Capture {
            bytes,
            mime,
            app_name: snap_shot::app_name(&path),
            window_title: hwnd.GetWindowText().unwrap_or_default(),
            icon_png: (!path.is_empty()).then(|| file_icon(&path)).flatten(),
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
        let (width, height) = ((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32);
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
                .capture_region((left - x) as u32, (top - y) as u32, (right - left) as u32, (bottom - top) as u32)
                .ok()?;
            image::imageops::replace(&mut image, &part, i64::from(left - rect.left), i64::from(top - rect.top));
            covered = true;
        }
        for pixel in image.pixels_mut() {
            pixel[3] = 255;
        }
        covered.then_some(image)
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

    /// `app.getFileIcon(path, { size: "normal" })` scaled to 64px, as PNG.
    fn file_icon(path: &str) -> Option<Vec<u8>> {
        let icon = file_icon_provider::get_file_icon(path, 32).ok()?;
        let image = RgbaImage::from_raw(icon.width, icon.height, icon.pixels)?;
        encode_png(&image::imageops::resize(&image, 64, 64, FilterType::Lanczos3))
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
            for pixel in bgra.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            let size = bgra.dimensions();
            Self { origin: grab.origin, size, image: Arc::new(RenderImage::new([image::Frame::new(bgra)])) }
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
    /// the captured pixels under the white.
    pub fn show_flash(flash: Flash, animated: bool, cx: &mut App) {
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
        let (Ok(window), Some(hwnd)) = (opened, hwnd) else { return };
        let window: AnyWindowHandle = window.into();
        FLASH.set(Some(window));
        cx.spawn(async move |cx| {
            // Placed in physical pixels outside any GPUI update: moving a window re-enters GPUI.
            if let Some(popup) = super::super::top_level_window(hwnd) {
                let _ = popup.SetWindowPos(
                    w::HwndPlace::Place(co::HWND_PLACE::TOPMOST),
                    w::POINT::with(origin.0, origin.1),
                    w::SIZE::with(extent.0 as i32, extent.1 as i32),
                    co::SWP::NOACTIVATE | co::SWP::SHOWWINDOW,
                );
            }
            cx.background_executor().timer(if animated { FLASH_ANIMATED } else { FLASH_STATIC }).await;
            let _ = cx.update(|cx| {
                if FLASH.with_borrow(|current| *current == Some(window)) {
                    retire_flash(cx);
                }
            });
        })
        .detach();
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

#[cfg(not(windows))]
mod imp {
    use gpui_kit::App;

    use super::{Capture, Grab};
    use crate::keybindings::Shortcut as Chord;
    use crate::snap_shot::{self, Modifier, Shortcut};

    pub struct Stop;

    pub struct Flash;

    impl Flash {
        pub fn new(_: &Grab) -> Self {
            Self
        }
    }

    pub fn available() -> bool {
        false
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

    pub fn play_sound(_: &'static [u8]) {}

    pub fn capture(_: bool, _: impl FnOnce(Grab)) -> Result<Capture, &'static str> {
        Err(snap_shot::CAPTURE_FAILED)
    }

    pub fn retire_flash(_: &mut App) {}

    pub fn show_flash(_: Flash, _: bool, _: &mut App) {}
}
