//! Wire-level ACP session setup: `session/resume` MUST NOT replay the
//! conversation via `session/update` before responding, `session/load` MUST.
//! Both are asserted on one session so the detector cannot stop detecting.
//!
//! Resume and close are then re-asserted against a turn that is actually
//! streaming, which is the state they exist to serve and the one an idle
//! session cannot exercise.

mod acp_harness;

use std::cell::RefCell;
use std::rc::Rc;

use acp_harness::{
    RPC_TIMEOUT, allow_once, connect_and_auth, ext_method, new_session, prompt_turn, run_agent_test,
};
use std::time::Duration;

/// How long resume is watched for a late replay after it responds. Generous,
/// since the whole point is to outlast a slow box's scheduling.
const POST_RESPONSE_QUIET: Duration = Duration::from_millis(600);
use agent_client_protocol::{self as acp, Agent as _};
use serde_json::json;

#[derive(Clone, Default)]
struct UpdateLog(Rc<RefCell<Vec<acp::SessionNotification>>>);

impl UpdateLog {
    fn saw_agent_chunk(&self, session: &acp::SessionId) -> bool {
        self.0.borrow().iter().any(|n| {
            n.session_id == *session && matches!(n.update, acp::SessionUpdate::AgentMessageChunk(_))
        })
    }

    fn take_for(&self, session: &acp::SessionId) -> Vec<acp::SessionUpdate> {
        std::mem::take(&mut *self.0.borrow_mut())
            .into_iter()
            .filter(|n| n.session_id == *session)
            .map(|n| n.update)
            .collect()
    }
}

/// Deny-by-default: `SessionUpdate` is `#[non_exhaustive]`. `Plan` is not chrome
/// because mid-turn plans are persisted and re-emitted by replay.
fn is_conversation_content(update: &acp::SessionUpdate) -> bool {
    !matches!(
        update,
        acp::SessionUpdate::AvailableCommandsUpdate(_)
            | acp::SessionUpdate::CurrentModeUpdate(_)
            | acp::SessionUpdate::ConfigOptionUpdate(_)
            | acp::SessionUpdate::SessionInfoUpdate(_)
    )
}

struct RecordingClient {
    log: UpdateLog,
}

#[async_trait::async_trait(?Send)]
impl acp::Client for RecordingClient {
    async fn request_permission(
        &self,
        args: acp::RequestPermissionRequest,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        Ok(acp::RequestPermissionResponse::new(allow_once(&args)))
    }

    async fn session_notification(&self, args: acp::SessionNotification) -> acp::Result<()> {
        self.log.0.borrow_mut().push(args);
        Ok(())
    }
}

async fn resident_sessions(conn: &acp::ClientSideConnection) -> u64 {
    let resp = ext_method(conn, "echo.agent/debug/agent", json!({})).await;
    resp["result"]["registries"]["sessions"]
        .as_u64()
        .unwrap_or_else(|| panic!("echo.agent/debug/agent: no sessions count in {resp}"))
}

/// Resume's headline path is a session whose actor is gone. Evict the
/// resident actor (the leader's client-disconnect signal), wait for
/// residency to drop, then hold cold resume to the same no-replay contract
/// as warm resume; the reused assertion ends with a prompt, proving the
/// respawned actor answers. Reverting the spawn path under resume, or the
/// eviction itself, fails here.
async fn assert_cold_resume_reattaches(
    conn: &acp::ClientSideConnection,
    log: &UpdateLog,
    session_id: &acp::SessionId,
    cwd: &std::path::Path,
) {
    let params = serde_json::value::RawValue::from_string(
        json!({ "sessionIds": [session_id.0] }).to_string(),
    )
    .expect("serialize evict params");
    conn.ext_notification(acp::ExtNotification::new(
        "echo.agent/internal/evict_sessions",
        std::sync::Arc::from(params),
    ))
    .await
    .expect("evict notification failed");

    // Eviction is asynchronous; wait for the unload before resuming cold.
    let deadline = tokio::time::Instant::now() + RPC_TIMEOUT;
    while resident_sessions(conn).await != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "eviction never unloaded the session"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_resume_does_not_replay(conn, log, session_id, cwd).await;
    assert_eq!(
        resident_sessions(conn).await,
        1,
        "cold resume must respawn the actor"
    );
}

/// Near-boundary: the ACP client settles responses inline but dispatches
/// notifications on spawned tasks, which the `LocalSet` drains before re-polling.
async fn assert_resume_does_not_replay(
    conn: &acp::ClientSideConnection,
    log: &UpdateLog,
    session_id: &acp::SessionId,
    cwd: &std::path::Path,
) {
    let _ = log.take_for(session_id);

    let resumed = tokio::time::timeout(
        RPC_TIMEOUT,
        conn.resume_session(acp::ResumeSessionRequest::new(
            session_id.clone(),
            cwd.to_path_buf(),
        )),
    )
    .await
    .expect("session/resume timed out")
    .expect("session/resume failed");

    // Notifications dispatch on spawned tasks, so they trail the response.
    // Watch a wall-clock window rather than a yield count: a fixed count on a
    // slow box can snapshot before a delayed replay lands and pass vacuously.
    let deadline = tokio::time::Instant::now() + POST_RESPONSE_QUIET;
    let mut replayed: Vec<acp::SessionUpdate> = Vec::new();
    while tokio::time::Instant::now() < deadline {
        replayed.extend(
            log.take_for(session_id)
                .into_iter()
                .filter(is_conversation_content),
        );
        assert!(
            replayed.is_empty(),
            "session/resume must not replay the conversation, got {replayed:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let meta = resumed.meta.expect("resume must return session meta");
    assert!(
        meta.contains_key("echo.agent/sessionConfig"),
        "resume must forward the session's config state, got keys {:?}",
        meta.keys().collect::<Vec<_>>()
    );

    prompt_turn(conn, session_id, "still usable after resume").await;
}

async fn assert_load_replays(
    conn: &acp::ClientSideConnection,
    log: &UpdateLog,
    session_id: &acp::SessionId,
    cwd: &std::path::Path,
) {
    let _ = log.take_for(session_id);

    tokio::time::timeout(
        RPC_TIMEOUT,
        conn.load_session(acp::LoadSessionRequest::new(
            session_id.clone(),
            cwd.to_path_buf(),
        )),
    )
    .await
    .expect("session/load timed out")
    .expect("session/load failed");

    // Chunks specifically: a stray `Plan` would satisfy "any content" without a
    // transcript, and this control is what makes the resume assertion falsifiable.
    let updates = log.take_for(session_id);
    assert!(
        updates.iter().any(|u| matches!(
            u,
            acp::SessionUpdate::UserMessageChunk(_) | acp::SessionUpdate::AgentMessageChunk(_)
        )),
        "session/load must replay the conversation, saw only {updates:?}. If this \
         fails, the resume assertion above proves nothing"
    );
}

/// The mock emits one SSE delta per space, so the word count is the chunk count.
///
/// The pace has to stay under the window a request can suppress output for
/// (single-digit milliseconds), or a sparse stream steps over the window and
/// notices nothing. The count then buys the margin: chunks must still be
/// arriving when the request under test lands, on a loaded box too.
const STREAMED_CHUNKS: usize = 300;
const STREAM_PACE: Duration = Duration::from_millis(3);

fn streamed_reply() -> String {
    (0..STREAMED_CHUNKS)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A prompt that is mid-stream: sent, first chunk delivered, not yet finished.
/// The caller drives the returned future to completion after its own request.
async fn start_paced_turn<'a>(
    conn: &'a acp::ClientSideConnection,
    log: &UpdateLog,
    session_id: &acp::SessionId,
    mock: &echo_agent_test_support::MockInferenceServer,
    text: &str,
) -> impl std::future::Future<Output = acp::Result<acp::PromptResponse>> + 'a {
    mock.set_response(streamed_reply());
    mock.set_chunk_delay(Some(STREAM_PACE));
    let _ = log.take_for(session_id);

    let mut turn = Box::pin(conn.prompt(acp::PromptRequest::new(
        session_id.clone(),
        vec![acp::ContentBlock::Text(acp::TextContent::new(
            text.to_owned(),
        ))],
    )));
    assert!(
        futures::poll!(&mut turn).is_pending(),
        "the first poll only sends the prompt"
    );

    let deadline = tokio::time::Instant::now() + RPC_TIMEOUT;
    while !log.saw_agent_chunk(session_id) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the turn never streamed a chunk"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        futures::poll!(&mut turn).is_pending(),
        "the turn finished before the test could act on it, so it proves nothing"
    );
    turn
}

/// Which chunks never reached the client. Reported as indices because output
/// suppressed by a request shows up as a short run in the middle, and a dump of
/// the whole reply buries it.
fn missing_chunks(streamed: &str) -> Vec<usize> {
    let seen: std::collections::HashSet<&str> = streamed.split_whitespace().collect();
    (0..STREAMED_CHUNKS)
        .filter(|i| !seen.contains(format!("w{i}").as_str()))
        .collect()
}

fn streamed_text(updates: &[acp::SessionUpdate]) -> String {
    updates
        .iter()
        .filter_map(|u| match u {
            acp::SessionUpdate::AgentMessageChunk(c) => Some(&c.content),
            _ => None,
        })
        .filter_map(|c| match c {
            acp::ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect()
}

/// Resume suppresses replay by closing the session's update gate. Closing it on
/// a session whose turn is still running silently drops the rest of that turn's
/// output, which is the failure mode the `noReplay` branch guards against.
async fn assert_resume_does_not_cut_a_live_turn(
    conn: &acp::ClientSideConnection,
    log: &UpdateLog,
    session_id: &acp::SessionId,
    cwd: &std::path::Path,
    mock: &echo_agent_test_support::MockInferenceServer,
) {
    let turn = start_paced_turn(conn, log, session_id, mock, "stream while I reattach").await;

    tokio::time::timeout(
        RPC_TIMEOUT,
        conn.resume_session(acp::ResumeSessionRequest::new(
            session_id.clone(),
            cwd.to_path_buf(),
        )),
    )
    .await
    .expect("session/resume timed out")
    .expect("session/resume during a live turn failed");

    let resp = tokio::time::timeout(RPC_TIMEOUT, turn)
        .await
        .expect("the live turn never finished after the resume")
        .expect("the live turn failed after the resume");
    assert!(
        matches!(resp.stop_reason, acp::StopReason::EndTurn),
        "resume must not disturb the running turn, got {:?}",
        resp.stop_reason
    );
    mock.set_chunk_delay(None);

    let missing = missing_chunks(&streamed_text(&log.take_for(session_id)));
    assert!(
        missing.is_empty(),
        "every chunk the turn emitted across the resume must reach the client, \
         but the client never saw {missing:?}"
    );
}

/// The spec's close cancels ongoing work. Every other close assertion here runs
/// against an idle session, where "cancel" costs nothing.
///
/// Honest about its reach: the turn-versus-teardown outcome is a race, and this
/// catches only the reliable half of it. It holds the line that a closed
/// session's turn ends and does not report a clean `EndTurn`; it will not
/// notice a regression that merely widens the window.
async fn assert_close_cancels_a_live_turn(
    conn: &acp::ClientSideConnection,
    log: &UpdateLog,
    session_id: &acp::SessionId,
    mock: &echo_agent_test_support::MockInferenceServer,
) {
    let turn = start_paced_turn(conn, log, session_id, mock, "stream while I close").await;

    tokio::time::timeout(
        RPC_TIMEOUT,
        conn.close_session(acp::CloseSessionRequest::new(session_id.clone())),
    )
    .await
    .expect("session/close timed out behind a running turn")
    .expect("session/close during a live turn failed");
    mock.set_chunk_delay(None);

    // The turn must end, one way or another: a close that leaves it running has
    // freed the session's bookkeeping and not its work.
    let ended = tokio::time::timeout(RPC_TIMEOUT, turn)
        .await
        .expect("the closed session's turn never ended");
    assert!(
        ended.is_err()
            || !matches!(
                ended.as_ref().unwrap().stop_reason,
                acp::StopReason::EndTurn
            ),
        "a closed session's turn must not report a normal EndTurn, got {ended:?}"
    );
    assert_eq!(
        resident_sessions(conn).await,
        0,
        "close must free the session even when it had work in flight"
    );
}

async fn assert_close_frees_the_session(
    conn: &acp::ClientSideConnection,
    session_id: &acp::SessionId,
) {
    assert_eq!(
        resident_sessions(conn).await,
        1,
        "the session must be resident before the close"
    );

    tokio::time::timeout(
        RPC_TIMEOUT,
        conn.close_session(acp::CloseSessionRequest::new(session_id.clone())),
    )
    .await
    .expect("session/close timed out")
    .expect("session/close failed");

    assert_eq!(
        resident_sessions(conn).await,
        0,
        "session/close must free the session's resources"
    );

    let after = tokio::time::timeout(
        RPC_TIMEOUT,
        conn.prompt(acp::PromptRequest::new(
            session_id.clone(),
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                "after close".to_owned(),
            ))],
        )),
    )
    .await
    .expect("prompt after close timed out");
    assert!(
        after.is_err(),
        "a prompt against a closed session must be rejected, got {after:?}"
    );

    tokio::time::timeout(
        RPC_TIMEOUT,
        conn.close_session(acp::CloseSessionRequest::new(session_id.clone())),
    )
    .await
    .expect("second session/close timed out")
    .expect("closing an already-closed session must succeed");
}

#[test]
fn acp_session_setup_conformance() {
    run_agent_test(|cwd, mock| async move {
        let log = UpdateLog::default();
        let (conn, init) = connect_and_auth(
            RecordingClient { log: log.clone() },
            "acp-session-setup-test",
        )
        .await;
        let capabilities = &init.agent_capabilities.session_capabilities;
        assert!(
            capabilities.resume.is_some() && capabilities.close.is_some(),
            "clients may not call resume/close unless advertised, got {capabilities:?}"
        );

        let session_id = new_session(&conn, &cwd).await;
        prompt_turn(&conn, &session_id, "remember this turn").await;

        assert_resume_does_not_replay(&conn, &log, &session_id, &cwd).await;
        assert_load_replays(&conn, &log, &session_id, &cwd).await;
        assert_cold_resume_reattaches(&conn, &log, &session_id, &cwd).await;
        assert_resume_does_not_cut_a_live_turn(&conn, &log, &session_id, &cwd, &mock).await;
        assert_close_frees_the_session(&conn, &session_id).await;

        // A second session, because the first one is closed. The live-turn close
        // is asserted last: it is the only case that needs work in flight.
        let session_id = new_session(&conn, &cwd).await;
        assert_close_cancels_a_live_turn(&conn, &log, &session_id, &mock).await;
    });
}
