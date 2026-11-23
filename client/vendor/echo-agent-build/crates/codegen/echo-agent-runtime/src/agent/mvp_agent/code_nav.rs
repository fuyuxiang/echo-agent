//! Code-navigation eligibility gating and codebase-index management for [`MvpAgent`].
//! Co-located child of `mvp_agent` (`use super::*`).

use super::*;

impl MvpAgent {
    /// Parse the `echo.agent/codeNavigation.enabled` capability from an initialize
    /// request.  Returns `false` if the field is absent or not `true`.
    pub(crate) fn parse_code_nav_capability(init: &acp::InitializeRequest) -> bool {
        init.client_capabilities
            .meta
            .as_ref()
            .and_then(|m| m.get("echo.agent/codeNavigation"))
            .and_then(|v| v.get("enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Start (or reuse) the codebase index for an eligible code-nav request.
    ///
    /// Returns `Some((handle, was_newly_started))` on success or `None` when
    /// config/git-root checks prevent starting.  The bool is the authoritative
    /// "first spawn vs reuse" signal threaded up from `CodebaseIndexManager`.
    ///
    /// This is the narrow `pub(crate)` entry point for lazy index startup
    /// from `extensions/code_nav.rs`.  Callers must verify eligibility with
    /// [`code_nav_eligibility_for_request`] before calling this.
    #[cfg(not(feature = "desktop"))]
    pub(crate) fn start_codebase_index_for_code_nav(
        &self,
        session_id: Option<&acp::SessionId>,
        cwd: &std::path::Path,
    ) -> Option<(std::sync::Arc<echo_agent_codebase_graph::IndexManagerHandle>, bool)> {
        let (handle, was_newly_started) = self.resolve_codebase_index(cwd)?;
        // Pin the index to the requesting session so the Weak in
        // CodebaseIndexManager doesn't orphan it immediately.
        if let Some(sid) = session_id {
            self.session_registry
                .set_codebase_index(sid, std::sync::Arc::clone(&handle));
        }
        Some((handle, was_newly_started))
    }

    /// Core eligibility check — pure function that accepts explicit client
    /// context rather than reading global agent state.
    ///
    /// This is the single place that applies all four gates.  Call it via
    /// [`code_nav_eligibility_for_request`] (leader-mode safe) or
    /// [`code_nav_eligibility`] (global state, non-leader use only).
    #[cfg(not(feature = "desktop"))]
    pub(super) fn code_nav_eligibility_inner(
        &self,
        cwd: &std::path::Path,
        client_type: ClientType,
        code_nav_enabled: bool,
    ) -> Result<(), CodeNavEligibility> {
        use crate::agent::config::CodebaseIndexingSetting;

        // Gate 1: client type
        if !matches!(client_type, ClientType::EchoAgentWeb) {
            tracing::info!(
                client_type = ?client_type,
                gate = "client_type",
                skip_reason = "client_not_web",
                "code-nav eligibility check: skipping (client type not eligible)"
            );
            return Err(CodeNavEligibility::ClientNotWeb);
        }

        // Gate 2: capability advertised
        if !code_nav_enabled {
            tracing::info!(
                gate = "capability",
                skip_reason = "capability_not_advertised",
                "code-nav eligibility check: skipping (echo.agent/codeNavigation.enabled not advertised)"
            );
            return Err(CodeNavEligibility::CapabilityNotAdvertised);
        }

        // Gate 3: config
        let setting = self.cfg.borrow().features.codebase_indexing.clone();
        if let CodebaseIndexingSetting::Enabled(false) = &setting {
            tracing::info!(
                gate = "config",
                skip_reason = "disabled_by_config",
                "code-nav eligibility check: skipping (codebase_indexing disabled in config)"
            );
            return Err(CodeNavEligibility::DisabledByConfig);
        }

        // Gate 4: git root / config globs
        let git_root = echo_agent_workspace::session::git::find_git_root_from_path(cwd).ok();
        match &setting {
            CodebaseIndexingSetting::Enabled(true) => {
                if git_root.is_none() {
                    tracing::info!(
                        cwd = %cwd.display(),
                        gate = "git_root",
                        skip_reason = "not_git_repo",
                        "code-nav eligibility check: skipping (not inside a git repo)"
                    );
                    return Err(CodeNavEligibility::NotGitRepo);
                }
            }
            CodebaseIndexingSetting::Patterns(_) => {
                let check_path = git_root.as_deref().unwrap_or(cwd);
                if !setting.should_index(check_path) {
                    tracing::info!(
                        cwd = %cwd.display(),
                        gate = "config_globs",
                        skip_reason = "disabled_by_config",
                        "code-nav eligibility check: skipping (not matched by config globs)"
                    );
                    return Err(CodeNavEligibility::DisabledByConfig);
                }
            }
            CodebaseIndexingSetting::Enabled(false) => {} // handled above
        }

        Ok(())
    }

    /// Check eligibility using per-session context (leader-mode safe).
    ///
    /// When `session_id` is provided, reads the session's own client type
    /// and code-nav capability — the values that were in effect when that
    /// specific client created the session.  This is correct in leader mode
    /// where multiple clients share one agent process and `initialize()` is
    /// called once per connection; the global fields on `MvpAgent` reflect
    /// only the **last** client to call `initialize()`.
    ///
    /// Falls back to global agent state when no session_id is given.
    #[cfg(not(feature = "desktop"))]
    pub(crate) fn code_nav_eligibility_for_request(
        &self,
        session_id: Option<&acp::SessionId>,
        cwd: &std::path::Path,
    ) -> Result<(), CodeNavEligibility> {
        let session_id = match session_id {
            Some(sid) => sid,
            // No session_id: per-client capability cannot be determined without a
            // session.  Reject with SessionRequired rather than fall back to shared
            // global state.  Callers must provide sessionId for echo.agent/code/* requests.
            None => return Err(CodeNavEligibility::SessionRequired),
        };

        let (client_type, code_nav_enabled) = if let Some(handle) = self.resident_handle(session_id)
        {
            let ct = crate::http::client_type_from_origin(handle.origin_client.as_ref());
            (ct, handle.code_nav_enabled)
        } else {
            // Session not found (evicted/unknown): reject rather than silently
            // falling back to shared global state: that would reintroduce the
            // last-client-wins bug for stale session IDs in leader mode.
            return Err(CodeNavEligibility::SessionRequired);
        };
        self.code_nav_eligibility_inner(cwd, client_type, code_nav_enabled)
    }

    /// Resolve and get-or-create the codebase index for `cwd`, applying config
    /// and git-root eligibility checks.
    ///
    /// Returns `Some((handle, was_newly_started))` when an index is available,
    /// `None` when config or git-root checks rule it out.  The bool is the
    /// authoritative "was this a first spawn?" signal from the manager.
    #[cfg(not(feature = "desktop"))]
    pub(super) fn resolve_codebase_index(
        &self,
        cwd: &std::path::Path,
    ) -> Option<(std::sync::Arc<echo_agent_codebase_graph::IndexManagerHandle>, bool)> {
        use crate::agent::config::CodebaseIndexingSetting;

        let setting = self.cfg.borrow().features.codebase_indexing.clone();
        let git_root = echo_agent_workspace::session::git::find_git_root_from_path(cwd).ok();

        match (&setting, &git_root) {
            (CodebaseIndexingSetting::Enabled(false), _) => {
                tracing::info!(
                    cwd = %cwd.display(),
                    skip_reason = "disabled_by_config",
                    "code-nav: skipping index creation (disabled in config)"
                );
                return None;
            }
            (CodebaseIndexingSetting::Enabled(true), None) => {
                tracing::info!(
                    cwd = %cwd.display(),
                    skip_reason = "not_git_repo",
                    "code-nav: skipping index creation (not inside a git repo)"
                );
                return None;
            }
            (CodebaseIndexingSetting::Patterns(_), _) => {
                let check_path = git_root.as_deref().unwrap_or(cwd);
                if !setting.should_index(check_path) {
                    tracing::info!(
                        cwd = %cwd.display(),
                        skip_reason = "disabled_by_config",
                        "code-nav: skipping index creation (not matched by config globs)"
                    );
                    return None;
                }
            }
            (CodebaseIndexingSetting::Enabled(true), Some(_)) => {}
        }

        let target = git_root.unwrap_or_else(|| cwd.to_path_buf());
        // get_or_create returns the authoritative (handle, was_newly_started) pair.
        // Log only on actual first spawn so reuse requests are not misleadingly
        // labelled as "starting".
        let (handle, was_newly_started) = self.get_or_create_codebase_index(target.clone());
        if was_newly_started {
            tracing::info!(
                cwd = %cwd.display(),
                index_target = %target.display(),
                event = "index_first_spawn",
                "code-nav: first lazy spawn of codebase index"
            );
        }
        Some((handle, was_newly_started))
    }

    pub(super) fn indexed_roots_for(&self, cwd: &std::path::Path) -> Vec<String> {
        if self.get_codebase_index(cwd).is_some() {
            return vec![cwd.to_string_lossy().into_owned()];
        }
        if let Ok(git_root) = echo_agent_workspace::session::git::find_git_root_from_path(cwd)
            && self.get_codebase_index(&git_root).is_some()
        {
            return vec![git_root.to_string_lossy().into_owned()];
        }
        Vec::new()
    }

    /// Returns `(handle, was_newly_started)` — the bool is the authoritative
    /// "did this call spawn a new actor?" bit from `CodebaseIndexManager::get_or_create`.
    #[cfg(not(feature = "desktop"))]
    pub(super) fn get_or_create_codebase_index(
        &self,
        cwd: PathBuf,
    ) -> (std::sync::Arc<echo_agent_codebase_graph::IndexManagerHandle>, bool) {
        self.codebase_indexes.lock().get_or_create(cwd)
    }

    /// Get an existing codebase index for the given cwd.
    /// Returns None if no index exists for this cwd.
    pub(crate) fn get_codebase_index(
        &self,
        cwd: &std::path::Path,
    ) -> Option<std::sync::Arc<echo_agent_codebase_graph::IndexManagerHandle>> {
        self.codebase_indexes.lock().get(cwd)
    }
}
