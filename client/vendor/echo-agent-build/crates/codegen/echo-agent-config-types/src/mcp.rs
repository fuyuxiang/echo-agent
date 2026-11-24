//! MCP server configuration value types, extracted from echo-agent-runtime
//! (config dependency inversion).

use agent_client_protocol as acp;
use echo_agent_mcp::oauth_config::McpOAuthConfig;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// serde default helper. Kept module-local rather than shared — the `pool`
/// module keeps its own copy for `PoolConfig`.
fn default_true() -> bool {
    true
}

/// Read an MCP OAuth client secret from the named env var. Moved here with
/// `McpServerConfig` (its only caller).
fn resolve_oauth_client_secret(env_var: Option<&String>) -> Option<String> {
    let env_var = env_var?;
    match std::env::var(env_var) {
        Ok(secret) => Some(secret),
        Err(_) => {
            tracing::warn!(
                env_var = env_var.as_str(),
                "MCP OAuth client_secret env var is configured but not set in the environment; \
                 proceeding without a client secret"
            );
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum McpServerTransportConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        env: Option<HashMap<String, String>>,
        /// Standard MCP JSON supports `cwd`, but ACP stdio server config does not yet expose it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    StreamableHttp {
        // Not `default`: a missing url must fail to deserialize, not become a
        // fake HTTP server with an empty url.
        #[serde(alias = "urlTemplate", alias = "url_template")]
        url: String,
        #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
        transport_type: Option<String>,
        /// Name of the environment variable to read and set for `Authorization: Bearer <token>`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bearer_token_env_var: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        headers: Option<HashMap<String, String>>,
        /// OAuth client ID for providers that don't support Dynamic Client Registration.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth_client_id: Option<String>,
        /// Name of the env var holding the OAuth client secret (for BYO credentials).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth_client_secret_env_var: Option<String>,
        /// OAuth scopes to request during authorization.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        oauth_scopes: Option<Vec<String>>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpServerProblemSeverity {
    Error,
    Warning,
}

/// A problem found loading an `[mcp_servers.*]` entry. Reported (never fatal)
/// and surfaced through `echoagent inspect`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfigProblem {
    pub server: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub severity: McpServerProblemSeverity,
    pub message: String,
}

/// Recognized wire keys for an `[mcp_servers.*]` entry. Needed because the
/// flattened untagged transport enum bypasses `serde_ignored`. Kept in sync by
/// `known_mcp_server_fields_cover_serialized_keys`.
pub const KNOWN_MCP_SERVER_FIELDS: &[&str] = &[
    "desktop_instance_id",
    "args",
    "bearer_token_env_var",
    "command",
    "cwd",
    "enabled",
    "env",
    "expose_image_base64",
    "headers",
    "oauth",
    "oauth_client_id",
    "oauth_client_secret_env_var",
    "oauth_scopes",
    "setup",
    "startup_timeout_sec",
    "tool_timeout_sec",
    "tool_timeouts",
    "type",
    "url",
    // Deserialize-only aliases for `url`.
    "urlTemplate",
    "url_template",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpJsonOAuthBlock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret_env_var: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpSetupConfig {
    #[serde(default)]
    pub fields: Vec<McpSetupField>,
    #[serde(default, alias = "values")]
    pub variables: HashMap<String, McpSetupDerivedValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpSetupField {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub field_type: McpSetupFieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default)]
    pub options: Vec<McpSetupOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum McpSetupFieldType {
    Select,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpSetupOption {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpSetupDerivedValue {
    pub from: String,
    pub map: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpPreferenceSource {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct McpServerPreferences {
    #[serde(default)]
    pub values: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<McpPreferenceSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpPreferencesFile {
    pub version: u32,
    #[serde(default)]
    pub servers: HashMap<String, McpServerPreferences>,
}

impl Default for McpPreferencesFile {
    fn default() -> Self {
        Self {
            version: 1,
            servers: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum McpSetupResolution {
    Resolved(Box<McpServerConfig>),
    Required(McpSetupConfig),
    Invalid(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Identity of a desktop-managed user definition. Ephemeral and project
    /// definitions normally have no desktop identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_instance_id: Option<String>,
    #[serde(flatten)]
    pub transport: McpServerTransportConfig,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpJsonOAuthBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<McpSetupConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_timeout_sec: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_timeout_sec: Option<u64>,
    /// Per-tool timeout overrides in seconds: `{ "create_issue" = 120, "search" = 30 }`.
    /// Falls back to `tool_timeout_sec` for tools not listed here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_timeouts: Option<HashMap<String, u64>>,
    /// Also keep the raw base64 in tool-result text so agents can forward
    /// bytes via path-based tools (`base64 -d > /tmp/x.png && send_file ...`).
    /// ~2× tokens per image. Overridden by `_meta.mcpConfig.<server>.exposeImageBase64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expose_image_base64: Option<bool>,
}

impl McpServerConfig {
    /// The transport field (`command` or `url`) that is present but blank, if
    /// any. Such a server can never connect, so the loader drops it.
    pub fn blank_transport_field(&self) -> Option<&'static str> {
        match &self.transport {
            McpServerTransportConfig::Stdio { command, .. } if command.trim().is_empty() => {
                Some("command")
            }
            McpServerTransportConfig::StreamableHttp { url, .. } if url.trim().is_empty() => {
                Some("url")
            }
            _ => None,
        }
    }
}

fn render_setup_template(
    input: &str,
    variables: &HashMap<String, String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        let (prefix, after_start) = rest.split_at(start);
        out.push_str(prefix);
        let after_start = &after_start[2..];
        let Some(end) = after_start.find("}}") else {
            return Err("unterminated setup variable template".to_string());
        };
        let key = after_start[..end].trim();
        let Some(value) = variables.get(key) else {
            return Err(format!("unresolved setup variable '{key}'"));
        };
        out.push_str(value);
        rest = &after_start[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

fn render_setup_templates(
    config: &mut McpServerConfig,
    variables: &HashMap<String, String>,
) -> Result<(), String> {
    let sub = |s: &str| render_setup_template(s, variables);
    match &mut config.transport {
        McpServerTransportConfig::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            *command = sub(command)?;
            for arg in args.iter_mut() {
                *arg = sub(arg)?;
            }
            if let Some(env) = env.as_mut() {
                for value in env.values_mut() {
                    *value = sub(value)?;
                }
            }
            if let Some(cwd) = cwd.as_mut() {
                *cwd = sub(cwd)?;
            }
        }
        McpServerTransportConfig::StreamableHttp { url, headers, .. } => {
            *url = sub(url)?;
            if let Some(headers) = headers.as_mut() {
                for value in headers.values_mut() {
                    *value = sub(value)?;
                }
            }
        }
    }
    Ok(())
}

impl McpServerConfig {
    /// Resolve `setup` templates using stored preferences.
    ///
    /// v0 supports exactly one select field with options. Multi-field schemas
    /// are Invalid until the TUI can collect them.
    pub fn resolve_setup(&self, preferences: Option<&McpServerPreferences>) -> McpSetupResolution {
        let Some(setup) = self.setup.as_ref() else {
            return McpSetupResolution::Resolved(Box::new(self.clone()));
        };

        if setup.fields.len() != 1 {
            return McpSetupResolution::Invalid(
                "setup schema must declare exactly one select field (v0)".to_string(),
            );
        }
        let field = &setup.fields[0];
        if !matches!(field.field_type, McpSetupFieldType::Select) || field.options.is_empty() {
            return McpSetupResolution::Invalid(
                "setup field must be a non-empty select (v0)".to_string(),
            );
        }

        let Some(preferences) = preferences else {
            return McpSetupResolution::Required(setup.clone());
        };

        let Some(value) = preferences.values.get(&field.id) else {
            return McpSetupResolution::Required(setup.clone());
        };
        if !field.options.iter().any(|option| option.value == *value) {
            return McpSetupResolution::Required(setup.clone());
        }

        let mut variables = HashMap::new();
        for (name, derived) in &setup.variables {
            if derived.from != field.id {
                return McpSetupResolution::Invalid(format!(
                    "setup variable '{name}' references unknown field '{}'",
                    derived.from
                ));
            }
            let Some(mapped) = derived.map.get(value) else {
                return McpSetupResolution::Required(setup.clone());
            };
            variables.insert(name.clone(), mapped.clone());
        }

        let mut resolved = self.clone();
        resolved.setup = None;
        match render_setup_templates(&mut resolved, &variables) {
            Ok(()) => McpSetupResolution::Resolved(Box::new(resolved)),
            Err(e) => McpSetupResolution::Invalid(e),
        }
    }

    pub fn expand_strings(&mut self, sub: &dyn Fn(&str) -> String) {
        match &mut self.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                *command = sub(command);
                for arg in args.iter_mut() {
                    *arg = sub(arg);
                }
                if let Some(env) = env.as_mut() {
                    for value in env.values_mut() {
                        *value = sub(value);
                    }
                }
                if let Some(cwd) = cwd.as_mut() {
                    *cwd = sub(cwd);
                }
            }
            McpServerTransportConfig::StreamableHttp { url, headers, .. } => {
                *url = sub(url);
                if let Some(headers) = headers.as_mut() {
                    for value in headers.values_mut() {
                        *value = sub(value);
                    }
                }
            }
        }
    }

    pub fn to_acp_mcp_server(&self, name: impl Into<String>) -> Option<acp::McpServer> {
        if !self.enabled || self.setup.is_some() {
            return None;
        }
        let name = name.into();
        let mut server = match &self.transport {
            McpServerTransportConfig::Stdio {
                command,
                args,
                env,
                cwd: _,
            } => {
                let env_variables: Vec<acp::EnvVariable> = env
                    .as_ref()
                    .map(|e| {
                        e.iter()
                            .map(|(k, v)| acp::EnvVariable::new(k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();

                Some(acp::McpServer::Stdio(
                    acp::McpServerStdio::new(name, PathBuf::from(command))
                        .args(args.clone())
                        .env(env_variables),
                ))
            }
            McpServerTransportConfig::StreamableHttp {
                url,
                transport_type,
                bearer_token_env_var,
                headers,
                ..
            } => {
                if url.is_empty() {
                    return None;
                }
                let mut http_headers: Vec<acp::HttpHeader> = headers
                    .as_ref()
                    .map(|h| {
                        h.iter()
                            .map(|(k, v)| acp::HttpHeader::new(k.clone(), v.clone()))
                            .collect()
                    })
                    .unwrap_or_default();

                // Add bearer token from environment variable if specified
                if let Some(env_var) = bearer_token_env_var {
                    match std::env::var(env_var) {
                        Ok(token) => {
                            http_headers.push(acp::HttpHeader::new(
                                "Authorization",
                                format!("Bearer {}", token),
                            ));
                        }
                        Err(_) => {
                            tracing::warn!(
                                "MCP server '{}': bearer_token_env_var '{}' not set in environment",
                                name,
                                env_var
                            );
                        }
                    }
                }

                let is_sse = transport_type
                    .as_deref()
                    .is_some_and(|transport| transport.eq_ignore_ascii_case("sse"))
                    || url.ends_with("/sse");

                Some(if is_sse {
                    acp::McpServer::Sse(
                        acp::McpServerSse::new(name, url.clone()).headers(http_headers),
                    )
                } else {
                    acp::McpServer::Http(
                        acp::McpServerHttp::new(name, url.clone()).headers(http_headers),
                    )
                })
            }
        }?;
        if let Some(id) = &self.desktop_instance_id {
            echo_agent_mcp::servers::set_user_mcp_instance_id(&mut server, id);
        }
        Some(server)
    }

    /// Extract OAuth configuration for this server, if any OAuth fields are set.
    pub fn oauth_config(&self) -> Option<McpOAuthConfig> {
        if let McpServerTransportConfig::StreamableHttp {
            oauth_client_id,
            oauth_client_secret_env_var,
            oauth_scopes,
            ..
        } = &self.transport
            && oauth_client_id.is_some()
        {
            return Some(McpOAuthConfig {
                client_id: oauth_client_id.clone(),
                client_secret: resolve_oauth_client_secret(oauth_client_secret_env_var.as_ref()),
                scopes: oauth_scopes.clone(),
                callback_port: None,
            });
        }

        if let Some(block) = &self.oauth
            && block.client_id.is_some()
        {
            return Some(McpOAuthConfig {
                client_id: block.client_id.clone(),
                client_secret: resolve_oauth_client_secret(block.client_secret_env_var.as_ref()),
                scopes: block.scopes.clone(),
                callback_port: block.callback_port,
            });
        }

        None
    }
}

/// Configuration for relay session sharing.
/// Set in config.toml under [relay] section.
///
/// Example:
/// ```toml
/// [relay]
/// enabled = true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RelaySyncConfig {
    pub enabled: Option<bool>,
}

impl RelaySyncConfig {
    /// Check if relay sync is enabled. Env var takes precedence over config.
    pub fn is_enabled(&self) -> bool {
        if let Ok(env_val) = std::env::var("ECHO_AGENT_RELAY_SYNC_ENABLED") {
            return env_val.eq_ignore_ascii_case("true") || env_val == "1";
        }
        self.enabled.unwrap_or(false)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct McpConfig {
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: IndexMap<String, McpServerConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site_select_setup_json() -> &'static str {
        r#"{
            "mcpServers": {
                "acme": {
                    "type": "http",
                    "urlTemplate": "{{url}}",
                    "setup": {
                        "fields": [{
                            "id": "site",
                            "label": "Site",
                            "type": "select",
                            "required": true,
                            "default": "us1",
                            "options": [
                                {"label": "US1", "value": "us1"},
                                {"label": "US5", "value": "us5"}
                            ]
                        }],
                        "values": {
                            "url": {
                                "from": "site",
                                "map": {
                                    "us1": "https://mcp.example.com/v1/mcp",
                                    "us5": "https://mcp.us5.example.com/v1/mcp"
                                }
                            }
                        }
                    }
                }
            }
        }"#
    }

    #[test]
    fn transport_less_entry_fails_to_deserialize() {
        for value in [
            serde_json::json!({ "enabled": false }),
            serde_json::json!({ "enabled": true }),
            serde_json::json!({}),
        ] {
            assert!(
                serde_json::from_value::<McpServerConfig>(value.clone()).is_err(),
                "transport-less entry must not deserialize: {value}"
            );
        }
    }

    #[test]
    fn blank_transport_field_is_detected_symmetrically() {
        let blank_url: McpServerConfig =
            serde_json::from_value(serde_json::json!({ "url": "  " })).unwrap();
        assert_eq!(blank_url.blank_transport_field(), Some("url"));

        let blank_command: McpServerConfig =
            serde_json::from_value(serde_json::json!({ "command": "\t" })).unwrap();
        assert_eq!(blank_command.blank_transport_field(), Some("command"));

        let ok: McpServerConfig =
            serde_json::from_value(serde_json::json!({ "command": "npx" })).unwrap();
        assert_eq!(ok.blank_transport_field(), None);
    }

    /// A newly added field cannot silently escape `KNOWN_MCP_SERVER_FIELDS`.
    #[test]
    fn known_mcp_server_fields_cover_serialized_keys() {
        let stdio = McpServerConfig {
            desktop_instance_id: None,
            transport: McpServerTransportConfig::Stdio {
                command: "npx".into(),
                args: vec!["-y".into()],
                env: Some(HashMap::from([("A".into(), "b".into())])),
                cwd: Some("/tmp".into()),
            },
            enabled: true,
            oauth: Some(McpJsonOAuthBlock::default()),
            setup: None,
            startup_timeout_sec: Some(10),
            tool_timeout_sec: Some(20),
            tool_timeouts: Some(HashMap::from([("t".into(), 1)])),
            expose_image_base64: Some(true),
        };
        let http = McpServerConfig {
            desktop_instance_id: None,
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://x/mcp".into(),
                transport_type: Some("http".into()),
                bearer_token_env_var: Some("TOK".into()),
                headers: Some(HashMap::from([("H".into(), "v".into())])),
                oauth_client_id: Some("id".into()),
                oauth_client_secret_env_var: Some("SEC".into()),
                oauth_scopes: Some(vec!["s".into()]),
            },
            enabled: true,
            oauth: None,
            setup: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            tool_timeouts: None,
            expose_image_base64: None,
        };
        for config in [stdio, http] {
            let value = serde_json::to_value(&config).unwrap();
            for key in value.as_object().unwrap().keys() {
                assert!(
                    KNOWN_MCP_SERVER_FIELDS.contains(&key.as_str()),
                    "field `{key}` is serialized but missing from KNOWN_MCP_SERVER_FIELDS"
                );
            }
        }
    }

    #[test]
    fn stdio_and_http_still_parse() {
        let stdio: McpServerConfig = serde_json::from_value(serde_json::json!({
            "command": "npx",
            "args": ["-y", "pkg"]
        }))
        .unwrap();
        assert!(stdio.enabled);
        assert!(matches!(
            stdio.transport,
            McpServerTransportConfig::Stdio { .. }
        ));

        let http: McpServerConfig = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp"
        }))
        .unwrap();
        assert!(matches!(
            http.transport,
            McpServerTransportConfig::StreamableHttp { .. }
        ));
        assert!(http.to_acp_mcp_server("x").is_some());
    }

    #[test]
    fn mcp_setup_schema_parses_and_missing_preference_requires_setup() {
        let config: McpConfig = serde_json::from_str(site_select_setup_json()).unwrap();
        let server = config.mcp_servers.get("acme").unwrap();
        let setup = server.setup.as_ref().unwrap();
        assert_eq!(setup.fields[0].id, "site");
        assert_eq!(setup.fields[0].default.as_deref(), Some("us1"));
        assert!(setup.variables.contains_key("url"));
        assert!(matches!(
            server.resolve_setup(None),
            McpSetupResolution::Required(_)
        ));
        assert!(server.to_acp_mcp_server("acme").is_none());
    }

    #[test]
    fn mcp_setup_valid_preference_resolves_mapped_url() {
        let config: McpConfig = serde_json::from_str(site_select_setup_json()).unwrap();
        let server = config.mcp_servers.get("acme").unwrap();
        let prefs = McpServerPreferences {
            values: HashMap::from([("site".to_string(), "us5".to_string())]),
            source: None,
            updated_at: None,
        };
        let resolved = match server.resolve_setup(Some(&prefs)) {
            McpSetupResolution::Resolved(config) => config,
            other => panic!("expected resolved config, got {other:?}"),
        };
        assert!(resolved.setup.is_none());
        assert!(resolved.to_acp_mcp_server("acme").is_some());
        match &resolved.transport {
            McpServerTransportConfig::StreamableHttp { url, .. } => {
                assert_eq!(url, "https://mcp.us5.example.com/v1/mcp");
            }
            _ => panic!("expected http config"),
        }
    }

    #[test]
    fn mcp_setup_invalid_preference_value_requires_setup() {
        let setup = McpSetupConfig {
            fields: vec![McpSetupField {
                id: "site".into(),
                label: "Site".into(),
                field_type: McpSetupFieldType::Select,
                required: true,
                default: Some("us1".into()),
                options: vec![McpSetupOption {
                    label: "US1".into(),
                    value: "us1".into(),
                }],
            }],
            variables: HashMap::new(),
        };
        let config = McpServerConfig {
            desktop_instance_id: None,
            transport: McpServerTransportConfig::StreamableHttp {
                url: "{{url}}".into(),
                transport_type: None,
                bearer_token_env_var: None,
                headers: None,
                oauth_client_id: None,
                oauth_client_secret_env_var: None,
                oauth_scopes: None,
            },
            enabled: true,
            oauth: None,
            setup: Some(setup),
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            tool_timeouts: None,
            expose_image_base64: None,
        };
        let prefs = McpServerPreferences {
            values: HashMap::from([("site".to_string(), "us5".to_string())]),
            source: None,
            updated_at: None,
        };
        assert!(matches!(
            config.resolve_setup(Some(&prefs)),
            McpSetupResolution::Required(_)
        ));
    }

    #[test]
    fn mcp_setup_multi_field_schema_is_invalid() {
        let setup = McpSetupConfig {
            fields: vec![
                McpSetupField {
                    id: "a".into(),
                    label: "A".into(),
                    field_type: McpSetupFieldType::Select,
                    required: true,
                    default: None,
                    options: vec![McpSetupOption {
                        label: "1".into(),
                        value: "1".into(),
                    }],
                },
                McpSetupField {
                    id: "b".into(),
                    label: "B".into(),
                    field_type: McpSetupFieldType::Select,
                    required: true,
                    default: None,
                    options: vec![McpSetupOption {
                        label: "2".into(),
                        value: "2".into(),
                    }],
                },
            ],
            variables: HashMap::new(),
        };
        let config = McpServerConfig {
            desktop_instance_id: None,
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://example.com".into(),
                transport_type: None,
                bearer_token_env_var: None,
                headers: None,
                oauth_client_id: None,
                oauth_client_secret_env_var: None,
                oauth_scopes: None,
            },
            enabled: true,
            oauth: None,
            setup: Some(setup),
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            tool_timeouts: None,
            expose_image_base64: None,
        };
        assert!(matches!(
            config.resolve_setup(None),
            McpSetupResolution::Invalid(_)
        ));
        assert!(config.to_acp_mcp_server("x").is_none());
    }
}
