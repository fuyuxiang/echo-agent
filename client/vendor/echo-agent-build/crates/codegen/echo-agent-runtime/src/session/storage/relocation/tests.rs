use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agent_client_protocol as acp;
use nix::sys::stat::Mode;
use nix::unistd::mkfifo;

use super::*;
use crate::session::info::Info;
use crate::session::storage::relocation::journal::AtomicWriteFault;

#[test]
fn journal_is_validated_commit_aware_and_externally_leased() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    let lease = storage.acquire("session").unwrap();
    assert!(matches!(
        storage.acquire("session"),
        Err(RelocationError::LeaseBusy(_))
    ));

    let journal =
        RelocationJournal::test_new("session", "/source", "/target", RelocationPhase::Prepared);
    storage.write_journal(&journal).unwrap();
    assert_eq!(storage.read_journal("session").unwrap(), journal);

    let mut invalid = journal.clone();
    invalid.cwd_generation = 0;
    assert!(matches!(
        storage.write_journal(&invalid),
        Err(WriteFailure::NotCommitted(_))
    ));
    drop(lease);
}

#[test]
fn atomic_journal_write_distinguishes_rename_commit() {
    for (fault, is_committed) in [
        (AtomicWriteFault::BeforeRename, false),
        (AtomicWriteFault::AfterRename, true),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let journal =
            RelocationJournal::test_new("session", "/source", "/target", RelocationPhase::Ready);
        let result = super::journal::write(temp.path(), &journal, Some(fault));
        assert_eq!(
            matches!(result, Err(WriteFailure::Committed(_))),
            is_committed
        );
        assert_eq!(
            super::journal::journal_path(temp.path(), "session").exists(),
            is_committed
        );
    }
}

#[test]
fn recursive_copy_preserves_modes_and_symlinks_and_rejects_special_files() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir_all(source.join("nested")).unwrap();
    let executable = source.join("nested/tool");
    fs::write(&executable, b"opaque\0bytes").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o751)).unwrap();
    std::os::unix::fs::symlink("nested/tool", source.join("relative-link")).unwrap();
    std::os::unix::fs::symlink("missing", source.join("dangling-link")).unwrap();

    storage.copy_directory(&source, &target).unwrap();
    assert_eq!(
        fs::metadata(target.join("nested/tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    assert_eq!(
        fs::read_link(target.join("relative-link")).unwrap(),
        std::path::Path::new("nested/tool")
    );
    assert_eq!(
        fs::read_link(target.join("dangling-link")).unwrap(),
        std::path::Path::new("missing")
    );

    let special_source = temp.path().join("special-source");
    fs::create_dir(&special_source).unwrap();
    mkfifo(&special_source.join("pipe"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    assert!(
        storage
            .copy_directory(&special_source, &temp.path().join("special-target"))
            .is_err()
    );
}

#[test]
fn copy_rejects_existing_target_without_changing_stale_contents() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("new"), "new").unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("stale"), "stale").unwrap();

    assert!(matches!(
        storage.copy_directory(&source, &target),
        Err(RelocationError::Collision(_))
    ));
    assert_eq!(fs::read_to_string(target.join("stale")).unwrap(), "stale");
    assert!(!target.join("new").exists());
}

#[test]
fn copy_rejects_same_or_nested_target_before_creation() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();

    for target in [source.clone(), source.join("nested/../target")] {
        assert!(matches!(
            storage.copy_directory(&source, &target),
            Err(RelocationError::Inconsistent(_))
        ));
        assert!(!source.join("target").exists());
    }
}

#[test]
fn copy_rejects_source_and_target_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    let source = temp.path().join("source");
    let source_alias = temp.path().join("source-alias");
    let target_parent_alias = temp.path().join("target-parent-alias");
    fs::create_dir(&source).unwrap();
    std::os::unix::fs::symlink(&source, &source_alias).unwrap();
    std::os::unix::fs::symlink(temp.path(), &target_parent_alias).unwrap();

    assert!(matches!(
        storage.copy_directory(&source, &source_alias),
        Err(RelocationError::Inconsistent(_))
    ));
    assert!(matches!(
        storage.copy_directory(&source, &target_parent_alias.join("source/nested")),
        Err(RelocationError::Inconsistent(_))
    ));
    assert!(!source.join("nested").exists());
}

#[test]
fn durable_remove_and_atomic_no_replace_are_inert_building_blocks() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    storage.create_directory(&source).unwrap();
    storage.create_directory(&target).unwrap();
    assert!(matches!(
        storage.publish_no_replace(&source, &target),
        Err(RelocationError::Collision(_))
    ));
    storage.remove_directory(&source).unwrap();
    storage.remove_directory(&source).unwrap();
}

fn request(id: &str, source: &str, target: &str, generation: u64) -> RelocationRequest {
    RelocationRequest {
        session_id: id.into(),
        nonce: format!("n-{}", std::process::id()),
        source_cwd: source.into(),
        target_cwd: target.into(),
        cwd_generation: generation,
        pending_reminder: PendingCwdSwitchReminder {
            cwd_generation: generation,
            previous_cwd: source.into(),
            destination_cwd: target.into(),
            content: "cwd switched".into(),
            destination_project_instructions: Some("target rules".into()),
        },
    }
}

fn session_dir(root: &Path, cwd: &str, id: &str) -> PathBuf {
    journal::session_dir_at(root, cwd, id)
}

fn create_source(root: &Path, cwd: &str, id: &str, generation: u64) -> PathBuf {
    let dir = session_dir(root, cwd, id);
    let nested = dir.join("unknown/nested");
    fs::create_dir_all(&nested).unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o750)).unwrap();
    let mut summary = Summary::new(
        &Info {
            id: acp::SessionId::new(id),
            cwd: cwd.into(),
        },
        acp::ModelId::new("test-model"),
    )
    .unwrap();
    summary.cwd_generation = generation;
    let mut value = serde_json::to_value(summary).unwrap();
    value["opaque_top"] = serde_json::json!({"future": [1, 2, 3]});
    value["info"]["opaque_info"] = serde_json::json!("future-info");
    let summary_path = dir.join(super::super::SUMMARY_FILE);
    fs::write(&summary_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    fs::set_permissions(&summary_path, fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(dir.join("chat_history.jsonl"), b"historical bytes\n").unwrap();
    let executable = nested.join("tool");
    fs::write(&executable, b"opaque\0bytes").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o751)).unwrap();
    dir
}

fn create_valid_target(root: &Path, journal: &RelocationJournal) -> PathBuf {
    let dir = create_source(
        root,
        &journal.target_cwd,
        &journal.session_id,
        journal.cwd_generation,
    );
    let path = dir.join(super::super::SUMMARY_FILE);
    let mut summary: Summary = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    summary.previous_cwd = Some(journal.source_cwd.clone());
    summary.pending_cwd_switch_reminder = Some(PendingCwdSwitchReminder {
        cwd_generation: journal.cwd_generation,
        previous_cwd: journal.source_cwd.clone(),
        destination_cwd: journal.target_cwd.clone(),
        content: "cwd switched".into(),
        destination_project_instructions: None,
    });
    fs::write(path, serde_json::to_vec_pretty(&summary).unwrap()).unwrap();
    dir
}

#[test]
fn stage_and_publish_creates_owner_only_target_parent() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    create_source(temp.path(), "/source", "perm", 0);
    let lease = storage.acquire("perm").unwrap();
    storage
        .stage_and_publish(&lease, request("perm", "/source", "/target", 1))
        .unwrap();

    let parent = session_dir(temp.path(), "/target", "perm")
        .parent()
        .unwrap()
        .to_path_buf();
    use crate::test_support::unix_mode;
    assert_eq!(
        unix_mode(&parent),
        0o700,
        "<encoded-cwd> target parent must be 0700"
    );
    assert_eq!(
        unix_mode(&temp.path().join("sessions")),
        0o700,
        "sessions root must be 0700"
    );
}

#[test]
fn commit_and_rollback_terminal_proofs_allow_retries_and_second_relocation() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    create_source(temp.path(), "/source", "again", 0);
    let lease = storage.acquire("again").unwrap();
    let staged = storage
        .stage_and_publish(&lease, request("again", "/source", "/target", 1))
        .unwrap();
    let rollback = storage.rollback(&lease, &staged).unwrap();
    assert!(!session_dir(temp.path(), "/target", "again").exists());
    storage.finalize_terminal(&lease, &rollback).unwrap();

    let staged = storage
        .stage_and_publish(&lease, request("again", "/source", "/target", 1))
        .unwrap();
    let committed = storage.mark_ready_and_commit(&lease, &staged).unwrap();
    let faulted = RelocationStorage::with_fault(temp.path().into(), TestFault::NamespaceBarrier);
    assert!(faulted.finalize_terminal(&lease, &committed).is_err());
    assert!(!journal::journal_path(temp.path(), "again").exists());
    storage.finalize_terminal(&lease, &committed).unwrap();
    let mut request = request("again", "/target", "/third", 2);
    request.nonce = format!("n2-{}", std::process::id());
    let next = storage.stage_and_publish(&lease, request).unwrap();
    assert!(matches!(
        storage.rollback(&lease, &staged),
        Err(RelocationError::TransactionMismatch)
    ));
    assert!(matches!(
        storage.mark_ready_and_commit(&lease, &staged),
        Err(RelocationError::TransactionMismatch)
    ));
    assert!(next.matches(&storage.read_journal("again").unwrap()));
}

#[test]
fn barriers_and_malformed_ready_fail_closed_before_source_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let missing = RelocationStorage::new(temp.path().into());
    let missing_lease = missing.acquire("missing").unwrap();
    assert!(matches!(
        missing.recover(&missing_lease),
        Err(RelocationError::JournalMissing(_))
    ));

    let temp = tempfile::tempdir().unwrap();
    let base = RelocationStorage::new(temp.path().into());
    create_source(temp.path(), "/source", "barrier", 0);
    let lease = base.acquire("barrier").unwrap();
    let staged = base
        .stage_and_publish(&lease, request("barrier", "/source", "/target", 1))
        .unwrap();
    let faulted = RelocationStorage::with_fault(
        temp.path().into(),
        TestFault::ReadyAfterRenameThenNamespaceBarrier,
    );
    assert!(matches!(
        faulted.mark_ready_and_commit(&lease, &staged),
        Err(RelocationError::RecoveryRequired {
            phase: RelocationPhase::Ready,
            ..
        })
    ));
    assert!(faulted.recover(&lease).is_err());

    let target = session_dir(temp.path(), "/target", "barrier");
    let decoy = temp.path().join("decoy");
    fs::rename(&target, &decoy).unwrap();
    std::os::unix::fs::symlink(&decoy, &target).unwrap();
    assert!(matches!(
        base.recover(&lease),
        Err(RelocationError::RecoveryRequired {
            phase: RelocationPhase::Ready,
            ..
        })
    ));

    let temp = tempfile::tempdir().unwrap();
    let base = RelocationStorage::new(temp.path().into());
    let source = create_source(temp.path(), "/source", "remove", 0);
    let summary = source.join(super::super::SUMMARY_FILE);
    let source_bytes = fs::read(&summary).unwrap();
    let lease = base.acquire("remove").unwrap();
    let staged = base
        .stage_and_publish(&lease, request("remove", "/source", "/target", 1))
        .unwrap();
    let mut malformed: serde_json::Value = serde_json::from_slice(&source_bytes).unwrap();
    malformed["info"]["id"] = "other".into();
    fs::write(&summary, serde_json::to_vec(&malformed).unwrap()).unwrap();
    assert!(matches!(
        base.recover(&lease),
        Err(RelocationError::RecoveryRequired {
            phase: RelocationPhase::TargetPublished,
            ..
        })
    ));
    assert!(session_dir(temp.path(), "/target", "remove").exists());
    fs::write(summary, source_bytes).unwrap();
    let faulted = RelocationStorage::with_fault(temp.path().into(), TestFault::RemoveBarrier);
    assert!(matches!(
        faulted.rollback(&lease, &staged),
        Err(RelocationError::RecoveryRequired {
            phase: RelocationPhase::TargetPublished,
            ..
        })
    ));
    assert_eq!(
        base.read_journal("remove").unwrap().phase,
        RelocationPhase::TargetPublished
    );
}

#[test]
fn copy_failure_self_cleans_and_post_publication_failure_requires_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    let source = create_source(temp.path(), "/source", "copy", 0);
    mkfifo(&source.join("unknown/pipe"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
    let lease = storage.acquire("copy").unwrap();
    let failure = storage
        .stage_and_publish(&lease, request("copy", "/source", "/target", 1))
        .unwrap_err();
    let proof = match failure {
        RelocationError::RolledBack { terminal, .. } => terminal,
        error => panic!("expected typed rollback proof, got {error}"),
    };
    fs::remove_file(source.join("unknown/pipe")).unwrap();
    storage.finalize_terminal(&lease, &proof).unwrap();
    storage
        .stage_and_publish(&lease, request("copy", "/source", "/target", 1))
        .unwrap();

    let temp = tempfile::tempdir().unwrap();
    let base = RelocationStorage::new(temp.path().into());
    create_source(temp.path(), "/source", "published", 0);
    let lease = base.acquire("published").unwrap();
    let faulted = RelocationStorage::with_fault(
        temp.path().into(),
        TestFault::Journal(
            RelocationPhase::TargetPublished,
            AtomicWriteFault::BeforeRename,
        ),
    );
    assert!(matches!(
        faulted.stage_and_publish(&lease, request("published", "/source", "/target", 1)),
        Err(RelocationError::RecoveryRequired {
            phase: RelocationPhase::Staged,
            ..
        })
    ));
    assert!(session_dir(temp.path(), "/target", "published").exists());
}

#[test]
fn loading_one_session_recovers_only_its_relocation() {
    let temp = tempfile::tempdir().unwrap();
    let storage = RelocationStorage::new(temp.path().into());
    for id in ["opening", "background"] {
        create_source(temp.path(), "/source", id, 0);
        fs::create_dir_all(session_dir(temp.path(), "/target", id).parent().unwrap()).unwrap();
        let journal = RelocationJournal::test_new(id, "/source", "/target", RelocationPhase::Prepared);
        super::journal::write(temp.path(), &journal, None).unwrap();
    }
    let _background = storage.acquire("background").unwrap();
    storage.recover_if_pending("opening").unwrap();
    assert!(matches!(storage.read_journal("opening"), Err(RelocationError::JournalMissing(_))));
    assert!(storage.read_journal("background").is_ok());
    assert!(session_dir(temp.path(), "/source", "opening").exists());
    storage.recover_if_pending("opening").unwrap();
}

#[test]
fn recover_all_finalizes_every_phase() {
    for phase in [
        RelocationPhase::Prepared,
        RelocationPhase::Staged,
        RelocationPhase::TargetPublished,
        RelocationPhase::Ready,
        RelocationPhase::Committed,
        RelocationPhase::RolledBack,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let journal = RelocationJournal::test_new("r", "/source", "/target", phase);
        for cwd in ["/source", "/target"] {
            fs::create_dir_all(session_dir(temp.path(), cwd, "r").parent().unwrap()).unwrap();
        }
        if phase != RelocationPhase::Committed {
            create_source(temp.path(), "/source", "r", 0);
        }
        if matches!(
            phase,
            RelocationPhase::TargetPublished | RelocationPhase::Ready | RelocationPhase::Committed
        ) {
            create_valid_target(temp.path(), &journal);
        }
        super::journal::write(temp.path(), &journal, None).unwrap();
        RelocationStorage::new(temp.path().into())
            .recover_all()
            .unwrap();
        let target = matches!(phase, RelocationPhase::Ready | RelocationPhase::Committed);
        assert_eq!(session_dir(temp.path(), "/source", "r").exists(), !target);
        assert_eq!(session_dir(temp.path(), "/target", "r").exists(), target);
        assert!(!journal::journal_path(temp.path(), "r").exists());
    }
}

#[test]
fn storage_view_follows_journal_authority_and_fails_closed_without_it() {
    for phase in [
        RelocationPhase::Prepared,
        RelocationPhase::Staged,
        RelocationPhase::TargetPublished,
        RelocationPhase::Ready,
        RelocationPhase::Committed,
        RelocationPhase::RolledBack,
    ] {
        let temp = tempfile::tempdir().unwrap();
        create_source(temp.path(), "/source", "session", 0);
        let journal = RelocationJournal::test_new("session", "/source", "/target", phase);
        create_valid_target(temp.path(), &journal);
        super::journal::write(temp.path(), &journal, None).unwrap();
        let expected_cwd = if matches!(phase, RelocationPhase::Ready | RelocationPhase::Committed) {
            "/target"
        } else {
            "/source"
        };
        assert_eq!(
            RelocationView::load(temp.path())
                .unwrap()
                .find_persisted_session_dir("session")
                .unwrap(),
            Some(session_dir(temp.path(), expected_cwd, "session"))
        );
    }

    for phase in [RelocationPhase::TargetPublished, RelocationPhase::Ready] {
        let temp = tempfile::tempdir().unwrap();
        let journal = RelocationJournal::test_new("missing", "/source", "/target", phase);
        if phase == RelocationPhase::TargetPublished {
            create_valid_target(temp.path(), &journal);
        } else {
            create_source(temp.path(), "/source", "missing", 0);
        }
        super::journal::write(temp.path(), &journal, None).unwrap();
        let view = RelocationView::load(temp.path()).unwrap();
        assert!(view.session_dirs(None).is_err());
        assert!(view.find_persisted_session_dir("missing").is_err());
        assert!(super::super::load_updates_for_replay_at("missing", temp.path()).is_err());
    }

    let temp = tempfile::tempdir().unwrap();
    let source = create_source(temp.path(), "/source", "linked", 0);
    let summary = source.join(super::super::SUMMARY_FILE);
    fs::rename(&summary, source.join("real-summary")).unwrap();
    std::os::unix::fs::symlink("real-summary", &summary).unwrap();
    let journal = RelocationJournal::test_new(
        "linked",
        "/source",
        "/target",
        RelocationPhase::TargetPublished,
    );
    super::journal::write(temp.path(), &journal, None).unwrap();
    assert!(
        RelocationView::load(temp.path())
            .unwrap()
            .session_dirs(None)
            .is_err()
    );

    let temp = tempfile::tempdir().unwrap();
    create_source(temp.path(), "/a", "card", 0);
    fs::create_dir_all(session_dir(temp.path(), "/b", "card").join("images")).unwrap();
    let view = RelocationView::load(temp.path()).unwrap();
    assert_eq!(
        view.find_persisted_session_dir("card").unwrap(),
        Some(session_dir(temp.path(), "/a", "card"))
    );
    create_source(temp.path(), "/b", "card", 0);
    assert_eq!(
        RelocationView::load(temp.path())
            .unwrap()
            .find_persisted_session_dir("card")
            .unwrap(),
        None
    );
    fs::create_dir_all(session_dir(temp.path(), "/a", ".hidden")).unwrap();
    assert!(
        RelocationView::load(temp.path())
            .unwrap()
            .session_dirs(None)
            .unwrap()
            .iter()
            .all(|path| !path.file_name().unwrap().to_string_lossy().starts_with('.'))
    );
}

#[test]
fn hinted_replay_skips_non_authoritative_source_while_journal_exists() {
    let temp = tempfile::tempdir().unwrap();
    let sid = "reloc-child";
    let source = create_source(temp.path(), "/source", sid, 0);
    let journal = RelocationJournal::test_new(sid, "/source", "/target", RelocationPhase::Ready);
    let target = create_valid_target(temp.path(), &journal);
    super::journal::write(temp.path(), &journal, None).unwrap();

    let source_line = r#"{"timestamp":1,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"source-transcript"}}}}"#;
    let target_line = r#"{"timestamp":1,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"target-transcript"}}}}"#;
    fs::write(
        source.join(super::super::UPDATES_FILE),
        format!("{source_line}\n"),
    )
    .unwrap();
    fs::write(
        target.join(super::super::UPDATES_FILE),
        format!("{target_line}\n"),
    )
    .unwrap();

    let mut texts = Vec::new();
    let emission =
        super::super::stream_replay_updates_at_hinted(
            sid,
            temp.path(),
            super::super::ReplayPathHint {
                parent_cwd: Some(Path::new("/source")),
                child_cwd: None,
                ..Default::default()
            },
            |update| {
                if let super::super::ReplayedUpdate::Acp(
                    acp::SessionUpdate::UserMessageChunk(chunk),
                    _,
                ) = update
                    && let acp::ContentBlock::Text(text) = chunk.content
                {
                    texts.push(text.text);
                }
            },
        )
        .unwrap();
    assert_eq!(emission, super::super::ReplayEmission::Emitted);
    assert_eq!(
        texts,
        vec!["target-transcript".to_string()],
        "Ready journal must win over the parent-cwd/sibling fast path"
    );
}

#[test]
fn cwd_scoped_storage_view_ignores_unrelated_malformed_authority() {
    let temp = tempfile::tempdir().unwrap();
    let requested = create_source(temp.path(), "/requested", "requested", 0);
    let unrelated = RelocationJournal::test_new(
        "unrelated",
        "/other-source",
        "/other-target",
        RelocationPhase::Ready,
    );
    create_source(temp.path(), "/other-source", "unrelated", 0);
    super::journal::write(temp.path(), &unrelated, None).unwrap();

    let view = RelocationView::load(temp.path()).unwrap();
    assert_eq!(
        view.session_dirs(Some("/requested")).unwrap(),
        vec![requested]
    );
    assert!(view.session_dirs(None).is_err());
}

#[test]
fn cwd_scoped_storage_view_fails_for_missing_authority_in_requested_cwd() {
    let temp = tempfile::tempdir().unwrap();
    create_source(temp.path(), "/other", "requested", 0);
    let requested =
        RelocationJournal::test_new("requested", "/source", "/requested", RelocationPhase::Ready);
    super::journal::write(temp.path(), &requested, None).unwrap();

    let view = RelocationView::load(temp.path()).unwrap();
    assert!(view.session_dirs(Some("/requested")).is_err());
    assert!(view.session_dirs(Some("/other")).unwrap().is_empty());
}

#[test]
fn long_cwd_marker_publication_is_atomic_and_retryable() {
    let target = format!("/{}", "long-segment/".repeat(40));
    for fault in [
        AtomicWriteFault::BeforeRename,
        AtomicWriteFault::AfterRename,
    ] {
        let temp = tempfile::tempdir().unwrap();
        create_source(temp.path(), "/source", "marker", 0);
        let base = RelocationStorage::new(temp.path().into());
        let lease = base.acquire("marker").unwrap();
        let faulted =
            RelocationStorage::with_fault(temp.path().into(), TestFault::CwdMarker(fault));
        assert!(
            faulted
                .stage_and_publish(&lease, request("marker", "/source", &target, 1))
                .is_err()
        );
        let marker = base.target_parent(&target).join(".cwd");
        if fault == AtomicWriteFault::BeforeRename {
            assert!(!marker.exists());
        } else {
            assert_eq!(fs::read_to_string(&marker).unwrap(), target);
        }
        base.stage_and_publish(&lease, request("marker", "/source", &target, 1))
            .unwrap();
        assert_eq!(fs::read_to_string(marker).unwrap(), target);
    }
}
