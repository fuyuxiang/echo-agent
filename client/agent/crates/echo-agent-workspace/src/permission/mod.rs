pub mod auto_mode;
pub mod bash_command_splitting;
pub mod claude_settings;
mod exec_risk;
mod gate_preflight;
mod hub_permission;
mod manager;
mod policy;
mod prompter;
pub mod resolution;
pub mod rules;
mod shell_access;
mod state;
pub mod types;

/// Declare a data-free "wire vocabulary" enum whose variant set, iteration
/// inventory (`ALL`), and wire strings (`wire_str`) are all generated from ONE
/// list, so they can never drift apart. Adding a value is a single list entry
/// that produces the variant, its `ALL` slot, and its wire string together;
/// removing/renaming one is a compile error at every exhaustive `match`.
///
/// This is the single source of truth for the owner-side permission vocabularies
/// (`PromptOutcomeKind`, `ClassifierSourceKind`, `ClassifierVerdict`) that
/// production event emission projects through and cross-crate telemetry drift
/// tests iterate.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($variant:ident => $wire:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $vis enum $name {
            $($variant),+
        }

        impl $name {
            /// Every variant, in declaration order. Generated with the enum, so it
            /// can never omit a variant (closes the `ALL`-drift hole structurally).
            $vis const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Canonical wire string. Generated with the enum from the same list,
            /// so a variant can never lack a wire value or carry a stale one.
            $vis const fn wire_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }
    };
}
pub(crate) use wire_enum;

pub use auto_mode::{
    AUTO_MODE_CLASSIFIER_SYSTEM_PROMPT, AutoFastPath, BashSecurityAssessment,
    CLASSIFIER_TURN_MAX_LEN, ClassifierContext, ClassifierFailure, ClassifierMessage,
    ClassifierMessageRole, ClassifierOutcome, ClassifierPromptType, ClassifierSecurityFinding,
    ClassifierSource, ClassifierSourceKind, ClassifierTurn, ClassifierVerdict, ClassifyTextChannel,
    ClassifyTextFn, FixedClassifier, HeuristicPermissionClassifier, LlmPermissionClassifier,
    PermissionClassifier, SharedClassifier, access_requires_user_interaction, auto_mode_fast_path,
    build_classifier_messages, classifier_output_json_schema, default_auto_mode_classifier,
    is_auto_mode_allowlisted_access, is_auto_mode_allowlisted_tool_name,
    parse_classifier_model_output, parse_classifier_model_text, permission_decision_args,
};
pub use hub_permission::{
    PermissionHookTransport, ToolServerPermissionTransport, access_kind_for_hub_tool,
    hitl_permission_live_enabled, prompt_outcome_allows, request_permission_via_hub,
};

/// Zero-init this module's metric families. See [`crate::init_metrics`].
pub(crate) fn init_metrics() {
    hub_permission::init_metrics();
}
pub use manager::{
    AUTO_DENY_CONSECUTIVE_LIMIT, AUTO_DENY_TOTAL_LIMIT, PermissionHandle,
    always_allow_scope_persists, default_always_allow_scope, default_always_deny_scope,
    minimum_always_allow_scope, reasons, spawn_permission_manager,
    spawn_permission_manager_with_hub,
};
pub use policy::{
    CompiledPolicy, bash_glob_is_catchall, bash_pattern_is_broad, bash_pattern_matches_command,
};
pub use prompter::{
    ALLOW_EDITS_SESSION_OPTION_ID, AcpPrompter, BashCommandPermission, BashCommandSelectedTerms,
    ENABLE_ALWAYS_APPROVE_OPTION_ID, MCP_TOOL_NAME_DELIMITER, McpScopeSelection, McpToolPermission,
    PromptOutcome, PromptOutcomeKind, is_enable_always_approve_option,
    mcp_pretty_name_if_qualified, mcp_titleize_segment, mcp_tool_action, mcp_tool_display_name,
};
pub use shell_access::{ProtectedEditPermission, ProtectedEditReason};
pub use state::PermissionState;
pub use state::cleanup_stale_permission_state;
pub use types::{
    AccessKind, ClientType, Decision, PermissionCommand, PermissionEvent, PermissionResolution,
};
