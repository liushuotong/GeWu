//! Image normalization for `read_file` media payloads.
//!
//! Images travel to the model as base64 inside the tool result, so every byte
//! of an oversized photo is context the model never asked for. Media is
//! normalized before it leaves the tool, mirroring opencode's
//! `image.normalize`: fit the longest edge to the configured limit, prefer
//! lossless PNG, then fall back to progressively lower JPEG qualities until the
//! payload fits the configured base64 cap.

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat, ImageReader};

/// Default base64 cap for one image payload.
pub const DEFAULT_MAX_BASE64_BYTES: usize = 5 * 1024 * 1024;
/// Default longest edge an image may keep before it is downscaled.
pub const DEFAULT_MAX_DIMENSION: u32 = 2000;
/// JPEG qualities tried, best first, once lossless PNG exceeds the cap.
const JPEG_QUALITIES: [u8; 5] = [80, 85, 70, 55, 40];
/// Resize attempts before giving up; each step shrinks both edges by 25%.
const MAX_RESIZE_STEPS: usize = 32;

/// Caps applied to `read_file` images before they reach the model.
///
/// The defaults mirror opencode's `attachment.image` config (2000 px per edge,
/// 5 MiB of base64); server config can override any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    /// Downscale oversized images instead of refusing them.
    pub auto_resize: bool,
    /// Longest allowed edge, in pixels.
    pub max_width: u32,
    /// Longest allowed edge, in pixels.
    pub max_height: u32,
    /// Cap on the base64 payload handed to the model.
    pub max_base64_bytes: usize,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            auto_resize: true,
            max_width: DEFAULT_MAX_DIMENSION,
            max_height: DEFAULT_MAX_DIMENSION,
            max_base64_bytes: DEFAULT_MAX_BASE64_BYTES,
        }
    }
}

/// Why an image could not be normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NormalizeError {
    /// Unsupported or corrupt image data that is too large to pass through.
    Unreadable { limits: ImageLimits },
    /// Resizing is disabled and the image exceeds the limits.
    ResizeDisabled {
        width: u32,
        height: u32,
        limits: ImageLimits,
    },
    /// Resizing never got the payload under the byte cap.
    TooLarge {
        width: u32,
        height: u32,
        limits: ImageLimits,
    },
}

impl std::fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NormalizeError::Unreadable { limits } => write!(
                f,
                "it is not a decodable image and exceeds the {} image cap",
                human_bytes(limits.max_base64_bytes)
            ),
            NormalizeError::ResizeDisabled {
                width,
                height,
                limits,
            } => write!(
                f,
                "it is {width}x{height} and exceeds the {}x{} / {} image limits (image resizing is disabled)",
                limits.max_width,
                limits.max_height,
                human_bytes(limits.max_base64_bytes)
            ),
            NormalizeError::TooLarge {
                width,
                height,
                limits,
            } => write!(
                f,
                "it is {width}x{height} and could not be shrunk below the {}x{} / {} image limits",
                limits.max_width,
                limits.max_height,
                human_bytes(limits.max_base64_bytes)
            ),
        }
    }
}

/// Renders a byte count for a model-facing error message.
fn human_bytes(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / (1024 * 1024))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// An image ready to be base64-encoded into a media payload.
#[derive(Debug)]
pub(crate) struct NormalizedImage {
    pub data: Vec<u8>,
    pub mime: String,
}

/// Downscale and re-encode `data` only when it exceeds the caps.
///
/// Images already within both caps keep their original bytes, so lossless
/// originals are never re-encoded. Formats the decoder is not compiled for
/// (AVIF, EXR, HDR, DDS) pass through untouched while they fit the byte cap
/// and are refused otherwise, so enabling a decoder feature later only widens
/// what can be shrunk. Re-encoded animation keeps only its first frame, which
/// matches opencode's resizer.
pub(crate) fn normalize(
    data: &[u8],
    mime: &str,
    limits: ImageLimits,
) -> Result<NormalizedImage, NormalizeError> {
    let Some((format, decoded)) = decode(data) else {
        return if base64_len(data.len()) <= limits.max_base64_bytes {
            Ok(NormalizedImage {
                data: data.to_vec(),
                mime: mime.to_string(),
            })
        } else {
            Err(NormalizeError::Unreadable { limits })
        };
    };

    let (width, height) = (decoded.width(), decoded.height());
    if width <= limits.max_width
        && height <= limits.max_height
        && base64_len(data.len()) <= limits.max_base64_bytes
    {
        return Ok(NormalizedImage {
            data: data.to_vec(),
            mime: format.to_mime_type().to_string(),
        });
    }
    if !limits.auto_resize {
        return Err(NormalizeError::ResizeDisabled {
            width,
            height,
            limits,
        });
    }

    for (target_width, target_height) in resize_steps(width, height, limits) {
        let resized = decoded.resize_exact(target_width, target_height, FilterType::Lanczos3);
        if let Some(png) = encode_png(&resized)
            && base64_len(png.len()) <= limits.max_base64_bytes
        {
            return Ok(NormalizedImage {
                data: png,
                mime: "image/png".to_string(),
            });
        }
        for quality in JPEG_QUALITIES {
            let Some(jpeg) = encode_jpeg(&resized, quality) else {
                continue;
            };
            if base64_len(jpeg.len()) <= limits.max_base64_bytes {
                return Ok(NormalizedImage {
                    data: jpeg,
                    mime: "image/jpeg".to_string(),
                });
            }
        }
    }

    Err(NormalizeError::TooLarge {
        width,
        height,
        limits,
    })
}

fn decode(data: &[u8]) -> Option<(ImageFormat, DynamicImage)> {
    let format = image::guess_format(data).ok()?;
    let decoded = ImageReader::with_format(std::io::Cursor::new(data), format)
        .decode()
        .ok()?;
    Some((format, decoded))
}

/// Target dimensions to try, in order: fit the longest edge to the configured
/// limits, then shrink 25% per step. Stops when a step repeats a size already
/// tried, which is how a 1px edge terminates the loop.
fn resize_steps(width: u32, height: u32, limits: ImageLimits) -> Vec<(u32, u32)> {
    // A zero limit would make the scale infinite; treat it as one pixel.
    let max_width = limits.max_width.max(1);
    let max_height = limits.max_height.max(1);
    let scale = f64::min(
        1.0,
        f64::min(
            f64::from(max_width) / f64::from(width.max(1)),
            f64::from(max_height) / f64::from(height.max(1)),
        ),
    );
    let mut steps: Vec<(u32, u32)> = Vec::new();
    let mut next = (
        ((f64::from(width) * scale).round() as u32).max(1),
        ((f64::from(height) * scale).round() as u32).max(1),
    );
    while steps.len() < MAX_RESIZE_STEPS && !steps.contains(&next) {
        steps.push(next);
        next = (shrink(next.0), shrink(next.1));
    }
    steps
}

fn shrink(edge: u32) -> u32 {
    if edge <= 1 { 1 } else { (edge * 3 / 4).max(1) }
}

fn encode_png(image: &DynamicImage) -> Option<Vec<u8>> {
    let pixels = image.to_rgba8();
    let mut out = Vec::new();
    PngEncoder::new(&mut out)
        .write_image(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            ExtendedColorType::Rgba8,
        )
        .ok()?;
    Some(out)
}

fn encode_jpeg(image: &DynamicImage, quality: u8) -> Option<Vec<u8>> {
    let pixels = image.to_rgb8();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, quality)
        .write_image(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(out)
}

/// Length of `bytes` once base64-encoded, without building the string.
fn base64_len(bytes: usize) -> usize {
    bytes.div_ceil(3) * 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::webp::WebPEncoder;
    use image::{Rgba, RgbaImage};

    /// Encode `image` as PNG, the format `read_file` hands to `normalize`.
    fn png_bytes(image: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        PngEncoder::new(&mut out)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        out
    }

    fn blank(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba([200, 40, 90, 255]))
    }

    /// Deterministic high-entropy pixels: incompressible, so PNG stays large.
    fn noise(width: u32, height: u32) -> RgbaImage {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        RgbaImage::from_fn(width, height, |_, _| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let bytes = seed.to_le_bytes();
            Rgba([bytes[0], bytes[1], bytes[2], bytes[3]])
        })
    }

    #[test]
    fn image_within_limits_is_returned_untouched() {
        let bytes = png_bytes(&blank(10, 10));
        let normalized = normalize(&bytes, "image/png", ImageLimits::default()).unwrap();
        assert_eq!(normalized.data, bytes);
        assert_eq!(normalized.mime, "image/png");
    }

    #[test]
    fn oversized_dimensions_are_downscaled() {
        let bytes = png_bytes(&blank(3000, 400));
        let normalized = normalize(&bytes, "image/png", ImageLimits::default()).unwrap();
        assert_eq!(normalized.mime, "image/png");
        let decoded = image::load_from_memory(&normalized.data).unwrap();
        assert_eq!(decoded.width(), DEFAULT_MAX_DIMENSION);
        assert!(decoded.height() <= 400);
        assert!(base64_len(normalized.data.len()) <= DEFAULT_MAX_BASE64_BYTES);
    }

    #[test]
    fn jpeg_input_is_decoded_and_reencoded() {
        let pixels = DynamicImage::ImageRgba8(blank(2500, 100)).to_rgb8();
        let mut bytes = Vec::new();
        JpegEncoder::new_with_quality(&mut bytes, 90)
            .write_image(
                pixels.as_raw(),
                pixels.width(),
                pixels.height(),
                ExtendedColorType::Rgb8,
            )
            .unwrap();

        let normalized = normalize(&bytes, "image/jpeg", ImageLimits::default()).unwrap();
        assert_eq!(normalized.mime, "image/png");
        assert_eq!(
            image::load_from_memory(&normalized.data).unwrap().width(),
            DEFAULT_MAX_DIMENSION
        );
    }

    #[test]
    fn webp_input_is_decoded_and_reencoded() {
        let image = blank(2500, 100);
        let mut bytes = Vec::new();
        WebPEncoder::new_lossless(&mut bytes)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                ExtendedColorType::Rgba8,
            )
            .unwrap();

        let normalized = normalize(&bytes, "image/webp", ImageLimits::default()).unwrap();
        assert_eq!(normalized.mime, "image/png");
        assert_eq!(
            image::load_from_memory(&normalized.data).unwrap().width(),
            DEFAULT_MAX_DIMENSION
        );
    }

    #[test]
    fn incompressible_image_falls_back_to_jpeg() {
        let bytes = png_bytes(&noise(1200, 1200));
        assert!(
            base64_len(bytes.len()) > DEFAULT_MAX_BASE64_BYTES,
            "fixture must exceed the cap, got {}",
            bytes.len()
        );
        let normalized = normalize(&bytes, "image/png", ImageLimits::default()).unwrap();
        assert_eq!(normalized.mime, "image/jpeg");
        assert!(base64_len(normalized.data.len()) <= DEFAULT_MAX_BASE64_BYTES);
        assert_eq!(&normalized.data[..2], &[0xFF, 0xD8], "not a JPEG");
    }

    #[test]
    fn undecodable_payload_within_the_cap_passes_through() {
        let bytes = b"not an image at all".to_vec();
        let normalized = normalize(&bytes, "image/webp", ImageLimits::default()).unwrap();
        assert_eq!(normalized.data, bytes);
        assert_eq!(normalized.mime, "image/webp");
    }

    #[test]
    fn undecodable_payload_over_the_cap_is_refused() {
        let limits = ImageLimits::default();
        let bytes = vec![0u8; DEFAULT_MAX_BASE64_BYTES / 4 * 3 + 1];
        assert_eq!(
            normalize(&bytes, "image/webp", limits).unwrap_err(),
            NormalizeError::Unreadable { limits }
        );
    }

    #[test]
    fn resize_steps_fit_the_longest_edge_then_shrink() {
        let limits = ImageLimits::default();
        assert_eq!(resize_steps(4000, 800, limits).first(), Some(&(2000, 400)));
        assert_eq!(resize_steps(4000, 800, limits).get(1), Some(&(1500, 300)));
        // The chain always keeps shrinking: the byte cap may still be
        // exceeded even when the dimensions already fit.
        assert_eq!(resize_steps(10, 10, limits).last(), Some(&(1, 1)));
        let narrow = resize_steps(1, 5000, limits);
        assert_eq!(narrow.first(), Some(&(1, 2000)));
        assert!(narrow.iter().all(|&(width, _)| width == 1));
        let steps = resize_steps(9000, 9000, limits);
        assert!(steps.len() <= MAX_RESIZE_STEPS);
        assert_eq!(steps.last(), Some(&(1, 1)));
        assert!(steps.windows(2).all(|pair| pair[0].0 > pair[1].0));
    }

    #[test]
    fn configured_dimensions_are_honored() {
        let limits = ImageLimits {
            max_width: 500,
            max_height: 300,
            ..ImageLimits::default()
        };
        let bytes = png_bytes(&blank(1200, 200));
        let normalized = normalize(&bytes, "image/png", limits).unwrap();
        assert_eq!(
            image::load_from_memory(&normalized.data).unwrap().width(),
            500
        );

        // Already inside the narrower limits: untouched.
        let small = png_bytes(&blank(400, 200));
        assert_eq!(normalize(&small, "image/png", limits).unwrap().data, small);
    }

    #[test]
    fn configured_byte_cap_forces_reencoding() {
        // A blank image at default limits would stay PNG; a tight cap forces
        // the JPEG fallback.
        let bytes = png_bytes(&noise(600, 600));
        let limits = ImageLimits {
            max_base64_bytes: 200 * 1024,
            ..ImageLimits::default()
        };
        let normalized = normalize(&bytes, "image/png", limits).unwrap();
        assert_eq!(normalized.mime, "image/jpeg");
        assert!(base64_len(normalized.data.len()) <= limits.max_base64_bytes);
    }

    #[test]
    fn disabled_resize_refuses_oversized_images() {
        let limits = ImageLimits {
            auto_resize: false,
            ..ImageLimits::default()
        };
        let bytes = png_bytes(&blank(3000, 100));
        match normalize(&bytes, "image/png", limits) {
            Err(NormalizeError::ResizeDisabled {
                width,
                height,
                limits: reported,
            }) => {
                assert_eq!((width, height), (3000, 100));
                assert_eq!(reported, limits);
            }
            other => panic!("expected ResizeDisabled, got {other:?}"),
        }

        // within the limits it still passes through
        let small = png_bytes(&blank(10, 10));
        assert_eq!(normalize(&small, "image/png", limits).unwrap().data, small);
    }

    #[test]
    fn base64_len_matches_the_encoder() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        for size in [0usize, 1, 2, 3, 4, 5, 6, 17] {
            let bytes = vec![7u8; size];
            assert_eq!(base64_len(size), STANDARD.encode(&bytes).len());
        }
    }
}
