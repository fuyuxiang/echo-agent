/// Canonical `decision_reason` values for the uploaded artifact. This module is
/// the single owner of the decision-reason vocabulary; downstream telemetry
/// enums assert coverage against [`reasons::ALL`] so a new reason here fails the
/// cross-crate drift test rather than being silently dropped.
pub const YOLO: &str = "yolo";
pub const POLICY_ALLOW: &str = "policy_allow";
pub const POLICY_DENY: &str = "policy_deny";
pub const POLICY_ASK: &str = "policy_ask";
pub const BASH_COMMAND_GATE_ASK: &str = "bash_command_gate_ask";
pub const SHELL_FILE_GATE_ASK: &str = "shell_file_gate_ask";
pub const AUTO_FAST_PATH: &str = "auto_fast_path";
pub const AUTO_CLASSIFIER_ALLOW: &str = "auto_classifier_allow";
pub const AUTO_CLASSIFIER_DENY: &str = "auto_classifier_deny";
pub const AUTO_CLASSIFIER_TIMEOUT: &str = "auto_classifier_timeout";
pub const AUTO_CLASSIFIER_UNAVAILABLE: &str = "auto_classifier_unavailable";
pub const AUTO_DENIAL_LIMIT: &str = "auto_denial_limit";
pub const SANDBOX_AUTO: &str = "sandbox_auto";
pub const PERSISTED_GRANT: &str = "persisted_grant";
pub const SESSION_GRANT: &str = "session_grant";
pub const STATIC_ALLOWLIST: &str = "static_allowlist";
pub const SAFE_COMMAND: &str = "safe_command";
pub const SESSION_DENY: &str = "session_deny";
pub const PROMPT_DENY: &str = "prompt_deny";
pub const NEEDS_USER: &str = "needs_user";
pub const BASH_REQUEST_FLOOR: &str = "bash_request_floor";
pub const OPAQUE_SHELL: &str = "opaque_shell";
pub const REQUESTER_GONE: &str = "requester_gone";

/// Every canonical reason, in declaration order. A new reason constant must be
/// added here; the telemetry `PermissionDecisionReason` drift test then fails
/// until the enum gains a matching variant.
pub const ALL: &[&str] = &[
    YOLO,
    POLICY_ALLOW,
    POLICY_DENY,
    POLICY_ASK,
    BASH_COMMAND_GATE_ASK,
    SHELL_FILE_GATE_ASK,
    AUTO_FAST_PATH,
    AUTO_CLASSIFIER_ALLOW,
    AUTO_CLASSIFIER_DENY,
    AUTO_CLASSIFIER_TIMEOUT,
    AUTO_CLASSIFIER_UNAVAILABLE,
    AUTO_DENIAL_LIMIT,
    SANDBOX_AUTO,
    PERSISTED_GRANT,
    SESSION_GRANT,
    STATIC_ALLOWLIST,
    SAFE_COMMAND,
    SESSION_DENY,
    PROMPT_DENY,
    NEEDS_USER,
    BASH_REQUEST_FLOOR,
    OPAQUE_SHELL,
    REQUESTER_GONE,
];
