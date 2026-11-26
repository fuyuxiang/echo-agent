//! Mid-turn interjection images: queue-row harvest and the
//! `drain_pending_interjections` image pipeline.
use super::support::*;
use super::*;

/// Send-now of an image-bearing queued prompt keeps its `ContentBlock::Image`s on the promoted row.
#[tokio::test]
async fn queue_send_now_keeps_prompt_block_images_on_promoted_row() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.front_message_committed = true;
                let mut item = user_item("p1", "A");
                item.prompt_blocks
                    .push(acp::ContentBlock::Image(test_image_content()));
                state.pending_inputs.push_back(item);
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            let cancel = actor
                .handle_interject_queued_prompt("p1", 0, None, None)
                .await;
            assert!(cancel, "promotion behind a running turn requests cancel");

            let state = actor.state.lock().await;
            let promoted = state
                .pending_inputs
                .iter()
                .find(|i| i.prompt_id == "p1")
                .expect("promoted row stays queued to run next");
            assert_eq!(
                promoted
                    .prompt_blocks
                    .iter()
                    .filter(|b| matches!(b, acp::ContentBlock::Image(_)))
                    .count(),
                1,
                "image blocks must survive promotion"
            );
            assert!(
                actor.pending_interjections.is_empty(),
                "send-now never buffers into the running turn"
            );
        })
        .await;
}

#[tokio::test]
async fn goal_send_now_routes_text_and_image_as_planner_steering_and_interjection() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
            }
            actor.goal_tracker.lock().create_goal(
                "goal".into(),
                "objective".into(),
                None,
                0,
                "2026-01-01T00:00:00Z".into(),
                None,
            );
            let cancel = tokio_util::sync::CancellationToken::new();
            actor.goal_tracker.lock().start_planner_run(cancel.clone());

            let (respond_to, response_rx) = tokio::sync::oneshot::channel();
            let cancelled = actor
                .queue_input(QueueInputRequest {
                    send_now: true,
                    ..queue_input_request(
                        vec![
                            acp::ContentBlock::Text(acp::TextContent::new("steer")),
                            acp::ContentBlock::Image(test_image_content()),
                        ],
                        "steer-image",
                        respond_to,
                    )
                })
                .await;

            assert!(!cancelled);
            assert!(cancel.is_cancelled());
            assert!(matches!(
                response_rx.await.unwrap().unwrap().completion_kind,
                PromptCompletionKind::RemovedFromQueue
            ));
            let run = actor.goal_tracker.lock().take_planner_run().unwrap();
            assert_eq!(run.steering, ["steer"]);
            let interjections = actor.pending_interjections.drain_all();
            assert_eq!(interjections.len(), 1);
            assert_eq!(interjections[0].text, "steer");
            assert_eq!(interjections[0].attachments.len(), 1);
        })
        .await;
}

/// Draining an image-bearing interjection injects structured
/// `ContentPart::Image` parts (base64 data URL) on the synthetic user
/// message, preserving `SyntheticReason::Interjection`.
#[tokio::test]
async fn drain_interjection_with_images_attaches_image_parts() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "look at [Image #1]".to_string(),
                attachments: vec![test_image_content()],
            });

            assert!(actor.drain_pending_interjections().await);

            let conversation = actor.chat_state_handle.get_conversation().await;
            let user_item = match conversation.last() {
                Some(ConversationItem::User(u)) => u,
                other => panic!("conversation tail must be a user item, got: {other:?}"),
            };
            assert_eq!(
                user_item.synthetic_reason,
                Some(SyntheticReason::Interjection)
            );
            let image_urls: Vec<&str> = user_item
                .content
                .iter()
                .filter_map(|p| match p {
                    echo_agent_sampling_types::ContentPart::Image { url } => Some(url.as_ref()),
                    _ => None,
                })
                .collect();
            assert_eq!(image_urls.len(), 1, "image part must be attached");
            assert!(
                image_urls[0].starts_with("data:image/"),
                "inline base64 data URL expected, got {}",
                &image_urls[0][..image_urls[0].len().min(32)]
            );
            let text = conversation.last().unwrap().text_content();
            assert!(
                text.contains("[Image #1]") && text.contains("<user_query>"),
                "placeholder text must survive in the wrapped query, got: {text}"
            );
        })
        .await;
}

/// The drain strips `[Image #N: <path>]` → `[Image #N]` before the text
/// reaches the model — same gate as the prompt path. Covers raw text from
/// legacy clients AND the queue-interject harvest (raw `queue_meta.text`).
#[tokio::test]
async fn drain_interjection_strips_placeholder_paths_from_text() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "look at [Image #1: /tmp/secret/x.png] please".to_string(),
                attachments: vec![test_image_content()],
            });

            assert!(actor.drain_pending_interjections().await);

            let conversation = actor.chat_state_handle.get_conversation().await;
            let text = conversation.last().expect("user item").text_content();
            assert!(
                text.contains("[Image #1]"),
                "bare placeholder must survive, got: {text}"
            );
            assert!(
                !text.contains("/tmp/secret/x.png"),
                "path must be stripped from the model-visible text, got: {text}"
            );
        })
        .await;
}

/// Draining an interjection whose text is a skill slash invocation appends
/// the loaded `<skill_information>` envelope after the wrapped
/// `<user_query>` — send-now of a queued `/skill` row (and a typed `/skill`
/// interjection) must not reach the model unexpanded.
#[tokio::test]
async fn drain_interjection_expands_skill_slash_reference() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("SKILL.md");
            std::fs::write(&path, "Find sessions matching $ARGUMENTS").unwrap();
            let skill = echo_agent_tools::implementations::skills::types::SkillInfo {
                name: "find-session".to_owned(),
                description: "Find past sessions".to_owned(),
                path: path.to_string_lossy().into_owned(),
                ..Default::default()
            };
            actor
                .agent
                .borrow()
                .tool_bridge()
                .clone()
                .seed_skill_discovery(
                    Some(std::path::PathBuf::from("/tmp")),
                    None,
                    vec![skill],
                    None,
                    Some(256_000),
                    None,
                    echo_agent_tools::types::compat::CompatConfig::default(),
                )
                .await;

            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "/find-session foo".to_string(),
                attachments: vec![],
            });
            assert!(actor.drain_pending_interjections().await);

            let conversation = actor.chat_state_handle.get_conversation().await;
            let text = conversation.last().expect("user item").text_content();
            assert!(
                text.contains("<user_query>\n/find-session foo\n</user_query>"),
                "raw slash text stays the visible query, got: {text}"
            );
            let query_end = text.find("</user_query>").expect("wrapped query");
            let envelope = text
                .find("<skill_information>")
                .unwrap_or_else(|| panic!("skill envelope must be appended, got: {text}"));
            assert!(
                query_end < envelope,
                "envelope must follow the query, got: {text}"
            );
            assert!(
                text.contains("Find sessions matching foo"),
                "SKILL.md body with substituted args must ride along, got: {text}"
            );

            // A steering interjection that only MENTIONS the skill mid-text
            // (no leading slash) stays untouched — mirrors turn-start
            // gating, where "don't run /commit yet" is not an invocation.
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "don't run /find-session yet".to_string(),
                attachments: vec![],
            });
            assert!(actor.drain_pending_interjections().await);
            let conversation = actor.chat_state_handle.get_conversation().await;
            let text = conversation.last().expect("user item").text_content();
            assert!(
                !text.contains("<skill_information>"),
                "non-leading slash mentions must not grow an envelope, got: {text}"
            );
        })
        .await;
}

/// `format_interjection`'s large-prompt truncation applies to the TEXT only —
/// image data rides structurally and is never truncated or inlined.
#[tokio::test]
async fn drain_interjection_truncation_never_touches_image_data() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _gateway_rx) = build_actor().await;
            let original_image = test_image_content();
            // Way over LARGE_PROMPT_THRESHOLD so the text path truncates.
            let huge_text = "x".repeat(3_000_000);
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: huge_text,
                attachments: vec![original_image.clone()],
            });

            assert!(actor.drain_pending_interjections().await);

            let conversation = actor.chat_state_handle.get_conversation().await;
            let user_item = match conversation.last() {
                Some(ConversationItem::User(u)) => u,
                other => panic!("conversation tail must be a user item, got: {other:?}"),
            };
            let text = conversation.last().unwrap().text_content();
            assert!(text.contains("[truncated]"), "oversized text must truncate");
            let image_url = user_item
                .content
                .iter()
                .find_map(|p| match p {
                    echo_agent_sampling_types::ContentPart::Image { url } => Some(url.as_ref()),
                    _ => None,
                })
                .expect("image part must survive truncation");
            assert!(
                image_url.ends_with(&original_image.data),
                "image payload must be byte-identical (never truncated)"
            );
        })
        .await;
}

/// An interjection converted to a fallback prompt turn lands FRONT of the
/// queue (send-now beats queued-for-later), carries the text + image blocks,
/// and uses the persist-only `interject-fallback-` prompt-id prefix.
#[tokio::test]
async fn interjection_fallback_prompt_queues_front_with_prefix() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(user_item("queued-later", "A"));
            }

            actor
                .queue_interjection_fallback_prompt(
                    "steer now".to_string(),
                    vec![test_image_content()],
                    true,
                )
                .await;

            let state = actor.state.lock().await;
            assert_eq!(state.pending_inputs.len(), 2);
            let front = state.pending_inputs.front().expect("front item");
            assert!(
                front.prompt_id.starts_with("interject-fallback-"),
                "fallback prompt id must carry the persist-only prefix, got {}",
                front.prompt_id
            );
            assert!(
                matches!(
                    front.prompt_blocks.first(),
                    Some(acp::ContentBlock::Text(t)) if t.text == "steer now"
                ),
                "text block first"
            );
            assert!(
                matches!(
                    front.prompt_blocks.get(1),
                    Some(acp::ContentBlock::Image(_))
                ),
                "image blocks ride along"
            );
            assert!(front.queue_meta.is_none(), "not a shared-queue row");
            assert_eq!(
                state.pending_inputs[1].prompt_id, "queued-later",
                "previously queued prompt stays behind the send-now text"
            );
        })
        .await;
}

/// Interjections that miss the completed turn's final drain are flushed into
/// fallback prompt turns — front of the queue, original order — instead of
/// stranding in `pending_interjections` (the queue-jam: pager said
/// "Interjection sent" but the message was never sent).
#[tokio::test]
async fn flush_stranded_interjections_converts_to_front_prompts_in_order() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state
                    .pending_inputs
                    .push_back(user_item("queued-later", "A"));
            }
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "first steer".to_string(),
                attachments: vec![],
            });
            actor.pending_interjections.push(PendingInterjection {
                scope: None,
                text: "second steer".to_string(),
                attachments: vec![],
            });

            assert_eq!(actor.flush_stranded_interjections().await, 2);
            assert!(
                actor.pending_interjections.is_empty(),
                "flush must drain the buffer"
            );

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
                vec![
                    "first steer".to_string(),
                    "second steer".to_string(),
                    "text for queued-later".to_string()
                ],
                "stranded interjections run next, in arrival order"
            );
        })
        .await;
}

/// An empty buffer flushes to nothing (no phantom turns).
#[tokio::test]
async fn flush_stranded_interjections_noop_when_empty() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            assert_eq!(actor.flush_stranded_interjections().await, 0);
            assert!(actor.state.lock().await.pending_inputs.is_empty());
        })
        .await;
}

/// Review fix: front placement never displaces a pinned running front — the
/// fallback item lands right behind it when a promotion raced the check.
#[tokio::test]
async fn fallback_prompt_lands_behind_running_front() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut state = actor.state.lock().await;
                state.pending_inputs.push_back(user_item("running", "A"));
                state.running_task = Some(running_task_stub("running"));
                state.pending_inputs.push_back(user_item("later", "A"));
            }
            *actor
                .current_prompt_id
                .lock()
                .expect("current_prompt_id mutex poisoned") = Some("running".into());

            actor
                .queue_interjection_fallback_prompt("urgent".to_string(), vec![], true)
                .await;

            let state = actor.state.lock().await;
            let ids: Vec<&str> = state
                .pending_inputs
                .iter()
                .map(|i| i.prompt_id.as_str())
                .collect();
            assert_eq!(ids[0], "running", "running front stays pinned");
            assert!(
                ids[1].starts_with("interject-fallback-"),
                "fallback lands right behind the running front, got {ids:?}"
            );
            assert_eq!(ids[2], "later");
        })
        .await;
}

/// A fallback prompt turn created while plan mode is active must not escape
/// the plan gate: it carries `PromptMode::Plan`.
#[tokio::test]
async fn fallback_prompt_respects_active_plan_mode() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let (actor, _rx) = build_actor().await;
            {
                let mut tracker = actor.plan_mode.lock();
                tracker.enter_pending();
                tracker.activate();
            }

            actor
                .queue_interjection_fallback_prompt("plan steer".to_string(), vec![], true)
                .await;

            let state = actor.state.lock().await;
            let front = state.pending_inputs.front().expect("fallback queued");
            assert_eq!(
                front.prompt_mode,
                crate::session::plan_mode::PromptMode::Plan,
                "fallback turn must stay inside plan mode"
            );
        })
        .await;
}
