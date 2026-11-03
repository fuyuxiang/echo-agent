//! Native verified export receipts are part of a session's durable history.
//! Recording a receipt never authorizes paths supplied by the renderer.
use crate::document_export::DocumentExportReceipt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::State;

const MAX_BYTES: u64 = 2 * 1024 * 1024;
static WRITES: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionExport {
    pub receipt: DocumentExportReceipt,
    pub title: String,
    pub created_at: u64,
    #[serde(default)]
    pub pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_digest: Option<String>,
}

fn store_path(home: &Path, session: &str) -> Result<PathBuf, String> {
    if session.is_empty() || session.len() > 256 || session.chars().any(char::is_control) {
        return Err("成果记录的会话标识无效".into());
    }
    let root = home.join("session-artifacts");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    if std::fs::symlink_metadata(&root)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("成果目录不能是符号链接".into());
    }
    crate::paths::harden_private_dir(&root)?;
    Ok(root.join(format!(
        "{}.json",
        hex::encode(Sha256::digest(session.as_bytes()))
    )))
}

fn read_at(home: &Path, session: &str) -> Result<Vec<SessionExport>, String> {
    let path = store_path(home, session)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = crate::shell_fs::read_regular_file_bounded(&path, MAX_BYTES)?;
    serde_json::from_slice(&bytes).map_err(|e| format!("成果记录损坏，原文件已保留：{e}"))
}

fn record_at(
    home: &Path,
    session: &str,
    receipt: DocumentExportReceipt,
    title: &str,
    pending: bool,
) -> Result<(), String> {
    let _guard = WRITES.lock().map_err(|_| "成果存储锁已损坏")?;
    let mut entries = read_at(home, session)?;
    if pending {
        for entry in &mut entries {
            if entry.pending
                && entry.receipt.path == receipt.path
                && staged_receipt_matches(&entry.receipt)
            {
                entry.pending = false;
            }
        }
    }
    // Staging must preserve the prior published receipt until installation
    // succeeds. Promotion removes only older versions of the same output.
    let identity = entries
        .iter()
        .find(|e| e.receipt.path == receipt.path && e.receipt.sha256 == receipt.sha256)
        .map(|e| (e.request_key.clone(), e.request_digest.clone()))
        .unwrap_or_default();
    entries.retain(|entry| entry.receipt.path != receipt.path || (pending && !entry.pending));
    entries.push(SessionExport {
        receipt,
        pending,
        title: title.chars().take(200).collect(),
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_millis() as u64,
        request_key: identity.0,
        request_digest: identity.1,
    });
    let bytes = serde_json::to_vec(&entries).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("会话成果记录超过2MB，原记录已保留".into());
    }
    crate::paths::write_private_file(&store_path(home, session)?, &bytes)
}

pub(crate) fn creation_receipt(
    session: &str,
    key: &str,
    digest: &str,
) -> Result<Option<DocumentExportReceipt>, String> {
    creation_receipt_at(&crate::paths::echo_agent_home_dir(), session, key, digest)
}

fn creation_receipt_at(
    home: &Path,
    session: &str,
    key: &str,
    digest: &str,
) -> Result<Option<DocumentExportReceipt>, String> {
    let _guard = WRITES.lock().map_err(|_| "成果存储锁已损坏")?;
    let entries = read_at(home, session)?;
    let Some(entry) = entries
        .iter()
        .rev()
        .find(|e| e.request_key.as_deref() == Some(key))
    else {
        return Ok(None);
    };
    if entry.request_digest.as_deref() != Some(digest) {
        return Err("该请求或文件名已经用于其他内容，请使用新的请求标识和文件名".into());
    }
    if !staged_receipt_matches(&entry.receipt) {
        if entry.pending && !Path::new(&entry.receipt.path).exists() {
            return Ok(None);
        }
        return Err("此前生成的文件已移动、删除或被修改，请指定新的文件名后重新生成".into());
    }
    Ok(Some(entry.receipt.clone()))
}

pub(crate) fn identify_creation(
    session: &str,
    path: &str,
    key: &str,
    digest: &str,
) -> Result<(), String> {
    identify_creation_at(
        &crate::paths::echo_agent_home_dir(),
        session,
        path,
        key,
        digest,
    )
}

fn identify_creation_at(
    home: &Path,
    session: &str,
    path: &str,
    key: &str,
    digest: &str,
) -> Result<(), String> {
    let _guard = WRITES.lock().map_err(|_| "成果存储锁已损坏")?;
    let mut entries = read_at(home, session)?;
    let entry = entries
        .iter_mut()
        .rev()
        .find(|e| e.receipt.path == path)
        .ok_or("成果回执不存在")?;
    entry.request_key = Some(key.into());
    entry.request_digest = Some(digest.into());
    let bytes = serde_json::to_vec(&entries).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("成果记录超过 2MB".into());
    }
    crate::paths::write_private_file(&store_path(home, session)?, &bytes)
}

/// Only the native exporter calls this with its validated output receipt.
pub(crate) fn record(
    session: &str,
    receipt: DocumentExportReceipt,
    title: &str,
) -> Result<(), String> {
    record_at(
        &crate::paths::echo_agent_home_dir(),
        session,
        receipt,
        title,
        false,
    )
}

pub(crate) fn stage(
    session: &str,
    receipt: DocumentExportReceipt,
    title: &str,
) -> Result<(), String> {
    record_at(
        &crate::paths::echo_agent_home_dir(),
        session,
        receipt,
        title,
        true,
    )
}

fn staged_receipt_matches(receipt: &DocumentExportReceipt) -> bool {
    // Receipt paths were canonicalized before publication. A replaced
    // ancestor/final symlink must not redirect recovery reads.
    if dunce::canonicalize(&receipt.path).ok().as_deref() != Some(Path::new(&receipt.path)) {
        return false;
    }
    matches!(crate::document_export::validate_output(Path::new(&receipt.path), &receipt.format),
        Ok((size, hash)) if size == receipt.byte_size && hash == receipt.sha256)
}

fn published_entries(entries: Vec<SessionExport>) -> Vec<SessionExport> {
    let mut published = std::collections::BTreeMap::new();
    for mut entry in entries {
        if entry.pending {
            if !staged_receipt_matches(&entry.receipt) {
                continue;
            }
            entry.pending = false;
        }
        published.insert(entry.receipt.path.clone(), entry);
    }
    published.into_values().collect()
}

#[tauri::command]
pub fn session_exports_list(
    state: State<'_, crate::commands::AppState>,
    session_id: String,
) -> Result<Vec<SessionExport>, String> {
    state.session_workspace(&session_id)?;
    let _guard = WRITES.lock().map_err(|_| "成果存储锁已损坏")?;
    read_at(&crate::paths::echo_agent_home_dir(), &session_id).map(published_entries)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogExport {
    pub session_id: String,
    pub session_title: String,
    pub cwd: String,
    #[serde(flatten)]
    pub export: SessionExport,
}

fn catalog_at(
    home: &Path,
    sessions: &[(String, String, String)],
) -> Result<Vec<CatalogExport>, String> {
    let _guard = WRITES.lock().map_err(|_| "成果存储锁已损坏")?;
    let mut result = Vec::new();
    for (session_id, title, cwd) in sessions {
        for export in published_entries(read_at(home, session_id)?) {
            result.push(CatalogExport {
                session_id: session_id.clone(),
                session_title: title.clone(),
                cwd: cwd.clone(),
                export,
            });
        }
    }
    Ok(result)
}

/// Read-only catalog; only an explicit open verifies and grants the exact file.
#[tauri::command]
pub async fn session_exports_catalog() -> Result<Vec<CatalogExport>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let sessions = crate::sessions::list_all_sessions(true)?
            .into_iter()
            .map(|session| (session.session_id, session.title, session.cwd))
            .collect::<Vec<_>>();
        catalog_at(&crate::paths::echo_agent_home_dir(), &sessions)
    })
    .await
    .map_err(|e| format!("读取成果目录失败：{e}"))?
}

/// Grant a single output only after an explicit click and verification against
/// the native export ledger. Renderer paths and cached catalog rows grant nothing.
#[tauri::command]
pub async fn session_export_authorize(
    access: State<'_, crate::shell_fs::FilesystemAccess>,
    session_id: String,
    path: String,
    sha256: String,
) -> Result<(), String> {
    let home = crate::paths::echo_agent_home_dir();
    let receipt = tauri::async_runtime::spawn_blocking(move || {
        verified_receipt_at(&home, &session_id, &path, &sha256)
    })
    .await
    .map_err(|error| format!("无法核验导出文件：{error}"))??;
    access.authorize_file(Path::new(&receipt.path))?;
    Ok(())
}

fn verified_receipt_at(
    home: &Path,
    session: &str,
    path: &str,
    sha256: &str,
) -> Result<DocumentExportReceipt, String> {
    if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("导出回执无效，请重新打开原会话".into());
    }
    let _guard = WRITES.lock().map_err(|_| "成果存储锁已损坏")?;
    let entry = read_at(home, session)?
        .into_iter()
        .rev()
        .find(|entry| entry.receipt.path == path && entry.receipt.sha256 == sha256)
        .ok_or("未找到此会话的导出回执，请重新打开原会话")?;
    if !staged_receipt_matches(&entry.receipt) {
        return Err("导出文件已移动、删除或内容已变化。原成果记录已保留；请重新导出，或通过文件选择器重新选择当前文件".into());
    }
    Ok(entry.receipt)
}

pub(crate) fn has_export_path(session: &str, path: &Path) -> Result<bool, String> {
    Ok(read_at(&crate::paths::echo_agent_home_dir(), session)?.iter().any(|entry| {
        dunce::canonicalize(&entry.receipt.path).is_ok_and(|recorded| recorded == path)
    }))
}

pub(crate) fn forget_session(session: &str) -> Result<(), String> {
    let _guard = WRITES.lock().map_err(|_| "成果存储锁已损坏")?;
    let path = store_path(&crate::paths::echo_agent_home_dir(), session)?;
    crate::document_store::forget_session(&crate::paths::echo_agent_home_dir(), session)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn creation_retries_reuse_only_the_same_verified_output_after_restart() {
        let home = tempfile::tempdir().unwrap();
        let output = dunce::canonicalize(home.path())
            .unwrap()
            .join("created.pdf");
        let bytes = b"%PDF-1.7\nverified creation\n";
        let receipt = DocumentExportReceipt {
            path: output.to_string_lossy().into_owned(),
            format: "pdf".into(),
            byte_size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        record_at(home.path(), "task", receipt.clone(), "成果", true).unwrap();
        identify_creation_at(home.path(), "task", &receipt.path, "request-1", "digest-1").unwrap();
        // A failed pre-publication attempt can be retried without inventing a
        // successful receipt. A matching published file survives a crash before
        // the pending index is promoted.
        assert!(
            creation_receipt_at(home.path(), "task", "request-1", "digest-1")
                .unwrap()
                .is_none()
        );
        std::fs::write(&output, bytes).unwrap();
        assert_eq!(
            creation_receipt_at(home.path(), "task", "request-1", "digest-1")
                .unwrap()
                .unwrap()
                .path,
            receipt.path
        );
        record_at(home.path(), "task", receipt.clone(), "成果", false).unwrap();
        assert!(
            creation_receipt_at(home.path(), "task", "request-1", "digest-1")
                .unwrap()
                .is_some()
        );
        assert!(creation_receipt_at(home.path(), "task", "request-1", "changed-digest").is_err());
        assert!(
            creation_receipt_at(home.path(), "other-task", "request-1", "digest-1")
                .unwrap()
                .is_none()
        );
        std::fs::write(&output, b"%PDF-1.7\nuser changes\n").unwrap();
        assert!(creation_receipt_at(home.path(), "task", "request-1", "digest-1").is_err());
        std::fs::remove_file(&output).unwrap();
        assert!(creation_receipt_at(home.path(), "task", "request-1", "digest-1").is_err());
    }

    #[test]
    fn restart_access_is_restored_only_for_the_clicked_unchanged_native_receipt() {
        let home = tempfile::tempdir().unwrap();
        let outputs = tempfile::tempdir().unwrap();
        let path = dunce::canonicalize(outputs.path())
            .unwrap()
            .join("report.pdf");
        let sibling = path.with_file_name("private.pdf");
        let bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
        std::fs::write(&path, bytes).unwrap();
        std::fs::write(&sibling, bytes).unwrap();
        let receipt = DocumentExportReceipt {
            path: path.to_string_lossy().into_owned(),
            format: "pdf".into(),
            byte_size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        record_at(home.path(), "session", receipt.clone(), "报告", false).unwrap();
        let restarted_access = crate::shell_fs::FilesystemAccess::default();
        assert!(restarted_access.require_authorized_file(&path).is_err());
        assert!(verified_receipt_at(
            home.path(),
            "session",
            sibling.to_str().unwrap(),
            &receipt.sha256
        )
        .is_err());
        assert!(
            verified_receipt_at(home.path(), "other-session", &receipt.path, &receipt.sha256)
                .is_err()
        );
        assert!(
            verified_receipt_at(home.path(), "session", &receipt.path, &"0".repeat(64)).is_err()
        );
        let verified =
            verified_receipt_at(home.path(), "session", &receipt.path, &receipt.sha256).unwrap();
        restarted_access
            .authorize_file(Path::new(&verified.path))
            .unwrap();
        assert!(restarted_access.require_authorized_file(&path).is_ok());
        assert!(restarted_access.require_authorized_file(&sibling).is_err());
        std::fs::write(&path, b"%PDF-1.7\nchanged").unwrap();
        assert!(
            verified_receipt_at(home.path(), "session", &receipt.path, &receipt.sha256)
                .unwrap_err()
                .contains("内容已变化")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            verified_receipt_at(home.path(), "session", &receipt.path, &receipt.sha256).is_err()
        );
        assert_eq!(read_at(home.path(), "session").unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn receipt_access_refuses_symlink_replacements() {
        let home = tempfile::tempdir().unwrap();
        let path = dunce::canonicalize(home.path()).unwrap().join("report.pdf");
        let bytes = b"%PDF-1.7\nsynthetic output\n";
        let target = path.with_file_name("target.pdf");
        std::fs::write(&target, bytes).unwrap();
        let receipt = DocumentExportReceipt {
            path: path.to_string_lossy().into_owned(),
            format: "pdf".into(),
            byte_size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        record_at(home.path(), "s", receipt.clone(), "报告", false).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(verified_receipt_at(home.path(), "s", &receipt.path, &receipt.sha256).is_err());
    }

    #[test]
    fn staged_receipt_recovers_published_file_after_failed_promotion() {
        let home = tempfile::tempdir().unwrap();
        let path = dunce::canonicalize(home.path())
            .unwrap()
            .join("recover.pdf");
        let bytes = b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n";
        let receipt = DocumentExportReceipt {
            path: path.to_string_lossy().into_owned(),
            format: "pdf".into(),
            byte_size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        };
        record_at(home.path(), "session", receipt.clone(), "恢复成果", true).unwrap();
        assert!(published_entries(read_at(home.path(), "session").unwrap()).is_empty());
        std::fs::write(&path, bytes).unwrap();
        let recovered = published_entries(read_at(home.path(), "session").unwrap());
        assert_eq!(recovered.len(), 1);
        assert!(!recovered[0].pending);
        std::fs::write(&path, b"%PDF-1.7\nmodified").unwrap();
        assert!(published_entries(read_at(home.path(), "session").unwrap()).is_empty());
        std::fs::write(&path, bytes).unwrap();
        let mut next = receipt.clone();
        next.sha256 = "b".repeat(64);
        record_at(home.path(), "session", next, "下一次导出尚未发布", true).unwrap();
        let retained = published_entries(read_at(home.path(), "session").unwrap());
        assert_eq!(
            retained.len(),
            1,
            "a new staging operation must retain a previously recovered published output"
        );
        assert_eq!(retained[0].receipt.sha256, receipt.sha256);
    }

    #[test]
    fn verified_exports_roundtrip_and_replace_only_the_same_output() {
        let home = tempfile::tempdir().unwrap();
        for (path, hash) in [
            ("/synthetic/a.docx", "a"),
            ("/synthetic/b.pdf", "b"),
            ("/synthetic/a.docx", "c"),
        ] {
            record_at(
                home.path(),
                "session",
                DocumentExportReceipt {
                    path: path.into(),
                    format: "docx".into(),
                    byte_size: 42,
                    sha256: hash.repeat(64),
                },
                "成果",
                false,
            )
            .unwrap();
        }
        let records = read_at(home.path(), "session").unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].receipt.sha256, "c".repeat(64));
        assert!(read_at(home.path(), "another-session").unwrap().is_empty());
        std::fs::write(store_path(home.path(), "session").unwrap(), b"broken").unwrap();
        assert!(record_at(
            home.path(),
            "session",
            records[0].receipt.clone(),
            "new",
            false
        )
        .is_err());
        assert_eq!(
            std::fs::read(store_path(home.path(), "session").unwrap()).unwrap(),
            b"broken"
        );
    }
    #[test]
    fn catalog_reads_archived_session_receipts_without_live_session_or_ui_cache() {
        let home = tempfile::tempdir().unwrap();
        record_at(
            home.path(),
            "archived",
            DocumentExportReceipt {
                path: "/output.docx".into(),
                format: "docx".into(),
                byte_size: 42,
                sha256: "a".repeat(64),
            },
            "export",
            false,
        )
        .unwrap();
        let catalog = catalog_at(
            home.path(),
            &[("archived".into(), "old task".into(), "/old".into())],
        )
        .unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].session_id, "archived");
        assert_eq!(catalog[0].export.receipt.path, "/output.docx");
    }
}
