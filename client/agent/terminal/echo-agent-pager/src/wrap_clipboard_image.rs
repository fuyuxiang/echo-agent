//! Host clipboard image paste mediated by `echoagent wrap`.
//!
//! On a full remote paste miss (no image/text/file URLs) with
//! `osc52_sink_active()`, remote emits a private OSC on stderr; wrap injects a
//! bracketed-paste frame on PTY stdin for the normal paste-chip path.
//!
//! # Trust model
//!
//! Answering the private request OSC is effectively an image clipboard *read*
//! for the wrapped session: any process that can write to the PTY (not only
//! the inner `echoagent`) can solicit the host pasteboard. That is intentional and
//! acceptable for `echoagent wrap` because (1) the user opted into wrap on their
//! own host, (2) the answer stays inside their session, and (3) the remote
//! only requests when `osc52_sink_active()` (wrap already set
//! `ECHO_AGENT_OSC52_SINK` / `LC_ECHO_AGENT_OSC52_SINK`). Do not generalize this pattern
//! to untrusted multiplexers without an explicit allowlist.

use base64::Engine as _;
use echo_agent_pager_render::clipboard::{ImageData, osc52_sink_active};

/// OSC body after `ESC ]` for a host image request.
pub const REQUEST_BODY: &[u8] = b"999;EchoAgentWrapClipboardImage?";

/// Full request sequence written to stderr by the remote pager (`ESC ]` body `BEL`).
pub fn request_osc_bytes() -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + REQUEST_BODY.len() + 1);
    v.push(0x1b);
    v.push(b']');
    v.extend_from_slice(REQUEST_BODY);
    v.push(0x07);
    v
}

/// Successful host image frame: `ECHO_AGENT_WRAP_IMG\n<mime>\n<base64>`.
pub const MAGIC_IMG: &str = "ECHO_AGENT_WRAP_IMG";

/// Host has no image (`ECHO_AGENT_WRAP_NONE`; not a prefix of [`MAGIC_IMG`]).
pub const MAGIC_NONE: &str = "ECHO_AGENT_WRAP_NONE";

/// Max decoded image bytes on this path (OSC 52 text limits unchanged).
/// Retina screenshots are often multi‑MB PNG; 4 MiB was too small and
/// silently became [`MAGIC_NONE`]. 20 MiB covers typical screenshots;
/// host inject may JPEG-recompress if still over budget.
pub const MAX_WRAP_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// Result of decoding a wrap-injected bracketed paste.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WrapImagePaste {
    Image(ImageData),
    /// Explicit host "no image" — must not be inserted as text.
    NoImage,
}

/// Emit on a full remote miss; true only when the request was written and flushed.
pub fn maybe_request_wrap_host_image(
    local_image: Option<&ImageData>,
    local_text: Option<&str>,
    local_file_urls: Option<&str>,
) -> bool {
    maybe_request_wrap_host_image_with(
        osc52_sink_active(),
        local_image,
        local_text,
        local_file_urls,
        write_request_osc,
    )
}

fn maybe_request_wrap_host_image_with(
    sink_active: bool,
    local_image: Option<&ImageData>,
    local_text: Option<&str>,
    local_file_urls: Option<&str>,
    emit: impl FnOnce() -> std::io::Result<()>,
) -> bool {
    if !sink_active
        || local_image.is_some()
        || local_text.is_some_and(|t| !t.trim().is_empty())
        || local_file_urls.is_some_and(|u| !u.trim().is_empty())
    {
        return false;
    }
    emit().is_ok()
}

fn write_request_osc() -> std::io::Result<()> {
    use std::io::Write;
    echo_agent_runtime::util::with_locked_stderr(|stderr| {
        stderr.write_all(&request_osc_bytes())?;
        stderr.flush()
    })
}

/// Decode wrap host-image paste content (`Event::Paste` payload).
///
/// `None` → not wrap magic (caller treats as normal text). Malformed wrap
/// frames yield [`WrapImagePaste::NoImage`] so they never land as text.
pub fn try_decode_wrap_host_image_paste(text: &str) -> Option<WrapImagePaste> {
    if text == MAGIC_NONE {
        return Some(WrapImagePaste::NoImage);
    }
    let rest = text.strip_prefix(MAGIC_IMG)?;
    let Some(rest) = rest.strip_prefix('\n') else {
        return Some(WrapImagePaste::NoImage);
    };
    let Some((mime, b64)) = rest.split_once('\n') else {
        return Some(WrapImagePaste::NoImage);
    };
    if mime.is_empty() || b64.is_empty() {
        return Some(WrapImagePaste::NoImage);
    }
    let b64 = b64.trim_end();
    // Size-check before allocating the decoded buffer.
    let approx_decoded = b64.len().saturating_mul(3) / 4;
    if approx_decoded > MAX_WRAP_IMAGE_BYTES {
        return Some(WrapImagePaste::NoImage);
    }
    let Ok(data) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return Some(WrapImagePaste::NoImage);
    };
    if data.is_empty() || data.len() > MAX_WRAP_IMAGE_BYTES {
        return Some(WrapImagePaste::NoImage);
    }
    Some(WrapImagePaste::Image(ImageData {
        data,
        mime_type: mime.to_owned(),
    }))
}

/// Bracketed-paste bytes for wrap to inject into PTY stdin.
/// Oversized host images are JPEG-recompressed when possible so screenshots
/// still arrive instead of a silent [`MAGIC_NONE`].
pub fn encode_wrap_image_response(image: Option<&ImageData>) -> Vec<u8> {
    let Some(img) = image.filter(|i| !i.data.is_empty()) else {
        return format!("\x1b[200~{MAGIC_NONE}\x1b[201~").into_bytes();
    };
    let payload = fit_image_for_wrap(img);
    match payload {
        Some((mime, data)) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
            format!("\x1b[200~{MAGIC_IMG}\n{mime}\n{b64}\x1b[201~").into_bytes()
        }
        None => format!("\x1b[200~{MAGIC_NONE}\x1b[201~").into_bytes(),
    }
}

/// Max pixel area when decoding oversized clipboard images for recompression
/// (~48MP — large enough for retina screenshots, not a decompression bomb).
const MAX_WRAP_DECODE_PIXELS: u64 = 48 * 1024 * 1024;

/// Keep encoded bytes under [`MAX_WRAP_IMAGE_BYTES`], JPEG-recompressing if needed.
fn fit_image_for_wrap(img: &ImageData) -> Option<(String, Vec<u8>)> {
    if img.data.len() <= MAX_WRAP_IMAGE_BYTES {
        return Some((img.mime_type.clone(), img.data.clone()));
    }
    let decoded = {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&img.data))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(16_384);
        limits.max_image_height = Some(16_384);
        limits.max_alloc = Some(MAX_WRAP_DECODE_PIXELS.saturating_mul(4));
        reader.limits(limits);
        reader.decode().ok()?
    };
    // Prefer JPEG for size; step quality down until under budget.
    for quality in [85_u8, 70, 55, 40, 25] {
        let mut buf = std::io::Cursor::new(Vec::new());
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
        if decoded.write_with_encoder(enc).is_err() {
            continue;
        }
        let data = buf.into_inner();
        if !data.is_empty() && data.len() <= MAX_WRAP_IMAGE_BYTES {
            return Some(("image/jpeg".into(), data));
        }
    }
    // Last resort: half-res JPEG.
    let small = decoded.thumbnail(
        decoded.width().saturating_div(2).max(1),
        decoded.height().saturating_div(2).max(1),
    );
    let mut buf = std::io::Cursor::new(Vec::new());
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 60);
    small.write_with_encoder(enc).ok()?;
    let data = buf.into_inner();
    if data.is_empty() || data.len() > MAX_WRAP_IMAGE_BYTES {
        return None;
    }
    Some(("image/jpeg".into(), data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_png() -> ImageData {
        ImageData {
            data: vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3],
            mime_type: "image/png".into(),
        }
    }

    fn bracketed_inner(framed: &[u8]) -> &str {
        let s = std::str::from_utf8(framed).unwrap();
        s.strip_prefix("\x1b[200~")
            .and_then(|s| s.strip_suffix("\x1b[201~"))
            .expect("bracketed paste frame")
    }

    #[test]
    fn encode_decode_roundtrip_image() {
        let img = tiny_png();
        let decoded = try_decode_wrap_host_image_paste(bracketed_inner(
            &encode_wrap_image_response(Some(&img)),
        ))
        .expect("wrap paste");
        match decoded {
            WrapImagePaste::Image(got) => {
                assert_eq!(got.mime_type, "image/png");
                assert_eq!(got.data, img.data);
            }
            WrapImagePaste::NoImage => panic!("expected image"),
        }
    }

    #[test]
    fn encode_decode_no_image_response() {
        assert_eq!(
            try_decode_wrap_host_image_paste(bracketed_inner(&encode_wrap_image_response(None))),
            Some(WrapImagePaste::NoImage)
        );
    }

    #[test]
    fn decode_none_magic_literal() {
        assert_eq!(
            try_decode_wrap_host_image_paste(MAGIC_NONE),
            Some(WrapImagePaste::NoImage)
        );
    }

    #[test]
    fn garbage_is_not_wrap_paste() {
        assert_eq!(try_decode_wrap_host_image_paste("hello world"), None);
        assert_eq!(try_decode_wrap_host_image_paste(""), None);
        assert_eq!(try_decode_wrap_host_image_paste("ECHO_AGENT_WRAP_IM"), None);
    }

    #[test]
    fn malformed_img_frame_consumed_not_text() {
        assert_eq!(
            try_decode_wrap_host_image_paste("ECHO_AGENT_WRAP_IMG"),
            Some(WrapImagePaste::NoImage)
        );
        assert_eq!(
            try_decode_wrap_host_image_paste("ECHO_AGENT_WRAP_IMG\nbad"),
            Some(WrapImagePaste::NoImage)
        );
        assert_eq!(
            try_decode_wrap_host_image_paste("ECHO_AGENT_WRAP_IMG\nimage/png\n!!!"),
            Some(WrapImagePaste::NoImage)
        );
    }

    #[test]
    fn oversized_b64_rejected_before_decode() {
        let huge = "A".repeat((MAX_WRAP_IMAGE_BYTES / 3 + 10) * 4);
        let payload = format!("ECHO_AGENT_WRAP_IMG\nimage/png\n{huge}");
        assert_eq!(
            try_decode_wrap_host_image_paste(&payload),
            Some(WrapImagePaste::NoImage)
        );
    }

    #[test]
    fn oversized_image_encodes_as_none() {
        let img = ImageData {
            data: vec![0u8; MAX_WRAP_IMAGE_BYTES + 1],
            mime_type: "image/png".into(),
        };
        // Random bytes fail JPEG fit → NONE frame.
        let s = String::from_utf8_lossy(&encode_wrap_image_response(Some(&img))).into_owned();
        assert!(s.contains(MAGIC_NONE));
    }

    #[test]
    fn request_osc_matches_body() {
        let osc = request_osc_bytes();
        assert_eq!(&osc[2..osc.len() - 1], REQUEST_BODY);
        assert_eq!(osc.first().copied(), Some(0x1b));
        assert_eq!(osc.get(1).copied(), Some(b']'));
        assert_eq!(osc.last().copied(), Some(0x07));
    }

    #[test]
    fn host_image_request_reports_emitted() {
        let writes = std::cell::Cell::new(0);
        let emitted = maybe_request_wrap_host_image_with(true, None, None, None, || {
            writes.set(writes.get() + 1);
            Ok(())
        });

        assert!(emitted);
        assert_eq!(writes.get(), 1);
    }

    #[test]
    fn host_image_request_reports_not_emitted() {
        assert!(!maybe_request_wrap_host_image_with(
            false,
            None,
            None,
            None,
            || panic!("disabled sink must not write"),
        ));
        assert!(!maybe_request_wrap_host_image_with(
            true,
            None,
            Some("clipboard text"),
            None,
            || panic!("local payload must not request host image"),
        ));
        assert!(!maybe_request_wrap_host_image_with(
            true,
            None,
            None,
            None,
            || Err(std::io::Error::other("write failed")),
        ));
    }
}
