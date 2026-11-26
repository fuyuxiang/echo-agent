//! The server-signed is-managed claim: verifiers, domain separation, and the
//! impose/defer signal (sidecar-removal downgrade closure).

use super::super::*;
use super::{keyset, payload, sign, test_keypair};

fn claim(principal: &str, fail_closed: bool, expires_at: u64) -> ManagedIdentityClaim {
    ManagedIdentityClaim {
        typ: MANAGED_IDENTITY_TYP.into(),
        principal: principal.into(),
        fail_closed,
        expires_at,
        key_id: "v1".into(),
    }
}

fn sign_claim(
    kp: &ring::signature::Ed25519KeyPair,
    claim: &ManagedIdentityClaim,
) -> SignatureEnvelope {
    let signed_payload = serde_json::to_string(claim).unwrap();
    let sig = kp.sign(signed_payload.as_bytes());
    SignatureEnvelope {
        signed_payload,
        signature: base64::engine::general_purpose::STANDARD.encode(sig.as_ref()),
        key_id: claim.key_id.clone(),
    }
}

fn write_claim(home: &std::path::Path, sidecar: &SignatureEnvelope) {
    write_managed_identity_sidecar(home, sidecar).unwrap();
}

/// The required `typ` closes signature confusion: neither message type substitutes
/// for the other, even genuinely signed by the same key.
#[test]
fn domain_separation_rejects_cross_type_substitution() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let keys = keyset("v1", &pubkey);

    // An authentic identity claim must NOT verify as a policy payload.
    let claim_sidecar = sign_claim(&kp, &claim("team-007", true, 4_000_000_000));
    assert_eq!(
        verify_signed_payload(
            &claim_sidecar.signed_payload,
            &claim_sidecar.signature,
            &keys
        ),
        Err(SigError::WrongType),
        "an identity claim must be rejected by the policy verifier"
    );

    // End-to-end: the authentic claim copied over the policy sidecar (policy files
    // deleted) must read NoAuthenticSidecar, never Trusted — the pre-fix exploit
    // started such a fail_closed principal unmanaged.
    std::fs::write(
        sidecar_path(home),
        serde_json::to_string(&claim_sidecar).unwrap(),
    )
    .unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keys, Some("team-007"), 1_000),
        SignedVerdict::NoAuthenticSidecar,
        "a substituted claim is not an authentic policy verdict"
    );

    // Reverse: a policy envelope must not verify as a claim (its shape has no
    // `principal`, so it fails at parse).
    let policy_sidecar = sign(&kp, &payload());
    assert_eq!(
        verify_managed_identity_claim(
            &policy_sidecar.signed_payload,
            &policy_sidecar.signature,
            &keys
        ),
        Err(SigError::BadPayload),
        "a policy envelope must be rejected by the claim verifier"
    );

    // And a claim-shaped blob carrying the POLICY tag trips the typ guard itself.
    let mut wrong_typ = claim("team-007", true, 4_000_000_000);
    wrong_typ.typ = MANAGED_POLICY_TYP.into();
    let bad = sign_claim(&kp, &wrong_typ);
    assert_eq!(
        verify_managed_identity_claim(&bad.signed_payload, &bad.signature, &keys),
        Err(SigError::WrongType)
    );
}

/// The claim imposes ONLY when authentic + bound + fail_closed; permissive,
/// foreign, unknown-principal, forged, and absent claims are all silent.
#[test]
fn claim_imposes_only_for_bound_fail_closed_claim() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let keys = keyset("v1", &pubkey);

    // Absent → silent.
    assert!(!managed_identity_claim_imposes_with_keys(
        home,
        &keys,
        Some("team-007"),
        1_000
    ));

    write_claim(
        home,
        &sign_claim(&kp, &claim("team-007", true, 4_000_000_000)),
    );
    assert!(
        managed_identity_claim_imposes_with_keys(home, &keys, Some("team-007"), 1_000),
        "an authentic bound fail_closed claim imposes"
    );
    assert!(
        !managed_identity_claim_imposes_with_keys(home, &keys, Some("team-evil"), 1_000),
        "a claim for another principal must not bind us"
    );
    assert!(
        !managed_identity_claim_imposes_with_keys(home, &keys, None, 1_000),
        "an unbindable claim must not gate startup"
    );

    write_claim(
        home,
        &sign_claim(&kp, &claim("team-007", false, 4_000_000_000)),
    );
    assert!(
        !managed_identity_claim_imposes_with_keys(home, &keys, Some("team-007"), 1_000),
        "a permissive claim defers to the marker"
    );

    let mut forged = sign_claim(&kp, &claim("team-007", true, 4_000_000_000));
    forged.signature = base64::engine::general_purpose::STANDARD.encode([0u8; 64]);
    write_claim(home, &forged);
    assert!(
        !managed_identity_claim_imposes_with_keys(home, &keys, Some("team-007"), 1_000),
        "a forged claim imposes nothing"
    );
}

/// An expired claim is silent (callers pass the floor-clamped now, so a rolled-back
/// clock cannot un-expire it).
#[test]
fn expired_claim_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let keys = keyset("v1", &pubkey);
    write_claim(home, &sign_claim(&kp, &claim("team-007", true, 2_000)));

    assert!(managed_identity_claim_imposes_with_keys(
        home,
        &keys,
        Some("team-007"),
        1_000
    ));
    assert!(
        !managed_identity_claim_imposes_with_keys(home, &keys, Some("team-007"), 3_000),
        "past expiry → silent"
    );
}

/// Fetch-time claim verification enforces expiry (the persist gate).
#[test]
fn verify_fetched_claim_rejects_expired() {
    let (kp, pubkey) = test_keypair();
    let keys = keyset("v1", &pubkey);
    let sidecar = sign_claim(&kp, &claim("team-007", true, 2_000));
    assert!(verify_fetched_claim_with_keys(&sidecar, &keys, 1_000).is_ok());
    assert_eq!(
        verify_fetched_claim_with_keys(&sidecar, &keys, 3_000),
        Err(SigError::Expired)
    );
}

/// Corrupt claim bytes read as Absent (not Present): impose is silent, never
/// refuses on garbage — same read_envelope_at path as the policy sidecar.
#[test]
fn corrupt_claim_file_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let keys = keyset("v1", &pubkey);
    std::fs::write(managed_identity_sidecar_path(home), "{not-json").unwrap();
    assert!(
        !managed_identity_claim_imposes_with_keys(home, &keys, Some("team-007"), 1_000),
        "unparseable claim bytes must not impose"
    );
    // A half-shaped envelope (missing signature fields) is the same Absent path.
    std::fs::write(managed_identity_sidecar_path(home), "{\"key_id\":\"v1\"}").unwrap();
    assert!(!managed_identity_claim_imposes_with_keys(
        home,
        &keys,
        Some("team-007"),
        1_000
    ));
    // Sanity: a real claim at the same path still imposes.
    write_claim(
        home,
        &sign_claim(&kp, &claim("team-007", true, 4_000_000_000)),
    );
    assert!(managed_identity_claim_imposes_with_keys(
        home,
        &keys,
        Some("team-007"),
        1_000
    ));
}

/// A directory squatting the claim slot is Absent (non-regular), never an
/// imposing claim — and never the lenient Unreadable blip (that is EACCES on a
/// regular file only).
#[test]
fn directory_squatting_claim_slot_is_silent() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (_kp, pubkey) = test_keypair();
    let keys = keyset("v1", &pubkey);
    let path = managed_identity_sidecar_path(home);
    std::fs::create_dir(&path).unwrap();
    assert!(
        !managed_identity_claim_imposes_with_keys(home, &keys, Some("team-007"), 1_000),
        "a directory at the claim path must not impose"
    );
}
