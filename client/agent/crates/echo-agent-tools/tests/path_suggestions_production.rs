//! Data-driven tests for path-not-found hint logic using synthetic filesystem
//! layouts that exercise common model path-guess failure modes.
//!
//! Each [`Case`] sets up a temporary filesystem layout, calls
//! [`path_not_found_hint`] or [`format_not_found_error`], and asserts the
//! output matches what we want the model to see.
//!
//! To add a new pattern, add a `Case` struct literal to the relevant
//! `#[tokio::test]` function.  No boilerplate needed.
//!
//! Run:
//! ```bash
//! cargo test -p echo-agent-tools --test path_suggestions_production
//! ```

use echo_agent_tools::util::path_suggestions::{format_not_found_error, path_not_found_hint};
use std::path::PathBuf;
use tempfile::TempDir;

// ── Helpers ──────────────────────────────────────────────────────────────

/// Set up a temp dir with the given dirs and files, then return (tmpdir, root).
fn setup_fs(dirs: &[&str], files: &[&str]) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    for d in dirs {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    for f in files {
        let p = root.join(f);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, b"").unwrap();
    }
    (tmp, root)
}

/// Extract leaf file names from the `similar` vec for assertion.
fn similar_names(hint: &echo_agent_tools::util::path_suggestions::PathNotFoundHint) -> Vec<String> {
    hint.similar
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// Pattern 1: Hallucinated deep paths — model guesses plausible paths where
// the parent directory exists but the leaf file doesn't.
//
// Examples:
//   path: features/billing/impl/src/.../BillingFeaturesImpl.kt
//   path: subsystem/core/components/impl/src/test
//   path: .github/PULL_REQUEST_TEMPLATE.md
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn pattern1_parent_exists_wrong_leaf_suggests_similar() {
    // Model asks for BillingFeaturesImpl.kt but BillingFeatures.kt exists.
    let (_tmp, root) = setup_fs(
        &["features/billing/impl/src/main/kotlin/com/example/billing"],
        &["features/billing/impl/src/main/kotlin/com/example/billing/BillingFeatures.kt"],
    );
    let cwd = root.clone();
    let missing = root
        .join("features/billing/impl/src/main/kotlin/com/example/billing/BillingFeaturesImpl.kt");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    assert!(hint.suggestion.is_none(), "dropped-folder should not fire");
    let names = similar_names(&hint);
    assert!(
        names.iter().any(|n| n == "BillingFeatures.kt"),
        "should suggest BillingFeatures.kt, got: {names:?}"
    );
}

#[tokio::test]
async fn pattern1_pluralization_typo() {
    // Model asks for "lib" directory but "libs" exists at root.
    let (_tmp, root) = setup_fs(&["libs"], &[]);
    let cwd = root.clone();
    let missing = root.join("lib");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    let names = similar_names(&hint);
    assert!(
        names.iter().any(|n| n == "libs"),
        "should suggest 'libs' for 'lib', got: {names:?}"
    );
}

#[tokio::test]
async fn pattern1_missing_extension() {
    // Model asks for "README" but "README.md" exists.
    let (_tmp, root) = setup_fs(&[], &["README.md"]);
    let cwd = root.clone();
    let missing = root.join("README");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    let names = similar_names(&hint);
    assert!(
        names.iter().any(|n| n == "README.md"),
        "should suggest README.md for README, got: {names:?}"
    );
}

#[tokio::test]
async fn pattern1_wrong_suffix() {
    // Model asks for "helpers.rs" but "helper.rs" exists.
    let (_tmp, root) = setup_fs(&["src/util"], &["src/util/helper.rs"]);
    let cwd = root.clone();
    let missing = root.join("src/util/helpers.rs");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    let names = similar_names(&hint);
    assert!(
        names.iter().any(|n| n == "helper.rs"),
        "should suggest helper.rs for helpers.rs, got: {names:?}"
    );
}

#[tokio::test]
async fn pattern1_parent_dir_itself_missing() {
    // Model asks for "nonexistent_dir/foo.rs" — parent doesn't exist either.
    // Should gracefully return empty similar, no crash.
    let (_tmp, root) = setup_fs(&[], &[]);
    let cwd = root.clone();
    let missing = root.join("nonexistent_dir/foo.rs");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    assert!(hint.suggestion.is_none());
    assert!(hint.similar.is_empty(), "no parent dir to scan");
    assert!(!hint.cwd_note.is_empty(), "CWD note always present");
}

#[tokio::test]
async fn pattern1_contributing_md_guess() {
    // Model guesses CONTRIBUTING.md exists (common file, not every repo).
    // No similar names should appear if nothing matches.
    let (_tmp, root) = setup_fs(&[".github"], &["LICENSE", "Cargo.toml"]);
    let cwd = root.clone();
    let missing = root.join("CONTRIBUTING.md");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    assert!(hint.suggestion.is_none());
    // "CONTRIBUTING.md" doesn't substring-match "LICENSE" or "Cargo.toml"
    // so similar should be empty.
    // (It might match ".github" since "contribut" doesn't contain ".github".)
    assert!(!hint.cwd_note.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// Pattern 2: Absolute paths to wrong locations — model uses absolute paths
// pointing outside CWD (other user homes, worktree internals, cargo registry).
//
// Examples:
//   path: /Users/alice/.cargo/registry/...        (cwd: /Users/alice/project)
//   path: /tmp/.tool/sessions/%2F.../terminal/..  (cwd: /workspace/repo)
//   path: /Users/bob/workspace/worktrees/app/..   (cwd: /Users/bob/workspace/app/...)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn pattern2_absolute_path_completely_different_tree() {
    // Model asks for /Users/other/project/src/foo.rs, cwd is /Users/me/project.
    // Completely unrelated — no suggestion, just CWD note.
    let (_tmp, root) = setup_fs(&["src"], &["src/foo.rs"]);
    let cwd = root.clone();
    let unrelated = PathBuf::from("/Users/other/project/src/foo.rs");

    let hint = path_not_found_hint(&unrelated, &cwd, &cwd).await;

    assert!(hint.suggestion.is_none());
    assert!(hint.similar.is_empty());
    assert!(hint.cwd_note.contains(&cwd.display().to_string()));
}

#[tokio::test]
async fn pattern2_echo_agent_sessions_internal_path() {
    // Model searches internal session paths — no suggestion should fire.
    let (_tmp, root) = setup_fs(&["src"], &[]);
    let cwd = root.clone();
    let internal =
        PathBuf::from("/tmp/.tool/sessions/%2Fworkspace%2Frepo/abc-123/terminal/log.txt");

    let hint = path_not_found_hint(&internal, &cwd, &cwd).await;

    assert!(hint.suggestion.is_none());
    assert!(hint.similar.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// Pattern 3: Dropped repo folder — model omits the repo directory name from
// the path. E.g. asks for /parent/src when CWD is /parent/repo and
// /parent/repo/src exists.
//
// This is the primary target of try_suggest_under_cwd().
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn pattern3_dropped_folder_with_display_remap() {
    // Worktree scenario: resolved cwd differs from display cwd.
    // Suggestion must show the display path, not the resolved path.
    let (_tmp, root) = setup_fs(&["worktree/project/src"], &[]);
    let resolved_cwd = root.join("worktree/project");
    let display_cwd = PathBuf::from("/home/user/project");

    // Model asks for /home/user/src (dropped "project" folder from display path).
    // But try_suggest_under_cwd works on resolved paths, so we need the resolved
    // equivalent: root/worktree/src.
    let resolved_missing = root.join("worktree/src");

    let hint = path_not_found_hint(&resolved_missing, &resolved_cwd, &display_cwd).await;

    if let Some(ref suggestion) = hint.suggestion {
        // Suggestion must be in display space, not resolved space.
        let s = suggestion.display().to_string();
        assert!(
            s.contains("/home/user/project/"),
            "suggestion should use display path, got: {s}"
        );
        assert!(
            !s.contains("worktree"),
            "suggestion must NOT leak resolved worktree path, got: {s}"
        );
    } else {
        panic!("expected a dropped-folder suggestion");
    }
}

#[tokio::test]
async fn pattern3_dropped_folder_relative_path_skipped() {
    // Relative paths should never trigger the dropped-folder detector.
    let (_tmp, root) = setup_fs(&["repo/src"], &[]);
    let cwd = root.join("repo");

    let relative_missing = PathBuf::from("src/nonexistent.rs");

    let hint = path_not_found_hint(&relative_missing, &cwd, &cwd).await;

    assert!(
        hint.suggestion.is_none(),
        "relative paths must not trigger dropped-folder detection"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Pattern 4: Common prefix guesses — model uses src/, app/, lib/ as first
// component but the repo doesn't have that top-level dir, or uses a variant.
//
// Examples:
//   path: src/search_engine/index.py  (repo has no top-level src/)
//   path: lib/utils.rs                (repo uses libs/ not lib/)
//   path: app/_components/galaxy      (wrong component dir name)
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn pattern4_lib_vs_libs() {
    // Model asks for "lib/utils.rs", repo has "libs/" directory.
    let (_tmp, root) = setup_fs(&["libs"], &["libs/utils.rs"]);
    let cwd = root.clone();
    let missing = root.join("lib");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    let names = similar_names(&hint);
    assert!(
        names.iter().any(|n| n == "libs"),
        "should suggest 'libs' when model asks for 'lib', got: {names:?}"
    );
}

#[tokio::test]
async fn pattern4_src_does_not_exist_no_misleading_suggestion() {
    // Model asks for src/main.py but top-level has no src/ and nothing similar.
    let (_tmp, root) = setup_fs(&["python", "scripts"], &["setup.py"]);
    let cwd = root.clone();
    let missing = root.join("src");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    assert!(hint.suggestion.is_none());
    let names = similar_names(&hint);
    // "src" is 3 chars; should not match "python", "scripts", or "setup.py"
    assert!(
        !names.iter().any(|n| n == "setup.py"),
        "should not suggest unrelated files, got: {names:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Pattern 5: Root-level file guesses — model guesses a file exists at the
// repo root when it doesn't (CONTRIBUTING.md, .github, etc).
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn pattern5_root_file_with_close_match() {
    // Model asks for "CHANGELOG" (no extension), "CHANGELOG.md" exists.
    let (_tmp, root) = setup_fs(&[], &["CHANGELOG.md"]);
    let cwd = root.clone();
    let missing = root.join("CHANGELOG");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    let names = similar_names(&hint);
    assert!(
        names.iter().any(|n| n == "CHANGELOG.md"),
        "should suggest CHANGELOG.md, got: {names:?}"
    );
}

#[tokio::test]
async fn pattern5_root_file_no_match() {
    // Model asks for a crate-like name that is not a path at root.
    let (_tmp, root) = setup_fs(&["crates", "scripts"], &["Cargo.toml", "Cargo.lock"]);
    let cwd = root.clone();
    let missing = root.join("example-cli-tool");

    let hint = path_not_found_hint(&missing, &cwd, &cwd).await;

    assert!(hint.suggestion.is_none());
    // Unrelated root query should not substring-match "crates", "scripts", etc.
    let names = similar_names(&hint);
    assert!(
        names.is_empty(),
        "should have no similar names for unrelated query, got: {names:?}"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// format_not_found_error — integration tests verifying the full formatted
// output string for each major pattern.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn format_hallucinated_deep_path_with_similar() {
    // Model asks for wrong leaf in a deep path where parent exists.
    let (_tmp, root) = setup_fs(
        &["src/components"],
        &[
            "src/components/Button.tsx",
            "src/components/ButtonGroup.tsx",
        ],
    );
    let cwd = root.clone();
    let missing = root.join("src/components/Buttons.tsx");

    let msg = format_not_found_error(&missing, &missing, &cwd, &cwd, true).await;

    assert!(msg.contains("does not exist"), "msg: {msg}");
    assert!(
        msg.contains("Similar entries in parent directory"),
        "should show similar names, msg: {msg}"
    );
    assert!(
        msg.contains("Button.tsx") || msg.contains("ButtonGroup.tsx"),
        "should suggest a Button variant, msg: {msg}"
    );
    assert!(
        msg.contains("Note: your current working directory"),
        "msg: {msg}"
    );
}

#[tokio::test]
async fn format_hints_disabled_bare_error() {
    // When hints are off, output must be identical to the old behavior.
    let (_tmp, root) = setup_fs(&["src"], &["src/real.rs"]);
    let cwd = root.clone();
    let missing = root.join("src/fake.rs");

    let msg = format_not_found_error(&missing, &missing, &cwd, &cwd, false).await;

    assert!(msg.contains("does not exist"), "msg: {msg}");
    assert!(!msg.contains("Note:"), "no hints when disabled, msg: {msg}");
    assert!(
        !msg.contains("Similar"),
        "no suggestions when disabled, msg: {msg}"
    );
    assert!(
        !msg.contains("Did you mean"),
        "no suggestions when disabled, msg: {msg}"
    );
}

#[tokio::test]
async fn format_no_match_just_cwd_note() {
    // When nothing matches, the output should just have the CWD note.
    let (_tmp, root) = setup_fs(&[], &["totally_unrelated.py"]);
    let cwd = root.clone();
    let missing = root.join("xyz_nonexistent");

    let msg = format_not_found_error(&missing, &missing, &cwd, &cwd, true).await;

    assert!(msg.contains("does not exist"), "msg: {msg}");
    assert!(
        msg.contains("Note: your current working directory"),
        "msg: {msg}"
    );
    assert!(!msg.contains("Did you mean"), "msg: {msg}");
    assert!(!msg.contains("Similar"), "msg: {msg}");
}

#[tokio::test]
async fn format_dropped_folder_shows_display_path() {
    // Dropped-folder suggestion must use display path in final output.
    let (_tmp, root) = setup_fs(&["repo/src"], &[]);
    let cwd = root.join("repo");
    let display_cwd = &cwd; // same for simplicity
    let bad_path = root.join("src"); // dropped "repo"

    let msg = format_not_found_error(&bad_path, &bad_path, &cwd, display_cwd, true).await;

    assert!(msg.contains("does not exist"), "msg: {msg}");
    assert!(msg.contains("Did you mean"), "msg: {msg}");
    assert!(
        msg.contains("Note: your current working directory"),
        "msg: {msg}"
    );
}
