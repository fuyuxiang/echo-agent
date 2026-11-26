//! Tests for credit-limit upsells, paywall gating, and auto-topup.

use super::*;
use echo_agent_runtime::sampling::error::is_free_usage_exhausted_error;

// ── Credit-limit upsell / max-tier tests ───────────────────────────

/// Open the non-max-tier Q&A upsell modal. Panics if the modal was not created.
fn open_upsell_qa(app: &mut AppView, mode: CreditLimitUpsellMode) {
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    open_credit_limit_upsell(agent, mode, false);
}

/// Open the max-tier inline scrollback card upsell.
fn open_upsell_max_card(app: &mut AppView, mode: CreditLimitUpsellMode) {
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    open_credit_limit_upsell(agent, mode, true);
}

/// Return the `QuestionViewState` from agent 0. Panics if absent.
fn agent_qv(app: &AppView) -> &crate::views::question_view::QuestionViewState {
    app.agents
        .get(&AgentId(0))
        .unwrap()
        .question_view
        .as_ref()
        .unwrap()
}

/// Extract the last-pushed `CreditLimitBlock` from agent 0 scrollback.
fn last_credit_limit_block(
    app: &AppView,
    idx: usize,
) -> &crate::scrollback::blocks::CreditLimitBlock {
    let agent = app.agents.get(&AgentId(0)).unwrap();
    if let crate::scrollback::block::RenderBlock::CreditLimit(ref blk) =
        agent.scrollback.entry(idx).unwrap().block
    {
        blk
    } else {
        panic!("expected CreditLimit block at index {idx}");
    }
}

/// Dispatch a `BillingFetched` task result with sensible defaults.
fn open_usage_modal_nonce(app: &AppView) -> u64 {
    match app.agents[&AgentId(0)].active_modal.as_ref() {
        Some(crate::views::modal::ActiveModal::UsageInfo { state }) => state.fetch_nonce,
        _ => 0,
    }
}

fn dispatch_billing(
    app: &mut AppView,
    balance: Option<crate::views::credit_bar::CreditBalance>,
    silent: bool,
    subscription_tier: Option<String>,
) {
    let nonce = open_usage_modal_nonce(app);
    dispatch(
        Action::TaskComplete(TaskResult::BillingFetched {
            agent_id: AgentId(0),
            balance,
            silent,
            subscription_tier,
            autotopup: crate::views::credit_bar::AutoTopupFetch::Unchanged,
            nonce,
        }),
        app,
    );
}

#[test]
fn credit_limit_retry_preserves_image_submission_state() {
    let mut app = test_app_with_agent();
    let mut image = crate::prompt_images::from_clipboard_data(&crate::clipboard::ImageData {
        data: vec![1, 2, 3],
        mime_type: "image/png".into(),
    });
    image.display_number = 1;
    let prompt = crate::app::agent::InFlightPrompt {
        text: "retry [Image #1]".into(),
        images: vec![image],
        scrollback_entry: crate::scrollback::EntryId::new(0),
        combined_scrollback_entries: Vec::new(),
        chip_elements: vec![crate::app::agent::ChipElement {
            range: 6..16,
            kind: crate::views::prompt_widget::KIND_IMAGE,
            display: None,
        }],
    };
    app.agents
        .get_mut(&AgentId(0))
        .unwrap()
        .credit_limit_stashed_prompt = Some(prompt);

    let effects = dispatch(
        Action::TaskComplete(TaskResult::CreditLimitRecheckComplete {
            agent_id: AgentId(0),
            meta: Some(serde_json::json!({"subscription_tier": "Upgraded"})),
        }),
        &mut app,
    );
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::SendPromptBlocks { .. }))
    );
    let in_flight = app.agents[&AgentId(0)]
        .session
        .in_flight_prompt
        .as_ref()
        .unwrap();
    assert_eq!(in_flight.images.len(), 1);
    assert_eq!(in_flight.chip_elements.len(), 1);
}

#[test]
fn is_max_tier_positive_match() {
    assert!(is_max_tier(Some("echoagent_heavy")));
    assert!(is_max_tier(Some("EchoAgent Heavy")));
    assert!(is_max_tier(Some("SUPERECHO_AGENT_HEAVY")));
}

#[test]
fn is_max_tier_non_max_and_unknown() {
    assert!(!is_max_tier(Some("echoagent")));
    assert!(!is_max_tier(Some("premium")));
    assert!(!is_max_tier(Some("free")));
    // Unknown defaults to non-max → Q&A shown.
    assert!(!is_max_tier(None));
}

#[test]
fn is_max_tier_handles_mixed_case_and_whitespace() {
    assert!(is_max_tier(Some("EchoAgent_Heavy")));
    assert!(is_max_tier(Some("echoagent heavy")));
    assert!(is_max_tier(Some("SUPERECHO_AGENT HEAVY")));
}

#[test]
fn is_max_tier_rejects_partial_matches() {
    assert!(!is_max_tier(Some("echoagent_heav")));
    assert!(!is_max_tier(Some("echoagent_heavy_plus")));
    assert!(!is_max_tier(Some("")));
}

#[test]
fn upsell_non_max_shows_qa_with_two_options() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    let q = &agent_qv(&app).questions[0];
    assert_eq!(q.options.len(), 2);
    assert_eq!(q.options[0].label, "Upgrade tier");
    assert_eq!(q.options[0].id.as_deref(), Some(UPSELL_URL_UPGRADE));
    assert_eq!(q.options[1].label, "Pay as you go");
    assert_eq!(q.options[1].id.as_deref(), Some(UPSELL_URL_PAYG));
}

#[test]
fn upsell_non_max_payg_on_shows_increase_label() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: true },
    );
    let q = &agent_qv(&app).questions[0];
    assert_eq!(q.options.len(), 2);
    assert_eq!(q.options[1].label, "Increase limit");
}

#[test]
fn upsell_non_max_qa_heading_is_credit_limit_when_payg_off() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    let heading = &agent_qv(&app).questions[0].question;
    assert!(
        heading.contains("credit limit"),
        "expected 'credit limit' in heading, got: {heading}"
    );
}

#[test]
fn upsell_non_max_qa_heading_is_spending_cap_when_payg_on() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: true },
    );
    let heading = &agent_qv(&app).questions[0].question;
    assert!(
        heading.contains("spending cap"),
        "expected 'spending cap' in heading, got: {heading}"
    );
}

#[test]
fn upsell_non_max_upgrade_url_is_echoagent() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    let url = agent_qv(&app).questions[0].options[0]
        .id
        .as_deref()
        .unwrap();
    assert!(url.contains("echoagent"), "got: {url}");
    assert!(url.contains("referrer=echo-agent-build"), "got: {url}");
}

#[test]
fn upsell_non_max_payg_url_is_usage() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    let url = agent_qv(&app).questions[0].options[1]
        .id
        .as_deref()
        .unwrap();
    assert!(url.contains("_s=usage"), "got: {url}");
}

#[test]
fn upsell_non_max_payg_on_description_mentions_spending_cap() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: true },
    );
    assert_eq!(
        agent_qv(&app).questions[0].options[1].description,
        "Raise your pay-as-you-go spending cap"
    );
}

#[test]
fn upsell_non_max_payg_off_description_mentions_on_demand() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert_eq!(
        agent_qv(&app).questions[0].options[1].description,
        "Enable pay-as-you-go credits for on-demand usage"
    );
}

#[test]
fn upsell_non_max_unified_shows_buy_credits() {
    let mut app = test_app_with_agent();
    open_upsell_qa(&mut app, CreditLimitUpsellMode::UnifiedCredits);
    let q = &agent_qv(&app).questions[0];
    assert!(q.question.contains("weekly limit"));
    assert_eq!(
        q.options[0].description,
        "Upgrade to a higher tier for more usage"
    );
    assert_eq!(q.options[1].label, "Buy more credits");
    assert_eq!(
        q.options[1].description,
        "Purchase credits to keep using EchoAgent Build"
    );
}

#[test]
fn upsell_max_unified_card_mentions_purchasing() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    open_upsell_max_card(&mut app, CreditLimitUpsellMode::UnifiedCredits);
    let blk = last_credit_limit_block(&app, before);
    assert_eq!(
        blk.action,
        crate::scrollback::blocks::CreditLimitCardAction::PurchaseCredits
    );
    assert!(blk.heading.contains("weekly limit"));
}

#[test]
fn credit_limit_upsell_mode_prefers_unified_flag() {
    let mut bal = test_bal(100.0);
    bal.is_unified_billing_user = Some(true);
    bal.pay_as_you_go = true; // must not override explicit unified
    assert_eq!(
        credit_limit_upsell_mode(Some(&bal)),
        CreditLimitUpsellMode::UnifiedCredits
    );
    bal.is_unified_billing_user = Some(false);
    bal.pay_as_you_go = false;
    assert_eq!(
        credit_limit_upsell_mode(Some(&bal)),
        CreditLimitUpsellMode::LegacyPayg { enabled: false }
    );
    bal.is_unified_billing_user = None;
    bal.pay_as_you_go = true;
    assert_eq!(
        credit_limit_upsell_mode(Some(&bal)),
        CreditLimitUpsellMode::LegacyPayg { enabled: true }
    );
    bal.pay_as_you_go = false;
    assert_eq!(
        credit_limit_upsell_mode(Some(&bal)),
        CreditLimitUpsellMode::UnifiedCredits
    );
    assert_eq!(
        credit_limit_upsell_mode(None),
        CreditLimitUpsellMode::UnifiedCredits
    );
}

#[test]
fn is_credit_limit_error_matches_legacy_403_and_pool_402() {
    assert!(is_credit_limit_error(
        Some(403),
        "status 403: run out of credits"
    ));
    // 402 Payment Required is always credit/spend on this surface.
    assert!(is_credit_limit_error(Some(402), "anything"));
    assert!(is_credit_limit_error(
        None,
        "API error (status 402 Payment Required): EchoAgent Build usage balance exhausted"
    ));
    assert!(is_credit_limit_error(
        None,
        "status 403: run out of credits"
    ));
    assert!(!is_credit_limit_error(Some(403), "content safety blocked"));
    assert!(!is_credit_limit_error(Some(500), "internal server error"));
    // Pool phrases alone without 402/403 status do not match.
    assert!(!is_credit_limit_error(
        None,
        "usage balance exhausted without status"
    ));
}

#[test]
fn upsell_non_max_sets_no_freeform() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert!(
        agent_qv(&app).no_freeform,
        "upsell Q&A should disable freeform input"
    );
}

#[test]
fn upsell_non_max_qa_has_single_select() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert_eq!(
        agent_qv(&app).questions[0].multi_select,
        Some(false),
        "upsell should be single-select"
    );
}

#[test]
fn upsell_non_max_does_not_push_scrollback_block() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert_eq!(
        agent_scrollback_len(&app),
        before,
        "non-max-tier upsell should NOT push a scrollback block"
    );
}

#[test]
fn upsell_non_max_idempotent_when_question_view_already_open() {
    let mut app = test_app_with_agent();
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert!(app.agents.get(&AgentId(0)).unwrap().question_view.is_some());

    // Second call should be a no-op (guard at line 2070).
    let before = agent_scrollback_len(&app);
    open_upsell_qa(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert_eq!(
        agent_scrollback_len(&app),
        before,
        "second call should not push a block"
    );
}

#[test]
fn upsell_max_tier_pushes_scrollback_card_payg_off() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    open_upsell_max_card(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert!(
        app.agents.get(&AgentId(0)).unwrap().question_view.is_none(),
        "max-tier should NOT open the question modal"
    );
    assert_eq!(agent_scrollback_len(&app), before + 1);
    let blk = last_credit_limit_block(&app, before);
    assert!(blk.heading.contains("credit limit"));
    assert_eq!(
        blk.action,
        crate::scrollback::blocks::CreditLimitCardAction::EnablePayg
    );
    assert_eq!(blk.url, UPSELL_URL_PAYG);
}

#[test]
fn upsell_max_tier_pushes_scrollback_card_payg_on() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    open_upsell_max_card(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: true },
    );
    assert!(app.agents.get(&AgentId(0)).unwrap().question_view.is_none());
    assert_eq!(agent_scrollback_len(&app), before + 1);
    let blk = last_credit_limit_block(&app, before);
    assert!(blk.heading.contains("spending cap"));
    assert_eq!(
        blk.action,
        crate::scrollback::blocks::CreditLimitCardAction::IncreasePaygLimit
    );
    assert_eq!(blk.url, UPSELL_URL_PAYG);
}

#[test]
fn upsell_max_tier_does_not_open_question_view() {
    let mut app = test_app_with_agent();
    open_upsell_max_card(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert!(
        app.agents.get(&AgentId(0)).unwrap().question_view.is_none(),
        "max-tier should use inline card, not question modal"
    );
}

#[test]
fn upsell_max_tier_scrollback_card_url_is_payg() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    open_upsell_max_card(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert_eq!(last_credit_limit_block(&app, before).url, UPSELL_URL_PAYG);
}

#[test]
fn upsell_max_tier_not_idempotent_pushes_multiple_cards() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    // Max-tier path doesn't guard against duplicates — each call
    // pushes a new inline card.
    open_upsell_max_card(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    open_upsell_max_card(
        &mut app,
        CreditLimitUpsellMode::LegacyPayg { enabled: false },
    );
    assert_eq!(
        agent_scrollback_len(&app),
        before + 2,
        "max-tier path pushes a card on every call"
    );
}

// ── ShowUsage / session usage ───────────────────────────────────────

fn is_session_usage_fetch(effects: &[Effect]) -> bool {
    matches!(
        effects,
        [Effect::FetchSessionUsage { agent_id, .. }] if *agent_id == AgentId(0)
    )
}

fn is_nonsilent_billing(effects: &[Effect]) -> bool {
    matches!(
        effects,
        [Effect::FetchBilling { agent_id, silent, .. }] if *agent_id == AgentId(0) && !*silent
    )
}

fn complete_session_usage(
    app: &mut AppView,
    session_id: &str,
    usage: echo_agent_runtime::extensions::notification::PromptUsage,
) -> Vec<Effect> {
    dispatch(
        Action::TaskComplete(TaskResult::SessionUsageComplete {
            agent_id: AgentId(0),
            session_id: session_id.to_string().into(),
            usage: Box::new(usage),
            nonce: Default::default(),
        }),
        app,
    )
}

fn fail_session_usage(app: &mut AppView, session_id: &str, error: &str) -> Vec<Effect> {
    dispatch(
        Action::TaskComplete(TaskResult::SessionUsageFailed {
            agent_id: AgentId(0),
            session_id: session_id.to_string().into(),
            error: error.into(),
            nonce: Default::default(),
        }),
        app,
    )
}

#[test]
fn show_usage_schedules_session_fetch_only() {
    let mut app = test_app_with_agent();
    // Scrollback flow is minimal-only.
    app.screen_mode = crate::app::ScreenMode::Minimal;
    assert!(is_session_usage_fetch(&dispatch(
        Action::ShowUsage,
        &mut app
    )));

    app.usage_visible = false;
    assert!(is_session_usage_fetch(&dispatch(
        Action::ShowUsage,
        &mut app
    )));
}

#[test]
fn show_usage_without_session_still_surfaces_credits() {
    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    app.agents.get_mut(&AgentId(0)).unwrap().session.session_id = None;
    let before = agent_scrollback_len(&app);
    let effects = dispatch(Action::ShowUsage, &mut app);
    assert!(last_system_text(&app, AgentId(0)).contains("unavailable"));
    assert_eq!(agent_scrollback_len(&app), before + 1);
    assert!(is_nonsilent_billing(&effects));
}

#[test]
fn team_auth_disables_agent_billing_surface() {
    let mut app = test_app_with_agent();
    app.agents
        .get_mut(&AgentId(0))
        .unwrap()
        .billing_surface_visible = true;
    app.apply_auth_meta(&echo_agent_runtime::auth::AuthMeta {
        team_id: Some("team-uuid".into()),
        team_name: Some("Acme Corp".into()),
        ..Default::default()
    });
    assert!(!app.usage_visible);
    assert!(!app.agents.get(&AgentId(0)).unwrap().billing_surface_visible);
}

#[serial_test::serial(ECHO_AGENT_TEST_OPEN_URL_FILE)]
#[test]
fn manage_billing_gates_on_consumer_billing_surface() {
    let out = std::env::temp_dir().join(format!(
        "echo-agent-manage-billing-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&out);
    // SAFETY: serialized via `serial_test` so no other test races the env var.
    unsafe { std::env::set_var("ECHO_AGENT_TEST_OPEN_URL_FILE", &out) };
    let mut app = test_app_with_agent();
    dispatch(Action::ManageBilling, &mut app);
    let opened = std::fs::read_to_string(&out).unwrap_or_default();
    assert!(
        opened.contains("cloud.echo-agent.invalid/?_s=usage"),
        "got: {opened}"
    );
    let _ = std::fs::remove_file(&out);

    // Non-consumer: silent no-op (slash command never offers manage).
    let mut app = test_app_with_agent();
    app.usage_visible = false;
    let before = agent_scrollback_len(&app);
    assert!(dispatch(Action::ManageBilling, &mut app).is_empty());
    assert_eq!(agent_scrollback_len(&app), before);
}

#[test]
fn session_usage_complete_pushes_block_and_chains_billing() {
    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    let before = agent_scrollback_len(&app);
    let usage = echo_agent_runtime::extensions::notification::PromptUsage {
        totals: echo_agent_runtime::extensions::notification::PromptUsageModel {
            input_tokens: 1_000,
            output_tokens: 100,
            total_tokens: 1_100,
            model_calls: 3,
            cost_usd_ticks: Some(5_000_000_000),
            ..Default::default()
        },
        ..Default::default()
    };
    let effects = complete_session_usage(&mut app, "test-session", usage);
    assert_eq!(agent_scrollback_len(&app), before + 1);
    let text = last_system_text(&app, AgentId(0));
    assert!(
        text.contains("Session usage") && text.contains("$0.5000"),
        "{text}"
    );
    assert!(is_nonsilent_billing(&effects));
}

#[test]
fn session_usage_complete_no_billing_when_surface_hidden() {
    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    app.usage_visible = false;
    let before = agent_scrollback_len(&app);
    let effects = complete_session_usage(&mut app, "test-session", Default::default());
    assert!(effects.is_empty());
    // Only the credit follow-up is gated; the session block itself must land.
    assert_eq!(agent_scrollback_len(&app), before + 1);
}

#[test]
fn session_usage_complete_redirect_after_session_block() {
    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    app.usage_billing_redirect_url = Some("https://billing.example.com/me".into());
    // Dispatch defers the redirect until after the session block.
    let before = agent_scrollback_len(&app);
    assert!(is_session_usage_fetch(&dispatch(
        Action::ShowUsage,
        &mut app
    )));
    assert_eq!(agent_scrollback_len(&app), before);

    let effects = complete_session_usage(&mut app, "test-session", Default::default());
    assert!(effects.is_empty());
    assert_eq!(agent_scrollback_len(&app), before + 2);
    assert!(last_system_text(&app, AgentId(0)).contains("https://billing.example.com/me"));
}

#[test]
fn session_usage_complete_drops_stale_session() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    let effects = complete_session_usage(
        &mut app,
        "old-session",
        echo_agent_runtime::extensions::notification::PromptUsage {
            totals: echo_agent_runtime::extensions::notification::PromptUsageModel {
                model_calls: 99,
                cost_usd_ticks: Some(1_000_000_000_000),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert!(effects.is_empty());
    assert_eq!(agent_scrollback_len(&app), before);
}

#[test]
fn session_usage_failed_pushes_error_and_chains_billing() {
    let mut app = test_app_with_agent();
    app.screen_mode = crate::app::ScreenMode::Minimal;
    let before = agent_scrollback_len(&app);
    let effects = fail_session_usage(&mut app, "test-session", "boom");
    assert_eq!(agent_scrollback_len(&app), before + 1);
    assert!(last_system_text(&app, AgentId(0)).contains("Couldn't load session usage: boom"));
    assert!(is_nonsilent_billing(&effects));
}

#[test]
fn session_usage_failed_drops_stale_session() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    assert!(fail_session_usage(&mut app, "old-session", "boom").is_empty());
    assert_eq!(agent_scrollback_len(&app), before);
}

// ── BillingFetched dispatch tests ───────────────────────────────────

#[test]
fn billing_fetched_updates_app_credit_balance() {
    let mut app = test_app_with_agent();
    dispatch_billing(&mut app, Some(test_bal(42.0)), true, None);
    assert!(app.credit_balance.is_some());
    assert_eq!(app.credit_balance.as_ref().unwrap().usage_pct, 42.0);
}

#[test]
fn billing_fetched_updates_subscription_tier() {
    let mut app = test_app_with_agent();
    dispatch_billing(&mut app, None, true, Some("echoagent_heavy".into()));
    assert_eq!(app.subscription_tier.as_deref(), Some("echoagent_heavy"));
}

#[test]
fn billing_fetched_silent_does_not_push_scrollback() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    dispatch_billing(&mut app, Some(test_bal(50.0)), true, None);
    assert_eq!(
        agent_scrollback_len(&app),
        before,
        "silent billing fetch should not push a scrollback message"
    );
}

#[test]
fn billing_fetched_non_silent_pushes_scrollback_message() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    let bal = crate::views::credit_bar::CreditBalance {
        pay_as_you_go: true,
        on_demand_cap_cents: Some(1000),
        on_demand_used_cents: Some(350),
        period_end_display: Some("Jul 1, 00:00".into()),
        ..test_bal(75.5)
    };
    dispatch_billing(&mut app, Some(bal), false, None);
    assert_eq!(
        agent_scrollback_len(&app),
        before + 1,
        "non-silent billing fetch should push a scrollback message"
    );
}

#[test]
fn billing_fetched_none_balance_shows_no_data_message() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    dispatch_billing(&mut app, None, false, None);
    assert_eq!(agent_scrollback_len(&app), before + 1);
}

#[test]
fn billing_fetched_none_balance_clears_cached() {
    let mut app = test_app_with_agent();
    // Seed a known balance + polling, as a prior successful fetch would.
    dispatch_billing(&mut app, Some(test_bal(80.0)), true, None);
    app.billing_poll_wanted = true;
    // A response carrying no billing config clears the cached balance and
    // polling so the status bar agrees with the "No billing data" message
    // (parse/transport failures route to BillingError, not here).
    dispatch_billing(&mut app, None, false, None);
    assert!(
        app.credit_balance.is_none(),
        "None balance should clear the cached credit balance"
    );
    assert!(
        !app.billing_poll_wanted,
        "None balance should disable billing polling"
    );
}

#[test]
fn billing_fetched_high_usage_enables_poll() {
    let mut app = test_app_with_agent();
    assert!(!app.billing_poll_wanted);
    dispatch_billing(&mut app, Some(test_bal(99.5)), true, None);
    assert!(
        app.billing_poll_wanted,
        "usage >= 99% should enable billing polling"
    );
}

#[test]
fn billing_fetched_low_usage_disables_poll() {
    let mut app = test_app_with_agent();
    app.billing_poll_wanted = true;
    dispatch_billing(&mut app, Some(test_bal(50.0)), true, None);
    assert!(
        !app.billing_poll_wanted,
        "usage < 99% should disable billing polling"
    );
}

#[test]
fn billing_fetched_propagates_balance_to_agent() {
    let mut app = test_app_with_agent();
    let bal = crate::views::credit_bar::CreditBalance {
        effective_usage_pct: 60.0,
        pay_as_you_go: true,
        on_demand_cap_cents: Some(5000),
        on_demand_used_cents: Some(1200),
        period_end_display: Some("Aug 15, 00:00".into()),
        ..test_bal(88.0)
    };
    dispatch_billing(&mut app, Some(bal), true, None);
    let agent_bal = app
        .agents
        .get(&AgentId(0))
        .unwrap()
        .credit_balance
        .as_ref()
        .unwrap();
    assert_eq!(agent_bal.usage_pct, 88.0);
    assert_eq!(agent_bal.effective_usage_pct, 60.0);
    assert!(agent_bal.pay_as_you_go);
    assert_eq!(agent_bal.on_demand_cap_cents, Some(5000));
    assert_eq!(agent_bal.on_demand_used_cents, Some(1200));
}

#[test]
fn billing_fetched_stores_autotopup_on_app_and_agent() {
    let mut app = test_app_with_agent();
    let bal = crate::views::credit_bar::CreditBalance {
        prepaid_balance_cents: Some(1500),
        ..test_bal(100.0)
    };
    let autotopup = crate::views::credit_bar::AutoTopupInfo {
        enabled: true,
        topup_amount_cents: Some(2000),
        max_amount_cents: Some(10000),
    };
    dispatch(
        Action::TaskComplete(TaskResult::BillingFetched {
            agent_id: AgentId(0),
            balance: Some(bal),
            silent: true,
            subscription_tier: None,
            autotopup: crate::views::credit_bar::AutoTopupFetch::Resolved(autotopup),
            nonce: Default::default(),
        }),
        &mut app,
    );
    assert!(app.auto_topup.as_ref().is_some_and(|at| at.enabled));
    let agent_at = app.agents.get(&AgentId(0)).unwrap().auto_topup.as_ref();
    assert_eq!(agent_at.and_then(|at| at.max_amount_cents), Some(10000));
}

#[test]
fn billing_fetched_unchanged_autotopup_keeps_cached_rule() {
    let mut app = test_app_with_agent();
    let bal = || crate::views::credit_bar::CreditBalance {
        prepaid_balance_cents: Some(1500),
        ..test_bal(100.0)
    };
    let resolved = crate::views::credit_bar::AutoTopupFetch::Resolved(
        crate::views::credit_bar::AutoTopupInfo {
            enabled: true,
            topup_amount_cents: Some(2000),
            max_amount_cents: None,
        },
    );
    dispatch(
        Action::TaskComplete(TaskResult::BillingFetched {
            agent_id: AgentId(0),
            balance: Some(bal()),
            silent: true,
            subscription_tier: None,
            autotopup: resolved,
            nonce: Default::default(),
        }),
        &mut app,
    );
    // A later refresh whose auto-topup fetch failed must not clear the rule.
    dispatch(
        Action::TaskComplete(TaskResult::BillingFetched {
            agent_id: AgentId(0),
            balance: Some(bal()),
            silent: true,
            subscription_tier: None,
            autotopup: crate::views::credit_bar::AutoTopupFetch::Unchanged,
            nonce: Default::default(),
        }),
        &mut app,
    );
    assert!(app.auto_topup.as_ref().is_some_and(|at| at.enabled));
    let agent_at = app.agents.get(&AgentId(0)).unwrap().auto_topup.as_ref();
    assert!(agent_at.is_some_and(|at| at.enabled));
}

#[test]
fn billing_fetched_cleared_autotopup_resets_cache() {
    let mut app = test_app_with_agent();
    // Seed a known rule while credits exist.
    dispatch(
        Action::TaskComplete(TaskResult::BillingFetched {
            agent_id: AgentId(0),
            balance: Some(crate::views::credit_bar::CreditBalance {
                prepaid_balance_cents: Some(1500),
                ..test_bal(100.0)
            }),
            silent: true,
            subscription_tier: None,
            autotopup: crate::views::credit_bar::AutoTopupFetch::Resolved(
                crate::views::credit_bar::AutoTopupInfo {
                    enabled: true,
                    topup_amount_cents: Some(2000),
                    max_amount_cents: None,
                },
            ),
            nonce: Default::default(),
        }),
        &mut app,
    );
    // Credits gone → `Cleared` resets the cached rule to "unknown" so a later
    // credits period can't read a stale rule.
    dispatch(
        Action::TaskComplete(TaskResult::BillingFetched {
            agent_id: AgentId(0),
            balance: Some(test_bal(50.0)),
            silent: true,
            subscription_tier: None,
            autotopup: crate::views::credit_bar::AutoTopupFetch::Cleared,
            nonce: Default::default(),
        }),
        &mut app,
    );
    assert!(app.auto_topup.is_none());
    assert!(app.agents.get(&AgentId(0)).unwrap().auto_topup.is_none());
}

#[test]
fn app_billing_fetched_stores_autotopup() {
    let mut app = test_app_with_agent();
    let bal = crate::views::credit_bar::CreditBalance {
        prepaid_balance_cents: Some(500),
        ..test_bal(0.0)
    };
    dispatch(
        Action::TaskComplete(TaskResult::AppBillingFetched {
            balance: Some(bal),
            autotopup: crate::views::credit_bar::AutoTopupFetch::Resolved(
                crate::views::credit_bar::AutoTopupInfo::disabled(),
            ),
        }),
        &mut app,
    );
    assert_eq!(
        app.credit_balance.and_then(|b| b.prepaid_balance_cents),
        Some(500)
    );
    assert!(app.auto_topup.is_some_and(|at| !at.enabled));
}

// ── BillingError dispatch tests ─────────────────────────────────────

#[test]
fn billing_error_silent_does_not_push_scrollback() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    dispatch(
        Action::TaskComplete(TaskResult::BillingError {
            agent_id: AgentId(0),
            error: "network timeout".into(),
            silent: true,
            nonce: Default::default(),
        }),
        &mut app,
    );
    assert_eq!(
        agent_scrollback_len(&app),
        before,
        "silent billing error should not push a scrollback message"
    );
}

#[test]
fn billing_error_non_silent_pushes_error_message() {
    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    dispatch(
        Action::TaskComplete(TaskResult::BillingError {
            agent_id: AgentId(0),
            error: "service unavailable".into(),
            silent: false,
            nonce: Default::default(),
        }),
        &mut app,
    );
    assert_eq!(
        agent_scrollback_len(&app),
        before + 1,
        "non-silent billing error should push an error message"
    );
}

// ── Free-usage paywall tests ────────────────────────────────────────

#[test]
fn free_usage_error_detected_by_embedded_code() {
    // parse_error_bytes flattens the 429 body to "<code>: <message>".
    assert!(is_free_usage_exhausted_error(
        "API error (status 429 Too Many Requests): \
         subscription:free-usage-exhausted: You have used all your free usage."
    ));
    // Generic rate limits and other WKE codes must not match.
    assert!(!is_free_usage_exhausted_error(
        "API error (status 429 Too Many Requests): Rate limit exceeded"
    ));
    assert!(!is_free_usage_exhausted_error(
        "unauthorized:missing-acl: nope"
    ));
}

#[test]
fn free_usage_upsell_shows_three_options_with_exact_labels() {
    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    open_free_usage_upsell(agent, None);

    let qv = agent_qv(&app);
    assert!(matches!(
        qv.local_kind,
        Some(
            crate::views::question_view::LocalQuestionKind::FreeUsageUpsell {
                source: echo_agent_telemetry::events::EchoAgentUpsell::FreeUsagePaywall,
            }
        )
    ));
    let q = &qv.questions[0];
    assert_eq!(q.question, "You hit your free usage limit.");
    let expected = [
        (
            "Upgrade to EchoAgent",
            "For everyday coding and productivity tasks",
            Some(UPSELL_URL_UPGRADE),
        ),
        (
            "Upgrade to EchoAgent Plus",
            "Significantly higher usage and rate limits",
            Some(UPSELL_URL_UPGRADE),
        ),
        (
            "Upgrade to EchoAgent Heavy",
            "Get the most out of EchoAgent Build. Highest usage limits.",
            Some(UPSELL_URL_UPGRADE),
        ),
    ];
    assert_eq!(q.options.len(), expected.len());
    for (opt, (label, desc, id)) in q.options.iter().zip(expected) {
        assert_eq!(opt.label, label);
        assert_eq!(opt.description, desc);
        assert_eq!(opt.id.as_deref(), id);
    }
}

/// Replay the REAL free-usage sequence — send → `RetryState::Retrying` →
/// `Exhausted` → PromptResponse error — through the production handlers:
/// the paywall modal must open on the turn-end error.
#[test]
fn free_usage_failure_opens_paywall_modal() {
    use crate::app::acp_handler::apply_session_event_for_test;
    use echo_agent_runtime::extensions::notification::{RetryState, SessionUpdate};

    let mut app = test_app_with_agent();
    let id = AgentId(0);

    // 1. Real send.
    let effects = dispatch(Action::SendPrompt("draw me a cat".into()), &mut app);
    assert!(
        matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "draw me a cat"),
        "send must dispatch: {effects:?}"
    );
    let prompt_id = app.agents[&id].session.current_prompt_id.clone();
    assert!(prompt_id.is_some(), "send must mint a prompt id");

    // 2+3. Real notification sequence through the production handler.
    {
        let agent = app.agents.get_mut(&id).unwrap();
        apply_session_event_for_test(
            &SessionUpdate::RetryState(RetryState::Retrying {
                attempt: 1,
                max_retries: 2,
                reason: "429 Too Many Requests".into(),
            }),
            &mut agent.session,
            &mut agent.scrollback,
        );
        apply_session_event_for_test(
            &SessionUpdate::RetryState(RetryState::Exhausted {
                attempts: 2,
                reason: "API error (status 429 Too Many Requests): \
                         subscription:free-usage-exhausted: You have used all your free usage."
                    .into(),
                is_rate_limited: true,
            }),
            &mut agent.session,
            &mut agent.scrollback,
        );
        assert!(agent.session.free_usage_blocked);
    }

    // 4. Turn-end RPC error opens the upsell modal.
    let _ = dispatch(
        Action::TaskComplete(TaskResult::PromptResponse {
            agent_id: id,
            result: Err("rate limited".into()),
            http_status: Some(429),
            prompt_id,
        }),
        &mut app,
    );
    assert!(
        app.agents[&id].question_view.is_some(),
        "paywall modal must open"
    );
}

/// Answer translation: every upgrade option opens the upgrade URL.
#[test]
fn free_usage_translate_local_submit_maps_options() {
    use crate::app::agent_view::translate_local_submit_for_test;
    use crate::app::app_view::InputOutcome;
    use crate::views::question_view::{LocalQuestionKind, QuestionSelection};

    let mut app = test_app_with_agent();
    let agent = app.agents.get_mut(&AgentId(0)).unwrap();
    open_free_usage_upsell(agent, None);
    let mut qv = agent.question_view.take().unwrap();
    let kind = || LocalQuestionKind::FreeUsageUpsell {
        source: echo_agent_telemetry::events::EchoAgentUpsell::FreeUsagePaywall,
    };

    for idx in [0, 1, 2] {
        qv.selections[0] = QuestionSelection::Single(Some(idx));
        match translate_local_submit_for_test(&qv, kind(), false) {
            InputOutcome::Action(Action::OpenUrl(url)) => assert_eq!(url, UPSELL_URL_UPGRADE),
            other => panic!("expected OpenUrl for option {idx}, got {other:?}"),
        }
    }
}

// ── Restricted-command upsell tests ─────────────────────────────────

/// Submitting a tier-restricted command opens the three-option EchoAgent
/// upsell and neither runs the command nor leaks the text to the model.
#[test]
fn restricted_command_submit_opens_three_option_upsell() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents
        .get_mut(&id)
        .unwrap()
        .set_restricted_commands(&["imagine".to_string()]);

    let effects = dispatch(Action::SendPrompt("/imagine a sunset".into()), &mut app);

    assert!(
        effects.is_empty(),
        "restricted command must not produce a SendPrompt: {effects:?}"
    );
    let agent = &app.agents[&id];
    assert!(
        agent.session.pending_prompts.is_empty(),
        "restricted command must not be enqueued"
    );
    assert!(agent.prompt.text().is_empty(), "composer consumed");

    let qv = agent_qv(&app);
    assert!(matches!(
        qv.local_kind,
        Some(
            crate::views::question_view::LocalQuestionKind::FreeUsageUpsell {
                source: echo_agent_telemetry::events::EchoAgentUpsell::RestrictedCommand,
            }
        )
    ));
    let q = &qv.questions[0];
    assert_eq!(q.question, "Unlock all features with EchoAgent.");
    assert_eq!(q.options.len(), 3);
    assert_eq!(q.options[0].label, "Upgrade to EchoAgent");
    assert_eq!(q.options[0].id.as_deref(), Some(UPSELL_URL_UPGRADE));
    assert_eq!(q.options[1].label, "Upgrade to EchoAgent Plus");
    assert_eq!(q.options[1].id.as_deref(), Some(UPSELL_URL_UPGRADE));
    assert_eq!(q.options[2].label, "Upgrade to EchoAgent Heavy");
    assert_eq!(q.options[2].id.as_deref(), Some(UPSELL_URL_UPGRADE));
}

/// Aliases of a restricted command hit the same upsell (deny-list
/// matching covers aliases via the registry).
#[test]
fn restricted_command_alias_also_upsells() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents
        .get_mut(&id)
        .unwrap()
        .set_restricted_commands(&["usage".to_string()]);

    let effects = dispatch(Action::SendPrompt("/cost".into()), &mut app);

    assert!(effects.is_empty());
    assert!(app.agents[&id].question_view.is_some(), "upsell must open");
}

/// A restricted submit while ANOTHER question modal is already open
/// must not silently drop the typed text — the upsell can't open (the guard
/// never displaces a modal), so the composer keeps the text for a resubmit
/// after the modal closes. No passthrough, nothing enqueued, and the
/// existing modal survives untouched.
#[test]
fn restricted_command_with_open_modal_keeps_composer_text() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    {
        let agent = app.agents.get_mut(&id).unwrap();
        agent.set_restricted_commands(&["imagine".to_string()]);
        // A question modal is already up (credit-limit upsell).
        open_credit_limit_upsell(agent, CreditLimitUpsellMode::UnifiedCredits, false);
        assert!(agent.question_view.is_some());
        // The user typed the restricted command into the composer.
        agent.prompt.set_text("/imagine a sunset");
    }

    let effects = dispatch(Action::SendPrompt("/imagine a sunset".into()), &mut app);

    assert!(effects.is_empty(), "no passthrough / send: {effects:?}");
    let agent = &app.agents[&id];
    assert_eq!(
        agent.prompt.text(),
        "/imagine a sunset",
        "composer text must be preserved for a later resubmit"
    );
    assert!(
        matches!(
            agent
                .question_view
                .as_ref()
                .and_then(|qv| qv.local_kind.as_ref()),
            Some(crate::views::question_view::LocalQuestionKind::CreditLimitUpsell { .. })
        ),
        "the pre-existing modal must survive (no second modal)"
    );
    assert!(
        agent.session.pending_prompts.is_empty(),
        "nothing may be enqueued"
    );
}

/// Regression: genuinely unknown (non-restricted) commands keep the
/// PassThrough behavior shell/ACP commands rely on.
#[test]
fn unknown_non_restricted_command_still_passes_through() {
    let mut app = test_app_with_agent();
    let id = AgentId(0);
    app.agents
        .get_mut(&id)
        .unwrap()
        .set_restricted_commands(&["imagine".to_string()]);

    let effects = dispatch(Action::SendPrompt("/frobnicate arg".into()), &mut app);

    assert_eq!(effects.len(), 1);
    assert!(
        matches!(&effects[0], Effect::SendPrompt { text, .. } if text == "/frobnicate arg"),
        "unknown command must still pass through: {effects:?}"
    );
    assert!(
        app.agents[&id].question_view.is_none(),
        "no upsell for genuinely unknown commands"
    );
}

// ── Browser-unavailable URL fallback ────────────────────────────────

/// When the OS browser opener cannot run (simulated via a broken
/// `ECHO_AGENT_TEST_OPEN_URL_FILE` seam), `Action::OpenUrl` for a billing CTA
/// must push a scrollback system message that includes the full URL —
/// the headless-VM fix for silent Upgrade / Buy-more-credits no-ops.
#[serial_test::serial(ECHO_AGENT_TEST_OPEN_URL_FILE)]
#[test]
fn open_url_shows_manual_url_when_browser_unavailable() {
    // Point the test seam at a path whose parent dir does not exist so the
    // write fails and `open_url` returns false (BrowserUnavailable).
    let bad = std::env::temp_dir().join(format!(
        "echo-agent-open-url-missing-{}/out.txt",
        std::process::id()
    ));
    // SAFETY: serialized via `serial_test` so no other test races the env var.
    unsafe { std::env::set_var("ECHO_AGENT_TEST_OPEN_URL_FILE", &bad) };

    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    let url = UPSELL_URL_UPGRADE;
    let effects = dispatch(Action::OpenUrl(url.to_string()), &mut app);
    assert!(effects.is_empty());

    assert_eq!(
        agent_scrollback_len(&app),
        before + 1,
        "must push a system message with the URL"
    );
    let text = last_system_text(&app, AgentId(0));
    assert_eq!(
        text,
        crate::app::link_opener::browser_unavailable_message(url)
    );
    let toast = app.agents[&AgentId(0)]
        .toast
        .as_ref()
        .map(|(m, _)| m.as_str());
    assert_eq!(toast, Some("Browser unavailable - URL shown above"));

    // SAFETY: serialized via `serial_test`; restore the env for other tests.
    unsafe { std::env::remove_var("ECHO_AGENT_TEST_OPEN_URL_FILE") };
}

/// Successful open (test seam write OK) must not spam a fallback system message.
#[serial_test::serial(ECHO_AGENT_TEST_OPEN_URL_FILE)]
#[test]
fn open_url_does_not_show_fallback_when_opener_succeeds() {
    let url_file =
        std::env::temp_dir().join(format!("echo-agent-open-url-ok-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&url_file);
    // SAFETY: serialized via `serial_test`.
    unsafe { std::env::set_var("ECHO_AGENT_TEST_OPEN_URL_FILE", &url_file) };

    let mut app = test_app_with_agent();
    let before = agent_scrollback_len(&app);
    let url = UPSELL_URL_PAYG;
    let _ = dispatch(Action::OpenUrl(url.to_string()), &mut app);

    assert_eq!(
        agent_scrollback_len(&app),
        before,
        "successful open must not push a fallback system message"
    );
    let recorded = std::fs::read_to_string(&url_file).unwrap_or_default();
    assert!(
        recorded.lines().any(|l| l == url),
        "opener seam must record the URL; got {recorded:?}"
    );

    // SAFETY: serialized via `serial_test`.
    unsafe { std::env::remove_var("ECHO_AGENT_TEST_OPEN_URL_FILE") };
    let _ = std::fs::remove_file(&url_file);
}

/// Welcome has no scrollback: browser-unavailable OpenUrl must put a
/// single-line toast that includes the full URL (no `\n` — the welcome
/// painter is one row). Privacy-banner Terms/Policy clicks hit this path.
#[serial_test::serial(ECHO_AGENT_TEST_OPEN_URL_FILE)]
#[test]
fn open_url_welcome_toasts_single_line_url_when_browser_unavailable() {
    let bad = std::env::temp_dir().join(format!(
        "echo-agent-open-url-welcome-missing-{}/out.txt",
        std::process::id()
    ));
    // SAFETY: serialized via `serial_test` so no other test races the env var.
    unsafe { std::env::set_var("ECHO_AGENT_TEST_OPEN_URL_FILE", &bad) };

    let mut app = test_app();
    assert!(
        matches!(app.active_view, ActiveView::Welcome),
        "fixture must start on welcome"
    );

    use crate::app::link_opener::browser_unavailable_line;

    let terms = crate::views::privacy_banner::PRIVACY_BANNER_TERMS_URL;
    let effects = dispatch(Action::OpenUrl(terms.to_string()), &mut app);
    assert!(effects.is_empty());
    let toast = app
        .welcome_toast
        .as_ref()
        .map(|(m, _)| m.as_str())
        .unwrap_or("");
    // Structure only: clipboard delivery varies by host, so do not lock the
    // exact "copied" phrase here (constructor unit tests cover both arms).
    assert!(toast.starts_with(terms), "{toast}");
    assert!(!toast.contains('\n'), "{toast}");
    assert!(
        toast == browser_unavailable_line(terms, true)
            || toast == browser_unavailable_line(terms, false),
        "welcome toast must match a delivery-honest line form: {toast}"
    );

    // Policy URL is shorter than terms (compile-time constants); second toast
    // replaces the first in welcome toast state.
    let policy = crate::views::privacy_banner::PRIVACY_BANNER_POLICY_URL;
    let _ = dispatch(Action::OpenUrl(policy.to_string()), &mut app);
    let toast = app
        .welcome_toast
        .as_ref()
        .map(|(m, _)| m.as_str())
        .unwrap_or("");
    assert!(toast.starts_with(policy), "{toast}");
    assert!(
        toast == browser_unavailable_line(policy, true)
            || toast == browser_unavailable_line(policy, false),
        "second welcome toast must match a delivery-honest line form: {toast}"
    );

    // SAFETY: serialized via `serial_test`; restore the env for other tests.
    unsafe { std::env::remove_var("ECHO_AGENT_TEST_OPEN_URL_FILE") };
}

/// Credit-limit upsell Q&A submit routes through OpenUrl; when the browser
/// is unavailable the full option URL must land in scrollback.
#[serial_test::serial(ECHO_AGENT_TEST_OPEN_URL_FILE)]
#[test]
fn credit_limit_upsell_submit_shows_url_when_browser_unavailable() {
    use crate::app::agent_view::translate_local_submit_for_test;
    use crate::app::app_view::InputOutcome;
    use crate::views::question_view::{LocalQuestionKind, QuestionSelection};

    let bad = std::env::temp_dir().join(format!(
        "echo-agent-open-url-upsell-missing-{}/out.txt",
        std::process::id()
    ));
    // SAFETY: serialized via `serial_test`.
    unsafe { std::env::set_var("ECHO_AGENT_TEST_OPEN_URL_FILE", &bad) };

    let mut app = test_app_with_agent();
    open_upsell_qa(&mut app, CreditLimitUpsellMode::UnifiedCredits);
    let mut qv = app
        .agents
        .get_mut(&AgentId(0))
        .unwrap()
        .question_view
        .take()
        .expect("expected credit-limit upsell modal");
    // Select option 1 = "Buy more credits" (credits / usage URL).
    qv.selections[0] = QuestionSelection::Single(Some(1));
    let kind = LocalQuestionKind::CreditLimitUpsell {
        choices: vec![
            echo_agent_telemetry::events::CreditLimitChoice::UpgradeTier,
            echo_agent_telemetry::events::CreditLimitChoice::PurchaseCredits,
        ],
    };
    let InputOutcome::Action(Action::OpenUrl(url)) =
        translate_local_submit_for_test(&qv, kind, false)
    else {
        panic!("expected OpenUrl from upsell submit");
    };
    assert_eq!(url, UPSELL_URL_PAYG);

    let before = agent_scrollback_len(&app);
    let _ = dispatch(Action::OpenUrl(url.clone()), &mut app);
    let text = last_system_text(&app, AgentId(0));
    assert_eq!(agent_scrollback_len(&app), before + 1);
    assert!(
        text.contains(&url),
        "upsell URL missing from fallback: {text}"
    );

    // SAFETY: serialized via `serial_test`.
    unsafe { std::env::remove_var("ECHO_AGENT_TEST_OPEN_URL_FILE") };
}

#[test]
fn billing_fetched_clears_usage_modal_loading() {
    let mut app = test_app_with_agent();
    dispatch(Action::ShowUsage, &mut app);
    dispatch_billing(
        &mut app,
        Some(test_bal(50.0)),
        true,
        Some("EchoAgent".into()),
    );
    let agent = &app.agents[&AgentId(0)];
    let Some(crate::views::modal::ActiveModal::UsageInfo { state }) = agent.active_modal.as_ref()
    else {
        panic!("expected the usage modal to be open");
    };
    assert!(!state.billing_loading);
    assert!(state.billing_error.is_none());
    assert_eq!(state.ctx.subscription_tier.as_deref(), Some("EchoAgent"));
    // The modal renders from the agent's cached billing mirrors.
    assert_eq!(agent.credit_balance.as_ref().unwrap().usage_pct, 50.0);
}

#[test]
fn background_billing_reply_does_not_settle_modal_loading() {
    let mut app = test_app_with_agent();
    dispatch(Action::ShowUsage, &mut app);
    // A turn-end refresh (nonce 0) lands while the modal's own fetch is in
    // flight: mirrors update, but the modal's loading/error flags don't.
    dispatch(
        Action::TaskComplete(TaskResult::BillingError {
            agent_id: AgentId(0),
            error: "background boom".to_string(),
            silent: true,
            nonce: Default::default(),
        }),
        &mut app,
    );
    let Some(crate::views::modal::ActiveModal::UsageInfo { state }) =
        app.agents[&AgentId(0)].active_modal.as_ref()
    else {
        panic!("expected the usage modal to be open");
    };
    assert!(state.billing_loading, "still waiting on its own fetch");
    assert!(state.billing_error.is_none());
}

#[test]
fn billing_error_surfaces_in_usage_modal_without_scrollback() {
    let mut app = test_app_with_agent();
    dispatch(Action::ShowUsage, &mut app);
    let before = agent_scrollback_len(&app);
    let nonce = open_usage_modal_nonce(&app);
    dispatch(
        Action::TaskComplete(TaskResult::BillingError {
            agent_id: AgentId(0),
            error: "billing boom".to_string(),
            silent: true,
            nonce,
        }),
        &mut app,
    );
    let Some(crate::views::modal::ActiveModal::UsageInfo { state }) =
        app.agents[&AgentId(0)].active_modal.as_ref()
    else {
        panic!("expected the usage modal to be open");
    };
    assert!(!state.billing_loading);
    assert_eq!(state.billing_error.as_deref(), Some("billing boom"));
    assert_eq!(agent_scrollback_len(&app), before);
}
