//! The is-managed claim in the gate decision and the staleness refetch
//! (sidecar-removal downgrade closure).

use super::super::*;
use super::team;

/// Headline: a stripped policy sidecar + imposing claim refuses even over a fully
/// forged permissive marker; without the claim that state is the pre-fix downgrade.
#[test]
fn claim_refuses_stripped_sidecar_even_with_forged_marker() {
    use crate::signed_policy::SignedVerdict;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    // The forged-marker shape: permissive, nothing served, matching principal.
    let forged = ManagedConfigCache {
        principal: Some("team-007".into()),
        fail_closed: false,
        ..Default::default()
    };
    assert!(
        managed_policy_compromised_decision(
            SignedVerdict::NoAuthenticSidecar,
            || true,
            false,
            Some(&forged),
            home,
            &team("team-007")
        ),
        "an imposing claim outranks the forged marker when the policy sidecar is gone"
    );
    assert!(
        !managed_policy_compromised_decision(
            SignedVerdict::NoAuthenticSidecar,
            || false,
            false,
            Some(&forged),
            home,
            &team("team-007")
        ),
        "without the claim this exact state is the (documented) marker downgrade"
    );
}

/// A policy-sidecar read blip stays lenient: the claim is not consulted on
/// `SidecarUnreadable` (rationale on the variant doc).
#[test]
fn claim_not_consulted_on_sidecar_read_blip() {
    use crate::signed_policy::SignedVerdict;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::write(home.join("requirements.toml"), "[features]\n").unwrap();
    let served = ManagedConfigCache {
        principal: Some("team-007".into()),
        had_requirements: true,
        fail_closed: true,
        ..Default::default()
    };
    assert!(
        !managed_policy_compromised_decision(
            SignedVerdict::SidecarUnreadable,
            || true,
            false,
            Some(&served),
            home,
            &team("team-007")
        ),
        "a transient sidecar read blip must not refuse, claim or no claim"
    );
}

/// Armed: garbage claim alone (no fail-closed) does not trip gate or force refetch.
#[test]
fn garbage_claim_without_fail_closed_is_not_imposing() {
    assert!(crate::signed_policy::verification_active());
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    mark_managed_config_synced_at(
        home,
        SyncMarker {
            principal: Some("team-a"),
            had_managed_config: false,
            had_requirements: false,
            key_fingerprint: None,
            fail_closed: false,
        },
    );
    std::fs::write(
        home.join(crate::signed_policy::MANAGED_IDENTITY_SIDECAR_FILE),
        "{\"signed_payload\":\"{}\",\"signature\":\"\",\"key_id\":\"\"}",
    )
    .unwrap();
    assert!(
        !managed_policy_compromised_for_at(home, &team("team-a")),
        "garbage claim without fail-closed must not make the gate fail closed"
    );
    assert!(
        !is_managed_config_hard_stale_for_at(home, &team("team-a")),
        "garbage claim without fail-closed must not force a refetch"
    );
}

/// Keyless: claim file does not affect gate or staleness.
#[test]
fn claim_paths_are_inert_in_dark_build() {
    crate::signed_policy::test_seam::with_dark(|| {
        assert!(!crate::signed_policy::verification_active());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        mark_managed_config_synced_at(
            home,
            SyncMarker {
                principal: Some("team-a"),
                had_managed_config: false,
                had_requirements: false,
                key_fingerprint: None,
                fail_closed: false,
            },
        );
        std::fs::write(
            home.join(crate::signed_policy::MANAGED_IDENTITY_SIDECAR_FILE),
            r#"{"signed_payload":"{}","signature":"","key_id":""}"#,
        )
        .unwrap();
        assert!(
            !managed_policy_compromised_for_at(home, &team("team-a")),
            "dark build: a claim file must not make the gate fail closed"
        );
        assert!(
            !is_managed_config_hard_stale_for_at(home, &team("team-a")),
            "dark build: a claim file must not force a refetch"
        );
    });
}
