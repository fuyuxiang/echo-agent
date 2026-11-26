//! `image_edit` tool — edits or transforms images via the EchoAgent Imagine
//! `/images/edits` endpoint using one or more reference images.
//!
//! Use cases include likeness preservation, style transfer, subject lock,
//! remixing, and general image-to-image editing. The model chooses this
//! tool (instead of `image_gen`) when the user provides reference photos.
//!
//! Reference images are specified as filesystem paths or
//! `data:image/...;base64,...` URLs. The tool reads the bytes, compresses
//! them to fit API limits, and POSTs to the edit endpoint.
//!
//! Shares the same [`ImageGenClient`] and session credentials as
//! `image_gen` — no additional configuration is needed.

use std::io::Cursor;

use base64::Engine as _;
use image::ImageReader;

use crate::attribution::ToolConsumer;
use crate::implementations::echo_agent_build::image_gen::{ImageGenClient, ImageGenResponse};
use crate::types::output::{MediaGenOutput, ToolOutput};
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::SessionFolder;
use crate::types::tool::{ToolKind, ToolNamespace};
use crate::util::image_compress::{FilterType, ReEncodeParams, re_encode_under_limit};

pub(crate) const ECHO_AGENT_IMAGINE_EDIT_MODEL: &str = "echo-agent-imagine-image-quality";

/// Size/dimension limits for reference images sent to the Imagine API.
/// Tighter than the vision path; the backend returns 400 when exceeded.
const MAX_REF_RAW_BYTES: usize = 400 * 1024;
const MAX_REF_DIMENSION: u32 = 768;
const MIN_REF_DIMENSION: u32 = 256;
const REF_QUALITY_STEPS: &[u8] = &[80, 65, 50, 35];
const MAX_REF_DECODE_PIXELS: u64 = 12_000_000;

pub const IMAGE_EDIT_TOOL_NAME: &str = "image_edit";

// ---------------------------------------------------------------------------
// Compression
// ---------------------------------------------------------------------------

/// Compress a reference image to fit within Imagine API limits.
///
/// Returns `(bytes, mime)`. Small JPEG/PNG inputs pass through unchanged.
fn compress_reference(
    raw_bytes: Vec<u8>,
) -> Result<(Vec<u8>, &'static str), echo_agent_tool_runtime::ToolError> {
    // Fast path: small JPEG/PNG passes through unchanged. Other formats
    // (WebP, GIF, etc.) always re-encode to guarantee API-compatible output.
    if raw_bytes.len() <= MAX_REF_RAW_BYTES
        && let Some(kind) = infer::get(&raw_bytes)
    {
        match kind.mime_type() {
            "image/jpeg" => return Ok((raw_bytes, "image/jpeg")),
            "image/png" => return Ok((raw_bytes, "image/png")),
            _ => {}
        }
    }

    // Refuse to decode absurdly large images.
    let reader = ImageReader::new(Cursor::new(&raw_bytes))
        .with_guessed_format()
        .map_err(|_| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "could not detect image format for reference",
            )
        })?;

    if let Ok((w, h)) = reader.into_dimensions()
        && (w as u64) * (h as u64) > MAX_REF_DECODE_PIXELS
    {
        return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
            format!("image reference is too large to process ({w}\u{00d7}{h} pixels)",),
        ));
    }

    // `into_dimensions` consumed the reader; re-open to decode.
    let img = ImageReader::new(Cursor::new(&raw_bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.decode().ok())
        .ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "failed to decode image reference",
            )
        })?;

    let params = ReEncodeParams {
        max_bytes: MAX_REF_RAW_BYTES,
        max_side_px: MAX_REF_DIMENSION,
        // Imagine backend limits are side-based; no pixel-area cap applies.
        max_pixels: u64::MAX,
        min_side_px: MIN_REF_DIMENSION,
        quality_steps: REF_QUALITY_STEPS,
        filter: FilterType::Lanczos3,
    };

    let (buf, _w, _h, mime) = re_encode_under_limit(&img, &params).map_err(|e| {
        echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
            "could not compress image reference small enough for Imagine API: {e}"
        ))
    })?;

    Ok((buf, mime))
}

// ---------------------------------------------------------------------------
// Reference resolution
// ---------------------------------------------------------------------------

/// Resolve a reference (filesystem path or `data:image/...;base64,...` URL)
/// into a compressed data URL for the Imagine API.
async fn resolve_to_data_url(value: &str) -> Result<String, echo_agent_tool_runtime::ToolError> {
    let value = value.trim();
    // Accept `file://` URIs (e.g. an attachment's durable URI) by reading
    // the underlying path. Data URLs and bare paths are untouched.
    let value = value.strip_prefix("file://").unwrap_or(value);

    let raw_bytes = if value.starts_with("data:image/") {
        let comma = value.find(',').ok_or_else(|| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(
                "malformed data URL in image reference",
            )
        })?;
        if !value[..comma].contains(";base64") {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "image references only support base64 data URLs",
            ));
        }
        base64::engine::general_purpose::STANDARD
            .decode(&value[comma + 1..])
            .map_err(|e| {
                echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                    "invalid base64 in image reference: {e}"
                ))
            })?
    } else {
        tokio::fs::read(value).await.map_err(|e| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                "image reference not readable: {value} ({e})"
            ))
        })?
    };

    if raw_bytes.is_empty() {
        return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
            "image reference contained no data",
        ));
    }

    let (compressed, mime) = compress_reference(raw_bytes)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&compressed);
    Ok(format!("data:{mime};base64,{b64}"))
}

// ---------------------------------------------------------------------------
// Attachment reference resolution
// ---------------------------------------------------------------------------

/// Parse an attached-image reference token into its 1-based display number.
///
/// Accepts the forms the model naturally produces for an image the user
/// attached to the conversation: `[Image #1]`, `Image #1`, `image #1`, or
/// a bare `#1`. Returns `None` for anything else — filesystem paths and
/// `data:` / `file://` URLs fall through to direct resolution.
fn parse_attachment_token(value: &str) -> Option<usize> {
    let trimmed = value.trim();
    // Strip optional surrounding brackets: `[…]`.
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed)
        .trim();
    // Strip an optional leading `image` label (case-insensitive). The
    // 5-byte prefix is ASCII, so slicing at byte 5 stays on a boundary.
    let rest = match inner.get(..5).map(str::to_ascii_lowercase).as_deref() {
        Some("image") => inner[5..].trim_start(),
        _ => inner,
    };
    // Require the `#` sigil followed by a bare positive integer.
    let digits = rest.strip_prefix('#')?.trim();
    match digits.parse::<usize>() {
        Ok(n) if n >= 1 => Some(n),
        _ => None,
    }
}

/// Resolve a single `image` argument to a reference `resolve_to_data_url`
/// can read.
///
/// Attachment tokens (`[Image #N]`) are mapped to the durable reference
/// the shell recorded for the current turn; everything else (filesystem
/// paths, `data:` / `file://` URLs) passes through unchanged.
fn resolve_attachment_reference(
    reference: &str,
    attached: Option<&crate::types::resources::AttachedImages>,
) -> Result<String, echo_agent_tool_runtime::ToolError> {
    let Some(n) = parse_attachment_token(reference) else {
        return Ok(reference.to_owned());
    };
    let registry = attached.filter(|a| !a.0.is_empty()).ok_or_else(|| {
        // Tokens only resolve against the current message's attachments. An
        // empty registry usually means the image was attached in an earlier
        // message (cross-turn editing isn't supported yet), so steer the
        // model to ask for a re-attach rather than retry the dead token.
        echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
            "image reference {reference:?} matches no image attached to this message. If it was \
             attached earlier in the conversation, ask the user to re-attach it here; otherwise \
             pass an absolute filesystem path or a data: URL."
        ))
    })?;
    registry.reference_for(n).map(str::to_owned).ok_or_else(|| {
        let available: Vec<String> = registry
            .0
            .iter()
            .map(|(num, _)| format!("[Image #{num}]"))
            .collect();
        echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
            "image reference {reference:?} does not match any attached image. Available: {}.",
            available.join(", ")
        ))
    })
}

// ---------------------------------------------------------------------------
// Tool input / schema
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ImageEditInput {
    #[schemars(
        description = "A text description of the desired edit or transformation. Describe what the output image should look like, referencing the input image(s)."
    )]
    pub prompt: String,

    #[schemars(
        description = "Reference image(s) to condition the edit on. Each is one reference, in priority order: (1) a user attachment — its placeholder token, e.g. \"[Image #1]\" (attachments have no path you can see, so never invent one); (2) an absolute filesystem path the user gave you; (3) a `data:image/...;base64,...` URL."
    )]
    pub image: Vec<String>,

    #[serde(default = "default_aspect_ratio")]
    #[schemars(
        description = "The aspect ratio of the output image. For single-image edits this is ignored — the output matches the input image's aspect ratio. For multi-image edits, defaults to 'auto'. Supported values: 1:1, 16:9, 9:16, 4:3, 3:4, 3:2, 2:3, 2:1, 1:2, 19.5:9, 9:19.5, 20:9, 9:20, auto."
    )]
    pub aspect_ratio: String,
}

fn default_aspect_ratio() -> String {
    "auto".to_owned()
}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct ImageEditTool;

impl crate::types::tool_metadata::ToolMetadata for ImageEditTool {
    fn kind(&self) -> ToolKind {
        ToolKind::ImageGen
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        r##"Edit or transform existing image(s) via the EchoAgent Imagine API; use instead of image_gen for image-to-image work (preserve likeness, transfer style, remix). Returns the saved image's absolute path. When telling the user where it was saved, refer to it by its short session-relative path (e.g. `images/1.jpg`) rather than the absolute path, so it renders as a clickable link that opens the image. Each required `image` is one reference — a user-attachment token (e.g. "[Image #1]"), an absolute filesystem path, or a `data:image/...;base64,...` URL (see the `image` parameter for the resolution order and details)."##
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for ImageEditTool {
    type Args = ImageEditInput;
    type Output = ToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("image_edit").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "image_edit",
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Write),
            ..Default::default()
        }
    }

    #[tracing::instrument(
        name = "tool.image_edit",
        skip_all,
        fields(prompt_len = input.prompt.len(), num_images = input.image.len(), aspect_ratio = %input.aspect_ratio)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: ImageEditInput,
    ) -> Result<ToolOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        if input.image.is_empty() {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "image_edit requires at least one reference image. \
                 Use image_gen for text-only generation.",
            ));
        }

        let client = {
            let res = resources.lock().await;
            res.require::<ImageGenClient>()?.clone()
        };

        // Free / X Basic users are zero-limited on Imagine server-side; return
        // the upsell prose instead of a doomed request (shares `image_gen`'s
        // message and short-circuits before resolving any attachments).
        if client.is_tier_restricted() {
            return Ok(ToolOutput::Text(
                super::image_gen::TIER_RESTRICTED_UPSELL.into(),
            ));
        }

        // Snapshot the per-turn attachment registry so `[Image #N]` tokens
        // resolve to the real attachment (see `resolve_attachment_reference`).
        let attached_images = {
            let res = resources.lock().await;
            res.get::<crate::types::resources::AttachedImages>()
                .cloned()
        };

        // Resolve all references to compressed data URLs.
        let mut data_urls = Vec::with_capacity(input.image.len());
        for r in &input.image {
            let resolved = resolve_attachment_reference(r, attached_images.as_ref())?;
            data_urls.push(resolve_to_data_url(&resolved).await?);
        }
        tracing::info!(count = data_urls.len(), "resolved image references");

        let base = client.base_url().trim_end_matches('/');
        let url = format!("{base}/images/edits");

        let mut payload = serde_json::json!({
            "model": client.edit_model(),
            "prompt": input.prompt,
            "n": 1,
            "resolution": "1k",
            "response_format": "b64_json",
        });

        // API: single ref → "image" object; multiple → "images" array.
        // For single-image edits the API auto-detects aspect ratio from the
        // input image and ignores the `aspect_ratio` field. Only send it
        // for multi-image edits where the API needs an explicit ratio.
        let mut imgs: Vec<serde_json::Value> = data_urls
            .iter()
            .map(|u| serde_json::json!({ "url": u }))
            .collect();
        if imgs.len() == 1 {
            payload["image"] = imgs.pop().unwrap();
        } else {
            payload["images"] = serde_json::Value::Array(imgs);
            payload["aspect_ratio"] = serde_json::json!(input.aspect_ratio);
        }

        let sent_bearer = client.current_bearer().await;
        let req = client.post_json(&url, &payload, sent_bearer.as_deref());

        let response = req.send().await.map_err(|e| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                "Image edit API request failed: {e}"
            ))
        })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            client.record_401_attribution(ToolConsumer::ImageGen, sent_bearer.as_deref());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let truncated: String = body.chars().take(200).collect();
            tracing::warn!(http_status = %status, "Imagine edit API error: {truncated}");
            return Err(echo_agent_tool_runtime::ToolError::new(
                echo_agent_tool_runtime::ToolErrorKind::Custom,
                format!("Image edit failed with HTTP {status}: {truncated}"),
            )
            .with_details(serde_json::json!({"code": "http_failure", "status": status.as_u16()})));
        }

        let body = response.text().await.map_err(|e| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                "Failed to read image edit response body: {e}"
            ))
        })?;

        let resp_json: ImageGenResponse = serde_json::from_str(&body).map_err(|e| {
            let preview: String = body.chars().take(500).collect();
            tracing::warn!("Imagine edit API returned unparseable body: {preview}");
            echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                "Failed to parse image edit response: {e} — body preview: {preview}"
            ))
        })?;

        let b64_data = resp_json.b64_data().unwrap_or("");
        if b64_data.is_empty() {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Image edit returned no image data.",
            ));
        }

        let image_bytes = base64::engine::general_purpose::STANDARD
            .decode(b64_data)
            .map_err(|e| {
                echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                    "Failed to decode base64 image data: {e}"
                ))
            })?;

        let session_folder = {
            let res = resources.lock().await;
            res.require::<SessionFolder>()?.0.clone()
        };

        let absolute_path = client
            .writer()
            .save(&session_folder, &image_bytes, None)
            .await
            .map_err(|e| echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string()))?;

        tracing::info!(
            path = %absolute_path.display(),
            bytes = image_bytes.len(),
            "edited image saved to disk"
        );

        Ok(ToolOutput::ImageEdit(MediaGenOutput::new(absolute_path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx_with_call_id;

    #[test]
    fn tool_name_and_description() {
        let tool = ImageEditTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "image_edit"
        );
        assert!(
            crate::types::tool_metadata::ToolMetadata::description_template(&tool)
                .contains("Edit or transform")
        );
    }

    #[test]
    fn input_deserialization() {
        let input: ImageEditInput =
            serde_json::from_str(r#"{"prompt": "anime style", "image": ["/Users/me/photo.jpg"]}"#)
                .unwrap();
        assert_eq!(input.prompt, "anime style");
        assert_eq!(input.image, vec!["/Users/me/photo.jpg"]);
        assert_eq!(input.aspect_ratio, "auto");
    }

    #[test]
    fn input_requires_image() {
        // image field is required by schema — empty array is a runtime check.
        let input: ImageEditInput =
            serde_json::from_str(r#"{"prompt": "test", "image": []}"#).unwrap();
        assert!(input.image.is_empty());
    }

    #[tokio::test]
    async fn rejects_empty_image_array() {
        let tool = ImageEditTool;
        let resources = crate::types::resources::Resources::new();
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "test-call"),
            ImageEditInput {
                prompt: "test".into(),
                image: vec![],
                aspect_ratio: "auto".into(),
            },
        )
        .await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("at least one reference image"), "got: {err}");
    }

    #[tokio::test]
    async fn errors_when_client_missing() {
        let tool = ImageEditTool;
        let resources = crate::types::resources::Resources::new();
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "test-call"),
            ImageEditInput {
                prompt: "test".into(),
                image: vec!["/some/path.jpg".into()],
                aspect_ratio: "auto".into(),
            },
        )
        .await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing required resource"), "got: {err}");
    }

    // ── compress_reference ───────────────────────────────────────────

    fn tiny_jpeg() -> Vec<u8> {
        use image::{DynamicImage, RgbImage};
        let img = DynamicImage::ImageRgb8(RgbImage::new(2, 2));
        let mut buf = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut buf),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        buf
    }

    fn tiny_png() -> Vec<u8> {
        use image::{DynamicImage, RgbaImage};
        let img = DynamicImage::ImageRgba8(RgbaImage::new(2, 2));
        let mut buf = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    #[test]
    fn compress_small_jpeg_passthrough() {
        let jpeg = tiny_jpeg();
        let (out, mime) = compress_reference(jpeg.clone()).unwrap();
        assert_eq!(out, jpeg);
        assert_eq!(mime, "image/jpeg");
    }

    #[test]
    fn compress_small_png_passthrough() {
        let png = tiny_png();
        let (out, mime) = compress_reference(png.clone()).unwrap();
        assert_eq!(out, png);
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn compress_oversized_shrinks() {
        use image::{DynamicImage, RgbImage};
        let mut img = RgbImage::new(1600, 1600);
        for (i, px) in img.pixels_mut().enumerate() {
            let v = (i * 37 + 13) as u8;
            *px = image::Rgb([v, v.wrapping_add(80), v.wrapping_add(160)]);
        }
        let mut buf = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 100);
        DynamicImage::ImageRgb8(img)
            .write_with_encoder(enc)
            .unwrap();
        assert!(buf.len() > MAX_REF_RAW_BYTES);

        let (out, mime) = compress_reference(buf).unwrap();
        assert!(out.len() <= MAX_REF_RAW_BYTES);
        assert!(mime == "image/jpeg" || mime == "image/png");
    }

    // ── resolve_to_data_url ──────────────────────────────────────────

    #[tokio::test]
    async fn resolve_filesystem_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jpg");
        std::fs::write(&path, tiny_jpeg()).unwrap();
        let url = resolve_to_data_url(path.to_str().unwrap()).await.unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
    }

    #[tokio::test]
    async fn resolve_data_url_roundtrip() {
        let jpeg = tiny_jpeg();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);
        let input = format!("data:image/jpeg;base64,{b64}");
        let url = resolve_to_data_url(&input).await.unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
    }

    #[tokio::test]
    async fn resolve_missing_file_errors() {
        assert!(resolve_to_data_url("/nonexistent/image.jpg").await.is_err());
    }

    #[tokio::test]
    async fn resolve_malformed_data_url_errors() {
        assert!(resolve_to_data_url("data:image/jpeg").await.is_err());
    }

    #[tokio::test]
    async fn resolve_file_uri_reads_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jpg");
        std::fs::write(&path, tiny_jpeg()).unwrap();
        let uri = format!("file://{}", path.display());
        let url = resolve_to_data_url(&uri).await.unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"));
    }

    // ── parse_attachment_token ───────────────────────────────────────

    #[test]
    fn parse_attachment_token_accepts_known_forms() {
        assert_eq!(parse_attachment_token("[Image #1]"), Some(1));
        assert_eq!(parse_attachment_token("Image #2"), Some(2));
        assert_eq!(parse_attachment_token("image #3"), Some(3));
        assert_eq!(parse_attachment_token("[image #4]"), Some(4));
        assert_eq!(parse_attachment_token("Image#5"), Some(5));
        assert_eq!(parse_attachment_token("#6"), Some(6));
        assert_eq!(parse_attachment_token("  [Image #7]  "), Some(7));
    }

    #[test]
    fn parse_attachment_token_rejects_non_tokens() {
        assert_eq!(parse_attachment_token("/Users/me/photo.jpg"), None);
        assert_eq!(parse_attachment_token("data:image/png;base64,AAAA"), None);
        assert_eq!(parse_attachment_token("file:///tmp/x.png"), None);
        assert_eq!(parse_attachment_token("[Image #0]"), None);
        assert_eq!(parse_attachment_token("[Image #]"), None);
        assert_eq!(parse_attachment_token("Image one"), None);
        assert_eq!(parse_attachment_token(""), None);
    }

    // ── resolve_attachment_reference ─────────────────────────────────

    #[test]
    fn resolve_reference_passes_through_non_tokens() {
        let resolved = resolve_attachment_reference("/Users/me/photo.jpg", None).unwrap();
        assert_eq!(resolved, "/Users/me/photo.jpg");
    }

    #[test]
    fn resolve_reference_maps_token_to_registry() {
        let attached = crate::types::resources::AttachedImages(vec![
            (1, "/tmp/a.png".to_owned()),
            (2, "/tmp/b.png".to_owned()),
        ]);
        assert_eq!(
            resolve_attachment_reference("[Image #1]", Some(&attached)).unwrap(),
            "/tmp/a.png"
        );
        assert_eq!(
            resolve_attachment_reference("Image #2", Some(&attached)).unwrap(),
            "/tmp/b.png"
        );
    }

    #[test]
    fn resolve_reference_maps_by_number_not_position() {
        // After a mid-compose chip removal the surviving numbers are
        // non-contiguous (`#1`, `#3`). Resolution must key on the number,
        // not the list position, or `[Image #3]` would resolve to the wrong
        // file (or wrongly error).
        let attached = crate::types::resources::AttachedImages(vec![
            (1, "/tmp/first.png".to_owned()),
            (3, "/tmp/third.png".to_owned()),
        ]);
        assert_eq!(
            resolve_attachment_reference("[Image #3]", Some(&attached)).unwrap(),
            "/tmp/third.png"
        );
        // `[Image #2]` was removed → no match.
        assert!(resolve_attachment_reference("[Image #2]", Some(&attached)).is_err());
    }

    #[test]
    fn resolve_reference_token_without_registry_errors() {
        let err = resolve_attachment_reference("[Image #1]", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("re-attach"), "got: {err}");
    }

    #[test]
    fn resolve_reference_unmatched_number_errors() {
        let attached = crate::types::resources::AttachedImages(vec![(1, "/tmp/a.png".to_owned())]);
        let err = resolve_attachment_reference("[Image #2]", Some(&attached))
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not match"), "got: {err}");
        assert!(err.contains("[Image #1]"), "should list available: {err}");
    }
}
