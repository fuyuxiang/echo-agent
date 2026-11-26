use super::*;
use crate::test_util::EchoAgentHomeFixture;
use clap::Parser;

/// In-memory `Summary` via serde: every field without `#[serde(default)]`
/// must be present, and a struct literal would break on each new field.
fn summary(id: &str, title: Option<&str>, manual: bool) -> Summary {
    serde_json::from_value(serde_json::json!({
        "info": { "id": id, "cwd": "/ws" },
        "session_summary": "auto summary",
        "created_at": "2026-07-01T00:00:00Z",
        "updated_at": "2026-07-01T00:00:00Z",
        "num_messages": 1,
        "current_model_id": "echo-agent-build",
        "generated_title": title,
        "title_is_manual": manual,
    }))
    .expect("valid Summary JSON")
}

fn id_of(s: Option<&Summary>) -> String {
    s.expect("expected a selected summary").info.id.to_string()
}

#[test]
fn blank_or_unmatched_arg_selects_none() {
    let s = [summary("a", Some("Fix Login"), false)];
    assert!(select_by_title("nope", &s).unwrap().is_none());
    assert!(select_by_title("   ", &s).unwrap().is_none());
}

#[test]
fn single_match_is_case_insensitive_and_trimmed() {
    let s = [
        summary("a", Some("Fix Login Bug"), false),
        summary("b", Some("Other"), false),
    ];
    assert_eq!(id_of(select_by_title("  FIX login bug ", &s).unwrap()), "a");
}

/// The contract is a simple lowercase comparison: accented letters match
/// across case, but one-to-many case folds do not (`to_lowercase` maps
/// U+00DF to itself, so "STRASSE" never equals a stored "straße").
#[test]
fn non_ascii_case_matching_contract() {
    let s = [summary("a", Some("Café Löschen"), false)];
    assert_eq!(id_of(select_by_title("CAFÉ LÖSCHEN", &s).unwrap()), "a");
    let sharp = [summary("b", Some("straße"), false)];
    assert!(select_by_title("STRASSE", &sharp).unwrap().is_none());
}

#[test]
fn duplicate_auto_titles_error_lists_ids_with_escaped_titles() {
    // A title with a newline would corrupt the one-match-per-line listing if
    // rendered raw.
    let s = [
        summary("id-a", Some("Dup\nTitle"), false),
        summary("id-b", Some("Dup\nTitle"), false),
    ];
    let msg = select_by_title("dup\ntitle", &s).unwrap_err().to_string();
    assert!(
        msg.contains("id-a") && msg.contains("id-b"),
        "both ids must be listed: {msg}"
    );
    assert!(
        msg.contains("Dup\\nTitle"),
        "titles must be Debug-escaped: {msg}"
    );
}

#[test]
fn sole_manual_rename_wins_among_duplicates() {
    let s = [
        summary("auto1", Some("Dup"), false),
        summary("man1", Some("Dup"), true),
        summary("auto2", Some("Dup"), false),
    ];
    assert_eq!(id_of(select_by_title("dup", &s).unwrap()), "man1");
}

#[test]
fn pulled_sole_manual_summary_wins_among_duplicate_autos() {
    // Shape pull writes: generated_title + title_is_manual, session_summary
    // matching the remote title (hop contract for `--resume <title>`).
    let pulled = summary("pulled-hop", Some("Dup"), true);
    assert_eq!(pulled.manual_title_opt().as_deref(), Some("Dup"));
    let s = [
        summary("auto1", Some("Dup"), false),
        pulled,
        summary("auto2", Some("Dup"), false),
    ];
    assert_eq!(id_of(select_by_title("dup", &s).unwrap()), "pulled-hop");
}

#[test]
fn two_manual_renames_stay_ambiguous() {
    let s = [
        summary("man1", Some("Dup"), true),
        summary("man2", Some("Dup"), true),
    ];
    let msg = select_by_title("Dup", &s).unwrap_err().to_string();
    assert!(
        msg.contains("man1") && msg.contains("man2"),
        "both manual ids must be listed: {msg}"
    );
}

#[test]
fn uuid_shaped_arg_never_matches_titles() {
    let uuid = "12345678-1234-1234-1234-123456789abc";
    let s = [summary("a", Some(uuid), true)];
    assert!(select_by_title(uuid, &s).unwrap().is_none());
}

#[test]
fn title_miss_hint_escapes_arg_and_suggests_search() {
    let hint = title_miss_hint("evil\ntitle");
    assert!(hint.contains("evil\\ntitle"), "arg must be escaped: {hint}");
    assert!(
        hint.contains("echoagent sessions search"),
        "missing hint: {hint}"
    );
}

/// The worktree defer drops the local zero-match context; the failure
/// message restores it only for a threaded deferred-miss target. A resolved
/// legacy non-UUID id (no threaded miss) must not get a false no-match hint.
#[test]
fn worktree_failure_message_hint_follows_threaded_provenance() {
    let msg = worktree_resume_failure_message(Some("typo title"), "restore failed");
    assert!(msg.contains("couldn't resume worktree session: restore failed"));
    assert!(msg.contains("no session id or title matched"), "{msg}");
    assert!(msg.contains("echoagent sessions search"), "{msg}");
    let resolved_msg = worktree_resume_failure_message(None, "restore failed");
    assert_eq!(
        resolved_msg,
        "couldn't resume worktree session: restore failed"
    );
}

/// Regression (production wiring): pinning rewrites the `-r` title to the
/// canonical id, the profile peek sees the saved profile, and a conflicting
/// explicit profile is refused exactly like id resume.
#[serial_test::serial(ECHO_AGENT_HOME)]
#[test]
fn pin_title_resume_finds_saved_profile_and_conflicts() {
    let mut fx = EchoAgentHomeFixture::new();
    let cwd_str = fx.cwd_str();
    let id = "abcdabcd-1111-2222-3333-444444444444";
    fx.write_summary(
        &cwd_str,
        id,
        serde_json::json!({
            "generated_title": "Locked Down",
            "title_is_manual": true,
            "sandbox_profile": "strict",
        }),
    );
    let mut args = crate::app::cli::PagerArgs::try_parse_from([
        "echoagent",
        "-r",
        "locked down",
        "--sandbox",
        "off",
    ])
    .unwrap();
    args.pin_local_resume_target_for_cwd(Some(&cwd_str))
        .unwrap();
    assert_eq!(args.session_to_resume(), Some(id));

    let saved = args.saved_resume_profile_for_cwd(Some(&cwd_str));
    assert_eq!(saved.as_deref(), Some("strict"));
    match args.startup_sandbox_profile(saved.as_deref()) {
        crate::app::cli::SandboxStartup::Conflict { requested, saved } => {
            assert_eq!(requested, "off");
            assert_eq!(saved, "strict");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
}

/// Regression: a non-UUID remote id with a restored local child pins to the
/// child, so the peek reads the child's profile instead of an exact same-id
/// session in another cwd.
#[serial_test::serial(ECHO_AGENT_HOME)]
#[test]
fn pin_prefers_restored_child_over_same_id_in_other_cwd() {
    let mut fx = EchoAgentHomeFixture::new();
    let cwd_str = fx.cwd_str();
    let child = "cafecafe-1111-2222-3333-444444444444";
    fx.write_summary(
        &cwd_str,
        child,
        serde_json::json!({
            "parent_session_id": "legacy-remote-7",
            "sandbox_profile": "strict",
        }),
    );
    let other_cwd = tempfile::tempdir().expect("other cwd tempdir");
    let other_str = other_cwd.path().to_string_lossy().to_string();
    fx.write_summary(
        &other_str,
        "legacy-remote-7",
        serde_json::json!({ "sandbox_profile": "off" }),
    );

    let mut args =
        crate::app::cli::PagerArgs::try_parse_from(["echoagent", "-r", "legacy-remote-7"]).unwrap();
    args.pin_local_resume_target_for_cwd(Some(&cwd_str))
        .unwrap();
    assert_eq!(args.session_to_resume(), Some(child));
    assert_eq!(
        args.saved_resume_profile_for_cwd(Some(&cwd_str)).as_deref(),
        Some("strict")
    );
}

/// Regression: materialization consumes the pinned id via the ordinary id
/// path. A rename/create between the pre-sandbox pin and materialization
/// must not re-select by title.
#[serial_test::serial(ECHO_AGENT_HOME)]
#[tokio::test]
async fn materialization_consumes_pinned_id_after_concurrent_rename() {
    let mut fx = EchoAgentHomeFixture::new();
    let cwd_str = fx.cwd_str();
    let pinned = "dadadada-1111-2222-3333-444444444444";
    fx.write_summary(
        &cwd_str,
        pinned,
        serde_json::json!({ "generated_title": "Alpha", "title_is_manual": true }),
    );

    let mut args =
        crate::app::cli::PagerArgs::try_parse_from(["echoagent", "-r", "alpha"]).unwrap();
    args.pin_local_resume_target_for_cwd(Some(&cwd_str))
        .unwrap();
    assert_eq!(args.session_to_resume(), Some(pinned));

    // Concurrent rename/create after the pin: the pinned session loses the
    // title and a decoy gains it.
    fx.write_summary(
        &cwd_str,
        pinned,
        serde_json::json!({ "generated_title": "Beta", "title_is_manual": true }),
    );
    fx.write_summary(
        &cwd_str,
        "dadadada-1111-2222-3333-555555555555",
        serde_json::json!({ "generated_title": "Alpha", "title_is_manual": true }),
    );

    use crate::app::session_startup::{MaterializedStartup, materialize_startup_for_cwd};
    let intent = args.session_startup_intent().unwrap();
    let out = materialize_startup_for_cwd(pinned_local_ctx(), intent, &cwd_str)
        .await
        .unwrap();
    match out {
        MaterializedStartup::Resume { session_id, .. } => assert_eq!(session_id, pinned),
        other => panic!("expected Resume, got {other:?}"),
    }
}

/// Local-only ctx carrying the composition root's pin outcome
/// (`resume_target_pinned` maps to `PinnedPreSandbox` in production).
fn pinned_local_ctx() -> crate::app::session_startup::MaterializeCtx {
    crate::app::session_startup::MaterializeCtx {
        has_worktree: false,
        allow_remote_restore: false,
        chat_mode: false,
        title_resolution: crate::app::session_startup::TitleResolution::PinnedPreSandbox,
        restore_code: false,
        restore_progress_on_stdout: false,
    }
}

/// Regression: an ambiguous title now fails at the pin, before the
/// irreversible sandbox, instead of deferring to materialization.
#[serial_test::serial(ECHO_AGENT_HOME)]
#[test]
fn pin_ambiguous_title_errors_before_sandbox() {
    let mut fx = EchoAgentHomeFixture::new();
    let cwd_str = fx.cwd_str();
    fx.write_summary(
        &cwd_str,
        "e0e0e0e0-1111-2222-3333-444444444444",
        serde_json::json!({ "generated_title": "Dup" }),
    );
    fx.write_summary(
        &cwd_str,
        "e0e0e0e0-1111-2222-3333-555555555555",
        serde_json::json!({ "generated_title": "Dup" }),
    );

    let mut args = crate::app::cli::PagerArgs::try_parse_from(["echoagent", "-r", "Dup"]).unwrap();
    let msg = args
        .pin_local_resume_target_for_cwd(Some(&cwd_str))
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("Multiple sessions match title"),
        "unexpected message: {msg}"
    );
}

/// Regression: a definitive pre-sandbox no-match must not be re-selected by
/// title at materialization — a session created/renamed into the title after
/// the sandbox would resume under an unverified profile.
#[serial_test::serial(ECHO_AGENT_HOME)]
#[tokio::test]
async fn pinned_no_match_does_not_retry_title_after_sandbox() {
    let mut fx = EchoAgentHomeFixture::new();
    let cwd_str = fx.cwd_str();

    let mut args =
        crate::app::cli::PagerArgs::try_parse_from(["echoagent", "-r", "ghost"]).unwrap();
    args.pin_local_resume_target_for_cwd(Some(&cwd_str))
        .unwrap();
    assert!(args.resume_target_pinned);
    assert_eq!(args.session_to_resume(), Some("ghost"));

    // The title appears only after the pin (and the sandbox).
    let late = "f0f0f0f0-1111-2222-3333-444444444444";
    fx.write_summary(
        &cwd_str,
        late,
        serde_json::json!({ "generated_title": "ghost", "title_is_manual": true }),
    );

    use crate::app::session_startup::{
        MaterializeCtx, TitleResolution, materialize_startup_for_cwd,
    };
    let intent = args.session_startup_intent().unwrap();
    let msg = materialize_startup_for_cwd(pinned_local_ctx(), intent.clone(), &cwd_str)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("no session id or title matched"),
        "must not resume the late title match: {msg}"
    );
    // Contrast: an unpinned caller (no pre-sandbox pin ran) may still select
    // the title — the gate, not the data, decides.
    let allowed_ctx = MaterializeCtx {
        title_resolution: TitleResolution::Allowed,
        ..pinned_local_ctx()
    };
    let out = materialize_startup_for_cwd(allowed_ctx, intent, &cwd_str)
        .await
        .unwrap();
    match out {
        crate::app::session_startup::MaterializedStartup::Resume { session_id, .. } => {
            assert_eq!(session_id, late);
        }
        other => panic!("expected Resume, got {other:?}"),
    }
}

/// Regression: a pinned non-UUID id that vanishes before materialization
/// must not be reinterpreted as another session's title.
#[serial_test::serial(ECHO_AGENT_HOME)]
#[tokio::test]
async fn pinned_non_uuid_id_is_not_reinterpreted_as_title() {
    let mut fx = EchoAgentHomeFixture::new();
    let cwd_str = fx.cwd_str();
    fx.write_summary(&cwd_str, "legacy-remote-7", serde_json::json!({}));

    let mut args =
        crate::app::cli::PagerArgs::try_parse_from(["echoagent", "-r", "legacy-remote-7"]).unwrap();
    args.pin_local_resume_target_for_cwd(Some(&cwd_str))
        .unwrap();
    assert!(args.resume_target_pinned);
    assert_eq!(args.session_to_resume(), Some("legacy-remote-7"));

    // The pinned id vanishes; a decoy session gains it as a title.
    fx.remove_session(&cwd_str, "legacy-remote-7");
    fx.write_summary(
        &cwd_str,
        "f1f1f1f1-1111-2222-3333-444444444444",
        serde_json::json!({ "generated_title": "legacy-remote-7", "title_is_manual": true }),
    );

    use crate::app::session_startup::materialize_startup_for_cwd;
    let intent = args.session_startup_intent().unwrap();
    let msg = materialize_startup_for_cwd(pinned_local_ctx(), intent, &cwd_str)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("no session id or title matched"),
        "must not resume the decoy titled with the pinned id: {msg}"
    );
}

/// Regression: a legacy id duplicated across cwd dirs is ambiguous to the
/// session listings (`RelocationView::select` drops multi-path journal-less
/// ids before the cwd filter), so its title never reaches selection: the pin
/// stays unresolved, the profile peek finds nothing, and materialization
/// fails closed with the hint instead of resuming under an unverified
/// profile. The carried-profile path for unique ids is pinned by
/// `pin_title_resume_finds_saved_profile_and_conflicts`.
#[serial_test::serial(ECHO_AGENT_HOME)]
#[tokio::test]
async fn duplicate_legacy_id_is_not_title_addressable() {
    let mut fx = EchoAgentHomeFixture::new();
    let cwd_str = fx.cwd_str();
    fx.write_summary(
        &cwd_str,
        "legacy-twin",
        serde_json::json!({
            "generated_title": "Locked Down",
            "title_is_manual": true,
            "sandbox_profile": "strict",
        }),
    );
    let other_cwd = tempfile::tempdir().expect("other cwd tempdir");
    let other_str = other_cwd.path().to_string_lossy().to_string();
    fx.write_summary(
        &other_str,
        "legacy-twin",
        serde_json::json!({ "sandbox_profile": "off" }),
    );

    let mut args =
        crate::app::cli::PagerArgs::try_parse_from(["echoagent", "-r", "locked down"]).unwrap();
    args.pin_local_resume_target_for_cwd(Some(&cwd_str))
        .unwrap();
    assert!(args.resume_target_pinned);
    assert_eq!(args.session_to_resume(), Some("locked down"));
    assert!(args.saved_resume_profile_for_cwd(Some(&cwd_str)).is_none());

    use crate::app::session_startup::{
        MaterializeCtx, TitleResolution, materialize_startup_for_cwd,
    };
    let ctx = MaterializeCtx::from_pager_args(&args);
    assert_eq!(ctx.title_resolution, TitleResolution::PinnedPreSandbox);

    let intent = args.session_startup_intent().unwrap();
    let msg = materialize_startup_for_cwd(pinned_local_ctx(), intent, &cwd_str)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("no session id or title matched"),
        "duplicate-id session must fail closed, not resume: {msg}"
    );
}
