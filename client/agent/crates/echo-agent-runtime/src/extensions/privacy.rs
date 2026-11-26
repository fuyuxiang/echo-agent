//! `echo.agent/privacy/setCodingDataRetention` extension handler.
//!
//! PUTs the new opt-out flag to cli-chat-proxy and updates local auth state
//! to match. The local update is fire-and-forget (best-effort cache refresh).

use agent_client_protocol as acp;
use serde::Deserialize;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/privacy/setCodingDataRetention" => handle_set(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_set(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Params {
        coding_data_retention_opt_out: bool,
    }

    let params: Params = parse_params(args)?;

    let auth = agent.auth_manager.auth().await.map_err(|e| {
        tracing::warn!(error = %e, "privacy: auth resolution failed");
        acp::Error::auth_required()
            .data("Authentication required. Run `echoagent login` to re-authenticate.")
    })?;

    let proxy_url = agent.cfg.borrow().endpoints.proxy_url();
    let url = format!("{proxy_url}/privacy/coding-data-retention");
    let token_header = agent
        .auth_manager
        .echo_agent_com_config()
        .token_header
        .clone();

    let body = serde_json::json!({
        "codingDataRetentionOptOut": params.coding_data_retention_opt_out,
    });

    let provider: std::sync::Arc<dyn echo_agent_auth::AuthCredentialProvider> = std::sync::Arc::new(
        crate::auth::credential_provider::ShellAuthCredentialProvider::new(
            agent.auth_manager.clone(),
            None,
            None,
        ),
    );
    let client = crate::http::with_auth_retry(crate::http::shared_client(), provider);

    let resp = client
        .put(&url)
        .header("X-Echo-Agent-Token-Auth", &token_header)
        .header("x-echo-agent-client-version", echo_agent_version::VERSION)
        .header(
            crate::http::CLIENT_MODE_HEADER,
            crate::http::process_client_mode(),
        )
        .json(&body)
        .send()
        .await
        .map_err(|e| acp::Error::internal_error().data(format!("HTTP request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(status, "setCodingDataRetention request failed");
        let friendly = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .or_else(|| v.get("message"))
                    .and_then(|e| e.as_str().map(String::from))
            })
            .unwrap_or_else(|| format!("server returned HTTP {status}"));
        return Err(acp::Error::internal_error().data(friendly));
    }

    // Update local auth state to reflect the change.
    // Use save_without_enrichment to avoid a race: update() spawns a
    // background GET /user enrichment that may read stale ACL state
    // and overwrite the opt-out flag back to its previous value.
    let mut updated = auth.clone();
    updated.coding_data_retention_opt_out = params.coding_data_retention_opt_out;
    let _ = agent.auth_manager.save_without_enrichment(updated).await;

    to_raw_response(&serde_json::json!({
        "codingDataRetentionOptOut": params.coding_data_retention_opt_out,
    }))
}
