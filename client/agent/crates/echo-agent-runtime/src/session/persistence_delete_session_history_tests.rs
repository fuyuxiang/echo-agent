use super::{DeleteSessionError, SessionDeletion, classify_remote_delete};
use crate::remote::client::BackendError;

#[test]
fn deletion_paths_include_corrupt_and_missing_summaries_without_catalog_limits() {
    let home = tempfile::tempdir().unwrap();
    let root = home.path().join("sessions");
    let first = root.join("workspace-a").join("owned-id");
    let second = root.join("workspace-b").join("owned-id");
    let other = root.join("workspace-b").join("other-id");
    for path in [&first, &second, &other] {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("updates.jsonl"), "private history").unwrap();
    }
    std::fs::write(first.join("summary.json"), "{broken").unwrap();
    let paths = super::session_history_paths_in(&root, "owned-id", None).unwrap();
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&first) && paths.contains(&second));
    assert!(super::session_history_paths_in(&root, "../other-id", None).is_err());
    assert!(
        super::session_history_paths_in(&root, "missing-id", None)
            .unwrap()
            .is_empty()
    );
    #[cfg(unix)]
    {
        let linked = root.join("workspace-c");
        std::fs::create_dir(&linked).unwrap();
        std::os::unix::fs::symlink(&other, linked.join("owned-id")).unwrap();
        assert!(super::session_history_paths_in(&root, "owned-id", None).is_err());
        assert!(other.join("updates.jsonl").exists());
    }
}

#[test]
fn remote_ok_reports_removed() {
    assert!(
        classify_remote_delete(Ok(())).unwrap(),
        "a 2xx delete must report that a remote copy was removed"
    );
}

#[test]
fn remote_404_is_treated_as_already_deleted() {
    let removed = classify_remote_delete(Err(BackendError::RequestFailed {
        status: 404,
        body: "not found".into(),
    }))
    .expect("a 404 means the remote copy is gone — deletion must stay idempotent");
    assert!(
        !removed,
        "a 404 must report that nothing was removed remotely"
    );
}

#[test]
fn remote_non_404_request_failure_aborts() {
    let res = classify_remote_delete(Err(BackendError::RequestFailed {
        status: 500,
        body: "boom".into(),
    }));
    assert!(matches!(res, Err(DeleteSessionError::Remote(_))));
}

#[test]
fn remote_auth_failure_aborts() {
    let res = classify_remote_delete(Err(BackendError::Auth("denied".into())));
    assert!(matches!(res, Err(DeleteSessionError::Remote(_))));
}

#[test]
fn any_removed_reflects_either_location() {
    assert!(!SessionDeletion::default().any_removed());
    assert!(
        SessionDeletion {
            local_removed: true,
            remote_removed: false,
        }
        .any_removed()
    );
    assert!(
        SessionDeletion {
            local_removed: false,
            remote_removed: true,
        }
        .any_removed(),
        "a remote-only delete must count as removed"
    );
}
