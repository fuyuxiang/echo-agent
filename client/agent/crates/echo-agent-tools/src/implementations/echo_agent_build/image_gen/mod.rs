//! `image_gen` tool — generates images via the EchoAgent Imagine API and saves
//! them to the local filesystem so the model can reference them in code
//! (e.g. `<img src="images/hero.jpg">`).
//!
//! Architecture follows the same pattern as `web_search`:
//!
//! - [`ImageGenConfig`] is built from session credentials by the host and
//!   injected into the tool registry.
//! - When `Enabled`, an [`ImageGenClient`] is constructed once and injected
//!   into `Resources`. The tool reads it at runtime via `resources.require()`.
//! - When `Disabled`, the tool is not registered so the model never sees it.
//!
//! The generated image is written to `<session_folder>/images/<n>.jpg`
//! where `<n>` is a session-scoped counter (1, 2, 3, ... — 1 token each).
//! The tool returns the absolute path so the model can copy or move the
//! image into the project working directory when it needs a persistent asset.

use base64::Engine as _;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};

use crate::attribution::{SharedAttributionCallback, ToolConsumer};
use crate::types::SharedApiKeyProvider;

use crate::types::output::{MediaGenOutput, ToolOutput};
use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::resources::SessionFolder;
use crate::types::tool::{ToolKind, ToolNamespace};

/// Default Imagine model for `image_gen`. Used unless an explicit
/// `model_override` is supplied via `ImageGenConfig::Enabled`.
const ECHO_AGENT_IMAGINE_MODEL: &str = "echo-agent-imagine-image-quality";
// Some Imagine models (e.g. `echo-agent-imagine-image`, selectable via `model_override`)
// expand the prompt then generate, and the proxy buffers
// the whole image before sending any bytes — so the client may receive nothing
// for well over a minute. Keep these generous so a slow-but-progressing
// generation isn't cut off.
const IMAGE_GEN_TIMEOUT_SECS: u64 = 300;
const IMAGE_GEN_READ_TIMEOUT_SECS: u64 = 240;
const DEFAULT_IMAGE_DIR: &str = "images";

pub use echo_agent_tools_api::slash_commands::{
    IMAGE_GEN_TOOL_NAME, IMAGINE_COMMAND_NAME, imagine_instruction, imagine_usage_message,
};

/// Prose returned to the model (as a normal, successful tool result) when a
/// free / X Basic user calls `image_gen` or `image_edit`. The model relays it
/// to the user. The deliberate `/imagine` slash command shows the richer
/// EchoAgent upsell modal instead; this covers the natural-language path.
pub(crate) const TIER_RESTRICTED_UPSELL: &str = "Image generation is a EchoAgent feature and isn't available on the free or X Basic tier. Let the user know they can unlock image and video generation by upgrading to EchoAgent: https://cloud.echo-agent.invalid/echoagent?referrer=echo-agent-build. Do not retry this tool.";

/// HTTP client for EchoAgent Imagine API. Cloned per-request; shares `Arc` state.
#[derive(Clone)]
pub struct ImageGenClient {
    http: reqwest::Client,
    base_url: String,
    /// Imagine model slug used by `generate()`. Selected at construction
    /// from `ImageGenConfig::model_override` (falling back to
    /// [`ECHO_AGENT_IMAGINE_MODEL`]). `image_edit` uses its own model and is
    /// unaffected.
    model: String,
    edit_model: String,
    writer: super::storage::SessionFileWriter,
    api_key_provider: Option<SharedApiKeyProvider>,
    /// Optional 401-attribution hook. Hosts wire this so a 401 from the
    /// Imagine API emits an `auth_401_attribution` event with
    /// `consumer == "ImageGen"` for unified auth-failure telemetry.
    attribution_callback: Option<SharedAttributionCallback>,
    /// When `true`, the user is on a tier the Imagine server zero-limits
    /// (free / X Basic). `image_gen` / `image_edit` short-circuit before any
    /// HTTP call and return the EchoAgent upsell prose instead. See
    /// [`ImageGenClient::is_tier_restricted`].
    tier_restricted: bool,
    /// Per-request [`SESSION_ID_HEADER`]; kept off `default_headers` so the
    /// transport stays session-independent and cacheable.
    session_header: Option<HeaderValue>,
    defaults_have_session_header: bool,
}

impl ImageGenClient {
    pub fn new(
        config: &ImageGenConfig,
        api_key_provider: Option<SharedApiKeyProvider>,
    ) -> Result<Self, echo_agent_tool_runtime::ToolError> {
        let ImageGenConfig::Enabled {
            api_key,
            base_url,
            extra_headers,
            model_override,
            edit_model_override,
            tier_restricted,
            ..
        } = config
        else {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Cannot create ImageGenClient from disabled config",
            ));
        };
        let model = model_override
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| ECHO_AGENT_IMAGINE_MODEL.to_owned());
        let edit_model = edit_model_override
            .clone()
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| super::image_edit::ECHO_AGENT_IMAGINE_EDIT_MODEL.to_owned());

        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        // Always bake the static api_key as the default Authorization header.
        // The dynamic provider overrides per-request; this is the fallback.
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|e| {
                echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                    "Invalid API key for header: {e}"
                ))
            })?,
        );

        extra_headers.into_iter().try_for_each(|(key, value)| {
            let header_name =
                reqwest::header::HeaderName::from_bytes(key.as_bytes()).map_err(|e| {
                    echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                        "Invalid header name '{key}': {e}"
                    ))
                })?;
            let header_value = HeaderValue::from_str(value).map_err(|e| {
                echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                    "Invalid header value for '{key}': {e}"
                ))
            })?;
            headers.insert(header_name, header_value);
            Ok::<(), echo_agent_tool_runtime::ToolError>(())
        })?;

        // Process-cached: timeouts are constants, so the headers key
        // suffices; the session id is attached per request, not here.
        let defaults_have_session_header = headers.contains_key(SESSION_ID_HEADER);
        let key = crate::util::shared_http::cache_key("image_gen", &headers);
        let http = crate::util::shared_http::cached_client(key, || {
            echo_agent_extra_ca::build_reqwest_client(|builder| {
                builder
                    .timeout(std::time::Duration::from_secs(IMAGE_GEN_TIMEOUT_SECS))
                    .read_timeout(std::time::Duration::from_secs(IMAGE_GEN_READ_TIMEOUT_SECS))
                    .default_headers(headers.clone())
            })
        })
        .map_err(|e| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                "Failed to build HTTP client: {e}"
            ))
        })?;

        Ok(Self {
            http,
            base_url: base_url.clone(),
            model,
            edit_model,
            writer: super::storage::SessionFileWriter::new(DEFAULT_IMAGE_DIR, "jpg"),
            api_key_provider,
            attribution_callback: None,
            tier_restricted: *tier_restricted,
            session_header: None,
            defaults_have_session_header,
        })
    }

    /// Attach [`SESSION_ID_HEADER`] per request; a caller-provided
    /// `extra_headers` value is never overridden.
    pub fn with_session_id(mut self, session_id: &str) -> Self {
        if !self.defaults_have_session_header
            && let Ok(value) = HeaderValue::from_str(session_id)
        {
            self.session_header = Some(value);
        }
        self
    }

    /// Whether the current user's tier (free / X Basic) is zero-limited on
    /// Imagine server-side. `image_gen` / `image_edit` use this to short-circuit
    /// with the EchoAgent upsell instead of issuing a doomed request.
    pub(crate) fn is_tier_restricted(&self) -> bool {
        self.tier_restricted
    }

    /// Wire a 401-attribution callback into this client. Idempotent;
    /// safe to call before or after the first request. Builder-style
    /// so `new()` callers that don't care can ignore it.
    pub fn with_attribution_callback(
        mut self,
        callback: Option<SharedAttributionCallback>,
    ) -> Self {
        self.attribution_callback = callback;
        self
    }

    pub(crate) async fn current_bearer(&self) -> Option<String> {
        crate::types::api_key_provider::resolve_bearer(self.api_key_provider.as_ref()).await
    }

    pub(crate) fn record_401_attribution(&self, consumer: ToolConsumer, sent_bearer: Option<&str>) {
        crate::attribution::emit_401(self.attribution_callback.as_ref(), consumer, sent_bearer);
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Every Imagine-API POST goes through here so no call site can miss
    /// the bearer or per-request session header (image_edit once did).
    pub(crate) fn post_json(
        &self,
        url: &str,
        payload: &serde_json::Value,
        sent_bearer: Option<&str>,
    ) -> reqwest::RequestBuilder {
        let mut req = self.http.post(url).json(payload);
        if let Some(key) = sent_bearer {
            req = req.header(AUTHORIZATION, format!("Bearer {key}"));
        }
        if let Some(ref session) = self.session_header {
            req = req.header(SESSION_ID_HEADER, session.clone());
        }
        req
    }

    pub(crate) fn writer(&self) -> &super::storage::SessionFileWriter {
        &self.writer
    }

    pub(crate) fn edit_model(&self) -> &str {
        &self.edit_model
    }

    pub async fn generate(
        &self,
        prompt: &str,
        aspect_ratio: &str,
    ) -> Result<Vec<u8>, echo_agent_tool_runtime::ToolError> {
        let url = format!("{}/images/generations", self.base_url.trim_end_matches('/'));

        let payload = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "n": 1,
            "aspect_ratio": aspect_ratio,
            "resolution": "1k",
            "response_format": "b64_json",
        });

        // Capture the bearer once so the request and the 401-attribution
        // emit see the same value (even if the provider rotates between
        // the send and the response handling).
        let sent_bearer = self.current_bearer().await;
        let req = self.post_json(&url, &payload, sent_bearer.as_deref());

        let response = req.send().await.map_err(|e| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                "Image generation API request failed: {e}"
            ))
        })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.record_401_attribution(ToolConsumer::ImageGen, sent_bearer.as_deref());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let truncated: String = body.chars().take(200).collect();
            tracing::warn!(http_status = %status, "Imagine API error: {truncated}");
            return Err(echo_agent_tool_runtime::ToolError::new(
                echo_agent_tool_runtime::ToolErrorKind::Custom,
                format!("Image generation failed with HTTP {status}: {truncated}"),
            )
            .with_details(serde_json::json!({"code": "http_failure", "status": status.as_u16()})));
        }

        let body = response.text().await.map_err(|e| {
            echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                "Failed to read image generation response body: {e}"
            ))
        })?;

        let resp_json: ImageGenResponse = serde_json::from_str(&body).map_err(|e| {
            let preview: String = body.chars().take(500).collect();
            tracing::warn!("Imagine API returned unparseable body: {preview}");
            echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                "Failed to parse image generation response: {e} — body preview: {preview}"
            ))
        })?;

        let b64_data = resp_json.b64_data().unwrap_or("");

        if b64_data.is_empty() {
            return Err(echo_agent_tool_runtime::ToolError::invalid_arguments(
                "Image generation returned no image data.",
            ));
        }

        base64::engine::general_purpose::STANDARD
            .decode(b64_data)
            .map_err(|e| {
                echo_agent_tool_runtime::ToolError::invalid_arguments(format!(
                    "Failed to decode base64 image data: {e}"
                ))
            })
    }
}

/// `Enabled` means credentials are present; each tool has its own gate.
#[derive(Debug, Clone, Default)]
pub enum ImageGenConfig {
    #[default]
    Disabled,
    Enabled {
        api_key: String,
        base_url: String,
        extra_headers: indexmap::IndexMap<String, String>,
        image_gen_enabled: bool,
        image_edit_enabled: bool,
        /// Optional Imagine model override for `image_gen`. When `Some(non-empty)`,
        /// `image_gen` calls that model instead of the default quality model
        /// ([`ECHO_AGENT_IMAGINE_MODEL`]). Driven by the remote
        /// `image_gen_model_override` config flag. `image_edit` is unaffected.
        model_override: Option<String>,
        edit_model_override: Option<String>,
        /// `true` when the user is on a tier the Imagine server zero-limits
        /// (free / X Basic). The tools stay advertised to the model, but
        /// `image_gen` / `image_edit` short-circuit at call time with the
        /// EchoAgent upsell prose instead of a doomed request. Set by the
        /// host from the subscription tier; always `false` for team /
        /// API-key / workspace callers.
        tier_restricted: bool,
    },
}

/// Session-id header attached to imagine API requests; matches the header
/// chat requests already carry.
pub const SESSION_ID_HEADER: &str = "x-echo-agent-session-id";

impl ImageGenConfig {
    /// Credentials present — required to construct any of the clients.
    pub fn has_credentials(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }

    pub fn image_gen_enabled(&self) -> bool {
        matches!(
            self,
            Self::Enabled {
                image_gen_enabled: true,
                ..
            }
        )
    }

    pub fn image_edit_enabled(&self) -> bool {
        matches!(
            self,
            Self::Enabled {
                image_edit_enabled: true,
                ..
            }
        )
    }

    /// The configured `image_gen` model override, if any. `None` means the
    /// default quality model ([`ECHO_AGENT_IMAGINE_MODEL`]) is used.
    pub fn model_override(&self) -> Option<&str> {
        match self {
            Self::Enabled { model_override, .. } => {
                model_override.as_deref().filter(|m| !m.trim().is_empty())
            }
            Self::Disabled => None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ImageGenInput {
    #[schemars(description = "Text description of the image to generate.")]
    pub prompt: String,

    #[serde(default = "default_aspect_ratio")]
    #[schemars(
        description = "Aspect ratio of the generated image, decide it based on the user's request. Defaults to 'auto'. 1:1 for square (icons, profiles), 16:9 for wide (landscapes, cinematic), 9:16 for tall (phone wallpapers, stories), 3:2 for horizontal photos, 2:3 for vertical (portraits, posters)."
    )]
    pub aspect_ratio: String,
}

fn default_aspect_ratio() -> String {
    "auto".to_owned()
}

#[derive(Debug, serde::Deserialize)]
pub struct ImageGenResponse {
    #[serde(default)]
    data: Vec<ImageGenData>,
}

impl ImageGenResponse {
    pub fn b64_data(&self) -> Option<&str> {
        self.data.first().and_then(|d| d.b64_json.as_deref())
    }
}

#[derive(Debug, serde::Deserialize)]
struct ImageGenData {
    b64_json: Option<String>,
}

#[derive(Debug, Default)]
pub struct ImageGenTool;

impl crate::types::tool_metadata::ToolMetadata for ImageGenTool {
    fn kind(&self) -> ToolKind {
        ToolKind::ImageGen
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        "Generate a new image from a text description using Imagine; returns the saved image's absolute path. When telling the user where it was saved, refer to it by its short session-relative path (e.g. `images/1.jpg`) rather than the absolute path, so it renders as a clickable link that opens the image. To produce multiple images, emit multiple tool calls with distinct prompts."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for ImageGenTool {
    type Args = ImageGenInput;
    type Output = ToolOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new("image_gen").expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            "image_gen",
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
        name = "tool.image_gen",
        skip_all,
        fields(prompt_len = input.prompt.len(), aspect_ratio = %input.aspect_ratio)
    )]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: ImageGenInput,
    ) -> Result<ToolOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        let client = {
            let res = resources.lock().await;
            res.require::<ImageGenClient>()?.clone()
        };

        // Free / X Basic users are zero-limited on Imagine server-side; return
        // the upsell prose instead of a doomed request (the tool stays
        // advertised so the model can surface the nudge in-conversation).
        if client.is_tier_restricted() {
            return Ok(ToolOutput::Text(TIER_RESTRICTED_UPSELL.into()));
        }

        let image_bytes = client.generate(&input.prompt, &input.aspect_ratio).await?;

        let session_folder = {
            let res = resources.lock().await;
            res.require::<SessionFolder>()?.0.clone()
        };

        let absolute_path = client
            .writer
            .save(&session_folder, &image_bytes, None)
            .await
            .map_err(|e| echo_agent_tool_runtime::ToolError::invalid_arguments(e.to_string()))?;

        tracing::info!(
            path = %absolute_path.display(),
            bytes = image_bytes.len(),
            "image saved to disk"
        );

        Ok(ToolOutput::ImageGen(MediaGenOutput::new(absolute_path)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::tool_metadata::test_ctx_with_call_id;

    #[test]
    fn tool_name_and_description() {
        let tool = ImageGenTool;
        assert_eq!(
            echo_agent_tool_runtime::Tool::id(&tool).as_str(),
            "image_gen"
        );
        assert!(
            crate::types::tool_metadata::ToolMetadata::description_template(&tool)
                .contains("Generate a new image from a text description")
        );
    }

    #[test]
    fn default_aspect_ratio_is_auto() {
        let input: ImageGenInput = serde_json::from_str(r#"{"prompt": "test"}"#).unwrap();
        assert_eq!(input.aspect_ratio, "auto");
    }

    #[test]
    fn per_tool_gates_are_independent() {
        let cfg = ImageGenConfig::Enabled {
            api_key: "k".into(),
            base_url: "https://api.echo-agent.invalid/v1".into(),
            extra_headers: indexmap::IndexMap::new(),
            image_gen_enabled: false,
            image_edit_enabled: true,
            model_override: Some("echo-agent-imagine-image".into()),
            edit_model_override: None,
            tier_restricted: false,
        };
        assert!(cfg.has_credentials());
        assert!(!cfg.image_gen_enabled());
        assert!(cfg.image_edit_enabled());
        assert_eq!(cfg.model_override(), Some("echo-agent-imagine-image"));

        assert!(!ImageGenConfig::Disabled.has_credentials());
    }

    #[test]
    fn with_session_id_defers_to_caller_configured_header() {
        let mut preset = indexmap::IndexMap::new();
        preset.insert(SESSION_ID_HEADER.to_string(), "caller-set".to_string());
        let cfg = ImageGenConfig::Enabled {
            api_key: "k".into(),
            base_url: "https://api.echo-agent.invalid/v1".into(),
            extra_headers: preset,
            image_gen_enabled: true,
            image_edit_enabled: true,
            model_override: None,
            edit_model_override: None,
            tier_restricted: false,
        };
        let client = ImageGenClient::new(&cfg, None)
            .unwrap()
            .with_session_id("sess-1");
        assert!(client.session_header.is_none());

        let cfg_plain = ImageGenConfig::Enabled {
            api_key: "k".into(),
            base_url: "https://api.echo-agent.invalid/v1".into(),
            extra_headers: indexmap::IndexMap::new(),
            image_gen_enabled: true,
            image_edit_enabled: true,
            model_override: None,
            edit_model_override: None,
            tier_restricted: false,
        };
        let client = ImageGenClient::new(&cfg_plain, None)
            .unwrap()
            .with_session_id("sess-1");
        assert_eq!(
            client.session_header.as_ref().and_then(|v| v.to_str().ok()),
            Some("sess-1")
        );
    }

    // Pins the image_edit wire regression: every POST routes through
    // post_json, which attaches both bearer and session id.
    #[tokio::test]
    async fn post_json_attaches_session_and_bearer_headers() {
        let cfg = ImageGenConfig::Enabled {
            api_key: "k".into(),
            base_url: "https://api.echo-agent.invalid/v1".into(),
            extra_headers: indexmap::IndexMap::new(),
            image_gen_enabled: true,
            image_edit_enabled: true,
            model_override: None,
            edit_model_override: None,
            tier_restricted: false,
        };
        let client = ImageGenClient::new(&cfg, None)
            .unwrap()
            .with_session_id("sess-42");
        let req = client
            .post_json(
                "https://api.echo-agent.invalid/v1/images",
                &serde_json::json!({}),
                Some("tok"),
            )
            .build()
            .unwrap();
        assert_eq!(
            req.headers()
                .get(SESSION_ID_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some("sess-42")
        );
        assert_eq!(
            req.headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer tok")
        );
    }

    #[test]
    fn client_selects_model_from_override() {
        let mk = |model_override: Option<&str>| ImageGenConfig::Enabled {
            api_key: "k".into(),
            base_url: "https://api.echo-agent.invalid/v1".into(),
            extra_headers: indexmap::IndexMap::new(),
            image_gen_enabled: true,
            image_edit_enabled: true,
            model_override: model_override.map(String::from),
            edit_model_override: None,
            tier_restricted: false,
        };
        // No override → default quality model.
        assert_eq!(
            ImageGenClient::new(&mk(None), None).unwrap().model,
            ECHO_AGENT_IMAGINE_MODEL
        );
        // Empty override → treated as no override.
        assert_eq!(
            ImageGenClient::new(&mk(Some("")), None).unwrap().model,
            ECHO_AGENT_IMAGINE_MODEL
        );
        // Override → that exact model slug.
        assert_eq!(
            ImageGenClient::new(&mk(Some("echo-agent-imagine-image")), None)
                .unwrap()
                .model,
            "echo-agent-imagine-image"
        );
    }

    #[test]
    fn client_selects_edit_model_from_override() {
        let mk = |edit_model_override: Option<&str>| ImageGenConfig::Enabled {
            api_key: "k".into(),
            base_url: "https://api.echo-agent.invalid/v1".into(),
            extra_headers: indexmap::IndexMap::new(),
            image_gen_enabled: true,
            image_edit_enabled: true,
            model_override: None,
            edit_model_override: edit_model_override.map(String::from),
            tier_restricted: false,
        };
        assert_eq!(
            ImageGenClient::new(&mk(None), None).unwrap().edit_model(),
            super::super::image_edit::ECHO_AGENT_IMAGINE_EDIT_MODEL
        );
        assert_eq!(
            ImageGenClient::new(&mk(Some("  ")), None)
                .unwrap()
                .edit_model(),
            super::super::image_edit::ECHO_AGENT_IMAGINE_EDIT_MODEL
        );
        let client = ImageGenClient::new(&mk(Some("echo-agent-imagine-image-v2")), None).unwrap();
        assert_eq!(client.edit_model(), "echo-agent-imagine-image-v2");
        assert_eq!(client.model, ECHO_AGENT_IMAGINE_MODEL);
    }

    #[tokio::test]
    async fn errors_when_client_missing() {
        let tool = ImageGenTool;
        let resources = crate::types::resources::Resources::new();
        let result = echo_agent_tool_runtime::Tool::run(
            &tool,
            test_ctx_with_call_id(resources.into_shared(), "test-call"),
            ImageGenInput {
                prompt: "a test image".into(),
                aspect_ratio: "auto".into(),
            },
        )
        .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("missing required resource"),
            "Expected MissingResource error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn tier_restricted_short_circuits_with_upsell() {
        // A free / X Basic user's image_gen call returns the EchoAgent upsell
        // prose as a normal result (no HTTP, no error card) so the model can
        // relay it. Only the client is inserted — the short-circuit returns
        // before any other resource (e.g. SessionFolder) is required.
        let cfg = ImageGenConfig::Enabled {
            api_key: "k".into(),
            base_url: "https://api.echo-agent.invalid/v1".into(),
            extra_headers: indexmap::IndexMap::new(),
            image_gen_enabled: true,
            image_edit_enabled: true,
            model_override: None,
            edit_model_override: None,
            tier_restricted: true,
        };
        let mut resources = crate::types::resources::Resources::new();
        resources.insert(ImageGenClient::new(&cfg, None).unwrap());

        let result = echo_agent_tool_runtime::Tool::run(
            &ImageGenTool,
            test_ctx_with_call_id(resources.into_shared(), "test-call"),
            ImageGenInput {
                prompt: "a cat".into(),
                aspect_ratio: "auto".into(),
            },
        )
        .await
        .expect("tier-restricted call must succeed with upsell prose");

        match result {
            ToolOutput::Text(t) => {
                assert!(t.text.contains("EchoAgent"), "got: {}", t.text);
                assert!(t.text.contains("echoagent?referrer=echo-agent-build"));
            }
            other => panic!("expected Text upsell, got {other:?}"),
        }
    }
}
