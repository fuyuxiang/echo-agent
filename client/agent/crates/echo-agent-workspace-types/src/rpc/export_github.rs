use serde::{Deserialize, Serialize};

use super::{RpcActivityClass, WorkspaceRpc};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportGithubReq {
    pub project_dir: String,
    #[serde(default)]
    pub repo_full_name: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub commit_message: Option<String>,
}

impl WorkspaceRpc for ExportGithubReq {
    const METHOD: &'static str = "workspace.export_github";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = ExportGithubResponse;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportGithubResponse {
    pub repo_full_name: String,
    pub repo_url: String,
    pub branch: String,
    pub commit_sha: String,
    pub no_changes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportGithubError {
    RepoNotSpecified,
    InvalidRepoName,
    ProjectDirInvalid,
    AuthFailed,
    PushRejected,
    Timeout,
    GitFailed,
}

impl ExportGithubError {
    pub const ALL: [ExportGithubError; 7] = [
        Self::RepoNotSpecified,
        Self::InvalidRepoName,
        Self::ProjectDirInvalid,
        Self::AuthFailed,
        Self::PushRejected,
        Self::Timeout,
        Self::GitFailed,
    ];

    pub fn wire_code(self) -> &'static str {
        match self {
            Self::RepoNotSpecified => "gh_export_repo_not_specified",
            Self::InvalidRepoName => "gh_export_invalid_repo_name",
            Self::ProjectDirInvalid => "gh_export_project_dir_invalid",
            Self::AuthFailed => "gh_export_auth_failed",
            Self::PushRejected => "gh_export_push_rejected",
            Self::Timeout => "gh_export_timeout",
            Self::GitFailed => "gh_export_git_failed",
        }
    }

    pub fn from_wire_code(code: &str) -> Option<Self> {
        Some(match code {
            "gh_export_repo_not_specified" => Self::RepoNotSpecified,
            "gh_export_invalid_repo_name" => Self::InvalidRepoName,
            "gh_export_project_dir_invalid" => Self::ProjectDirInvalid,
            "gh_export_auth_failed" => Self::AuthFailed,
            "gh_export_push_rejected" => Self::PushRejected,
            "gh_export_timeout" => Self::Timeout,
            "gh_export_git_failed" => Self::GitFailed,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ExportGithubError;

    #[test]
    fn export_error_wire_code_round_trips() {
        for kind in ExportGithubError::ALL {
            assert_eq!(
                ExportGithubError::from_wire_code(kind.wire_code()),
                Some(kind),
                "round-trip failed for {kind:?}"
            );
        }
    }

    #[test]
    fn export_error_rejects_unknown_code() {
        assert_eq!(ExportGithubError::from_wire_code("hub_error"), None);
        assert_eq!(ExportGithubError::from_wire_code(""), None);
    }
}
