//! Publish (strict) — the merge-or-fail publish flow.
//!
//! `Publish(app)` = commit if dirty → `MergeToMain(conv → main)` → on success
//! build/deploy from the merge SHA and record it; on merge failure return an
//! error and **do not deploy**. No silent force-push, no deploy
//! off an unmerged conv branch.
//!
//! The flow is expressed over the [`PublishBackend`] trait so it is unit-testable
//! without a live workspace/deployer; the production adapter wires each method to
//! the corresponding WorkspaceOp (`Commit`, `MergeToMain`) and the app deployer.

use async_trait::async_trait;

pub use echo_agent_workspace_types::rpc::git::GitMergeToMainOutcome;

/// The result of a `MergeToMain` step, decoupled from the wire type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
    /// The conv branch merged (or fast-forwarded) into the target; `sha` is the
    /// new target HEAD to build from.
    Merged { sha: String },
    /// The conv branch was already merged; `sha` is the current target HEAD.
    UpToDate { sha: String },
    /// The merge hit conflicts; publish must not deploy.
    Conflicts { files: Vec<String> },
}

impl From<GitMergeToMainOutcome> for MergeResult {
    fn from(outcome: GitMergeToMainOutcome) -> Self {
        match outcome {
            GitMergeToMainOutcome::Merged { sha } => MergeResult::Merged { sha },
            GitMergeToMainOutcome::UpToDate { sha } => MergeResult::UpToDate { sha },
            GitMergeToMainOutcome::Conflicts { files } => MergeResult::Conflicts { files },
        }
    }
}

/// The successful result of a publish: the merge SHA that was built and the
/// deployment that references it (every deploy records the SHA it was built
/// from).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishOutcome {
    pub merge_sha: String,
    pub deployment_id: String,
}

/// Why a publish did not deploy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    /// A pre-deploy git/commit step failed.
    #[error("commit before publish failed: {0}")]
    Commit(String),
    /// The merge into the target branch hit conflicts. The conversation should
    /// be parked in a "needs resolution" FE state; nothing is deployed.
    #[error("merge into '{target}' has conflicts in {} file(s); not deployed", .files.len())]
    MergeConflict { target: String, files: Vec<String> },
    /// The merge failed for a non-conflict reason (e.g. remote unavailable).
    #[error("merge into '{target}' failed: {message}")]
    MergeFailed { target: String, message: String },
    /// A backend/status probe failed before any deploy could happen.
    #[error("publish precondition failed: {0}")]
    Precondition(String),
    /// Merge succeeded but the deploy step failed. The merge is durable on the
    /// target branch (git = source of truth); a retry can re-deploy the SHA.
    #[error("deploy of merge sha {sha} failed: {message}")]
    Deploy { sha: String, message: String },
}

/// The operations `publish` drives. Each maps to an explicit platform op — the
/// agent never performs these autonomously.
#[async_trait]
pub trait PublishBackend {
    /// Is the conv-branch working tree dirty (uncommitted changes)?
    async fn is_dirty(&self) -> Result<bool, PublishError>;
    /// Commit the working tree onto the conv branch. Returns the new HEAD sha.
    async fn commit(&self, message: &str) -> Result<Option<String>, PublishError>;
    /// Merge the conv branch into `target` (never rebase, never force). `push`
    /// requires the merged target to be delivered to the durable remote before
    /// returning success — so a deployed SHA is always on `origin`, never
    /// local-only (invariants #1/#3).
    async fn merge_to_main(
        &self,
        conv_branch: &str,
        target: &str,
        push: bool,
    ) -> Result<MergeResult, PublishError>;
    /// Build and deploy from `merge_sha`; returns the deployment id.
    async fn deploy(&self, merge_sha: &str) -> Result<String, PublishError>;
}

/// Run the strict publish flow. Commits the dirty tree (if any), merges the conv
/// branch into `target`, pushes the target to the durable remote, and only then
/// builds/deploys from the merge SHA. A conflict or merge failure returns an
/// error and never deploys.
///
/// The caller must first ensure the workspace is on the conversation branch tip
/// (via `EnsureBinding`, restoring the resume snapshot if needed) — this flow
/// operates on the current working tree and does not resolve the branch itself.
pub async fn publish<B: PublishBackend + ?Sized>(
    backend: &B,
    conv_branch: &str,
    target: &str,
    commit_message: &str,
) -> Result<PublishOutcome, PublishError> {
    // 1. Commit if dirty (explicit WorkspaceOp; publish forces one).
    if backend.is_dirty().await? {
        backend.commit(commit_message).await?;
    }

    // 2. MergeToMain (with push) — the only path that writes the target branch.
    // Push is required so the deployed SHA is on the durable remote, not local.
    let merge_sha = match backend.merge_to_main(conv_branch, target, true).await? {
        MergeResult::Merged { sha } | MergeResult::UpToDate { sha } => sha,
        MergeResult::Conflicts { files } => {
            // Merge or fail: no deploy off an unmerged conv branch.
            return Err(PublishError::MergeConflict {
                target: target.to_owned(),
                files,
            });
        }
    };

    // 3. Build/deploy from the pushed merge SHA and record it.
    let deployment_id = backend.deploy(&merge_sha).await?;
    Ok(PublishOutcome {
        merge_sha,
        deployment_id,
    })
}

/// Multi-repo publish: commit + dry-run merge on **every** repo first, then
/// serial merge+push+deploy. A dry-run conflict aborts before any remote push.
///
/// `push = false` on the dry-run must not update the durable remote. Implementers
/// that cannot preview without mutating local `main` should reset after the probe.
pub async fn publish_serial_after_dry_run<B: PublishBackend + ?Sized>(
    backends: &[&B],
    conv_branch: &str,
    target: &str,
    commit_message: &str,
) -> Result<Vec<PublishOutcome>, PublishError> {
    if backends.is_empty() {
        return Err(PublishError::Precondition(
            "publish_serial_after_dry_run requires at least one repo".to_owned(),
        ));
    }

    for backend in backends {
        if backend.is_dirty().await? {
            backend.commit(commit_message).await?;
        }
    }

    for backend in backends {
        match backend
            .merge_to_main(conv_branch, target, /* push */ false)
            .await?
        {
            MergeResult::Conflicts { files } => {
                return Err(PublishError::MergeConflict {
                    target: target.to_owned(),
                    files,
                });
            }
            MergeResult::Merged { .. } | MergeResult::UpToDate { .. } => {}
        }
    }

    let mut outcomes = Vec::with_capacity(backends.len());
    for backend in backends {
        let merge_sha = match backend
            .merge_to_main(conv_branch, target, /* push */ true)
            .await?
        {
            MergeResult::Merged { sha } | MergeResult::UpToDate { sha } => sha,
            MergeResult::Conflicts { files } => {
                return Err(PublishError::MergeConflict {
                    target: target.to_owned(),
                    files,
                });
            }
        };
        let deployment_id = backend.deploy(&merge_sha).await?;
        outcomes.push(PublishOutcome {
            merge_sha,
            deployment_id,
        });
    }
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Calls {
        committed: Vec<String>,
        merged: bool,
        merge_push: Option<bool>,
        merge_calls: Vec<bool>,
        deployed_sha: Option<String>,
    }

    struct MockBackend {
        dirty: bool,
        merge: Result<MergeResult, PublishError>,
        deploy: Result<String, PublishError>,
        calls: Mutex<Calls>,
    }

    impl MockBackend {
        fn new(dirty: bool, merge: MergeResult) -> Self {
            Self {
                dirty,
                merge: Ok(merge),
                deploy: Ok("dep-1".to_owned()),
                calls: Mutex::new(Calls::default()),
            }
        }
    }

    #[async_trait]
    impl PublishBackend for MockBackend {
        async fn is_dirty(&self) -> Result<bool, PublishError> {
            Ok(self.dirty)
        }
        async fn commit(&self, message: &str) -> Result<Option<String>, PublishError> {
            self.calls
                .lock()
                .unwrap()
                .committed
                .push(message.to_owned());
            Ok(Some("committed-sha".to_owned()))
        }
        async fn merge_to_main(
            &self,
            _conv_branch: &str,
            _target: &str,
            push: bool,
        ) -> Result<MergeResult, PublishError> {
            let mut calls = self.calls.lock().unwrap();
            calls.merged = true;
            calls.merge_push = Some(push);
            calls.merge_calls.push(push);
            self.merge.clone()
        }
        async fn deploy(&self, merge_sha: &str) -> Result<String, PublishError> {
            self.calls.lock().unwrap().deployed_sha = Some(merge_sha.to_owned());
            self.deploy.clone()
        }
    }

    #[tokio::test]
    async fn clean_merge_deploys_from_merge_sha() {
        let backend = MockBackend::new(
            false,
            MergeResult::Merged {
                sha: "abc123".to_owned(),
            },
        );
        let out = publish(&backend, "conv/1", "main", "publish")
            .await
            .unwrap();
        assert_eq!("abc123", out.merge_sha);
        assert_eq!("dep-1", out.deployment_id);

        let calls = backend.calls.lock().unwrap();
        assert!(calls.committed.is_empty(), "clean tree skips the commit");
        assert!(calls.merged);
        // Publish always merges with push, so the deployed SHA is on the remote.
        assert_eq!(Some(true), calls.merge_push);
        assert_eq!(Some("abc123".to_owned()), calls.deployed_sha);
    }

    #[tokio::test]
    async fn dirty_tree_is_committed_before_merge() {
        let backend = MockBackend::new(
            true,
            MergeResult::Merged {
                sha: "abc123".to_owned(),
            },
        );
        publish(&backend, "conv/1", "main", "publish msg")
            .await
            .unwrap();

        let calls = backend.calls.lock().unwrap();
        assert_eq!(vec!["publish msg".to_owned()], calls.committed);
        assert_eq!(Some("abc123".to_owned()), calls.deployed_sha);
    }

    #[tokio::test]
    async fn up_to_date_merge_still_deploys_target_head() {
        let backend = MockBackend::new(
            false,
            MergeResult::UpToDate {
                sha: "head9".to_owned(),
            },
        );
        let out = publish(&backend, "conv/1", "main", "publish")
            .await
            .unwrap();
        assert_eq!("head9", out.merge_sha);
        assert_eq!(
            Some("head9".to_owned()),
            backend.calls.lock().unwrap().deployed_sha
        );
    }

    #[tokio::test]
    async fn merge_conflict_does_not_deploy() {
        let backend = MockBackend::new(
            false,
            MergeResult::Conflicts {
                files: vec!["src/a.rs".to_owned()],
            },
        );
        let err = publish(&backend, "conv/1", "main", "publish")
            .await
            .unwrap_err();
        match err {
            PublishError::MergeConflict { target, files } => {
                assert_eq!("main", target);
                assert_eq!(vec!["src/a.rs".to_owned()], files);
            }
            other => panic!("expected MergeConflict, got {other:?}"),
        }
        // The critical invariant: a conflicted merge never deploys.
        assert_eq!(None, backend.calls.lock().unwrap().deployed_sha);
    }

    #[tokio::test]
    async fn merge_failure_does_not_deploy() {
        let backend = MockBackend {
            dirty: false,
            merge: Err(PublishError::MergeFailed {
                target: "main".to_owned(),
                message: "remote unavailable".to_owned(),
            }),
            deploy: Ok("dep-1".to_owned()),
            calls: Mutex::new(Calls::default()),
        };
        let err = publish(&backend, "conv/1", "main", "publish")
            .await
            .unwrap_err();
        assert!(matches!(err, PublishError::MergeFailed { .. }));
        assert_eq!(None, backend.calls.lock().unwrap().deployed_sha);
    }

    #[tokio::test]
    async fn commit_failure_short_circuits_before_merge() {
        struct FailingCommit;
        #[async_trait]
        impl PublishBackend for FailingCommit {
            async fn is_dirty(&self) -> Result<bool, PublishError> {
                Ok(true)
            }
            async fn commit(&self, _message: &str) -> Result<Option<String>, PublishError> {
                Err(PublishError::Commit("nothing staged".to_owned()))
            }
            async fn merge_to_main(
                &self,
                _c: &str,
                _t: &str,
                _push: bool,
            ) -> Result<MergeResult, PublishError> {
                panic!("merge must not run after a failed commit");
            }
            async fn deploy(&self, _sha: &str) -> Result<String, PublishError> {
                panic!("deploy must not run after a failed commit");
            }
        }
        let err = publish(&FailingCommit, "conv/1", "main", "m")
            .await
            .unwrap_err();
        assert!(matches!(err, PublishError::Commit(_)));
    }

    #[tokio::test]
    async fn deploy_failure_after_successful_merge_is_reported() {
        let backend = MockBackend {
            dirty: false,
            merge: Ok(MergeResult::Merged {
                sha: "abc123".to_owned(),
            }),
            deploy: Err(PublishError::Deploy {
                sha: "abc123".to_owned(),
                message: "deployer 500".to_owned(),
            }),
            calls: Mutex::new(Calls::default()),
        };
        let err = publish(&backend, "conv/1", "main", "publish")
            .await
            .unwrap_err();
        assert!(matches!(err, PublishError::Deploy { .. }));
        // The merge did happen and deploy was attempted (merge is durable in git).
        let calls = backend.calls.lock().unwrap();
        assert!(calls.merged);
        assert_eq!(Some("abc123".to_owned()), calls.deployed_sha);
    }

    #[tokio::test]
    async fn multi_repo_dry_run_conflict_skips_all_deploys() {
        let ok = MockBackend::new(
            false,
            MergeResult::Merged {
                sha: "a".to_owned(),
            },
        );
        let bad = MockBackend::new(
            false,
            MergeResult::Conflicts {
                files: vec!["lib.rs".to_owned()],
            },
        );
        let err = publish_serial_after_dry_run(&[&ok, &bad], "conv/1", "main", "pub")
            .await
            .unwrap_err();
        assert!(matches!(err, PublishError::MergeConflict { .. }));
        assert_eq!(None, ok.calls.lock().unwrap().deployed_sha);
        assert_eq!(None, bad.calls.lock().unwrap().deployed_sha);
        assert_eq!(&ok.calls.lock().unwrap().merge_calls, &[false]);
        assert_eq!(&bad.calls.lock().unwrap().merge_calls, &[false]);
    }

    #[tokio::test]
    async fn multi_repo_dry_run_then_serial_push_and_deploy() {
        let a = MockBackend::new(
            true,
            MergeResult::Merged {
                sha: "sha-a".to_owned(),
            },
        );
        let b = MockBackend::new(
            false,
            MergeResult::UpToDate {
                sha: "sha-b".to_owned(),
            },
        );
        let outs = publish_serial_after_dry_run(&[&a, &b], "conv/1", "main", "pub")
            .await
            .unwrap();
        assert_eq!(outs.len(), 2);
        assert_eq!(outs[0].merge_sha, "sha-a");
        assert_eq!(outs[1].merge_sha, "sha-b");
        assert_eq!(a.calls.lock().unwrap().committed, vec!["pub".to_owned()]);
        assert!(b.calls.lock().unwrap().committed.is_empty());
        // Dry-run (push=false) then publish (push=true) per repo.
        assert_eq!(&a.calls.lock().unwrap().merge_calls, &[false, true]);
        assert_eq!(&b.calls.lock().unwrap().merge_calls, &[false, true]);
        assert_eq!(
            Some("sha-a".to_owned()),
            a.calls.lock().unwrap().deployed_sha
        );
        assert_eq!(
            Some("sha-b".to_owned()),
            b.calls.lock().unwrap().deployed_sha
        );
    }
}
