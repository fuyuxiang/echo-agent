//! MCP integration crate.
//!
//! Two responsibilities:
//!
//! 1. **Quarantines `rmcp` 2.1 and `reqwest` 0.13.** `rmcp` 2.1 requires
//!    `reqwest >= 0.13.2`. The rest of the workspace consumes `reqwest` 0.12
//!    and a transitive ecosystem (`opentelemetry-otlp`, `oauth2`,
//!    `echo-agent-mixpanel`, `echo-agent-tools`, ...) also pinned to 0.12. Bumping every
//!    crate to 0.13 to satisfy `rmcp` triggers a cascade — an OpenTelemetry
//!    `HttpClient` adapter and cross-version test breakage when a crate
//!    carries both versions under a renamed `package = "reqwest"` alias.
//!    reqwest 0.13 is now a fully private impl detail of [`servers`]; no
//!    re-export. Consumers reach `rmcp` model types through this namespace
//!    (`echo_agent_mcp::rmcp::*`).
//!
//! 2. **Owns MCP-specific integration code**:
//!    - [`credentials`] -- on-disk `$ECHO_AGENT_HOME/mcp_credentials.json` store and
//!      the rmcp `CredentialStore` adapter.
//!    - [`oauth`] -- browser-based OAuth flow with cross-process + in-process
//!      dedup.
//!    - [`oauth_config`] -- BYO OAuth config types parsed out of `config.toml`.
//!    - [`servers`] -- MCP transport layer (rmcp's `StreamableHttpClientTransport`
//!      and `TokioChildProcess`) plus client lifecycle, tool invocation, error
//!      classification, and managed-MCP refresh.
//!    - [`mcp_http_client`] -- backoff wrapper around the HTTP client handed to
//!      rmcp's streamable-HTTP transport (works around rmcp's zero-backoff SSE
//!      reconnect loop).

pub use rmcp;

pub mod acp_transport;
pub mod credentials;
pub mod elicitation;
pub mod liveness;
pub mod mcp_http_client;
pub mod oauth;
pub mod oauth_config;
pub mod owned_clients;
pub mod servers;
pub mod wire;
