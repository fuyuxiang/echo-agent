//! Nested ignore-file chain used by workspace file listings.
//!
//! Walks from `path.parent()` upwards through `ignore_files` (defaults to
//! `.gitignore`, `.ignore`, `.echoagentignore`). At each ancestor directory it
//! loads any matching ignore rule file and asks `ignore::gitignore` whether
//! the target path is matched. A deeper `!whitelist` always wins over a
//! shallower `ignore`, mirroring Git's precedence.

use std::path::Path;

/// Default ignore-file basenames consulted by the chain.
pub const DEFAULT_IGNORE_FILES: &[&str] = &[".gitignore", ".ignore", ".echoagentignore"];

/// Return true when the target path should be hidden by the ignore chain.
///
/// `ignore_files` lists the basenames (without leading `./`) of files to
/// inspect at every ancestor directory, e.g. `&[".gitignore",
/// ".echoagentignore"]`. The closest ancestor is consulted first.
pub fn apply_nested_gitignore(
    root: &Path,
    path: &Path,
    is_dir: bool,
    ignore_files: &[&str],
) -> bool {
    if !path.starts_with(root) {
        // Outside the workspace: never honour an ignore rule, treat as not-ignored
        // so workspace-authorised listings stay consistent with `require_workspace`.
        return false;
    }
    let mut directory: Option<&Path> = path.parent();
    while let Some(current) = directory {
        if !current.starts_with(root) {
            break;
        }
        for name in ignore_files {
            let ignore_path = current.join(name);
            if !ignore_path.is_file() {
                continue;
            }
            let mut builder = ignore::gitignore::GitignoreBuilder::new(current);
            let _ = builder.add(ignore_path);
            if let Ok(matcher) = builder.build() {
                // Build a path relative to the ignore file's directory so the
                // matcher can apply its patterns correctly (absolute paths are
                // not honoured by `matched_path_or_any_parents`).
                let relative = path.strip_prefix(current).unwrap_or(path);
                let matched = matcher.matched_path_or_any_parents(relative, is_dir);
                if matched.is_ignore() {
                    return true;
                }
                if matched.is_whitelist() {
                    return false;
                }
            }
        }
        if current == root {
            break;
        }
        directory = current.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
    }

    #[test]
    fn single_layer_gitignore_ignores_matching_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "dist/\n");
        let dist = root.join("dist");
        fs::create_dir(&dist).unwrap();
        assert!(apply_nested_gitignore(
            root,
            &dist,
            true,
            DEFAULT_IGNORE_FILES
        ));
    }

    #[test]
    fn deeper_whitelist_overrides_shallower_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".gitignore"), "**/secret.txt\n");
        let sub = root.join("sub");
        fs::create_dir(&sub).unwrap();
        write(&sub.join(".echoagentignore"), "!secret.txt\n");
        let secret = sub.join("secret.txt");
        write(&secret, "shh");
        assert!(!apply_nested_gitignore(
            root,
            &secret,
            false,
            DEFAULT_IGNORE_FILES,
        ));
    }

    #[test]
    fn outside_workspace_is_never_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let outside = tempfile::tempdir().unwrap();
        let some_path = outside.path().join("foo.txt");
        write(&some_path, "x");
        assert!(!apply_nested_gitignore(
            root,
            &some_path,
            false,
            DEFAULT_IGNORE_FILES,
        ));
    }

    #[test]
    fn unrecognised_rule_file_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // `.customignore` lists `vendor/`, but we ask about `dist/`. The
        // matcher should report NoMatch → not ignored.
        write(&root.join(".customignore"), "vendor/\n");
        let dist = root.join("dist");
        fs::create_dir(&dist).unwrap();
        assert!(!apply_nested_gitignore(
            root,
            &dist,
            true,
            &[".customignore"]
        ));
    }

    #[test]
    fn echoagentignore_alone_filters() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join(".echoagentignore"), "build/\n");
        let build = root.join("build");
        fs::create_dir(&build).unwrap();
        assert!(apply_nested_gitignore(
            root,
            &build,
            true,
            DEFAULT_IGNORE_FILES
        ));
    }
}
