//! Multi-provider API key configuration.
//!
//! The runtime config in `~/.echo-agent/config.toml` supports two separate
//! concepts that together model "one provider, many models":
//!
//!   - `[model_providers.<id>]` — a connection/auth profile (base_url,
//!     api_key, api_backend, auth_scheme, context_window, extra_headers).
//!   - `[model.<id>]` — a single model catalog entry that may reference a
//!     provider via `model_provider = "<id>"`, inheriting its connection
//!     config. The runtime merges the provider defaults into each model in
//!     `resolve_model_list` (see vendor/.../config.rs).
//!
//! This is the runtime's preferred shape for BYOK: one key/url stored once per
//! provider, shared by every model that points at it. We expose a typed façade
//! over that file so the frontend can list/save without learning TOML or
//! the underlying schema.
//!
//! Storage rules:
//!   - Keys live in owner-only `config.toml`; `api_key` is the runtime's
//!     highest-priority credential source.
//!   - We **merge** rather than overwrite: any keys we don't recognize are
//!     preserved, so a user who hand-edits config.toml doesn't lose tweaks.
//!   - **Lazy migration**: legacy `[model.*]` tables that still carry their
//!     own `api_key`/`base_url` (the old "one table per model" shape) are
//!     grouped for *display* by base_url+api_key into synthetic providers,
//!     but the disk file is only rewritten when the user actively saves.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::State;
use toml::map::Map;
use toml::Value;

use futures::StreamExt;

const ORGANIZATION_PROVIDER_ID: &str = "echoagent-organization";
const ORGANIZATION_MODEL_PREFIX: &str = "organization/";
pub(crate) const BUILTIN_PROVIDER_ID: &str = "echoagent-ojlab";
pub(crate) const BUILTIN_MODEL_PREFIX: &str = "echoagent-ojlab/";
pub(crate) const BUILTIN_DEFAULT_MODEL_ID: &str = "echoagent-ojlab/Echo Reed";
// Exact server-side chat model IDs. Retrieval models must never enter this catalog.
const BUILTIN_CHAT_MODELS: &[(&str, i64, i64)] = &[
    ("Echo Cedar", 131_072, 8_192),
    ("Echo Willow", 32_768, 8_192),
    ("Echo Grove", 262_144, 8_192),
    ("Echo Brook", 131_072, 8_192),
    ("Echo Reed", 262_144, 65_536),
];
const MANAGED_BY_KEY: &str = "echoagent_managed_by";
const MANAGED_BY_ORGANIZATION: &str = "organization";
const MANAGED_BY_BUILTIN: &str = "builtin";
const LABEL_KEY: &str = "echoagent_label";
const ALLOW_INSECURE_HTTP_KEY: &str = "echoagent_allow_insecure_http";
const SYNCED_AT_KEY: &str = "echoagent_synced_at";
const LEASE_UNTIL_KEY: &str = "echoagent_lease_until";
const ALLOW_INSECURE_LOOPBACK_HTTP_ENV: &str = "ECHO_AGENT_ALLOW_INSECURE_LOOPBACK_HTTP";
const MAX_PROVIDER_ERROR_BYTES: usize = 64 * 1024;
const MAX_INFERENCE_PROBE_BYTES: usize = 1024 * 1024;
const MAX_MODEL_LIST_BYTES: usize = 4 * 1024 * 1024;
const MAX_DISCOVERED_MODELS: usize = 10_000;
const MAX_RUNTIME_CONFIG_BYTES: u64 = 8 * 1024 * 1024;

fn insecure_loopback_http_enabled() -> bool {
    std::env::var(ALLOW_INSECURE_LOOPBACK_HTTP_ENV)
        .ok()
        .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"))
}

fn is_ojlab_base_url(raw: &str) -> bool {
    raw.trim().trim_end_matches('/') == crate::agent_runtime::OJLAB_BASE_URL
}

fn provider_can_omit_api_key(table: &Map<String, Value>) -> bool {
    table
        .get("base_url")
        .and_then(Value::as_str)
        .is_some_and(is_ojlab_base_url)
        && table.get("api_backend").and_then(Value::as_str) == Some("chat_completions")
}

fn validate_provider_base_url_with_policy(
    raw: &str,
    allow_insecure_loopback: bool,
) -> Result<url::Url, String> {
    validate_provider_base_url_with_options(raw, allow_insecure_loopback, false)
}

fn validate_provider_base_url_with_options(
    raw: &str,
    allow_insecure_loopback: bool,
    allow_explicit_http: bool,
) -> Result<url::Url, String> {
    let parsed =
        url::Url::parse(raw.trim()).map_err(|error| format!("Base URL 格式不正确：{error}"))?;
    if parsed.host_str().is_none() {
        return Err("Base URL 必须包含主机名".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Base URL 不得内嵌用户名或密码".into());
    }
    if parsed.fragment().is_some() {
        return Err("Base URL 不得包含 fragment".into());
    }
    if parsed
        .path()
        .strip_prefix("/:")
        .and_then(|path| path.split('/').next())
        .is_some_and(|segment| {
            !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(
            "端口号写在路径里了：请把 /:端口/ 改成 :端口/，例如 http://example.com:60100/v1".into(),
        );
    }
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" => {
            if is_ojlab_base_url(raw) || allow_explicit_http {
                return Ok(parsed);
            }
            let exact_loopback = parsed.host_str().is_some_and(|host| {
                matches!(host, "localhost" | "127.0.0.1") || host.trim_matches(['[', ']']) == "::1"
            });
            if !exact_loopback {
                return Err("远程 Base URL 必须使用 HTTPS".into());
            }
            if !allow_insecure_loopback {
                return Err(format!(
                    "本机 HTTP 仅用于开发；如确有需要，启动前设置 {ALLOW_INSECURE_LOOPBACK_HTTP_ENV}=1"
                ));
            }
            Ok(parsed)
        }
        _ => Err("Base URL 必须使用 HTTPS".into()),
    }
}

fn validate_provider_base_url(raw: &str) -> Result<url::Url, String> {
    validate_provider_base_url_with_policy(raw, insecure_loopback_http_enabled())
}

fn validate_personal_provider_base_url(
    raw: &str,
    allow_insecure_http: bool,
) -> Result<url::Url, String> {
    validate_provider_base_url_with_options(
        raw,
        insecure_loopback_http_enabled(),
        allow_insecure_http,
    )
}

fn provider_http_client(timeout: std::time::Duration) -> Result<reqwest::Client, String> {
    // Provider credentials must never be replayed through an implicit redirect.
    // API base URLs are explicit configuration; callers can save the canonical
    // endpoint if their gateway intentionally lives elsewhere.
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("创建 HTTP 客户端失败：{error}"))
}

async fn provider_response_bytes(
    response: reqwest::Response,
    limit: usize,
) -> Result<(Vec<u8>, bool), String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取 API 响应失败：{error}"))?;
        let remaining = limit.saturating_add(1).saturating_sub(bytes.len());
        if remaining == 0 {
            return Ok((bytes, true));
        }
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if bytes.len() > limit {
            bytes.truncate(limit);
            return Ok((bytes, true));
        }
    }
    Ok((bytes, false))
}

// ---------------------------------------------------------------------------
// Built-in presets (endpoint + wire protocol per provider_kind).
// ---------------------------------------------------------------------------

/// Built-in preset: endpoint + wire protocol + auth header style per provider.
/// `base_url` is None for "custom-like" kinds (custom / custom_anthropic) where
/// the user must supply the endpoint, but the protocol/auth are still preset.
struct ProviderPreset {
    base_url: Option<&'static str>,
    api_backend: &'static str,
    auth_scheme: &'static str,
}

fn preset(kind: &str) -> Option<ProviderPreset> {
    match kind {
        "anthropic" => Some(ProviderPreset {
            base_url: Some("https://api.anthropic.com/v1"),
            api_backend: "messages",
            auth_scheme: "x_api_key",
        }),
        "openai" => Some(ProviderPreset {
            base_url: Some("https://api.openai.com/v1"),
            // OpenAI's current model API and coding/reasoning models are
            // exposed through Responses. EchoAgent remains the only agent
            // runtime; this provider is transport/authentication only.
            api_backend: "responses",
            auth_scheme: "bearer",
        }),
        "minimax" => Some(ProviderPreset {
            base_url: Some("https://api.minimax.cn/v1"),
            api_backend: "chat_completions",
            auth_scheme: "bearer",
        }),
        "deepseek" => Some(ProviderPreset {
            base_url: Some("https://api.deepseek.com"),
            api_backend: "chat_completions",
            auth_scheme: "bearer",
        }),
        "qwen" => Some(ProviderPreset {
            // 通义千问 OpenAI-compatible endpoint.
            base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
            api_backend: "chat_completions",
            auth_scheme: "bearer",
        }),
        // Anthropic-compatible custom endpoint: protocol/auth locked to the
        // Anthropic wire shape, but the user must supply base_url.
        "custom_anthropic" => Some(ProviderPreset {
            base_url: None,
            api_backend: "messages",
            auth_scheme: "x_api_key",
        }),
        // `custom` (OpenAI-compatible) intentionally has no preset at all —
        // caller must supply every field.
        _ => None,
    }
}

/// Reverse-map a provider by sniffing base_url + api_backend. Falls back to
/// `custom` for anything unrecognized so we never silently drop a user's entry.
fn infer_provider_kind(table: &Map<String, Value>) -> String {
    let backend = table
        .get("api_backend")
        .and_then(Value::as_str)
        .unwrap_or("");
    let base = table.get("base_url").and_then(Value::as_str).unwrap_or("");
    match backend {
        "messages" => {
            if base.contains("api.anthropic.com") {
                "anthropic".into()
            } else {
                "custom_anthropic".into()
            }
        }
        "chat_completions" | "responses" => {
            if base.contains("api.openai.com") {
                "openai".into()
            } else if base.contains("api.minimax.cn") || base.contains("api.minimaxi.com") {
                "minimax".into()
            } else if base.contains("api.deepseek.com") {
                "deepseek".into()
            } else if base.contains("dashscope.aliyuncs.com") {
                "qwen".into()
            } else {
                "custom".into()
            }
        }
        _ => "custom".into(),
    }
}

/// Validate an api_backend value. Empty string (treat as "unset") is allowed.
fn validate_api_backend(v: &str) -> Result<(), String> {
    match v {
        "" | "chat_completions" | "responses" | "messages" => Ok(()),
        other => Err(format!(
            "invalid api_backend '{other}': must be chat_completions | responses | messages"
        )),
    }
}

/// Validate an auth_scheme value. Empty string (treat as "unset") is allowed.
fn validate_auth_scheme(v: &str) -> Result<(), String> {
    match v {
        "" | "bearer" | "x_api_key" => Ok(()),
        other => Err(format!(
            "invalid auth_scheme '{other}': must be bearer | x_api_key"
        )),
    }
}

// ---------------------------------------------------------------------------
// Config I/O (shared with sibling modules).
// ---------------------------------------------------------------------------

fn config_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("config.toml")
}

/// Serialize complete config read-modify-write transactions inside the desktop
/// process. Locking the individual read and write calls is insufficient: two
/// callers could otherwise both read the same snapshot and overwrite one
/// another's changes.
static CONFIG_TRANSACTION: OnceLock<Mutex<()>> = OnceLock::new();

fn config_transaction_lock() -> &'static Mutex<()> {
    CONFIG_TRANSACTION.get_or_init(|| Mutex::new(()))
}

/// Stable, secret-safe fingerprint of the model-related Runtime configuration.
///
/// The digest lets the desktop shell prove that the in-process Runtime has
/// acknowledged the exact configuration currently on disk. The source values
/// (including API keys) never cross the Tauri boundary or enter logs.
pub(crate) fn model_config_revision() -> String {
    model_config_revision_of(&read_config())
}

/// Pure digest of the model-related sections of a config value.
fn model_config_revision_of(config: &Value) -> String {
    let mut relevant = Map::new();
    let table = config.as_table();
    // `internal/reload_models` refreshes Config::models (the whole `[models]`
    // table: default model, allowed/hidden/disabled filters, search and summary
    // model pins), Config::config_models and Config::model_providers. All three
    // must be part of the digest: a filter-only edit changes what the Runtime
    // catalog exposes, so a revision that ignored `[models]` could not prove the
    // Runtime had applied the configuration currently on disk. Other sections
    // are runtime/startup settings this endpoint never applies; including them
    // would permanently lock chat after an unrelated save.
    for key in ["model", "models", "model_providers"] {
        if let Some(value) = table.and_then(|table| table.get(key)) {
            relevant.insert(key.to_string(), value.clone());
        }
    }
    let encoded = toml::to_string(&Value::Table(relevant)).unwrap_or_default();
    let digest = Sha256::digest(encoded.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Read config.toml as a TOML value, or an empty table if missing/corrupt.
/// (Corrupt → back up to `config.toml.corrupt.<millis>` so we don't silently
/// clobber the user's file.)
///
/// Exposed `pub(crate)` so sibling modules (permission_config) can reuse the
/// same atomic read-modify-write pattern without each re-implementing it.
pub(crate) fn read_config() -> Value {
    let _guard = config_transaction_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = config_path();
    let _shared_transaction =
        match echo_agent_runtime::util::config::acquire_config_transaction_lock_at(&path) {
            Ok(transaction) => transaction,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "failed to lock Runtime config for read");
                return Value::Table(Map::new());
            }
        };
    read_config_unlocked()
}

/// Read settings without quarantining or replacing unreadable user data.
fn read_config_checked_unlocked() -> Result<Value, String> {
    let path = config_path();
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Table(Map::new()))
        }
        Err(error) => return Err(format!("无法读取配置：{error}")),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("配置不是普通文件".into())
        }
        Ok(_) => {}
    }
    let bytes = crate::shell_fs::read_regular_file_bounded(&path, MAX_RUNTIME_CONFIG_BYTES)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| format!("配置编码无效：{error}"))?;
    text.parse::<Value>()
        .map_err(|error| format!("配置无法解析，请修复后重试：{error}"))
}

pub(crate) fn read_config_checked() -> Result<Value, String> {
    update_config_checked(|config| Ok(config.clone()))
}

pub(crate) fn update_config_checked<T>(
    update: impl FnOnce(&mut Value) -> Result<T, String>,
) -> Result<T, String> {
    let _guard = config_transaction_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let path = config_path();
    let _transaction = echo_agent_runtime::util::config::acquire_config_transaction_lock_at(&path)
        .map_err(|error| format!("无法锁定配置：{error}"))?;
    let mut config = read_config_checked_unlocked()?;
    let original = config.clone();
    let result = update(&mut config)?;
    if config != original {
        write_config_unlocked(&config)?;
    }
    Ok(result)
}

fn read_config_unlocked() -> Value {
    let path = config_path();
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Value::Table(Map::new());
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "failed to inspect Runtime config");
            return Value::Table(Map::new());
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            tracing::warn!(path = %path.display(), "Runtime config is not a regular file");
            return Value::Table(Map::new());
        }
        Ok(_) => {}
    }
    let bytes = match crate::shell_fs::read_regular_file_bounded(&path, MAX_RUNTIME_CONFIG_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "failed to read Runtime config");
            quarantine_invalid_config(&path, "unreadable or exceeds 8MB");
            return Value::Table(Map::new());
        }
    };
    match std::str::from_utf8(&bytes)
        .ok()
        .and_then(|contents| contents.parse::<Value>().ok())
    {
        Some(config) => config,
        None => {
            quarantine_invalid_config(&path, "invalid UTF-8 or TOML");
            Value::Table(Map::new())
        }
    }
}

fn quarantine_invalid_config(path: &std::path::Path, reason: &str) {
    let backup = path.with_extension(format!("toml.corrupt.{}", uuid::Uuid::now_v7().simple()));
    if let Err(error) = std::fs::rename(path, &backup) {
        tracing::warn!(path = %path.display(), %error, %reason, "failed to quarantine invalid Runtime config");
    } else {
        let _ = crate::paths::harden_private_file(&backup);
        tracing::warn!(path = %path.display(), backup = %backup.display(), %reason, "invalid Runtime config was quarantined");
    }
}

fn write_config_unlocked(v: &Value) -> Result<(), String> {
    let path = config_path();
    let body = toml::to_string_pretty(v).map_err(|e| format!("serialize config: {e}"))?;
    if body.len() as u64 > MAX_RUNTIME_CONFIG_BYTES {
        return Err("Runtime config exceeds 8MB limit".into());
    }
    crate::paths::write_private_file(&path, body.as_bytes())
}

fn approved_insecure_http_urls(config: &Value) -> Vec<String> {
    config
        .get("model_providers")
        .and_then(Value::as_table)
        .into_iter()
        .flat_map(|providers| providers.iter())
        .filter_map(|(id, provider)| provider.as_table().map(|table| (id.as_str(), table)))
        .filter(
            |(id, provider)| match provider.get(MANAGED_BY_KEY).and_then(Value::as_str) {
                None => true,
                Some(MANAGED_BY_ORGANIZATION) => *id == ORGANIZATION_PROVIDER_ID,
                _ => false,
            },
        )
        .filter(|(_, provider)| {
            provider
                .get(ALLOW_INSECURE_HTTP_KEY)
                .and_then(Value::as_bool)
                == Some(true)
        })
        .filter_map(|(_, provider)| provider.get("base_url").and_then(Value::as_str))
        .filter(|url| {
            validate_provider_base_url_with_options(url, false, true)
                .is_ok_and(|parsed| parsed.scheme() == "http")
        })
        .map(str::to_owned)
        .collect()
}

fn sync_insecure_http_approvals_from_config(config: &Value) {
    echo_agent_sampler::replace_approved_insecure_http_urls(approved_insecure_http_urls(config));
}

pub(crate) fn sync_insecure_http_approvals() {
    sync_insecure_http_approvals_from_config(&read_config());
}

/// Atomically mutate the latest config snapshot while holding the process-wide
/// transaction lock across both the read and the replace. The upstream Runtime
/// and desktop writers also share an advisory file lock, so async MCP/settings
/// updates cannot overwrite a desktop-side synchronous update. Sibling modules
/// must use this API for every read-modify-write operation.
pub(crate) fn update_config<T>(
    update: impl FnOnce(&mut Value) -> Result<T, String>,
) -> Result<T, String> {
    let _guard = config_transaction_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = config_path();
    let _shared_transaction =
        echo_agent_runtime::util::config::acquire_config_transaction_lock_at(&path)
            .map_err(|error| format!("lock Runtime config: {error}"))?;
    let mut config = read_config_unlocked();
    let original = config.clone();
    let result = update(&mut config)?;
    // Status/lease checks are frequent. Do not atomically replace config.toml
    // when their mutation is a no-op: besides needless disk churn, a replace
    // wakes Runtime config watchers even though the semantic revision is stable.
    if config != original {
        write_config_unlocked(&config)?;
    }
    sync_insecure_http_approvals_from_config(&config);
    Ok(result)
}

// ---------------------------------------------------------------------------
// Frontend-facing data model: providers + models.
// ---------------------------------------------------------------------------

/// One connection/auth profile as the frontend sees it. Written to
/// `[model_providers.<id>]`. `api_key`: None = unchanged, Some("") = cleared,
/// Some("x") = set. When read back it is masked as `"••••"`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderEntry {
    /// The `[model_providers.<id>]` key; models reference it via
    /// `model_provider = "<id>"`. Stable id derived from provider_kind.
    pub id: String,
    /// `anthropic` | `openai` | `deepseek` | `qwen` | `custom`.
    pub provider_kind: String,
    /// User-facing connection name. EchoAgent stores it as desktop metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The secret. None = unchanged, Some("") = cleared, Some("x") = set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_backend: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<String>,
    /// Max context window in tokens. The runtime accepts this at the provider level
    /// (shared by all referencing models) — see ModelProviderConfig.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Personal connection opt-in for transmitting credentials and prompts via HTTP.
    #[serde(default)]
    pub allow_insecure_http: bool,
    /// `personal` | `organization` | `legacy`. Derived from stored metadata.
    #[serde(default)]
    pub source: String,
    /// Managed entries are read-only and can only be changed by org sync.
    #[serde(default)]
    pub managed: bool,
    /// Whether a usable credential is present without exposing the secret.
    #[serde(default)]
    pub credential_configured: bool,
    /// Organization sync timestamp in Unix milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<u64>,
    /// Original provider type sent by the organization server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_provider: Option<String>,
}

/// One model catalog entry as the frontend sees it. Written to
/// `[model.<model_id>]` with a `model_provider` reference.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelEntry {
    /// Stable local catalog key used by EchoAgent sessions and selectors.
    pub model_id: String,
    /// Exact upstream model slug sent in requests. Defaults to `model_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_model_id: Option<String>,
    /// References `[model_providers.<id>]`.
    pub provider_id: String,
    /// Human-readable display name (the runtime's `name` field, used in selectors).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Per-model context-window override (wins over the provider's value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Maximum response length supported by this model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Request-only flag: an omitted limit keeps the stored override; clearing it is explicit.
    #[serde(default, skip_serializing)]
    pub clear_max_output_tokens: bool,
    #[serde(default)]
    pub managed: bool,
}

/// The list result: every provider + every model, joined by `provider_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderListModel {
    pub providers: Vec<ModelProviderEntry>,
    pub models: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveConnectionResult {
    pub provider_id: String,
    pub model_ids: Vec<String>,
}

/// Complete chat configuration downloaded after organization sign-in.
///
/// It is intentionally written through the same config.toml schema as a
/// provider/model created in Settings, so the embedded Runtime needs no
/// organization-specific sampling path.
pub(crate) struct OrganizationModelConfig {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub lease_until: u64,
}

// ---------------------------------------------------------------------------
// Read path (with lazy grouping of legacy per-model entries).
// ---------------------------------------------------------------------------

fn resolved_provider_api_key(table: &Map<String, Value>) -> Option<String> {
    table
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(String::from)
        .or_else(|| {
            let env_keys = table.get("env_key")?;
            let names = match env_keys {
                Value::String(name) => vec![name.as_str()],
                Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            names.into_iter().find_map(|name| {
                std::env::var(name)
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
        })
}

fn resolved_or_optional_api_key(table: &Map<String, Value>) -> Option<String> {
    resolved_provider_api_key(table).or_else(|| provider_can_omit_api_key(table).then(String::new))
}

/// Secret-bearing provider context used only by native meeting jobs.  It is
/// deliberately never serialized across the Tauri boundary.
pub(crate) struct MeetingProviderAccess {
    pub api_key: String,
    pub base_url: String,
    pub remote_model_id: String,
}

/// Resolve a meeting model's chat connection without exposing its credential
/// to the frontend. Speech-to-Text support is checked against the actual
/// endpoint separately; neither the model name nor the provider brand proves it.
fn meeting_provider_access_from_config(
    config: &Value,
    model_id: &str,
    expected_provider_id: &str,
) -> Result<MeetingProviderAccess, String> {
    let model = config
        .get("model")
        .and_then(Value::as_table)
        .and_then(|models| models.get(model_id))
        .and_then(Value::as_table)
        .ok_or("当前模型已不存在，请重新选择会议模型")?;
    // Current configurations reference a shared provider. Legacy BYOK models
    // stored the same fields directly in `[model.<id>]`; keep those usable so
    // adding meeting support does not force a destructive settings migration.
    let referenced_provider_id = model.get("model_provider").and_then(Value::as_str);
    if referenced_provider_id.is_some_and(|provider_id| provider_id != expected_provider_id) {
        return Err("会议任务的模型提供商已变化，请新建会议后重试".into());
    }
    let provider = if let Some(provider_id) = referenced_provider_id {
        config
            .get("model_providers")
            .and_then(Value::as_table)
            .and_then(|providers| providers.get(provider_id))
            .and_then(Value::as_table)
            .ok_or("会议模型连接已被删除")?
    } else {
        model
    };
    if expected_provider_id == BUILTIN_PROVIDER_ID
        || model_id.starts_with(BUILTIN_MODEL_PREFIX)
        || provider.get(MANAGED_BY_KEY).and_then(Value::as_str) == Some(MANAGED_BY_BUILTIN)
    {
        return Err("内置模型不提供录音转写".into());
    }
    if provider.get(MANAGED_BY_KEY).and_then(Value::as_str) == Some(MANAGED_BY_ORGANIZATION) {
        let lease_until = provider
            .get(LEASE_UNTIL_KEY)
            .and_then(Value::as_integer)
            .unwrap_or(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        if lease_until <= now {
            return Err("组织模型配置已过期，请重新同步".into());
        }
    }
    let base_url = provider
        .get("base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("会议模型 Base URL 未配置")?
        .trim_end_matches('/')
        .to_string();
    if provider.get("api_backend").and_then(Value::as_str) != Some("chat_completions")
        || provider.get("auth_scheme").and_then(Value::as_str) != Some("bearer")
    {
        return Err("会议纪要需要使用 Bearer Key 的 Chat Completions 模型连接".into());
    }
    let allow_http = provider
        .get(ALLOW_INSECURE_HTTP_KEY)
        .and_then(Value::as_bool)
        == Some(true);
    validate_provider_base_url_with_options(&base_url, false, allow_http)?;
    let api_key = resolved_provider_api_key(provider).ok_or("会议模型 API Key 未配置或已失效")?;
    let remote_model_id = model
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(model_id)
        .to_string();
    if !remote_model_id.to_ascii_lowercase().contains("minimax") {
        return Err("当前会议模型不是 MiniMax 模型".into());
    }
    Ok(MeetingProviderAccess {
        api_key,
        base_url,
        remote_model_id,
    })
}

pub(crate) fn meeting_provider_access(
    model_id: &str,
    expected_provider_id: &str,
) -> Result<MeetingProviderAccess, String> {
    crate::policy::require_model(model_id)?;
    meeting_provider_access_from_config(&read_config(), model_id, expected_provider_id)
}

/// Mask helper: returns `Some("••••")` only when a static key or referenced
/// environment variable currently resolves to a non-empty credential.
fn masked_key(table: &Map<String, Value>) -> Option<String> {
    if resolved_provider_api_key(table).is_some() {
        Some("••••".into())
    } else {
        None
    }
}

/// Read a `[model_providers.<id>]` table into an entry. The api_key is masked.
fn provider_from_table(id: &str, table: &Map<String, Value>) -> ModelProviderEntry {
    let source = match table.get(MANAGED_BY_KEY).and_then(Value::as_str) {
        Some(MANAGED_BY_ORGANIZATION) => "organization",
        Some(MANAGED_BY_BUILTIN) => "builtin",
        _ => "personal",
    };
    let managed = source != "personal";
    let api_key = masked_key(table);
    ModelProviderEntry {
        id: id.to_string(),
        provider_kind: infer_provider_kind(table),
        label: table
            .get(LABEL_KEY)
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| managed.then(|| "组织提供".to_string())),
        api_key: api_key.clone(),
        base_url: table
            .get("base_url")
            .and_then(Value::as_str)
            .map(String::from),
        api_backend: table
            .get("api_backend")
            .and_then(Value::as_str)
            .map(String::from),
        auth_scheme: table
            .get("auth_scheme")
            .and_then(Value::as_str)
            .map(String::from),
        context_window: table
            .get("context_window")
            .and_then(Value::as_integer)
            .map(|n| n as u64),
        allow_insecure_http: (!managed
            || source == "organization" && id == ORGANIZATION_PROVIDER_ID)
            && table.get(ALLOW_INSECURE_HTTP_KEY).and_then(Value::as_bool) == Some(true),
        source: source.into(),
        managed,
        credential_configured: api_key.is_some() || provider_can_omit_api_key(table),
        synced_at: table
            .get(SYNCED_AT_KEY)
            .and_then(Value::as_integer)
            .map(|n| n as u64),
        organization_provider: table
            .get("organization_provider")
            .and_then(Value::as_str)
            .map(String::from),
    }
}

/// Group legacy `[model.*]` entries (those carrying their own key/url, i.e. the
/// old per-model shape) into synthetic providers keyed by `base_url|api_key`.
/// Returns (synthetic_providers, synthetic_models). Disk is NOT modified.
///
/// Each group's id is derived from its provider_kind, de-duplicated against the
/// `taken_ids` set so two different custom endpoints don't collide.
fn group_legacy_models(
    models: &Map<String, Value>,
    taken_ids: &mut std::collections::HashSet<String>,
) -> (Vec<ModelProviderEntry>, Vec<ModelEntry>) {
    // group_key -> (provider_kind, base_url, first table for field inference)
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut group_order: Vec<String> = Vec::new();
    // group_key -> [model_id]
    let mut members: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (model_id, v) in models {
        let Some(table) = v.as_table() else { continue };
        // Only legacy entries that carry their own connection config.
        let has_key = table.contains_key("api_key") || table.contains_key("env_key");
        let has_url = table.contains_key("base_url");
        if !has_key && !has_url {
            continue;
        }
        // Skip entries already migrated (they reference a provider).
        if table.contains_key("model_provider") {
            continue;
        }
        let base_url = table
            .get("base_url")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // Keep distinct credentials/protocols in distinct synthetic groups.
        // The full signature stays in memory only and is never returned to the
        // frontend or logged.
        let credential_sig = table
            .get("api_key")
            .map(Value::to_string)
            .or_else(|| table.get("env_key").map(Value::to_string))
            .unwrap_or_else(|| "nokey".into());
        let backend_sig = table
            .get("api_backend")
            .map(Value::to_string)
            .unwrap_or_default();
        let auth_sig = table
            .get("auth_scheme")
            .map(Value::to_string)
            .unwrap_or_default();
        let group_key = format!("{base_url}|{credential_sig}|{backend_sig}|{auth_sig}");
        if !groups.contains_key(&group_key) {
            let kind = infer_provider_kind(table);
            groups.insert(group_key.clone(), (kind, base_url.clone()));
            group_order.push(group_key.clone());
        }
        members
            .entry(group_key.clone())
            .or_default()
            .push(model_id.clone());
    }

    let mut providers = Vec::new();
    let mut out_models = Vec::new();
    for gk in group_order {
        let (kind, base_url) = groups[&gk].clone();
        let id = allocate_provider_id(&kind, taken_ids);
        // Determine representative fields from the first member's table.
        let first_table = members[&gk]
            .first()
            .and_then(|mid| models.get(mid))
            .and_then(Value::as_table);
        providers.push(ModelProviderEntry {
            id: id.clone(),
            provider_kind: kind,
            label: None,
            api_key: first_table.and_then(masked_key),
            base_url: Some(base_url),
            api_backend: first_table
                .and_then(|t| t.get("api_backend"))
                .and_then(Value::as_str)
                .map(String::from),
            auth_scheme: first_table
                .and_then(|t| t.get("auth_scheme"))
                .and_then(Value::as_str)
                .map(String::from),
            context_window: first_table
                .and_then(|t| t.get("context_window"))
                .and_then(Value::as_integer)
                .map(|n| n as u64),
            allow_insecure_http: first_table
                .and_then(|t| t.get(ALLOW_INSECURE_HTTP_KEY))
                .and_then(Value::as_bool)
                == Some(true),
            source: "legacy".into(),
            managed: false,
            credential_configured: first_table.and_then(masked_key).is_some(),
            synced_at: None,
            organization_provider: None,
        });
        for mid in &members[&gk] {
            let table = models
                .get(mid)
                .and_then(Value::as_table)
                .expect("checked above");
            out_models.push(ModelEntry {
                model_id: mid.clone(),
                remote_model_id: table.get("model").and_then(Value::as_str).map(String::from),
                provider_id: id.clone(),
                name: table.get("name").and_then(Value::as_str).map(String::from),
                context_window: table
                    .get("context_window")
                    .and_then(Value::as_integer)
                    .map(|n| n as u64),
                max_output_tokens: table
                    .get("max_completion_tokens")
                    .and_then(Value::as_integer)
                    .and_then(|n| u32::try_from(n).ok()),
                clear_max_output_tokens: false,
                managed: false,
            });
        }
    }
    (providers, out_models)
}

/// Locate the connection fields and members behind one synthetic legacy
/// provider id. This lets the new editor test and migrate old per-model config
/// without sending the original secret through the webview.
fn legacy_provider_details(config: &Value, provider_id: &str) -> Option<(Value, Vec<ModelEntry>)> {
    let providers = config.get("model_providers").and_then(Value::as_table);
    let mut taken = providers
        .map(|providers| providers.keys().cloned().collect())
        .unwrap_or_default();
    let model_tables = config.get("model").and_then(Value::as_table)?;
    let (legacy_providers, legacy_models) = group_legacy_models(model_tables, &mut taken);
    if !legacy_providers
        .iter()
        .any(|provider| provider.id == provider_id)
    {
        return None;
    }
    let members = legacy_models
        .into_iter()
        .filter(|model| model.provider_id == provider_id)
        .collect::<Vec<_>>();
    let representative = members
        .first()
        .and_then(|model| model_tables.get(&model.model_id))
        .and_then(Value::as_table)?;
    let mut connection = Map::new();
    for key in [
        "base_url",
        "api_base_url",
        "api_key",
        "env_key",
        "api_backend",
        "auth_scheme",
        "extra_headers",
        "query_params",
        "env_http_headers",
        "auth_provider",
        "auth",
        "context_window",
    ] {
        if let Some(value) = representative.get(key) {
            connection.insert(key.into(), value.clone());
        }
    }
    Some((Value::Table(connection), members))
}

/// Produce a stable, human-readable provider id that isn't already taken.
/// `openai`, then `openai-2`, `openai-3`, ... Inserts the chosen id into
/// `taken` so successive calls within one read don't collide.
fn allocate_provider_id(kind: &str, taken: &mut std::collections::HashSet<String>) -> String {
    if !taken.contains(kind) {
        taken.insert(kind.to_string());
        return kind.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{kind}-{n}");
        if !taken.contains(&candidate) {
            taken.insert(candidate.clone());
            return candidate;
        }
        n += 1;
    }
}

fn stored_model_slug(model_id: &str, value: &Value) -> String {
    value
        .as_table()
        .and_then(|table| table.get("model"))
        .and_then(Value::as_str)
        .unwrap_or(model_id)
        .to_string()
}

fn allocate_model_id(
    remote_model_id: &str,
    provider_id: &str,
    models: &Map<String, Value>,
) -> String {
    if let Some((id, _)) = models.iter().find(|(id, value)| {
        value
            .as_table()
            .and_then(|table| table.get("model_provider"))
            .and_then(Value::as_str)
            == Some(provider_id)
            && stored_model_slug(id, value) == remote_model_id
    }) {
        return id.clone();
    }
    if !models.contains_key(remote_model_id) {
        return remote_model_id.to_string();
    }
    let base = format!("{provider_id}/{remote_model_id}");
    if !models.contains_key(&base) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !models.contains_key(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn remove_unselected_provider_models(
    models: &mut Map<String, Value>,
    provider_id: &str,
    selected: &std::collections::HashSet<String>,
) -> Vec<String> {
    let stale = models
        .iter()
        .filter_map(|(model_id, value)| {
            let belongs_to_provider = value
                .as_table()
                .and_then(|table| table.get("model_provider"))
                .and_then(Value::as_str)
                == Some(provider_id);
            (belongs_to_provider && !selected.contains(model_id) && !is_organization_model(value))
                .then(|| model_id.clone())
        })
        .collect::<Vec<_>>();
    for model_id in &stale {
        models.remove(model_id);
    }
    stale
}

fn validate_personal_provider_target(config: &Value, provider_id: &str) -> Result<(), String> {
    if provider_id == BUILTIN_PROVIDER_ID {
        return Err("内置模型连接不能由本地设置修改".into());
    }
    if provider_id == ORGANIZATION_PROVIDER_ID {
        return Err("组织托管连接不能由本地设置修改".into());
    }
    let existing = config
        .get("model_providers")
        .and_then(Value::as_table)
        .and_then(|providers| providers.get(provider_id));
    if existing.is_some_and(is_organization_managed) {
        return Err("组织托管连接不能由本地设置修改".into());
    }
    if existing.is_some_and(is_builtin_managed) {
        return Err("内置模型连接不能由本地设置修改".into());
    }
    Ok(())
}

fn validate_personal_model_target(config: &Value, model_id: &str) -> Result<(), String> {
    if model_id.starts_with(BUILTIN_MODEL_PREFIX) {
        return Err("内置模型不能由本地设置修改".into());
    }
    if model_id.starts_with(ORGANIZATION_MODEL_PREFIX) {
        return Err("组织模型目录不能由本地设置修改".into());
    }
    let existing = config
        .get("model")
        .and_then(Value::as_table)
        .and_then(|models| models.get(model_id));
    if existing.is_some_and(is_organization_model) {
        return Err("组织托管模型不能由本地设置修改".into());
    }
    if existing.is_some_and(is_builtin_model) {
        return Err("内置模型不能由本地设置修改".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Write helpers.
// ---------------------------------------------------------------------------

/// Resolve a single field using the priority chain:
///   explicit (non-empty Some) > existing disk value > preset default > error.
fn resolve_field(
    explicit: &Option<String>,
    existing: Option<&Value>,
    preset_val: Option<&'static str>,
    field_name: &str,
    is_custom: bool,
) -> Result<String, String> {
    if let Some(v) = explicit {
        if !v.is_empty() {
            return Ok(v.clone());
        }
    }
    if let Some(v) = existing.and_then(Value::as_str) {
        return Ok(v.to_string());
    }
    if let Some(p) = preset_val {
        return Ok(p.to_string());
    }
    if is_custom {
        Err(format!(
            "custom provider is missing required field '{field_name}' (set it in the Advanced section)"
        ))
    } else {
        Err(format!("internal error: no preset for {field_name}"))
    }
}

/// Render a `[model_providers.<id>]` table from an entry, preserving any
/// unrecognized keys already on disk.
fn provider_to_table(p: &ModelProviderEntry, existing: Option<&Value>) -> Result<Value, String> {
    let preset = preset(&p.provider_kind);
    // "Custom-like" kinds (custom / custom_anthropic) have no preset base_url,
    // so base_url must be user-supplied → a missing one is a hard error.
    let needs_base_url = preset.as_ref().and_then(|p| p.base_url).is_none();

    if let Some(b) = &p.api_backend {
        validate_api_backend(b)?;
    }
    if let Some(s) = &p.auth_scheme {
        validate_auth_scheme(s)?;
    }

    let mut table = match existing.and_then(Value::as_table) {
        Some(t) => t.clone(),
        None => Map::new(),
    };
    let existing_str = |key: &str| existing.and_then(Value::as_table).and_then(|t| t.get(key));

    let base_url = resolve_field(
        &p.base_url,
        existing_str("base_url"),
        preset.as_ref().and_then(|p| p.base_url),
        "base_url",
        needs_base_url,
    )?;
    let personal_http = p.allow_insecure_http
        && !p.managed
        && !matches!(p.source.as_str(), "organization" | "builtin");
    let organization_http = p.allow_insecure_http
        && p.managed
        && p.source == "organization"
        && p.id == ORGANIZATION_PROVIDER_ID;
    let allow_insecure_http = personal_http || organization_http;
    if is_ojlab_base_url(&base_url) && !allow_insecure_http {
        return Err("远端 HTTP 服务需要明确同意明文传输".into());
    }
    let parsed_base_url = validate_personal_provider_base_url(&base_url, allow_insecure_http)?;
    let keyless_service = is_ojlab_base_url(&base_url);
    let api_backend = if keyless_service {
        "chat_completions".to_owned()
    } else {
        resolve_field(
            &p.api_backend,
            existing_str("api_backend"),
            preset.as_ref().map(|p| p.api_backend),
            "api_backend",
            needs_base_url,
        )?
    };
    let auth_scheme = if keyless_service {
        "bearer".to_owned()
    } else {
        resolve_field(
            &p.auth_scheme,
            existing_str("auth_scheme"),
            preset.as_ref().map(|p| p.auth_scheme),
            "auth_scheme",
            needs_base_url,
        )?
    };

    table.insert("base_url".into(), Value::String(base_url));
    table.insert("api_backend".into(), Value::String(api_backend));
    table.insert("auth_scheme".into(), Value::String(auth_scheme));
    if allow_insecure_http && parsed_base_url.scheme() == "http" {
        table.insert(ALLOW_INSECURE_HTTP_KEY.into(), Value::Boolean(true));
    } else {
        table.remove(ALLOW_INSECURE_HTTP_KEY);
    }

    if let Some(label) = p.label.as_deref() {
        let label = label.trim();
        if label.is_empty() {
            table.remove(LABEL_KEY);
        } else {
            table.insert(LABEL_KEY.into(), Value::String(label.to_string()));
        }
    }

    if let Some(cw) = p.context_window {
        table.insert("context_window".into(), Value::Integer(cw as i64));
    } else {
        table.remove("context_window");
    }

    // Only touch api_key when the caller supplied one. Some("") clears it.
    if let Some(key) = &p.api_key {
        let key = key.trim();
        if key.is_empty() {
            table.remove("api_key");
        } else if key.starts_with('•') {
            // Mask coming back from the UI — treat as no-op.
        } else {
            table.insert("api_key".into(), Value::String(key.to_string()));
        }
    }
    if keyless_service {
        // This public endpoint never needs a credential. Do not persist or
        // forward an old provider key if a connection is repointed at it.
        for field in ["api_key", "env_key", "auth_provider", "auth"] {
            table.remove(field);
        }
    }

    Ok(Value::Table(table))
}

/// Render a `[model.<id>]` table from an entry. Connection config lives on the
/// provider now, so we strip any legacy base_url/api_key/api_backend/auth_scheme
/// the table may have carried (migration). Preserves unrecognized keys.
fn model_to_table(m: &ModelEntry, existing: Option<&Value>) -> Value {
    let mut table = match existing.and_then(Value::as_table) {
        Some(t) => t.clone(),
        None => Map::new(),
    };

    // The model slug the runtime will request. Defaults to the table key when absent.
    let remote_model_id = m
        .remote_model_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .unwrap_or(&m.model_id);
    table.insert("model".into(), Value::String(remote_model_id.to_string()));
    // Reference the provider for connection/auth config.
    table.insert(
        "model_provider".into(),
        Value::String(m.provider_id.clone()),
    );

    if let Some(name) = &m.name {
        if name.is_empty() {
            table.remove("name");
        } else {
            table.insert("name".into(), Value::String(name.clone()));
        }
    } else {
        table.remove("name");
    }

    if let Some(cw) = m.context_window {
        table.insert("context_window".into(), Value::Integer(cw as i64));
    } else {
        // Per-model override cleared → fall back to provider's value.
        table.remove("context_window");
    }
    if m.clear_max_output_tokens {
        table.remove("max_completion_tokens");
    } else if let Some(max_output) = m.max_output_tokens {
        table.insert(
            "max_completion_tokens".into(),
            Value::Integer(i64::from(max_output)),
        );
    }

    // Migrate away legacy per-model connection fields (now on the provider).
    for k in [
        "base_url",
        "api_key",
        "api_backend",
        "auth_scheme",
        "env_key",
    ] {
        table.remove(k);
    }

    Value::Table(table)
}

fn validate_model_limits(
    model: &ModelEntry,
    provider_context_window: Option<u64>,
    existing: Option<&Value>,
) -> Result<(), String> {
    if model.clear_max_output_tokens && model.max_output_tokens.is_some() {
        return Err("不能同时设置和清除最大输出".into());
    }
    if model.context_window == Some(0) || provider_context_window == Some(0) {
        return Err("上下文窗口必须大于 0".into());
    }
    let saved_max_output = existing
        .and_then(Value::as_table)
        .and_then(|table| table.get("max_completion_tokens"))
        .and_then(Value::as_integer)
        .and_then(|value| u32::try_from(value).ok());
    let effective_max_output = if model.clear_max_output_tokens {
        None
    } else {
        model.max_output_tokens.or(saved_max_output)
    };
    if let Some(max_output) = effective_max_output {
        if max_output == 0 {
            return Err("最大输出必须大于 0".into());
        }
        if model
            .context_window
            .or(provider_context_window)
            .is_some_and(|window| u64::from(max_output) > window)
        {
            return Err("最大输出不能超过上下文窗口".into());
        }
    }
    Ok(())
}

/// Ensure a top-level table exists in the config root, returning a mut ref.
fn ensure_table<'a>(
    config: &'a mut Value,
    key: &str,
) -> Result<&'a mut Map<String, Value>, String> {
    let root = config
        .as_table_mut()
        .ok_or_else(|| "config root not a table".to_string())?;
    if !root.contains_key(key) {
        root.insert(key.into(), Value::Table(Map::new()));
    }
    root.get_mut(key)
        .and_then(Value::as_table_mut)
        .ok_or_else(|| format!("config.{key} not a table"))
}

fn is_organization_managed(value: &Value) -> bool {
    value
        .as_table()
        .and_then(|table| table.get(MANAGED_BY_KEY))
        .and_then(Value::as_str)
        == Some(MANAGED_BY_ORGANIZATION)
}

fn is_organization_model(value: &Value) -> bool {
    is_organization_managed(value)
        || value
            .as_table()
            .and_then(|table| table.get("model_provider"))
            .and_then(Value::as_str)
            == Some(ORGANIZATION_PROVIDER_ID)
}

fn is_builtin_managed(value: &Value) -> bool {
    value
        .as_table()
        .and_then(|table| table.get(MANAGED_BY_KEY))
        .and_then(Value::as_str)
        == Some(MANAGED_BY_BUILTIN)
}

fn is_builtin_model(value: &Value) -> bool {
    is_builtin_managed(value)
        || value
            .as_table()
            .and_then(|table| table.get("model_provider"))
            .and_then(Value::as_str)
            == Some(BUILTIN_PROVIDER_ID)
}

const LEGACY_BUILTIN_MODEL_IDS: &[&str] = &[
    "echoagent-ojlab/chat-xc",
    "echoagent-ojlab/chat-glm",
    "echoagent-ojlab/chat-qwen",
];

pub(crate) fn is_legacy_builtin_model_id(id: &str) -> bool {
    LEGACY_BUILTIN_MODEL_IDS.contains(&id)
}

pub(crate) fn migrate_builtin_model_reference(value: &mut serde_json::Value) -> bool {
    if value.as_str().is_some_and(is_legacy_builtin_model_id) {
        *value = serde_json::Value::String(BUILTIN_DEFAULT_MODEL_ID.into());
        true
    } else {
        false
    }
}

fn migrate_builtin_model_preferences(selection: &mut Map<String, Value>) {
    // Empty/missing default means Auto. Never persist a fallback as a user choice.
    // Bootstrap reads this value again from disk. Normalize whitespace to the
    // empty string it recognizes as Auto instead of an explicit unknown model.
    if let Some(Value::String(default)) = selection.get_mut("default") {
        if default.trim().is_empty() {
            default.clear();
        }
    }
    for key in [
        "default",
        "web_search",
        "session_summary",
        "image_description",
        "prompt_suggestion",
    ] {
        if selection
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(is_legacy_builtin_model_id)
        {
            selection.insert(key.into(), Value::String(BUILTIN_DEFAULT_MODEL_ID.into()));
        }
    }
    // Preserve every original filter, including personal matches and invalid
    // patterns (Runtime reports those). Add only the retired models' successor.
    for key in ["allowed_models", "hidden_models", "disabled_models"] {
        let Some(patterns) = selection.get_mut(key).and_then(Value::as_array_mut) else {
            continue;
        };
        let covers_retired = patterns.iter().filter_map(Value::as_str).any(|pattern| {
            globset::Glob::new(pattern).is_ok_and(|glob| {
                let matcher = glob.compile_matcher();
                LEGACY_BUILTIN_MODEL_IDS.iter().any(|id| {
                    matcher.is_match(id)
                        || matcher.is_match(id.strip_prefix(BUILTIN_MODEL_PREFIX).unwrap())
                })
            })
        });
        if covers_retired
            && !patterns
                .iter()
                .any(|value| value.as_str() == Some(BUILTIN_DEFAULT_MODEL_ID))
        {
            patterns.push(Value::String(BUILTIN_DEFAULT_MODEL_ID.into()));
        }
    }
}

fn apply_builtin_model_config(config: &mut Value) -> Result<(), String> {
    let mut provider = Map::new();
    provider.insert(
        "base_url".into(),
        Value::String(crate::agent_runtime::OJLAB_BASE_URL.into()),
    );
    provider.insert(
        "api_backend".into(),
        Value::String("chat_completions".into()),
    );
    provider.insert("auth_scheme".into(), Value::String("bearer".into()));
    provider.insert(LABEL_KEY.into(), Value::String("内置模型".into()));
    provider.insert(
        MANAGED_BY_KEY.into(),
        Value::String(MANAGED_BY_BUILTIN.into()),
    );
    let providers = ensure_table(config, "model_providers")?;
    if providers
        .get(BUILTIN_PROVIDER_ID)
        .is_some_and(|entry| !is_builtin_managed(entry))
    {
        return Err("内置模型连接 ID 已被个人配置占用".into());
    }
    providers.insert(BUILTIN_PROVIDER_ID.into(), Value::Table(provider));

    let models = ensure_table(config, "model")?;
    // Reconcile the managed catalog rather than accumulating retired models.
    // Personal and organization entries are outside this provider's ownership.
    models.retain(|id, entry| {
        !is_builtin_model(entry)
            || BUILTIN_CHAT_MODELS
                .iter()
                .any(|(slug, _, _)| id == &format!("{BUILTIN_MODEL_PREFIX}{slug}"))
    });
    for &(slug, context_window, max_output_tokens) in BUILTIN_CHAT_MODELS {
        let id = format!("{BUILTIN_MODEL_PREFIX}{slug}");
        if models
            .get(&id)
            .is_some_and(|entry| !is_builtin_model(entry))
        {
            return Err(format!("内置模型 ID {id} 已被个人配置占用"));
        }
        let mut entry = Map::new();
        entry.insert("model".into(), Value::String(slug.into()));
        entry.insert("name".into(), Value::String(slug.into()));
        entry.insert(
            "model_provider".into(),
            Value::String(BUILTIN_PROVIDER_ID.into()),
        );
        // The service's input limit is context_window - max_completion_tokens.
        entry.insert("context_window".into(), Value::Integer(context_window));
        entry.insert(
            "max_completion_tokens".into(),
            Value::Integer(max_output_tokens),
        );
        // Leave room for the response. Willow and Reed accept input up to 75%
        // of their context, so the Runtime's default 85% compaction is too late.
        let compact_percent =
            (100 * (context_window - max_output_tokens) / context_window - 5).min(85);
        if compact_percent < 85 {
            entry.insert(
                "auto_compact_threshold_percent".into(),
                Value::Integer(compact_percent),
            );
        }
        entry.insert(
            MANAGED_BY_KEY.into(),
            Value::String(MANAGED_BY_BUILTIN.into()),
        );
        models.insert(id, Value::Table(entry));
    }
    let selection = ensure_table(config, "models")?;
    migrate_builtin_model_preferences(selection);
    Ok(())
}

pub(crate) fn ensure_builtin_model_config() -> Result<(), String> {
    update_config(apply_builtin_model_config)
}

/// Apply a downloaded organization model to an in-memory config document.
/// Existing personal providers/models are preserved. A namespaced local model
/// id prevents a server model such as `gpt-4o` from replacing a user's own
/// entry; the nested `model` value remains the exact upstream model slug.
fn apply_organization_model_config(
    config: &mut Value,
    downloaded: &OrganizationModelConfig,
) -> Result<String, String> {
    let provider = downloaded.provider.trim();
    let model = downloaded.model.trim();
    let base_url = downloaded.base_url.trim().trim_end_matches('/');
    let api_key = downloaded.api_key.trim();
    if provider.is_empty() || model.is_empty() || base_url.is_empty() || api_key.is_empty() {
        return Err("organization model config is incomplete".into());
    }
    let parsed_base_url = validate_provider_base_url_with_options(base_url, false, true)
        .map_err(|error| format!("invalid organization model Base URL: {error}"))?;

    let normalized_provider = provider.to_ascii_lowercase();
    let provider_kind = match normalized_provider.as_str() {
        "openai" => "openai",
        "anthropic" => "anthropic",
        "deepseek" => "deepseek",
        "qwen" | "dashscope" => "qwen",
        "minimax" => "minimax",
        "anthropic-compatible" | "anthropic_compatible" | "custom_anthropic" => "custom_anthropic",
        _ => "custom",
    };
    let provider_preset = preset(provider_kind);
    let api_backend = provider_preset
        .as_ref()
        .map(|preset| preset.api_backend)
        .unwrap_or("chat_completions");
    let auth_scheme = provider_preset
        .as_ref()
        .map(|preset| preset.auth_scheme)
        .unwrap_or("bearer");
    let synced_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let entry = ModelProviderEntry {
        id: ORGANIZATION_PROVIDER_ID.into(),
        provider_kind: provider_kind.into(),
        label: Some("组织提供".into()),
        api_key: Some(api_key.into()),
        base_url: Some(base_url.into()),
        api_backend: Some(api_backend.into()),
        auth_scheme: Some(auth_scheme.into()),
        context_window: None,
        source: "organization".into(),
        managed: true,
        allow_insecure_http: parsed_base_url.scheme() == "http",
        credential_configured: true,
        synced_at: Some(synced_at),
        organization_provider: Some(provider.into()),
    };
    {
        let providers = ensure_table(config, "model_providers")?;
        let existing = providers.get(ORGANIZATION_PROVIDER_ID);
        let mut rendered = provider_to_table(&entry, existing)?;
        let table = rendered
            .as_table_mut()
            .ok_or("organization provider config is not a table")?;
        table.insert(
            MANAGED_BY_KEY.into(),
            Value::String(MANAGED_BY_ORGANIZATION.into()),
        );
        table.insert(
            "organization_provider".into(),
            Value::String(provider.into()),
        );
        table.insert(SYNCED_AT_KEY.into(), Value::Integer(synced_at as i64));
        table.insert(
            LEASE_UNTIL_KEY.into(),
            Value::Integer(downloaded.lease_until as i64),
        );
        providers.insert(ORGANIZATION_PROVIDER_ID.into(), rendered);
    }

    let model_id = format!("{ORGANIZATION_MODEL_PREFIX}{model}");
    {
        let models = ensure_table(config, "model")?;
        let stale: Vec<String> = models
            .iter()
            .filter(|(_, value)| is_organization_model(value))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            models.remove(&id);
        }

        let mut rendered = model_to_table(
            &ModelEntry {
                model_id: model_id.clone(),
                remote_model_id: Some(model.into()),
                provider_id: ORGANIZATION_PROVIDER_ID.into(),
                name: Some(model.into()),
                context_window: None,
                max_output_tokens: None,
                clear_max_output_tokens: false,
                managed: true,
            },
            models.get(&model_id),
        );
        let table = rendered
            .as_table_mut()
            .ok_or("organization model config is not a table")?;
        // The table key is namespaced locally, while requests must use the
        // exact model slug configured by the organization administrator.
        table.insert("model".into(), Value::String(model.into()));
        models.insert(model_id.clone(), rendered);
    }

    if let Some(defaults) = config
        .as_table_mut()
        .and_then(|root| root.get_mut("models"))
        .and_then(Value::as_table_mut)
    {
        if defaults
            .get("default")
            .and_then(Value::as_str)
            .is_some_and(|selected| {
                selected.starts_with(ORGANIZATION_MODEL_PREFIX) && selected != model_id
            })
        {
            defaults.remove("default");
        }
    }

    Ok(model_id)
}

/// Persist a downloaded organization model alongside the user's own models.
pub(crate) fn save_organization_model_config(
    downloaded: OrganizationModelConfig,
) -> Result<String, String> {
    update_config(|config| apply_organization_model_config(config, &downloaded))
}

/// Remove every organization-managed provider/model while preserving personal
/// connections. Used when the server disables chat configuration or the user
/// signs out, so downloaded credentials cannot outlive organization access.
pub(crate) fn remove_organization_model_config() -> Result<bool, String> {
    update_config(|config| Ok(remove_organization_model_config_from(config)))
}

fn remove_organization_model_config_from(config: &mut Value) -> bool {
    let mut changed = false;
    let mut removed_model_ids = Vec::new();

    if let Some(providers) = config
        .as_table_mut()
        .and_then(|root| root.get_mut("model_providers"))
        .and_then(Value::as_table_mut)
    {
        let stale = providers
            .iter()
            .filter(|(_, value)| is_organization_managed(value))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in stale {
            changed |= providers.remove(&id).is_some();
        }
    }

    if let Some(models) = config
        .as_table_mut()
        .and_then(|root| root.get_mut("model"))
        .and_then(Value::as_table_mut)
    {
        let stale = models
            .iter()
            .filter(|(_, value)| is_organization_model(value))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in stale {
            if models.remove(&id).is_some() {
                removed_model_ids.push(id);
                changed = true;
            }
        }
    }

    if !removed_model_ids.is_empty() {
        if let Some(defaults) = config
            .as_table_mut()
            .and_then(|root| root.get_mut("models"))
            .and_then(Value::as_table_mut)
        {
            let selected = defaults.get("default").and_then(Value::as_str);
            if selected.is_some_and(|id| removed_model_ids.iter().any(|removed| removed == id)) {
                defaults.remove("default");
                changed = true;
            }
        }
    }

    changed
}

/// Fail closed after the signed organization policy lease expires. Temporary
/// network failures may keep the last downloaded configuration only while its
/// last verified policy is still valid.
pub(crate) fn enforce_organization_model_lease() -> Result<bool, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    update_config(|config| {
        let managed_provider = config
            .get("model_providers")
            .and_then(Value::as_table)
            .and_then(|providers| providers.get(ORGANIZATION_PROVIDER_ID))
            .filter(|provider| is_organization_managed(provider));
        let lease_until = managed_provider
            .and_then(Value::as_table)
            .and_then(|provider| provider.get(LEASE_UNTIL_KEY))
            .and_then(Value::as_integer)
            .map(|value| value as u64)
            .unwrap_or(0);
        Ok(if managed_provider.is_some() && lease_until <= now {
            remove_organization_model_config_from(config)
        } else {
            false
        })
    })
}

// ---------------------------------------------------------------------------
// Tauri commands.
// ---------------------------------------------------------------------------

/// List configured providers + models. Legacy per-model entries are grouped
/// into synthetic providers for display; disk is not modified.
#[tauri::command]
pub fn providers_list() -> ProviderListModel {
    let config = read_config();

    let mut providers = Vec::new();
    let mut models = Vec::new();
    let mut taken_ids = std::collections::HashSet::new();

    // 1) Real [model_providers.*] entries.
    if let Some(mps) = config
        .as_table()
        .and_then(|t| t.get("model_providers"))
        .and_then(Value::as_table)
    {
        for (id, v) in mps {
            let Some(table) = v.as_table() else { continue };
            taken_ids.insert(id.clone());
            providers.push(provider_from_table(id, table));
        }
    }

    // 2) Models that reference a provider.
    if let Some(mdls) = config
        .as_table()
        .and_then(|t| t.get("model"))
        .and_then(Value::as_table)
    {
        for (model_id, v) in mdls {
            let Some(table) = v.as_table() else { continue };
            if let Some(pid) = table.get("model_provider").and_then(Value::as_str) {
                models.push(ModelEntry {
                    model_id: model_id.clone(),
                    remote_model_id: table.get("model").and_then(Value::as_str).map(String::from),
                    provider_id: pid.to_string(),
                    name: table.get("name").and_then(Value::as_str).map(String::from),
                    context_window: table
                        .get("context_window")
                        .and_then(Value::as_integer)
                        .map(|n| n as u64),
                    max_output_tokens: table
                        .get("max_completion_tokens")
                        .and_then(Value::as_integer)
                        .and_then(|n| u32::try_from(n).ok()),
                    clear_max_output_tokens: false,
                    managed: is_organization_model(v) || is_builtin_model(v),
                });
            }
        }
    }

    // 3) Legacy per-model entries → grouped synthetic providers (display only).
    if let Some(mdls) = config
        .as_table()
        .and_then(|t| t.get("model"))
        .and_then(Value::as_table)
    {
        let (mut synth_p, mut synth_m) = group_legacy_models(mdls, &mut taken_ids);
        providers.append(&mut synth_p);
        models.append(&mut synth_m);
    }

    ProviderListModel { providers, models }
}

/// Return models whose referenced connection has both a valid HTTP(S) endpoint
/// and a stored credential. This powers the shell readiness gate so a dangling
/// model table no longer produces a false "ready" state.
pub(crate) fn usable_model_ids() -> (Vec<String>, Option<String>) {
    let list = providers_list();
    if list.models.is_empty() {
        return (
            Vec::new(),
            Some("尚未配置模型，请前往“设置 → 模型与连接”添加连接。".into()),
        );
    }
    let mut usable = Vec::new();
    for model in &list.models {
        let Some(provider) = list
            .providers
            .iter()
            .find(|provider| provider.id == model.provider_id)
        else {
            continue;
        };
        let endpoint_valid = provider
            .base_url
            .as_deref()
            .is_some_and(|base_url| validate_provider_base_url(base_url).is_ok());
        let model_valid = !model
            .remote_model_id
            .as_deref()
            .unwrap_or(&model.model_id)
            .trim()
            .is_empty();
        if provider.credential_configured && endpoint_valid && model_valid {
            usable.push(model.model_id.clone());
        }
    }
    let reason = usable.is_empty().then(|| {
        "已有模型条目，但连接缺少有效的 Base URL 或 API Key，请在“设置 → 模型与连接”中修复。".into()
    });
    (usable, reason)
}

/// Save (merge) a provider into `[model_providers.<id>]`. Preserves unknown keys.
#[tauri::command]
pub fn providers_save_provider(
    state: State<'_, crate::commands::AppState>,
    provider: ModelProviderEntry,
) -> Result<(), String> {
    if provider.id.trim().is_empty() {
        return Err("provider id 不能为空".into());
    }
    update_config(|config| {
        validate_personal_provider_target(config, provider.id.trim())?;
        let mps = ensure_table(config, "model_providers")?;
        let existing = mps.get(&provider.id);
        let rendered = provider_to_table(&provider, existing)?;
        mps.insert(provider.id.clone(), rendered);
        Ok(())
    })?;
    // The disk revision changed. Keep sends blocked until the in-process
    // Runtime acknowledges the new model catalog via the awaited reload path.
    state.mark_runtime_models_initializing();
    Ok(())
}

/// Atomically save a connection and the models selected during discovery.
/// An empty provider id allocates a collision-free id such as `custom-2`.
#[tauri::command]
pub fn providers_save_connection(
    state: State<'_, crate::commands::AppState>,
    mut provider: ModelProviderEntry,
    mut models: Vec<ModelEntry>,
    replace_models: bool,
) -> Result<SaveConnectionResult, String> {
    let result = update_config(|config| {
        let legacy = (provider.source == "legacy")
            .then(|| legacy_provider_details(config, provider.id.trim()))
            .flatten();
        let provider_id = if provider.id.trim().is_empty() {
            let mut taken = config
                .get("model_providers")
                .and_then(Value::as_table)
                .map(|providers| providers.keys().cloned().collect())
                .unwrap_or_default();
            allocate_provider_id(provider.provider_kind.trim(), &mut taken)
        } else {
            provider.id.trim().to_string()
        };
        if provider.provider_kind.trim().is_empty() {
            return Err("请选择接口类型".into());
        }
        validate_personal_provider_target(config, &provider_id)?;
        provider.id.clone_from(&provider_id);
        provider.managed = false;
        provider.source = "personal".into();

        {
            let providers = ensure_table(config, "model_providers")?;
            let existing = providers
                .get(&provider_id)
                .or_else(|| legacy.as_ref().map(|(connection, _)| connection));
            let rendered = provider_to_table(&provider, existing)?;
            let credential_configured = rendered
                .as_table()
                .and_then(resolved_provider_api_key)
                .is_some();
            if !credential_configured && !rendered.as_table().is_some_and(provider_can_omit_api_key)
            {
                return Err("请填写 API Key".into());
            }
            providers.insert(provider_id.clone(), rendered);
        }

        // Saving any part of a legacy connection migrates all of its existing
        // members unless the caller explicitly chose a replacement set.
        if !replace_models {
            if let Some((_, legacy_models)) = &legacy {
                let supplied = models
                    .iter()
                    .map(|model| model.model_id.clone())
                    .collect::<std::collections::HashSet<_>>();
                models.extend(
                    legacy_models
                        .iter()
                        .filter(|model| !supplied.contains(&model.model_id))
                        .cloned(),
                );
            }
        }

        let mut model_ids = Vec::with_capacity(models.len());
        let removed_model_ids = {
            let model_tables = ensure_table(config, "model")?;
            for mut model in models {
                let remote_model_id = model
                    .remote_model_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .or_else(|| {
                        let id = model.model_id.trim();
                        (!id.is_empty()).then_some(id)
                    })
                    .ok_or("模型 ID 不能为空")?
                    .to_string();
                let local_id = if model.model_id.trim().is_empty() {
                    allocate_model_id(&remote_model_id, &provider_id, model_tables)
                } else {
                    model.model_id.trim().to_string()
                };
                if local_id.starts_with(ORGANIZATION_MODEL_PREFIX)
                    || model_tables
                        .get(&local_id)
                        .is_some_and(is_organization_model)
                {
                    return Err("组织托管模型不能由本地设置修改".into());
                }
                if local_id.starts_with(BUILTIN_MODEL_PREFIX)
                    || model_tables.get(&local_id).is_some_and(is_builtin_model)
                {
                    return Err("内置模型不能由本地设置修改".into());
                }
                if let Some(other_provider) = model_tables
                    .get(&local_id)
                    .and_then(Value::as_table)
                    .and_then(|table| table.get("model_provider"))
                    .and_then(Value::as_str)
                    .filter(|existing| *existing != provider_id)
                {
                    return Err(format!(
                        "模型目录 ID「{local_id}」已属于连接「{other_provider}」"
                    ));
                }
                model.model_id.clone_from(&local_id);
                model.remote_model_id = Some(remote_model_id);
                model.provider_id.clone_from(&provider_id);
                model.managed = false;
                let existing = model_tables.get(&local_id);
                validate_model_limits(&model, provider.context_window, existing)?;
                let rendered = model_to_table(&model, existing);
                model_tables.insert(local_id.clone(), rendered);
                model_ids.push(local_id);
            }
            if replace_models {
                let selected = model_ids.iter().cloned().collect();
                remove_unselected_provider_models(model_tables, &provider_id, &selected)
            } else {
                Vec::new()
            }
        };

        if !removed_model_ids.is_empty() {
            if let Some(defaults) = config
                .as_table_mut()
                .and_then(|root| root.get_mut("models"))
                .and_then(Value::as_table_mut)
            {
                let selected = defaults.get("default").and_then(Value::as_str);
                if selected.is_some_and(|id| removed_model_ids.iter().any(|removed| removed == id))
                {
                    defaults.remove("default");
                }
            }
        }

        Ok(SaveConnectionResult {
            provider_id,
            model_ids,
        })
    })?;
    state.mark_runtime_models_initializing();
    Ok(result)
}

/// Save a model into `[model.<model_id>]` with a `model_provider` reference.
/// Migrates the entry away from legacy per-model connection fields.
#[tauri::command]
pub fn providers_save_model(
    state: State<'_, crate::commands::AppState>,
    mut model: ModelEntry,
) -> Result<String, String> {
    if model.provider_id.trim().is_empty() {
        return Err("provider_id 不能为空".into());
    }
    let local_id = update_config(|config| {
        validate_personal_provider_target(config, model.provider_id.trim())?;
        let provider_context_window = config
            .get("model_providers")
            .and_then(Value::as_table)
            .and_then(|providers| providers.get(model.provider_id.trim()))
            .and_then(Value::as_table)
            .and_then(|provider| provider.get("context_window"))
            .and_then(Value::as_integer)
            .and_then(|window| u64::try_from(window).ok());
        let remote_model_id = model
            .remote_model_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .or_else(|| {
                let id = model.model_id.trim();
                (!id.is_empty()).then_some(id)
            })
            .ok_or("模型 ID 不能为空")?
            .to_string();
        let mdls = ensure_table(config, "model")?;
        let local_id = if model.model_id.trim().is_empty() {
            allocate_model_id(&remote_model_id, model.provider_id.trim(), mdls)
        } else {
            model.model_id.trim().to_string()
        };
        if local_id.starts_with(ORGANIZATION_MODEL_PREFIX)
            || mdls.get(&local_id).is_some_and(is_organization_model)
        {
            return Err("组织托管模型不能由本地设置修改".into());
        }
        if local_id.starts_with(BUILTIN_MODEL_PREFIX)
            || mdls.get(&local_id).is_some_and(is_builtin_model)
        {
            return Err("内置模型不能由本地设置修改".into());
        }
        model.model_id.clone_from(&local_id);
        model.remote_model_id = Some(remote_model_id);
        model.managed = false;
        let existing = mdls.get(&local_id);
        validate_model_limits(&model, provider_context_window, existing)?;
        let rendered = model_to_table(&model, existing);
        mdls.insert(local_id.clone(), rendered);
        Ok(local_id)
    })?;
    state.mark_runtime_models_initializing();
    Ok(local_id)
}

/// Delete a provider AND every model that references it.
#[tauri::command]
pub fn providers_delete_provider(
    state: State<'_, crate::commands::AppState>,
    id: String,
) -> Result<(), String> {
    let changed = update_config(|config| {
        validate_personal_provider_target(config, id.trim())?;

        // Remove the provider table.
        let removed_provider = config
            .as_table_mut()
            .and_then(|t| t.get_mut("model_providers"))
            .and_then(Value::as_table_mut)
            .and_then(|m| m.remove(&id))
            .is_some();

        // Cascade: drop every [model.*] referencing it.
        let mut cascaded = 0usize;
        let mut removed_model_ids = Vec::new();
        if let Some(mdls) = config
            .as_table_mut()
            .and_then(|t| t.get_mut("model"))
            .and_then(Value::as_table_mut)
        {
            let stale: Vec<String> = mdls
                .iter()
                .filter_map(|(mid, v)| {
                    let refs = v
                        .as_table()
                        .and_then(|t| t.get("model_provider"))
                        .and_then(Value::as_str);
                    if refs == Some(id.as_str()) {
                        Some(mid.clone())
                    } else {
                        None
                    }
                })
                .collect();
            for mid in stale {
                if mdls.remove(&mid).is_some() {
                    removed_model_ids.push(mid);
                    cascaded += 1;
                }
            }
        }

        if let Some(defaults) = config
            .as_table_mut()
            .and_then(|root| root.get_mut("models"))
            .and_then(Value::as_table_mut)
        {
            let selected = defaults.get("default").and_then(Value::as_str);
            if selected.is_some_and(|model_id| removed_model_ids.iter().any(|id| id == model_id)) {
                defaults.remove("default");
            }
        }

        Ok(removed_provider || cascaded > 0)
    })?;
    if changed {
        state.mark_runtime_models_initializing();
    }
    Ok(())
}

/// Delete a single model entry.
#[tauri::command]
pub fn providers_delete_model(
    state: State<'_, crate::commands::AppState>,
    model_id: String,
) -> Result<(), String> {
    let removed = update_config(|config| {
        validate_personal_model_target(config, model_id.trim())?;
        let removed = config
            .as_table_mut()
            .and_then(|t| t.get_mut("model"))
            .and_then(Value::as_table_mut)
            .and_then(|m| m.remove(&model_id))
            .is_some();
        if removed {
            if let Some(defaults) = config
                .as_table_mut()
                .and_then(|root| root.get_mut("models"))
                .and_then(Value::as_table_mut)
            {
                if defaults.get("default").and_then(Value::as_str) == Some(model_id.as_str()) {
                    defaults.remove("default");
                }
            }
        }
        Ok(removed)
    })?;
    if removed {
        state.mark_runtime_models_initializing();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Back-compat shims (deprecated). Keep the old command names registered so a
// stale frontend build doesn't hit "unknown command" at runtime; they no-op.
// ---------------------------------------------------------------------------

/// Deprecated: use `providers_save_provider` / `providers_save_model`.
#[tauri::command]
pub fn providers_save(
    _state: State<'_, crate::commands::AppState>,
    _providers: Vec<serde_json::Value>,
) -> Result<(), String> {
    Err("providers_save is deprecated; use providers_save_provider / providers_save_model".into())
}

/// Deprecated: use `providers_delete_model`.
#[tauri::command]
pub fn providers_delete(_model_id: String) -> Result<(), String> {
    Err("providers_delete is deprecated; use providers_delete_model".into())
}

// ---------------------------------------------------------------------------
// Remote model discovery.
// ---------------------------------------------------------------------------

/// One model entry returned by a provider's `GET /models` endpoint.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
}

/// Resolve the effective base_url for a fetch, given the provider kind and an
/// optional override. Presets provide the default; `custom` requires the caller
/// to supply `base_url`.
fn resolve_fetch_base_url(
    kind: &str,
    base_url: &Option<String>,
    allow_insecure_http: bool,
) -> Result<String, String> {
    let resolved = if let Some(url) = base_url {
        let trimmed = url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err("base_url 不能为空".into());
        }
        trimmed.to_string()
    } else {
        match preset(kind).and_then(|p| p.base_url) {
            Some(b) => b.trim_end_matches('/').to_string(),
            None => return Err("自定义提供商必须填写 Base URL".into()),
        }
    };
    validate_personal_provider_base_url(&resolved, allow_insecure_http)?;
    Ok(resolved)
}

/// Fetch the list of available models from a provider's `/models` endpoint.
///
/// Works for any OpenAI-compatible endpoint (`GET {base_url}/models` returning
/// `{"data":[{"id":"...","owned_by":"..."}]}`) and for Anthropic (same path,
/// but requires `anthropic-version` header + `x-api-key` auth).
///
/// The `api_key` is used only for this single request — it is never persisted
/// or logged.
#[tauri::command]
pub async fn providers_fetch_models(
    provider_kind: String,
    api_key: String,
    base_url: Option<String>,
) -> Result<Vec<FetchedModel>, String> {
    fetch_models(&provider_kind, &api_key, &base_url, None, false).await
}

/// Fetch models using an already-saved provider credential. The secret stays
/// inside native code, so Settings never asks users to re-enter it or exposes
/// organization-managed keys to the webview.
#[tauri::command]
pub async fn providers_fetch_models_for_provider(
    provider_id: String,
) -> Result<Vec<FetchedModel>, String> {
    let config = read_config();
    let table = config
        .get("model_providers")
        .and_then(Value::as_table)
        .and_then(|providers| providers.get(provider_id.trim()))
        .and_then(Value::as_table)
        .ok_or("连接不存在")?;
    let provider_kind = infer_provider_kind(table);
    let api_key = resolved_or_optional_api_key(table).ok_or("该连接没有可用的已保存 API Key")?;
    let base_url = table
        .get("base_url")
        .and_then(Value::as_str)
        .map(String::from);
    let auth_scheme = table.get("auth_scheme").and_then(Value::as_str);
    let allow_insecure_http = table.get(MANAGED_BY_KEY).is_none()
        && table.get(ALLOW_INSECURE_HTTP_KEY).and_then(Value::as_bool) == Some(true);
    fetch_models(
        &provider_kind,
        &api_key,
        &base_url,
        auth_scheme,
        allow_insecure_http,
    )
    .await
}

/// Validate an unsaved connection draft. When editing and the key is blank or
/// masked, reuse the existing native-side secret without sending it to React.
#[tauri::command]
pub async fn providers_test_connection(
    provider: ModelProviderEntry,
) -> Result<Vec<FetchedModel>, String> {
    let config = read_config();
    if !provider.id.trim().is_empty() {
        validate_personal_provider_target(&config, provider.id.trim())?;
    }
    let legacy_existing = (provider.source == "legacy")
        .then(|| legacy_provider_details(&config, provider.id.trim()))
        .flatten()
        .map(|(connection, _)| connection);
    let existing = (!provider.id.trim().is_empty())
        .then(|| {
            config
                .get("model_providers")
                .and_then(Value::as_table)
                .and_then(|providers| providers.get(provider.id.trim()))
        })
        .flatten()
        .or(legacy_existing.as_ref());
    let rendered = provider_to_table(&provider, existing)?;
    let table = rendered.as_table().ok_or("连接配置无效")?;
    let provider_kind = infer_provider_kind(table);
    let api_key = resolved_or_optional_api_key(table).ok_or("请填写 API Key")?;
    let base_url = table
        .get("base_url")
        .and_then(Value::as_str)
        .map(String::from);
    let auth_scheme = table.get("auth_scheme").and_then(Value::as_str);
    let allow_insecure_http =
        table.get(ALLOW_INSECURE_HTTP_KEY).and_then(Value::as_bool) == Some(true);
    fetch_models(
        &provider_kind,
        &api_key,
        &base_url,
        auth_scheme,
        allow_insecure_http,
    )
    .await
}

/// Test the actual inference endpoint with a concrete model id. This is kept
/// separate from model discovery because many OpenAI/Anthropic-compatible
/// gateways intentionally do not expose `GET /models`.
#[tauri::command]
pub async fn providers_test_model_connection(
    provider: ModelProviderEntry,
    model_id: String,
) -> Result<(), String> {
    let model_id = model_id.trim();
    if model_id.is_empty() {
        return Err("请填写 Model ID".into());
    }

    let config = read_config();
    let stored = (!provider.id.trim().is_empty())
        .then(|| {
            config
                .get("model_providers")
                .and_then(Value::as_table)
                .and_then(|providers| providers.get(provider.id.trim()))
        })
        .flatten();

    // Organization-managed connections are tested exactly as downloaded;
    // never accept webview-provided overrides for their endpoint or secret.
    let rendered = if stored
        .is_some_and(|entry| is_organization_managed(entry) || is_builtin_managed(entry))
    {
        stored.cloned().ok_or("连接不存在")?
    } else {
        if !provider.id.trim().is_empty() {
            validate_personal_provider_target(&config, provider.id.trim())?;
        }
        let legacy_existing = (provider.source == "legacy")
            .then(|| legacy_provider_details(&config, provider.id.trim()))
            .flatten()
            .map(|(connection, _)| connection);
        let existing = stored.or(legacy_existing.as_ref());
        provider_to_table(&provider, existing)?
    };

    let table = rendered.as_table().ok_or("连接配置无效")?;
    let provider_kind = infer_provider_kind(table);
    let api_key = resolved_or_optional_api_key(table).ok_or("请填写 API Key")?;
    let base_url = table
        .get("base_url")
        .and_then(Value::as_str)
        .map(String::from);
    let api_backend = table
        .get("api_backend")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            preset(&provider_kind)
                .map(|provider| provider.api_backend)
                .unwrap_or("chat_completions")
        });
    let auth_scheme = table.get("auth_scheme").and_then(Value::as_str);
    let allow_insecure_http = table.get(MANAGED_BY_KEY).is_none()
        && table.get(ALLOW_INSECURE_HTTP_KEY).and_then(Value::as_bool) == Some(true);
    test_model_inference(
        &provider_kind,
        &api_key,
        &base_url,
        api_backend,
        auth_scheme,
        model_id,
        allow_insecure_http,
    )
    .await
}

fn inference_endpoint(base: &str, api_backend: &str) -> Result<String, String> {
    validate_api_backend(api_backend)?;
    let path = match api_backend {
        "responses" => "responses",
        "messages" => "messages",
        _ => "chat/completions",
    };
    let base = base.trim().trim_end_matches('/');
    if base.ends_with(path) {
        Ok(base.to_string())
    } else {
        Ok(format!("{base}/{path}"))
    }
}

async fn test_model_inference(
    provider_kind: &str,
    api_key: &str,
    base_url: &Option<String>,
    api_backend: &str,
    explicit_auth_scheme: Option<&str>,
    model_id: &str,
    allow_insecure_http: bool,
) -> Result<(), String> {
    let key = api_key.trim();
    let base = resolve_fetch_base_url(provider_kind, base_url, allow_insecure_http)?;
    if key.is_empty() && !is_ojlab_base_url(&base) {
        return Err("请先填写 API Key".into());
    }
    let url = inference_endpoint(&base, api_backend)?;
    let auth_scheme = explicit_auth_scheme
        .map(str::trim)
        .filter(|scheme| !scheme.is_empty())
        .map(str::to_string)
        .or_else(|| preset(provider_kind).map(|provider| provider.auth_scheme.to_string()))
        .unwrap_or_else(|| "bearer".into());
    validate_auth_scheme(&auth_scheme)?;

    let payload = crate::provider_response::inference_probe_payload(api_backend, model_id);

    let client = provider_http_client(std::time::Duration::from_secs(30))?;
    let mut request = client.post(&url).json(&payload);
    if !key.is_empty() {
        request = match auth_scheme.as_str() {
            "x_api_key" => request
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            _ => request.header("Authorization", format!("Bearer {key}")),
        };
    }

    let response = request
        .send()
        .await
        .map_err(|error| {
            if error.is_connect() {
                format!("无法连接模型服务。请检查 Base URL、端口和网络；HTTPS 地址还需确认服务已启用 TLS。详情：{error}")
            } else if error.is_timeout() {
                format!("模型服务连接超时。请检查服务状态和网络。详情：{error}")
            } else {
                format!("请求模型失败：{error}")
            }
        })?;
    let status = response.status();
    if status.is_success() {
        let (body, truncated) =
            provider_response_bytes(response, MAX_INFERENCE_PROBE_BYTES).await?;
        if truncated {
            return Err("模型测试响应超过 1MB 限制".into());
        }
        return crate::provider_response::validate_inference(&body, api_backend, key);
    }

    let (body, truncated) = provider_response_bytes(response, MAX_PROVIDER_ERROR_BYTES)
        .await
        .unwrap_or_default();
    let snippet = crate::provider_response::error_detail(&body, key, truncated, 300);
    Err(if snippet.trim().is_empty() {
        format!("模型接口返回 {status}")
    } else {
        format!("模型接口返回 {status}：{snippet}")
    })
}

async fn fetch_models(
    provider_kind: &str,
    api_key: &str,
    base_url: &Option<String>,
    explicit_auth_scheme: Option<&str>,
    allow_insecure_http: bool,
) -> Result<Vec<FetchedModel>, String> {
    let key = api_key.trim();
    let base = resolve_fetch_base_url(provider_kind, base_url, allow_insecure_http)?;
    if key.is_empty() && !is_ojlab_base_url(&base) {
        return Err("请先填写 API Key".into());
    }
    let auth_scheme = explicit_auth_scheme
        .map(str::trim)
        .filter(|scheme| !scheme.is_empty())
        .map(str::to_string)
        .or_else(|| preset(provider_kind).map(|p| p.auth_scheme.to_string()))
        .unwrap_or_else(|| "bearer".into());
    validate_auth_scheme(&auth_scheme)?;

    let url = format!("{base}/models");

    let client = provider_http_client(std::time::Duration::from_secs(15))?;

    let mut req = client.get(&url);
    // Auth header per the provider's scheme.
    if !key.is_empty() {
        req = match auth_scheme.as_str() {
            "x_api_key" => req
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01"),
            _ => req.header("Authorization", format!("Bearer {key}")),
        };
    }

    let resp = req.send().await.map_err(|e| format!("请求失败：{e}"))?;

    let status = resp.status();
    if !status.is_success() {
        let (body, truncated) = provider_response_bytes(resp, MAX_PROVIDER_ERROR_BYTES)
            .await
            .unwrap_or_default();
        let snippet = crate::provider_response::error_detail(&body, key, truncated, 200);
        return Err(format!("API 返回 {status}：{snippet}"));
    }

    // Parse the OpenAI-compatible `{ "data": [{ "id": "…", "owned_by": "…" }] }` shape.
    if resp
        .content_length()
        .is_some_and(|length| length > MAX_MODEL_LIST_BYTES as u64)
    {
        return Err("模型列表响应超过 4MB 限制".into());
    }
    let (body, truncated) = provider_response_bytes(resp, MAX_MODEL_LIST_BYTES).await?;
    if truncated {
        return Err("模型列表响应超过 4MB 限制".into());
    }
    let json: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("解析响应失败（非 JSON）：{e}"))?;

    parse_fetched_models(&json, is_ojlab_base_url(&base))
}

fn parse_fetched_models(
    json: &serde_json::Value,
    builtin_service: bool,
) -> Result<Vec<FetchedModel>, String> {
    let data = json
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| "响应缺少 data 数组（该端点可能不支持 /models 列表）".to_string())?;

    let models = data
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(|v| v.as_str())?.to_string();
            if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) {
                return None;
            }
            if builtin_service && !BUILTIN_CHAT_MODELS.iter().any(|(slug, _, _)| *slug == id) {
                return None;
            }
            let owned_by = item
                .get("owned_by")
                .and_then(|v| v.as_str())
                .filter(|value| value.len() <= 512 && !value.chars().any(char::is_control))
                .map(|s| s.to_string());
            Some(FetchedModel { id, owned_by })
        })
        .take(MAX_DISCOVERED_MODELS)
        .collect::<Vec<_>>();

    Ok(models)
}

// ---------------------------------------------------------------------------
// Unit tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_and_model_filters_keep_their_semantics_after_catalog_migration() {
        for preferred in [None, Some(""), Some("  "), Some("mine")] {
            let mut config: Value = r#"
[model.mine]
model_provider = "mine"
model = "chat-qwen"
[models]
allowed_models = ["mine"]
"#
            .parse()
            .unwrap();
            if let Some(preferred) = preferred {
                config["models"]
                    .as_table_mut()
                    .unwrap()
                    .insert("default".into(), Value::String(preferred.into()));
            }
            apply_builtin_model_config(&mut config).unwrap();
            assert_eq!(
                config["models"].get("default").and_then(Value::as_str),
                preferred.map(|value| if value.trim().is_empty() { "" } else { value })
            );
            assert_eq!(
                config["models"]["allowed_models"].as_array().unwrap(),
                &vec![Value::String("mine".into())]
            );
        }
        let mut config: Value = r#"
[models]
default = "echoagent-ojlab/chat-qwen"
session_summary = "echoagent-ojlab/chat-glm"
web_search = "mine/chat-qwen"
allowed_models = ["echoagent-ojlab/chat-*", "mine/*"]
hidden_models = ["chat-gl[m]"]
disabled_models = ["echoagent-ojlab/chat-xc"]
"#
        .parse()
        .unwrap();
        apply_builtin_model_config(&mut config).unwrap();
        for key in ["default", "session_summary"] {
            assert_eq!(
                config["models"][key].as_str(),
                Some(BUILTIN_DEFAULT_MODEL_ID)
            );
        }
        assert_eq!(
            config["models"]["web_search"].as_str(),
            Some("mine/chat-qwen")
        );
        for key in ["allowed_models", "hidden_models", "disabled_models"] {
            assert!(config["models"][key]
                .as_array()
                .unwrap()
                .contains(&Value::String(BUILTIN_DEFAULT_MODEL_ID.into())));
        }
        let first = config.clone();
        apply_builtin_model_config(&mut config).unwrap();
        assert_eq!(config, first);
    }

    #[test]
    fn builtin_catalog_is_idempotent_and_preserves_personal_and_organization_models() {
        let mut config: Value = r#"
[model_providers.mine]
base_url = "https://example.com/v1"
api_key = "personal-key"
[model.mine]
model_provider = "mine"
model = "mine"
[model_providers.echoagent-organization]
echoagent_managed_by = "organization"
[model."organization/team"]
model_provider = "echoagent-organization"
model = "team"
"#
        .parse()
        .unwrap();
        apply_builtin_model_config(&mut config).unwrap();
        let first = config.clone();
        apply_builtin_model_config(&mut config).unwrap();
        assert_eq!(config, first);
        assert!(config["model"]["mine"].is_table());
        assert!(config["model"]["organization/team"].is_table());
        assert_eq!(
            config["model"][BUILTIN_DEFAULT_MODEL_ID]["model"].as_str(),
            Some("Echo Reed")
        );
        assert_eq!(
            config["models"].get("default"),
            None,
            "Auto must stay unset when no explicit default was saved"
        );
        assert_eq!(config["model"].as_table().unwrap().len(), 7);
        for (slug, context_window, max_input_tokens, max_output_tokens) in [
            ("Echo Cedar", 131_072, 122_880, 8_192),
            ("Echo Willow", 32_768, 24_576, 8_192),
            ("Echo Grove", 262_144, 253_952, 8_192),
            ("Echo Brook", 131_072, 122_880, 8_192),
            ("Echo Reed", 262_144, 196_608, 65_536),
        ] {
            let entry = &config["model"][format!("{BUILTIN_MODEL_PREFIX}{slug}").as_str()];
            assert_eq!(entry["model"].as_str(), Some(slug));
            assert_eq!(entry["name"].as_str(), Some(slug));
            assert_eq!(entry["context_window"].as_integer(), Some(context_window));
            assert_eq!(
                entry["max_completion_tokens"].as_integer(),
                Some(max_output_tokens)
            );
            assert_eq!(context_window - max_output_tokens, max_input_tokens);
            if matches!(slug, "Echo Willow" | "Echo Reed") {
                assert_eq!(
                    entry["auto_compact_threshold_percent"].as_integer(),
                    Some(70)
                );
            }
        }
        let provider = config["model_providers"][BUILTIN_PROVIDER_ID]
            .as_table()
            .unwrap();
        assert!(provider_can_omit_api_key(provider));
        assert!(provider_from_table(BUILTIN_PROVIDER_ID, provider).managed);
        assert!(resolved_provider_api_key(provider).is_none());
        assert!(validate_personal_provider_target(&config, BUILTIN_PROVIDER_ID).is_err());
        assert!(validate_personal_model_target(&config, BUILTIN_DEFAULT_MODEL_ID).is_err());
    }

    #[test]
    fn builtin_catalog_replaces_retired_models_and_migrates_only_builtin_defaults() {
        for preferred in [
            "",
            "echoagent-ojlab/chat-xc",
            "echoagent-ojlab/chat-glm",
            "echoagent-ojlab/chat-qwen",
            "mine",
            "organization/team",
            "echoagent-ojlab/Echo Grove",
        ] {
            let mut config: Value = r#"
[model."echoagent-ojlab/chat-xc"]
model_provider = "echoagent-ojlab"
model = "chat-xc"
[model."echoagent-ojlab/chat-glm"]
echoagent_managed_by = "builtin"
model = "chat-glm"
[model."echoagent-ojlab/chat-qwen"]
model_provider = "echoagent-ojlab"
model = "chat-qwen"
[model."echoagent-ojlab/embed-pro"]
model_provider = "echoagent-ojlab"
model = "embed-pro"
[model.mine]
model_provider = "mine"
model = "chat-xc"
[models]
default = ""
"#
            .parse()
            .unwrap();
            config["models"]["default"] = Value::String(preferred.into());
            let personal = config["model"]["mine"].clone();
            apply_builtin_model_config(&mut config).unwrap();
            assert_eq!(config["model"].as_table().unwrap().len(), 6);
            assert_eq!(config["model"]["mine"], personal);
            for retired in [
                "chat-xc",
                "chat-glm",
                "chat-qwen",
                "embed-pro",
                "rerank-pro",
            ] {
                assert!(config["model"]
                    .get(format!("{BUILTIN_MODEL_PREFIX}{retired}"))
                    .is_none());
            }
            let expected = if is_legacy_builtin_model_id(preferred) {
                BUILTIN_DEFAULT_MODEL_ID
            } else {
                preferred
            };
            assert_eq!(config["models"]["default"].as_str(), Some(expected));
            let first = config.clone();
            apply_builtin_model_config(&mut config).unwrap();
            assert_eq!(config, first);
        }
    }

    #[test]
    fn builtin_model_discovery_excludes_retrieval_and_retired_models() {
        let response = serde_json::json!({ "data": [
            {"id": "Echo Cedar"}, {"id": "Echo Willow"}, {"id": "Echo Grove"},
            {"id": "Echo Brook"}, {"id": "Echo Reed"}, {"id": "embed-pro"},
            {"id": "rerank-pro"}, {"id": "chat-xc"}, {"id": "chat-glm"},
            {"id": "chat-qwen"}, {"id": "Echo Unknown"}, {"id": ""},
        ] });
        let builtin = parse_fetched_models(&response, true).unwrap();
        assert_eq!(
            builtin
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Echo Cedar",
                "Echo Willow",
                "Echo Grove",
                "Echo Brook",
                "Echo Reed"
            ]
        );
        // A personal service keeps its own catalog; this restriction is specific
        // to the known built-in endpoint.
        assert_eq!(parse_fetched_models(&response, false).unwrap().len(), 11);
    }

    #[test]
    fn only_the_named_http_service_can_run_without_a_key() {
        assert!(validate_provider_base_url_with_policy(
            crate::agent_runtime::OJLAB_BASE_URL,
            false
        )
        .is_ok());
        assert!(validate_provider_base_url_with_policy(
            "http://123.56.188.16:8088/v1/other",
            false
        )
        .is_err());
        assert!(
            validate_provider_base_url_with_policy("http://123.56.188.16.evil:8088/v1", false)
                .is_err()
        );
        let mut provider = Map::new();
        provider.insert(
            "base_url".into(),
            Value::String(crate::agent_runtime::OJLAB_BASE_URL.into()),
        );
        provider.insert(
            "api_backend".into(),
            Value::String("chat_completions".into()),
        );
        assert_eq!(resolved_or_optional_api_key(&provider), Some(String::new()));
        provider.insert(
            "base_url".into(),
            Value::String("https://example.com/v1".into()),
        );
        assert_eq!(resolved_or_optional_api_key(&provider), None);

        let rendered = provider_to_table(
            &ModelProviderEntry {
                provider_kind: "custom".into(),
                base_url: Some(crate::agent_runtime::OJLAB_BASE_URL.into()),
                api_backend: Some("responses".into()),
                auth_scheme: Some("x_api_key".into()),
                api_key: Some("must-not-be-sent".into()),
                allow_insecure_http: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        assert!(rendered.as_table().unwrap().get("api_key").is_none());
        assert_eq!(rendered["api_backend"].as_str(), Some("chat_completions"));
        assert_eq!(rendered["auth_scheme"].as_str(), Some("bearer"));
    }

    // --- model_config_revision ---

    /// `internal/reload_models` reloads the whole `[models]` table, so a filter
    /// edit must move the revision. Otherwise a stale Runtime catalog could be
    /// certified as synchronized against the new configuration.
    #[test]
    fn revision_tracks_model_filter_changes() {
        let base: Value = r#"
[models]
default = "model-a"
[model.model-a]
[model_providers.p1]
base_url = "https://example.com"
"#
        .parse()
        .expect("base config parses");
        let filtered: Value = r#"
[models]
default = "model-a"
hidden_models = ["model-a"]
[model.model-a]
[model_providers.p1]
base_url = "https://example.com"
"#
        .parse()
        .expect("filtered config parses");

        assert_ne!(
            model_config_revision_of(&base),
            model_config_revision_of(&filtered),
            "a [models] filter edit must change the revision"
        );
        assert_eq!(
            model_config_revision_of(&base),
            model_config_revision_of(&base.clone()),
            "the digest must be stable for identical input"
        );
    }

    /// Sections the reload endpoint never applies must not move the revision, or
    /// an unrelated save would lock chat until the next reload.
    #[test]
    fn revision_ignores_unrelated_sections() {
        let base: Value = "[models]\ndefault = \"model-a\"\n"
            .parse()
            .expect("base parses");
        let with_extra: Value = "[models]\ndefault = \"model-a\"\n[tui]\ntheme = \"dark\"\n"
            .parse()
            .expect("extended parses");
        assert_eq!(
            model_config_revision_of(&base),
            model_config_revision_of(&with_extra)
        );
    }

    // --- infer_provider_kind ---

    #[test]
    fn infer_anthropic_from_messages_backend() {
        let mut table = Map::new();
        table.insert("api_backend".into(), Value::String("messages".into()));
        table.insert(
            "base_url".into(),
            Value::String("https://api.anthropic.com/v1".into()),
        );
        assert_eq!(infer_provider_kind(&table), "anthropic");
    }

    #[test]
    fn infer_openai() {
        let mut table = Map::new();
        table.insert(
            "api_backend".into(),
            Value::String("chat_completions".into()),
        );
        table.insert(
            "base_url".into(),
            Value::String("https://api.openai.com/v1".into()),
        );
        assert_eq!(infer_provider_kind(&table), "openai");
    }

    #[test]
    fn infer_minimax_enables_meeting_capability_without_model_name_matching() {
        let mut table = Map::new();
        table.insert(
            "api_backend".into(),
            Value::String("chat_completions".into()),
        );
        table.insert(
            "base_url".into(),
            Value::String("https://api.minimax.cn/v1".into()),
        );
        assert_eq!(infer_provider_kind(&table), "minimax");
    }

    #[test]
    fn meeting_access_accepts_personal_and_organization_compatible_speech_gateways() {
        let mut config: Value = r#"
            [model_providers.personal]
            base_url = "https://gateway.example/v1"
            api_backend = "chat_completions"
            auth_scheme = "bearer"
            api_key = "personal-key"
            [model_providers.echoagent-organization]
            base_url = "http://managed.example/v1"
            api_backend = "chat_completions"
            auth_scheme = "bearer"
            api_key = "organization-key"
            echoagent_allow_insecure_http = true
            echoagent_managed_by = "organization"
            echoagent_lease_until = 9223372036854775807
            organization_provider = "openai-compatible"
            [model."personal/MiniMax-M3"]
            model_provider = "personal"
            model = "MiniMax-M3"
            [model."organization/MiniMax-M3"]
            model_provider = "echoagent-organization"
            model = "MiniMax-M3"
        "#
        .parse()
        .unwrap();
        let personal =
            meeting_provider_access_from_config(&config, "personal/MiniMax-M3", "personal")
                .unwrap();
        assert_eq!(personal.base_url, "https://gateway.example/v1");
        assert_eq!(personal.remote_model_id, "MiniMax-M3");
        let organization = meeting_provider_access_from_config(
            &config,
            "organization/MiniMax-M3",
            ORGANIZATION_PROVIDER_ID,
        )
        .unwrap();
        assert_eq!(organization.base_url, "http://managed.example/v1");
        assert_eq!(organization.api_key, "organization-key");
        config
            .get_mut("model_providers")
            .and_then(Value::as_table_mut)
            .and_then(|providers| providers.get_mut(ORGANIZATION_PROVIDER_ID))
            .and_then(Value::as_table_mut)
            .unwrap()
            .insert(LEASE_UNTIL_KEY.into(), Value::Integer(0));
        assert!(meeting_provider_access_from_config(
            &config,
            "organization/MiniMax-M3",
            ORGANIZATION_PROVIDER_ID,
        )
        .err()
        .unwrap()
        .contains("过期"));
    }

    #[test]
    fn meeting_access_rejects_unapproved_http_and_unsupported_chat_protocol() {
        let mut config: Value = r#"
            [model_providers.personal]
            base_url = "http://gateway.example/v1"
            api_backend = "chat_completions"
            auth_scheme = "bearer"
            api_key = "personal-key"
            [model.meeting]
            model_provider = "personal"
            model = "MiniMax-M3"
        "#
        .parse()
        .unwrap();
        assert!(meeting_provider_access_from_config(&config, "meeting", "personal").is_err());
        let provider = config
            .get_mut("model_providers")
            .and_then(Value::as_table_mut)
            .and_then(|providers| providers.get_mut("personal"))
            .and_then(Value::as_table_mut)
            .unwrap();
        provider.insert(ALLOW_INSECURE_HTTP_KEY.into(), Value::Boolean(true));
        provider.insert("api_backend".into(), Value::String("messages".into()));
        assert!(meeting_provider_access_from_config(&config, "meeting", "personal").is_err());
    }

    #[test]
    fn meeting_access_rejects_builtin_even_with_a_minimax_model_name() {
        let config: Value = r#"
            [model_providers.echoagent-ojlab]
            base_url = "https://example.com/v1"
            api_backend = "chat_completions"
            auth_scheme = "bearer"
            api_key = "test-key"
            echoagent_managed_by = "builtin"
            [model."echoagent-ojlab/MiniMax-M3"]
            model_provider = "echoagent-ojlab"
            model = "MiniMax-M3"
        "#
        .parse()
        .unwrap();
        let error = meeting_provider_access_from_config(
            &config,
            "echoagent-ojlab/MiniMax-M3",
            BUILTIN_PROVIDER_ID,
        )
        .err()
        .unwrap();
        assert!(error.contains("内置"));
    }

    #[test]
    fn infer_custom_for_unknown() {
        let mut table = Map::new();
        table.insert(
            "api_backend".into(),
            Value::String("chat_completions".into()),
        );
        table.insert(
            "base_url".into(),
            Value::String("https://my-custom.api/v1".into()),
        );
        assert_eq!(infer_provider_kind(&table), "custom");
    }

    // --- allocate_provider_id ---

    #[test]
    fn allocate_id_first_use() {
        let mut taken = std::collections::HashSet::new();
        assert_eq!(allocate_provider_id("openai", &mut taken), "openai");
        assert!(taken.contains("openai"));
    }

    #[test]
    fn allocate_id_dedup_suffix() {
        let mut taken = std::collections::HashSet::new();
        taken.insert("custom".into());
        assert_eq!(allocate_provider_id("custom", &mut taken), "custom-2");
        assert_eq!(allocate_provider_id("custom", &mut taken), "custom-3");
    }

    // --- provider_to_table (new shape) ---

    #[test]
    fn provider_to_table_openai_uses_responses_transport() {
        let p = ModelProviderEntry {
            id: "openai".into(),
            provider_kind: "openai".into(),
            api_key: Some("sk-test".into()),
            ..Default::default()
        };
        let result = provider_to_table(&p, None).unwrap();
        let table = result.as_table().unwrap();
        assert_eq!(
            table["base_url"].as_str(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(table["api_backend"].as_str(), Some("responses"));
        assert_eq!(table["auth_scheme"].as_str(), Some("bearer"));
        assert!(!table.contains_key(ALLOW_INSECURE_HTTP_KEY));
    }

    #[test]
    fn personal_http_requires_opt_in_and_https_never_needs_it() {
        let mut provider = ModelProviderEntry {
            id: "custom".into(),
            provider_kind: "custom".into(),
            api_key: Some("test-key".into()),
            base_url: Some("http://example.com:60100/v1".into()),
            api_backend: Some("chat_completions".into()),
            auth_scheme: Some("bearer".into()),
            source: "personal".into(),
            ..Default::default()
        };
        assert!(provider_to_table(&provider, None).is_err());
        provider.allow_insecure_http = true;
        let approved = provider_to_table(&provider, None).expect("opted-in personal HTTP");
        let table = approved.as_table().unwrap();
        assert_eq!(
            table.get(ALLOW_INSECURE_HTTP_KEY).and_then(Value::as_bool),
            Some(true)
        );
        assert!(provider_from_table("custom", table).allow_insecure_http);
        assert_eq!(
            table.get("base_url").and_then(Value::as_str),
            Some("http://example.com:60100/v1")
        );

        provider.base_url = Some("https://example.com/v1".into());
        let secure = provider_to_table(&provider, Some(&approved)).expect("HTTPS is unaffected");
        assert!(!secure
            .as_table()
            .unwrap()
            .contains_key(ALLOW_INSECURE_HTTP_KEY));

        provider.base_url = Some("http://example.com:60100/v1".into());
        provider.source = "organization".into();
        provider.managed = true;
        assert!(provider_to_table(&provider, None).is_err());
    }

    #[test]
    fn keyless_remote_service_still_requires_personal_http_consent() {
        let mut provider = ModelProviderEntry {
            id: "custom".into(),
            provider_kind: "custom".into(),
            base_url: Some(crate::agent_runtime::OJLAB_BASE_URL.into()),
            source: "personal".into(),
            ..Default::default()
        };
        assert!(provider_to_table(&provider, None).is_err());
        provider.allow_insecure_http = true;
        let approved = provider_to_table(&provider, None).unwrap();
        assert_eq!(approved[ALLOW_INSECURE_HTTP_KEY].as_bool(), Some(true));
    }

    #[test]
    fn runtime_http_approval_uses_personal_and_canonical_organization_urls() {
        let config: Value = r#"
            [model_providers.personal]
            base_url = "http://approved.example:60100/v1"
            echoagent_allow_insecure_http = true
            echoagent_source = "personal"
            [model_providers.other]
            base_url = "http://other.example/v1"
            [model_providers.organization]
            base_url = "http://organization.example/v1"
            echoagent_allow_insecure_http = true
            echoagent_managed_by = "organization"
            [model_providers.echoagent-organization]
            base_url = "http://managed.example/v1"
            echoagent_allow_insecure_http = true
            echoagent_managed_by = "organization"
            [model_providers.builtin]
            base_url = "http://builtin.example/v1"
            echoagent_allow_insecure_http = true
            echoagent_managed_by = "builtin"
        "#
        .parse()
        .unwrap();
        let approved = approved_insecure_http_urls(&config);
        assert_eq!(approved.len(), 2);
        assert!(approved.contains(&"http://approved.example:60100/v1".to_string()));
        assert!(approved.contains(&"http://managed.example/v1".to_string()));
    }

    #[test]
    fn provider_to_table_anthropic_preset() {
        let p = ModelProviderEntry {
            id: "anthropic".into(),
            provider_kind: "anthropic".into(),
            label: None,
            api_key: Some("sk-ant-test".into()),
            base_url: None,
            api_backend: None,
            auth_scheme: None,
            context_window: Some(200_000),
            ..Default::default()
        };
        let result = provider_to_table(&p, None).unwrap();
        let table = result.as_table().unwrap();
        assert_eq!(
            table["base_url"].as_str().unwrap(),
            "https://api.anthropic.com/v1"
        );
        assert_eq!(table["api_backend"].as_str().unwrap(), "messages");
        assert_eq!(table["auth_scheme"].as_str().unwrap(), "x_api_key");
        assert_eq!(table["api_key"].as_str().unwrap(), "sk-ant-test");
        assert_eq!(table["context_window"].as_integer().unwrap(), 200_000);
    }

    #[test]
    fn provider_to_table_masked_key_is_noop() {
        let mut existing = Map::new();
        existing.insert(
            "base_url".into(),
            Value::String("https://api.openai.com/v1".into()),
        );
        existing.insert(
            "api_backend".into(),
            Value::String("chat_completions".into()),
        );
        existing.insert("auth_scheme".into(), Value::String("bearer".into()));
        existing.insert("api_key".into(), Value::String("real-key".into()));

        let p = ModelProviderEntry {
            id: "openai".into(),
            provider_kind: "openai".into(),
            label: None,
            api_key: Some("••••".into()),
            base_url: None,
            api_backend: None,
            auth_scheme: None,
            context_window: None,
            ..Default::default()
        };
        let table = provider_to_table(&p, Some(&Value::Table(existing))).unwrap();
        assert_eq!(
            table.as_table().unwrap()["api_key"].as_str().unwrap(),
            "real-key"
        );
    }

    #[test]
    fn provider_to_table_custom_missing_field_errors() {
        let p = ModelProviderEntry {
            id: "custom".into(),
            provider_kind: "custom".into(),
            label: None,
            api_key: None,
            base_url: None,
            api_backend: None,
            auth_scheme: None,
            context_window: None,
            ..Default::default()
        };
        assert!(provider_to_table(&p, None).is_err());
    }

    // --- model_to_table (migration strips legacy connection fields) ---

    #[test]
    fn model_to_table_sets_reference_and_strips_legacy_fields() {
        let mut existing = Map::new();
        // Legacy per-model connection fields that must be removed on migration.
        existing.insert(
            "base_url".into(),
            Value::String("https://api.openai.com/v1".into()),
        );
        existing.insert("api_key".into(), Value::String("sk-old".into()));
        existing.insert(
            "api_backend".into(),
            Value::String("chat_completions".into()),
        );
        existing.insert("auth_scheme".into(), Value::String("bearer".into()));
        // Unrecognized key preserved.
        existing.insert("temperature".into(), Value::String("0.5".into()));

        let m = ModelEntry {
            model_id: "gpt-4o".into(),
            provider_id: "openai".into(),
            name: Some("GPT-4o".into()),
            context_window: None,
            ..Default::default()
        };
        let table = model_to_table(&m, Some(&Value::Table(existing)))
            .as_table()
            .unwrap()
            .clone();

        assert_eq!(table["model"].as_str().unwrap(), "gpt-4o");
        assert_eq!(table["model_provider"].as_str().unwrap(), "openai");
        assert_eq!(table["name"].as_str().unwrap(), "GPT-4o");
        // Legacy connection fields stripped.
        assert!(!table.contains_key("base_url"));
        assert!(!table.contains_key("api_key"));
        assert!(!table.contains_key("api_backend"));
        assert!(!table.contains_key("auth_scheme"));
        // Unrecognized key preserved.
        assert_eq!(table["temperature"].as_str().unwrap(), "0.5");
    }

    #[test]
    fn model_to_table_per_model_context_window_override() {
        let m = ModelEntry {
            model_id: "gpt-4o".into(),
            provider_id: "openai".into(),
            name: None,
            context_window: Some(128_000),
            max_output_tokens: Some(8_192),
            ..Default::default()
        };
        let table = model_to_table(&m, None).as_table().unwrap().clone();
        assert_eq!(table["context_window"].as_integer().unwrap(), 128_000);
        assert_eq!(table["max_completion_tokens"].as_integer(), Some(8_192));
    }

    #[test]
    fn model_output_limit_can_be_preserved_or_explicitly_cleared() {
        let mut existing = Map::new();
        existing.insert("max_completion_tokens".into(), Value::Integer(8_192));
        let mut model = ModelEntry {
            model_id: "my-model".into(),
            provider_id: "my-provider".into(),
            ..Default::default()
        };
        assert_eq!(
            model_to_table(&model, Some(&Value::Table(existing.clone())))["max_completion_tokens"]
                .as_integer(),
            Some(8_192)
        );
        model.clear_max_output_tokens = true;
        assert!(model_to_table(&model, Some(&Value::Table(existing)))
            .get("max_completion_tokens")
            .is_none());
        model.max_output_tokens = Some(16_384);
        assert!(validate_model_limits(&model, Some(32_768), None).is_err());
        model.clear_max_output_tokens = false;
        model.max_output_tokens = None;
        model.context_window = Some(4_096);
        let existing = Value::Table(Map::from_iter([(
            "max_completion_tokens".into(),
            Value::Integer(8_192),
        )]));
        assert!(validate_model_limits(&model, Some(32_768), Some(&existing)).is_err());
    }

    #[test]
    fn model_output_clear_flag_is_request_only() {
        let model: ModelEntry = serde_json::from_value(serde_json::json!({
            "modelId": "my-model",
            "providerId": "my-provider",
            "clearMaxOutputTokens": true
        }))
        .unwrap();
        assert!(model.clear_max_output_tokens);
        assert!(serde_json::to_value(model)
            .unwrap()
            .get("clearMaxOutputTokens")
            .is_none());
    }

    #[test]
    fn model_to_table_keeps_local_id_separate_from_remote_slug() {
        let model = ModelEntry {
            model_id: "custom-2/shared-model".into(),
            remote_model_id: Some("shared-model".into()),
            provider_id: "custom-2".into(),
            ..Default::default()
        };
        let table = model_to_table(&model, None);
        assert_eq!(table["model"].as_str(), Some("shared-model"));
        assert_eq!(table["model_provider"].as_str(), Some("custom-2"));
    }

    #[test]
    fn allocate_model_id_namespaces_cross_provider_collision() {
        let mut models = Map::new();
        let mut first = Map::new();
        first.insert("model".into(), Value::String("shared-model".into()));
        first.insert("model_provider".into(), Value::String("first".into()));
        models.insert("shared-model".into(), Value::Table(first));

        assert_eq!(
            allocate_model_id("shared-model", "second", &models),
            "second/shared-model"
        );
    }

    #[test]
    fn replacing_connection_models_removes_only_unselected_models_from_that_connection() {
        let mut models = Map::new();
        for (local_id, provider_id) in [
            ("keep", "personal"),
            ("remove", "personal"),
            ("other", "other-provider"),
        ] {
            models.insert(
                local_id.into(),
                Value::Table(Map::from_iter([
                    ("model".into(), Value::String(local_id.into())),
                    ("model_provider".into(), Value::String(provider_id.into())),
                ])),
            );
        }

        let removed = remove_unselected_provider_models(
            &mut models,
            "personal",
            &std::collections::HashSet::from(["keep".to_string()]),
        );

        assert_eq!(removed, vec!["remove"]);
        assert!(models.contains_key("keep"));
        assert!(!models.contains_key("remove"));
        assert!(models.contains_key("other"));
    }

    #[test]
    fn organization_model_config_preserves_personal_entries_and_uses_exact_model_slug() {
        let mut root = Map::new();
        let mut personal_provider = Map::new();
        personal_provider.insert(
            "base_url".into(),
            Value::String("https://personal.example/v1".into()),
        );
        personal_provider.insert("api_key".into(), Value::String("sk-personal".into()));
        let mut providers = Map::new();
        providers.insert("personal".into(), Value::Table(personal_provider));
        root.insert("model_providers".into(), Value::Table(providers));

        let mut personal_model = Map::new();
        personal_model.insert("model".into(), Value::String("glm-5".into()));
        personal_model.insert("model_provider".into(), Value::String("personal".into()));
        let mut models = Map::new();
        models.insert("glm-5".into(), Value::Table(personal_model));
        root.insert("model".into(), Value::Table(models));

        let mut config = Value::Table(root);
        let local_id = apply_organization_model_config(
            &mut config,
            &OrganizationModelConfig {
                provider: "openai-compatible".into(),
                model: "glm-5".into(),
                base_url: "https://organization.example/v1/".into(),
                api_key: "sk-organization".into(),
                lease_until: u64::MAX,
            },
        )
        .unwrap();

        assert_eq!(local_id, "organization/glm-5");
        let root = config.as_table().unwrap();
        let providers = root["model_providers"].as_table().unwrap();
        assert_eq!(
            providers["personal"]["api_key"].as_str(),
            Some("sk-personal")
        );
        let organization = providers[ORGANIZATION_PROVIDER_ID].as_table().unwrap();
        assert_eq!(
            organization["base_url"].as_str(),
            Some("https://organization.example/v1")
        );
        assert_eq!(organization["api_key"].as_str(), Some("sk-organization"));
        assert_eq!(
            organization["api_backend"].as_str(),
            Some("chat_completions")
        );
        assert_eq!(organization["auth_scheme"].as_str(), Some("bearer"));

        let models = root["model"].as_table().unwrap();
        assert!(models.contains_key("glm-5"));
        let organization_model = models["organization/glm-5"].as_table().unwrap();
        assert_eq!(organization_model["model"].as_str(), Some("glm-5"));
        assert_eq!(
            organization_model["model_provider"].as_str(),
            Some(ORGANIZATION_PROVIDER_ID)
        );
    }

    #[test]
    fn organization_http_model_approves_only_its_current_base_url() {
        let mut config = Value::Table(Map::new());
        apply_organization_model_config(
            &mut config,
            &OrganizationModelConfig {
                provider: "openai-compatible".into(),
                model: "MiniMax-M3".into(),
                base_url: "http://123.56.188.16:60100/v1/".into(),
                api_key: "sk-organization".into(),
                lease_until: u64::MAX,
            },
        )
        .unwrap();
        let organization = config["model_providers"][ORGANIZATION_PROVIDER_ID]
            .as_table()
            .unwrap();
        assert_eq!(
            organization["base_url"].as_str(),
            Some("http://123.56.188.16:60100/v1")
        );
        assert_eq!(organization[ALLOW_INSECURE_HTTP_KEY].as_bool(), Some(true));
        assert!(provider_from_table(ORGANIZATION_PROVIDER_ID, organization).allow_insecure_http);
        assert_eq!(
            approved_insecure_http_urls(&config),
            vec!["http://123.56.188.16:60100/v1"]
        );

        apply_organization_model_config(
            &mut config,
            &OrganizationModelConfig {
                provider: "openai-compatible".into(),
                model: "MiniMax-M3".into(),
                base_url: "https://managed.example/v1".into(),
                api_key: "sk-organization".into(),
                lease_until: u64::MAX,
            },
        )
        .unwrap();
        assert!(approved_insecure_http_urls(&config).is_empty());
        assert!(config["model_providers"][ORGANIZATION_PROVIDER_ID]
            .get(ALLOW_INSECURE_HTTP_KEY)
            .is_none());
    }

    #[test]
    fn organization_model_config_replaces_only_the_previous_downloaded_model() {
        let mut config = Value::Table(Map::new());
        for model in ["old-model", "new-model"] {
            apply_organization_model_config(
                &mut config,
                &OrganizationModelConfig {
                    provider: "openai".into(),
                    model: model.into(),
                    base_url: "https://api.openai.com/v1".into(),
                    api_key: format!("sk-{model}"),
                    lease_until: u64::MAX,
                },
            )
            .unwrap();
            if model == "old-model" {
                ensure_table(&mut config, "models").unwrap().insert(
                    "default".into(),
                    Value::String("organization/old-model".into()),
                );
            }
        }

        let models = config.get("model").and_then(Value::as_table).unwrap();
        assert!(!models.contains_key("organization/old-model"));
        assert!(models.contains_key("organization/new-model"));
        assert!(config["models"].get("default").is_none());
        let provider = config
            .get("model_providers")
            .and_then(Value::as_table)
            .and_then(|providers| providers.get(ORGANIZATION_PROVIDER_ID))
            .and_then(Value::as_table)
            .unwrap();
        assert_eq!(provider["api_key"].as_str(), Some("sk-new-model"));
    }

    #[test]
    fn organization_cleanup_preserves_personal_entries_and_clears_default() {
        let mut config = Value::Table(Map::new());
        apply_organization_model_config(
            &mut config,
            &OrganizationModelConfig {
                provider: "openai".into(),
                model: "managed-model".into(),
                base_url: "https://organization.example/v1".into(),
                api_key: "sk-managed".into(),
                lease_until: u64::MAX,
            },
        )
        .unwrap();
        config["model_providers"].as_table_mut().unwrap().insert(
            "personal".into(),
            Value::Table(Map::from_iter([
                (
                    "base_url".into(),
                    Value::String("https://personal.example/v1".into()),
                ),
                ("api_key".into(), Value::String("sk-personal".into())),
            ])),
        );
        config["model"].as_table_mut().unwrap().insert(
            "personal-model".into(),
            Value::Table(Map::from_iter([
                ("model".into(), Value::String("personal-model".into())),
                ("model_provider".into(), Value::String("personal".into())),
            ])),
        );
        config.as_table_mut().unwrap().insert(
            "models".into(),
            Value::Table(Map::from_iter([(
                "default".into(),
                Value::String("organization/managed-model".into()),
            )])),
        );

        assert!(remove_organization_model_config_from(&mut config));
        assert!(config["model_providers"].get("personal").is_some());
        assert!(config["model"].get("personal-model").is_some());
        assert!(config["model_providers"]
            .get(ORGANIZATION_PROVIDER_ID)
            .is_none());
        assert!(config["model"].get("organization/managed-model").is_none());
        assert!(config["models"].get("default").is_none());
    }

    // --- group_legacy_models (lazy migration, display only) ---

    #[test]
    fn group_legacy_merges_same_endpoint_into_one_provider() {
        let mut models = Map::new();
        for (mid, key) in [
            ("gpt-4o", "sk-aaa"),
            ("gpt-4o-mini", "sk-aaa"),
            ("claude-sonnet-4-5", "sk-ant-bbb"),
        ] {
            let mut entry = Map::new();
            let is_anthropic = mid.starts_with("claude");
            entry.insert(
                "base_url".into(),
                Value::String(
                    if is_anthropic {
                        "https://api.anthropic.com/v1"
                    } else {
                        "https://api.openai.com/v1"
                    }
                    .into(),
                ),
            );
            entry.insert(
                "api_backend".into(),
                Value::String(
                    if is_anthropic {
                        "messages"
                    } else {
                        "chat_completions"
                    }
                    .into(),
                ),
            );
            entry.insert("api_key".into(), Value::String(key.into()));
            models.insert(mid.into(), Value::Table(entry));
        }

        let mut taken = std::collections::HashSet::new();
        let (providers, out_models) = group_legacy_models(&models, &mut taken);

        // Two provider groups (openai + anthropic), three models total.
        assert_eq!(providers.len(), 2);
        assert_eq!(out_models.len(), 3);
        // Each model references one of the synthetic providers.
        let pids: Vec<String> = providers.iter().map(|p| p.id.clone()).collect();
        assert!(pids.contains(&"openai".to_string()));
        assert!(pids.contains(&"anthropic".to_string()));
        // Models grouped under the matching provider.
        for m in &out_models {
            if m.model_id.starts_with("gpt") {
                assert_eq!(m.provider_id, "openai");
            } else {
                assert_eq!(m.provider_id, "anthropic");
            }
        }
        // Disk-like input map is unchanged (display-only grouping).
        assert_eq!(models.len(), 3);
        assert!(models["gpt-4o"].as_table().unwrap().contains_key("api_key"));
    }

    #[test]
    fn group_legacy_keeps_different_credentials_separate() {
        let mut models = Map::new();
        for (model_id, api_key) in [("model-a", "sk-a"), ("model-b", "sk-b")] {
            models.insert(
                model_id.into(),
                Value::Table(Map::from_iter([
                    (
                        "base_url".into(),
                        Value::String("https://gateway.example/v1".into()),
                    ),
                    ("api_key".into(), Value::String(api_key.into())),
                    (
                        "api_backend".into(),
                        Value::String("chat_completions".into()),
                    ),
                ])),
            );
        }

        let mut taken = std::collections::HashSet::new();
        let (providers, grouped_models) = group_legacy_models(&models, &mut taken);
        assert_eq!(providers.len(), 2);
        assert_eq!(grouped_models.len(), 2);
        assert_ne!(grouped_models[0].provider_id, grouped_models[1].provider_id);
    }

    #[test]
    fn legacy_provider_details_keeps_secret_native_for_transparent_migration() {
        let mut config = Value::Table(Map::new());
        config.as_table_mut().unwrap().insert(
            "model".into(),
            Value::Table(Map::from_iter([(
                "legacy-model".into(),
                Value::Table(Map::from_iter([
                    (
                        "base_url".into(),
                        Value::String("https://legacy.example/v1".into()),
                    ),
                    ("api_key".into(), Value::String("sk-legacy".into())),
                    (
                        "api_backend".into(),
                        Value::String("chat_completions".into()),
                    ),
                ])),
            )])),
        );

        let (connection, members) = legacy_provider_details(&config, "custom").unwrap();
        assert_eq!(connection["api_key"].as_str(), Some("sk-legacy"));
        assert_eq!(
            connection["base_url"].as_str(),
            Some("https://legacy.example/v1")
        );
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].model_id, "legacy-model");
    }

    #[test]
    fn group_legacy_skips_migrated_entries() {
        let mut models = Map::new();
        let mut migrated = Map::new();
        migrated.insert("model_provider".into(), Value::String("openai".into()));
        models.insert("gpt-4o".into(), Value::Table(migrated));

        let mut taken = std::collections::HashSet::new();
        let (providers, out_models) = group_legacy_models(&models, &mut taken);
        assert!(providers.is_empty());
        assert!(out_models.is_empty());
    }

    #[test]
    fn group_legacy_dedups_custom_ids() {
        let mut models = Map::new();
        for (mid, url) in [
            ("m1", "https://endpoint-a/v1"),
            ("m2", "https://endpoint-b/v1"),
        ] {
            let mut entry = Map::new();
            entry.insert("base_url".into(), Value::String(url.into()));
            entry.insert(
                "api_backend".into(),
                Value::String("chat_completions".into()),
            );
            entry.insert("api_key".into(), Value::String("k".into()));
            models.insert(mid.into(), Value::Table(entry));
        }
        let mut taken = std::collections::HashSet::new();
        let (providers, _) = group_legacy_models(&models, &mut taken);
        let ids: Vec<String> = providers.iter().map(|p| p.id.clone()).collect();
        assert!(ids.contains(&"custom".to_string()));
        assert!(ids.contains(&"custom-2".to_string()));
    }

    // --- providers_list round-trip over a synthetic config ---

    #[test]
    fn providers_list_reads_new_format_and_legacy() {
        let mut config = Map::new();

        // New-format provider + referencing model.
        let mut mp = Map::new();
        mp.insert(
            "base_url".into(),
            Value::String("https://api.openai.com/v1".into()),
        );
        mp.insert(
            "api_backend".into(),
            Value::String("chat_completions".into()),
        );
        mp.insert("auth_scheme".into(), Value::String("bearer".into()));
        mp.insert("api_key".into(), Value::String("sk-xxx".into()));
        mp.insert("context_window".into(), Value::Integer(128_000));
        let mut mps = Map::new();
        mps.insert("openai".into(), Value::Table(mp));
        config.insert("model_providers".into(), Value::Table(mps));

        let mut model = Map::new();
        model.insert("model".into(), Value::String("gpt-4o".into()));
        model.insert("model_provider".into(), Value::String("openai".into()));
        model.insert("name".into(), Value::String("GPT-4o".into()));
        let mut mdls = Map::new();
        mdls.insert("gpt-4o".into(), Value::Table(model));

        // Legacy per-model entry (no model_provider).
        let mut legacy = Map::new();
        legacy.insert(
            "base_url".into(),
            Value::String("https://api.anthropic.com/v1".into()),
        );
        legacy.insert("api_backend".into(), Value::String("messages".into()));
        legacy.insert("api_key".into(), Value::String("sk-ant".into()));
        mdls.insert("claude-sonnet-4-5".into(), Value::Table(legacy));

        config.insert("model".into(), Value::Table(mdls));

        let v = Value::Table(config);
        // Reconstruct a ProviderListModel the same way providers_list does.
        let mut providers = Vec::new();
        let mut models = Vec::new();
        let mut taken = std::collections::HashSet::new();
        let root = v.as_table().unwrap();
        if let Some(mps) = root.get("model_providers").and_then(Value::as_table) {
            for (id, t) in mps {
                if let Some(tbl) = t.as_table() {
                    taken.insert(id.clone());
                    providers.push(provider_from_table(id, tbl));
                }
            }
        }
        if let Some(mdls) = root.get("model").and_then(Value::as_table) {
            for (mid, t) in mdls {
                let Some(tbl) = t.as_table() else { continue };
                if let Some(pid) = tbl.get("model_provider").and_then(Value::as_str) {
                    models.push(ModelEntry {
                        model_id: mid.clone(),
                        provider_id: pid.to_string(),
                        name: tbl.get("name").and_then(Value::as_str).map(String::from),
                        context_window: tbl
                            .get("context_window")
                            .and_then(Value::as_integer)
                            .map(|n| n as u64),
                        ..Default::default()
                    });
                }
            }
            let (mut sp, mut sm) = group_legacy_models(mdls, &mut taken);
            providers.append(&mut sp);
            models.append(&mut sm);
        }

        // One real provider (openai, with context_window) + one legacy group (anthropic).
        assert_eq!(providers.len(), 2);
        let openai = providers.iter().find(|p| p.id == "openai").unwrap();
        assert_eq!(openai.context_window, Some(128_000));
        assert_eq!(openai.api_key.as_deref(), Some("••••"));
        // gpt-4o references openai; claude-sonnet-4-5 references the legacy group.
        assert_eq!(models.len(), 2);
        let gpt = models.iter().find(|m| m.model_id == "gpt-4o").unwrap();
        assert_eq!(gpt.provider_id, "openai");
        assert_eq!(gpt.name.as_deref(), Some("GPT-4o"));
        let claude = models
            .iter()
            .find(|m| m.model_id == "claude-sonnet-4-5")
            .unwrap();
        assert_eq!(claude.provider_id, "anthropic");
    }

    // --- resolve_field ---

    #[test]
    fn resolve_field_explicit_wins() {
        let result = resolve_field(
            &Some("https://custom.api".into()),
            Some(&Value::String("https://old.api".into())),
            Some("https://preset.api"),
            "base_url",
            false,
        );
        assert_eq!(result.unwrap(), "https://custom.api");
    }

    #[test]
    fn resolve_field_preset_fallback() {
        let result = resolve_field(&None, None, Some("https://preset.api"), "base_url", false);
        assert_eq!(result.unwrap(), "https://preset.api");
    }

    #[test]
    fn resolve_field_custom_missing_errors() {
        let result = resolve_field(&None, None, None, "base_url", true);
        assert!(result.is_err());
    }

    // --- validate helpers ---

    #[test]
    fn validate_backend_and_scheme() {
        assert!(validate_api_backend("chat_completions").is_ok());
        assert!(validate_api_backend("graphql").is_err());
        assert!(validate_auth_scheme("bearer").is_ok());
        assert!(validate_auth_scheme("basic").is_err());
    }

    #[test]
    fn provider_urls_require_https_or_explicit_loopback_development_mode() {
        assert!(
            validate_provider_base_url_with_policy("https://api.example.com/v1", false).is_ok()
        );
        assert!(validate_provider_base_url_with_policy("http://api.example.com/v1", true).is_err());
        assert!(
            validate_provider_base_url_with_policy("http://example.com/:60100/v1", false)
                .unwrap_err()
                .contains("端口号写在路径里")
        );
        assert!(
            validate_provider_base_url_with_policy("http://127.0.0.1:11434/v1", false).is_err()
        );
        assert!(validate_provider_base_url_with_policy("http://127.0.0.1:11434/v1", true).is_ok());
        assert!(validate_provider_base_url_with_policy("http://localhost:11434/v1", true).is_ok());
        assert!(validate_provider_base_url_with_policy("http://[::1]:11434/v1", true).is_ok());
        assert!(validate_provider_base_url_with_policy("http://127.0.0.2:11434/v1", true).is_err());
        assert!(
            validate_provider_base_url_with_policy("https://user:pass@api.example.com", false)
                .is_err()
        );
    }

    #[test]
    fn preset_known_kinds() {
        assert!(preset("anthropic").is_some());
        assert!(preset("custom").is_none());
    }

    #[test]
    fn openai_preset_is_a_responses_model_provider() {
        let provider = preset("openai").expect("OpenAI has a built-in preset");
        assert_eq!(provider.base_url, Some("https://api.openai.com/v1"));
        assert_eq!(provider.api_backend, "responses");
        assert_eq!(provider.auth_scheme, "bearer");
    }

    #[test]
    fn preset_custom_anthropic_has_protocol_but_no_base_url() {
        // custom_anthropic locks protocol/auth to the Anthropic wire shape but
        // has no base_url preset (user must supply the endpoint).
        let p = preset("custom_anthropic").expect("custom_anthropic has a preset");
        assert_eq!(p.base_url, None);
        assert_eq!(p.api_backend, "messages");
        assert_eq!(p.auth_scheme, "x_api_key");
    }

    #[test]
    fn provider_to_table_custom_anthropic_requires_base_url() {
        // No base_url supplied and none in the preset → hard error.
        let p = ModelProviderEntry {
            id: "custom_anthropic".into(),
            provider_kind: "custom_anthropic".into(),
            label: None,
            api_key: Some("sk-ant-test".into()),
            base_url: None,
            api_backend: None,
            auth_scheme: None,
            context_window: None,
            ..Default::default()
        };
        assert!(provider_to_table(&p, None).is_err());
    }

    #[test]
    fn provider_to_table_custom_anthropic_with_base_url_uses_anthropic_protocol() {
        let p = ModelProviderEntry {
            id: "custom_anthropic".into(),
            provider_kind: "custom_anthropic".into(),
            label: None,
            api_key: Some("sk-ant-test".into()),
            base_url: Some("https://my-anthropic-proxy/v1".into()),
            api_backend: None, // should fall back to preset "messages"
            auth_scheme: None, // should fall back to preset "x_api_key"
            context_window: None,
            ..Default::default()
        };
        let table = provider_to_table(&p, None).unwrap();
        let t = table.as_table().unwrap();
        assert_eq!(
            t["base_url"].as_str().unwrap(),
            "https://my-anthropic-proxy/v1"
        );
        assert_eq!(t["api_backend"].as_str().unwrap(), "messages");
        assert_eq!(t["auth_scheme"].as_str().unwrap(), "x_api_key");
        assert_eq!(t["api_key"].as_str().unwrap(), "sk-ant-test");
    }

    #[test]
    fn inference_endpoint_uses_protocol_path_without_duplication() {
        assert_eq!(
            inference_endpoint("https://api.example.com/v1", "chat_completions").unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            inference_endpoint(
                "https://api.example.com/v1/chat/completions/",
                "chat_completions"
            )
            .unwrap(),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            inference_endpoint("https://api.example.com/v1", "responses").unwrap(),
            "https://api.example.com/v1/responses"
        );
        assert_eq!(
            inference_endpoint("https://api.example.com/v1", "messages").unwrap(),
            "https://api.example.com/v1/messages"
        );
        assert!(inference_endpoint("https://api.example.com/v1", "graphql").is_err());
    }
}
