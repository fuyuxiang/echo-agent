//! Shared image-bytes validation: sniff format, MIME allow-list, optional
//! full-pixel decode (catches CRC/IDAT corruption a header-only check
//! misses).

use image::{ImageError, ImageFormat};

/// `image`-crate error message substrings classified as `Truncated`.
const TRUNCATED_NEEDLES: &[&str] = &[
    "unexpected eof",
    "end of stream",
    "unexpected end of data",
    "unexpected end",
];

/// Why `validate_image_bytes_with` rejected the input.
#[derive(Debug, thiserror::Error)]
pub enum ImageValidateError {
    #[error("image bytes are empty")]
    Empty,
    /// Format could not be sniffed (no recognised magic bytes).
    #[error("unsupported or unrecognised image format")]
    Unsupported,
    /// Header read failed because the file is shorter than expected.
    #[error("image bytes are truncated")]
    Truncated,
    /// PNG IDAT/IHDR or equivalent chunk CRC mismatch.
    #[error("image has bad chunk CRC")]
    BadCrc,
    /// Sniffed format is not on the allow-list (e.g. SVG, x-icon, TGA).
    #[error("image format is not in the allow-list")]
    WrongFormat,
    #[error("image decode failed: {0}")]
    Decode(String),
}

fn validate_inner(
    bytes: &[u8],
    validate_full_decode: bool,
) -> Result<(u32, u32, ImageFormat), ImageValidateError> {
    if bytes.is_empty() {
        return Err(ImageValidateError::Empty);
    }
    let format = image::guess_format(bytes).map_err(classify_image_error)?;
    if validate_full_decode {
        // The JPEG decoder (zune-jpeg) pads missing scan data instead of
        // erroring, so a truncated JPEG passes a full pixel decode; the
        // API still rejects it. Enforce marker-structure completeness.
        if format == ImageFormat::Jpeg && !jpeg_reaches_eoi(bytes) {
            return Err(ImageValidateError::Truncated);
        }
        let img = image::load_from_memory(bytes).map_err(classify_image_error)?;
        return Ok((img.width(), img.height(), format));
    }
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| classify_io_kind(e.kind(), e.to_string()))?
        .into_dimensions()
        .map_err(classify_image_error)?;
    Ok((w, h, format))
}

fn allowlist_mime(format: ImageFormat) -> Result<&'static str, ImageValidateError> {
    match format {
        ImageFormat::Png => Ok("image/png"),
        ImageFormat::Jpeg => Ok("image/jpeg"),
        ImageFormat::Gif => Ok("image/gif"),
        ImageFormat::WebP => Ok("image/webp"),
        ImageFormat::Bmp => Ok("image/bmp"),
        ImageFormat::Tiff => Ok("image/tiff"),
        _ => Err(ImageValidateError::WrongFormat),
    }
}

/// Validate `bytes` decode as an allow-listed image
/// (PNG/JPEG/GIF/WebP/BMP/TIFF). When `validate_full_decode` is `true`,
/// runs a full pixel decode (catches CRC-corrupt PNGs); otherwise parses
/// only the header.
pub fn validate_image_bytes_with(
    bytes: &[u8],
    validate_full_decode: bool,
) -> Result<(u32, u32, &'static str), ImageValidateError> {
    let (w, h, format) = validate_inner(bytes, validate_full_decode)?;
    let mime = allowlist_mime(format)?;
    Ok((w, h, mime))
}

/// Default full-decode validation; catches magic-byte forgeries and
/// CRC-corrupt PNGs.
pub fn validate_image_bytes(bytes: &[u8]) -> Result<(u32, u32, &'static str), ImageValidateError> {
    validate_image_bytes_with(bytes, true)
}

/// Unrestricted dimension probe — accepts any format the `image` crate
/// can identify (TGA, ICO, PNM, HDR, Farbfeld, etc.). Inference-bound
/// paths MUST use [`validate_image_bytes_with`] (allow-list enforced).
pub fn validate_image_bytes_unrestricted(
    bytes: &[u8],
    validate_full_decode: bool,
) -> Result<(u32, u32, ImageFormat), ImageValidateError> {
    validate_inner(bytes, validate_full_decode)
}

/// Walk the JPEG marker structure and report whether a top-level EOI
/// (`FFD9`) is reached. Truncated files end inside a segment or the
/// entropy-coded stream and never reach it.
///
/// Structure-only (no pixel decode): length-prefixed segments are skipped
/// by their declared length — so an `FFD9` inside e.g. an EXIF thumbnail
/// does not count — and entropy-coded data after SOS is scanned with
/// byte-stuffing awareness (`FF00` literal, `FFD0`-`FFD7` restart markers).
/// Trailing bytes after the first top-level EOI (EXIF trailers, motion
/// photos) are ignored.
///
/// Stray non-`FF` bytes at marker positions (broken EXIF/APPn writers) are
/// skipped rather than rejected, mirroring libjpeg's `next_marker` — every
/// decoder in the accept chain (libjpeg/PIL, zune-jpeg, image-rs) reads
/// such files, so rejecting them here would drop images the API accepts.
/// Truncation detection is unaffected: a cut file still runs off the
/// buffer end without a top-level EOI.
///
/// Assumes Huffman byte-stuffing; arithmetic-coded entropy data (T.81
/// Annex D, which none of our decoders or the inference API accept) may
/// be false-rejected.
pub fn jpeg_reaches_eoi(bytes: &[u8]) -> bool {
    let n = bytes.len();
    if n < 2 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return false;
    }
    let mut i = 2;
    loop {
        // Find the next marker: skip stray garbage, then FF fill bytes.
        while i < n && bytes[i] != 0xFF {
            i += 1;
        }
        while i < n && bytes[i] == 0xFF {
            i += 1;
        }
        if i >= n {
            return false;
        }
        let marker = bytes[i];
        i += 1;
        match marker {
            // Not a marker (stuffed/stray `FF00`): keep scanning.
            0x00 => {}
            0xD9 => return true, // EOI
            // Standalone markers without a length field.
            0x01 | 0xD0..=0xD7 => {}
            0xDA => {
                // SOS: skip the length-prefixed header, then scan the
                // entropy-coded stream for the next real marker.
                let Some(next) = skip_segment(bytes, i) else {
                    return false;
                };
                i = next;
                loop {
                    while i < n && bytes[i] != 0xFF {
                        i += 1;
                    }
                    if i + 1 >= n {
                        return false;
                    }
                    match bytes[i + 1] {
                        // Byte-stuffed FF or fill byte: still entropy data.
                        0x00 => i += 2,
                        0xFF => i += 1,
                        // Restart marker: entropy data continues after it.
                        0xD0..=0xD7 => i += 2,
                        // Real marker terminates the scan; outer loop consumes it.
                        _ => break,
                    }
                }
            }
            _ => {
                let Some(next) = skip_segment(bytes, i) else {
                    return false;
                };
                i = next;
            }
        }
    }
}

/// Skip a length-prefixed JPEG segment starting at its 2-byte length field.
/// Returns the offset just past the segment, or `None` if it runs off the end.
fn skip_segment(bytes: &[u8], at: usize) -> Option<usize> {
    let len_bytes = bytes.get(at..at + 2)?;
    let len = usize::from(len_bytes[0]) << 8 | usize::from(len_bytes[1]);
    if len < 2 {
        return None;
    }
    let end = at.checked_add(len)?;
    (end <= bytes.len()).then_some(end)
}

/// Walk PNG chunks to an `IEND` chunk, verifying each chunk's CRC along
/// the way (no pixel decode). This is exactly the inference API's
/// per-request `validate_png_chunk_crcs` gate, so a pass here means the
/// server's PNG validation passes too — and a reject here is never a
/// false drop, because the server would reject the same bytes.
pub fn png_structurally_valid(bytes: &[u8]) -> bool {
    const PNG_SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    let n = bytes.len();
    if !bytes.starts_with(PNG_SIG) {
        return false;
    }
    let mut i = PNG_SIG.len();
    // Each chunk: 4-byte length, 4-byte type, data, 4-byte CRC (over
    // type + data).
    while let Some(header) = bytes.get(i..i + 8) {
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let is_iend = &header[4..8] == b"IEND";
        let data_start = i + 8;
        let Some(data_end) = data_start.checked_add(len) else {
            return false;
        };
        let Some(end) = data_end.checked_add(4) else {
            return false;
        };
        if end > n {
            return false;
        }
        let expected = u32::from_be_bytes([
            bytes[data_end],
            bytes[data_end + 1],
            bytes[data_end + 2],
            bytes[data_end + 3],
        ]);
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&bytes[i + 4..data_end]);
        if hasher.finalize() != expected {
            return false;
        }
        if is_iend {
            return true;
        }
        i = end;
    }
    false
}

/// WebP: the RIFF header declares the total payload size at bytes 4..8;
/// truncation leaves the buffer shorter than declared. An optional pad
/// byte (odd riff size) and trailing garbage are tolerated.
pub fn webp_riff_complete(bytes: &[u8]) -> bool {
    let Some(header) = bytes.get(..12) else {
        return false;
    };
    if &header[..4] != b"RIFF" || &header[8..12] != b"WEBP" {
        return false;
    }
    let riff_size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    riff_size
        .checked_add(8)
        .is_some_and(|end| end <= bytes.len())
}

/// Structural validity walk for a known `format`. Formats without a
/// dedicated walk return `true` — their decoders already reject
/// truncation strictly.
pub fn format_structurally_complete(format: ImageFormat, bytes: &[u8]) -> bool {
    match format {
        ImageFormat::Jpeg => jpeg_reaches_eoi(bytes),
        ImageFormat::Png => png_structurally_valid(bytes),
        ImageFormat::WebP => webp_riff_complete(bytes),
        _ => true,
    }
}

/// [`format_structurally_complete`] on the sniffed format; unsniffable
/// bytes fail — the inference API rejects them regardless.
pub fn image_structurally_complete(bytes: &[u8]) -> bool {
    match image::guess_format(bytes) {
        Ok(format) => format_structurally_complete(format, bytes),
        Err(_) => false,
    }
}

/// Decode-bomb guard: reject oversized inputs before full pixel decode.
const MAX_TRANSCODE_DECODE_PIXELS: u64 = 16_000_000;

/// Upscale tiny inputs so the PNG clears the backend `MIN_IMAGE_PIXELS`
/// (512) floor; a native PNG below it is rejected, not upscaled, server-side.
/// Matches the backend's `ICO_MIN_UPSCALE_DIMENSION`.
const TRANSCODE_MIN_UPSCALE_SIDE: u32 = 128;

/// Formats we re-encode as PNG before send. Engines only sample JPEG/PNG/WebP;
/// the backend rejects GIF/BMP/TIFF (it transcodes ICO server-side, not these).
fn is_client_transcode_format(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Ico | ImageFormat::Gif | ImageFormat::Bmp | ImageFormat::Tiff
    )
}

/// Whether `bytes` needs client-side PNG conversion (GIF/BMP/TIFF/ICO).
/// Engine-native JPG/PNG/WebP return `false`.
pub fn needs_endpoint_transcode(bytes: &[u8]) -> bool {
    matches!(
        image::guess_format(bytes),
        Ok(fmt) if is_client_transcode_format(fmt)
    )
}

/// Transcode ICO/GIF/BMP/TIFF to PNG. Returns `None` for already-native
/// (JPG/PNG/WebP) or unrecognised input (caller keeps the original bytes);
/// `Some(Err)` on decode failure. Tiny inputs are upscaled (see
/// [`TRANSCODE_MIN_UPSCALE_SIDE`]).
pub fn transcode_to_endpoint_png(bytes: &[u8]) -> Option<Result<Vec<u8>, ImageValidateError>> {
    let format = image::guess_format(bytes).ok()?;
    if !is_client_transcode_format(format) {
        return None;
    }
    Some(decode_to_png(bytes, format))
}

fn decode_to_png(bytes: &[u8], format: ImageFormat) -> Result<Vec<u8>, ImageValidateError> {
    // Probe dimensions from the header before decoding the full bitmap.
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| classify_io_kind(e.kind(), e.to_string()))?
        .into_dimensions()
        .map_err(classify_image_error)?;
    if (w as u64) * (h as u64) > MAX_TRANSCODE_DECODE_PIXELS {
        return Err(ImageValidateError::Decode(format!(
            "{format:?} {w}x{h} exceeds {MAX_TRANSCODE_DECODE_PIXELS} px decode limit"
        )));
    }
    let mut img = image::load_from_memory(bytes).map_err(classify_image_error)?;
    // Upscale the shorter side to TRANSCODE_MIN_UPSCALE_SIDE (aspect preserved),
    // but only if the post-resize pixel count still fits the decode budget: a
    // thin ultra-wide frame can pass the header check yet blow it after scaling.
    let shortest = img.width().min(img.height());
    if shortest > 0 && shortest < TRANSCODE_MIN_UPSCALE_SIDE {
        let scale = TRANSCODE_MIN_UPSCALE_SIDE as f32 / shortest as f32;
        let new_w = ((img.width() as f32 * scale).round() as u32).max(TRANSCODE_MIN_UPSCALE_SIDE);
        let new_h = ((img.height() as f32 * scale).round() as u32).max(TRANSCODE_MIN_UPSCALE_SIDE);
        let post_pixels = (new_w as u64).saturating_mul(new_h as u64);
        if post_pixels <= MAX_TRANSCODE_DECODE_PIXELS {
            img = img.resize_exact(new_w, new_h, image::imageops::FilterType::Lanczos3);
        }
        // else: keep original size rather than allocate an unbounded bitmap.
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), ImageFormat::Png)
        .map_err(classify_image_error)?;
    Ok(out)
}

fn classify_image_error(e: ImageError) -> ImageValidateError {
    match &e {
        ImageError::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            return ImageValidateError::Truncated;
        }
        ImageError::Unsupported(_) => return ImageValidateError::Unsupported,
        _ => {}
    }
    let msg = e.to_string();
    let lower = msg.to_ascii_lowercase();
    if lower.contains("crc") {
        ImageValidateError::BadCrc
    } else if TRUNCATED_NEEDLES.iter().any(|n| lower.contains(n)) {
        ImageValidateError::Truncated
    } else {
        ImageValidateError::Decode(msg)
    }
}

fn classify_io_kind(kind: std::io::ErrorKind, msg: String) -> ImageValidateError {
    if kind == std::io::ErrorKind::UnexpectedEof {
        return ImageValidateError::Truncated;
    }
    let lower = msg.to_ascii_lowercase();
    if TRUNCATED_NEEDLES.iter().any(|n| lower.contains(n)) {
        ImageValidateError::Truncated
    } else {
        ImageValidateError::Decode(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn png_bytes(w: u32, h: u32) -> Vec<u8> {
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_pixel(w, h, Rgba([1, 2, 3, 4]));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn jpeg_bytes(w: u32, h: u32) -> Vec<u8> {
        use image::codecs::jpeg::JpegEncoder;
        use image::{DynamicImage, ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(w, h, Rgb([10, 20, 30]));
        let mut buf = Vec::new();
        JpegEncoder::new_with_quality(&mut buf, 85)
            .encode_image(&DynamicImage::ImageRgb8(img))
            .unwrap();
        buf
    }

    fn bmp_bytes(w: u32, h: u32) -> Vec<u8> {
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_pixel(w, h, Rgba([1, 2, 3, 4]));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Bmp)
            .unwrap();
        buf
    }

    fn gif_bytes(w: u32, h: u32) -> Vec<u8> {
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_pixel(w, h, Rgba([1, 2, 3, 4]));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Gif)
            .unwrap();
        buf
    }

    #[test]
    fn valid_png_round_trips() {
        let bytes = png_bytes(8, 4);
        let (w, h, mime) = validate_image_bytes(&bytes).unwrap();
        assert_eq!((w, h), (8, 4));
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn valid_jpeg() {
        let bytes = jpeg_bytes(16, 12);
        let (w, h, mime) = validate_image_bytes(&bytes).unwrap();
        assert_eq!((w, h), (16, 12));
        assert_eq!(mime, "image/jpeg");
    }

    #[test]
    fn valid_bmp() {
        let bytes = bmp_bytes(5, 5);
        let (_, _, mime) = validate_image_bytes(&bytes).unwrap();
        assert_eq!(mime, "image/bmp");
    }

    #[test]
    fn valid_gif() {
        let bytes = gif_bytes(2, 2);
        let (_, _, mime) = validate_image_bytes_with(&bytes, true).unwrap();
        assert_eq!(mime, "image/gif");
    }

    #[test]
    fn header_only_valid_gif_and_bmp() {
        let gif = gif_bytes(3, 5);
        let (gw, gh, gmime) = validate_image_bytes_with(&gif, false).unwrap();
        assert_eq!((gw, gh, gmime), (3, 5, "image/gif"));
        let bmp = bmp_bytes(4, 7);
        let (bw, bh, bmime) = validate_image_bytes_with(&bmp, false).unwrap();
        assert_eq!((bw, bh, bmime), (4, 7, "image/bmp"));
    }

    /// WebP round-trip pins the `ImageFormat::WebP` match arm.
    #[test]
    fn valid_webp() {
        use image::{DynamicImage, ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(8, 6, Rgba([1u8, 2, 3, 4]));
        let mut buf = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut buf),
                image::ImageFormat::WebP,
            )
            .unwrap();
        let (w, h, mime) = validate_image_bytes_with(&buf, true).unwrap();
        assert_eq!((w, h), (8, 6));
        assert_eq!(mime, "image/webp");
    }

    /// TIFF round-trip pins the `ImageFormat::Tiff` match arm.
    #[test]
    fn valid_tiff() {
        use image::{ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(5, 5, Rgba([9u8, 8, 7, 6]));
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Tiff,
        )
        .unwrap();
        let (_, _, mime) = validate_image_bytes_with(&buf, true).unwrap();
        assert_eq!(mime, "image/tiff");
    }

    #[test]
    fn empty_input_rejected() {
        assert!(matches!(
            validate_image_bytes_with(&[], true),
            Err(ImageValidateError::Empty)
        ));
        assert!(matches!(
            validate_image_bytes_unrestricted(&[], true),
            Err(ImageValidateError::Empty)
        ));
    }

    /// Random non-image bytes → `Unsupported`.
    #[test]
    fn random_bytes_rejected_as_unsupported() {
        let bytes = b"not an image at all, just plain text data";
        let err = validate_image_bytes_with(bytes, true).unwrap_err();
        assert!(
            matches!(err, ImageValidateError::Unsupported),
            "expected Unsupported, got: {err:?}"
        );
    }

    /// PNG magic + garbage tail → header passes, full-decode → `Decode(_)`.
    #[test]
    fn png_magic_with_garbage_tail_rejected() {
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(b"this is not really a PNG payload at all");
        let err = validate_image_bytes_with(&bytes, true).unwrap_err();
        assert!(
            matches!(err, ImageValidateError::Decode(_)),
            "expected Decode, got: {err:?}"
        );
    }

    /// Truncated PNG → `Truncated`.
    #[test]
    fn truncated_png_pinned_as_truncated() {
        let mut bytes = png_bytes(64, 64);
        bytes.truncate(bytes.len() / 2);
        let err = validate_image_bytes_with(&bytes, true).unwrap_err();
        assert!(
            matches!(err, ImageValidateError::Truncated),
            "expected Truncated, got: {err:?}"
        );
    }

    /// Bit-flipped IDAT passes header-only but fails full decode as
    /// `Decode(_)` (`image` crate reports deflate-stream, not "crc").
    #[test]
    fn header_only_accepts_full_decode_rejects_idat_corrupt_png() {
        let mut bytes = png_bytes(32, 32);
        let tag = b"IDAT";
        let pos = bytes
            .windows(4)
            .position(|w| w == tag)
            .expect("IDAT chunk present");
        bytes[pos + 8] ^= 0xFF;
        let r_header = validate_image_bytes_with(&bytes, false);
        assert!(r_header.is_ok(), "header-only must admit");
        let r_full = validate_image_bytes_with(&bytes, true).unwrap_err();
        assert!(
            matches!(r_full, ImageValidateError::Decode(_)),
            "expected Decode, got: {r_full:?}"
        );
    }

    /// `BadCrc` classifier branch — pins the "crc" needle.
    #[test]
    fn classify_image_error_maps_crc_error_to_bad_crc() {
        use image::ImageError;
        use image::error::{DecodingError, ImageFormatHint};
        let hint = ImageFormatHint::Exact(image::ImageFormat::Png);
        let err = ImageError::Decoding(DecodingError::new(
            hint,
            "Format error decoding Png: chunk crc mismatch",
        ));
        let classified = classify_image_error(err);
        assert!(
            matches!(classified, ImageValidateError::BadCrc),
            "expected BadCrc, got: {classified:?}"
        );
    }

    /// `Truncated` classifier branch via `IoError(UnexpectedEof)`.
    #[test]
    fn classify_image_error_maps_io_unexpected_eof_to_truncated() {
        let io = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "short read");
        let err = image::ImageError::IoError(io);
        let classified = classify_image_error(err);
        assert!(
            matches!(classified, ImageValidateError::Truncated),
            "expected Truncated, got: {classified:?}"
        );
    }

    /// Minimal valid ICO wrapping one PNG frame. The `image` crate's `ico`
    /// feature is enabled, so `guess_format` returns `ImageFormat::Ico` —
    /// which is intentionally NOT on the inference-side allow-list.
    fn ico_with_png_frame() -> Vec<u8> {
        echo_agent_test_utils::image::ico_with_png_frame(&png_bytes(8, 8), 8, 8)
    }

    /// ICO → `WrongFormat` (recognised format, not allow-listed).
    #[test]
    fn ico_rejected_as_wrong_format() {
        let buf = ico_with_png_frame();
        let err = validate_image_bytes_with(&buf, false).unwrap_err();
        assert!(
            matches!(err, ImageValidateError::WrongFormat),
            "got: {err:?}"
        );
    }

    /// Unrestricted variant accepts ICO so prompt-side viewer paths work.
    #[test]
    fn unrestricted_accepts_ico() {
        let buf = ico_with_png_frame();
        let (w, h, fmt) = validate_image_bytes_unrestricted(&buf, false).unwrap();
        assert_eq!((w, h), (8, 8));
        assert_eq!(fmt, image::ImageFormat::Ico);
    }

    #[test]
    fn unrestricted_accepts_png() {
        let bytes = png_bytes(8, 8);
        let (w, h, fmt) = validate_image_bytes_unrestricted(&bytes, true).unwrap();
        assert_eq!((w, h), (8, 8));
        assert_eq!(fmt, image::ImageFormat::Png);
    }

    /// Engine-native formats must not be flagged for client-side PNG conversion.
    #[test]
    fn needs_endpoint_transcode_false_for_png_jpeg_webp() {
        assert!(!needs_endpoint_transcode(&png_bytes(4, 4)));
        assert!(!needs_endpoint_transcode(&jpeg_bytes(8, 8)));
        use image::{DynamicImage, ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(4, 4, Rgba([1u8, 2, 3, 4]));
        let mut webp = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut webp),
                image::ImageFormat::WebP,
            )
            .unwrap();
        assert!(!needs_endpoint_transcode(&webp));
        assert!(!needs_endpoint_transcode(b"not an image"));
    }

    /// GIF/BMP/TIFF/ICO need client-side PNG conversion.
    #[test]
    fn needs_endpoint_transcode_true_for_gif_bmp_tiff_ico() {
        assert!(needs_endpoint_transcode(&gif_bytes(4, 4)));
        assert!(needs_endpoint_transcode(&bmp_bytes(4, 4)));
        let mut tiff = Vec::new();
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(4, 4, Rgba([1u8, 2, 3, 4]));
        img.write_to(
            &mut std::io::Cursor::new(&mut tiff),
            image::ImageFormat::Tiff,
        )
        .unwrap();
        assert!(needs_endpoint_transcode(&tiff));
        assert!(needs_endpoint_transcode(&ico_with_png_frame()));
    }

    /// GIF/BMP/TIFF survive as real PNG bytes after client transcode.
    #[test]
    fn transcode_to_endpoint_png_converts_gif_bmp_tiff() {
        for bytes in [gif_bytes(6, 4), bmp_bytes(5, 5)] {
            let png = transcode_to_endpoint_png(&bytes)
                .expect("must need transcode")
                .expect("transcode succeeds");
            let (w, h, mime) = validate_image_bytes(&png).unwrap();
            assert_eq!(mime, "image/png");
            assert!(w > 0 && h > 0);
        }
        assert!(
            transcode_to_endpoint_png(&png_bytes(4, 4)).is_none(),
            "PNG is already engine-native"
        );
    }

    /// Tiny GIF must upscale so it clears the inference backend's 512-pixel floor after we PNG.
    #[test]
    fn transcode_to_endpoint_png_upscales_tiny_gif() {
        let png = transcode_to_endpoint_png(&gif_bytes(16, 16))
            .expect("needs transcode")
            .expect("decode ok");
        let (w, h, mime) = validate_image_bytes(&png).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(
            (w, h),
            (TRANSCODE_MIN_UPSCALE_SIDE, TRANSCODE_MIN_UPSCALE_SIDE)
        );
        assert!(
            (w as u64) * (h as u64) >= 512,
            "must clear IC MIN_IMAGE_PIXELS=512, got {w}x{h}"
        );
    }

    /// Already-large inputs are not upscaled further.
    #[test]
    fn transcode_to_endpoint_png_leaves_large_gif_dimensions() {
        let png = transcode_to_endpoint_png(&gif_bytes(200, 150))
            .expect("needs transcode")
            .expect("decode ok");
        let (w, h, _) = validate_image_bytes(&png).unwrap();
        assert_eq!((w, h), (200, 150));
    }

    /// Extreme aspect ratio: header under the pixel budget, but favicon-style
    /// upscale would exceed MAX_TRANSCODE_DECODE_PIXELS — must not resize.
    #[test]
    fn transcode_to_endpoint_png_skips_upscale_when_post_resize_exceeds_budget() {
        // 8000×2 = 16_000 px (under 16M). Short side 2 needs ×64 → 512_000×128 =
        // 65_536_000 px (> 16M). Upscale must be skipped.
        let png = transcode_to_endpoint_png(&bmp_bytes(8000, 2))
            .expect("needs transcode")
            .expect("decode ok");
        let (w, h, mime) = validate_image_bytes(&png).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(
            (w, h),
            (8000, 2),
            "must keep original size when upscale would OOM"
        );
    }

    /// ICO still transcodes to PNG through the general path.
    #[test]
    fn transcode_to_endpoint_png_handles_ico() {
        let ico = ico_with_png_frame();
        let png = transcode_to_endpoint_png(&ico)
            .expect("needs transcode")
            .expect("decode ok");
        let (_, _, mime) = validate_image_bytes(&png).unwrap();
        assert_eq!(mime, "image/png");
    }

    /// Corrupt GIF must surface as an error, not as `None` (caller would otherwise
    /// pass the broken bytes through and trip the inference backend with a 400).
    #[test]
    fn transcode_to_endpoint_png_corrupt_gif_is_err_not_none() {
        let mut gif = gif_bytes(8, 8);
        // Truncate past the header so guess_format still says Gif but decode fails.
        gif.truncate(20.min(gif.len()));
        if !matches!(image::guess_format(&gif), Ok(ImageFormat::Gif)) {
            // If truncation lost the magic, keep GIF89a so we still enter the GIF arm.
            let mut with_magic = b"GIF89a".to_vec();
            with_magic.extend_from_slice(&gif);
            gif = with_magic;
        }
        let result = transcode_to_endpoint_png(&gif);
        assert!(
            matches!(result, Some(Err(_))),
            "corrupt GIF must be Some(Err), got {result:?}"
        );
    }

    // ─── jpeg_reaches_eoi / png_structurally_valid structural walks ─────────

    /// Noisy JPEG whose entropy stream is long enough to cut mid-scan.
    fn noisy_jpeg(w: u32, h: u32) -> Vec<u8> {
        use image::codecs::jpeg::JpegEncoder;
        use image::{DynamicImage, ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(w, h, |x, y| {
            Rgb([
                (x.wrapping_mul(13) ^ y) as u8,
                (x.wrapping_mul(7).wrapping_add(y * 3)) as u8,
                (x.wrapping_add(y).wrapping_mul(11)) as u8,
            ])
        });
        let mut buf = Vec::new();
        JpegEncoder::new_with_quality(&mut buf, 85)
            .encode_image(&DynamicImage::ImageRgb8(img))
            .unwrap();
        buf
    }

    #[test]
    fn jpeg_reaches_eoi_valid_jpeg_true() {
        assert!(jpeg_reaches_eoi(&noisy_jpeg(64, 64)));
        assert!(jpeg_reaches_eoi(&jpeg_bytes(16, 12)));
    }

    /// The production failure shape: entropy stream cut at an arbitrary
    /// point (a data URI sliced by tool-output truncation). zune-jpeg
    /// decodes these leniently, so only the marker walk catches them.
    #[test]
    fn jpeg_reaches_eoi_truncated_false_at_any_cut() {
        let jpeg = noisy_jpeg(128, 96);
        for frac in [3usize, 5, 7, 9] {
            let mut t = jpeg.clone();
            t.truncate(jpeg.len() * frac / 10);
            assert!(!jpeg_reaches_eoi(&t), "cut at {frac}0% must not reach EOI");
        }
    }

    /// The lenient decoder accepts what the walk rejects — the disagreement
    /// this fix exists for.
    #[test]
    fn truncated_jpeg_decodes_leniently_but_fails_walk() {
        let mut t = noisy_jpeg(128, 96);
        t.truncate(t.len() / 2);
        assert!(image::load_from_memory(&t).is_ok(), "zune-jpeg is lenient");
        assert!(!jpeg_reaches_eoi(&t));
    }

    /// Trailing bytes after EOI (EXIF trailers, motion photos) are legal.
    #[test]
    fn jpeg_reaches_eoi_trailing_garbage_true() {
        let mut jpeg = noisy_jpeg(32, 32);
        jpeg.extend_from_slice(b"trailing application data \xFF\xD8 junk");
        assert!(jpeg_reaches_eoi(&jpeg));
    }

    /// An EOI inside a length-prefixed segment (e.g. an EXIF thumbnail in
    /// APP1) must not count as the top-level terminator.
    #[test]
    fn jpeg_reaches_eoi_ignores_eoi_inside_app_segment() {
        let thumb = noisy_jpeg(16, 16);
        // SOI + APP1 wrapping a complete thumbnail JPEG, then nothing.
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE1];
        let seg_len = thumb.len() + 2;
        bytes.extend_from_slice(&[(seg_len >> 8) as u8, (seg_len & 0xFF) as u8]);
        bytes.extend_from_slice(&thumb);
        assert!(
            !jpeg_reaches_eoi(&bytes),
            "thumbnail EOI inside APP1 must not terminate the walk"
        );
        // Appending a real EOI completes it.
        bytes.extend_from_slice(&[0xFF, 0xD9]);
        assert!(jpeg_reaches_eoi(&bytes));
    }

    #[test]
    fn jpeg_reaches_eoi_rejects_degenerate_inputs() {
        assert!(!jpeg_reaches_eoi(&[]));
        assert!(!jpeg_reaches_eoi(&[0xFF]));
        assert!(!jpeg_reaches_eoi(&[0xFF, 0xD8])); // bare SOI
        assert!(!jpeg_reaches_eoi(b"not a jpeg"));
        // Segment length running past the end of the buffer.
        assert!(!jpeg_reaches_eoi(&[
            0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xFF, 0x00
        ]));
        // Zero segment length would loop forever if unchecked.
        assert!(!jpeg_reaches_eoi(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x00]));
    }

    /// Restart-marker arm: hand-assembled structure (image-rs's encoder
    /// never emits DRI/RST). Entropy data with byte-stuffed FF00 and
    /// FFD0/FFD1 restarts must scan through to the EOI.
    #[test]
    fn jpeg_reaches_eoi_scans_through_restart_markers() {
        #[rustfmt::skip]
        let full: Vec<u8> = vec![
            0xFF, 0xD8,                                     // SOI
            0xFF, 0xDD, 0x00, 0x04, 0x00, 0x02,             // DRI, interval 2
            0xFF, 0xDA, 0x00, 0x08, 1, 2, 3, 4, 5, 6,       // SOS header
            0x12, 0x34, 0xFF, 0x00, 0x56,                   // entropy + stuffed FF
            0xFF, 0xD0, 0x78, 0x9A,                         // RST0, more entropy
            0xFF, 0xD1, 0xBC,                               // RST1, more entropy
            0xFF, 0xD9,                                     // EOI
        ];
        assert!(jpeg_reaches_eoi(&full));
        for cut in 3..full.len() {
            assert!(
                !jpeg_reaches_eoi(&full[..cut]),
                "cut at {cut} must not reach EOI"
            );
        }
    }

    /// Progressive (multi-SOS) arm: a second SOS after the first scan's
    /// entropy data must be consumed as a segment, not end the walk.
    #[test]
    fn jpeg_reaches_eoi_walks_multiple_scans() {
        #[rustfmt::skip]
        let full: Vec<u8> = vec![
            0xFF, 0xD8,                                     // SOI
            0xFF, 0xDA, 0x00, 0x08, 1, 2, 3, 4, 5, 6,       // SOS 1
            0x11, 0x22, 0xFF, 0x00, 0x33,                   // entropy 1
            0xFF, 0xDA, 0x00, 0x08, 1, 2, 3, 4, 5, 6,       // SOS 2
            0x44, 0x55,                                     // entropy 2
            0xFF, 0xD9,                                     // EOI
        ];
        assert!(jpeg_reaches_eoi(&full));
        let cut = full.len() - 3; // inside entropy 2
        assert!(!jpeg_reaches_eoi(&full[..cut]));
    }

    #[test]
    fn webp_riff_complete_valid_true_truncated_false() {
        use image::{DynamicImage, ImageBuffer, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(16, 16, Rgba([5u8, 6, 7, 255]));
        let mut webp = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut webp),
                image::ImageFormat::WebP,
            )
            .unwrap();
        assert!(webp_riff_complete(&webp));
        // Trailing garbage after the declared size is tolerated.
        let mut padded = webp.clone();
        padded.extend_from_slice(b"trailer");
        assert!(webp_riff_complete(&padded));
        let mut t = webp.clone();
        t.truncate(t.len() / 2);
        assert!(!webp_riff_complete(&t));
        assert!(!webp_riff_complete(b"RIFF"));
        assert!(!webp_riff_complete(b"not a webp at all"));
        assert!(format_structurally_complete(ImageFormat::WebP, &webp));
        t.truncate(12);
        assert!(!format_structurally_complete(ImageFormat::WebP, &t));
    }

    #[test]
    fn png_structurally_valid_valid_true_truncated_false() {
        let png = png_bytes(32, 32);
        assert!(png_structurally_valid(&png));
        let mut t = png.clone();
        t.truncate(t.len() / 2);
        assert!(!png_structurally_valid(&t));
        // Cutting inside the IEND trailer itself.
        let mut t2 = png.clone();
        t2.truncate(png.len() - 2);
        assert!(!png_structurally_valid(&t2));
        assert!(!png_structurally_valid(&[]));
    }

    /// The server verifies every chunk CRC per request; a bit-flipped but
    /// well-terminated PNG 400s there, so the walk must reject it too.
    #[test]
    fn png_structurally_valid_rejects_corrupt_crc() {
        let png = png_bytes(32, 32);
        // Flip one byte inside the IDAT payload (past sig + IHDR chunk and
        // the IDAT header, before the trailing IEND + CRCs).
        let mut corrupt = png.clone();
        let idat = corrupt
            .windows(4)
            .position(|w| w == b"IDAT")
            .expect("IDAT present");
        corrupt[idat + 6] ^= 0xFF;
        assert!(!png_structurally_valid(&corrupt));
        assert!(png_structurally_valid(&png));
    }

    /// Stray non-FF bytes between marker segments (broken EXIF/APPn
    /// writers): every decoder in the accept chain skips them, so the walk
    /// must too — rejecting here would irreversibly strip working images
    /// at session load.
    #[test]
    fn jpeg_reaches_eoi_skips_stray_inter_segment_bytes() {
        let jpeg = noisy_jpeg(64, 64);
        // Splice garbage right after the APP0 segment (SOI + APP0 header
        // at offset 2; APP0 length at offset 4).
        assert_eq!(&jpeg[2..4], &[0xFF, 0xE0], "encoder emits APP0 first");
        let app0_len = usize::from(jpeg[4]) << 8 | usize::from(jpeg[5]);
        let after_app0 = 4 + app0_len;
        let garbage_runs: [&[u8]; 3] = [&[0x12], &[0x12, 0x34], &[1, 2, 3, 4, 5, 6, 7, 8]];
        for garbage in garbage_runs {
            let mut spliced = jpeg[..after_app0].to_vec();
            spliced.extend_from_slice(garbage);
            spliced.extend_from_slice(&jpeg[after_app0..]);
            assert!(
                image::load_from_memory(&spliced).is_ok(),
                "precondition: decoders accept stray inter-segment bytes"
            );
            assert!(
                jpeg_reaches_eoi(&spliced),
                "walk must skip {} stray bytes like libjpeg's next_marker",
                garbage.len()
            );
            // Truncation detection is unaffected by the lenience.
            let mut cut = spliced.clone();
            cut.truncate(cut.len() / 2);
            assert!(!jpeg_reaches_eoi(&cut));
        }
        // A stray FF00 pair at a marker position is skipped, not treated
        // as a marker.
        let mut stuffed = jpeg[..after_app0].to_vec();
        stuffed.extend_from_slice(&[0xFF, 0x00]);
        stuffed.extend_from_slice(&jpeg[after_app0..]);
        assert!(jpeg_reaches_eoi(&stuffed));
    }

    #[test]
    fn image_structurally_complete_dispatches_by_format() {
        assert!(image_structurally_complete(&noisy_jpeg(16, 16)));
        assert!(image_structurally_complete(&png_bytes(8, 8)));
        let mut t = noisy_jpeg(64, 64);
        t.truncate(t.len() / 2);
        assert!(!image_structurally_complete(&t));
        // Formats without a walk (strict decoders) pass through.
        assert!(image_structurally_complete(&gif_bytes(4, 4)));
        // Unsniffable bytes fail.
        assert!(!image_structurally_complete(b"plain text"));
    }

    /// The seam the session pipeline relies on: full-decode validation must
    /// reject a truncated JPEG even though the pixel decode succeeds.
    #[test]
    fn validate_image_bytes_rejects_truncated_jpeg() {
        let mut t = noisy_jpeg(128, 96);
        t.truncate(t.len() / 2);
        let err = validate_image_bytes(&t).unwrap_err();
        assert!(
            matches!(err, ImageValidateError::Truncated),
            "expected Truncated, got: {err:?}"
        );
        // Header-only probing stays permissive (used by non-inference paths).
        assert!(validate_image_bytes_with(&t, false).is_ok());
    }

    /// Pin every `Display` string so a future `#[error("...")]` typo is caught.
    #[test]
    fn display_strings_pinned() {
        assert_eq!(
            ImageValidateError::Empty.to_string(),
            "image bytes are empty"
        );
        assert_eq!(
            ImageValidateError::Unsupported.to_string(),
            "unsupported or unrecognised image format"
        );
        assert_eq!(
            ImageValidateError::Truncated.to_string(),
            "image bytes are truncated"
        );
        assert_eq!(
            ImageValidateError::BadCrc.to_string(),
            "image has bad chunk CRC"
        );
        assert_eq!(
            ImageValidateError::WrongFormat.to_string(),
            "image format is not in the allow-list"
        );
        assert_eq!(
            ImageValidateError::Decode("oops".into()).to_string(),
            "image decode failed: oops"
        );
    }
}
