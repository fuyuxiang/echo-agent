//! Tool-stream chunks ([`ToolChunk`]) and the paired sampler-to-workspace
//! response messages ([`ToolResponse`]).
//!
//! The two enums together form the bidirectional stream that carries a
//! tool invocation. The workspace yields `ToolChunk` values to the
//! sampler; the sampler yields `ToolResponse` values back to the
//! workspace whenever it sees a `Need*` chunk.

use serde::{Deserialize, Serialize};

use crate::chunks::ChunkKind;
use crate::types::interaction::{UserAnswer, UserQuestion};
use crate::types::permission::{PermissionDecision, PermissionRequest};
use crate::types::plan_mode::{PlanModeDecision, PlanModeTransition};
use crate::types::tools::{ToolCallResult, ToolDef, ToolOutputChunk, ToolProgress};

/// Streaming chunk for a tool call.
///
/// Stream contract:
/// - For tool invocations, the stream emits zero or more
///   `Output` / `Progress` chunks then **exactly one** `Final` chunk and
///   closes.
/// - A tool may additionally yield zero or more
///   [`NeedPermission`](Self::NeedPermission) /
///   [`NeedUserAnswer`](Self::NeedUserAnswer) /
///   [`NeedPlanModeChange`](Self::NeedPlanModeChange) chunks before its
///   `Final`. Each blocks the tool until the sampler replies with the
///   matching [`ToolResponse`] on the paired response sender
///   (correlated by `req_id`). The sampler is the unique consumer of
///   the stream, so exhaustive matching forces it to handle every
///   variant; this is how the "must respond" property is enforced at
///   the type level.
/// - For `ToolRequest::Definitions`, the stream emits exactly one
///   `Definitions` chunk and closes.
///
/// `Eq` is not derived because [`Progress`](Self::Progress) carries
/// `ToolProgress::Percent.fraction: f32`. `PartialEq` is sufficient
/// for round-trip and equality tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ToolChunk {
    /// Incremental tool output (e.g. bash stdout). Zero or more.
    Output(ToolOutputChunk),
    /// Tool-emitted progress / lifecycle event.
    Progress(ToolProgress),
    /// Terminal result. Exactly one, last. Stream closes after this.
    Final(ToolCallResult),
    /// Tool definitions response. Single chunk.
    Definitions(Vec<ToolDef>),
    /// Tool needs the sampler to make a permission decision before
    /// continuing. The sampler must reply with
    /// [`ToolResponse::Permission { req_id, decision }`](ToolResponse::Permission)
    /// on the bidi response sender.
    NeedPermission {
        /// Correlation id; the sampler must echo this back in the
        /// matching [`ToolResponse::Permission`].
        req_id: String,
        /// Permission request payload (forwarded to the user).
        request: PermissionRequest,
    },
    /// Tool needs the sampler to collect answers from the user. The
    /// sampler must reply with
    /// [`ToolResponse::UserAnswer { req_id, answers }`](ToolResponse::UserAnswer)
    /// on the bidi response sender.
    NeedUserAnswer {
        /// Correlation id; the sampler must echo this back in the
        /// matching [`ToolResponse::UserAnswer`].
        req_id: String,
        /// Questions to ask the user. The reply
        /// (`ToolResponse::UserAnswer { answers, .. }`) supplies one
        /// [`UserAnswer`] per [`UserQuestion`].
        questions: Vec<UserQuestion>,
    },
    /// Tool needs the sampler to approve a plan-mode transition
    /// (entering or exiting plan mode). The sampler must reply with
    /// [`ToolResponse::PlanModeChange { req_id, decision }`](ToolResponse::PlanModeChange)
    /// on the bidi response sender.
    ///
    /// Plan mode transitions are deliberately *not* broadcast on the
    /// EventBus: sampler-caused state
    /// flows back via the call's stream chunks (here) and the
    /// resulting `Final` payload, never via EventBus.
    NeedPlanModeChange {
        /// Correlation id; the sampler must echo this back in the
        /// matching [`ToolResponse::PlanModeChange`].
        req_id: String,
        /// Direction of the transition (enter / exit) plus optional
        /// plan content for the UI to preview.
        transition: PlanModeTransition,
    },
}

impl ToolChunk {
    /// Discriminator for the current variant.
    pub fn kind(&self) -> ChunkKind {
        match self {
            Self::Output(_) => ChunkKind::ToolOutput,
            Self::Progress(_) => ChunkKind::ToolProgress,
            Self::Final(_) => ChunkKind::ToolFinal,
            Self::Definitions(_) => ChunkKind::ToolDefinitions,
            Self::NeedPermission { .. } => ChunkKind::NeedPermission,
            Self::NeedUserAnswer { .. } => ChunkKind::NeedUserAnswer,
            Self::NeedPlanModeChange { .. } => ChunkKind::NeedPlanModeChange,
        }
    }
}

/// Sampler-to-workspace message sent on the tool's bidi response
/// sender to satisfy a [`ToolChunk::NeedPermission`],
/// [`ToolChunk::NeedUserAnswer`], or [`ToolChunk::NeedPlanModeChange`].
///
/// One `ToolResponse` per `Need*` chunk; correlated by `req_id`
/// (echoed back from the corresponding `Need*` chunk).
///
/// Tagged with `tag = "type", content = "data"` (adjacent tagging) to
/// match every other wire enum in the crate. See `crate::lib`
/// doc-comment "# Wire format" for the rationale.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ToolResponse {
    /// Reply to a [`ToolChunk::NeedPermission`].
    Permission {
        /// Correlation id echoed from the `NeedPermission` chunk.
        req_id: String,
        /// User's decision.
        decision: PermissionDecision,
    },
    /// Reply to a [`ToolChunk::NeedUserAnswer`].
    UserAnswer {
        /// Correlation id echoed from the `NeedUserAnswer` chunk.
        req_id: String,
        /// One answer per question in the original
        /// `NeedUserAnswer.questions`, in the same order.
        answers: Vec<UserAnswer>,
    },
    /// Reply to a [`ToolChunk::NeedPlanModeChange`].
    PlanModeChange {
        /// Correlation id echoed from the `NeedPlanModeChange` chunk.
        req_id: String,
        /// User's decision (approve / reject / defer).
        decision: PlanModeDecision,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ToolCallId;
    use crate::types::interaction::UserQuestionOption;
    use std::collections::HashSet;

    fn samples() -> Vec<ToolChunk> {
        vec![
            ToolChunk::Output(ToolOutputChunk {
                call_id: ToolCallId::new("c1"),
                stream: "stdout".into(),
                bytes: b"hello".to_vec(),
                at: chrono::Utc::now(),
            }),
            ToolChunk::Progress(ToolProgress::Started {
                call_id: ToolCallId::new("c1"),
            }),
            ToolChunk::Final(ToolCallResult {
                call_id: ToolCallId::new("c1"),
                exit_code: 0,
                summary: "ok".into(),
                output_json: "{}".into(),
                cancelled: false,
            }),
            ToolChunk::Definitions(vec![ToolDef {
                name: "read_file".into(),
                description: "Read a file from disk.".into(),
                input_schema_json: "{}".into(),
                requires_permission: false,
            }]),
            ToolChunk::NeedPermission {
                req_id: "test-perm-1".into(),
                request: PermissionRequest {
                    tool_name: "run_terminal_cmd".into(),
                    summary: "rm -rf /tmp/scratch".into(),
                    input_json: r#"{"cmd":"rm -rf /tmp/scratch"}"#.into(),
                    destructive: true,
                },
            },
            ToolChunk::NeedUserAnswer {
                req_id: "test-q-1".into(),
                questions: vec![UserQuestion {
                    question: "Pick a color?".into(),
                    options: vec![UserQuestionOption {
                        label: "Red".into(),
                        description: "warm".into(),
                        preview: None,
                    }],
                    multi_select: false,
                }],
            },
            ToolChunk::NeedPlanModeChange {
                req_id: "test-pm-1".into(),
                transition: PlanModeTransition::Enter {
                    plan: Some("step 1".into()),
                },
            },
        ]
    }

    fn response_samples() -> Vec<ToolResponse> {
        vec![
            ToolResponse::Permission {
                req_id: "test-perm-1".into(),
                decision: PermissionDecision::AllowOnce,
            },
            ToolResponse::Permission {
                req_id: "test-perm-2".into(),
                decision: PermissionDecision::Deny {
                    reason: "no".into(),
                },
            },
            ToolResponse::UserAnswer {
                req_id: "test-q-1".into(),
                answers: vec![UserAnswer::Selected("Red".into())],
            },
            ToolResponse::PlanModeChange {
                req_id: "test-pm-1".into(),
                decision: PlanModeDecision::Approve,
            },
            ToolResponse::PlanModeChange {
                req_id: "test-pm-2".into(),
                decision: PlanModeDecision::Reject {
                    feedback: Some("not yet".into()),
                },
            },
            ToolResponse::PlanModeChange {
                req_id: "test-pm-3".into(),
                decision: PlanModeDecision::Defer,
            },
        ]
    }

    #[test]
    fn kind_discriminators_are_unique() {
        let kinds: HashSet<ChunkKind> = samples().iter().map(ToolChunk::kind).collect();
        assert_eq!(kinds.len(), samples().len(), "duplicate ToolChunk kind()");
    }

    #[test]
    fn round_trips_through_json() {
        for chunk in samples() {
            let json = serde_json::to_string(&chunk).unwrap();
            let back: ToolChunk = serde_json::from_str(&json).unwrap();
            assert_eq!(chunk, back);
        }
    }

    #[test]
    fn tool_response_round_trips_through_json() {
        for resp in response_samples() {
            let json = serde_json::to_string(&resp).unwrap();
            let back: ToolResponse = serde_json::from_str(&json).unwrap();
            assert_eq!(resp, back);
        }
    }
}
