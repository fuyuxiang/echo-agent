pub mod config;
pub(crate) mod dual_clock;
pub mod echo_agent_auth_credentials;
pub mod hooks;
pub mod limits;
pub(crate) mod subprocess;
pub(crate) mod user_identity;

// The foundation utilities live in `echo-agent-runtime-base` (upstream of this
// crate so they build in parallel). Re-exported at the original paths so
// existing `crate::util::…` / `echo_agent_runtime::util::…` users compile
// unchanged.
pub use echo_agent_runtime_base::util::*;

pub(crate) fn is_user_instruction_path(
    path: &std::path::Path,
    echo_agent_home: &std::path::Path,
    vendor_homes: &[(std::path::PathBuf, bool)],
    workspace_roots: &[&std::path::Path],
) -> bool {
    let parent = path.parent();
    let echo_agent_rules = echo_agent_home.join("rules");
    let is_exact_home_surface = parent
        .is_some_and(|parent| parent == echo_agent_home || parent == echo_agent_rules)
        || vendor_homes.iter().any(|(vendor_home, named_enabled)| {
            parent.is_some_and(|parent| {
                (*named_enabled && parent == vendor_home) || parent == vendor_home.join("rules")
            })
        });
    if is_exact_home_surface {
        return true;
    }
    // Both prefixes are workspace because forks mix display-rewritten and on-disk paths.
    if workspace_roots.iter().any(|root| path.starts_with(root)) {
        return false;
    }
    path.starts_with(echo_agent_home)
        || vendor_homes
            .iter()
            .any(|(vendor_home, _)| path.starts_with(vendor_home))
}

/// Aborts the wrapped tokio task when dropped.
///
/// Use to tie a spawned helper task's lifetime to an async scope so that
/// cancelling the parent future (e.g. a turn abort dropping the tool loop)
/// also tears down the helper instead of leaving it running detached.
/// Aborting an already-finished task is a no-op, so this is safe to hold
/// across normal scope exit too.
pub(crate) struct AbortOnDrop(pub tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Expand a leading `~` to the home directory; other paths pass through.
pub(crate) fn expand_home(s: &str) -> std::path::PathBuf {
    if let Some(stripped) = s.strip_prefix("~/") {
        if let Some(home) = echo_agent_config::discovery_home_dir() {
            return home.join(stripped);
        }
    } else if s == "~"
        && let Some(home) = echo_agent_config::discovery_home_dir()
    {
        return home;
    }
    std::path::PathBuf::from(s)
}

#[cfg(test)]
mod expand_home_tests {
    use super::expand_home;

    #[test]
    fn passthrough_for_absolute_path() {
        assert_eq!(
            expand_home("/abs/path"),
            std::path::PathBuf::from("/abs/path")
        );
    }

    #[test]
    fn passthrough_for_relative_path() {
        assert_eq!(
            expand_home("rel/path"),
            std::path::PathBuf::from("rel/path")
        );
    }

    #[test]
    fn bare_tilde() {
        let home = echo_agent_config::discovery_home_dir().expect("home_dir required for this test");
        assert_eq!(expand_home("~"), home);
    }

    #[test]
    fn tilde_slash() {
        let home = echo_agent_config::discovery_home_dir().expect("home_dir required for this test");
        assert_eq!(expand_home("~/foo/bar"), home.join("foo/bar"));
    }

    #[test]
    fn does_not_handle_user_tilde() {
        // `~bob/path` is treated as a literal relative path.
        assert_eq!(
            expand_home("~bob/path"),
            std::path::PathBuf::from("~bob/path")
        );
    }
}

#[cfg(test)]
mod is_user_instruction_path_tests {
    use super::is_user_instruction_path;
    use std::path::Path;

    #[test]
    fn echo_agent_home_named_file_nested_in_workspace_is_user_scoped() {
        assert!(is_user_instruction_path(
            Path::new("/repo/config/AGENTS.md"),
            Path::new("/repo/config"),
            &[],
            &[Path::new("/repo")],
        ));
        assert!(!is_user_instruction_path(
            Path::new("/repo/config/src/AGENTS.md"),
            Path::new("/repo/config"),
            &[],
            &[Path::new("/repo")],
        ));
    }

    #[test]
    fn workspace_descendants_under_echo_agent_home_stay_project_scoped() {
        assert!(!is_user_instruction_path(
            Path::new("/custom/echoagent/worktrees/repo/src/AGENTS.md"),
            Path::new("/custom/echoagent"),
            &[],
            &[Path::new("/custom/echoagent/worktrees/repo")],
        ));
    }
}
