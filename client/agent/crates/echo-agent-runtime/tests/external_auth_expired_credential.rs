//! Regression: an expired `auth_provider_command` credential must route the
//! user into the provider's sign-in flow, not into a silent 401 loop.
//!
//! The deployment under test: an operator binary mints the session credential,
//! and it cannot mint from the headless refresh because it needs the user to
//! complete an SSO flow. Before the fix a *stale* credential was treated better
//! than no credential — the client skipped login, the dead bearer was accepted,
//! and the first turn 401'd under "no need to run /login".
//!
//! The mirror of phase 3 — a provider that blocks until it is killed, leaving
//! no verdict behind — is a unit test (`auth::manager::remedy`); driving it here
//! would buy the same assertions for two more timeout budgets of wall clock.
//!
//! One `#[test]`: the phases share one process-global `ECHO_AGENT_HOME` and env, so
//! nothing else may run concurrently.
#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::{self as acp, Agent as _};
use echo_agent_acp::{
    AcpAgentGatewayReceiver as GatewayReceiver, AcpAgentGatewaySender as GatewaySender,
    LineBufferedRead,
};
use echo_agent_runtime::agent::config::Config as AgentConfig;
use echo_agent_runtime::agent::mvp_agent::MvpAgent;
use echo_agent_test_support::{MockInferenceServer, MockModelEntry};
use serde_json::json;
use tempfile::TempDir;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

const DUPLEX_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const RPC_TIMEOUT: Duration = Duration::from_secs(60);

/// The only bearer the mock accepts; no credential in this test is it.
const FRESH_TOKEN: &str = "fresh-token-the-provider-cannot-mint";
const STALE_TOKEN: &str = "stale-external-token";
const PROVIDER_LABEL: &str = "Acme SSO";

/// Records `echo.agent/session/update` payloads so a phase can read the terminal
/// `retryState`.
#[derive(Clone, Default)]
struct Capture {
    updates: std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>,
    arrived: std::rc::Rc<tokio::sync::Notify>,
}

impl Capture {
    fn record(&self, update: serde_json::Value) {
        self.updates.borrow_mut().push(update);
        self.arrived.notify_one();
    }

    /// `(error_type, message)` of the turn's terminal failure.
    ///
    /// Awaited, not read: the failed prompt's JSON-RPC response and this
    /// notification reach the client down independent paths, so the response
    /// routinely arrives first.
    async fn await_terminal_failure(&self, within: Duration) -> (String, String) {
        let found = tokio::time::timeout(within, async {
            loop {
                if let Some(failure) = self.terminal_failure() {
                    return failure;
                }
                self.arrived.notified().await;
            }
        })
        .await;
        found.unwrap_or_else(|_| {
            panic!(
                "the turn must report a terminal retryState; captured instead: {:#}",
                serde_json::Value::Array(self.updates.borrow().clone())
            )
        })
    }

    fn terminal_failure(&self) -> Option<(String, String)> {
        self.updates.borrow().iter().find_map(|value| {
            let update = value.get("update")?;
            if update.get("sessionUpdate")? != "retry_state" || update.get("type")? != "failed" {
                return None;
            }
            let error_type = update.get("error_type")?.as_str()?.to_owned();
            let message = update.get("message")?.as_str()?.to_owned();
            Some((error_type, message))
        })
    }
}

struct QuietClient(Capture);

#[async_trait::async_trait(?Send)]
impl acp::Client for QuietClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let outcome = args
            .options
            .first()
            .map(|o| {
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                    o.option_id.clone(),
                ))
            })
            .unwrap_or(acp::RequestPermissionOutcome::Cancelled);
        Ok(acp::RequestPermissionResponse::new(outcome))
    }

    async fn session_notification(&self, _args: acp::SessionNotification) -> acp::Result<()> {
        Ok(())
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> acp::Result<()> {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(args.params.get()) {
            self.0.record(value);
        }
        Ok(())
    }
}

/// Written under the legacy scope key, which `lookup_auth` falls back to for
/// any configured scope.
fn seed_credential(echo_agent_home: &Path, expires_at: chrono::DateTime<chrono::Utc>) {
    let auth = json!({
        "https://accounts.echo-agent.invalid/sign-in": {
            "key": STALE_TOKEN,
            "auth_mode": "external",
            "create_time": (chrono::Utc::now() - chrono::Duration::hours(9)).to_rfc3339(),
            "user_id": "user-ext-1",
            "email": "engineer@acme.example",
            "expires_at": expires_at.to_rfc3339(),
        }
    });
    std::fs::create_dir_all(echo_agent_home).expect("create echoagent home");
    std::fs::write(
        echo_agent_home.join("auth.json"),
        serde_json::to_string_pretty(&auth).expect("serialize auth.json"),
    )
    .expect("write auth.json");
}

/// A provider that can only sign the user in interactively: it prints its SSO
/// link to stderr and exits non-zero, like a real device-code helper with no
/// human at the keyboard.
fn write_interactive_only_provider(echo_agent_home: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;

    let script = echo_agent_home.join("acme-auth.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         echo run >> \"$(dirname \"$0\")/provider-runs\"\n\
         echo 'Sign in at https://sso.acme.example/device' >&2\n\
         exit 1\n",
    )
    .expect("write provider script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod provider script");
    script.display().to_string()
}

async fn connect(
    client_type: &str,
    capture: Capture,
) -> (acp::ClientSideConnection, acp::InitializeResponse) {
    let agent_config = AgentConfig::default();
    let auth_manager = Arc::new(agent_config.create_auth_manager());
    let (gw_tx, gw_rx) = tokio::sync::mpsc::unbounded_channel();
    let gateway = GatewaySender::new(gw_tx);
    let agent = MvpAgent::new(gateway, &agent_config, auth_manager, None).expect("valid config");

    let (c2a_a, c2a_b) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
    let (a2c_a, a2c_b) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);

    let agent_incoming = LineBufferedRead::spawn_local(c2a_b.compat());
    let (agent_conn, agent_io) =
        acp::AgentSideConnection::new(agent, a2c_a.compat_write(), agent_incoming, |fut| {
            tokio::task::spawn_local(fut);
        });
    tokio::task::spawn_local(
        GatewayReceiver::new(gw_rx, agent_conn)
            .with_on_meta(echo_agent_file_utils::trace_context::span_from_meta_traceparent)
            .run(),
    );
    tokio::task::spawn_local(agent_io);

    let client_incoming = LineBufferedRead::spawn_local(a2c_b.compat());
    let (client_conn, client_io) = acp::ClientSideConnection::new(
        QuietClient(capture),
        c2a_a.compat_write(),
        client_incoming,
        |fut| {
            tokio::task::spawn_local(fut);
        },
    );
    tokio::task::spawn_local(client_io);

    let init = tokio::time::timeout(
        RPC_TIMEOUT,
        client_conn.initialize(
            acp::InitializeRequest::new(acp::ProtocolVersion::V1)
                .client_capabilities(
                    acp::ClientCapabilities::new()
                        .fs(acp::FileSystemCapabilities::new())
                        .terminal(false),
                )
                .meta(
                    json!({
                        "startupHints": {
                            "nonInteractive": true,
                            "skipGitStatus": true,
                            "skipProjectLayout": true,
                        },
                        "clientType": client_type,
                        "clientVersion": "0.0-test",
                    })
                    .as_object()
                    .cloned(),
                ),
        ),
    )
    .await
    .expect("initialize timed out")
    .expect("initialize failed");

    (client_conn, init)
}

/// How many times the provider binary has been invoked so far.
fn provider_runs(echo_agent_home: &Path) -> usize {
    std::fs::read_to_string(echo_agent_home.join("provider-runs"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// `(method id, external_provider flag)` per advertised method, in order.
fn advertised(init: &acp::InitializeResponse) -> Vec<(String, bool)> {
    init.auth_methods
        .iter()
        .map(|m| {
            let external = m
                .meta()
                .as_ref()
                .and_then(|v| v.get("external_provider"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            (m.id().0.to_string(), external)
        })
        .collect()
}

/// Environment entries an unattended mint could take a service endpoint from,
/// matched by shape rather than by name: the test needs "no endpoint anywhere",
/// and a build wired to a different recovery backend must not silently regain
/// one. Collected before removal — the caller mutates the environment it reads.
fn ambient_mint_endpoints() -> Vec<String> {
    std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| name.ends_with("_SERVICE_ENDPOINT") || name.ends_with("_SERVICE_URL"))
        .collect()
}

#[test]
fn expired_external_credential_routes_to_the_provider_login_flow() {
    echo_agent_extra_ca::ensure_default_crypto_provider();

    let mock_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("mock runtime");
    let server = mock_rt
        .block_on(MockInferenceServer::start_with_required_auth(
            vec![MockModelEntry::new("test-model")],
            FRESH_TOKEN,
        ))
        .expect("mock server");

    let echo_agent_home = TempDir::new().expect("echoagent home");
    let workdir = TempDir::new().expect("workdir");
    seed_credential(
        echo_agent_home.path(),
        chrono::Utc::now() - chrono::Duration::hours(1),
    );
    let provider = write_interactive_only_provider(echo_agent_home.path());

    // SAFETY: the only other live threads are the mock runtime's HTTP workers,
    // which never read the process environment.
    unsafe {
        std::env::set_var("ECHO_AGENT_HOME", echo_agent_home.path());
        std::env::set_var("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_API_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_MODELS_BASE_URL", server.url());
        std::env::set_var("ECHO_AGENT_AUTH_PROVIDER_COMMAND", &provider);
        std::env::set_var("ECHO_AGENT_AUTH_PROVIDER_LABEL", PROVIDER_LABEL);
        // An API key would be advertised first and mask the session-auth path.
        std::env::remove_var("ECHO_AGENT_API_KEY");
        std::env::remove_var("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
        // Last-resort 401 recovery can mint a credential from an endpoint
        // named in the ambient environment, which on a container-hosted
        // runner would rescue the session behind the test's back. Leave it
        // nothing to mint from: the deployment under test is one where only
        // the operator's binary can produce a credential.
        for name in ambient_mint_endpoints() {
            std::env::remove_var(&name);
        }
        std::env::set_var("ECHO_AGENT_TELEMETRY_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_FEEDBACK_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_TRACE_UPLOAD", "false");
    }

    let agent_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("agent runtime");
    let local = tokio::task::LocalSet::new();
    agent_rt.block_on(local.run_until(async move {
        // Phase 1 — startup with the expired credential.
        let (_conn, init) = connect("external-auth-expired", Capture::default()).await;
        let methods = advertised(&init);
        assert_eq!(
            methods.first().map(|(id, _)| id.as_str()),
            Some("cloud.echo-agent.invalid"),
            "an expired credential the provider cannot renew must advertise the \
             login method first, not `cached_token`; got {methods:?}"
        );
        assert!(
            methods.first().is_some_and(|(_, external)| *external),
            "the login method must carry external_provider so the client runs \
             the operator's binary instead of opening a browser; got {methods:?}"
        );
        assert!(
            !methods.iter().any(|(id, _)| id == "cached_token"),
            "the dead bearer must not be offered at all; got {methods:?}"
        );
        assert_eq!(
            provider_runs(echo_agent_home.path()),
            1,
            "startup owes the provider exactly one headless attempt — the escalation \
             above must come after it, and the attempt must not be re-run per launch"
        );

        // Phase 2 — parity with a launch that has no credential at all.
        std::fs::remove_file(echo_agent_home.path().join("auth.json")).expect("remove auth.json");
        let (_conn, init) = connect("external-auth-cold", Capture::default()).await;
        assert_eq!(
            advertised(&init),
            methods,
            "an expired credential must be treated exactly like no credential"
        );
        assert_eq!(
            provider_runs(echo_agent_home.path()),
            1,
            "with nothing to refresh there is no headless attempt to make; the \
             binary runs when the client starts the login flow"
        );

        // Phase 3 — mid-session: a credential that has not locally expired but
        // that the backend rejects.
        seed_credential(
            echo_agent_home.path(),
            chrono::Utc::now() + chrono::Duration::hours(1),
        );
        let capture = Capture::default();
        let (conn, init) = connect("external-auth-mid-session", capture.clone()).await;
        assert_eq!(
            advertised(&init).first().map(|(id, _)| id.as_str()),
            Some("cached_token"),
            "a live credential is still a frictionless start"
        );
        tokio::time::timeout(
            RPC_TIMEOUT,
            conn.authenticate(
                acp::AuthenticateRequest::new(acp::AuthMethodId::new("cached_token"))
                    .meta(json!({ "headless": true }).as_object().cloned()),
            ),
        )
        .await
        .expect("authenticate timed out")
        .expect("authenticate with a live credential must succeed");

        let session = tokio::time::timeout(
            RPC_TIMEOUT,
            conn.new_session(
                acp::NewSessionRequest::new(workdir.path().to_path_buf())
                    .meta(json!({ "modelId": "test-model" }).as_object().cloned()),
            ),
        )
        .await
        .expect("session/new timed out")
        .expect("session/new failed");

        let outcome = tokio::time::timeout(
            RPC_TIMEOUT,
            conn.prompt(acp::PromptRequest::new(
                session.session_id.clone(),
                vec![acp::ContentBlock::Text(acp::TextContent::new(
                    "say hi".to_string(),
                ))],
            )),
        )
        .await
        .expect("prompt timed out");
        let error = outcome.expect_err("the mock 401s every inference request");
        // `error_data_with_status` carries a bare string when the sampler had
        // no HTTP status to attach, and an object when it did; a 401 that
        // classifies as `SamplingErrorKind::Auth` is routinely the former.
        let data = error.data.as_ref().expect("a failed turn explains itself");
        let message = data
            .get("message")
            .unwrap_or(data)
            .as_str()
            .expect("the turn error's message is a string")
            .to_owned();
        assert!(
            !message.contains("no need to run /login"),
            "the message must not tell the user to wait it out: {message}"
        );
        assert!(
            message.contains(PROVIDER_LABEL) && message.contains("/login"),
            "the message must name the provider and the remedy: {message}"
        );
        let auth_line = message
            .lines()
            .find(|line| line.trim_start().starts_with("Auth:"))
            .expect("the 401 diagnostics must report an auth mode");
        assert!(
            auth_line.contains("External"),
            "the diagnostics must report the real auth mode, not the ApiKey \
             fallback `current()` produces for an expired session: {auth_line}"
        );

        let (error_type, notified) = capture.await_terminal_failure(RPC_TIMEOUT).await;
        assert_eq!(
            error_type, "auth",
            "a 401 the provider could not refresh is not a transient blip: \
             `auth_transient` is excluded from `is_reauthable_failure`, so the \
             client would show neither the banner nor a way forward"
        );
        assert_eq!(
            notified, message,
            "the banner and the turn error must say the same thing"
        );
    }));
}
