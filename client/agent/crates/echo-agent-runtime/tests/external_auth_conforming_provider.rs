//! The published external-auth contract, end to end.
//!
//! Operator binaries live outside this repo and read `ECHO_AGENT_AUTH_EXPIRED=1` as
//! "headless, don't prompt", declining a run they cannot complete silently. So
//! a binary that declines the boot probe must still be able to sign the user
//! in, and the two runs have to reach it in the order boot produces them.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use echo_agent_runtime::auth::{
    AuthMode, EchoAgentAuth, EchoAgentComConfig, ensure_authenticated, try_ensure_fresh_auth,
};

const STALE_TOKEN: &str = "stale-token-the-provider-will-not-renew";
const SSO_TOKEN: &str = "token-minted-by-the-interactive-flow";

/// A regression here reaches the browser login, which hangs rather than fails.
const LOGIN_BUDGET: Duration = Duration::from_secs(60);

/// Measured at ~0.3s healthy and 47s while the flow contended with its own
/// `auth.json.lock`; loose enough for a loaded CI runner in between.
const NO_SELF_CONTENTION: Duration = Duration::from_secs(20);

/// The skeleton published in `README.md` and `docs/user-guide/02-authentication.md`,
/// which operators copy.
fn write_conforming_provider(home: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;

    let log = invocation_log(home);
    let script = home.join("acme-auth.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             echo \"expired=${{ECHO_AGENT_AUTH_EXPIRED:-unset}}\" >> {log}\n\
             if [ \"$ECHO_AGENT_AUTH_EXPIRED\" = \"1\" ]; then\n\
             \x20   echo 'SSO session lapsed; cannot mint without the user' >&2\n\
             \x20   exit 1\n\
             fi\n\
             echo 'Authenticating via Acme Corp SSO...' >&2\n\
             printf '%s' {SSO_TOKEN}\n",
            log = log.display(),
        ),
    )
    .expect("write provider script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod provider script");
    script.display().to_string()
}

fn invocation_log(home: &Path) -> PathBuf {
    home.join("provider-invocations")
}

fn invocations(home: &Path) -> Vec<String> {
    std::fs::read_to_string(invocation_log(home))
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn seed_expired_credential(home: &Path, scope: &str) {
    let expired = EchoAgentAuth {
        key: STALE_TOKEN.to_owned(),
        auth_mode: AuthMode::External,
        expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        ..EchoAgentAuth::default()
    };
    let store: BTreeMap<String, EchoAgentAuth> =
        [(scope.to_owned(), expired)].into_iter().collect();
    std::fs::write(
        home.join("auth.json"),
        serde_json::to_string(&store).expect("serialize auth store"),
    )
    .expect("write auth.json");
}

fn dead_endpoint() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn a_provider_that_declines_the_headless_run_can_still_sign_the_user_in() {
    let home = tempfile::tempdir().expect("echoagent home");
    let provider = write_conforming_provider(home.path());
    let dead = dead_endpoint();

    // SAFETY: single-threaded test entry, before any thread that reads the
    // environment is spawned. `echo_agent_home()` memoizes, so this must stay the
    // only test in the binary.
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", home.path());
        std::env::set_var("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL", &dead);
        std::env::set_var("ECHO_AGENT_API_BASE_URL", &dead);
        std::env::remove_var("ECHO_AGENT_API_KEY");
        std::env::remove_var("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
        std::env::set_var("ECHO_AGENT_TELEMETRY_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_FEEDBACK_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_TRACE_UPLOAD", "false");
    }

    let config = EchoAgentComConfig {
        auth_provider_command: Some(provider),
        ..EchoAgentComConfig::default()
    };
    seed_expired_credential(home.path(), &config.auth_scope());

    assert!(
        try_ensure_fresh_auth(&config).await.is_none(),
        "the provider declines a run it cannot complete silently"
    );
    assert_eq!(
        invocations(home.path()),
        ["expired=1"],
        "the headless refresh is the run the flag exists for"
    );

    let started = Instant::now();
    let auth = tokio::time::timeout(LOGIN_BUDGET, ensure_authenticated(&config, false, None))
        .await
        .expect("the sign-in must reach the provider's interactive branch, not the browser login")
        .expect("the provider mints when it is allowed to prompt");
    let elapsed = started.elapsed();

    assert_eq!(
        auth.key, SSO_TOKEN,
        "the credential must come from the operator's binary, not a fallback"
    );
    assert_eq!(auth.auth_mode, AuthMode::External);
    assert_eq!(
        invocations(home.path()),
        ["expired=1", "expired=unset"],
        "a sign-in is not a headless run, whatever the state of the credential \
         it replaces"
    );
    assert!(
        elapsed < NO_SELF_CONTENTION,
        "the sign-in must not wait on a lock this process is already holding; \
         took {elapsed:?}"
    );
}
