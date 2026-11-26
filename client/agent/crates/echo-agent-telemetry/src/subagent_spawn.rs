//! Per-spawn phase timings for subagent session construction.
//!
//! A closed phase schema in the spirit of [`crate::startup::StartupPhase`]:
//! time anything else with a `tracing` span, or extend the enum deliberately.
//! Phases are recorded once per spawned child and reported on the
//! `subagent_completed` event; names follow the `echo_agent_code_subagent_spawn_*`
//! metric taxonomy.
#![deny(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Phases are hierarchical: `AgentBuild` and `ToolSetup` are measured inside
/// `SessionBootstrap`, so summing all phases double-counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentSpawnPhase {
    /// Time waiting for a concurrency slot before the run started.
    QueueWait,
    /// Preparing the spawn before the session exists: type resolution,
    /// worktree creation, context bootstrap, metadata persist.
    SpawnPrepare,
    /// Child session construction wall time: thread + runtime + actor build.
    SessionBootstrap,
    /// Agent construction inside the bootstrap (toolset + prompt render).
    AgentBuild,
    /// Post-build tool setup inside the bootstrap: resource seeding, context
    /// collection, workspace toolset bind.
    ToolSetup,
    /// Session ready to first child turn submitted.
    ReadyToFirstTurn,
}

/// Per-spawn phase recorder: cheap `Arc` handle, a fixed handful of mutex
/// pushes per spawn regardless of telemetry mode (sink gating is at emission).
#[derive(Debug, Default)]
pub struct SubagentSpawnTimer {
    phases: Mutex<Vec<(SubagentSpawnPhase, u64)>>,
}

pub type SharedSubagentSpawnTimer = Arc<SubagentSpawnTimer>;

impl SubagentSpawnTimer {
    pub fn new_shared() -> SharedSubagentSpawnTimer {
        Arc::new(Self::default())
    }

    /// Last write wins; each phase records once per spawn.
    pub fn record(&self, phase: SubagentSpawnPhase, elapsed: Duration) {
        let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let mut phases = self
            .phases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(slot) = phases.iter_mut().find(|(p, _)| *p == phase) {
            slot.1 = ms;
        } else {
            phases.push((phase, ms));
        }
    }

    #[cfg(test)]
    fn ms(&self, phase: SubagentSpawnPhase) -> Option<u64> {
        self.phases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(p, _)| *p == phase)
            .map(|(_, ms)| *ms)
    }

    /// Writes each recorded phase into its typed slot on `event`. The match in
    /// [`phase_event_slot`] is the single source of the phase→event mapping, so
    /// a new [`SubagentSpawnPhase`] variant fails compilation there until it is
    /// wired to an event field rather than silently dropping from the wire.
    pub fn write_event_phases(&self, event: &mut crate::events::SubagentCompleted) {
        for (phase, ms) in self
            .phases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            *phase_event_slot(event, *phase) = Some(*ms);
        }
    }
}

/// The single phase→event field mapping (see
/// [`SubagentSpawnTimer::write_event_phases`]). The typed `*_ms` fields are the
/// stable wire shape; adding a [`SubagentSpawnPhase`] variant fails to compile
/// here until it is given one.
fn phase_event_slot(
    event: &mut crate::events::SubagentCompleted,
    phase: SubagentSpawnPhase,
) -> &mut Option<u64> {
    match phase {
        SubagentSpawnPhase::QueueWait => &mut event.queue_wait_ms,
        SubagentSpawnPhase::SpawnPrepare => &mut event.spawn_prepare_ms,
        SubagentSpawnPhase::SessionBootstrap => &mut event.session_bootstrap_ms,
        SubagentSpawnPhase::AgentBuild => &mut event.agent_build_ms,
        SubagentSpawnPhase::ToolSetup => &mut event.tool_setup_ms,
        SubagentSpawnPhase::ReadyToFirstTurn => &mut event.ready_to_first_turn_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_is_last_write_wins_and_absent_reads_none() {
        let timer = SubagentSpawnTimer::default();
        assert_eq!(timer.ms(SubagentSpawnPhase::SpawnPrepare), None);
        timer.record(SubagentSpawnPhase::SpawnPrepare, Duration::from_millis(5));
        timer.record(SubagentSpawnPhase::SpawnPrepare, Duration::from_millis(9));
        assert_eq!(timer.ms(SubagentSpawnPhase::SpawnPrepare), Some(9));
        assert_eq!(timer.ms(SubagentSpawnPhase::AgentBuild), None);
    }
}
