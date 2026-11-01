//! Server-authoritative prompt queue.
use super::support::*;
use super::*;

/// The shared-queue text must use a block's compact `displayText` (e.g. a
/// locally-expanded `/loop` invocation) rather than the raw expanded wire text,
/// so other clients' turn-start shim renders the compact user block — not the
/// full skill instruction.
#[test]
fn queue_text_prefers_display_text_over_raw_wire_text() {
    let block = acp::ContentBlock::Text(
        acp::TextContent::new(
            "# /loop -- schedule a recurring prompt\n\nParse the input ...".to_string(),
        )
        .meta(
            serde_json::json!({ "displayText": "/loop 5s echo \"hello\"" })
                .as_object()
                .cloned(),
        ),
    );
    assert_eq!(
        SessionActor::queue_text_from_blocks(&[block]),
        "/loop 5s echo \"hello\""
    );
}

#[test]
fn queue_text_falls_back_to_raw_text_without_display_text() {
    let block = acp::ContentBlock::Text(acp::TextContent::new("just a normal prompt".to_string()));
    assert_eq!(
        SessionActor::queue_text_from_blocks(&[block]),
        "just a normal prompt"
    );

    // An empty displayText is ignored — fall back to the raw text.
    let block_empty = acp::ContentBlock::Text(
        acp::TextContent::new("raw text".to_string()).meta(
            serde_json::json!({ "displayText": "   " })
                .as_object()
                .cloned(),
        ),
    );
    assert_eq!(
        SessionActor::queue_text_from_blocks(&[block_empty]),
        "raw text"
    );
}

fn ids(wire: &[crate::session::prompt_queue::QueueEntryWire]) -> Vec<String> {
    wire.iter().map(|e| e.id.clone()).collect()
}

fn protected_item(id: &str) -> InputItem {
    let mut item = user_item(id, "protected");
    item.queue_mutation_policy = QueueMutationPolicy::new(true, false);
    item
}

/// A queued user bash item (`!cmd`), mirroring `queue_input`'s derivation.
fn bash_item(id: &str, owner: &str, command: &str) -> InputItem {
    let mut item = user_item(id, owner);
    let meta = serde_json::to_value(crate::extensions::prompt_meta::PromptBlockMeta::bash(
        command,
    ))
    .unwrap()
    .as_object()
    .cloned();
    item.prompt_blocks = vec![acp::ContentBlock::Text(
        acp::TextContent::new(command.to_string()).meta(meta),
    )];
    let meta = item.queue_meta.as_mut().unwrap();
    meta.kind = "bash".to_string();
    meta.text = command.to_string();
    item
}

#[test]
fn combine_front_merges_consecutive_plain_prompts() {
    use crate::session::commands::{PromptCompletionKind, PromptTurnOk};

    let (p1, _) = user_item_with_rx("p1", "A");
    let (p2, rx2) = user_item_with_rx("p2", "A");
    let (p3, rx3) = user_item_with_rx("p3", "A");
    let mut pending = std::collections::VecDeque::from([p1, p2, p3]);

    SessionActor::combine_front_pending_inputs(&mut pending, &[]);

    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].prompt_id, "p1");
    let combined = "text for p1\n\ntext for p2\n\ntext for p3";
    assert_eq!(
        SessionActor::queue_text_from_blocks(&pending[0].prompt_blocks),
        combined
    );
    assert_eq!(
        pending[0].queue_meta.as_ref().map(|m| m.text.as_str()),
        Some(combined)
    );
    assert_eq!(
        pending[0]
            .queue_meta
            .as_ref()
            .and_then(|m| m.combined_texts.as_ref())
            .map(|v| v.as_slice()),
        Some(
            [
                "text for p1".to_string(),
                "text for p2".to_string(),
                "text for p3".to_string()
            ]
            .as_slice()
        )
    );
    // Content-block meta for echo/replay multi-bubble paint.
    let segs = pending[0]
        .prompt_blocks
        .first()
        .and_then(|b| match b {
            acp::ContentBlock::Text(t) => t.meta.as_ref(),
            _ => None,
        })
        .and_then(|m| m.get(crate::session::prompt_queue::COMBINED_DISPLAY_TEXTS_META))
        .and_then(|v| v.as_array())
        .expect("combinedDisplayTexts stamped");
    assert_eq!(
        segs.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
        ["text for p1", "text for p2", "text for p3"]
    );
    for mut rx in [rx2, rx3] {
        assert!(matches!(
            rx.try_recv(),
            Ok(Ok(PromptTurnOk {
                completion_kind: PromptCompletionKind::RemovedFromQueue,
                ..
            }))
        ));
    }
}

#[test]
fn combine_front_rejects_protected_rows() {
    for mut pending in [
        std::collections::VecDeque::from([protected_item("parent"), user_item("user", "A")]),
        std::collections::VecDeque::from([user_item("user", "A"), protected_item("parent")]),
    ] {
        SessionActor::combine_front_pending_inputs(&mut pending, &[]);
        assert_eq!(pending.len(), 2);
    }
}

#[test]
fn combine_front_stops_at_bash() {
    let mut pending = std::collections::VecDeque::from([
        user_item("p1", "A"),
        user_item("p2", "A"),
        bash_item("bash1", "A", "ls"),
        user_item("p3", "A"),
    ]);

    SessionActor::combine_front_pending_inputs(&mut pending, &[]);

    assert_eq!(pending.len(), 3);
    assert_eq!(
        SessionActor::queue_text_from_blocks(&pending[0].prompt_blocks),
        "text for p1\n\ntext for p2"
    );
    assert_eq!(pending[1].prompt_id, "bash1");
    assert_eq!(pending[2].prompt_id, "p3");
}

#[test]
fn combine_front_noop_when_ineligible() {
    let mut single = std::collections::VecDeque::from([user_item("only", "A")]);
    SessionActor::combine_front_pending_inputs(&mut single, &[]);
    assert_eq!(single.len(), 1);

    let mut bash_front =
        std::collections::VecDeque::from([bash_item("b", "A", "pwd"), user_item("p", "A")]);
    SessionActor::combine_front_pending_inputs(&mut bash_front, &[]);
    assert_eq!(bash_front.len(), 2);
    assert_eq!(bash_front[0].prompt_id, "b");
}

#[test]
fn combine_front_skips_client_expanded_skill() {
    let mut skill = user_item("skill", "A");
    skill.prompt_blocks = vec![acp::ContentBlock::Text(
        acp::TextContent::new("# expanded skill body".to_string()).meta(
            serde_json::json!({ "displayText": "/commit fix" })
                .as_object()
                .cloned(),
        ),
    )];
    let mut pending = std::collections::VecDeque::from([skill, user_item("follow", "A")]);

    SessionActor::combine_front_pending_inputs(&mut pending, &[]);

    assert_eq!(
        pending.len(),
        2,
        "displayText front must not absorb followers"
    );
}

#[test]
fn combine_front_skips_edit_hold() {
    let (p1, _) = user_item_with_rx("p1", "A");
    let (p2, mut rx2) = user_item_with_rx("p2", "A");
    let (p3, _) = user_item_with_rx("p3", "A");
    let mut pending = std::collections::VecDeque::from([p1, p2, p3]);
    SessionActor::combine_front_pending_inputs(&mut pending, &["p2"]);
    assert_eq!(pending.len(), 3, "edit-hold follower must not be absorbed");
    assert!(rx2.try_recv().is_err(), "held row must stay queued");
}

fn x_search_cutoff_update() -> echo_agent_sampling_types::ToolOverridesUpdate {
    echo_agent_sampling_types::ToolOverridesUpdate {
        x_search: Some(Some(echo_agent_sampling_types::XSearchOptions {
            date_bound: Some(
                echo_agent_sampling_types::SearchDateBound::new(
                    None,
                    Some("2024-03-15".to_string()),
                )
                .unwrap(),
            ),
        })),
        web_search: None,
    }
}

#[test]
fn combine_front_stops_at_a_per_turn_override_follower() {
    // A follower carrying an override pins its own bound, so it stops the run and keeps its row.
    let (p1, _) = user_item_with_rx("p1", "A");
    let (mut p2, mut rx2) = user_item_with_rx("p2", "A");
    p2.tool_overrides_update = Some(x_search_cutoff_update());
    let (p3, _) = user_item_with_rx("p3", "A");
    let mut pending = std::collections::VecDeque::from([p1, p2, p3]);

    SessionActor::combine_front_pending_inputs(&mut pending, &[]);

    assert_eq!(
        pending.len(),
        3,
        "an override-bearing follower must not be absorbed"
    );
    assert_eq!(pending[0].prompt_id, "p1");
    assert_eq!(pending[1].prompt_id, "p2");
    assert_eq!(pending[2].prompt_id, "p3");
    assert!(
        rx2.try_recv().is_err(),
        "the pinned follower must stay queued"
    );
}

#[test]
fn combine_front_noop_when_front_carries_a_per_turn_override() {
    // An override-bearing front pins its own bound, so it must run alone rather than absorb a
    // follower into its turn under that bound.
    let mut front = user_item("p1", "A");
    front.tool_overrides_update = Some(x_search_cutoff_update());
    let mut pending = std::collections::VecDeque::from([front, user_item("p2", "A")]);

    SessionActor::combine_front_pending_inputs(&mut pending, &[]);

    assert_eq!(
        pending.len(),
        2,
        "an override-bearing front must not absorb followers"
    );
    assert_eq!(pending[0].prompt_id, "p1");
    assert_eq!(pending[1].prompt_id, "p2");
}

/// Two prompts arrive (serialized by the actor mailbox → FIFO); the agent
/// drains the front; an edit against the already-drained item is a benign
/// no-op that re-broadcasts the current queue; a stale-version edit is also
/// a no-op; a correct-version remove empties the queue.
#[tokio::test]
async fn two_enqueues_drain_fifo_and_stale_edit_is_noop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx) = build_actor().await;

            // p1 then p2 (arrival order).
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "A"));
                actor.broadcast_queue_changed(&state);
                state.pending_inputs.push_back(user_item("p2", "B"));
                actor.broadcast_queue_changed(&state);
                assert_eq!(ids(&actor.build_queue_wire(&state)), vec!["p1", "p2"]);
            }

            // Agent drains the front (FIFO) — simulate turn-completion pop.
            {
                let mut state = actor.state.lock().await;
                let drained = state.pending_inputs.pop_front().unwrap();
                assert_eq!(drained.prompt_id, "p1");
                actor.broadcast_queue_changed(&state);
                assert_eq!(ids(&actor.build_queue_wire(&state)), vec!["p2"]);
            }

            // Edit against drained p1 → no-op + rebroadcast of [p2].
            actor.handle_remove_queued_prompt("p1", 0, None).await;
            {
                let state = actor.state.lock().await;
                assert_eq!(ids(&actor.build_queue_wire(&state)), vec!["p2"]);
            }

            // Stale-version edit against live p2 → no-op.
            actor.handle_remove_queued_prompt("p2", 99, None).await;
            {
                let state = actor.state.lock().await;
                assert_eq!(ids(&actor.build_queue_wire(&state)), vec!["p2"]);
            }

            // Correct-version remove → empties the queue.
            actor.handle_remove_queued_prompt("p2", 0, None).await;
            {
                let state = actor.state.lock().await;
                assert!(actor.build_queue_wire(&state).is_empty());
            }

            // The final broadcast must reflect the empty queue.
            let mut last: Option<crate::session::prompt_queue::QueueChanged> = None;
            while let Ok(msg) = gateway_rx.try_recv() {
                if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
                    && args.request.method.as_ref()
                        == crate::session::prompt_queue::QUEUE_CHANGED_METHOD
                {
                    last = serde_json::from_str(args.request.params.get()).ok();
                }
            }
            let last = last.expect("at least one queue/changed broadcast");
            assert!(
                last.entries.is_empty(),
                "final broadcast must show empty queue"
            );
        })
        .await;
}

#[tokio::test]
async fn protected_rows_reject_generic_mutations() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let held_at = std::time::Instant::now();
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("user-1", "A"));
                state.pending_inputs.push_back(protected_item("parent"));
                state.pending_inputs.push_back(user_item("user-2", "A"));
                state.edit_holds.insert("parent".into(), held_at);
            }
            actor.handle_remove_queued_prompt("parent", 0, None).await;
            actor
                .handle_remove_queued_prompt("parent", 0, Some("parent"))
                .await;
            actor
                .handle_remove_queued_prompt("parent", 0, Some("forged"))
                .await;
            actor
                .handle_edit_queued_prompt("parent", "changed".into(), None)
                .await;
            assert!(
                !actor
                    .handle_interject_queued_prompt("parent", 0, None, Some("changed again"))
                    .await
            );
            actor.handle_hold_edit("parent".into()).await;
            actor.handle_release_edit("parent").await;
            actor
                .handle_reorder_queue(&["user-2".into(), "parent".into(), "user-1".into()])
                .await;
            {
                let state = actor.state.lock().await;
                assert_eq!(
                    state
                        .pending_inputs
                        .iter()
                        .map(|item| item.prompt_id.as_str())
                        .collect::<Vec<_>>(),
                    vec!["user-2", "parent", "user-1"]
                );
            }
            actor.handle_clear_queue(None).await;
            let state = actor.state.lock().await;
            assert_eq!(state.pending_inputs.len(), 1);
            assert_eq!(state.pending_inputs[0].prompt_id, "parent");
            let meta = state.pending_inputs[0].queue_meta.as_ref().unwrap();
            assert_eq!(meta.text, "text for parent");
            assert_eq!(meta.version, 0);
            assert_eq!(state.edit_holds.get("parent"), Some(&held_at));
            assert_eq!(ids(&actor.build_queue_wire(&state)), vec!["parent"]);
        })
        .await;
}

/// Owner-scoped clear removes only the requesting client's queued prompts.
#[tokio::test]
async fn clear_queue_is_owner_scoped() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("a1", "A"));
                state.pending_inputs.push_back(user_item("b1", "B"));
                state.pending_inputs.push_back(user_item("a2", "A"));
            }
            actor.handle_clear_queue(Some("A")).await;
            let state = actor.state.lock().await;
            assert_eq!(ids(&actor.build_queue_wire(&state)), vec!["b1"]);
        })
        .await;
}

/// Removing a queued prompt must resolve its in-flight `session/prompt` RPC
/// with `Cancelled` rather than dropping the `respond_to` sender. A bare drop
/// surfaces to the client as `RecvError` → "session failed to respond", which
/// — because the client's PromptResponse prompt-id gate only runs on the `Ok`
/// path — is misattributed to the running turn and rendered as a spurious
/// "Turn failed". Regression guard.
#[tokio::test]
async fn remove_queued_prompt_resolves_rpc_cancelled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let (item, prompt_rx) = user_item_with_rx("p1", "A");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(item);
            }

            actor.handle_remove_queued_prompt("p1", 0, None).await;

            let turn_result = prompt_rx
                .await
                .expect("respond_to must be fulfilled, not dropped");
            assert!(
                matches!(
                    turn_result,
                    Ok(crate::session::commands::PromptTurnOk {
                        stop_reason: acp::StopReason::Cancelled,
                        completion_kind: PromptCompletionKind::RemovedFromQueue,
                        ..
                    })
                ),
                "removed queued prompt must report RemovedFromQueue"
            );
        })
        .await;
}

/// In-place LWW edit: replacing the text of a queued prompt
/// bumps `version`, records `last_editor`, preserves the original `owner`, and
/// re-broadcasts. The underlying `prompt_blocks` is also rebuilt so the agent
/// runs the new text when the prompt is eventually drained.
#[tokio::test]
async fn edit_queued_prompt_replaces_text_and_bumps_version() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
            }

            actor
                .handle_edit_queued_prompt("p1", "edited".into(), Some("bob"))
                .await;

            let state = actor.state.lock().await;
            let item = state
                .pending_inputs
                .iter()
                .find(|i| i.queue_meta.as_ref().is_some_and(|m| m.id == "p1"))
                .expect("p1 still in queue");
            let meta = item.queue_meta.as_ref().unwrap();
            assert_eq!(meta.text, "edited");
            assert_eq!(meta.version, 1, "edit bumps version");
            assert_eq!(meta.owner.as_deref(), Some("alice"), "owner preserved");
            assert_eq!(
                meta.last_editor.as_deref(),
                Some("bob"),
                "last_editor recorded"
            );

            // Underlying prompt_blocks was rebuilt with the new text.
            assert_eq!(item.prompt_blocks.len(), 1);
            match &item.prompt_blocks[0] {
                acp::ContentBlock::Text(t) => assert_eq!(t.text, "edited"),
                other => panic!("expected text block, got {other:?}"),
            }

            // The wire projection also reflects the new state.
            let wire = actor.build_queue_wire(&state);
            assert_eq!(wire.len(), 1);
            assert_eq!(wire[0].text, "edited");
            assert_eq!(wire[0].version, 1);
            assert_eq!(wire[0].owner.as_deref(), Some("alice"));
            assert_eq!(wire[0].last_editor.as_deref(), Some("bob"));
        })
        .await;
}

/// Applying a queued edit clears that row's combine hold with the new text, so
/// combine can't merge it on stale text before the edit lands. See
/// pager `exit_editing_mode_keeping_hold` for the race this closes.
#[tokio::test]
async fn edit_queued_prompt_clears_combine_hold() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                state
                    .edit_holds
                    .insert("p1".to_string(), std::time::Instant::now());
            }

            actor
                .handle_edit_queued_prompt("p1", "edited".into(), Some("bob"))
                .await;

            let state = actor.state.lock().await;
            assert!(
                !state.edit_holds.contains_key("p1"),
                "applying the edit must clear the combine hold for that row"
            );
            let item = state
                .pending_inputs
                .iter()
                .find(|i| i.queue_meta.as_ref().is_some_and(|m| m.id == "p1"))
                .expect("p1 still in queue");
            assert_eq!(item.queue_meta.as_ref().unwrap().text, "edited");
        })
        .await;
}

/// Empty or stale edit requests also clear their unscoped hold so the
/// promoter cannot remain parked on an early return.
#[tokio::test(flavor = "current_thread")]
async fn edit_early_returns_clear_combine_hold() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                state
                    .edit_holds
                    .insert("p1".to_string(), std::time::Instant::now());
            }
            actor
                .handle_edit_queued_prompt("p1", "   ".into(), Some("bob"))
                .await;
            assert!(
                !actor.state.lock().await.edit_holds.contains_key("p1"),
                "empty edit must clear the row hold"
            );

            {
                actor
                    .state
                    .lock()
                    .await
                    .edit_holds
                    .insert("missing".to_string(), std::time::Instant::now());
            }
            actor
                .handle_edit_queued_prompt("missing", "text".into(), Some("bob"))
                .await;
            assert!(
                !actor.state.lock().await.edit_holds.contains_key("missing"),
                "missing-row edit must clear the row hold"
            );
        })
        .await;
}

/// A front id under edit hold must not promote; clearing the hold then
/// re-kicking `maybe_start_running_task` starts the turn.
#[tokio::test(flavor = "current_thread")]
async fn maybe_start_blocks_when_front_under_edit_hold() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                state
                    .edit_holds
                    .insert("p1".to_string(), std::time::Instant::now());
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor
                .clone()
                .maybe_start_running_task(completion_tx.clone())
                .await;

            {
                let state = actor.state.try_lock().expect("uncontended");
                assert!(state.running_task.is_none(), "held front must not promote");
                assert_eq!(state.pending_inputs.len(), 1);
                assert!(state.edit_holds.contains_key("p1"));
            }

            {
                let mut state = actor.state.lock().await;
                state.edit_holds.remove("p1");
            }
            actor.clone().maybe_start_running_task(completion_tx).await;

            {
                let state = actor.state.try_lock().expect("no await since promote");
                assert_eq!(state.running_prompt_id(), Some("p1"));
                if let Some(task) = state.running_task.as_ref() {
                    task.handle.abort();
                }
            }
        })
        .await;
}

/// A second `hold_edit` through the actor inserts a fresh stamp so re-entering
/// edit after a dropped release does not inherit an aged leak bound.
#[tokio::test(flavor = "current_thread")]
async fn repeated_hold_refreshes_leak_bound() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            actor.state.lock().await.pending_inputs.push_back(user_item("p1", "A"));
            actor.handle_hold_edit("p1".to_string()).await;
            {
                let mut state = actor.state.lock().await;
                super::backdate_edit_hold(
                    &mut state.edit_holds,
                    "p1",
                    std::time::Duration::from_secs(60),
                );
            }
            let aged = {
                let state = actor.state.lock().await;
                *state.edit_holds.get("p1").expect("first hold present")
            };

            actor.handle_hold_edit("p1".to_string()).await;

            let state = actor.state.lock().await;
            let refreshed = *state.edit_holds.get("p1").expect("second hold present");
            assert!(
                refreshed > aged,
                "second hold_edit must refresh the stamp (got aged={aged:?} refreshed={refreshed:?})"
            );
        })
        .await;
}

/// A leaked hold older than `EDIT_HOLD_TTL` is discarded by the promote poll.
#[tokio::test(flavor = "current_thread")]
async fn maybe_start_expires_stale_hold_then_promotes() {
    use crate::session::acp_session::EDIT_HOLD_TTL;

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                state
                    .edit_holds
                    .insert("p1".to_string(), std::time::Instant::now());
                super::backdate_edit_hold(
                    &mut state.edit_holds,
                    "p1",
                    EDIT_HOLD_TTL + std::time::Duration::from_secs(1),
                );
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.clone().maybe_start_running_task(completion_tx).await;

            let state = actor.state.try_lock().expect("no await since promote");
            assert_eq!(state.running_prompt_id(), Some("p1"));
            assert!(
                !state.edit_holds.contains_key("p1"),
                "TTL expiry must discard the leaked hold"
            );
            if let Some(task) = state.running_task.as_ref() {
                task.handle.abort();
            }
        })
        .await;
}

/// Edit clears the hold; re-kick promotes the front with the new text.
#[tokio::test(flavor = "current_thread")]
async fn edit_clears_hold_then_promote_runs_edited_front() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                state
                    .edit_holds
                    .insert("p1".to_string(), std::time::Instant::now());
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor
                .clone()
                .maybe_start_running_task(completion_tx.clone())
                .await;
            assert!(
                actor.state.lock().await.running_task.is_none(),
                "must stay parked under hold"
            );

            actor
                .handle_edit_queued_prompt("p1", "edited".into(), Some("bob"))
                .await;
            actor.clone().maybe_start_running_task(completion_tx).await;

            {
                let state = actor.state.try_lock().expect("no await since promote");
                assert_eq!(state.running_prompt_id(), Some("p1"));
                assert!(!state.edit_holds.contains_key("p1"));
                if let Some(task) = state.running_task.as_ref() {
                    task.handle.abort();
                }
            }
        })
        .await;
}

/// Send-now interject of a held idle front clears the hold and promote
/// starts the edited text (not the original).
#[tokio::test(flavor = "current_thread")]
async fn interject_held_front_then_promote_runs_edited_text() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                state
                    .edit_holds
                    .insert("p1".to_string(), std::time::Instant::now());
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor
                .clone()
                .maybe_start_running_task(completion_tx.clone())
                .await;
            assert!(
                actor.state.lock().await.running_task.is_none(),
                "held front must not promote"
            );

            let _ = actor
                .handle_interject_queued_prompt("p1", 0, Some("alice"), Some("edited"))
                .await;
            actor.clone().maybe_start_running_task(completion_tx).await;

            {
                let state = actor.state.try_lock().expect("no await since promote");
                assert_eq!(state.running_prompt_id(), Some("p1"));
                assert!(
                    !state.edit_holds.contains_key("p1"),
                    "interject must clear the edit hold"
                );
                assert_eq!(
                    state
                        .pending_inputs
                        .front()
                        .and_then(|i| i.queue_meta.as_ref().map(|m| m.text.as_str())),
                    Some("edited"),
                    "promote must run the edited text",
                );
                if let Some(task) = state.running_task.as_ref() {
                    task.handle.abort();
                }
            }
        })
        .await;
}

/// Remove of a held front clears the hold; re-kick promotes the next row
/// (delete-while-editing must not leave the queue parked).
#[tokio::test(flavor = "current_thread")]
async fn remove_held_front_then_promote_starts_next() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                state.pending_inputs.push_back(user_item("p2", "alice"));
                state
                    .edit_holds
                    .insert("p1".to_string(), std::time::Instant::now());
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor
                .clone()
                .maybe_start_running_task(completion_tx.clone())
                .await;
            assert!(
                actor.state.lock().await.running_task.is_none(),
                "held front parks promote"
            );

            actor
                .handle_remove_queued_prompt("p1", 0, Some("alice"))
                .await;
            assert!(
                !actor.state.lock().await.edit_holds.contains_key("p1"),
                "remove must clear the edit hold"
            );
            actor.clone().maybe_start_running_task(completion_tx).await;

            {
                let state = actor.state.try_lock().expect("no await since promote");
                assert_eq!(state.running_prompt_id(), Some("p2"));
                if let Some(task) = state.running_task.as_ref() {
                    task.handle.abort();
                }
            }
        })
        .await;
}

/// Stale-version remove of a held front is a no-op on the row but must still
/// drop the hold so re-kick can promote the still-queued front.
#[tokio::test(flavor = "current_thread")]
async fn stale_remove_held_front_drops_hold_then_promote() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                state
                    .edit_holds
                    .insert("p1".to_string(), std::time::Instant::now());
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor
                .clone()
                .maybe_start_running_task(completion_tx.clone())
                .await;
            assert!(
                actor.state.lock().await.running_task.is_none(),
                "held front parks promote"
            );

            // Stale version: row stays queued; hold must still drop.
            actor
                .handle_remove_queued_prompt("p1", 99, Some("alice"))
                .await;
            {
                let state = actor.state.lock().await;
                assert!(
                    !state.edit_holds.contains_key("p1"),
                    "stale remove must still drop the edit hold"
                );
                assert_eq!(state.pending_inputs.len(), 1, "row must remain queued");
            }
            actor.clone().maybe_start_running_task(completion_tx).await;

            {
                let state = actor.state.try_lock().expect("no await since promote");
                assert_eq!(state.running_prompt_id(), Some("p1"));
                if let Some(task) = state.running_task.as_ref() {
                    task.handle.abort();
                }
            }
        })
        .await;
}

/// A hold on a follower must not block promote of an unheld front.
#[tokio::test(flavor = "current_thread")]
async fn follower_hold_does_not_block_front_promote() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                state.pending_inputs.push_back(user_item("p2", "alice"));
                state
                    .edit_holds
                    .insert("p2".to_string(), std::time::Instant::now());
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.clone().maybe_start_running_task(completion_tx).await;

            {
                let state = actor.state.try_lock().expect("no await since promote");
                assert_eq!(state.running_prompt_id(), Some("p1"));
                assert!(
                    state.pending_inputs.iter().any(|i| i.prompt_id == "p2"),
                    "held follower must remain queued",
                );
                if let Some(task) = state.running_task.as_ref() {
                    task.handle.abort();
                }
            }
        })
        .await;
}

/// End-to-end for the hold race: after an edit clears the hold, combine merges
/// using the edited text (not the pre-edit value). The edited follower is
/// absorbed into the front as `RemovedFromQueue` only after contributing the
/// new text — the race this closes dropped the edit by merging on stale text.
#[tokio::test]
async fn edit_then_combine_uses_edited_text() {
    use crate::session::commands::{PromptCompletionKind, PromptTurnOk};

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let (p1, mut p1_rx) = user_item_with_rx("p1", "alice");
            let (p2, mut p2_rx) = user_item_with_rx("p2", "alice");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(p1);
                state.pending_inputs.push_back(p2);
                // Follower under edit: skip_ids only gate followers.
                state
                    .edit_holds
                    .insert("p2".to_string(), std::time::Instant::now());
            }

            // While held, combine must not absorb the follower.
            {
                let mut state = actor.state.lock().await;
                SessionActor::combine_front_pending_inputs(&mut state.pending_inputs, &["p2"]);
                assert_eq!(
                    state.pending_inputs.len(),
                    2,
                    "held follower must not be absorbed"
                );
                assert!(p2_rx.try_recv().is_err(), "held row must stay queued");
            }

            actor
                .handle_edit_queued_prompt("p2", "edited follower".into(), Some("bob"))
                .await;

            // Edit cleared the hold under the same lock; combine now merges with
            // the new text. The front survives; the follower is absorbed.
            {
                let mut state = actor.state.lock().await;
                assert!(
                    !state.edit_holds.contains_key("p2"),
                    "edit must clear the hold before combine can absorb the row"
                );
                // Row still present with edited text before combine runs.
                let edited_text = state
                    .pending_inputs
                    .iter()
                    .find(|i| i.prompt_id == "p2")
                    .and_then(|i| i.queue_meta.as_ref().map(|m| m.text.clone()))
                    .expect("edited row still queued after edit");
                assert_eq!(edited_text, "edited follower");

                SessionActor::combine_front_pending_inputs(&mut state.pending_inputs, &[]);

                assert_eq!(state.pending_inputs.len(), 1);
                assert_eq!(state.pending_inputs[0].prompt_id, "p1");
                let combined = "text for p1\n\nedited follower";
                assert_eq!(
                    SessionActor::queue_text_from_blocks(&state.pending_inputs[0].prompt_blocks),
                    combined,
                    "merge must use the post-edit text, not the pre-edit value"
                );
                assert_eq!(
                    state.pending_inputs[0]
                        .queue_meta
                        .as_ref()
                        .map(|m| m.text.as_str()),
                    Some(combined)
                );
            }

            assert!(
                p1_rx.try_recv().is_err(),
                "front must remain queued after absorbing the follower"
            );
            // Absorbed after contributing the edited text (not with stale pre-edit text).
            assert!(matches!(
                p2_rx.try_recv(),
                Ok(Ok(PromptTurnOk {
                    completion_kind: PromptCompletionKind::RemovedFromQueue,
                    ..
                }))
            ));
        })
        .await;
}

/// Two sequential edits — last write wins (the actor mailbox serializes them).
#[tokio::test]
async fn edit_queued_prompt_is_last_writer_wins() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
            }

            actor
                .handle_edit_queued_prompt("p1", "first-write".into(), Some("bob"))
                .await;
            actor
                .handle_edit_queued_prompt("p1", "second-write".into(), Some("carol"))
                .await;

            let state = actor.state.lock().await;
            let meta = state
                .pending_inputs
                .iter()
                .find_map(|i| i.queue_meta.as_ref().filter(|m| m.id == "p1"))
                .expect("p1 still in queue");
            assert_eq!(meta.text, "second-write", "LWW: last edit wins");
            assert_eq!(meta.version, 2, "each edit bumps version");
            assert_eq!(meta.last_editor.as_deref(), Some("carol"));
            assert_eq!(meta.owner.as_deref(), Some("alice"), "owner unchanged");
        })
        .await;
}

/// Editing a missing id is a benign no-op (the entry was already drained or
/// removed by another client), but it still releases the editor's edit hold
/// so promote is not parked on a vanished row.
#[tokio::test]
async fn edit_queued_prompt_missing_id_is_noop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                // Hold whose row vanished mid-edit: the rejected save is its release point.
                state
                    .edit_holds
                    .insert("ghost".into(), std::time::Instant::now());
            }

            actor
                .handle_edit_queued_prompt("ghost", "ignored".into(), Some("bob"))
                .await;

            let state = actor.state.lock().await;
            let meta = state
                .pending_inputs
                .iter()
                .find_map(|i| i.queue_meta.as_ref().filter(|m| m.id == "p1"))
                .expect("p1 still in queue");
            // p1 untouched.
            assert_eq!(meta.text, "text for p1");
            assert_eq!(meta.version, 0);
            assert!(meta.last_editor.is_none());
            assert!(
                !state.edit_holds.contains_key("ghost"),
                "a rejected edit must release its edit hold"
            );
        })
        .await;
}

/// Editing the currently-running turn is a no-op — the in-flight prompt is
/// out of scope for queue edits.
#[tokio::test]
async fn edit_queued_prompt_running_turn_is_noop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
                // Mark p1 as the running turn (race-free identity: the task
                // slot, not the `current_prompt_id` pin).
                state.running_task = Some(running_task_stub("p1"));
                // Editor opened while p1 was still queued; promoted mid-edit.
                state
                    .edit_holds
                    .insert("p1".into(), std::time::Instant::now());
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("p1".into());

            actor
                .handle_edit_queued_prompt("p1", "no-op".into(), Some("bob"))
                .await;

            let state = actor.state.lock().await;
            let meta = state
                .pending_inputs
                .iter()
                .find_map(|i| i.queue_meta.as_ref().filter(|m| m.id == "p1"))
                .expect("p1 still in queue");
            assert_eq!(meta.text, "text for p1", "running turn untouched");
            assert_eq!(meta.version, 0);
            assert!(meta.last_editor.is_none());
            assert!(
                !state.edit_holds.contains_key("p1"),
                "a save rejected for the running turn must release its edit hold"
            );
        })
        .await;
}

/// An edit with no editor (None) clears `last_editor` rather than preserving
/// the previous editor — the most recent edit's identity is what we want to
/// surface.
#[tokio::test]
async fn edit_queued_prompt_clears_last_editor_when_none() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
            }

            actor
                .handle_edit_queued_prompt("p1", "v1".into(), Some("bob"))
                .await;
            actor
                .handle_edit_queued_prompt("p1", "v2".into(), None)
                .await;

            let state = actor.state.lock().await;
            let meta = state
                .pending_inputs
                .iter()
                .find_map(|i| i.queue_meta.as_ref().filter(|m| m.id == "p1"))
                .expect("p1 still in queue");
            assert_eq!(meta.text, "v2");
            assert_eq!(meta.version, 2);
            assert!(
                meta.last_editor.is_none(),
                "last_editor reflects the most recent edit"
            );
        })
        .await;
}

/// Owner-scoped clear must resolve every cleared prompt's RPC with `Cancelled`
/// (not drop it) — same failure mode as remove. Prompts owned by other clients
/// stay queued and keep their RPC pending.
#[tokio::test]
async fn clear_queue_resolves_cleared_rpcs_cancelled() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let (a1, a1_rx) = user_item_with_rx("a1", "A");
            let (b1, b1_rx) = user_item_with_rx("b1", "B");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(a1);
                state.pending_inputs.push_back(b1);
            }

            actor.handle_clear_queue(Some("A")).await;

            // a1 (owned by A) was cleared → its RPC resolves Cancelled.
            let a1_result = a1_rx.await.expect("cleared prompt RPC must be resolved");
            assert!(
                matches!(
                    a1_result,
                    Ok(crate::session::commands::PromptTurnOk {
                        stop_reason: acp::StopReason::Cancelled,
                        completion_kind: PromptCompletionKind::RemovedFromQueue,
                        ..
                    })
                ),
                "cleared queued prompt must report RemovedFromQueue"
            );

            // b1 (owned by B) stays queued → its RPC is still pending.
            let state = actor.state.lock().await;
            assert_eq!(ids(&actor.build_queue_wire(&state)), vec!["b1"]);
            drop(b1_rx);
        })
        .await;
}

/// With NO turn running (e.g. the turn ended in the `Send now` race window) the
/// interject is a benign no-op: the prompt stays queued so it runs normally as
/// its own turn, and nothing is stranded in the interjection buffer.
#[tokio::test]
async fn interject_queued_prompt_noop_without_running_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "A"));
            }
            // current_prompt_id is None → no turn running.

            let _ = actor
                .handle_interject_queued_prompt("p1", 0, None, None)
                .await;

            let state = actor.state.lock().await;
            assert_eq!(
                ids(&actor.build_queue_wire(&state)),
                vec!["p1"],
                "prompt stays queued when no turn is running"
            );
            assert!(
                actor.pending_interjections.is_empty(),
                "nothing buffered without a running turn"
            );
        })
        .await;
}

/// A stale `expected_version` interject is a benign no-op (mirrors remove): the
/// prompt stays queued and no interjection is buffered.
#[tokio::test]
async fn interject_queued_prompt_stale_version_noop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "A"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            // Version 99 != live version 0 → no-op.
            let _ = actor
                .handle_interject_queued_prompt("p1", 99, None, None)
                .await;

            let state = actor.state.lock().await;
            assert_eq!(ids(&actor.build_queue_wire(&state)), vec!["p1"]);
            assert!(actor.pending_interjections.is_empty());
        })
        .await;
}

/// Interject in the cancel gap (turn cleared, next prompt not started) must do nothing: no buffer
/// into `pending_interjections` (no drain), prompt stays queued to run alone; queue rebroadcasts.
#[tokio::test]
async fn interject_after_cancel_does_nothing_and_keeps_prompt_queued() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.pending_inputs.push_back(user_item("p1", "A"));
                state.running_task = Some(AgentTask {
                    prompt_id: "running".into(),
                    handle: tokio::task::spawn_local(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    })
                    .abort_handle(),
                });
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            let _ = actor
                .cancel_running_task(crate::session::CancelOptions {
                    cancel_subagents: true,
                    user_initiated: true,
                    ..Default::default()
                })
                .await;

            // p1 would start next; the interject lands in the gap where no turn runs yet.
            let _ = actor
                .handle_interject_queued_prompt("p1", 0, None, None)
                .await;

            let state = actor.state.lock().await;
            assert_eq!(
                ids(&actor.build_queue_wire(&state)),
                vec!["p1"],
                "prompt must stay queued (it runs as its own turn)"
            );
            assert!(
                actor.pending_interjections.is_empty(),
                "no interjection may buffer without a running turn (nothing would drain it)"
            );
            drop(state);

            // The interject no-op still rebroadcasts so clients reconcile.
            let mut saw_broadcast = false;
            while let Ok(msg) = gateway_rx.try_recv() {
                if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
                    && args.request.method.as_ref()
                        == crate::session::prompt_queue::QUEUE_CHANGED_METHOD
                {
                    saw_broadcast = true;
                }
            }
            assert!(saw_broadcast, "interject no-op must rebroadcast the queue");
        })
        .await;
}

/// Turn ended before the edited interject landed (the `Send now` race): the
/// edit is saved to the queued row as an LWW write — nothing is buffered, but
/// the row drains later with the EDITED text instead of silently reverting.
#[tokio::test]
async fn interject_queued_prompt_with_new_text_no_running_turn_saves_edit() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                let mut item = user_item("p1", "A");
                item.prompt_blocks
                    .push(acp::ContentBlock::Image(test_image_content()));
                state.pending_inputs.push_back(item);
            }
            // current_prompt_id is None → no turn running.

            let _ = actor
                .handle_interject_queued_prompt("p1", 0, None, Some("EDITED text"))
                .await;

            {
                let state = actor.state.lock().await;
                let wire = actor.build_queue_wire(&state);
                assert_eq!(ids(&wire), vec!["p1"], "row stays queued");
                assert_eq!(wire[0].text, "EDITED text", "edit saved to the row");
                assert_eq!(wire[0].version, 1, "LWW edit bumps the version");
                assert!(
                    actor.pending_interjections.is_empty(),
                    "nothing buffered without a running turn"
                );
            }

            // The row's Image blocks survive the text-only LWW edit — the
            // edit must not silently detach the queued prompt's images.
            {
                let state = actor.state.lock().await;
                let images: usize = state.pending_inputs[0]
                    .prompt_blocks
                    .iter()
                    .filter(|b| matches!(b, acp::ContentBlock::Image(_)))
                    .count();
                assert_eq!(images, 1, "image block must survive the LWW edit");
            }

            // Stale version gets NO fallback even without a running turn —
            // a concurrent edit won and losing ours is correct LWW.
            let _ = actor
                .handle_interject_queued_prompt("p1", 99, None, Some("LOSER edit"))
                .await;
            let state = actor.state.lock().await;
            let wire = actor.build_queue_wire(&state);
            assert_eq!(wire[0].text, "EDITED text", "stale edit must not win");
            assert_eq!(wire[0].version, 1);
        })
        .await;
}

/// Editing a queued bash row rebuilds the bash `PromptBlockMeta` with the edited text.
#[tokio::test]
async fn edit_queued_bash_row_rebuilds_bash_meta_with_new_text() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(bash_item("p1", "alice", "ls"));
            }

            actor
                .handle_edit_queued_prompt("p1", "ls -la".into(), Some("bob"))
                .await;

            let state = actor.state.lock().await;
            let item = state
                .pending_inputs
                .iter()
                .find(|i| i.queue_meta.as_ref().is_some_and(|m| m.id == "p1"))
                .expect("p1 still in queue");
            assert_eq!(
                SessionActor::extract_bash_command(&item.prompt_blocks).as_deref(),
                Some("ls -la"),
                "edited row must still execute as a bash command with the NEW text"
            );
            let meta = item.queue_meta.as_ref().unwrap();
            assert_eq!(meta.kind, "bash", "wire kind unchanged");
            assert_eq!(meta.text, "ls -la");
            assert_eq!(meta.version, 1);
        })
        .await;
}

/// A blank `newText` never blanks a queued row.
#[tokio::test]
async fn edit_queued_prompt_empty_text_is_noop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(bash_item("p1", "alice", "ls"));
            }

            actor
                .handle_edit_queued_prompt("p1", "  ".into(), None)
                .await;

            let state = actor.state.lock().await;
            let wire = actor.build_queue_wire(&state);
            assert_eq!(wire[0].text, "ls", "row text untouched");
            assert_eq!(wire[0].version, 0, "no LWW bump for a blank edit");
        })
        .await;
}

/// A plain-prompt edit must not gain bash meta.
#[tokio::test]
async fn edit_queued_plain_row_stays_plain() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "alice"));
            }

            actor
                .handle_edit_queued_prompt("p1", "! looks like bash".into(), Some("bob"))
                .await;

            let state = actor.state.lock().await;
            let item = &state.pending_inputs[0];
            assert!(
                SessionActor::extract_bash_command(&item.prompt_blocks).is_none(),
                "plain rows must not acquire bash meta"
            );
        })
        .await;
}

/// Interjecting a queued bash row is a benign no-op that still rebroadcasts.
#[tokio::test]
async fn interject_queued_bash_row_noop_keeps_row_queued() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, mut gateway_rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(bash_item("p1", "A", "ls"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            let _ = actor
                .handle_interject_queued_prompt("p1", 0, None, None)
                .await;

            let state = actor.state.lock().await;
            assert_eq!(
                ids(&actor.build_queue_wire(&state)),
                vec!["p1"],
                "bash row must stay queued"
            );
            assert!(
                actor.pending_interjections.is_empty(),
                "a bash command must never buffer as an interjection"
            );
            drop(state);

            let mut saw_broadcast = false;
            while let Ok(msg) = gateway_rx.try_recv() {
                if let echo_agent_acp::AcpClientMessage::ExtNotification(args) = msg
                    && args.request.method.as_ref()
                        == crate::session::prompt_queue::QUEUE_CHANGED_METHOD
                {
                    saw_broadcast = true;
                }
            }
            assert!(saw_broadcast, "interject no-op must rebroadcast the queue");
        })
        .await;
}

#[tokio::test]
async fn promote_queued_as_interjections_sends_plain_and_stops_at_bash() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(user_item("m1", "A"));
                state.pending_inputs.push_back(user_item("m2", "A"));
                state.pending_inputs.push_back(bash_item("b3", "A", "ls"));
                state.pending_inputs.push_back(user_item("m4", "A"));
                state.running_task = Some(running_task_stub("running"));
            }

            actor.promote_queued_as_interjections().await;

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "b3", "m4"]);
            drop(state);
            let interjections: Vec<String> = actor
                .pending_interjections
                .drain_all()
                .into_iter()
                .map(|entry| entry.text)
                .collect();
            assert_eq!(interjections, vec!["text for m1", "text for m2"]);
        })
        .await;
}

#[tokio::test]
async fn promote_queued_as_interjections_stops_at_edit_hold() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(user_item("m1", "A"));
                state.pending_inputs.push_back(user_item("m2", "A"));
                state.pending_inputs.push_back(user_item("m3", "A"));
                state
                    .edit_holds
                    .insert("m2".into(), std::time::Instant::now());
                state.running_task = Some(running_task_stub("running"));
            }

            actor.promote_queued_as_interjections().await;

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "m2", "m3"]);
            assert!(state.edit_holds.contains_key("m2"));
            drop(state);
            let interjections: Vec<String> = actor
                .pending_interjections
                .drain_all()
                .into_iter()
                .map(|entry| entry.text)
                .collect();
            assert_eq!(interjections, vec!["text for m1"]);
        })
        .await;
}

#[tokio::test]
async fn promote_queued_as_interjections_stops_at_send_now() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                let mut send_now = user_item("m1", "A");
                send_now.send_now = true;
                state.pending_inputs.push_back(send_now);
                state.pending_inputs.push_back(user_item("m2", "A"));
                state.running_task = Some(running_task_stub("running"));
            }

            actor.promote_queued_as_interjections().await;

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "m1", "m2"]);
            assert!(state.pending_inputs[1].send_now);
            drop(state);
            assert!(
                actor.pending_interjections.is_empty(),
                "send-now must stay queued to run as the next turn"
            );
        })
        .await;
}

/// Product gate: with Steer off, a held plain row must not promote at a
/// safe point (queue stays; no interjection in conversation).
#[tokio::test]
async fn drain_at_safe_point_with_steer_off_does_not_promote_held_row() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::util::config::set_follow_up_steer_cache(false);
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(user_item("held", "A"));
                state.running_task = Some(running_task_stub("running"));
            }

            assert!(!actor.drain_interjections_at_safe_point().await);
            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "held"]);
            drop(state);
            assert!(
                actor.pending_interjections.is_empty(),
                "Queue mode must not promote into the interjection buffer"
            );
        })
        .await;
}

/// Product gate: with Steer on, a held plain row promotes and drains into a
/// synthetic interjection user item.
#[tokio::test]
async fn drain_at_safe_point_with_steer_on_promotes_and_drains_held_row() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::util::config::set_follow_up_steer_cache(true);
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(user_item("held", "A"));
                state.running_task = Some(running_task_stub("running"));
            }

            assert!(
                actor.drain_interjections_at_safe_point().await,
                "Steer must promote and drain the held follow-up"
            );
            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running"]);
            drop(state);
            assert!(actor.pending_interjections.is_empty());
            let conversation = actor.chat_state_handle.get_conversation().await;
            let last = conversation
                .last()
                .expect("interjection must land in conversation");
            let text = last.text_content();
            assert!(
                text.contains("text for held"),
                "drained interjection must include held prompt text, got: {text}"
            );
        })
        .await;
}

/// Leader multi-client: only promote rows owned by the *running* client.
/// Another client's "I'll go next" row stops the FIFO prefix (not skipped).
#[tokio::test]
async fn promote_queued_as_interjections_stops_at_other_owner() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(user_item("a1", "A"));
                state.pending_inputs.push_back(user_item("b1", "B"));
                state.pending_inputs.push_back(user_item("a2", "A"));
                state.running_task = Some(running_task_stub("running"));
            }

            actor.promote_queued_as_interjections().await;

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            // a1 promoted; b1 blocks a2 (FIFO — do not jump B).
            assert_eq!(order, vec!["running", "b1", "a2"]);
            drop(state);
            let interjections: Vec<String> = actor
                .pending_interjections
                .drain_all()
                .into_iter()
                .map(|entry| entry.text)
                .collect();
            assert_eq!(interjections, vec!["text for a1"]);
        })
        .await;
}

/// Per-turn tool overrides are applied at turn promotion, not via
/// interjection; stop rather than drop the override payload.
#[tokio::test]
async fn promote_queued_as_interjections_stops_at_tool_overrides() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(user_item("plain", "A"));
                let mut with_override = user_item("override", "A");
                with_override.tool_overrides_update = Some(x_search_cutoff_update());
                state.pending_inputs.push_back(with_override);
                state.pending_inputs.push_back(user_item("after", "A"));
                state.running_task = Some(running_task_stub("running"));
            }

            actor.promote_queued_as_interjections().await;

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "override", "after"]);
            assert!(
                state.pending_inputs[1].tool_overrides_update.is_some(),
                "override row must stay queued with its payload"
            );
            drop(state);
            let interjections: Vec<String> = actor
                .pending_interjections
                .drain_all()
                .into_iter()
                .map(|entry| entry.text)
                .collect();
            assert_eq!(interjections, vec!["text for plain"]);
        })
        .await;
}

#[tokio::test]
async fn promote_queued_as_interjections_does_not_steal_other_owners_next_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                // A is running; B only has a queued next-turn prompt.
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(user_item("b_next", "B"));
                state.running_task = Some(running_task_stub("running"));
            }

            actor.promote_queued_as_interjections().await;

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "b_next"]);
            drop(state);
            assert!(
                actor.pending_interjections.is_empty(),
                "must not inject another client's next-turn into A's turn"
            );
        })
        .await;
}

/// Protected (visible, non-editable) rows pin their slot: ordinary editable
/// prefix still promotes, but the protected row is not dequeued/interjected
/// and stops FIFO so later editable rows stay behind the pin.
#[tokio::test]
async fn promote_queued_as_interjections_keeps_protected_rows_pinned() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(user_item("m1", "A"));
                state.pending_inputs.push_back(protected_item("parent"));
                state.pending_inputs.push_back(user_item("m2", "A"));
                state.running_task = Some(running_task_stub("running"));
            }

            actor.promote_queued_as_interjections().await;

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                order,
                vec!["running", "parent", "m2"],
                "protected pin stays; only the editable prefix promotes"
            );
            assert!(
                state.pending_inputs[1].is_queue_protected(),
                "parent row must remain protected after promote"
            );
            drop(state);
            let interjections: Vec<String> = actor
                .pending_interjections
                .drain_all()
                .into_iter()
                .map(|entry| entry.text)
                .collect();
            assert_eq!(interjections, vec!["text for m1"]);
        })
        .await;
}

/// A protected row at the head of held work blocks steer promotion entirely.
#[tokio::test]
async fn promote_queued_as_interjections_stops_when_protected_is_next() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(protected_item("parent"));
                state.pending_inputs.push_back(user_item("m1", "A"));
                state.running_task = Some(running_task_stub("running"));
            }

            actor.promote_queued_as_interjections().await;

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "parent", "m1"]);
            drop(state);
            assert!(
                actor.pending_interjections.is_empty(),
                "must not interject past or through a protected pin"
            );
        })
        .await;
}

/// Steer-on safe-point drain must not treat a protected pin as promotable held
/// work (pair with direct promote tests above).
#[tokio::test]
async fn drain_at_safe_point_with_steer_on_leaves_protected_row_queued() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            crate::util::config::set_follow_up_steer_cache(true);
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.pending_inputs.push_back(protected_item("parent"));
                state.pending_inputs.push_back(user_item("held", "A"));
                state.running_task = Some(running_task_stub("running"));
            }

            assert!(
                !actor.drain_interjections_at_safe_point().await,
                "protected-only held prefix must not arm steer promotion"
            );
            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "parent", "held"]);
            drop(state);
            assert!(actor.pending_interjections.is_empty());
        })
        .await;
}

/// An edited interject of a bash row refuses the interject but keeps the edit.
#[tokio::test]
async fn interject_queued_bash_row_with_new_text_saves_edit() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(bash_item("p1", "A", "ls"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            let _ = actor
                .handle_interject_queued_prompt("p1", 0, None, Some("ls -la"))
                .await;

            let state = actor.state.lock().await;
            let wire = actor.build_queue_wire(&state);
            assert_eq!(ids(&wire), vec!["p1"], "bash row must stay queued");
            assert_eq!(wire[0].text, "ls -la", "the edit must be kept (LWW)");
            assert_eq!(wire[0].version, 1, "LWW edit bumps the version");
            assert_eq!(wire[0].kind, "bash", "kind survives the refused interject");
            assert!(actor.pending_interjections.is_empty());
        })
        .await;
}

/// A stale version no-ops the WHOLE edited interject: no interjection (edited
/// text included) and the row untouched — edit + interject is one atomic op.
#[tokio::test]
async fn interject_queued_prompt_with_new_text_stale_version_full_noop() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("p1", "A"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            let _ = actor
                .handle_interject_queued_prompt("p1", 99, None, Some("EDITED text"))
                .await;

            let state = actor.state.lock().await;
            assert_eq!(
                ids(&actor.build_queue_wire(&state)),
                vec!["p1"],
                "row untouched on stale version"
            );
            assert!(
                actor.pending_interjections.is_empty(),
                "edited text must not interject on stale version"
            );
        })
        .await;
}

/// Send-now `queue_input`: prompt lands behind the running front and cancels the turn.
#[tokio::test]
async fn queue_input_send_now_inserts_behind_running_front_and_requests_cancel() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.front_message_committed = true;
                state.pending_inputs.push_back(user_item("held", "A"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            let (respond_to, _prx) = oneshot::channel();
            let cancel = actor
                .queue_input(QueueInputRequest {
                    send_now: true,
                    ..queue_input_request(
                        vec![acp::ContentBlock::Text(acp::TextContent::new("now"))],
                        "d-now",
                        respond_to,
                    )
                })
                .await;
            assert!(cancel, "send-now behind a running turn must cancel it");

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                order,
                vec!["running", "d-now", "held"],
                "send-now runs next; held rows keep their place behind it"
            );
        })
        .await;
}

#[tokio::test]
async fn queue_input_send_now_during_goal_turn_merges_as_interjections_fifo() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.pending_inputs.push_back(user_item("held", "A"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());
            actor.goal_tracker.lock().create_goal(
                "goal".into(),
                "objective".into(),
                None,
                0,
                "2026-01-01T00:00:00Z".into(),
                None,
            );

            for id in ["sn-1", "sn-2"] {
                let (respond_to, _prx) = oneshot::channel();
                let cancel = actor
                    .queue_input(QueueInputRequest {
                        send_now: true,
                        ..queue_input_request(
                            vec![acp::ContentBlock::Text(acp::TextContent::new(id))],
                            id,
                            respond_to,
                        )
                    })
                    .await;
                assert!(!cancel, "goal turns never cancel-and-send");
            }

            assert_eq!(
                actor
                    .state
                    .lock()
                    .await
                    .pending_inputs
                    .iter()
                    .map(|item| item.prompt_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["running", "held"]
            );
            let interjections: Vec<String> = actor
                .pending_interjections
                .drain_all()
                .into_iter()
                .map(|entry| entry.text)
                .collect();
            assert_eq!(interjections, vec!["sn-1", "sn-2"]);
        })
        .await;
}

#[tokio::test]
async fn queue_input_wait_keeps_existing_queue_order() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            let (respond_to, _p1) = oneshot::channel();
            let cancel = actor
                .queue_input(queue_input_request(
                    vec![acp::ContentBlock::Text(acp::TextContent::new("early"))],
                    "pre-wait",
                    respond_to,
                ))
                .await;
            assert!(!cancel, "pre-wait rows must not cancel the turn");

            actor.tool_context.blocking_wait_depth.set_depth_for_test(1);
            let (respond_to, _p2) = oneshot::channel();
            let cancel = actor
                .queue_input(queue_input_request(
                    vec![acp::ContentBlock::Text(acp::TextContent::new("mid-wait"))],
                    "d-mid",
                    respond_to,
                ))
                .await;
            assert!(
                !cancel,
                "with a held queue present, mid-wait prompts must not cancel-and-send"
            );

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                order,
                vec!["running", "pre-wait", "d-mid"],
                "mid-wait prompt appends behind existing held rows"
            );
        })
        .await;
}

#[tokio::test]
async fn queue_input_ordinary_goal_steering_during_wait_never_cancels() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.front_message_committed = true;
            }
            actor.tool_context.blocking_wait_depth.set_depth_for_test(1);
            actor.goal_tracker.lock().create_goal(
                "goal".into(),
                "objective".into(),
                None,
                0,
                "2026-01-01T00:00:00Z".into(),
                None,
            );
            let (respond_to, _rx) = oneshot::channel();
            assert!(
                !actor
                    .queue_input(queue_input_request(
                        vec![acp::ContentBlock::Text(acp::TextContent::new("补充约束"))],
                        "steering",
                        respond_to
                    ))
                    .await
            );
            assert_eq!(actor.pending_interjections.drain_all().len(), 1);
            assert_eq!(actor.state.lock().await.pending_inputs.len(), 1);
        })
        .await;
}

#[tokio::test]
async fn queue_input_wait_without_held_rows_never_replaces_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.front_message_committed = true;
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());
            actor.tool_context.blocking_wait_depth.set_depth_for_test(1);

            let (respond_to, _p) = oneshot::channel();
            let cancel = actor
                .queue_input(queue_input_request(
                    vec![acp::ContentBlock::Text(acp::TextContent::new("first"))],
                    "first",
                    respond_to,
                ))
                .await;
            assert!(
                !cancel,
                "an ordinary prompt must not replace a turn during a wait"
            );

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "first"]);
            assert!(
                !state
                    .pending_inputs
                    .iter()
                    .find(|i| i.prompt_id == "first")
                    .is_some_and(|i| i.send_now),
                "the queued message must retain its explicit send-now=false intent"
            );

            drop(state);
            let (respond_to, _p2) = oneshot::channel();
            let cancel2 = actor
                .queue_input(queue_input_request(
                    vec![acp::ContentBlock::Text(acp::TextContent::new("second"))],
                    "second",
                    respond_to,
                ))
                .await;
            assert!(!cancel2, "second prompt with held row must not cancel");
            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["running", "first", "second"]);
            assert!(
                !state
                    .pending_inputs
                    .iter()
                    .find(|i| i.prompt_id == "second")
                    .is_some_and(|i| i.send_now),
                "second prompt is a plain held append"
            );
        })
        .await;
}

/// Hidden user-origin interjection fallbacks still count as held work: a mid-wait
/// prompt must not auto-send-now / cancel the running turn just because the
/// fallback is queue-hidden.
#[tokio::test]
async fn queue_input_wait_preserves_hidden_user_fallback() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                // Queue-hidden user fallback (same shape as interjection fallback).
                let mut fallback = input_with_origin_rx(
                    "interject-fallback-held",
                    crate::session::PromptOrigin::User,
                )
                .0;
                fallback.queue_mutation_policy = QueueMutationPolicy::hidden();
                assert!(
                    !fallback.is_queue_visible(),
                    "fallback under test must be queue-hidden"
                );
                assert!(
                    matches!(
                        fallback.input_origin.policy().shutdown,
                        crate::session::ShutdownPolicy::Drain
                    ),
                    "fallback remains Drain-held user work"
                );
                state.pending_inputs.push_back(fallback);
                state.running_task = Some(running_task_stub("running"));
                state.front_message_committed = true;
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());
            actor.tool_context.blocking_wait_depth.set_depth_for_test(1);

            let (respond_to, _p) = oneshot::channel();
            let cancel = actor
                .queue_input(queue_input_request(
                    vec![acp::ContentBlock::Text(acp::TextContent::new("mid-wait"))],
                    "d-mid",
                    respond_to,
                ))
                .await;
            assert!(
                !cancel,
                "hidden user fallback must block auto-send-now cancel"
            );
            let state = actor.state.lock().await;
            let mid = state
                .pending_inputs
                .iter()
                .find(|i| i.prompt_id == "d-mid")
                .expect("mid-wait row queued");
            assert!(
                !mid.send_now,
                "mid-wait row must append as ordinary held work, not send-now"
            );
        })
        .await;
}

/// Foreground subagent waits obey the same queue contract as terminal waits.
#[tokio::test]
async fn queue_input_foreground_subagent_wait_never_replaces_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.front_message_committed = true;
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            let depth = actor.tool_context.blocking_wait_depth.clone();
            assert_eq!(depth.depth(), 0, "no wait window yet");

            let wait_guard = crate::tools::tool_context::BlockingWaitGuard::enter(depth.clone());
            assert_eq!(
                depth.depth(),
                1,
                "a foreground subagent await must raise blocking_wait_depth"
            );

            let (respond_to, _p1) = oneshot::channel();
            let cancel = actor
                .queue_input(queue_input_request(
                    vec![acp::ContentBlock::Text(acp::TextContent::new("preempt"))],
                    "during-await",
                    respond_to,
                ))
                .await;
            assert!(
                !cancel,
                "a foreground subagent wait must not grant permission to replace the turn"
            );

            drop(wait_guard);
            assert_eq!(
                depth.depth(),
                0,
                "guard drop must restore blocking_wait_depth"
            );

            let (respond_to, _p2) = oneshot::channel();
            let cancel = actor
                .queue_input(queue_input_request(
                    vec![acp::ContentBlock::Text(acp::TextContent::new("later"))],
                    "after-await",
                    respond_to,
                ))
                .await;
            assert!(
                !cancel,
                "outside the await window prompts must queue normally"
            );

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                order,
                vec!["running", "during-await", "after-await"],
                "the mid-await prompt runs next; the later one queues behind it"
            );
        })
        .await;
}

#[tokio::test]
async fn queue_input_send_now_exempts_synthetic_prompts() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());
            actor.tool_context.blocking_wait_depth.set_depth_for_test(1);

            let (respond_to, _p1) = oneshot::channel();
            let cancel = actor
                .queue_input(QueueInputRequest {
                    verbatim: true,
                    ..queue_input_request(
                        vec![acp::ContentBlock::Text(acp::TextContent::new("done"))],
                        "task-completed-bg1",
                        respond_to,
                    )
                })
                .await;
            assert!(!cancel, "synthetic prompts must never cancel the turn");
        })
        .await;
}

#[tokio::test]
async fn queue_send_now_during_goal_routes_by_kind() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.pending_inputs.push_back(user_item("q1", "A"));
                state.pending_inputs.push_back(bash_item("b1", "A", "ls"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());
            actor.goal_tracker.lock().create_goal(
                "goal".into(),
                "objective".into(),
                None,
                0,
                "2026-01-01T00:00:00Z".into(),
                None,
            );

            let cancel = actor
                .handle_interject_queued_prompt("q1", 0, Some("A"), None)
                .await;
            assert!(!cancel);
            let state = actor.state.lock().await;
            assert_eq!(
                state
                    .pending_inputs
                    .iter()
                    .map(|item| item.prompt_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["running", "b1"]
            );
            drop(state);
            let interjections: Vec<String> = actor
                .pending_interjections
                .drain_all()
                .into_iter()
                .map(|entry| entry.text)
                .collect();
            assert_eq!(interjections, vec!["text for q1"]);

            let cancel = actor
                .handle_interject_queued_prompt("b1", 0, Some("A"), None)
                .await;
            assert!(!cancel);
            let state = actor.state.lock().await;
            assert!(
                state
                    .pending_inputs
                    .iter()
                    .any(|item| item.prompt_id == "b1"),
                "bash rows remain queued during goals"
            );
        })
        .await;
}

/// Queue-row send-now: the row (any kind) promotes to run behind the running
/// front with its RPC live and an LWW edit applied, and cancels the turn.
#[tokio::test]
async fn queue_send_now_promotes_row_and_requests_cancel() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.front_message_committed = true;
                state.pending_inputs.push_back(user_item("held", "A"));
                state.pending_inputs.push_back(bash_item("b1", "A", "ls"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            let cancel = actor
                .handle_interject_queued_prompt("b1", 0, Some("A"), Some("ls -la"))
                .await;
            assert!(cancel, "promoting a row behind a running turn cancels it");

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                order,
                vec!["running", "b1", "held"],
                "promoted row runs next; the held row stays behind it"
            );
            let promoted = &state.pending_inputs[1];
            assert_eq!(
                promoted.queue_meta.as_ref().map(|m| m.text.as_str()),
                Some("ls -la"),
                "edit applies LWW before promotion"
            );
            assert!(
                actor.pending_interjections.is_empty(),
                "send-now never merges into the running turn"
            );
        })
        .await;
}

/// Queue-row send-now with no running turn: the row fronts but nothing cancels.
#[tokio::test]
async fn queue_send_now_idle_fronts_row_without_cancel() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("q1", "A"));
                state.pending_inputs.push_back(user_item("q2", "A"));
            }

            let cancel = actor
                .handle_interject_queued_prompt("q2", 0, Some("A"), None)
                .await;
            assert!(!cancel, "no running turn — nothing to cancel");

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["q2", "q1"], "send-now row runs first");
        })
        .await;
}

/// Send racing turn completion: the front-pin keys on `running_prompt_id()`,
/// not the already-cleared `current_prompt_id`, so the unpopped front survives.
#[tokio::test]
async fn queue_input_send_now_pins_front_on_running_task_identity() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("finished", "A"));
                state.running_task = Some(running_task_stub("finished"));
                state.front_message_committed = true;
            }
            // Completion-race window: current_prompt_id cleared, front finished but unpopped.
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = None;

            let (respond_to, _prx) = oneshot::channel();
            let cancel = actor
                .queue_input(QueueInputRequest {
                    send_now: true,
                    ..queue_input_request(
                        vec![acp::ContentBlock::Text(acp::TextContent::new("now"))],
                        "d-now",
                        respond_to,
                    )
                })
                .await;
            assert!(
                cancel,
                "running_task present = a turn to cancel, even mid-completion-race"
            );

            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(
                order,
                vec!["finished", "d-now"],
                "the finished-but-unpopped front must NOT be displaced"
            );
        })
        .await;
}

/// A stale completion must not clear the promoted turn's `running_task` (would double-spawn).
#[tokio::test]
async fn stale_completion_does_not_clear_promoted_turns_running_task() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("promoted", "A"));
                state.running_task = Some(running_task_stub("promoted"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("promoted".into());

            actor
                .handle_completion(
                    "cancelled-old".to_string(),
                    Ok(crate::session::commands::PromptTurnOk {
                        stop_reason: acp::StopReason::EndTurn,
                        total_tokens: 0,
                        turn_snapshot: None,
                        completion_kind: crate::session::commands::PromptCompletionKind::Completed,
                        structured_output: None,
                        usage: None,
                        tool_overrides: None,
                    }),
                )
                .await;

            let state = actor.state.lock().await;
            assert!(
                state.running_task.is_some(),
                "stale completion must not clear the promoted turn's task"
            );
            assert_eq!(
                state.running_prompt_id(),
                Some("promoted"),
                "promoted turn still owns the front"
            );
            assert_eq!(
                actor
                    .current_prompt_id
                    .lock()
                    .expect("current_prompt_id mutex poisoned")
                    .as_deref(),
                Some("promoted"),
                "stale completion must not clear the promoted turn's prompt id"
            );
        })
        .await;
}

#[tokio::test]
async fn tool_overrides_update_applies_at_promotion_never_at_enqueue() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let options = echo_agent_sampling_types::XSearchOptions {
                date_bound: Some(
                    echo_agent_sampling_types::SearchDateBound::new(
                        None,
                        Some("2024-03-15".to_string()),
                    )
                    .unwrap(),
                ),
            };
            // A per-turn update that SETS the x_search override to `options`.
            let set_update = || echo_agent_sampling_types::ToolOverridesUpdate {
                x_search: Some(Some(options.clone())),
                web_search: None,
            };
            let expected = echo_agent_sampling_types::ToolOverrides {
                x_search: Some(options.clone()),
                web_search: None,
            };

            let (mut item, prompt_rx) = user_item_with_rx("p1", "alice");
            item.tool_overrides_update = Some(set_update());
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(item);
            }
            assert_eq!(
                *actor.tool_overrides.borrow(),
                None,
                "an enqueued update must not rebind the session before its turn starts"
            );

            actor.handle_remove_queued_prompt("p1", 0, None).await;
            assert_eq!(
                *actor.tool_overrides.borrow(),
                None,
                "a removed prompt's update must never apply"
            );
            let removed = prompt_rx.await.expect("removed prompt resolves its RPC");
            assert!(
                matches!(
                    removed,
                    Ok(crate::session::commands::PromptTurnOk {
                        completion_kind: PromptCompletionKind::RemovedFromQueue,
                        tool_overrides: None,
                        ..
                    })
                ),
                "the removal response echoes the session's standing overrides (none)"
            );

            let (mut promoted, _promoted_rx) = user_item_with_rx("p2", "alice");
            promoted.tool_overrides_update = Some(set_update());
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(promoted);
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.clone().maybe_start_running_task(completion_tx).await;
            assert_eq!(
                actor.tool_overrides.borrow().as_ref(),
                Some(&expected),
                "promotion applies the front prompt's update to the session override"
            );
            assert_eq!(
                actor
                    .resolved_tool_overrides
                    .load_full()
                    .map(|o| (*o).clone()),
                Some(expected.clone()),
                "promotion also republishes the configured cutoff into the cell subagents inherit"
            );

            actor.apply_tool_overrides_update(None);
            assert_eq!(
                actor.tool_overrides.borrow().as_ref(),
                Some(&expected),
                "a prompt with no update leaves the sticky override in place"
            );
            actor.apply_tool_overrides_update(Some(
                echo_agent_sampling_types::ToolOverridesUpdate {
                    x_search: Some(None),
                    web_search: None,
                },
            ));
            assert_eq!(
                *actor.tool_overrides.borrow(),
                None,
                "an explicit clear removes the override"
            );
            assert!(
                actor.resolved_tool_overrides.load().is_none(),
                "clearing the override republishes an empty configured cutoff to the shared cell"
            );
        })
        .await;
}

#[tokio::test]
async fn effective_tool_overrides_echoes_and_gates_on_backend_search() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            // Backend search on, with a bare (unbounded) x_search hosted tool.
            *actor.agent.borrow_mut() =
                test_agent_backend_search(vec![echo_agent_sampling_types::HostedTool::XSearch {
                    options: None,
                }])
                .await;
            actor.supports_backend_search.set(true);
            assert!(
                actor.backend_search_active(),
                "fixture must actually reach the enabled-backend-search path"
            );

            // A standing per-turn cutoff (toDate only).
            let options = echo_agent_sampling_types::XSearchOptions {
                date_bound: Some(
                    echo_agent_sampling_types::SearchDateBound::new(
                        None,
                        Some("2024-03-15".to_string()),
                    )
                    .unwrap(),
                ),
            };
            let expected = echo_agent_sampling_types::ToolOverrides {
                x_search: Some(options.clone()),
                web_search: None,
            };
            *actor.tool_overrides.borrow_mut() = Some(expected.clone());

            assert_eq!(
                actor.effective_tool_overrides(),
                Some(expected.clone()),
                "backend search on ⇒ the applied cutoff echoes back for attestation"
            );
            assert_eq!(
                actor.effective_hosted_tools(),
                vec![echo_agent_sampling_types::HostedTool::XSearch {
                    options: Some(options.clone()),
                }],
                "the wire's XSearch entry carries exactly the bound the echo attests (wire == echo)"
            );

            actor.supports_backend_search.set(false);
            assert!(
                actor.tool_overrides.borrow().is_some(),
                "the standing override is unchanged — only per-model support flipped"
            );
            assert_eq!(
                actor.effective_tool_overrides(),
                None,
                "backend search off ⇒ echo is None: never attest a cutoff the wire never carried"
            );
        })
        .await;
}

/// Moving `web_search` onto the raw-JSON `extra_tool_entries` channel must not leak it past the
/// backend-search gate. `hosted_tools_for_turn` is the only thing that populates a request's
/// `hosted_tools`, and `extra_tool_entries` is derived from that, so a model without server-side
/// search sends no hosted tool on either channel, configured domain policy or not.
#[tokio::test]
async fn unsupported_backend_search_sends_no_hosted_tool_on_either_channel() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let configured = echo_agent_sampling_types::WebSearchOptions {
                allowed_domains: None,
                excluded_domains: Some(vec!["reddit.com".to_string()]),
            };
            *actor.agent.borrow_mut() =
                test_agent_backend_search(vec![echo_agent_sampling_types::HostedTool::WebSearch {
                    options: Some(configured),
                }])
                .await;

            // Model advertises server-side search: the hosted tool rides both channels.
            actor.supports_backend_search.set(true);
            assert!(!actor.hosted_tools_for_turn().is_empty());
            assert_eq!(
                echo_agent_sampling_types::extra_tool_entries(&actor.hosted_tools_for_turn()).len(),
                1
            );

            // Model does not: the gate empties the list before it can reach the wire.
            actor.supports_backend_search.set(false);
            assert!(
                actor.hosted_tools_for_turn().is_empty(),
                "the backend-search gate must drop the hosted tool"
            );
            assert!(
                echo_agent_sampling_types::extra_tool_entries(&actor.hosted_tools_for_turn())
                    .is_empty(),
                "and so no raw-JSON entry is produced to splice"
            );
        })
        .await;
}

/// The `[toolset.web_search]` policy is folded into the agent's hosted tools at build
/// time (see `agent_rebuild`), and it beats agent frontmatter. A per-turn
/// `ToolOverridesUpdate` is a deliberate API-level override, so it intentionally still
/// wins on top of the config policy: the one bypass the config does not close.
#[tokio::test]
async fn per_turn_tool_overrides_win_over_the_config_web_search_policy() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            // The hosted tool as `build_agent` leaves it: config blocklist already folded in.
            let configured = echo_agent_sampling_types::WebSearchOptions {
                allowed_domains: None,
                excluded_domains: Some(vec!["reddit.com".to_string()]),
            };
            *actor.agent.borrow_mut() =
                test_agent_backend_search(vec![echo_agent_sampling_types::HostedTool::WebSearch {
                    options: Some(configured.clone()),
                }])
                .await;
            actor.supports_backend_search.set(true);
            assert!(actor.backend_search_active());

            // No per-turn update: the config policy is what reaches the wire.
            assert_eq!(
                actor.effective_hosted_tools(),
                vec![echo_agent_sampling_types::HostedTool::WebSearch {
                    options: Some(configured.clone()),
                }],
            );

            let per_turn = echo_agent_sampling_types::WebSearchOptions {
                allowed_domains: Some(vec!["docs.echo-agent.invalid".to_string()]),
                excluded_domains: None,
            };
            actor.apply_tool_overrides_update(Some(
                echo_agent_sampling_types::ToolOverridesUpdate {
                    x_search: None,
                    web_search: Some(Some(per_turn.clone())),
                },
            ));

            assert_eq!(
                actor.effective_hosted_tools(),
                vec![echo_agent_sampling_types::HostedTool::WebSearch {
                    options: Some(per_turn.clone()),
                }],
                "an explicit per-turn override replaces the configured policy on the wire"
            );
            assert_eq!(
                actor.effective_tool_overrides(),
                Some(echo_agent_sampling_types::ToolOverrides {
                    x_search: None,
                    web_search: Some(per_turn),
                }),
                "and the echo attests exactly what the wire carried"
            );

            // Clearing the per-turn override falls back to the configured policy.
            actor.apply_tool_overrides_update(Some(
                echo_agent_sampling_types::ToolOverridesUpdate {
                    x_search: None,
                    web_search: Some(None),
                },
            ));
            assert_eq!(
                actor.effective_hosted_tools(),
                vec![echo_agent_sampling_types::HostedTool::WebSearch {
                    options: Some(configured),
                }],
            );
        })
        .await;
}

/// An agent rebuild (model switch) swaps the definition seed, so it must republish the cutoff cell;
/// the fixture keeps `supports_backend_search == false` to also pin that publishing isn't gated on
/// the parent's own search.
#[tokio::test]
async fn agent_rebuild_republishes_the_configured_cutoff() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            assert!(
                !actor.backend_search_active(),
                "fixture must exercise the not-gated-on-backend-search path",
            );
            assert!(
                actor.resolved_tool_overrides.load().is_none(),
                "the default definition seeds no cutoff",
            );

            let seed = echo_agent_sampling_types::ToolOverrides {
                x_search: Some(echo_agent_sampling_types::XSearchOptions {
                    date_bound: Some(
                        echo_agent_sampling_types::SearchDateBound::new(
                            None,
                            Some("2020-01-01".to_string()),
                        )
                        .unwrap(),
                    ),
                }),
                web_search: None,
            };
            let mut seeded = echo_agent_core::AgentDefinition::default_echo_agent_build();
            seeded.tool_overrides = Some(seed.clone());
            actor
                .handle_rebuild_agent_for_definition(seeded)
                .await
                .expect("zero-turn rebuild should succeed");
            assert_eq!(
                actor
                    .resolved_tool_overrides
                    .load_full()
                    .map(|o| (*o).clone()),
                Some(seed),
                "rebuild must republish the new definition seed for subagent inheritance",
            );

            // Rebuilding to a seedless definition must clear the cell; a stale bound is a divergence.
            actor
                .handle_rebuild_agent_for_definition(
                    echo_agent_core::AgentDefinition::default_echo_agent_build(),
                )
                .await
                .expect("second rebuild should succeed");
            assert!(
                actor.resolved_tool_overrides.load().is_none(),
                "rebuild to a seedless definition must not leave a stale cutoff",
            );
        })
        .await;
}

/// A spawned subagent is seeded via `SetToolOverrides` before its first prompt. The seed must
/// publish the inheritance cell immediately, with no turn run, so the child's own subagents read
/// the inherited cutoff regardless of turn timing.
#[tokio::test]
async fn set_tool_overrides_publishes_the_inheritance_cell_before_any_turn() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            assert!(actor.resolved_tool_overrides.load().is_none());
            let cutoff = echo_agent_sampling_types::ToolOverrides {
                x_search: Some(echo_agent_sampling_types::XSearchOptions {
                    date_bound: Some(
                        echo_agent_sampling_types::SearchDateBound::new(
                            None,
                            Some("2020-01-01".to_string()),
                        )
                        .unwrap(),
                    ),
                }),
                web_search: None,
            };
            actor.set_tool_overrides(cutoff.clone());
            assert_eq!(
                actor
                    .resolved_tool_overrides
                    .load_full()
                    .map(|o| (*o).clone()),
                Some(cutoff),
                "seeding must publish the inheritance cell before any turn runs",
            );
        })
        .await;
}

/// Queue a plain-text send-now prompt, returning the shell's cancel decision.
async fn queue_text_send_now(actor: &SessionActor, id: &str) -> bool {
    let (respond_to, _rx) = oneshot::channel();
    actor
        .queue_input(QueueInputRequest {
            send_now: true,
            ..queue_input_request(
                vec![acp::ContentBlock::Text(acp::TextContent::new(id))],
                id,
                respond_to,
            )
        })
        .await
}

/// Repeated "Enter to send now" used to cancel the just-promoted previous
/// prompt before it made a model call, invisibly destroying its message.
#[tokio::test(flavor = "current_thread")]
async fn queue_send_now_never_cancels_uncommitted_front() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let (mut m1, _m1_rx) = user_item_with_rx("m1", "client");
            m1.send_now = true;
            let m2 = user_item("m2", "client");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(m1);
                state.pending_inputs.push_back(m2);
                state.running_task = Some(running_task_stub("m1"));
                state.front_message_committed = false;
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("m1".into());

            let cancel = actor
                .handle_interject_queued_prompt("m2", 0, Some("client"), None)
                .await;
            assert!(!cancel, "an uncommitted front must not be cancelled");
            let state = actor.state.lock().await;
            let order: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(order, vec!["m1", "m2"], "m2 promotes right behind m1");
        })
        .await;
}

/// An explicit send-now against an uncommitted front queues behind it instead
/// of cancelling it; a committed front still cancels.
#[tokio::test(flavor = "current_thread")]
async fn queue_input_send_now_spares_uncommitted_front() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let (m1, _m1_rx) = user_item_with_rx("m1", "client");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(m1);
                state.running_task = Some(running_task_stub("m1"));
                state.front_message_committed = false;
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("m1".into());

            assert!(
                !queue_text_send_now(&actor, "sn-2").await,
                "an explicit send-now must not cancel an uncommitted front"
            );
            {
                let state = actor.state.lock().await;
                let order: Vec<&str> = state
                    .pending_inputs
                    .iter()
                    .map(|i| i.prompt_id.as_str())
                    .collect();
                assert_eq!(order, vec!["m1", "sn-2"], "FIFO behind the front");
            }

            actor.state.lock().await.front_message_committed = true;
            assert!(
                queue_text_send_now(&actor, "sn-3").await,
                "a committed front still cancels"
            );
        })
        .await;
}

/// A buffered interjection survives a send-now cancel as a front-queued
/// prompt turn instead of being cleared.
#[tokio::test(flavor = "current_thread")]
async fn send_now_cancel_flushes_buffered_interjections_as_prompts() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let (wait_turn, mut wait_rx) = user_item_with_rx("wait-turn", "client");
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(wait_turn);
                state.running_task = Some(running_task_stub("wait-turn"));
                state.front_message_committed = true;
                let mut item = user_item("sn-1", "client");
                item.send_now = true;
                state.pending_inputs.insert(1, item);
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("wait-turn".into());
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "buffered steer".to_string(),
                attachments: vec![],
            });

            let mut replay_buffer = ReplayBuffer::new(None);
            actor.cancel_turn_for_send_now(&mut replay_buffer).await;

            assert!(
                matches!(
                    wait_rx.try_recv(),
                    Ok(Ok(crate::session::commands::PromptTurnOk {
                        stop_reason: acp::StopReason::Cancelled,
                        ..
                    }))
                ),
                "the running turn itself is cancelled (send-now semantics)"
            );
            assert!(actor.pending_interjections.is_empty());
            let state = actor.state.lock().await;
            let texts: Vec<String> = state
                .pending_inputs
                .iter()
                .map(|i| match i.prompt_blocks.first() {
                    Some(acp::ContentBlock::Text(t)) => t.text.clone(),
                    other => panic!("expected text block, got {other:?}"),
                })
                .collect();
            assert_eq!(
                texts,
                vec!["buffered steer".to_string(), "text for sn-1".to_string()],
                "the interjection runs next, ahead of the send-now prompt"
            );
            assert!(
                is_interject_fallback(&state.pending_inputs[0].prompt_id),
                "converted interjections use the persist-only fallback prefix"
            );
        })
        .await;
}

/// Welds `state.rewindable` to its real arm (promoter) and disarm (first
/// update) sites so moving either fails the suite.
#[tokio::test(flavor = "current_thread")]
async fn promoter_arms_rewind_window_and_first_update_disarms_it() {
    fn agent_msg_update(text: &str) -> acp::SessionUpdate {
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(text.to_string()),
        )))
    }

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("m1", "client"));
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.clone().maybe_start_running_task(completion_tx).await;

            // Race-free: the promoter spawned the task last and nothing has awaited since.
            {
                let state = actor.state.try_lock().expect("no await since promote");
                assert_eq!(state.running_prompt_id(), Some("m1"));
                assert!(
                    state.rewindable,
                    "the promoter must arm the first-output window"
                );
            }

            // Intake diagnostics are not output — they must leave the window open.
            for update in [
                EchoAgentSessionUpdate::HookExecution {
                    event_name: "user_prompt_submit".into(),
                    tool_name: None,
                    prompt_id: Some("m1".into()),
                    runs: vec![],
                },
                EchoAgentSessionUpdate::ImageCompressed {
                    images: vec![],
                    message: "resized".into(),
                },
                EchoAgentSessionUpdate::ImageDropped { notes: vec![] },
            ] {
                actor.send_echo_agent_notification(update).await;
                assert!(
                    actor.state.try_lock().expect("uncontended").rewindable,
                    "prompt-intake diagnostics must not close the rewind window"
                );
            }

            actor
                .send_update(agent_msg_update("first delta"), Some(1))
                .await;
            assert!(
                !actor.state.try_lock().expect("uncontended").rewindable,
                "the first outbound update must disarm the first-output window"
            );
        })
        .await;
}

/// Claimed rewind pops OLD only; a queued NEW stays pending.
#[tokio::test(flavor = "current_thread")]
async fn claimed_rewind_leaves_queued_next_prompt_untouched() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let (old_item, mut old_rx) =
                input_with_origin_rx("old-0", crate::session::PromptOrigin::User);
            let (new_item, mut new_rx) =
                input_with_origin_rx("new-1", crate::session::PromptOrigin::User);
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(old_item);
                state.pending_inputs.push_back(new_item);
                state.running_task = Some(running_task_stub("old-0"));
                state.rewindable = true;
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("old-0".into());

            let outcome = actor
                .cancel_running_task(crate::session::CancelOptions {
                    cancel_subagents: true,
                    history: crate::session::CancelHistoryDisposition::RewindIfNoOutput {
                        prompt_id: Some("old-0".into()),
                    },
                    trigger: Some(crate::session::CancelTrigger::CtrlC),
                    user_initiated: true,
                    ..Default::default()
                })
                .await;

            assert!(outcome.turn_stopped);
            assert!(
                matches!(
                    old_rx.try_recv(),
                    Ok(Ok(crate::session::commands::PromptTurnOk {
                        completion_kind: PromptCompletionKind::Rewound,
                        ..
                    }))
                ),
                "OLD must resolve as Rewound"
            );
            assert!(
                new_rx.try_recv().is_err(),
                "NEW's respond_to must stay pending — the claimed rewind must not resolve it"
            );
            let state = actor.state.lock().await;
            assert_eq!(
                state.pending_inputs.front().map(|f| f.prompt_id.as_str()),
                Some("new-1"),
                "NEW must be the next front after OLD is popped"
            );
        })
        .await;
}

/// Stale rewind `promptId` is a no-op: NEW's running turn is untouched.
#[tokio::test(flavor = "current_thread")]
async fn stale_rewind_prompt_id_does_not_cancel_promoted_front() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let (item, mut rx) = input_with_origin_rx("new-1", crate::session::PromptOrigin::User);
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(item);
                state.running_task = Some(running_task_stub("new-1"));
                state.rewindable = true;
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("new-1".into());

            let outcome = actor
                .cancel_running_task(crate::session::CancelOptions {
                    cancel_subagents: true,
                    history: crate::session::CancelHistoryDisposition::RewindIfNoOutput {
                        prompt_id: Some("old-0".into()),
                    },
                    trigger: Some(crate::session::CancelTrigger::CtrlC),
                    user_initiated: true,
                    ..Default::default()
                })
                .await;

            assert!(!outcome.turn_stopped, "a stale rewind must not stop NEW");
            assert!(
                rx.try_recv().is_err(),
                "NEW's respond_to must stay pending — a stale rewind must not resolve it"
            );
            let state = actor.state.lock().await;
            let task = state
                .running_task
                .as_ref()
                .expect("NEW's task slot must survive");
            assert!(
                !task.handle.is_finished(),
                "NEW's running task must not be aborted"
            );
            assert_eq!(
                state.pending_inputs.front().map(|f| f.prompt_id.as_str()),
                Some("new-1"),
                "NEW must stay at the queue front"
            );
            assert!(state.rewindable, "NEW's own rewind window must survive");
            assert!(
                !state.notifications_suppressed,
                "a stale rewind must not arm the stop-gesture wake barrier"
            );
        })
        .await;
}

/// Welds the flag to the real promoter and commit sites so moving either
/// fails the suite.
#[tokio::test(flavor = "current_thread")]
async fn promoter_clears_committed_flag_and_handle_prompt_sets_it() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let actor = actor_with_persistence_drain().await;
            {
                let mut state = actor.state.lock().await;
                state.front_message_committed = true;
                state.pending_inputs.push_back(user_item("m1", "client"));
            }
            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.clone().maybe_start_running_task(completion_tx).await;

            // Abort the turn unpolled: polling `handle_prompt` here overflows
            // the default 2 MB test-thread stack in debug builds.
            {
                let state = actor.state.try_lock().expect("no await since promote");
                assert_eq!(state.running_prompt_id(), Some("m1"));
                assert!(
                    !state.front_message_committed,
                    "the promoter must clear the committed flag"
                );
                if let Some(task) = state.running_task.as_ref() {
                    task.handle.abort();
                }
            }

            // The persist ack resolves after the history commit, so the flag is set once it fires.
            let actor_for_prompt = actor.clone();
            let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
            let prompt_task = tokio::task::spawn_local(async move {
                actor_for_prompt
                    .handle_prompt(
                        "m2",
                        vec![acp::ContentBlock::Text(acp::TextContent::new("hello"))],
                        PromptMode::Agent,
                        /* trace_gcs_config */ None,
                        /* artifact_tracker */ None,
                        /* client_identifier */ None,
                        /* screen_mode */ None,
                        /* verbatim */ true,
                        /* send_now */ false,
                        /* json_schema */ None,
                        Some(ack_tx),
                        /* parsed_prompt_tx */ None,
                    )
                    .await
            });
            assert!(ack_rx.await.is_ok(), "persist ack should resolve");
            assert!(
                actor.state.lock().await.front_message_committed,
                "handle_prompt must set the committed flag at the history commit"
            );
            prompt_task.abort();
        })
        .await;
}

/// A terminal whose commands never finish, holding a bash turn mid-run.
struct NeverFinishesTerminal;

#[async_trait::async_trait]
impl crate::terminal::AsyncTerminalRunner for NeverFinishesTerminal {
    async fn run(
        &self,
        _request: crate::terminal::runner::TerminalRunRequest,
    ) -> Result<crate::terminal::runner::TerminalRunResult, crate::terminal::runner::TerminalError>
    {
        std::future::pending().await
    }
}

/// The bash path must set the committed flag before running the command, or a
/// send-now during a long command would queue behind it forever.
#[tokio::test(flavor = "current_thread")]
async fn bash_turn_sets_committed_flag_before_running_the_command() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (gateway_tx, _gateway_rx) =
                tokio::sync::mpsc::unbounded_channel::<echo_agent_acp::AcpClientMessage>();
            let (persistence_tx, _prx) = tokio::sync::mpsc::unbounded_channel::<PersistenceMsg>();
            let (actor, _ev) = create_test_actor_with_terminal(
                0,
                256_000,
                85,
                gateway_tx,
                persistence_tx,
                std::sync::Arc::new(NeverFinishesTerminal),
            )
            .await;
            let actor = std::sync::Arc::new(actor);
            actor.state.lock().await.front_message_committed = false;

            let bash_actor = actor.clone();
            let bash_turn = tokio::task::spawn_local(async move {
                bash_actor
                    .handle_direct_bash_command(
                        "bash-1",
                        "sleep 30".to_string(),
                        &[acp::ContentBlock::Text(acp::TextContent::new("!sleep 30"))],
                    )
                    .await
            });

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !actor.state.lock().await.front_message_committed {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the bash path must mark the commit before the command finishes"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            assert!(
                !bash_turn.is_finished(),
                "the command never finishes, so the flag was set mid-run"
            );
            bash_turn.abort();
        })
        .await;
}

/// Builtin turns carry no user message; they commit at intake so a send-now
/// can cancel a long-running builtin like `/compact`.
#[tokio::test(flavor = "current_thread")]
async fn builtin_turn_commits_immediately() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            actor.state.lock().await.front_message_committed = false;

            let _ = actor
                .execute_builtin_slash_command(BuiltinAction::ContextInfo)
                .await;

            assert!(
                actor.state.lock().await.front_message_committed,
                "a builtin turn must commit at intake"
            );
        })
        .await;
}

/// `rewindIfPristine` must not pop a promoted interjection-fallback front —
/// it is not the prompt the client rewound; it takes the normal cancel.
#[tokio::test(flavor = "current_thread")]
async fn rewind_if_pristine_never_pops_an_interjection_fallback_front() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            let (item, mut rx) =
                input_with_origin_rx("interject-fallback-1", crate::session::PromptOrigin::User);
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(item);
                state.running_task = Some(running_task_stub("interject-fallback-1"));
                state.rewindable = true;
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("interject-fallback-1".into());

            let _ = actor
                .cancel_running_task(crate::session::CancelOptions {
                    history: crate::session::CancelHistoryDisposition::RewindIfNoOutput {
                        prompt_id: None,
                    },
                    trigger: Some(crate::session::CancelTrigger::Esc),
                    user_initiated: true,
                    ..Default::default()
                })
                .await;

            assert!(
                matches!(
                    rx.try_recv(),
                    Ok(Ok(crate::session::commands::PromptTurnOk {
                        completion_kind: PromptCompletionKind::Cancelled { .. },
                        ..
                    }))
                ),
                "a fallback front resolves through the normal cancel, never the rewind pop"
            );
        })
        .await;
}

/// Yield predicate: skip synthetics/running front; held first user blocks.
#[tokio::test]
async fn goal_yield_predicate_ignores_synthetics_and_the_running_front() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.pending_inputs.push_back(
                    input_with_origin_rx(
                        "goal-summary-1",
                        crate::session::PromptOrigin::GoalSummary,
                    )
                    .0,
                );
            }
            assert!(
                !actor.has_runnable_queued_user_row().await,
                "the running front and synthetic rows are not queued user work"
            );

            actor
                .state
                .lock()
                .await
                .pending_inputs
                .push_back(user_item("held", "A"));
            assert!(
                actor.has_runnable_queued_user_row().await,
                "a user row queued behind the running turn is queued user work"
            );

            // Mid-edit row must not trigger yield (promote is blocked on hold).
            actor
                .state
                .lock()
                .await
                .edit_holds
                .insert("held".into(), std::time::Instant::now());
            assert!(
                !actor.has_runnable_queued_user_row().await,
                "a row under composer edit is not yieldable user work"
            );

            // FIFO: unheld row behind a held front must not re-arm the yield.
            actor
                .state
                .lock()
                .await
                .pending_inputs
                .push_back(user_item("behind-held", "A"));
            assert!(
                !actor.has_runnable_queued_user_row().await,
                "an unheld row behind a held front must not trigger the yield"
            );

            actor.state.lock().await.edit_holds.remove("held");
            assert!(
                actor.has_runnable_queued_user_row().await,
                "clearing the hold re-arms the yield"
            );
        })
        .await;
}

/// A leaked (expired) hold on the first user row no longer blocks the yield.
/// The leaked-hold GC cannot run during the in-turn goal loop, so the predicate
/// must apply the same TTL itself or a crashed editor starves the queue.
#[tokio::test]
async fn goal_yield_ignores_expired_edit_hold() {
    use crate::session::acp_session::EDIT_HOLD_TTL;
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.pending_inputs.push_back(user_item("held", "A"));
                state
                    .edit_holds
                    .insert("held".into(), std::time::Instant::now());
            }
            assert!(
                !actor.has_runnable_queued_user_row().await,
                "a live hold on the first user row blocks the yield"
            );

            super::backdate_edit_hold(
                &mut actor.state.lock().await.edit_holds,
                "held",
                EDIT_HOLD_TTL + std::time::Duration::from_secs(1),
            );
            assert!(
                actor.has_runnable_queued_user_row().await,
                "a hold older than the TTL is expired and no longer blocks the yield"
            );
        })
        .await;
}

/// A queued goal continuation is detected so a user turn can skip
/// run_goal_round_end (and still hit run_stop_gate) instead of driving the
/// goal loop and resuming the goal a second time.
#[tokio::test]
async fn has_pending_goal_continuation_detects_queued_continuation() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            assert!(
                !actor.has_pending_goal_continuation().await,
                "an empty queue has no pending continuation"
            );

            actor
                .state
                .lock()
                .await
                .pending_inputs
                .push_back(user_item("u1", "alice"));
            assert!(
                !actor.has_pending_goal_continuation().await,
                "a queued user row is not a goal continuation"
            );

            actor.state.lock().await.pending_inputs.push_back(
                input_with_origin_rx("goal-summary-1", crate::session::PromptOrigin::GoalSummary).0,
            );
            assert!(
                actor.has_pending_goal_continuation().await,
                "a queued GoalSummary is a pending continuation"
            );
        })
        .await;
}

/// Stale GoalSummary front is dropped at promote when the goal is inactive.
#[tokio::test(flavor = "current_thread")]
async fn stale_goal_summary_front_dropped_when_goal_inactive() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(
                    input_with_origin_rx(
                        "goal-summary-1",
                        crate::session::PromptOrigin::GoalSummary,
                    )
                    .0,
                );
                state.pending_inputs.push_back(user_item("p1", "alice"));
            }

            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.clone().maybe_start_running_task(completion_tx).await;

            // Abort the promoted turn unpolled (see
            // `promoter_clears_committed_flag_and_handle_prompt_sets_it`).
            let state = actor.state.try_lock().expect("no await since promote");
            assert_eq!(
                state.running_prompt_id(),
                Some("p1"),
                "the user's queued prompt runs; the stale continuation does not"
            );
            assert!(
                !state
                    .pending_inputs
                    .iter()
                    .any(|i| i.prompt_id == "goal-summary-1"),
                "the stale continuation left the queue"
            );
            if let Some(task) = state.running_task.as_ref() {
                task.handle.abort();
            }
        })
        .await;
}

/// Active-goal GoalSummary front is not stale and still promotes.
#[tokio::test(flavor = "current_thread")]
async fn goal_summary_front_promotes_while_goal_active() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            set_goal_harness_for_tests(&actor);
            actor.goal_tracker.lock().create_goal(
                "goal".into(),
                "objective".into(),
                None,
                0,
                "2026-01-01T00:00:00Z".into(),
                None,
            );
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(
                    input_with_origin_rx(
                        "goal-summary-1",
                        crate::session::PromptOrigin::GoalSummary,
                    )
                    .0,
                );
            }

            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor.clone().maybe_start_running_task(completion_tx).await;

            let state = actor.state.try_lock().expect("no await since promote");
            assert_eq!(
                state.running_prompt_id(),
                Some("goal-summary-1"),
                "an Active goal's continuation is not stale"
            );
            if let Some(task) = state.running_task.as_ref() {
                task.handle.abort();
            }
        })
        .await;
}

/// The full yield ordering: with a user row queued behind a running goal turn, the yield's
/// success turn end re-arms the continuation BEHIND that row, the row promotes and runs as the
/// next turn, and the continuation promotes after it so the goal resumes. Pins the ordering a
/// refactor of the round loop, `handle_turn_end`, or promote is most likely to break.
#[tokio::test(flavor = "current_thread")]
async fn goal_yield_runs_queued_row_next_then_resumes_goal() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            set_goal_harness_for_tests(&actor);
            actor.goal_tracker.lock().create_goal(
                "goal".into(),
                "objective".into(),
                None,
                0,
                "2026-01-01T00:00:00Z".into(),
                None,
            );
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(user_item("goal-round", "alice"));
                state.running_task = Some(running_task_stub("goal-round"));
                state.pending_inputs.push_back(user_item("p1", "alice"));
            }
            assert!(
                actor.has_runnable_queued_user_row().await,
                "the queued row arms the yield in the goal round loop"
            );

            // The yield breaks out of the round loop as a success; this is the
            // turn end it reaches.
            actor.handle_turn_end(true, false).await;
            let continuation_id = {
                let mut state = actor.state.lock().await;
                let order: Vec<String> = state
                    .pending_inputs
                    .iter()
                    .map(|i| i.prompt_id.clone())
                    .collect();
                assert_eq!(
                    order.len(),
                    3,
                    "turn end queued one continuation: {order:?}"
                );
                assert_eq!(order[1], "p1", "the user row stays ahead: {order:?}");
                assert!(
                    order[2].starts_with("goal-summary-"),
                    "the continuation re-arms behind the user row: {order:?}"
                );
                // The yielded turn finishes: its front row drains and the task
                // slot clears, as after any completed turn.
                if let Some(task) = state.running_task.take() {
                    task.handle.abort();
                }
                state.pending_inputs.retain(|i| i.prompt_id != "goal-round");
                order[2].clone()
            };

            let (completion_tx, _completion_rx) = tokio::sync::mpsc::unbounded_channel();
            actor
                .clone()
                .maybe_start_running_task(completion_tx.clone())
                .await;
            {
                // Abort the promoted turn unpolled (see
                // `promoter_clears_committed_flag_and_handle_prompt_sets_it`).
                let mut state = actor.state.try_lock().expect("no await since promote");
                assert_eq!(
                    state.running_prompt_id(),
                    Some("p1"),
                    "the queued user row runs as the next turn"
                );
                if let Some(task) = state.running_task.take() {
                    task.handle.abort();
                }
                state.pending_inputs.retain(|i| i.prompt_id != "p1");
            }

            actor.clone().maybe_start_running_task(completion_tx).await;
            let state = actor.state.try_lock().expect("no await since promote");
            assert_eq!(
                state.running_prompt_id(),
                Some(continuation_id.as_str()),
                "the goal resumes behind the user row"
            );
            if let Some(task) = state.running_task.as_ref() {
                task.handle.abort();
            }
        })
        .await;
}
