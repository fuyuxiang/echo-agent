//! Integration coverage for the planner trigger inside
//! `setup_goal` and the session-load reconciliation hook
//! `maybe_reconcile_active_goal_without_plan`. Uses the same
//! single-thread + LocalSet pattern as the verification-stage e2e suite.

use super::support::*;
use super::*;
use echo_agent_tools::implementations::echo_agent_build::task::types::{
    SubagentEvent, SubagentResult,
};
use serial_test::serial;
use std::sync::Arc as StdArc;
use std::sync::atomic::{AtomicUsize, Ordering as SeqOrd};
use tempfile::TempDir;

/// Pull the planner's plan-file path from the prompt by its backtick-quoted
/// `.md` token, so rewording the surrounding sentence can't silently break the
/// fake (which would otherwise write nothing and fail far from the cause).
fn plan_path_from_prompt(prompt: &str) -> Option<String> {
    // Backtick-delimited tokens sit at the odd indices of the split.
    prompt
        .split('`')
        .skip(1)
        .step_by(2)
        .find(|token| token.ends_with(".md"))
        .map(str::to_owned)
}

/// Spawn behaviour knobs for the planner-coordinator stub.
enum SpawnBehaviour {
    /// Parse `{PLAN_FILE}` out of the prompt, write `body` there,
    /// then respond `Done`.
    WritePlanThenDone { body: &'static [u8] },
    WaitForCancelsThenWrite {
        cancels: usize,
        started: tokio::sync::mpsc::UnboundedSender<usize>,
        objectives: StdArc<std::sync::Mutex<Vec<String>>>,
        body: &'static [u8],
    },
    /// Reply success but never write the file.
    NoWriteThenDone,
    /// Reply with subagent runtime failure.
    Runtime { message: String, cancelled: bool },
}

/// Captured planner spawn flags (harness-internal `SubagentRequest` fields).
#[derive(Default)]
struct PlannerSpawnCapture {
    fork_context: StdArc<std::sync::Mutex<Vec<bool>>>,
    surface_completion: StdArc<std::sync::Mutex<Vec<bool>>>,
    model: StdArc<std::sync::Mutex<Vec<Option<String>>>>,
}

/// Stand up a coordinator that handles exactly the spawn behaviours
/// the planner exercises (Spawn → result_tx). Returns the sender
/// half + a spawn-count counter the test reads at the end.
fn spawn_planner_coordinator(
    behaviour: SpawnBehaviour,
) -> (
    tokio::sync::mpsc::UnboundedSender<SubagentEvent>,
    StdArc<AtomicUsize>,
) {
    let (tx, count, _capture) = spawn_planner_coordinator_capturing(behaviour);
    (tx, count)
}

/// Like [`spawn_planner_coordinator`] but also records harness flags
/// (`fork_context`, `surface_completion`) from each spawn.
fn spawn_planner_coordinator_capturing(
    behaviour: SpawnBehaviour,
) -> (
    tokio::sync::mpsc::UnboundedSender<SubagentEvent>,
    StdArc<AtomicUsize>,
    PlannerSpawnCapture,
) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
    let spawn_count = StdArc::new(AtomicUsize::new(0));
    let count_task = StdArc::clone(&spawn_count);
    let capture = PlannerSpawnCapture::default();
    let fork_log = StdArc::clone(&capture.fork_context);
    let surface_log = StdArc::clone(&capture.surface_completion);
    let model_log = StdArc::clone(&capture.model);
    tokio::task::spawn_local(async move {
        while let Some(ev) = rx.recv().await {
            if let SubagentEvent::Spawn(req) = ev {
                count_task.fetch_add(1, SeqOrd::SeqCst);
                fork_log.lock().unwrap().push(req.fork_context);
                surface_log.lock().unwrap().push(req.surface_completion);
                model_log
                    .lock()
                    .unwrap()
                    .push(req.runtime_overrides.model.clone());
                let plan_path = plan_path_from_prompt(&req.prompt);
                let result = match &behaviour {
                    SpawnBehaviour::WritePlanThenDone { body } => {
                        if let Some(p) = plan_path.as_deref() {
                            let _ =
                                std::fs::create_dir_all(std::path::Path::new(p).parent().unwrap());
                            let _ = std::fs::write(p, body);
                        }
                        SubagentResult {
                            success: true,
                            output: StdArc::from("Done"),
                            subagent_id: req.id.clone(),
                            child_session_id: req.id.clone(),
                            ..Default::default()
                        }
                    }
                    SpawnBehaviour::WaitForCancelsThenWrite {
                        cancels,
                        started,
                        objectives,
                        body,
                    } => {
                        objectives.lock().unwrap().push(req.prompt.clone());
                        let spawn = count_task.load(SeqOrd::SeqCst);
                        let _ = started.send(spawn);
                        if spawn <= *cancels {
                            req.cancel_token.cancelled().await;
                            SubagentResult {
                                success: false,
                                error: Some("cancelled".into()),
                                cancelled: true,
                                subagent_id: req.id.clone(),
                                child_session_id: req.id.clone(),
                                ..Default::default()
                            }
                        } else {
                            if let Some(p) = plan_path.as_deref() {
                                let _ = std::fs::create_dir_all(
                                    std::path::Path::new(p).parent().unwrap(),
                                );
                                let _ = std::fs::write(p, body);
                            }
                            SubagentResult {
                                success: true,
                                output: StdArc::from("Done"),
                                subagent_id: req.id.clone(),
                                child_session_id: req.id.clone(),
                                ..Default::default()
                            }
                        }
                    }
                    SpawnBehaviour::NoWriteThenDone => SubagentResult {
                        success: true,
                        output: StdArc::from("Done"),
                        subagent_id: req.id.clone(),
                        child_session_id: req.id.clone(),
                        ..Default::default()
                    },
                    SpawnBehaviour::Runtime { message, cancelled } => SubagentResult {
                        success: false,
                        error: Some(message.clone()),
                        cancelled: *cancelled,
                        subagent_id: req.id.clone(),
                        child_session_id: req.id.clone(),
                        ..Default::default()
                    },
                };
                let _ = req.result_tx.send(result);
            }
        }
    });
    (tx, spawn_count, capture)
}

/// Build a `SessionActor` with goal harness enabled, planner
/// enabled, the supplied coordinator, a unique tempdir session
/// dir (so each test's `plan_path()` is isolated), and **no
/// active goal yet** — the caller drives `setup_goal` or
/// `create_goal` directly.
async fn make_planner_actor(
    coordinator_tx: Option<tokio::sync::mpsc::UnboundedSender<SubagentEvent>>,
    planner_enabled: bool,
) -> (StdArc<SessionActor>, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let (gateway_tx, _gateway_rx) =
        tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
    let (persistence_tx, _persistence_rx) =
        tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    actor.events = crate::session::events::EventTracker::new(tmp.path());
    actor.goal_enabled = true;
    set_goal_harness_for_tests(&actor);
    actor.goal_planner_enabled = planner_enabled;
    actor.goal_tracker = Arc::new(parking_lot::Mutex::new(
        crate::session::goal_tracker::GoalTracker::new(tmp.path().to_path_buf()),
    ));
    if let Some(tx) = coordinator_tx {
        actor.tool_context.subagent_event_tx = Some(tx);
    }
    (StdArc::new(actor), tmp)
}

/// Like [`make_planner_actor`] but retains the persistence receiver
/// so a test can inspect the `GoalUpdated` notifications the planner
/// run emits (used to assert the wire-only `planning` flag is set
/// then cleared).
async fn make_planner_actor_capturing(
    coordinator_tx: Option<tokio::sync::mpsc::UnboundedSender<SubagentEvent>>,
    planner_enabled: bool,
) -> (
    StdArc<SessionActor>,
    TempDir,
    tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>,
) {
    let tmp = TempDir::new().expect("tempdir");
    let (gateway_tx, _gateway_rx) =
        tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
    let (persistence_tx, persistence_rx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
    let mut actor = create_test_actor(0, 256_000, 85, gateway_tx, persistence_tx).await;
    actor.events = crate::session::events::EventTracker::new(tmp.path());
    actor.goal_enabled = true;
    set_goal_harness_for_tests(&actor);
    actor.goal_planner_enabled = planner_enabled;
    actor.goal_tracker = Arc::new(parking_lot::Mutex::new(
        crate::session::goal_tracker::GoalTracker::new(tmp.path().to_path_buf()),
    ));
    if let Some(tx) = coordinator_tx {
        actor.tool_context.subagent_event_tx = Some(tx);
    }
    (StdArc::new(actor), tmp, persistence_rx)
}

/// Drain every persisted `GoalUpdated` notification and project to
/// its wire-only `planning` flag, preserving emission order.
fn drain_goal_planning_flags(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<PersistenceMsg>,
) -> Vec<Option<bool>> {
    let mut out = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        if let PersistenceMsg::Update(crate::session::storage::SessionUpdate::EchoAgent(n)) = msg
            && let crate::extensions::notification::SessionUpdate::GoalUpdated { planning, .. } =
                n.update
        {
            out.push(planning);
        }
    }
    out
}

fn create_test_goal(actor: &SessionActor) {
    actor.goal_tracker.lock().create_goal(
        "g-test".into(),
        "test objective".into(),
        None,
        0,
        "2026-01-01T00:00:00Z".into(),
        None,
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn send_now_restarts_planner_with_all_steering() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
            let objectives = StdArc::new(std::sync::Mutex::new(Vec::new()));
            let (tx, spawn_count) =
                spawn_planner_coordinator(SpawnBehaviour::WaitForCancelsThenWrite {
                    cancels: 2,
                    started: started_tx,
                    objectives: StdArc::clone(&objectives),
                    body: b"# Plan\n",
                });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(user_item("goal-running", "A"));
                state.running_task = Some(running_task_stub("goal-running"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("goal-running".into());

            let planner = {
                let actor = StdArc::clone(&actor);
                tokio::task::spawn_local(async move { actor.setup_goal("do X", None).await })
            };

            for (spawn, text) in [(1, "first"), (2, "second")] {
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(5), started_rx.recv())
                        .await
                        .expect("planner spawn"),
                    Some(spawn),
                );
                let (respond_to, response_rx) = tokio::sync::oneshot::channel();
                assert!(
                    !actor
                        .queue_input(QueueInputRequest {
                            send_now: true,
                            ..queue_input_request(
                                vec![acp::ContentBlock::Text(acp::TextContent::new(text))],
                                &format!("user-steer-{spawn}"),
                                respond_to,
                            )
                        })
                        .await
                );
                assert!(matches!(
                    response_rx.await.unwrap().unwrap().completion_kind,
                    PromptCompletionKind::RemovedFromQueue
                ));
            }

            tokio::time::timeout(std::time::Duration::from_secs(5), planner)
                .await
                .expect("planner completion")
                .unwrap();
            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 3);
            let prompts = objectives.lock().unwrap();
            let objectives = prompts
                .iter()
                .map(|prompt| {
                    prompt
                        .split_once("\n\nOBJECTIVE:\n")
                        .unwrap()
                        .1
                        .split_once("\n\nCONTEXT:\n")
                        .unwrap()
                        .0
                })
                .collect::<Vec<_>>();
            assert_eq!(
                objectives,
                [
                    "do X",
                    "do X\n\nUser steering:\nfirst",
                    "do X\n\nUser steering:\nfirst\n\nsecond",
                ]
            );

            // Staged files (`plan-<uuid>.md`, `plan-baseline-<uuid>.md`) are
            // `TempPath`s: interrupted attempts drop theirs and the winner
            // renames onto `plan.md`, so nothing matching `plan-*.md` survives.
            let goal_dir = actor.goal_tracker.lock().plan_path();
            let goal_dir = goal_dir.parent().expect("plan path has a parent");
            let leaked: Vec<String> = std::fs::read_dir(goal_dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| name.starts_with("plan-") && name.ends_with(".md"))
                .collect();
            assert!(leaked.is_empty(), "staged planner files leaked: {leaked:?}");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_early_exit_clears_planning_latch() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _tmp) = make_planner_actor(None, true).await;
            create_test_goal(&actor);
            {
                let mut tracker = actor.goal_tracker.lock();
                tracker.snapshot_mut().unwrap().planning_in_flight = true;
                assert!(tracker.pause(crate::session::goal_tracker::GoalPauseReason::User));
            }

            actor.maybe_run_goal_planner("do X").await;

            assert!(
                !actor
                    .goal_tracker
                    .lock()
                    .snapshot()
                    .unwrap()
                    .planning_in_flight
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_success_stamps_plan_file_on_orchestration() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, spawn_count) =
                spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone { body: b"# Plan\n" });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);
            let plan_path = actor.goal_tracker.lock().plan_path();
            let baseline_path = actor.goal_tracker.lock().plan_baseline_path();
            std::fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
            std::fs::write(&plan_path, "stale plan").unwrap();
            std::fs::write(&baseline_path, "stale baseline").unwrap();

            actor.maybe_run_goal_planner("do X").await;

            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 1);
            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert_eq!(snap.plan_file.as_deref(), Some(plan_path.as_path()));
            assert_eq!(
                snap.plan_baseline_file.as_deref(),
                Some(baseline_path.as_path())
            );
            assert_eq!(std::fs::read_to_string(plan_path).unwrap(), "# Plan\n");
            assert_eq!(std::fs::read_to_string(baseline_path).unwrap(), "# Plan\n");
            assert_eq!(
                snap.status,
                crate::session::goal_tracker::GoalStatus::Active,
                "success must NOT pause the goal",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_spawn_sets_harness_only_fork_context() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, spawn_count, capture) =
                spawn_planner_coordinator_capturing(SpawnBehaviour::WritePlanThenDone {
                    body: b"# Plan\n",
                });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 1);
            let forks = capture.fork_context.lock().unwrap().clone();
            let surfaces = capture.surface_completion.lock().unwrap().clone();
            assert_eq!(forks, vec![true], "planner must request chat-prefix fork");
            assert_eq!(
                surfaces,
                vec![false],
                "planner remains harness-internal (no idle reminder)"
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_fork_inherits_parent_model() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, spawn_count, capture) =
                spawn_planner_coordinator_capturing(SpawnBehaviour::WritePlanThenDone {
                    body: b"# Plan\n",
                });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            // Configure an EXPLICIT planner role model different from the parent.
            // The mirror-child fork must IGNORE it and inherit the parent model,
            // since the radix prefix is per-model. Without the Step-7 forcing this
            // configured model would flow through and the assertion below would
            // catch the regression.
            let actor = StdArc::new(SessionActor {
                status_wake: Default::default(),
                goal_role_models: crate::session::GoalRoleModelConfig {
                    planner: crate::agent::config::GoalRoleModelChoice::Explicit(
                        crate::util::config::GoalRoleModel {
                            model: "some-other-planner-model".to_string(),
                            agent_type: "general-purpose".to_string(),
                        },
                    ),
                    ..Default::default()
                },
                ..StdArc::try_unwrap(actor).ok().expect("single-owner actor")
            });
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 1);
            let models = capture.model.lock().unwrap().clone();
            assert_eq!(
                models,
                vec![None],
                "planner fork must ignore the configured role model and inherit the parent (None)"
            );
        })
        .await;
}

/// The planner's ORIGINAL plan is snapshotted to `plan.baseline.md` once,
/// right after the plan is written, and is NOT re-synced to later edits.
/// A second `maybe_run_goal_planner` (early-returns: a plan already
/// exists) must leave the baseline pinned to the original body even after
/// `plan.md` itself is edited on disk.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_snapshots_plan_baseline_once_and_does_not_overwrite() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, _spawn_count) = spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone {
                body: b"# Plan v1\n",
            });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);
            let baseline_path = actor.goal_tracker.lock().plan_baseline_path();

            actor.maybe_run_goal_planner("do X").await;

            // Baseline recorded on the orchestration and written to disk with
            // the planner's original body.
            let recorded = actor
                .goal_tracker
                .lock()
                .snapshot()
                .unwrap()
                .plan_baseline_file
                .clone();
            assert_eq!(
                recorded.as_deref(),
                Some(baseline_path.as_path()),
                "plan_baseline_file must point at plan.baseline.md",
            );
            assert_eq!(
                std::fs::read_to_string(&baseline_path).unwrap(),
                "# Plan v1\n",
                "baseline must hold the planner's ORIGINAL plan body",
            );

            // The agent edits plan.md mid-run; a second planner invocation
            // early-returns (plan already present) and must NOT re-snapshot.
            let plan_path = actor.goal_tracker.lock().plan_path();
            std::fs::write(&plan_path, "# Plan v2 (agent edited)\n").unwrap();
            actor.maybe_run_goal_planner("do X").await;

            assert_eq!(
                std::fs::read_to_string(&baseline_path).unwrap(),
                "# Plan v1\n",
                "baseline must remain the ORIGINAL plan, never overwritten",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_records_own_harness_trace_turn_with_footer() {
    // Option B: the planner subagent is represented by its OWN trace turn.
    // After `maybe_run_goal_planner`, the chat-state side buffer holds exactly
    // one sealed harness trace turn carrying the synthetic `task` call + result
    // pair, and the result keeps the `<subagent_result>` footer (with the child
    // session id) so the trace viewer can discover the planner subagent.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, _spawn_count) =
                spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone { body: b"# Plan\n" });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            let turns = actor.chat_state_handle.take_harness_trace_turns().await;
            assert_eq!(turns.len(), 1, "planner rides its own trace turn");
            let items = &turns[0];
            assert_eq!(items.len(), 2, "synthetic task call + result pair");
            assert!(
                matches!(
                    &items[0],
                    crate::sampling::ConversationItem::Assistant(a) if !a.tool_calls.is_empty()
                ),
                "first item is the synthetic task call",
            );
            let result_text = items[1].text_content();
            assert!(
                result_text.contains("<subagent_result>"),
                "footer present for trace-viewer / subagent discovery: {result_text}",
            );
            assert!(
                result_text.contains("subagent_id:"),
                "subagent_id present in footer: {result_text}",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_disabled_records_no_harness_trace_turn() {
    // Non-goal / planner-off fast path: no harness trace turn is produced.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _tmp) = make_planner_actor(None, false).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            let turns = actor.chat_state_handle.take_harness_trace_turns().await;
            assert!(turns.is_empty(), "planner disabled — no trace turn");
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_success_sets_then_clears_planning_flag() {
    // The transient "planning…" badge fires before the subagent runs
    // (planning=Some(true)) and is cleared on the success exit path
    // by a snapshot-derived GoalUpdated (planning=None).
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, _spawn_count) =
                spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone { body: b"# Plan\n" });
            let (actor, _tmp, mut persistence_rx) =
                make_planner_actor_capturing(Some(tx), true).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            let flags = drain_goal_planning_flags(&mut persistence_rx);
            assert_eq!(
                flags.first(),
                Some(&Some(true)),
                "planner must emit planning=true first; got {flags:?}",
            );
            assert_eq!(
                flags.last(),
                Some(&None),
                "success path must clear the planning badge; got {flags:?}",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_clears_planning_latch_before_publishing_the_plan() {
    // Regression (Bugbot: "Late steering skips replan"): the "planning…" badge
    // must be cleared at the commit-to-publish point — the moment the planner
    // run is taken and we commit to publishing the produced plan — NOT only at
    // the very end after the plan/baseline I/O. Before the fix the latch stayed
    // set through the whole publish window, so the pager advertised "planning"
    // while steering could no longer replan (it no-ops with no active run and
    // is merged only as an interjection).
    //
    // Observed deterministically: the first instant `plan.md` is published on
    // disk (renamed from the staged attempt file, right before the baseline
    // copy `.await` yields), latch the goal's `planning_in_flight`. The fix
    // clears it before that publish; before the fix it was still set.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, _spawn_count) =
                spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone { body: b"# Plan\n" });
            let (actor, _tmp, mut persistence_rx) =
                make_planner_actor_capturing(Some(tx), true).await;
            create_test_goal(&actor);
            let plan_path = actor.goal_tracker.lock().plan_path();

            let observed_at_publish = StdArc::new(std::sync::Mutex::new(None::<bool>));
            let observer = {
                let actor = StdArc::clone(&actor);
                let plan_path = plan_path.clone();
                let observed = StdArc::clone(&observed_at_publish);
                tokio::task::spawn_local(async move {
                    loop {
                        if plan_path.exists() {
                            *observed.lock().unwrap() = actor
                                .goal_tracker
                                .lock()
                                .snapshot()
                                .map(|g| g.planning_in_flight);
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
            };

            actor.maybe_run_goal_planner("do X").await;
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), observer).await;

            assert_eq!(
                *observed_at_publish.lock().unwrap(),
                Some(false),
                "planning_in_flight must already be cleared by the time the plan is published \
                 (badge must not linger through the publish window)",
            );
            // End state: plan published, latch cleared, and the emitted flag
            // sequence is exactly the transient badge on then off — no duplicate
            // `planning=None` from the earlier clear plus the final catch-all.
            assert!(
                actor
                    .goal_tracker
                    .lock()
                    .snapshot()
                    .unwrap()
                    .plan_file
                    .is_some()
            );
            let flags = drain_goal_planning_flags(&mut persistence_rx);
            assert_eq!(
                flags,
                vec![Some(true), None],
                "expected badge on then a single clear; got {flags:?}",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_fail_closed_clears_planning_flag() {
    // Reset-on-all-paths guard: even when the planner fails closed
    // (goal paused), the last GoalUpdated must clear planning so the
    // "planning…" badge can never stick on screen.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, _spawn_count) = spawn_planner_coordinator(SpawnBehaviour::Runtime {
                message: "model rejected".into(),
                cancelled: false,
            });
            let (actor, _tmp, mut persistence_rx) =
                make_planner_actor_capturing(Some(tx), true).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            assert!(actor.goal_tracker.lock().status().unwrap().is_paused());
            let flags = drain_goal_planning_flags(&mut persistence_rx);
            assert_eq!(
                flags.first(),
                Some(&Some(true)),
                "planner must emit planning=true first; got {flags:?}",
            );
            assert_eq!(
                flags.last(),
                Some(&None),
                "fail-closed path must clear the planning badge; got {flags:?}",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planning_badge_survives_intervening_goal_update() {
    // Regression: the subagent-spawn / token-accounting `GoalUpdated`
    // that fires while the planner runs must NOT clear the "planning…"
    // badge. The latch keeps every snapshot-derived update carrying it.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _tmp, mut persistence_rx) = make_planner_actor_capturing(None, true).await;
            create_test_goal(&actor);

            actor.emit_goal_planning(0);
            // Intervening snapshot-derived emit (mirrors the spawn path).
            let (tokens_used, finished_marginal) = actor.goal_tokens(0);
            actor.goal_notify_sender().emit_goal_updated(
                &mut actor.goal_tracker.lock(),
                tokens_used,
                finished_marginal,
            );

            let flags = drain_goal_planning_flags(&mut persistence_rx);
            assert_eq!(flags.len(), 2, "expected both emits; got {flags:?}");
            assert!(
                flags.iter().all(|f| *f == Some(true)),
                "badge must persist across intervening updates; got {flags:?}",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_runtime_failure_pauses_goal_with_canonical_message() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, spawn_count) = spawn_planner_coordinator(SpawnBehaviour::Runtime {
                message: "model rejected".into(),
                cancelled: false,
            });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 1);
            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert!(snap.plan_file.is_none(), "no plan written");
            assert!(
                snap.status.is_paused(),
                "fail-closed must pause; got {:?}",
                snap.status,
            );
            assert_eq!(
                snap.pause_message.as_deref(),
                Some(planner_failure_pause_message().as_str()),
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_missing_plan_file_pauses_goal() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, _spawn_count) = spawn_planner_coordinator(SpawnBehaviour::NoWriteThenDone);
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert!(snap.plan_file.is_none());
            assert!(snap.status.is_paused());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_disabled_short_circuits_no_spawn_no_attempt() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, spawn_count) =
                spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone { body: b"x" });
            let (actor, _tmp) = make_planner_actor(Some(tx), false).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 0);
            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert!(snap.plan_file.is_none());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_no_coordinator_skips_silently() {
    // External harness path: planner enabled but no subagent_event_tx.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _tmp) = make_planner_actor(None, true).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;

            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert!(snap.plan_file.is_none());
            assert_eq!(
                snap.status,
                crate::session::goal_tracker::GoalStatus::Active
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_existing_plan_does_not_re_fire() {
    // Defensive: if `plan_file` is already populated (e.g.
    // future re-trigger path that calls the helper twice), we
    // do NOT re-spawn.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, spawn_count) =
                spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone { body: b"x" });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);
            {
                let mut tracker = actor.goal_tracker.lock();
                tracker.snapshot_mut().unwrap().plan_file =
                    Some(std::path::PathBuf::from("/tmp/preexisting/plan.md"));
            }

            actor.maybe_run_goal_planner("do X").await;

            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 0);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn reconcile_pauses_active_goal_with_no_plan() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _tmp) = make_planner_actor(None, true).await;
            create_test_goal(&actor);
            assert!(
                actor
                    .goal_tracker
                    .lock()
                    .snapshot()
                    .unwrap()
                    .plan_file
                    .is_none()
            );

            actor.maybe_reconcile_active_goal_without_plan().await;

            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert!(snap.status.is_paused());
            assert_eq!(
                snap.pause_message.as_deref(),
                Some(planner_failure_pause_message().as_str()),
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn reconcile_skips_active_goal_with_plan() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _tmp) = make_planner_actor(None, true).await;
            create_test_goal(&actor);
            {
                let mut tracker = actor.goal_tracker.lock();
                tracker.snapshot_mut().unwrap().plan_file =
                    Some(std::path::PathBuf::from("/tmp/has-plan/plan.md"));
            }

            actor.maybe_reconcile_active_goal_without_plan().await;

            assert_eq!(
                actor.goal_tracker.lock().status(),
                Some(crate::session::goal_tracker::GoalStatus::Active),
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn reconcile_is_idempotent_via_atomic_flag() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _tmp) = make_planner_actor(None, true).await;
            create_test_goal(&actor);
            actor.maybe_reconcile_active_goal_without_plan().await;
            assert!(actor.goal_tracker.lock().status().unwrap().is_paused());

            // Re-activate the tracker directly and re-run. The
            // atomic short-circuit must skip the work — a regression
            // that removed the swap would re-pause the goal.
            actor.goal_tracker.lock().resume();
            assert_eq!(
                actor.goal_tracker.lock().status(),
                Some(crate::session::goal_tracker::GoalStatus::Active),
            );
            actor.maybe_reconcile_active_goal_without_plan().await;
            assert_eq!(
                actor.goal_tracker.lock().status(),
                Some(crate::session::goal_tracker::GoalStatus::Active),
                "atomic short-circuit must prevent re-pause; reconciler ran twice",
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
#[serial]
async fn reconcile_skips_when_planner_disabled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _tmp) = make_planner_actor(None, false).await;
            create_test_goal(&actor);

            actor.maybe_reconcile_active_goal_without_plan().await;

            assert_eq!(
                actor.goal_tracker.lock().status(),
                Some(crate::session::goal_tracker::GoalStatus::Active),
            );
        })
        .await;
}

/// Planner subagent tokens fold into the goal's total via the
/// shared `subagent_token_records` map. The planner spawn routes
/// through the same `SubagentSpawned` notification path the
/// classifier uses; the actor's notification handler tags the
/// record with the active goal_id and `goal_tokens()` sums every
/// matching record. This test simulates that tagging (the real
/// handler isn't reachable from this scaffold without standing up
/// a full notification pipeline) and pins the structural
/// invariant: a planner-tagged subagent record under the active
/// goal_id flows into the chip's `tokens_used`.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn planner_subagent_tokens_fold_into_goal_total() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, spawn_count) =
                spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone { body: b"# Plan\n" });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);
            let goal_id = actor
                .goal_tracker
                .lock()
                .snapshot()
                .unwrap()
                .goal_id
                .clone();

            actor.maybe_run_goal_planner("do X").await;
            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 1);

            // Simulate the SubagentSpawned notification handler:
            // the planner's spawn produced 12_000 cumulative tokens
            // (anchor=0 → last=12_000). The handler tags the record
            // with the live goal_id. While the planner is still running
            // the record is unsealed (`finished: false`).
            actor.subagent_token_records.lock().insert(
                "planner-subagent-id".to_string(),
                SubagentTokenRecord {
                    goal_id: Some(goal_id),
                    resume_anchor_cumulative: 0,
                    last_cumulative_reported: 12_000,
                    model: None,
                    finished: false,
                },
            );

            // In-flight: the marginal folds into the ratcheted `tokens_used`,
            // but NOT into the wire `finished_subagent_tokens`. The pager adds
            // its own live active-subagent sum on top of that field, so an
            // unsealed subagent counted here too would be double-counted.
            let (tokens_used, finished_marginal) = actor.goal_tokens(0);
            assert_eq!(
                finished_marginal, 0,
                "an in-flight subagent must be excluded from `finished_marginal`",
            );
            assert!(
                tokens_used >= 12_000,
                "goal chip tokens_used must include planner marginal (got {tokens_used})",
            );
            // Pager-equivalent live combine while the subagent runs:
            // parent_delta (0 here) + finished_subagent_tokens + the pager's
            // own active-subagent sum. It must never exceed the shell's total.
            let active_subagent_tokens = 12_000i64;
            assert!(
                finished_marginal.saturating_add(active_subagent_tokens) <= tokens_used,
                "pager live combine must not exceed goal_tokens total while a subagent runs",
            );

            // Once the planner finishes, `SubagentFinished` seals the record;
            // the same marginal now lands in `finished_subagent_tokens`.
            actor
                .subagent_token_records
                .lock()
                .get_mut("planner-subagent-id")
                .expect("planner record present")
                .finished = true;
            let (tokens_used, finished_marginal) = actor.goal_tokens(0);
            assert_eq!(
                finished_marginal, 12_000,
                "a sealed planner subagent marginal must reach `finished_marginal`",
            );
            assert!(tokens_used >= 12_000);
        })
        .await;
}

/// Lifecycle regression guard: planner fails → goal paused with
/// canonical message → `GoalResume` re-fires the planner → second
/// attempt succeeds → goal Active with `plan_file` set.
/// Pins the retry-on-resume contract — without it, the pause
/// message ("resume with /goal to retry") would be a lie.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn lifecycle_fail_pause_resume_retry_success() {
    use crate::session::goal_tracker::GoalStatus;
    use std::sync::Mutex;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<SubagentEvent>();
            let spawn_count = StdArc::new(AtomicUsize::new(0));
            let count_task = StdArc::clone(&spawn_count);
            let plan_targets: StdArc<Mutex<Vec<String>>> = StdArc::new(Mutex::new(Vec::new()));
            let targets_task = StdArc::clone(&plan_targets);
            tokio::task::spawn_local(async move {
                while let Some(ev) = rx.recv().await {
                    if let SubagentEvent::Spawn(req) = ev {
                        let n = count_task.fetch_add(1, SeqOrd::SeqCst);
                        let plan_path = plan_path_from_prompt(&req.prompt);
                        if let Some(ref p) = plan_path {
                            targets_task.lock().unwrap().push(p.clone());
                        }
                        let result = if n == 0 {
                            SubagentResult {
                                success: false,
                                error: Some("planner failed".into()),
                                cancelled: false,
                                subagent_id: req.id.clone(),
                                child_session_id: req.id.clone(),
                                ..Default::default()
                            }
                        } else {
                            if let Some(p) = plan_path.as_deref() {
                                let _ = std::fs::create_dir_all(
                                    std::path::Path::new(p).parent().unwrap(),
                                );
                                let _ = std::fs::write(p, b"# Plan\n");
                            }
                            SubagentResult {
                                success: true,
                                output: StdArc::from("Done"),
                                subagent_id: req.id.clone(),
                                child_session_id: req.id.clone(),
                                ..Default::default()
                            }
                        };
                        let _ = req.result_tx.send(result);
                    }
                }
            });

            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;
            {
                let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
                assert!(snap.plan_file.is_none());
                assert!(snap.status.is_paused(), "got {:?}", snap.status);
                assert_eq!(
                    snap.pause_message.as_deref(),
                    Some(planner_failure_pause_message().as_str()),
                );
            }
            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 1);

            let _ = actor.resume_goal().await;

            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert_eq!(
                spawn_count.load(SeqOrd::SeqCst),
                2,
                "retry must re-fire planner"
            );
            assert_eq!(snap.status, GoalStatus::Active);
            assert!(
                snap.plan_file.is_some(),
                "successful retry writes plan_file"
            );
            let plan_path = actor.goal_tracker.lock().plan_path();
            assert_eq!(snap.plan_file.as_deref(), Some(plan_path.as_path()));
            let targets = plan_targets.lock().unwrap();
            assert_eq!(targets.len(), 2);
            assert_ne!(
                targets[0], targets[1],
                "each attempt must use an isolated plan path"
            );
            assert!(targets.iter().all(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("plan-"))
            }));
        })
        .await;
}

/// Repeated-failure variant: planner fails → resume → planner
/// fails again → goal re-paused with the canonical message. Pins
/// that the retry path uses the same fail-closed semantics.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn lifecycle_fail_pause_resume_retry_fail_repauses() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            // Coordinator that always fails.
            let (tx, spawn_count) = spawn_planner_coordinator(SpawnBehaviour::Runtime {
                message: "still broken".into(),
                cancelled: false,
            });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);

            actor.maybe_run_goal_planner("do X").await;
            assert!(actor.goal_tracker.lock().status().unwrap().is_paused());

            let outcome = actor.resume_goal().await;
            // Planner re-failed → goal re-paused → resume must end the turn
            // (Message), not flow through to inference on a paused goal.
            assert!(
                matches!(outcome, GoalResumeOutcome::Message(_)),
                "re-paused resume must end the turn, not run inference",
            );

            assert_eq!(spawn_count.load(SeqOrd::SeqCst), 2);
            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert!(snap.plan_file.is_none());
            assert!(snap.status.is_paused(), "got {:?}", snap.status);
            assert_eq!(
                snap.pause_message.as_deref(),
                Some(planner_failure_pause_message().as_str()),
            );
        })
        .await;
}

/// Resume of a goal that already has a `plan_file` must NOT
/// re-fire the planner (defensive — the retry path keys off
/// `plan_file.is_none()`).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn lifecycle_resume_with_plan_does_not_re_fire_planner() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, spawn_count) =
                spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone { body: b"x" });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);
            {
                let mut tracker = actor.goal_tracker.lock();
                let snap = tracker.snapshot_mut().unwrap();
                snap.plan_file = Some(std::path::PathBuf::from("/tmp/has-plan/plan.md"));
            }
            // Pause the goal manually (planner-failure simulation).
            let _ = actor
                .auto_pause_goal_if_active_with_message(
                    crate::session::goal_tracker::GoalPauseReason::User,
                    "user pause".into(),
                )
                .await;

            let _ = actor.resume_goal().await;

            assert_eq!(
                spawn_count.load(SeqOrd::SeqCst),
                0,
                "resume with plan present must not spawn planner",
            );
            assert_eq!(
                actor.goal_tracker.lock().status(),
                Some(crate::session::goal_tracker::GoalStatus::Active),
            );
        })
        .await;
}

/// End-to-end gate (enabled side): when the planner is on and writes a
/// plan, `setup_goal`'s reminder folds in the plan-aware block carrying
/// the actual `plan_path()` pointer plus the seed-todos / `## Deviations`
/// / verifier-threading instructions, with the legacy discipline intact.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn setup_goal_reminder_is_plan_aware_when_planner_enabled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, _c) = spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone {
                body: b"# Plan\n\n1. do it\n",
            });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            let plan_path = actor.goal_tracker.lock().plan_path();

            let reminder = actor.setup_goal("ship it", None).await;

            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert_eq!(snap.plan_file.as_deref(), Some(plan_path.as_path()));
            let expected = format!("\nPlan: {}\n", plan_path.display());
            assert!(
                reminder.contains(&expected),
                "reminder must carry the plan pointer line `{expected}`:\n{reminder}"
            );
            assert!(
                reminder.contains(PLAN_SEED_TODOS_PHRASE),
                "reminder must seed todos from the plan:\n{reminder}"
            );
            assert!(
                reminder.contains("append a bullet to the plan's single"),
                "reminder must instruct the deviation amendment:\n{reminder}"
            );
            assert!(
                reminder.contains("<task_completion_discipline>")
                    && reminder.contains("TEST PROACTIVELY:"),
                "discipline + TEST PROACTIVELY must survive in the consolidated block:\n{reminder}"
            );
        })
        .await;
}

/// End-to-end gate (disabled side, the default today): with the planner
/// off, `setup_goal` writes no plan and the reminder renders the
/// no-plan block — no dangling `Plan:` pointer, no plan-aware phrasing —
/// while the discipline + slim TRACKING/TEST sections remain.
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn setup_goal_reminder_is_no_plan_when_planner_disabled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _tmp) = make_planner_actor(None, false).await;

            let reminder = actor.setup_goal("ship it", None).await;

            let snap = actor.goal_tracker.lock().snapshot().cloned().unwrap();
            assert!(snap.plan_file.is_none(), "planner off writes no plan");
            assert!(
                !reminder.contains("\nPlan: "),
                "no-plan reminder must not carry a `Plan:` pointer:\n{reminder}"
            );
            assert!(
                !reminder.contains(PLAN_SEED_TODOS_PHRASE),
                "no-plan reminder must omit plan-aware seeding:\n{reminder}"
            );
            assert!(
                reminder.contains("<task_completion_discipline>")
                    && reminder.contains("TEST PROACTIVELY:"),
                "discipline + TEST PROACTIVELY must remain:\n{reminder}"
            );
        })
        .await;
}

/// The SECOND gated render site. `/goal resume` on a
/// planner-enabled goal with a plan must build a plan-aware reminder
/// (carrying the real `plan_path()` pointer). Guards against the resume
/// site regressing to `None` while setup_goal stays correct (a prior
/// regression: the sibling branch was untested). The reminder is returned as the
/// `Inference` turn content (resume flows through to inference now).
#[tokio::test(flavor = "current_thread")]
#[serial]
async fn goal_resume_reminder_is_plan_aware_when_planner_enabled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (tx, _c) =
                spawn_planner_coordinator(SpawnBehaviour::WritePlanThenDone { body: b"x" });
            let (actor, _tmp) = make_planner_actor(Some(tx), true).await;
            create_test_goal(&actor);
            let plan_path = actor.goal_tracker.lock().plan_path();
            {
                let mut tracker = actor.goal_tracker.lock();
                tracker.snapshot_mut().unwrap().plan_file = Some(plan_path.clone());
            }
            let _ = actor
                .auto_pause_goal_if_active_with_message(
                    crate::session::goal_tracker::GoalPauseReason::User,
                    "user pause".into(),
                )
                .await;

            let GoalResumeOutcome::Inference { reminder, .. } = actor.resume_goal().await else {
                panic!("resumed paused goal must flow through to inference");
            };

            assert!(
                reminder.contains("Continue working now."),
                "resume reminder must close with the continuation directive:\n{reminder}"
            );
            let expected = format!("\nPlan: {}\n", plan_path.display());
            assert!(
                reminder.contains(&expected),
                "resume reminder must carry the plan pointer `{expected}`:\n{reminder}"
            );
            assert!(
                reminder.contains(PLAN_SEED_TODOS_PHRASE),
                "resume reminder must be plan-aware:\n{reminder}"
            );
        })
        .await;
}
