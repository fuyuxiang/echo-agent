//! Unit tests for [`super::manager::AuthManager`]. Extracted from
//! `manager.rs` so the implementation reads top-to-bottom; wired in
//! via `#[path = "manager_tests.rs"] mod tests;` in manager.rs.

use super::*;
use crate::auth::error::RefreshTokenError;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

fn make_auth(expires_at: Option<DateTime<Utc>>, create_time: DateTime<Utc>) -> EchoAgentAuth {
    EchoAgentAuth {
        auth_mode: AuthMode::External,
        create_time,
        user_id: String::new(),
        expires_at,
        ..EchoAgentAuth::test_default()
    }
}

#[test]
fn expired_within_5min_buffer() {
    let auth = make_auth(Some(Utc::now() + Duration::minutes(4)), Utc::now());
    assert!(is_expired(&auth));
}

#[test]
fn fallback_ttl_when_no_expires_at() {
    let old = Utc::now() - Duration::days(30) + Duration::minutes(4);
    let auth = make_auth(None, old);
    assert!(is_expired(&auth));

    let recent = Utc::now() - Duration::days(29);
    let auth = make_auth(None, recent);
    assert!(!is_expired(&auth));
}

#[tokio::test]
async fn refresh_path_lock_acquire_attaches_the_heartbeat() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let outcome = mgr
        .acquire_refresh_lock_or_adopt(RefreshReason::PreRequest)
        .await
        .expect("uncontended refresh-lock acquire");
    let super::refresh_chain::LockOutcome::Held(guard) = outcome else {
        panic!("an empty auth dir has no sibling token to adopt");
    };
    assert!(
        guard.heartbeat.is_some(),
        "the refresh-path hold must carry the heartbeat that placates old binaries"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn lock_loss_revalidation_adopts_the_sibling_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let guard = mgr
        .try_lock_auth_file_async(REFRESH_LOCK_TIMEOUT, lock::Heartbeat::Attach)
        .await
        .into_guard()
        .expect("initial acquire");
    let lock_path = dir.path().join("auth.json.lock");
    std::fs::remove_file(&lock_path).unwrap();
    std::fs::write(&lock_path, b"").unwrap();

    let fresh_disk = EchoAgentAuth {
        key: "fresh-key-from-sibling".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("new-rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, fresh_disk);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let outcome = mgr
        .revalidate_lock_or_reacquire(guard, RefreshReason::PreRequest)
        .await
        .expect("lock-loss revalidation must re-acquire on the live inode");
    let super::refresh_chain::LockOutcome::Adopted(adopted) = outcome else {
        panic!("a sibling token persisted during lock loss must be adopted");
    };
    assert_eq!(adopted.key, "fresh-key-from-sibling");
}

#[test]
fn has_usable_disk_token_reads_disk_independent_of_memory() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    assert!(!mgr.has_usable_disk_token());

    let valid = make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now());
    mgr.persist_and_swap(valid);
    mgr.clear_in_memory();
    assert!(mgr.current().is_none(), "in-memory cleared");
    assert!(
        mgr.has_usable_disk_token(),
        "a valid token on disk is usable even when in-memory is empty"
    );

    let expired = make_auth(Some(Utc::now() - Duration::hours(1)), Utc::now());
    mgr.persist_and_swap(expired);
    mgr.clear_in_memory();
    assert!(
        !mgr.has_usable_disk_token(),
        "an expired token on disk is not usable"
    );
}

#[test]
fn has_usable_token_covers_memory_and_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    assert!(!mgr.has_usable_token(), "nothing in memory or on disk");

    mgr.hot_swap(make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now()));
    assert!(!mgr.has_usable_disk_token(), "disk still empty");
    assert!(mgr.has_usable_token(), "valid in-memory token is usable");

    mgr.persist_and_swap(make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now()));
    mgr.hot_swap(make_auth(Some(Utc::now() - Duration::hours(1)), Utc::now()));
    assert!(mgr.current().is_none(), "in-memory token is expired");
    assert!(mgr.has_usable_token(), "fresh disk token keeps it usable");

    mgr.persist_and_swap(make_auth(Some(Utc::now() - Duration::hours(1)), Utc::now()));
    assert!(
        !mgr.has_usable_token(),
        "expired in memory and on disk is not usable"
    );
}

#[test]
fn auth_scope_uses_oauth2_when_present() {
    let cfg = EchoAgentComConfig::default();
    // Default config always has oauth2 set to the EchoAgent defaults.
    assert_eq!(
        cfg.auth_scope(),
        format!(
            "{}::{}",
            crate::auth::config::ECHO_AGENT_OAUTH2_ISSUER,
            obfstr::obfstr!("b1a00492-073a-47ea-816f-4c329264a828"),
        )
    );
}

#[test]
fn legacy_scope_fallback_reads_old_auth_json() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");

    // Write auth.json with the legacy scope key (as `x setup` copies from
    // a machine that was authenticated with an older echoagent version).
    let legacy_auth = make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now());
    let mut store = AuthStore::new();
    store.insert(LEGACY_SCOPE.to_string(), legacy_auth);
    write_auth_json(&auth_path, &store).unwrap();

    // AuthManager uses the new OAuth2 scope, but should still find the
    // token under the legacy key.
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));
    let current = mgr.current();
    assert!(current.is_some(), "should fall back to legacy scope key");
    assert_eq!(current.unwrap().key, "test-key");
}

#[test]
fn new_scope_takes_precedence_over_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");

    let legacy_auth = EchoAgentAuth {
        key: "legacy-key".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    let new_auth = EchoAgentAuth {
        key: "new-key".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };

    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();

    let mut store = AuthStore::new();
    store.insert(LEGACY_SCOPE.to_string(), legacy_auth);
    store.insert(scope, new_auth);
    write_auth_json(&auth_path, &store).unwrap();

    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));
    let current = mgr.current().expect("should find auth");
    assert_eq!(current.key, "new-key", "new scope should take precedence");
}

// -- Near-expiry (5-minute buffer) behavior ------------------------

/// Regression test: a token within the 5-minute early-invalidation buffer
/// must be invisible to `current()` (returns None) but visible to
/// `expired_auth()` so that callers can attempt a silent refresh.
#[test]
fn near_expiry_token_invisible_to_current_visible_to_expired_auth() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    // Token expires in 3 minutes -- inside the 5-minute buffer.
    let near_expiry = EchoAgentAuth {
        key: "near-expiry-key".into(),
        user_id: "user-1".into(),
        email: Some("user@test.com".into()),
        refresh_token: Some("rt-valid".into()),
        expires_at: Some(Utc::now() + Duration::minutes(3)),
        oidc_issuer: Some("https://idp.example.com".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(near_expiry);

    // current() must return None (token is "expired" per buffer)
    assert!(
        mgr.current().is_none(),
        "current() should return None for token within 5-min buffer"
    );

    // is_expired() must return true
    assert!(
        mgr.is_expired(),
        "is_expired() should be true for token within 5-min buffer"
    );

    // expired_auth() must return the token so refresh can use it
    let expired = mgr.expired_auth();
    assert!(
        expired.is_some(),
        "expired_auth() should return the near-expiry token"
    );
    assert_eq!(expired.as_ref().unwrap().key, "near-expiry-key");
    assert_eq!(
        expired.as_ref().unwrap().refresh_token.as_deref(),
        Some("rt-valid"),
        "refresh_token must be preserved for silent refresh"
    );
}

#[tokio::test]
async fn update_preserves_other_scope_entries() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));

    // Pre-populate with an external auth entry
    let external = EchoAgentAuth {
        key: "external-key".into(),
        auth_mode: AuthMode::External,
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    {
        let mut map = AuthStore::new();
        map.insert("other-scope".into(), external);
        write_auth_json(&dir.path().join("auth.json"), &map).unwrap();
    }

    // Now update via auth_manager
    let new_auth = EchoAgentAuth {
        key: "oidc-token".into(),
        auth_mode: AuthMode::Oidc,
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.update(new_auth).await.unwrap();

    // Both entries should exist
    let store = read_auth_json(&dir.path().join("auth.json")).unwrap();
    assert!(store.contains_key("other-scope"));
    assert!(store.contains_key(&cfg.auth_scope()));
}

/// Regression: when auth.json contains corrupt JSON, update() must not
/// clobber the file with a single-entry map. Instead it should update
/// in-memory only and leave the file untouched.
#[tokio::test]
async fn update_recovers_from_corrupt_auth_json_by_backing_up_old_file() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));

    let bad_content = b"NOT VALID JSON {{{";
    std::fs::write(&auth_path, bad_content).unwrap();

    let new_auth = EchoAgentAuth {
        key: "fresh-token".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("fresh-rt".into()),
        user_id: "fresh-user".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };

    let result = mgr.update(new_auth).await;
    assert!(
        result.is_ok(),
        "update must succeed and persist after corrupt recovery: {result:?}"
    );

    let current = mgr.current();
    assert_eq!(
        current.as_ref().map(|a| a.key.as_str()),
        Some("fresh-token")
    );

    let on_disk_raw = std::fs::read_to_string(&auth_path).unwrap();
    assert!(
        on_disk_raw.contains("fresh-token"),
        "auth.json must contain the new credential after recovery, got: {on_disk_raw}"
    );
    let on_disk: AuthStore =
        serde_json::from_str(&on_disk_raw).expect("auth.json must be valid JSON after recovery");
    assert!(on_disk.contains_key(&cfg.auth_scope()));

    let mut backup_found = None;
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("auth.json.corrupt.") {
            backup_found = Some(entry.path());
            break;
        }
    }
    let backup_path = backup_found.expect("a .corrupt.* backup file must have been created");
    let backup_content = std::fs::read_to_string(&backup_path).unwrap();
    assert!(
        backup_content.contains("NOT VALID JSON"),
        "backup must contain the original corrupt content, got: {backup_content}"
    );
}

/// Regression test: update() must preserve team fields from the OIDC flow
/// when the proxy `/user` response does not include them.
#[tokio::test]
async fn update_preserves_team_fields_when_proxy_omits_them() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    // Point proxy_base_url to a non-existent server so the /user call
    // fails and falls back to the auth-flow values.
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url("http://127.0.0.1:1"));

    let team_auth = EchoAgentAuth {
        key: "team-token".into(),
        auth_mode: AuthMode::Oidc,
        principal_type: Some("Team".into()),
        principal_id: Some("team-xyz".into()),
        team_id: Some("team-xyz".into()),
        team_name: None,
        team_role: None,
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };

    let saved = mgr.update(team_auth).await.unwrap();

    assert_eq!(
        saved.principal_type.as_deref(),
        Some("Team"),
        "principal_type must survive proxy fallback"
    );
    assert_eq!(
        saved.principal_id.as_deref(),
        Some("team-xyz"),
        "principal_id must survive proxy fallback"
    );
    assert_eq!(
        saved.team_id.as_deref(),
        Some("team-xyz"),
        "team_id must survive proxy fallback"
    );

    // Verify on-disk too
    let store = read_auth_json(&dir.path().join("auth.json")).unwrap();
    let on_disk = store.values().next().unwrap();
    assert_eq!(on_disk.principal_type.as_deref(), Some("Team"));
    assert_eq!(on_disk.team_id.as_deref(), Some("team-xyz"));
}

/// Team tokens are stored under the base scope key (same as personal).
/// There is at most one OAuth entry per issuer/client pair.
#[tokio::test]
async fn update_stores_team_token_under_base_scope() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let base_scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url("http://127.0.0.1:1"));

    let team_auth = EchoAgentAuth {
        key: "team-token".into(),
        auth_mode: AuthMode::Oidc,
        principal_type: Some("Team".into()),
        principal_id: Some("team-abc".into()),
        team_id: Some("team-abc".into()),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };

    mgr.update(team_auth).await.unwrap();

    let store = read_auth_json(&dir.path().join("auth.json")).unwrap();
    assert!(
        store.contains_key(&base_scope),
        "team token must be stored under base scope '{}', found keys: {:?}",
        base_scope,
        store.keys().collect::<Vec<_>>()
    );
    assert_eq!(store.get(&base_scope).unwrap().key, "team-token");
}

/// Logging in as personal must evict any existing team token
/// (at most one OAuth session per issuer/client pair).
#[tokio::test]
async fn team_login_then_personal_evicts_team_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let base_scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url("http://127.0.0.1:1"));

    // Step 1: login as team
    let team_auth = EchoAgentAuth {
        key: "team-token".into(),
        principal_type: Some("Team".into()),
        principal_id: Some("team-abc".into()),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.update(team_auth).await.unwrap();

    // Step 2: login as personal
    let personal_auth = EchoAgentAuth {
        key: "personal-token".into(),
        principal_type: None,
        principal_id: None,
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.update(personal_auth).await.unwrap();

    let store = read_auth_json(&dir.path().join("auth.json")).unwrap();
    assert_eq!(
        store.len(),
        1,
        "only one OAuth entry should remain, found: {:?}",
        store.keys().collect::<Vec<_>>()
    );
    assert!(store.contains_key(&base_scope));
    assert_eq!(store.get(&base_scope).unwrap().key, "personal-token");
}

/// Regression test: clear() must only remove the current scope, not the
/// legacy scope. Previously, logging in with OAuth would also delete the
/// legacy `https://accounts.echo-agent.invalid/sign-in` entry from auth.json.
#[test]
fn clear_does_not_remove_legacy_scope() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");

    let legacy_auth = EchoAgentAuth {
        key: "legacy-key".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    let oauth_auth = EchoAgentAuth {
        key: "oauth-key".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };

    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();

    let mut store = AuthStore::new();
    store.insert(LEGACY_SCOPE.to_string(), legacy_auth);
    store.insert(scope, oauth_auth);
    write_auth_json(&auth_path, &store).unwrap();

    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));
    // clear() should only remove the OAuth scope, not legacy
    mgr.clear().unwrap();

    let on_disk = read_auth_json(&auth_path).unwrap();
    assert!(
        on_disk.contains_key(LEGACY_SCOPE),
        "legacy scope should be preserved after clear()"
    );
    assert!(
        !on_disk.contains_key(&mgr.scope),
        "current scope should be removed after clear()"
    );
}

#[test]
fn is_data_collection_disabled_matrix() {
    // (team_blocked_reasons, coding_data_retention_opt_out, expected)
    let cases: &[(&[&str], bool, bool)] = &[
        // ZDR team alone
        (&["BLOCKED_REASON_NO_LOGS"], false, true),
        (&["BLOCKED_REASON_NO_LOGS_MODERATED"], false, true),
        // Opt-out alone
        (&[], true, true),
        // Both
        (&["BLOCKED_REASON_NO_LOGS"], true, true),
        // Neither
        (&[], false, false),
        // Unrelated blocked reasons
        (
            &["BLOCKED_REASON_BILLING", "BLOCKED_REASON_SUSPENDED"],
            false,
            false,
        ),
        (&["BLOCKED_REASON_BILLING"], true, true),
        // ZDR mixed with other reasons
        (
            &["BLOCKED_REASON_BILLING", "BLOCKED_REASON_NO_LOGS"],
            false,
            true,
        ),
    ];
    for (reasons, opt_out, expected) in cases {
        let auth = EchoAgentAuth {
            team_blocked_reasons: reasons.iter().map(|s| (*s).into()).collect(),
            coding_data_retention_opt_out: *opt_out,
            ..EchoAgentAuth::test_default()
        };
        assert_eq!(
            auth.is_data_collection_disabled(),
            *expected,
            "reasons={reasons:?} opt_out={opt_out} expected={expected}",
        );
    }
}

/// Fail-direction contract of the two `AuthManager` collection predicates:
/// `is_data_collection_disabled` fails open on missing credentials (legacy
/// semantics shared by telemetry/sync gates), `allows_data_collection` fails
/// closed (nothing may leave the machine while privacy state is unknown,
/// e.g. after a mid-session `/logout`).
#[test]
fn manager_collection_predicates_fail_directions() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // No credential: disabled=false (fail-open), allows=false (fail-closed).
    assert!(!mgr.is_data_collection_disabled());
    assert!(
        !mgr.allows_data_collection(),
        "missing credential must fail closed for collection"
    );

    // Normal user: both predicates allow collection.
    mgr.hot_swap(EchoAgentAuth::test_default());
    assert!(!mgr.is_data_collection_disabled());
    assert!(mgr.allows_data_collection());

    // Opted-out user: both predicates suppress collection.
    mgr.hot_swap(EchoAgentAuth {
        coding_data_retention_opt_out: true,
        ..EchoAgentAuth::test_default()
    });
    assert!(mgr.is_data_collection_disabled());
    assert!(!mgr.allows_data_collection());

    // Mid-session `/logout`: the fail-closed predicate flips back to
    // "no collection" even after a previously permissive credential.
    mgr.hot_swap(EchoAgentAuth::test_default());
    assert!(mgr.allows_data_collection(), "precondition");
    mgr.clear_in_memory();
    assert!(
        !mgr.allows_data_collection(),
        "cleared credentials must close the collection gate"
    );
}

// -- bearer_suffix ----------------------------------------------------------------

#[test]
fn token_suffix_matrix() {
    let cases: &[(&str, &str)] = &[
        ("abcdefghijklmnop", "efghijklmnop"), // takes last 12
        ("short", "short"),                   // short unchanged
        ("", ""),                             // empty
        ("123456789012", "123456789012"),     // exact 12
    ];
    for (input, expected) in cases {
        assert_eq!(bearer_suffix(input), *expected, "input={input:?}");
    }
}

// -- read_disk_auth ----------------------------------------------------------

// -- hot_swap / try_use_disk_token ---------------------------------------

#[test]
fn hot_swap_updates_in_memory_without_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    assert!(mgr.current().is_none());
    let auth = EchoAgentAuth {
        key: "swapped".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.hot_swap(auth);
    assert_eq!(mgr.current().unwrap().key, "swapped");
    // Disk should NOT have the token
    assert!(mgr.read_disk_auth().is_none());
}

#[test]
fn try_use_disk_token_accepts_valid_disk_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let valid_disk = EchoAgentAuth {
        key: "valid-disk".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    let result = mgr.try_use_disk_token(Some(&valid_disk), RefreshReason::PreRequest);
    assert_eq!(result.unwrap().key, "valid-disk");
    // Should also hot-swap into memory
    assert_eq!(mgr.current().unwrap().key, "valid-disk");
}

#[test]
fn try_use_disk_token_rejects_expired_disk_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let expired_disk = make_auth(Some(Utc::now() - Duration::hours(1)), Utc::now());
    assert_eq!(
        mgr.try_use_disk_token(Some(&expired_disk), RefreshReason::PreRequest)
            .err(),
        Some(DiskTokenDecline::Expired)
    );
}

#[test]
fn try_use_disk_token_rejects_same_key_on_server_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let auth = EchoAgentAuth {
        key: "same-key".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.hot_swap(auth.clone());

    // ServerRejected should not accept a disk token with the same key
    assert_eq!(
        mgr.try_use_disk_token(Some(&auth), RefreshReason::ServerRejected)
            .err(),
        Some(DiskTokenDecline::SameKeyAsRejected)
    );
}

#[test]
fn try_use_disk_token_accepts_different_key_on_server_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let mem_auth = EchoAgentAuth {
        key: "old-key".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.hot_swap(mem_auth);

    let disk_auth = EchoAgentAuth {
        key: "new-key".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    let result = mgr.try_use_disk_token(Some(&disk_auth), RefreshReason::ServerRejected);
    assert_eq!(result.unwrap().key, "new-key");
}

/// Disk lagging memory (`update()` kept a mint after a failed disk write) is
/// not a sibling rotation: a valid disk token minted BEFORE the live
/// in-memory one must not clobber it — on ServerRejected that would restore
/// the very bearer the caller is rejecting.
#[test]
fn try_use_disk_token_skips_disk_token_older_than_memory_mint() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let fresh_mint = EchoAgentAuth {
        key: "fresh-mint".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.hot_swap(fresh_mint);

    let lagging_disk = EchoAgentAuth {
        key: "stale-disk".into(),
        ..make_auth(
            Some(Utc::now() + Duration::minutes(30)),
            Utc::now() - Duration::hours(1),
        )
    };
    for reason in [RefreshReason::PreRequest, RefreshReason::ServerRejected] {
        assert_eq!(
            mgr.try_use_disk_token(Some(&lagging_disk), reason).err(),
            Some(DiskTokenDecline::LaggingMemoryMint),
            "an older disk token must not clobber the in-memory mint ({reason:?})"
        );
        assert_eq!(mgr.current().unwrap().key, "fresh-mint");
    }
}

/// The lagging-mint guard must hold when the in-memory bearer sits inside
/// the early-invalidation buffer — the exact state that routes a refresh
/// into the adopt paths. `current()` hides a buffered bearer, so a
/// `current()`-gated guard was skipped in precisely that window and a
/// lagging disk token could clobber the newest local mint.
#[test]
fn try_use_disk_token_lagging_guard_holds_for_buffered_in_memory_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    // Inside the 5-minute buffer: hidden by `current()`, visible to
    // `current_or_expired()`, still the newest local mint.
    let buffered_mint = EchoAgentAuth {
        key: "buffered-mint".into(),
        ..make_auth(Some(Utc::now() + Duration::minutes(2)), Utc::now())
    };
    mgr.hot_swap(buffered_mint);
    assert!(mgr.current().is_none(), "bearer is inside the buffer");

    let lagging_disk = EchoAgentAuth {
        key: "stale-disk".into(),
        ..make_auth(
            Some(Utc::now() + Duration::minutes(30)),
            Utc::now() - Duration::hours(1),
        )
    };
    for reason in [RefreshReason::PreRequest, RefreshReason::ServerRejected] {
        assert_eq!(
            mgr.try_use_disk_token(Some(&lagging_disk), reason).err(),
            Some(DiskTokenDecline::LaggingMemoryMint),
            "a buffered bearer is still the newest mint ({reason:?})"
        );
        assert_eq!(mgr.current_or_expired().unwrap().key, "buffered-mint");
    }
}

/// `pick_up_sibling_token` routes through the shared enforcement point, so
/// it refuses a lagging disk token instead of replacing a newer in-memory
/// mint with it (previously it checked expiry + key only and wrote state
/// directly).
#[test]
fn pick_up_sibling_token_refuses_lagging_disk_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let fresh_mint = EchoAgentAuth {
        key: "fresh-mint".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.hot_swap(fresh_mint);

    // Valid, different key, but minted an hour before the in-memory token:
    // disk lagging memory, not a sibling rotation.
    let lagging_disk = EchoAgentAuth {
        key: "stale-disk".into(),
        ..make_auth(
            Some(Utc::now() + Duration::minutes(30)),
            Utc::now() - Duration::hours(1),
        )
    };
    let mut store = AuthStore::new();
    store.insert(scope, lagging_disk);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    assert!(
        !mgr.pick_up_sibling_token(),
        "a lagging disk token is not an adoption"
    );
    assert_eq!(mgr.current().unwrap().key, "fresh-mint");
}

// -- File locking ----------------------------------------------------------

// -- Disk-refresh race simulation ------------------------------------------

/// Simulates the core scenario this PR fixes: an expired in-memory token
/// where another process has already refreshed on disk. The manager should
/// pick up the valid disk token via try_use_disk_token instead of
/// attempting its own refresh.
#[tokio::test]
async fn disk_refresh_wins_over_expired_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    // Simulate: in-memory token is expired
    let expired = EchoAgentAuth {
        key: "expired-key".into(),
        refresh_token: Some("old-rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);
    assert!(mgr.is_expired());
    assert!(mgr.current().is_none());

    // Simulate: another process wrote a valid token to disk
    let fresh_disk = EchoAgentAuth {
        key: "fresh-key-from-sibling".into(),
        refresh_token: Some("new-rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, fresh_disk);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    // Acquire lock + read disk (mirrors flow.rs logic)
    let _lock = mgr
        .try_lock_auth_file_async(StdDuration::from_secs(1), lock::Heartbeat::Skip)
        .await
        .into_guard();
    assert!(_lock.is_some());

    let disk_auth = mgr.read_disk_auth();
    assert!(disk_auth.is_some());
    assert!(!is_expired(disk_auth.as_ref().unwrap()));

    // try_use_disk_token should accept it and hot-swap
    let result = mgr.try_use_disk_token(disk_auth.as_ref(), RefreshReason::PreRequest);
    assert_eq!(result.unwrap().key, "fresh-key-from-sibling");
    assert_eq!(mgr.current().unwrap().key, "fresh-key-from-sibling");
}

struct CountingRefresher {
    call_count: Arc<AtomicU32>,
    delay: StdDuration,
}

#[async_trait::async_trait]
impl TokenRefresher for CountingRefresher {
    async fn refresh(&self, _reason: RefreshReason) -> crate::auth::refresh::RefreshOutcome {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        let fresh = EchoAgentAuth {
            key: "fresh-token".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            refresh_token: Some("rt-new".into()),
            ..EchoAgentAuth::test_default()
        };
        crate::auth::refresh::RefreshOutcome::Success(Box::new(fresh))
    }
}

struct FailingRefresher {
    call_count: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl TokenRefresher for FailingRefresher {
    async fn refresh(&self, _reason: RefreshReason) -> crate::auth::refresh::RefreshOutcome {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        crate::auth::refresh::RefreshOutcome::permanent(
            crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
            None,
        )
    }
}

/// Record a permanent failure scoped to the auth manager's current (or expired)
/// credential key, mirroring what `refresh_chain` does in production.
fn record_permanent_failure(
    auth_manager: &AuthManager,
    reason: crate::auth::error::RefreshTokenFailedReason,
) {
    let key = auth_manager
        .current()
        .or_else(|| auth_manager.expired_auth())
        .map(|a| a.key)
        .unwrap_or_default();
    auth_manager.record_permanent_failure(key, reason.into());
}

/// A convoy member whose sibling already rotated the token must adopt it
/// BEFORE contending the flock: with the flock held elsewhere for the whole
/// call, `refresh_chain` still returns the sibling token promptly, with no
/// IdP call.
#[tokio::test]
async fn refresh_chain_adopts_sibling_pre_lock_without_flock() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    mgr.hot_swap(EchoAgentAuth {
        key: "expired-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("old-rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    let fresh_disk = EchoAgentAuth {
        key: "fresh-key-from-sibling".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("new-rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, fresh_disk);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let calls = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: calls.clone(),
        delay: StdDuration::ZERO,
    }));

    let _held = mgr
        .try_lock_auth_file_async(REFRESH_LOCK_TIMEOUT, lock::Heartbeat::Attach)
        .await
        .into_guard()
        .expect("uncontended first acquisition");

    let adopted = tokio::time::timeout(
        StdDuration::from_secs(2),
        mgr.refresh_chain(TokenType::OidcSession, RefreshReason::PreRequest),
    )
    .await
    .expect("pre-lock adoption must not wait on the held flock")
    .expect("adoption returns the sibling token");
    assert_eq!(adopted.key, "fresh-key-from-sibling");
    assert_eq!(mgr.current().unwrap().key, "fresh-key-from-sibling");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "a pure adoption must not reach the IdP"
    );
}

/// ServerRejected with the disk token identical to the rejected one must NOT
/// adopt pre-lock: the caller needs a genuinely new credential, so it falls
/// through to a locked mint.
#[tokio::test]
async fn refresh_chain_server_rejected_same_key_skips_pre_lock_adopt() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let rejected = EchoAgentAuth {
        key: "rejected-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-live".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(rejected.clone());
    let mut store = AuthStore::new();
    store.insert(scope, rejected);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let calls = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: calls.clone(),
        delay: StdDuration::ZERO,
    }));

    let minted = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::ServerRejected)
        .await
        .expect("locked mint");
    assert_eq!(minted.key, "fresh-token");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a same-key disk token must mint under the flock"
    );
}

/// A buffered-expired sibling token on disk is not adoptable: the pre-lock
/// check declines and the chain mints under the flock, so adoption can never
/// hand back a token the next request would immediately re-refresh.
#[tokio::test]
async fn refresh_chain_pre_lock_adopt_ignores_expired_disk_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    mgr.hot_swap(EchoAgentAuth {
        key: "expired-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("old-rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    // Inside the 5-minute early-invalidation buffer: wire-alive but not
    // adoptable per `try_use_disk_token`'s buffer-inclusive expiry check.
    let buffered_disk = EchoAgentAuth {
        key: "buffered-sibling-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-buffered".into()),
        expires_at: Some(Utc::now() + Duration::minutes(3)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, buffered_disk);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let calls = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: calls.clone(),
        delay: StdDuration::ZERO,
    }));

    let minted = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::PreRequest)
        .await
        .expect("locked mint");
    assert_eq!(minted.key, "fresh-token");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a buffered-expired disk token must not be adopted"
    );
}

/// Disk lagging memory must not be "adopted" pre-lock: with a live mint in
/// memory and an older still-valid token on disk (`update()` disk write
/// failed), `refresh_chain` returns the in-memory mint untouched instead of
/// hot-swapping the older bearer back in.
#[tokio::test]
async fn refresh_chain_pre_lock_adopt_skips_disk_token_older_than_memory() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let lagging_disk = EchoAgentAuth {
        key: "stale-disk-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() + Duration::minutes(30)),
        create_time: Utc::now() - Duration::hours(1),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, lagging_disk);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    mgr.hot_swap(EchoAgentAuth {
        key: "fresh-mint-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-new".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });

    let calls = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: calls.clone(),
        delay: StdDuration::ZERO,
    }));

    let auth = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::PreRequest)
        .await
        .expect("in-memory mint is returned");
    assert_eq!(auth.key, "fresh-mint-key");
    assert_eq!(mgr.current().unwrap().key, "fresh-mint-key");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "neither adoption nor a mint may replace the fresher in-memory token"
    );
}

/// Same lagging-disk guard on its only reachable step-1c path: with a valid
/// in-memory token, `PreRequest` short-circuits at step 1, so only
/// `ServerRejected` carries a live mint into the pre-lock adopt. A
/// different-key disk token minted well before the rejected one must not be
/// adopted — the chain mints under the flock instead.
#[tokio::test]
async fn refresh_chain_server_rejected_skips_lagging_disk_token_pre_lock() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    // Past the 60s skew tolerance, so this is unambiguously disk-lagging.
    let lagging_disk = EchoAgentAuth {
        key: "stale-disk-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() + Duration::minutes(30)),
        create_time: Utc::now() - Duration::minutes(10),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, lagging_disk);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    mgr.hot_swap(EchoAgentAuth {
        key: "rejected-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-live".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });

    let calls = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: calls.clone(),
        delay: StdDuration::ZERO,
    }));

    let minted = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::ServerRejected)
        .await
        .expect("locked mint");
    assert_eq!(minted.key, "fresh-token");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a lagging disk token must not be adopted in place of the rejected mint"
    );
}

/// The bounded wrapper returns a transient error at the deadline WITHOUT
/// dropping the mint: the spawned chain finishes afterwards, hot-swaps the
/// minted token, and persists it to disk, so the rotated refresh token is
/// never abandoned and siblings can adopt it.
#[tokio::test]
async fn refresh_chain_bounded_times_out_without_dropping_mint() {
    /// Signals just before returning `Success`, so the test can await the
    /// mint's completion instead of polling on a scheduler-dependent clock.
    struct SlowSignallingRefresher {
        call_count: Arc<AtomicU32>,
        returning: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl TokenRefresher for SlowSignallingRefresher {
        async fn refresh(&self, _reason: RefreshReason) -> crate::auth::refresh::RefreshOutcome {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            // Far past the caller's 250ms budget, so the early return below
            // cannot race a fast mint on a loaded shard.
            tokio::time::sleep(StdDuration::from_secs(5)).await;
            let fresh = EchoAgentAuth {
                key: "fresh-token".into(),
                expires_at: Some(Utc::now() + Duration::hours(1)),
                refresh_token: Some("rt-new".into()),
                ..EchoAgentAuth::test_default()
            };
            self.returning.notify_one();
            crate::auth::refresh::RefreshOutcome::Success(Box::new(fresh))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    mgr.hot_swap(EchoAgentAuth {
        key: "expired-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("old-rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });

    let calls = Arc::new(AtomicU32::new(0));
    let returning = Arc::new(tokio::sync::Notify::new());
    mgr.set_refresher(Arc::new(SlowSignallingRefresher {
        call_count: calls.clone(),
        returning: returning.clone(),
    }));

    let started = Instant::now();
    let result = mgr
        .refresh_chain_bounded(
            TokenType::OidcSession,
            RefreshReason::PreRequest,
            StdDuration::from_millis(250),
        )
        .await;
    let err = result.expect_err("the deadline elapses before the slow mint");
    assert!(err.is_transient(), "the deadline maps to a retryable error");
    assert!(
        err.to_string().contains("bounded refresh deadline elapsed"),
        "timeout arm, not a refresh failure: {err}"
    );
    assert!(
        started.elapsed() < StdDuration::from_secs(5),
        "the caller returns at ~budget, not the refresher's sleep"
    );

    // The detached chain keeps driving the refresher to completion...
    tokio::time::timeout(StdDuration::from_secs(30), returning.notified())
        .await
        .expect("the spawned refresh_chain must run the refresher to completion");
    // ...then persists + hot-swaps on the same task; only that short tail
    // needs a bounded wait.
    let deadline = Instant::now() + StdDuration::from_secs(5);
    while mgr.current().map(|a| a.key).as_deref() != Some("fresh-token") {
        assert!(
            Instant::now() < deadline,
            "mint must be hot-swapped after the refresher returns"
        );
        tokio::time::sleep(StdDuration::from_millis(25)).await;
    }
    assert_eq!(
        mgr.read_disk_auth().map(|a| a.key).as_deref(),
        Some("fresh-token"),
        "mint must be persisted for sibling adoption, not only hot-swapped"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "exactly one IdP call: bounded return must not re-mint"
    );
}

/// With `inner == None` but a dead refresh-token on disk, the refresher still
/// exchanges that disk RT. The verdict must be keyed on the
/// credential actually tried (the disk RT), so repeated reactive refreshes
/// short-circuit on it instead of hammering the IdP.
#[tokio::test]
async fn storm_cap_engages_with_empty_inner_and_dead_disk_refresh_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    // Disk: an expired token carrying the (dead) refresh_token the OIDC
    // refresher resolves. `inner` stays empty.
    let dead = EchoAgentAuth {
        key: "disk-dead".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-dead".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = read_auth_json(&dir.path().join("auth.json")).unwrap_or_default();
    store.insert(scope, dead);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();
    assert!(mgr.current_or_expired().is_none(), "inner must be empty");

    let calls = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(FailingRefresher {
        call_count: calls.clone(),
    }));

    for _ in 0..5 {
        let _ = mgr
            .refresh_chain(TokenType::OidcSession, RefreshReason::ServerRejected)
            .await;
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "storm cap must hold the IdP to one call even with empty inner + dead disk RT",
    );
}

/// Record/check consistency: in-mem and disk are DIFFERENT stale credentials.
/// The refresher reports `tried_key = disk`; with a retain-path permanent
/// (`ClientRejected`) credentials stay, so the verdict stays scoped to disk.
/// Swapping the in-mem bearer must not re-open the IdP (a verdict mis-keyed to
/// the in-mem bearer would read absent after the swap). The `tried_key == None`
/// fallback (external-binary flow → `attempted_verdict_key`) is covered by
/// `storm_cap_engages_with_empty_inner_and_dead_disk_refresh_token`.
#[tokio::test]
async fn verdict_not_keyed_on_in_mem_bearer() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    // in-mem: stale bearer K_mem (expired, with RT).
    mgr.hot_swap(EchoAgentAuth {
        key: "mem-stale".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-mem".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    // disk: a DIFFERENT stale credential K_disk (expired, with RT) — what the
    // refresher claims to have tried.
    let disk = EchoAgentAuth {
        key: "disk-stale".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-disk".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = read_auth_json(&dir.path().join("auth.json")).unwrap_or_default();
    store.insert(scope, disk);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let calls = Arc::new(AtomicU32::new(0));
    struct TriedKeyClientRejected {
        tried_key: String,
        call_count: Arc<AtomicU32>,
    }
    #[async_trait::async_trait]
    impl TokenRefresher for TriedKeyClientRejected {
        async fn refresh(&self, _reason: RefreshReason) -> crate::auth::refresh::RefreshOutcome {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            // ClientRejected retains credentials (unlike RefreshTokenRejected),
            // so the disk-scoped verdict remains the storm cap after mem swap.
            crate::auth::refresh::RefreshOutcome::permanent(
                crate::auth::error::RefreshTokenFailedReason::ClientRejected,
                Some(self.tried_key.clone()),
            )
        }
    }
    mgr.set_refresher(Arc::new(TriedKeyClientRejected {
        tried_key: "disk-stale".into(),
        call_count: calls.clone(),
    }));

    let _ = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::ServerRejected)
        .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "first call hits the IdP once"
    );
    assert!(
        mgr.read_disk_auth().is_some(),
        "ClientRejected must retain the disk credential the verdict is keyed on",
    );

    // Swap the in-mem bearer to yet another stale key: a verdict mis-keyed to
    // the old in-mem bearer would now read absent.
    mgr.hot_swap(EchoAgentAuth {
        key: "mem-stale-2".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-mem-2".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });

    let _ = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::ServerRejected)
        .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "verdict keyed on the tried disk credential must survive an in-mem swap",
    );
}

/// Success → persist-failure → transient: a refresh that obtains a fresh token
/// but cannot write it to disk must surface `Transient` AND still swap the
/// in-memory bearer to the fresh token (the "always update in-memory even if the
/// disk write failed" invariant — without it a disk hiccup strands the session).
/// The write is failed deterministically (root-safe) via the path-scoped
/// `WRITE_FAULT_PATH` injection in `storage.rs`; the auth.json read (file
/// absent) and the file lock still succeed.
#[tokio::test]
async fn refresh_persist_failure_is_transient_but_swaps_in_memory() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Expired in-mem bearer so the chain proceeds to the IdP (no early return).
    mgr.hot_swap(EchoAgentAuth {
        key: "stale".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });

    // Fail every atomic write to THIS tempdir's auth.json (path-scoped, so
    // parallel tests are unaffected). Cleared on drop.
    struct FaultGuard;
    impl Drop for FaultGuard {
        fn drop(&mut self) {
            *crate::auth::storage::WRITE_FAULT_PATH
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
    let _fault = FaultGuard;
    *crate::auth::storage::WRITE_FAULT_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(dir.path().join("auth.json"));

    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
        delay: StdDuration::ZERO,
    }));

    let err = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::ServerRejected)
        .await
        .expect_err("persist failure must surface an error");
    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Transient(_))),
        "persist failure must be transient (retryable), got {err:?}",
    );
    assert_eq!(
        mgr.current().map(|a| a.key),
        Some("fresh-token".to_string()),
        "in-memory bearer must hold the fresh token despite the failed disk write",
    );
}

#[tokio::test]
async fn auth_concurrent_refresh_deduplicates() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    let expired = EchoAgentAuth {
        key: "expired-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);

    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(50),
    }));

    // Spawn 4 concurrent tasks that all call auth().
    let mut handles = Vec::new();
    for _ in 0..4 {
        let m = mgr.clone();
        handles.push(tokio::spawn(async move { m.auth().await }));
    }

    let mut results = Vec::new();
    for h in handles {
        results.push(h.await.unwrap());
    }

    // All 4 should succeed with the same fresh token.
    for r in &results {
        assert_eq!(
            r.as_ref().unwrap().key,
            "fresh-token",
            "all tasks must get the fresh token"
        );
    }

    // The refresher should have been called exactly once.
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "refresher must be called exactly once despite 4 concurrent callers"
    );
}

#[tokio::test]
async fn auth_permanent_failure_stops_retries() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    let expired = EchoAgentAuth {
        key: "expired-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);

    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(FailingRefresher {
        call_count: call_count.clone(),
    }));

    // First auth(): refresher called, refresh_chain records permanent failure.
    let err1 = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err1, AuthError::Refresh(RefreshTokenError::Permanent(_))),
        "first call should return PermanentFailure, got: {err1:?}"
    );

    // Second auth(): permanent failure cached, refresher NOT called.
    let err2 = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err2, AuthError::Refresh(RefreshTokenError::Permanent(_))),
        "second call should return PermanentFailure, got: {err2:?}"
    );

    // Refresher must have been called exactly once.
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "refresher must be called exactly once"
    );

    // hot_swap clears permanent failure; subsequent auth() succeeds.
    let valid = EchoAgentAuth {
        key: "new-valid-key".into(),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(valid);
    assert_eq!(mgr.auth().await.unwrap().key, "new-valid-key");
}

/// auth() re-reads disk via pick_up_sibling_token and returns the
/// sibling-written token when the in-memory token is stale.
#[tokio::test]
async fn auth_legacy_session_picks_up_sibling_disk_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    mgr.hot_swap(EchoAgentAuth {
        key: "stale-oidc".into(),
        auth_mode: AuthMode::Oidc,
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });

    // Sibling writes a valid token to disk.
    let fresh = EchoAgentAuth {
        key: "fresh-from-sibling".into(),
        auth_mode: AuthMode::Oidc,
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, fresh);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let auth = mgr.auth().await.expect("should pick up sibling token");
    assert_eq!(auth.key, "fresh-from-sibling");
}

/// refresh_chain returns TransientFailure when the refresher reports one.
#[tokio::test]
async fn refresh_chain_surfaces_transient_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(EchoAgentAuth {
        key: "expired".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });

    struct TransientRefresher;
    #[async_trait::async_trait]
    impl TokenRefresher for TransientRefresher {
        async fn refresh(&self, _: RefreshReason) -> crate::auth::refresh::RefreshOutcome {
            crate::auth::refresh::RefreshOutcome::TransientFailure {
                message: "idp timeout".into(),
            }
        }
    }
    mgr.set_refresher(Arc::new(TransientRefresher));

    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Transient(_))),
        "TransientFailure should surface as a transient refresh error, got {err:?}"
    );
}

/// Regression: `current()` and `auth()` must agree on whether an
/// expired API key is usable. Pre-fix, `current()` filtered with
/// `!is_token_expired()` (returning None) while the `auth()`
/// `TokenType::ApiKey` branch cloned the stale entry, so the UI saw
/// "logged out" while downstream consumers (trace upload, MCP,
/// embeddings) sent the stale key and hit 401.
#[tokio::test]
async fn auth_returns_expired_api_key_consistently_with_current() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Seed an API key that is past the 30-day TTL: `create_time` 60
    // days ago and no `expires_at`. `is_token_expired` falls through
    // to the TTL check and reports `true`.
    let expired_key = EchoAgentAuth {
        key: "stale-api-key".into(),
        auth_mode: AuthMode::ApiKey,
        create_time: Utc::now() - Duration::days(60),
        expires_at: None,
        refresh_token: None,
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired_key);

    // UI / sync read path: the stale key is filtered out.
    assert!(
        mgr.current().is_none(),
        "current() must hide the expired api_key (matches UI/login state)"
    );

    // Async path: must NOT clone the stale key for downstream
    // consumers. Surface `TokenExpiredNoRefresh` so callers can
    // funnel the user back through `echoagent login`.
    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err, AuthError::TokenExpiredNoRefresh),
        "auth() must report TokenExpiredNoRefresh for expired api_key, got: {err:?}",
    );
    assert!(
        mgr.get_valid_token().await.is_err(),
        "get_valid_token() must error rather than return the stale key"
    );

    // Sanity: a fresh API key restores both paths.
    let fresh_key = EchoAgentAuth {
        key: "fresh-api-key".into(),
        auth_mode: AuthMode::ApiKey,
        create_time: Utc::now(),
        expires_at: None,
        refresh_token: None,
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(fresh_key);
    assert_eq!(
        mgr.current().map(|a| a.key).as_deref(),
        Some("fresh-api-key")
    );
    assert_eq!(
        mgr.get_valid_token().await.ok().as_deref(),
        Some("fresh-api-key")
    );
}

/// Regression: after a permanent refresh failure (e.g. `invalid_grant`),
/// the proactive refresh task must back off rather than hammer
/// `auth()` in a tight loop. Pre-fix, an expired token + cached
/// PermanentFailure caused `sleep_dur=0` -> `auth()` -> error -> repeat.
///
/// Verified by observing the loop's iteration counter directly: in a
/// 300ms window we tolerate at most a few iterations (one for the
/// initial failure-recording pass, then back-off). Pre-fix the
/// counter would have been in the thousands.
#[tokio::test]
async fn proactive_refresh_backs_off_on_permanent_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Past-expiry OIDC token: without the backoff guard, the
    // proactive loop computes sleep_dur=0 forever.
    let expired = EchoAgentAuth {
        key: "expired".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);

    // Refresher returns invalid_grant the first time it is called and
    // counts every invocation. After the first call records the
    // permanent failure, the proactive loop must skip subsequent
    // calls until the failure is cleared.
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(FailingRefresher {
        call_count: call_count.clone(),
    }));

    let cancel = CancellationToken::new();
    mgr.start_proactive_refresh(cancel.clone());

    // Window: past the PROACTIVE_MIN_SLEEP-delayed first pass, yet two
    // orders of magnitude under BACKOFF_INTERVAL — a backed-off loop
    // completes at most a couple of iterations in it.
    tokio::time::sleep(PROACTIVE_MIN_SLEEP + StdDuration::from_millis(500)).await;

    let iterations = mgr.proactive_iteration_count();
    let after_failure = call_count.load(Ordering::SeqCst);

    // Direct observation of loop progress: a busy-loop produces
    // hundreds-to-thousands of iterations in 300ms, the backed-off
    // loop produces <= 5.
    assert!(
        iterations <= 5,
        "proactive refresh busy-looped after permanent failure: \
         {iterations} iterations (refresher calls: {after_failure})",
    );
    // Refresher invocation count is a secondary check: the
    // permanent_failure short-circuit in `refresh_chain` (added in
    // this PR) means at most 1 invocation here.
    assert!(
        after_failure <= 1,
        "refresher must be invoked at most once before the permanent \
         failure is recorded, got {after_failure} calls"
    );
    assert!(
        mgr.current_or_expired().is_none(),
        "permanent refresh failure must clear credentials",
    );
    // The proactive (background) loop must never emit the manual_auth KPI:
    // a background failure is not a user-facing forced re-login.
    assert!(
        mgr.manual_auth_last_emit().is_none(),
        "the proactive background loop must not emit a manual_auth event",
    );

    cancel.cancel();
}

/// Regression: `start_proactive_refresh` must be
/// idempotent. Calling it twice on the same `Arc<AuthManager>` was
/// previously valid (no guard) and would `tokio::spawn` two
/// background tasks racing on the same in-memory state.
///
/// Asserting on `proactive_iteration_count` is not a meaningful signal
/// because the test fixture (ApiKey + expires_at: None) made every
/// spawned task sleep for `BACKOFF_INTERVAL` immediately. With or
/// without the guard the iteration counter stayed at 0, so that
/// assertion was vacuous (removing the guard left the test passing). The
/// fix is to assert on the new `proactive_start_count()` accessor,
/// which is bumped *inside* the `compare_exchange` success branch
/// in `start_proactive_refresh` -- so it is exactly 1 if the guard
/// fires and N otherwise. This directly observes the invariant
/// instead of inferring it from loop-iteration mechanics.
#[tokio::test]
async fn start_proactive_refresh_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    let stale_api_key = EchoAgentAuth {
        key: "stale-api-key".into(),
        auth_mode: AuthMode::ApiKey,
        create_time: Utc::now() - Duration::days(60),
        expires_at: None,
        refresh_token: None,
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(stale_api_key);

    let cancel = CancellationToken::new();
    // First call spawns the task; subsequent calls must be no-ops.
    mgr.start_proactive_refresh(cancel.clone());
    mgr.start_proactive_refresh(cancel.clone());
    mgr.start_proactive_refresh(cancel.clone());

    // Direct observation of the guard's behavior. Pre-fix: 3.
    // Post-fix: exactly 1.
    assert_eq!(
        mgr.proactive_start_count(),
        1,
        "start_proactive_refresh idempotency guard failed; expected exactly \
         1 spawn after 3 calls",
    );

    cancel.cancel();
}

/// Proactive path: near-expiry OIDC token -> background task fires
/// refresh_chain(PreRequest) -> consumer sees fresh token.
#[tokio::test]
async fn proactive_refresh_and_consumer_see_fresh_token_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // expires_at inside the 5-min buffer -> proactive fires immediately.
    mgr.hot_swap(EchoAgentAuth {
        key: "soon-to-expire".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-original".into()),
        expires_at: Some(Utc::now() + Duration::seconds(2)),
        ..EchoAgentAuth::test_default()
    });

    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));

    let cancel = CancellationToken::new();
    mgr.start_proactive_refresh(cancel.clone());
    // The loop's first pass runs after PROACTIVE_MIN_SLEEP (1 s), so the
    // observation window must comfortably exceed the floor.
    tokio::time::sleep(PROACTIVE_MIN_SLEEP + StdDuration::from_millis(1000)).await;

    assert!(call_count.load(Ordering::SeqCst) >= 1);
    assert_eq!(mgr.get_valid_token().await.unwrap(), "fresh-token");

    cancel.cancel();
}

/// Reactive path: expired OIDC token -> try_recover_unauthorized ->
/// refresh_chain(ServerRejected) -> refresher -> consumer sees fresh token.
#[tokio::test]
async fn reactive_401_recovery_produces_fresh_token_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    mgr.hot_swap(EchoAgentAuth {
        key: "expired-bearer".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-valid".into()),
        expires_at: Some(Utc::now() - Duration::minutes(10)),
        ..EchoAgentAuth::test_default()
    });

    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));

    assert!(
        mgr.try_recover_unauthorized(crate::auth::recovery::RecoverySource::Background)
            .await
    );
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
    assert_eq!(mgr.get_valid_token().await.unwrap(), "fresh-token");
}

// refresh_chain permanent-failure short-circuit via recovery is tested
// in recovery::tests::refresh_authority_short_circuits_on_cached_permanent_failure.

/// Different disk RT with expired AT: demote to transient so a sibling's
/// still-usable RT is not wiped by permanent clear.
#[tokio::test]
async fn refresh_chain_demotes_when_disk_rt_differs_even_if_at_expired() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    // Memory has rt-old; disk has rt-new (different RT) but its
    // access_token is also expired so try_use_disk_token rejects it
    // and we fall through to the refresher.
    let stale = EchoAgentAuth {
        key: "stale-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(stale);

    let sibling = EchoAgentAuth {
        key: "sibling-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-new".into()),
        expires_at: Some(Utc::now() - Duration::minutes(30)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, sibling.clone());
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    struct FailingRefresher;
    #[async_trait::async_trait]
    impl crate::auth::refresh::TokenRefresher for FailingRefresher {
        async fn refresh(
            &self,
            _reason: crate::auth::manager::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            crate::auth::refresh::RefreshOutcome::permanent(
                crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
                None,
            )
        }
    }
    mgr.set_refresher(Arc::new(FailingRefresher));

    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Transient(_))),
        "disk RT mismatch must demote even when sibling AT is expired, got: {err:?}",
    );
    assert_eq!(
        mgr.read_disk_auth().and_then(|a| a.refresh_token),
        Some("rt-new".into()),
        "sibling RT on disk must not be wiped when AT is only expired",
    );
    assert!(
        mgr.permanent_failure().is_none(),
        "demotion must not record a sticky permanent verdict",
    );
}

/// Regression test for the multi-process logout incident.
///
/// This is the shape `OidcRefresher` actually emits in production: the tried
/// credential is **fully attributed** (`tried_key` *and* `tried_refresh_token`
/// are `Some`). The pre-existing demotion tests all built the outcome with
/// `tried_key = None` — the external-binary shape — so they passed while the
/// OIDC path was gated behind `tried_key.is_none()` and could never demote.
///
/// Scenario: a sibling rotated the RT while our token exchange was in flight,
/// so the IdP rejected the RT we spent. That is a lost race, not a revoked
/// session: it must demote to transient and leave the sibling's credential on
/// disk untouched.
#[tokio::test]
async fn refresh_chain_demotes_when_attributed_tried_rt_differs_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    // We hold, and spend, the predecessor RT.
    let tried = EchoAgentAuth {
        key: "tried-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-spent".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(tried.clone());

    // A sibling already rotated: disk carries the successor RT. Its AT is
    // expired too, so disk adoption cannot short-circuit the failure path —
    // the demotion is the only thing standing between us and a wipe.
    let sibling = EchoAgentAuth {
        key: "sibling-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-successor".into()),
        expires_at: Some(Utc::now() - Duration::minutes(30)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, sibling);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    struct AttributedRejection(EchoAgentAuth);
    #[async_trait::async_trait]
    impl TokenRefresher for AttributedRejection {
        async fn refresh(
            &self,
            _reason: crate::auth::manager::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            // Exactly what OidcRefresher builds on a 400 invalid_grant.
            crate::auth::refresh::RefreshOutcome::permanent_for(
                crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
                &self.0,
            )
        }
    }
    mgr.set_refresher(Arc::new(AttributedRejection(tried)));

    let err = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::PreRequest)
        .await
        .unwrap_err();

    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Transient(_))),
        "a rejected RT that disk has already rotated past is a lost race, \
         not a revoked session; must demote to transient, got: {err:?}",
    );
    assert_eq!(
        mgr.read_disk_auth().and_then(|a| a.refresh_token),
        Some("rt-successor".into()),
        "the sibling's successor RT must survive our rejection",
    );
    assert!(
        mgr.permanent_failure().is_none(),
        "demotion must not record a sticky verdict that locks out every \
         sibling process until the user re-runs `echoagent login`",
    );
}

/// The demotion must *not* fire when disk still holds the very RT that was
/// just rejected: nobody rotated, the session really is dead, and holding on
/// to a known-revoked credential would loop forever.
#[tokio::test]
async fn refresh_chain_still_discards_when_attributed_tried_rt_matches_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let tried = EchoAgentAuth {
        key: "only-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-revoked".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(tried.clone());
    let mut store = AuthStore::new();
    store.insert(scope, tried.clone());
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    struct AttributedRejection(EchoAgentAuth);
    #[async_trait::async_trait]
    impl TokenRefresher for AttributedRejection {
        async fn refresh(
            &self,
            _reason: crate::auth::manager::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            crate::auth::refresh::RefreshOutcome::permanent_for(
                crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
                &self.0,
            )
        }
    }
    mgr.set_refresher(Arc::new(AttributedRejection(tried)));

    let err = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::PreRequest)
        .await
        .unwrap_err();

    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Permanent(_))),
        "an un-rotated rejected RT is a genuinely dead session, got: {err:?}",
    );
    assert!(
        mgr.permanent_failure().is_some(),
        "a genuine revocation must still record a verdict",
    );
}

/// Disk-first invalid_grant must not wipe an untried in-memory successor RT
/// (mem-ahead-of-disk after a failed persist of a successful rotation).
#[tokio::test]
async fn permanent_rtr_clears_only_the_tried_side_when_rts_diverge() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    // Mem: successor RT after a successful refresh whose disk write failed.
    mgr.hot_swap(EchoAgentAuth {
        key: "mem-successor".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-new".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    });
    // Disk: revoked predecessor RT (disk-first resolve will try this).
    let disk = EchoAgentAuth {
        key: "disk-predecessor".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, disk);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let calls = Arc::new(AtomicU32::new(0));
    struct TriedDiskRtr(Arc<AtomicU32>);
    #[async_trait::async_trait]
    impl TokenRefresher for TriedDiskRtr {
        async fn refresh(
            &self,
            _reason: crate::auth::manager::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            crate::auth::refresh::RefreshOutcome::permanent(
                crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
                Some("disk-predecessor".into()),
            )
        }
    }
    mgr.set_refresher(Arc::new(TriedDiskRtr(calls.clone())));

    let err = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::ServerRejected)
        .await
        .unwrap_err();
    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Permanent(_))),
        "must surface permanent for the tried disk RT, got: {err:?}",
    );
    assert!(
        mgr.read_disk_auth().is_none(),
        "rejected disk predecessor must be cleared",
    );
    assert_eq!(
        mgr.current_or_expired().and_then(|a| a.refresh_token),
        Some("rt-new".into()),
        "untried in-memory successor RT must not be wiped",
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Retain-path permanent (ClientRejected) still graces a soft-expired wire-valid AT.
#[tokio::test]
async fn client_rejected_graces_soft_expired_access_token() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Inside the early-invalidation buffer but still hard-valid.
    mgr.hot_swap(EchoAgentAuth {
        key: "buffered-at".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() + Duration::seconds(30)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    });

    struct AlwaysClientRejected;
    #[async_trait::async_trait]
    impl TokenRefresher for AlwaysClientRejected {
        async fn refresh(
            &self,
            _reason: crate::auth::manager::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            crate::auth::refresh::RefreshOutcome::permanent(
                crate::auth::error::RefreshTokenFailedReason::ClientRejected,
                Some("buffered-at".into()),
            )
        }
    }
    mgr.set_refresher(Arc::new(AlwaysClientRejected));

    let auth = mgr
        .auth()
        .await
        .expect("retain-path permanent must grace wire-valid AT");
    assert_eq!(auth.key, "buffered-at");
    assert!(
        mgr.current_or_expired().is_some(),
        "ClientRejected must retain credentials",
    );
}

/// Escalated permanent `Other` retains AT+RT (only RefreshTokenRejected discards).
#[tokio::test]
async fn permanent_other_retains_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    let session = EchoAgentAuth {
        key: "live-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-still-valid".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(session.clone());
    // Persist so disk clear would be observable.
    let mut store = AuthStore::new();
    store.insert(EchoAgentComConfig::default().auth_scope(), session);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    struct OtherPermanent;
    #[async_trait::async_trait]
    impl crate::auth::refresh::TokenRefresher for OtherPermanent {
        async fn refresh(
            &self,
            _reason: crate::auth::manager::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            crate::auth::refresh::RefreshOutcome::permanent(
                crate::auth::error::RefreshTokenFailedReason::Other,
                Some("live-key".into()),
            )
        }
    }
    mgr.set_refresher(Arc::new(OtherPermanent));

    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Permanent(_))),
        "escalated Other must still surface permanent, got: {err:?}",
    );
    assert!(
        mgr.read_disk_auth().is_some(),
        "Other must not clear disk credentials",
    );
    assert_eq!(
        mgr.current_or_expired().and_then(|a| a.refresh_token),
        Some("rt-still-valid".into()),
        "Other must retain in-memory RT",
    );
}

/// Sticky permanent must not block a different credential key (sibling RT).
#[tokio::test]
async fn sticky_permanent_allows_refresh_when_attempted_key_differs() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    mgr.hot_swap(EchoAgentAuth {
        key: "dead-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-dead".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );
    assert!(mgr.permanent_failure().is_some());

    // Sibling writes a different key + RT (AT hard-expired, RT may still work).
    let sibling = EchoAgentAuth {
        key: "sibling-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-sibling".into()),
        expires_at: Some(Utc::now() - Duration::minutes(30)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, sibling.clone());
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();
    // Load sibling into memory without clearing sticky via wire-valid hot_swap.
    mgr.with_inner_write(|inner| *inner = Some(sibling));

    assert!(
        mgr.permanent_failure().is_none(),
        "sticky verdict must not apply to a different credential key",
    );

    let calls = Arc::new(AtomicU32::new(0));
    struct CountingOk(Arc<AtomicU32>);
    #[async_trait::async_trait]
    impl crate::auth::refresh::TokenRefresher for CountingOk {
        async fn refresh(
            &self,
            _reason: crate::auth::manager::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            crate::auth::refresh::RefreshOutcome::Success(Box::new(EchoAgentAuth {
                key: "fresh-from-sibling-rt".into(),
                auth_mode: AuthMode::Oidc,
                refresh_token: Some("rt-sibling".into()),
                expires_at: Some(Utc::now() + Duration::hours(1)),
                ..EchoAgentAuth::test_default()
            }))
        }
    }
    mgr.set_refresher(Arc::new(CountingOk(calls.clone())));

    let auth = mgr
        .auth()
        .await
        .expect("sibling key must reach refresh_chain");
    assert_eq!(auth.key, "fresh-from-sibling-rt");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// Different disk RT with valid AT: adopt the sibling's token directly.
#[tokio::test]
async fn refresh_chain_demotes_to_transient_when_disk_rt_differs_and_at_valid() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let stale = EchoAgentAuth {
        key: "stale-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(stale);

    let sibling = EchoAgentAuth {
        key: "sibling-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-new".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-1".into()),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, sibling);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let calls = Arc::new(AtomicU32::new(0));
    struct CountingFailRefresher(Arc<AtomicU32>);
    #[async_trait::async_trait]
    impl crate::auth::refresh::TokenRefresher for CountingFailRefresher {
        async fn refresh(
            &self,
            _reason: crate::auth::manager::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            self.0.fetch_add(1, Ordering::SeqCst);
            crate::auth::refresh::RefreshOutcome::permanent(
                crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
                None,
            )
        }
    }
    mgr.set_refresher(Arc::new(CountingFailRefresher(calls.clone())));

    let result = mgr.auth().await;
    assert!(
        result.is_ok(),
        "should adopt valid sibling token: {result:?}"
    );
    assert_eq!(result.unwrap().key, "sibling-key");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "refresher must not be called when disk has a valid token"
    );
}

/// Regression: after `clear()` the verdict must *read as absent*
/// — nothing drops it explicitly; it is scoped to the cleared credential and
/// reads through as `None` once that credential is gone — so subsequent
/// `auth()` reports the more useful `NotLoggedIn` (rather than the stale
/// `invalid_grant` from the just-cleared session).
#[tokio::test]
async fn permanent_failure_reads_absent_after_clear_so_auth_reports_not_logged_in() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Seed + record a permanent failure (as if invalid_grant fired).
    let session = EchoAgentAuth {
        key: "broken-session".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-revoked".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(session);
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );
    assert!(mgr.permanent_failure().is_some());

    // User runs `echoagent logout` which calls clear().
    mgr.clear().unwrap();

    // The diagnostic the user now sees on the next request should be
    // "Not logged in. Run `echoagent login`.", not the stale invalid_grant.
    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err, AuthError::NotLoggedIn),
        "auth() after clear() must report NotLoggedIn, got: {err:?}",
    );
    assert!(
        mgr.permanent_failure().is_none(),
        "the credential-scoped verdict must read as absent after clear()",
    );

    // Same check for the hot_swap_clear() path.
    let session = EchoAgentAuth {
        key: "broken-2".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-2".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(session);
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );
    mgr.clear_in_memory();
    // clear_in_memory drops the credential but keeps a sticky permanent
    // verdict so a just-revoked RT is not re-tried until login.
    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(
            err,
            AuthError::Refresh(RefreshTokenError::Permanent(_)) | AuthError::NotLoggedIn
        ),
        "auth() after clear_in_memory must not re-hit a dead RT, got: {err:?}",
    );
}

/// `PERMANENT_FAILURE_TTL` means "5 *real* minutes", not "5 awake minutes":
/// a recoverable permanent failure cached just before a system suspend must
/// expire while the machine sleeps. The monotonic clock pauses across suspend,
/// so expiry is judged on both clocks (see `ScopedRefreshFailure::recorded_at`)
/// — this simulates the suspend by rewinding only the wall-clock arm and
/// asserts the failure no longer short-circuits `auth()` on wake.
#[tokio::test]
async fn permanent_failure_expires_on_wall_clock_across_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Seed a credential so the verdict scopes to it (an unscoped verdict
    // reads through as absent), using the non-sticky `Other` reason — the
    // "transient escalation just before lid close" case the TTL exists for.
    mgr.hot_swap(EchoAgentAuth {
        key: "tok".into(),
        ..EchoAgentAuth::test_default()
    });
    record_permanent_failure(&mgr, crate::auth::error::RefreshTokenFailedReason::Other);
    assert!(
        mgr.permanent_failure().is_some(),
        "freshly recorded failure must be live on both clocks",
    );

    // Simulate a >TTL suspend: monotonic elapsed stays ~0 (paused during
    // sleep), wall clock advanced past the TTL.
    mgr.force_permanent_failure_wall_aged_out();

    assert!(
        mgr.permanent_failure().is_none(),
        "a slept-through TTL must expire the cached permanent failure on wake",
    );
    assert!(
        !mgr.has_permanent_failure(),
        "has_permanent_failure must agree with permanent_failure()",
    );
}

// -- Regression: api_key in config.toml must not block OIDC refresh --

/// When a user has an OIDC session (auth.json) AND a model with api_key
/// in config.toml, the OIDC token must still be refreshable. auth()
/// checks TokenType (from AuthManager), not the global auth_method_id.
#[tokio::test]
async fn oidc_refresh_not_blocked_by_model_api_key() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Expired OIDC token (user has config.toml with api_key on another model).
    let expired_oidc = EchoAgentAuth {
        key: "expired-session-token".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("valid-rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired_oidc);

    // TokenType is OidcSession regardless of what models exist in config.
    assert_eq!(mgr.token_type(), TokenType::OidcSession);

    // auth() must attempt OIDC refresh, not short-circuit as ApiKey.
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(10),
    }));

    let result = mgr.auth().await;
    assert!(result.is_ok(), "auth() should succeed via OIDC refresh");
    assert_eq!(result.unwrap().key, "fresh-token");
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

// -- direct unit tests for `compute_proactive_sleep` --------
//
// The proactive task's gate chain is a small pure function; testing
// it directly (rather than through `start_proactive_refresh` and a
// sleep window) gives us per-branch coverage that would have caught
// the original vacuity in seconds. Each test below pins one
// arm of `compute_proactive_sleep`.

/// Permanent-failure cached -> backs off (>= BACKOFF_INTERVAL, plus jitter).
#[test]
fn compute_proactive_sleep_permanent_failure_returns_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    let oidc = EchoAgentAuth {
        key: "x".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(oidc);
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );
    let sleep = compute_proactive_sleep(&mgr);
    assert!(
        sleep >= BACKOFF_INTERVAL && sleep < BACKOFF_INTERVAL + StdDuration::from_secs(60),
        "expected backoff + jitter, got {sleep:?}"
    );
}

/// Non-refreshable types (LegacySession, ApiKey, None) -> BACKOFF_INTERVAL
/// even when expires_at is past. This is the gate the original
/// test failed to exercise.
#[test]
fn compute_proactive_sleep_non_refreshable_returns_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    // Inject a refresher so the "no refresher" branch doesn't mask
    // the gate we're testing.
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
        delay: StdDuration::from_millis(0),
    }));

    // (a) LegacySession (WebLogin) + Some(past) -- the canonical
    //     scenario where the absence of the gate produces a busy-loop.
    mgr.hot_swap(EchoAgentAuth {
        key: "legacy".into(),
        auth_mode: AuthMode::WebLogin,
        create_time: Utc::now() - Duration::hours(2),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    assert_eq!(mgr.token_type(), TokenType::LegacySession);
    assert_eq!(compute_proactive_sleep(&mgr), BACKOFF_INTERVAL);

    // (b) ApiKey + Some(past).
    mgr.hot_swap(EchoAgentAuth {
        key: "api".into(),
        auth_mode: AuthMode::ApiKey,
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    assert_eq!(mgr.token_type(), TokenType::ApiKey);
    assert_eq!(compute_proactive_sleep(&mgr), BACKOFF_INTERVAL);

    // (c) None (no credentials loaded).
    mgr.clear_in_memory();
    assert_eq!(mgr.token_type(), TokenType::None);
    assert_eq!(compute_proactive_sleep(&mgr), BACKOFF_INTERVAL);
}

/// Sleep gate raised -> BACKOFF_INTERVAL even for a refreshable token past
/// its expiry. Without this gate `refresh_chain` defers every attempt while
/// the proactive loop spins at `sleep_dur=0` (the busy-loop).
#[test]
fn compute_proactive_sleep_sleep_gated_returns_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
        delay: StdDuration::from_millis(0),
    }));
    // Refreshable OidcSession past the early-invalidation boundary: without
    // the gate this returns 0 (would busy-loop).
    mgr.hot_swap(EchoAgentAuth {
        key: "oidc".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    assert_eq!(
        compute_proactive_sleep(&mgr),
        PROACTIVE_MIN_SLEEP,
        "precondition: ungated expired refreshable token yields the floor sleep"
    );

    mgr.set_system_sleep_imminent(true);
    assert_eq!(
        compute_proactive_sleep(&mgr),
        BACKOFF_INTERVAL,
        "sleep gate must back the proactive loop off instead of busy-looping"
    );
}

/// Dark wake -> BACKOFF_INTERVAL while a wire-valid token can still be served:
/// `refresh_chain` defers that case, so the loop must back off rather than spin
/// at `sleep_dur=0` against the deferral. Once nothing usable can be served the
/// deferral stops and so does the backoff — both arms are asserted below.
#[test]
fn compute_proactive_sleep_dark_wake_returns_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
        delay: StdDuration::from_millis(0),
    }));
    // Wire-valid but past the early-invalidation boundary: renewal is due, and
    // `refresh_chain` will defer it in dark wake — so the loop must back off
    // rather than busy-loop against that deferral.
    mgr.hot_swap(EchoAgentAuth {
        key: "oidc".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() + Duration::minutes(2)),
        ..EchoAgentAuth::test_default()
    });
    assert_eq!(
        compute_proactive_sleep(&mgr),
        PROACTIVE_MIN_SLEEP,
        "precondition: renewal due outside dark wake yields the floor sleep"
    );

    mgr.set_dark_wake_for_test(true);
    assert_eq!(
        compute_proactive_sleep(&mgr),
        BACKOFF_INTERVAL,
        "dark wake must back the proactive loop off instead of busy-looping"
    );

    // Hard-expired: `refresh_chain` no longer defers, so backing off here would
    // strand the session for a full interval.
    mgr.hot_swap(EchoAgentAuth {
        key: "oidc".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    assert_eq!(
        compute_proactive_sleep(&mgr),
        PROACTIVE_MIN_SLEEP,
        "dark wake must not delay recovery when no usable token can be served"
    );
}

/// No refresher configured -> BACKOFF_INTERVAL even for refreshable
/// types. This is the startup-race guard.
#[test]
fn compute_proactive_sleep_no_refresher_returns_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(EchoAgentAuth {
        key: "oidc".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    // No `set_refresher` call -- the refresher slot is None.
    assert!(mgr.refresher.read().is_none());
    assert_eq!(compute_proactive_sleep(&mgr), BACKOFF_INTERVAL);
}

/// Refreshable type + no `expires_at` -> BACKOFF_INTERVAL (the
/// "external binary that doesn't return expiry" case).
#[test]
fn compute_proactive_sleep_refreshable_no_expiry_returns_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
        delay: StdDuration::from_millis(0),
    }));
    mgr.hot_swap(EchoAgentAuth {
        key: "external".into(),
        auth_mode: AuthMode::External,
        expires_at: None,
        ..EchoAgentAuth::test_default()
    });
    assert_eq!(mgr.token_type(), TokenType::ExternalBinary);
    assert_eq!(compute_proactive_sleep(&mgr), BACKOFF_INTERVAL);
}

/// Refreshable type + `Some(past)` and gates pass -> the floor sleep
/// (refresh on the next pass; floored so the adopt/skip fast paths can't
/// spin). This is the "happy path" the gates don't block.
#[test]
fn compute_proactive_sleep_refreshable_past_expiry_returns_floor() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
        delay: StdDuration::from_millis(0),
    }));
    mgr.hot_swap(EchoAgentAuth {
        key: "oidc".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    assert_eq!(mgr.token_type(), TokenType::OidcSession);
    assert_eq!(compute_proactive_sleep(&mgr), PROACTIVE_MIN_SLEEP);
}

/// Refreshable type + `Some(future)` and gates pass -> sleep_dur ~=
/// expires_at - buffer (positive, <= delta). We use a 1-hour horizon
/// and assert the result is in a sane range rather than an exact value
/// (executor scheduling jitter).
#[test]
fn compute_proactive_sleep_refreshable_future_expiry_returns_delta() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
        delay: StdDuration::from_millis(0),
    }));
    let expires_at = Utc::now() + Duration::hours(1);
    mgr.hot_swap(EchoAgentAuth {
        key: "oidc".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(expires_at),
        ..EchoAgentAuth::test_default()
    });
    let dur = compute_proactive_sleep(&mgr);
    // Expected: 1h - 5min (early_invalidation) - jitter (0–60s) ≈ 54–55min.
    // Range is generous (51–59min) to absorb both clock granularity and
    // the random jitter added by `compute_proactive_sleep`.
    assert!(
        dur >= StdDuration::from_secs(51 * 60) && dur <= StdDuration::from_secs(59 * 60),
        "expected ~55min, got {dur:?}",
    );
}

/// `permanent_failure` cache auto-expires after `PERMANENT_FAILURE_TTL`,
/// so a misclassified transient IdP error (e.g. `invalid_client` during
/// an OAuth client rotation) doesn't permanently log the user out.
#[tokio::test]
async fn permanent_failure_expires_after_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(EchoAgentAuth {
        key: "tok".into(),
        ..EchoAgentAuth::test_default()
    });
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::ClientRejected,
    );
    assert!(
        mgr.permanent_failure().is_some(),
        "freshly recorded failure should be sticky"
    );
    mgr.force_permanent_failure_aged_out();
    assert!(
        mgr.permanent_failure().is_none(),
        "aged-out recoverable failure should auto-expire so a retry can succeed"
    );

    // A revoked refresh token never self-heals: the verdict is sticky past the
    // TTL (only a credential change clears it). Stops re-pinging a dead RT.
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );
    mgr.force_permanent_failure_aged_out();
    assert!(
        mgr.permanent_failure().is_some(),
        "RefreshTokenRejected must stay sticky past the TTL",
    );
}

/// The sticky verdict is exempt from BOTH TTL clocks — the monotonic arm
/// (awake time) AND the wall arm (real time across a suspend, added by the
/// sleep-straddle fix). A revoked refresh token never self-heals with time:
/// re-pinging the IdP with it can only fail again, so no amount of aging on
/// either clock may expire the verdict. Only a credential change heals it —
/// the scoped read-through pinned by the `hot_swap` phase below. This is a
/// composition guard: the sticky/non-sticky split and the wall-clock arm
/// landed separately, so neither parent change could test their intersection.
#[tokio::test]
async fn sticky_verdict_survives_both_clocks_but_not_a_credential_change() {
    // Guard against a vacuous pass: with < TTL of monotonic uptime the aging
    // hook's `checked_sub` no-ops, and a *fresh* verdict would trivially
    // satisfy the survival asserts below.
    if std::time::Instant::now()
        .checked_sub(PERMANENT_FAILURE_TTL + StdDuration::from_secs(1))
        .is_none()
    {
        eprintln!(
            "skipping sticky_verdict_survives_both_clocks: host uptime < PERMANENT_FAILURE_TTL"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(EchoAgentAuth {
        key: "dead".into(),
        ..EchoAgentAuth::test_default()
    });
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );

    // Age the verdict past the TTL on the monotonic clock AND rewind the
    // wall-clock arm past it (what a >TTL suspend looks like to the reader).
    mgr.force_permanent_failure_aged_out();
    mgr.force_permanent_failure_wall_aged_out();
    match mgr.permanent_failure() {
        Some(AuthError::Refresh(RefreshTokenError::Permanent(e))) => assert_eq!(
            e.reason,
            crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
            "the surviving verdict must carry the sticky reason",
        ),
        other => panic!("sticky verdict must survive both clocks aging out, got {other:?}"),
    }

    // Time never heals it; a credential change does (read-through, no clear).
    mgr.hot_swap(EchoAgentAuth {
        key: "fresh".into(),
        ..EchoAgentAuth::test_default()
    });
    assert!(
        mgr.permanent_failure().is_none(),
        "stickiness must not outlive the credential it is scoped to",
    );
}

/// The verdict is scoped to the credential that produced it: swapping in a
/// different credential makes it read through as absent, with no explicit
/// clear.
#[tokio::test]
async fn permanent_failure_is_scoped_to_its_credential() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    mgr.hot_swap(EchoAgentAuth {
        key: "dead".into(),
        ..EchoAgentAuth::test_default()
    });
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );
    assert!(mgr.permanent_failure().is_some());

    // A different credential — no clear call — reads through as no failure.
    mgr.hot_swap(EchoAgentAuth {
        key: "fresh".into(),
        ..EchoAgentAuth::test_default()
    });
    assert!(
        mgr.permanent_failure().is_none(),
        "verdict must not apply to a different credential",
    );
}

/// The verdict is about the *refresh* token: `auth()` must serve a cached
/// access token that is still within its real `expires_at` (buffer-expired
/// but wire-valid) despite a permanent verdict scoped to that credential,
/// without consulting the refresher. Once the same credential passes real
/// expiry, the bypass no longer applies and the permanent error surfaces.
#[tokio::test]
async fn auth_serves_wire_valid_token_despite_permanent_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    // CI runs in K8s pods where is_devbox_environment() is true; without this
    // the past-expiry phase would mint via devbox recovery instead of
    // surfacing the permanent error.
    mgr.set_devbox_env_for_test(false);

    // Token in the 5-min buffer (1 min before real expiry): buffer-expired,
    // still valid by the IdP's clock.
    mgr.hot_swap(EchoAgentAuth {
        key: "wire-valid".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-dead".into()),
        expires_at: Some(Utc::now() + Duration::minutes(1)),
        ..EchoAgentAuth::test_default()
    });
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );
    assert!(
        mgr.permanent_failure().is_some(),
        "verdict must scope to the live credential",
    );

    // A refresher is wired but must never be consulted: the verdict
    // short-circuits the chain and the bypass serves the cached bearer.
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::ZERO,
    }));

    let served = mgr
        .auth()
        .await
        .expect("a wire-valid token must be served despite the verdict");
    assert_eq!(
        served.key, "wire-valid",
        "auth() must return the cached wire-valid bearer",
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "the verdict must gate the refresher; serving the cached token is free",
    );

    // Same credential (same key, so the verdict still scopes to it) past its
    // real expiry: the bypass no longer applies.
    mgr.hot_swap(EchoAgentAuth {
        key: "wire-valid".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-dead".into()),
        expires_at: Some(Utc::now() - Duration::minutes(1)),
        ..EchoAgentAuth::test_default()
    });
    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Permanent(_))),
        "past real expiry the verdict must surface, got: {err:?}",
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "the cached verdict must keep short-circuiting the refresher",
    );
}

/// Refresh-failure grace: when the in-memory token is in the 5-min
/// early-invalidation buffer AND `refresh_chain` fails, `auth()`
/// returns the cached token if it's still within its real `expires_at`.
/// The user doesn't see a chat-turn failure for an IdP blip during
/// the buffer window.
#[tokio::test]
async fn auth_returns_cached_token_when_refresh_fails_within_real_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    // Point at an unreachable proxy so refresh_chain fails fast.
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg).with_proxy_base_url("http://127.0.0.1:1"));

    // Token in the 5-min buffer (1 min before real expiry) -- past
    // the buffer threshold but still valid by the IdP's clock.
    let in_buffer = EchoAgentAuth {
        key: "still-valid-by-idp".into(),
        auth_mode: AuthMode::Oidc,
        create_time: Utc::now() - Duration::minutes(55),
        user_id: "user-42".into(),
        refresh_token: Some("rt".into()),
        // Real expiry 1 min away; our 5-min buffer marks it expired.
        expires_at: Some(Utc::now() + Duration::minutes(1)),
        oidc_issuer: Some("http://127.0.0.1:1".into()),
        oidc_client_id: Some("client".into()),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(in_buffer);

    let result = mgr.auth().await.expect("grace should return cached token");
    assert_eq!(
        result.key, "still-valid-by-idp",
        "auth() must return the cached token when refresh fails within real expiry"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_writes_disk_before_user_enrichment() {
    // Mock /user endpoint that blocks on a Notify before responding.
    let release = Arc::new(tokio::sync::Notify::new());
    let release_for_handler = Arc::clone(&release);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/user",
        axum::routing::get(move || {
            let r = Arc::clone(&release_for_handler);
            async move {
                r.notified().await;
                axum::Json(serde_json::json!({
                    "userId": "enriched-user-id",
                    "email": "enriched@example.com",
                    "teamId": "enriched-team",
                }))
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), cfg).with_proxy_base_url(&format!("http://127.0.0.1:{port}")),
    );

    // user_id starts empty -- a freshly rotated OIDC token doesn't
    // yet know its user_id; that's exactly what /user enriches.
    // (If user_id were set AND mismatched the proxy's response, the
    // enrichment would correctly bail with reason=user_changed.)
    let new_auth = EchoAgentAuth {
        key: "rotated-key".into(),
        refresh_token: Some("rotated-rt".into()),
        user_id: String::new(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };

    // `update()` must return well before the `/user` timeout. The
    // proxy handler is blocked on `release.notified()` until we say
    // so; if `update()` was awaiting `/user` inline, this would
    // hang.
    let returned = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        mgr.update(new_auth.clone()),
    )
    .await
    .expect("update() must not block on /user")
    .expect("update() must succeed");
    assert_eq!(returned.key, "rotated-key");

    // Disk must already reflect the rotated tokens, even though
    // /user has not responded yet.
    let on_disk_before = read_auth_json(&dir.path().join("auth.json")).unwrap();
    let entry_before = on_disk_before.values().next().expect("entry written");
    assert_eq!(
        entry_before.key, "rotated-key",
        "rotated key must be on disk before /user lands"
    );
    assert_eq!(
        entry_before.refresh_token.as_deref(),
        Some("rotated-rt"),
        "rotated refresh_token must be on disk before /user lands"
    );
    assert_eq!(
        entry_before.team_id, None,
        "enrichment must not have landed yet"
    );

    // Now release the /user handler and wait for the enrichment
    // task to merge into disk. Poll up to 5s.
    release.notify_one();
    let auth_path = dir.path().join("auth.json");
    let mut enriched = None;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let store = read_auth_json(&auth_path).unwrap();
        let entry = store.values().next().unwrap().clone();
        if entry.team_id.is_some() {
            enriched = Some(entry);
            break;
        }
    }
    let enriched = enriched.expect("enrichment must land within 5s");

    // Enrichment must have merged in WITHOUT clobbering the rotated
    // tokens.
    assert_eq!(enriched.key, "rotated-key", "tokens preserved");
    assert_eq!(
        enriched.refresh_token.as_deref(),
        Some("rotated-rt"),
        "refresh_token preserved"
    );
    assert_eq!(enriched.team_id.as_deref(), Some("enriched-team"));
    assert_eq!(enriched.user_id, "enriched-user-id");

    server.abort();
}

/// Regression: back-to-back `update()` calls with different
/// `refresh_token`s must converge to the LATEST token on disk, even
/// though both spawned enrichment tasks read-modify-write disk
/// concurrently. This locks the property the spawn-task file lock
/// buys us; without it, the next "drop the lock for performance"
/// PR silently regresses (an interleaved enrichment write can
/// resurrect the older `refresh_token`, re-opening the
/// `invalid_grant` race).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrichment_task_preserves_interleaved_token_rotation() {
    // /user returns the SAME user_id for both calls so neither
    // enrichment aborts via `user_changed`. The 50 ms latency keeps
    // task v1 alive past the v2 update.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/user",
        axum::routing::get(|| async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            axum::Json(serde_json::json!({
                "userId": "stable-user",
                "email": "user@corp.com",
                "teamId": "team-alpha",
            }))
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), cfg).with_proxy_base_url(&format!("http://127.0.0.1:{port}")),
    );

    // Same user_id so neither enrichment aborts; only the rotated
    // token fields differ -- the property under test.
    let auth_v1 = EchoAgentAuth {
        key: "key-v1".into(),
        refresh_token: Some("rt-v1".into()),
        user_id: "stable-user".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    let auth_v2 = EchoAgentAuth {
        key: "key-v2".into(),
        refresh_token: Some("rt-v2".into()),
        user_id: "stable-user".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };

    // Two rotations back-to-back. v2's update() lands while v1's
    // spawned enrichment task is still in /user.
    mgr.update(auth_v1).await.unwrap();
    mgr.update(auth_v2).await.unwrap();

    // Wait for both spawned tasks to land. Each: 50ms /user + lock
    // wait + write. We poll for the eventually-consistent state.
    let auth_path = dir.path().join("auth.json");
    let mut final_state = None;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let store = read_auth_json(&auth_path).unwrap();
        let entry = store.values().next().unwrap().clone();
        // Both rotations done AND enrichment landed.
        if entry.refresh_token.as_deref() == Some("rt-v2") && entry.team_id.is_some() {
            final_state = Some(entry);
            break;
        }
    }
    let final_state = final_state.expect("v2 + enrichment must land within 3s");

    // Core invariant: v2's tokens survive both enrichment writes.
    assert_eq!(
        final_state.refresh_token.as_deref(),
        Some("rt-v2"),
        "v2 refresh_token must survive v1's stale enrichment write"
    );
    assert_eq!(
        final_state.key, "key-v2",
        "v2 access token must survive v1's stale enrichment write"
    );
    // Enrichment actually ran.
    assert_eq!(final_state.team_id.as_deref(), Some("team-alpha"));
    assert_eq!(final_state.user_id, "stable-user");

    server.abort();
}

/// Regression for the user-switch abort path: if disk's `user_id`
/// changes during an in-flight `/user` call (a different user
/// signed in via a sibling process), the spawned enrichment must
/// abort cleanly rather than overlay a previous user's
/// team/org/profile fields onto the new user's entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrichment_aborts_when_disk_user_changes_mid_flight() {
    // Slow /user so we have time to swap the disk entry mid-flight.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/user",
        axum::routing::get(|| async {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            axum::Json(serde_json::json!({
                "userId": "fetched-user",
                "email": "fetched@corp.com",
                "teamId": "fetched-team",
            }))
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), cfg.clone())
            .with_proxy_base_url(&format!("http://127.0.0.1:{port}")),
    );

    // Initial entry's user_id matches what /user will return, so
    // enrichment WOULD apply normally.
    let initial = EchoAgentAuth {
        key: "initial-key".into(),
        refresh_token: Some("initial-rt".into()),
        user_id: "fetched-user".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.update(initial).await.unwrap();

    // Race: while /user is in-flight, a "different user" overwrites
    // disk. The enrichment must NOT overlay onto this new entry.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let intruder = EchoAgentAuth {
        key: "intruder-key".into(),
        refresh_token: Some("intruder-rt".into()),
        user_id: "intruder-user".into(),
        team_id: Some("intruder-team".into()),
        email: Some("intruder@corp.com".into()),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    let mut store = AuthStore::new();
    store.insert(scope.clone(), intruder);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    // The /user mock takes 300 ms; after that the spawned enrichment
    // either writes (overlay path -- the regression we're guarding
    // against) or aborts silently. Poll the disk over a 3 s window
    // and fail fast at the first poll that shows an overlay -- a
    // wall-clock `sleep(800ms)` would mask both slow-CI flakes and
    // a real regression that just happens to land >800ms in.
    let auth_path = dir.path().join("auth.json");
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let store = read_auth_json(&auth_path).unwrap();
        let entry = store.get(&scope).expect("entry exists");
        assert_eq!(
            entry.user_id, "intruder-user",
            "intruder's user_id must survive aborted enrichment"
        );
        assert_eq!(
            entry.refresh_token.as_deref(),
            Some("intruder-rt"),
            "intruder's refresh_token must survive aborted enrichment"
        );
        assert_eq!(
            entry.key, "intruder-key",
            "intruder's access token must survive aborted enrichment"
        );
        assert_eq!(
            entry.team_id.as_deref(),
            Some("intruder-team"),
            "intruder's team must NOT be overwritten with fetched-team"
        );
        assert_eq!(
            entry.email.as_deref(),
            Some("intruder@corp.com"),
            "intruder's email must NOT be overwritten with fetched@corp.com"
        );
    }

    server.abort();
}

/// Regression: on initial Team-principal login, the OIDC flow
/// stamps `auth.user_id = team_id` as a placeholder so telemetry
/// can distinguish teams immediately (see `extract_user_info` in
/// `oidc.rs`). The `/user` enrichment then returns the *real*
/// user_id and must overlay it onto disk -- this is the entire
/// point of the enrichment call for Team logins. Earlier revisions
/// of this PR compared `disk.user_id` against `user_info.user_id`
/// and treated this legitimate placeholder->real swap as a
/// concurrent user-switch, throwing away the email / team_name /
/// org fields. The guard now compares against the user_id we
/// *wrote* (`auth.user_id`), which matches disk on the bootstrap
/// path and only diverges when a sibling actually stomped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrichment_overlays_team_login_placeholder_user_id() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/user",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "userId": "real-user-id",
                "email": "user@corp.com",
                "firstName": "Real",
                "lastName": "User",
                "principalType": "Team",
                "principalId": "team-xyz",
                "teamId": "team-xyz",
                "teamName": "Some Team",
                "teamRole": "MEMBER",
                "organizationId": "org-abc",
                "organizationName": "Some Org",
                "organizationRole": "ORGANIZATION_ROLE_MEMBER",
            }))
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(
        AuthManager::new(dir.path(), cfg).with_proxy_base_url(&format!("http://127.0.0.1:{port}")),
    );

    // Mirrors what `extract_user_info` returns for a Team principal:
    // user_id stamped with the team_id placeholder; email + profile
    // + team_name + org_* all empty until /user lands.
    let team_login = EchoAgentAuth {
        key: "team-key".into(),
        refresh_token: Some("team-rt".into()),
        user_id: "team-xyz".into(),
        email: None,
        first_name: None,
        last_name: None,
        principal_type: Some("Team".into()),
        principal_id: Some("team-xyz".into()),
        team_id: Some("team-xyz".into()),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };
    mgr.update(team_login).await.unwrap();

    // Wait for the spawned enrichment to land.
    let auth_path = dir.path().join("auth.json");
    let mut enriched = None;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let store = read_auth_json(&auth_path).unwrap();
        let entry = store.values().next().expect("entry exists").clone();
        if entry.email.is_some() {
            enriched = Some(entry);
            break;
        }
    }
    let enriched = enriched.expect("enrichment must overlay onto Team login");

    // The whole point: real user_id replaces the team_id placeholder.
    assert_eq!(
        enriched.user_id, "real-user-id",
        "team_id placeholder must be replaced by real user_id from /user"
    );
    assert_eq!(enriched.email.as_deref(), Some("user@corp.com"));
    assert_eq!(enriched.first_name.as_deref(), Some("Real"));
    assert_eq!(enriched.last_name.as_deref(), Some("User"));
    assert_eq!(enriched.team_name.as_deref(), Some("Some Team"));
    assert_eq!(enriched.team_role.as_deref(), Some("MEMBER"));
    assert_eq!(enriched.organization_id.as_deref(), Some("org-abc"));
    assert_eq!(enriched.organization_name.as_deref(), Some("Some Org"));
    assert_eq!(
        enriched.organization_role.as_deref(),
        Some("ORGANIZATION_ROLE_MEMBER")
    );
    // Tokens and team-id-as-principal-id preserved.
    assert_eq!(enriched.key, "team-key");
    assert_eq!(enriched.refresh_token.as_deref(), Some("team-rt"));
    assert_eq!(enriched.principal_type.as_deref(), Some("Team"));
    assert_eq!(enriched.team_id.as_deref(), Some("team-xyz"));

    server.abort();
}

/// Type-system invariant: `apply_user_info_enrichment` must NEVER
/// touch `key`, `refresh_token`, `expires_at`, `oidc_issuer`,
/// `oidc_client_id`, `auth_mode`, `create_time`, or
/// `has_echo_agent_code_access`. The `&mut EchoAgentAuth` signature already
/// enforces this at the type level (you cannot construct a fresh
/// auth from a `UserInfo` -- there's no `From` impl), but a unit
/// test pins the exact list of preserved fields so a future
/// contributor adding a token-like field to both `EchoAgentAuth` and
/// `UserInfo` is forced to look here.
#[test]
fn apply_user_info_enrichment_preserves_token_fields() {
    let mut disk = EchoAgentAuth {
        key: "ROT_KEY".into(),
        refresh_token: Some("ROT_RT".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        oidc_issuer: Some("https://issuer.example".into()),
        oidc_client_id: Some("client-xyz".into()),
        auth_mode: AuthMode::Oidc,
        create_time: Utc::now() - Duration::minutes(10),
        has_echo_agent_code_access: Some(true),
        user_id: "old-user".into(),
        email: Some("old@corp.com".into()),
        team_id: Some("old-team".into()),
        ..EchoAgentAuth::test_default()
    };
    let snapshot = disk.clone();

    let user_info = UserInfo {
        user_id: "new-user".into(),
        email: Some("new@corp.com".into()),
        first_name: Some("New".into()),
        last_name: Some("User".into()),
        profile_image_asset_id: None,
        principal_type: None,
        principal_id: None,
        team_id: Some("new-team".into()),
        team_name: Some("New Team".into()),
        team_role: None,
        organization_id: None,
        organization_name: None,
        organization_role: None,
        user_blocked_reason: None,
        team_blocked_reasons: None,
        coding_data_retention_opt_out: None,
        subscription_tier: None,
    };

    apply_user_info_enrichment(&mut disk, user_info);

    // Token fields and provenance untouched.
    assert_eq!(disk.key, snapshot.key);
    assert_eq!(disk.refresh_token, snapshot.refresh_token);
    assert_eq!(disk.expires_at, snapshot.expires_at);
    assert_eq!(disk.oidc_issuer, snapshot.oidc_issuer);
    assert_eq!(disk.oidc_client_id, snapshot.oidc_client_id);
    assert_eq!(disk.auth_mode, snapshot.auth_mode);
    assert_eq!(disk.create_time, snapshot.create_time);
    assert_eq!(
        disk.has_echo_agent_code_access,
        snapshot.has_echo_agent_code_access
    );

    // Enrichment fields updated.
    assert_eq!(disk.user_id, "new-user");
    assert_eq!(disk.email.as_deref(), Some("new@corp.com"));
    assert_eq!(disk.team_id.as_deref(), Some("new-team"));
    assert_eq!(disk.team_name.as_deref(), Some("New Team"));
    assert_eq!(disk.first_name.as_deref(), Some("New"));
}

/// Regression: async provider calls must drive `auth()` so tool requests get refreshed tokens.
#[tokio::test]
#[serial_test::serial] // reaches `resolve_static_api_key`, which reads the key env vars
async fn current_api_key_async_drives_refresh_chain() {
    use echo_agent_test_support::EnvGuard;
    use echo_agent_tools::types::ApiKeyProvider;

    let _echo_agent = EnvGuard::unset("ECHO_AGENT_API_KEY");
    let _legacy = EnvGuard::unset("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(EchoAgentAuth {
        key: "expired-oidc".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));

    let provider = super::SharedAuthKeyProvider(mgr.clone());
    assert_eq!(provider.current_api_key().as_deref(), Some("expired-oidc"));
    let key = provider.current_api_key_async().await;
    assert_eq!(key.as_deref(), Some("fresh-token"));
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

/// Regression: empty or corrupt auth.json must be recoverable on login.
/// Previously the guard in `update()` would skip the disk write on any
/// non-NotFound error, leaving a working in-memory session but a broken file.
#[tokio::test]
async fn update_recovers_from_empty_auth_json() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");
    let cfg = EchoAgentComConfig::default();
    std::fs::write(&auth_path, b"").unwrap();
    assert_eq!(std::fs::metadata(&auth_path).unwrap().len(), 0);

    let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));

    let new_auth = EchoAgentAuth {
        key: "recovered-token".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("recovered-rt".into()),
        user_id: "recovered-user".into(),
        email: Some("user@example.com".into()),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };

    let result = mgr.update(new_auth.clone()).await;
    assert!(
        result.is_ok(),
        "update must succeed and write to disk: {result:?}"
    );

    let current = mgr.current();
    assert_eq!(
        current.as_ref().map(|a| a.key.as_str()),
        Some("recovered-token")
    );

    let on_disk_raw = std::fs::read_to_string(&auth_path).unwrap();
    assert!(
        !on_disk_raw.is_empty(),
        "auth.json must not be empty after recovery"
    );
    let on_disk: AuthStore =
        serde_json::from_str(&on_disk_raw).expect("auth.json must be valid JSON after recovery");
    assert!(
        on_disk.contains_key(&cfg.auth_scope()),
        "persisted scope must be present"
    );
    assert_eq!(
        on_disk.get(&cfg.auth_scope()).map(|a| a.key.as_str()),
        Some("recovered-token")
    );
}

/// Same as above, but for whitespace-only content.
#[tokio::test]
async fn update_recovers_from_whitespace_only_auth_json() {
    let dir = tempfile::tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");
    let cfg = EchoAgentComConfig::default();
    std::fs::write(&auth_path, b"  \n\t  ").unwrap();

    let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));

    let new_auth = EchoAgentAuth {
        key: "ws-token".into(),
        auth_mode: AuthMode::Oidc,
        user_id: "ws-user".into(),
        ..make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now())
    };

    let result = mgr.update(new_auth).await;
    assert!(
        result.is_ok(),
        "update must succeed for whitespace-only file: {result:?}"
    );

    let on_disk = std::fs::read_to_string(&auth_path).unwrap();
    assert!(on_disk.contains("ws-token"), "credential must be persisted");
}

// -- sibling-rotation comparison ------------------------------------------

/// The demotion — and therefore whether a dozen processes keep their
/// credentials — rests entirely on this comparison, so pin its three cases
/// directly rather than only through the refresh chain.
#[test]
fn refresh_token_superseded_needs_a_successor_on_disk() {
    assert!(
        AuthManager::refresh_token_superseded(Some("rt-successor"), "rt-spent"),
        "a different RT on disk is a sibling's successor: demote"
    );
    assert!(
        !AuthManager::refresh_token_superseded(Some("rt-spent"), "rt-spent"),
        "disk still holding the RT the IdP just rejected is a real revocation"
    );
    assert!(
        !AuthManager::refresh_token_superseded(None, "rt-spent"),
        "no RT on disk means there is no successor to fall back to, so the \
         rejection must be honored rather than demoted into a retry loop"
    );
}

// -- sibling_has_different_refresh_token ----------------------------------

/// Expired disk AT with different RT is still treated as a sibling RT
/// (may still be refreshable; must not be wiped by permanent clear).
#[tokio::test]
async fn sibling_different_rt_with_expired_at_is_still_sibling() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));

    // In-memory: the original RT (revoked via rotation), AT expired.
    let original = EchoAgentAuth {
        key: "original-at".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-original".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(original);

    // Disk: the successor RT from rotation, AT also expired.
    let successor = EchoAgentAuth {
        key: "successor-at".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-successor".into()),
        expires_at: Some(Utc::now() - Duration::minutes(30)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(cfg.auth_scope(), successor);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let disk_rt = mgr.read_disk_auth().and_then(|a| a.refresh_token);
    assert!(
        mgr.sibling_has_different_refresh_token(disk_rt.as_deref()),
        "different disk RT must demote even when the sibling AT is expired"
    );
}

/// Valid disk AT with different RT is a live sibling.
#[tokio::test]
async fn sibling_different_rt_with_valid_at_is_treated_as_live() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg.clone()));

    let original = EchoAgentAuth {
        key: "original-at".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-original".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(original);

    // Disk: valid token from sibling process.
    let sibling = EchoAgentAuth {
        key: "sibling-at".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-sibling".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(cfg.auth_scope(), sibling);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let disk_rt = mgr.read_disk_auth().and_then(|a| a.refresh_token);
    assert!(
        mgr.sibling_has_different_refresh_token(disk_rt.as_deref()),
        "valid disk token with different RT must be treated as live sibling"
    );
}

/// Regression: refresh_chain(ServerRejected) must bypass the "double-check"
/// early return when the in-memory token is still valid (not expired).
/// Without this, a JWT that is time-valid but missing a subscription claim
/// (post-purchase) is returned as-is and the IdP is never contacted.
#[tokio::test]
async fn refresh_chain_server_rejected_bypasses_valid_token_double_check() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Seed a valid (non-expired) token — simulates a JWT that is missing
    // the subscription claim but is otherwise fine.
    let valid_but_rejected = EchoAgentAuth {
        key: "pre-subscription-jwt".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-original".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(valid_but_rejected);

    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));

    // Confirm the token is considered valid before refresh.
    assert_eq!(mgr.current().unwrap().key, "pre-subscription-jwt");

    // ServerRejected must force a real refresh despite the token being valid.
    let result = mgr
        .refresh_chain(
            crate::auth::token_type::TokenType::OidcSession,
            RefreshReason::ServerRejected,
        )
        .await;

    assert_eq!(
        result.unwrap().key,
        "fresh-token",
        "refresh_chain(ServerRejected) must contact the IdP even with a valid token"
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "refresher must be called exactly once"
    );
    assert_eq!(
        mgr.current().unwrap().key,
        "fresh-token",
        "in-memory token must be updated to the refreshed one"
    );
}

/// When two tasks both get 401 and call refresh_chain(ServerRejected)
/// concurrently, the second caller must return the already-refreshed token
/// without contacting the IdP again. This prevents the double-refresh race
/// where the second caller sends a rotated refresh token → invalid_grant.
#[tokio::test]
async fn refresh_chain_server_rejected_concurrent_skips_redundant_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Seed the "rejected" token that both tasks will see.
    let rejected = EchoAgentAuth {
        key: "rejected-jwt".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(rejected);

    let call_count = Arc::new(AtomicU32::new(0));
    // Slow refresher so the second task blocks on the lock long enough
    // to observe the first task's refresh result.
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(50),
    }));

    // Both tasks snapshot pre_lock_key = "rejected-jwt", then race for
    // the lock. The first refreshes → "fresh-token". The second finds
    // current() = "fresh-token" != pre_lock_key → returns early.
    let mgr1 = mgr.clone();
    let mgr2 = mgr.clone();

    let (r1, r2) = tokio::join!(
        mgr1.refresh_chain(
            crate::auth::token_type::TokenType::OidcSession,
            RefreshReason::ServerRejected,
        ),
        mgr2.refresh_chain(
            crate::auth::token_type::TokenType::OidcSession,
            RefreshReason::ServerRejected,
        ),
    );

    // Both must succeed with the refreshed token.
    assert_eq!(r1.unwrap().key, "fresh-token");
    assert_eq!(r2.unwrap().key, "fresh-token");

    // The IdP must be contacted exactly once, not twice.
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "refresher must be called exactly once; second caller should \
         return the already-refreshed token via the double-check guard"
    );
}

/// Counterpart: refresh_chain(PreRequest) with a valid token must
/// short-circuit and NOT call the refresher.
#[tokio::test]
async fn refresh_chain_pre_request_short_circuits_on_valid_token() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    let valid = EchoAgentAuth {
        key: "still-good".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(valid);

    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));

    let result = mgr
        .refresh_chain(
            crate::auth::token_type::TokenType::OidcSession,
            RefreshReason::PreRequest,
        )
        .await;

    assert_eq!(result.unwrap().key, "still-good");
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "PreRequest must NOT call refresher when token is valid"
    );
}

// -- login-time inline enrichment -------------------------------------------

/// Axum `/user` stub serving `body`; rejects requests missing `Bearer {token}`.
async fn spawn_user_stub(token: &'static str, body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = axum::Router::new().route(
        "/user",
        axum::routing::get(move |headers: axum::http::HeaderMap| async move {
            let authz = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if authz != format!("Bearer {token}") {
                return Err(axum::http::StatusCode::UNAUTHORIZED);
            }
            Ok(([("content-type", "application/json")], body))
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn enrich_auth_inline_populates_zdr_flags() {
    let body = r#"{"userId":"u-1","teamBlockedReasons":["BLOCKED_REASON_NO_LOGS"],"codingDataRetentionOptOut":true}"#;
    let base = spawn_user_stub("tok", body).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr =
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base);

    let mut auth = EchoAgentAuth {
        key: "tok".into(),
        ..EchoAgentAuth::test_default()
    };
    assert!(!auth.is_data_collection_disabled(), "precondition");

    mgr.enrich_auth_inline(&mut auth).await;
    assert!(auth.is_zdr_team(), "team_blocked_reasons must be merged");
    assert!(auth.coding_data_retention_opt_out);
    assert_eq!(auth.user_id, "u-1");
}

#[tokio::test]
async fn enrich_auth_inline_keeps_fields_absent_from_response() {
    // `/user` omitting a field must not clear a value the login flow set.
    let body = r#"{"userId":"u-1","teamBlockedReasons":["BLOCKED_REASON_NO_LOGS_MODERATED"]}"#;
    let base = spawn_user_stub("tok", body).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr =
        AuthManager::new(dir.path(), EchoAgentComConfig::default()).with_proxy_base_url(&base);

    let mut auth = EchoAgentAuth {
        key: "tok".into(),
        principal_type: Some("Team".into()),
        principal_id: Some("team-1".into()),
        ..EchoAgentAuth::test_default()
    };

    mgr.enrich_auth_inline(&mut auth).await;
    assert_eq!(auth.user_id, "u-1");
    assert_eq!(auth.principal_type.as_deref(), Some("Team"));
    assert_eq!(auth.principal_id.as_deref(), Some("team-1"));
    assert!(auth.is_zdr_team());
    assert!(
        !auth.coding_data_retention_opt_out,
        "absent field stays unchanged"
    );
}

#[tokio::test]
async fn enrich_auth_inline_unreachable_server_leaves_auth_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    // Bind-then-drop to get a port that refuses connections.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mgr = AuthManager::new(dir.path(), EchoAgentComConfig::default())
        .with_proxy_base_url(&format!("http://127.0.0.1:{port}"));

    let mut auth = EchoAgentAuth {
        key: "tok".into(),
        ..EchoAgentAuth::test_default()
    };
    let before = auth.clone();
    mgr.enrich_auth_inline(&mut auth).await;
    assert_eq!(auth.user_id, before.user_id);
    assert!(!auth.is_data_collection_disabled());
}

// ── force_login_team_uuid spine enforcement ───────────────────────────
//
// Regression coverage for the cached-token bypass: the pin must hold for every
// token the manager hands out (startup, sync reads, `auth()`), not just fresh
// login. Each test fails on the pre-fix tree.

/// `jsonwebtoken` needs a process-level CryptoProvider; tests that encode
/// JWTs can't rely on another test having installed it first.
fn ensure_crypto_provider() {
    let _ = jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.install_default();
}

/// A signed (HS256) access token carrying a `Team` principal, matching the
/// shape `peek_access_token_principal` extracts in production.
fn team_jwt(principal_id: &str) -> String {
    ensure_crypto_provider();
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &serde_json::json!({
            "sub": "user-1",
            "principal_type": "Team",
            "principal_id": principal_id,
            "exp": 9999999999u64,
        }),
        &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
    )
    .unwrap()
}

/// An access token carrying `principal_id` but NO `principal_type`.
fn principal_id_only_jwt(principal_id: &str) -> String {
    ensure_crypto_provider();
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &serde_json::json!({
            "sub": "user-1",
            "principal_id": principal_id,
            "exp": 9999999999u64,
        }),
        &jsonwebtoken::EncodingKey::from_secret(b"test-secret"),
    )
    .unwrap()
}

fn pinned_cfg(team: &str) -> EchoAgentComConfig {
    EchoAgentComConfig {
        force_login_team_uuid: Some(crate::auth::config::ForceLoginTeam::Single(
            team.to_string(),
        )),
        ..EchoAgentComConfig::default()
    }
}

/// A valid, non-expired OIDC session whose access token carries `principal_id`.
fn oidc_session_for_team(principal_id: &str) -> EchoAgentAuth {
    EchoAgentAuth {
        key: team_jwt(principal_id),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        oidc_issuer: Some(crate::auth::config::ECHO_AGENT_OAUTH2_ISSUER.to_string()),
        oidc_client_id: Some("client".into()),
        ..EchoAgentAuth::test_default()
    }
}

/// The repro: a wrong-team session persisted to disk (e.g. logged in before
/// the pin was deployed) must be cleared at construction, not silently loaded.
#[test]
fn new_clears_wrong_team_token_loaded_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = pinned_cfg("team-good");
    let scope = cfg.auth_scope();

    let mut store = AuthStore::new();
    store.insert(scope, oidc_session_for_team("team-wrong"));
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));
    assert!(mgr.current().is_none(), "wrong-team token must be hidden");
    assert!(
        mgr.current_or_expired().is_none(),
        "wrong-team token must be cleared from memory, not just hidden"
    );
    assert!(
        !dir.path().join("auth.json").exists(),
        "wrong-team auth.json must be cleared so the next launch re-logs in"
    );
}

/// A matching-team session on disk is loaded normally (no false positive).
#[test]
fn new_keeps_matching_team_token_loaded_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = pinned_cfg("team-good");
    let scope = cfg.auth_scope();
    let tok = oidc_session_for_team("team-good");

    let mut store = AuthStore::new();
    store.insert(scope, tok.clone());
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));
    assert_eq!(mgr.current().map(|a| a.key), Some(tok.key));
    assert!(dir.path().join("auth.json").exists());
}

/// `auth()` (the wire-bound chokepoint used by pager / MCP /
/// `try_ensure_fresh_auth`) rejects and clears a wrong-team cached token.
#[tokio::test]
async fn auth_rejects_and_clears_wrong_team_cached_token() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), pinned_cfg("team-good")));
    // hot_swap bypasses the pin (like a sibling adoption mid-session).
    mgr.hot_swap(oidc_session_for_team("team-wrong"));

    assert!(mgr.current().is_none(), "sync read must hide the token");

    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err, AuthError::PinnedTeamMismatch { .. }),
        "auth() must surface the policy violation, got {err:?}"
    );
    assert!(
        mgr.current_or_expired().is_none(),
        "auth() must clear the violating session"
    );
}

/// A matching-team cached token flows through `auth()` unchanged.
#[tokio::test]
async fn auth_accepts_matching_team_cached_token() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), pinned_cfg("team-good")));
    let tok = oidc_session_for_team("team-good");
    mgr.hot_swap(tok.clone());

    assert_eq!(mgr.current().map(|a| a.key.clone()), Some(tok.key.clone()));
    assert_eq!(mgr.auth().await.unwrap().key, tok.key);
}

/// No pin configured: any team is accepted (the enforcement is opt-in and
/// must not affect default deployments).
#[tokio::test]
async fn no_pin_accepts_any_team_cached_token() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    let tok = oidc_session_for_team("team-anything");
    mgr.hot_swap(tok.clone());

    assert_eq!(mgr.current().map(|a| a.key.clone()), Some(tok.key.clone()));
    assert_eq!(mgr.auth().await.unwrap().key, tok.key);
}

/// A token that silently refreshes into a wrong-team principal is rejected by
/// `auth()` (the wrapper gates refresh results, not just the cached fast path).
#[tokio::test]
async fn auth_rejects_token_refreshed_into_wrong_team() {
    struct WrongTeamRefresher {
        jwt: String,
    }
    #[async_trait::async_trait]
    impl TokenRefresher for WrongTeamRefresher {
        async fn refresh(&self, _reason: RefreshReason) -> crate::auth::refresh::RefreshOutcome {
            crate::auth::refresh::RefreshOutcome::Success(Box::new(EchoAgentAuth {
                key: self.jwt.clone(),
                auth_mode: AuthMode::Oidc,
                refresh_token: Some("rt-new".into()),
                expires_at: Some(Utc::now() + Duration::hours(1)),
                ..EchoAgentAuth::test_default()
            }))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), pinned_cfg("team-good")));
    // Expired matching session forces a refresh; the refresher returns a
    // wrong-team token (e.g. a re-pinned token family).
    mgr.hot_swap(EchoAgentAuth {
        expires_at: Some(Utc::now() - Duration::minutes(10)),
        ..oidc_session_for_team("team-good")
    });
    mgr.set_refresher(Arc::new(WrongTeamRefresher {
        jwt: team_jwt("team-wrong"),
    }));

    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err, AuthError::PinnedTeamMismatch { .. }),
        "refreshed wrong-team token must be rejected, got {err:?}"
    );
}

/// A sibling-written wrong-team token picked up by `force_reload_from_disk`
/// (relay reconnect) is cleared, not just hidden.
#[test]
fn force_reload_clears_wrong_team_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = pinned_cfg("team-good");
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg)); // empty disk at startup

    let mut store = AuthStore::new();
    store.insert(scope, oidc_session_for_team("team-wrong"));
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    mgr.force_reload_from_disk();
    assert!(
        mgr.current_or_expired().is_none(),
        "reloaded wrong-team token must be cleared, not just hidden"
    );
    assert!(
        !dir.path().join("auth.json").exists(),
        "force_reload must clear auth.json on a pin violation"
    );
}

// -- force_reload_from_disk: transient disk anomaly vs real logout ----------

/// A real incident in miniature: a live in-memory OIDC session (RT
/// present, no permanent_failure) while `auth.json` transiently reads as
/// missing — e.g. the first read right after wake-from-sleep resolves the path
/// to `ENOENT`. The refresh token may exist nowhere else, so the reload must
/// RETAIN it, not discard it (the discard previously kicked off a
/// 401 -> reactive refresh -> suspend-straddle -> invalid_grant cascade).
#[test]
fn force_reload_retains_live_rt_on_transient_file_missing() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    let session = EchoAgentAuth {
        key: "live-session".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("live-rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(session);
    assert!(mgr.permanent_failure().is_none());

    // No auth.json on disk at all -> FileMissing on every read.
    assert!(mgr.read_disk_auth().is_none());

    // Zero backoff so the retry budget is exhausted instantly.
    mgr.force_reload_from_disk_with(RELOAD_RETRY_TRIES, StdDuration::ZERO);

    let retained = mgr.current_or_expired();
    assert!(
        retained.is_some(),
        "a live RT must NOT be discarded on a transient FileMissing",
    );
    let retained = retained.unwrap();
    assert_eq!(retained.key, "live-session");
    assert_eq!(retained.refresh_token.as_deref(), Some("live-rt"));
}

/// Contrast with the retain case: once a `permanent_failure` is cached the RT
/// is known-dead, so a persistent FileMissing must drop it (and clear the
/// permanent_failure with it) so the next request reports `NotLoggedIn`.
#[tokio::test]
async fn force_reload_drops_rt_when_permanent_failure_set() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    let session = EchoAgentAuth {
        key: "broken".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-revoked".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(session);
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );
    assert!(mgr.permanent_failure().is_some());

    mgr.force_reload_from_disk_with(RELOAD_RETRY_TRIES, StdDuration::ZERO);

    assert!(
        mgr.current_or_expired().is_none(),
        "a known-dead RT (permanent_failure set) must be dropped",
    );
    assert!(
        mgr.permanent_failure().is_none(),
        "dropping creds must clear the cached permanent_failure",
    );
    assert!(matches!(
        mgr.auth().await.unwrap_err(),
        AuthError::NotLoggedIn
    ));
}

/// A readable `auth.json` that simply lacks our scope is the trustworthy
/// "logged out / scope removed" signal (distinct from a missing file), so the
/// in-memory credentials are dropped even though an RT is present.
#[test]
fn force_reload_drops_creds_on_entry_missing() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    let session = EchoAgentAuth {
        key: "live-session".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("live-rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(session);

    // auth.json exists and is readable, but holds only an unrelated scope ->
    // EntryMissing for this manager's scope.
    let mut store = AuthStore::new();
    store.insert(
        "https://example.invalid::nobody".to_string(),
        make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now()),
    );
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    mgr.force_reload_from_disk_with(RELOAD_RETRY_TRIES, StdDuration::ZERO);

    assert!(
        mgr.current_or_expired().is_none(),
        "scope absent on a readable auth.json is a real logout -> drop",
    );
}

/// When disk holds a fresh token for our scope, the reload adopts it on the
/// first read (no retry) — the healthy path is unchanged.
#[test]
fn force_reload_adopts_fresh_disk_token() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = EchoAgentComConfig::default();
    let scope = cfg.auth_scope();
    let mgr = Arc::new(AuthManager::new(dir.path(), cfg));

    let expired = EchoAgentAuth {
        key: "stale".into(),
        refresh_token: Some("old-rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(expired);

    let fresh = EchoAgentAuth {
        key: "fresh-from-disk".into(),
        refresh_token: Some("new-rt".into()),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };
    let mut store = AuthStore::new();
    store.insert(scope, fresh);
    write_auth_json(&dir.path().join("auth.json"), &store).unwrap();

    mgr.force_reload_from_disk_with(RELOAD_RETRY_TRIES, StdDuration::ZERO);

    assert_eq!(mgr.current().unwrap().key, "fresh-from-disk");
}

/// A token carrying `principal_id` without `principal_type` is matched on the
/// id alone: the pinned team is accepted, not falsely rejected.
#[tokio::test]
async fn pin_matches_principal_id_without_principal_type() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), pinned_cfg("team-good")));
    mgr.hot_swap(EchoAgentAuth {
        key: principal_id_only_jwt("team-good"),
        auth_mode: AuthMode::Oidc,
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });

    assert!(
        mgr.current().is_some(),
        "matching team id must be accepted even without principal_type"
    );
    assert!(mgr.auth().await.is_ok());
}

/// A cached `AuthMode::ApiKey` session is rejected under the kill switch (here
/// implied by a team pin), and honored when it's off.
#[tokio::test]
async fn cached_api_key_session_rejected_when_api_key_auth_disabled() {
    let api_key_session = || EchoAgentAuth {
        key: "echo-agent-cached-key".into(),
        auth_mode: AuthMode::ApiKey,
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    };

    // Switch ON (via a team pin, which implies api_key_auth_disabled): reject.
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), pinned_cfg("team-good")));
    mgr.hot_swap(api_key_session());
    assert!(
        mgr.current().is_none(),
        "cached api-key session must be hidden under the kill switch"
    );
    assert!(
        matches!(mgr.auth().await, Err(AuthError::ApiKeyAuthDisabled)),
        "auth() must reject a cached api-key session under the kill switch"
    );

    // Switch OFF (no pin / no disable): the api-key session is honored.
    let dir2 = tempfile::tempdir().unwrap();
    let mgr2 = Arc::new(AuthManager::new(dir2.path(), EchoAgentComConfig::default()));
    mgr2.hot_swap(api_key_session());
    assert_eq!(
        mgr2.current().map(|a| a.key),
        Some("echo-agent-cached-key".to_string()),
        "api-key session must work normally when the switch is off"
    );
}

#[tokio::test]
async fn shared_api_key_provider_resolves_live_bearer() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    let auth = EchoAgentAuth {
        key: "shared-provider-token".into(),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        create_time: Utc::now(),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(auth);

    let provider = shared_api_key_provider(mgr.clone());

    // Synchronous accessor surfaces the current (non-expired) bearer.
    assert_eq!(
        provider.current_api_key(),
        Some("shared-provider-token".to_string()),
        "shared_api_key_provider must expose the live bearer to out-of-crate consumers"
    );

    // Async accessor resolves a valid bearer without a network refresh when
    // the cached token is still fresh.
    assert_eq!(
        provider.current_api_key_async().await,
        Some("shared-provider-token".to_string()),
        "async accessor must resolve the current bearer for a fresh token"
    );

    // A hot-swap is reflected on the next resolution (no startup snapshot).
    let rotated = EchoAgentAuth {
        key: "rotated-token".into(),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        create_time: Utc::now(),
        ..EchoAgentAuth::test_default()
    };
    mgr.hot_swap(rotated);
    assert_eq!(
        provider.current_api_key(),
        Some("rotated-token".to_string()),
        "provider must follow the manager's refresh chain rather than snapshot at startup"
    );
}

/// No OAuth session → env or auth.json `echoagent::api_key` for voice/tools.
#[tokio::test]
#[serial_test::serial]
async fn shared_api_key_provider_static_fallthrough() {
    use echo_agent_test_support::EnvGuard;

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    let provider = shared_api_key_provider(mgr.clone());

    {
        let _legacy = EnvGuard::unset("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
        let _key = EnvGuard::set("ECHO_AGENT_API_KEY", "env-only-key");
        assert_eq!(
            provider.current_api_key_async().await.as_deref(),
            Some("env-only-key")
        );
    }

    {
        let _echo_agent = EnvGuard::unset("ECHO_AGENT_API_KEY");
        let _legacy = EnvGuard::unset("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
        crate::auth::store_api_key(dir.path(), "disk-api-key").unwrap();
        assert_eq!(
            provider.current_api_key_async().await.as_deref(),
            Some("disk-api-key")
        );
    }

    {
        let _key = EnvGuard::set("ECHO_AGENT_API_KEY", "env-should-lose");
        mgr.hot_swap(EchoAgentAuth {
            key: "session-bearer".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            create_time: Utc::now(),
            ..EchoAgentAuth::test_default()
        });
        assert_eq!(
            provider.current_api_key_async().await.as_deref(),
            Some("session-bearer")
        );
    }
}

#[tokio::test]
#[serial_test::serial]
async fn shared_api_key_provider_kill_switch_blocks_static() {
    use echo_agent_test_support::EnvGuard;

    let _key = EnvGuard::set("ECHO_AGENT_API_KEY", "blocked");
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(
        dir.path(),
        EchoAgentComConfig {
            disable_api_key_auth: Some(true),
            ..EchoAgentComConfig::default()
        },
    ));
    assert_eq!(
        shared_api_key_provider(mgr).current_api_key_async().await,
        None
    );
}

#[tokio::test]
#[serial_test::serial]
async fn shared_api_key_provider_oidc_preferred_blocks_static() {
    use echo_agent_test_support::EnvGuard;

    let _key = EnvGuard::set("ECHO_AGENT_API_KEY", "should-not-use");
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(
        dir.path(),
        EchoAgentComConfig {
            preferred_method: Some(crate::auth::PreferredAuthMethod::Oidc),
            ..EchoAgentComConfig::default()
        },
    ));
    assert_eq!(
        shared_api_key_provider(mgr).current_api_key_async().await,
        None
    );
}

/// preferred_method=api_key: leftover session must not beat static API key.
#[tokio::test]
#[serial_test::serial]
async fn shared_api_key_provider_api_key_preferred_skips_session() {
    use echo_agent_test_support::EnvGuard;

    let _legacy = EnvGuard::unset("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
    let _key = EnvGuard::set("ECHO_AGENT_API_KEY", "static-preferred");
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(
        dir.path(),
        EchoAgentComConfig {
            preferred_method: Some(crate::auth::PreferredAuthMethod::ApiKey),
            ..EchoAgentComConfig::default()
        },
    ));
    mgr.hot_swap(EchoAgentAuth {
        key: "leftover-oidc".into(),
        expires_at: Some(Utc::now() + Duration::hours(1)),
        create_time: Utc::now(),
        ..EchoAgentAuth::test_default()
    });
    assert_eq!(
        shared_api_key_provider(mgr)
            .current_api_key_async()
            .await
            .as_deref(),
        Some("static-preferred")
    );
}

/// Expired OAuth must not block static fallthrough on the sync path.
#[tokio::test]
#[serial_test::serial]
async fn shared_api_key_provider_sync_falls_through_when_session_expired() {
    use echo_agent_test_support::EnvGuard;

    let _legacy = EnvGuard::unset("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
    let _key = EnvGuard::set("ECHO_AGENT_API_KEY", "static-after-expiry");
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(EchoAgentAuth {
        key: "expired-oidc".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    let provider = shared_api_key_provider(mgr);
    assert_eq!(
        provider.current_api_key().as_deref(),
        Some("static-after-expiry"),
        "sync path must not return a dead session token over a live static key"
    );
    assert_eq!(
        provider.current_api_key_async().await.as_deref(),
        Some("static-after-expiry")
    );
}

/// A session inside the early-invalidation buffer is still wire-valid and
/// must beat a static key on the sync path.
#[tokio::test]
#[serial_test::serial]
async fn shared_api_key_provider_sync_buffered_session_beats_static() {
    use echo_agent_test_support::EnvGuard;
    use echo_agent_tools::types::ApiKeyProvider;

    let _legacy = EnvGuard::unset("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
    let _key = EnvGuard::set("ECHO_AGENT_API_KEY", "leftover-static");
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    // Two minutes out: inside the 5-minute buffer, but accepted on the wire.
    mgr.hot_swap(EchoAgentAuth {
        key: "buffered-oidc".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        expires_at: Some(Utc::now() + Duration::minutes(2)),
        ..EchoAgentAuth::test_default()
    });
    let provider = super::SharedAuthKeyProvider(mgr);
    assert_eq!(provider.current_api_key().as_deref(), Some("buffered-oidc"));
}

/// Auth.json create, rewrite (including same-length, caught by the inode in
/// the memo stamp), and logout must all invalidate the disk static-key memo.
#[tokio::test]
#[serial_test::serial]
async fn shared_api_key_provider_disk_memo_follows_rewrites() {
    use echo_agent_test_support::EnvGuard;

    let _echo_agent = EnvGuard::unset("ECHO_AGENT_API_KEY");
    let _legacy = EnvGuard::unset("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    let provider = shared_api_key_provider(mgr);

    assert_eq!(provider.current_api_key_async().await, None);

    for key in ["first-key", "fresh-key", "second-key-rotated"] {
        crate::auth::store_api_key(dir.path(), key).unwrap();
        assert_eq!(provider.current_api_key_async().await.as_deref(), Some(key));
    }

    crate::auth::clear_api_key(dir.path()).unwrap();
    assert_eq!(provider.current_api_key_async().await, None);
}

#[tokio::test]
#[serial_test::serial]
async fn process_key_from_model_env_key() {
    use crate::agent::config::{Config, resolve_model_list};
    use echo_agent_test_support::EnvGuard;

    const ENV: &str = "TEST_MODEL_ENV_KEY";
    const TOKEN: &str = "model-env-token";

    let _echo_agent = EnvGuard::unset("ECHO_AGENT_API_KEY");
    let _legacy = EnvGuard::unset("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
    let _tok = EnvGuard::set(ENV, TOKEN);

    let dm = crate::models::default_model();
    let cfg = Config::new_from_toml_cfg(
        &toml::from_str(&format!(
            r#"
            [model."{dm}"]
            model = "{dm}"
            env_key = "{ENV}"
            "#
        ))
        .unwrap(),
    )
    .unwrap();
    let key = resolve_model_list(&cfg, None)
        .get(dm)
        .and_then(|m| m.own_credential())
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    assert!(mgr.current().is_none());
    mgr.set_process_static_api_key(Some(key));
    assert_eq!(
        shared_api_key_provider(mgr)
            .current_api_key_async()
            .await
            .as_deref(),
        Some(TOKEN)
    );
}

#[tokio::test]
#[serial_test::serial]
async fn process_key_precedence() {
    use echo_agent_test_support::EnvGuard;

    let _echo_agent = EnvGuard::unset("ECHO_AGENT_API_KEY");
    let _legacy = EnvGuard::unset("ECHO_AGENT_CODE_ECHO_AGENT_API_KEY");
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    let provider = shared_api_key_provider(mgr.clone());

    assert_eq!(provider.current_api_key_async().await, None);

    crate::auth::store_api_key(dir.path(), "disk").unwrap();
    assert_eq!(
        provider.current_api_key_async().await.as_deref(),
        Some("disk")
    );

    mgr.set_process_static_api_key(Some("  process  ".into()));
    assert_eq!(
        provider.current_api_key_async().await.as_deref(),
        Some("process")
    );

    {
        let _key = EnvGuard::set("ECHO_AGENT_API_KEY", "env");
        assert_eq!(
            provider.current_api_key_async().await.as_deref(),
            Some("env")
        );
    }

    mgr.set_process_static_api_key(None);
    assert_eq!(
        provider.current_api_key_async().await.as_deref(),
        Some("disk")
    );

    let dir_blocked = tempfile::tempdir().unwrap();
    let blocked = Arc::new(AuthManager::new(
        dir_blocked.path(),
        EchoAgentComConfig {
            disable_api_key_auth: Some(true),
            ..EchoAgentComConfig::default()
        },
    ));
    blocked.set_process_static_api_key(Some("ignored".into()));
    assert_eq!(
        shared_api_key_provider(blocked)
            .current_api_key_async()
            .await,
        None
    );
}

fn expired_oidc() -> EchoAgentAuth {
    EchoAgentAuth {
        key: "expired-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    }
}

/// Signals when it has started, then blocks until released.
struct BlockingRefresher {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    call_count: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl TokenRefresher for BlockingRefresher {
    async fn refresh(&self, _reason: RefreshReason) -> crate::auth::refresh::RefreshOutcome {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        self.release.notified().await;
        crate::auth::refresh::RefreshOutcome::Success(Box::new(EchoAgentAuth {
            key: "fresh-token".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            refresh_token: Some("rt-new".into()),
            ..EchoAgentAuth::test_default()
        }))
    }
}

#[tokio::test]
async fn sleep_gate_defers_refresh_without_calling_idp() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(expired_oidc());
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));

    mgr.set_system_sleep_imminent(true);

    let err = mgr.auth().await.unwrap_err();
    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Transient(_))),
        "gated refresh must return a transient refresh error, got {err:?}"
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "the IdP refresher must NOT be called while the sleep gate is raised"
    );
}

/// A sleep-deferred refresh must not poison auth state: the deferral is a
/// typed transient (retryable on wake), maps to no `manual_auth` reason (a
/// lid close must never count as a forced re-login in the KPI), and records
/// no permanent-failure verdict — even after more deferred attempts than the
/// refresher-level escalation budget tolerates (the transient-blip budget
/// lives in the refresher, which a deferral never reaches).
///
/// Coverage depth: the gate is raised before the chain starts, so this drives
/// the step-3a deferral. The step-3c pre-IdP re-check (gate raised inside the
/// 3a→3c race window) returns the identical transient error and touches the
/// same state, but is not deterministically reachable without production test
/// hooks, so it is pinned only indirectly by these assertions.
#[tokio::test]
async fn sleep_deferred_refresh_is_transient_no_kpi_no_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    // Pin non-devbox so a deferred refresh surfaces the transient error
    // instead of minting via devbox recovery (CI runs in K8s pods).
    mgr.set_devbox_env_for_test(false);
    mgr.hot_swap(expired_oidc());
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));

    mgr.set_system_sleep_imminent(true);

    // More attempts than MAX_CONSECUTIVE_TRANSIENT_FAILURES: deferrals must
    // never accrue toward an escalated permanent verdict.
    for _ in 0..4 {
        let err = mgr.auth().await.unwrap_err();
        assert!(
            matches!(err, AuthError::Refresh(RefreshTokenError::Transient(_))),
            "a sleep-deferred refresh must be transient, got {err:?}"
        );
        assert_eq!(
            crate::auth::recovery::manual_auth_reason(&err),
            None,
            "a lid-close deferral must never map to a manual_auth KPI reason",
        );
    }
    assert!(
        mgr.permanent_failure().is_none(),
        "deferrals must not record a permanent-failure verdict",
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "the refresher must never run while the gate is raised",
    );

    // End-to-end through 401 recovery on a user-facing source: a deferred
    // recovery terminates with the transient error and emits no manual_auth.
    let mut rec = mgr.unauthorized_recovery(
        mgr.current_or_expired(),
        crate::auth::recovery::RecoverySource::Turn,
    );
    let err = rec.next().await.unwrap_err();
    assert!(
        matches!(err, AuthError::Refresh(RefreshTokenError::Transient(_))),
        "deferred recovery must surface the transient deferral, got {err:?}"
    );
    assert!(
        mgr.manual_auth_last_emit().is_none(),
        "a sleep-deferred recovery must not emit the manual_auth event",
    );
}

/// Dark wake defers a refresh only while a *wire-valid* token can still be
/// served — then the deferral costs nothing but latency.
#[tokio::test]
async fn dark_wake_defers_refresh_while_a_live_token_can_be_served() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    // Inside the early-invalidation buffer: due for renewal (`current()` is
    // None, so `refresh_chain` proceeds) but still accepted on the wire.
    mgr.hot_swap(EchoAgentAuth {
        key: "live-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() + Duration::minutes(2)),
        ..EchoAgentAuth::test_default()
    });
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));
    mgr.set_dark_wake_for_test(true);

    let err = mgr
        .refresh_chain(TokenType::OidcSession, RefreshReason::PreRequest)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            AuthError::Refresh(crate::auth::error::RefreshTokenError::Transient(_))
        ),
        "dark-wake refresh must return a transient refresh error, got {err:?}"
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        0,
        "with a live token to serve, the refresh token must not be sent into a \
         possible re-sleep"
    );
}

/// The inverse, and the one field logs caught: with **no** usable token, a
/// dark-wake deferral guarantees the caller 401s instead of merely delaying it.
/// A machine doing background work with the lid shut (leader mode + subagents)
/// accumulated hundreds of 401s across hours of continuous dark wake while
/// every recovery refresh was deferred. Refresh must proceed in that state.
#[tokio::test]
async fn dark_wake_does_not_defer_when_no_usable_token() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(expired_oidc());
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));
    mgr.set_dark_wake_for_test(true);

    assert_eq!(
        mgr.auth().await.unwrap().key,
        "fresh-token",
        "an expired credential in dark wake must still be refreshed"
    );
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

/// A 401 recovery is never deferred for dark wake: the server already rejected
/// what we hold, so deferring can only prolong the failure.
#[tokio::test]
async fn dark_wake_does_not_defer_server_rejected_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(EchoAgentAuth {
        key: "rejected-but-unexpired".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() + Duration::minutes(2)),
        ..EchoAgentAuth::test_default()
    });
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));
    mgr.set_dark_wake_for_test(true);

    assert_eq!(
        mgr.refresh_chain(TokenType::OidcSession, RefreshReason::ServerRejected)
            .await
            .unwrap()
            .key,
        "fresh-token",
        "ServerRejected recovery must reach the IdP even in dark wake"
    );
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

/// A machine stuck reporting a *continuous* dark wake (e.g. an interactive Mac
/// with no display) must not defer refresh forever — once the deferral budget
/// (`DARK_WAKE_DEFER_MAX`) is exhausted, one refresh is forced through. Without
/// this bound the user reaches the same logged-out state the dark-wake guard
/// was added to prevent.
#[tokio::test]
async fn dark_wake_defer_forces_refresh_after_max() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    // Wire-valid but due for renewal, so the deferral path (and its budget) is
    // the thing under test — a hard-expired token is never deferred at all.
    mgr.hot_swap(EchoAgentAuth {
        key: "live-key".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-old".into()),
        expires_at: Some(Utc::now() + Duration::minutes(2)),
        ..EchoAgentAuth::test_default()
    });
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));

    mgr.set_dark_wake_for_test(true);

    // Backdate the start of the deferral run past the bound on both clocks, as
    // if we had been continuously in dark wake longer than DARK_WAKE_DEFER_MAX.
    let back = super::sleep_gate::DARK_WAKE_DEFER_MAX + StdDuration::from_secs(5);
    let (Some(mono), Some(wall)) = (
        Instant::now().checked_sub(back),
        std::time::SystemTime::now().checked_sub(back),
    ) else {
        return; // machine/clock can't represent the backdate — skip
    };
    *mgr.dark_wake_defer_since.write() = Some(crate::util::dual_clock::DualClock { mono, wall });

    assert_eq!(
        mgr.auth().await.unwrap().key,
        "fresh-token",
        "an exhausted dark-wake deferral budget must force the refresh through"
    );
    assert_eq!(
        call_count.load(Ordering::SeqCst),
        1,
        "the IdP refresher must be invoked once the dark-wake defer budget is exhausted"
    );
    assert!(
        mgr.dark_wake_defer_since.read().is_none(),
        "forcing a refresh through must reset the defer budget"
    );
}

/// A `DidWake` (`SYSTEM_HAS_POWERED_ON`) event must not reset the dark-wake
/// defer budget while the system is *still* in a dark wake — macOS can deliver
/// powered-on events for dark wakes, and resetting then would stop the budget
/// from ever exhausting, so the forced refresh would never run. Only a genuine
/// full wake clears it.
#[test]
fn dark_wake_defer_budget_survives_powered_on_during_dark_wake() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    // Begin a deferral run.
    mgr.set_dark_wake_for_test(true);
    assert!(
        mgr.should_defer_for_dark_wake(),
        "a fresh dark wake should defer and start the budget"
    );
    assert!(mgr.dark_wake_defer_since.read().is_some());

    // A powered-on event arrives while still in a dark wake: the budget must
    // persist so it can eventually exhaust and force a refresh through.
    mgr.set_system_sleep_imminent(false);
    assert!(
        mgr.dark_wake_defer_since.read().is_some(),
        "a powered-on event during a dark wake must not reset the defer budget"
    );

    // A genuine full wake clears the run.
    mgr.set_dark_wake_for_test(false);
    mgr.set_system_sleep_imminent(false);
    assert!(
        mgr.dark_wake_defer_since.read().is_none(),
        "a full wake must clear the defer budget"
    );
}

/// The `power_listener_started` guard in `is_dark_wake` must short-circuit to
/// `false` when no OS power listener was started (headless / server), so
/// those processes never treat the OS power state as a dark wake. Exercises the
/// guard directly (no dark-wake override installed).
#[test]
#[serial_test::serial(force_dark_wake_env)] // reads ECHO_AGENT_AUTH_FORCE_DARK_WAKE
fn is_dark_wake_false_when_power_listener_not_started() {
    let _unset = echo_agent_test_support::EnvGuard::unset("ECHO_AGENT_AUTH_FORCE_DARK_WAKE");
    let dir = tempfile::tempdir().unwrap();
    let mgr = AuthManager::new(dir.path(), EchoAgentComConfig::default());
    assert!(
        !mgr.is_dark_wake(),
        "is_dark_wake must be false when the power listener was never started"
    );
}

/// `ECHO_AGENT_AUTH_FORCE_DARK_WAKE` forces the dark-wake answer for manual and
/// integration testing — read BEFORE the `power_listener_started` check,
/// because a headless run never starts the listener and the override
/// exists precisely so such a run can drive the dark-wake paths against a
/// real binary.
#[test]
#[serial_test::serial(force_dark_wake_env)]
fn is_dark_wake_env_override_forces_both_states() {
    use echo_agent_test_support::EnvGuard;
    let dir = tempfile::tempdir().unwrap();
    let mgr = AuthManager::new(dir.path(), EchoAgentComConfig::default());
    // Precondition: no power listener, so without the override this is
    // unconditionally false.
    {
        let _g = EnvGuard::set("ECHO_AGENT_AUTH_FORCE_DARK_WAKE", "1");
        assert!(
            mgr.is_dark_wake(),
            "=1 must force dark wake even without a power listener"
        );
    }
    {
        let _g = EnvGuard::set("ECHO_AGENT_AUTH_FORCE_DARK_WAKE", "0");
        assert!(!mgr.is_dark_wake(), "=0 must force full wake");
    }
    {
        // Unrecognized values fall through to the OS query (listener not
        // started here, so false) rather than picking a state.
        let _g = EnvGuard::set("ECHO_AGENT_AUTH_FORCE_DARK_WAKE", "yes");
        assert!(!mgr.is_dark_wake(), "non-1/0 values must not force a state");
    }
}

#[tokio::test]
async fn sleep_gate_cleared_on_wake_allows_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(expired_oidc());
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(CountingRefresher {
        call_count: call_count.clone(),
        delay: StdDuration::from_millis(0),
    }));

    mgr.set_system_sleep_imminent(true);
    mgr.set_system_sleep_imminent(false); // wake

    let auth = mgr.auth().await.expect("refresh should succeed after wake");
    assert_eq!(auth.key, "fresh-token");
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn sleep_gate_auto_expires_after_max() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    mgr.set_system_sleep_imminent(true);
    assert!(mgr.is_sleep_gated(), "freshly-raised gate must be active");

    // Simulate a missed wake while awake the whole time: both clocks were
    // raised longer ago than the bound.
    let back = super::sleep_gate::SLEEP_GATE_MAX + StdDuration::from_secs(5);
    let (Some(mono), Some(wall)) = (
        Instant::now().checked_sub(back),
        std::time::SystemTime::now().checked_sub(back),
    ) else {
        return; // machine/clock can't represent the backdate — not reproducible; skip
    };
    *mgr.sleep_gate.raised_at.write() = Some(crate::util::dual_clock::DualClock { mono, wall });

    assert!(
        !mgr.is_sleep_gated(),
        "a gate older than SLEEP_GATE_MAX must auto-expire"
    );
    assert!(
        mgr.sleep_gate.raised_at.read().is_none(),
        "auto-expiry must also lower the gate so a stale state can't linger"
    );
}

/// Regression test for the dual-clock backstop: a gate that straddled a real
/// system sleep must auto-expire even though the monotonic clock is still
/// fresh, because the wall clock advanced past the bound during sleep. Before
/// the wall-clock arm this gate stayed shut and an expired token reached the
/// server — the 401 this fix targets.
#[tokio::test]
async fn sleep_gate_auto_expires_when_wall_clock_passes_during_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    mgr.set_system_sleep_imminent(true);
    assert!(mgr.is_sleep_gated(), "freshly-raised gate must be active");

    // Monotonic clock fresh (as if the machine just slept rather than spending
    // the time awake); wall clock pushed past the bound (real time elapsed
    // while asleep, where the monotonic clock is frozen).
    let back = super::sleep_gate::SLEEP_GATE_MAX + StdDuration::from_secs(5);
    let Some(wall) = std::time::SystemTime::now().checked_sub(back) else {
        return; // clock can't represent the backdate — not reproducible; skip
    };
    *mgr.sleep_gate.raised_at.write() = Some(crate::util::dual_clock::DualClock {
        mono: Instant::now(),
        wall,
    });

    assert!(
        !mgr.is_sleep_gated(),
        "a gate whose wall-clock age exceeds SLEEP_GATE_MAX must auto-expire \
         even though the monotonic clock is still fresh"
    );
    assert!(
        mgr.sleep_gate.raised_at.read().is_none(),
        "auto-expiry must also lower the gate so a stale state can't linger"
    );
}

#[tokio::test]
async fn sleep_gate_lets_in_flight_refresh_complete() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(expired_oidc());

    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let call_count = Arc::new(AtomicU32::new(0));
    mgr.set_refresher(Arc::new(BlockingRefresher {
        started: started.clone(),
        release: release.clone(),
        call_count: call_count.clone(),
    }));

    let m = mgr.clone();
    let handle = tokio::spawn(async move { m.auth().await });

    started.notified().await;
    assert_eq!(
        mgr.refresh_in_flight.load(Ordering::SeqCst),
        1,
        "refresh must be counted as in flight while the IdP call is pending"
    );
    // `set_system_sleep_imminent` now holds the OS sleep ack until the
    // in-flight refresh drains. Drive it from a separate thread — as the real
    // OS power-listener thread does — so the tokio runtime stays free to
    // complete the refresh while the hold waits.
    let sleeper = mgr.clone();
    let ack = std::thread::spawn(move || {
        let start = Instant::now();
        sleeper.set_system_sleep_imminent(true);
        start.elapsed()
    });

    release.notify_one();

    let auth = tokio::time::timeout(StdDuration::from_secs(5), handle)
        .await
        .expect("auth() must return")
        .unwrap()
        .expect("in-flight refresh must complete, not abort");
    let ack_waited = ack.join().expect("ack thread panicked");

    assert_eq!(auth.key, "fresh-token");
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
    assert!(mgr.is_sleep_gated(), "WillSleep must raise the sleep gate");
    assert!(
        ack_waited < super::sleep_gate::SLEEP_ACK_MAX_WAIT,
        "the sleep-ack hold must release when the refresh drains, not wait out \
         SLEEP_ACK_MAX_WAIT; waited {ack_waited:?}"
    );
    assert_eq!(
        mgr.refresh_in_flight.load(Ordering::SeqCst),
        0,
        "in-flight counter must be balanced after completion"
    );
}

/// With nothing in flight, the sleep-ack hold must return promptly so the OS
/// suspend is never delayed unnecessarily.
#[test]
fn sleep_ack_hold_returns_immediately_when_nothing_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

    let start = Instant::now();
    mgr.test_hold_sleep_ack(StdDuration::from_secs(5));
    let waited = start.elapsed();

    assert!(
        waited < StdDuration::from_millis(250),
        "no in-flight refresh must not delay the suspend; waited {waited:?}"
    );
}

/// The sleep-ack hold must unblock as soon as the in-flight refresh drains,
/// well before the bound — this is the straddle the fix prevents.
#[test]
fn sleep_ack_hold_releases_when_in_flight_refresh_drains() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.test_enter_refresh_in_flight();

    let releaser = mgr.clone();
    let drain = std::thread::spawn(move || {
        std::thread::sleep(StdDuration::from_millis(120));
        releaser.test_exit_refresh_in_flight();
    });

    let start = Instant::now();
    mgr.test_hold_sleep_ack(StdDuration::from_secs(5));
    let waited = start.elapsed();
    drain.join().unwrap();

    assert!(
        waited >= StdDuration::from_millis(100),
        "must hold the ack until the refresh drains; waited only {waited:?}"
    );
    assert!(
        waited < StdDuration::from_secs(2),
        "must release shortly after the drain, not near the bound; waited {waited:?}"
    );
    assert_eq!(mgr.refresh_in_flight.load(Ordering::SeqCst), 0);
}

/// A refresh that never drains must not pin the machine awake: the hold is
/// bounded and returns at the deadline, leaving the refresh running (never
/// aborted) for the existing straddle telemetry to catch.
#[test]
fn sleep_ack_hold_times_out_when_refresh_never_drains() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.test_enter_refresh_in_flight(); // never exits

    let start = Instant::now();
    mgr.test_hold_sleep_ack(StdDuration::from_millis(150));
    let waited = start.elapsed();

    assert!(
        waited >= StdDuration::from_millis(140),
        "must wait out the bound; waited only {waited:?}"
    );
    assert!(
        waited < StdDuration::from_secs(1),
        "must not exceed the bound by much; waited {waited:?}"
    );
    assert_eq!(
        mgr.refresh_in_flight.load(Ordering::SeqCst),
        1,
        "the refresh is left running, not aborted, when the hold times out"
    );
}

// ── manual_auth KPI ──────────────────────────────────────────

#[test]
fn manual_auth_reason_maps_terminal_and_skips_non_forcing() {
    use crate::auth::error::RefreshTokenFailedReason as Reason;
    use crate::auth::recovery::manual_auth_reason;
    use echo_agent_telemetry::events::ManualAuthReason as R;

    let permanent = |reason: Reason| manual_auth_reason(&AuthError::permanent(reason));
    // A revoked refresh token forces a re-login -> counts.
    assert_eq!(
        permanent(Reason::RefreshTokenRejected),
        Some(R::RefreshTokenRejected)
    );
    // Every terminal pipeline error maps to its own bucket (a swapped mapping
    // would mis-attribute the KPI).
    assert_eq!(
        manual_auth_reason(&AuthError::ServerRejectedNoRecovery),
        Some(R::NoRefreshAuthority)
    );
    assert_eq!(
        manual_auth_reason(&AuthError::RecoveryExhausted),
        Some(R::RecoveryExhausted)
    );
    assert_eq!(
        manual_auth_reason(&AuthError::TokenExpiredNoRefresh),
        Some(R::TokenExpiredNoRefresh)
    );
    assert_eq!(
        manual_auth_reason(&AuthError::PinnedTeamMismatch {
            message: String::new()
        }),
        Some(R::WrongTeam)
    );
    // Before this reason existed these lockouts hid under the self-healing
    // `Other` bucket and never surfaced in the KPI at all.
    assert_eq!(
        permanent(Reason::ProviderInteractiveRequired),
        Some(R::ProviderInteractiveRequired)
    );
    // Self-healing (TTL) reasons, transient / no-credential, and API-key
    // lockouts (out of scope for this KPI) don't count.
    assert_eq!(permanent(Reason::ClientRejected), None);
    assert_eq!(permanent(Reason::Other), None);
    assert_eq!(manual_auth_reason(&AuthError::transient("x")), None);
    assert_eq!(manual_auth_reason(&AuthError::NotLoggedIn), None);
    assert_eq!(manual_auth_reason(&AuthError::ApiKeyAuthDisabled), None);
}

/// Truth table for `relay_should_cancel`: the relay gives up on any terminal
/// auth failure — including `ApiKeyAuthDisabled`, which is deliberately out of
/// the `manual_auth` KPI's scope — and keeps reconnecting through transient
/// blips, absent credentials, and the self-healing permanent reasons (those
/// age out via the TTL, so cancelling on them would orphan a session that
/// recovers minutes later).
#[test]
fn relay_should_cancel_gives_up_only_on_terminal_failures() {
    use crate::auth::error::RefreshTokenFailedReason as Reason;
    use crate::auth::recovery::relay_should_cancel;

    // Terminal: the handshake can't recover; stop reconnecting.
    assert!(relay_should_cancel(&AuthError::permanent(
        Reason::RefreshTokenRejected
    )));
    assert!(relay_should_cancel(&AuthError::ServerRejectedNoRecovery));
    assert!(relay_should_cancel(&AuthError::RecoveryExhausted));
    assert!(relay_should_cancel(&AuthError::TokenExpiredNoRefresh));
    assert!(relay_should_cancel(&AuthError::PinnedTeamMismatch {
        message: String::new()
    }));
    // Cancelled even though it never emits the KPI (a kill-switched API key
    // means rotate the key, not `/login`).
    assert!(relay_should_cancel(&AuthError::ApiKeyAuthDisabled));
    // Reconnecting would replay the same 401 until the user signs in.
    assert!(relay_should_cancel(&AuthError::permanent(
        Reason::ProviderInteractiveRequired
    )));

    // Recoverable: fall through and reconnect.
    assert!(!relay_should_cancel(&AuthError::transient("network blip")));
    assert!(!relay_should_cancel(&AuthError::permanent(
        Reason::ClientRejected
    )));
    assert!(!relay_should_cancel(&AuthError::permanent(Reason::Other)));
    assert!(!relay_should_cancel(&AuthError::NotLoggedIn));
}

// Async so `record` has a runtime for its telemetry `tokio::spawn`: another
// test in the same process can enable the global telemetry client, which would
// otherwise make this emit path panic under a plain `#[test]`.
#[tokio::test]
async fn manual_auth_capture_attributes_and_recorder_debounces() {
    use crate::auth::recovery::{ManualAuthTracker, RejectedAuth};
    use echo_agent_telemetry::events::{AuthTokenKind, ManualAuthSurface};

    let auth = EchoAgentAuth {
        key: "dead-token".into(),
        user_id: "user-1".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt".into()),
        ..EchoAgentAuth::test_default()
    };
    let snap = RejectedAuth::capture(Some(&auth));
    assert_eq!(snap.principal_for_test(), Some("user-1"));
    assert_eq!(snap.token_kind_for_test(), AuthTokenKind::OidcSession);

    let rec = ManualAuthTracker::default();
    let last = || rec.last_token_for_test();
    // Records once; a repeat on the same credential debounces.
    rec.record(
        &snap,
        &AuthError::RecoveryExhausted,
        ManualAuthSurface::Turn,
    );
    let id = last();
    assert!(id.is_some());
    rec.record(
        &snap,
        &AuthError::PinnedTeamMismatch {
            message: String::new(),
        },
        ManualAuthSurface::Turn,
    );
    assert_eq!(last(), id);

    // A different credential re-arms.
    let rearmed = EchoAgentAuth {
        key: "another-token".into(),
        ..auth.clone()
    };
    let fresh = RejectedAuth::capture(Some(&rearmed));
    rec.record(
        &fresh,
        &AuthError::RecoveryExhausted,
        ManualAuthSurface::Turn,
    );
    assert!(last().is_some() && last() != id);

    // A self-healing reason never emits — the KPI counts only forced re-logins.
    let healing = ManualAuthTracker::default();
    healing.record(
        &snap,
        &AuthError::permanent(crate::auth::error::RefreshTokenFailedReason::ClientRejected),
        ManualAuthSurface::Turn,
    );
    assert!(healing.last_token_for_test().is_none());
}

/// End-to-end: `next()` emits only for a user-facing, in-scope terminal failure.
/// A credential with no refresh authority terminates with
/// `ServerRejectedNoRecovery` without a refresher.
#[tokio::test]
async fn manual_auth_emits_only_for_user_facing_source() {
    use crate::auth::recovery::RecoverySource;

    fn mgr_with(dir: &std::path::Path, key: &str, mode: AuthMode) -> Arc<AuthManager> {
        let mgr = Arc::new(AuthManager::new(dir, EchoAgentComConfig::default()));
        let mut auth = make_auth(Some(Utc::now() + Duration::hours(1)), Utc::now());
        auth.user_id = "u1".into();
        auth.key = key.into();
        auth.auth_mode = mode;
        auth.refresh_token = None; // Oidc-sans-refresh-token => LegacySession (in scope)
        mgr.hot_swap(auth);
        // CI runs in K8s pods where is_devbox_environment() is true; without this
        // DevboxRecovery would adopt the seeded valid token and recovery would
        // return Ok instead of the terminal ServerRejectedNoRecovery.
        mgr.set_devbox_env_for_test(false);
        mgr
    }

    // User-facing + in-scope (legacy session) records.
    let d1 = tempfile::tempdir().unwrap();
    let turn = mgr_with(d1.path(), "sess-turn", AuthMode::Oidc);
    let err = turn
        .unauthorized_recovery(turn.current_or_expired(), RecoverySource::Turn)
        .next()
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::ServerRejectedNoRecovery));
    // Assert the emitted payload, not just that something fired.
    use echo_agent_telemetry::events::{
        AuthTokenKind, ManualAuth, ManualAuthReason, ManualAuthSurface,
    };
    assert_eq!(
        turn.manual_auth_last_emit(),
        Some(ManualAuth {
            reason: ManualAuthReason::NoRefreshAuthority,
            trigger: ManualAuthSurface::Turn,
            token_kind: AuthTokenKind::LegacySession,
            principal: Some("u1".to_string()),
        }),
    );

    // Background source does not record.
    let d2 = tempfile::tempdir().unwrap();
    let bg = mgr_with(d2.path(), "sess-bg", AuthMode::Oidc);
    let _ = bg
        .unauthorized_recovery(bg.current_or_expired(), RecoverySource::Background)
        .next()
        .await;
    assert!(bg.manual_auth_last_token().is_none());

    // API-key 401 is out of KPI scope even on a user-facing source.
    let d3 = tempfile::tempdir().unwrap();
    let api = mgr_with(d3.path(), "api-key", AuthMode::ApiKey);
    let _ = api
        .unauthorized_recovery(api.current_or_expired(), RecoverySource::Turn)
        .next()
        .await;
    assert!(api.manual_auth_last_token().is_none());
}

// ── requires_manual_reauth: transient-vs-terminal authority ─────────

/// A refreshable credential with no sticky verdict must NOT demand a manual
/// `/login` — this is the authority the sampler consults before painting the
/// pager's re-auth banner, and the post-wake network gap must classify as
/// self-healing.
#[tokio::test]
async fn requires_manual_reauth_false_for_refreshable_credential() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(EchoAgentAuth {
        key: "expired-at".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-live".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    mgr.set_refresher(Arc::new(FailingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
    }));

    assert!(
        !mgr.requires_manual_reauth(),
        "expired AT with a live RT and no verdict must be self-healing"
    );

    // A recoverable verdict (escalated `Other` — e.g. post-wake blips) still
    // self-heals after its TTL, so it must not demand manual re-auth either.
    record_permanent_failure(&mgr, crate::auth::error::RefreshTokenFailedReason::Other);
    assert!(
        !mgr.requires_manual_reauth(),
        "a recoverable Other verdict must not demand /login"
    );
}

/// A sticky `RefreshTokenRejected` verdict (IdP revoked the RT) is fixable
/// only by `/login`; likewise a manager with no refresh authority at all.
#[tokio::test]
async fn requires_manual_reauth_true_for_sticky_verdict_and_no_refresher() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
    mgr.hot_swap(EchoAgentAuth {
        key: "expired-at".into(),
        auth_mode: AuthMode::Oidc,
        refresh_token: Some("rt-dead".into()),
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });

    // No refresher configured: nothing can silently heal the session.
    assert!(
        mgr.requires_manual_reauth(),
        "no refresh authority must demand /login"
    );

    mgr.set_refresher(Arc::new(FailingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
    }));
    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected,
    );
    assert!(
        mgr.requires_manual_reauth(),
        "a sticky RefreshTokenRejected verdict must demand /login"
    );
}

/// Treating a failed provider run as self-healing is what let an expired
/// credential in and then 401'd every turn. The verdict still ages out, so a
/// later launch gets to retry the provider.
#[tokio::test]
async fn requires_manual_reauth_true_after_external_provider_refresh_failed() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = Arc::new(AuthManager::new(dir.path(), external_provider_config()));
    mgr.hot_swap(EchoAgentAuth {
        key: "expired-external".into(),
        auth_mode: AuthMode::External,
        expires_at: Some(Utc::now() - Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    mgr.set_refresher(Arc::new(FailingRefresher {
        call_count: Arc::new(AtomicU32::new(0)),
    }));

    assert!(
        !mgr.requires_manual_reauth(),
        "before any attempt the provider may still mint silently"
    );

    record_permanent_failure(
        &mgr,
        crate::auth::error::RefreshTokenFailedReason::ProviderInteractiveRequired,
    );
    assert!(
        mgr.requires_manual_reauth(),
        "a failed headless provider run leaves only the interactive flow"
    );

    mgr.force_permanent_failure_aged_out();
    assert!(
        !mgr.requires_manual_reauth(),
        "the verdict is non-sticky: past its TTL the provider gets another chance"
    );
}

/// Config for a deployment that mints sessions with an external binary.
fn external_provider_config() -> EchoAgentComConfig {
    EchoAgentComConfig {
        auth_provider_command: Some("acme-auth".to_owned()),
        ..EchoAgentComConfig::default()
    }
}

// ── proactive_failure_backoff ────────────────────────────────────────

/// The proactive loop's failure backoff: zero before any failure (schedule is
/// purely expiry-driven), grows with consecutive failures, and is capped so a
/// long outage still retries at the regular cadence. Guards against the
/// zero-delay spin that burned the OIDC escalation budget while post-wake
/// Wi-Fi was still associating.
#[test]
fn proactive_failure_backoff_shape() {
    assert_eq!(
        crate::auth::manager::proactive_failure_backoff(0),
        std::time::Duration::ZERO,
        "no failures → no extra delay"
    );
    let b1 = crate::auth::manager::proactive_failure_backoff(1);
    assert!(
        b1 >= std::time::Duration::from_secs(5) && b1 < std::time::Duration::from_secs(9),
        "first failure backs off ~5s (plus jitter), got {b1:?}"
    );
    let b3 = crate::auth::manager::proactive_failure_backoff(3);
    assert!(
        b3 >= std::time::Duration::from_secs(20) && b3 < std::time::Duration::from_secs(24),
        "third failure backs off ~20s (plus jitter), got {b3:?}"
    );
    let huge = crate::auth::manager::proactive_failure_backoff(u32::MAX);
    assert!(
        huge <= BACKOFF_INTERVAL + std::time::Duration::from_secs(3),
        "backoff must cap at BACKOFF_INTERVAL (+jitter), got {huge:?}"
    );
}

// ── try_devbox_recovery: the wait-on-the-lock double-check ───────────

/// Seed a credential that is locally valid but that the caller has been told
/// the server rejects — the shape that made the double-check lie.
fn devbox_manager(dir: &std::path::Path, key: &str) -> Arc<AuthManager> {
    let mgr = Arc::new(AuthManager::new(dir, EchoAgentComConfig::default()));
    mgr.set_devbox_env_for_test(true);
    mgr.hot_swap(EchoAgentAuth {
        key: key.into(),
        auth_mode: AuthMode::External,
        expires_at: Some(Utc::now() + Duration::hours(1)),
        ..EchoAgentAuth::test_default()
    });
    mgr
}

/// The credential the caller already knows is dead can never be the answer.
///
/// `try_devbox_recovery` short-circuits on whatever `current()` holds, to
/// catch a sibling task that refreshed while we waited on `refresh_lock`.
/// Told nothing about the rejected bearer it used to return that bearer, so
/// on a devbox every 401 against a still-locally-valid token reported
/// "recovered" and the turn resubmitted it until its retry budget ran out.
#[tokio::test]
async fn devbox_recovery_never_re_serves_the_credential_it_was_given_up_on() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = devbox_manager(dir.path(), "rejected-but-locally-valid");
    assert!(
        mgr.current().is_some(),
        "precondition: the rejected bearer is still locally valid"
    );

    // Asserted as "not this credential" rather than as an error: on a real
    // devbox the mint can genuinely succeed, and a *different* credential is
    // exactly the outcome we want. Everywhere else there is no mint endpoint
    // and this is an error.
    let outcome = mgr
        .try_devbox_recovery(Some("rejected-but-locally-valid"))
        .await;
    assert!(
        !matches!(&outcome, Ok(auth) if auth.key == "rejected-but-locally-valid"),
        "recovery must not report success with the rejected bearer, got {outcome:?}"
    );
}

/// The double-check still does its job: a credential that is *not* the one
/// the caller gave up on means a sibling task refreshed, so take it and skip
/// the mint.
#[tokio::test]
async fn devbox_recovery_short_circuits_on_a_credential_someone_else_landed() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = devbox_manager(dir.path(), "landed-by-a-sibling-task");

    let auth = mgr
        .try_devbox_recovery(Some("the-bearer-the-server-rejected"))
        .await
        .expect("a different live credential is a recovery");
    assert_eq!(auth.key, "landed-by-a-sibling-task");
}
