use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::source;
use super::{ManagedConfigError, ManagedConfigOutcome, ManagedConfigPlan, ManagedConfigStatus};

static ARTIFACT_NONCE: AtomicU64 = AtomicU64::new(0);
const ARTIFACT_RESERVATION_ATTEMPTS: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TransactionPhase {
    AfterLock,
    BeforeBackupReserve,
    AfterBackupReserved,
    BeforeTempReserve,
    BeforeTempWrite,
    AfterTempWritten,
    AfterValidation,
    BeforePublish,
    AfterPublish,
    BeforeParentSync,
    AfterParentSync,
    BeforeVerify,
    BeforeRollback,
    BeforeRollbackSync,
    AfterRollback,
}

impl TransactionPhase {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::AfterLock => "after-lock",
            Self::BeforeBackupReserve => "before-backup-reserve",
            Self::AfterBackupReserved => "after-backup-reserved",
            Self::BeforeTempReserve => "before-temp-reserve",
            Self::BeforeTempWrite => "before-temp-write",
            Self::AfterTempWritten => "after-temp-written",
            Self::AfterValidation => "after-validation",
            Self::BeforePublish => "before-publish",
            Self::AfterPublish => "after-publish",
            Self::BeforeParentSync => "before-parent-sync",
            Self::AfterParentSync => "after-parent-sync",
            Self::BeforeVerify => "before-verify",
            Self::BeforeRollback => "before-rollback",
            Self::BeforeRollbackSync => "before-rollback-sync",
            Self::AfterRollback => "after-rollback",
        }
    }
}

pub(super) trait TransactionObserver: Send + Sync {
    fn phase(&self, _phase: TransactionPhase, _plan: &ManagedConfigPlan) -> std::io::Result<()> {
        Ok(())
    }

    fn mutate_written_temp(&self, _path: &Path, _plan: &ManagedConfigPlan) -> std::io::Result<()> {
        Ok(())
    }

    fn publish(&self, temp: &Path, target: &Path) -> std::io::Result<()> {
        fs::rename(temp, target)
    }

    fn sync_parent(
        &self,
        parent: &source::ParentAnchor,
        _rollback: bool,
    ) -> Result<(), ManagedConfigError> {
        parent.sync()
    }
}

pub(super) struct NoopObserver;
impl TransactionObserver for NoopObserver {}

pub(super) fn sibling_artifact(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".to_owned());
    path.with_file_name(format!("{name}.{suffix}"))
}

pub(super) fn artifact_hint(path: &Path, kind: &str) -> PathBuf {
    artifact_candidate(path, kind, 0)
}

fn artifact_candidate(path: &Path, kind: &str, attempt: usize) -> PathBuf {
    let suffix = if attempt == 0 {
        format!("{kind}.{}", std::process::id())
    } else {
        let nonce = ARTIFACT_NONCE.fetch_add(1, Ordering::Relaxed);
        format!("{kind}.{}.{}", std::process::id(), nonce)
    };
    sibling_artifact(path, &suffix)
}

pub(super) fn apply(
    plan: ManagedConfigPlan,
    observer: &dyn TransactionObserver,
) -> Result<ManagedConfigOutcome, ManagedConfigError> {
    let parent_anchor = plan.parent_plan.ensure_and_anchor()?;
    if !plan.changes_file() {
        parent_anchor.revalidate()?;
        source::revalidate(&plan)?;
        return Ok(ManagedConfigOutcome {
            status: ManagedConfigStatus::NoChange,
            requested_path: plan.requested_path,
            target_path: plan.target_path,
            backup_path: None,
        });
    }

    let lock = open_lock(&plan.lock_path)?;
    lock.lock().map_err(|source| ManagedConfigError::Lock {
        path: plan.lock_path.clone(),
        source,
    })?;
    observe(observer, TransactionPhase::AfterLock, &plan)?;
    parent_anchor.revalidate()?;
    source::revalidate(&plan)?;

    let mut backup = None;
    let mut temp = None;
    let precommit = (|| {
        if let Some(bytes) = &plan.original.bytes {
            observe(observer, TransactionPhase::BeforeBackupReserve, &plan)?;
            let (path, mut file) = reserve_artifact(
                &plan.target_path,
                "echo-agent-backup",
                plan.backup_path_hint.as_deref(),
                plan.original.mode,
            )?;
            backup = Some(path.clone());
            write_reserved(&path, &mut file, bytes, plan.original.mode)?;
            observe(observer, TransactionPhase::AfterBackupReserved, &plan)?;
        }

        observe(observer, TransactionPhase::BeforeTempReserve, &plan)?;
        let (temp_path, mut temp_file) = reserve_artifact(
            &plan.target_path,
            "echo-agent-tmp",
            plan.temp_path_hint.as_deref(),
            plan.original.mode,
        )?;
        temp = Some(temp_path.clone());
        observe(observer, TransactionPhase::BeforeTempWrite, &plan)?;
        write_reserved(
            &temp_path,
            &mut temp_file,
            &plan.updated,
            plan.original.mode,
        )?;
        observe(observer, TransactionPhase::AfterTempWritten, &plan)?;

        if let Some(validator) = &plan.request.validator {
            super::validator::validate_temp(validator, &temp_path)?;
        }
        observe(observer, TransactionPhase::AfterValidation, &plan)?;
        parent_anchor.revalidate()?;
        source::revalidate(&plan)?;
        observe(observer, TransactionPhase::BeforePublish, &plan)?;
        parent_anchor.revalidate()?;
        apply_exact_path_mode(&temp_path, plan.original.mode)?;
        observer
            .publish(&temp_path, &plan.target_path)
            .map_err(|source| ManagedConfigError::Publish {
                path: plan.target_path.clone(),
                source,
            })?;
        temp = None;
        Ok::<(), ManagedConfigError>(())
    })();

    if let Err(error) = precommit {
        cleanup(temp.as_deref());
        cleanup(backup.as_deref());
        return Err(error);
    }

    let post_publish = (|| {
        observe(observer, TransactionPhase::AfterPublish, &plan)?;
        parent_anchor.revalidate()?;
        observe(observer, TransactionPhase::BeforeParentSync, &plan)?;
        observer.sync_parent(&parent_anchor, false)?;
        observe(observer, TransactionPhase::AfterParentSync, &plan)?;
        parent_anchor.revalidate()?;
        observe(observer, TransactionPhase::BeforeVerify, &plan)?;
        observer
            .mutate_written_temp(&plan.target_path, &plan)
            .map_err(|source| ManagedConfigError::Phase {
                phase: "mutate-published-target",
                source,
            })?;
        verify_published(&plan)?;
        Ok::<(), ManagedConfigError>(())
    })();

    if let Err(primary) = post_publish {
        match rollback(&plan, observer, &parent_anchor) {
            Ok(()) => {
                cleanup(backup.as_deref());
                return Err(primary);
            }
            Err(recovery) => {
                return Err(ManagedConfigError::Recovery {
                    primary: Box::new(primary),
                    recovery: Box::new(recovery),
                });
            }
        }
    }

    Ok(ManagedConfigOutcome {
        status: ManagedConfigStatus::Applied,
        requested_path: plan.requested_path,
        target_path: plan.target_path,
        backup_path: backup,
    })
}

fn observe(
    observer: &dyn TransactionObserver,
    phase: TransactionPhase,
    plan: &ManagedConfigPlan,
) -> Result<(), ManagedConfigError> {
    observer
        .phase(phase, plan)
        .map_err(|source| ManagedConfigError::Phase {
            phase: phase.name(),
            source,
        })
}

fn reserve_artifact(
    target: &Path,
    kind: &str,
    hint: Option<&Path>,
    mode: Option<u32>,
) -> Result<(PathBuf, File), ManagedConfigError> {
    for attempt in 0..ARTIFACT_RESERVATION_ATTEMPTS {
        let candidate = if attempt == 0 {
            hint.map(Path::to_path_buf)
                .unwrap_or_else(|| artifact_candidate(target, kind, attempt))
        } else {
            artifact_candidate(target, kind, attempt)
        };
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        match options.open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(ManagedConfigError::Write {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Err(ManagedConfigError::Write {
        path: target.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("could not reserve a unique {kind} artifact"),
        ),
    })
}

fn write_reserved(
    path: &Path,
    file: &mut File,
    bytes: &[u8],
    mode: Option<u32>,
) -> Result<(), ManagedConfigError> {
    file.write_all(bytes)
        .and_then(|()| apply_exact_mode(file, mode))
        .and_then(|()| file.sync_all())
        .map_err(|source| ManagedConfigError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn apply_exact_mode(file: &File, mode: Option<u32>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(mode) = mode {
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_exact_mode(_: &File, _: Option<u32>) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn apply_exact_path_mode(path: &Path, mode: Option<u32>) -> Result<(), ManagedConfigError> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(mode) = mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| {
            ManagedConfigError::Write {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_exact_path_mode(_: &Path, _: Option<u32>) -> Result<(), ManagedConfigError> {
    Ok(())
}

fn verify_published(plan: &ManagedConfigPlan) -> Result<(), ManagedConfigError> {
    let published = fs::read(&plan.target_path).map_err(|source| ManagedConfigError::Read {
        path: plan.target_path.clone(),
        source,
    })?;
    if published != plan.updated {
        return Err(ManagedConfigError::Verification {
            path: plan.target_path.clone(),
            reason: "published bytes differ from the confirmed plan".to_owned(),
        });
    }
    if current_mode(&plan.target_path)? != plan.original.mode {
        return Err(ManagedConfigError::Verification {
            path: plan.target_path.clone(),
            reason: "published mode differs from the confirmed source mode".to_owned(),
        });
    }
    Ok(())
}

fn rollback(
    plan: &ManagedConfigPlan,
    observer: &dyn TransactionObserver,
    parent_anchor: &source::ParentAnchor,
) -> Result<(), ManagedConfigError> {
    observe(observer, TransactionPhase::BeforeRollback, plan)?;
    parent_anchor.revalidate()?;
    if let Some(original) = &plan.original.bytes {
        let (rollback_path, mut rollback_file) = reserve_artifact(
            &plan.target_path,
            "echo-agent-rollback",
            None,
            plan.original.mode,
        )?;
        if let Err(error) = write_reserved(
            &rollback_path,
            &mut rollback_file,
            original,
            plan.original.mode,
        ) {
            cleanup(Some(&rollback_path));
            return Err(error);
        }
        apply_exact_path_mode(&rollback_path, plan.original.mode)?;
        if let Err(source) = fs::rename(&rollback_path, &plan.target_path) {
            cleanup(Some(&rollback_path));
            return Err(ManagedConfigError::Publish {
                path: plan.target_path.clone(),
                source,
            });
        }
    } else {
        match fs::remove_file(&plan.target_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ManagedConfigError::Publish {
                    path: plan.target_path.clone(),
                    source,
                });
            }
        }
    }
    observe(observer, TransactionPhase::BeforeRollbackSync, plan)?;
    observer.sync_parent(parent_anchor, true)?;
    verify_rollback(plan)?;
    observe(observer, TransactionPhase::AfterRollback, plan)
}

fn verify_rollback(plan: &ManagedConfigPlan) -> Result<(), ManagedConfigError> {
    match &plan.original.bytes {
        Some(original) => {
            let restored =
                fs::read(&plan.target_path).map_err(|source| ManagedConfigError::Read {
                    path: plan.target_path.clone(),
                    source,
                })?;
            if &restored != original || current_mode(&plan.target_path)? != plan.original.mode {
                return Err(ManagedConfigError::Verification {
                    path: plan.target_path.clone(),
                    reason: "rollback did not restore the original bytes and mode".to_owned(),
                });
            }
        }
        None if plan.target_path.exists() => {
            return Err(ManagedConfigError::Verification {
                path: plan.target_path.clone(),
                reason: "rollback did not remove the newly created target".to_owned(),
            });
        }
        None => {}
    }
    Ok(())
}

#[cfg(unix)]
fn current_mode(path: &Path) -> Result<Option<u32>, ManagedConfigError> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::metadata(path)
        .map(|metadata| Some(metadata.permissions().mode() & 0o7777))
        .map_err(|source| ManagedConfigError::Read {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn current_mode(_: &Path) -> Result<Option<u32>, ManagedConfigError> {
    Ok(None)
}

fn open_lock(path: &Path) -> Result<File, ManagedConfigError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|source| ManagedConfigError::Lock {
            path: path.to_path_buf(),
            source,
        })
}

fn cleanup(path: Option<&Path>) {
    if let Some(path) = path {
        let _ = fs::remove_file(path);
    }
}
