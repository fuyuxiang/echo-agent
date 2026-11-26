//! Provisioned-repo listing (`workspace.repos_list`) and the on-disk
//! in-sandbox manifest contract (`{workspace}/.echo-agent/repos.json`).
//!
//! The sandbox provisioner writes this manifest; the workspace list op
//! reads it. Field names are the frontend/integration API — add optional
//! fields with `#[serde(default)]` rather than renaming existing ones.

use serde::{Deserialize, Serialize};

use super::{RpcActivityClass, WorkspaceRpc};

/// Relative path of the provisioner manifest from the **sandbox**
/// `workspace_directory` (pre-grove-rewrite init root, usually `/workspace`).
/// Not relative to agent / workspace-server `--cwd` after a single-repo grove
/// rewrite (`/workspace/app`). Writers and `workspace.repos_list` must join
/// this to that sandbox root.
pub const REPOS_MANIFEST_RELATIVE_PATH: &str = ".echo-agent/repos.json";

/// Current on-disk / wire manifest version.
pub const REPOS_MANIFEST_VERSION: u32 = 1;

/// `workspace.repos_list` — list repos materialized into this workspace.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReposListReq {}

impl WorkspaceRpc for ReposListReq {
    const METHOD: &'static str = "workspace.repos_list";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = ReposListResponse;
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReposListResponse {
    /// On-disk manifest version (`REPOS_MANIFEST_VERSION` for a missing file).
    #[serde(default)]
    pub version: u32,
    pub repos: Vec<ProvisionedRepo>,
}

/// One provisioned repository as exposed to frontend / workspace callers.
///
/// Expandable: new optional fields should use `#[serde(default, skip_serializing_if = "Option::is_none")]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvisionedRepo {
    /// Short directory / display name (usually the git repo name).
    pub name: String,
    /// Repository identity (`owner/repo` or normalized URL form).
    pub repository: String,
    /// Absolute in-sandbox (or workspace-relative absolute) mount path.
    pub mount_path: String,
    /// Fork-from ref. Empty = unset (missing session branch is fatal).
    /// `"HEAD"` = remote default. Do not treat empty as HEAD.
    pub base_branch: String,
    /// Session working branch created at provision time.
    pub session_branch: String,
}

/// On-disk manifest written by the sandbox provisioner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoManifest {
    pub version: u32,
    pub repos: Vec<ProvisionedRepo>,
}

impl RepoManifest {
    pub fn new(repos: Vec<ProvisionedRepo>) -> Self {
        Self {
            version: REPOS_MANIFEST_VERSION,
            repos,
        }
    }

    /// Parse bytes from `{workspace}/.echo-agent/repos.json`.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    /// Every distinct provisioned mount, or `[workspace_root]` when the
    /// manifest is empty (single-tree / no repos.json). Prompt, graph, and
    /// fs-notify walk this list so multi-repo workspaces are not primary-only.
    pub fn materialized_mounts(&self, workspace_root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out: Vec<std::path::PathBuf> = Vec::new();
        for repo in &self.repos {
            let raw = repo.mount_path.trim();
            if raw.is_empty() {
                continue;
            }
            let mount = std::path::PathBuf::from(raw);
            // Confine to the workspace: a compromised/malicious `.echo-agent/repos.json`
            // must not point prompt/graph/fs-notify walks at paths outside the
            // sandbox workspace. Reject `..` traversal and any mount that is not
            // under `workspace_root` (mirrors the confinement `unnamed_cwd` /
            // `confine_mount_under_workspace` already apply on the other paths).
            if mount
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
                || !mount.starts_with(workspace_root)
            {
                continue;
            }
            if !out
                .iter()
                .any(|existing| existing.components().eq(mount.components()))
            {
                out.push(mount);
            }
        }
        if out.is_empty() {
            out.push(workspace_root.to_path_buf());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_constant() {
        assert_eq!(ReposListReq::METHOD, "workspace.repos_list");
    }

    #[test]
    fn manifest_round_trip() {
        let manifest = RepoManifest::new(vec![
            ProvisionedRepo {
                name: "app".into(),
                repository: "acme/app".into(),
                mount_path: "/workspace/app".into(),
                base_branch: "main".into(),
                session_branch: "echoagent/s1".into(),
            },
            ProvisionedRepo {
                name: "lib".into(),
                repository: "acme/lib".into(),
                mount_path: "/workspace/lib".into(),
                base_branch: "HEAD".into(),
                session_branch: "feat/x".into(),
            },
        ]);
        let bytes = manifest.to_json_bytes().expect("serialize");
        let recovered = RepoManifest::from_json_bytes(&bytes).expect("parse");
        assert_eq!(manifest, recovered);
    }

    #[test]
    fn materialized_mounts_empty_falls_back_to_workspace_root() {
        let mounts =
            RepoManifest::new(Vec::new()).materialized_mounts(std::path::Path::new("/workspace"));
        assert_eq!(mounts, vec![std::path::PathBuf::from("/workspace")]);
    }

    #[test]
    fn materialized_mounts_lists_every_distinct_repo() {
        let mounts =
            nested_two_repo_manifest().materialized_mounts(std::path::Path::new("/workspace"));
        assert_eq!(
            mounts,
            vec![
                std::path::PathBuf::from("/workspace/app"),
                std::path::PathBuf::from("/workspace/lib"),
            ]
        );
    }

    #[test]
    fn materialized_mounts_rejects_out_of_workspace_and_traversal() {
        // A compromised repos.json must not escape the workspace; unsafe mounts
        // are dropped and the safe workspace-root fallback is used.
        let manifest = RepoManifest::new(vec![
            ProvisionedRepo {
                name: "evil".into(),
                repository: "acme/evil".into(),
                mount_path: "/etc".into(),
                base_branch: "main".into(),
                session_branch: "conv/1".into(),
            },
            ProvisionedRepo {
                name: "traverse".into(),
                repository: "acme/traverse".into(),
                mount_path: "/workspace/../etc".into(),
                base_branch: "main".into(),
                session_branch: "conv/1".into(),
            },
        ]);
        let mounts = manifest.materialized_mounts(std::path::Path::new("/workspace"));
        assert_eq!(mounts, vec![std::path::PathBuf::from("/workspace")]);
    }

    #[test]
    fn materialized_mounts_keeps_safe_and_drops_unsafe() {
        let manifest = RepoManifest::new(vec![
            ProvisionedRepo {
                name: "app".into(),
                repository: "acme/app".into(),
                mount_path: "/workspace/app".into(),
                base_branch: "main".into(),
                session_branch: "conv/1".into(),
            },
            ProvisionedRepo {
                name: "evil".into(),
                repository: "acme/evil".into(),
                mount_path: "/tmp/evil".into(),
                base_branch: "main".into(),
                session_branch: "conv/1".into(),
            },
        ]);
        let mounts = manifest.materialized_mounts(std::path::Path::new("/workspace"));
        assert_eq!(mounts, vec![std::path::PathBuf::from("/workspace/app")]);
    }

    fn nested_two_repo_manifest() -> RepoManifest {
        RepoManifest::new(vec![
            ProvisionedRepo {
                name: "app".into(),
                repository: "acme/app".into(),
                mount_path: "/workspace/app".into(),
                base_branch: "main".into(),
                session_branch: "conv/1".into(),
            },
            ProvisionedRepo {
                name: "lib".into(),
                repository: "acme/lib".into(),
                mount_path: "/workspace/lib".into(),
                base_branch: "main".into(),
                session_branch: "feat/x".into(),
            },
        ])
    }
}
