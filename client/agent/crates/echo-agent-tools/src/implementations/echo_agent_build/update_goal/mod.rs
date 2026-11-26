//! `update_goal` — model-driven goal progress reporting.
//!
//! Each invocation is paired with an `oneshot::Sender<UpdateGoalAck>`
//! over the channel to `SessionActor`; the tool blocks on that ack so
//! the model's tool reply reflects the real outcome (classifier
//! verdict / transition / rejection), not a misleading instant success.

use crate::types::requirements::{Expr, ToolRequirement};
use crate::types::tool::{ToolKind, ToolNamespace};

pub use echo_agent_tools_api::slash_commands::UPDATE_GOAL_TOOL_NAME;

// ---------------------------------------------------------------------------
// Input schema
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateGoalInput {
    #[serde(
        default,
        deserialize_with = "crate::types::schema::deserialize_lenient_option_bool"
    )]
    #[schemars(
        description = "Set to true ONLY when the goal is fully achieved. This ends goal mode. Use together with `message` to include a completion summary."
    )]
    pub completed: Option<bool>,

    #[serde(default)]
    #[schemars(
        description = "Optional short message logged as progress (visible in tool response, not surfaced to the pager dashboard). Use with `completed: true` for a completion summary."
    )]
    pub message: Option<String>,

    #[serde(default)]
    #[schemars(
        description = "Set only when truly stuck after 3+ consecutive failed attempts at the same problem. If set, the goal is paused as blocked. This is a FAILURE signal — never put success text here. For success, use `completed: true` with `message`."
    )]
    pub blocked_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Channel types — inserted into Resources, read by SessionActor
// ---------------------------------------------------------------------------

/// Outcome of an `update_goal` call as delivered by the session actor.
#[derive(Debug)]
pub enum UpdateGoalAck {
    /// `message`-only or `blocked_reason` update accepted.
    Accepted { summary: String },
    /// Classifier judged the goal achieved.
    ClassifierAchieved { details_path: String },
    /// Classifier could not produce a verdict (infra failure); the
    /// harness fails open and treats the goal as achieved.
    ClassifierFailOpenAchieved { reason: &'static str },
    /// Classifier rejected the completion; `attempt < max_runs` so
    /// another attempt is still available.
    ClassifierNotAchieved {
        details_path: String,
        attempt: u32,
        max_runs: u32,
    },
    /// Classifier rejected the completion AND the per-goal cap was
    /// reached; the goal has been auto-paused with `BackOff`.
    ClassifierCapReached { details_path: String, attempt: u32 },
    /// Classifier rejected the completion with the same flagged gaps as
    /// the prior attempt (no progress); the goal auto-paused early
    /// before the cap.
    ClassifierStalled { details_path: String, attempt: u32 },
    /// Verification found no model-fixable path (every refuter flagged a
    /// contradiction or environment-unverifiable blocker); the goal
    /// paused for a user decision.
    ClassifierBlocked { details_path: String },
    /// Classifier disabled by policy; goal marked complete directly.
    CompletedWithoutClassifier,
    /// Second `update_goal(completed: true)` arrived while a
    /// classifier was already verifying the previous attempt; routed
    /// through the synthetic-NotAchieved accounting.
    ClassifierConcurrentInFlight {
        details_path: String,
        attempt: u32,
        max_runs: u32,
    },
    /// Mid-turn `completed: true` was queued for classifier
    /// verification at turn-end. The verdict arrives as a system
    /// reminder in the next user turn; the model must NOT call
    /// `update_goal(completed: true)` again until then.
    ///
    /// Invariant: the ack is resolved IMMEDIATELY at defer time, NOT
    /// parked — parking deadlocks the single-task actor.
    DeferredToTurnEnd { pending_depth: u32 },
    /// Update was rejected; `reason` discriminates the cause and
    /// drives the tool-error code, `detail` is the model-facing
    /// message.
    Rejected {
        reason: RejectReason,
        detail: String,
    },
}

/// Structured cause for `UpdateGoalAck::Rejected`; each variant maps
/// to a stable `error_code()` the model sees as the `ToolError` kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// A prior cmd in this drain transitioned the goal to Blocked.
    BlockSeenInDrain,
    /// `blocked_reason` set but the goal was not Active.
    BlockedAgainstNonActive,
    /// `completed: true` arrived after the classifier cap auto-paused
    /// the goal — model must wait for user resume.
    PostCap,
    /// `completed: true` against a non-Active goal for reasons OTHER
    /// than the classifier cap.
    NonActive,
    /// The goal harness is not enabled for this session (no `/goal`
    /// run in progress), so there is no orchestration to update. The
    /// `update_goal` tool and its `GoalUpdateHandle` are always
    /// exposed, so a model can call the tool outside goal mode; the
    /// drain rejects cleanly with this reason instead of dropping the
    /// ack oneshot (which would surface as the misleading
    /// `harness_no_ack` "dropped the response channel" error).
    HarnessDisabled,
    /// Reserved for strict-mode eviction surfacing; not currently
    /// constructed (the new design acks evicted entries as
    /// `DeferredToTurnEnd` at their own defer time).
    PendingQueueEvicted,
    /// The goal auto-paused mid-drain (cap, stall/no_progress, or
    /// blocked); this strictly-later entry was dropped without
    /// re-verification.
    DroppedAfterPauseInDrain,
    /// `GoalOrchestration` snapshot vanished between guard and reserve.
    OrchestrationVanished,
    /// Goal transitioned out of Active while the classifier awaited
    /// a verdict (user paused mid-fire).
    StatusChangedDuringClassifier,
    /// In-flight short-circuit but the orchestration snapshot
    /// vanished mid-flight.
    InFlightOrchestrationVanished,
}

impl RejectReason {
    /// Stable error code surfaced as the tool's `ToolError.kind`.
    pub fn error_code(self) -> &'static str {
        match self {
            Self::BlockSeenInDrain => "goal_update_block_seen",
            Self::BlockedAgainstNonActive => "goal_update_blocked_against_non_active",
            Self::PostCap => "goal_update_post_cap",
            Self::NonActive => "goal_update_non_active",
            Self::HarnessDisabled => "goal_update_harness_disabled",
            Self::PendingQueueEvicted => "goal_update_evicted",
            Self::DroppedAfterPauseInDrain => "goal_update_dropped_after_pause",
            Self::OrchestrationVanished => "goal_update_no_orchestration",
            Self::StatusChangedDuringClassifier => "goal_update_status_changed",
            Self::InFlightOrchestrationVanished => "goal_update_in_flight_orchestration_vanished",
        }
    }
}

/// Item posted across the goal-update channel: the model's input
/// paired with the oneshot the tool will await for its reply.
pub type UpdateGoalEnvelope = (UpdateGoalInput, tokio::sync::oneshot::Sender<UpdateGoalAck>);

/// Wrap an `UpdateGoalInput` in an envelope whose ack receiver is
/// discarded. Test-only helper; `pub` is needed for cross-crate test
/// access from `echo-agent-runtime`.
#[doc(hidden)]
pub fn envelope_for_test(input: UpdateGoalInput) -> UpdateGoalEnvelope {
    let (ack_tx, _ack_rx) = tokio::sync::oneshot::channel();
    (input, ack_tx)
}

/// Handle for the `update_goal` tool to send commands to the session.
/// Inserted into Resources as an ephemeral (non-serialized) resource.
pub struct GoalUpdateHandle(pub tokio::sync::mpsc::UnboundedSender<UpdateGoalEnvelope>);

impl std::fmt::Debug for GoalUpdateHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoalUpdateHandle").finish()
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct UpdateGoalOutput {
    pub success: bool,
    pub summary: String,
}

impl echo_agent_tool_runtime::ToolOutput for UpdateGoalOutput {}

// ---------------------------------------------------------------------------
// Tool implementation
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct UpdateGoalTool;

impl crate::types::tool_metadata::ToolMetadata for UpdateGoalTool {
    fn kind(&self) -> ToolKind {
        ToolKind::GoalUpdate
    }

    fn tool_namespace(&self) -> ToolNamespace {
        ToolNamespace::EchoAgentBuild
    }

    fn description_template(&self) -> &str {
        "Report progress on the active goal. Use the parameters to log a status message, mark the goal completed, or flag that you're blocked."
    }

    fn requires_expr(&self) -> Expr<ToolRequirement> {
        Expr::True
    }
}

impl echo_agent_tool_runtime::Tool for UpdateGoalTool {
    type Args = UpdateGoalInput;
    type Output = UpdateGoalOutput;

    fn id(&self) -> echo_agent_tool_protocol::ToolId {
        echo_agent_tool_protocol::ToolId::new(UPDATE_GOAL_TOOL_NAME).expect("valid tool id")
    }

    fn description(
        &self,
        _ctx: &::echo_agent_tool_runtime::ListToolsContext,
    ) -> echo_agent_tool_types::ToolDescription {
        echo_agent_tool_types::ToolDescription::new(
            UPDATE_GOAL_TOOL_NAME,
            crate::types::tool_metadata::ToolMetadata::sanitized_description_template(self),
        )
    }

    fn capabilities(&self) -> echo_agent_tool_protocol::ToolCapabilities {
        echo_agent_tool_protocol::ToolCapabilities {
            is_read_only: false,
            tool_scope: Some(echo_agent_tool_protocol::ToolScope::Read),
            ..Default::default()
        }
    }

    #[tracing::instrument(name = "new_tool.update_goal", skip_all)]
    async fn run(
        &self,
        ctx: echo_agent_tool_runtime::ToolCallContext,
        input: UpdateGoalInput,
    ) -> Result<UpdateGoalOutput, echo_agent_tool_runtime::ToolError> {
        use crate::types::tool_metadata::shared_resources;
        let resources = shared_resources(&ctx)?;

        // Fallback summary used only if the actor drops the ack
        // oneshot without responding.
        let fallback_summary = build_summary(&input);

        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel::<UpdateGoalAck>();

        // Clone the sender out of resources so we don't hold the
        // resources mutex across the channel write.
        let sender = {
            let res = resources.lock().await;
            res.get::<GoalUpdateHandle>()
                .ok_or_else(|| {
                    echo_agent_tool_runtime::ToolError::custom(
                        "goal_not_active",
                        "No active goal to update (GoalUpdateHandle not registered)",
                    )
                })?
                .0
                .clone()
        };
        sender.send((input, ack_tx)).map_err(|_| {
            echo_agent_tool_runtime::ToolError::custom(
                "goal_channel_closed",
                "Goal update channel closed — the session may be shutting down",
            )
        })?;

        // Block the tool reply on the actor's verdict-aware ack.
        // An `Err` here means the actor dropped the sender without
        // responding (harness bug) — surface a loud tool error
        // instead of a misleading `success: true`.
        let ack = match ack_rx.await {
            Ok(ack) => ack,
            Err(_) => {
                tracing::warn!(
                    "update_goal: actor dropped ack oneshot without responding — surfacing as \
                     tool error"
                );
                return Err(echo_agent_tool_runtime::ToolError::custom(
                    "harness_no_ack",
                    format!(
                        "Goal-update harness dropped the response channel before producing an \
                         ack. This is a harness-side bug; the goal may not have been updated. \
                         Original intent: {fallback_summary}",
                    ),
                ));
            }
        };
        render_ack_into_output(ack)
    }
}

/// Map an [`UpdateGoalAck`] to the model-facing tool result. Public
/// so host session tests can assert the same model-facing strings.
pub fn render_ack_into_output(
    ack: UpdateGoalAck,
) -> Result<UpdateGoalOutput, echo_agent_tool_runtime::ToolError> {
    match ack {
        UpdateGoalAck::Accepted { summary } => Ok(UpdateGoalOutput {
            success: true,
            summary,
        }),
        UpdateGoalAck::CompletedWithoutClassifier => Ok(UpdateGoalOutput {
            success: true,
            summary: "Goal marked complete.".to_string(),
        }),
        UpdateGoalAck::ClassifierAchieved { details_path } => Ok(UpdateGoalOutput {
            success: true,
            summary: format!(
                "Goal classifier verdict: Achieved. Goal complete. See {details_path}"
            ),
        }),
        UpdateGoalAck::ClassifierFailOpenAchieved { reason } => Ok(UpdateGoalOutput {
            success: true,
            summary: format!(
                "Goal marked complete via fail-open (reason: {reason}). No classifier verdict \
                 was produced."
            ),
        }),
        UpdateGoalAck::ClassifierNotAchieved {
            details_path,
            attempt,
            max_runs,
        } => Err(echo_agent_tool_runtime::ToolError::custom(
            "goal_classifier_not_achieved",
            format!(
                "Goal classifier rejected this completion attempt ({attempt}/{max_runs}). \
                 Review {details_path} and continue working; another attempt is available."
            ),
        )),
        UpdateGoalAck::ClassifierCapReached {
            details_path,
            attempt,
        } => {
            // Empty path ⇒ the harness wrote no synthetic details (e.g. a
            // squatted scratch root); omit the "See …" pointer entirely.
            let pointer = if details_path.trim().is_empty() {
                String::new()
            } else {
                format!(" See {details_path}")
            };
            Err(echo_agent_tool_runtime::ToolError::custom(
                "goal_classifier_cap_reached",
                format!(
                    "Goal classifier rejected completion {attempt} times — goal auto-paused.{pointer}"
                ),
            ))
        }
        UpdateGoalAck::ClassifierStalled {
            details_path,
            attempt,
        } => Err(echo_agent_tool_runtime::ToolError::custom(
            "goal_classifier_stalled",
            format!(
                "Goal verification saw no change in the flagged gaps across {attempt} attempts \
                 — goal auto-paused. Review {details_path}; the user must resume."
            ),
        )),
        UpdateGoalAck::ClassifierBlocked { details_path } => {
            Err(echo_agent_tool_runtime::ToolError::custom(
                "goal_classifier_blocked",
                format!(
                    "Goal verification found no model-fixable path (objective/plan contradiction or \
                 evidence that cannot be captured here) — goal paused for your decision. \
                 See {details_path}"
                ),
            ))
        }
        UpdateGoalAck::ClassifierConcurrentInFlight {
            details_path,
            attempt,
            max_runs,
        } => {
            // Empty path ⇒ no harness-written details to point at; omit the
            // "; see …" pointer (never reference content we didn't write).
            let pointer = if details_path.trim().is_empty() {
                String::new()
            } else {
                format!("; see {details_path}")
            };
            Err(echo_agent_tool_runtime::ToolError::custom(
                "goal_classifier_in_flight",
                format!(
                    "Goal classifier is still verifying a previous completion — do NOT call \
                     update_goal(completed: true) again until you receive a verdict reminder. \
                     This attempt was recorded as Not Achieved ({attempt}/{max_runs}){pointer}"
                ),
            ))
        }
        UpdateGoalAck::DeferredToTurnEnd { pending_depth } => Ok(UpdateGoalOutput {
            success: true,
            summary: format!(
                "Goal completion queued for classifier verification at end of turn \
                 (pending_depth={pending_depth}). The verdict will be delivered as a \
                 system reminder before your next reply; do NOT call update_goal \
                 again until you see it."
            ),
        }),
        UpdateGoalAck::Rejected { reason, detail } => Err(
            echo_agent_tool_runtime::ToolError::custom(reason.error_code(), detail),
        ),
    }
}

fn build_summary(input: &UpdateGoalInput) -> String {
    let mut parts = Vec::new();
    if input.completed == Some(true) {
        parts.push("Goal marked complete".to_string());
    }
    if let Some(ref reason) = input.blocked_reason {
        parts.push(format!("Goal blocked: {reason}"));
    }
    if let Some(ref msg) = input.message {
        parts.push(msg.clone());
    }
    if parts.is_empty() {
        "Goal updated.".to_string()
    } else {
        parts.join(". ") + "."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_input() -> UpdateGoalInput {
        UpdateGoalInput {
            completed: None,
            message: None,
            blocked_reason: None,
        }
    }

    #[test]
    fn build_summary_empty_input() {
        assert_eq!(build_summary(&empty_input()), "Goal updated.");
    }

    #[test]
    fn build_summary_completed() {
        let input = UpdateGoalInput {
            completed: Some(true),
            ..empty_input()
        };
        assert_eq!(build_summary(&input), "Goal marked complete.");
    }

    #[test]
    fn build_summary_message_only() {
        let input = UpdateGoalInput {
            message: Some("Working on it".into()),
            ..empty_input()
        };
        assert_eq!(build_summary(&input), "Working on it.");
    }

    #[test]
    fn build_summary_blocked_reason_only() {
        let input = UpdateGoalInput {
            blocked_reason: Some("no windows sdk".into()),
            ..empty_input()
        };
        assert_eq!(build_summary(&input), "Goal blocked: no windows sdk.");
    }

    #[test]
    fn build_summary_blocked_reason_with_message() {
        let input = UpdateGoalInput {
            blocked_reason: Some("X".into()),
            message: Some("longer body".into()),
            ..empty_input()
        };
        let summary = build_summary(&input);
        assert!(summary.contains("Goal blocked: X"));
        assert!(summary.contains("longer body"));
    }

    #[test]
    fn build_summary_completed_with_message() {
        let input = UpdateGoalInput {
            completed: Some(true),
            message: Some("All done".into()),
            ..empty_input()
        };
        let summary = build_summary(&input);
        assert!(summary.contains("Goal marked complete"));
        assert!(summary.contains("All done"));
    }

    #[test]
    fn build_summary_completed_false_treated_as_noop() {
        let input = UpdateGoalInput {
            completed: Some(false),
            ..empty_input()
        };
        assert_eq!(build_summary(&input), "Goal updated.");
    }
}
