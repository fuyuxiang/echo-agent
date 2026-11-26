use std::time::Duration;

use anyhow::{Result, bail};
use echo_agent_file_utils::storage_client::StorageClient;

use crate::agent::session_registry_client::{SessionRecord, SessionRegistryClient};

const UNAVAILABLE: &str = "Remote session restore is not available in this build";

#[derive(Debug)]
pub struct RestoreResult {
    pub session_id: String,
    pub local_session_id: String,
    pub codebase: CodebaseRestoreResult,
    pub memory: MemoryRestoreResult,
    pub session_state: SessionStateRestoreResult,
}

pub type ProgressCallback = Box<dyn Fn(&RestoreProgressEvent) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseStep {
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreProgressEvent {
    pub phase: RestorePhase,
    pub step: PhaseStep,
    pub incomplete: bool,
    pub message: String,
    pub detail: Option<String>,
    pub elapsed: Duration,
}

impl RestoreProgressEvent {
    pub fn display_line(&self) -> String {
        match self.detail.as_deref() {
            Some(detail) if !detail.is_empty() => {
                format!("{} — {}", self.message, detail)
            }
            _ => self.message.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestorePhase {
    SessionRecord,
    Download,
    Codebase,
    Memory,
    SessionState,
    Finalize,
}

#[derive(Debug)]
pub struct CodebaseRestoreResult {
    pub strategy: RestoreStrategy,
    pub commits_applied: usize,
    pub staged_applied: bool,
    pub unstaged_applied: bool,
    pub untracked_copied: usize,
    pub binary_copied: usize,
    pub warnings: Vec<String>,
    pub stash_created: bool,
}

impl CodebaseRestoreResult {
    pub fn skipped(reason: &str) -> Self {
        Self {
            strategy: RestoreStrategy::Skipped(reason.to_string()),
            commits_applied: 0,
            staged_applied: false,
            unstaged_applied: false,
            untracked_copied: 0,
            binary_copied: 0,
            warnings: Vec::new(),
            stash_created: false,
        }
    }
}

#[derive(Debug)]
pub enum RestoreStrategy {
    DirectCheckout,
    PatchReplay,
    Skipped(String),
}

impl RestoreStrategy {
    pub fn label(&self) -> &'static str {
        match self {
            Self::DirectCheckout => "direct_checkout",
            Self::PatchReplay => "patch_replay",
            Self::Skipped(_) => "skipped",
        }
    }
}

#[derive(Debug)]
pub struct MemoryRestoreResult {
    pub sessions_copied: usize,
}

impl MemoryRestoreResult {
    pub fn skipped(_reason: &str) -> Self {
        Self { sessions_copied: 0 }
    }
}

#[derive(Debug, Default)]
pub struct SessionStateRestoreResult {
    pub files_copied: u32,
    pub summary_restored: bool,
    pub updates_restored: bool,
}

impl SessionStateRestoreResult {
    pub fn skipped() -> Self {
        Self::default()
    }

    pub(crate) fn is_skipped(&self) -> bool {
        self.files_copied == 0
    }
}

pub(crate) async fn restore_session(
    _client: &SessionRegistryClient,
    _session_id: &str,
    _target_cwd: &str,
    _turn_override: Option<i32>,
) -> Result<RestoreResult> {
    bail!(UNAVAILABLE)
}

pub struct RestoreSessionOpts {
    pub turn_override: Option<i32>,
    pub progress: Option<ProgressCallback>,
    pub restore_code: bool,
}

impl RestoreSessionOpts {
    pub fn conversation_only() -> Self {
        Self {
            turn_override: None,
            progress: None,
            restore_code: false,
        }
    }
}

pub async fn restore_session_with_progress(
    _client: &SessionRegistryClient,
    _session_id: &str,
    _target_cwd: &str,
    _opts: RestoreSessionOpts,
) -> Result<RestoreResult> {
    bail!(UNAVAILABLE)
}

pub async fn restore_session_with_storage(
    _client: &SessionRegistryClient,
    _storage_client: &StorageClient,
    _session_id: &str,
    _target_cwd: &str,
    _opts: RestoreSessionOpts,
) -> Result<RestoreResult> {
    bail!(UNAVAILABLE)
}

pub(crate) fn resolve_restore_turn(record: &SessionRecord, turn_override: Option<i32>) -> i32 {
    turn_override.unwrap_or_else(|| {
        record
            .restorable_turn_number
            .unwrap_or(record.last_turn_number)
    })
}

pub(crate) async fn download_to_tempfile(
    _client: &SessionRegistryClient,
    _session_id: &str,
    _filename: &str,
    _turn: i32,
) -> anyhow::Result<tempfile::NamedTempFile> {
    bail!(UNAVAILABLE)
}

pub(crate) async fn apply_memory_download(
    _download: anyhow::Result<tempfile::NamedTempFile>,
    _target_cwd: &str,
) -> MemoryRestoreResult {
    MemoryRestoreResult::skipped(UNAVAILABLE)
}

pub(crate) async fn apply_session_state_download(
    _download: anyhow::Result<tempfile::NamedTempFile>,
    _session_id: &str,
    _target_cwd: &str,
) -> (SessionStateRestoreResult, String) {
    (SessionStateRestoreResult::skipped(), String::new())
}

pub fn format_session_line(r: &SessionRecord) -> String {
    r.session_id.clone()
}

pub fn format_search_results(sessions: &[SessionRecord]) -> String {
    if sessions.is_empty() {
        "No sessions found.".to_string()
    } else {
        sessions
            .iter()
            .map(format_session_line)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub(crate) async fn apply_session_state_in_place(
    _download: anyhow::Result<tempfile::NamedTempFile>,
    _session_id: &str,
    _target_cwd: &str,
) -> SessionStateRestoreResult {
    SessionStateRestoreResult::skipped()
}
