//! Reminder policy — wraps echo-agent-tools reminder config.

/// Default per-prompt fire cap for the runtime turn-end TodoGate. Used
/// only as the default for `TodoGateConfig`; the runtime consumer reads
/// the live value from `ReminderPolicy.todo_gate.max_fires_per_prompt`,
/// so this constant is NOT a hardcoded cap.
pub const DEFAULT_TODO_GATE_MAX_FIRES: u32 = 2;

/// Session-level system reminder policy.
///
/// Controls whether system reminders are enabled and configures
/// the TodoNudge and TodoGate behavior.
#[derive(Debug, Clone)]
pub struct ReminderPolicy {
    /// Whether system reminders are enabled at all.
    pub enabled: bool,
    /// Configuration for the periodic TodoWrite nudge reminder.
    pub todo_nudge: TodoNudgeConfig,
    /// Configuration for the runtime turn-end TodoGate.
    pub todo_gate: TodoGateConfig,
}

impl Default for ReminderPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            todo_nudge: TodoNudgeConfig::default(),
            todo_gate: TodoGateConfig::default(),
        }
    }
}

/// Configuration for the TodoWrite nudge reminder.
///
/// The system will remind the model to use `todo_write` when it
/// hasn't done so within a configurable number of turns.
#[derive(Debug, Clone)]
pub struct TodoNudgeConfig {
    /// Whether the TodoNudge reminder is enabled.
    pub enabled: bool,
    /// Number of turns since last `todo_write` call before nudging.
    pub turns_since_todo_write: u32,
    /// Minimum turns between nudge reminders.
    pub turns_between_reminders: u32,
}

impl Default for TodoNudgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            turns_since_todo_write: 3,
            turns_between_reminders: 5,
        }
    }
}

/// Configuration for the runtime turn-end TodoGate.
///
/// The gate inspects `TodoState` after every content-only assistant
/// message and forces another turn via `<system-reminder>` injection
/// if pending/unbacked-in-progress todos remain — see
/// `echo-agent-runtime::session::acp_session::evaluate_todo_gate`.
///
/// **Disabled by default.** Operators opt in via the remote
/// `todo_gate_enabled = true` remote settings key, or via the
/// `--todo-gate` CLI flag (session-scoped force-enable, highest
/// precedence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TodoGateConfig {
    /// Whether the gate runs at all.
    pub enabled: bool,
    /// Hard cap on how many times the gate may fire per user prompt
    /// before the next turn is allowed to end with `TurnOutcome::Completed`.
    /// Bounds the worst-case extra inference cost.
    pub max_fires_per_prompt: u32,
}

impl Default for TodoGateConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_fires_per_prompt: DEFAULT_TODO_GATE_MAX_FIRES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_todo_gate_is_disabled_with_const_cap() {
        let cfg = TodoGateConfig::default();
        assert!(!cfg.enabled, "TodoGate must be opt-in");
        assert_eq!(cfg.max_fires_per_prompt, DEFAULT_TODO_GATE_MAX_FIRES);
        assert_eq!(DEFAULT_TODO_GATE_MAX_FIRES, 2);
    }

    #[test]
    fn reminder_policy_default_disables_gate_but_keeps_nudge_and_global_enabled() {
        let policy = ReminderPolicy::default();
        assert!(
            policy.enabled,
            "global system reminders stay enabled by default"
        );
        assert!(
            !policy.todo_gate.enabled,
            "TodoGate ships disabled; remote/local opt-in required"
        );
        assert_eq!(policy.todo_gate.max_fires_per_prompt, 2);
        // The two reminder mechanisms are independent — flipping one
        // must not change the other (regression guard).
        assert!(policy.todo_nudge.enabled);
    }

    #[test]
    fn todo_gate_enable_does_not_disturb_nudge() {
        // Remote opt-in (or `[reminder.todo_gate] enabled = true` local
        // config) flips the gate to on without touching the periodic
        // TodoNudge as a side-effect.
        let mut policy = ReminderPolicy::default();
        policy.todo_gate.enabled = true;
        assert!(policy.todo_gate.enabled);
        assert!(policy.todo_nudge.enabled, "TodoNudge must stay enabled");
        assert!(policy.enabled, "global enable must stay true");
    }
}
