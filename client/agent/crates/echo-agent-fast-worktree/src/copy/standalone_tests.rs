use std::path::{Path, PathBuf};

use echo_agent_test_utils::git::{git_commit_all, init_git_repo, run_git};
use tempfile::TempDir;

use super::{
    checkout_origin_name, narrow_origin_fetch_spec, origin_keep_names_for_git_ref,
    rewrite_origin_fetch_in_config, sanitize_standalone_git_dir,
};
use crate::copy::gitdir::{copy_git_dir, copy_git_dir_keeping_origin};

fn heads_fetch_spec(branch: &str) -> String {
    format!("+refs/heads/{branch}:refs/remotes/origin/{branch}")
}

struct CopiedRepo {
    _temp: TempDir,
    source: PathBuf,
    dest_git: PathBuf,
    dest_root: PathBuf,
    branch: String,
}

fn write_commit(repo: &Path, file: &str, contents: &str, message: &str) -> String {
    std::fs::write(repo.join(file), contents).unwrap();
    git_commit_all(repo, message);
    run_git(repo, &["rev-parse", "HEAD"])
}

fn add_origin(repo: &Path, url: &str, fetch: &str) {
    run_git(repo, &["remote", "add", "origin", url]);
    run_git(repo, &["config", "--unset-all", "remote.origin.fetch"]);
    run_git(repo, &["config", "--add", "remote.origin.fetch", fetch]);
}

fn copy_source(source: &Path) -> (TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new().unwrap();
    let dest_root = temp.path().join("dest");
    let dest_git = dest_root.join(".git");
    std::fs::create_dir_all(&dest_root).unwrap();
    copy_git_dir(&source.join(".git"), &dest_git).unwrap();
    (temp, dest_root, dest_git)
}

fn setup_repo_with_wildcard_fetch() -> CopiedRepo {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    write_commit(&source, "file.txt", "content", "initial");
    let branch = run_git(&source, &["rev-parse", "--abbrev-ref", "HEAD"]);
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/heads/*",
    );
    let dest_root = temp.path().join("dest");
    let dest_git = dest_root.join(".git");
    std::fs::create_dir_all(&dest_root).unwrap();
    copy_git_dir(&source.join(".git"), &dest_git).unwrap();
    CopiedRepo {
        _temp: temp,
        source,
        dest_git,
        dest_root,
        branch,
    }
}

#[test]
fn checkout_origin_name_accepts_remotes_origin_prefix() {
    assert_eq!(
        checkout_origin_name("remotes/origin/feature").as_deref(),
        Some("feature")
    );
    assert_eq!(
        checkout_origin_name("refs/remotes/origin/feature").as_deref(),
        Some("feature")
    );
    assert_eq!(
        checkout_origin_name("origin/feature").as_deref(),
        Some("feature")
    );
    assert_eq!(checkout_origin_name("feature").as_deref(), Some("feature"));
}

#[test]
fn copy_rewrites_wildcard_heads_fetch_to_exact_current_branch_spec() {
    let repo = setup_repo_with_wildcard_fetch();
    let expected = heads_fetch_spec(&repo.branch);
    assert_eq!(
        run_git(
            &repo.dest_root,
            &["config", "--get-all", "remote.origin.fetch"]
        ),
        expected
    );
    assert_eq!(
        run_git(&repo.dest_root, &["config", "--get", "remote.origin.url"]),
        "https://github.com/example/echoagent.git"
    );
    assert!(
        std::fs::read_to_string(repo.source.join(".git/config"))
            .unwrap()
            .contains("+refs/heads/*"),
        "source fetch must stay the wildcard so this test actually rewrites"
    );
}

#[test]
fn copy_rewrites_default_star_mapping_fetch_spec() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    write_commit(&source, "file.txt", "content", "initial");
    let branch = run_git(&source, &["rev-parse", "--abbrev-ref", "HEAD"]);
    run_git(
        &source,
        &["remote", "add", "origin", "https://example.com/repo.git"],
    );
    assert_eq!(
        run_git(&source, &["config", "--get", "remote.origin.fetch"]),
        "+refs/heads/*:refs/remotes/origin/*"
    );
    let (_tmp, dest, _) = copy_source(&source);
    assert_eq!(
        run_git(&dest, &["config", "--get-all", "remote.origin.fetch"]),
        heads_fetch_spec(&branch)
    );
}

#[test]
fn copy_without_origin_does_not_invent_a_remote() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    write_commit(&source, "file.txt", "content", "initial");
    let before = std::fs::read_to_string(source.join(".git/config")).unwrap();
    let (_tmp, dest, dest_git) = copy_source(&source);
    let after = std::fs::read_to_string(dest_git.join("config")).unwrap();
    assert_eq!(after, before);
    let fetch = std::process::Command::new("git")
        .current_dir(&dest)
        .args(["config", "--get", "remote.origin.fetch"])
        .output()
        .unwrap();
    assert!(!fetch.status.success(), "no origin → no fetch spec");
}

#[test]
fn detached_head_uses_exact_non_wildcard_fetch_spec() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    write_commit(&source, "file.txt", "content", "initial");
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/heads/*:refs/remotes/origin/*",
    );
    run_git(&source, &["checkout", "--detach", "HEAD"]);
    let (_tmp, dest, dest_git) = copy_source(&source);
    assert_eq!(
        run_git(&dest, &["config", "--get-all", "remote.origin.fetch"]),
        "+HEAD:refs/remotes/origin/HEAD"
    );
    assert_eq!(
        narrow_origin_fetch_spec(&dest_git),
        "+HEAD:refs/remotes/origin/HEAD"
    );
}

#[test]
fn detached_head_with_origin_head_uses_resolved_default_branch() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    write_commit(&source, "file.txt", "content", "initial");
    let head = run_git(&source, &["rev-parse", "HEAD"]);
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/heads/*",
    );
    run_git(&source, &["update-ref", "refs/remotes/origin/main", &head]);
    std::fs::write(
        source.join(".git/refs/remotes/origin/HEAD"),
        "ref: refs/remotes/origin/main\n",
    )
    .unwrap();
    run_git(&source, &["checkout", "--detach", "HEAD"]);
    let (_tmp, dest, _) = copy_source(&source);
    assert_eq!(
        run_git(&dest, &["config", "--get-all", "remote.origin.fetch"]),
        heads_fetch_spec("main")
    );
}

#[test]
fn inconsistent_shallow_graft_is_not_copied() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    let a = write_commit(&source, "a.txt", "a", "A");
    let b = write_commit(&source, "b.txt", "b", "B");
    run_git(&source, &["checkout", "-b", "feature", &a]);
    let _d = write_commit(&source, "d.txt", "d", "D");
    run_git(&source, &["update-ref", "refs/heads/main", &b]);
    run_git(&source, &["update-ref", "refs/remotes/origin/main", &b]);
    let b_on_head = std::process::Command::new("git")
        .current_dir(&source)
        .args(["merge-base", "--is-ancestor", &b, "HEAD"])
        .status()
        .unwrap();
    assert!(
        !b_on_head.success(),
        "precondition: B must not be on HEAD's first-parent chain"
    );
    let b_on_main = std::process::Command::new("git")
        .current_dir(&source)
        .args(["merge-base", "--is-ancestor", &b, "main"])
        .status()
        .unwrap();
    assert!(
        b_on_main.success(),
        "precondition: B must be origin/main (ancestor of main)"
    );
    std::fs::write(source.join(".git/shallow"), format!("{b}\n")).unwrap();

    let (_tmp, dest, dest_git) = copy_source(&source);
    assert!(
        !dest_git.join("shallow").exists(),
        "inconsistent shallow must not be retained"
    );
    assert_eq!(
        run_git(&dest, &["rev-parse", "--is-shallow-repository"]),
        "false"
    );
    let parent = run_git(&dest, &["rev-parse", "HEAD^"]);
    assert_eq!(parent, a);
    assert_eq!(
        run_git(&dest, &["cat-file", "-t", &parent]),
        "commit",
        "HEAD's parent must still be in the ODB"
    );
}

#[test]
fn consistent_shallow_is_kept() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let full = temp.path().join("full");
    std::fs::create_dir(&full).unwrap();
    init_git_repo(&full);
    write_commit(&full, "a.txt", "a", "A");
    write_commit(&full, "b.txt", "b", "B");
    write_commit(&full, "c.txt", "c", "C");
    let shallow_src = temp.path().join("shallow-src");
    run_git(
        temp.path(),
        &[
            "clone",
            "--depth",
            "2",
            &format!("file://{}", full.display()),
            shallow_src.to_str().unwrap(),
        ],
    );
    assert_eq!(
        run_git(&shallow_src, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    let shallow_before = std::fs::read_to_string(shallow_src.join(".git/shallow")).unwrap();
    assert!(
        !shallow_before.trim().is_empty(),
        "precondition: clone --depth 2 must write .git/shallow"
    );
    let head_parent = run_git(&shallow_src, &["rev-parse", "HEAD^"]);
    assert!(
        shallow_before.to_ascii_lowercase().contains(&head_parent)
            || shallow_before
                .to_ascii_lowercase()
                .contains(&run_git(&shallow_src, &["rev-parse", "HEAD"])),
        "precondition: graft must be HEAD or HEAD's parent, got {shallow_before:?}"
    );

    let dest_root = temp.path().join("dest");
    let dest_git = dest_root.join(".git");
    std::fs::create_dir_all(&dest_root).unwrap();
    copy_git_dir(&shallow_src.join(".git"), &dest_git).unwrap();

    assert_eq!(
        std::fs::read_to_string(dest_git.join("shallow")).unwrap(),
        shallow_before
    );
    assert_eq!(
        run_git(&dest_root, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
}

#[test]
fn extra_origin_remote_refs_are_not_copied() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    let head = write_commit(&source, "file.txt", "content", "initial");
    let branch = run_git(&source, &["rev-parse", "--abbrev-ref", "HEAD"]);
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/heads/*:refs/remotes/origin/*",
    );
    run_git(&source, &["update-ref", "refs/remotes/origin/main", &head]);
    run_git(
        &source,
        &[
            "update-ref",
            &format!("refs/remotes/origin/{branch}"),
            &head,
        ],
    );
    for i in 0..40 {
        run_git(
            &source,
            &[
                "update-ref",
                &format!("refs/remotes/origin/branch-{i}"),
                &head,
            ],
        );
    }
    let (_tmp, dest, dest_git) = copy_source(&source);
    assert_eq!(
        run_git(&dest, &["rev-parse", "refs/remotes/origin/main"]),
        head
    );
    assert_eq!(
        run_git(
            &dest,
            &["rev-parse", &format!("refs/remotes/origin/{branch}")]
        ),
        head
    );
    for i in 0..40 {
        let extra = dest_git.join(format!("refs/remotes/origin/branch-{i}"));
        assert!(
            !extra.exists(),
            "extra loose remote {} must not be copied",
            extra.display()
        );
        let packed = std::fs::read_to_string(dest_git.join("packed-refs")).unwrap_or_default();
        assert!(
            !packed.contains(&format!("refs/remotes/origin/branch-{i}")),
            "extra packed remote branch-{i} must not be copied"
        );
        let show = std::process::Command::new("git")
            .current_dir(&dest)
            .args([
                "show-ref",
                "--verify",
                &format!("refs/remotes/origin/branch-{i}"),
            ])
            .output()
            .unwrap();
        assert!(
            !show.status.success(),
            "origin/branch-{i} must be absent from dest"
        );
    }
}

#[test]
fn copy_keeps_dest_branch_origin_ref_when_source_head_differs() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    let main_tip = write_commit(&source, "file.txt", "content", "initial");
    let default_branch = run_git(&source, &["rev-parse", "--abbrev-ref", "HEAD"]);
    run_git(&source, &["checkout", "-b", "feature"]);
    let feature_tip = write_commit(&source, "feat.txt", "feat", "feature");
    run_git(&source, &["checkout", &default_branch]);
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/heads/*:refs/remotes/origin/*",
    );
    run_git(
        &source,
        &["update-ref", "refs/remotes/origin/main", &main_tip],
    );
    run_git(
        &source,
        &["update-ref", "refs/remotes/origin/feature", &feature_tip],
    );
    run_git(
        &source,
        &["update-ref", "refs/remotes/origin/noise", &main_tip],
    );
    let keep = origin_keep_names_for_git_ref(&source.join(".git"), "feature");
    let dest_root = temp.path().join("dest");
    let dest_git = dest_root.join(".git");
    std::fs::create_dir_all(&dest_root).unwrap();
    copy_git_dir_keeping_origin(&source.join(".git"), &dest_git, &keep).unwrap();

    assert_eq!(
        run_git(&dest_root, &["rev-parse", "refs/remotes/origin/feature"]),
        feature_tip,
        "dest-branch origin ref must survive source-HEAD sanitize"
    );
    assert_eq!(
        run_git(&dest_root, &["rev-parse", "refs/remotes/origin/main"]),
        main_tip
    );
    let noise = std::process::Command::new("git")
        .current_dir(&dest_root)
        .args(["show-ref", "--verify", "refs/remotes/origin/noise"])
        .output()
        .unwrap();
    assert!(
        !noise.status.success(),
        "unrelated origin/noise must still be pruned"
    );
}

#[test]
fn packed_origin_remote_refs_are_pruned() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    let head = write_commit(&source, "file.txt", "content", "initial");
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/heads/*",
    );
    run_git(&source, &["update-ref", "refs/remotes/origin/main", &head]);
    for i in 0..40 {
        run_git(
            &source,
            &[
                "update-ref",
                &format!("refs/remotes/origin/branch-{i}"),
                &head,
            ],
        );
    }
    run_git(&source, &["pack-refs", "--all"]);
    assert!(source.join(".git/packed-refs").is_file());
    let packed_src = std::fs::read_to_string(source.join(".git/packed-refs")).unwrap();
    assert!(
        packed_src.contains("refs/remotes/origin/branch-0"),
        "precondition: extras must be packed"
    );

    let (_tmp, dest, dest_git) = copy_source(&source);
    assert_eq!(
        run_git(&dest, &["rev-parse", "refs/remotes/origin/main"]),
        head
    );
    let packed_dest = std::fs::read_to_string(dest_git.join("packed-refs")).unwrap();
    assert!(
        packed_dest.contains("refs/remotes/origin/main"),
        "allowlisted origin/main must remain in packed-refs"
    );
    for i in 0..40 {
        assert!(
            !packed_dest.contains(&format!("refs/remotes/origin/branch-{i}")),
            "packed-refs must not retain origin/branch-{i}"
        );
    }
}

#[test]
fn copy_rewrites_case_insensitive_fetch_key() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    write_commit(&source, "file.txt", "content", "initial");
    let branch = run_git(&source, &["rev-parse", "--abbrev-ref", "HEAD"]);
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/heads/unused:refs/remotes/origin/unused",
    );
    let config_path = source.join(".git/config");
    let config = std::fs::read_to_string(&config_path).unwrap();
    let config = config.replace(
        "fetch = +refs/heads/unused:refs/remotes/origin/unused",
        "Fetch = +refs/heads/*",
    );
    assert!(
        config.contains("Fetch = +refs/heads/*"),
        "precondition: config must use case-variant Fetch key, got:\n{config}"
    );
    std::fs::write(&config_path, config).unwrap();

    let (_tmp, dest, _) = copy_source(&source);
    assert_eq!(
        run_git(&dest, &["config", "--get-all", "remote.origin.fetch"]),
        heads_fetch_spec(&branch)
    );
    assert_eq!(
        run_git(&dest, &["config", "--get", "remote.origin.url"]),
        "https://github.com/example/echoagent.git"
    );
}

#[test]
fn copy_rewrites_refs_star_wildcard_fetch_spec() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    write_commit(&source, "file.txt", "content", "initial");
    let branch = run_git(&source, &["rev-parse", "--abbrev-ref", "HEAD"]);
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/*:refs/remotes/origin/*",
    );
    let (_tmp, dest, _) = copy_source(&source);
    assert_eq!(
        run_git(&dest, &["config", "--get-all", "remote.origin.fetch"]),
        heads_fetch_spec(&branch)
    );
}

#[test]
fn rewrite_does_not_inject_heads_spec_over_non_heads_fetch_lines() {
    let config = "[remote \"origin\"]\n\turl = https://example.com/repo.git\n\tfetch = +refs/pull/1/head:refs/remotes/origin/pr/1\n";
    let spec = "+refs/heads/main:refs/remotes/origin/main";
    let rewritten = rewrite_origin_fetch_in_config(config, spec).unwrap();
    assert!(
        rewritten.contains("fetch = +refs/pull/1/head:refs/remotes/origin/pr/1"),
        "non-heads fetch must be kept, got:\n{rewritten}"
    );
    assert!(
        !rewritten.contains("refs/heads/main"),
        "must not inject a heads spec when no rewriteable fetch exists, got:\n{rewritten}"
    );
}

#[test]
fn rewrite_quotes_fetch_specs_that_contain_gitconfig_comment_chars() {
    let config = "[remote \"origin\"]\n\turl = https://example.com/repo.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n";
    let hash_spec = "+refs/heads/feat#123:refs/remotes/origin/feat#123";
    let rewritten = rewrite_origin_fetch_in_config(config, hash_spec).unwrap();
    assert!(
        rewritten.contains(r#"fetch = "+refs/heads/feat#123:refs/remotes/origin/feat#123""#),
        "hash in ref name must be quoted, got:\n{rewritten}"
    );
    assert!(
        !rewritten.contains("fetch = +refs/heads/feat#123"),
        "unquoted hash spec is a gitconfig comment, got:\n{rewritten}"
    );

    let semi_spec = "+refs/heads/wip;tmp:refs/remotes/origin/wip;tmp";
    let rewritten = rewrite_origin_fetch_in_config(config, semi_spec).unwrap();
    assert!(
        rewritten.contains(r#"fetch = "+refs/heads/wip;tmp:refs/remotes/origin/wip;tmp""#),
        "semicolon in ref name must be quoted, got:\n{rewritten}"
    );
}

#[test]
fn copy_rewrites_hash_branch_fetch_spec_without_truncating() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    write_commit(&source, "file.txt", "content", "initial");
    run_git(&source, &["checkout", "-b", "feat#123"]);
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/heads/*:refs/remotes/origin/*",
    );
    let (_tmp, dest, dest_git) = copy_source(&source);
    assert_eq!(
        run_git(&dest, &["config", "--get-all", "remote.origin.fetch"]),
        heads_fetch_spec("feat#123")
    );
    let raw = std::fs::read_to_string(dest_git.join("config")).unwrap();
    assert!(
        raw.contains(r#"fetch = "+refs/heads/feat#123:refs/remotes/origin/feat#123""#),
        "raw gitconfig must quote #, got:\n{raw}"
    );
}

#[test]
fn copy_rewrites_quoted_wildcard_fetch_spec() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir(&source).unwrap();
    init_git_repo(&source);
    write_commit(&source, "file.txt", "content", "initial");
    let branch = run_git(&source, &["rev-parse", "--abbrev-ref", "HEAD"]);
    add_origin(
        &source,
        "https://github.com/example/echoagent.git",
        "+refs/heads/unused:refs/remotes/origin/unused",
    );
    let config_path = source.join(".git/config");
    let config = std::fs::read_to_string(&config_path).unwrap();
    let config = config.replace(
        "fetch = +refs/heads/unused:refs/remotes/origin/unused",
        r#"fetch = "+refs/heads/*""#,
    );
    assert!(
        config.contains(r#"fetch = "+refs/heads/*""#),
        "precondition: quoted wildcard fetch, got:\n{config}"
    );
    std::fs::write(&config_path, config).unwrap();

    let (_tmp, dest, _) = copy_source(&source);
    assert_eq!(
        run_git(&dest, &["config", "--get-all", "remote.origin.fetch"]),
        heads_fetch_spec(&branch)
    );
}

#[test]
fn orphan_head_keeps_shallow_when_graft_parent_is_missing() {
    echo_agent_test_utils::require_git!();
    let temp = TempDir::new().unwrap();
    let full = temp.path().join("full");
    std::fs::create_dir(&full).unwrap();
    init_git_repo(&full);
    write_commit(&full, "a.txt", "a", "A");
    write_commit(&full, "b.txt", "b", "B");
    write_commit(&full, "c.txt", "c", "C");
    let shallow_src = temp.path().join("shallow-src");
    run_git(
        temp.path(),
        &[
            "clone",
            "--depth",
            "1",
            &format!("file://{}", full.display()),
            shallow_src.to_str().unwrap(),
        ],
    );
    assert_eq!(
        run_git(&shallow_src, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    let main_tip = run_git(&shallow_src, &["rev-parse", "HEAD"]);
    run_git(
        &shallow_src,
        &["update-ref", "refs/remotes/origin/main", &main_tip],
    );
    run_git(&shallow_src, &["checkout", "--orphan", "pages"]);
    // Clone does not inherit user.identity; git_commit_all would then fail
    // silently on an unborn orphan branch (rev-parse HEAD → "unknown revision").
    std::fs::write(shallow_src.join("pages.txt"), "pages").unwrap();
    run_git(&shallow_src, &["add", "."]);
    run_git(&shallow_src, &["commit", "-m", "pages"]);
    let pages = run_git(&shallow_src, &["rev-parse", "HEAD"]);
    assert_eq!(run_git(&shallow_src, &["rev-parse", "HEAD"]), pages);
    let shallow_before = std::fs::read_to_string(shallow_src.join(".git/shallow")).unwrap();
    assert!(
        !shallow_before.trim().is_empty(),
        "precondition: depth-1 clone must write .git/shallow"
    );
    let parent_missing = std::process::Command::new("git")
        .current_dir(&shallow_src)
        .args(["cat-file", "-t", &format!("{main_tip}^")])
        .output()
        .unwrap();
    assert!(
        !parent_missing.status.success(),
        "precondition: origin/main graft parent must be missing"
    );

    let dest_root = temp.path().join("dest");
    let dest_git = dest_root.join(".git");
    std::fs::create_dir_all(&dest_root).unwrap();
    copy_git_dir(&shallow_src.join(".git"), &dest_git).unwrap();

    assert_eq!(
        std::fs::read_to_string(dest_git.join("shallow")).unwrap(),
        shallow_before,
        "orphan HEAD must not drop a graft that still hides missing parents"
    );
    assert_eq!(
        run_git(&dest_root, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
}

#[test]
fn sanitize_is_idempotent_on_already_narrow_fetch() {
    echo_agent_test_utils::require_git!();
    let repo = setup_repo_with_wildcard_fetch();
    let expected = heads_fetch_spec(&repo.branch);
    sanitize_standalone_git_dir(&repo.dest_git).unwrap();
    assert_eq!(
        run_git(
            &repo.dest_root,
            &["config", "--get-all", "remote.origin.fetch"]
        ),
        expected
    );
}
