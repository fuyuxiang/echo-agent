//! Durable, source-retaining relocation of a dormant session directory.
//!
//! The journal phase is the authority boundary: source wins through
//! `TargetPublished`; target wins from `Ready` onward.

mod fs;
mod journal;
mod view;

use std::fs as std_fs;
use std::io;
use std::path::{Path, PathBuf};

use self::journal::{AtomicWriteFault, WriteFailure};
pub(crate) use self::journal::{RelocationJournal, RelocationLease, RelocationPhase};
pub(crate) use self::view::RelocationView;
use crate::session::persistence::{PendingCwdSwitchReminder, Summary};

/// True when `~/.echo-agent/relocations/<session_id>.json` exists.
///
/// Replay's parent-cwd/sibling fast path must not trust a found `updates.jsonl`
/// while a journal is present: source and target dirs can both exist, and the
/// journal phase is the only authority.
pub(crate) fn has_relocation_journal(echo_agent_home: &Path, session_id: &str) -> bool {
    journal::journal_path(echo_agent_home, session_id).is_file()
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RelocationError {
    #[error("invalid relocation {field}: {value:?}")]
    InvalidComponent { field: &'static str, value: String },
    #[error("session {0} already has an active relocation lease")]
    LeaseBusy(String),
    #[error("relocation journal already exists for session {0}")]
    JournalExists(String),
    #[error("relocation journal is missing for session {0}")]
    JournalMissing(String),
    #[error("relocation phase {actual:?} does not permit {operation}")]
    InvalidPhase {
        operation: &'static str,
        actual: RelocationPhase,
    },
    #[error("relocation transaction identity does not match the current journal")]
    TransactionMismatch,
    #[error("relocation failed and was rolled back: {source}")]
    RolledBack {
        #[source]
        source: Box<RelocationError>,
        terminal: TerminalRelocation,
    },
    #[error("relocation collision at {0}")]
    Collision(PathBuf),
    #[error("relocation state is inconsistent: {0}")]
    Inconsistent(String),
    #[error("atomic no-replace publication is unsupported on this platform or filesystem")]
    UnsupportedPublication,
    #[error("relocation requires recovery from persisted phase {phase:?}: {source}")]
    RecoveryRequired {
        phase: RelocationPhase,
        #[source]
        source: Box<RelocationError>,
    },
    #[error("relocation failed ({source}) and rollback also failed ({rollback})")]
    RollbackFailed {
        source: Box<RelocationError>,
        rollback: Box<RelocationError>,
    },
    #[error("{operation} {path}: {source}", path = path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode {path}: {source}", path = path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub(crate) type Result<T> = std::result::Result<T, RelocationError>;

#[derive(Debug, Clone)]
pub(crate) struct RelocationRequest {
    pub(crate) session_id: String,
    pub(crate) nonce: String,
    pub(crate) source_cwd: String,
    pub(crate) target_cwd: String,
    pub(crate) cwd_generation: u64,
    pub(crate) pending_reminder: PendingCwdSwitchReminder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryAction {
    RollBackToSource,
    CommitTarget,
    VerifyCommitted,
    VerifyRolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelocationAuthority {
    pub(crate) session_id: String,
    pub(crate) cwd: String,
    pub(crate) phase: RelocationPhase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedRelocation {
    session_id: String,
    nonce: String,
    cwd_generation: u64,
}

impl StagedRelocation {
    fn matches(&self, journal: &RelocationJournal) -> bool {
        self.session_id == journal.session_id
            && self.nonce == journal.nonce
            && self.cwd_generation == journal.cwd_generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerminalRelocation {
    journal: RelocationJournal,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestFault {
    Journal(RelocationPhase, AtomicWriteFault),
    RemoveBarrier,
    NamespaceBarrier,
    ReadyAfterRenameThenNamespaceBarrier,
    CwdMarker(AtomicWriteFault),
}

#[derive(Debug, Clone)]
pub(crate) struct RelocationStorage {
    echo_agent_home: PathBuf,
    #[cfg(test)]
    fault: Option<TestFault>,
}

impl RelocationStorage {
    pub(crate) fn new(echo_agent_home: PathBuf) -> Self {
        Self {
            echo_agent_home,
            #[cfg(test)]
            fault: None,
        }
    }

    #[cfg(test)]
    fn with_fault(echo_agent_home: PathBuf, fault: TestFault) -> Self {
        Self {
            echo_agent_home,
            fault: Some(fault),
        }
    }

    pub(crate) fn acquire(&self, session_id: &str) -> Result<RelocationLease> {
        journal::acquire(&self.echo_agent_home, session_id)
    }

    pub(crate) fn read_journal(&self, session_id: &str) -> Result<RelocationJournal> {
        journal::read(&self.echo_agent_home, session_id)
    }

    fn write_journal(&self, journal: &RelocationJournal) -> std::result::Result<(), WriteFailure> {
        journal::write(
            &self.echo_agent_home,
            journal,
            self.journal_fault(journal.phase),
        )
    }

    pub(crate) fn sync_journal_namespace(&self) -> Result<()> {
        #[cfg(test)]
        if matches!(
            self.fault,
            Some(TestFault::NamespaceBarrier | TestFault::ReadyAfterRenameThenNamespaceBarrier)
        ) {
            return Err(RelocationError::Inconsistent(
                "injected relocation namespace barrier failure".into(),
            ));
        }
        journal::sync_namespace(&self.echo_agent_home)
    }

    pub(crate) fn copy_directory(&self, source: &Path, target: &Path) -> Result<()> {
        fs::copy_directory(source, target)
    }

    pub(crate) fn create_directory(&self, path: &Path) -> Result<()> {
        fs::create_dir_durable(path)
    }

    pub(crate) fn remove_directory(&self, path: &Path) -> Result<()> {
        fs::remove_dir_durable(path)
    }

    pub(crate) fn publish_no_replace(&self, source: &Path, target: &Path) -> Result<()> {
        fs::rename_no_replace(source, target)
    }

    pub(crate) fn stage_and_publish(
        &self,
        lease: &RelocationLease,
        request: RelocationRequest,
    ) -> Result<StagedRelocation> {
        self.validate_request(lease, &request)?;
        let mut journal = RelocationJournal::new(
            request.session_id,
            request.nonce,
            request.source_cwd,
            request.target_cwd,
            request.cwd_generation,
        );
        journal.validate(&self.echo_agent_home)?;
        let source = journal::session_dir_at(
            &self.echo_agent_home,
            &journal.source_cwd,
            &journal.session_id,
        );
        self.validate_source_summary(&journal)?;
        match self.read_journal(&journal.session_id) {
            Err(RelocationError::JournalMissing(_)) => {}
            Ok(_) => return Err(RelocationError::JournalExists(journal.session_id)),
            Err(error) => return Err(error),
        }

        let target_parent = self.target_parent(&journal.target_cwd);
        let target = target_parent.join(&journal.session_id);
        let staging = target_parent.join(staging_name(&journal.session_id, &journal.nonce));
        reject_existing(&target)?;
        reject_existing(&staging)?;
        self.ensure_target_parent(&journal.target_cwd)?;
        match self.write_journal(&journal) {
            Ok(()) => {}
            Err(WriteFailure::NotCommitted(error)) => return Err(error),
            Err(WriteFailure::Committed(error)) => {
                return Err(recovery_required(RelocationPhase::Prepared, error));
            }
        }

        let staged = fs::copy_directory(&source, &staging)
            .and_then(|()| rewrite_staged_summary(&staging, &journal, request.pending_reminder))
            .and_then(|()| {
                fs::sync_dir(&target_parent).map_err(|e| fs::io_error("sync", &target_parent, e))
            });
        if let Err(error) = staged {
            return self.rollback_failed_stage(journal, error);
        }
        journal.phase = RelocationPhase::Staged;
        match self.write_journal(&journal) {
            Ok(()) => {}
            Err(WriteFailure::NotCommitted(error)) => {
                journal.phase = RelocationPhase::Prepared;
                return self.rollback_failed_stage(journal, error);
            }
            Err(WriteFailure::Committed(error)) => {
                return Err(recovery_required(RelocationPhase::Staged, error));
            }
        }
        if let Err(error) = fs::rename_no_replace(&staging, &target) {
            return self.rollback_failed_stage(journal, error);
        }
        fs::sync_dir(&target_parent)
            .map_err(|e| fs::io_error("sync", &target_parent, e))
            .map_err(|error| recovery_required(RelocationPhase::Staged, error))?;

        journal.phase = RelocationPhase::TargetPublished;
        self.write_transition(&journal, RelocationPhase::Staged)?;
        Ok(StagedRelocation {
            session_id: journal.session_id,
            nonce: journal.nonce,
            cwd_generation: journal.cwd_generation,
        })
    }

    pub(crate) fn mark_ready_and_commit(
        &self,
        lease: &RelocationLease,
        transaction: &StagedRelocation,
    ) -> Result<TerminalRelocation> {
        let mut journal = self.load_transaction(lease, transaction)?;
        match journal.phase {
            RelocationPhase::TargetPublished => {
                self.validate_target(&journal)
                    .map_err(|error| recovery_required(RelocationPhase::TargetPublished, error))?;
                journal.phase = RelocationPhase::Ready;
                self.write_transition(&journal, RelocationPhase::TargetPublished)?;
            }
            RelocationPhase::Ready | RelocationPhase::Committed => {}
            actual => {
                return Err(RelocationError::InvalidPhase {
                    operation: "commit",
                    actual,
                });
            }
        }
        self.finish_commit(&mut journal)
    }

    pub(crate) fn rollback(
        &self,
        lease: &RelocationLease,
        transaction: &StagedRelocation,
    ) -> Result<TerminalRelocation> {
        let mut journal = self.load_transaction(lease, transaction)?;
        if has_target_authority(journal.phase) {
            return Err(RelocationError::InvalidPhase {
                operation: "rollback",
                actual: journal.phase,
            });
        }
        self.finish_rollback(&mut journal)
    }

    pub(crate) fn recover(
        &self,
        lease: &RelocationLease,
    ) -> Result<(RecoveryAction, TerminalRelocation)> {
        let mut journal = self.load_for_lease(lease)?;
        self.sync_journal_namespace()
            .map_err(|error| recovery_required(journal.phase, error))?;
        let action = recovery_action(journal.phase);
        let terminal = match action {
            RecoveryAction::RollBackToSource | RecoveryAction::VerifyRolledBack => {
                self.finish_rollback(&mut journal)?
            }
            RecoveryAction::CommitTarget | RecoveryAction::VerifyCommitted => {
                self.finish_commit(&mut journal)?
            }
        };
        Ok((action, terminal))
    }

    pub(crate) fn recover_all(&self) -> Result<()> {
        for session_id in RelocationView::journal_ids(&self.echo_agent_home)? {
            let lease = match self.acquire(&session_id) {
                Ok(lease) => lease,
                Err(RelocationError::LeaseBusy(_)) => continue,
                Err(error) => return Err(error),
            };
            let (_, terminal) = self.recover(&lease)?;
            self.finalize_terminal(&lease, &terminal)?;
        }
        Ok(())
    }

    pub(crate) fn recover_if_pending(&self, session_id: &str) -> Result<()> {
        match self.read_journal(session_id) {
            Err(RelocationError::JournalMissing(_)) => return Ok(()),
            result => { result?; }
        }
        let lease = self.acquire(session_id)?;
        // Another process can finish recovery between the first read and the lease.
        match self.recover(&lease) {
            Ok((_, terminal)) => self.finalize_terminal(&lease, &terminal),
            Err(RelocationError::JournalMissing(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn authority(&self, session_id: &str) -> Result<RelocationAuthority> {
        let journal = self.read_journal(session_id)?;
        let cwd = if has_target_authority(journal.phase) {
            journal.target_cwd
        } else {
            journal.source_cwd
        };
        Ok(RelocationAuthority {
            session_id: journal.session_id,
            cwd,
            phase: journal.phase,
        })
    }

    pub(crate) fn finalize_terminal(
        &self,
        lease: &RelocationLease,
        proof: &TerminalRelocation,
    ) -> Result<()> {
        proof.journal.validate(&self.echo_agent_home)?;
        if proof.journal.session_id != lease.session_id
            || !matches!(
                proof.journal.phase,
                RelocationPhase::Committed | RelocationPhase::RolledBack
            )
        {
            return Err(RelocationError::Inconsistent(
                "terminal proof does not match the held lease".into(),
            ));
        }
        match self.read_journal(&lease.session_id) {
            Ok(journal) if journal == proof.journal => {}
            Ok(_) => {
                return Err(RelocationError::Inconsistent(
                    "terminal proof does not match the current journal".into(),
                ));
            }
            Err(RelocationError::JournalMissing(_)) => {
                return self.sync_journal_namespace();
            }
            Err(error) => return Err(error),
        }
        let path = journal::journal_path(&self.echo_agent_home, &lease.session_id);
        std_fs::remove_file(&path).map_err(|e| fs::io_error("remove", &path, e))?;
        self.sync_journal_namespace()
    }

    fn rollback_failed_stage<T>(
        &self,
        mut journal: RelocationJournal,
        source: RelocationError,
    ) -> Result<T> {
        match self.finish_rollback(&mut journal) {
            Ok(terminal) => Err(RelocationError::RolledBack {
                source: Box::new(source),
                terminal,
            }),
            Err(rollback) => {
                let phase = match &rollback {
                    RelocationError::RecoveryRequired { phase, .. } => *phase,
                    _ => journal.phase,
                };
                Err(recovery_required(
                    phase,
                    RelocationError::RollbackFailed {
                        source: Box::new(source),
                        rollback: Box::new(rollback),
                    },
                ))
            }
        }
    }

    fn finish_commit(&self, journal: &mut RelocationJournal) -> Result<TerminalRelocation> {
        let phase = journal.phase;
        let result = (|| {
            journal.validate(&self.echo_agent_home)?;
            self.sync_journal_namespace()?;
            self.validate_target(journal)?;
            let source = journal::session_dir_at(
                &self.echo_agent_home,
                &journal.source_cwd,
                &journal.session_id,
            );
            self.remove_transaction_directory(&source)?;
            self.remove_transaction_directory(&self.staging_dir(journal))?;
            if journal.phase != RelocationPhase::Committed {
                journal.phase = RelocationPhase::Committed;
                self.write_transition(journal, phase)?;
            }
            Ok(TerminalRelocation {
                journal: journal.clone(),
            })
        })();
        result.map_err(|error| recovery_required(journal.phase, error))
    }

    fn finish_rollback(&self, journal: &mut RelocationJournal) -> Result<TerminalRelocation> {
        let phase = journal.phase;
        let result = (|| {
            self.validate_source_summary(journal)?;
            self.remove_transaction_directory(&self.staging_dir(journal))?;
            let target = journal::session_dir_at(
                &self.echo_agent_home,
                &journal.target_cwd,
                &journal.session_id,
            );
            if target.exists() {
                self.validate_target(journal)?;
            }
            self.remove_transaction_directory(&target)?;
            if journal.phase != RelocationPhase::RolledBack {
                journal.phase = RelocationPhase::RolledBack;
                self.write_transition(journal, phase)?;
            }
            Ok(TerminalRelocation {
                journal: journal.clone(),
            })
        })();
        result.map_err(|error| recovery_required(journal.phase, error))
    }

    fn write_transition(
        &self,
        journal: &RelocationJournal,
        previous: RelocationPhase,
    ) -> Result<()> {
        match self.write_journal(journal) {
            Ok(()) => Ok(()),
            Err(WriteFailure::NotCommitted(error)) => Err(recovery_required(previous, error)),
            Err(WriteFailure::Committed(error)) => Err(recovery_required(journal.phase, error)),
        }
    }

    fn validate_request(&self, lease: &RelocationLease, request: &RelocationRequest) -> Result<()> {
        journal::validate_component("session id", &request.session_id)?;
        journal::validate_component("nonce", &request.nonce)?;
        journal::validate_cwd("source cwd", &request.source_cwd)?;
        journal::validate_cwd("target cwd", &request.target_cwd)?;
        if lease.session_id != request.session_id {
            return Err(RelocationError::Inconsistent(
                "lease belongs to another session".into(),
            ));
        }
        if journal::session_dir_at(
            &self.echo_agent_home,
            &request.source_cwd,
            &request.session_id,
        ) == journal::session_dir_at(
            &self.echo_agent_home,
            &request.target_cwd,
            &request.session_id,
        ) {
            return Err(RelocationError::Inconsistent(
                "source and target storage paths are identical".into(),
            ));
        }
        let reminder = &request.pending_reminder;
        if request.cwd_generation == 0
            || reminder.cwd_generation != request.cwd_generation
            || reminder.previous_cwd != request.source_cwd
            || reminder.destination_cwd != request.target_cwd
        {
            return Err(RelocationError::Inconsistent(
                "pending reminder does not match relocation request".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn validate_authoritative_dir(
        &self,
        journal: &RelocationJournal,
        path: &Path,
    ) -> Result<()> {
        if has_target_authority(journal.phase) {
            self.validate_target(journal)?;
        } else {
            self.validate_source_summary(journal)?;
        }
        let expected_cwd = if has_target_authority(journal.phase) {
            &journal.target_cwd
        } else {
            &journal.source_cwd
        };
        if path
            != journal::session_dir_at(&self.echo_agent_home, expected_cwd, &journal.session_id)
                .as_path()
        {
            return Err(RelocationError::Inconsistent(
                "authoritative session path does not match journal".into(),
            ));
        }
        Ok(())
    }

    fn validate_source_summary(&self, journal: &RelocationJournal) -> Result<()> {
        let source = journal::session_dir_at(
            &self.echo_agent_home,
            &journal.source_cwd,
            &journal.session_id,
        );
        fs::require_directory(&source)?;
        let path = source.join(super::SUMMARY_FILE);
        require_regular_file(&path)?;
        let summary = read_summary(&path)?;
        let expected_generation = summary
            .cwd_generation
            .checked_add(1)
            .ok_or_else(|| RelocationError::Inconsistent("cwd generation overflow".into()))?;
        if summary.info.id.to_string() != journal.session_id
            || summary.info.cwd != journal.source_cwd
            || expected_generation != journal.cwd_generation
        {
            return Err(RelocationError::Inconsistent(
                "source summary identity, cwd, or generation does not match request".into(),
            ));
        }
        Ok(())
    }

    fn validate_target(&self, journal: &RelocationJournal) -> Result<()> {
        let target = journal::session_dir_at(
            &self.echo_agent_home,
            &journal.target_cwd,
            &journal.session_id,
        );
        fs::require_directory(&target)?;
        let summary_path = target.join(super::SUMMARY_FILE);
        require_regular_file(&summary_path)?;
        let summary = read_summary(&summary_path)?;
        let pending_matches = summary
            .pending_cwd_switch_reminder
            .as_ref()
            .is_some_and(|pending| {
                pending.cwd_generation == journal.cwd_generation
                    && pending.previous_cwd == journal.source_cwd
                    && pending.destination_cwd == journal.target_cwd
            });
        let reminder_committed = summary.pending_cwd_switch_reminder.is_none()
            && summary.cwd_switch_bookkeeping_generation >= journal.cwd_generation;
        if summary.info.id.to_string() != journal.session_id
            || summary.info.cwd != journal.target_cwd
            || summary.cwd_generation != journal.cwd_generation
            || summary.previous_cwd.as_deref() != Some(journal.source_cwd.as_str())
            || (!pending_matches && !reminder_committed)
        {
            return Err(RelocationError::Inconsistent(format!(
                "target summary does not match journal: {}",
                target.display()
            )));
        }
        Ok(())
    }

    fn target_parent(&self, cwd: &str) -> PathBuf {
        self.echo_agent_home
            .join("sessions")
            .join(echo_agent_config::encode_cwd_dirname(cwd))
    }

    fn ensure_target_parent(&self, cwd: &str) -> Result<PathBuf> {
        let sessions = self.echo_agent_home.join("sessions");
        fs::create_dir_durable(&sessions)?;
        echo_agent_config::set_dir_owner_only(&sessions);
        let encoded = echo_agent_config::encode_cwd_dirname(cwd);
        let dir = self.target_parent(cwd);
        fs::create_dir_durable(&dir)?;
        // Owner-only root + parent shield the dirs beneath (the staged copy
        // preserves source modes).
        echo_agent_config::set_dir_owner_only(&dir);
        if encoded != urlencoding::encode(cwd).as_ref() {
            let path = dir.join(".cwd");
            match std_fs::read_to_string(&path) {
                Ok(existing) if existing == cwd => {
                    fs::sync_dir(&dir).map_err(|e| fs::io_error("sync", &dir, e))?;
                    return Ok(dir);
                }
                Ok(_) => {
                    return Err(RelocationError::Inconsistent(
                        "cwd metadata collision".into(),
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(fs::io_error("read", &path, error)),
            }
            match fs::write_new_durable(&path, cwd.as_bytes(), self.cwd_marker_fault()) {
                Ok(()) => {}
                Err(WriteFailure::NotCommitted(error)) => return Err(error),
                Err(WriteFailure::Committed(error)) => {
                    if std_fs::read_to_string(&path).map_err(|e| fs::io_error("read", &path, e))?
                        == cwd
                    {
                        return Err(error);
                    }
                    return Err(RelocationError::Inconsistent(
                        "cwd metadata collision".into(),
                    ));
                }
            }
        }
        Ok(dir)
    }

    fn staging_dir(&self, journal: &RelocationJournal) -> PathBuf {
        self.echo_agent_home
            .join("sessions")
            .join(echo_agent_config::encode_cwd_dirname(&journal.target_cwd))
            .join(staging_name(&journal.session_id, &journal.nonce))
    }

    fn load_for_lease(&self, lease: &RelocationLease) -> Result<RelocationJournal> {
        self.read_journal(&lease.session_id)
    }

    fn load_transaction(
        &self,
        lease: &RelocationLease,
        transaction: &StagedRelocation,
    ) -> Result<RelocationJournal> {
        let journal = self.load_for_lease(lease)?;
        if !transaction.matches(&journal) {
            return Err(RelocationError::TransactionMismatch);
        }
        Ok(journal)
    }

    fn remove_transaction_directory(&self, path: &Path) -> Result<()> {
        #[cfg(test)]
        if self.fault == Some(TestFault::RemoveBarrier) {
            return fs::remove_dir_with_barrier_fault(path);
        }
        fs::remove_dir_durable(path)
    }

    #[cfg(test)]
    fn journal_fault(&self, phase: RelocationPhase) -> Option<AtomicWriteFault> {
        match self.fault {
            Some(TestFault::Journal(fault_phase, fault)) if fault_phase == phase => Some(fault),
            Some(TestFault::ReadyAfterRenameThenNamespaceBarrier)
                if phase == RelocationPhase::Ready =>
            {
                Some(AtomicWriteFault::AfterRename)
            }
            _ => None,
        }
    }

    #[cfg(not(test))]
    fn journal_fault(&self, _phase: RelocationPhase) -> Option<AtomicWriteFault> {
        None
    }

    #[cfg(test)]
    fn cwd_marker_fault(&self) -> Option<AtomicWriteFault> {
        match self.fault {
            Some(TestFault::CwdMarker(fault)) => Some(fault),
            _ => None,
        }
    }

    #[cfg(not(test))]
    fn cwd_marker_fault(&self) -> Option<AtomicWriteFault> {
        None
    }
}

pub(crate) fn recovery_action(phase: RelocationPhase) -> RecoveryAction {
    match phase {
        RelocationPhase::Prepared | RelocationPhase::Staged | RelocationPhase::TargetPublished => {
            RecoveryAction::RollBackToSource
        }
        RelocationPhase::Ready => RecoveryAction::CommitTarget,
        RelocationPhase::Committed => RecoveryAction::VerifyCommitted,
        RelocationPhase::RolledBack => RecoveryAction::VerifyRolledBack,
    }
}

pub(super) fn has_target_authority(phase: RelocationPhase) -> bool {
    matches!(phase, RelocationPhase::Ready | RelocationPhase::Committed)
}

fn staging_name(session_id: &str, nonce: &str) -> String {
    format!(".{session_id}.relocating-{nonce}")
}

fn reject_existing(path: &Path) -> Result<()> {
    match std_fs::symlink_metadata(path) {
        Ok(_) => Err(RelocationError::Collision(path.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(fs::io_error("inspect", path, error)),
    }
}

fn rewrite_staged_summary(
    staging: &Path,
    journal: &RelocationJournal,
    pending: PendingCwdSwitchReminder,
) -> Result<()> {
    let path = staging.join(super::SUMMARY_FILE);
    let bytes = std_fs::read(&path).map_err(|e| fs::io_error("read", &path, e))?;
    let source: Summary =
        serde_json::from_slice(&bytes).map_err(|source| RelocationError::Json {
            path: path.clone(),
            source,
        })?;
    if source.info.id.to_string() != journal.session_id
        || source.info.cwd != journal.source_cwd
        || source
            .cwd_generation
            .checked_add(1)
            .is_none_or(|generation| generation != journal.cwd_generation)
    {
        return Err(RelocationError::Inconsistent(
            "copied summary no longer matches the validated source".into(),
        ));
    }

    let mut value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|source| RelocationError::Json {
            path: path.clone(),
            source,
        })?;
    let top = value
        .as_object_mut()
        .ok_or_else(|| RelocationError::Inconsistent("summary must be a JSON object".into()))?;
    let info = top
        .get_mut("info")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| RelocationError::Inconsistent("summary info must be an object".into()))?;
    info.insert("cwd".into(), journal.target_cwd.clone().into());
    top.insert("cwd_generation".into(), journal.cwd_generation.into());
    top.insert("previous_cwd".into(), journal.source_cwd.clone().into());
    top.insert(
        "pending_cwd_switch_reminder".into(),
        serde_json::to_value(pending).map_err(|source| RelocationError::Json {
            path: path.clone(),
            source,
        })?,
    );
    let permissions = std_fs::metadata(&path)
        .map_err(|e| fs::io_error("inspect", &path, e))?
        .permissions();
    let bytes = serde_json::to_vec_pretty(&value).map_err(|source| RelocationError::Json {
        path: path.clone(),
        source,
    })?;
    fs::write_atomic_durable(&path, &bytes, Some(permissions), None).map_err(write_failure_error)
}

fn require_regular_file(path: &Path) -> Result<()> {
    let metadata = std_fs::symlink_metadata(path).map_err(|e| fs::io_error("inspect", path, e))?;
    if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(RelocationError::Inconsistent(format!(
            "expected regular file: {}",
            path.display()
        )))
    }
}

fn read_summary(path: &Path) -> Result<Summary> {
    let bytes = std_fs::read(path).map_err(|e| fs::io_error("read", path, e))?;
    serde_json::from_slice(&bytes).map_err(|source| RelocationError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn write_failure_error(error: WriteFailure) -> RelocationError {
    match error {
        WriteFailure::NotCommitted(error) | WriteFailure::Committed(error) => error,
    }
}

fn recovery_required(phase: RelocationPhase, source: RelocationError) -> RelocationError {
    match source {
        RelocationError::RecoveryRequired { .. } => source,
        source => RelocationError::RecoveryRequired {
            phase,
            source: Box::new(source),
        },
    }
}

#[cfg(all(test, unix))]
mod tests;
