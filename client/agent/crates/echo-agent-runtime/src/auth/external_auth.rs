use crate::auth::AuthMode;
use crate::auth::EchoAgentAuth;
use crate::auth::token_output::parse_token_output;
use crate::util::subprocess::CommandLog;
use crate::util::subprocess::RunError;
use crate::util::subprocess::RunOptions;
use crate::util::subprocess::run_detached_with_timeout;
use crate::util::subprocess::shell_c;
use std::time::Duration;

/// Parse stdout into a session-credential `EchoAgentAuth`.
pub(crate) fn parse_output(output: &std::process::Output) -> anyhow::Result<EchoAgentAuth> {
    let parsed = parse_token_output(output)?;
    Ok(EchoAgentAuth {
        key: parsed.access_token,
        auth_mode: AuthMode::External,
        create_time: chrono::Utc::now(),
        user_id: String::new(),
        email: None,
        first_name: None,
        last_name: None,
        profile_image_asset_id: None,
        principal_type: None,
        principal_id: None,
        team_id: None,
        team_name: None,
        team_role: None,
        organization_id: None,
        organization_name: None,
        organization_role: None,
        user_blocked_reason: None,
        team_blocked_reasons: vec![],
        coding_data_retention_opt_out: crate::auth::default_coding_data_retention_opt_out(),
        has_echo_agent_code_access: None,
        refresh_token: parsed.refresh_token,
        expires_at: parsed.expires_at,
        oidc_issuer: parsed.issuer,
        oidc_client_id: None,
    })
}

/// Short timeout for a mid-session refresh: it must not hang the session.
/// The single run in `ExternalBinaryRefresher` gets this whole budget.
const EXTERNAL_AUTH_REFRESH_TIMEOUT: Duration = Duration::from_secs(7);

/// Runs the external auth binary for a headless mid-session refresh. Initial,
/// interactive sign-in takes a separate path (`flow::run_external_auth_provider`,
/// which bridges the provider's stderr link), so this handles refresh only.
pub(crate) async fn run_external_refresh(command: &str) -> Option<EchoAgentAuth> {
    tracing::info!(cmd = %command, timeout_secs = EXTERNAL_AUTH_REFRESH_TIMEOUT.as_secs(), "auth: running external auth provider (headless refresh)");

    let mut cmd = shell_c(command);
    cmd.env("ECHO_AGENT_AUTH_EXPIRED", "1");
    // Route through the group-killing runner so a provider that spawns helpers
    // is torn down as a unit on timeout.
    let output = match run_detached_with_timeout(
        cmd,
        EXTERNAL_AUTH_REFRESH_TIMEOUT,
        RunOptions {
            label: "external auth provider",
            command_log: CommandLog::Shown(command),
        },
    )
    .await
    {
        Ok(output) => output,
        Err(e) => {
            let reason = match e {
                RunError::TimedOut => {
                    "timed out (a timeout usually means it needs interactive sign-in)"
                }
                RunError::SpawnFailed => "failed to start",
                RunError::WaitFailed => "errored while running",
            };
            tracing::warn!(cmd = %command, "auth: external auth provider {reason}");
            return None;
        }
    };

    match parse_output(&output) {
        Ok(auth) => {
            tracing::info!("auth: external auth provider returned fresh token");
            Some(auth)
        }
        Err(e) => {
            tracing::warn!(error = %e, "auth: external auth provider failed");
            None
        }
    }
}

/// Run external auth provider, carrying forward `/user`-derived fields from previous auth.
pub(crate) async fn refresh_with_command(
    command: &str,
    prev_auth: &EchoAgentAuth,
) -> Option<EchoAgentAuth> {
    let mut auth = run_external_refresh(command).await?;
    auth.carry_user_profile_from(prev_auth);
    Some(auth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_output_nonzero_exit_is_err() {
        let output = std::process::Output {
            status: std::process::Command::new("false").status().unwrap(),
            stdout: b"token".to_vec(),
            stderr: vec![],
        };
        assert!(parse_output(&output).is_err());
    }

    #[test]
    fn parse_output_empty_stdout_is_err() {
        let output = std::process::Output {
            status: std::process::Command::new("true").status().unwrap(),
            stdout: b"  \n".to_vec(),
            stderr: vec![],
        };
        assert!(parse_output(&output).is_err());
    }

    #[test]
    fn parse_output_issuer_claim_enables_echo_agent_auth() {
        let ok = |stdout: &str| std::process::Output {
            status: std::process::Command::new("true").status().unwrap(),
            stdout: stdout.as_bytes().to_vec(),
            stderr: vec![],
        };

        // echo.agent issuer claim → first-party session (relay-eligible).
        let auth = parse_output(&ok(
            r#"{"access_token":"t","expires_in":900,"issuer":"https://auth.echo-agent.invalid"}"#,
        ))
        .unwrap();
        assert_eq!(
            auth.oidc_issuer.as_deref(),
            Some("https://auth.echo-agent.invalid")
        );
        assert!(auth.is_echo_agent_auth());

        // Non-echo.agent issuer is stored but stays third-party.
        let auth = parse_output(&ok(
            r#"{"access_token":"t","issuer":"https://idp.acme.example"}"#,
        ))
        .unwrap();
        assert_eq!(
            auth.oidc_issuer.as_deref(),
            Some("https://idp.acme.example")
        );
        assert!(!auth.is_echo_agent_auth());

        // Missing / empty / whitespace issuer → None.
        let auth = parse_output(&ok(r#"{"access_token":"t"}"#)).unwrap();
        assert_eq!(auth.oidc_issuer, None);
        assert!(!auth.is_echo_agent_auth());
        let auth = parse_output(&ok(r#"{"access_token":"t","issuer":"  "}"#)).unwrap();
        assert_eq!(auth.oidc_issuer, None);

        // Bare-token output never carries an issuer.
        let auth = parse_output(&ok("bare-token")).unwrap();
        assert_eq!(auth.oidc_issuer, None);
        assert!(!auth.is_echo_agent_auth());
    }

    #[test]
    fn parse_output_json_shaped_but_invalid_is_err() {
        let output = std::process::Output {
            status: std::process::Command::new("true").status().unwrap(),
            stdout: b"{not valid json}".to_vec(),
            stderr: vec![],
        };
        assert!(parse_output(&output).is_err());
    }

    #[tokio::test]
    async fn spawn_failure_returns_none() {
        assert!(run_external_refresh("/nonexistent/binary").await.is_none());
    }

    #[tokio::test]
    async fn sets_echo_agent_auth_expired_env_on_refresh() {
        let auth = run_external_refresh("echo $ECHO_AGENT_AUTH_EXPIRED")
            .await
            .unwrap();
        assert_eq!(auth.key, "1");
    }

    #[tokio::test]
    async fn refresh_carries_zdr_flags_forward() {
        let prev = EchoAgentAuth {
            user_blocked_reason: Some("BLOCKED_REASON_OTHER".into()),
            team_blocked_reasons: vec!["BLOCKED_REASON_NO_LOGS".into()],
            coding_data_retention_opt_out: true,
            organization_id: Some("org-1".into()),
            ..EchoAgentAuth::test_default()
        };
        let auth = refresh_with_command("echo fresh-token", &prev)
            .await
            .unwrap();
        assert_eq!(auth.key, "fresh-token");
        assert!(auth.is_zdr_team(), "ZDR flag must survive refresh");
        assert!(auth.coding_data_retention_opt_out);
        assert_eq!(
            auth.user_blocked_reason.as_deref(),
            Some("BLOCKED_REASON_OTHER")
        );
        assert_eq!(auth.user_id, "test-user", "profile must survive refresh");
        assert_eq!(auth.organization_id.as_deref(), Some("org-1"));
    }

    #[tokio::test]
    async fn refresh_interactive_times_out() {
        // Binary writes a link to stderr then blocks; the 5s refresh timeout kills it.
        let cmd = r#"echo 'Visit http://example.com/auth' >&2; sleep 20; echo token"#;
        let start = std::time::Instant::now();
        let result = run_external_refresh(cmd).await;
        let elapsed = start.elapsed();
        assert!(result.is_none(), "should timeout and return None");
        assert!(
            elapsed.as_secs() < 10,
            "refresh should use 5s timeout, not 60s (took {}s)",
            elapsed.as_secs()
        );
    }
}
