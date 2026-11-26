//! Extract base64-encoded images from tool result text so they can be
//! sent as multimodal vision tokens instead of raw text.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::LazyLock;

use regex::Regex;

/// Image payload captured before text truncation for session harvest
/// (multimodal vision follow-ups).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ExtractedImage {
    pub data: String,
    pub mime_type: String,
}

pub struct ExtractionResult {
    pub text: String,
    pub images: Vec<ExtractedImage>,
}

/// In-text stand-in for a captured data-URI image. Keep in lockstep with
/// extract and tool-layer writers.
pub const IMAGE_CONTENT_PLACEHOLDER: &str = "[image content will be provided separately]";

/// Skip tiny decorative icons (favicons, spacer GIFs).
const MIN_PAYLOAD_LEN: usize = 1024;

/// Prevent OOM from pathological MCP tool output.
const MAX_PAYLOAD_LEN: usize = 10 * 1024 * 1024;

/// Cap per tool result to avoid flooding the context with vision tokens.
const MAX_IMAGES: usize = 5;

/// Prefix regex for `data:<mime>;base64,`. The payload is scanned manually
/// from prefix end so line-wrapped producers (Python `base64.encodebytes`,
/// OpenSSL, Perl `MIME::Base64`) round-trip byte-equal. The leading
/// `(?:[^a-zA-Z0-9]|^)` rejects word-internal matches like
/// `metadata:image/...`. Only raster MIME types `image_normalize` can
/// decode are matched. Groups: (1) full prefix, (2) MIME type.
static IMAGE_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?:[^a-zA-Z0-9]|^)",
        r"(data:(image/(?:png|jpeg|gif|webp|bmp|tiff))",
        r"(?:;[^\s,;]{1,120})*",
        r";base64,)",
    ))
    .unwrap()
});

/// Sister of [`IMAGE_PREFIX_RE`] for `data:application/pdf;base64,`.
static PDF_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?:[^a-zA-Z0-9]|^)",
        r"(data:application/pdf",
        r"(?:;[^\s,;]{1,120})*",
        r";base64,)",
    ))
    .unwrap()
});

fn next_prefix_after(prefix_positions: &[usize], pos: usize) -> Option<usize> {
    prefix_positions.iter().copied().find(|&p| p > pos)
}

#[inline]
fn is_base64_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')
}

/// Scan a base64 payload starting at `start`, returning the exclusive end.
///
/// Admits a greedy core run plus any number of `\r?\n[ \t]*<base64>+`
/// continuation chunks, so line-wrapped output round-trips byte-equal. A
/// chunk ending in `=` (real base64 padding) ends the scan. The scan is
/// also bounded by `end_cap` (the next URI prefix) so adjacent data URIs
/// do not bleed into each other.
///
/// Trade-off: pure base64-alphabet prose on the line after a payload IS
/// absorbed; the downstream integrity check in
/// `image_normalize::normalize_one` rejects the resulting corrupt image.
fn scan_payload_end(text: &str, start: usize, end_cap: usize) -> usize {
    let bytes = text.as_bytes();
    let cap = end_cap.min(bytes.len());
    let mut i = start;
    while i < cap && is_base64_byte(bytes[i]) {
        i += 1;
    }
    if i > start && bytes[i - 1] == b'=' {
        return i;
    }
    loop {
        let mut p = i;
        if p < cap && bytes[p] == b'\r' {
            p += 1;
        }
        if !(p < cap && bytes[p] == b'\n') {
            break;
        }
        p += 1;
        while p < cap && matches!(bytes[p], b' ' | b'\t') {
            p += 1;
        }
        let chunk_start = p;
        while p < cap && is_base64_byte(bytes[p]) {
            p += 1;
        }
        let chunk_len = p - chunk_start;
        if chunk_len == 0 {
            break;
        }
        i = p;
        if bytes[p - 1] == b'=' {
            break;
        }
    }
    i
}

/// Strip ASCII whitespace from a base64 payload; zero-alloc when clean.
fn strip_b64_whitespace(s: &str) -> Cow<'_, str> {
    if !s.bytes().any(|b| b.is_ascii_whitespace()) {
        return Cow::Borrowed(s);
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    Cow::Owned(String::from_utf8(bytes).expect("ascii by char-class invariant"))
}

/// Pre-cap before stripping so a malicious oversize payload doesn't force
/// a large allocation just to be rejected. 2× headroom for line-wrap.
const GROSS_PAYLOAD_PRE_CAP: usize = MAX_PAYLOAD_LEN * 2;

fn collect_prefix_positions(text: &str) -> Vec<usize> {
    let mut positions: Vec<usize> = IMAGE_PREFIX_RE
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.start()))
        .chain(
            PDF_PREFIX_RE
                .captures_iter(text)
                .filter_map(|c| c.get(1).map(|m| m.start())),
        )
        .collect();
    positions.sort_unstable();
    positions.dedup();
    positions
}

/// Strip `data:application/pdf;base64,...` URIs from MCP tool results.
/// Each match is replaced with a placeholder showing the approximate
/// decoded size — the model cannot interpret raw PDF bytes.
fn strip_pdf_data_uris(text: &str) -> Option<String> {
    let needle = b"data:application/pdf";
    let has_pdf = text
        .as_bytes()
        .windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle));
    if !has_pdf {
        return None;
    }
    let prefix_positions = collect_prefix_positions(text);
    let mut result = String::with_capacity(text.len());
    let mut last_end = 0;
    let mut matched = false;

    for caps in PDF_PREFIX_RE.captures_iter(text) {
        let Some(prefix) = caps.get(1) else { continue };
        let next_start = next_prefix_after(&prefix_positions, prefix.start()).unwrap_or(text.len());
        let payload_end = scan_payload_end(text, prefix.end(), next_start);
        let payload_span = &text[prefix.end()..payload_end];
        let size_kb = if payload_span.len() > GROSS_PAYLOAD_PRE_CAP {
            payload_span.len() * 3 / 4 / 1024
        } else {
            strip_b64_whitespace(payload_span).len() * 3 / 4 / 1024
        };
        matched = true;
        result.push_str(&text[last_end..prefix.start()]);
        let _ = write!(result, "[PDF attachment removed \u{2014} {size_kb} KB]");
        last_end = payload_end;
    }

    if !matched {
        return None;
    }

    result.push_str(&text[last_end..]);
    Some(result)
}

/// Scan `s` for data-URI images, replacing each with a placeholder and
/// capturing the payload bytes for downstream multimodal injection.
///
/// Returns `None` when nothing was modified.
fn scan_and_extract(s: &str) -> Option<(String, Vec<ExtractedImage>)> {
    if !s.contains("data:image") {
        return None;
    }
    let prefix_positions = collect_prefix_positions(s);

    let mut result = String::with_capacity(s.len());
    let mut images = Vec::new();
    let mut last_end = 0;

    for caps in IMAGE_PREFIX_RE.captures_iter(s) {
        let (Some(prefix), Some(mime_match)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        let next_start = next_prefix_after(&prefix_positions, prefix.start()).unwrap_or(s.len());
        let payload_end = scan_payload_end(s, prefix.end(), next_start);
        let payload_span = &s[prefix.end()..payload_end];

        if payload_span.len() > GROSS_PAYLOAD_PRE_CAP {
            result.push_str(&s[last_end..prefix.start()]);
            result.push_str("[large image removed]");
            last_end = payload_end;
            continue;
        }

        let cleaned = strip_b64_whitespace(payload_span);
        let payload_len = cleaned.len() - (cleaned.len() % 4);
        if payload_len < MIN_PAYLOAD_LEN {
            continue;
        }

        let mime = mime_match.as_str().to_owned();
        result.push_str(&s[last_end..prefix.start()]);

        if payload_len > MAX_PAYLOAD_LEN {
            result.push_str("[large image removed]");
        } else if images.len() >= MAX_IMAGES {
            result.push_str("[additional image omitted]");
        } else {
            let data = match cleaned {
                Cow::Borrowed(b) => b[..payload_len].to_owned(),
                Cow::Owned(mut o) => {
                    o.truncate(payload_len);
                    o
                }
            };
            images.push(ExtractedImage {
                data,
                mime_type: mime,
            });
            result.push_str(IMAGE_CONTENT_PLACEHOLDER);
        }

        last_end = payload_end;
    }

    if last_end == 0 {
        return None;
    }

    result.push_str(&s[last_end..]);
    Some((result, images))
}

/// Extract image data URIs from `text`, replacing each with a placeholder.
/// Small payloads and non-image data URIs survive; PDF data URIs are
/// stripped first. Owned-input convenience over [`try_extract_base64_images`]
/// — when nothing matched, the original `text` is returned unmodified.
pub fn extract_base64_images(text: String) -> ExtractionResult {
    try_extract_base64_images(&text).unwrap_or_else(|| ExtractionResult {
        text,
        images: Vec::new(),
    })
}

/// Borrowed-input variant: returns `Some` only when at least one URI was
/// matched (image captured, or PDF / oversize stripped). Returns `None`
/// on the no-op fast path so callers (e.g. the per-line scan inside
/// `extract_file_content_lines`) can avoid an allocation.
pub fn try_extract_base64_images(text: &str) -> Option<ExtractionResult> {
    let after_pdf = strip_pdf_data_uris(text);
    let input = after_pdf.as_deref().unwrap_or(text);
    match scan_and_extract(input) {
        Some((cleaned, images)) => Some(ExtractionResult {
            text: cleaned,
            images,
        }),
        // Propagate PDF-only modifications when no image URIs matched.
        None => after_pdf.map(|t| ExtractionResult {
            text: t,
            images: Vec::new(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(n: usize) -> String {
        "A".repeat(n)
    }

    #[test]
    fn no_images_returns_text_unchanged() {
        let input = "Plain text with no images.".to_owned();
        let result = extract_base64_images(input.clone());
        assert_eq!(result.text, input);
        assert!(result.images.is_empty());
    }

    #[test]
    fn no_data_uri_prefix_returns_unchanged() {
        let input = "iVBORw0KGgoAAAANSUhEUg== but not a data URI".to_owned();
        let result = extract_base64_images(input.clone());
        assert_eq!(result.text, input);
        assert!(result.images.is_empty());
    }

    #[test]
    fn single_image_extracted() {
        let p = payload(2000);
        let input = format!("Before ![logo](data:image/png;base64,{p}) after");
        let result = extract_base64_images(input);
        assert_eq!(
            result.text,
            "Before ![logo]([image content will be provided separately]) after"
        );
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime_type, "image/png");
        assert_eq!(result.images[0].data, p);
    }

    #[test]
    fn multiple_images_extracted() {
        let p1 = payload(2000);
        let p2 = payload(3000);
        let input =
            format!("First data:image/png;base64,{p1} middle data:image/jpeg;base64,{p2} end");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 2);
        assert_eq!(result.images[0].mime_type, "image/png");
        assert_eq!(result.images[0].data, p1);
        assert_eq!(result.images[1].mime_type, "image/jpeg");
        assert_eq!(result.images[1].data, p2);
        assert!(
            result
                .text
                .contains("[image content will be provided separately]")
        );
        assert!(!result.text.contains("base64,"));
    }

    #[test]
    fn non_image_mime_not_extracted() {
        let input = format!("data:text/plain;base64,{} end", payload(2000));
        let result = extract_base64_images(input.clone());
        assert_eq!(result.text, input);
        assert!(result.images.is_empty());
    }

    #[test]
    fn application_pdf_stripped_not_extracted() {
        let p = payload(2000);
        let input = format!("data:application/pdf;base64,{p} end");
        let result = extract_base64_images(input);
        assert!(result.images.is_empty());
        let expected_kb = 2000 * 3 / 4 / 1024;
        assert!(result.text.contains(&format!(
            "[PDF attachment removed \u{2014} {expected_kb} KB]"
        )));
        assert!(!result.text.contains("base64,"));
    }

    #[test]
    fn below_threshold_not_extracted() {
        let input = format!("data:image/gif;base64,{} end", payload(100));
        let result = extract_base64_images(input.clone());
        assert_eq!(result.text, input);
        assert!(result.images.is_empty());
    }

    #[test]
    fn max_images_cap() {
        let p = payload(2000);
        let mut input = String::new();
        for i in 0..10 {
            input.push_str(&format!("img{i} data:image/png;base64,{p} "));
        }
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), MAX_IMAGES);
        let omitted_count = result.text.matches("[additional image omitted]").count();
        assert_eq!(omitted_count, 5);
        assert_eq!(result.text.matches("[large image removed]").count(), 0);
    }

    #[test]
    fn oversized_payload_stripped_but_not_extracted() {
        let huge = payload(MAX_PAYLOAD_LEN + 4);
        let input = format!("before data:image/png;base64,{huge} after");
        let result = extract_base64_images(input);
        assert!(result.images.is_empty());
        assert!(result.text.contains("[large image removed]"));
        assert!(result.text.contains("before"));
        assert!(result.text.contains("after"));
        assert!(!result.text.contains(&huge));
    }

    #[test]
    fn word_internal_data_prefix_ignored() {
        let input = format!("metadata:image/png;base64,{} end", payload(2000));
        let result = extract_base64_images(input.clone());
        assert_eq!(result.text, input);
        assert!(result.images.is_empty());
    }

    #[test]
    fn mixed_image_and_non_image() {
        let img = payload(2000);
        let txt = payload(2000);
        let input = format!("data:image/png;base64,{img} middle data:text/html;base64,{txt} end");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime_type, "image/png");
        assert!(result.text.contains("data:text/html;base64,"));
    }

    #[test]
    fn whitespace_in_header_rejected() {
        let input = format!("data:image /png;base64,{} end", payload(2000));
        let result = extract_base64_images(input.clone());
        assert_eq!(result.text, input);
        assert!(result.images.is_empty());
    }

    #[test]
    fn case_insensitive_base64_marker() {
        let result = extract_base64_images(format!("data:image/png;Base64,{} end", payload(2000)));
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime_type, "image/png");
    }

    #[test]
    fn multi_param_header() {
        let result = extract_base64_images(format!(
            "data:image/jpeg;charset=utf-8;base64,{} end",
            payload(2000)
        ));
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime_type, "image/jpeg");
    }

    #[test]
    fn no_comma_after_data_prefix() {
        let input = "data:image/png;base64 with no comma".to_owned();
        let result = extract_base64_images(input.clone());
        assert_eq!(result.text, input);
        assert!(result.images.is_empty());
    }

    #[test]
    fn preserves_surrounding_text() {
        let p = payload(2000);
        let input = format!("Title: Ticket\ndata:image/png;base64,{p}<Comments: ok");
        let result = extract_base64_images(input);
        assert!(result.text.contains("Title: Ticket"));
        assert!(result.text.contains("Comments: ok"));
        assert!(
            result
                .text
                .contains("[image content will be provided separately]")
        );
        assert!(!result.text.contains(&p));
    }

    #[test]
    fn svg_xml_not_extracted() {
        let input = format!("data:image/svg+xml;base64,{} end", payload(2000));
        let result = extract_base64_images(input.clone());
        assert_eq!(result.text, input);
        assert!(result.images.is_empty());
    }

    #[test]
    fn unsupported_image_mime_not_extracted() {
        let input = format!("data:image/x-icon;base64,{} end", payload(2000));
        let result = extract_base64_images(input.clone());
        assert_eq!(result.text, input);
        assert!(result.images.is_empty());
    }

    #[test]
    fn payload_boundary_aligned_to_base64() {
        let p = payload(2000);
        let input = format!("data:image/png;base64,{p}X end");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].data, p);
    }

    #[test]
    fn webp_and_bmp_extracted() {
        let p = payload(2000);
        for mime in ["image/webp", "image/bmp", "image/tiff"] {
            let input = format!("data:{mime};base64,{p} end");
            let result = extract_base64_images(input);
            assert_eq!(result.images.len(), 1, "expected extraction for {mime}");
            assert_eq!(result.images[0].mime_type, mime);
        }
    }

    #[test]
    fn strip_pdf_single_uri() {
        let pdf_b64 = payload(4096);
        let input = format!("Before data:application/pdf;base64,{pdf_b64} after");
        let result = strip_pdf_data_uris(&input).unwrap();
        let expected_kb = 4096 * 3 / 4 / 1024;
        assert!(result.contains(&format!(
            "[PDF attachment removed \u{2014} {expected_kb} KB]"
        )));
        assert!(result.contains("Before"));
        assert!(result.contains("after"));
        assert!(!result.contains("base64,"));
    }

    #[test]
    fn strip_pdf_multiple_uris() {
        let p1 = payload(2048);
        let p2 = payload(8192);
        let input = format!(
            "first data:application/pdf;base64,{p1} middle data:application/pdf;base64,{p2} end"
        );
        let result = strip_pdf_data_uris(&input).unwrap();
        assert_eq!(result.matches("[PDF attachment removed").count(), 2);
        assert!(result.contains("first"));
        assert!(result.contains("middle"));
        assert!(result.contains("end"));
    }

    #[test]
    fn strip_pdf_no_pdf_returns_none() {
        let input = "Plain text with data:image/png;base64,AAAA stuff";
        assert!(strip_pdf_data_uris(input).is_none());
    }

    #[test]
    fn strip_pdf_mixed_with_images() {
        let pdf_b64 = payload(4096);
        let img_b64 = payload(2000);
        let input = format!(
            "data:application/pdf;base64,{pdf_b64} then data:image/png;base64,{img_b64} end"
        );
        let result = extract_base64_images(input);
        assert!(result.text.contains("[PDF attachment removed"));
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime_type, "image/png");
    }

    #[test]
    fn strip_pdf_case_insensitive() {
        let pdf_b64 = payload(4096);
        let input = format!("data:Application/PDF;Base64,{pdf_b64} end");
        let result = strip_pdf_data_uris(&input).unwrap();
        assert!(result.contains("[PDF attachment removed"));
        assert!(!result.contains("Base64,"));
    }

    #[test]
    fn strip_pdf_with_extra_params() {
        let pdf_b64 = payload(4096);
        let input = format!("data:application/pdf;charset=utf-8;base64,{pdf_b64} end");
        let result = strip_pdf_data_uris(&input).unwrap();
        assert!(result.contains("[PDF attachment removed"));
    }

    #[test]
    fn strip_pdf_size_calculation() {
        let pdf_b64 = payload(12288); // 12288 * 3/4 / 1024 = 9 KB
        let input = format!("data:application/pdf;base64,{pdf_b64} end");
        let result = strip_pdf_data_uris(&input).unwrap();
        assert!(result.contains("[PDF attachment removed \u{2014} 9 KB]"));
    }

    #[test]
    fn word_internal_pdf_prefix_ignored() {
        let input = format!("metadata:application/pdf;base64,{} end", payload(2000));
        assert!(strip_pdf_data_uris(&input).is_none());
    }

    #[test]
    fn lf_wrapped_payload_extracted_in_full() {
        // 76-column LF wrap (Python `base64.encodebytes` style).
        let line = "A".repeat(76);
        let wrapped = std::iter::repeat_n(line.as_str(), 30)
            .collect::<Vec<_>>()
            .join("\n");
        let input = format!("data:image/png;base64,{wrapped}<end");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1, "expected one image after LF wrap");
        let expected_len = 76 * 30;
        assert_eq!(result.images[0].data.len(), expected_len);
        assert!(result.images[0].data.chars().all(|c| c == 'A'));
        assert!(result.text.contains("end"), "trailing prose preserved");
    }

    #[test]
    fn crlf_wrapped_payload_stripped() {
        let line = "A".repeat(76);
        let wrapped = std::iter::repeat_n(line.as_str(), 20)
            .collect::<Vec<_>>()
            .join("\r\n");
        let input = format!("data:image/png;base64,{wrapped}<end");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].data.len(), 76 * 20);
        assert!(
            !result.images[0].data.contains(['\r', '\n']),
            "CR/LF should be stripped from payload"
        );
    }

    #[test]
    fn leading_line_indentation_stripped() {
        let line = "A".repeat(72);
        let body = format!(
            "{line}\n\t{line}\n  {line}\n    {line}\n  {line}\n\t{line}\n{line}\n{line}\n{line}\n{line}\n{line}\n{line}\n{line}\n{line}\n{line}\n{line}"
        );
        let input = format!("data:image/png;base64,{body}<end");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].data.len(), 72 * 16);
        assert!(
            !result.images[0]
                .data
                .chars()
                .any(|c| c.is_ascii_whitespace()),
            "whitespace should not appear in extracted data"
        );
    }

    /// Python `base64.encodebytes` short final padded line must round-trip.
    #[test]
    fn lf_wrapped_short_tail_with_padding_round_trips() {
        let full_line = "A".repeat(76);
        let body = std::iter::repeat_n(full_line.as_str(), 20)
            .collect::<Vec<_>>()
            .join("\n");
        let short_tail = "AAA=";
        let wrapped = format!("{body}\n{short_tail}");
        let input = format!("data:image/png;base64,{wrapped}<rest text after");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1, "padded short tail must be kept");
        let expected_len = 76 * 20 + short_tail.len();
        assert_eq!(result.images[0].data.len(), expected_len);
        assert!(result.images[0].data.ends_with("AAA="));
        assert!(result.text.contains("rest text after"));
    }

    /// Long 76-col-wrapped payload with 72-char unpadded trailing line
    /// must round-trip byte-equal.
    #[test]
    fn long_encoded_payload_wraps_round_trip() {
        let p = payload(133_300);
        let mut wrapped = String::with_capacity(p.len() + 1800);
        for (i, chunk) in p.as_bytes().chunks(76).enumerate() {
            if i > 0 {
                wrapped.push('\n');
            }
            wrapped.push_str(std::str::from_utf8(chunk).unwrap());
        }
        let input = format!("data:image/png;base64,{wrapped}<rest");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].data, p);
    }

    /// 3-aligned input lengths produce SHORT unpadded trailing lines from
    /// `base64.encodebytes` 76-col wrap; these tails must round-trip
    /// instead of being silently truncated.
    #[test]
    fn unpadded_3aligned_short_tail_round_trips() {
        let full_line = "A".repeat(76);
        let body = std::iter::repeat_n(full_line.as_str(), 18)
            .collect::<Vec<_>>()
            .join("\n");
        let tail = "BCDEFGHIJKLM";
        let wrapped = format!("{body}\n{tail}");
        let input = format!("data:image/png;base64,{wrapped}<eof");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1, "unpadded short tail must be kept");
        assert_eq!(result.images[0].data.len(), 76 * 18 + tail.len());
        assert!(result.images[0].data.ends_with("BCDEFGHIJKLM"));
        assert!(result.text.contains("eof"));
    }

    /// `GROSS_PAYLOAD_PRE_CAP` short-circuits before `strip_b64_whitespace`
    /// allocates.
    #[test]
    fn gross_payload_pre_cap_short_circuits() {
        let huge = "A".repeat(GROSS_PAYLOAD_PRE_CAP + 1024);
        let input = format!("before data:image/png;base64,{huge} after");
        let result = extract_base64_images(input);
        assert!(result.images.is_empty(), "pre-cap must not emit image");
        assert!(result.text.contains("[large image removed]"));
        assert!(result.text.contains("before"));
        assert!(result.text.contains("after"));
        assert!(!result.text.contains(&huge));
    }

    /// Trade-off pin: pure-alphanumeric prose immediately after a `\n`
    /// (no other terminator) IS absorbed; downstream integrity check
    /// then rejects the resulting corrupt image.
    #[test]
    fn prose_after_newline_is_absorbed_then_trimmed() {
        let p = payload(2000);
        let input = format!("data:image/png;base64,{p}\nComments: ok");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1);
        // 2000 + 8 ("Comments") = 2008 (mod 4 == 0). ": ok" stays in text.
        assert_eq!(result.images[0].data.len(), 2008);
        assert!(result.text.contains(": ok"));
    }

    #[test]
    fn payload_aligned_after_strip_admitted_whole() {
        // 257 * 4 = 1028 — exact mod-4 boundary kept whole (no spurious trim).
        let chunk = "A".repeat(257);
        let wrapped = format!("{chunk}\n{chunk}\n{chunk}\n{chunk}");
        let input = format!("data:image/png;base64,{wrapped} end");
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].data.len(), 1028);
    }

    #[test]
    fn strip_b64_whitespace_zero_alloc_when_clean() {
        let clean = "AAAABBBB";
        match strip_b64_whitespace(clean) {
            Cow::Borrowed(b) => assert_eq!(b, clean),
            Cow::Owned(_) => panic!("expected borrowed for whitespace-free input"),
        }
        match strip_b64_whitespace("AA\nBB") {
            Cow::Owned(o) => assert_eq!(o, "AABB"),
            Cow::Borrowed(_) => panic!("expected owned for input with whitespace"),
        }
    }

    /// Contract with `echo_agent_mcp::servers::format_mcp_image` dual-emit:
    /// the data URI becomes a vision token; the raw `<mcp_image_base64>`
    /// block survives verbatim for agent decoding (e.g. `send_file`).
    #[test]
    fn mcp_dual_emit_extracts_data_uri_keeps_raw_block() {
        let p = payload(2000);
        let input = format!(
            "data:image/png;base64,{p}\n\
             <mcp_image_base64 mime=\"image/png\">\n\
             {p}\n\
             </mcp_image_base64>"
        );
        let result = extract_base64_images(input);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime_type, "image/png");
        assert_eq!(result.images[0].data, p);
        assert!(
            result
                .text
                .contains("<mcp_image_base64 mime=\"image/png\">")
        );
        assert!(result.text.contains(&p));
        assert!(result.text.contains("</mcp_image_base64>"));
        assert!(
            result
                .text
                .contains("[image content will be provided separately]")
        );
    }

    // ─── try_extract_base64_images tests ──────────────────────────────

    #[test]
    fn try_extract_no_images_returns_none() {
        assert!(try_extract_base64_images("Plain text with no images.").is_none());
    }

    #[test]
    fn try_extract_captures_inline_image() {
        let p = payload(2000);
        let input = format!("before data:image/png;base64,{p} after");
        let result = try_extract_base64_images(&input).expect("URI must be captured");
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime_type, "image/png");
        assert_eq!(result.images[0].data, p);
        assert!(
            result
                .text
                .contains("[image content will be provided separately]")
        );
        assert!(!result.text.contains(&p));
    }

    #[test]
    fn try_extract_below_threshold_returns_none() {
        // MIN_PAYLOAD_LEN gate: tiny icons survive untouched.
        let input = format!("data:image/gif;base64,{} end", payload(100));
        assert!(try_extract_base64_images(&input).is_none());
    }

    #[test]
    fn try_extract_multiple_uris() {
        let p = payload(2000);
        let input = format!("first data:image/png;base64,{p} mid data:image/jpeg;base64,{p} end");
        let result = try_extract_base64_images(&input).expect("two URIs must be captured");
        assert_eq!(result.images.len(), 2);
        assert_eq!(result.images[0].mime_type, "image/png");
        assert_eq!(result.images[1].mime_type, "image/jpeg");
        assert_eq!(
            result
                .text
                .matches("[image content will be provided separately]")
                .count(),
            2
        );
    }

    /// PDF-only input: image-scan is a no-op, but PDF strip still runs
    /// and propagates the modification through.
    #[test]
    fn try_extract_pdf_only_propagates_strip() {
        let pdf_b64 = payload(4096);
        let input = format!("Before data:application/pdf;base64,{pdf_b64} after");
        let result = try_extract_base64_images(&input).expect("PDF must be stripped");
        assert!(result.images.is_empty());
        assert!(result.text.contains("[PDF attachment removed"));
        assert!(!result.text.contains("base64,"));
    }

    /// Long single-line URI must be captured byte-equal before
    /// `truncate_line` could cut it mid-payload.
    #[test]
    fn try_extract_runs_before_truncation_would_corrupt_payload() {
        let p = payload(50_000);
        let input = format!("![logo](data:image/png;base64,{p})");
        let result = try_extract_base64_images(&input).expect("long URI must be captured");
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].data.len(), 50_000);
        assert_eq!(result.images[0].data, p);
        assert!(result.text.len() < 200);
        assert!(
            result
                .text
                .contains("[image content will be provided separately]")
        );
        assert!(result.text.starts_with("![logo]("));
        assert!(result.text.ends_with(')'));
    }
}
