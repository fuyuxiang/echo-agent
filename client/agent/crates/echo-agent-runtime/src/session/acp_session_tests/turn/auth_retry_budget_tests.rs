//! Real-turn-loop tests against a mock server that 401s unauthenticated
//! requests and 200s a fresh bearer: a fail-closed (credential-less) 401
//! must not consume `AuthRetrySchedule` budget — the field failure mode
//! where each sleep cycle burned one slot — while credentialed 401s must
//! still exhaust after `MAX_RETRIES`.

use super::support::*;
use super::*;
use crate::auth::{AuthManager, AuthMode, EchoAgentAuth, EchoAgentComConfig};
use echo_agent_test_support::{MockInferenceServer, MockModelEntry};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

/// The token the mock server accepts and the refresher mints on success.
const FRESH_TOKEN: &str = "refreshed-test-token";

/// With `fail_pre_request`, mimics the post-wake sequence: pre-send
/// (`PreRequest`) refreshes fail transiently so the send goes out
/// fail-closed, while the 401-triggered recovery (`ServerRejected`)
/// succeeds and mints [`FRESH_TOKEN`]. Otherwise always succeeds.
struct WakeGapRefresher {
    calls: Arc<AtomicU32>,
    fail_pre_request: bool,
}

#[async_trait::async_trait]
impl crate::auth::refresh::TokenRefresher for WakeGapRefresher {
    async fn refresh(
        &self,
        reason: crate::auth::refresh::RefreshReason,
    ) -> crate::auth::refresh::RefreshOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_pre_request && reason == crate::auth::refresh::RefreshReason::PreRequest {
            return crate::auth::refresh::RefreshOutcome::TransientFailure {
                message: "simulated post-wake network gap".to_string(),
            };
        }
        crate::auth::refresh::RefreshOutcome::success(EchoAgentAuth {
            key: FRESH_TOKEN.to_string(),
            auth_mode: AuthMode::Oidc,
            refresh_token: Some("rt-new".into()),
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        })
    }
}

/// `(tempdir, manager)` with a hard-expired OIDC token, so the wire-valid
/// resolver has nothing to stamp until the refresher succeeds. The tempdir
/// must outlive the manager (auth.json path).
fn expired_auth_manager(
    refresher: Arc<dyn crate::auth::refresh::TokenRefresher>,
) -> (tempfile::TempDir, Arc<AuthManager>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let am = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    am.hot_swap(EchoAgentAuth {
        key: "initial-test-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    am.set_refresher(refresher);
    (dir, am)
}

/// `echo.agent/session_notification` payloads the client was sent.
type EchoAgentUpdates = Arc<parking_lot::Mutex<Vec<serde_json::Value>>>;

fn drain_gateway(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<echo_agent_acp::AcpClientMessage>,
) -> EchoAgentUpdates {
    let captured = EchoAgentUpdates::default();
    let sink = captured.clone();
    tokio::task::spawn_local(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                echo_agent_acp::AcpClientMessage::SessionNotification(args) => {
                    let _ = args.response_tx.send(Ok(()));
                }
                echo_agent_acp::AcpClientMessage::ExtNotification(args) => {
                    if let Ok(value) = serde_json::from_str(args.params.get()) {
                        sink.lock().push(value);
                    }
                }
                _ => {}
            }
        }
    });
    captured
}

/// `(error_type, message)` of the turn's terminal `retryState`, if the client
/// was told about one at all.
fn terminal_failure(updates: &EchoAgentUpdates) -> Option<(String, String)> {
    updates.lock().iter().find_map(|value| {
        let update = value.get("update")?;
        if update.get("sessionUpdate")? != "retry_state" || update.get("type")? != "failed" {
            return None;
        }
        Some((
            update.get("error_type")?.as_str()?.to_owned(),
            update.get("message")?.as_str()?.to_owned(),
        ))
    })
}

fn drain_persistence(mut rx: tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>) {
    tokio::task::spawn_local(async move {
        while let Some(msg) = rx.recv().await {
            if let PersistenceMsg::FlushAndAck { respond_to } = msg {
                let _ = respond_to.send(Ok(()));
            }
        }
    });
}

/// Actor wired for session-token auth against the mock server: real sampler,
/// `cached_token` method, `NotByok` model facts (so the session-token gate is
/// active against the loopback URL), and the supplied auth manager.
async fn session_token_actor(
    server: &MockInferenceServer,
    auth_manager: Arc<AuthManager>,
) -> (Arc<SessionActor>, EchoAgentUpdates) {
    let sampling_cfg = echo_agent_sampler::SamplerConfig {
        base_url: server.url(),
        model: "test".to_string(),
        api_backend: echo_agent_sampler::ApiBackend::Responses,
        context_window: 256_000,
        max_retries: Some(0),
        idle_timeout_secs: Some(30),
        ..Default::default()
    };
    let (sampler_event_tx, sampler_event_rx) =
        tokio::sync::mpsc::unbounded_channel::<echo_agent_sampler::SamplingEvent>();
    let sampler_handle = echo_agent_sampler::SamplerActor::spawn(
        sampling_cfg,
        echo_agent_sampler::RetryPolicy {
            max_retries: 0,
            rate_limit_retry_threshold: 0,
            ..Default::default()
        },
        sampler_event_tx,
    );

    let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let echo_agent_updates = drain_gateway(gateway_rx);
    let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    drain_persistence(persistence_rx);

    let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    actor.sampler_handle = sampler_handle;
    actor.auth_manager = Some(auth_manager);
    actor.auth_method_id = test_auth_method_id("cached_token");

    let mut cfg = actor
        .chat_state_handle
        .get_sampling_config()
        .await
        .expect("test actor has sampling config");
    cfg.base_url = server.url();
    cfg.api_backend = echo_agent_sampling_types::ApiBackend::Responses;
    cfg.model = "test".to_string();
    actor.chat_state_handle.update_sampling_config(cfg);
    let mut creds = actor.chat_state_handle.get_credentials().await;
    creds.api_key = None;
    creds.auth_type = echo_agent_chat_state::AuthType::SessionToken;
    actor.chat_state_handle.update_credentials(creds);

    // Definite NotByok: the session-token gate must stay active against the
    // loopback mock URL (an `Unknown` would demand a first-party host).
    actor
        .model_auth_memo
        .replace(Some(crate::session::acp_session::ModelAuthMemo {
            model_id: "test".to_string(),
            facts: crate::agent::config::ModelAuthFacts {
                byok: crate::agent::auth_method::ModelByok::NotByok,
                auth_scheme: Default::default(),
            },
            provider: None,
        }));

    actor
        .workspace_ops
        .bind_local_session(
            &actor.session_id_string(),
            actor.tool_context.cwd.as_path().to_path_buf(),
            actor.tool_context.hunk_tracker_handle.clone(),
            actor.agent.borrow().tool_bridge().toolset(),
            None,
        )
        .expect("bind_local_session");

    let actor = Arc::new(actor);
    {
        let drainer = actor.clone();
        let mut sampler_event_rx = sampler_event_rx;
        tokio::task::spawn_local(async move {
            while let Some(event) = sampler_event_rx.recv().await {
                drainer.handle_sampling_event(event).await;
            }
        });
    }
    (actor, echo_agent_updates)
}

async fn run_prompt(
    actor: &Arc<SessionActor>,
    prompt_id: &str,
) -> Result<crate::session::commands::PromptTurnOk, acp::Error> {
    let prompt_blocks = vec![acp::ContentBlock::Text(acp::TextContent::new(
        "hello".to_string(),
    ))];
    tokio::time::timeout(
        Duration::from_secs(60),
        actor.handle_prompt(
            prompt_id,
            prompt_blocks,
            PromptMode::Agent,
            None,
            None,
            None,
            None,
            true,
            /* send_now */ false,
            None,
            None,
            None,
        ),
    )
    .await
    .expect("turn must finish within timeout")
}

/// The wake sequence: the resolver has nothing wire-valid, the send goes
/// out with no `Authorization` header, the server 401s it, recovery lands a
/// fresh token. The turn must survive and resubmit with the fresh bearer.
#[tokio::test(flavor = "current_thread")]
async fn fail_closed_401_is_uncharged_and_turn_survives() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let server = MockInferenceServer::start_with_required_auth(
                vec![MockModelEntry::new("test")],
                FRESH_TOKEN,
            )
            .await
            .expect("mock inference server");

            let calls = Arc::new(AtomicU32::new(0));
            // Pre-send refreshes fail like a post-wake network gap, so the
            // first send goes out fail-closed; the 401-recovery refresh
            // succeeds.
            let refresher = Arc::new(WakeGapRefresher {
                calls: calls.clone(),
                fail_pre_request: true,
            });
            let (_dir, am) = expired_auth_manager(refresher);
            let (actor, _updates) = session_token_actor(&server, am).await;

            let outcome = run_prompt(&actor, "auth-retry-budget-fail-closed").await;
            assert!(
                outcome.is_ok(),
                "fail-closed 401 must not fail the turn: {outcome:?}"
            );

            let inference: Vec<_> = server
                .requests()
                .into_iter()
                .filter(|r| r.path.contains("/responses"))
                .collect();
            assert!(
                inference.len() >= 2,
                "expected the fail-closed send plus the resubmit; got {}",
                inference.len()
            );
            assert_eq!(
                inference[0].authorization, None,
                "first send must carry no Authorization header"
            );
            assert_eq!(
                inference.last().unwrap().authorization.as_deref(),
                Some(&format!("Bearer {FRESH_TOKEN}") as &str),
                "resubmit must carry the freshly refreshed bearer"
            );
            assert!(
                calls.load(Ordering::SeqCst) >= 2,
                "both the failing pre-flight and the recovery refresh must run"
            );
        })
        .await;
}

/// Real credential rejections must still terminate: when every request
/// carries a bearer the server rejects, the escalating budget exhausts after
/// `MAX_RETRIES` and the failure names authenticated rejections — not a
/// generic budget message. Use the real clock with real TCP: auto-advancing
/// virtual time can expire the outer watchdog while peer I/O is pending.
/// The retry ladder itself has separate virtual-clock unit coverage.
#[tokio::test(flavor = "current_thread")]
async fn authenticated_401s_still_exhaust_after_three_retries() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // The server only accepts a token the refresher never mints, so
            // every authenticated send is rejected.
            let server = MockInferenceServer::start_with_required_auth(
                vec![MockModelEntry::new("test")],
                "never-issued-token",
            )
            .await
            .expect("mock inference server");

            let refresher = Arc::new(WakeGapRefresher {
                calls: Arc::new(AtomicU32::new(0)),
                fail_pre_request: false,
            });
            let (_dir, am) = expired_auth_manager(refresher);
            let (actor, updates) = session_token_actor(&server, am).await;

            let outcome = run_prompt(&actor, "auth-retry-budget-exhaust").await;
            let err = outcome.expect_err("authenticated 401s must exhaust and fail the turn");
            let rendered = serde_json::to_string(&err.data).unwrap_or_default();
            assert!(
                rendered.contains("authenticated inference requests were still rejected"),
                "exhaustion must name authenticated rejections, got: {rendered}"
            );

            let authenticated = server
                .requests()
                .into_iter()
                .filter(|r| r.path.contains("/responses"))
                .filter(|r| r.authorization.as_deref() == Some(&format!("Bearer {FRESH_TOKEN}")))
                .count();
            assert_eq!(
                authenticated, 4,
                "initial send plus MAX_RETRIES resubmits, all authenticated"
            );

            // The budget is the one terminal path that lives outside
            // `handle_sampling_failure`, and it used to return its error with
            // no notification at all — leaving the pager with no re-auth
            // prompt and no turn-failed block for a turn that died on 401s.
            let (error_type, message) = terminal_failure(&updates)
                .expect("an exhausted turn must report a terminal retryState");
            assert_eq!(
                error_type, "auth",
                "the client keys its re-auth prompt off this: {message}"
            );
            assert!(
                message.contains("authenticated inference requests were still rejected"),
                "the notification must carry the same story as the error: {message}"
            );
        })
        .await;
}
