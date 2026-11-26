//! Session file-state / rewind methods (`workspace.begin_prompt`,
//! `workspace.end_prompt`, `workspace.rewind_to`).

use serde::{Deserialize, Serialize};

use super::{RpcActivityClass, WorkspaceRpc};

/// Begin tracking file state for a prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeginPromptReq {
    pub session_id: String,
    pub prompt_index: usize,
}

impl WorkspaceRpc for BeginPromptReq {
    const METHOD: &'static str = "workspace.begin_prompt";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = ();
}

/// End tracking file state for a prompt (captures after-snapshots).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndPromptReq {
    pub session_id: String,
    pub prompt_index: usize,
}

impl WorkspaceRpc for EndPromptReq {
    const METHOD: &'static str = "workspace.end_prompt";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Read;
    type Response = ();
}

/// Rewind files to the state before a given prompt index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewindToReq {
    pub session_id: String,
    pub target_prompt_index: usize,
}

impl WorkspaceRpc for RewindToReq {
    const METHOD: &'static str = "workspace.rewind_to";
    const ACTIVITY: RpcActivityClass = RpcActivityClass::Mutation;
    type Response = FileRewindResponse;
}

/// Type of external modification detected during file rewind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictType {
    DeletedExternally,
    CreatedExternally,
    ModifiedExternally,
}

/// A single conflict detected during file rewind.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRewindConflict {
    pub path: String,
    pub conflict_type: ConflictType,
}

/// Response returned by `workspace.rewind_to` RPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRewindResponse {
    pub success: bool,
    pub target_prompt_index: usize,
    pub reverted_files: Vec<String>,
    pub clean_files: Vec<String>,
    pub conflicts: Vec<FileRewindConflict>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_constants() {
        assert_eq!(BeginPromptReq::METHOD, "workspace.begin_prompt");
        assert_eq!(EndPromptReq::METHOD, "workspace.end_prompt");
        assert_eq!(RewindToReq::METHOD, "workspace.rewind_to");
    }

    #[test]
    fn conflict_type_snake_case_wire_values() {
        assert_eq!(
            serde_json::to_value(ConflictType::DeletedExternally).unwrap(),
            serde_json::json!("deleted_externally")
        );
    }
}
