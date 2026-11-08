pub mod acp_types;
pub mod announcement_state;
pub mod commands;
pub(crate) mod compaction_config;
pub mod handle;
pub(crate) mod memory_state;
pub mod merge;
pub mod notifications;
pub mod pending_interaction;
pub mod prompt_queue;
pub mod two_pass;
pub use self::acp_session::*;
pub use self::acp_types::*;
pub use self::commands::*;
pub use self::fork::{ForkSessionRequest, ForkSessionResponse, fork_session};
pub use self::handle::*;
pub use self::persistence::{
    LocalFeedbackEntry, UserFeedbackEntry, find_local_child_for_remote, resolve_local_session,
    resolve_local_session_any_cwd, session_exists_for_cwd,
};
pub use self::result::{Empty, ExtMethodResult};
pub use self::share::{ShareSessionRequest, ShareSessionResponse};
pub use echo_agent_chat_proxy_types::feedback_types::{
    ClientType, FeedbackImage, FeedbackTerminalInfo, MAX_FEEDBACK_IMAGE_BYTES,
    MAX_FEEDBACK_IMAGE_TOTAL_BYTES, MAX_FEEDBACK_IMAGES, RatingType, feedback_image_extension,
    validate_feedback_images,
};
pub use echo_agent_fsnotify::{
    FsConfig, FsEvent, FsEventKind, FsEventSource, FsNotifyError, GitMetaKind,
};
/// `false` twin: this template is not compiled into this build, so no
/// template matches. Keeps ungated call sites compiling in both
/// configurations.
pub(crate) fn is_cursor_user_template(
    _template: &echo_agent_core::prompt::user_message::UserMessageTemplate,
) -> bool {
    false
}
/// `false` twin of [`is_cursor_system_template`]; see [`is_cursor_user_template`].
pub(crate) fn is_cursor_system_template(
    _template: &echo_agent_core::prompt::context::TemplateOverride,
) -> bool {
    false
}
/// Pull the `ContentBlock::Image`s out of a block list — the single spelling
/// of "only Image blocks ride structurally" (interject parse + queue-interject
/// harvest).
pub(crate) fn image_blocks(
    blocks: impl IntoIterator<Item = agent_client_protocol::ContentBlock>,
) -> Vec<agent_client_protocol::ImageContent> {
    blocks
        .into_iter()
        .filter_map(|block| match block {
            agent_client_protocol::ContentBlock::Image(img) => Some(img),
            _ => None,
        })
        .collect()
}
pub use echo_agent_lifecycle::{
    AnalyticsClass, CompactionClass, InputAuthority, InputPolicy, QueuePolicy, ShutdownPolicy,
    TurnBoundary,
};
/// Describes who originated a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptOrigin {
    /// A normal user-initiated prompt.
    User,
    /// Auto-wake prompt injected when a background terminal task completed.
    TaskCompleted {
        /// The background task ID (without the `task-completed-` prefix).
        task_id: String,
    },
    /// Auto-wake prompt injected when a background subagent completed.
    SubagentCompleted {
        /// The subagent ID (without the `subagent-completed-` prefix).
        subagent_id: String,
    },
    WorkflowCompleted {
        completion_id: String,
    },
    /// Server-initiated prompt from the idle-gated notification drain
    /// (`maybe_drain_notifications`). Batches one or more monitor-event
    /// or bash-task-completed notifications into a single turn while the
    /// user is idle.
    NotificationDrain,
    /// Orchestrator-initiated summary turn. The goal orchestrator injects a
    /// system reminder into context and then triggers a model turn so the
    /// model can print a visible progress update.
    GoalSummary,
    /// Verification-stage nudge injected after the verification stage
    /// achieved — keep working" system-reminder body alongside the
    /// path to the persisted details file. The variant name retains
    /// the `Classifier` prefix for wire stability.
    GoalClassifierNudge,
    /// Scheduled task (`/loop`) prompt fired by the scheduler via the pager.
    SchedulerFired,
    /// Turn injected after a resumed plan-approval decision: the
    /// shell re-parked `exit_plan_mode` on resume, the user approved/revised,
    /// and the shell injects the follow-up turn. Synthetic so the user never
    /// typed it — kept out of prompt history — but it still runs a real turn.
    PlanResume,
}
impl PromptOrigin {
    /// Safe client provenance. The internal prompt/reminder body stays hidden.
    pub fn client_metadata(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Self::User => json!({"origin":"user"}),
            Self::TaskCompleted { task_id } => {
                json!({"origin":"background_task","sourceId":task_id})
            }
            Self::SubagentCompleted { subagent_id } => {
                json!({"origin":"subagent","sourceId":subagent_id})
            }
            Self::WorkflowCompleted { completion_id } => {
                json!({"origin":"runtime","sourceId":completion_id})
            }
            Self::NotificationDrain => json!({"origin":"notification"}),
            Self::GoalSummary | Self::GoalClassifierNudge => json!({"origin":"goal"}),
            Self::SchedulerFired => json!({"origin":"scheduled"}),
            Self::PlanResume => json!({"origin":"plan_resume"}),
        }
    }
    /// Parse a prompt_id string into a `PromptOrigin`.
    pub fn from_prompt_id(prompt_id: &str) -> Self {
        if let Some(task_id) = prompt_id.strip_prefix("task-completed-") {
            Self::TaskCompleted {
                task_id: task_id.to_string(),
            }
        } else if let Some(subagent_id) = prompt_id.strip_prefix("subagent-completed-") {
            Self::SubagentCompleted {
                subagent_id: subagent_id.to_string(),
            }
        } else if let Some(completion_id) = prompt_id.strip_prefix("workflow-completed-") {
            Self::WorkflowCompleted {
                completion_id: completion_id.to_string(),
            }
        } else if prompt_id.starts_with("notifications-") {
            Self::NotificationDrain
        } else if prompt_id.starts_with("goal-summary-") {
            Self::GoalSummary
        } else if prompt_id.starts_with("goal-classifier-nudge-") {
            Self::GoalClassifierNudge
        } else if prompt_id.starts_with("scheduler-fired-") {
            Self::SchedulerFired
        } else if prompt_id.starts_with("plan-resume-") {
            Self::PlanResume
        } else {
            Self::User
        }
    }
    pub fn policy(&self) -> InputPolicy {
        match self {
            Self::User => InputPolicy {
                authority: InputAuthority::HumanIntent,
                turn_boundary: TurnBoundary::Conversational,
                analytics: AnalyticsClass::HumanPrompt,
                compaction: CompactionClass::HumanAnchor,
                queue: QueuePolicy::VisibleEditable,
                shutdown: ShutdownPolicy::Drain,
            },
            Self::TaskCompleted { .. }
            | Self::SubagentCompleted { .. }
            | Self::WorkflowCompleted { .. }
            | Self::SchedulerFired => InputPolicy {
                authority: InputAuthority::RuntimeControl,
                turn_boundary: TurnBoundary::Conversational,
                analytics: AnalyticsClass::RuntimeWake,
                compaction: CompactionClass::RuntimeEphemera,
                queue: QueuePolicy::Hidden,
                shutdown: ShutdownPolicy::CancelWithProducer,
            },
            Self::NotificationDrain
            | Self::GoalSummary
            | Self::GoalClassifierNudge
            | Self::PlanResume => InputPolicy {
                authority: InputAuthority::RuntimeControl,
                turn_boundary: TurnBoundary::Conversational,
                analytics: AnalyticsClass::RuntimeWake,
                compaction: CompactionClass::RuntimeEphemera,
                queue: QueuePolicy::Hidden,
                shutdown: ShutdownPolicy::DropEphemeral,
            },
        }
    }
    /// Returns `true` for auto-wake (synthetic) prompts.
    pub fn is_synthetic(&self) -> bool {
        !matches!(self, Self::User)
    }
    /// Whether a `UserMessageChunk` echo for this origin must stay out of
    /// client scrollback (live and on resume). Model-only / side-channel
    /// content — UI already surfaces it via task pane, monitor gutter, etc.
    ///
    /// Cron (`SchedulerFired`) and plan-resume follow-ups still render;
    /// real user turns always render.
    pub fn hide_user_echo_from_scrollback(&self) -> bool {
        match self {
            Self::User | Self::SchedulerFired | Self::PlanResume => false,
            Self::TaskCompleted { .. }
            | Self::SubagentCompleted { .. }
            | Self::WorkflowCompleted { .. }
            | Self::NotificationDrain
            | Self::GoalSummary
            | Self::GoalClassifierNudge => true,
        }
    }
    pub fn completion_id(&self) -> Option<&str> {
        match self {
            Self::TaskCompleted { task_id } => Some(task_id),
            Self::SubagentCompleted { subagent_id } => Some(subagent_id),
            Self::WorkflowCompleted { completion_id } => Some(completion_id),
            Self::User
            | Self::NotificationDrain
            | Self::GoalSummary
            | Self::GoalClassifierNudge
            | Self::SchedulerFired
            | Self::PlanResume => None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::{PromptOrigin, QueuePolicy};
    #[test]
    fn client_turn_metadata_preserves_source_without_internal_prompt() {
        assert_eq!(
            PromptOrigin::User.client_metadata(),
            serde_json::json!({"origin":"user"})
        );
        assert_eq!(
            PromptOrigin::TaskCompleted {
                task_id: "task-1".into()
            }
            .client_metadata(),
            serde_json::json!({"origin":"background_task","sourceId":"task-1"})
        );
        assert_eq!(
            PromptOrigin::SubagentCompleted {
                subagent_id: "child-1".into()
            }
            .client_metadata(),
            serde_json::json!({"origin":"subagent","sourceId":"child-1"})
        );
        assert_eq!(
            PromptOrigin::NotificationDrain.client_metadata(),
            serde_json::json!({"origin":"notification"})
        );
    }
    #[test]
    fn from_prompt_id_user() {
        assert_eq!(
            PromptOrigin::from_prompt_id("my-prompt"),
            PromptOrigin::User
        );
        assert!(!PromptOrigin::from_prompt_id("my-prompt").is_synthetic());
    }
    #[test]
    fn from_prompt_id_task_completed() {
        let origin = PromptOrigin::from_prompt_id("task-completed-abc-123");
        assert_eq!(
            origin,
            PromptOrigin::TaskCompleted {
                task_id: "abc-123".into()
            }
        );
        assert!(origin.is_synthetic());
        assert_eq!(origin.completion_id(), Some("abc-123"));
    }
    #[test]
    fn from_prompt_id_subagent_completed() {
        let origin = PromptOrigin::from_prompt_id("subagent-completed-xyz-789");
        assert_eq!(
            origin,
            PromptOrigin::SubagentCompleted {
                subagent_id: "xyz-789".into()
            }
        );
        assert!(origin.is_synthetic());
        assert_eq!(origin.completion_id(), Some("xyz-789"));
    }
    #[test]
    fn from_prompt_id_notification_drain() {
        let origin =
            PromptOrigin::from_prompt_id("notifications-019e0000-0000-7000-8000-0000000000aa");
        assert_eq!(origin, PromptOrigin::NotificationDrain);
        assert!(origin.is_synthetic());
        assert_eq!(origin.completion_id(), None);
    }
    #[test]
    fn goal_summary_origin_from_prompt_id() {
        let origin = PromptOrigin::from_prompt_id("goal-summary-019e2d3e");
        assert!(matches!(origin, PromptOrigin::GoalSummary));
        assert!(origin.is_synthetic());
        assert_eq!(origin.completion_id(), None);
    }
    #[test]
    fn goal_classifier_nudge_origin_from_prompt_id() {
        let origin = PromptOrigin::from_prompt_id("goal-classifier-nudge-019e2d3e");
        assert!(matches!(origin, PromptOrigin::GoalClassifierNudge));
        assert!(origin.is_synthetic());
        assert_eq!(origin.completion_id(), None);
    }
    #[test]
    fn goal_classifier_nudge_origin_round_trips_through_from_prompt_id() {
        let prompt_id = format!("goal-classifier-nudge-{}", uuid::Uuid::now_v7());
        let origin = PromptOrigin::from_prompt_id(&prompt_id);
        assert!(matches!(origin, PromptOrigin::GoalClassifierNudge));
        assert!(origin.is_synthetic());
    }
    #[test]
    fn scheduler_fired_origin_from_prompt_id() {
        let origin = PromptOrigin::from_prompt_id("scheduler-fired-019e51a3-abcd-1234");
        assert!(matches!(origin, PromptOrigin::SchedulerFired));
        assert!(origin.is_synthetic());
        assert_eq!(origin.completion_id(), None);
    }
    #[test]
    fn plan_resume_origin_from_prompt_id() {
        let origin = PromptOrigin::from_prompt_id("plan-resume-1730000000000");
        assert!(matches!(origin, PromptOrigin::PlanResume));
        assert!(origin.is_synthetic());
        assert_eq!(origin.completion_id(), None);
    }
    #[test]
    fn notification_drain_is_server_initiated() {
        let prompt_id = "notifications-019e0000-0000-7000-8000-0000000000aa";
        assert!(PromptOrigin::from_prompt_id(prompt_id).is_synthetic());
    }
    #[test]
    fn current_origin_queue_policies_are_preserved() {
        let cases = [
            (PromptOrigin::User, QueuePolicy::VisibleEditable),
            (
                PromptOrigin::TaskCompleted {
                    task_id: "t".into(),
                },
                QueuePolicy::Hidden,
            ),
            (
                PromptOrigin::SubagentCompleted {
                    subagent_id: "s".into(),
                },
                QueuePolicy::Hidden,
            ),
            (
                PromptOrigin::WorkflowCompleted {
                    completion_id: "w".into(),
                },
                QueuePolicy::Hidden,
            ),
            (PromptOrigin::NotificationDrain, QueuePolicy::Hidden),
            (PromptOrigin::GoalSummary, QueuePolicy::Hidden),
            (PromptOrigin::GoalClassifierNudge, QueuePolicy::Hidden),
            (PromptOrigin::SchedulerFired, QueuePolicy::Hidden),
            (PromptOrigin::PlanResume, QueuePolicy::Hidden),
        ];
        for (origin, queue) in cases {
            assert_eq!(origin.policy().queue, queue, "{origin:?}");
        }
    }
    #[test]
    fn hide_user_echo_from_scrollback_by_origin() {
        assert!(!PromptOrigin::User.hide_user_echo_from_scrollback());
        assert!(
            !PromptOrigin::from_prompt_id("scheduler-fired-abc").hide_user_echo_from_scrollback()
        );
        assert!(!PromptOrigin::from_prompt_id("plan-resume-1").hide_user_echo_from_scrollback());
        assert!(PromptOrigin::from_prompt_id("task-completed-t1").hide_user_echo_from_scrollback());
        assert!(
            PromptOrigin::from_prompt_id("subagent-completed-s1").hide_user_echo_from_scrollback()
        );
        assert!(
            PromptOrigin::from_prompt_id("notifications-uuid").hide_user_echo_from_scrollback()
        );
        assert!(
            PromptOrigin::from_prompt_id("workflow-completed-wf-1-9")
                .hide_user_echo_from_scrollback()
        );
        assert!(PromptOrigin::from_prompt_id("goal-summary-1").hide_user_echo_from_scrollback());
        assert!(
            PromptOrigin::from_prompt_id("goal-classifier-nudge-1")
                .hide_user_echo_from_scrollback()
        );
    }
}
/// Client-requested fs notification mode (was echo_agent_fsnotify::FsNotifyMode).
/// Determines whether the session sends an initial file index to the client
/// or just streams raw file events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub(crate) enum ClientFsMode {
    #[default]
    Events,
    Index,
}
/// Client-side fs notification config: fs source settings + mode.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClientFsConfig {
    pub fs: FsConfig,
    pub mode: ClientFsMode,
}
/// Share session request/response types
pub mod share {
    /// Request to share a session via URL
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ShareSessionRequest {
        pub session_id: String,
    }
    /// Response containing the shareable URL
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    pub struct ShareSessionResponse {
        pub share_url: String,
    }
}
/// Proxy config for the session registry client.
/// Shared between `acp_session` (slash commands) and `persistence` (title generation).
#[derive(Clone)]
pub(crate) struct RegistryConfig {
    pub base_url: String,
    pub user_token: String,
    pub deployment_key: Option<String>,
    pub alpha_test_key: Option<String>,
}
pub mod acp_conversion;
pub(crate) mod acp_mcp;
pub(crate) mod acp_session;
pub(crate) mod agent_rebuild;
pub(crate) mod chat_persistence;
pub(crate) mod events;
pub mod export;
pub mod feedback;
pub mod feedback_manager;
pub mod file_system;
pub mod fork;
pub(crate) mod fs_watch;
pub(crate) mod goal_classifier;
pub(crate) mod goal_evaluator;
pub(crate) mod goal_next_step;
pub(crate) mod goal_orchestrator;
pub(crate) mod goal_planner;
pub(crate) mod goal_role_tools;
pub(crate) mod goal_stop_detector;
pub(crate) mod goal_strategist;
pub(crate) mod goal_summarizer;
pub mod goal_tracker;
pub mod helpers;
pub(crate) mod image_describe;
pub(crate) mod image_normalize;
pub(crate) mod inference_metrics;
pub use echo_agent_shared::session::info;
pub mod managed_mcp;
pub(crate) mod mcp_descriptors;
pub(crate) mod mcp_dispatcher;
#[cfg(test)]
mod mcp_dispatcher_e2e_tests;
pub(crate) mod mcp_elicitation;
pub(crate) mod mcp_restart;
pub mod mcp_servers;
pub mod memory;
pub(crate) mod memory_observation;
pub(crate) mod normalize_cache;
pub mod persistence;
pub use echo_agent_shared::placeholder_images;
pub mod plan_mode;
pub mod prompt_history;
pub mod prompt_parser;
pub(crate) mod prompt_timing;
pub(crate) mod replay_events;
pub mod repo_changes;
#[path = "restore_stub.rs"]
pub mod restore;
pub mod result;
pub mod signals;
pub(crate) mod slash_commands;
pub use slash_commands::PAGER_COMMAND_KEYS;
pub mod storage;
pub(crate) mod streaming_capture;
pub(crate) mod summary;
pub(crate) mod telemetry;
#[cfg(feature = "test-support")]
pub mod testkit;
pub mod tool_index;
pub(crate) mod turn_completion;
pub mod unified_list;
pub(crate) mod user_message;
pub(crate) mod wire_tags;
pub(crate) mod workflow;
pub mod worktree;
pub mod worktree_pool;
