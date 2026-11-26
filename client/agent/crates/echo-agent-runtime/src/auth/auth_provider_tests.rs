// Slot names are process-global, so every test uses a unique name (no #[serial]
// needed). No test mutates the process env: the scrub test sets its leak values
// on the child command instead.

use super::test_counting_provider as counting_provider;
use super::*;

#[tokio::test]
async fn provider_token_is_cached_while_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let provider = counting_provider("test-cache", dir.path());
    assert_eq!(
        provider.cached_token(),
        None,
        "cache-only read must miss on a cold cache without running the command"
    );
    let first = provider.ensure_fresh_token(None).await.rotated().unwrap();
    let second = provider.ensure_fresh_token(None).await.rotated().unwrap();
    assert_eq!(first, "tok-1");
    assert_eq!(second, "tok-1", "fresh token must be served from cache");
    assert_eq!(
        provider.cached_token().as_deref(),
        Some("tok-1"),
        "sync cache-only read must serve the warm cache"
    );
}

#[tokio::test]
async fn provider_token_reminted_when_expired() {
    let dir = tempfile::tempdir().unwrap();
    let provider = counting_provider("test-expiry", dir.path());
    assert_eq!(
        provider.ensure_fresh_token(None).await.rotated().unwrap(),
        "tok-1"
    );
    test_expire_provider_token("test-expiry");
    assert_eq!(
        provider.cached_token(),
        None,
        "cache-only read must not serve a stale token"
    );
    assert_eq!(
        provider.ensure_fresh_token(None).await.rotated().unwrap(),
        "tok-2",
        "expired token must be re-minted"
    );
}

#[tokio::test]
async fn provider_pre_turn_refresh_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let provider = counting_provider("test-stale", dir.path());
    let token = provider.ensure_fresh_token(None).await.rotated().unwrap();

    assert_eq!(
        provider.ensure_fresh_token(Some(&token)).await,
        ProviderRefreshOutcome::Unchanged,
        "fresh matching token must not be re-minted pre-turn"
    );
    assert_eq!(
        provider
            .ensure_fresh_token(Some("lagging-chat-state-key"))
            .await
            .rotated()
            .as_deref(),
        Some("tok-1"),
        "chat-state lagging behind a rotation adopts the fresh cached token"
    );
    test_expire_provider_token("test-stale");
    assert_eq!(
        provider
            .ensure_fresh_token(Some(&token))
            .await
            .rotated()
            .as_deref(),
        Some("tok-2"),
        "stale token must be re-minted pre-turn"
    );
}

#[tokio::test]
async fn provider_401_recovery_has_fresh_mint_guard() {
    let dir = tempfile::tempdir().unwrap();
    let provider = counting_provider("test-401", dir.path());
    let token = provider.ensure_fresh_token(None).await.rotated().unwrap();

    assert_eq!(
        provider.recover_rejected_token(&token).await,
        None,
        "a token minted moments ago must not be re-minted on 401 (loop guard)"
    );

    test_backdate_provider_mint("test-401", std::time::Duration::from_secs(60));
    assert_eq!(
        provider.recover_rejected_token(&token).await.as_deref(),
        Some("tok-2"),
        "an aged rejected token is re-minted once"
    );

    assert_eq!(
        provider.recover_rejected_token(&token).await.as_deref(),
        Some("tok-2"),
        "a rejection of the already-replaced key adopts the fresh token without a re-run"
    );
}

/// Regression: a warm cache must not outlive the provider's config.
#[tokio::test]
async fn provider_removed_from_config_drops_cached_token() {
    let dir = tempfile::tempdir().unwrap();
    let provider = counting_provider("test-removed", dir.path());
    let token = provider.ensure_fresh_token(None).await.rotated().unwrap();

    let removed = AuthProviderRef::new("test-removed".to_owned(), AuthProviderConfig::default());
    assert_eq!(
        removed.cached_token(),
        None,
        "empty command must fail closed even with a warm cache"
    );
    assert_eq!(
        removed.ensure_fresh_token(Some(&token)).await,
        ProviderRefreshOutcome::Unusable
    );
    let restored = counting_provider("test-removed", dir.path());
    assert_eq!(
        restored
            .ensure_fresh_token(Some(&token))
            .await
            .rotated()
            .as_deref(),
        Some("tok-2"),
        "the removed provider's token must not survive in the slot"
    );
}

#[tokio::test]
async fn provider_config_edit_invalidates_cached_token() {
    let dir = tempfile::tempdir().unwrap();
    let old = counting_provider("test-freshen", dir.path());
    assert_eq!(
        old.ensure_fresh_token(None).await.rotated().unwrap(),
        "tok-1"
    );

    let edited = AuthProviderRef::new(
        "test-freshen".to_owned(),
        AuthProviderConfig {
            command: "printf edited-token".to_owned(),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: None,
        },
    );
    assert_eq!(
        edited.cached_token(),
        None,
        "the unexpired old token must not be served under the edited table"
    );
    assert_eq!(
        edited
            .ensure_fresh_token(Some("tok-1"))
            .await
            .rotated()
            .as_deref(),
        Some("edited-token"),
        "refresh must run the edited command without waiting for expiry"
    );
}

/// The fresh-mint guard applies per table version.
#[tokio::test]
async fn provider_401_recovery_reminted_under_edited_config() {
    let dir = tempfile::tempdir().unwrap();
    let old = counting_provider("test-401-edited", dir.path());
    let token = old.ensure_fresh_token(None).await.rotated().unwrap();

    let edited = AuthProviderRef::new(
        "test-401-edited".to_owned(),
        AuthProviderConfig {
            command: "printf new-config-token".to_owned(),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: None,
        },
    );
    assert_eq!(
        edited.recover_rejected_token(&token).await.as_deref(),
        Some("new-config-token"),
        "recovery must run the edited command, not adopt the old-table token"
    );
}

/// Editing only `timeout_secs` keeps the token; it is not part of
/// `token_identity`.
#[tokio::test]
async fn provider_timeout_edit_does_not_invalidate_token() {
    let dir = tempfile::tempdir().unwrap();
    let provider = counting_provider("test-timeout-edit", dir.path());
    provider.ensure_fresh_token(None).await.rotated().unwrap();

    let retimed = AuthProviderRef::new(
        "test-timeout-edit".to_owned(),
        AuthProviderConfig {
            command: provider.config.command.clone(),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: Some(5),
            cwd: None,
        },
    );
    assert_eq!(
        retimed.cached_token().as_deref(),
        Some("tok-1"),
        "a timeout-only edit must not invalidate the cached token"
    );
}

/// `cwd` is part of `token_identity`, so editing it invalidates the cache: the
/// same helper in a different directory can mint a different token.
#[tokio::test]
async fn provider_cwd_edit_invalidates_cached_token() {
    let dir = tempfile::tempdir().unwrap();
    let provider = counting_provider("test-cwd-edit", dir.path());
    provider.ensure_fresh_token(None).await.rotated().unwrap();

    let moved = AuthProviderRef::new(
        "test-cwd-edit".to_owned(),
        AuthProviderConfig {
            command: provider.config.command.clone(),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: Some("/some/other/dir".to_owned()),
        },
    );
    assert_eq!(
        moved.cached_token(),
        None,
        "a cwd edit must invalidate the cached token"
    );
}

#[tokio::test]
async fn attach_trusted_config_lets_a_revived_ref_mint() {
    let dir = tempfile::tempdir().unwrap();
    let template = counting_provider("test-attach", dir.path());
    let mut revived: AuthProviderRef = serde_json::from_str(r#"{"name": "test-attach"}"#).unwrap();
    assert_eq!(
        revived.ensure_fresh_token(None).await,
        ProviderRefreshOutcome::Unusable
    );
    revived.attach_trusted_config(Some(&template.config));
    assert_eq!(
        revived.ensure_fresh_token(None).await.rotated().as_deref(),
        Some("tok-1"),
        "a re-attached ref must be able to mint"
    );
}

/// A ref revived from bytes never mutates the shared slot: a mutating
/// call fails closed and leaves a resolved ref's token intact.
#[tokio::test]
async fn deserialized_ref_never_drops_the_shared_token() {
    let dir = tempfile::tempdir().unwrap();
    let resolved = counting_provider("test-unresolved", dir.path());
    resolved.ensure_fresh_token(None).await.rotated().unwrap();

    let revived: AuthProviderRef = serde_json::from_str(r#"{"name": "test-unresolved"}"#).unwrap();
    assert_eq!(
        revived.ensure_fresh_token(None).await,
        ProviderRefreshOutcome::Unusable
    );
    assert_eq!(revived.recover_rejected_token("tok-1").await, None);
    assert_eq!(
        resolved.cached_token().as_deref(),
        Some("tok-1"),
        "the resolved ref's token must survive a mutating call on the stub"
    );
}

/// A ref serializes to its name only: the revived ref carries no command
/// and fails closed until re-attached, while the shared slot still serves
/// resolved refs of the same name.
#[tokio::test]
async fn provider_ref_serializes_name_only_and_drops_config() {
    let dir = tempfile::tempdir().unwrap();
    let provider = counting_provider("test-serde", dir.path());
    provider.ensure_fresh_token(None).await.rotated().unwrap();

    let bytes = serde_json::to_string(&provider).unwrap();
    assert!(bytes.contains("test-serde"));
    assert!(
        !bytes.contains("tok-%s") && !bytes.contains("command"),
        "the serialized form must carry the name only: {bytes}"
    );
    let revived: AuthProviderRef = serde_json::from_str(&bytes).unwrap();
    assert_eq!(revived.name, "test-serde");
    assert_eq!(
        revived.config,
        AuthProviderConfig::default(),
        "a serialized command must not survive deserialization"
    );
    assert_eq!(
        revived.cached_token(),
        None,
        "an unresolved ref fails closed"
    );
    let same_name = counting_provider("test-serde", dir.path());
    assert_eq!(
        same_name.cached_token().as_deref(),
        Some("tok-1"),
        "the shared slot still serves refs constructed with the real config"
    );
}

#[tokio::test]
async fn provider_refresh_sets_expired_env() {
    let provider = AuthProviderRef::new(
        "test-expired-env".to_owned(),
        AuthProviderConfig {
            command: "printf 'tok-%s' \"${ECHO_AGENT_AUTH_EXPIRED:-0}\"".to_owned(),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: None,
        },
    );
    assert_eq!(
        provider.ensure_fresh_token(None).await.rotated().as_deref(),
        Some("tok-0"),
        "first mint runs without ECHO_AGENT_AUTH_EXPIRED"
    );
    test_expire_provider_token("test-expired-env");
    assert_eq!(
        provider.ensure_fresh_token(None).await.rotated().as_deref(),
        Some("tok-1"),
        "re-mints run with ECHO_AGENT_AUTH_EXPIRED=1"
    );
}

#[tokio::test]
async fn provider_concurrent_mints_single_flight() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("count");
    let provider = AuthProviderRef::new(
        "test-single-flight".to_owned(),
        AuthProviderConfig {
            command: format!(
                "sleep 0.3; echo run >> {c}; printf 'tok-%s' \"$(wc -l < {c} | tr -d ' ')\"",
                c = counter.display()
            ),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: None,
        },
    );
    let (a, b) = tokio::join!(
        provider.ensure_fresh_token(None),
        provider.ensure_fresh_token(None)
    );
    assert_eq!(a.rotated().as_deref(), Some("tok-1"));
    assert_eq!(
        b.rotated().as_deref(),
        Some("tok-1"),
        "second caller adopts, never re-runs"
    );
    let runs = std::fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(runs, 1, "the command must run exactly once");
}

/// Proven by staleness: an expiry inside the 60s skew re-mints, a
/// distant one serves from cache.
#[tokio::test]
async fn provider_expiry_source_precedence() {
    fn short_jwt() -> String {
        // exp within the skew window: stale immediately if consumed.
        jwt_with_exp(chrono::Utc::now().timestamp() + 30)
    }
    fn long_jwt() -> String {
        jwt_with_exp(chrono::Utc::now().timestamp() + 7200)
    }
    fn jwt_with_exp(exp: i64) -> String {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &serde_json::json!({ "exp": exp }),
            &jsonwebtoken::EncodingKey::from_secret(b"test"),
        )
        .unwrap()
    }
    async fn mints_after_first(
        name: &str,
        command: String,
        token_ttl_secs: Option<u64>,
        counter: &std::path::Path,
    ) -> usize {
        let provider = AuthProviderRef::new(
            name.to_owned(),
            AuthProviderConfig {
                command,
                args: None,
                token_ttl_secs,
                timeout_secs: None,
                cwd: None,
            },
        );
        let first = provider
            .ensure_fresh_token(None)
            .await
            .rotated()
            .expect("first mint");
        let _ = provider.ensure_fresh_token(Some(&first)).await;
        std::fs::read_to_string(counter).unwrap().lines().count()
    }

    let dir = tempfile::tempdir().unwrap();

    // expires_in=10 (stale) wins over token_ttl_secs=3600 (fresh): re-mints.
    let c1 = dir.path().join("c1");
    let cmd1 = format!(
        "echo run >> {}; printf '{{\"access_token\":\"t1\",\"expires_in\":10}}'",
        c1.display()
    );
    assert_eq!(
        mints_after_first("test-exp-expires-in", cmd1, Some(3600), &c1).await,
        2,
        "expires_in must win over token_ttl_secs"
    );

    // token_ttl_secs=1 (stale) wins over a 2h JWT exp (fresh): re-mints.
    let c2 = dir.path().join("c2");
    let cmd2 = format!("echo run >> {}; printf '{}'", c2.display(), long_jwt());
    assert_eq!(
        mints_after_first("test-exp-ttl", cmd2, Some(1), &c2).await,
        2,
        "token_ttl_secs must win over the JWT exp claim"
    );

    // JWT exp alone: a near-expiry claim (inside the skew) re-mints,
    // proving the claim is consumed when nothing else is configured.
    let c3 = dir.path().join("c3");
    let cmd3 = format!("echo run >> {}; printf '{}'", c3.display(), short_jwt());
    assert_eq!(
        mints_after_first("test-exp-jwt", cmd3, None, &c3).await,
        2,
        "the JWT exp claim must apply when expires_in and token_ttl_secs are absent"
    );
}

#[tokio::test]
async fn provider_unusable_expiry_still_mints() {
    let provider = AuthProviderRef::new(
        "test-overflow".to_owned(),
        AuthProviderConfig {
            command: format!(
                "printf '{{\"access_token\":\"t\",\"expires_in\":{}}}'",
                u64::MAX
            ),
            args: None,
            token_ttl_secs: Some(u64::MAX),
            timeout_secs: None,
            cwd: None,
        },
    );
    assert_eq!(
        provider.ensure_fresh_token(None).await.rotated().as_deref(),
        Some("t"),
        "an unusable expiry still mints; the token just has no expiry"
    );
    assert_eq!(
        provider.ensure_fresh_token(Some("t")).await,
        ProviderRefreshOutcome::Unchanged,
        "no expiry source: never proactively re-minted"
    );
}

#[tokio::test]
async fn provider_args_run_without_a_shell() {
    let provider = AuthProviderRef::new(
        "test-args".to_owned(),
        AuthProviderConfig {
            command: "printf".to_owned(),
            // Shell metacharacters stay literal under direct exec.
            args: Some(vec!["tok-$HOME;42".to_owned()]),
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: None,
        },
    );
    assert_eq!(
        provider.ensure_fresh_token(None).await.rotated().as_deref(),
        Some("tok-$HOME;42"),
    );
}

#[tokio::test]
async fn provider_command_times_out() {
    let provider = AuthProviderRef::new(
        "test-timeout".to_owned(),
        AuthProviderConfig {
            command: "sleep 20; printf never".to_owned(),
            args: None,
            token_ttl_secs: None,
            timeout_secs: Some(1),
            cwd: None,
        },
    );
    let start = std::time::Instant::now();
    assert_eq!(
        provider.ensure_fresh_token(None).await,
        ProviderRefreshOutcome::MintFailed
    );
    assert!(
        start.elapsed().as_secs() < 5,
        "1s timeout_secs must bound the mint (took {}s)",
        start.elapsed().as_secs()
    );
}

#[tokio::test]
async fn provider_zero_timeout_clamps_to_one_second() {
    // `timeout_secs = 0` clamps up to the 1s floor, so an instant helper mints
    // rather than failing immediately.
    let fast = AuthProviderRef::new(
        "test-zero-timeout-fast".to_owned(),
        AuthProviderConfig {
            command: "printf tok".to_owned(),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: Some(0),
            cwd: None,
        },
    );
    assert_eq!(
        fast.ensure_fresh_token(None).await.rotated().as_deref(),
        Some("tok")
    );

    // ...and clamps down from the 30s default: a helper that runs past 1s times
    // out, proving the effective bound is the clamp, not the default.
    let slow = AuthProviderRef::new(
        "test-zero-timeout-slow".to_owned(),
        AuthProviderConfig {
            command: "sleep 5; printf tok".to_owned(),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: Some(0),
            cwd: None,
        },
    );
    assert!(
        matches!(
            slow.ensure_fresh_token(None).await,
            ProviderRefreshOutcome::MintFailed
        ),
        "a >1s helper under timeout_secs=0 must time out at the 1s clamp"
    );
}

/// The distinct mint-failure modes (timeout, spawn failure, ran-but-no-token)
/// surface distinct, greppable error messages so operators can triage them.
#[tokio::test]
async fn mint_error_messages_distinguish_failure_modes() {
    let timed_out = AuthProviderRef::new(
        "test-classify-timeout".to_owned(),
        AuthProviderConfig {
            command: "sleep 20".to_owned(),
            args: None,
            token_ttl_secs: None,
            timeout_secs: Some(1),
            cwd: None,
        },
    );
    let err = mint_provider_token(&timed_out, false, None)
        .await
        .err()
        .expect("timeout must fail the mint");
    assert!(err.to_string().contains("timed out"), "got: {err}");

    let missing = AuthProviderRef::new(
        "test-classify-spawn".to_owned(),
        AuthProviderConfig {
            command: "/nonexistent/provider-binary".to_owned(),
            args: Some(vec![]),
            token_ttl_secs: None,
            timeout_secs: Some(5),
            cwd: None,
        },
    );
    let err = mint_provider_token(&missing, false, None)
        .await
        .err()
        .expect("spawn failure must fail the mint");
    assert!(err.to_string().contains("failed to start"), "got: {err}");

    let empty_output = AuthProviderRef::new(
        "test-classify-permanent".to_owned(),
        AuthProviderConfig {
            command: "printf ''".to_owned(),
            args: None,
            token_ttl_secs: None,
            timeout_secs: Some(5),
            cwd: None,
        },
    );
    let err = mint_provider_token(&empty_output, false, None)
        .await
        .err()
        .expect("empty output must fail the mint");
    assert!(err.to_string().contains("no output"), "got: {err}");
}

/// On an in-session re-mint, the prior credential is handed back to the command
/// via `ECHO_AGENT_AUTH_PROVIDER_*`, so a refresh-grant command can refresh instead of
/// re-authenticating. Nothing is written to disk.
#[tokio::test]
async fn re_mint_hands_the_prior_token_back_to_the_command() {
    let provider = AuthProviderRef::new(
        "test-handback".to_owned(),
        AuthProviderConfig {
            command: "printf 'seen-%s' \"${ECHO_AGENT_AUTH_PROVIDER_ACCESS_TOKEN:-none}\""
                .to_owned(),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: None,
        },
    );

    let first = provider.ensure_fresh_token(None).await.rotated().unwrap();
    assert_eq!(first, "seen-none", "the first mint has no prior credential");
    test_expire_provider_token("test-handback");
    assert_eq!(
        provider
            .ensure_fresh_token(Some(&first))
            .await
            .rotated()
            .as_deref(),
        Some("seen-seen-none"),
        "the re-mint must receive the prior access token via env"
    );
}

/// A 401 whose re-mint fails invalidates the rejected token, so it is not
/// re-served next turn (fail closed) even while still locally unexpired.
#[tokio::test]
async fn failed_401_remint_invalidates_the_cached_token() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("count");
    // Mints tok-1 on the first run, then exits non-zero on every later run.
    let provider = AuthProviderRef::new(
        "test-401-invalidate".to_owned(),
        AuthProviderConfig {
            command: format!(
                "echo run >> {c}; n=$(wc -l < {c} | tr -d ' '); \
                 [ \"$n\" = 1 ] && printf 'tok-1' || exit 1",
                c = counter.display()
            ),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: None,
        },
    );

    let token = provider.ensure_fresh_token(None).await.rotated().unwrap();
    assert_eq!(token, "tok-1");
    // Age past the fresh-mint guard so recovery attempts a re-mint.
    test_backdate_provider_mint("test-401-invalidate", PROVIDER_TOKEN_FRESH_MINT_GUARD * 2);

    assert_eq!(
        provider.recover_rejected_token(&token).await,
        None,
        "a failed re-mint surfaces the 401"
    );
    assert_eq!(
        provider.cached_token(),
        None,
        "a rejected token whose re-mint failed must not be re-served"
    );
}

/// A pre-turn re-mint that fails over a now-stale cached token leaves nothing
/// servable: the stale token is never handed to the wire (mirror of the 401
/// path, for the pre-turn path).
#[tokio::test]
async fn failed_pre_turn_mint_does_not_serve_the_stale_token() {
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("count");
    let provider = AuthProviderRef::new(
        "test-pre-turn-stale".to_owned(),
        AuthProviderConfig {
            command: format!(
                "echo run >> {c}; n=$(wc -l < {c} | tr -d ' '); \
                 [ \"$n\" = 1 ] && printf 'tok-1' || exit 1",
                c = counter.display()
            ),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: None,
        },
    );

    let token = provider.ensure_fresh_token(None).await.rotated().unwrap();
    assert_eq!(token, "tok-1");
    // Make the cached token stale so the next pre-turn call re-mints (and fails).
    test_expire_provider_token("test-pre-turn-stale");

    assert!(matches!(
        provider.ensure_fresh_token(Some(token.as_str())).await,
        ProviderRefreshOutcome::MintFailed
    ));
    assert_eq!(
        provider.cached_token(),
        None,
        "a stale token whose pre-turn re-mint failed must not be served"
    );
}

/// A helper that writes past the stdout cap fails closed (permanent), so a
/// runaway command can't exhaust memory or put a huge token on the wire.
#[tokio::test]
async fn provider_output_over_cap_fails_closed() {
    let over = PROVIDER_STDOUT_CAP_BYTES + 4096;
    let provider = AuthProviderRef::new(
        "test-stdout-cap".to_owned(),
        AuthProviderConfig {
            command: format!("head -c {over} /dev/zero"),
            args: None,
            token_ttl_secs: None,
            timeout_secs: Some(5),
            cwd: None,
        },
    );
    let err = mint_provider_token(&provider, false, None)
        .await
        .err()
        .expect("over-cap output must fail the mint");
    assert!(
        err.to_string().contains("more than"),
        "an over-cap write must be reported as such, got: {err}"
    );
    assert_eq!(
        provider.ensure_fresh_token(None).await,
        ProviderRefreshOutcome::MintFailed
    );
}

/// Every first-party credential env var is scrubbed from the helper, so a BYOK
/// helper never inherits the keys BYOK isolates on the wire.
///
/// The test drives its set/echo from an independent audited `EXPECTED` list, not
/// from the scrub const, so it is not tautological: dropping an entry from
/// `FIRST_PARTY_CREDENTIAL_ENV_VARS` alone leaves that var set on the command and
/// trips the assert below, and removing one from both requires deliberately
/// editing this audited list.
///
/// The leak values are set on the child command, not the process env, so the
/// test is hermetic: it needs no `#[serial]` and cannot race a sibling test that
/// reads a first-party credential (e.g. the `auth::manager` session tests).
#[tokio::test]
async fn provider_helper_env_scrubs_first_party_credentials() {
    // The credentials a BYOK helper must never inherit. Editing this list is the
    // audit checkpoint: it must equal the production scrub const.
    const EXPECTED: &[&str] = &[
        "ECHO_AGENT_API_KEY",
        "ECHO_AGENT_CODE_ECHO_AGENT_API_KEY",
        "ECHO_AGENT_AUTH",
        "ECHO_AGENT_AUTH_PATH",
        "ECHO_AGENT_DEPLOYMENT_KEY",
        "ECHO_AGENT_EXTRA_AUTH_KEY",
        "ECHO_AGENT_TRACE_UPLOAD_CREDENTIALS_FILE",
        "OTEL_EXPORTER_OTLP_HEADERS",
        "ECHO_AGENT_INTERNAL_OTLP_HEADERS",
    ];
    assert_eq!(
        crate::agent::config::FIRST_PARTY_CREDENTIAL_ENV_VARS,
        EXPECTED,
        "the scrub list changed: re-audit that every entry is a first-party \
         credential a BYOK helper must not inherit, then update EXPECTED"
    );

    // Echo each expected var back; the scrub must leave every one empty. A
    // scrub-const entry that EXPECTED still lists but production stopped removing
    // stays at its leak value and surfaces here.
    let echo = EXPECTED
        .iter()
        .map(|v| format!("${{{v}-}}"))
        .collect::<Vec<_>>()
        .join("");
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", &format!("printf 'tok[%s]' \"{echo}\"")]);
    for var in EXPECTED {
        cmd.env(var, "first-party-leak");
    }
    super::scrub_first_party_credentials(&mut cmd);

    let output = cmd.output().await.expect("helper spawns");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "tok[]",
        "no first-party credential may survive into the helper env"
    );
}

/// `resolve_program` branches: bare name via `PATH`, absolute as-is, relative
/// against `cwd`.
#[test]
fn resolve_program_resolves_against_cwd() {
    let cwd = std::path::Path::new("/work");
    assert_eq!(
        super::resolve_program("token-helper", Some(cwd)),
        std::path::PathBuf::from("token-helper")
    );
    let abs = if cfg!(windows) {
        r"C:\bin\helper.exe"
    } else {
        "/usr/local/bin/helper"
    };
    assert_eq!(
        super::resolve_program(abs, Some(cwd)),
        std::path::PathBuf::from(abs)
    );
    assert_eq!(
        super::resolve_program("bin/helper", Some(cwd)),
        cwd.join("bin/helper")
    );
    assert_eq!(
        super::resolve_program("bin/helper", None),
        std::path::PathBuf::from("bin/helper"),
        "with no cwd a relative path is left to the process cwd"
    );
}

/// The `args` form (the portable, no-shell shape a desktop/Windows helper
/// should use) resolves a relative program against the provider's `cwd`.
#[cfg(unix)]
#[tokio::test]
async fn provider_resolves_relative_program_against_cwd() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("token.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'cwd-tok'\n").unwrap();
    let mut perms = std::fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script, perms).unwrap();

    let provider = AuthProviderRef::new(
        "test-cwd-relative".to_owned(),
        AuthProviderConfig {
            command: "./token.sh".to_owned(),
            args: Some(vec![]),
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
        },
    );
    assert_eq!(
        provider.ensure_fresh_token(None).await.rotated().as_deref(),
        Some("cwd-tok")
    );
}

/// `cwd` is the command's runtime directory: reading a file by relative name
/// only succeeds if `current_dir` took effect (here via the shell form).
#[cfg(unix)]
#[tokio::test]
async fn provider_command_runs_in_cwd() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("token.txt"), "file-tok").unwrap();

    let provider = AuthProviderRef::new(
        "test-cwd-shell".to_owned(),
        AuthProviderConfig {
            command: "cat token.txt".to_owned(),
            args: None,
            token_ttl_secs: Some(3600),
            timeout_secs: None,
            cwd: Some(dir.path().to_string_lossy().into_owned()),
        },
    );
    assert_eq!(
        provider.ensure_fresh_token(None).await.rotated().as_deref(),
        Some("file-tok")
    );
}
