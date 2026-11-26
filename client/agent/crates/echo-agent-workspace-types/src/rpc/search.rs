//! Search methods (`workspace.ripgrep`, `workspace.fuzzy_*`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{RpcActivityClass, WorkspaceRpc};

// =========================================================================
// Content search (`workspace.ripgrep`)
// =========================================================================

fn default_respect_gitignore() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearchRequest {
    pub pattern: String,
    #[serde(default)]
    pub case_insensitive: bool,
    #[serde(default)]
    pub whole_word: bool,
    #[serde(default)]
    pub is_regex: bool,
    #[serde(default)]
    pub include_globs: Vec<String>,
    #[serde(default)]
    pub exclude_globs: Vec<String>,
    #[serde(default)]
    pub max_files: Option<usize>,
    #[serde(default)]
    pub max_matches: Option<usize>,
    #[serde(default = "default_respect_gitignore")]
    pub respect_gitignore: bool,
    /// Absolute search root (the per-session cwd), resolved by the shell.
    /// Falls back to the workspace root when absent.
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
    /// Session id used in the `echo.agent/search/content/status` payload.
    #[serde(default)]
    pub context_id: Option<String>,
}

impl WorkspaceRpc for ContentSearchRequest {
    const METHOD: &'static str = "workspace.ripgrep";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = ContentSearchData;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatch {
    pub line: usize,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_end: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMatchFile {
    pub name: String,
    pub path: String,
    pub matches: Vec<ContentMatch>,
}

impl ContentMatchFile {
    pub fn new(path: impl Into<String>) -> Self {
        let path = path.into();
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.clone());
        Self {
            name,
            path,
            matches: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentSearchData {
    pub files: Vec<ContentMatchFile>,
    pub total_matches: usize,
    pub total_files: usize,
    pub truncated: bool,
}

// =========================================================================
// Fuzzy file search (`workspace.fuzzy_*`)
// =========================================================================

/// Client ID structure for routing notifications across relay instances.
/// (Duplicated here for Phase 1 independence from shell extensions.)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientId {
    pub instance_id: String,
    pub conn_id: String,
}

/// Target client ID for routing notifications.
/// Used to specify which client should receive a notification.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TargetClientId {
    #[default]
    None,
    ClientId(ClientId),
}

impl TargetClientId {
    pub fn is_none(&self) -> bool {
        matches!(self, TargetClientId::None)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuzzyOpenReq {
    /// Absolute search root (the per-session cwd joined with any subpath),
    /// resolved by the shell. Falls back to the workspace root when absent.
    pub root: Option<std::path::PathBuf>,
    pub request_id: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    /// Session id stored for notification routing.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Target client (relay routing), stored for notification addressing.
    #[serde(default)]
    pub target_client_id: TargetClientId,
}

impl WorkspaceRpc for FuzzyOpenReq {
    const METHOD: &'static str = "workspace.fuzzy_open";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = String;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuzzyChangeReq {
    pub search_id: String,
    pub query: String,
    #[serde(default)]
    pub dirs_only: bool,
    /// Max matches per status notification (default 100).
    #[serde(default)]
    pub limit: Option<usize>,
}

// Response: Whether the search existed (so the shell can return "not found").
impl WorkspaceRpc for FuzzyChangeReq {
    const METHOD: &'static str = "workspace.fuzzy_change";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = bool;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuzzyCloseReq {
    pub search_id: String,
}

impl WorkspaceRpc for FuzzyCloseReq {
    const METHOD: &'static str = "workspace.fuzzy_close";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = bool;
}

/// `workspace.fuzzy_search` — poll the current results of an open fuzzy
/// search. The response is the serialized result set (or `null` when the
/// search no longer exists).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FuzzyStatusReq {
    pub search_id: String,
}

impl WorkspaceRpc for FuzzyStatusReq {
    const METHOD: &'static str = "workspace.fuzzy_search";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = Value;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_constants() {
        assert_eq!(ContentSearchRequest::METHOD, "workspace.ripgrep");
        assert_eq!(FuzzyOpenReq::METHOD, "workspace.fuzzy_open");
        assert_eq!(FuzzyChangeReq::METHOD, "workspace.fuzzy_change");
        assert_eq!(FuzzyCloseReq::METHOD, "workspace.fuzzy_close");
        assert_eq!(FuzzyStatusReq::METHOD, "workspace.fuzzy_search");
    }

    #[test]
    fn target_client_id_untagged_round_trip() {
        let none: TargetClientId = serde_json::from_value(Value::Null).unwrap();
        assert!(none.is_none());

        let raw = serde_json::json!({"instanceId": "i-1", "connId": "c-1"});
        let target: TargetClientId = serde_json::from_value(raw.clone()).unwrap();
        let TargetClientId::ClientId(id) = &target else {
            panic!("expected ClientId variant");
        };
        assert_eq!(id.instance_id, "i-1");
        assert_eq!(serde_json::to_value(&target).unwrap(), raw);
    }

    #[test]
    fn content_match_file_new_derives_name() {
        let f = ContentMatchFile::new("/repo/src/lib.rs");
        assert_eq!(f.name, "lib.rs");
        assert_eq!(f.path, "/repo/src/lib.rs");
        assert!(f.matches.is_empty());
    }

    #[test]
    fn content_search_request_defaults() {
        let req: ContentSearchRequest =
            serde_json::from_value(serde_json::json!({"pattern": "foo"})).unwrap();
        assert!(req.respect_gitignore, "default_respect_gitignore");
        assert!(!req.case_insensitive);
        assert!(req.cwd.is_none());
    }
}
