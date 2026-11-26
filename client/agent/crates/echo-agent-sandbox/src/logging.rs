//! Sandbox event logger.
//!
//! Records sandbox events (profile applied, violations, bypasses) for
//! telemetry and debugging. Events are kept in memory and can be flushed
//! to a JSONL file at `~/.echo-agent/sandbox-events.jsonl`.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::types::{SandboxEvent, SandboxEventType, SandboxMetrics};

/// Logger that collects sandbox events and maintains violation counters.
pub struct SandboxLogger {
    events: Mutex<Vec<SandboxEvent>>,
    metrics: SandboxMetrics,
}

impl SandboxLogger {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            metrics: SandboxMetrics::default(),
        }
    }

    /// Record an event, updating metrics counters as appropriate.
    pub fn log(&self, event: SandboxEvent) {
        match &event.event_type {
            SandboxEventType::FsViolation => self.metrics.inc_fs_violation(),
            SandboxEventType::NetViolation => self.metrics.inc_net_violation(),
            SandboxEventType::BypassGranted => self.metrics.inc_bypass_granted(),
            SandboxEventType::BypassDenied => self.metrics.inc_bypass_denied(),
            _ => {}
        }

        tracing::debug!(
            event_type = ?event.event_type,
            profile = %event.profile,
            target = ?event.target,
            operation = ?event.operation,
            "sandbox event"
        );

        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }

    /// Get a reference to the metrics counters.
    pub fn metrics(&self) -> &SandboxMetrics {
        &self.metrics
    }

    /// Take all accumulated events, draining the internal buffer.
    pub fn take_events(&self) -> Vec<SandboxEvent> {
        self.events
            .lock()
            .map(|mut events| std::mem::take(&mut *events))
            .unwrap_or_default()
    }

    /// Flush accumulated events to the JSONL log file.
    /// Each event is written as a single JSON line.
    pub fn flush_to_disk(&self) -> anyhow::Result<()> {
        let events = self.take_events();
        if events.is_empty() {
            return Ok(());
        }

        let log_path = Self::log_file_path();
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;

        for event in &events {
            if let Ok(json) = serde_json::to_string(event) {
                writeln!(file, "{}", json)?;
            }
        }

        tracing::debug!(
            path = %log_path.display(),
            count = events.len(),
            "flushed sandbox events to disk"
        );

        Ok(())
    }

    fn log_file_path() -> PathBuf {
        echo_agent_config::echo_agent_home().join("sandbox-events.jsonl")
    }
}

impl Default for SandboxLogger {
    fn default() -> Self {
        Self::new()
    }
}
