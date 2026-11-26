use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use super::transaction::{TransactionObserver, TransactionPhase};
use super::*;

fn request(path: &Path, items: &[(&str, &str)]) -> ManagedConfigRequest {
    ManagedConfigRequest {
        path: path.to_path_buf(),
        namespace: "echoagent doctor".to_owned(),
        owned_item_prefix: "terminal.".to_owned(),
        items: items
            .iter()
            .map(|(name, body)| {
                let name = if name.starts_with("terminal.") {
                    (*name).to_owned()
                } else {
                    format!("terminal.{name}")
                };
                ManagedItem::new(name, *body)
            })
            .collect(),
        comments: CommentSyntax::hash(),
        validator: None,
    }
}

fn expected(body: &str, newline: &str) -> String {
    [
        "# >>> echoagent doctor >>>",
        "# >>> terminal.ssh-wrap >>>",
        body,
        "# <<< terminal.ssh-wrap <<<",
        "# <<< echoagent doctor <<<",
    ]
    .join(newline)
}

fn artifacts(directory: &Path) -> HashSet<String> {
    fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".echo-agent-"))
        .collect()
}

#[test]
fn missing_empty_normal_no_final_newline_and_crlf_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing.rc");
    let plan = ManagedConfig::plan(request(
        &missing,
        &[("terminal.ssh-wrap", "alias ssh='echoagent wrap ssh'")],
    ))
    .unwrap();
    assert_eq!(
        plan.updated_bytes(),
        expected("alias ssh='echoagent wrap ssh'", "\n").as_bytes()
    );
    assert!(plan.backup_path_hint().is_none());
    ManagedConfig::apply(plan).unwrap();
    assert_eq!(
        fs::read_to_string(&missing).unwrap(),
        expected("alias ssh='echoagent wrap ssh'", "\n")
    );

    let empty = temp.path().join("empty.rc");
    fs::write(&empty, "").unwrap();
    let plan = ManagedConfig::plan(request(
        &empty,
        &[("terminal.ssh-wrap", "alias ssh='echoagent wrap ssh'")],
    ))
    .unwrap();
    assert!(plan.backup_path_hint().is_some());
    ManagedConfig::apply(plan).unwrap();

    let normal = temp.path().join("normal.rc");
    fs::write(&normal, "export KEEP=1\n").unwrap();
    let plan = ManagedConfig::plan(request(
        &normal,
        &[("terminal.ssh-wrap", "alias ssh='echoagent wrap ssh'")],
    ))
    .unwrap();
    assert_eq!(
        String::from_utf8(plan.updated_bytes().to_vec()).unwrap(),
        format!(
            "export KEEP=1\n{}\n",
            expected("alias ssh='echoagent wrap ssh'", "\n")
        )
    );

    let no_final = temp.path().join("no-final.rc");
    fs::write(&no_final, "export KEEP=1").unwrap();
    let plan = ManagedConfig::plan(request(&no_final, &[("item", "body")])).unwrap();
    assert!(
        !String::from_utf8(plan.updated_bytes().to_vec())
            .unwrap()
            .ends_with('\n')
    );

    let crlf = temp.path().join("crlf.rc");
    fs::write(&crlf, b"set -x KEEP 1\r\n").unwrap();
    let plan = ManagedConfig::plan(request(&crlf, &[("item", "body")])).unwrap();
    let rendered = String::from_utf8(plan.updated_bytes().to_vec()).unwrap();
    assert!(!rendered.replace("\r\n", "").contains('\n'));
}

#[test]
fn typed_inspection_and_item_updates_share_one_validated_parse() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.rc");
    fs::write(
        &path,
        "before\n# >>> echoagent doctor >>>\n# >>> terminal.old >>>\nold\n# <<< terminal.old <<<\n# <<< echoagent doctor <<<\nafter\n",
    )
    .unwrap();
    let plan = ManagedConfig::plan(request(&path, &[("new", "new body")])).unwrap();
    let original = fs::read_to_string(&path).unwrap();
    assert_eq!(plan.inspection().original_text(), Some(original.as_str()));
    assert_eq!(plan.inspection().unmanaged_text(), "before\nafter\n");
    let block = plan.managed_block().unwrap();
    assert!(block.contains("# >>> terminal.old >>>\nold\n# <<< terminal.old <<<"));
    assert!(block.contains("# >>> terminal.new >>>\nnew body\n# <<< terminal.new <<<"));
    ManagedConfig::apply(plan).unwrap();

    let plan = ManagedConfig::plan(request(&path, &[("old", "replaced")])).unwrap();
    let rendered = String::from_utf8(plan.updated_bytes().to_vec()).unwrap();
    assert!(rendered.contains("# >>> terminal.old >>>\nreplaced\n# <<< terminal.old <<<"));
    assert!(rendered.starts_with("before\n"));
    assert!(rendered.ends_with("after\n"));
}

#[test]
fn prose_and_exports_with_owned_words_and_chevrons_are_inert() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("inert");
    let content = [
        "# Terminal.app note: terminal. support >>> may vary <<< by host",
        "# echoagent doctor docs say >>> run this later <<<",
        "export NOTE='terminal.ssh-wrap >>> not a marker'",
        "printf '%s\\n' 'echoagent doctor <<< prose >>>'",
        "#terminal.future prose >>> lacks marker grammar",
        "echo '# >>> terminal.future >>> embedded text'",
    ]
    .join("\n");
    fs::write(&path, &content).unwrap();
    let plan = ManagedConfig::plan(request(&path, &[("terminal.current", "body")])).unwrap();
    assert!(String::from_utf8_lossy(plan.updated_bytes()).starts_with(&content));
}

#[test]
fn malformed_structural_owned_near_markers_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    for (index, content) in [
        "# >>> terminal.future >>\n",
        "# <<< terminal.future <<\n",
        "#   >>> terminal.future >> extra\n",
        "#\t<<< terminal.future <<< extra\n",
        "# >>> echoagent doctor >>\n",
    ]
    .iter()
    .enumerate()
    {
        let path = temp.path().join(format!("near-{index}"));
        fs::write(&path, content).unwrap();
        assert!(matches!(
            ManagedConfig::plan(request(&path, &[("terminal.current", "body")])),
            Err(ManagedConfigError::InvalidMarkers { .. })
        ));
    }
}

#[test]
fn owned_future_markers_are_rejected_independent_of_requested_items() {
    let temp = tempfile::tempdir().unwrap();
    for (index, content) in [
        "# >>> terminal.future >>>\nbody\n# <<< terminal.future <<<\n",
        "# >>> echoagent doctor >>>\n# >>> terminal.current >>>\nbody\n# <<< terminal.current <<<\n# <<< echoagent doctor <<<\n# >>> terminal.future >>>\nbody\n# <<< terminal.future <<<\n",
    ]
    .iter()
    .enumerate()
    {
        let path = temp.path().join(format!("future-{index}"));
        fs::write(&path, content).unwrap();
        assert!(matches!(
            ManagedConfig::plan(request(&path, &[("terminal.current", "body")])),
            Err(ManagedConfigError::InvalidMarkers { .. })
        ));
    }

    let unrelated = temp.path().join("unrelated");
    fs::write(
        &unrelated,
        "# >>> user custom >>>\nnot ours\n# <<< user custom <<<\n",
    )
    .unwrap();
    assert!(ManagedConfig::plan(request(&unrelated, &[("terminal.current", "body")])).is_ok());
}

#[test]
fn exact_noop_creates_no_transaction_artifacts_or_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.rc");
    let content = expected("body", "\n");
    fs::write(&path, &content).unwrap();
    let before = fs::metadata(&path).unwrap().modified().unwrap();
    let outcome = ManagedConfig::apply(
        ManagedConfig::plan(request(&path, &[("terminal.ssh-wrap", "body")])).unwrap(),
    )
    .unwrap();
    assert_eq!(outcome.status, ManagedConfigStatus::NoChange);
    assert_eq!(fs::read_to_string(&path).unwrap(), content);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), before);
    assert!(artifacts(temp.path()).is_empty());
}

#[test]
fn invalid_inputs_and_all_marker_shapes_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let oversize = temp.path().join("oversize");
    fs::write(
        &oversize,
        vec![b'x'; super::source::MAX_CONFIG_BYTES as usize + 1],
    )
    .unwrap();
    let nul = temp.path().join("nul");
    fs::write(&nul, b"a\0b").unwrap();
    let non_utf8 = temp.path().join("non-utf8");
    fs::write(&non_utf8, [0xff]).unwrap();
    for path in [&oversize, &nul, &non_utf8] {
        assert!(matches!(
            ManagedConfig::plan(request(path, &[("item", "body")])),
            Err(ManagedConfigError::UnsafePath { .. })
        ));
    }

    let cases = [
        "# >>> echoagent doctor >>>\n",
        "# <<< echoagent doctor <<<\n# >>> echoagent doctor >>>\n",
        "# >>> echoagent doctor >>\n",
        "# >>> echoagent doctor >>>\nraw\n# <<< echoagent doctor <<<\n",
        "# >>> echoagent doctor >>>\n# <<< terminal.item <<<\n# <<< echoagent doctor <<<\n",
        "# >>> echoagent doctor >>>\n# >>> terminal.item >>>\nbody\n# <<< terminal.other <<<\n# <<< echoagent doctor <<<\n",
        "# >>> echoagent doctor >>>\n# >>> terminal.item >>>\nbody\n# <<< terminal.item <<<\n# >>> terminal.item >>>\nbody\n# <<< terminal.item <<<\n# <<< echoagent doctor <<<\n",
        "# >>> terminal.item >>>\nbody\n# <<< terminal.item <<<\n",
    ];
    for (index, content) in cases.iter().enumerate() {
        let path = temp.path().join(format!("marker-{index}"));
        fs::write(&path, content).unwrap();
        assert!(matches!(
            ManagedConfig::plan(request(&path, &[("item", "new")])),
            Err(ManagedConfigError::InvalidMarkers { .. })
        ));
    }
}

#[cfg(unix)]
#[test]
fn symlink_resolution_depth_cycles_and_parent_symlinks_are_refused() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let physical = temp.path().join("physical");
    fs::write(&physical, "keep\n").unwrap();
    let relative = temp.path().join("relative");
    symlink("physical", &relative).unwrap();
    let plan = ManagedConfig::plan(request(&relative, &[("item", "body")])).unwrap();
    assert_eq!(
        plan.target_path(),
        fs::canonicalize(&physical).unwrap().as_path()
    );
    ManagedConfig::apply(plan).unwrap();
    assert!(
        fs::symlink_metadata(&relative)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let cycle_a = temp.path().join("cycle-a");
    let cycle_b = temp.path().join("cycle-b");
    symlink("cycle-b", &cycle_a).unwrap();
    symlink("cycle-a", &cycle_b).unwrap();
    assert!(ManagedConfig::plan(request(&cycle_a, &[("item", "body")])).is_err());

    let mut last = temp.path().join("depth-target");
    fs::write(&last, "body").unwrap();
    for index in 0..=super::source::MAX_SYMLINKS {
        let next = temp.path().join(format!("depth-{index}"));
        symlink(&last, &next).unwrap();
        last = next;
    }
    assert!(ManagedConfig::plan(request(&last, &[("item", "body")])).is_err());

    let real_parent = temp.path().join("real-parent");
    fs::create_dir(&real_parent).unwrap();
    let linked_parent = temp.path().join("linked-parent");
    symlink(&real_parent, &linked_parent).unwrap();
    let plan =
        ManagedConfig::plan(request(&linked_parent.join("rc"), &[("item", "body")])).unwrap();
    assert_eq!(
        plan.target_path().parent(),
        Some(fs::canonicalize(&real_parent).unwrap().as_path())
    );
}

#[cfg(unix)]
#[test]
fn bytes_mode_and_actual_backup_are_exact() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.rc");
    let original = b"export KEEP=1\r\n";
    fs::write(&path, original).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let plan = ManagedConfig::plan(request(&path, &[("item", "body")])).unwrap();
    let hint = plan.backup_path_hint().unwrap().to_path_buf();
    let outcome = ManagedConfig::apply(plan).unwrap();
    let backup = outcome.backup_path.unwrap();
    assert_eq!(backup, hint);
    assert_eq!(fs::read(&backup).unwrap(), original);
    assert_eq!(
        fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn stale_source_and_parent_swap_are_rejected_before_publication() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("parent/config.rc");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "before\n").unwrap();
    let plan = ManagedConfig::plan(request(&path, &[("item", "body")])).unwrap();
    fs::write(&path, "changed\n").unwrap();
    assert!(matches!(
        ManagedConfig::apply(plan),
        Err(ManagedConfigError::StalePlan(_))
    ));

    let plan = ManagedConfig::plan(request(&path, &[("item", "body")])).unwrap();
    let old_parent = temp.path().join("old-parent");
    fs::rename(path.parent().unwrap(), &old_parent).unwrap();
    fs::create_dir(path.parent().unwrap()).unwrap();
    assert!(matches!(
        ManagedConfig::apply(plan),
        Err(ManagedConfigError::ParentChanged(_))
    ));
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn missing_parent_revalidation_rejects_new_symlink_component() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(temp.path()).unwrap();
    let path = root.join("missing/child/config.rc");
    let parent_plan = super::source::ParentPlan::capture(path.parent().unwrap()).unwrap();
    let target = root.join("redirected");
    fs::create_dir(&target).unwrap();
    fs::create_dir(root.join("missing")).unwrap();
    symlink(&target, root.join("missing/child")).unwrap();

    assert!(matches!(
        parent_plan.ensure_and_anchor(),
        Err(ManagedConfigError::UnsafePath { .. })
    ));
}

#[test]
fn backup_and_temp_hint_collisions_retry_under_lock() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.rc");
    fs::write(&path, "original\n").unwrap();
    let plan = ManagedConfig::plan(request(&path, &[("item", "body")])).unwrap();
    let backup_hint = plan.backup_path_hint().unwrap().to_path_buf();
    let temp_hint = plan.temp_path_hint.as_ref().unwrap().clone();
    fs::write(&backup_hint, "unrelated backup").unwrap();
    fs::write(&temp_hint, "unrelated temp").unwrap();
    let outcome = ManagedConfig::apply(plan).unwrap();
    assert_ne!(outcome.backup_path.as_deref(), Some(backup_hint.as_path()));
    assert_eq!(
        fs::read_to_string(&backup_hint).unwrap(),
        "unrelated backup"
    );
    assert_eq!(fs::read_to_string(&temp_hint).unwrap(), "unrelated temp");
}

struct FailAt(TransactionPhase);
impl TransactionObserver for FailAt {
    fn phase(&self, phase: TransactionPhase, _: &ManagedConfigPlan) -> io::Result<()> {
        if phase == self.0 {
            Err(io::Error::other(format!("injected {}", phase.name())))
        } else {
            Ok(())
        }
    }
}

struct CorruptTemp;
impl TransactionObserver for CorruptTemp {
    fn mutate_written_temp(&self, path: &Path, _: &ManagedConfigPlan) -> io::Result<()> {
        fs::write(path, "corrupt")
    }
}

#[test]
fn all_precommit_phase_failures_cleanup_and_preserve_original() {
    let phases = [
        TransactionPhase::BeforeBackupReserve,
        TransactionPhase::AfterBackupReserved,
        TransactionPhase::BeforeTempReserve,
        TransactionPhase::BeforeTempWrite,
        TransactionPhase::AfterTempWritten,
        TransactionPhase::AfterValidation,
        TransactionPhase::BeforePublish,
    ];
    for phase in phases {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.rc");
        fs::write(&path, "original\n").unwrap();
        let plan = ManagedConfig::plan(request(&path, &[("item", "body")])).unwrap();
        assert!(matches!(
            ManagedConfig::apply_with_observer(plan, &FailAt(phase)),
            Err(ManagedConfigError::Phase { .. })
        ));
        assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
        assert!(artifacts(temp.path()).is_empty());
    }
}

#[test]
fn post_publish_failures_rollback_existing_and_remove_new_target() {
    let phases = [
        TransactionPhase::AfterPublish,
        TransactionPhase::BeforeParentSync,
        TransactionPhase::AfterParentSync,
        TransactionPhase::BeforeVerify,
    ];
    for phase in phases {
        for existing in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("config.rc");
            if existing {
                fs::write(&path, "original\n").unwrap();
            }
            let plan = ManagedConfig::plan(request(&path, &[("item", "body")])).unwrap();
            assert!(ManagedConfig::apply_with_observer(plan, &FailAt(phase)).is_err());
            if existing {
                assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
            } else {
                assert!(!path.exists());
            }
            assert!(artifacts(temp.path()).is_empty());
        }
    }
}

#[test]
fn verification_failure_rolls_back_exact_original() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.rc");
    fs::write(&path, "original\n").unwrap();
    let plan = ManagedConfig::plan(request(&path, &[("item", "body")])).unwrap();
    let error = ManagedConfig::apply_with_observer(plan, &CorruptTemp).unwrap_err();
    assert!(matches!(error, ManagedConfigError::Verification { .. }));
    assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
    assert!(artifacts(temp.path()).is_empty());
}

#[test]
fn primary_and_rollback_errors_are_both_reported() {
    struct FailBoth;
    impl TransactionObserver for FailBoth {
        fn phase(&self, phase: TransactionPhase, _: &ManagedConfigPlan) -> io::Result<()> {
            if matches!(
                phase,
                TransactionPhase::AfterPublish | TransactionPhase::BeforeRollback
            ) {
                Err(io::Error::other("injected failure"))
            } else {
                Ok(())
            }
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.rc");
    fs::write(&path, "original\n").unwrap();
    let plan = ManagedConfig::plan(request(&path, &[("item", "body")])).unwrap();
    assert!(matches!(
        ManagedConfig::apply_with_observer(plan, &FailBoth),
        Err(ManagedConfigError::Recovery { .. })
    ));
}

#[test]
fn publish_and_parent_sync_failures_are_injected_at_the_real_operations() {
    struct PublishFailure;
    impl TransactionObserver for PublishFailure {
        fn publish(&self, _: &Path, _: &Path) -> io::Result<()> {
            Err(io::Error::other("injected publish failure"))
        }
    }
    struct SyncFailure;
    impl TransactionObserver for SyncFailure {
        fn sync_parent(
            &self,
            parent: &super::source::ParentAnchor,
            rollback: bool,
        ) -> Result<(), ManagedConfigError> {
            if rollback {
                parent.sync()
            } else {
                Err(ManagedConfigError::Sync {
                    path: Path::new("injected-parent").to_path_buf(),
                    source: io::Error::other("injected sync failure"),
                })
            }
        }
    }

    for observer in [
        &PublishFailure as &dyn TransactionObserver,
        &SyncFailure as &dyn TransactionObserver,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.rc");
        fs::write(&path, "original\n").unwrap();
        let plan = ManagedConfig::plan(request(&path, &[("item", "body")])).unwrap();
        assert!(ManagedConfig::apply_with_observer(plan, observer).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
        assert!(artifacts(temp.path()).is_empty());
    }
}

#[test]
fn failed_validator_cleans_reserved_backup_and_temp() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.rc");
    fs::write(&path, "original\n").unwrap();
    let mut request = request(&path, &[("item", "body")]);
    request.validator = Some(SyntaxValidator {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "exit 7".into()],
        timeout: Duration::from_secs(1),
    });
    assert!(matches!(
        ManagedConfig::apply(ManagedConfig::plan(request).unwrap()),
        Err(ManagedConfigError::Validation { .. })
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
    assert!(artifacts(temp.path()).is_empty());
}

#[test]
fn transaction_lock_blocks_second_apply_then_stale_revalidation_wins() {
    struct BlockAfterLock {
        reached: Arc<Barrier>,
        release: Arc<Barrier>,
    }
    impl TransactionObserver for BlockAfterLock {
        fn phase(&self, phase: TransactionPhase, _: &ManagedConfigPlan) -> io::Result<()> {
            if phase == TransactionPhase::AfterLock {
                self.reached.wait();
                self.release.wait();
            }
            Ok(())
        }
    }

    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.rc");
    fs::write(&path, "original\n").unwrap();
    let first = ManagedConfig::plan(request(&path, &[("one", "one")])).unwrap();
    let second = ManagedConfig::plan(request(&path, &[("two", "two")])).unwrap();
    let reached = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let observer = BlockAfterLock {
        reached: reached.clone(),
        release: release.clone(),
    };
    let first_thread =
        std::thread::spawn(move || ManagedConfig::apply_with_observer(first, &observer));
    reached.wait();

    let result = Arc::new(Mutex::new(None));
    let result_thread = result.clone();
    let second_thread = std::thread::spawn(move || {
        *result_thread.lock().unwrap() = Some(ManagedConfig::apply(second));
    });
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        result.lock().unwrap().is_none(),
        "second apply must block on lock"
    );
    release.wait();
    assert!(first_thread.join().unwrap().is_ok());
    second_thread.join().unwrap();
    assert!(matches!(
        result.lock().unwrap().take().unwrap(),
        Err(ManagedConfigError::StalePlan(_))
    ));
}

#[test]
fn validator_timeout_is_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.rc");
    fs::write(&path, "original\n").unwrap();
    let mut request = request(&path, &[("item", "body")]);
    request.validator = Some(SyntaxValidator {
        program: "/bin/sh".into(),
        args: vec!["-c".into(), "sleep 5".into()],
        timeout: Duration::from_millis(20),
    });
    let started = Instant::now();
    assert!(ManagedConfig::apply(ManagedConfig::plan(request).unwrap()).is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(fs::read_to_string(&path).unwrap(), "original\n");
}
