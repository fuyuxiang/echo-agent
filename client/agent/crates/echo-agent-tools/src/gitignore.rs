//! Shared gitignore matching utility.
//!
//! Single source of truth for checking whether a path is ignored by
//! `.gitignore` rules. Used by both the initial AGENTS.md discovery
//! (`echo-agent-core::prompt::ignore`) and the runtime tracker
//! (`AgentsMdTracker`).

use ignore::gitignore::Gitignore;
use std::path::Path;

/// Check if a path is ignored by the given gitignore rules.
///
/// Strips `git_root` prefix before matching — gitignore patterns are
/// repo-relative, so `/repo/build/out.o` becomes `build/out.o` when
/// `git_root` is `/repo`.
///
/// This is a pure function — no filesystem access, just `Gitignore::matched()`.
pub fn is_ignored(gitignore: &Gitignore, path: &Path, git_root: Option<&Path>) -> bool {
    let check_path = match git_root {
        Some(root) => match path.strip_prefix(root) {
            Ok(relative) => relative,
            // Outside the repo (e.g. ~/.echo-agent/Agents.md) → not ignored.
            Err(_) => return false,
        },
        None => {
            // Absolute path + no git root → can't strip to repo-relative;
            // the `ignore` crate panics on absolute paths not under root.
            if path.is_absolute() {
                return false;
            }
            path
        }
    };
    // matched_path_or_any_parents checks parent dirs too, so
    // `build/AGENTS.md` correctly matches a `build/` pattern.
    gitignore
        .matched_path_or_any_parents(check_path, check_path.is_dir())
        .is_ignore()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ignore::gitignore::GitignoreBuilder;

    fn build_gitignore(root: &Path, patterns: &[&str]) -> Gitignore {
        let mut builder = GitignoreBuilder::new(root);
        for pattern in patterns {
            builder.add_line(None, pattern).unwrap();
        }
        builder.build().unwrap()
    }

    #[test]
    fn is_ignored_matches_gitignored_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Canonicalize to handle macOS /tmp → /private/tmp
        let root = &dunce::canonicalize(root).unwrap();
        let gi = build_gitignore(root, &["build/"]);
        assert!(is_ignored(&gi, &root.join("build/out.o"), Some(root)));
        assert!(is_ignored(&gi, &root.join("build/sub/file.rs"), Some(root)));
    }

    #[test]
    fn is_ignored_does_not_match_non_gitignored_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let root = &dunce::canonicalize(root).unwrap();
        let gi = build_gitignore(root, &["build/"]);
        assert!(!is_ignored(&gi, &root.join("src/main.rs"), Some(root)));
        assert!(!is_ignored(&gi, &root.join("AGENTS.md"), Some(root)));
    }

    #[test]
    fn is_ignored_strips_git_root_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let root = &dunce::canonicalize(root).unwrap();
        let gi = build_gitignore(root, &["build/"]);
        // With root: strips prefix, matches build/out.o
        assert!(is_ignored(&gi, &root.join("build/out.o"), Some(root)));
        // Without root: relative path still matches
        assert!(is_ignored(
            &gi,
            &std::path::PathBuf::from("build/out.o"),
            None
        ));
    }

    #[test]
    fn is_ignored_returns_false_for_path_outside_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let root = &dunce::canonicalize(root).unwrap();
        let gi = build_gitignore(root, &["build/", "*.md"]);
        // A path completely outside the git root should not be checked
        // against the repo's .gitignore (e.g., ~/.echo-agent/Agents.md).
        let outside_path = std::path::PathBuf::from("/some/other/path/Agents.md");
        assert!(!is_ignored(&gi, &outside_path, Some(root)));
    }

    /// Regression: running outside a git repo panicked with
    /// "path is expected to be under the root" (ignore crate assert).
    #[test]
    fn regression_no_panic_on_absolute_path_without_git_root() {
        let gi = build_gitignore(Path::new("."), &["node_modules/", "*.log"]);
        let abs_path = Path::new("/Users/someone/home/AGENTS.md");

        // Proves the raw crate panics with these inputs.
        assert!(
            std::panic::catch_unwind(|| {
                gi.matched_path_or_any_parents(abs_path, false);
            })
            .is_err()
        );

        // Our wrapper guards against it.
        assert!(!is_ignored(&gi, abs_path, None));
    }
}
