//! Optional multi-user workspace helpers for loading per-user agent config.
//!
//! When optional workspace root and user env vars are set and the resolved
//! directory exists, that path can contribute AGENTS.md / rules / skills
//! discovery. Unset env vars are a no-op (typical for standalone installs).

use std::path::PathBuf;

/// If optional workspace env vars are set, returns the user's config directory
/// when the resolved path exists on disk. Unset or missing paths yield `None`.
pub fn optional_workspace_user_dir() -> Option<PathBuf> {
    let root = std::env::var("ECHO_AGENT_ROOT").ok()?;
    let user = std::env::var("ECHO_AGENT_USER").ok()?;
    resolve_workspace_user_dir(&root, &workspace_user_relpath(&user))
}

/// Map `$ECHO_AGENT_USER` to a path relative to the workspace root.
///
/// A bare username is nested one level under `x/` so it cannot collide with an
/// unrelated same-named directory at the workspace root. Values that already
/// contain a path separator are used as-is (explicit relative path).
fn workspace_user_relpath(user: &str) -> String {
    if user.contains('/') || user.contains('\\') {
        user.to_string()
    } else {
        format!("x/{user}")
    }
}

/// Pure logic: join `root` with a relative `user` path and return it if the
/// directory exists on disk.
///
/// Returns `None` if either argument is empty or the resulting path is not
/// a directory.
///
/// Example: `resolve_workspace_user_dir("/workspace", "users/alice")`
///          → `Some("/workspace/users/alice")` if that directory exists.
pub fn resolve_workspace_user_dir(root: &str, user: &str) -> Option<PathBuf> {
    if root.is_empty() || user.is_empty() {
        return None;
    }
    let path = PathBuf::from(root).join(user);
    path.is_dir().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ── resolve_workspace_user_dir (pure, no env vars) ───────────────

    #[test]
    fn resolve_returns_none_for_empty_root() {
        assert!(resolve_workspace_user_dir("", "users/someone").is_none());
    }

    #[test]
    fn resolve_returns_none_for_empty_user() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_workspace_user_dir(tmp.path().to_str().unwrap(), "").is_none());
    }

    #[test]
    fn resolve_returns_none_for_both_empty() {
        assert!(resolve_workspace_user_dir("", "").is_none());
    }

    #[test]
    fn resolve_returns_none_when_dir_does_not_exist() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(
            resolve_workspace_user_dir(tmp.path().to_str().unwrap(), "users/nonexistent").is_none()
        );
    }

    #[test]
    fn resolve_returns_path_when_dir_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let user_dir = tmp.path().join("users").join("testuser");
        fs::create_dir_all(&user_dir).unwrap();

        let result = resolve_workspace_user_dir(tmp.path().to_str().unwrap(), "users/testuser");
        assert_eq!(result, Some(user_dir));
    }

    #[test]
    fn resolve_handles_single_component_user() {
        let tmp = tempfile::tempdir().unwrap();
        let user_dir = tmp.path().join("alice");
        fs::create_dir_all(&user_dir).unwrap();

        let result = resolve_workspace_user_dir(tmp.path().to_str().unwrap(), "alice");
        assert_eq!(result, Some(user_dir));
    }

    #[test]
    fn resolve_handles_deeply_nested_user() {
        let tmp = tempfile::tempdir().unwrap();
        let user_dir = tmp.path().join("org").join("team").join("user");
        fs::create_dir_all(&user_dir).unwrap();

        let result = resolve_workspace_user_dir(tmp.path().to_str().unwrap(), "org/team/user");
        assert_eq!(result, Some(user_dir));
    }

    #[test]
    fn resolve_returns_none_when_path_is_file_not_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("users").join("testuser");
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        fs::write(&file_path, "not a directory").unwrap();

        assert!(
            resolve_workspace_user_dir(tmp.path().to_str().unwrap(), "users/testuser").is_none()
        );
    }

    #[test]
    fn resolve_supports_nested_user_layout_path() {
        let tmp = tempfile::tempdir().unwrap();
        let user_dir = tmp.path().join("x").join("testuser");
        fs::create_dir_all(&user_dir).unwrap();

        let result = resolve_workspace_user_dir(tmp.path().to_str().unwrap(), "x/testuser");
        assert_eq!(result, Some(user_dir));
    }

    // ── workspace_user_relpath ───────────────────────────────────────

    #[test]
    fn bare_username_is_nested_under_x() {
        assert_eq!(workspace_user_relpath("alice"), "x/alice");
        assert_eq!(workspace_user_relpath("bob"), "x/bob");
    }

    #[test]
    fn multi_segment_user_is_explicit_relative_path() {
        assert_eq!(workspace_user_relpath("users/alice"), "users/alice");
        assert_eq!(workspace_user_relpath(r"users\alice"), r"users\alice");
    }

    #[test]
    fn bare_username_does_not_resolve_to_same_named_root_dir() {
        // Prefer the nested layout even when a same-named directory exists at
        // the workspace root.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("alice")).unwrap();
        let user_dir = root.join("x").join("alice");
        fs::create_dir_all(&user_dir).unwrap();

        let rel = workspace_user_relpath("alice");
        let resolved = resolve_workspace_user_dir(root.to_str().unwrap(), &rel);
        assert_eq!(resolved, Some(user_dir));
        assert_ne!(
            resolved.as_deref(),
            Some(root.join("alice").as_path()),
            "must not resolve to a same-named directory at the workspace root"
        );
    }
}
