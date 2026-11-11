//! Images prepared for attachment (`imageCompression.ts` `compressImageToByteLimit`,
//! `imageAttachmentSize.ts`): PNG and JPEG past the longest-edge limit are downscaled
//! in their own format, and anything still over the byte limit is re-encoded as JPEG,
//! shrinking quality and then size until it fits. The web prefers lossy WebP and falls
//! back to JPEG; the `image` crate only writes lossless WebP, so JPEG is the encoding here.
//! Everything here is CPU work for a background thread.

use std::io::Cursor;

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder as _, ImageError, ImageFormat, ImageReader, RgbImage};

/// `IMAGE_ATTACHMENT_MAX_EDGE`: Anthropic rejects larger images once a request holds more
/// than 20, and Claude Code forwards images sent into a running turn unchanged.
pub const MAX_EDGE: u32 = 2000;
/// `PROVIDER_SEND_TURN_MAX_IMAGE_BYTES`.
pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// `MAX_COMPRESSIBLE_SOURCE_BYTES`: larger sources are refused rather than decoded.
pub const MAX_SOURCE_BYTES: usize = 50 * 1024 * 1024;
const QUALITY_STEPS: [u8; 4] = [92, 85, 78, 68];
const FALLBACK_SCALE_STEPS: [f64; 2] = [0.75, 0.55];
const JPEG: &str = "image/jpeg";

/// `ImageCompressionFailureReason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    TooLarge,
    Unreadable,
}

impl Failure {
    /// The composer's error for `name` (`useComposerAttachmentIntake.ts`).
    pub fn message(self, name: &str) -> String {
        match self {
            Failure::TooLarge => format!("'{name}' is too large to attach, even after compression."),
            Failure::Unreadable => format!("'{name}' could not be read as an image."),
        }
    }
}

#[derive(Debug)]
pub struct Prepared {
    pub name: String,
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

/// `fitImageAttachmentSize`: the size that brings the longest edge to [`MAX_EDGE`], or
/// `None` when it already fits.
pub fn fit_size(width: u32, height: u32) -> Option<(u32, u32)> {
    let longest = width.max(height);
    if longest <= MAX_EDGE {
        return None;
    }
    let scale = f64::from(MAX_EDGE) / f64::from(longest);
    Some((scaled(width, scale).min(MAX_EDGE), scaled(height, scale).min(MAX_EDGE)))
}

/// `edge * scale`, rounded, and at least one pixel.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a positive scale of a `u32` edge, at most 1"
)]
fn scaled(edge: u32, scale: f64) -> u32 {
    ((f64::from(edge) * scale).round() as u32).max(1)
}

/// `prepareImageForAttachment` for GIF, JPEG, PNG and WebP `bytes` of type `mime`,
/// under `max_bytes`.
pub fn prepare(name: &str, mime: &'static str, bytes: Vec<u8>, max_bytes: usize) -> Result<Prepared, Failure> {
    let fitted = fit_to_max_edge(mime, &bytes);
    match fitted {
        Some(fitted) if fitted.len() <= max_bytes => Ok(Prepared { name: name.to_owned(), mime, bytes: fitted }),
        None if bytes.len() <= max_bytes => Ok(Prepared { name: name.to_owned(), mime, bytes }),
        _ if bytes.len() > MAX_SOURCE_BYTES => Err(Failure::TooLarge),
        fitted => {
            let source = fitted.as_deref().unwrap_or(&bytes);
            let bytes = reencode_within_budget(source, max_bytes / 3 * 4)?;
            Ok(Prepared { name: jpeg_name(name), mime: JPEG, bytes })
        }
    }
}

/// Decodes with the image's EXIF orientation applied, as `createImageBitmap` does.
fn decode(bytes: &[u8]) -> Result<DynamicImage, ImageError> {
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(ImageError::IoError)?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(image)
}

/// `fitImageToMaxEdge`: a PNG or JPEG over [`MAX_EDGE`], downscaled in its own format;
/// `None` when it fits already or can't be read.
fn fit_to_max_edge(mime: &str, bytes: &[u8]) -> Option<Vec<u8>> {
    let format = match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        _ => return None,
    };
    if bytes.len() > MAX_SOURCE_BYTES {
        return None;
    }
    let (width, height) = ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    fit_size(width, height)?;
    let image = decode(bytes).ok()?;
    let (width, height) = fit_size(image.width(), image.height())?;
    let image = image.resize_exact(width, height, FilterType::Lanczos3);
    match format {
        ImageFormat::Png => {
            let mut out = Cursor::new(Vec::new());
            image.write_to(&mut out, ImageFormat::Png).ok()?;
            Some(out.into_inner())
        }
        _ => encode_jpeg(&on_white(&image), QUALITY_STEPS[0]),
    }
}

/// `reencodeWithinBudget`: JPEG at falling quality, then smaller, until its data URL fits
/// `budget_chars`.
fn reencode_within_budget(bytes: &[u8], budget_chars: usize) -> Result<Vec<u8>, Failure> {
    let image = decode(bytes).map_err(|error| match error {
        ImageError::Limits(_) => Failure::TooLarge,
        _ => Failure::Unreadable,
    })?;
    let base = MAX_EDGE.min(image.width().max(image.height()));
    for scale in std::iter::once(1.).chain(FALLBACK_SCALE_STEPS) {
        let target = scaled(base, scale);
        if let Some(encoded) = encode_within_budget(&image, target, budget_chars) {
            return Ok(encoded);
        }
    }
    Err(Failure::TooLarge)
}

/// `encodeWithinBudget`, with JPEG's white matte under any transparency.
fn encode_within_budget(image: &DynamicImage, max_dimension: u32, budget_chars: usize) -> Option<Vec<u8>> {
    let scale = (f64::from(max_dimension) / f64::from(image.width().max(image.height()))).min(1.);
    let (width, height) = (scaled(image.width(), scale), scaled(image.height(), scale));
    let resized;
    let image = if (width, height) == (image.width(), image.height()) {
        image
    } else {
        resized = image.resize_exact(width, height, FilterType::Lanczos3);
        &resized
    };
    let rgb = on_white(image);
    QUALITY_STEPS
        .into_iter()
        .filter_map(|quality| encode_jpeg(&rgb, quality))
        .find(|encoded| data_url_length(JPEG, encoded.len()) <= budget_chars)
}

fn encode_jpeg(rgb: &RgbImage, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality).encode_image(rgb).ok()?;
    Some(out)
}

/// The image over white, since JPEG has no alpha.
fn on_white(image: &DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.to_rgb8();
    }
    let rgba = image.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let [r, g, b, a] = rgba.get_pixel(x, y).0;
        let over = |c: u8| {
            let mixed = (u16::from(c) * u16::from(a) + 255 * (255 - u16::from(a)) + 127) / 255;
            u8::try_from(mixed).unwrap_or(u8::MAX)
        };
        image::Rgb([over(r), over(g), over(b)])
    })
}

/// `dataUrlLength`: what the bytes cost as a base64 data URL.
fn data_url_length(mime: &str, size: usize) -> usize {
    format!("data:{mime};base64,").len() + 4 * size.div_ceil(3)
}

/// `fileNameForMimeType` for JPEG: the name with a `.jpg` extension.
fn jpeg_name(name: &str) -> String {
    let base = match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    };
    format!("{base}.jpg")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> Rgba<u8>) -> Vec<u8> {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_fn(width, height, pixel));
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// Pixels no codec can squeeze much, so byte limits bite.
    fn noise(x: u32, y: u32) -> Rgba<u8> {
        let h = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(2_246_822_519)).wrapping_mul(3_266_489_917);
        let [r, g, b, _] = h.to_be_bytes();
        Rgba([r, g, b, 255])
    }

    fn dimensions(bytes: &[u8]) -> (u32, u32) {
        ImageReader::new(Cursor::new(bytes)).with_guessed_format().unwrap().into_dimensions().unwrap()
    }

    #[test]
    fn the_longest_edge_fits_the_limit_like_the_web() {
        assert_eq!(fit_size(2000, 1500), None);
        assert_eq!(fit_size(4000, 3000), Some((2000, 1500)));
        assert_eq!(fit_size(1000, 4001), Some((500, 2000)));
        assert_eq!(fit_size(100_000, 10), Some((2000, 1)), "a sliver keeps one pixel");
    }

    #[test]
    fn a_small_image_is_sent_as_it_is() {
        let bytes = png(40, 30, noise);
        let prepared = prepare("shot.png", "image/png", bytes.clone(), MAX_IMAGE_BYTES).unwrap();
        assert_eq!((prepared.name.as_str(), prepared.mime), ("shot.png", "image/png"));
        assert_eq!(prepared.bytes, bytes);
    }

    #[test]
    fn a_wide_png_is_downscaled_and_stays_png() {
        let bytes = png(2500, 20, |_, _| Rgba([10, 20, 30, 255]));
        let prepared = prepare("wide.png", "image/png", bytes, MAX_IMAGE_BYTES).unwrap();
        assert_eq!((prepared.name.as_str(), prepared.mime), ("wide.png", "image/png"));
        assert_eq!(dimensions(&prepared.bytes), (2000, 16));
    }

    #[test]
    fn an_image_over_the_byte_limit_is_reencoded_as_jpeg_within_it() {
        let bytes = png(300, 300, noise);
        let limit = bytes.len() / 4;
        let prepared = prepare("big.screen.png", "image/png", bytes, limit).unwrap();
        assert_eq!((prepared.name.as_str(), prepared.mime), ("big.screen.jpg", "image/jpeg"));
        assert!(data_url_length(JPEG, prepared.bytes.len()) <= limit / 3 * 4);
        assert_eq!(&prepared.bytes[..2], &[0xFF, 0xD8], "a JPEG");
    }

    #[test]
    fn failures_say_what_went_wrong() {
        let unreadable = prepare("x.png", "image/png", vec![1; 64], 32);
        assert_eq!(unreadable.unwrap_err(), Failure::Unreadable);
        // Even the smallest fallback can't fit a budget this tight.
        let too_large = prepare("x.png", "image/png", png(64, 64, noise), 64);
        assert_eq!(too_large.unwrap_err(), Failure::TooLarge);
        assert_eq!(Failure::TooLarge.message("a.png"), "'a.png' is too large to attach, even after compression.");
        assert_eq!(Failure::Unreadable.message("a.png"), "'a.png' could not be read as an image.");
    }

    #[test]
    fn transparency_flattens_onto_white() {
        let clear = DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, Rgba([0, 0, 0, 0])));
        assert_eq!(on_white(&clear).get_pixel(0, 0).0, [255, 255, 255]);
    }
}
