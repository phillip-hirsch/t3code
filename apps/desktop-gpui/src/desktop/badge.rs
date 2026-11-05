//! The taskbar badge for unseen thread notifications (`notificationBadge.ts`): on Windows a red
//! overlay icon with the count, drawn like `setNotificationBadge` in `threadNotifications.ts`.

/// The count as the badge shows it.
pub fn label(count: usize) -> String {
    if count > 9 { "9+".into() } else { count.to_string() }
}

/// `setNotificationBadge`'s canvas as SVG: a red disc with the count in Segoe UI Semibold.
pub fn svg(count: usize) -> String {
    let size = if count > 9 { 30 } else { 40 };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><circle cx="32" cy="32" r="28" fill="#e5484d"/><text x="32" y="34" fill="white" font-family="Segoe UI" font-weight="600" font-size="{size}" text-anchor="middle" dominant-baseline="central">{}</text></svg>"##,
        label(count)
    )
}

/// Shows `count` on the window's taskbar button; 0 clears it.
#[cfg(windows)]
pub fn set(hwnd: isize, count: usize) {
    use std::sync::OnceLock;
    use std::sync::mpsc::{Sender, channel};

    // One thread applies updates in order, off the UI thread (COM and font loading).
    static WORKER: OnceLock<Option<std::sync::Mutex<Sender<(isize, usize)>>>> = OnceLock::new();
    let worker = WORKER.get_or_init(|| {
        let (sender, receiver) = channel::<(isize, usize)>();
        let spawned = std::thread::Builder::new().name("taskbar-badge".into()).spawn(move || {
            let mut taskbar = windows_overlay::Taskbar::default();
            while let Ok(mut update) = receiver.recv() {
                // Only the latest count matters.
                while let Ok(next) = receiver.try_recv() {
                    update = next;
                }
                if let Err(error) = taskbar.apply(update.0, update.1) {
                    log::warn!("could not update the taskbar badge: {error}");
                }
            }
        });
        spawned.ok().map(|_| std::sync::Mutex::new(sender))
    });
    if let Some(sender) = worker {
        let _ = sender.lock().map(|sender| sender.send((hwnd, count)));
    }
}

#[cfg(not(windows))]
pub fn set(_hwnd: isize, _count: usize) {}

#[cfg(windows)]
mod windows_overlay {
    use std::sync::Arc;

    use resvg::{tiny_skia, usvg};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS, DeleteObject,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
    };
    use windows::core::HSTRING;

    #[derive(Default)]
    pub struct Taskbar {
        list: Option<ITaskbarList3>,
        fonts: Option<Arc<usvg::fontdb::Database>>,
    }

    impl Taskbar {
        pub fn apply(&mut self, hwnd: isize, count: usize) -> windows::core::Result<()> {
            let list = match &self.list {
                Some(list) => list.clone(),
                None => unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                    let list: ITaskbarList3 = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
                    list.HrInit()?;
                    self.list = Some(list.clone());
                    list
                },
            };
            let hwnd = HWND(hwnd as *mut _);
            if count == 0 {
                return unsafe { list.SetOverlayIcon(hwnd, HICON::default(), &HSTRING::new()) };
            }
            let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as u32;
            let Some(pixels) = self.render(count, size) else { return Ok(()) };
            let icon = icon(&pixels, size)?;
            let description = HSTRING::from(format!("{count} threads with new notifications"));
            let result = unsafe { list.SetOverlayIcon(hwnd, icon, &description) };
            unsafe {
                let _ = DestroyIcon(icon);
            }
            result
        }

        /// BGRA rows with straight alpha, top to bottom.
        fn render(&mut self, count: usize, size: u32) -> Option<Vec<u8>> {
            let fonts = self.fonts.get_or_insert_with(|| {
                let mut fonts = usvg::fontdb::Database::new();
                let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
                let dir = std::path::Path::new(&windir).join("Fonts");
                for file in ["seguisb.ttf", "segoeui.ttf"] {
                    let _ = fonts.load_font_file(dir.join(file));
                }
                Arc::new(fonts)
            });
            let options = usvg::Options { fontdb: fonts.clone(), ..usvg::Options::default() };
            let tree = usvg::Tree::from_str(&super::svg(count), &options).ok()?;
            let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
            let scale = size as f32 / 64.;
            resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
            Some(
                pixmap
                    .pixels()
                    .iter()
                    .flat_map(|pixel| {
                        let c = pixel.demultiply();
                        [c.blue(), c.green(), c.red(), c.alpha()]
                    })
                    .collect(),
            )
        }
    }

    fn icon(pixels: &[u8], size: u32) -> windows::core::Result<HICON> {
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO { bmiHeader: header, ..Default::default() };
        unsafe {
            let mut bits = std::ptr::null_mut();
            let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
            // The alpha channel shapes the icon; the 1-bit mask (rows padded to 16 bits) stays clear.
            let mask_bits = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
            let mask = CreateBitmap(size as i32, size as i32, 1, 1, Some(mask_bits.as_ptr().cast()));
            let result = CreateIconIndirect(&ICONINFO {
                fIcon: true.into(),
                hbmMask: mask,
                hbmColor: color,
                ..Default::default()
            });
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
            result
        }
    }
}
