//! Durable task context and receipts. Execution authority is never restored.
use super::task::TaskExecution;
use sha2::{Digest, Sha256};
fn path(session: &str) -> std::path::PathBuf {
    crate::paths::echo_agent_home_dir()
        .join("automation/checkpoints")
        .join(format!("{:x}.json", Sha256::digest(session.as_bytes())))
}
pub fn save(session: &str, task: &TaskExecution) -> Result<(), String> {
    let path = path(session);
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    crate::paths::harden_private_dir(dir)?;
    let mut task = task.clone();
    task.authorized = false;
    let bytes = serde_json::to_vec(&task).map_err(|e| e.to_string())?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::now_v7()));
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result.map_err(|e| format!("checkpoint_unavailable: {e}"))
}
pub fn load(session: &str) -> Option<TaskExecution> {
    let path = path(session);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 256 * 1024 {
        return None;
    }
    let mut task: TaskExecution = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if task.message_text.is_none()
        && (task.reason.as_deref() == Some("message_text_required")
            || task
                .scope
                .as_deref()
                .is_some_and(super::task::message_text_required))
    {
        task.message_text_required = true;
    }
    let terminal = matches!(
        task.phase,
        super::task::Phase::Completed | super::task::Phase::Stopped | super::task::Phase::Failed
    )
    .then_some(task.phase);
    task.pause("application_restarted");
    if let Some(phase) = terminal {
        task.phase = phase;
    }
    task.current_tool = None;
    Some(task)
}
pub fn remove(session: &str) {
    if let Err(error) = std::fs::remove_file(path(session)) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(%error, "failed to remove deleted desktop task checkpoint");
        }
    }
}
