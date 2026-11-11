//! A recording's video: captured frames encoded to AV1 with rav1e (pure Rust, no assembly) and
//! muxed as `WebM`, which Chromium, Firefox and WebKit play.

use image::RgbaImage;
use rav1e::prelude::{
    ChromaSampling, ColorDescription, ColorPrimaries, Config, Context, EncoderConfig, EncoderStatus, FrameType,
    MatrixCoefficients, PixelRange, Rational, TransferCharacteristics,
};

use super::webm::{self, Block};

/// Frames over this many pixels are scaled down: rav1e without assembly falls behind the capture
/// on larger ones, and dropped frames read worse than a softer picture.
const MAX_PIXELS: u64 = 1280 * 800;
/// rav1e's fastest preset; real-time capture needs it.
const SPEED: u8 = 10;
/// The constant quantizer (0–255): screen text stays legible at a modest bitrate.
const QUANTIZER: usize = 100;

/// The size frames are encoded at: the capture's, scaled down to [`MAX_PIXELS`], in even pixels
/// as 4:2:0 chroma needs. `None` for a capture too small to encode.
pub(super) fn encode_size(width: u32, height: u32) -> Option<(u32, u32)> {
    let pixels = u64::from(width) * u64::from(height);
    let scale = if pixels > MAX_PIXELS { (MAX_PIXELS as f64 / pixels as f64).sqrt() } else { 1. };
    let even = |side: u32| {
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "scaled down from a u32")]
        let scaled = (f64::from(side) * scale) as u32;
        scaled & !1
    };
    let (width, height) = (even(width), even(height));
    (width >= 16 && height >= 16).then_some((width, height))
}

/// BT.709 limited-range 4:2:0 planes from RGBA pixels; `width` and `height` are even.
pub(super) fn rgba_to_yuv420(rgba: &[u8], width: usize, height: usize, planes: &mut [Vec<u8>; 3]) {
    let [y, u, v] = planes;
    y.resize(width * height, 0);
    u.resize(width * height / 4, 0);
    v.resize(width * height / 4, 0);
    let pixel = |x: usize, row: usize| {
        let at = (row * width + x) * 4;
        (i32::from(rgba[at]), i32::from(rgba[at + 1]), i32::from(rgba[at + 2]))
    };
    let clamp = |value: i32| u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX);
    for row in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let (mut red, mut green, mut blue) = (0, 0, 0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (r, g, b) = pixel(x + dx, row + dy);
                y[(row + dy) * width + x + dx] = clamp(((47 * r + 157 * g + 16 * b + 128) >> 8) + 16);
                (red, green, blue) = (red + r, green + g, blue + b);
            }
            let (r, g, b) = ((red + 2) / 4, (green + 2) / 4, (blue + 2) / 4);
            let chroma = (row / 2) * (width / 2) + x / 2;
            u[chroma] = clamp(((-26 * r - 86 * g + 112 * b + 128) >> 8) + 128);
            v[chroma] = clamp(((112 * r - 102 * g - 10 * b + 128) >> 8) + 128);
        }
    }
}

/// An AV1 encode in progress.
pub(super) struct Encoder {
    context: Context<u8>,
    width: u32,
    height: u32,
    planes: [Vec<u8>; 3],
    /// Each sent frame's time, by frame number.
    times: Vec<u64>,
    blocks: Vec<Block>,
    sequence_header: Option<Vec<u8>>,
}

impl Encoder {
    pub(super) fn new(width: u32, height: u32, frame_rate: u32) -> Result<Self, String> {
        let mut config = EncoderConfig::with_speed_preset(SPEED);
        (config.width, config.height) = (width as usize, height as usize);
        config.time_base = Rational::new(1, frame_rate.into());
        config.chroma_sampling = ChromaSampling::Cs420;
        config.pixel_range = PixelRange::Limited;
        config.color_description = Some(ColorDescription {
            color_primaries: ColorPrimaries::BT709,
            transfer_characteristics: TransferCharacteristics::SRGB,
            matrix_coefficients: MatrixCoefficients::BT709,
        });
        // Frames go out in order as they arrive, with a keyframe at least every two seconds to seek to.
        config.low_latency = true;
        config.set_key_frame_interval(1, u64::from(frame_rate) * 2);
        config.quantizer = QUANTIZER;
        config.bitrate = 0;
        let threads = std::thread::available_parallelism().map_or(2, |count| count.get().saturating_sub(1).clamp(1, 8));
        config.tiles = threads;
        let context = Config::new()
            .with_encoder_config(config)
            .with_threads(threads)
            .new_context()
            .map_err(|error| format!("Unable to start the video encoder: {error}"))?;
        Ok(Self {
            context,
            width,
            height,
            planes: Default::default(),
            times: Vec::new(),
            blocks: Vec::new(),
            sequence_header: None,
        })
    }

    /// Encodes one captured frame, scaled to the encode size when the capture's size changed.
    pub(super) fn push(&mut self, image: &RgbaImage, at_ms: u64) -> Result<(), String> {
        let resized;
        let image = if image.dimensions() == (self.width, self.height) {
            image
        } else {
            resized = image::imageops::resize(image, self.width, self.height, image::imageops::FilterType::Triangle);
            &resized
        };
        let (width, height) = (self.width as usize, self.height as usize);
        rgba_to_yuv420(image.as_raw(), width, height, &mut self.planes);
        let mut frame = self.context.new_frame();
        for (plane, (data, stride)) in frame.planes.iter_mut().zip([
            (&self.planes[0], width),
            (&self.planes[1], width / 2),
            (&self.planes[2], width / 2),
        ]) {
            plane.copy_from_raw_u8(data, stride, 1);
        }
        // Times only move forward, so blocks stay in order.
        self.times.push(at_ms.max(self.times.last().copied().unwrap_or(0)));
        match self.context.send_frame(frame) {
            Ok(()) => {}
            Err(error) => return Err(format!("Unable to encode the recording: {error}")),
        }
        self.drain()
    }

    fn drain(&mut self) -> Result<(), String> {
        loop {
            match self.context.receive_packet() {
                Ok(packet) => {
                    let frame = usize::try_from(packet.input_frameno).unwrap_or(usize::MAX);
                    let at_ms = self.times.get(frame).or(self.times.last()).copied().unwrap_or(0);
                    let key = packet.frame_type == FrameType::KEY;
                    if self.sequence_header.is_none() {
                        self.sequence_header = webm::obus(&packet.data).and_then(|units| {
                            units
                                .into_iter()
                                .find(|(kind, _)| *kind == webm::OBU_SEQUENCE_HEADER)
                                .map(|(_, bytes)| bytes.to_vec())
                        });
                    }
                    self.blocks.push(Block { at_ms, key, data: webm::strip_temporal_delimiters(packet.data) });
                }
                Err(EncoderStatus::Encoded) => {}
                Err(EncoderStatus::NeedMoreData | EncoderStatus::LimitReached) => return Ok(()),
                Err(error) => return Err(format!("Unable to encode the recording: {error}")),
            }
        }
    }

    /// Whether any frame was sent.
    pub(super) fn is_empty(&self) -> bool {
        self.times.is_empty()
    }

    /// Flushes the encoder and returns the `WebM` file, `duration_ms` long.
    pub(super) fn finish(mut self, duration_ms: u64) -> Result<Vec<u8>, String> {
        self.context.flush();
        self.drain()?;
        let mut codec_private = self.context.container_sequence_header();
        codec_private.extend(self.sequence_header.unwrap_or_default());
        let duration_ms = duration_ms.max(self.times.last().copied().unwrap_or(0));
        Ok(webm::mux(self.width, self.height, &codec_private, &self.blocks, duration_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_captures_scale_down_to_even_sides() {
        assert_eq!(encode_size(800, 600), Some((800, 600)));
        assert_eq!(encode_size(801, 601), Some((800, 600)), "odd sides drop a pixel");
        let (width, height) = encode_size(2560, 1600).expect("encodable");
        assert!(u64::from(width) * u64::from(height) <= MAX_PIXELS);
        assert_eq!((width, height), (1280, 800));
        assert_eq!(encode_size(10, 400), None, "too narrow to encode");
    }

    #[test]
    fn colours_map_to_limited_range_bt709() {
        let mut planes: [Vec<u8>; 3] = Default::default();
        let white = [255u8; 16];
        rgba_to_yuv420(&white, 2, 2, &mut planes);
        assert_eq!(planes, [vec![235; 4], vec![128], vec![128]]);
        let black = [0, 0, 0, 255].repeat(4);
        rgba_to_yuv420(&black, 2, 2, &mut planes);
        assert_eq!(planes, [vec![16; 4], vec![128], vec![128]]);
        let red = [255, 0, 0, 255].repeat(4);
        rgba_to_yuv420(&red, 2, 2, &mut planes);
        assert_eq!(planes, [vec![63; 4], vec![102], vec![240]]);
    }

    #[test]
    fn frames_encode_to_a_webm_with_one_block_each() {
        let mut encoder = Encoder::new(64, 64, 30).expect("an encoder");
        for (index, shade) in [0u8, 128, 255].into_iter().enumerate() {
            let image = RgbaImage::from_pixel(64, 64, image::Rgba([shade, 64, 255 - shade, 255]));
            encoder.push(&image, index as u64 * 33).expect("encoded");
        }
        // A frame of another size is scaled to the stream's.
        encoder.push(&RgbaImage::new(80, 50), 99).expect("encoded");
        encoder.context.flush();
        encoder.drain().expect("flushed");
        assert_eq!(encoder.blocks.iter().map(|block| block.at_ms).collect::<Vec<_>>(), [0, 33, 66, 99]);
        assert!(encoder.blocks[0].key, "the stream opens on a keyframe");
        let header = encoder.sequence_header.clone().expect("a sequence header from the first keyframe");
        assert_eq!((header[0] >> 3) & 0xF, webm::OBU_SEQUENCE_HEADER);
        let file = encoder.finish(132).expect("a file");
        assert!(file.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]));
        let codec_private = file.windows(2).position(|w| w == [0x63, 0xA2]).expect("codec private");
        assert_eq!(file[codec_private + 3], 0x81, "av1C marker and version");
        assert_eq!(
            file[codec_private + 3 + 4..codec_private + 3 + 4 + header.len()],
            header,
            "config OBUs follow av1C"
        );
    }
}
