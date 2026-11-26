//! Manager-resolved permission analytics projected onto product events.
//!
//! Closed enums + [`PermissionDecisionPayload`] fields. Unknown manager strings
//! are rejected by `TryFrom` and omitted by the shell rather than exported.

use serde::Serialize;

use super::{AccessKind, PermissionOutcome};
use crate::enums::PermissionMode;

// ─────────────────────────────────────────────────────────────────────────────
// Manager-resolved permission analytics (content-free, additive to product events)
//
// These closed enums project the workspace manager's authoritative
// `PermissionEvent` (in `echo-agent-workspace`) onto `PermissionDecisionPayload`.
// The mappings are the single canonical `TryFrom<&str>` inverse of the manager's
// string constants; unknown strings are rejected (the caller omits the field and
// records a fixed local diagnostic — never exports the raw value). None of these
// carry commands, paths, arguments, or any free text.
// ─────────────────────────────────────────────────────────────────────────────

/// Normalized human prompt outcome. Only `Allow`/`Reject` count as a human
/// response for the primary KPI denominator; the manager's finer raw outcome
/// strings (`allow_once`, `reject_always_bash`, …) all collapse here.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPromptOutcome {
    Allow,
    Reject,
    Cancel,
    Followup,
    Error,
}

impl PermissionPromptOutcome {
    /// Every normalized category, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::Allow,
        Self::Reject,
        Self::Cancel,
        Self::Followup,
        Self::Error,
    ];
}

impl TryFrom<&str> for PermissionPromptOutcome {
    type Error = ();
    /// Map a manager raw prompt-outcome string to the normalized category.
    /// Covers every `outcome_str` the manager emits at its prompt path.
    fn try_from(s: &str) -> Result<Self, ()> {
        match s {
            "allow_once"
            | "allow_always"
            | "allow_always_bash"
            | "allow_always_bash_glob"
            | "allow_always_domain"
            | "allow_always_mcp_tool"
            | "allow_always_mcp_server"
            | "allow_edits_for_session" => Ok(Self::Allow),
            "reject_once"
            | "reject_always_bash"
            | "reject_always_mcp_tool"
            | "reject_always_domain" => Ok(Self::Reject),
            "cancelled" => Ok(Self::Cancel),
            "followup" => Ok(Self::Followup),
            "error" => Ok(Self::Error),
            _ => Err(()),
        }
    }
}

/// Granular prompt outcome, preserving the per-row detail that
/// [`PermissionPromptOutcome`] collapses — measures "Always allow …" /
/// "Never allow" adoption separately from allow-once clicks. Additive; the
/// KPI denominator stays on the normalized enum.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPromptOutcomeDetail {
    AllowOnce,
    AllowAlways,
    AllowEditsForSession,
    AllowAlwaysBash,
    AllowAlwaysBashGlob,
    AllowAlwaysDomain,
    AllowAlwaysMcpTool,
    AllowAlwaysMcpServer,
    RejectOnce,
    RejectAlwaysBash,
    RejectAlwaysMcpTool,
    RejectAlwaysDomain,
    Cancelled,
    Followup,
    Error,
}

impl PermissionPromptOutcomeDetail {
    /// Every variant, in declaration order. The shell drift test asserts a
    /// bijection with the manager's `PromptOutcomeKind::ALL`.
    pub const ALL: &'static [Self] = &[
        Self::AllowOnce,
        Self::AllowAlways,
        Self::AllowEditsForSession,
        Self::AllowAlwaysBash,
        Self::AllowAlwaysBashGlob,
        Self::AllowAlwaysDomain,
        Self::AllowAlwaysMcpTool,
        Self::AllowAlwaysMcpServer,
        Self::RejectOnce,
        Self::RejectAlwaysBash,
        Self::RejectAlwaysMcpTool,
        Self::RejectAlwaysDomain,
        Self::Cancelled,
        Self::Followup,
        Self::Error,
    ];
}

impl TryFrom<&str> for PermissionPromptOutcomeDetail {
    type Error = ();
    /// Inverse of the manager's `PromptOutcomeKind::wire_str` vocabulary.
    fn try_from(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "allow_once" => Self::AllowOnce,
            "allow_always" => Self::AllowAlways,
            "allow_edits_for_session" => Self::AllowEditsForSession,
            "allow_always_bash" => Self::AllowAlwaysBash,
            "allow_always_bash_glob" => Self::AllowAlwaysBashGlob,
            "allow_always_domain" => Self::AllowAlwaysDomain,
            "allow_always_mcp_tool" => Self::AllowAlwaysMcpTool,
            "allow_always_mcp_server" => Self::AllowAlwaysMcpServer,
            "reject_once" => Self::RejectOnce,
            "reject_always_bash" => Self::RejectAlwaysBash,
            "reject_always_mcp_tool" => Self::RejectAlwaysMcpTool,
            "reject_always_domain" => Self::RejectAlwaysDomain,
            "cancelled" => Self::Cancelled,
            "followup" => Self::Followup,
            "error" => Self::Error,
            _ => return Err(()),
        })
    }
}

/// Canonical closed decision-reason (the manager's `decision_reason` trigger).
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecisionReason {
    Yolo,
    PolicyAllow,
    PolicyDeny,
    PolicyAsk,
    BashCommandGateAsk,
    ShellFileGateAsk,
    AutoFastPath,
    AutoClassifierAllow,
    AutoClassifierDeny,
    AutoClassifierTimeout,
    AutoClassifierUnavailable,
    AutoDenialLimit,
    SandboxAuto,
    PersistedGrant,
    SessionGrant,
    StaticAllowlist,
    SafeCommand,
    SessionDeny,
    PromptDeny,
    NeedsUser,
    BashRequestFloor,
    OpaqueShell,
    RequesterGone,
}

impl PermissionDecisionReason {
    /// Every variant, in declaration order. Used by drift tests to assert the
    /// enum is a bijection with the manager's owned `reasons::ALL` vocabulary.
    pub const ALL: &'static [Self] = &[
        Self::Yolo,
        Self::PolicyAllow,
        Self::PolicyDeny,
        Self::PolicyAsk,
        Self::BashCommandGateAsk,
        Self::ShellFileGateAsk,
        Self::AutoFastPath,
        Self::AutoClassifierAllow,
        Self::AutoClassifierDeny,
        Self::AutoClassifierTimeout,
        Self::AutoClassifierUnavailable,
        Self::AutoDenialLimit,
        Self::SandboxAuto,
        Self::PersistedGrant,
        Self::SessionGrant,
        Self::StaticAllowlist,
        Self::SafeCommand,
        Self::SessionDeny,
        Self::PromptDeny,
        Self::NeedsUser,
        Self::BashRequestFloor,
        Self::OpaqueShell,
        Self::RequesterGone,
    ];
}

impl TryFrom<&str> for PermissionDecisionReason {
    type Error = ();
    /// Inverse of the manager `reasons::*` constants. Every constant must map.
    fn try_from(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "yolo" => Self::Yolo,
            "policy_allow" => Self::PolicyAllow,
            "policy_deny" => Self::PolicyDeny,
            "policy_ask" => Self::PolicyAsk,
            "bash_command_gate_ask" => Self::BashCommandGateAsk,
            "shell_file_gate_ask" => Self::ShellFileGateAsk,
            "auto_fast_path" => Self::AutoFastPath,
            "auto_classifier_allow" => Self::AutoClassifierAllow,
            "auto_classifier_deny" => Self::AutoClassifierDeny,
            "auto_classifier_timeout" => Self::AutoClassifierTimeout,
            "auto_classifier_unavailable" => Self::AutoClassifierUnavailable,
            "auto_denial_limit" => Self::AutoDenialLimit,
            "sandbox_auto" => Self::SandboxAuto,
            "persisted_grant" => Self::PersistedGrant,
            "session_grant" => Self::SessionGrant,
            "static_allowlist" => Self::StaticAllowlist,
            "safe_command" => Self::SafeCommand,
            "session_deny" => Self::SessionDeny,
            "prompt_deny" => Self::PromptDeny,
            "needs_user" => Self::NeedsUser,
            "bash_request_floor" => Self::BashRequestFloor,
            "opaque_shell" => Self::OpaqueShell,
            "requester_gone" => Self::RequesterGone,
            _ => return Err(()),
        })
    }
}

/// Auto-classifier path taken (the manager's `classifier_source`). `NotWired`
/// means the Auto classifier route was entered but no classifier was installed
/// (`set_classifier(None)`), so nothing was actually judged — distinct from
/// `Heuristic`, which is a real heuristic verdict.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PermissionClassifierSource {
    Llm,
    Heuristic,
    Timeout,
    TransportError,
    FastPath,
    NotWired,
}

impl PermissionClassifierSource {
    /// Every source, in declaration order. Used by the shell drift test to assert
    /// a bijection with the workspace owner projection `ClassifierSourceKind::ALL`
    /// (classifier provenances plus the `fast_path`/`not_wired` states).
    pub const ALL: &'static [Self] = &[
        Self::Llm,
        Self::Heuristic,
        Self::Timeout,
        Self::TransportError,
        Self::FastPath,
        Self::NotWired,
    ];
}

impl TryFrom<&str> for PermissionClassifierSource {
    type Error = ();
    fn try_from(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "llm" => Self::Llm,
            "heuristic" => Self::Heuristic,
            "timeout" => Self::Timeout,
            "transport_error" => Self::TransportError,
            "fast_path" => Self::FastPath,
            "not_wired" => Self::NotWired,
            _ => return Err(()),
        })
    }
}

/// Auto-classifier verdict (the manager's `classifier_verdict`).
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PermissionClassifierVerdict {
    Allow,
    Block,
    Unavailable,
}

impl PermissionClassifierVerdict {
    /// Every verdict, in declaration order. Used by the shell drift test to assert
    /// a bijection with the workspace `ClassifierVerdict::ALL` vocabulary.
    pub const ALL: &'static [Self] = &[Self::Allow, Self::Block, Self::Unavailable];
}

impl TryFrom<&str> for PermissionClassifierVerdict {
    type Error = ();
    fn try_from(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "allow" => Self::Allow,
            "block" => Self::Block,
            "unavailable" => Self::Unavailable,
            _ => return Err(()),
        })
    }
}

/// Fixed harness-owned static-analysis finding token. Mirrors the workspace
/// `ClassifierSecurityFinding` wire tokens; carries no command/path/argument.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PermissionSecurityFinding {
    FailClosedPolicy,
    UnparseableShell,
    OpaqueShell,
    ExecOrAmbientGit,
    EnvInjection,
    UnvettedEnv,
    FileWrite,
    DangerousCommand,
    SpecialExecSurface,
}

impl PermissionSecurityFinding {
    /// Every variant, in declaration order. Used by the drift test to assert the
    /// enum is a bijection with the workspace `ClassifierSecurityFinding` tokens.
    pub const ALL: &'static [Self] = &[
        Self::FailClosedPolicy,
        Self::UnparseableShell,
        Self::OpaqueShell,
        Self::ExecOrAmbientGit,
        Self::EnvInjection,
        Self::UnvettedEnv,
        Self::FileWrite,
        Self::DangerousCommand,
        Self::SpecialExecSurface,
    ];
}

impl TryFrom<&str> for PermissionSecurityFinding {
    type Error = ();
    fn try_from(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "fail_closed_policy" => Self::FailClosedPolicy,
            "unparseable_shell" => Self::UnparseableShell,
            "opaque_shell" => Self::OpaqueShell,
            "exec_or_ambient_git" => Self::ExecOrAmbientGit,
            "env_injection" => Self::EnvInjection,
            "unvetted_env" => Self::UnvettedEnv,
            "file_write" => Self::FileWrite,
            "dangerous_command" => Self::DangerousCommand,
            "special_exec_surface" => Self::SpecialExecSurface,
            _ => return Err(()),
        })
    }
}

/// Primary Auto-mode KPI classification for one decision. The cohort is a real
/// classifier Block escalated to a UI prompt with an explicit human response;
/// see [`auto_denial_kpi`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AutoDenialKpi {
    /// Human agreed with the classifier (rejected the action).
    Alignment,
    /// Human overrode the classifier (allowed the action) — an eval candidate,
    /// not proof the classifier was wrong.
    Disagreement,
}

/// Whether a decision belongs to the primary Auto denial-limit KPI cohort and,
/// if so, which side. Cohort: `permission_mode = auto`,
/// `decision_reason = auto_denial_limit`, `classifier_verdict = block`, and a
/// human `prompt_outcome` of `allow` (disagreement) or `reject` (alignment).
/// All other outcomes (cancel, followup, error, timeouts, policy/gate prompts)
/// return `None` and are excluded from the denominator.
pub fn auto_denial_kpi(p: &PermissionDecisionPayload) -> Option<AutoDenialKpi> {
    if p.permission_mode != PermissionMode::Auto
        || p.decision_reason != Some(PermissionDecisionReason::AutoDenialLimit)
        || p.classifier_verdict != Some(PermissionClassifierVerdict::Block)
    {
        return None;
    }
    match p.prompt_outcome {
        Some(PermissionPromptOutcome::Allow) => Some(AutoDenialKpi::Disagreement),
        Some(PermissionPromptOutcome::Reject) => Some(AutoDenialKpi::Alignment),
        _ => None,
    }
}

#[derive(Serialize)]
pub struct PermissionPrompted {
    pub tool_name: String,
    pub access_kind: AccessKind,
    pub permission_mode: PermissionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
}

#[derive(Serialize)]
pub struct PermissionDecisionPayload {
    pub tool_name: String,
    pub access_kind: AccessKind,
    pub decision: PermissionOutcome,
    pub wait_ms: u64,
    pub permission_mode: PermissionMode,
    /// Decision provenance (`config`/`user_reject`/`user_abort`/…), from
    /// shell's `permission_decision_source`. Additive analytics-visible field, added
    /// for the external `tool_decision` event (design ‡ footnote).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    // ── Manager-resolved analytics (content-free; None when the manager
    //    returned no event, so the shell omits rather than fabricates). These
    //    additions are NOT part of the external OTEL projection: `map_tool_decision`
    //    ignores them, so the external record stays byte-for-byte unchanged. ──
    /// Whether the manager *attempted* a UI prompt (its `user_prompted`). This is
    /// not proof the client rendered a prompt; a human response is proven only by
    /// `prompt_outcome` being `Allow`/`Reject`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager_prompt_attempted: Option<bool>,
    /// Normalized human prompt outcome; `None` unless the request was prompted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_outcome: Option<PermissionPromptOutcome>,
    /// Granular prompt outcome (per-row detail); `None` unless prompted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_outcome_detail: Option<PermissionPromptOutcomeDetail>,
    /// Whether the `remember_tool_approvals` gate was on for this decision;
    /// `None` on legacy manager events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remember_tool_approvals: Option<bool>,
    /// Canonical decision-reason trigger.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<PermissionDecisionReason>,
    /// Auto-classifier path, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_source: Option<PermissionClassifierSource>,
    /// Auto-classifier verdict, if the classifier route produced one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_verdict: Option<PermissionClassifierVerdict>,
    /// Fixed finding tokens (no descriptions/payload). `Some([])` means the
    /// classifier route ran with an empty attempted assessment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub security_findings: Option<Vec<PermissionSecurityFinding>>,
    /// Milliseconds spent in classification, if a classifier ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_latency_ms: Option<u64>,
    /// Consecutive auto denials at decision time, clamped to the manager budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_denials_consecutive: Option<u32>,
    /// Total auto denials at decision time, clamped to the manager budget.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_denials_total: Option<u32>,
}

#[cfg(test)]
mod permission_analytics_tests {
    use super::*;

    /// Self-consistency for the reason enum: every variant serializes to a
    /// snake_case string that `TryFrom` maps back to the same variant. The
    /// cross-crate bijection against the manager's owned `reasons::ALL`
    /// vocabulary is enforced by the shell drift test (which can depend on the
    /// workspace crate); this crate cannot, so it guards enum↔wire consistency.
    #[test]
    fn decision_reason_enum_round_trips_every_variant() {
        for &variant in PermissionDecisionReason::ALL {
            let wire = serde_json::to_value(variant).unwrap();
            let s = wire.as_str().expect("reason serializes to a string");
            assert_eq!(
                PermissionDecisionReason::try_from(s),
                Ok(variant),
                "reason {s} must round-trip"
            );
        }
        assert!(PermissionDecisionReason::try_from("not_a_reason").is_err());
    }

    /// Normalization behavior of a few representative raw outcomes (the KPI-
    /// relevant ones). The exhaustive owner-vocabulary coverage — every wire
    /// string the manager can emit maps — is enforced by the shell drift test
    /// against `PromptOutcomeKind::ALL` (this crate cannot depend on the owner).
    #[test]
    fn prompt_outcome_normalizes_representative_outcomes() {
        use PermissionPromptOutcome as O;
        assert_eq!(
            PermissionPromptOutcome::try_from("allow_once"),
            Ok(O::Allow)
        );
        assert_eq!(
            PermissionPromptOutcome::try_from("reject_once"),
            Ok(O::Reject)
        );
        assert_eq!(
            PermissionPromptOutcome::try_from("reject_always_mcp_tool"),
            Ok(O::Reject)
        );
        assert_eq!(
            PermissionPromptOutcome::try_from("reject_always_domain"),
            Ok(O::Reject)
        );
        assert_eq!(
            PermissionPromptOutcome::try_from("cancelled"),
            Ok(O::Cancel)
        );
        assert_eq!(
            PermissionPromptOutcome::try_from("followup"),
            Ok(O::Followup)
        );
        assert_eq!(PermissionPromptOutcome::try_from("error"), Ok(O::Error));
        assert!(PermissionPromptOutcome::try_from("mystery").is_err());
    }

    /// Enum↔wire round-trip for every detail variant. The cross-crate
    /// bijection lives in the shell drift test.
    #[test]
    fn prompt_outcome_detail_round_trips_every_variant() {
        for &variant in PermissionPromptOutcomeDetail::ALL {
            let wire = serde_json::to_value(variant).unwrap();
            let s = wire.as_str().expect("detail serializes to a string");
            assert_eq!(
                PermissionPromptOutcomeDetail::try_from(s),
                Ok(variant),
                "detail {s} must round-trip"
            );
        }
        assert!(PermissionPromptOutcomeDetail::try_from("mystery").is_err());
    }

    /// Enum↔wire self-consistency for the symmetric analytics enums: every
    /// variant serializes to a snake_case string that `TryFrom` maps back to the
    /// same variant. The cross-crate bijection against the workspace owner
    /// vocabularies lives in the shell drift tests.
    #[test]
    fn classifier_source_verdict_finding_enums_round_trip() {
        for &v in PermissionClassifierSource::ALL {
            let s = serde_json::to_value(v).unwrap();
            let s = s.as_str().unwrap();
            assert_eq!(PermissionClassifierSource::try_from(s), Ok(v), "{s}");
        }
        assert!(PermissionClassifierSource::try_from("nope").is_err());
        for &v in PermissionClassifierVerdict::ALL {
            let s = serde_json::to_value(v).unwrap();
            let s = s.as_str().unwrap();
            assert_eq!(PermissionClassifierVerdict::try_from(s), Ok(v), "{s}");
        }
        assert!(PermissionClassifierVerdict::try_from("nope").is_err());
        for &f in PermissionSecurityFinding::ALL {
            let s = serde_json::to_value(f).unwrap();
            let s = s.as_str().unwrap();
            assert_eq!(PermissionSecurityFinding::try_from(s), Ok(f), "{s}");
        }
        assert!(PermissionSecurityFinding::try_from("made_up").is_err());
    }

    fn kpi_payload(
        mode: PermissionMode,
        reason: Option<PermissionDecisionReason>,
        verdict: Option<PermissionClassifierVerdict>,
        outcome: Option<PermissionPromptOutcome>,
    ) -> PermissionDecisionPayload {
        PermissionDecisionPayload {
            tool_name: "run_terminal_cmd".into(),
            access_kind: AccessKind::Bash,
            decision: PermissionOutcome::Deny,
            wait_ms: 0,
            permission_mode: mode,
            source: None,
            subagent_session_id: None,
            subagent_type: None,
            manager_prompt_attempted: Some(true),
            prompt_outcome: outcome,
            prompt_outcome_detail: None,
            remember_tool_approvals: Some(true),
            decision_reason: reason,
            classifier_source: Some(PermissionClassifierSource::Llm),
            classifier_verdict: verdict,
            security_findings: Some(vec![PermissionSecurityFinding::DangerousCommand]),
            classifier_latency_ms: Some(10),
            auto_denials_consecutive: Some(3),
            auto_denials_total: Some(3),
        }
    }

    #[test]
    fn auto_denial_kpi_cohort_and_sides() {
        use AutoDenialKpi::*;
        use PermissionClassifierVerdict as V;
        use PermissionDecisionReason as R;
        use PermissionPromptOutcome as O;
        // In-cohort: human Reject = Alignment, human Allow = Disagreement.
        assert_eq!(
            auto_denial_kpi(&kpi_payload(
                PermissionMode::Auto,
                Some(R::AutoDenialLimit),
                Some(V::Block),
                Some(O::Reject)
            )),
            Some(Alignment)
        );
        assert_eq!(
            auto_denial_kpi(&kpi_payload(
                PermissionMode::Auto,
                Some(R::AutoDenialLimit),
                Some(V::Block),
                Some(O::Allow)
            )),
            Some(Disagreement)
        );
        // Excluded: cancel, wrong mode, wrong reason, wrong verdict, no outcome.
        for excluded in [
            kpi_payload(
                PermissionMode::Auto,
                Some(R::AutoDenialLimit),
                Some(V::Block),
                Some(O::Cancel),
            ),
            kpi_payload(
                PermissionMode::Ask,
                Some(R::AutoDenialLimit),
                Some(V::Block),
                Some(O::Reject),
            ),
            kpi_payload(
                PermissionMode::Auto,
                Some(R::AutoClassifierDeny),
                Some(V::Block),
                Some(O::Reject),
            ),
            kpi_payload(
                PermissionMode::Auto,
                Some(R::AutoDenialLimit),
                Some(V::Allow),
                Some(O::Reject),
            ),
            kpi_payload(
                PermissionMode::Auto,
                Some(R::AutoDenialLimit),
                Some(V::Block),
                None,
            ),
        ] {
            assert_eq!(auto_denial_kpi(&excluded), None);
        }
    }
    // NB: the four-event cohort denominator/rate smoke lives in the shell crate's
    // `permission_analytics_tests`, where it runs the full production
    // event→payload projection on real manager `PermissionEvent`s.
}
