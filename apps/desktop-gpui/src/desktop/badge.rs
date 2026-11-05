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

/// A one-image `.ico` file: 32-bit BGRA `pixels` (straight alpha, rows top to bottom) and a clear AND mask.
pub fn ico(pixels: &[u8], size: u32) -> Vec<u8> {
    let side = size as usize;
    assert_eq!(pixels.len(), side * side * 4, "a {size}px icon needs {} bytes", side * side * 4);
    // Mask rows are 1 bit per pixel, padded to 32 bits.
    let mask = side.div_ceil(32) * 4 * side;
    let image = 40 + pixels.len() + mask;
    let dimension = if size >= 256 { 0 } else { size as u8 };
    let mut ico = Vec::with_capacity(22 + image);
    ico.extend_from_slice(&[0, 0, 1, 0, 1, 0]);
    ico.extend_from_slice(&[dimension, dimension, 0, 0, 1, 0, 32, 0]);
    ico.extend_from_slice(&(image as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes());
    // BITMAPINFOHEADER: the height counts the color rows and the mask rows.
    ico.extend_from_slice(&40u32.to_le_bytes());
    ico.extend_from_slice(&size.to_le_bytes());
    ico.extend_from_slice(&(size * 2).to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&32u16.to_le_bytes());
    ico.extend_from_slice(&0u32.to_le_bytes());
    ico.extend_from_slice(&((pixels.len() + mask) as u32).to_le_bytes());
    ico.extend_from_slice(&[0; 16]);
    // Bitmap rows run bottom to top.
    for row in pixels.chunks_exact(side * 4).rev() {
        ico.extend_from_slice(row);
    }
    ico.resize(ico.len() + mask, 0);
    ico
}

#[cfg(windows)]
mod windows_overlay {
    use std::sync::Arc;

    use resvg::{tiny_skia, usvg};
    use winsafe::{self as w, co, prelude::*};

    #[derive(Default)]
    pub struct Taskbar {
        list: Option<(w::ITaskbarList3, w::guard::CoUninitializeGuard)>,
        fonts: Option<Arc<usvg::fontdb::Database>>,
    }

    impl Taskbar {
        pub fn apply(&mut self, hwnd: isize, count: usize) -> Result<(), String> {
            let list = match &self.list {
                Some((list, _)) => list,
                None => {
                    let com = w::CoInitializeEx(co::COINIT::APARTMENTTHREADED).map_err(|e| e.to_string())?;
                    let list: w::ITaskbarList3 =
                        w::CoCreateInstance(&co::CLSID::TaskbarList, None::<&w::IUnknown>, co::CLSCTX::INPROC_SERVER)
                            .map_err(|e| e.to_string())?;
                    list.HrInit().map_err(|e| e.to_string())?;
                    &self.list.insert((list, com)).0
                }
            };
            let window = super::super::top_level_window(hwnd).ok_or("the window is gone")?;
            if count == 0 {
                return list.SetOverlayIcon(&window, None, "").map_err(|e| e.to_string());
            }
            let size = w::GetSystemMetrics(co::SM::CXSMICON).max(16) as u32;
            let Some(pixels) = render(&mut self.fonts, count, size) else { return Ok(()) };
            let icon = load_icon(&super::ico(&pixels, size), size)?;
            let description = format!("{count} threads with new notifications");
            list.SetOverlayIcon(&window, Some(&icon), &description).map_err(|e| e.to_string())
        }
    }

    /// Windows builds icons from pixels only through raw handles, so the icon goes through a file.
    fn load_icon(ico: &[u8], size: u32) -> Result<w::guard::DestroyIconGuard, String> {
        let path = std::env::temp_dir().join(format!("t3code-badge-{}.ico", std::process::id()));
        std::fs::write(&path, ico).map_err(|e| e.to_string())?;
        let icon = w::HINSTANCE::NULL.LoadImageIcon(
            w::IdOicStr::Str(w::WString::from_str(path.to_string_lossy())),
            w::SIZE::with(size as i32, size as i32),
            co::LR::LOADFROMFILE,
        );
        let _ = std::fs::remove_file(&path);
        icon.map_err(|e| e.to_string())
    }

    /// BGRA rows with straight alpha, top to bottom.
    fn render(fonts: &mut Option<Arc<usvg::fontdb::Database>>, count: usize, size: u32) -> Option<Vec<u8>> {
        let fonts = fonts.get_or_insert_with(|| {
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn windows_loads_the_rendered_badge_icon() {
            for (count, size) in [(3, 16), (12, 20), (1, 32)] {
                let pixels = render(&mut None, count, size).expect("the badge renders");
                load_icon(&crate::desktop::badge::ico(&pixels, size), size)
                    .unwrap_or_else(|error| panic!("a {size}px badge for {count} loads: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ico_holds_one_bottom_up_image_with_a_clear_mask() {
        // 2x2: the top row red, the bottom row blue (BGRA).
        let red = [0, 0, 255, 255];
        let blue = [255, 0, 0, 255];
        let pixels: Vec<u8> = [red, red, blue, blue].concat();
        let ico = ico(&pixels, 2);
        let u32_at = |at: usize| u32::from_le_bytes(ico[at..at + 4].try_into().unwrap());
        assert_eq!(&ico[..6], &[0, 0, 1, 0, 1, 0], "an icon file with one image");
        assert_eq!((ico[6], ico[7]), (2, 2));
        assert_eq!(u32_at(18), 22, "the image follows the directory");
        assert_eq!(u32_at(14) as usize, ico.len() - 22);
        assert_eq!((u32_at(22), u32_at(26), u32_at(30)), (40, 2, 4), "the height counts the mask rows");
        assert_eq!(&ico[62..70], &[blue, blue].concat()[..], "rows run bottom to top");
        assert_eq!(&ico[70..78], &[red, red].concat()[..]);
        assert_eq!(&ico[78..], &[0; 8], "two mask rows of 32 bits, all clear");
    }
}
