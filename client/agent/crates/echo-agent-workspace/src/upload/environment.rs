//! Workspace environment capture — `workspace_environment.json`.
//!
//! Captures the session's owner/host/sandbox context once at session bind
//! and serializes it to `{session_id}/workspace_environment.json` so the
//! session can be attributed. [`WorkspaceIdentity`] (who owns
//! the workspace, also used for upload 401-attribution) is resolved at
//! workspace construction; [`WorkspaceEnvironment`] is the full on-disk record.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Schema version stamped into every `workspace_environment.json`; bumped on
/// incompatible shape changes.
pub(crate) const SCHEMA_VERSION: &str = "v1";

/// `principal_type` wire value for a team-scoped principal.
pub(crate) const PRINCIPAL_TYPE_TEAM: &str = "Team";

/// Identity of the principal that owns a workspace.
///
/// `principal_type` is the OAuth wire string `"User"` or `"Team"`;
/// `principal_id` carries the team id when `principal_type == "Team"` and is
/// `None` for personal (`"User"`) principals. `user_id` is always the
/// individual user behind the bearer token.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceIdentity {
    /// Stable user identifier (owner of the bearer token).
    pub user_id: String,
    /// `"User"` or `"Team"`. `None` when the auth source does not distinguish
    /// principal kinds (e.g. a local-dev bearer token).
    pub principal_type: Option<String>,
    /// Team id when `principal_type == "Team"`; otherwise `None`.
    pub principal_id: Option<String>,
}

impl WorkspaceIdentity {
    /// Construct an identity from its parts.
    pub fn new(
        user_id: impl Into<String>,
        principal_type: Option<String>,
        principal_id: Option<String>,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            principal_type,
            principal_id,
        }
    }

    /// Construct a team-scoped identity (`principal_type == "Team"`, the team id
    /// in `principal_id`). Keeps the `PRINCIPAL_TYPE_TEAM` wire string in one
    /// place so callers (e.g. the in-process shell identity) don't duplicate the
    /// `"Team"` literal across crate boundaries.
    pub fn team(user_id: impl Into<String>, team_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            principal_type: Some(PRINCIPAL_TYPE_TEAM.to_string()),
            principal_id: Some(team_id.into()),
        }
    }

    /// Whether this identity is a team principal (`principal_type == "Team"`).
    pub(crate) fn is_team(&self) -> bool {
        self.principal_type.as_deref() == Some(PRINCIPAL_TYPE_TEAM)
    }

    /// The team id **iff** this is a team principal: `None` for `"User"`
    /// principals even if `principal_id` is populated (per the wire contract).
    /// Deriving `team_id` from `principal_id` is intentional — the standalone
    /// server's `AuthEntry` never carries the shell's separate
    /// `EchoAgentAuth.team_id`, and for a Team principal `principal_id` *is* the
    /// team id.
    pub(crate) fn team_id(&self) -> Option<String> {
        self.is_team().then(|| self.principal_id.clone()).flatten()
    }

    /// The user id as an `Option`, mapping the empty string (no resolved
    /// identity, e.g. headless/local-dev) to `None`.
    pub(crate) fn user_id_opt(&self) -> Option<String> {
        if self.user_id.is_empty() {
            None
        } else {
            Some(self.user_id.clone())
        }
    }
}

/// Derive the workspace owner identity from the server auth provider's
/// [`AuthIdentity`](echo_agent_computer_hub_sdk::AuthIdentity). The two types carry the
/// same principal fields; this is the single conversion point so the workspace
/// reads identity from `HubConfig.auth` instead of a separate auth.json read.
impl From<echo_agent_computer_hub_sdk::AuthIdentity> for WorkspaceIdentity {
    fn from(id: echo_agent_computer_hub_sdk::AuthIdentity) -> Self {
        Self::new(id.user_id, id.principal_type, id.principal_id)
    }
}

/// On-disk `workspace_environment.json` record.
///
/// Every field is always serialized (no `skip_serializing_if`) so the artifact
/// presents a stable, fully-populated schema to consumers; absent
/// values surface as JSON `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEnvironment {
    /// Schema version of this record (always [`SCHEMA_VERSION`]).
    pub schema_version: String,
    /// Session this environment was captured for.
    pub session_id: String,
    /// RFC3339 capture time (UTC).
    pub recorded_at: String,
    /// Version of the `echo-agent-workspace` crate that produced the record.
    pub workspace_version: String,
    /// Stable hub server identity (`--server-id`), when registered.
    pub server_id: Option<String>,
    /// Owner user id (empty-string identities are emitted as `null`).
    pub user_id: Option<String>,
    /// `"User"` / `"Team"` principal type, mirrored from the identity.
    pub principal_type: Option<String>,
    /// Team id when the principal is a team, mirrored from the identity.
    pub principal_id: Option<String>,
    /// Sandbox id that provisioned this workspace server (from server metadata).
    pub sandbox_id: Option<String>,
    /// Sandbox profile name (`$ECHO_AGENT_SANDBOX_PROFILE`), e.g. `"devbox"`.
    pub sandbox_profile: Option<String>,
    /// Whether the workspace is running inside a bubblewrap sandbox.
    pub inside_bwrap: bool,
    /// Host name (`$HOSTNAME`); `None` on hosts that don't export it (macOS dev).
    pub hostname: Option<String>,
    /// Working directory the session is rooted at.
    pub cwd: String,
    /// Host OS (`std::env::consts::OS`).
    pub host_os: String,
    /// Host architecture (`std::env::consts::ARCH`).
    pub host_arch: String,
    /// Git working-tree root for `cwd`, when inside a repository.
    pub repo_root: Option<String>,
    /// `origin` remote URL for the repository, when present.
    pub remote_url: Option<String>,
}

impl WorkspaceEnvironment {
    /// Capture the live environment for a session at bind time: host/sandbox
    /// facts from the process environment, git facts from `cwd`, identity and
    /// server facts from the caller.
    pub(crate) fn capture(
        session_id: &str,
        cwd: &Path,
        identity: &WorkspaceIdentity,
        server_id: Option<String>,
        sandbox_id: Option<String>,
    ) -> Self {
        let (repo_root, remote_url) = git_repo_facts(cwd);
        Self::assemble(
            session_id,
            cwd,
            identity,
            server_id,
            sandbox_id,
            std::env::var("ECHO_AGENT_SANDBOX_PROFILE").ok(),
            echo_agent_sandbox::is_inside_bwrap(),
            std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty()),
            repo_root,
            remote_url,
        )
    }

    /// Assemble the record from already-resolved facts. Separated from
    /// [`capture`](Self::capture) so unit tests can inject every field without
    /// mutating process-global env; only `recorded_at` (`Utc::now()`) is
    /// non-injected.
    fn assemble(
        session_id: &str,
        cwd: &Path,
        identity: &WorkspaceIdentity,
        server_id: Option<String>,
        sandbox_id: Option<String>,
        sandbox_profile: Option<String>,
        inside_bwrap: bool,
        hostname: Option<String>,
        repo_root: Option<String>,
        remote_url: Option<String>,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: session_id.to_string(),
            recorded_at: chrono::Utc::now().to_rfc3339(),
            workspace_version: env!("CARGO_PKG_VERSION").to_string(),
            server_id,
            user_id: identity.user_id_opt(),
            principal_type: identity.principal_type.clone(),
            principal_id: identity.principal_id.clone(),
            sandbox_id,
            sandbox_profile,
            inside_bwrap,
            hostname,
            cwd: cwd.to_string_lossy().into_owned(),
            host_os: std::env::consts::OS.to_string(),
            host_arch: std::env::consts::ARCH.to_string(),
            repo_root,
            remote_url,
        }
    }

    /// Serialize to pretty JSON bytes for enqueue.
    pub(crate) fn to_json_bytes(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec_pretty(self)
    }
}

/// Resolve `(repo_root, origin_remote_url)` for `cwd` via libgit2; both `None`
/// outside a git repository. The `origin` URL has embedded credentials
/// stripped before it is stored — HTTPS remotes can carry a token in the
/// userinfo, and this artifact is uploaded, so the raw URL must never
/// be persisted.
fn git_repo_facts(cwd: &Path) -> (Option<String>, Option<String>) {
    let Ok(repo) = git2::Repository::discover(cwd) else {
        return (None, None);
    };
    let repo_root = repo.workdir().map(|p| p.to_string_lossy().into_owned());
    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().ok().map(crate::session::git::strip_url_credentials));
    (repo_root, remote_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team_identity() -> WorkspaceIdentity {
        WorkspaceIdentity::new(
            "user-123",
            Some("Team".to_string()),
            Some("team-456".to_string()),
        )
    }

    fn user_identity() -> WorkspaceIdentity {
        WorkspaceIdentity::new("user-123", Some("User".to_string()), None)
    }

    #[test]
    fn team_identity_resolves_team_id() {
        let id = team_identity();
        assert!(id.is_team());
        assert_eq!(id.team_id().as_deref(), Some("team-456"));
        assert_eq!(id.user_id_opt().as_deref(), Some("user-123"));
    }

    #[test]
    fn user_identity_has_no_team_id() {
        let id = user_identity();
        assert!(!id.is_team());
        assert_eq!(id.team_id(), None);
        assert_eq!(id.user_id_opt().as_deref(), Some("user-123"));
    }

    #[test]
    fn team_constructor_sets_team_principal() {
        let id = WorkspaceIdentity::team("user-123", "team-456");
        assert!(id.is_team());
        assert_eq!(id.principal_type.as_deref(), Some(PRINCIPAL_TYPE_TEAM));
        assert_eq!(id.team_id().as_deref(), Some("team-456"));
        assert_eq!(id.user_id_opt().as_deref(), Some("user-123"));
    }

    #[test]
    fn team_id_ignores_principal_id_when_not_team() {
        // A stray principal_id on a non-Team identity must not leak as team_id.
        let id = WorkspaceIdentity::new("u", Some("User".to_string()), Some("t".to_string()));
        assert_eq!(id.team_id(), None);
    }

    #[test]
    fn empty_user_id_maps_to_none() {
        let id = WorkspaceIdentity::default();
        assert!(!id.is_team());
        assert_eq!(id.user_id_opt(), None);
    }

    #[test]
    fn assemble_populates_every_field() {
        let env = WorkspaceEnvironment::assemble(
            "sess-1",
            Path::new("/work/repo"),
            &team_identity(),
            Some("server-9".to_string()),
            Some("sb_abc123".to_string()),
            Some("devbox".to_string()),
            true,
            Some("host-1".to_string()),
            Some("/work/repo".to_string()),
            Some("git@github.com:example/echo-agent-repo.git".to_string()),
        );

        assert_eq!(env.schema_version, "v1");
        assert_eq!(env.session_id, "sess-1");
        assert_eq!(env.workspace_version, env!("CARGO_PKG_VERSION"));
        assert!(!env.workspace_version.is_empty());
        assert_eq!(env.server_id.as_deref(), Some("server-9"));
        assert_eq!(env.user_id.as_deref(), Some("user-123"));
        assert_eq!(env.principal_type.as_deref(), Some("Team"));
        assert_eq!(env.principal_id.as_deref(), Some("team-456"));
        assert_eq!(env.sandbox_id.as_deref(), Some("sb_abc123"));
        assert_eq!(env.sandbox_profile.as_deref(), Some("devbox"));
        assert!(env.inside_bwrap);
        assert_eq!(env.hostname.as_deref(), Some("host-1"));
        assert_eq!(env.cwd, "/work/repo");
        assert_eq!(env.host_os, std::env::consts::OS);
        assert_eq!(env.host_arch, std::env::consts::ARCH);
        assert_eq!(env.repo_root.as_deref(), Some("/work/repo"));
        assert_eq!(
            env.remote_url.as_deref(),
            Some("git@github.com:example/echo-agent-repo.git")
        );
        assert!(chrono::DateTime::parse_from_rfc3339(&env.recorded_at).is_ok());
    }

    #[test]
    fn user_principal_env_has_null_team_and_sandbox_fields() {
        let env = WorkspaceEnvironment::assemble(
            "sess-2",
            Path::new("/tmp/x"),
            &user_identity(),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        );
        assert_eq!(env.principal_type.as_deref(), Some("User"));
        assert_eq!(env.principal_id, None);
        assert_eq!(env.sandbox_id, None);
        assert!(!env.inside_bwrap);
    }

    #[test]
    fn serializes_with_all_fields_present() {
        let env = WorkspaceEnvironment::assemble(
            "sess-3",
            Path::new("/work/repo"),
            &team_identity(),
            Some("server-9".to_string()),
            Some("sb_abc123".to_string()),
            Some("devbox".to_string()),
            true,
            Some("host-1".to_string()),
            Some("/work/repo".to_string()),
            Some("https://github.com/example/echo-agent-repo".to_string()),
        );
        let bytes = env.to_json_bytes().expect("serialize");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("parse");

        for key in [
            "schema_version",
            "session_id",
            "recorded_at",
            "workspace_version",
            "server_id",
            "user_id",
            "principal_type",
            "principal_id",
            "sandbox_id",
            "sandbox_profile",
            "inside_bwrap",
            "hostname",
            "cwd",
            "host_os",
            "host_arch",
            "repo_root",
            "remote_url",
        ] {
            assert!(
                value.get(key).is_some(),
                "serialized environment is missing field `{key}`"
            );
        }

        assert_eq!(value["schema_version"], "v1");
        assert_eq!(value["principal_type"], "Team");
        assert_eq!(value["principal_id"], "team-456");
        assert_eq!(value["sandbox_id"], "sb_abc123");
        assert_eq!(value["sandbox_profile"], "devbox");
        assert_eq!(value["inside_bwrap"], true);

        let parsed: WorkspaceEnvironment = serde_json::from_slice(&bytes).expect("roundtrip");
        assert_eq!(parsed, env);
    }

    #[test]
    fn null_optionals_are_emitted_not_skipped() {
        let env = WorkspaceEnvironment::assemble(
            "sess-4",
            Path::new("/tmp/x"),
            &user_identity(),
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        );
        let value: serde_json::Value =
            serde_json::from_slice(&env.to_json_bytes().unwrap()).unwrap();
        // Absent optionals serialize as explicit JSON null (stable schema).
        assert!(value.get("sandbox_id").is_some());
        assert_eq!(value["sandbox_id"], serde_json::Value::Null);
        assert_eq!(value["principal_id"], serde_json::Value::Null);
        assert_eq!(value["repo_root"], serde_json::Value::Null);
    }

    /// A token embedded in an HTTPS `origin` must never survive into the
    /// captured `remote_url`.
    #[test]
    fn git_repo_facts_strips_remote_url_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(dir.path()).unwrap();
        repo.remote(
            "origin",
            "https://x-access-token:secret-token@github.com/example/echo-agent-repo.git",
        )
        .unwrap();
        drop(repo);

        let (repo_root, remote_url) = git_repo_facts(dir.path());
        assert!(
            repo_root.is_some(),
            "workdir should resolve for an init'd repo"
        );
        let url = remote_url.expect("origin remote url present");
        assert!(
            !url.contains("secret-token") && !url.contains("x-access-token"),
            "credentials must be stripped from remote_url, got {url}"
        );
        assert_eq!(url, "https://github.com/example/echo-agent-repo.git");
    }
}
