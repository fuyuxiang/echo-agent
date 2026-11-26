//! Managed-policy preflight for one permission request.
//!
//! Evaluates the direct rule pass and both bash security gates once and keeps
//! each gate's `Ask` provenance, so the manager can tell a rule-match Ask (an
//! actual policy match — stays a prompt) from a fail-closed Ask (analysis
//! could not decompose the command to check rules). In auto mode a fail-closed
//! Ask defers to the classifier; the manager consumes this single result
//! instead of correlating parallel booleans at every decision site.

use std::path::Path;

use crate::permission::manager::reasons;
use crate::permission::policy::{CompiledPolicy, GateDecision};
use crate::permission::shell_access::combine_decisions;
use crate::permission::types::{AccessKind, Decision};

/// One request's managed-policy evaluation, computed before any fast path.
pub(crate) struct GatePreflight {
    direct: Option<Decision>,
    bash_command: Option<GateDecision>,
    shell_file: Option<GateDecision>,
    /// Auto mode + a fail-closed gate Ask with no rule match: the classifier
    /// may run (Allow executes, Block denies within budget). A rule-match Ask
    /// never defers.
    defers_gate_ask: bool,
}

impl GatePreflight {
    /// `cwd` is the requesting session's execution cwd (not necessarily the
    /// manager's): path rules and shell-file operands anchor to it.
    pub(crate) fn evaluate(
        policy: Option<&CompiledPolicy>,
        access: &AccessKind,
        cwd: &Path,
        auto_mode: bool,
    ) -> Self {
        let direct = policy.and_then(|policy| policy.evaluate_with_cwd(access, Some(cwd)));
        let (bash_command, shell_file) = match (policy, access) {
            (Some(policy), AccessKind::Bash(cmd)) => (
                policy.evaluate_bash_command_gate(cmd),
                policy.evaluate_shell_file_access_gate(cmd, cwd),
            ),
            _ => (None, None),
        };
        let rule_match_ask = matches!(direct, Some(Decision::Ask))
            || matches!(bash_command, Some(GateDecision::AskRuleMatch))
            || matches!(shell_file, Some(GateDecision::AskRuleMatch));
        let fail_closed_ask = matches!(bash_command, Some(GateDecision::AskFailClosed))
            || matches!(shell_file, Some(GateDecision::AskFailClosed));
        // WHY: a fail-closed Ask means analysis could not decompose the command
        // to check rules, so the classifier arbitrates it; a rule-match Ask is
        // an actual policy match that stays a prompt (never waived by a model).
        let defers_gate_ask = auto_mode && fail_closed_ask && !rule_match_ask;
        Self {
            direct,
            bash_command,
            shell_file,
            defers_gate_ask,
        }
    }

    /// Combined managed decision (deny > ask > allow), as the manager applied
    /// it before provenance existed.
    pub(crate) fn policy_decision(&self) -> Option<Decision> {
        let bash_command = self.bash_command.clone().map(GateDecision::into_decision);
        let shell_file = self.shell_file.clone().map(GateDecision::into_decision);
        combine_decisions(
            combine_decisions(self.direct.clone(), bash_command),
            shell_file,
        )
    }

    pub(crate) fn policy_forced_prompt(&self) -> bool {
        matches!(self.policy_decision(), Some(Decision::Ask))
    }

    /// An `Ask` from either bash gate; blocks the YOLO fast path.
    pub(crate) fn shell_forced_prompt(&self) -> bool {
        self.bash_command.as_ref().is_some_and(GateDecision::is_ask)
            || self.shell_file_forced_prompt()
    }

    /// Blocks bash grants from satisfying a Read/Edit ask escalated from
    /// shell-file access.
    pub(crate) fn shell_file_forced_prompt(&self) -> bool {
        self.shell_file.as_ref().is_some_and(GateDecision::is_ask)
    }

    /// Whether the auto classifier may run despite a gate Ask: no Ask at all,
    /// or a fail-closed Ask that defers.
    pub(crate) fn admits_auto_classifier(&self) -> bool {
        !self.policy_forced_prompt() || self.defers_gate_ask()
    }

    /// Deferral is active: fail-closed Ask may be classified; Block denies
    /// within budget (not prompt-binding, not budget-exempt).
    pub(crate) fn defers_gate_ask(&self) -> bool {
        self.defers_gate_ask
    }

    /// The gate-owned prompt trigger for telemetry, or `None` when a bash floor
    /// or plain needs-user forced the prompt. Rule-match Asks keep their gate
    /// label; a deferrable Ask does not.
    pub(crate) fn prompt_trigger(
        &self,
        auto_prompt_reason: Option<&'static str>,
    ) -> Option<&'static str> {
        if matches!(self.direct, Some(Decision::Ask)) {
            return Some(reasons::POLICY_ASK);
        }
        // WHY: a preempting request floor owns the reason, so a deferrable Ask
        // whose classifier a floor blocked (`auto_prompt_reason` None) yields it.
        if self.defers_gate_ask() {
            return auto_prompt_reason;
        }
        if self.bash_command.as_ref().is_some_and(GateDecision::is_ask) {
            return Some(reasons::BASH_COMMAND_GATE_ASK);
        }
        if self.shell_file_forced_prompt() {
            return Some(reasons::SHELL_FILE_GATE_ASK);
        }
        auto_prompt_reason
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::types::{
        PatternMode, PermissionConfig, PermissionRule, RuleAction, ToolFilter,
    };

    fn bash_rule(action: RuleAction, pattern: &str) -> PermissionRule {
        PermissionRule {
            action,
            tool: ToolFilter::Bash,
            pattern: Some(pattern.to_owned()),
            pattern_mode: PatternMode::Glob,
        }
    }

    fn policy() -> CompiledPolicy {
        CompiledPolicy::new(PermissionConfig::new(vec![
            bash_rule(RuleAction::Deny, "rm -rf *"),
            bash_rule(RuleAction::Ask, "git push*"),
        ]))
    }

    #[test]
    fn preflight_reports_gate_state_coherently() {
        let policy = policy();
        let cwd = Path::new("/work");
        let bash = |cmd: &str| AccessKind::Bash(cmd.to_owned());

        // Fail-closed Ask in auto: classified; Block denies within budget; trigger follows classifier outcome.
        let deferred = GatePreflight::evaluate(Some(&policy), &bash("echo \"$(date)\""), cwd, true);
        assert!(deferred.policy_forced_prompt());
        assert!(deferred.admits_auto_classifier());
        assert!(deferred.defers_gate_ask());
        assert_eq!(
            deferred.prompt_trigger(Some(reasons::AUTO_CLASSIFIER_DENY)),
            Some(reasons::AUTO_CLASSIFIER_DENY)
        );

        // Same request outside auto mode: nothing admits the classifier and
        // the gate label is the trigger.
        let ask_mode =
            GatePreflight::evaluate(Some(&policy), &bash("echo \"$(date)\""), cwd, false);
        assert!(ask_mode.policy_forced_prompt());
        assert!(!ask_mode.admits_auto_classifier());
        assert!(!ask_mode.defers_gate_ask());
        assert_eq!(
            ask_mode.prompt_trigger(None),
            Some(reasons::BASH_COMMAND_GATE_ASK)
        );

        // Rule-match Ask in auto mode stays binding with its gate label — a
        // rule match never defers, even alongside a fail-closed floor.
        let rule_match = GatePreflight::evaluate(
            Some(&policy),
            &bash("echo hi && git push origin main"),
            cwd,
            true,
        );
        assert!(!rule_match.admits_auto_classifier());
        assert!(!rule_match.defers_gate_ask());
        assert_eq!(
            rule_match.prompt_trigger(None),
            Some(reasons::BASH_COMMAND_GATE_ASK)
        );

        // No policy at all: inert preflight.
        let inert = GatePreflight::evaluate(None, &bash("echo hi"), cwd, true);
        assert!(inert.policy_decision().is_none());
        assert!(inert.admits_auto_classifier());
        assert!(!inert.defers_gate_ask());
        assert_eq!(inert.prompt_trigger(None), None);
    }
}
