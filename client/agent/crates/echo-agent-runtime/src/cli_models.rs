//! Data APIs for `echoagent models`. Clients own display.

use agent_client_protocol as acp;
use anyhow::Result;
use echo_agent_acp::{AcpAgentTx, acp_send};

use crate::agent::config::Config as AgentConfig;

/// Status for the `echoagent models` banner (display order ≠ sampling priority; see [`AuthStatus::resolve`]).
#[derive(Debug, PartialEq, Eq)]
pub enum AuthStatus {
    ApiKey,
    /// Auth host from `echo_agent_ws_origin` (scheme stripped).
    LoggedIn(String),
    /// Catalog key of the first model with own `api_key`/`env_key`.
    ModelCredentials(String),
    DeploymentKey,
    NotAuthenticated,
}

impl AuthStatus {
    /// Banner status: env key → session → BYOK → deployment → none.
    ///
    /// Differs from sampling (`resolve_credentials`: BYOK → session → env) so a
    /// logged-in user sees the login host. BYOK uses
    /// [`crate::agent::auth_method::should_advertise_echo_agent_api_key`] so
    /// `disable_api_key_auth` is honored.
    pub fn resolve(agent_config: &AgentConfig) -> Self {
        if crate::agent::auth_method::has_echo_agent_api_key_env() {
            return Self::ApiKey;
        }
        if agent_config.create_auth_manager().current().is_some() {
            let origin = &agent_config.echo_agent_com_config.echo_agent_ws_origin;
            let host = origin
                .strip_prefix("https://")
                .or_else(|| origin.strip_prefix("http://"))
                .unwrap_or(origin);
            return Self::LoggedIn(host.to_owned());
        }
        let models = crate::agent::config::resolve_model_list(agent_config, None);
        if crate::agent::auth_method::should_advertise_echo_agent_api_key(
            agent_config.echo_agent_com_config.api_key_auth_disabled(),
            models.values(),
        ) && let Some(name) = models
            .iter()
            .find_map(|(name, entry)| entry.has_own_credentials().then(|| name.clone()))
        {
            return Self::ModelCredentials(name);
        }
        if agent_config.endpoints.deployment_key.is_some() {
            return Self::DeploymentKey;
        }
        Self::NotAuthenticated
    }
}

/// Fetch model state (available models + default) over an ACP channel.
pub async fn list_models(
    acp_tx: &AcpAgentTx,
    client_type: &str,
    client_version: &str,
) -> Result<acp::SessionModelState> {
    let _init: acp::InitializeResponse = acp_send(
        acp::InitializeRequest::new(acp::ProtocolVersion::V1)
            .client_capabilities(
                acp::ClientCapabilities::new()
                    .fs(acp::FileSystemCapabilities::new())
                    .terminal(false),
            )
            .meta(
                serde_json::json!({
                    "clientType": client_type,
                    "clientVersion": client_version,
                })
                .as_object()
                .cloned(),
            ),
        acp_tx,
    )
    .await?;

    fetch_model_state(acp_tx).await
}

/// Fetch model state via `echo.agent/models/list` over an initialized channel.
pub async fn fetch_model_state(acp_tx: &AcpAgentTx) -> Result<acp::SessionModelState> {
    let params = serde_json::value::to_raw_value(&serde_json::json!({}))?;
    let resp: acp::ExtResponse = acp_send(
        acp::ExtRequest::new("echo.agent/models/list", params.into()),
        acp_tx,
    )
    .await?;
    parse_models_list_response(resp.0.get())
}

/// Parse an `echo.agent/models/list` payload; a handler error wins over a
/// missing result.
fn parse_models_list_response(raw: &str) -> Result<acp::SessionModelState> {
    let parsed: crate::session::ExtMethodResult<acp::SessionModelState> =
        serde_json::from_str(raw)?;
    if let Some(err) = parsed.error {
        anyhow::bail!("models/list failed: {err}");
    }
    parsed
        .result
        .ok_or_else(|| anyhow::anyhow!("models/list response missing result"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::auth_method::{
        ECHO_AGENT_API_KEY_ENV_VAR, LEGACY_ECHO_AGENT_API_KEY_ENV_VAR,
    };
    use crate::agent::config::Config;
    use crate::auth::{AuthMode, EchoAgentAuth};
    use echo_agent_test_support::EnvGuard;
    use serial_test::serial;

    /// Isolate process-global auth sources that `AuthStatus::resolve` consults.
    ///
    /// Uses `ECHO_AGENT_AUTH_PATH` (not `ECHO_AGENT_HOME`) so a OnceLock-cached real home
    /// with `auth.json` cannot leak into these tests.
    fn isolate_auth_sources() -> (tempfile::TempDir, [EnvGuard; 7]) {
        let dir = tempfile::tempdir().unwrap();
        let auth_path = dir.path().join("no-auth.json");
        let guards = [
            EnvGuard::unset(ECHO_AGENT_API_KEY_ENV_VAR),
            EnvGuard::unset(LEGACY_ECHO_AGENT_API_KEY_ENV_VAR),
            EnvGuard::unset("ECHO_AGENT_AUTH"),
            EnvGuard::set("ECHO_AGENT_AUTH_PATH", auth_path.to_str().unwrap()),
            EnvGuard::unset("ECHO_AGENT_DEPLOYMENT_KEY"),
            EnvGuard::unset("ECHO_AGENT_WS_ORIGIN"),
            EnvGuard::unset("ECHO_AGENT_DISABLE_API_KEY_AUTH"),
        ];
        (dir, guards)
    }

    fn byok_and_deployment_toml(model_id: &str) -> String {
        format!(
            r#"
            [endpoints]
            deployment_key = "deploy-key"

            [model."{model_id}"]
            model = "{model_id}"
            api_key = "sk-byok"
            "#
        )
    }

    fn config_from_toml(toml_src: &str) -> Config {
        let toml: toml::Value = toml::from_str(toml_src).unwrap();
        Config::new_from_toml_cfg(&toml).expect("config should parse")
    }

    #[test]
    #[serial]
    fn resolve_api_key_env() {
        let (_dir, _g) = isolate_auth_sources();
        let _key = EnvGuard::set(ECHO_AGENT_API_KEY_ENV_VAR, "echo-agent-test-key");
        assert_eq!(AuthStatus::resolve(&Config::default()), AuthStatus::ApiKey);
    }

    #[test]
    #[serial]
    fn resolve_legacy_api_key_env() {
        let (_dir, _g) = isolate_auth_sources();
        let _key = EnvGuard::set(LEGACY_ECHO_AGENT_API_KEY_ENV_VAR, "legacy-key");
        assert_eq!(AuthStatus::resolve(&Config::default()), AuthStatus::ApiKey);
    }

    #[test]
    #[serial]
    fn resolve_oauth_session() {
        let (_dir, _g) = isolate_auth_sources();
        let token = EchoAgentAuth {
            key: "session-token".into(),
            auth_mode: AuthMode::WebLogin,
            ..EchoAgentAuth::test_default()
        };
        let json = serde_json::to_string(&token).unwrap();
        let _auth = EnvGuard::set("ECHO_AGENT_AUTH", &json);

        assert_eq!(
            AuthStatus::resolve(&Config::default()),
            AuthStatus::LoggedIn("cloud.echo-agent.invalid".to_owned())
        );
    }

    #[test]
    #[serial]
    fn resolve_model_api_key_byok() {
        let (_dir, _g) = isolate_auth_sources();
        let dm = crate::models::default_model();
        let cfg = config_from_toml(&format!(
            r#"
            [model."{dm}"]
            model = "{dm}"
            api_key = "sk-byok-inline"
            "#
        ));
        assert_eq!(
            AuthStatus::resolve(&cfg),
            AuthStatus::ModelCredentials(dm.to_owned())
        );
    }

    #[test]
    #[serial]
    fn resolve_model_env_key_byok() {
        let (_dir, _g) = isolate_auth_sources();
        const TEST_ENV: &str = "TEST_AUTH_STATUS_BYOK_ENV_KEY";
        let dm = crate::models::default_model();
        let cfg = config_from_toml(&format!(
            r#"
            [model."{dm}"]
            model = "{dm}"
            env_key = "{TEST_ENV}"
            "#
        ));

        {
            let _unset = EnvGuard::unset(TEST_ENV);
            assert_eq!(AuthStatus::resolve(&cfg), AuthStatus::NotAuthenticated);
        }
        {
            let _set = EnvGuard::set(TEST_ENV, "secret-token");
            assert_eq!(
                AuthStatus::resolve(&cfg),
                AuthStatus::ModelCredentials(dm.to_owned())
            );
        }
    }

    #[test]
    #[serial]
    fn resolve_deployment_key() {
        let (_dir, _g) = isolate_auth_sources();
        let mut cfg = Config::default();
        cfg.endpoints.deployment_key = Some("deploy-key".into());
        assert_eq!(AuthStatus::resolve(&cfg), AuthStatus::DeploymentKey);
    }

    #[test]
    fn models_list_response_round_trips() {
        let state = acp::SessionModelState::new(
            acp::ModelId::new("echo-agent-4"),
            vec![acp::ModelInfo::new(
                acp::ModelId::new("echo-agent-4"),
                "EchoAgent 4",
            )],
        );
        let ok = crate::session::ExtMethodResult::success(state)
            .to_ext_response()
            .unwrap();
        let parsed = parse_models_list_response(ok.0.get()).unwrap();
        assert_eq!(parsed.current_model_id.0.as_ref(), "echo-agent-4");
        assert_eq!(parsed.available_models.len(), 1);

        let err = crate::session::ExtMethodResult::<acp::SessionModelState>::failure("boom")
            .to_ext_response()
            .unwrap();
        let msg = parse_models_list_response(err.0.get())
            .unwrap_err()
            .to_string();
        assert!(msg.contains("boom"), "{msg}");
    }

    #[test]
    #[serial]
    fn resolve_not_authenticated() {
        let (_dir, _g) = isolate_auth_sources();
        assert_eq!(
            AuthStatus::resolve(&Config::default()),
            AuthStatus::NotAuthenticated
        );
    }

    #[test]
    #[serial]
    fn resolve_priority_api_key_over_byok_and_deployment() {
        let (_dir, _g) = isolate_auth_sources();
        let _key = EnvGuard::set(ECHO_AGENT_API_KEY_ENV_VAR, "echo-agent-test-key");
        let dm = crate::models::default_model();
        let cfg = config_from_toml(&byok_and_deployment_toml(dm));
        assert_eq!(AuthStatus::resolve(&cfg), AuthStatus::ApiKey);
    }

    #[test]
    #[serial]
    fn resolve_priority_session_over_byok_and_deployment() {
        let (_dir, _g) = isolate_auth_sources();
        let token = EchoAgentAuth {
            key: "session-token".into(),
            auth_mode: AuthMode::WebLogin,
            ..EchoAgentAuth::test_default()
        };
        let json = serde_json::to_string(&token).unwrap();
        let _auth = EnvGuard::set("ECHO_AGENT_AUTH", &json);

        let dm = crate::models::default_model();
        let cfg = config_from_toml(&byok_and_deployment_toml(dm));
        assert_eq!(
            AuthStatus::resolve(&cfg),
            AuthStatus::LoggedIn("cloud.echo-agent.invalid".to_owned())
        );
    }

    #[test]
    #[serial]
    fn resolve_priority_byok_over_deployment() {
        let (_dir, _g) = isolate_auth_sources();
        let dm = crate::models::default_model();
        let cfg = config_from_toml(&byok_and_deployment_toml(dm));
        assert_eq!(
            AuthStatus::resolve(&cfg),
            AuthStatus::ModelCredentials(dm.to_owned())
        );
    }

    #[test]
    #[serial]
    fn resolve_disable_api_key_auth_suppresses_byok_banner() {
        let (_dir, _g) = isolate_auth_sources();
        let dm = crate::models::default_model();
        let cfg = config_from_toml(&format!(
            r#"
            [echo_agent_com_config]
            disable_api_key_auth = true

            [model."{dm}"]
            model = "{dm}"
            api_key = "sk-byok"
            "#
        ));
        assert_eq!(AuthStatus::resolve(&cfg), AuthStatus::NotAuthenticated);
    }

    #[test]
    #[serial]
    fn resolve_disable_api_key_auth_falls_through_to_deployment() {
        let (_dir, _g) = isolate_auth_sources();
        let dm = crate::models::default_model();
        let cfg = config_from_toml(&format!(
            r#"
            [echo_agent_com_config]
            disable_api_key_auth = true

            [endpoints]
            deployment_key = "deploy-key"

            [model."{dm}"]
            model = "{dm}"
            api_key = "sk-byok"
            "#
        ));
        assert_eq!(AuthStatus::resolve(&cfg), AuthStatus::DeploymentKey);
    }

    #[test]
    #[serial]
    fn resolve_model_credentials_uses_first_catalog_key() {
        let (_dir, _g) = isolate_auth_sources();
        let cfg = config_from_toml(
            r#"
            [model."my-openai"]
            model = "gpt-4o"
            api_key = "sk-first"

            [model."my-anthropic"]
            model = "claude"
            api_key = "sk-second"
            "#,
        );
        assert_eq!(
            AuthStatus::resolve(&cfg),
            AuthStatus::ModelCredentials("my-openai".to_owned())
        );
    }
}
