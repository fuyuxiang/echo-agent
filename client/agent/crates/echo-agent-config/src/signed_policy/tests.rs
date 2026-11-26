use super::*;
use ring::signature::KeyPair;

fn test_keypair() -> (ring::signature::Ed25519KeyPair, Vec<u8>) {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).unwrap();
    let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
    let pubkey = kp.public_key().as_ref().to_vec();
    (kp, pubkey)
}

fn keyset<'a>(id: &'a str, pubkey: &'a [u8]) -> Vec<(&'a str, &'a [u8])> {
    vec![(id, pubkey)]
}

fn sign(kp: &ring::signature::Ed25519KeyPair, payload: &SignedPayload) -> SignatureEnvelope {
    let signed_payload = serde_json::to_string(payload).unwrap();
    let sig = kp.sign(signed_payload.as_bytes());
    SignatureEnvelope {
        signed_payload,
        signature: base64::engine::general_purpose::STANDARD.encode(sig.as_ref()),
        key_id: payload.key_id.clone(),
    }
}

fn payload() -> SignedPayload {
    SignedPayload {
        typ: MANAGED_POLICY_TYP.into(),
        version: 1,
        deployment_id: None,
        team_id: Some("team-007".into()),
        managed_config: Some("[cli]\ntheme = \"dark\"\n".into()),
        requirements: Some("[features]\nweb_fetch = false\n".into()),
        fail_closed: false,
        expires_at: 4_000_000_000,
        nonce: String::new(),
        key_id: "v1".into(),
    }
}

fn write_policy(home: &std::path::Path, p: &SignedPayload) {
    std::fs::write(
        home.join("managed_config.toml"),
        p.managed_config.as_ref().unwrap(),
    )
    .unwrap();
    std::fs::write(
        home.join("requirements.toml"),
        p.requirements.as_ref().unwrap(),
    )
    .unwrap();
}

/// Pins the wire contract: a raw server-shaped JSON payload must verify and parse here.
#[test]
fn server_wire_format_is_client_verifiable() {
    let (kp, pubkey) = test_keypair();
    let signed_payload = serde_json::json!({
        "typ": "echoagent.managed_policy.v1",
        "deployment_id": serde_json::Value::Null,
        "team_id": "team-007",
        "managed_config": "[cli]\n",
        "requirements": "[features]\n",
        "fail_closed": true,
        "expires_at": 4_000_000_000u64,
        "key_id": "v1",
    })
    .to_string();
    let sig = kp.sign(signed_payload.as_bytes());
    let sidecar = SignatureEnvelope {
        signed_payload,
        signature: base64::engine::general_purpose::STANDARD.encode(sig.as_ref()),
        key_id: "v1".into(),
    };
    let payload =
        verify_fetched_with_keys(&sidecar, &keyset("v1", &pubkey), Some("team-007"), 1_000)
            .expect("server format must verify");
    assert_eq!(payload.team_id.as_deref(), Some("team-007"));
    assert_eq!(payload.requirements.as_deref(), Some("[features]\n"));
    assert!(payload.fail_closed);
}

/// A payload missing `fail_closed` (an older server) parses lenient — the field is additive.
#[test]
fn missing_fail_closed_defaults_false() {
    let (kp, pubkey) = test_keypair();
    let signed_payload = serde_json::json!({
        "typ": "echoagent.managed_policy.v1",
        "team_id": "team-007",
        "expires_at": 4_000_000_000u64,
        "key_id": "v1",
    })
    .to_string();
    let sig = kp.sign(signed_payload.as_bytes());
    let sidecar = SignatureEnvelope {
        signed_payload,
        signature: base64::engine::general_purpose::STANDARD.encode(sig.as_ref()),
        key_id: "v1".into(),
    };
    let payload =
        verify_fetched_with_keys(&sidecar, &keyset("v1", &pubkey), Some("team-007"), 1_000)
            .expect("must verify");
    assert!(!payload.fail_closed);
}

#[test]
fn valid_signature_round_trips() {
    let (kp, pubkey) = test_keypair();
    let sidecar = sign(&kp, &payload());
    let out = verify_signed_payload(
        &sidecar.signed_payload,
        &sidecar.signature,
        &keyset("v1", &pubkey),
    )
    .expect("valid signature must verify");
    assert_eq!(out, payload());
}

#[test]
fn tampered_payload_fails() {
    let (kp, pubkey) = test_keypair();
    let mut sidecar = sign(&kp, &payload());
    // Flip a byte in the signed payload (an attacker editing the policy).
    sidecar.signed_payload = sidecar.signed_payload.replace("dark", "evil");
    assert_eq!(
        verify_signed_payload(
            &sidecar.signed_payload,
            &sidecar.signature,
            &keyset("v1", &pubkey)
        ),
        Err(SigError::SignatureMismatch)
    );
}

#[test]
fn wrong_key_fails() {
    let (kp, _) = test_keypair();
    let (_, other_pubkey) = test_keypair();
    let sidecar = sign(&kp, &payload());
    assert_eq!(
        verify_signed_payload(
            &sidecar.signed_payload,
            &sidecar.signature,
            &keyset("v1", &other_pubkey)
        ),
        Err(SigError::SignatureMismatch)
    );
}

/// Pins where the fetch binding ([`check_fetch_identity`], expiry enforced, deployment
/// trusted on signature alone) diverges from the at-rest rule
/// ([`signed_principal_matches`], strict effective-principal equality, expiry-free).
#[test]
fn binding_rejects_other_team_and_expiry() {
    let p = payload();
    assert_eq!(
        check_fetch_identity(&p, Some("team-evil"), 1_000),
        Err(SigError::PrincipalMismatch)
    );
    assert_eq!(
        check_fetch_identity(&p, Some("team-007"), p.expires_at + 1),
        Err(SigError::Expired)
    );
    assert!(check_fetch_identity(&p, Some("team-007"), 1_000).is_ok());
    // No resolvable active team is lenient (an auth.json read blip must not brick a session).
    assert!(check_fetch_identity(&p, None, 1_000).is_ok());
    // A DEPLOYMENT-signed policy is accepted even when the active team differs.
    let dep = SignedPayload {
        deployment_id: Some("dep-1".into()),
        team_id: Some("team-other".into()),
        ..payload()
    };
    assert!(check_fetch_identity(&dep, Some("team-007"), 1_000).is_ok());
    // The at-rest rule instead requires the effective principal (its deployment_id)...
    assert!(signed_principal_matches(&dep, Some("dep-1")));
    assert!(!signed_principal_matches(&dep, Some("team-007")));
    // ...and is expiry-free: an expired payload still matches, while the fetch binding rejects it.
    let expired = SignedPayload {
        expires_at: 10,
        ..payload()
    };
    assert!(signed_principal_matches(&expired, Some("team-007")));
    assert_eq!(
        check_fetch_identity(&expired, Some("team-007"), 1_000),
        Err(SigError::Expired)
    );
}

#[test]
fn on_disk_content_must_match_signed() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let p = payload();
    write_policy(home, &p);
    assert!(check_on_disk_matches(home, &p).is_ok());

    // Editing the enforced file is caught even though it still exists.
    std::fs::write(
        home.join("requirements.toml"),
        "[features]\nweb_fetch = true\n",
    )
    .unwrap();
    assert_eq!(
        check_on_disk_matches(home, &p),
        Err(SigError::ContentMismatch("requirements"))
    );
}

/// A locally planted file in a signed-ABSENT slot is tamper on both the refetch and
/// gate paths; an absent or empty on-disk file is clean.
#[test]
fn planted_artifact_in_signed_absent_slot_is_tamper() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        requirements: None,
        fail_closed: true,
        ..payload()
    };
    std::fs::write(
        home.join("managed_config.toml"),
        p.managed_config.as_ref().unwrap(),
    )
    .unwrap();
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    // Absent → clean; empty file → clean (some tooling touches empty files).
    assert!(check_on_disk_matches(home, &p).is_ok());
    std::fs::write(home.join("requirements.toml"), "").unwrap();
    assert!(check_on_disk_matches(home, &p).is_ok());

    // Planted non-empty requirements → tamper on both paths.
    std::fs::write(home.join("requirements.toml"), "[endpoints]\n").unwrap();
    assert_eq!(
        check_on_disk_matches(home, &p),
        Err(SigError::ContentMismatch("requirements"))
    );
    assert!(cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("team-007"),
        1_000
    ));
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Compromised
    );

    // A signed EMPTY artifact ("" in the payload) binds absence the same way.
    let p_empty = SignedPayload {
        requirements: Some(String::new()),
        ..p
    };
    assert_eq!(
        check_on_disk_matches(home, &p_empty),
        Err(SigError::ContentMismatch("requirements"))
    );
}

/// An unreadable regular file (EACCES) is a read blip, not tamper: the refetch trigger
/// fires but the gate does not refuse. Unix-only; self-skips when running as root.
#[cfg(unix)]
#[test]
fn unreadable_artifact_refetches_but_does_not_refuse() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    let path = home.join("requirements.toml");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_to_string(&path).is_ok() {
        eprintln!("skipping: permissions not enforced (running as root?)");
        return;
    }

    assert_eq!(
        check_on_disk_matches(home, &p),
        Err(SigError::Unreadable("requirements"))
    );
    assert!(
        cloud_cache_signature_invalid_with_keys(
            home,
            &keyset("v1", &pubkey),
            Some("team-007"),
            1_000
        ),
        "an unreadable artifact must trigger a refetch"
    );
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Trusted,
        "an unreadable artifact must not refuse at the gate"
    );
}

/// A directory squatting in an artifact slot is tamper, not a read blip: the gate
/// refuses and the refetch fires.
#[test]
fn directory_squat_is_tamper_not_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    std::fs::remove_file(home.join("requirements.toml")).unwrap();
    std::fs::create_dir(home.join("requirements.toml")).unwrap();

    assert_eq!(
        check_on_disk_matches(home, &p),
        Err(SigError::ContentMismatch("requirements"))
    );
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Compromised,
        "a directory squat must read compromised at the gate"
    );
    assert!(cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("team-007"),
        1_000
    ));
}

/// A read blip on the SIDECAR mirrors the artifact-slot semantics: SidecarUnreadable
/// at the gate (no refusal) while the refetch trigger fires. Unix-only; self-skips as root.
#[cfg(unix)]
#[test]
fn sidecar_read_blip_is_lenient_at_gate_but_refetches() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    let path = home.join(SIGNATURE_SIDECAR_FILE);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_to_string(&path).is_ok() {
        eprintln!("skipping: permissions not enforced (running as root?)");
        return;
    }

    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::SidecarUnreadable,
        "a sidecar read blip is not tamper — it must not read NoAuthenticSidecar"
    );
    assert!(
        cloud_cache_signature_invalid_with_keys(
            home,
            &keyset("v1", &pubkey),
            Some("team-007"),
            1_000
        ),
        "the blip must trigger a refetch"
    );
}

/// A symlink at an artifact slot is tamper (no-follow classification): even one
/// pointing at a byte-identical file (reads could be redirected later), and one
/// pointing at a directory — never the lenient Unreadable.
#[cfg(unix)]
#[test]
fn symlink_at_artifact_slot_is_tamper() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    // Symlink → file carrying the exact signed bytes.
    let slot = home.join("requirements.toml");
    let target = home.join("elsewhere.toml");
    std::fs::rename(&slot, &target).unwrap();
    std::os::unix::fs::symlink(&target, &slot).unwrap();
    assert_eq!(
        check_on_disk_matches(home, &p),
        Err(SigError::ContentMismatch("requirements")),
        "a matching-content symlink is still tamper"
    );
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Compromised
    );

    // Symlink → directory.
    std::fs::remove_file(&slot).unwrap();
    let squat_dir = home.join("squat_dir");
    std::fs::create_dir(&squat_dir).unwrap();
    std::os::unix::fs::symlink(&squat_dir, &slot).unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Compromised
    );
}

/// A symlink at the SIDECAR slot reads NoAuthenticSidecar (absence-with-teeth under a
/// fail-closed marker), never the lenient SidecarUnreadable — even one pointing at a
/// perfectly valid sidecar file, and one pointing at a directory.
#[cfg(unix)]
#[test]
fn symlink_at_sidecar_slot_is_absence_not_a_blip() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    // Symlink → a byte-identical valid sidecar elsewhere.
    let path = home.join(SIGNATURE_SIDECAR_FILE);
    let target = home.join("sidecar_copy.json");
    std::fs::rename(&path, &target).unwrap();
    std::os::unix::fs::symlink(&target, &path).unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::NoAuthenticSidecar,
        "a symlinked sidecar is not an authentic sidecar"
    );

    // Symlink → directory.
    std::fs::remove_file(&path).unwrap();
    let squat_dir = home.join("squat_dir");
    std::fs::create_dir(&squat_dir).unwrap();
    std::os::unix::fs::symlink(&squat_dir, &path).unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::NoAuthenticSidecar
    );
}

/// A directory squatting at the SIDECAR path reads NoAuthenticSidecar, never SidecarUnreadable.
#[test]
fn sidecar_directory_squat_is_absence_not_a_blip() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    let path = home.join(SIGNATURE_SIDECAR_FILE);
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::NoAuthenticSidecar
    );
}

#[test]
fn sidecar_round_trips_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (kp, _) = test_keypair();
    let sidecar = sign(&kp, &payload());
    write_sidecar(dir.path(), &sidecar).unwrap();
    let SidecarRead::Present(read) = read_sidecar(dir.path()) else {
        panic!("a just-written sidecar must read back Present");
    };
    assert_eq!(read.signed_payload, sidecar.signed_payload);
    assert_eq!(read.signature, sidecar.signature);
}

#[test]
fn verification_armed_with_embedded_key() {
    // Armed: prod v1 key compiled in.
    assert!(verification_active());
    assert_eq!(EMBEDDED_DEPLOYMENT_CONFIG_PUBKEYS.len(), 1);
    assert_eq!(EMBEDDED_DEPLOYMENT_CONFIG_PUBKEYS[0].0, "v1");
    assert!(embedded_key_id_trusted("v1"));
    assert!(!embedded_key_id_trusted("v0"));
    // Fingerprint pin against silent typos.
    let digest = ring::digest::digest(
        &ring::digest::SHA256,
        EMBEDDED_DEPLOYMENT_CONFIG_PUBKEYS[0].1,
    );
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex, EMBEDDED_V1_PUBKEY_SHA256_HEX,
        "embedded v1 pubkey bytes must match the documented SHA-256 fingerprint"
    );
}

/// Empty seam → verification off (incident-disarm shape).
#[test]
fn with_dark_forces_keyless_verification_inactive() {
    test_seam::with_dark(|| {
        assert!(
            !verification_active(),
            "Some(&[]) must force the keyless build for rollback tests"
        );
        assert!(!embedded_key_id_trusted("v1"));
    });
    // restored
    assert!(verification_active());
}

/// Armed: flags missing/untrusted sidecar; nothing on disk → not invalid.
#[test]
fn cloud_cache_signature_invalid_when_armed() {
    let dir = tempfile::tempdir().unwrap();
    assert!(verification_active());
    assert!(!cloud_cache_signature_invalid(
        dir.path(),
        Some("team-007"),
        1_000
    ));
    write_policy(dir.path(), &payload());
    assert!(cloud_cache_signature_invalid(
        dir.path(),
        Some("team-007"),
        1_000
    ));
    let (kp, _) = test_keypair();
    write_sidecar(dir.path(), &sign(&kp, &payload())).unwrap();
    assert!(cloud_cache_signature_invalid(
        dir.path(),
        Some("team-007"),
        1_000
    ));
}

/// Keyless: public gate inert with unsigned policy on disk.
#[test]
fn cloud_cache_signature_invalid_inert_when_dark() {
    test_seam::with_dark(|| {
        let dir = tempfile::tempdir().unwrap();
        write_policy(dir.path(), &payload());
        assert!(!verification_active());
        assert!(
            !cloud_cache_signature_invalid(dir.path(), Some("team-007"), 1_000),
            "dark build must not flag unsigned on-disk policy"
        );
    });
}

/// No policy on disk → nothing to verify → not invalid.
#[test]
fn cloud_cache_signature_invalid_is_false_when_no_policy() {
    let dir = tempfile::tempdir().unwrap();
    let (_, pubkey) = test_keypair();
    assert!(!cloud_cache_signature_invalid_with_keys(
        dir.path(),
        &keyset("v1", &pubkey),
        Some("team-007"),
        1_000,
    ));
}

/// Keyed: a policy with no/edited signature is invalid; a fully covered one is not.
#[test]
fn cloud_cache_signature_invalid_detects_missing_and_edited() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = payload();
    write_policy(home, &p);

    // Policy present, no sidecar → invalid.
    assert!(cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("team-007"),
        1_000
    ));

    // Valid sidecar + matching files → valid.
    write_sidecar(home, &sign(&kp, &p)).unwrap();
    assert!(!cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("team-007"),
        1_000
    ));

    // Wrong active team → invalid (substituted cache / cross-team replay).
    assert!(cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("team-evil"),
        1_000
    ));

    // Editing a present, signed file → invalid (in-place tamper).
    std::fs::write(
        home.join("requirements.toml"),
        "[features]\nweb_fetch = true\n",
    )
    .unwrap();
    assert!(cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("team-007"),
        1_000
    ));
}

/// The refetch trigger flags a signature-authentic but FOREIGN-bound cache as stale
/// (the cross-tenant replay the gate blocks must also rebind online).
#[test]
fn cloud_cache_signature_invalid_flags_foreign_authentic_cache() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();

    let team = payload();
    write_policy(home, &team);
    write_sidecar(home, &sign(&kp, &team)).unwrap();
    assert!(!cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("team-007"),
        1_000
    ));
    assert!(cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("team-999"),
        1_000
    ));

    let deploy = SignedPayload {
        deployment_id: Some("deploy-A".into()),
        team_id: None,
        ..payload()
    };
    write_policy(home, &deploy);
    write_sidecar(home, &sign(&kp, &deploy)).unwrap();
    assert!(!cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("deploy-A"),
        1_000
    ));
    assert!(cloud_cache_signature_invalid_with_keys(
        home,
        &keyset("v1", &pubkey),
        Some("deploy-B"),
        1_000
    ));
}

/// A signed opt-in over an in-place-edited policy reads compromised — regardless of
/// the (forgeable) marker, which this verdict never consults.
#[test]
fn signed_cache_compromised_honors_signed_opt_in() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    // Opted in + bound + intact → not compromised.
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Trusted
    );

    // In-place edit of the signed file → compromised.
    std::fs::write(
        home.join("requirements.toml"),
        "[features]\nweb_fetch = true\n",
    )
    .unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Compromised
    );
}

/// A signed OPT-OUT is never enforced: even an edited policy reads `Trusted` at the
/// gate (the refetch trigger still catches the edit).
#[test]
fn signed_cache_compromised_respects_signed_opt_out() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = payload(); // fail_closed = false
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    std::fs::write(
        home.join("requirements.toml"),
        "[features]\nweb_fetch = true\n",
    )
    .unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Trusted
    );
}

/// No sidecar or a forged signature reads `NoAuthenticSidecar` — never `Compromised`
/// from unverified bytes.
#[test]
fn signed_cache_compromised_none_without_authentic_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);

    // No sidecar → not an authentic verdict.
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::NoAuthenticSidecar
    );

    // Forged signature → not an authentic verdict either (never Compromised).
    let mut bad = sign(&kp, &p);
    bad.signature = base64::engine::general_purpose::STANDARD.encode([0u8; 64]);
    write_sidecar(home, &bad).unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::NoAuthenticSidecar
    );
}

/// A deployment-signed policy bound to a different deployment id is a cross-tenant
/// replay and reads compromised; a matching id (or no recorded id yet) does not.
#[test]
fn signed_cache_compromised_rejects_foreign_deployment() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        deployment_id: Some("dep-foreign".into()),
        team_id: None,
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();

    // Locally-recorded deployment is "dep-local" → foreign signed id → compromised.
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("dep-local"), 1_000),
        SignedVerdict::Compromised
    );
    // Matching deployment id → not compromised.
    assert_eq!(
        signed_cache_compromised_with_keys(
            home,
            &keyset("v1", &pubkey),
            Some("dep-foreign"),
            1_000
        ),
        SignedVerdict::Trusted
    );
    // No locally-recorded id yet (first trusted fetch) → lenient.
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), None, 1_000),
        SignedVerdict::Trusted
    );
}

/// A replayed TEAM-signed cache on a deployment-key machine reads foreign rather than
/// slipping past a deployment-only check.
#[test]
fn signed_cache_compromised_rejects_team_cache_on_deployment_machine() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let team = SignedPayload {
        deployment_id: None,
        team_id: Some("team-x".into()),
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &team);
    write_sidecar(home, &sign(&kp, &team)).unwrap();
    // Expected principal = the machine's recorded deployment id.
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("dep-local"), 1_000),
        SignedVerdict::Compromised,
        "a team-signed cache on a deployment machine must read foreign"
    );
}

/// A foreign-bound but PERMISSIVE policy still reads compromised: identity runs BEFORE
/// the opt-in short-circuit, so another tenant's lenient policy can't escape a strict one.
#[test]
fn signed_cache_compromised_rejects_foreign_permissive_policy() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();

    // Permissive policy bound to a FOREIGN deployment.
    let dep = SignedPayload {
        deployment_id: Some("dep-foreign".into()),
        team_id: None,
        fail_closed: false,
        ..payload()
    };
    write_policy(home, &dep);
    write_sidecar(home, &sign(&kp, &dep)).unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("dep-local"), 1_000),
        SignedVerdict::Compromised,
        "a foreign permissive deployment policy must be rejected, not short-circuited"
    );

    // Permissive policy bound to a FOREIGN team.
    let other_team = SignedPayload {
        deployment_id: None,
        team_id: Some("team-evil".into()),
        fail_closed: false,
        ..payload()
    };
    write_policy(home, &other_team);
    write_sidecar(home, &sign(&kp, &other_team)).unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Compromised,
        "a foreign permissive team policy must be rejected"
    );

    // OUR OWN permissive policy is fine (not compromised).
    let ours = SignedPayload {
        deployment_id: None,
        team_id: Some("team-007".into()),
        fail_closed: false,
        ..payload()
    };
    write_policy(home, &ours);
    write_sidecar(home, &sign(&kp, &ours)).unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::Trusted,
        "our own permissive policy is not compromised"
    );
}

/// Keyless: public entry → Inactive.
#[test]
fn signed_cache_compromised_inactive_when_dark() {
    test_seam::with_dark(|| {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        write_policy(home, &payload());
        assert!(!verification_active());
        assert_eq!(
            signed_cache_compromised(home, Some("team-007"), 1_000),
            SignedVerdict::Inactive
        );
    });
}

/// Armed: foreign key → NoAuthenticSidecar (never Inactive).
#[test]
fn signed_cache_compromised_is_no_authentic_sidecar_when_armed() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, _) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();
    assert!(verification_active());
    assert_eq!(
        signed_cache_compromised(home, Some("team-007"), 1_000),
        SignedVerdict::NoAuthenticSidecar
    );
}

/// Anti-rollback TTL: an authentic opted-in sidecar reads compromised past its signed
/// `expires_at` even with intact content and a matching principal; inside the window it holds.
#[test]
fn signed_cache_compromised_expired_reads_compromised() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        fail_closed: true,
        expires_at: 1_000,
        ..payload()
    };
    write_policy(home, &p);
    write_sidecar(home, &sign(&kp, &p)).unwrap();
    // Past expiry → compromised, despite intact content + matching principal.
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_001),
        SignedVerdict::Compromised,
        "an expired authentic sidecar must read compromised (anti-rollback TTL)"
    );
    // Just inside the window → honored.
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 999),
        SignedVerdict::Trusted
    );
}

/// Rewriting the untrusted outer `sidecar.key_id` can't redirect verification —
/// only the SIGNED payload's `key_id` selects the verifying key.
#[test]
fn untrusted_sidecar_key_id_does_not_affect_verification() {
    let (kp, pubkey) = test_keypair();
    let mut sidecar = sign(&kp, &payload());
    sidecar.key_id = "attacker-controlled-key".into();
    let out = verify_signed_payload(
        &sidecar.signed_payload,
        &sidecar.signature,
        &keyset("v1", &pubkey),
    )
    .expect("the untrusted outer key_id must not affect verification");
    assert_eq!(out.key_id, "v1", "only the signed key_id is authoritative");
}

/// A SIGNED `key_id` outside the trusted set is rejected, and the sidecar reads
/// inauthentic on both the gate and refetch paths.
#[test]
fn unknown_signed_key_id_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let (kp, pubkey) = test_keypair();
    let p = SignedPayload {
        nonce: String::new(),
        key_id: "v9".into(),
        fail_closed: true,
        ..payload()
    };
    let sidecar = sign(&kp, &p);
    assert_eq!(
        verify_signed_payload(
            &sidecar.signed_payload,
            &sidecar.signature,
            &keyset("v1", &pubkey)
        ),
        Err(SigError::UnknownKeyId)
    );

    write_policy(home, &p);
    write_sidecar(home, &sidecar).unwrap();
    assert_eq!(
        signed_cache_compromised_with_keys(home, &keyset("v1", &pubkey), Some("team-007"), 1_000),
        SignedVerdict::NoAuthenticSidecar,
        "an unknown key id is not an authentic verdict"
    );
    assert!(
        cloud_cache_signature_invalid_with_keys(
            home,
            &keyset("v1", &pubkey),
            Some("team-007"),
            1_000
        ),
        "an unknown key id must trigger a refetch"
    );
}

/// Rotation: a {v1, v2} client verifies a payload signed with either key; a payload
/// CLAIMING a trusted id but signed with a different key still fails.
#[test]
fn rotation_selects_the_trusted_key_by_signed_key_id() {
    let (kp1, pubkey1) = test_keypair();
    let (kp2, pubkey2) = test_keypair();
    let both: Vec<(&str, &[u8])> = vec![("v1", &pubkey1), ("v2", &pubkey2)];

    let v1 = sign(&kp1, &payload());
    let v2_payload = SignedPayload {
        nonce: String::new(),
        key_id: "v2".into(),
        ..payload()
    };
    let v2 = sign(&kp2, &v2_payload);
    assert!(verify_signed_payload(&v1.signed_payload, &v1.signature, &both).is_ok());
    let out = verify_signed_payload(&v2.signed_payload, &v2.signature, &both)
        .expect("the v2-signed payload must verify against the rotated set");
    assert_eq!(out.key_id, "v2");

    // A client that has dropped v1 (only-v2 set) still verifies the v2 envelope
    // and rejects the v1 one.
    let only_v2: Vec<(&str, &[u8])> = vec![("v2", &pubkey2)];
    assert!(verify_signed_payload(&v2.signed_payload, &v2.signature, &only_v2).is_ok());
    assert_eq!(
        verify_signed_payload(&v1.signed_payload, &v1.signature, &only_v2),
        Err(SigError::UnknownKeyId)
    );

    // Claiming v1 while signed with kp2 picks the v1 key — and fails to verify.
    let imposter = SignatureEnvelope {
        signed_payload: v1.signed_payload.clone(),
        signature: v2.signature.clone(),
        key_id: "v1".into(),
    };
    assert_eq!(
        verify_signed_payload(&imposter.signed_payload, &imposter.signature, &both),
        Err(SigError::SignatureMismatch)
    );
}

// The is-managed claim tests live in a sibling child module (this file is at the
// 1k-line mark); same private access via the #[path] include below.
#[path = "claim_tests.rs"]
mod claim_tests;

/// Serialize tests that mutate the process-global kill-switch / key seam.
fn with_remote_disarm_lock<R>(f: impl FnOnce() -> R) -> R {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

#[test]
fn remote_kill_switch_dark_embed_stays_inactive() {
    with_remote_disarm_lock(|| {
        // Forced dark: inactive regardless of kill-switch (prod embed is keyed).
        test_seam::with_dark(|| {
            apply_remote_managed_config_signature_verification(Some(true), true);
            assert!(!verification_active());

            apply_remote_managed_config_signature_verification(Some(false), true);
            assert!(!verification_active());

            apply_remote_managed_config_signature_verification(None, true);
            assert!(!verification_active());
        });
    });
}

/// With keys embedded (prod pin), disarm flips verification off and re-arm restores it.
/// Untrusted origin cannot disarm.
#[test]
fn remote_kill_switch_with_keys_disarms_and_rearms() {
    with_remote_disarm_lock(|| {
        apply_remote_managed_config_signature_verification(Some(true), true);
        assert!(
            verification_active(),
            "keys embedded + armed must be verification_active"
        );

        apply_remote_managed_config_signature_verification(Some(false), true);
        assert!(
            !verification_active(),
            "trusted Some(false) must disarm keyed verification"
        );

        apply_remote_managed_config_signature_verification(Some(true), true);
        assert!(
            verification_active(),
            "Some(true) must re-arm keyed verification"
        );

        apply_remote_managed_config_signature_verification(Some(false), false);
        assert!(
            verification_active(),
            "untrusted Some(false) must not disarm when keys are embedded"
        );

        apply_remote_managed_config_signature_verification(None, true);
        assert!(verification_active());
    });
}
