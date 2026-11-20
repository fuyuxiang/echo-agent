use super::*;
fn bazel_skip(name: &str) -> bool {
    if std::env::var("BAZEL_TEST").is_ok() {
        eprintln!("skipping {name} under Bazel sandbox (git CLI unavailable)");
        true
    } else {
        false
    }
}
async fn init_repo_with_commit(dir: &Path) -> String {
    git_cli(dir, &["init", "-q", "-b", "main"]).await.unwrap();
    git_cli(dir, &["config", "user.email", "t@t.com"])
        .await
        .unwrap();
    git_cli(dir, &["config", "user.name", "t"]).await.unwrap();
    git_cli(dir, &["config", "commit.gpgsign", "false"])
        .await
        .unwrap();
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    git_cli(dir, &["add", "."]).await.unwrap();
    git_cli(dir, &["commit", "-q", "-m", "init"]).await.unwrap();
    git_cli(dir, &["rev-parse", "HEAD"])
        .await
        .unwrap()
        .trim()
        .to_owned()
}
#[tokio::test]
async fn stash_before_destructive_op_clean_tree_returns_clean() {
    if bazel_skip("stash_before_destructive_op_clean_tree_returns_clean") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    let outcome = stash_before_destructive_op(tmp.path(), "test", "sess-1").await;
    assert!(matches!(outcome, StashOutcome::Clean));
}
#[tokio::test]
async fn stash_before_destructive_op_dirty_tracked_returns_ref() {
    if bazel_skip("stash_before_destructive_op_dirty_tracked_returns_ref") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("README.md"), "changed\n").unwrap();
    let outcome = stash_before_destructive_op(tmp.path(), "test", "sess-2").await;
    let r = match outcome {
        StashOutcome::Stashed(r) => r,
        other => panic!("expected Stashed, got {other:?}"),
    };
    assert!(!r.is_empty());
    let porcelain = git_cli(tmp.path(), &["status", "--porcelain"])
        .await
        .unwrap();
    assert!(porcelain.trim().is_empty(), "got: {porcelain:?}");
    let list = git_cli(tmp.path(), &["stash", "list"]).await.unwrap();
    assert!(
        list.contains("echoagent: pre-test sess-2"),
        "stash list missing session id: {list}"
    );
}
#[tokio::test]
async fn stash_before_destructive_op_dirty_untracked_returns_ref() {
    if bazel_skip("stash_before_destructive_op_dirty_untracked_returns_ref") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("new.txt"), "new\n").unwrap();
    let outcome = stash_before_destructive_op(tmp.path(), "test", "sess-3").await;
    assert!(matches!(outcome, StashOutcome::Stashed(_)));
    assert!(!tmp.path().join("new.txt").exists());
}
#[tokio::test]
async fn stash_before_destructive_op_staged_only_returns_ref() {
    if bazel_skip("stash_before_destructive_op_staged_only_returns_ref") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("README.md"), "staged\n").unwrap();
    git_cli(tmp.path(), &["add", "README.md"]).await.unwrap();
    let outcome = stash_before_destructive_op(tmp.path(), "test", "sess-staged").await;
    assert!(matches!(outcome, StashOutcome::Stashed(_)));
    let porcelain = git_cli(tmp.path(), &["status", "--porcelain"])
        .await
        .unwrap();
    assert!(porcelain.trim().is_empty(), "got: {porcelain:?}");
}
#[tokio::test]
async fn stash_before_destructive_op_skips_during_merge() {
    if bazel_skip("stash_before_destructive_op_skips_during_merge") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head = init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("README.md"), "dirty\n").unwrap();
    std::fs::write(tmp.path().join(".git").join("MERGE_HEAD"), head).unwrap();
    let outcome = stash_before_destructive_op(tmp.path(), "test", "sess-merge").await;
    let reason = match outcome {
        StashOutcome::Skipped(r) => r,
        other => panic!("expected Skipped, got {other:?}"),
    };
    assert!(reason.contains("MERGE_HEAD"), "reason: {reason}");
    let porcelain = git_cli(tmp.path(), &["status", "--porcelain"])
        .await
        .unwrap();
    assert!(
        !porcelain.trim().is_empty(),
        "dirty state must be preserved when stash is skipped"
    );
}
#[tokio::test]
async fn stash_before_destructive_op_detached_head_dirty_returns_ref() {
    if bazel_skip("stash_before_destructive_op_detached_head_dirty_returns_ref") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head = init_repo_with_commit(tmp.path()).await;
    git_cli(tmp.path(), &["checkout", "-q", "--detach", &head])
        .await
        .unwrap();
    std::fs::write(tmp.path().join("README.md"), "dirty\n").unwrap();
    let outcome = stash_before_destructive_op(tmp.path(), "test", "sess-detached").await;
    assert!(matches!(outcome, StashOutcome::Stashed(_)));
}
#[test]
fn restore_code_checkout_allowed_worktree_cwd_is_allowed() {
    let worktrees = Path::new("/home/u/.echo-agent/worktrees");
    assert!(restore_code_checkout_allowed_in(
        Path::new("/home/u/.echo-agent/worktrees/home-u-repo/2026-05-22-9f2e51ce"),
        Some("/home/u/repo"),
        worktrees,
    ));
}
#[test]
fn restore_code_checkout_allowed_same_cwd_is_allowed() {
    let worktrees = Path::new("/home/u/.echo-agent/worktrees");
    assert!(restore_code_checkout_allowed_in(
        Path::new("/home/u/repo"),
        Some("/home/u/repo"),
        worktrees,
    ));
    assert!(restore_code_checkout_allowed_in(
        Path::new("/home/u/repo/"),
        Some("/home/u/repo"),
        worktrees,
    ));
}
#[test]
fn restore_code_checkout_allowed_source_repo_with_worktree_session_is_refused() {
    let worktrees = Path::new("/home/u/.echo-agent/worktrees");
    assert!(!restore_code_checkout_allowed_in(
        Path::new("/home/u/repo"),
        Some("/home/u/.echo-agent/worktrees/home-u-repo/2026-05-22-9f2e51ce"),
        worktrees,
    ));
}
#[test]
fn restore_code_checkout_allowed_missing_persisted_cwd_is_refused() {
    let worktrees = Path::new("/home/u/.echo-agent/worktrees");
    assert!(!restore_code_checkout_allowed_in(
        Path::new("/home/u/repo"),
        None,
        worktrees,
    ));
}
#[tokio::test]
async fn checkout_session_commit_clean_tree_no_stash() {
    if bazel_skip("checkout_session_commit_clean_tree_no_stash") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head = init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("second.txt"), "x\n").unwrap();
    git_cli(tmp.path(), &["add", "."]).await.unwrap();
    git_cli(tmp.path(), &["commit", "-q", "-m", "second"])
        .await
        .unwrap();
    let outcome = checkout_session_commit(tmp.path(), &head, true, "sess-clean").await;
    assert!(outcome.checked_out);
    assert!(outcome.stash_ref.is_none());
    assert!(outcome.stash_skipped_reason.is_none());
}
#[tokio::test]
async fn checkout_session_commit_dirty_tree_stashes_and_checks_out() {
    if bazel_skip("checkout_session_commit_dirty_tree_stashes_and_checks_out") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head = init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("second.txt"), "x\n").unwrap();
    git_cli(tmp.path(), &["add", "."]).await.unwrap();
    git_cli(tmp.path(), &["commit", "-q", "-m", "second"])
        .await
        .unwrap();
    std::fs::write(tmp.path().join("README.md"), "dirty\n").unwrap();
    let outcome = checkout_session_commit(tmp.path(), &head, true, "sess-dirty").await;
    assert!(outcome.checked_out);
    assert!(outcome.stash_ref.is_some());
    assert!(outcome.stash_skipped_reason.is_none());
    let on_head = git_cli(tmp.path(), &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(on_head.trim(), head);
}
#[tokio::test]
async fn checkout_session_commit_already_at_target_sets_checked_out_true() {
    if bazel_skip("checkout_session_commit_already_at_target_sets_checked_out_true") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head = init_repo_with_commit(tmp.path()).await;
    let outcome = checkout_session_commit(tmp.path(), &head, true, "sess-noop").await;
    assert!(
        outcome.checked_out,
        "already-at-target must report checked_out=true"
    );
    assert!(outcome.stash_ref.is_none());
    assert!(outcome.stash_skipped_reason.is_none());
    let stash_list = git_cli(tmp.path(), &["stash", "list"]).await.unwrap();
    assert!(
        stash_list.trim().is_empty(),
        "no stash should be created on no-op early-return"
    );
}
#[tokio::test]
async fn checkout_session_commit_invalid_sha_returns_not_checked_out() {
    if bazel_skip("checkout_session_commit_invalid_sha_returns_not_checked_out") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    let bogus = "0000000000000000000000000000000000000000";
    let outcome = checkout_session_commit(tmp.path(), bogus, true, "sess-bogus").await;
    assert!(!outcome.checked_out);
}
#[tokio::test]
async fn checkout_session_commit_refuses_non_oid_refspec() {
    if bazel_skip("checkout_session_commit_refuses_non_oid_refspec") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    for bogus in ["origin/main", "abc1234"] {
        let outcome = checkout_session_commit(tmp.path(), bogus, true, "sess-ref").await;
        assert!(!outcome.checked_out, "{bogus}");
        assert!(
            !tmp.path().join(".git/FETCH_HEAD").exists(),
            "must not fetch for non-oid {bogus}"
        );
    }
}
#[tokio::test]
async fn checkout_session_commit_fetches_then_checks_out() {
    if bazel_skip("checkout_session_commit_fetches_then_checks_out") {
        return;
    }
    let upstream = tempfile::tempdir().unwrap();
    init_repo_with_commit(upstream.path()).await;
    let dest_root = tempfile::tempdir().unwrap();
    let repo = dest_root.path().join("repo");
    git_cli(
        dest_root.path(),
        &[
            "clone",
            "-q",
            &upstream.path().to_string_lossy(),
            &repo.to_string_lossy(),
        ],
    )
    .await
    .unwrap();
    std::fs::write(upstream.path().join("two.txt"), "two\n").unwrap();
    git_cli(upstream.path(), &["add", "."]).await.unwrap();
    git_cli(upstream.path(), &["commit", "-q", "-m", "two"])
        .await
        .unwrap();
    let second = git_cli(upstream.path(), &["rev-parse", "HEAD"])
        .await
        .unwrap();
    let second = second.trim().to_owned();
    let outcome = checkout_session_commit(&repo, &second, true, "sess-fetch").await;
    assert!(outcome.checked_out, "expected checkout after fetch");
    let on_head = git_cli(&repo, &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(on_head.trim(), second);
    assert_eq!(
        git_cli(&repo, &["rev-parse", "--is-shallow-repository"])
            .await
            .unwrap()
            .trim(),
        "false"
    );
}
#[tokio::test]
async fn checkout_commit_with_fetch_already_on_target() {
    if bazel_skip("checkout_commit_with_fetch_already_on_target") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head = init_repo_with_commit(tmp.path()).await;
    let response = checkout_commit_with_fetch(tmp.path(), &head, false).await;
    assert_eq!(
        (
            response.checked_out,
            response.fetched,
            response.stashed,
            response.error.is_none()
        ),
        (true, false, false, true)
    );
}
#[tokio::test]
async fn checkout_commit_with_fetch_rejects_unsafe_refspec() {
    if bazel_skip("checkout_commit_with_fetch_rejects_unsafe_refspec") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    let response = checkout_commit_with_fetch(tmp.path(), "foo:bar", false).await;
    assert!(!response.checked_out);
    assert!(!response.fetched);
    let err = response.error.expect("error");
    assert!(
        err.contains("unsupported") || err.contains("refusing"),
        "got: {err}"
    );
    assert!(!tmp.path().join(".git/FETCH_HEAD").exists());
}
#[tokio::test]
async fn checkout_commit_with_fetch_fetches_branch_ref() {
    if bazel_skip("checkout_commit_with_fetch_fetches_branch_ref") {
        return;
    }
    let upstream = tempfile::tempdir().unwrap();
    init_repo_with_commit(upstream.path()).await;
    let dest_root = tempfile::tempdir().unwrap();
    let repo = dest_root.path().join("repo");
    git_cli(
        dest_root.path(),
        &[
            "clone",
            "-q",
            &upstream.path().to_string_lossy(),
            &repo.to_string_lossy(),
        ],
    )
    .await
    .unwrap();
    git_cli(upstream.path(), &["checkout", "-q", "-b", "feature"])
        .await
        .unwrap();
    std::fs::write(upstream.path().join("two.txt"), "two\n").unwrap();
    git_cli(upstream.path(), &["add", "."]).await.unwrap();
    git_cli(upstream.path(), &["commit", "-q", "-m", "two"])
        .await
        .unwrap();
    let second = git_cli(upstream.path(), &["rev-parse", "HEAD"])
        .await
        .unwrap();
    let second = second.trim().to_owned();
    let response = checkout_commit_with_fetch(&repo, "feature", false).await;
    assert!(response.fetched, "error={:?}", response.error);
    assert!(response.checked_out, "error={:?}", response.error);
    let on_head = git_cli(&repo, &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(on_head.trim(), second);
}
#[tokio::test]
async fn checkout_commit_with_fetch_fetches_origin_tracking_ref() {
    if bazel_skip("checkout_commit_with_fetch_fetches_origin_tracking_ref") {
        return;
    }
    let upstream = tempfile::tempdir().unwrap();
    init_repo_with_commit(upstream.path()).await;
    let dest_root = tempfile::tempdir().unwrap();
    let repo = dest_root.path().join("repo");
    git_cli(
        dest_root.path(),
        &[
            "clone",
            "-q",
            &upstream.path().to_string_lossy(),
            &repo.to_string_lossy(),
        ],
    )
    .await
    .unwrap();
    git_cli(upstream.path(), &["checkout", "-q", "-b", "feature"])
        .await
        .unwrap();
    std::fs::write(upstream.path().join("two.txt"), "two\n").unwrap();
    git_cli(upstream.path(), &["add", "."]).await.unwrap();
    git_cli(upstream.path(), &["commit", "-q", "-m", "two"])
        .await
        .unwrap();
    let second = git_cli(upstream.path(), &["rev-parse", "HEAD"])
        .await
        .unwrap();
    let second = second.trim().to_owned();
    let response = checkout_commit_with_fetch(&repo, "origin/feature", false).await;
    assert!(response.fetched, "error={:?}", response.error);
    assert!(response.checked_out, "error={:?}", response.error);
    let on_head = git_cli(&repo, &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(on_head.trim(), second);
}
#[tokio::test]
async fn checkout_commit_with_fetch_fetches_tag_ref() {
    if bazel_skip("checkout_commit_with_fetch_fetches_tag_ref") {
        return;
    }
    let upstream = tempfile::tempdir().unwrap();
    init_repo_with_commit(upstream.path()).await;
    git_cli(upstream.path(), &["tag", "v1.0.0"]).await.unwrap();
    let tagged = git_cli(upstream.path(), &["rev-parse", "refs/tags/v1.0.0"])
        .await
        .unwrap();
    let tagged = tagged.trim().to_owned();
    git_cli(upstream.path(), &["checkout", "-q", "-b", "dev"])
        .await
        .unwrap();
    std::fs::write(upstream.path().join("two.txt"), "two\n").unwrap();
    git_cli(upstream.path(), &["add", "."]).await.unwrap();
    git_cli(upstream.path(), &["commit", "-q", "-m", "two"])
        .await
        .unwrap();
    let dest_root = tempfile::tempdir().unwrap();
    let repo = dest_root.path().join("repo");
    git_cli(
        dest_root.path(),
        &[
            "clone",
            "-q",
            "--no-tags",
            &upstream.path().to_string_lossy(),
            &repo.to_string_lossy(),
        ],
    )
    .await
    .unwrap();
    assert!(
        git_cli(&repo, &["rev-parse", "refs/tags/v1.0.0"])
            .await
            .is_err()
    );
    let response = checkout_commit_with_fetch(&repo, "refs/tags/v1.0.0", false).await;
    assert!(response.fetched, "error={:?}", response.error);
    assert!(response.checked_out, "error={:?}", response.error);
    let on_head = git_cli(&repo, &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(on_head.trim(), tagged);
    let local_tag = git_cli(&repo, &["rev-parse", "refs/tags/v1.0.0"])
        .await
        .unwrap();
    assert_eq!(local_tag.trim(), tagged);
}
#[tokio::test]
async fn checkout_commit_with_fetch_rejects_abbreviated_sha() {
    if bazel_skip("checkout_commit_with_fetch_rejects_abbreviated_sha") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    let response = checkout_commit_with_fetch(tmp.path(), "abc1234f", false).await;
    assert!(!response.checked_out);
    assert!(!response.fetched);
    let err = response.error.expect("error");
    assert!(
        err.contains("unsupported") || err.contains("refusing"),
        "got: {err}"
    );
    assert!(!tmp.path().join(".git/FETCH_HEAD").exists());
}
#[tokio::test]
async fn checkout_commit_with_fetch_pops_stash_on_unsupported_target() {
    if bazel_skip("checkout_commit_with_fetch_pops_stash_on_unsupported_target") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("README.md"), "dirty edit\n").unwrap();
    let response = checkout_commit_with_fetch(tmp.path(), "deadbeef", true).await;
    assert!(!response.checked_out);
    assert!(!response.stashed, "auto-stash must be popped on failure");
    assert!(response.error.is_some());
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("README.md")).unwrap(),
        "dirty edit\n"
    );
    let stash_list = git_cli(tmp.path(), &["stash", "list"]).await.unwrap();
    assert!(
        stash_list.trim().is_empty(),
        "no leftover stash, got: {stash_list:?}"
    );
}
#[tokio::test]
async fn pop_checkout_auto_stash_restores_dirty_tree() {
    if bazel_skip("pop_checkout_auto_stash_restores_dirty_tree") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("README.md"), "dirty edit\n").unwrap();
    git_cli(
        tmp.path(),
        &["stash", "push", "-m", "auto-stash before checkout deadbeef"],
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("README.md")).unwrap(),
        "hello\n"
    );
    let response = pop_checkout_auto_stash(
        tmp.path(),
        true,
        false,
        "targeted fetch timed out".to_owned(),
    )
    .await;
    assert_eq!(
        (
            response.checked_out,
            response.fetched,
            response.stashed,
            response.error.as_deref()
        ),
        (false, false, false, Some("targeted fetch timed out"))
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("README.md")).unwrap(),
        "dirty edit\n"
    );
    let stash_list = git_cli(tmp.path(), &["stash", "list"]).await.unwrap();
    assert!(
        stash_list.trim().is_empty(),
        "no leftover stash, got: {stash_list:?}"
    );
}
#[tokio::test]
async fn checkout_session_commit_dirty_during_merge_surfaces_skipped_reason() {
    if bazel_skip("checkout_session_commit_dirty_during_merge_surfaces_skipped_reason") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head = init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("second.txt"), "x\n").unwrap();
    git_cli(tmp.path(), &["add", "."]).await.unwrap();
    git_cli(tmp.path(), &["commit", "-q", "-m", "second"])
        .await
        .unwrap();
    std::fs::write(tmp.path().join("README.md"), "dirty\n").unwrap();
    std::fs::write(tmp.path().join(".git").join("MERGE_HEAD"), &head).unwrap();
    let outcome = checkout_session_commit(tmp.path(), &head, true, "sess-merge").await;
    assert!(outcome.stash_ref.is_none());
    let reason = outcome
        .stash_skipped_reason
        .expect("expected stash_skipped_reason");
    assert!(reason.contains("MERGE_HEAD"), "got: {reason}");
}
#[test]
fn append_stash_suffix_appends_when_some() {
    let mut s = String::from("checked out abc");
    append_stash_suffix(&mut s, Some("deadbeef"));
    assert_eq!(
        s,
        "checked out abc; saved your dirty changes to stash deadbeef"
    );
}
#[test]
fn append_stash_suffix_composes_after_closing_paren() {
    let mut s = String::from("checked out abc (archive unavailable)");
    append_stash_suffix(&mut s, Some("deadbeef"));
    assert_eq!(
        s,
        "checked out abc (archive unavailable); saved your dirty changes to stash deadbeef"
    );
}
#[test]
fn append_stash_suffix_noop_when_none() {
    let mut s = String::from("checked out abc");
    append_stash_suffix(&mut s, None);
    assert_eq!(s, "checked out abc");
}
fn outcome(
    checked_out: bool,
    stash_ref: Option<&str>,
    skipped: Option<&str>,
) -> CheckoutSessionOutcome {
    CheckoutSessionOutcome {
        checked_out,
        stash_ref: stash_ref.map(str::to_owned),
        stash_skipped_reason: skipped.map(str::to_owned),
    }
}
#[test]
fn build_restore_decision_checkout_failed_carries_stash_skipped_reason() {
    let d = build_restore_decision(
        Some("0123456789abcdef"),
        &outcome(false, None, Some("MERGE_HEAD present")),
        RestoreKind::RegistryOff,
    );
    assert!(!d.restored);
    assert!(d.degree.is_none());
    let s = d.summary.unwrap();
    assert!(s.contains("restore aborted"));
    assert!(s.contains("; stash skipped: MERGE_HEAD present"));
}
/// Passing the dedicated `CheckoutFailed` variant must yield the
/// same failure decision as the `!outcome.checked_out` short-circuit
/// — but the variant carries explicit semantic intent at the call
/// site.
#[test]
fn build_restore_decision_checkout_failed_variant_produces_failure() {
    let d = build_restore_decision(
        Some("0123456789abcdef"),
        &outcome(true, None, Some("MERGE_HEAD present")),
        RestoreKind::CheckoutFailed,
    );
    assert!(!d.restored);
    assert!(d.degree.is_none());
    let s = d.summary.unwrap();
    assert!(s.contains("restore aborted"));
    assert!(s.contains("; stash skipped: MERGE_HEAD present"));
}
#[test]
fn build_restore_decision_checkout_failed_variant_without_stash_reason() {
    let d = build_restore_decision(
        Some("0123456789abcdef"),
        &outcome(true, None, None),
        RestoreKind::CheckoutFailed,
    );
    assert!(!d.restored);
    assert!(d.degree.is_none());
    assert_eq!(d.summary.unwrap(), "restore aborted (checkout failed)");
}
#[test]
fn build_restore_decision_appends_stash_ref_on_success() {
    let d = build_restore_decision(
        Some("0123456789abcdef"),
        &outcome(true, Some("deadbeef"), None),
        RestoreKind::RegistryOff,
    );
    assert!(
        d.summary
            .unwrap()
            .contains("; saved your dirty changes to stash deadbeef")
    );
}
#[test]
fn restore_degree_serializes_snake_case() {
    let json = serde_json::to_string(&RestoreDegree::Full).unwrap();
    assert_eq!(json, "\"full\"");
    let json = serde_json::to_string(&RestoreDegree::HeadOnly).unwrap();
    assert_eq!(json, "\"head_only\"");
}
#[test]
fn restore_degree_deserialises_snake_case() {
    let v: RestoreDegree = serde_json::from_str("\"full\"").unwrap();
    assert_eq!(v, RestoreDegree::Full);
    let v: RestoreDegree = serde_json::from_str("\"head_only\"").unwrap();
    assert_eq!(v, RestoreDegree::HeadOnly);
}
#[test]
fn restore_degree_rejects_unknown_string() {
    let err = serde_json::from_str::<RestoreDegree>("\"full_\"");
    assert!(err.is_err(), "typo must not deserialize");
    let err = serde_json::from_str::<RestoreDegree>("\"FULL\"");
    assert!(err.is_err(), "wrong case must not deserialize");
}
/// MakeWriter that captures emitted log lines into a shared buffer.
#[derive(Clone, Default)]
struct CapturingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
impl std::io::Write for CapturingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturingWriter {
    type Writer = CapturingWriter;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
#[test]
fn should_warn_registry_disabled_truth_table() {
    let cases: [(bool, bool, bool); 4] = [
        (false, false, true),
        (false, true, false),
        (true, false, false),
        (true, true, false),
    ];
    for (is_jj, reg, expected) in cases {
        assert_eq!(
            should_warn_registry_disabled(is_jj, reg),
            expected,
            "(is_jj={is_jj}, registry_present={reg})"
        );
    }
}
#[test]
fn warn_registry_disabled_restore_emits_warn_with_target_and_session_id() {
    use tracing::subscriber::with_default;
    use tracing_subscriber::fmt;
    let buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>> = Default::default();
    let writer = CapturingWriter(buf.clone());
    let subscriber = fmt()
        .with_writer(writer)
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_target(true)
        .finish();
    with_default(subscriber, || {
        warn_registry_disabled_restore("session-xyz");
    });
    let out = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(out.contains("WARN"), "no WARN level in: {out}");
    assert!(
        out.contains(RESTORE_CODE_LOG),
        "missing target {RESTORE_CODE_LOG} in: {out}"
    );
    assert!(
        out.contains("session registry disabled"),
        "missing canonical message in: {out}"
    );
    assert!(out.contains("session-xyz"), "missing session_id in: {out}");
}
#[tokio::test]
async fn capture_git_state_records_head_and_staged() {
    if bazel_skip("capture_git_state_records_head_and_staged") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head = init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("staged.txt"), "work\n").unwrap();
    git_cli(tmp.path(), &["add", "staged.txt"]).await.unwrap();
    let state = capture_git_state(tmp.path())
        .await
        .expect("capture on a real repo");
    assert_eq!(state.head, head, "captured HEAD must match rev-parse HEAD");
    assert_eq!(
        state.staged,
        vec![PathBuf::from("staged.txt")],
        "captured staged set must list the staged path"
    );
}
/// Safety-critical invariant: a soft restore rewinds HEAD but never destroys a
/// turn-local commit — its content survives on disk (proving `--soft`) and the
/// commit stays reachable via the reflog.
#[tokio::test]
async fn soft_restore_preserves_turn_local_commit() {
    if bazel_skip("soft_restore_preserves_turn_local_commit") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head_a = init_repo_with_commit(tmp.path()).await;
    let state = capture_git_state(tmp.path()).await.unwrap();
    assert_eq!(state.head, head_a);
    assert!(state.staged.is_empty());
    std::fs::write(tmp.path().join("feature.txt"), "turn work\n").unwrap();
    git_cli(tmp.path(), &["add", "feature.txt"]).await.unwrap();
    git_cli(tmp.path(), &["commit", "-q", "-m", "turn-local"])
        .await
        .unwrap();
    let head_b = git_cli(tmp.path(), &["rev-parse", "HEAD"])
        .await
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(head_a, head_b, "turn-local commit must advance HEAD");
    let outcome = soft_restore_git_state(tmp.path(), &state, "sess-soft").await;
    assert!(outcome.restored, "soft restore should succeed");
    assert!(outcome.aborted_reason.is_none());
    let head_now = git_cli(tmp.path(), &["rev-parse", "HEAD"])
        .await
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(
        head_now, head_a,
        "HEAD must be rewound to the recorded commit"
    );
    let feature = tmp.path().join("feature.txt");
    assert!(
        feature.exists(),
        "soft restore must preserve working-tree content"
    );
    assert_eq!(std::fs::read_to_string(&feature).unwrap(), "turn work\n");
    let obj_type = git_cli(tmp.path(), &["cat-file", "-t", &head_b])
        .await
        .expect("turn-local commit object must still exist");
    assert_eq!(obj_type.trim(), "commit");
    let prev_head = git_cli(tmp.path(), &["rev-parse", "HEAD@{1}"])
        .await
        .unwrap();
    assert_eq!(
        prev_head.trim(),
        head_b,
        "turn-local commit must remain reachable via the reflog"
    );
}
#[tokio::test]
async fn git_checkpoint_store_record_first_wins_and_truncate() {
    let store = GitCheckpointStore::new();
    let mk = |h: &str| GitStateRef {
        head: h.to_owned(),
        staged: vec![],
    };
    store.record(0, mk("aaaaaaa")).await;
    store.record(1, mk("bbbbbbb")).await;
    store.record(1, mk("ccccccc")).await;
    assert_eq!(store.get(0).await.unwrap().head, "aaaaaaa");
    assert_eq!(store.get(1).await.unwrap().head, "bbbbbbb");
    assert!(store.get(2).await.is_none());
    store.truncate_from(1).await;
    assert!(
        store.get(0).await.is_some(),
        "indices below target are retained"
    );
    assert!(
        store.get(1).await.is_none(),
        "indices >= target are dropped"
    );
}
#[tokio::test]
async fn git_checkpoint_get_at_or_before_falls_back_to_nearest_earlier() {
    let store = GitCheckpointStore::new();
    let mk = |h: &str| GitStateRef {
        head: h.to_owned(),
        staged: vec![],
    };
    store.record(0, mk("aaaaaaa")).await;
    store.record(2, mk("ccccccc")).await;
    let (idx, state) = store.get_at_or_before(2).await.unwrap();
    assert_eq!((idx, state.head.as_str()), (2, "ccccccc"));
    let (idx, state) = store.get_at_or_before(3).await.unwrap();
    assert_eq!(
        (idx, state.head.as_str()),
        (2, "ccccccc"),
        "must return the greatest captured index <= target"
    );
    let (idx, _) = store.get_at_or_before(1).await.unwrap();
    assert_eq!(idx, 0, "index 1 is uncaptured; nearest <= 1 is 0");
    let store_late = GitCheckpointStore::new();
    store_late.record(5, mk("ddddddd")).await;
    assert!(
        store_late.get_at_or_before(3).await.is_none(),
        "no checkpoint at or before target ⇒ None"
    );
}
#[tokio::test]
async fn git_checkpoint_claim_attempt_is_once_per_prompt_until_truncate() {
    let store = GitCheckpointStore::new();
    assert!(
        store.claim_attempt(3).await,
        "the first begin claims the slot"
    );
    assert!(
        !store.claim_attempt(3).await,
        "a re-delivered begin must not re-claim (so it skips capturing mid-turn state)"
    );
    assert!(
        store.claim_attempt(4).await,
        "an unrelated prompt is claimed independently"
    );
    store.truncate_from(3).await;
    assert!(
        store.claim_attempt(3).await,
        "after truncate the prompt index can be re-claimed"
    );
}
/// A session cwd may be a repo subdirectory: capture and restore must both
/// anchor on the repo root so staged paths re-stage correctly (subdir-cwd regression).
#[tokio::test]
async fn capture_and_restore_anchor_on_repo_root_from_subdir_cwd() {
    if bazel_skip("capture_and_restore_anchor_on_repo_root_from_subdir_cwd") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let head_a = init_repo_with_commit(root).await;
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("root.txt"), "r\n").unwrap();
    std::fs::write(root.join("sub/s.txt"), "s\n").unwrap();
    git_cli(root, &["add", "root.txt", "sub/s.txt"])
        .await
        .unwrap();
    let subdir = root.join("sub");
    let state = capture_git_state(&subdir).await.unwrap();
    assert_eq!(state.head, head_a);
    let mut staged = state.staged.clone();
    staged.sort();
    assert_eq!(
        staged,
        vec![PathBuf::from("root.txt"), PathBuf::from("sub/s.txt")],
        "captured staged set must be repo-root-relative from a subdir cwd"
    );
    git_cli(root, &["commit", "-q", "-m", "turn-local"])
        .await
        .unwrap();
    let outcome = soft_restore_git_state(&subdir, &state, "sess-subdir").await;
    assert!(outcome.restored);
    let head_now = git_cli(root, &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(head_now.trim(), head_a);
    let staged_after_phase1 = git_cli(root, &["diff", "--cached", "--name-only"])
        .await
        .unwrap();
    assert!(
        staged_after_phase1.trim().is_empty(),
        "phase 1 unstages to HEAD; re-staging is deferred to phase 2"
    );
    restage_git_paths(&subdir, &state, "sess-subdir").await;
    let staged_now = git_cli(root, &["diff", "--cached", "--name-only"])
        .await
        .unwrap();
    let mut lines: Vec<&str> = staged_now.lines().collect();
    lines.sort();
    assert_eq!(
        lines,
        vec!["root.txt", "sub/s.txt"],
        "both staged paths must be re-staged from a subdir cwd"
    );
}
/// The abort path leaves git untouched: an unstashable dirty tree (in-progress
/// merge) returns `restored: false` with a reason and HEAD unchanged.
#[tokio::test]
async fn soft_restore_aborts_on_unstashable_dirty_tree_without_touching_git() {
    if bazel_skip("soft_restore_aborts_on_unstashable_dirty_tree_without_touching_git") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let head_a = init_repo_with_commit(tmp.path()).await;
    let state = capture_git_state(tmp.path()).await.unwrap();
    assert_eq!(state.head, head_a);
    std::fs::write(tmp.path().join("feature.txt"), "turn\n").unwrap();
    git_cli(tmp.path(), &["add", "feature.txt"]).await.unwrap();
    git_cli(tmp.path(), &["commit", "-q", "-m", "turn-local"])
        .await
        .unwrap();
    let head_b = git_cli(tmp.path(), &["rev-parse", "HEAD"])
        .await
        .unwrap()
        .trim()
        .to_owned();
    std::fs::write(tmp.path().join("README.md"), "dirty\n").unwrap();
    std::fs::write(tmp.path().join(".git").join("MERGE_HEAD"), &head_b).unwrap();
    let outcome = soft_restore_git_state(tmp.path(), &state, "sess-abort").await;
    assert!(
        !outcome.restored,
        "must not restore when a dirty tree cannot be stashed"
    );
    assert!(outcome.aborted_reason.is_some(), "abort reason must be set");
    assert!(outcome.stash_ref.is_none());
    let head_now = git_cli(tmp.path(), &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(head_now.trim(), head_b, "HEAD must be unchanged on abort");
}
/// When `reset --soft` fails after dirty work was stashed, the stash is popped
/// back (not stranded): `restored: false`, no leftover stash, dirty content back.
#[tokio::test]
async fn soft_restore_restores_stash_when_reset_fails() {
    if bazel_skip("soft_restore_restores_stash_when_reset_fails") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    init_repo_with_commit(tmp.path()).await;
    std::fs::write(tmp.path().join("README.md"), "dirty edit\n").unwrap();
    let bogus = GitStateRef {
        head: "0".repeat(40),
        staged: Vec::new(),
    };
    let outcome = soft_restore_git_state(tmp.path(), &bogus, "sess-reset-fail").await;
    assert!(
        !outcome.restored,
        "a failed reset must report restored: false"
    );
    assert!(
        outcome.stash_ref.is_none(),
        "the stash must be popped back, leaving nothing orphaned"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("README.md")).unwrap(),
        "dirty edit\n",
        "dirty work must be restored to the working tree"
    );
    let stash_list = git_cli(tmp.path(), &["stash", "list"]).await.unwrap();
    assert!(
        stash_list.trim().is_empty(),
        "no stash entry should remain after the pop, got: {stash_list:?}"
    );
}
fn skip_without_git_cli() -> bool {
    std::env::var("BAZEL_TEST").is_ok()
}
/// `origin.git` (bare, HEAD → main) plus a work clone on `conv/t` forked
/// from a pushed `main`. Returns (tempdir, work path).
async fn conv_repo_with_origin() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let bare = tmp.path().join("origin.git");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    git_cli(&bare, &["init", "--bare"]).await.unwrap();
    git_cli(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"])
        .await
        .unwrap();
    git_cli(&work, &["init", "-b", "main"]).await.unwrap();
    configure_test_identity(&work).await;
    std::fs::write(work.join("README.md"), "base\n").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "init"]).await.unwrap();
    git_cli(&work, &["remote", "add", "origin", bare.to_str().unwrap()])
        .await
        .unwrap();
    git_cli(&work, &["push", "-u", "origin", "main"])
        .await
        .unwrap();
    git_cli(&work, &["checkout", "-b", "conv/t"]).await.unwrap();
    (tmp, work)
}
async fn configure_test_identity(repo: &Path) {
    git_cli(repo, &["config", "user.name", "test"])
        .await
        .unwrap();
    git_cli(repo, &["config", "user.email", "test@test.com"])
        .await
        .unwrap();
    git_cli(repo, &["config", "commit.gpgsign", "false"])
        .await
        .unwrap();
}
fn conv_commit_req(push: bool) -> GitCommitReq {
    GitCommitReq {
        message: "conv commit".to_owned(),
        push,
        stage_all: true,
        seed_default_excludes: true,
        expected_branch: Some("conv/t".to_owned()),
        ..Default::default()
    }
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn conv_commit_seeds_excludes_and_pushes() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    std::fs::create_dir_all(work.join("node_modules")).unwrap();
    std::fs::write(work.join("node_modules/dep.js"), "x").unwrap();
    std::fs::write(work.join(".env"), "SECRET=1").unwrap();
    std::fs::write(work.join("src.txt"), "real").unwrap();
    let res = commit(&work, &conv_commit_req(true)).await.unwrap();
    let outcome = res.outcome.expect("git backend returns an outcome");
    assert!(!outcome.clean);
    assert!(outcome.pushed);
    assert_eq!(outcome.push, PushStatus::Ok);
    let sha = outcome.sha.expect("commit produced a HEAD");
    let tree = git_cli(&work, &["ls-tree", "-r", "--name-only", "HEAD"])
        .await
        .unwrap();
    assert!(tree.contains("src.txt"));
    assert!(
        !tree.contains(".env"),
        "seeded excludes must hide .env: {tree}"
    );
    assert!(!tree.contains("node_modules"), "{tree}");
    let remote_sha = git_cli(&work, &["rev-parse", "origin/conv/t"])
        .await
        .unwrap();
    assert_eq!(remote_sha, sha);
    let res2 = commit(&work, &conv_commit_req(true)).await.unwrap();
    let outcome2 = res2.outcome.unwrap();
    assert!(outcome2.clean);
    assert_eq!(outcome2.sha.as_deref(), Some(sha.as_str()));
    let exclude = git_cli(&work, &["rev-parse", "--git-path", "info/exclude"])
        .await
        .unwrap();
    let content = std::fs::read_to_string(work.join(exclude)).unwrap();
    assert_eq!(content.matches(DEFAULT_EXCLUDES_MARKER).count(), 1);
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn conv_commit_legacy_clean_tree_still_errors_without_stage_all() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    let req = GitCommitReq {
        message: "nothing staged".to_owned(),
        ..Default::default()
    };
    assert!(
        commit(&work, &req).await.is_err(),
        "legacy contract: committing with nothing staged errors"
    );
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn conv_commit_refuses_wrong_branch() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    let mut req = conv_commit_req(false);
    req.expected_branch = Some("conv/other".to_owned());
    let err = commit(&work, &req).await.expect_err("wrong branch refused");
    assert!(err.to_string().contains("expected 'conv/other'"), "{err}");
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn conv_commit_classifies_non_fast_forward_push_and_never_forces() {
    if skip_without_git_cli() {
        return;
    }
    let (tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("a.txt"), "a").unwrap();
    commit(&work, &conv_commit_req(true)).await.unwrap();
    let work2 = tmp.path().join("work2");
    git_cli(
        tmp.path(),
        &[
            "clone",
            "--branch",
            "conv/t",
            tmp.path().join("origin.git").to_str().unwrap(),
            work2.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    configure_test_identity(&work2).await;
    std::fs::write(work2.join("b.txt"), "b").unwrap();
    git_cli(&work2, &["add", "-A"]).await.unwrap();
    git_cli(&work2, &["commit", "-m", "out of band"])
        .await
        .unwrap();
    git_cli(&work2, &["push", "origin", "conv/t"])
        .await
        .unwrap();
    let diverged_sha = git_cli(&work2, &["rev-parse", "HEAD"]).await.unwrap();
    std::fs::write(work.join("c.txt"), "c").unwrap();
    let res = commit(&work, &conv_commit_req(true)).await.unwrap();
    let outcome = res.outcome.unwrap();
    assert!(!outcome.clean);
    assert!(!outcome.pushed);
    assert_eq!(outcome.push, PushStatus::Conflict);
    assert!(
        res.warning.is_some(),
        "push failure still surfaces a warning"
    );
    git_cli(&work, &["fetch", "origin", "conv/t"])
        .await
        .unwrap();
    let remote_sha = git_cli(&work, &["rev-parse", "FETCH_HEAD"]).await.unwrap();
    assert_eq!(remote_sha, diverged_sha);
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn sync_rebase_reports_the_rewritten_head() {
    if skip_without_git_cli() {
        return;
    }
    let (tmp, work) = conv_repo_with_origin().await;
    git_cli(&work, &["checkout", "main"]).await.unwrap();
    let work2 = tmp.path().join("work2");
    git_cli(
        tmp.path(),
        &[
            "clone",
            "--branch",
            "main",
            tmp.path().join("origin.git").to_str().unwrap(),
            work2.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    configure_test_identity(&work2).await;
    std::fs::write(work2.join("base.txt"), "base").unwrap();
    git_cli(&work2, &["add", "-A"]).await.unwrap();
    git_cli(&work2, &["commit", "-m", "advance main"])
        .await
        .unwrap();
    git_cli(&work2, &["push", "origin", "main"]).await.unwrap();
    std::fs::write(work.join("local.txt"), "local").unwrap();
    let req = GitCommitReq {
        message: "local commit".to_owned(),
        sync: true,
        stage_all: true,
        expected_branch: Some("main".to_owned()),
        ..Default::default()
    };
    let res = commit(&work, &req).await.unwrap();
    let outcome = res.outcome.unwrap();
    assert!(outcome.pushed);
    let head = git_cli(&work, &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(
        outcome.sha.as_deref(),
        Some(head.as_str()),
        "sha must be the post-rebase HEAD"
    );
    assert_eq!(res.data.commit_hash.as_deref(), Some(head.as_str()));
    let remote = git_cli(&work, &["rev-parse", "origin/main"]).await.unwrap();
    assert_eq!(remote, head);
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn sync_base_up_to_date_and_clean_merge() {
    if skip_without_git_cli() {
        return;
    }
    let (tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("conv.txt"), "conv").unwrap();
    commit(&work, &conv_commit_req(true)).await.unwrap();
    for base in [Some("main"), None] {
        let res = sync_base(&work, base, false, Some("conv/t")).await.unwrap();
        assert_eq!(res.outcome, GitSyncBaseOutcome::UpToDate, "base={base:?}");
    }
    let work2 = tmp.path().join("work2");
    git_cli(
        tmp.path(),
        &[
            "clone",
            "--branch",
            "main",
            tmp.path().join("origin.git").to_str().unwrap(),
            work2.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    configure_test_identity(&work2).await;
    std::fs::write(work2.join("base.txt"), "base change").unwrap();
    git_cli(&work2, &["add", "-A"]).await.unwrap();
    git_cli(&work2, &["commit", "-m", "advance main"])
        .await
        .unwrap();
    git_cli(&work2, &["push", "origin", "main"]).await.unwrap();
    let res = sync_base(&work, Some("main"), false, Some("conv/t"))
        .await
        .unwrap();
    match res.outcome {
        GitSyncBaseOutcome::Merged { sha } => {
            assert_eq!(sha, git_cli(&work, &["rev-parse", "HEAD"]).await.unwrap());
            assert!(
                work.join("base.txt").exists(),
                "merge brought the base file in"
            );
        }
        other => panic!("expected Merged, got {other:?}"),
    }
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn sync_base_conflicts_left_in_progress_and_abort_rolls_back() {
    if skip_without_git_cli() {
        return;
    }
    let (tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("README.md"), "conv version\n").unwrap();
    commit(&work, &conv_commit_req(true)).await.unwrap();
    let pre_merge_sha = git_cli(&work, &["rev-parse", "HEAD"]).await.unwrap();
    let work2 = tmp.path().join("work2");
    git_cli(
        tmp.path(),
        &[
            "clone",
            "--branch",
            "main",
            tmp.path().join("origin.git").to_str().unwrap(),
            work2.to_str().unwrap(),
        ],
    )
    .await
    .unwrap();
    configure_test_identity(&work2).await;
    std::fs::write(work2.join("README.md"), "main version\n").unwrap();
    git_cli(&work2, &["add", "-A"]).await.unwrap();
    git_cli(&work2, &["commit", "-m", "conflicting base change"])
        .await
        .unwrap();
    git_cli(&work2, &["push", "origin", "main"]).await.unwrap();
    let res = sync_base(&work, Some("main"), false, Some("conv/t"))
        .await
        .unwrap();
    match &res.outcome {
        GitSyncBaseOutcome::Conflicts { files } => {
            assert_eq!(files, &vec!["README.md".to_owned()]);
        }
        other => panic!("expected Conflicts, got {other:?}"),
    }
    assert!(
        git_cli_raw(&work, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
            .await
            .unwrap()
            .0,
        "conflicted merge must stay in progress"
    );
    assert!(
        sync_base(&work, Some("main"), false, Some("conv/t"))
            .await
            .is_err()
    );
    let res = sync_base(&work, None, true, Some("not-the-branch"))
        .await
        .unwrap();
    assert_eq!(res.outcome, GitSyncBaseOutcome::Aborted);
    assert!(
        !git_cli_raw(&work, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
            .await
            .unwrap()
            .0
    );
    assert_eq!(
        git_cli(&work, &["rev-parse", "HEAD"]).await.unwrap(),
        pre_merge_sha
    );
    assert_eq!(
        std::fs::read_to_string(work.join("README.md")).unwrap(),
        "conv version\n"
    );
    let res = sync_base(&work, None, true, None).await.unwrap();
    assert_eq!(res.outcome, GitSyncBaseOutcome::Aborted);
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn sync_base_refuses_wrong_branch() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    let err = sync_base(&work, Some("main"), false, Some("conv/other"))
        .await
        .expect_err("wrong branch refused");
    assert!(err.to_string().contains("expected 'conv/other'"), "{err}");
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn sync_pull_failure_reports_push_skipped() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("local.txt"), "local").unwrap();
    let req = GitCommitReq {
        message: "local".to_owned(),
        sync: true,
        stage_all: true,
        ..Default::default()
    };
    let res = commit(&work, &req).await.unwrap();
    assert!(res.warning.is_some(), "pull failure surfaces a warning");
    let outcome = res.outcome.unwrap();
    assert!(!outcome.pushed);
    assert_eq!(
        outcome.push,
        PushStatus::Skipped,
        "an implied push skipped by a pull failure must not read as never-requested"
    );
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn sync_base_refuses_dirty_tree() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("dirty.txt"), "uncommitted").unwrap();
    let err = sync_base(&work, Some("main"), false, Some("conv/t"))
        .await
        .expect_err("dirty tree refused");
    assert!(err.to_string().contains("not clean"), "{err}");
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn ensure_binding_forks_conv_branch_off_base_and_is_idempotent() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    git_cli(&work, &["checkout", "main"]).await.unwrap();
    let main_sha = git_cli(&work, &["rev-parse", "main"]).await.unwrap();
    let res = ensure_binding(&work, "conv/new", "main").await.unwrap();
    assert!(res.created);
    assert_eq!("conv/new", res.branch);
    assert_eq!(
        "conv/new",
        git_cli(&work, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap()
    );
    let fork_sha = git_cli(&work, &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(Some(fork_sha), res.head_sha);
    assert_eq!(main_sha, git_cli(&work, &["rev-parse", "HEAD^"]).await.unwrap());
    assert_eq!(
        git_cli(&work, &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])
            .await
            .unwrap(),
        ".gitignore"
    );
    assert_eq!(
        std::fs::read_to_string(work.join(".gitignore")).unwrap(),
        echo_agent_workspace_types::binding::DEFAULT_GITIGNORE
    );
    std::fs::write(work.join("f.txt"), "x").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "conv work"])
        .await
        .unwrap();
    let conv_sha = git_cli(&work, &["rev-parse", "HEAD"]).await.unwrap();
    assert_eq!(
        main_sha,
        git_cli(&work, &["rev-parse", "main"]).await.unwrap()
    );
    git_cli(&work, &["checkout", "main"]).await.unwrap();
    let again = ensure_binding(&work, "conv/new", "main").await.unwrap();
    assert!(!again.created);
    assert_eq!(Some(conv_sha), again.head_sha);
    assert_eq!(
        "conv/new",
        git_cli(&work, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap()
    );
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn ensure_binding_resumes_existing_remote_conv_branch() {
    if skip_without_git_cli() {
        return;
    }
    let (tmp, work) = conv_repo_with_origin().await;
    let bare = tmp.path().join("origin.git");
    git_cli(&work, &["checkout", "-b", "conv/resume", "main"])
        .await
        .unwrap();
    std::fs::write(work.join("remote-only.txt"), "x").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "remote conv work"])
        .await
        .unwrap();
    git_cli(&work, &["push", "-u", "origin", "conv/resume"])
        .await
        .unwrap();
    let remote_sha = git_cli(&work, &["rev-parse", "conv/resume"]).await.unwrap();
    let fresh = tmp.path().join("fresh");
    git_cli(tmp.path(), &["clone", bare.to_str().unwrap(), "fresh"])
        .await
        .unwrap();
    configure_test_identity(&fresh).await;
    let res = ensure_binding(&fresh, "conv/resume", "main").await.unwrap();
    assert!(!res.created, "must resume the remote branch, not re-fork");
    assert_eq!(Some(remote_sha), res.head_sha);
    assert_eq!(
        "conv/resume",
        git_cli(&fresh, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap()
    );
    assert!(fresh.join("remote-only.txt").exists());
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn merge_to_main_clean_merge_lands_on_target() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("feature.txt"), "feat").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "feature"]).await.unwrap();
    let conv_sha = git_cli(&work, &["rev-parse", "HEAD"]).await.unwrap();
    let res = merge_to_main(&work, "conv/t", "main", false).await.unwrap();
    match res.outcome {
        GitMergeToMainOutcome::Merged { sha } => assert_eq!(conv_sha, sha),
        other => panic!("expected Merged, got {other:?}"),
    }
    assert_eq!(
        conv_sha,
        git_cli(&work, &["rev-parse", "main"]).await.unwrap()
    );
    assert_eq!(
        "conv/t",
        git_cli(&work, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap()
    );
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn merge_to_main_with_push_updates_origin_and_restores_conv() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("feature.txt"), "feat").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "feature"]).await.unwrap();
    let conv_sha = git_cli(&work, &["rev-parse", "HEAD"]).await.unwrap();
    let res = merge_to_main(&work, "conv/t", "main", true).await.unwrap();
    assert!(matches!(res.outcome, GitMergeToMainOutcome::Merged { .. }));
    assert_eq!(
        conv_sha,
        git_cli(&work, &["rev-parse", "origin/main"]).await.unwrap()
    );
    assert_eq!(
        "conv/t",
        git_cli(&work, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap()
    );
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn merge_to_main_push_failure_errors_then_retry_repushes() {
    if skip_without_git_cli() {
        return;
    }
    let (tmp, work) = conv_repo_with_origin().await;
    let bare = tmp.path().join("origin.git");
    std::fs::write(work.join("feature.txt"), "feat").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "feature"]).await.unwrap();
    let conv_sha = git_cli(&work, &["rev-parse", "HEAD"]).await.unwrap();
    git_cli(
        &work,
        &["remote", "set-url", "origin", "/nonexistent/origin.git"],
    )
    .await
    .unwrap();
    let err = merge_to_main(&work, "conv/t", "main", true)
        .await
        .expect_err("push failure must be an error");
    assert!(err.to_string().contains("push failed"), "{err}");
    assert_eq!(
        conv_sha,
        git_cli(&work, &["rev-parse", "main"]).await.unwrap()
    );
    git_cli(
        &work,
        &["remote", "set-url", "origin", bare.to_str().unwrap()],
    )
    .await
    .unwrap();
    let res = merge_to_main(&work, "conv/t", "main", true).await.unwrap();
    assert!(matches!(
        res.outcome,
        GitMergeToMainOutcome::UpToDate { .. }
    ));
    assert_eq!(
        conv_sha,
        git_cli(&work, &["rev-parse", "origin/main"]).await.unwrap()
    );
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn merge_to_main_up_to_date_when_nothing_new() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    let res = merge_to_main(&work, "conv/t", "main", false).await.unwrap();
    assert!(matches!(
        res.outcome,
        GitMergeToMainOutcome::UpToDate { .. }
    ));
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn merge_to_main_conflict_is_aborted_and_restores_conv() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("c.txt"), "conv\n").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "conv side"])
        .await
        .unwrap();
    git_cli(&work, &["checkout", "main"]).await.unwrap();
    std::fs::write(work.join("c.txt"), "main\n").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "main side"])
        .await
        .unwrap();
    let res = merge_to_main(&work, "conv/t", "main", false).await.unwrap();
    match res.outcome {
        GitMergeToMainOutcome::Conflicts { files } => {
            assert!(files.contains(&"c.txt".to_owned()), "{files:?}");
        }
        other => panic!("expected Conflicts, got {other:?}"),
    }
    assert!(
        !git_cli_raw(&work, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
            .await
            .unwrap()
            .0,
        "merge must be aborted on conflict (no MERGE_HEAD left on target)"
    );
    assert_eq!(
        "conv/t",
        git_cli(&work, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .unwrap(),
        "HEAD must be restored to conv_branch after an aborted conflict merge"
    );
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn merge_to_main_refuses_dirty_tree() {
    if skip_without_git_cli() {
        return;
    }
    let (_tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("dirty.txt"), "x").unwrap();
    let err = merge_to_main(&work, "conv/t", "main", false)
        .await
        .expect_err("dirty tree refused");
    assert!(err.to_string().contains("not clean"), "{err}");
}
#[tokio::test]
#[cfg_attr(
    not(unix),
    ignore = "test invokes git CLI which is not always available"
)]
async fn push_branch_ok_then_conflict_on_divergence() {
    if skip_without_git_cli() {
        return;
    }
    let (tmp, work) = conv_repo_with_origin().await;
    std::fs::write(work.join("p.txt"), "1").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "p1"]).await.unwrap();
    let ok = push_branch(&work, Some("conv/t")).await.unwrap();
    assert_eq!(PushStatus::Ok, ok.status);
    let work2 = tmp.path().join("work2");
    let bare = tmp.path().join("origin.git");
    git_cli(tmp.path(), &["clone", bare.to_str().unwrap(), "work2"])
        .await
        .unwrap();
    configure_test_identity(&work2).await;
    git_cli(&work2, &["checkout", "conv/t"]).await.unwrap();
    std::fs::write(work2.join("p.txt"), "2").unwrap();
    git_cli(&work2, &["add", "-A"]).await.unwrap();
    git_cli(&work2, &["commit", "-m", "p2"]).await.unwrap();
    git_cli(&work2, &["push", "origin", "conv/t"])
        .await
        .unwrap();
    std::fs::write(work.join("p.txt"), "3").unwrap();
    git_cli(&work, &["add", "-A"]).await.unwrap();
    git_cli(&work, &["commit", "-m", "p3"]).await.unwrap();
    let conflict = push_branch(&work, Some("conv/t")).await.unwrap();
    assert_eq!(PushStatus::Conflict, conflict.status);
}
