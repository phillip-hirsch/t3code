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

pub use imp::{available, capture, check_chord, pair_held, play_sound, reduced_motion, register};

#[cfg(windows)]
mod imp {
    use std::io::Cursor;
    use std::sync::Once;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    use std::{mem, ptr, thread};

    use image::{ImageFormat, RgbaImage, imageops::FilterType};
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, RECT};
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, GetObjectW, GetStockObject, HBITMAP, HDC, ReleaseDC,
        SRCCOPY, SelectObject, WHITE_BRUSH,
    };
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{
        GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
    };
    use windows_sys::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, GetForegroundWindow,
        GetIconInfo, GetMessageW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, ICONINFO, LWA_ALPHA, MSG,
        PM_NOREMOVE, PM_REMOVE, PeekMessageW, PostThreadMessageW, RegisterClassExW, SPI_GETCLIENTAREAANIMATION,
        SW_SHOWNOACTIVATE, SetLayeredWindowAttributes, ShowWindow, SystemParametersInfoW, TranslateMessage, WM_HOTKEY,
        WM_QUIT, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
        WS_POPUP,
    };

    use super::Capture;
    use crate::keybindings::Shortcut as Chord;
    use crate::snap_shot::{self, Modifier, Shortcut};

    /// `GlobalShiftShortcutWorker`'s `POLL_INTERVAL_MS`.
    const PAIR_POLL: Duration = Duration::from_millis(50);
    const HOTKEY_ID: i32 = 1;
    /// `FLASH_PEAK_OPACITY`, `FLASH_ANIMATION_DURATION_MS`, `FLASH_STATIC_DURATION_MS`, `FLASH_FRAME_INTERVAL_MS`.
    const FLASH_PEAK_ALPHA: f32 = 0.08 * 255.;
    const FLASH_ANIMATED: Duration = Duration::from_millis(180);
    const FLASH_STATIC: Duration = Duration::from_millis(60);
    const FLASH_FRAME: Duration = Duration::from_millis(16);

    /// `ACCESSIBILITY_TIMEOUT_MS`.
    const ACCESSIBILITY_TIMEOUT: Duration = Duration::from_secs(3);

    /// Bumped by each capture so an older flash retires before the next screenshot.
    static FLASH_GENERATION: AtomicU64 = AtomicU64::new(0);

    pub struct Stop(Box<dyn FnMut() + Send>);

    impl Drop for Stop {
        fn drop(&mut self) {
            (self.0)();
        }
    }

    pub fn available() -> bool {
        true
    }

    /// Registers `shortcut`; each press sends on `trigger`.
    pub fn register(shortcut: &Shortcut, trigger: async_channel::Sender<()>) -> Option<super::Registration> {
        let stop = match shortcut {
            Shortcut::Pair(modifier) => watch_pair(*modifier, trigger),
            Shortcut::Chord(chord) => register_chord(chord, trigger)?,
        };
        Some(super::Registration { _stop: stop })
    }

    /// A thread owns the hotkey and its message loop; `WM_QUIT` unregisters it.
    fn register_chord(chord: &Chord, trigger: async_channel::Sender<()>) -> Option<Stop> {
        let (modifiers, vk) = snap_shot::hotkey(chord)?;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        thread::Builder::new()
            .name("snap-shot-hotkey".into())
            .spawn(move || unsafe {
                let mut msg: MSG = mem::zeroed();
                // Creates the queue `WM_QUIT` is posted to before anyone can post it.
                PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_NOREMOVE);
                let registered = RegisterHotKey(ptr::null_mut(), HOTKEY_ID, modifiers | MOD_NOREPEAT, vk) != 0;
                let _ = ready_tx.send(registered.then(|| GetCurrentThreadId()));
                if !registered {
                    return;
                }
                while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
                    if msg.message == WM_HOTKEY {
                        let _ = trigger.try_send(());
                    }
                }
                UnregisterHotKey(ptr::null_mut(), HOTKEY_ID);
            })
            .ok()?;
        let thread_id = ready_rx.recv().ok().flatten()?;
        Some(Stop(Box::new(move || unsafe {
            PostThreadMessageW(thread_id, WM_QUIT, 0, 0);
        })))
    }

    /// Polls both keys of `modifier`, sending once each time they go down together.
    fn watch_pair(modifier: Modifier, trigger: async_channel::Sender<()>) -> Stop {
        let stopped = std::sync::Arc::new(AtomicBool::new(false));
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

    pub fn pair_held(modifier: Modifier) -> bool {
        let (left, right) = modifier.virtual_keys();
        unsafe { GetAsyncKeyState(left) < 0 && GetAsyncKeyState(right) < 0 }
    }

    /// `probeGlobalShortcut`: registers and releases the chord.
    pub fn check_chord(chord: &Chord) -> bool {
        let Some((modifiers, vk)) = snap_shot::hotkey(chord) else { return false };
        thread::spawn(move || unsafe {
            let registered = RegisterHotKey(ptr::null_mut(), HOTKEY_ID, modifiers | MOD_NOREPEAT, vk) != 0;
            if registered {
                UnregisterHotKey(ptr::null_mut(), HOTKEY_ID);
            }
            registered
        })
        .join()
        .unwrap_or(false)
    }

    /// Windows' "Show animations" setting, which Electron reads as `prefersReducedMotion`.
    pub fn reduced_motion() -> bool {
        let mut animate = 1i32;
        let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut animate).cast(), 0) };
        ok != 0 && animate == 0
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

    /// Captures the foreground window as it shows on screen, then flashes it when `flash` is
    /// `Some(animated)`, reading its text too with `text`. Blocks; run it off the UI thread.
    pub fn capture(flash: Option<bool>, text: bool) -> Result<Capture, &'static str> {
        FLASH_GENERATION.fetch_add(1, Ordering::Relaxed);
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return Err(snap_shot::WINDOW_UNAVAILABLE);
            }
            let rect = window_rect(hwnd).ok_or(snap_shot::WINDOW_UNAVAILABLE)?;
            let (width, height) = ((rect.right - rect.left) as u32, (rect.bottom - rect.top) as u32);
            let pixels = grab(&rect).ok_or(snap_shot::CAPTURE_FAILED)?;
            if let Some(animated) = flash {
                show_flash(rect, animated);
            }
            let text = text.then(|| read_text(hwnd as isize));
            let mut image = RgbaImage::from_raw(width, height, pixels).ok_or(snap_shot::CAPTURE_FAILED)?;
            let (fit_width, fit_height) = snap_shot::fit_capture(width, height);
            if (fit_width, fit_height) != (width, height) {
                image = image::imageops::resize(&image, fit_width, fit_height, FilterType::Triangle);
            }
            let (bytes, mime) = encode(&image).ok_or(snap_shot::CAPTURE_FAILED)?;
            if bytes.len() > snap_shot::MAX_IMAGE_BYTES {
                return Err(snap_shot::TOO_LARGE);
            }
            let path = process_image_path(hwnd);
            Ok(Capture {
                bytes,
                mime,
                app_name: snap_shot::app_name(&path),
                window_title: window_text(hwnd),
                icon_png: (!path.is_empty()).then(|| file_icon(&path)).flatten(),
                accessible_text: text.and_then(|read| read.recv_timeout(ACCESSIBILITY_TIMEOUT).ok().flatten()),
            })
        }
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
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
        };
        use windows::Win32::UI::Accessibility::{
            CUIAutomation, IUIAutomation, TreeScope_Subtree, UIA_NamePropertyId, UIA_ValueValuePropertyId,
        };
        unsafe {
            let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let read = || -> windows::core::Result<String> {
                let automation: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
                let window = automation.ElementFromHandle(windows::Win32::Foundation::HWND(hwnd as *mut _))?;
                let request = automation.CreateCacheRequest()?;
                request.AddProperty(UIA_NamePropertyId)?;
                request.AddProperty(UIA_ValueValuePropertyId)?;
                let all = automation.CreateTrueCondition()?;
                let elements = window.FindAllBuildCache(TreeScope_Subtree, &all, &request)?;
                let count = elements.Length()?.max(0) as usize;
                let candidates = (0..count.min(snap_shot::ACCESSIBILITY_MAX_NODES)).flat_map(|i| {
                    let Ok(element) = elements.GetElement(i as i32) else { return [None, None] };
                    let name = element.CachedName().ok().map(|name| name.to_string());
                    let value = element
                        .GetCachedPropertyValue(UIA_ValueValuePropertyId)
                        .ok()
                        .and_then(|value| windows::core::BSTR::try_from(&value).ok())
                        .map(|value| value.to_string());
                    [name, value]
                });
                Ok(snap_shot::accessible_text(candidates.flatten(), snap_shot::ACCESSIBLE_TEXT_MAX))
            };
            let text = read().ok().filter(|text| !text.is_empty());
            if initialized {
                CoUninitialize();
            }
            text
        }
    }

    fn encode_png(image: &RgbaImage) -> Option<Vec<u8>> {
        let mut png = Vec::new();
        image.write_to(&mut Cursor::new(&mut png), ImageFormat::Png).ok()?;
        Some(png)
    }

    /// The window's visible frame: DWM's bounds leave out the invisible resize border.
    unsafe fn window_rect(hwnd: HWND) -> Option<RECT> {
        let mut rect: RECT = unsafe { mem::zeroed() };
        let size = mem::size_of::<RECT>() as u32;
        let dwm =
            unsafe { DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS as u32, (&raw mut rect).cast(), size) };
        if dwm != 0 && unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
            return None;
        }
        (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
    }

    /// The screen's pixels under `rect` as RGBA.
    unsafe fn grab(rect: &RECT) -> Option<Vec<u8>> {
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        unsafe {
            let screen = GetDC(ptr::null_mut());
            let memory = CreateCompatibleDC(screen);
            let mut bits = ptr::null_mut();
            let bitmap =
                CreateDIBSection(screen, &bitmap_info(width, height), DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
            let mut pixels = None;
            if !bitmap.is_null() && !bits.is_null() {
                let previous = SelectObject(memory, bitmap);
                if BitBlt(memory, 0, 0, width, height, screen, rect.left, rect.top, SRCCOPY | CAPTUREBLT) != 0 {
                    let bgra = std::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize);
                    pixels = Some(rgba(bgra, true));
                }
                SelectObject(memory, previous);
                DeleteObject(bitmap);
            }
            DeleteDC(memory);
            ReleaseDC(ptr::null_mut(), screen);
            pixels
        }
    }

    /// A top-down 32-bit DIB.
    fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
        let mut info: BITMAPINFO = unsafe { mem::zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { mem::zeroed() }
        };
        info
    }

    fn rgba(bgra: &[u8], opaque: bool) -> Vec<u8> {
        bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], if opaque { 255 } else { p[3] }]).collect()
    }

    unsafe fn window_text(hwnd: HWND) -> String {
        let mut buffer = [0u16; 1024];
        let len = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
        String::from_utf16_lossy(&buffer[..len.max(0) as usize])
    }

    unsafe fn process_image_path(hwnd: HWND) -> String {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid == 0 {
            return String::new();
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return String::new();
        }
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut len) };
        unsafe { CloseHandle(process) };
        if ok == 0 { String::new() } else { String::from_utf16_lossy(&buffer[..len as usize]) }
    }

    /// `app.getFileIcon(path, { size: "normal" })` scaled to 64px, as PNG.
    unsafe fn file_icon(path: &str) -> Option<Vec<u8>> {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        unsafe {
            let initialized = CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) >= 0;
            let mut info: SHFILEINFOW = mem::zeroed();
            let found = SHGetFileInfoW(
                wide.as_ptr(),
                0,
                &mut info,
                mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            );
            let png = if found != 0 && !info.hIcon.is_null() {
                let mut icon: ICONINFO = mem::zeroed();
                let png = if GetIconInfo(info.hIcon, &mut icon) != 0 {
                    let png = icon_pixels(icon.hbmColor).and_then(|image| {
                        let image = image::imageops::resize(&image, 64, 64, FilterType::Lanczos3);
                        encode_png(&image)
                    });
                    DeleteObject(icon.hbmColor);
                    DeleteObject(icon.hbmMask);
                    png
                } else {
                    None
                };
                DestroyIcon(info.hIcon);
                png
            } else {
                None
            };
            if initialized {
                CoUninitialize();
            }
            png
        }
    }

    unsafe fn icon_pixels(bitmap: HBITMAP) -> Option<RgbaImage> {
        if bitmap.is_null() {
            return None;
        }
        unsafe {
            let mut header: BITMAP = mem::zeroed();
            if GetObjectW(bitmap, mem::size_of::<BITMAP>() as i32, (&raw mut header).cast()) == 0 {
                return None;
            }
            let (width, height) = (header.bmWidth, header.bmHeight);
            let mut bgra = vec![0u8; (width * height * 4) as usize];
            let dc: HDC = GetDC(ptr::null_mut());
            let mut info = bitmap_info(width, height);
            let rows = GetDIBits(dc, bitmap, 0, height as u32, bgra.as_mut_ptr().cast(), &mut info, DIB_RGB_COLORS);
            ReleaseDC(ptr::null_mut(), dc);
            // Icons without an alpha channel leave it zero everywhere.
            let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
            (rows != 0).then(|| RgbaImage::from_raw(width as u32, height as u32, rgba(&bgra, !has_alpha))).flatten()
        }
    }

    /// A white, click-through, never-focused window over the capture, faded out when `animated`.
    fn show_flash(rect: RECT, animated: bool) {
        let generation = FLASH_GENERATION.load(Ordering::Relaxed);
        thread::spawn(move || unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let class: Vec<u16> = "T3SnapShotFlash\0".encode_utf16().collect();
            let instance = GetModuleHandleW(ptr::null());
            static REGISTER: Once = Once::new();
            REGISTER.call_once(|| {
                let mut window_class: WNDCLASSEXW = mem::zeroed();
                window_class.cbSize = mem::size_of::<WNDCLASSEXW>() as u32;
                window_class.lpfnWndProc = Some(DefWindowProcW);
                window_class.hInstance = instance;
                window_class.hbrBackground = GetStockObject(WHITE_BRUSH);
                window_class.lpszClassName = class.as_ptr();
                RegisterClassExW(&window_class);
            });
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                ptr::null(),
                WS_POPUP,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                ptr::null(),
            );
            if hwnd.is_null() {
                return;
            }
            SetLayeredWindowAttributes(hwnd, 0, FLASH_PEAK_ALPHA as u8, LWA_ALPHA);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let duration = if animated { FLASH_ANIMATED } else { FLASH_STATIC };
            let start = Instant::now();
            let mut msg: MSG = mem::zeroed();
            while start.elapsed() < duration && FLASH_GENERATION.load(Ordering::Relaxed) == generation {
                while PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                if animated {
                    let left = 1. - start.elapsed().as_secs_f32() / duration.as_secs_f32();
                    SetLayeredWindowAttributes(hwnd, 0, (FLASH_PEAK_ALPHA * left.max(0.)) as u8, LWA_ALPHA);
                }
                thread::sleep(FLASH_FRAME);
            }
            DestroyWindow(hwnd);
        });
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Capture;
    use crate::keybindings::Shortcut as Chord;
    use crate::snap_shot::{self, Modifier, Shortcut};

    pub struct Stop;

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

    pub fn capture(_: Option<bool>, _: bool) -> Result<Capture, &'static str> {
        Err(snap_shot::CAPTURE_FAILED)
    }
}
