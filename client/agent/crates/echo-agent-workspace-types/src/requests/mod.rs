//! Wire-format request enums.
//!
//! `WorkspaceRequest` is the outer envelope (matched on at the
//! transport-layer dispatch). The three inner enums (`ToolRequest`,
//! `WorkspaceOpsRequest`, `SessionLifecycleRequest`) are the actual
//! per-domain RPC payloads.
//!
//! Each request enum enumerates its full set of per-domain variants.

pub mod ops;
pub mod session;
pub mod tool;

use serde::{Deserialize, Serialize};

pub use ops::WorkspaceOpsRequest;
pub use session::SessionLifecycleRequest;
pub use tool::{ToolCallArgs, ToolRequest};

/// Outer-envelope wire request.
///
/// Each variant maps to one of the four streaming gRPC RPCs (`Tool`,
/// `Ops`, `Session`, plus `Events` which is a separate subscription
/// type and does not appear here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WorkspaceRequest {
    /// Tool RPCs.
    Tool(ToolRequest),
    /// Workspace-ops RPCs.
    Ops(WorkspaceOpsRequest),
    /// Session-lifecycle RPCs.
    Session(SessionLifecycleRequest),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::SessionId;

    #[test]
    fn round_trips_each_variant() {
        let samples = vec![
            WorkspaceRequest::Tool(ToolRequest::Definitions),
            WorkspaceRequest::Ops(WorkspaceOpsRequest::ListHunks),
            WorkspaceRequest::Session(SessionLifecycleRequest::Destroy(SessionId::new("s1"))),
        ];
        for req in samples {
            let json = serde_json::to_string(&req).unwrap();
            let back: WorkspaceRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(req, back);
        }
    }
}
