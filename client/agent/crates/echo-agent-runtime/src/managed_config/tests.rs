use super::*;

/// Fail closed only for a managed principal AND compromised policy; every other combination proceeds.
#[test]
fn gate_blocks_only_managed_principal_with_compromised_policy() {
    // The one blocking case.
    assert!(
        managed_policy_gate_decision(true, true).is_err(),
        "managed principal + compromised policy must fail closed"
    );
    // Policy intact / opted-out → proceed even for a managed principal.
    assert!(managed_policy_gate_decision(true, false).is_ok());
    // No managed principal → nothing to enforce.
    assert!(managed_policy_gate_decision(false, true).is_ok());
    assert!(managed_policy_gate_decision(false, false).is_ok());
}

/// Writes both artifacts and overwrites in place on re-fetch.
#[test]
fn apply_writes_and_overwrites_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::write(home.join("managed_config.toml"), "[cli]\nold = true\n").unwrap();

    let body = ManagedConfigResponse {
        deployment_id: None,
        team_id: None,
        managed_config: Some("[cli]\ntheme = \"dark\"\n".into()),
        requirements: Some("[features]\nweb_fetch = false\n".into()),
        ..Default::default()
    };
    assert!(apply_managed_config(home, &body).unwrap());

    assert_eq!(
        std::fs::read_to_string(home.join("managed_config.toml")).unwrap(),
        "[cli]\ntheme = \"dark\"\n",
        "managed_config is overwritten with the served content"
    );
    assert_eq!(
        std::fs::read_to_string(home.join("requirements.toml")).unwrap(),
        "[features]\nweb_fetch = false\n"
    );
}

/// An artifact the response no longer serves (absent or empty) is REMOVED — a withdrawn
/// policy must stop enforcing, and a leftover would trip the signed absence check.
#[test]
fn apply_removes_artifact_the_server_no_longer_serves() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::write(home.join("requirements.toml"), "[features]\n").unwrap();

    // Response carries managed_config but NOT requirements.
    let body = ManagedConfigResponse {
        deployment_id: None,
        team_id: None,
        managed_config: Some("[cli]\ntheme = \"dark\"\n".into()),
        requirements: None,
        ..Default::default()
    };
    assert!(apply_managed_config(home, &body).unwrap());

    assert!(home.join("managed_config.toml").exists());
    assert!(
        !home.join("requirements.toml").exists(),
        "an artifact the server no longer serves is removed"
    );

    // EMPTY served content means the same thing as absent: remove.
    let withdrawn = ManagedConfigResponse {
        deployment_id: None,
        team_id: None,
        managed_config: Some(String::new()),
        requirements: None,
        ..Default::default()
    };
    assert!(apply_managed_config(home, &withdrawn).unwrap());
    assert!(
        !home.join("managed_config.toml").exists(),
        "empty served content converges to absence"
    );

    // Converged state: another empty apply changes nothing.
    assert!(!apply_managed_config(home, &withdrawn).unwrap());
}

/// Partial-write robustness: if one artifact lands and the other write
/// fails, the error surfaces but the artifact that succeeded is kept.
#[cfg(unix)]
#[test]
fn apply_partial_write_failure_keeps_written_artifact() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    // Force the requirements write to fail: a squatting dir whose child can't be
    // unlinked (no write bit on the dir), so even the dir-squat clearing fails
    // and the rename onto the dir fails after it.
    let req = home.join("requirements.toml");
    std::fs::create_dir(&req).unwrap();
    std::fs::write(req.join("pin"), "x").unwrap();
    std::fs::set_permissions(&req, std::fs::Permissions::from_mode(0o500)).unwrap();
    if std::fs::remove_dir_all(&req).is_ok() {
        eprintln!("skipping: permissions not enforced (running as root?)");
        return;
    }

    let body = ManagedConfigResponse {
        deployment_id: None,
        team_id: None,
        managed_config: Some("[cli]\ninstaller = \"internal\"\n".into()),
        requirements: Some("[features]\nweb_fetch = false\n".into()),
        ..Default::default()
    };
    let result = apply_managed_config(home, &body);

    assert!(result.is_err(), "requirements write must fail");
    assert!(
        home.join("managed_config.toml").exists(),
        "the artifact that wrote successfully must be kept"
    );
    // Tidy so the tempdir can be cleaned up.
    let _ = std::fs::set_permissions(&req, std::fs::Permissions::from_mode(0o700));
}

/// The apply clears a squatting directory on both the overwrite and removal branches,
/// so a dir-squat can't permanently block convergence.
#[test]
fn apply_converges_over_a_squatting_directory() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    std::fs::create_dir(home.join("requirements.toml")).unwrap();
    std::fs::write(home.join("requirements.toml").join("junk"), "x").unwrap();
    std::fs::create_dir(home.join("managed_config.toml")).unwrap();

    // Overwrite branch clears the dir and writes; removal branch clears the dir.
    let body = ManagedConfigResponse {
        deployment_id: None,
        team_id: None,
        managed_config: Some("[cli]\ntheme = \"dark\"\n".into()),
        requirements: None,
        ..Default::default()
    };
    assert!(apply_managed_config(home, &body).unwrap());
    assert_eq!(
        std::fs::read_to_string(home.join("managed_config.toml")).unwrap(),
        "[cli]\ntheme = \"dark\"\n",
        "the squatting directory is replaced by the served file"
    );
    assert!(
        !home.join("requirements.toml").exists(),
        "the squatting directory in a served-absent slot is removed"
    );
}

/// The purge primitive (shared by logout and the identity-change purge) is best-effort over a
/// PARTIAL install: present artifacts are removed, absent ones are tolerated (no panic, no
/// error), and a directory squatting an artifact path is removed too.
#[test]
fn remove_managed_config_files_tolerates_partial_existence() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    // Only two of the four artifacts exist; one of them is a squatting DIRECTORY.
    std::fs::write(home.join("requirements.toml"), "[features]\n").unwrap();
    std::fs::create_dir(home.join("managed_config.toml")).unwrap();
    std::fs::write(home.join("managed_config.toml").join("junk"), "x").unwrap();

    remove_managed_config_files(home);

    for f in [
        "requirements.toml",
        "managed_config.toml",
        "managed_config_cache.json",
        "managed_config.sig.json",
    ] {
        assert!(
            !home.join(f).exists(),
            "{f} must be gone after the purge (absent ones tolerated, dir squat removed)"
        );
    }
}

/// The transport-interruption variant must be retryable (so the loop escapes a
/// poisoned connection) and must not be mistaken for an auth rejection.
#[test]
fn connection_interrupted_is_retryable_not_auth() {
    let e = ManagedConfigError::ConnectionInterrupted("closed".into());
    assert!(e.is_retryable(), "a transient interruption must be retried");
    assert!(
        !e.is_auth_rejection(),
        "a transport error is not an auth rejection"
    );
}

#[test]
fn transport_failure_maps_to_managed_config_error() {
    use crate::http::{TransportFailure, TransportFailureKind};

    let unreachable = map_transport_failure(TransportFailure {
        kind: TransportFailureKind::Unreachable,
        detail: "connection refused".into(),
    });
    assert!(matches!(unreachable, ManagedConfigError::Network(_)));
    assert!(
        unreachable.is_retryable(),
        "an unreachable server is retried"
    );
    assert!(!unreachable.is_auth_rejection());

    let interrupted = map_transport_failure(TransportFailure {
        kind: TransportFailureKind::Interrupted,
        detail: "connection closed before message completed".into(),
    });
    assert!(matches!(
        interrupted,
        ManagedConfigError::ConnectionInterrupted(_)
    ));
    assert!(
        interrupted.is_retryable(),
        "an in-flight interruption is retried"
    );

    let permanent = map_transport_failure(TransportFailure {
        kind: TransportFailureKind::Permanent,
        detail: "too many redirects".into(),
    });
    assert!(
        matches!(permanent, ManagedConfigError::RequestFailed(_)),
        "a client-side defect maps to RequestFailed, not InvalidResponse"
    );
    assert!(
        !permanent.is_retryable(),
        "a client-side defect is terminal and must not be retried"
    );
    assert!(!permanent.is_auth_rejection());

    let untrusted = map_transport_failure(TransportFailure {
        kind: TransportFailureKind::CertificateUntrusted,
        detail: "invalid peer certificate: UnknownIssuer".into(),
    });
    assert!(matches!(
        untrusted,
        ManagedConfigError::CertificateUntrusted(_)
    ));
    assert!(
        !untrusted.is_retryable(),
        "the same untrusted certificate will fail again until roots are installed"
    );

    let invalid = map_transport_failure(TransportFailure {
        kind: TransportFailureKind::CertificateInvalid,
        detail: "invalid peer certificate: Expired".into(),
    });
    assert!(matches!(invalid, ManagedConfigError::CertificateInvalid(_)));
    assert!(
        !invalid.is_retryable(),
        "an expired or wrong-host certificate will fail again; retrying cannot fix it"
    );
}

#[test]
fn certificate_detail_names_the_bundle_env_only_when_set() {
    assert_eq!(
        certificate_detail(
            "UnknownIssuer".into(),
            Some("ECHO_AGENT_EXTRA_CA_BUNDLE"),
            2
        ),
        "UnknownIssuer; ECHO_AGENT_EXTRA_CA_BUNDLE is set: verify it includes the issuing root CA"
    );
    assert_eq!(
        certificate_detail(
            "UnknownIssuer".into(),
            Some("ECHO_AGENT_EXTRA_CA_BUNDLE"),
            0
        ),
        "UnknownIssuer; ECHO_AGENT_EXTRA_CA_BUNDLE is set but no usable roots were loaded from it: check that the file is readable, contains PEM certificates, and is under the size cap"
    );
    assert_eq!(
        certificate_detail("UnknownIssuer".into(), None, 0),
        "UnknownIssuer"
    );
}

/// `send_with_retry_escaping_pool` combinator behavior with a counting op (no network):
/// retryable errors retry up to `max_attempts`, non-retryable fails fast, success
/// short-circuits, backoff awaited once per retry. The fresh-client swap on the final
/// attempt needs a real degraded upstream and stays covered by the headless/e2e pass.
#[tokio::test]
async fn send_with_retry_escaping_pool_combinator_behavior() {
    use std::sync::atomic::{AtomicU32, Ordering};

    // (a) all-retryable: op runs max_attempts times, backoff awaited max_attempts-1 times, last Err returned.
    let op_calls = AtomicU32::new(0);
    let backoffs = AtomicU32::new(0);
    let exhausted: Result<(), u32> = crate::http::send_with_retry_escaping_pool(
        |_client| {
            let n = op_calls.fetch_add(1, Ordering::SeqCst);
            async move { Err(n) }
        },
        3,
        |_e: &u32| true,
        |_attempt| {
            backoffs.fetch_add(1, Ordering::SeqCst);
            std::future::ready(())
        },
    )
    .await;
    assert_eq!(exhausted, Err(2), "returns the last attempt's error");
    assert_eq!(
        op_calls.load(Ordering::SeqCst),
        3,
        "op runs max_attempts times"
    );
    assert_eq!(
        backoffs.load(Ordering::SeqCst),
        2,
        "backoff awaited max_attempts-1 times"
    );

    // (b) non-retryable: fail fast after one op call, no backoff.
    let op_calls = AtomicU32::new(0);
    let backoffs = AtomicU32::new(0);
    let fast: Result<(), u32> = crate::http::send_with_retry_escaping_pool(
        |_client| {
            op_calls.fetch_add(1, Ordering::SeqCst);
            async { Err(7) }
        },
        5,
        |_e: &u32| false,
        |_attempt| {
            backoffs.fetch_add(1, Ordering::SeqCst);
            std::future::ready(())
        },
    )
    .await;
    assert_eq!(fast, Err(7));
    assert_eq!(
        op_calls.load(Ordering::SeqCst),
        1,
        "a non-retryable error fails fast"
    );
    assert_eq!(
        backoffs.load(Ordering::SeqCst),
        0,
        "no backoff on a fast failure"
    );

    // (c) success short-circuits: fail once (retryable), then succeed on the 2nd attempt.
    let op_calls = AtomicU32::new(0);
    let ok: Result<u32, u32> = crate::http::send_with_retry_escaping_pool(
        |_client| {
            let n = op_calls.fetch_add(1, Ordering::SeqCst);
            let outcome: Result<u32, u32> = if n == 0 { Err(1) } else { Ok(42) };
            async move { outcome }
        },
        5,
        |_e: &u32| true,
        |_attempt| std::future::ready(()),
    )
    .await;
    assert_eq!(ok, Ok(42));
    assert_eq!(
        op_calls.load(Ordering::SeqCst),
        2,
        "stops at the first success"
    );
}

/// The sync marker is structurally separate from the artifact list — it must only ever
/// be removed by the dedicated post-loop step in `remove_managed_config_files`.
#[test]
fn marker_is_not_a_managed_artifact() {
    assert!(
        !MANAGED_ARTIFACT_FILES.contains(&echo_agent_config::MANAGED_CONFIG_CACHE_FILE),
        "the marker must be removed last, never as part of the artifact loop"
    );
    // Pin the composed contents: the purge loops, tmp-prefix sweep, and eviction all
    // derive from these names, so a constant silently changing value would re-point
    // them all at once.
    assert_eq!(
        MANAGED_ARTIFACT_FILES,
        [
            "managed_config.toml",
            "requirements.toml",
            "managed_config.sig.json",
            "managed_identity.sig.json"
        ],
        "the artifact list is load-bearing for every derived loop; change it deliberately"
    );
}

/// Error prefixes re-arm the detector like crash prefixes: when an artifact removal
/// FAILS, the marker must survive, so the next start re-runs the purge instead of
/// leaving the prior tenant's policy live with the detector disarmed.
#[cfg(unix)]
#[test]
fn purge_keeps_marker_when_an_artifact_removal_fails() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    for name in MANAGED_ARTIFACT_FILES {
        std::fs::write(home.join(name), "x").unwrap();
    }
    std::fs::write(
        home.join(echo_agent_config::MANAGED_CONFIG_CACHE_FILE),
        "{}",
    )
    .unwrap();

    // Make one artifact unremovable: squat it with a dir whose read-only subdir
    // holds a file — `remove_dir_all` can't unlink inside the read-only subdir.
    let squat = home.join("requirements.toml");
    std::fs::remove_file(&squat).unwrap();
    let locked_subdir = squat.join("locked");
    std::fs::create_dir_all(&locked_subdir).unwrap();
    std::fs::write(locked_subdir.join("pin"), "x").unwrap();
    let readonly = std::fs::Permissions::from_mode(0o555);
    std::fs::set_permissions(&locked_subdir, readonly).unwrap();
    if std::fs::remove_dir_all(&squat).is_ok() {
        // The fault can't be injected: read-only perms don't block removal (root/CI edge).
        eprintln!("skipping: permissions not enforced (running as root?)");
        return;
    }

    remove_managed_config_files(home);
    assert!(
        home.join(echo_agent_config::MANAGED_CONFIG_CACHE_FILE)
            .exists(),
        "a failed artifact removal must keep the marker (detector stays armed)"
    );

    // Clear the fault: the next purge converges and only then drops the marker.
    std::fs::set_permissions(&locked_subdir, std::fs::Permissions::from_mode(0o755)).unwrap();
    remove_managed_config_files(home);
    for name in MANAGED_ARTIFACT_FILES {
        assert!(!home.join(name).exists(), "{name} must be purged");
    }
    assert!(
        !home
            .join(echo_agent_config::MANAGED_CONFIG_CACHE_FILE)
            .exists(),
        "with every artifact removed, the marker goes last"
    );
}

// --- The is-managed claim persist rules ---

/// Deployment id wins over team id (server parity).
#[test]
fn served_principal_prefers_deployment_id() {
    use echo_agent_config::signed_policy::SignedPayload;
    let payload = |dep: Option<&str>, team: Option<&str>| SignedPayload {
        typ: echo_agent_config::signed_policy::MANAGED_POLICY_TYP.into(),
        version: 1,
        deployment_id: dep.map(Into::into),
        team_id: team.map(Into::into),
        managed_config: None,
        requirements: None,
        fail_closed: false,
        expires_at: 0,
        nonce: String::new(),
        key_id: "v1".into(),
    };
    assert_eq!(
        served_principal_of(&payload(Some("dep-1"), Some("team-007"))),
        Some("dep-1")
    );
    assert_eq!(
        served_principal_of(&payload(None, Some("team-007"))),
        Some("team-007")
    );
    assert_eq!(served_principal_of(&payload(None, None)), None);
}

/// A verified claim persists ONLY when bound to the served principal.
#[test]
fn claim_persists_only_when_bound_to_served_principal() {
    let claim = |principal: &str| echo_agent_config::signed_policy::ManagedIdentityClaim {
        typ: echo_agent_config::signed_policy::MANAGED_IDENTITY_TYP.into(),
        principal: principal.into(),
        fail_closed: true,
        expires_at: 4_000_000_000,
        key_id: "v1".into(),
    };
    assert!(claim_binds_to(&claim("team-007"), Some("team-007")));
    assert!(!claim_binds_to(&claim("team-evil"), Some("team-007")));
    assert!(!claim_binds_to(&claim("team-007"), None));
}

/// Old server, no claim envelopes: nothing persists, nothing errors.
#[test]
fn absent_claim_is_skipped() {
    assert!(verified_claim_sidecar(&ManagedConfigResponse::default(), Some("team-007")).is_none());
}

/// The startup label: a deployment key wins outright, and an unreadable
/// `auth.json` is `unknown`, never `personal` — the sync still runs and
/// mislabeling it would hide the deployment-cost split.
#[test]
fn auth_mode_classification() {
    use echo_agent_telemetry::startup::AuthMode;
    let err = || std::io::Error::other("unreadable");
    assert_eq!(auth_mode(true, &Ok(true)), AuthMode::Deployment);
    assert_eq!(auth_mode(true, &Err(err())), AuthMode::Deployment);
    assert_eq!(auth_mode(false, &Ok(true)), AuthMode::Team);
    assert_eq!(auth_mode(false, &Ok(false)), AuthMode::Personal);
    assert_eq!(auth_mode(false, &Err(err())), AuthMode::Unknown);
}

/// The `ECHO_AGENT_CONFIG` overlay must not arm or disarm the managed-config sync gate:
/// the reader is overlay-free, so an overlay value never reaches it in either
/// direction. Requirements/managed layers still resolve normally.
#[test]
fn managed_config_gate_ignores_the_overlay_in_both_directions() {
    use crate::config::ConfigLayers;

    fn features_managed_config(v: bool) -> toml::Value {
        toml::from_str(&format!("[features]\nmanaged_config = {v}\n")).unwrap()
    }

    let mut layers = ConfigLayers {
        user: features_managed_config(true),
        env_overlay: Some(features_managed_config(false)),
        ..Default::default()
    };
    assert_eq!(managed_config_enabled_from_layers(&layers), Some(true));

    layers.user = features_managed_config(false);
    layers.env_overlay = Some(features_managed_config(true));
    assert_eq!(managed_config_enabled_from_layers(&layers), Some(false));

    layers.user = toml::Value::Table(Default::default());
    layers.env_overlay = Some(features_managed_config(false));
    assert_eq!(managed_config_enabled_from_layers(&layers), None);
}
