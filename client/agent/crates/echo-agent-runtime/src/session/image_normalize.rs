//! Re-encode decoded attachments that exceed [`MAX_IMAGE_BYTES`],
//! [`MAX_ENCODE_PIXELS`], or [`MAX_ENCODE_SIDE_PX`] to fit the conversation
//! caps. The primary dimension limit is the v9 pixel-area budget
//! ([`MAX_ENCODE_PIXELS`]); [`MAX_ENCODE_SIDE_PX`] is a model-agnostic side
//! clamp. Compute is amortised via
//! [`NormalizeCache`](crate::session::normalize_cache).
use crate::session::normalize_cache::{
    HarnessVariant, NormalizeCache, NormalizeError, NormalizedEntry, run_blocking,
};
use agent_client_protocol::ImageContent;
use base64::Engine as _;
use bytes::Bytes;
use echo_agent_tools::util::format_bytes;
use echo_agent_tools::util::image_compress::{FilterType, ReEncodeParams, re_encode_under_limit};
use std::borrow::Cow;
/// Decoded attachment bytes above this are re-encoded to fit this cap.
///
/// Kept low so many images fit under the inference proxy's ~50 MB request-body
/// limit before byte-budget image eviction has to kick in downstream. Base64
/// inflates raw bytes by ~4/3, so a 1.5 MB image is ~2 MB on the wire and ~25
/// fit under the limit. A low per-image cost means a conversation rarely
/// reaches the eviction threshold, so the server-side KV-cache prefix is rarely
/// rewritten. (Was 5 MB, which let only ~7 images reach the limit and then
/// forced cache-busting eviction on essentially every subsequent turn.)
pub(crate) const MAX_IMAGE_BYTES: usize = 1_500_000;
/// The attachment cap rendered the way the sizes beside it render.
fn limit_label() -> String {
    format_bytes(MAX_IMAGE_BYTES as u64)
}
/// Total pixel budget (w*h) before downscaling. Mirrors the v9 tokenizer's
/// `image_filter_max_pixels = 2_408_448` — larger images are downsampled
/// server-side anyway, so extra pixels only waste request bytes.
const MAX_ENCODE_PIXELS: u64 = 2_408_448;
/// Max width/height before downscaling. Model-agnostic side clamp because
/// images are normalized once at ingest and models can switch mid-session.
/// Not a v9 constraint — [`MAX_ENCODE_PIXELS`] is what the v9 encoder enforces.
const MAX_ENCODE_SIDE_PX: u32 = 2000;
/// External-harness image-resize path caps at 1024px before captioning.
const STRICT_MAX_ENCODE_SIDE_PX: u32 = 1024;
const MIN_ENCODE_SIDE_PX: u32 = 512;
const DOWNSCALE_FILTER: FilterType = FilterType::CatmullRom;
const JPEG_QUALITY_STEPS: &[u8] = &[88, 80, 72, 64, 56, 48, 40, 32];
/// Upper bound on decoded pixel count before refusing to decode. Matches
/// the API ceiling ([`MAX_VISION_TOTAL_PX`]) so any image the API would
/// accept can be decoded for the downscale re-encode — a 20-48 Mpx camera
/// photo must not be refused client-side (it downscales to the wire caps
/// anyway). Worst case is a transient ~716 MB RGBA bitmap inside
/// `spawn_blocking`, one image at a time.
const MAX_DECODE_PIXELS: u64 = MAX_VISION_TOTAL_PX;
/// Bounded ICO decode for load-time verification: real icons are far
/// smaller; bytes claiming more are kept un-verified rather than decoded
/// on the session-load path.
const MAX_LOAD_ICO_DECODE_PIXELS: u64 = 16_000_000;
/// Backend APIs reject images with either side < 8 px.
pub(crate) const MIN_VISION_SIDE_PX: u32 = 8;
/// Backend APIs also reject images with fewer than 512 total pixels
/// (`MIN_IMAGE_PIXELS`); e.g. a 16×16 icon is 256 px and draws a 400 that
/// poisons the conversation on every following turn.
pub(crate) const MIN_VISION_TOTAL_PX: u64 = 512;
/// Backend ceiling (`MAX_IMAGE_PIXELS`), header-checked server-side before
/// any resize. Send paths re-encode far below this; only legacy/foreign
/// history payloads can exceed it.
pub(crate) const MAX_VISION_TOTAL_PX: u64 = 178_956_970;
const NORMALIZE_PARAMS: ReEncodeParams = ReEncodeParams {
    max_bytes: MAX_IMAGE_BYTES,
    max_side_px: MAX_ENCODE_SIDE_PX,
    max_pixels: MAX_ENCODE_PIXELS,
    min_side_px: MIN_ENCODE_SIDE_PX,
    quality_steps: JPEG_QUALITY_STEPS,
    filter: DOWNSCALE_FILTER,
};
/// Resize target for the stricter image normalization path.
const STRICT_NORMALIZE_PARAMS: ReEncodeParams = ReEncodeParams {
    max_bytes: MAX_IMAGE_BYTES,
    max_side_px: STRICT_MAX_ENCODE_SIDE_PX,
    max_pixels: u64::MAX,
    min_side_px: MIN_ENCODE_SIDE_PX,
    quality_steps: JPEG_QUALITY_STEPS,
    filter: DOWNSCALE_FILTER,
};
#[derive(Debug, Clone, Copy)]
pub(crate) struct ImageCompressionInfo {
    pub index: usize,
    pub original_bytes: usize,
    pub compressed_bytes: usize,
    pub original_width: u32,
    pub original_height: u32,
    pub compressed_width: u32,
    pub compressed_height: u32,
    pub exceeded_size: bool,
    pub exceeded_dimensions: bool,
}
impl ImageCompressionInfo {
    fn reason_label(&self) -> Cow<'static, str> {
        match (self.exceeded_size, self.exceeded_dimensions) {
            (true, true) => Cow::Borrowed("was over the size and resolution limits"),
            (true, false) => Cow::Owned(format!("was over the {} attachment limit", limit_label())),
            (false, true) => Cow::Borrowed("was over the max input resolution"),
            (false, false) => Cow::Borrowed("compressed"),
        }
    }
    /// User-facing one-liner (TUI toast, headless events). Leads with the
    /// outcome, not the limit violation, so it doesn't read as an error;
    /// the why lives in the model-facing [`render_compression_notice`],
    /// which keeps [`Self::reason_label`].
    pub(crate) fn display(&self) -> String {
        let verb = if self.compressed_width == self.original_width
            && self.compressed_height == self.original_height
        {
            "Compressed"
        } else {
            "Downscaled"
        };
        format!(
            "{verb} Image {}: {} ({}x{}) \u{2192} {} ({}x{})",
            self.index,
            format_bytes(self.original_bytes as u64),
            self.original_width,
            self.original_height,
            format_bytes(self.compressed_bytes as u64),
            self.compressed_width,
            self.compressed_height,
        )
    }
}
#[derive(Default)]
pub(crate) struct NormalizeResult {
    pub images: Vec<ImageContent>,
    pub compressed: Vec<ImageCompressionInfo>,
    pub re_encode_fallbacks: Vec<String>,
    /// Images dropped entirely (integrity failure, too small, etc.).
    /// Surfaced via [`render_image_dropped_notice`].
    pub dropped: Vec<String>,
}
pub(crate) async fn normalize_images(
    images: Vec<ImageContent>,
    is_cursor: bool,
) -> NormalizeResult {
    normalize_images_in(images, is_cursor, NormalizeCache::global()).await
}
/// [`normalize_images`] with an injected cache (tests use a fresh
/// per-case instance to avoid singleton-state leakage).
pub(crate) async fn normalize_images_in(
    images: Vec<ImageContent>,
    is_cursor: bool,
    cache: &NormalizeCache,
) -> NormalizeResult {
    let mut out = Vec::with_capacity(images.len());
    let mut compressed = Vec::new();
    let mut re_encode_fallbacks = Vec::new();
    let mut dropped = Vec::new();
    for (i, img) in images.into_iter().enumerate() {
        let one_based = i + 1;
        match normalize_one_in(img, one_based, is_cursor, cache).await {
            Outcome::Unchanged(c) => out.push(c),
            Outcome::ReEncodingOversized(c) => {
                re_encode_fallbacks
                    .push(
                        format!(
                    "Image {one_based} could not be re-encoded under the {} limit; the original attachment was kept.",
                    limit_label()
                ),
                    );
                out.push(c);
            }
            Outcome::Compressed { content, info } => {
                out.push(content);
                compressed.push(info);
            }
            Outcome::Failed { index, error } => {
                tracing::warn!("image {index}: normalization failed: {error}");
                dropped.push(format!("Image {index} was dropped before send: {error}."));
            }
        }
    }
    NormalizeResult {
        images: out,
        compressed,
        re_encode_fallbacks,
        dropped,
    }
}
fn params_for(harness: HarnessVariant) -> &'static ReEncodeParams {
    match harness {
        HarnessVariant::Cursor => &STRICT_NORMALIZE_PARAMS,
        HarnessVariant::Default => &NORMALIZE_PARAMS,
    }
}
/// Resolve the active reminder tag via the canonical constants in
/// `echo_agent_tools::reminders` (free-fn shape because this module has no
/// `SessionActor`; see `reminder_wrapper_tag`).
fn reminder_tag(is_cursor: bool) -> &'static str {
    let _ = is_cursor;
    echo_agent_tools::reminders::DEFAULT_REMINDER_TAG
}
fn render_notice(notes: &[String], is_cursor: bool, inner_tag: &str) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let tag = reminder_tag(is_cursor);
    format!(
        "\n\n<{tag}>\n\
         <{inner_tag}>\n\
         {}\n\
         </{inner_tag}>\n\
         </{tag}>",
        notes.join("\n"),
    )
}
/// System-reminder for images dropped entirely before send.
pub(crate) fn render_image_dropped_notice(notes: &[String], is_cursor: bool) -> String {
    render_notice(notes, is_cursor, "image_dropped_notice")
}
/// Build the (system-reminder, owned-notes) pair for a
/// `NormalizeResult.dropped` list. Shared by the user-attachment flow
/// and the tool-result-extraction flow.
pub(crate) fn dropped_to_envelope(
    dropped: Vec<String>,
    is_cursor: bool,
) -> Option<(String, Vec<String>)> {
    if dropped.is_empty() {
        return None;
    }
    let notice = render_image_dropped_notice(&dropped, is_cursor);
    Some((notice, dropped))
}
/// System-reminder when oversized attachments were kept after re-encode failure.
pub(crate) fn render_re_encode_fallback_notice(notes: &[String], is_cursor: bool) -> String {
    render_notice(notes, is_cursor, "image_re_encode_fallback")
}
/// System-reminder listing images that were re-encoded under the cap.
pub(crate) fn render_compression_notice(
    compressed: &[ImageCompressionInfo],
    is_cursor: bool,
) -> String {
    let notes: Vec<String> = compressed
        .iter()
        .map(|c| {
            format!(
                "Image {} {} and was re-encoded from {}x{} ({}) \
                 to {}x{} ({}). Fine details may have been lost.",
                c.index,
                c.reason_label(),
                c.original_width,
                c.original_height,
                format_bytes(c.original_bytes as u64),
                c.compressed_width,
                c.compressed_height,
                format_bytes(c.compressed_bytes as u64),
            )
        })
        .collect();
    let tag = reminder_tag(is_cursor);
    format!(
        "\n\n<{tag}>\n\
         <image_compression_notice>\n\
         {}\n\
         </image_compression_notice>\n\
         </{tag}>",
        notes.join("\n"),
    )
}
/// Why persisted-history image bytes would be rejected by the API, or
/// `None` when sendable. Cheap (format sniff + structural walk + header
/// dimension probe; pixel decode only for ICO, bounded) — used at session
/// load to strip payloads that draw a 400 on every subsequent turn,
/// leaving the session bricked. The reason is logged when the loader
/// strips an image — the strip is re-persisted (irreversible), so the
/// evidence must reach logs.
pub(crate) fn persisted_image_reject_reason(bytes: &[u8]) -> Option<String> {
    use echo_agent_tools::util::image_validate as iv;
    use image::ImageFormat as F;
    let Ok(format) = image::guess_format(bytes) else {
        return Some(format!("unrecognized format ({} bytes)", bytes.len()));
    };
    match format {
        F::Ico => {
            let Ok((w, h, _)) = iv::validate_image_bytes_unrestricted(bytes, false) else {
                return Some("unreadable Ico header".to_owned());
            };
            if (w as u64) * (h as u64) > MAX_LOAD_ICO_DECODE_PIXELS {
                return None;
            }
            image::load_from_memory(bytes)
                .is_err()
                .then(|| format!("undecodable Ico ({} bytes)", bytes.len()))
        }
        F::Gif | F::Bmp | F::Tiff => Some(format!("API-rejected format {format:?}")),
        F::Jpeg | F::Png | F::WebP => {
            if !iv::format_structurally_complete(format, bytes) {
                return Some(format!(
                    "structurally incomplete {format:?} ({} bytes)",
                    bytes.len()
                ));
            }
            let Ok((w, h, _)) = iv::validate_image_bytes_unrestricted(bytes, false) else {
                return Some(format!("unreadable {format:?} header"));
            };
            let px = (w as u64) * (h as u64);
            if w < MIN_VISION_SIDE_PX || h < MIN_VISION_SIDE_PX || px < MIN_VISION_TOTAL_PX {
                return Some(format!("below dimension floor ({w}x{h})"));
            }
            (px > MAX_VISION_TOTAL_PX).then(|| format!("above pixel ceiling ({w}x{h})"))
        }
        _ => Some(format!("API-rejected format {format:?}")),
    }
}
/// Whether a `read_file` image may be attached inline to its tool result.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InlineAttachVerdict {
    Attach,
    TooSmall,
    /// Base64 or header probe failed. Fail closed: an unvalidatable image
    /// must be withheld, not attached (the API 400s it on every turn).
    Unreadable,
}
/// Gate for attaching a `read_file` image to its tool result: enforce the
/// API dimension floors on the decoded payload.
pub(crate) fn inline_attach_verdict(data_b64: &str) -> InlineAttachVerdict {
    use base64::Engine as _;
    let Ok(raw) = base64::engine::general_purpose::STANDARD.decode(data_b64) else {
        return InlineAttachVerdict::Unreadable;
    };
    let Ok((w, h, _)) =
        echo_agent_tools::util::image_validate::validate_image_bytes_with(&raw, false)
    else {
        return InlineAttachVerdict::Unreadable;
    };
    if w < MIN_VISION_SIDE_PX
        || h < MIN_VISION_SIDE_PX
        || (w as u64) * (h as u64) < MIN_VISION_TOTAL_PX
    {
        InlineAttachVerdict::TooSmall
    } else {
        InlineAttachVerdict::Attach
    }
}
#[derive(Debug)]
enum Outcome {
    Unchanged(ImageContent),
    /// Re-encode could not meet the byte cap; original bytes are passed through.
    ReEncodingOversized(ImageContent),
    Compressed {
        content: ImageContent,
        info: ImageCompressionInfo,
    },
    Failed {
        index: usize,
        error: String,
    },
}
async fn normalize_one_in(
    img: ImageContent,
    index: usize,
    is_cursor: bool,
    cache: &NormalizeCache,
) -> Outcome {
    let raw_bytes = match base64::engine::general_purpose::STANDARD.decode(&img.data) {
        Ok(b) => b,
        Err(e) => return fail(index, format!("base64 decode: {e}")),
    };
    use echo_agent_tools::util::image_validate as iv;
    let (img, raw_bytes) = if iv::needs_endpoint_transcode(&raw_bytes) {
        let png = match run_blocking(move || match iv::transcode_to_endpoint_png(&raw_bytes) {
            Some(r) => r.map_err(|e| NormalizeError(format!("non-native image transcode: {e}"))),
            None => Err(NormalizeError(
                "non-native image transcode: format no longer needs conversion".to_owned(),
            )),
        })
        .await
        {
            Ok(png) => png,
            Err(e) => return fail(index, e.0),
        };
        let converted = ImageContent::new(
            base64::engine::general_purpose::STANDARD.encode(&png),
            "image/png".to_owned(),
        )
        .uri(img.uri.clone())
        .annotations(img.annotations.clone())
        .meta(img.meta.clone());
        (converted, png)
    } else {
        (img, raw_bytes)
    };
    let harness = HarnessVariant::from_is_cursor(is_cursor);
    let entry_res = cache
        .get_or_try_insert_with(raw_bytes, harness, move |bytes| {
            compute_normalized(bytes, harness, index)
        })
        .await;
    match entry_res {
        Ok(entry) => entry_to_outcome(img, entry, index),
        Err(arc_err) => fail(index, arc_err.0.clone()),
    }
}
fn entry_to_outcome(orig: ImageContent, entry: NormalizedEntry, index: usize) -> Outcome {
    match entry {
        NormalizedEntry::Compressed {
            bytes, mime, info, ..
        } => Outcome::Compressed {
            content: ImageContent::new(
                base64::engine::general_purpose::STANDARD.encode(&bytes),
                mime.into_owned(),
            )
            .uri(orig.uri)
            .annotations(orig.annotations)
            .meta(orig.meta),
            info: ImageCompressionInfo { index, ..info },
        },
        NormalizedEntry::ReEncodingOversized { .. } => Outcome::ReEncodingOversized(orig),
        NormalizedEntry::Unchanged { .. } => Outcome::Unchanged(orig),
    }
}
async fn compute_normalized(
    raw_bytes: Vec<u8>,
    harness: HarnessVariant,
    index: usize,
) -> Result<NormalizedEntry, NormalizeError> {
    let params = params_for(harness);
    run_blocking(move || compute_normalized_blocking(raw_bytes, params, index)).await
}
/// CPU-bound normalize work. `params` widens to `&'static
/// ReEncodeParams` (instead of `HarnessVariant`) so tests can inject
/// `max_bytes = 0` to drive the `ReEncodingOversized` branch.
fn compute_normalized_blocking(
    raw_bytes: Vec<u8>,
    params: &'static ReEncodeParams,
    index: usize,
) -> Result<NormalizedEntry, NormalizeError> {
    let original_bytes = raw_bytes.len();
    let (orig_w, orig_h, orig_mime) =
        echo_agent_tools::util::image_validate::validate_image_bytes_with(&raw_bytes, false)
            .map_err(|e| NormalizeError(format!("validate: {e}")))?;
    if !echo_agent_tools::util::image_validate::image_structurally_complete(&raw_bytes) {
        return Err(NormalizeError(
            "integrity check failed: image bytes are truncated".to_owned(),
        ));
    }
    if orig_w < MIN_VISION_SIDE_PX || orig_h < MIN_VISION_SIDE_PX {
        return Err(NormalizeError(format!(
            "too small ({orig_w}×{orig_h}); images must be at least {MIN_VISION_SIDE_PX}×{MIN_VISION_SIDE_PX} pixels"
        )));
    }
    let pixels = (orig_w as u64) * (orig_h as u64);
    if pixels < MIN_VISION_TOTAL_PX {
        return Err(NormalizeError(format!(
            "too small ({orig_w}×{orig_h} = {pixels} px); images must have at least {MIN_VISION_TOTAL_PX} total pixels"
        )));
    }
    let exceeded_size = original_bytes > MAX_IMAGE_BYTES;
    let exceeded_dimensions = params.exceeds_dimension_caps(orig_w, orig_h);
    if !exceeded_size && !exceeded_dimensions {
        if let Err(e) = echo_agent_tools::util::image_validate::validate_image_bytes(&raw_bytes) {
            return Err(NormalizeError(format!("integrity check failed: {e}")));
        }
        return Ok(NormalizedEntry::Unchanged {
            bytes: Bytes::from(raw_bytes),
            mime: Cow::Borrowed(orig_mime),
        });
    }
    if pixels > MAX_DECODE_PIXELS {
        return Err(NormalizeError(format!(
            "image {orig_w}x{orig_h} exceeds {MAX_DECODE_PIXELS} px decode limit",
        )));
    }
    let decoded =
        image::load_from_memory(&raw_bytes).map_err(|e| NormalizeError(format!("decode: {e}")))?;
    let (buf, new_w, new_h, mime) = match re_encode_under_limit(&decoded, params) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                index,
                bytes = original_bytes,
                error = %e,
                "image re-encode failed; keeping original attachment"
            );
            return Ok(NormalizedEntry::ReEncodingOversized {
                bytes: Bytes::from(raw_bytes),
                mime: Cow::Borrowed(orig_mime),
            });
        }
    };
    if buf.len() >= original_bytes {
        return Ok(NormalizedEntry::Unchanged {
            bytes: Bytes::from(raw_bytes),
            mime: Cow::Borrowed(orig_mime),
        });
    }
    let compressed_bytes = buf.len();
    Ok(NormalizedEntry::Compressed {
        bytes: Bytes::from(buf),
        mime: Cow::Borrowed(mime),
        info: ImageCompressionInfo {
            index,
            original_bytes,
            compressed_bytes,
            original_width: orig_w,
            original_height: orig_h,
            compressed_width: new_w,
            compressed_height: new_h,
            exceeded_size,
            exceeded_dimensions,
        },
    })
}
fn fail(index: usize, error: String) -> Outcome {
    Outcome::Failed { index, error }
}
#[cfg(test)]
#[path = "image_normalize_tests.rs"]
mod tests;
