//! Linux reads a page from `WebKitGTK`'s own snapshot of it, so windows over the browser don't show
//! in its screenshots, annotation crops or recordings.

use gpui_kit::{Bounds, Pixels};
use image::RgbaImage;
use webkit2gtk::{SnapshotOptions, SnapshotRegion, WebViewExt as _};
use wry::WebViewExtUnix as _;

/// Snapshots what the page shows, on a white background; `done` gets it on the main thread.
pub(super) fn snapshot(view: &wry::WebView, done: impl FnOnce(Result<RgbaImage, String>) + 'static) {
    let cancellable: Option<&webkit2gtk::gio::Cancellable> = None;
    view.webview().snapshot(SnapshotRegion::Visible, SnapshotOptions::NONE, cancellable, move |surface| {
        done(surface.map_err(|error| format!("Unable to capture the page: {error}")).and_then(to_image));
    });
}

fn to_image(surface: cairo::Surface) -> Result<RgbaImage, String> {
    let unreadable = || "Unable to read the page's snapshot.".to_owned();
    let surface = cairo::ImageSurface::try_from(surface).map_err(|_| unreadable())?;
    if !matches!(surface.format(), cairo::Format::ARgb32 | cairo::Format::Rgb24) {
        return Err(unreadable());
    }
    let (Ok(width), Ok(height), Ok(stride)) =
        (u32::try_from(surface.width()), u32::try_from(surface.height()), usize::try_from(surface.stride()))
    else {
        return Err(unreadable());
    };
    let mut image = None;
    surface.with_data(|data| image = opaque(data, width, height, stride)).map_err(|_| unreadable())?;
    image.ok_or_else(unreadable)
}

/// Cairo's 32-bit pixels, native-endian `0xAARRGGBB` in rows `stride` bytes apart, as opaque RGBA.
fn opaque(data: &[u8], width: u32, height: u32, stride: usize) -> Option<RgbaImage> {
    let row_bytes = usize::try_from(width).ok()? * 4;
    if row_bytes == 0 || height == 0 || stride < row_bytes {
        return None;
    }
    let mut rgba = Vec::with_capacity(row_bytes * usize::try_from(height).ok()?);
    for row in data.chunks(stride).take(usize::try_from(height).ok()?) {
        for pixel in row.get(..row_bytes)?.as_chunks::<4>().0 {
            let [_, red, green, blue] = u32::from_ne_bytes(*pixel).to_be_bytes();
            rgba.extend_from_slice(&[red, green, blue, 255]);
        }
    }
    RgbaImage::from_raw(width, height, rgba)
}

/// `area`, in the pixels of a view `view_width` wide, of that view's snapshot.
pub(super) fn crop(image: &RgbaImage, view_width: f32, area: Bounds<Pixels>) -> Result<RgbaImage, String> {
    let scale = image.width() as f32 / view_width;
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to the image")]
    let edge = |value: Pixels, limit: u32| (f32::from(value) * scale).round().clamp(0., limit as f32) as u32;
    let (left, top) = (edge(area.origin.x, image.width()), edge(area.origin.y, image.height()));
    let right = edge(area.origin.x + area.size.width, image.width());
    let bottom = edge(area.origin.y + area.size.height, image.height());
    if right <= left || bottom <= top {
        return Err("The area is not on the page.".to_owned());
    }
    Ok(image::imageops::crop_imm(image, left, top, right - left, bottom - top).to_image())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, px, size};

    #[test]
    fn cairo_pixels_become_opaque_rgba() {
        let pixel = |argb: u32| argb.to_ne_bytes();
        // Two pixels a row, padded to a 12-byte stride.
        let data: Vec<u8> = [pixel(0xff10_2030), pixel(0x0040_5060), [9; 4], pixel(0xff70_8090), pixel(0xff00_0000)]
            .into_iter()
            .chain([[9; 4]])
            .flatten()
            .collect();
        let image = opaque(&data, 2, 2, 12).expect("a 2×2 image");
        let pixels: Vec<[u8; 4]> = image.pixels().map(|p| p.0).collect();
        assert_eq!(pixels, [[0x10, 0x20, 0x30, 255], [0x40, 0x50, 0x60, 255], [0x70, 0x80, 0x90, 255], [0, 0, 0, 255]]);
        assert!(opaque(&data, 2, 3, 12).is_none(), "a row short");
        assert!(opaque(&data, 4, 1, 12).is_none(), "a stride narrower than the row");
    }

    #[test]
    fn crops_scale_from_the_view_to_the_snapshot() {
        let image = RgbaImage::from_fn(200, 100, |x, _| image::Rgba([u8::try_from(x / 2).unwrap_or(0), 0, 0, 255]));
        let area = Bounds { origin: point(px(10.), px(5.)), size: size(px(20.), px(10.)) };
        let cropped = crop(&image, 100., area).expect("on the page");
        assert_eq!(cropped.dimensions(), (40, 20), "the snapshot has 2 pixels per view pixel");
        assert_eq!(cropped.get_pixel(0, 0).0[0], 10);
        let beyond = Bounds { origin: point(px(90.), px(0.)), size: size(px(50.), px(200.)) };
        assert_eq!(crop(&image, 100., beyond).map(|c| c.dimensions()), Ok((20, 100)), "clipped to the page");
        let off = Bounds { origin: point(px(120.), px(0.)), size: size(px(10.), px(10.)) };
        assert!(crop(&image, 100., off).is_err());
    }
}
