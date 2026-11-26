//! Image compression for conversation embedding.

/// Why [`compress_image_for_conversation`] could not produce a
/// model-embeddable image.
#[derive(Debug, thiserror::Error)]
pub enum CompressImageError {
    /// Pre-decode pixel-area cap exceeded.
    #[error("image dimensions {width}x{height} exceed the {limit_pixels} pixel decode limit")]
    PixelLimitExceeded {
        width: u32,
        height: u32,
        limit_pixels: u64,
    },
    /// Lowest quality / smallest dimension still exceeded the byte cap.
    #[error("compressed image still exceeds the {0}-byte conversation payload cap")]
    PayloadCapExceeded(usize),
    /// No recognised magic bytes, or the IO reader failed before header read.
    #[error("image format could not be detected")]
    FormatDetectionFailed,
    /// Format detected but pixel decode failed (CRC, IDAT truncation, ...).
    #[error("image decode failed: {0}")]
    DecodeFailed(String),
}

/// Max base64 size for an image embedded in the conversation.
pub const MAX_IMAGE_PAYLOAD_BYTES: usize = 768 * 1024;

/// Raw-byte budget derived from [`MAX_IMAGE_PAYLOAD_BYTES`].
pub(crate) const MAX_IMAGE_RAW_BYTES: usize = MAX_IMAGE_PAYLOAD_BYTES * 3 / 4;

/// Total pixel budget (w*h) for images sent to the model; preserves the old
/// 1024x1024 square budget as an aspect-agnostic area.
pub(crate) const MAX_IMAGE_PIXELS: u64 = 1_048_576;

/// Max pixel dimension (width or height) for images sent to the model.
/// Model-agnostic side clamp; the area cap above is the operative budget.
pub(crate) const MAX_IMAGE_DIMENSION: u32 = 2000;

/// Floor dimension — re-encode gives up when `max_side` falls to or below this.
const MIN_IMAGE_DIMENSION: u32 = 128;

/// JPEG quality ladder for the read-file image compression path.
const READFILE_QUALITY_STEPS: &[u8] = &[85, 70, 50, 40];

/// Absolute upper bound on decoded pixel count before we refuse to decode.
/// Matches the model API's `MAX_IMAGE_PIXELS` ceiling (and the shell's
/// `MAX_VISION_TOTAL_PX`) so any photo the API would accept can be read and
/// downscaled — a 20-48 Mpx camera photo must not fail `read_file`. Images
/// above this are rejected by the API regardless.
const MAX_DECODE_PIXELS: u64 = 178_956_970;

/// Resize and compress an image so its base64 form stays under
/// [`MAX_IMAGE_PAYLOAD_BYTES`].
pub fn compress_image_for_conversation(
    raw_bytes: Vec<u8>,
    original_mime: String,
) -> Result<(Vec<u8>, String), CompressImageError> {
    compress_image_for_conversation_with_caps(
        raw_bytes,
        original_mime,
        MAX_IMAGE_RAW_BYTES,
        MAX_IMAGE_PAYLOAD_BYTES,
    )
}

/// [`compress_image_for_conversation`] off the async path, mapped to the
/// read tools' output: an embeddable
/// [`ImageContent`](crate::types::output::ImageContent) on success, or
/// [`ImageSizeError`](crate::types::output::ReadFileOutput::ImageSizeError)
/// with the model-visible reason.
pub async fn image_read_output(
    file_bytes: Vec<u8>,
    mime_type: String,
) -> crate::types::output::ReadFileOutput {
    use crate::types::output::{ImageContent, ReadFileOutput};
    use base64::Engine as _;

    let (encoded_bytes, mime) = match tokio::task::spawn_blocking(move || {
        compress_image_for_conversation(file_bytes, mime_type)
    })
    .await
    {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            return ReadFileOutput::ImageSizeError(format!(
                "Could not embed image in conversation: {e}"
            ));
        }
        Err(e) => {
            // Don't leak `JoinError::Display` (panic payload / paths)
            // into model-visible text.
            tracing::warn!(error = %e, "image compression task panicked");
            return ReadFileOutput::ImageSizeError(
                "Image compression failed; see logs.".to_owned(),
            );
        }
    };
    ReadFileOutput::ImageContent(ImageContent {
        data: base64::engine::general_purpose::STANDARD.encode(&encoded_bytes),
        mime_type: mime,
        annotations: None,
        uri: None,
        meta: None,
    })
}

/// Cap-parametrised body — exposed for tests that need to reach the
/// `PayloadCapExceeded` branch deterministically.
fn compress_image_for_conversation_with_caps(
    raw_bytes: Vec<u8>,
    original_mime: String,
    max_raw_bytes: usize,
    max_payload_bytes: usize,
) -> Result<(Vec<u8>, String), CompressImageError> {
    use crate::util::image_compress::{FilterType, ReEncodeParams, re_encode_under_limit};
    use image::ImageReader;
    use std::io::Cursor;

    // Engines only sample JPEG/PNG/WebP; PNG ICO/GIF/BMP/TIFF here. Before the
    // small-image early return so we keep the converted bytes.
    let (raw_bytes, original_mime) =
        match crate::util::image_validate::transcode_to_endpoint_png(&raw_bytes) {
            Some(Ok(png)) => (png, "image/png".to_string()),
            Some(Err(e)) => {
                tracing::warn!(
                    error = %e,
                    "non-native image format transcode to PNG failed; cannot embed image"
                );
                return Err(CompressImageError::DecodeFailed(format!(
                    "non-native image format transcode failed: {e}"
                )));
            }
            None => (raw_bytes, original_mime),
        };

    let params = ReEncodeParams {
        max_bytes: max_raw_bytes,
        max_side_px: MAX_IMAGE_DIMENSION,
        max_pixels: MAX_IMAGE_PIXELS,
        min_side_px: MIN_IMAGE_DIMENSION,
        quality_steps: READFILE_QUALITY_STEPS,
        filter: FilterType::Lanczos3,
    };

    // An image can be small in bytes yet still too large in pixels (e.g. a
    // flat-colour UI screenshot). Only skip re-encoding when it is within both
    // the byte budget and the params' dimension caps.
    let within_pixel_budget = ImageReader::new(Cursor::new(&raw_bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .is_none_or(|(w, h)| !params.exceeds_dimension_caps(w, h));

    // Pass through untouched only if the bytes are a structurally complete
    // JPEG/PNG/WebP — the formats the API accepts on the wire. Anything
    // else (truncated container, HEIC/PSD/unsniffable bytes) falls through
    // to the re-encode chain, which either emits valid endpoint bytes or
    // fails this call — never embedding a payload that would 400 on this
    // and every following turn.
    let passthrough_sendable = match image::guess_format(&raw_bytes) {
        Ok(
            format
            @ (image::ImageFormat::Jpeg | image::ImageFormat::Png | image::ImageFormat::WebP),
        ) => crate::util::image_validate::format_structurally_complete(format, &raw_bytes),
        _ => false,
    };

    if (raw_bytes.len() * 4).div_ceil(3) <= max_payload_bytes
        && within_pixel_budget
        && passthrough_sendable
    {
        return Ok((raw_bytes, original_mime));
    }

    let reader = match ImageReader::new(Cursor::new(&raw_bytes)).with_guessed_format() {
        Ok(r) => r,
        Err(_) => {
            tracing::warn!("image format detection failed; cannot compress oversized image");
            return Err(CompressImageError::FormatDetectionFailed);
        }
    };

    if reader.format().is_none() {
        tracing::warn!("image format unknown; cannot compress oversized image");
        return Err(CompressImageError::FormatDetectionFailed);
    }

    if let Ok((w, h)) = reader.into_dimensions()
        && (w as u64) * (h as u64) > MAX_DECODE_PIXELS
    {
        tracing::warn!(
            width = w,
            height = h,
            "image exceeds {MAX_DECODE_PIXELS} px decode limit; cannot compress"
        );
        return Err(CompressImageError::PixelLimitExceeded {
            width: w,
            height: h,
            limit_pixels: MAX_DECODE_PIXELS,
        });
    }

    let img = match ImageReader::new(Cursor::new(&raw_bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.decode().ok())
    {
        Some(img) => img,
        None => {
            tracing::warn!("image decode failed; cannot compress oversized image");
            return Err(CompressImageError::DecodeFailed(
                "pixel decode returned no image".into(),
            ));
        }
    };

    use crate::util::image_compress::ReEncodeError;
    let (buf, _w, _h, mime) = match re_encode_under_limit(&img, &params) {
        Ok(v) => v,
        Err(ReEncodeError::CouldNotFit { .. }) => {
            tracing::warn!("image re-encode could not fit under payload cap");
            return Err(CompressImageError::PayloadCapExceeded(max_payload_bytes));
        }
    };

    Ok((buf, mime.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_noisy_png(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgba};
        let img = ImageBuffer::from_fn(width, height, |x, y| {
            let seed = (x as u64).wrapping_mul(6364136223846793005)
                ^ (y as u64).wrapping_mul(1442695040888963407);
            let r = (seed & 0xFF) as u8;
            let g = ((seed >> 8) & 0xFF) as u8;
            let b = ((seed >> 16) & 0xFF) as u8;
            Rgba([r, g, b, 255u8])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    fn make_small_png(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgba};
        let img = ImageBuffer::from_pixel(width, height, Rgba([0u8, 0, 0, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn compress_small_image_returns_unchanged() {
        let png = make_small_png(16, 16);
        let (result, mime) =
            compress_image_for_conversation(png.clone(), "image/png".into()).unwrap();
        assert_eq!(result, png);
        assert_eq!(mime, "image/png");
    }

    /// A truncated small JPEG must not pass through raw; it falls through
    /// to re-encode, which emits structurally complete bytes from the
    /// decodable portion.
    #[test]
    fn compress_truncated_small_jpeg_re_encodes_to_valid_bytes() {
        use image::codecs::jpeg::JpegEncoder;
        use image::{DynamicImage, ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(200, 150, |x, y| {
            Rgb([(x ^ y) as u8, (x * 3) as u8, (y * 5) as u8])
        });
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 85)
            .encode_image(&DynamicImage::ImageRgb8(img))
            .unwrap();
        jpeg.truncate(jpeg.len() / 2);
        assert!(
            !crate::util::image_validate::jpeg_reaches_eoi(&jpeg),
            "precondition: input is structurally incomplete"
        );
        let (result, _mime) =
            compress_image_for_conversation(jpeg.clone(), "image/jpeg".into()).unwrap();
        assert_ne!(result, jpeg, "raw truncated bytes must not pass through");
        assert!(
            crate::util::image_validate::image_structurally_complete(&result),
            "output must be structurally complete"
        );
    }

    /// Small GIF must become PNG (not pass through as image/gif).
    #[test]
    fn compress_small_gif_becomes_png() {
        use image::{ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(24, 24, Rgba([1u8, 2, 3, 255]));
        let mut gif = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut gif), image::ImageFormat::Gif)
            .unwrap();
        let (result, mime) =
            compress_image_for_conversation(gif, "image/gif".into()).expect("gif compresses");
        assert_eq!(mime, "image/png");
        assert_eq!(
            image::guess_format(&result).unwrap(),
            image::ImageFormat::Png
        );
    }

    #[test]
    fn compress_large_noisy_image_picks_jpeg() {
        let png = make_noisy_png(2048, 1536);
        let b64_before = (png.len() * 4).div_ceil(3);
        assert!(
            b64_before > MAX_IMAGE_PAYLOAD_BYTES,
            "test image ({b64_before} B b64) must exceed the payload limit"
        );

        let (result, mime) = compress_image_for_conversation(png, "image/png".into()).unwrap();
        assert_eq!(mime, "image/jpeg");

        let b64_after = (result.len() * 4).div_ceil(3);
        assert!(
            b64_after <= MAX_IMAGE_PAYLOAD_BYTES,
            "compressed image ({b64_after} B b64) must fit within {MAX_IMAGE_PAYLOAD_BYTES} B"
        );
    }

    /// Flat-colour image: huge in pixels, tiny in bytes. It fits the byte
    /// budget yet exceeds the pixel-area cap, so it must still be downscaled.
    #[test]
    fn compress_large_dimensions_small_bytes_downscales() {
        let png = make_small_png(2048, 2600);
        let b64_before = (png.len() * 4).div_ceil(3);
        assert!(
            b64_before <= MAX_IMAGE_PAYLOAD_BYTES,
            "fixture ({b64_before} B b64) must be under the byte cap to exercise the pixel gate"
        );

        let (result, _mime) =
            compress_image_for_conversation(png, "image/png".into()).expect("downscale succeeds");

        let (w, h) = image::ImageReader::new(std::io::Cursor::new(&result))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert!(
            w <= MAX_IMAGE_DIMENSION && h <= MAX_IMAGE_DIMENSION,
            "expected <= {MAX_IMAGE_DIMENSION}px per side, got {w}x{h}"
        );
        assert!(
            u64::from(w) * u64::from(h) <= MAX_IMAGE_PIXELS,
            "expected <= {MAX_IMAGE_PIXELS} px total, got {w}x{h}"
        );
    }

    /// Wide image within the pixel-area budget passes through untouched —
    /// under the old 1024px side cap this was downscaled for no byte gain.
    #[test]
    fn compress_wide_image_under_area_budget_passes_through() {
        // 1600x600 = 0.96 Mpx <= MAX_IMAGE_PIXELS with both sides <= 2000.
        let png = make_small_png(1600, 600);
        let (result, mime) =
            compress_image_for_conversation(png.clone(), "image/png".into()).unwrap();
        assert_eq!(result, png, "within-budget image must not be re-encoded");
        assert_eq!(mime, "image/png");
    }

    /// A large screenshot read from disk lands within the pixel-area budget
    /// (~1403x747 for a 3438x1830 source), aspect preserved.
    #[test]
    fn compress_screenshot_respects_area_budget() {
        let png = make_small_png(3438, 1830);
        let (result, _mime) =
            compress_image_for_conversation(png, "image/png".into()).expect("downscale succeeds");
        let (w, h) = image::ImageReader::new(std::io::Cursor::new(&result))
            .with_guessed_format()
            .unwrap()
            .into_dimensions()
            .unwrap();
        assert!(
            u64::from(w) * u64::from(h) <= MAX_IMAGE_PIXELS,
            "expected <= {MAX_IMAGE_PIXELS} px total, got {w}x{h}"
        );
        let r_in = 3438.0 / 1830.0;
        let r_out = w as f64 / h as f64;
        assert!(
            (r_in - r_out).abs() < 0.05,
            "aspect ratio {r_in} -> {r_out} ({w}x{h})"
        );
    }

    /// Regression: a 25 Mpx camera-class photo (cf. a real 5184×3888 iPhone
    /// shot rejected under the old 16 Mpx cap) must compress, not error —
    /// the API accepts up to ~178.9 Mpx and we downscale before the wire.
    #[test]
    fn compress_camera_sized_photo_succeeds() {
        let png = make_noisy_png(5000, 5000);
        let (out, mime) = compress_image_for_conversation(png, "image/png".into())
            .expect("25 Mpx photo must compress");
        assert_eq!(mime, "image/jpeg");
        let (w, h, _) = crate::util::image_validate::validate_image_bytes(&out).unwrap();
        assert!(u64::from(w) * u64::from(h) <= MAX_IMAGE_PIXELS);
    }

    /// Above the API's own ceiling the decode is refused (the API would 400
    /// it regardless). SOF dims are patched — encoding a real >178 Mpx
    /// fixture is infeasible.
    #[test]
    fn compress_above_api_ceiling_returns_pixel_limit_exceeded() {
        use image::codecs::jpeg::JpegEncoder;
        use image::{DynamicImage, ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(64, 64, Rgb([7, 8, 9]));
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 85)
            .encode_image(&DynamicImage::ImageRgb8(img))
            .unwrap();
        let sof = jpeg
            .windows(2)
            .position(|w| w == [0xFF, 0xC0])
            .expect("baseline SOF0 present");
        // 16384 x 16384 = 268 Mpx, above the 178.9 Mpx ceiling.
        jpeg[sof + 5..sof + 9].copy_from_slice(&[0x40, 0x00, 0x40, 0x00]);
        let err = compress_image_for_conversation(jpeg, "image/jpeg".into()).unwrap_err();
        match err {
            CompressImageError::PixelLimitExceeded {
                width,
                height,
                limit_pixels,
            } => {
                assert_eq!((width, height), (16384, 16384));
                assert_eq!(limit_pixels, MAX_DECODE_PIXELS);
            }
            other => panic!("expected PixelLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn compress_small_undecodable_fails_closed() {
        let garbage = b"not an image at all".to_vec();
        let err = compress_image_for_conversation(garbage, "image/svg+xml".into())
            .expect_err("unsniffable bytes must not pass through raw");
        assert!(
            matches!(err, CompressImageError::FormatDetectionFailed),
            "got: {err:?}"
        );
    }

    #[test]
    fn compress_barely_over_limit_succeeds() {
        let png = make_noisy_png(1400, 1050);
        let b64_before = (png.len() * 4).div_ceil(3);
        if b64_before <= MAX_IMAGE_PAYLOAD_BYTES {
            return;
        }
        match compress_image_for_conversation(png, "image/png".into()) {
            Ok((result, _mime)) => {
                let b64_after = (result.len() * 4).div_ceil(3);
                assert!(b64_after <= MAX_IMAGE_PAYLOAD_BYTES);
            }
            Err(e) => panic!("expected compression to succeed for barely-over-limit image: {e}"),
        }
    }

    /// All-zero bytes → `FormatDetectionFailed`.
    #[test]
    fn compress_oversized_zero_bytes_returns_format_detection_failed() {
        let bytes = vec![0u8; MAX_IMAGE_PAYLOAD_BYTES + 4096];
        let err = compress_image_for_conversation(bytes, "image/png".into()).unwrap_err();
        assert!(
            matches!(err, CompressImageError::FormatDetectionFailed),
            "got {err:?}"
        );
    }

    /// Valid PNG header + corrupted IDAT → `DecodeFailed`.
    #[test]
    fn compress_oversized_corrupt_png_returns_decode_failed() {
        let mut png = make_noisy_png(1024, 1024);
        let tag = b"IDAT";
        let pos = png.windows(4).position(|w| w == tag).unwrap();
        for i in 0..512 {
            png[pos + 8 + i] ^= 0x5A;
        }
        let err = compress_image_for_conversation(png, "image/png".into()).unwrap_err();
        assert!(
            matches!(err, CompressImageError::DecodeFailed(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn payload_cap_exceeded_display_string_pinned() {
        let cap_err = CompressImageError::PayloadCapExceeded(768 * 1024);
        assert!(
            cap_err.to_string().contains("786432"),
            "rendered: {cap_err}"
        );
    }

    /// Tiny caps (~1.4 KB base64) on a 256×256 noise PNG exhaust the
    /// quality ladder and surface `PayloadCapExceeded`.
    #[test]
    fn payload_cap_exceeded_reached_through_production_path() {
        let png = make_noisy_png(256, 256);
        let err = compress_image_for_conversation_with_caps(png, "image/png".into(), 1024, 1400)
            .unwrap_err();
        match err {
            CompressImageError::PayloadCapExceeded(cap) => {
                assert_eq!(cap, 1400);
            }
            other => panic!("expected PayloadCapExceeded, got {other:?}"),
        }
    }
}
