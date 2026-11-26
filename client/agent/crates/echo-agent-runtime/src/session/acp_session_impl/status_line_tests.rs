use super::{
    build_context_window, emit_loop, live_turn, split_normalized_remote, strip_trailing_separator,
};
use crate::extensions::notification::PromptUsageModel;
use echo_agent_acp::AcpClientMessage;
use echo_agent_workspace::session::git::normalize_repo_url;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedReceiver;

#[test]
fn session_usage_splits_fresh_input_from_the_cache_buckets() {
    let totals = PromptUsageModel {
        input_tokens: 30_000,
        output_tokens: 900,
        cached_read_tokens: 8_000,
        cache_creation_tokens: 5_000,
        model_calls: 1,
        ..Default::default()
    };
    let window = build_context_window(100_000, Some(42_000), Some(&totals), 80);

    // The wire's 30k `input_tokens` already contains both cache buckets, so 17k
    // is what was fresh and the three fields must not overlap.
    let usage = window.session_usage.unwrap();
    assert_eq!(usage.input_tokens, 17_000);
    assert_eq!(usage.cache_creation_input_tokens, 5_000);
    assert_eq!(usage.cache_read_input_tokens, 8_000);
    // The billed total keeps the cache buckets the fresh count sheds.
    assert_eq!(window.session_input_tokens, Some(30_000));
}

#[test]
fn a_turn_is_on_the_wire_only_while_one_is_running() {
    let started = 1_730_000_000_000;

    assert_eq!(
        live_turn(Some(started), Some("prompt-1")),
        Some(echo_agent_status_line::StatusLineTurn {
            started_at_ms: started
        })
    );
    assert_eq!(
        live_turn(Some(started), None),
        None,
        "chat state keeps the stamp after the turn ends, and the prompt id does not"
    );
    assert_eq!(live_turn(None, Some("prompt-1")), None);
}

#[test]
fn percentages_are_whole_numbers_inside_zero_to_one_hundred() {
    let window = build_context_window(300_000, Some(100_000), None, 80);
    assert_eq!(window.used_percentage, Some(33));
    assert_eq!(window.remaining_percentage, Some(67));

    let over = build_context_window(1_000, Some(4_000), None, 80);
    assert_eq!(over.used_percentage, Some(100));
    assert_eq!(over.remaining_percentage, Some(0));
}

#[test]
fn session_usage_is_null_until_a_call_bills() {
    let window = build_context_window(100_000, Some(0), Some(&PromptUsageModel::default()), 80);
    assert!(window.session_usage.is_none());
}

#[test]
fn strips_the_trailing_separator_git_adds() {
    assert_eq!(
        strip_trailing_separator(Path::new("/repo/wt/")),
        PathBuf::from("/repo/wt")
    );
    assert_eq!(strip_trailing_separator(Path::new("/")), PathBuf::from("/"));
}

#[test]
fn only_origin_names_the_repo() {
    use super::remote_url;

    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    repo.remote("upstream", "https://example.com/parent/widget.git")
        .unwrap();
    assert_eq!(remote_url(&repo), None);

    repo.remote("origin", "https://example.com/fork/widget.git")
        .unwrap();
    assert_eq!(
        remote_url(&repo).as_deref(),
        Some("https://example.com/fork/widget.git")
    );
}

#[test]
fn splits_remote_into_host_owner_name() {
    let repo = split_normalized_remote("example.com/acme/widget").unwrap();
    assert_eq!(repo.host, "example.com");
    assert_eq!(repo.owner.as_deref(), Some("acme"));
    assert_eq!(repo.name, "widget");

    let nested = split_normalized_remote("example.com/group/sub/proj").unwrap();
    assert_eq!(nested.owner.as_deref(), Some("sub"));
    assert_eq!(nested.name, "proj");

    let ownerless = split_normalized_remote("example.com/widget").unwrap();
    assert_eq!(ownerless.name, "widget");
    assert_eq!(ownerless.owner, None);

    let tokenized = "https://user:token@example.com/acme/widget.git";
    let clean = split_normalized_remote(&normalize_repo_url(tokenized).unwrap()).unwrap();
    assert_eq!(clean.host, "example.com");
    assert_eq!(clean.name, "widget");
}

#[tokio::test(start_paused = true)]
async fn burst_during_a_build_is_answered_by_one_more_build() {
    let wake = Arc::new(Notify::new());
    let builds = Rc::new(Cell::new(0usize));

    let parked = tokio::time::timeout(
        Duration::from_secs(10),
        emit_loop(wake.clone(), || {
            let builds = builds.clone();
            let wake = wake.clone();
            Some(async move {
                builds.set(builds.get() + 1);
                if builds.get() == 1 {
                    for _ in 0..5 {
                        wake.notify_one();
                    }
                }
                tokio::task::yield_now().await;
            })
        }),
    )
    .await;

    assert!(parked.is_err(), "the loop ran out of wakes and parked");
    assert_eq!(builds.get(), 2, "one build answers the burst, not five");
}

#[tokio::test(start_paused = true)]
async fn nothing_left_to_build_ends_the_loop() {
    tokio::time::timeout(
        Duration::from_secs(10),
        emit_loop(Arc::new(Notify::new()), || None::<std::future::Ready<()>>),
    )
    .await
    .expect("a loop with nothing to build must return");
}

#[tokio::test]
async fn client_that_cannot_draw_the_row_never_builds_one() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (subagent, mut dropped) = emitter_fixture(Client::Subagent).await;
            let refusing = super::run_status_emitter(Arc::downgrade(&subagent));
            let refused = tokio::time::timeout(Duration::from_secs(10), refusing).await;
            assert!(refused.is_ok(), "a subagent's emitter parked on the wake");
            assert!(dropped.try_recv().is_err(), "a subagent built a row");

            let (session, mut painted) = emitter_fixture(Client::WithoutTheRow).await;
            let emitter =
                tokio::task::spawn_local(super::run_status_emitter(Arc::downgrade(&session)));
            session.emit_status_snapshot_detached();
            // Lets the emitter consume the wake while the row is still off.
            tokio::task::yield_now().await;

            session.status_line_enabled.store(true, Ordering::Relaxed);
            session.emit_status_snapshot_detached();
            let seeded = tokio::time::timeout(Duration::from_secs(10), painted.recv()).await;
            assert!(matches!(seeded, Ok(Some(_))), "a later attach must build");

            // Ends the loop, so a build started by the earlier wake has landed
            // before the receiver below is drained.
            drop(session);
            tokio::time::timeout(Duration::from_secs(10), emitter)
                .await
                .expect("the emitter returns once the session is gone")
                .expect("the emitter task panicked");
            assert!(
                painted.try_recv().is_err(),
                "the wake before echo.agent/statusLine built a payload as well"
            );
        })
        .await;
}

#[tokio::test]
async fn the_notification_payload_serializes_without_a_trigger_key() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (session, _rx) = emitter_fixture(Client::WithoutTheRow).await;
            let ctx = session.build_status_context().await;
            let payload = serde_json::to_value(&ctx).expect("the payload serializes");
            assert!(
                payload.get("trigger").is_none(),
                "the notification describes the session, not a run: `trigger` \
                 belongs on a command row's stdin alone"
            );
        })
        .await;
}

#[tokio::test]
async fn a_dropped_session_ends_its_parked_emitter() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (session, _painted) = emitter_fixture(Client::WithoutTheRow).await;
            let emitter =
                tokio::task::spawn_local(super::run_status_emitter(Arc::downgrade(&session)));
            // Parks the emitter on the wake: without the yield it has not
            // reached one, and the test would pass on the loop's first pass.
            tokio::task::yield_now().await;
            assert!(
                !emitter.is_finished(),
                "the emitter left before the session"
            );

            drop(session);
            tokio::time::timeout(Duration::from_secs(10), emitter)
                .await
                .expect("a parked emitter outlived the session that owns it")
                .expect("the emitter task panicked");
        })
        .await;
}

enum Client {
    Subagent,
    WithoutTheRow,
}

async fn emitter_fixture(
    client: Client,
) -> (
    Arc<super::SessionActor>,
    UnboundedReceiver<AcpClientMessage>,
) {
    let (gateway_tx, gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut actor =
        super::super::support::create_test_actor(50_000, 100_000, 85, gateway_tx, persistence_tx)
            .await;
    // A subagent advertises the row and still must not build one.
    let (is_subagent, wants_a_row) = match client {
        Client::Subagent => (true, true),
        Client::WithoutTheRow => (false, false),
    };
    actor.startup_hints.is_subagent = is_subagent;
    actor
        .status_line_enabled
        .store(wants_a_row, Ordering::Relaxed);
    (Arc::new(actor), gateway_rx)
}
