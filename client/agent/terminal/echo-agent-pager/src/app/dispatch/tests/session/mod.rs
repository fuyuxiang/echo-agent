//! Tests for session lifecycle, loading, pickers, modals, forking, and trust.

use super::*;

mod foreign;
mod fork;
mod lifecycle;
mod load;
mod modal;
mod take_deferred;

/// Like [`test_app`] but with `cwd` set to this crate's directory,
/// which lives inside the git repo.  Worktree tests require a git
/// ancestor to pass the `has_git_ancestor` pre-check.
fn test_app_git() -> AppView {
    let mut app = test_app();
    app.cwd = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    app.cwd_has_git_ancestor = true;
    app
}

fn count_extension_fetches(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::FetchHooksList { .. }
                    | Effect::FetchPluginsList { .. }
                    | Effect::FetchMarketplaceList { .. }
                    | Effect::FetchMcpsList { .. }
                    | Effect::FetchSkillsList { .. }
            )
        })
        .count()
}

/// Build a single-agent app for the `/new` dispatcher tests.
///
/// Sets `current_branch` to `Some("main")` so the agent appears to be
/// inside a git repo (mirrors `fork_test_app`).
fn new_session_test_app() -> AppView {
    let mut app = test_app_with_agent();
    app.agents.get_mut(&AgentId(0)).unwrap().current_branch = Some("main".into());
    app.cwd_has_git_ancestor = true;
    app
}
