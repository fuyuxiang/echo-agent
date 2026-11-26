#![cfg_attr(rustfmt, rustfmt::skip)]
    use super::*;

    // ── apply_session_event ────────────────────────────────────────────

    #[test]
    fn apply_compaction_started_sets_activity() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.in_flight_prompt = Some(InFlightPrompt {
            text: "hi".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(1),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        let update = EchoAgentSessionUpdate::AutoCompactStarted {
            tokens_used: 90000,
            context_window: 131072,
            percentage: 85,
            reason: "threshold".into(),
        };
        assert!(apply_session_event(&update, &mut session, &mut scrollback, false));
        assert!(
            session.in_flight_prompt.is_none(),
            "compaction start implies server activity — cancel must not rewind prompt"
        );
        assert_eq!(
            session.compact_held_prompt.as_ref().map(|p| p.text.as_str()),
            Some("hi"),
            "hold prompt text for re-auth auto-resubmit if compact fails with auth"
        );
    }

    /// Compact failure keeps the hold; PromptResponse reauth gate decides stash.
    #[test]
    fn apply_compaction_failed_keeps_held_prompt() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.compact_held_prompt = Some(InFlightPrompt {
            text: "retry after login".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(1),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        for error in [
            "authentication problem — re-authenticate using /login and retry.",
            "this conversation is too large to compact.",
        ] {
            let update = EchoAgentSessionUpdate::AutoCompactFailed {
                error: error.into(),
            };
            assert!(apply_session_event(&update, &mut session, &mut scrollback, false));
            assert_eq!(
                session.compact_held_prompt.as_ref().map(|p| p.text.as_str()),
                Some("retry after login"),
            );
        }
    }

    /// `ImageDropped` joins notes with `\n` and pushes a system block.
    /// Pin the `\n` separator so a `notes.join(" ")` regression is caught.
    #[test]
    fn apply_image_dropped_pushes_scrollback_block() {
        use crate::scrollback::block::RenderBlock;
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        let before = scrollback.len();
        let notes = vec![
            "Image 1 was dropped: corrupt.".to_string(),
            "Image 2 was dropped: too small (4×3).".to_string(),
        ];
        let update = EchoAgentSessionUpdate::ImageDropped {
            notes: notes.clone(),
        };
        let changed = apply_session_event(&update, &mut session, &mut scrollback, false);
        assert!(changed);
        assert_eq!(scrollback.len(), before + 1);
        let entry = scrollback.entries_mut().last().expect("entry pushed");
        match &entry.block {
            RenderBlock::System(b) => {
                assert!(b.text.contains(&notes[0]));
                assert!(b.text.contains(&notes[1]));
                assert!(
                    b.text.contains('\n'),
                    "expected \\n separator between dropped notes, got: {:?}",
                    b.text
                );
            }
            other => panic!("expected System block, got {other:?}"),
        }
    }

    /// A successful compression needs no user action: log-only — no toast,
    /// no scrollback block, no redraw. Same live and on session replay.
    #[test]
    fn image_compressed_is_invisible_in_tui() {
        for replay in [false, true] {
            let mut agent = make_agent(Some("s1"));
            agent.session.loading_replay = replay;
            assert!(!apply_image_compressed(
                &mut agent,
                &[compressed_entry(1), compressed_entry(2)],
                "Compressed Image 1: 4.2 MB (3024x1964) \u{2192} 780 KB (1568x1018)",
            ));
            assert!(agent.toast.is_none(), "no toast (replay={replay})");
            assert_eq!(agent.scrollback.len(), 0, "no block (replay={replay})");
        }
    }

    /// The re-encode fallback (empty `images`) means the oversized original
    /// was kept — a persistent warning line, not a transient toast.
    #[test]
    fn image_compressed_fallback_warning_stays_in_scrollback() {
        use crate::scrollback::block::RenderBlock;
        let mut agent = make_agent(Some("s1"));
        let msg = "Image 1 could not be re-encoded under the 1.5 MB limit; the original attachment was kept.";
        assert!(apply_image_compressed(&mut agent, &[], msg));
        assert!(agent.toast.is_none(), "warning must not be transient");
        let entry = agent.scrollback.entries_mut().last().expect("block pushed");
        match &entry.block {
            RenderBlock::System(b) => assert_eq!(b.text, msg),
            other => panic!("expected System block, got {other:?}"),
        }
    }

    #[test]
    fn apply_retry_state_retrying_clears_in_flight_prompt() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.in_flight_prompt = Some(InFlightPrompt {
            text: "retry me".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(2),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        let retry = RetryState::Retrying {
            attempt: 1,
            max_retries: 3,
            reason: "rate limited".into(),
        };
        apply_retry_state(&retry, &mut session, &mut scrollback, false);
        assert!(
            session.in_flight_prompt.is_none(),
            "RetryState bypasses session/update in_flight hook"
        );
    }

    #[test]
    fn retry_exhausted_rate_limited_sets_flag() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();

        assert!(!session.rate_limited);
        apply_retry_state(
            &RetryState::Exhausted {
                attempts: 3,
                reason: "rate limited".into(),
                is_rate_limited: true,
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            session.rate_limited,
            "rate_limited flag must be set when is_rate_limited is true"
        );
    }

    #[test]
    fn retry_exhausted_rate_limited_empty_reason_uses_oauth_fallback() {
        use echo_agent_runtime::sampling::error::RATE_LIMITED_USER_MESSAGE_OAUTH;

        let empty = RetryState::Exhausted {
            attempts: 3,
            reason: "".into(),
            is_rate_limited: true,
        };

        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(&empty, &mut session, &mut scrollback, false);
        match last_session_event(&scrollback) {
            Some(SessionEvent::RetryFailed { error, .. }) => {
                assert_eq!(error, RATE_LIMITED_USER_MESSAGE_OAUTH);
            }
            other => panic!("expected empty-rate-limit RetryFailed, got {other:?}"),
        }
    }

    /// Production `RetryState::Exhausted.reason` is `SamplingError::Api`'s
    /// Display: `API error (status 429 Too Many Requests): …`.
    #[test]
    fn retry_exhausted_rate_limited_surfaces_server_detail() {
        let body = "The model is currently at capacity due to high demand. Please try again.";
        let reason = format!("API error (status 429 Too Many Requests): {body}");
        let exhausted = RetryState::Exhausted {
            attempts: 3,
            reason: reason.clone(),
            is_rate_limited: true,
        };

        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(&exhausted, &mut session, &mut scrollback, false);
        match last_session_event(&scrollback) {
            Some(SessionEvent::RetryFailed { error, .. }) => {
                assert_eq!(error, body);
                assert!(!error.contains("API error (status"));
            }
            other => panic!("expected detail RetryFailed, got {other:?}"),
        }
    }

    #[test]
    fn retry_exhausted_api_key_rewrites_consumer_subscription_upsell() {
        use echo_agent_runtime::sampling::error::RATE_LIMITED_USER_MESSAGE_API_KEY;

        let rpm = RetryState::Exhausted {
            attempts: 2,
            reason: "API error (status 429 Too Many Requests): \
                     Some resource has been exhausted: You are sending requests too quickly. \
                     Please slow down, or upgrade to a EchoAgent subscription for higher limits: \
                     https://cloud.echo-agent.invalid/echoagent"
                .into(),
            is_rate_limited: true,
        };

        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(&rpm, &mut session, &mut scrollback, true);
        match last_session_event(&scrollback) {
            Some(SessionEvent::RetryFailed { error, .. }) => {
                assert_eq!(error, RATE_LIMITED_USER_MESSAGE_API_KEY);
                assert!(!error.contains("cloud.echo-agent.invalid/echoagent"));
            }
            other => panic!("expected API-key rate-limit RetryFailed, got {other:?}"),
        }
    }

    #[test]
    fn retry_exhausted_non_rate_limited_does_not_set_flag() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();

        apply_retry_state(
            &RetryState::Exhausted {
                attempts: 3,
                reason: "server error".into(),
                is_rate_limited: false,
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            !session.rate_limited,
            "rate_limited flag must not be set when is_rate_limited is false"
        );
    }

    /// A rate-limit exhaustion whose flattened reason carries the
    /// free-usage code sets both flags and pushes NO generic block (the
    /// driver shows the paywall modal on PromptResponse; viewers keep no
    /// marker).
    #[test]
    fn retry_exhausted_free_usage_sets_paywall_flag_without_marker() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.in_flight_prompt = Some(InFlightPrompt {
            text: "try me again".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(2),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });

        apply_retry_state(
            &RetryState::Exhausted {
                attempts: 0,
                reason: "API error (status 429 Too Many Requests): \
                         subscription:free-usage-exhausted: You have used all your free usage."
                    .into(),
                is_rate_limited: true,
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            session.rate_limited,
            "free-usage keeps rate_limited (TurnFailed/toast suppression)"
        );
        assert!(session.free_usage_blocked);
        assert_eq!(
            scrollback.len(),
            0,
            "no RetryFailed marker — the paywall modal shows instead"
        );
        assert!(
            session.in_flight_prompt.is_none(),
            "free-usage exhaustion clears in_flight_prompt like other failures"
        );
    }

    #[test]
    fn apply_retry_state_disk_full_pushes_session_event() {
        use echo_agent_runtime::extensions::notification::{
            DISK_FULL_ERROR_TYPE, DISK_FULL_USER_MESSAGE,
        };
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(
            &RetryState::Failed {
                error_type: DISK_FULL_ERROR_TYPE.into(),
                message: DISK_FULL_USER_MESSAGE.into(),
            },
            &mut session,
            &mut scrollback,
            false,
        );
        match scrollback.last().map(|e| &e.block) {
            Some(RenderBlock::SessionEvent(ev)) => {
                assert!(matches!(ev.event, SessionEvent::DiskFull));
                assert_eq!(ev.event.message(), DISK_FULL_USER_MESSAGE);
            }
            other => panic!("expected DiskFull session event, got {other:?}"),
        }
    }

    #[test]
    fn apply_retry_state_credit_limit_exhausted_preserves_in_flight_prompt() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.in_flight_prompt = Some(InFlightPrompt {
            text: "stash me".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(2),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        apply_retry_state(
            &RetryState::Exhausted {
                attempts: 3,
                reason: "status 403: run out of credits".into(),
                is_rate_limited: false,
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            session.credit_limit_blocked,
            "credit_limit_blocked must be set for credit-limit 403"
        );
        assert!(
            session.in_flight_prompt.is_some(),
            "in_flight_prompt must be preserved so PromptResponse handler can stash it"
        );
        assert_eq!(session.in_flight_prompt.unwrap().text, "stash me");
    }

    #[test]
    fn apply_retry_state_credit_limit_failed_preserves_in_flight_prompt() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.in_flight_prompt = Some(InFlightPrompt {
            text: "stash me too".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(3),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        apply_retry_state(
            &RetryState::Failed {
                error_type: "api".into(),
                message: "status 403: run out of credits".into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            session.credit_limit_blocked,
            "credit_limit_blocked must be set for credit-limit 403"
        );
        assert!(
            session.in_flight_prompt.is_some(),
            "in_flight_prompt must be preserved so PromptResponse handler can stash it"
        );
        assert_eq!(session.in_flight_prompt.unwrap().text, "stash me too");
    }

    #[test]
    fn apply_retry_state_pool_402_sets_credit_limit_blocked() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.in_flight_prompt = Some(InFlightPrompt {
            text: "pool blocked".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(5),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        apply_retry_state(
            &RetryState::Failed {
                error_type: "api".into(),
                message:
                    "API error (status 402 Payment Required): EchoAgent Build usage balance exhausted"
                        .into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            session.credit_limit_blocked,
            "credit_limit_blocked must be set for pool 402 balance exhausted"
        );
        assert!(session.in_flight_prompt.is_some());
    }

    #[test]
    fn apply_retry_state_non_credit_limit_failed_clears_in_flight_prompt() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.in_flight_prompt = Some(InFlightPrompt {
            text: "gone".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(4),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        apply_retry_state(
            &RetryState::Failed {
                error_type: "api".into(),
                message: "internal server error".into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            !session.credit_limit_blocked,
            "credit_limit_blocked must NOT be set for non-credit-limit errors"
        );
        assert!(
            session.in_flight_prompt.is_none(),
            "in_flight_prompt must be cleared for non-credit-limit errors"
        );
    }

    #[test]
    fn is_reauthable_failure_matrix() {
        assert!(is_reauthable_failure(Some("auth"), "Unauthorized (401)"));
        assert!(is_reauthable_failure(
            Some("api"),
            "Unauthorized (401) from https://proxy/v1/responses"
        ));
        assert!(is_reauthable_failure(None, "Unauthorized (401)"));
        // legacy_auth carries its own migration guidance — excluded.
        assert!(!is_reauthable_failure(
            Some("legacy_auth"),
            "Unauthorized (401) ... deprecated authentication method"
        ));
        // auth_transient = the shell says the failure self-heals (refreshable
        // credential, no sticky verdict — e.g. post-wake network gap). Even
        // with a 401 in the message, the `/login` banner must not fire.
        assert!(!is_reauthable_failure(
            Some("auth_transient"),
            "Unauthorized (401)\n\nAuthentication is temporarily unavailable"
        ));
        // Unrelated failures must not be treated as re-authable.
        assert!(!is_reauthable_failure(
            Some("api"),
            "internal server error"
        ));
        assert!(!is_reauthable_failure(Some("api"), "model not found"));
    }

    /// A 401 with `error_type == "auth"` surfaces the actionable re-auth
    /// prompt instead of the raw "Retry failed: Unauthorized (401) …" dump.
    #[test]
    fn apply_retry_state_auth_failure_pushes_reauth_prompt() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(
            &RetryState::Failed {
                error_type: "auth".into(),
                message: "Unauthorized (401) from https://chat-proxy.echo-agent.invalid/v1/messages: \
                          no auth context"
                    .into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            matches!(
                last_session_event(&scrollback),
                Some(SessionEvent::ReAuthRequired)
            ),
            "auth 401 must surface the actionable re-auth prompt"
        );
        assert!(!session.credit_limit_blocked);
    }

    /// A recoverable auth failure preserves `in_flight_prompt` so the
    /// PromptResponse handler can stash it for auto-resubmit after re-auth.
    #[test]
    fn apply_retry_state_auth_failure_preserves_in_flight_prompt() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.in_flight_prompt = Some(InFlightPrompt {
            text: "retry after login".into(),
            images: Vec::new(),
            scrollback_entry: EntryId::new(5),
            combined_scrollback_entries: Vec::new(),
            chip_elements: Vec::new(),
        });
        apply_retry_state(
            &RetryState::Failed {
                error_type: "auth".into(),
                message: "Unauthorized (401) from https://proxy/v1/messages".into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            session.in_flight_prompt.is_some(),
            "in_flight_prompt must be preserved on a recoverable auth failure"
        );
        assert_eq!(session.in_flight_prompt.unwrap().text, "retry after login");
    }

    /// A 401 reported with a non-auth `error_type` but an "Unauthorized
    /// (401)" message (the `SamplingErrorKind::Api` path) also prompts.
    #[test]
    fn apply_retry_state_401_message_without_auth_type_prompts_reauth() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(
            &RetryState::Failed {
                error_type: "api".into(),
                message: "Unauthorized (401) from https://proxy/v1/responses: invalid credentials"
                    .into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(matches!(
            last_session_event(&scrollback),
            Some(SessionEvent::ReAuthRequired)
        ));
    }

    /// Legacy WebLogin auth keeps its verbose message (with `echoagent logout` /
    /// `echoagent login` guidance), not the generic re-auth prompt.
    #[test]
    fn apply_retry_state_legacy_auth_keeps_detailed_message() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(
            &RetryState::Failed {
                error_type: "legacy_auth".into(),
                message: "Unauthorized (401) ... deprecated authentication method (WebLogin) ... \
                          run `echoagent logout` then `echoagent login`"
                    .into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(matches!(
            last_session_event(&scrollback),
            Some(SessionEvent::RetryFailed { .. })
        ));
    }

    /// Non-auth terminal failures render the formatted RequestFailed banner
    /// (same visual treatment as 401 re-auth), not a raw RetryFailed dump.
    #[test]
    fn apply_retry_state_generic_failure_shows_request_failed_banner() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(
            &RetryState::Failed {
                error_type: "api".into(),
                message: r#"API error (status 500 Internal Server Error): {"error":"upstream exploded"}"#.into(),
            },
            &mut session,
            &mut scrollback, false);
        match last_session_event(&scrollback) {
            Some(SessionEvent::RequestFailed {
                status,
                headline,
                detail,
            }) => {
                assert_eq!(status, Some(500));
                assert_eq!(headline, "Server error (500)");
                assert_eq!(
                    detail,
                    "Something went wrong on our side. Wait a minute and send again."
                );
            }
            other => panic!("expected RequestFailed, got {other:?}"),
        }
    }

    #[test]
    fn apply_retry_state_403_shows_clean_denied_banner() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(
            &RetryState::Failed {
                error_type: "api".into(),
                message: "API error (status 403 Forbidden): Access to the chat endpoint is denied"
                    .into(),
            },
            &mut session,
            &mut scrollback,
            false,
        );
        match last_session_event(&scrollback) {
            Some(SessionEvent::RequestFailed {
                status,
                headline,
                detail,
            }) => {
                assert_eq!(status, Some(403));
                assert_eq!(headline, "Request denied (403)");
                assert_eq!(detail, "Access to the chat endpoint is denied");
            }
            other => panic!("expected RequestFailed, got {other:?}"),
        }
    }

    /// A context overflow surfaces the actionable `ContextTooLarge` prompt (not the
    /// raw `RetryFailed`); `PromptResponse` then suppresses the redundant `TurnFailed`.
    #[test]
    fn apply_retry_state_context_length_shows_context_too_large() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(
            &RetryState::Failed {
                error_type: "context_length".into(),
                message: "API error (status 500): the prompt is too long for this model's \
                          context window"
                    .into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            matches!(
                last_session_event(&scrollback),
                Some(SessionEvent::ContextTooLarge)
            ),
            "context overflow must surface the actionable ContextTooLarge prompt"
        );
    }

    /// Overflow-shaped copy without `error_type=context_length` must not take
    /// the ContextTooLarge path — the shell is what tags overflow.
    #[test]
    fn apply_retry_state_overflow_copy_without_type_is_not_context_too_large() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        apply_retry_state(
            &RetryState::Failed {
                error_type: "api".into(),
                message: "API error (status 500): the prompt is too long for this model's \
                          context window"
                    .into(),
            },
            &mut session,
            &mut scrollback,
            false,
        );
        assert!(
            matches!(
                last_session_event(&scrollback),
                Some(SessionEvent::RequestFailed { .. })
            ),
            "without error_type=context_length this is a generic banner, not overflow UX"
        );
    }

    /// When the compaction handler already showed its "too large to compact" message,
    /// the overflow path does NOT stack a second `ContextTooLarge` prompt on top.
    #[test]
    fn apply_retry_state_context_length_does_not_duplicate_compaction_failed() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        scrollback.push_block(RenderBlock::session_event(SessionEvent::CompactionFailed {
            error: "this conversation is too large to compact.".into(),
        }));
        apply_retry_state(
            &RetryState::Failed {
                error_type: "context_length".into(),
                message: "the prompt is too long for this model's context window".into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            matches!(
                last_session_event(&scrollback),
                Some(SessionEvent::CompactionFailed { .. })
            ),
            "must not push a duplicate prompt on top of CompactionFailed"
        );
    }

    #[test]
    fn apply_compaction_completed_defers_message_until_turn_end() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        session.set_compaction_activity(Some(TurnActivity::AutoCompacting));
        let update = EchoAgentSessionUpdate::AutoCompactCompleted {
            tokens_before: Some(858_000),
            tokens_after: 66_000,
            elapsed_ms: Some(500),
            summary_preview: None,
        };
        assert!(apply_session_event(&update, &mut session, &mut scrollback, false));
        assert_eq!(
            scrollback.len(),
            0,
            "live compaction completion must be deferred, not pushed immediately"
        );

        session.note_context_used(43_000);

        session.finish_turn(&mut scrollback,
        );
        match last_session_event(&scrollback) {
            Some(SessionEvent::CompactionCompleted {
                tokens_before,
                tokens_after,
                ..
            }) => {
                assert_eq!(tokens_before, Some(858_000));
                assert_eq!(
                    tokens_after, 43_000,
                    "must flush the model-confirmed count, not the 66k estimate"
                );
            }
            other => panic!("expected deferred CompactionCompleted, got {other:?}"),
        }
    }

    #[test]
    fn apply_compaction_completed_falls_back_to_estimate_without_confirmation() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        let update = EchoAgentSessionUpdate::AutoCompactCompleted {
            tokens_before: Some(90_000),
            tokens_after: 20_000,
            elapsed_ms: Some(500),
            summary_preview: None,
        };
        assert!(apply_session_event(&update, &mut session, &mut scrollback, false));
        session.finish_turn(&mut scrollback,
        );
        match last_session_event(&scrollback) {
            Some(SessionEvent::CompactionCompleted { tokens_after, .. }) => {
                assert_eq!(
                    tokens_after, 20_000,
                    "fallback to estimate when unconfirmed"
                );
            }
            other => panic!("expected fallback CompactionCompleted, got {other:?}"),
        }
    }

    #[test]
    fn apply_compaction_completed_renders_immediately_during_replay() {
        let mut session = make_session(Some("s1"));
        session.loading_replay = true;
        let mut scrollback = ScrollbackState::new();
        let update = EchoAgentSessionUpdate::AutoCompactCompleted {
            tokens_before: Some(90_000),
            tokens_after: 20_000,
            elapsed_ms: Some(500),
            summary_preview: None,
        };
        assert!(apply_session_event(&update, &mut session, &mut scrollback, false));
        match last_session_event(&scrollback) {
            Some(SessionEvent::CompactionCompleted { tokens_after, .. }) => {
                assert_eq!(
                    tokens_after, 20_000,
                    "replay renders the recorded count immediately"
                );
            }
            other => panic!("expected immediate CompactionCompleted on replay, got {other:?}"),
        }
    }

    #[test]
    fn deferred_compaction_flushes_confirmed_count_over_estimate_refresh() {
        let mut agent = make_agent(Some("s1"));
        agent
            .session
            .set_compaction_activity(Some(TurnActivity::AutoCompacting));

        let update = EchoAgentSessionUpdate::AutoCompactCompleted {
            tokens_before: Some(858_000),
            tokens_after: 66_000,
            elapsed_ms: Some(500),
            summary_preview: None,
        };
        assert!(apply_session_event(
            &update,
            &mut agent.session,
            &mut agent.scrollback, false));

        refresh_context_used(&mut agent, 66_000);
        confirm_context_used(&mut agent, 43_000);

        agent.session.finish_turn(&mut agent.scrollback,
        );
        match last_session_event(&agent.scrollback) {
            Some(SessionEvent::CompactionCompleted {
                tokens_before,
                tokens_after,
                ..
            }) => {
                assert_eq!(tokens_before, Some(858_000));
                assert_eq!(
                    tokens_after, 43_000,
                    "deferred line must flush the confirmed 43k, not the 66k \
                     estimate refresh that updated the bar first"
                );
            }
            other => panic!("expected deferred CompactionCompleted, got {other:?}"),
        }
    }

    #[test]
    fn apply_unhandled_event_returns_false() {
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();
        let update = EchoAgentSessionUpdate::MemoryFlushStarted;
        assert!(!apply_session_event(&update, &mut session, &mut scrollback, false));
    }

    // ── handle_child_session_notification ──────────────────────────────

    #[test]
    fn child_compact_completed_updates_subagent_info() {
        let mut agent = make_agent(Some("root-sess"));
        let child_sid = "child-sess-1";
        agent
            .subagent_sessions
            .insert(child_sid.into(), make_subagent_info(child_sid));
        let child_view = make_agent(Some(child_sid));
        agent
            .subagent_views
            .insert(child_sid.into(), Box::new(child_view));

        let update = EchoAgentSessionUpdate::AutoCompactCompleted {
            tokens_before: Some(90000),
            tokens_after: 25000,
            elapsed_ms: Some(300),
            summary_preview: None,
        };
        let changed = handle_child_session_notification(update, child_sid, &mut agent, false);
        assert!(changed);

        let info = agent.subagent_sessions.get(child_sid).unwrap();
        assert_eq!(info.tokens_used, Some(25000));
        // 25000 / 131072 * 100 ~= 19
        assert_eq!(info.context_usage_pct, Some(19));

        // The child view's context_state.used (context-bar numerator) must
        // also be reset — see the comment in handle_child_session_notification.
        let child_view = agent.subagent_views.get(child_sid).unwrap();
        assert_eq!(
            child_view.context_state.as_ref().map(|c| c.used),
            Some(25000)
        );
    }

    #[test]
    fn child_compact_started_does_not_reset_context_used() {
        // Sibling variants in the same outer arm must not touch the numerator;
        // guards against accidental widening of the AutoCompactCompleted gate.
        let mut agent = make_agent(Some("root-sess"));
        let child_sid = "child-sess-3";
        agent
            .subagent_sessions
            .insert(child_sid.into(), make_subagent_info(child_sid));
        let mut child_view = make_agent(Some(child_sid));
        child_view.context_state = Some(echo_agent_runtime::session::ContextInfo::from_notification(
            90_000, 131_072,
        ));
        agent
            .subagent_views
            .insert(child_sid.into(), Box::new(child_view));

        let update = EchoAgentSessionUpdate::AutoCompactStarted {
            tokens_used: 95_000,
            context_window: 131_072,
            percentage: 72,
            reason: "threshold".into(),
        };
        let _ = handle_child_session_notification(update, child_sid, &mut agent, false);

        let child_view = agent.subagent_views.get(child_sid).unwrap();
        assert_eq!(
            child_view.context_state.as_ref().map(|c| c.used),
            Some(90_000)
        );
    }

    #[test]
    fn child_notification_without_view_returns_false() {
        let mut agent = make_agent(Some("root-sess"));
        // No child view registered.
        let update = EchoAgentSessionUpdate::AutoCompactStarted {
            tokens_used: 90000,
            context_window: 131072,
            percentage: 85,
            reason: "threshold".into(),
        };
        let changed = handle_child_session_notification(update, "unknown-child", &mut agent, false);
        assert!(!changed);
    }

    #[test]
    fn child_compact_completed_without_view_returns_false() {
        let mut agent = make_agent(Some("root-sess"));
        let child_sid = "child-sess-2";
        // SubagentInfo exists but no child view (race between notification and spawn).
        agent
            .subagent_sessions
            .insert(child_sid.into(), make_subagent_info(child_sid));

        let update = EchoAgentSessionUpdate::AutoCompactCompleted {
            tokens_before: Some(90000),
            tokens_after: 25000,
            elapsed_ms: Some(300),
            summary_preview: None,
        };
        let changed = handle_child_session_notification(update, child_sid, &mut agent, false);
        // No child_view means nothing visible changed — must not trigger redraw.
        assert!(!changed);
        // SubagentInfo should still be updated (data correctness).
        let info = agent.subagent_sessions.get(child_sid).unwrap();
        assert_eq!(info.tokens_used, Some(25000));
        assert_eq!(info.context_usage_pct, Some(19));
    }

    #[test]
    fn child_unknown_event_returns_false() {
        let mut agent = make_agent(Some("root-sess"));
        let update = EchoAgentSessionUpdate::MemoryFlushStarted;
        let changed = handle_child_session_notification(update, "child-1", &mut agent, false);
        assert!(!changed);
    }

    #[test]
    fn tool_call_delta_chunk_sets_writing_activity() {
        let mut app = make_app_with_agent("sess-1");
        app.agents.get_mut(&AgentId(0)).unwrap().session.state = AgentState::TurnRunning;

        let changed = handle(
            make_ext_session_notification(
                "sess-1",
                EchoAgentSessionUpdate::ToolCallDeltaChunk {
                    tool_call_id: Some("call_1".into()),
                    tool_index: 0,
                    name: Some("spawn_subagent".into()),
                    arguments_delta: None,
                },
            ),
            &mut app,
        );
        assert!(changed, "first delta must request a redraw");
        let agent = app.agents.get(&AgentId(0)).unwrap();
        let Some(TurnActivity::WritingToolCall(writing)) = agent.session.tracker.activity() else {
            panic!("expected WritingToolCall activity");
        };
        assert_eq!(writing.label(), "Writing subagent prompt…");
    }

    /// A delta-first turn still counts as first activity: stash drops, TTFA stamps.
    #[test]
    fn tool_call_delta_chunk_clears_in_flight_prompt() {
        let mut app = make_app_with_agent("sess-1");
        let started = Instant::now();
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.turn_started_at = Some(started);
            agent.session.in_flight_prompt = Some(InFlightPrompt {
                text: "hi".into(),
                images: Vec::new(),
                scrollback_entry: EntryId::new(1),
                combined_scrollback_entries: Vec::new(),
                chip_elements: Vec::new(),
            });
        }

        let _ = handle(
            make_ext_session_notification(
                "sess-1",
                EchoAgentSessionUpdate::ToolCallDeltaChunk {
                    tool_call_id: Some("call_1".into()),
                    tool_index: 0,
                    name: Some("write".into()),
                    arguments_delta: None,
                },
            ),
            &mut app,
        );
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.session.in_flight_prompt.is_none(),
            "first activity on the delta rail must drop the rewind stash"
        );
        assert_eq!(
            agent.first_activity_logged_for,
            Some(started),
            "TTFA must be stamped for a delta-first turn"
        );
    }

    /// Hook / image-intake diagnostics must not consume the Ctrl+C rewind stash.
    #[test]
    fn hook_and_image_intake_notifications_keep_in_flight_prompt() {
        let updates = [
            EchoAgentSessionUpdate::HookExecution {
                event_name: "user_prompt_submit".into(),
                tool_name: None,
                prompt_id: Some("p1".into()),
                runs: vec![],
            },
            EchoAgentSessionUpdate::ImageCompressed {
                images: vec![],
                message: "resized".into(),
            },
            EchoAgentSessionUpdate::ImageDropped { notes: vec![] },
        ];
        for update in updates {
            let label = format!("{update:?}");
            let mut app = make_app_with_agent("sess-1");
            {
                let agent = app.agents.get_mut(&AgentId(0)).unwrap();
                agent.session.state = AgentState::TurnRunning;
                agent.turn_started_at = Some(Instant::now());
                agent.session.in_flight_prompt = Some(InFlightPrompt {
                    text: "hi".into(),
                    images: Vec::new(),
                    scrollback_entry: EntryId::new(1),
                    combined_scrollback_entries: Vec::new(),
                    chip_elements: Vec::new(),
                });
            }

            let _ = handle(make_ext_session_notification("sess-1", update), &mut app);

            let agent = app.agents.get(&AgentId(0)).unwrap();
            assert!(
                agent.session.in_flight_prompt.is_some(),
                "intake diagnostic must not eat the rewind stash: {label}"
            );
        }
    }

    /// Deltas carry no prompt id — while a wake turn is in flight the chunk is
    /// dropped whole: no tracker write, no rewind-stash consumption, no TTFA.
    #[test]
    fn wake_gated_delta_chunk_is_fully_inert() {
        let mut app = make_app_with_agent("sess-1");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.turn_started_at = Some(Instant::now());
            agent.session.in_flight_prompt = Some(InFlightPrompt {
                text: "hi".into(),
                images: Vec::new(),
                scrollback_entry: EntryId::new(1),
                combined_scrollback_entries: Vec::new(),
                chip_elements: Vec::new(),
            });
            agent
                .session
                .tracker
                .set_retry_activity(Some(crate::acp::tracker::TurnActivity::Retrying {
                    attempt: 1,
                    max_retries: 3,
                    reason: "overloaded".into(),
                }));
            agent.running_wake_turn = Some(crate::app::agent_view::RunningWakeTurn {
                prompt_id: "task-completed-1".into(),
                cancel_sent: false,
            });
        }

        let changed = handle(
            make_ext_session_notification(
                "sess-1",
                EchoAgentSessionUpdate::ToolCallDeltaChunk {
                    tool_call_id: Some("call_1".into()),
                    tool_index: 0,
                    name: Some("write".into()),
                    arguments_delta: None,
                },
            ),
            &mut app,
        );
        assert!(!changed);
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert!(
            agent.session.in_flight_prompt.is_some(),
            "a dropped delta must not eat the rewind stash"
        );
        assert_eq!(agent.first_activity_logged_for, None);
        assert!(
            matches!(
                agent.session.tracker.activity(),
                Some(TurnActivity::Retrying { .. })
            ),
            "wake-attributable delta must not clear the local turn's retry override"
        );
    }

    /// Defense-in-depth: the shell never emits replay-marked deltas.
    #[test]
    fn tool_call_delta_chunk_ignored_during_replay() {
        let mut app = make_app_with_agent("sess-1");
        {
            let agent = app.agents.get_mut(&AgentId(0)).unwrap();
            agent.session.state = AgentState::TurnRunning;
            agent.session.loading_replay = true;
            agent.session.in_flight_prompt = Some(InFlightPrompt {
                text: "hi".into(),
                images: Vec::new(),
                scrollback_entry: EntryId::new(1),
                combined_scrollback_entries: Vec::new(),
                chip_elements: Vec::new(),
            });
        }

        let (tx, _rx) = tokio::sync::oneshot::channel();
        let payload = SessionNotification {
            session_id: acp::SessionId::new("sess-1"),
            update: EchoAgentSessionUpdate::ToolCallDeltaChunk {
                tool_call_id: Some("call_1".into()),
                tool_index: 0,
                name: Some("write".into()),
                arguments_delta: Some("{".into()),
            },
            meta: Some(serde_json::json!({ "isReplay": true })),
        };
        let raw = serde_json::value::to_raw_value(&payload).unwrap();
        let request = acp::ExtNotification::new("echo.agent/session_notification", raw.into());
        let changed = handle(
            AcpClientMessage::ExtNotification(echo_agent_acp::AcpArgs {
                request,
                response_tx: tx,
            }),
            &mut app,
        );
        assert!(!changed);
        let agent = app.agents.get(&AgentId(0)).unwrap();
        assert_eq!(
            agent.session.tracker.activity(),
            None,
            "replayed delta must not set a writing label"
        );
        assert!(
            agent.session.in_flight_prompt.is_some(),
            "a dropped delta must not eat the rewind stash"
        );
    }

    // ── apply_retry_state ─────────────────────────────────────────────

    #[test]
    fn retry_failed_encrypted_content_sets_model_incompatible() {
        use echo_agent_runtime::extensions::notification::RetryState;
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();

        assert!(!session.model_incompatible);
        apply_retry_state(
            &RetryState::Failed {
                error_type: "encrypted_content_mismatch".into(),
                message: "incompatible history".into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            session.model_incompatible,
            "encrypted_content_mismatch should set model_incompatible flag"
        );
    }

    #[test]
    fn retry_failed_other_type_does_not_set_model_incompatible() {
        use echo_agent_runtime::extensions::notification::RetryState;
        let mut session = make_session(Some("s1"));
        let mut scrollback = ScrollbackState::new();

        apply_retry_state(
            &RetryState::Failed {
                error_type: "api_400".into(),
                message: "bad request".into(),
            },
            &mut session,
            &mut scrollback, false);
        assert!(
            !session.model_incompatible,
            "non-encrypted_content error types must not set model_incompatible"
        );
    }

    fn summary_generated_ext(
        session_id: &str,
        title: &str,
        title_is_manual: bool,
    ) -> acp::ExtNotification {
        let meta = if title_is_manual {
            Some(echo_agent_runtime::extensions::notification::title_is_manual_meta())
        } else {
            None
        };
        let notif = SessionNotification {
            session_id: acp::SessionId::new(session_id),
            update: EchoAgentSessionUpdate::SessionSummaryGenerated {
                session_summary: title.into(),
            },
            meta,
        };
        let raw = serde_json::value::to_raw_value(&notif).unwrap();
        acp::ExtNotification::new("echo.agent/session_notification", std::sync::Arc::from(raw))
    }

    #[test]
    fn manual_title_notification_sets_display_name_without_entity_decode() {
        let mut app = make_app_with_agent("sess-1");
        let changed = handle_session_notification(
            &summary_generated_ext("sess-1", "a &amp; b", true),
            &mut app,
        );
        assert!(changed);
        let agent = &app.agents[&AgentId(0)];
        assert_eq!(
            agent.display_name.as_deref(),
            Some("a &amp; b"),
            "manual meta must set display_name from the raw title"
        );
        assert_eq!(
            agent.generated_session_title.as_deref(),
            Some("a &amp; b"),
            "manual meta must skip HTML-entity decode"
        );
    }

    #[test]
    fn auto_title_blank_after_sanitize_does_not_clear_existing() {
        let mut app = make_app_with_agent("sess-1");
        app.agents.get_mut(&AgentId(0)).unwrap().generated_session_title =
            Some("Keep Me".into());
        assert!(handle_session_notification(
            &summary_generated_ext("sess-1", "\u{1b}\u{07}", false),
            &mut app,
        ));
        assert_eq!(
            app.agents[&AgentId(0)]
                .generated_session_title
                .as_deref(),
            Some("Keep Me"),
            "control-only auto replay must not wipe an existing title"
        );
    }

    #[test]
    fn auto_title_notification_does_not_set_display_name() {
        let mut app = make_app_with_agent("sess-1");
        let changed = handle_session_notification(
            &summary_generated_ext("sess-1", "a &amp; b", false),
            &mut app,
        );
        assert!(changed);
        let agent = &app.agents[&AgentId(0)];
        assert!(
            agent.display_name.is_none(),
            "auto titles must not promote to display_name"
        );
        assert_eq!(
            agent.generated_session_title.as_deref(),
            Some("a & b"),
            "auto titles still HTML-entity-decode"
        );
    }

    #[test]
    fn auto_title_notification_does_not_clobber_existing_display_name() {
        let mut app = make_app_with_agent("sess-1");
        app.agents.get_mut(&AgentId(0)).unwrap().display_name = Some("Pinned".into());
        let changed = handle_session_notification(
            &summary_generated_ext("sess-1", "a &amp; b", false),
            &mut app,
        );
        assert!(changed);
        let agent = &app.agents[&AgentId(0)];
        assert_eq!(agent.display_name.as_deref(), Some("Pinned"));
        assert_eq!(agent.generated_session_title.as_deref(), Some("a & b"));
    }

    #[test]
    fn manual_meta_false_clears_display_name() {
        let mut app = make_app_with_agent("sess-1");
        app.agents.get_mut(&AgentId(0)).unwrap().display_name = Some("Pinned".into());
        app.agents.get_mut(&AgentId(0)).unwrap().generated_session_title =
            Some("Pinned".into());
        let n = SessionNotification {
            session_id: acp::SessionId::new("sess-1"),
            update: EchoAgentSessionUpdate::SessionSummaryGenerated {
                session_summary: String::new(),
            },
            meta: Some(serde_json::json!({ "echo.agent/titleIsManual": false })),
        };
        let raw = serde_json::value::to_raw_value(&n).unwrap();
        let notif = acp::ExtNotification::new("echo.agent/session_notification", std::sync::Arc::from(raw));
        assert!(handle_session_notification(&notif, &mut app));
        let agent = &app.agents[&AgentId(0)];
        assert!(
            agent.display_name.is_none(),
            "explicit unpin meta must clear display_name"
        );
        assert!(
            agent.generated_session_title.is_none(),
            "empty unpin summary must drop the follower's manual generated title"
        );
    }

    #[test]
    fn manual_meta_false_empty_summary_keeps_leftover_auto_title() {
        let mut app = make_app_with_agent("sess-1");
        app.agents.get_mut(&AgentId(0)).unwrap().display_name = Some("Pinned".into());
        app.agents.get_mut(&AgentId(0)).unwrap().generated_session_title =
            Some("Auto".into());
        let n = SessionNotification {
            session_id: acp::SessionId::new("sess-1"),
            update: EchoAgentSessionUpdate::SessionSummaryGenerated {
                session_summary: String::new(),
            },
            meta: Some(serde_json::json!({ "echo.agent/titleIsManual": false })),
        };
        let raw = serde_json::value::to_raw_value(&n).unwrap();
        let notif = acp::ExtNotification::new("echo.agent/session_notification", std::sync::Arc::from(raw));
        assert!(handle_session_notification(&notif, &mut app));
        let agent = &app.agents[&AgentId(0)];
        assert!(
            agent.display_name.is_none(),
            "explicit unpin meta must still clear display_name"
        );
        assert_eq!(
            agent.generated_session_title.as_deref(),
            Some("Auto"),
            "empty unpin fan-out must not wipe a leftover auto title"
        );
    }

    #[test]
    fn auto_title_notification_strips_controls_and_caps() {
        use echo_agent_runtime::session::persistence::MAX_TITLE_SCALARS;
        let mut app = make_app_with_agent("sess-1");
        let dirty = format!(
            "ok\u{1b}]0;PWNED\u{07}{}",
            "é".repeat(MAX_TITLE_SCALARS + 5)
        );
        assert!(handle_session_notification(
            &summary_generated_ext("sess-1", &dirty, false),
            &mut app,
        ));
        const PREFIX: &str = "ok]0;PWNED";
        let expected = format!(
            "{PREFIX}{}",
            "é".repeat(MAX_TITLE_SCALARS - PREFIX.chars().count())
        );
        let agent = &app.agents[&AgentId(0)];
        assert!(
            agent.display_name.is_none(),
            "auto titles must not promote to display_name"
        );
        assert_eq!(
            agent.generated_session_title.as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn manual_title_notification_strips_controls_and_caps() {
        use echo_agent_runtime::session::persistence::MAX_TITLE_SCALARS;
        let mut app = make_app_with_agent("sess-1");
        let dirty = format!("ok\u{1b}]0;PWNED\u{07}{}", "é".repeat(MAX_TITLE_SCALARS + 5));
        assert!(handle_session_notification(
            &summary_generated_ext("sess-1", &dirty, true),
            &mut app,
        ));
        const PREFIX: &str = "ok]0;PWNED";
        let expected = format!(
            "{PREFIX}{}",
            "é".repeat(MAX_TITLE_SCALARS - PREFIX.chars().count())
        );
        let agent = &app.agents[&AgentId(0)];
        assert_eq!(agent.display_name.as_deref(), Some(expected.as_str()));
        assert_eq!(
            agent.generated_session_title.as_deref(),
            Some(expected.as_str())
        );
    }

