//! `echo.agent/consent/record` extension handler.
//!
//! POSTs an accepted notice to the configured proxy. The client has already accepted it
//! locally, so a failure here loses the server-side record but does not block the user.

use agent_client_protocol as acp;
use serde::Deserialize;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;

/// `shared_client` sets no request timeout, so a hung proxy would hold the extension forever.
const RECORD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        "echo.agent/consent/record" => handle_record(agent, args).await,
        _ => Err(acp::Error::method_not_found()),
    }
}

async fn handle_record(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Params {
        notice_id: String,
        version: i32,
    }

    let params: Params = parse_params(args)?;

    // Checked before the POST so a logged-out caller is told to log in instead of getting a 401.
    agent.auth_manager.auth().await.map_err(|e| {
        tracing::warn!(error = %e, "consent: auth resolution failed");
        acp::Error::auth_required()
            .data("Authentication required. Run `echoagent login` to re-authenticate.")
    })?;

    let proxy_url = agent.cfg.borrow().endpoints.proxy_url();
    let url = format!("{proxy_url}/consent/accept");
    let token_header = agent
        .auth_manager
        .echo_agent_com_config()
        .token_header
        .clone();

    let provider: std::sync::Arc<dyn echo_agent_auth::AuthCredentialProvider> = std::sync::Arc::new(
        crate::auth::credential_provider::ShellAuthCredentialProvider::new(
            agent.auth_manager.clone(),
            None,
            None,
        ),
    );
    let client = crate::http::with_auth_retry(crate::http::shared_client(), provider);

    let resp = client
        .post(&url)
        .timeout(RECORD_TIMEOUT)
        // The server re-runs the same targeting rules, and those read this header.
        .header(
            "x-echo-agent-client-identifier",
            crate::http::process_client_identifier(),
        )
        .header("X-Echo-Agent-Token-Auth", &token_header)
        .header("x-echo-agent-client-version", echo_agent_version::VERSION)
        .header(
            crate::http::CLIENT_MODE_HEADER,
            crate::http::process_client_mode(),
        )
        .json(&serde_json::json!({
            "noticeId": params.notice_id,
            "version": params.version,
        }))
        .send()
        .await
        .map_err(|e| acp::Error::internal_error().data(format!("HTTP request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        // A 502 answers with an HTML page, so take the server's own message when there is one.
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .or_else(|| v.get("message"))
                    .and_then(|e| e.as_str().map(String::from))
            })
            .unwrap_or_else(|| format!("server returned HTTP {status}"));
        tracing::warn!(status, notice_id = %params.notice_id, %message, "consent record rejected");

        return Err(acp::Error::internal_error().data(message));
    }

    to_raw_response(&serde_json::json!({
        "noticeId": params.notice_id,
        "version": params.version,
    }))
}
