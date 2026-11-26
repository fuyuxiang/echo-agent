use super::*;
use image::DynamicImage;
use image::codecs::jpeg::JpegEncoder;
fn fresh_cache() -> NormalizeCache {
    let cache = NormalizeCache::with_capacity(64 * 1024 * 1024);
    cache.set_enabled(true);
    cache
}
fn make_test_png(width: u32, height: u32) -> Vec<u8> {
    use image::{ImageBuffer, Rgba};
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(width, height, Rgba([128, 64, 32, 255]));
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    buf
}
fn make_image_content(width: u32, height: u32) -> ImageContent {
    let png = make_test_png(width, height);
    ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&png),
        "image/png",
    )
}
fn make_ico_content(width: u32, height: u32) -> ImageContent {
    let png = make_test_png(width, height);
    let buf = echo_agent_test_utils::image::ico_with_png_frame(&png, width as u8, height as u8);
    ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&buf),
        "image/x-icon",
    )
}
/// An attached ICO survives as PNG instead of being dropped by the allow-list.
#[tokio::test]
async fn ico_attachment_transcoded_to_png() {
    let cache = fresh_cache();
    let content = match normalize_one_in(make_ico_content(16, 16), 1, false, &cache).await {
        Outcome::Unchanged(c) | Outcome::Compressed { content: c, .. } => c,
        other => panic!("expected ICO to survive as PNG, got {other:?}"),
    };
    assert_eq!(content.mime_type, "image/png");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&content.data)
        .unwrap();
    assert_eq!(
        image::guess_format(&decoded).unwrap(),
        image::ImageFormat::Png
    );
}
fn make_gif_content(width: u32, height: u32) -> ImageContent {
    use image::{ImageBuffer, Rgba};
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(width, height, Rgba([1u8, 2, 3, 255]));
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Gif)
        .unwrap();
    ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&buf),
        "image/gif",
    )
}
/// GIF must be PNG'd before send — engines do not sample GIF on the wire.
#[tokio::test]
async fn gif_attachment_transcoded_to_png() {
    let cache = fresh_cache();
    let content = match normalize_one_in(make_gif_content(32, 24), 1, false, &cache).await {
        Outcome::Unchanged(c) | Outcome::Compressed { content: c, .. } => c,
        other => panic!("expected GIF to survive as PNG, got {other:?}"),
    };
    assert_eq!(content.mime_type, "image/png");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&content.data)
        .unwrap();
    assert_eq!(
        image::guess_format(&decoded).unwrap(),
        image::ImageFormat::Png
    );
}
#[tokio::test]
async fn small_image_unchanged() {
    let img = make_image_content(100, 80);
    let original_data = img.data.clone();
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Unchanged(c) => assert_eq!(c.data, original_data),
        other => panic!("expected Unchanged, got {other:?}"),
    }
}
#[tokio::test]
async fn large_dimensions_resized_when_over_side_limit() {
    let img = make_image_content(3000, 2000);
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Compressed { content, info } => {
            assert_eq!(content.mime_type, "image/png");
            assert!(info.compressed_bytes < info.original_bytes);
            assert!(info.compressed_width <= MAX_ENCODE_SIDE_PX);
            assert!(info.compressed_height <= MAX_ENCODE_SIDE_PX);
            let area = u64::from(info.compressed_width) * u64::from(info.compressed_height);
            assert!(area <= MAX_ENCODE_PIXELS, "area {area} over v9 budget");
        }
        other => panic!("expected Compressed, got {other:?}"),
    }
}
/// A flat 1700x1700 attachment is under the 2000px side clamp but its
/// 2.89 Mpx area exceeds the v9 pixel budget, so it must be downscaled —
/// under a side-only cap this passed through unchanged at full resolution.
#[tokio::test]
async fn attachment_over_area_cap_is_downscaled() {
    let img = make_image_content(1700, 1700);
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Compressed { info, .. } => {
            assert!(
                info.exceeded_dimensions,
                "1700x1700 = 2.89 Mpx exceeds the {MAX_ENCODE_PIXELS} px area cap"
            );
            assert!(info.compressed_width <= MAX_ENCODE_SIDE_PX);
            assert!(info.compressed_height <= MAX_ENCODE_SIDE_PX);
            let area = u64::from(info.compressed_width) * u64::from(info.compressed_height);
            assert!(area <= MAX_ENCODE_PIXELS, "area {area} over v9 budget");
        }
        other => panic!("expected a 2.89 Mpx image to be downscaled, got {other:?}"),
    }
}
/// 3438x1830 flat screenshot: aspect > ~1.67, so the 2000px side clamp
/// binds before the area cap and the long side lands exactly on 2000.
#[tokio::test]
async fn wide_screenshot_clamped_to_side_limit_and_area_budget() {
    let img = make_image_content(3438, 1830);
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Compressed { info, .. } => {
            assert!(info.exceeded_dimensions);
            assert!(!info.exceeded_size, "flat PNG must be small in bytes");
            assert_eq!(
                info.compressed_width.max(info.compressed_height),
                MAX_ENCODE_SIDE_PX,
                "long side must land on the 2000px compat clamp"
            );
            let area = u64::from(info.compressed_width) * u64::from(info.compressed_height);
            assert!(area <= MAX_ENCODE_PIXELS, "area {area} over v9 budget");
        }
        other => panic!("expected Compressed, got {other:?}"),
    }
}
/// Near-square 1800x1700 = 3.06 Mpx: sides are within the 2000px clamp,
/// so only the v9 area cap triggers; the result stays under 2000 per side.
#[tokio::test]
async fn near_square_over_area_budget_downscaled_below_side_clamp() {
    let img = make_image_content(1800, 1700);
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Compressed { info, .. } => {
            assert!(info.exceeded_dimensions);
            assert!(info.compressed_width < MAX_ENCODE_SIDE_PX);
            assert!(info.compressed_height < MAX_ENCODE_SIDE_PX);
            let area = u64::from(info.compressed_width) * u64::from(info.compressed_height);
            assert!(area <= MAX_ENCODE_PIXELS, "area {area} over v9 budget");
            let r_in = 1800.0 / 1700.0;
            let r_out = info.compressed_width as f64 / info.compressed_height as f64;
            assert!(
                (r_in - r_out).abs() < 0.05,
                "aspect ratio {r_in} -> {r_out}"
            );
        }
        other => panic!("expected Compressed, got {other:?}"),
    }
}
/// External-harness pin: the 1024px side-only resize with area cap
/// disabled, so behavior matches the pre-v9-area-cap path —
/// a 1300x900 paste still lands on a 1024px long side.
#[tokio::test]
async fn strict_path_still_downscales_to_1024_side_only() {
    let img = make_image_content(1300, 900);
    let cache = fresh_cache();
    match normalize_one_in(img, 1, true, &cache).await {
        Outcome::Compressed { info, .. } => {
            assert!(
                info.exceeded_dimensions,
                "1300px exceeds the stricter 1024px resize target"
            );
            assert_eq!(
                info.compressed_width.max(info.compressed_height),
                STRICT_MAX_ENCODE_SIDE_PX,
                "external long side must land exactly on 1024"
            );
            assert!(info.compressed_height <= STRICT_MAX_ENCODE_SIDE_PX);
        }
        other => panic!("expected Compressed on the strict path, got {other:?}"),
    }
}
/// External-harness pin: within the 1024px side cap nothing triggers — the
/// attachment passes through untouched.
#[tokio::test]
async fn strict_path_passes_through_within_1024_side() {
    let img = make_image_content(1000, 800);
    let original_data = img.data.clone();
    let cache = fresh_cache();
    match normalize_one_in(img, 1, true, &cache).await {
        Outcome::Unchanged(c) => assert_eq!(c.data, original_data),
        other => panic!("expected Unchanged on the strict path, got {other:?}"),
    }
}
fn jpeg_larger_than_limit() -> Vec<u8> {
    use image::{ImageBuffer, Rgb};
    let mut side = 2200u32;
    for _ in 0..64 {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(side, side, |x, y| {
            Rgb([
                (x.wrapping_mul(17).wrapping_add(y)) as u8,
                (x.wrapping_mul(31).wrapping_add(y.wrapping_mul(7))) as u8,
                (x.wrapping_add(y).wrapping_mul(13)) as u8,
            ])
        });
        let mut buf = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut buf, 98);
        enc.encode_image(&DynamicImage::ImageRgb8(img))
            .expect("encode test JPEG");
        if buf.len() > MAX_IMAGE_BYTES {
            return buf;
        }
        side += 250;
    }
    panic!("could not synthesize test JPEG above limit");
}
#[tokio::test]
async fn oversize_bytes_becomes_jpeg_under_limit() {
    let raw = jpeg_larger_than_limit();
    assert!(raw.len() > MAX_IMAGE_BYTES);
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&raw),
        "image/jpeg",
    );
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Compressed { content, info } => {
            assert_eq!(content.mime_type, "image/jpeg");
            assert!(info.compressed_bytes <= MAX_IMAGE_BYTES);
            assert!(info.compressed_bytes < info.original_bytes);
            assert!(info.compressed_width <= MAX_ENCODE_SIDE_PX);
            assert!(info.compressed_height <= MAX_ENCODE_SIDE_PX);
            let area = u64::from(info.compressed_width) * u64::from(info.compressed_height);
            assert!(area <= MAX_ENCODE_PIXELS, "area {area} over v9 budget");
        }
        other => panic!("expected Compressed, got {other:?}"),
    }
}
#[tokio::test]
async fn bad_base64_fails() {
    let img = ImageContent::new(String::from("!!!"), "image/png");
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Failed { index, error } => {
            assert_eq!(index, 1);
            assert!(
                error.contains("base64"),
                "expected base64 mention in error, got: {error}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
#[tokio::test]
async fn oversized_invalid_bytes_fail_decode() {
    let raw = vec![0u8; MAX_IMAGE_BYTES + 1];
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&raw),
        "image/png",
    );
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Failed { index, error } => {
            assert_eq!(index, 1);
            assert!(
                error.contains("validate") || error.contains("decode"),
                "expected validate/decode mention, got: {error}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
#[tokio::test]
async fn preserves_index() {
    let raw = jpeg_larger_than_limit();
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&raw),
        "image/jpeg",
    );
    let cache = fresh_cache();
    match normalize_one_in(img, 7, false, &cache).await {
        Outcome::Compressed { info, .. } => assert_eq!(info.index, 7),
        other => panic!("expected Compressed, got {other:?}"),
    }
}
#[tokio::test]
async fn normalize_images_filters_bad_and_keeps_good() {
    let good = make_image_content(100, 100);
    let bad = ImageContent::new(String::from("!!!"), "image/png");
    let cache = fresh_cache();
    let result = normalize_images_in(vec![good, bad], false, &cache).await;
    assert_eq!(result.images.len(), 1);
    assert!(result.compressed.is_empty());
    assert!(result.re_encode_fallbacks.is_empty());
    assert_eq!(result.dropped.len(), 1, "bad image must surface as dropped");
    assert!(
        result.dropped[0].contains("Image 2"),
        "drop note must name the per-call index, got: {}",
        result.dropped[0]
    );
}
/// Wide raster so `resize(max_side, max_side)` must not equal a square output.
fn wide_jpeg_above_limit() -> (Vec<u8>, u32, u32) {
    use image::{ImageBuffer, Rgb};
    let h = 500u32;
    let mut w = 3600u32;
    for _ in 0..48 {
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(w, h, |x, y| {
            Rgb([
                (x.wrapping_mul(11).wrapping_add(y)) as u8,
                (x.wrapping_mul(19)) as u8,
                (y.wrapping_mul(7)) as u8,
            ])
        });
        let mut buf = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut buf, 98);
        enc.encode_image(&DynamicImage::ImageRgb8(img))
            .expect("encode wide test JPEG");
        if buf.len() > MAX_IMAGE_BYTES {
            return (buf, w, h);
        }
        w += 200;
    }
    panic!("could not synthesize wide JPEG above limit");
}
#[tokio::test]
async fn oversize_wide_image_keeps_aspect_ratio() {
    let (raw, ow, oh) = wide_jpeg_above_limit();
    assert!(raw.len() > MAX_IMAGE_BYTES);
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&raw),
        "image/jpeg",
    );
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Compressed { info, .. } => {
            let r_in = ow as f64 / oh as f64;
            let r_out = info.compressed_width as f64 / info.compressed_height as f64;
            assert!(
                (r_in - r_out).abs() < 0.2,
                "aspect ratio {r_in} -> {r_out} ({}x{} -> {}x{})",
                ow,
                oh,
                info.compressed_width,
                info.compressed_height
            );
            assert_ne!(info.compressed_width, info.compressed_height);
            assert!(info.compressed_width <= MAX_ENCODE_SIDE_PX);
            assert!(info.compressed_height <= MAX_ENCODE_SIDE_PX);
            let area = u64::from(info.compressed_width) * u64::from(info.compressed_height);
            assert!(area <= MAX_ENCODE_PIXELS, "area {area} over v9 budget");
        }
        other => panic!("expected Compressed, got {other:?}"),
    }
}
#[test]
fn compression_notice_contains_dimensions() {
    let info = ImageCompressionInfo {
        index: 1,
        original_bytes: 8_200_000,
        compressed_bytes: 1_400_000,
        original_width: 5000,
        original_height: 3000,
        compressed_width: 2000,
        compressed_height: 1200,
        exceeded_size: true,
        exceeded_dimensions: true,
    };
    let notice = render_compression_notice(std::slice::from_ref(&info), false);
    assert!(notice.contains("<system-reminder>"));
    assert!(notice.contains("</system-reminder>"));
    assert!(!notice.contains("<system_reminder>"));
    assert!(notice.contains("<image_compression_notice>"));
    assert!(notice.contains("was over the size and resolution limits"));
    assert!(notice.contains("5000x3000"));
    assert!(notice.contains("2000x1200"));
    assert!(notice.contains("7.8 MB"));
    assert!(notice.contains("1.3 MB"));
    let size_only = ImageCompressionInfo {
        exceeded_size: true,
        exceeded_dimensions: false,
        ..info
    };
    assert!(
        render_compression_notice(&[size_only], false)
            .contains("was over the 1.4 MB attachment limit")
    );
    let dims_only = ImageCompressionInfo {
        exceeded_size: false,
        exceeded_dimensions: true,
        ..info
    };
    assert!(
        render_compression_notice(&[dims_only], false)
            .contains("was over the max input resolution")
    );
}
#[test]
fn re_encode_fallback_notice_picks_tag_per_harness() {
    let notes = vec!["Image 1 could not be re-encoded under the cap.".to_string()];
    let echoagent = render_re_encode_fallback_notice(&notes, false);
    assert!(echoagent.contains("<system-reminder>"));
    assert!(echoagent.contains("</system-reminder>"));
    assert!(!echoagent.contains("<system_reminder>"));
    assert!(echoagent.contains("<image_re_encode_fallback>"));
}
#[test]
fn display_format() {
    let info_both = ImageCompressionInfo {
        index: 2,
        original_bytes: 5 * 1024 * 1024,
        compressed_bytes: 800 * 1024,
        original_width: 4000,
        original_height: 3000,
        compressed_width: 2000,
        compressed_height: 1500,
        exceeded_size: true,
        exceeded_dimensions: true,
    };
    assert_eq!(
        info_both.display(),
        "Downscaled Image 2: 5.0 MB (4000x3000) \u{2192} 800.0 KB (2000x1500)"
    );
    let info_size = ImageCompressionInfo {
        exceeded_size: true,
        exceeded_dimensions: false,
        ..info_both
    };
    assert_eq!(info_size.display(), info_both.display());
    let info_size_same_dims = ImageCompressionInfo {
        compressed_width: 4000,
        compressed_height: 3000,
        ..info_size
    };
    assert_eq!(
        info_size_same_dims.display(),
        "Compressed Image 2: 5.0 MB (4000x3000) \u{2192} 800.0 KB (4000x3000)"
    );
}
/// CRC-corrupt PNG must be rejected by the Unchanged integrity check.
#[tokio::test]
async fn crc_corrupt_png_fails_integrity_check() {
    let mut bytes = make_test_png(64, 64);
    let tag = b"IDAT";
    let pos = bytes
        .windows(4)
        .position(|w| w == tag)
        .expect("IDAT chunk present");
    bytes[pos + 12] ^= 0xFF;
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&bytes),
        "image/png",
    );
    let cache = fresh_cache();
    match normalize_one_in(img, 3, false, &cache).await {
        Outcome::Failed { index, error } => {
            assert_eq!(index, 3);
            assert!(
                error.contains("integrity check failed"),
                "expected integrity-check error, got: {error}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
/// Truncated PNG is rejected by the Unchanged integrity check.
#[tokio::test]
async fn truncated_png_fails_integrity_check_in_normalize_one() {
    let mut bytes = make_test_png(64, 64);
    bytes.truncate(bytes.len() / 2);
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&bytes),
        "image/png",
    );
    let cache = fresh_cache();
    match normalize_one_in(img, 11, false, &cache).await {
        Outcome::Failed { index, error } => {
            assert_eq!(index, 11);
            assert!(
                error.contains("integrity check failed"),
                "expected integrity-check error, got: {error}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
/// Truncated JPEG is rejected somewhere in the normalize pipeline
/// (dimension-probe or integrity-check, depending on the codec).
#[tokio::test]
async fn corrupt_jpeg_rejected_by_normalize_one() {
    use image::{ImageBuffer, Rgb};
    let img_buf: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(64, 64, |x, y| {
        Rgb([
            (x.wrapping_mul(13) ^ y) as u8,
            (x.wrapping_mul(7).wrapping_add(y * 3)) as u8,
            (x.wrapping_add(y).wrapping_mul(11)) as u8,
        ])
    });
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 80)
        .encode_image(&DynamicImage::ImageRgb8(img_buf))
        .unwrap();
    let sos_pos = bytes
        .windows(2)
        .position(|w| w == [0xFF, 0xDA])
        .expect("SOS marker present");
    bytes.truncate(sos_pos + 12);
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&bytes),
        "image/jpeg",
    );
    let cache = fresh_cache();
    match normalize_one_in(img, 12, false, &cache).await {
        Outcome::Failed { index, error } => {
            assert_eq!(index, 12);
            assert!(
                error.contains("validate")
                    || error.contains("decode")
                    || error.contains("truncated"),
                "expected validate/decode/truncated error, got: {error}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
/// Entropy-cut JPEG (a data URI sliced mid-payload by tool output):
/// zune-jpeg decodes it leniently, so only the structural walk rejects it.
#[tokio::test]
async fn truncated_jpeg_under_size_limits_is_dropped() {
    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageBuffer, Rgb};
    let img_buf: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(400, 300, |x, y| {
        Rgb([(x ^ y) as u8, (x * 7) as u8, (y * 5) as u8])
    });
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, 85)
        .encode_image(&DynamicImage::ImageRgb8(img_buf))
        .unwrap();
    bytes.truncate(bytes.len() / 2);
    assert!(
        image::load_from_memory(&bytes).is_ok(),
        "precondition: the lenient decoder accepts the truncated JPEG"
    );
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&bytes),
        "image/jpeg",
    );
    let cache = fresh_cache();
    match normalize_one_in(img, 4, false, &cache).await {
        Outcome::Failed { index, error } => {
            assert_eq!(index, 4);
            assert!(
                error.contains("truncated"),
                "expected truncated error, got: {error}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
/// A 16×16 icon clears the 8px-side floor but violates the API's
/// 512-total-pixel floor.
#[tokio::test]
async fn below_total_pixel_floor_is_dropped() {
    let img = make_image_content(16, 16);
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Failed { error, .. } => {
            assert!(
                error.contains("total pixels"),
                "expected total-pixel floor error, got: {error}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    let ok = make_image_content(32, 16);
    match normalize_one_in(ok, 1, false, &fresh_cache()).await {
        Outcome::Unchanged(_) => {}
        other => panic!("expected Unchanged at exactly 512 px, got {other:?}"),
    }
}
#[test]
fn persisted_image_reject_reason_verdicts() {
    let reason = persisted_image_reject_reason;
    assert_eq!(reason(&make_test_png(32, 32)), None);
    let mut jpeg = {
        use image::codecs::jpeg::JpegEncoder;
        use image::{ImageBuffer, Rgb};
        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_fn(64, 64, |x, y| Rgb([(x ^ y) as u8, x as u8, y as u8]));
        let mut buf = Vec::new();
        JpegEncoder::new_with_quality(&mut buf, 85)
            .encode_image(&image::DynamicImage::ImageRgb8(img))
            .unwrap();
        buf
    };
    assert_eq!(reason(&jpeg), None);
    jpeg.truncate(jpeg.len() / 2);
    assert!(
        reason(&jpeg).is_some_and(|r| r.contains("structurally incomplete")),
        "truncated JPEG must be rejected"
    );
    assert!(reason(&make_test_png(16, 16)).is_some_and(|r| r.contains("dimension floor")),);
    let mut gif = Vec::new();
    image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
        64,
        64,
        image::Rgba([1u8, 2, 3, 255]),
    ))
    .write_to(&mut std::io::Cursor::new(&mut gif), image::ImageFormat::Gif)
    .unwrap();
    assert!(reason(&gif).is_some_and(|r| r.contains("format")));
    let ico = echo_agent_test_utils::image::ico_with_png_frame(&make_test_png(16, 16), 16, 16);
    assert_eq!(reason(&ico), None);
    let mut cut_ico = ico.clone();
    cut_ico.truncate(cut_ico.len() - 8);
    assert!(
        reason(&cut_ico).is_some_and(|r| r.contains("Ico")),
        "truncated ICO must be rejected"
    );
    let mut garbage_ico = vec![0x00, 0x00, 0x01, 0x00];
    garbage_ico.extend_from_slice(&[0xAB; 64]);
    assert!(reason(&garbage_ico).is_some_and(|r| r.contains("Ico")));
    assert!(reason(b"not an image").is_some());
}
/// Regression: a camera-class photo above the old 16 Mpx decode cap
/// (production shape: a 5184×3888 ≈ 20 Mpx attachment refused with
/// "exceeds … px decode limit") must normalize via downscale, not be
/// rejected — the API accepts up to ~178.9 Mpx.
#[tokio::test]
async fn camera_sized_photo_is_compressed_not_rejected() {
    use image::codecs::jpeg::JpegEncoder;
    use image::{DynamicImage, ImageBuffer, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(5184, 3888, Rgb([90, 120, 150]));
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&DynamicImage::ImageRgb8(img))
        .unwrap();
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&jpeg),
        "image/jpeg",
    );
    match normalize_one_in(img, 0, false, &fresh_cache()).await {
        Outcome::Compressed { content, .. } => {
            assert!(!content.data.is_empty());
        }
        other => panic!("expected Compressed, got {other:?}"),
    }
}
/// Above the API ceiling the decode is still refused (the API would
/// 400 it regardless). SOF dims are patched — a real fixture that
/// large is infeasible to encode.
#[tokio::test]
async fn above_api_ceiling_is_rejected_by_normalize() {
    use image::codecs::jpeg::JpegEncoder;
    use image::{DynamicImage, ImageBuffer, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_pixel(64, 64, Rgb([1, 2, 3]));
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&DynamicImage::ImageRgb8(img))
        .unwrap();
    let sof = jpeg
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("baseline SOF0 present");
    jpeg[sof + 5..sof + 9].copy_from_slice(&[0x40, 0x00, 0x40, 0x00]);
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&jpeg),
        "image/jpeg",
    );
    match normalize_one_in(img, 0, false, &fresh_cache()).await {
        Outcome::Failed { error, .. } => {
            assert!(error.contains("decode limit"), "got: {error}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
/// The API also 400s images whose header dims exceed its
/// `MAX_IMAGE_PIXELS` ceiling; a kept one would brick the session the
/// same way as a below-floor image. (SOF dims are patched because
/// encoding a real >178 Mpx fixture is infeasible.)
#[test]
fn persisted_image_reject_reason_pixel_ceiling() {
    use image::codecs::jpeg::JpegEncoder;
    use image::{DynamicImage, ImageBuffer, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_fn(64, 64, |x, y| Rgb([(x ^ y) as u8, x as u8, y as u8]));
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 85)
        .encode_image(&DynamicImage::ImageRgb8(img))
        .unwrap();
    let sof = jpeg
        .windows(2)
        .position(|w| w == [0xFF, 0xC0])
        .expect("baseline SOF0 present");
    jpeg[sof + 5..sof + 9].copy_from_slice(&[0x40, 0x00, 0x40, 0x00]);
    assert!(
        persisted_image_reject_reason(&jpeg).is_some_and(|r| r.contains("above pixel ceiling")),
    );
}
/// The `read_file` inline-attach gate (the below-floor icon enforcement
/// point): floors enforced, and unvalidatable payloads fail closed.
#[test]
fn inline_attach_verdict_gates_floors_and_fails_closed() {
    use base64::Engine as _;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    assert_eq!(
        inline_attach_verdict(&b64(&make_test_png(16, 16))),
        InlineAttachVerdict::TooSmall
    );
    assert_eq!(
        inline_attach_verdict(&b64(&make_test_png(32, 16))),
        InlineAttachVerdict::Attach
    );
    assert_eq!(
        inline_attach_verdict(&b64(&make_test_png(64, 64))),
        InlineAttachVerdict::Attach
    );
    assert_eq!(
        inline_attach_verdict(&b64(&make_test_png(4, 200))),
        InlineAttachVerdict::TooSmall
    );
    assert_eq!(
        inline_attach_verdict("!!!not base64!!!"),
        InlineAttachVerdict::Unreadable
    );
    assert_eq!(
        inline_attach_verdict(&b64(b"not an image")),
        InlineAttachVerdict::Unreadable
    );
}
/// Oversized truncated JPEG must be dropped too — not silently healed
/// into a mostly-grey re-encode.
#[tokio::test]
async fn truncated_oversized_jpeg_is_dropped() {
    let mut raw = jpeg_larger_than_limit();
    raw.truncate(raw.len() / 2);
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&raw),
        "image/jpeg",
    );
    let cache = fresh_cache();
    match normalize_one_in(img, 2, false, &cache).await {
        Outcome::Failed { error, .. } => {
            assert!(
                error.contains("truncated"),
                "expected truncated error, got: {error}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
#[tokio::test]
async fn small_well_formed_png_unchanged_after_integrity_check() {
    let img = make_image_content(50, 40);
    let original_data = img.data.clone();
    let cache = fresh_cache();
    match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Unchanged(c) => assert_eq!(c.data, original_data),
        other => panic!("expected Unchanged, got {other:?}"),
    }
}
/// Dropped notes propagate to the top-level `NormalizeResult`.
#[tokio::test]
async fn normalize_images_collects_dropped_notes() {
    let mut bytes = make_test_png(32, 32);
    let tag = b"IDAT";
    let pos = bytes.windows(4).position(|w| w == tag).unwrap();
    bytes[pos + 12] ^= 0xFF;
    let bad = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&bytes),
        "image/png",
    );
    let good = make_image_content(60, 40);
    let cache = fresh_cache();
    let result = normalize_images_in(vec![good, bad], false, &cache).await;
    assert_eq!(result.images.len(), 1, "good image preserved");
    assert_eq!(result.dropped.len(), 1, "one drop note");
    assert!(result.dropped[0].contains("Image 2"), "drop names index");
}
#[test]
fn dropped_to_envelope_returns_none_for_empty() {
    assert!(dropped_to_envelope(Vec::new(), false).is_none());
    assert!(dropped_to_envelope(Vec::new(), true).is_none());
}
#[test]
fn dropped_to_envelope_emits_notice_and_notes() {
    let notes = vec!["Image 1 was dropped before send: corrupt.".to_string()];
    let (notice, returned) = dropped_to_envelope(notes.clone(), false).unwrap();
    assert!(notice.contains("<system-reminder>"));
    assert!(notice.contains("<image_dropped_notice>"));
    assert!(notice.contains(&notes[0]));
    assert_eq!(returned, notes);
}
#[test]
fn image_dropped_notice_picks_tag_per_harness() {
    let notes = vec!["Image 5 was dropped before send: corrupt".to_string()];
    let echoagent = render_image_dropped_notice(&notes, false);
    assert!(echoagent.contains("<system-reminder>"));
    assert!(echoagent.contains("</system-reminder>"));
    assert!(!echoagent.contains("<system_reminder>"));
    assert!(echoagent.contains("<image_dropped_notice>"));
    assert!(echoagent.contains("Image 5"));
    assert_eq!(render_image_dropped_notice(&[], false), "");
}
/// Large flat-color images compress better as PNG than JPEG; the
/// normalizer must pick PNG when it wins.
#[tokio::test]
async fn flat_color_oversized_picks_png() {
    use image::{ImageBuffer, Rgb};
    let side = 4000u32;
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_pixel(side, side, Rgb([40, 80, 120]));
    let mut png_buf = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut png_buf),
        image::ImageFormat::Png,
    )
    .unwrap();
    let padding = MAX_IMAGE_BYTES + 1 - png_buf.len();
    png_buf.resize(png_buf.len() + padding, 0xAA);
    assert!(png_buf.len() > MAX_IMAGE_BYTES);
    let content = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&png_buf),
        "image/png",
    );
    let cache = fresh_cache();
    match normalize_one_in(content, 1, false, &cache).await {
        Outcome::Compressed { content, info } => {
            assert_eq!(
                content.mime_type, "image/png",
                "flat-color image should pick PNG over JPEG"
            );
            assert!(info.compressed_bytes <= MAX_IMAGE_BYTES);
            assert!(info.compressed_bytes < info.original_bytes);
        }
        other => panic!("expected Compressed, got {other:?}"),
    }
}
/// `Bytes::as_ptr` identity + re-stamped per-call `index` together
/// prove the second call is a cache hit through `entry_to_outcome`.
#[tokio::test]
async fn normalize_one_in_uses_cache_for_repeat_input() {
    let cache = fresh_cache();
    let img = make_image_content(48, 48);
    let raw_decoded = base64::engine::general_purpose::STANDARD
        .decode(&img.data)
        .expect("test base64 decode");
    let dup = img.clone();
    let first_data = match normalize_one_in(img, 1, false, &cache).await {
        Outcome::Unchanged(c) => c.data,
        other => panic!("expected Unchanged on first call, got {other:?}"),
    };
    let cached_first = cache
        .get_for_tests(&raw_decoded, HarnessVariant::Default)
        .await
        .expect("first call must populate the cache");
    let p1 = match &cached_first {
        NormalizedEntry::Unchanged { bytes, .. } => bytes.as_ptr(),
        other => panic!("expected Unchanged in cache, got {other:?}"),
    };
    let second_data = match normalize_one_in(dup, 9, false, &cache).await {
        Outcome::Unchanged(c) => c.data,
        other => panic!("expected Unchanged on second call, got {other:?}"),
    };
    assert_eq!(
        first_data, second_data,
        "cache-served output must match the first compute"
    );
    let cached_second = cache
        .get_for_tests(&raw_decoded, HarnessVariant::Default)
        .await
        .expect("cache entry survived");
    let p2 = match &cached_second {
        NormalizedEntry::Unchanged { bytes, .. } => bytes.as_ptr(),
        _ => unreachable!("invariant: variant pinned above"),
    };
    assert_eq!(p1, p2, "`Bytes::as_ptr` identity proves cache hit");
}
/// Forces `re_encode_under_limit` to exhaust every step (drives
/// the `ReEncodingOversized` path).
static UNSATISFIABLE_PARAMS: ReEncodeParams = ReEncodeParams {
    max_bytes: 0,
    max_side_px: MAX_ENCODE_SIDE_PX,
    max_pixels: MAX_ENCODE_PIXELS,
    min_side_px: MIN_ENCODE_SIDE_PX,
    quality_steps: JPEG_QUALITY_STEPS,
    filter: DOWNSCALE_FILTER,
};
fn unsatisfiable_params() -> &'static ReEncodeParams {
    &UNSATISFIABLE_PARAMS
}
#[tokio::test]
async fn re_encoding_oversized_routing() {
    let raw = jpeg_larger_than_limit();
    let entry = compute_normalized_blocking(raw.clone(), unsatisfiable_params(), 5)
        .expect("blocking compute ok");
    match &entry {
        NormalizedEntry::ReEncodingOversized { bytes, mime } => {
            assert_eq!(bytes.as_ref(), raw.as_slice(), "original bytes preserved");
            assert_eq!(mime.as_ref(), "image/jpeg");
        }
        other => panic!("expected ReEncodingOversized, got {other:?}"),
    }
    let orig = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&raw),
        "image/jpeg",
    );
    match entry_to_outcome(orig.clone(), entry, 5) {
        Outcome::ReEncodingOversized(c) => {
            assert_eq!(c.data, orig.data, "original ImageContent passed through");
            assert_eq!(c.mime_type, "image/jpeg");
        }
        other => panic!("expected ReEncodingOversized, got {other:?}"),
    }
}
#[tokio::test]
async fn re_encoding_oversized_round_trips_through_cache() {
    let raw = jpeg_larger_than_limit();
    let cache = fresh_cache();
    let params = unsatisfiable_params();
    cache
        .get_or_try_insert_with(
            raw.clone(),
            HarnessVariant::Default,
            move |bytes| async move { compute_normalized_blocking(bytes, params, 1) },
        )
        .await
        .expect("seed cache");
    let cached = cache
        .get_for_tests(&raw, HarnessVariant::Default)
        .await
        .expect("seed succeeded");
    let p1 = match &cached {
        NormalizedEntry::ReEncodingOversized { bytes, .. } => bytes.as_ptr(),
        other => panic!("expected ReEncodingOversized in cache, got {other:?}"),
    };
    let input = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&raw),
        "image/jpeg",
    );
    match normalize_one_in(input, 3, false, &cache).await {
        Outcome::ReEncodingOversized(c) => assert_eq!(c.mime_type, "image/jpeg"),
        other => panic!("expected ReEncodingOversized from cache hit, got {other:?}"),
    }
    let cached_after = cache
        .get_for_tests(&raw, HarnessVariant::Default)
        .await
        .expect("entry survived");
    let p2 = match &cached_after {
        NormalizedEntry::ReEncodingOversized { bytes, .. } => bytes.as_ptr(),
        _ => unreachable!(),
    };
    assert_eq!(
        p1, p2,
        "ReEncodingOversized cache hit must share backing buffer"
    );
}
#[tokio::test]
async fn normalize_images_in_collects_re_encode_fallback_note() {
    let raw = jpeg_larger_than_limit();
    let cache = fresh_cache();
    let params = unsatisfiable_params();
    cache
        .get_or_try_insert_with(
            raw.clone(),
            HarnessVariant::Default,
            move |bytes| async move { compute_normalized_blocking(bytes, params, 1) },
        )
        .await
        .expect("seed cache");
    let img = ImageContent::new(
        base64::engine::general_purpose::STANDARD.encode(&raw),
        "image/jpeg",
    );
    let result = normalize_images_in(vec![img], false, &cache).await;
    assert_eq!(result.images.len(), 1, "image preserved despite oversize");
    assert_eq!(
        result.re_encode_fallbacks.len(),
        1,
        "one fallback note per oversized image"
    );
    assert!(
        result.re_encode_fallbacks[0].contains("Image 1"),
        "fallback note must name the per-call index, got: {}",
        result.re_encode_fallbacks[0],
    );
}
#[tokio::test]
async fn sub_8x8_image_is_rejected() {
    let tiny = make_image_content(4, 3);
    let ok = make_image_content(30, 30);
    let cache = fresh_cache();
    let result = normalize_images_in(vec![tiny, ok], false, &cache).await;
    assert_eq!(result.images.len(), 1, "only the >=8x8 image proceeds");
    assert_eq!(result.dropped.len(), 1);
    assert!(
        result.dropped[0].contains("4×3") && result.dropped[0].contains("8×8"),
        "dropped note must mention the offending dims and the min: {}",
        result.dropped[0]
    );
    assert!(
        result.dropped[0].contains("too small"),
        "dropped: {}",
        result.dropped[0]
    );
}
/// Boundary: 8×8 clears the per-side floor but not the API's
/// 512-total-pixel floor (64 px would 400 server-side).
#[tokio::test]
async fn exactly_8x8_is_rejected_by_total_pixel_floor() {
    let img = make_image_content(8, 8);
    let cache = fresh_cache();
    let result = normalize_images_in(vec![img], false, &cache).await;
    assert!(result.images.is_empty());
    assert_eq!(result.dropped.len(), 1);
    assert!(
        result.dropped[0].contains("total pixels"),
        "dropped: {}",
        result.dropped[0]
    );
}
/// One dimension below threshold.
#[tokio::test]
async fn seven_by_eight_is_rejected() {
    let img = make_image_content(7, 8);
    let cache = fresh_cache();
    let result = normalize_images_in(vec![img], false, &cache).await;
    assert!(result.images.is_empty());
    assert_eq!(result.dropped.len(), 1);
    assert!(result.dropped[0].contains("7×8"));
}
