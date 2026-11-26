use super::*;

#[test]
fn modified_or_untracked_file_keeps_the_worktree() {
    use KeepReason::{Dirty, IgnoredContent};
    use Safety::{Delete, Keep};
    let cases: &[(&str, &str, &str, Safety)] = &[
        ("modified-tracked", "", "tracked.txt", Keep(Dirty)),
        ("untracked-source", "", "src/feature.rs", Keep(Dirty)),
        (
            "ignored-secret",
            ".env\n",
            ".env",
            Keep(IgnoredContent(".env".into())),
        ),
        (
            "ignored-secret-under-build",
            "**/.env\n",
            "build/.env",
            Keep(IgnoredContent("build/.env".into())),
        ),
        (
            "ignored-secret-under-an-untracked-build-name",
            "*.env\n",
            "dist/prod.env",
            Keep(IgnoredContent("dist".into())),
        ),
        (
            "untracked-tool-output",
            "",
            "node_modules/pkg/index.js",
            Keep(Dirty),
        ),
        (
            "untracked-directory-named-like-a-build-symlink",
            "",
            "bazel-out/notes.md",
            Keep(Dirty),
        ),
        ("untracked-file-named-target", "", "target", Keep(Dirty)),
        (
            "ignored-directory-named-junk",
            ".DS_Store/\n",
            ".DS_Store/notes.md",
            Keep(IgnoredContent(".DS_Store".into())),
        ),
        ("build-output", "target/\n", "target/debug/artifact", Delete),
        (
            "untracked-directory-under-a-build-name",
            "",
            "build/notes/scratch.md",
            Keep(Dirty),
        ),
        (
            "ignored-tool-cache",
            ".ruff_cache/\n",
            ".ruff_cache/0.1.2/index.json",
            Delete,
        ),
    ];
    for (case, ignore_lines, file, want) in cases {
        let fixture = Fixture::new(ignore_lines);
        let worktree = fixture.linked_worktree(case);
        let written = worktree.join(file);
        std::fs::create_dir_all(written.parent().unwrap()).unwrap();
        std::fs::write(&written, b"work").unwrap();

        assert_eq!(&reclaim(&worktree), want, "{case}");
        match want {
            Keep(_) => assert_eq!(
                std::fs::read(&written)
                    .unwrap_or_else(|e| panic!("{case}: the bytes are gone: {e}")),
                b"work"
            ),
            Delete => assert!(
                !worktree.exists(),
                "{case}: the worktree must be gone, and it is not"
            ),
        }
    }
}

#[test]
fn embedded_repository_keeps_the_worktree_whatever_was_captured() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("embedded");
    let embedded = worktree.join("tool");
    std::fs::create_dir(&embedded).unwrap();
    run_git(&embedded, &["init"]);
    run_git(&embedded, &["config", "core.excludesFile", "/dev/null"]);
    std::fs::write(embedded.join("main.rs"), b"fn main() {}").unwrap();
    run_git(&embedded, &["add", "."]);
    run_git(
        &embedded,
        &["commit", "-m", "work in a repository of its own"],
    );

    assert_eq!(
        reclaim_after_snapshot(&worktree, &fixture.source, "refs/echoagent/subagents/test"),
        Safety::Keep(KeepReason::EmbeddedRepo("tool".to_string()))
    );
    assert_eq!(
        run_git(&embedded, &["show", "HEAD:main.rs"]),
        "fn main() {}"
    );
}

#[cfg(unix)]
#[test]
fn repository_inside_an_ignored_directory_keeps_the_worktree() {
    let fixture = Fixture::new("node_modules/\n");
    let worktree = fixture.linked_worktree("vendored-repo");
    let vendored = worktree.join("node_modules/pkg");
    std::fs::create_dir_all(&vendored).unwrap();
    run_git(&vendored, &["init", "-b", "main"]);
    std::fs::write(vendored.join("main.rs"), b"work only this clone holds").unwrap();
    run_git(&vendored, &["add", "."]);
    run_git(&vendored, &["commit", "-m", "vendored work"]);

    assert_eq!(
        reclaim(&worktree),
        Safety::Keep(KeepReason::EmbeddedRepo("node_modules/pkg".to_string()))
    );
    assert_eq!(
        std::fs::read(vendored.join("main.rs")).expect("the clone's work is gone"),
        b"work only this clone holds"
    );
}

#[test]
fn staged_path_no_snapshot_can_carry_keeps_the_worktree() {
    let fixture = Fixture::new(".env\n");
    let worktree = fixture.linked_worktree("staged-ignored");
    std::fs::write(worktree.join(".env"), b"SECRET").unwrap();
    run_git(&worktree, &["add", "-f", ".env"]);

    assert_eq!(
        reclaim_after_snapshot(&worktree, &fixture.source, "refs/echoagent/subagents/test"),
        Safety::Keep(KeepReason::Dirty)
    );
    assert_eq!(
        std::fs::read(worktree.join(".env")).expect("the secret is gone"),
        b"SECRET"
    );
}

#[test]
fn user_status_untracked_config_does_not_change_the_dirty_verdict() {
    for setting in ["no", "all"] {
        let fixture = Fixture::new("target/\n");
        run_git(
            &fixture.source,
            &["config", "status.showUntrackedFiles", setting],
        );

        let dirty = fixture.linked_worktree(&format!("untracked-{setting}"));
        std::fs::write(dirty.join("feature.rs"), b"work").unwrap();
        assert_eq!(
            reclaim(&dirty),
            Safety::Keep(KeepReason::Dirty),
            "{setting}"
        );
        assert_eq!(
            std::fs::read(dirty.join("feature.rs")).expect("the untracked file is gone"),
            b"work"
        );

        let built = fixture.linked_worktree(&format!("build-{setting}"));
        std::fs::create_dir_all(built.join("target/debug")).unwrap();
        std::fs::write(built.join("target/debug/artifact"), b"built").unwrap();
        assert_eq!(reclaim(&built), Safety::Delete, "{setting}");
    }
}

#[test]
fn git_directory_in_the_working_tree_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("fixtures");
    let ref_name = "refs/echoagent/subagents/fixtures";
    let fixture_file = worktree.join("testdata/.git/fixture.txt");
    std::fs::create_dir_all(fixture_file.parent().unwrap()).unwrap();
    std::fs::write(&fixture_file, b"the only copy of it\n").unwrap();
    std::fs::write(worktree.join("testdata/README"), b"ordinary\n").unwrap();

    assert_eq!(
        reclaim_after_snapshot(&worktree, &fixture.source, ref_name),
        Safety::Keep(KeepReason::EmbeddedRepo("testdata".to_string()))
    );
    assert_eq!(
        std::fs::read(&fixture_file).expect("the fixture is gone"),
        b"the only copy of it\n"
    );
}

#[cfg(unix)]
#[test]
fn pruned_symlink_keeps_the_worktree_and_an_empty_one_does_not() {
    let fixture = Fixture::new("");
    let empty = fixture.linked_worktree("empty-dot-git");
    std::fs::create_dir_all(empty.join("testdata/.git")).unwrap();
    std::fs::write(empty.join("testdata/README"), b"ordinary\n").unwrap();

    assert_eq!(
        reclaim_after_snapshot(
            &empty,
            &fixture.source,
            "refs/echoagent/subagents/empty-dot-git"
        ),
        Safety::Delete,
        "an empty .git holds nothing to lose"
    );
    assert!(!empty.exists());

    let linked = fixture.linked_worktree("linked-dot-git");
    std::fs::create_dir_all(linked.join("testdata")).unwrap();
    std::fs::create_dir_all(linked.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink("../elsewhere", linked.join("testdata/.git")).unwrap();

    assert_eq!(
        safe_to_delete_worktree_after_snapshot(
            &linked,
            Some(&fixture.source),
            "refs/echoagent/subagents/linked-dot-git"
        ),
        Safety::Keep(KeepReason::EmbeddedRepo("testdata".to_string()))
    );
}

#[test]
fn a_submodule_checkout_keeps_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("adopted-clone");
    let store = worktree.join("sub/.git");
    seed_module_store(fixture.source.parent().unwrap(), &store);
    run_git(&worktree.join("sub"), &["checkout", "-f", "main"]);
    run_git(&worktree, &["add", "sub"]);
    run_git(&worktree, &["commit", "-m", "adopt the clone"]);
    publish(&worktree, "adopted");
    assert_eq!(
        run_git(&worktree, &["status", "--porcelain"]),
        "",
        "the walk must see nothing, or this tests the wrong thing"
    );

    assert_eq!(
        reclaim(&worktree),
        Safety::Keep(KeepReason::EmbeddedRepo("sub".to_string()))
    );
    assert_eq!(
        std::fs::read(worktree.join("sub/file.txt")).expect("the adopted clone is gone"),
        b"sub\n"
    );
}

#[test]
fn dirty_submodule_keeps_the_worktree_whatever_was_captured() {
    let fixture = Fixture::new("");
    let upstream = fixture.source.with_file_name("submodule-upstream");
    let scratch = fixture.source.with_file_name("submodule-scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    run_git(
        fixture.source.parent().unwrap(),
        &["init", "--bare", upstream.to_str().unwrap()],
    );
    run_git(&upstream, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    run_git(&scratch, &["init", "-b", "main"]);
    std::fs::write(scratch.join("file.txt"), "sub\n").unwrap();
    run_git(&scratch, &["add", "."]);
    run_git(&scratch, &["commit", "-m", "submodule seed"]);
    run_git(
        &scratch,
        &["push", upstream.to_str().unwrap(), "HEAD:refs/heads/main"],
    );
    let commit = run_git(&scratch, &["rev-parse", "HEAD"]);
    std::fs::write(
        fixture.source.join(".gitmodules"),
        format!(
            "[submodule \"sub\"]\n\tpath = sub\n\turl = {}\n",
            upstream.display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(fixture.source.join("sub")).unwrap();
    run_git(&fixture.source, &["add", ".gitmodules"]);
    run_git(
        &fixture.source,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{commit},sub"),
        ],
    );
    run_git(&fixture.source, &["commit", "-m", "add the submodule"]);
    publish(&fixture.source, "main");

    let worktree = fixture.linked_worktree("dirty-submodule");
    let registration = PathBuf::from(run_git(&worktree, &["rev-parse", "--absolute-git-dir"]));
    let store = registration.join("modules/sub");
    let checkout = worktree.join("sub");
    let staging = fixture.source.with_file_name("submodule-clone");
    run_git(
        fixture.source.parent().unwrap(),
        &[
            "clone",
            "--no-checkout",
            upstream.to_str().unwrap(),
            staging.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(store.parent().unwrap()).unwrap();
    std::fs::rename(staging.join(".git"), &store).unwrap();
    std::fs::remove_dir_all(&staging).unwrap();
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(
        checkout.join(".git"),
        format!("gitdir: {}\n", store.display()),
    )
    .unwrap();
    run_git(
        &checkout,
        &["config", "core.worktree", checkout.to_str().unwrap()],
    );
    run_git(&checkout, &["checkout", "-f", "main"]);
    let inside = worktree.join("sub/file.txt");
    std::fs::write(&inside, b"work only the submodule holds").unwrap();

    assert!(
        worktree.join(".git").is_file() && !fixture.source.join(".git/modules/sub").exists(),
        "the store must be the registration's, or this tests the easy shape"
    );

    assert_eq!(
        reclaim_after_snapshot(&worktree, &fixture.source, "refs/echoagent/subagents/sub"),
        Safety::Keep(KeepReason::EmbeddedRepo("sub".to_string()))
    );
    assert_eq!(
        std::fs::read(&inside).expect("the submodule's work is gone"),
        b"work only the submodule holds"
    );
}

#[test]
fn a_build_directory_goes_only_where_the_repository_excludes_it() {
    for (name, ignore_lines, expected) in [
        ("excluded", "/target/\n", Safety::Delete),
        ("named-only", "", Safety::Keep(KeepReason::Dirty)),
    ] {
        let fixture = Fixture::new(ignore_lines);
        let worktree = fixture.linked_worktree(name);
        let built = worktree.join("target/debug");
        std::fs::create_dir_all(&built).unwrap();
        std::fs::write(built.join("artifact"), b"rebuildable").unwrap();

        assert_eq!(reclaim(&worktree), expected, "{name}");
        match expected {
            Safety::Delete => assert!(!worktree.exists(), "{name}"),
            Safety::Keep(_) => assert_eq!(
                std::fs::read(built.join("artifact")).expect("gone"),
                b"rebuildable",
                "{name}"
            ),
        }
    }

    let fixture = Fixture::new("");
    let global_excludes = fixture.source.parent().unwrap().join("global.excludes");
    std::fs::write(&global_excludes, "build/\n").unwrap();
    run_git(
        &fixture.source,
        &[
            "config",
            "core.excludesFile",
            global_excludes.to_str().unwrap(),
        ],
    );
    let worktree = fixture.linked_worktree("global-only");
    let built = worktree.join("build/notes");
    std::fs::create_dir_all(&built).unwrap();
    std::fs::write(built.join("scratch.md"), b"hand-written").unwrap();
    assert!(
        run_git(&worktree, &["status", "--porcelain", "--ignored=matching"]).contains("build/"),
        "git itself must see the global rule, or this tests nothing"
    );

    let safety = reclaim(&worktree);
    assert!(
        matches!(
            &safety,
            Safety::Keep(KeepReason::IgnoredContent(path)) if path.starts_with("build")
        ),
        "a globally-ignored build/ is still somebody's work, got {safety:?}"
    );
    assert_eq!(
        std::fs::read(built.join("scratch.md")).expect("gone"),
        b"hand-written"
    );
}

#[test]
fn a_cachedir_tag_dir_is_not_treated_as_dirt() {
    let fixture = Fixture::new("debug/\n");
    let worktree = fixture.linked_worktree("tagged-target");
    let built = worktree.join(".rmt-target");
    std::fs::create_dir_all(built.join("debug")).unwrap();
    write_cache_tag(&built);
    std::fs::write(built.join("debug/artifact"), b"built").unwrap();
    assert_eq!(
        run_git(&worktree, &["status", "--porcelain"]),
        "?? .rmt-target/"
    );
    assert!(
        run_git(&worktree, &["status", "--porcelain", "--ignored=matching"])
            .contains(".rmt-target/debug/"),
        "the ignored child must be an entry of its own, or this tests nothing"
    );

    assert_eq!(reclaim(&worktree), Safety::Delete);
    assert!(!worktree.exists());
}

#[test]
fn a_cachedir_tag_at_the_root_does_not_ignore_the_worktree() {
    let fixture = Fixture::new("");
    let worktree = fixture.linked_worktree("tagged-root");
    write_cache_tag(&worktree);
    std::fs::write(worktree.join("feature.rs"), b"work").unwrap();

    assert_eq!(reclaim(&worktree), Safety::Keep(KeepReason::Dirty));
    assert_eq!(
        std::fs::read(worktree.join("feature.rs")).expect("the work is gone"),
        b"work"
    );
}

#[cfg(unix)]
#[test]
fn removing_a_worktree_deletes_bazel_symlinks_not_their_targets() {
    let fixture = Fixture::new("/bazel-*\n");
    let worktree = fixture.linked_worktree("bazel");
    let output_base = fixture.source.with_file_name("output-base");
    std::fs::create_dir_all(&output_base).unwrap();
    std::fs::write(output_base.join("built"), b"hours of compilation").unwrap();
    for name in ["bazel-out", "bazel-bin", "bazel-source"] {
        std::os::unix::fs::symlink(&output_base, worktree.join(name)).unwrap();
    }

    assert_eq!(reclaim(&worktree), Safety::Delete);
    assert!(!worktree.exists());
    assert_eq!(
        std::fs::read(output_base.join("built")).expect("the output base went with the links"),
        b"hours of compilation"
    );
}

fn write_cache_tag(directory: &Path) {
    std::fs::write(
        directory.join("CACHEDIR.TAG"),
        b"Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
}
