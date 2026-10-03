//! Versioned, streaming backups. Restore is staged and applied before services
//! start, with a rollback journal so an interrupted restore is recoverable.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

const MAX_TOTAL: u64 = 8 * 1024 * 1024 * 1024;
const FILES: &[&str] = &[
    "echoagent-projects.json",
    "echoagent-state.json",
    "echoagent-automations.json",
    "echoagent-automation-records.json",
    "echoagent-notifications.json",
    "desktop/desktop-preferences.json",
];
const DIRS: &[&str] = &[
    "sessions",
    "memory",
    "projects",
    "meetings",
    "coding",
    "clipboard-images",
];
pub const UI_KEYS: &[&str] = &[
    "echoagent.projects",
    "echoagent.drafts.v1",
    "echoagent.outbox.v1",
    "echoagent.draft-attachments.v1",
    "echoagent.task-artifacts.v1",
    "echoagent.usage",
    "echoagent.quota",
    "echoagent.usage-snapshots.v1",
    "echoagent.theme",
    "echoagent.fontSize",
];
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    name: String,
    bytes: u64,
    sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: u32,
    created_at: String,
    files: Vec<Entry>,
    ui_state: BTreeMap<String, String>,
    home: String,
    app_data: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    token: String,
    created_at: String,
    file_count: usize,
    total_bytes: u64,
    ui_keys: Vec<String>,
}
#[derive(Default)]
pub struct BackupState(Mutex<Option<(String, PathBuf)>>);
impl Drop for BackupState {
    fn drop(&mut self) {
        if let Ok(candidate) = self.0.get_mut() {
            if let Some((_, path)) = candidate.take() {
                let _ = fs::remove_file(path);
            }
        }
    }
}

fn source_path(home: &Path, app_data: &Path, name: &str) -> PathBuf {
    if let Some(name) = name.strip_prefix("desktop/") {
        app_data.join(name)
    } else if name == "clipboard-images" || name.starts_with("clipboard-images/") {
        app_data.join(name)
    } else {
        home.join(name)
    }
}

fn allowed(name: &str) -> bool {
    if name.contains('\\')
        || name.len() > 4096
        || name.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.contains(':')
                || part.chars().any(char::is_control)
        })
    {
        return false;
    }
    let path = Path::new(name);
    let parts: Vec<_> = path.components().collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return false;
    }
    FILES.contains(&name) || (parts.len() > 1 && DIRS.iter().any(|dir| path.starts_with(dir)))
}
fn validate(manifest: &Manifest) -> Result<(), String> {
    if manifest.version != 1 || manifest.files.len() > 100_000 {
        return Err("不支持的备份版本或文件过多".into());
    }
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for entry in &manifest.files {
        if !allowed(&entry.name) || !seen.insert(&entry.name) || entry.sha256.len() != 64 {
            return Err("备份包含无效或重复路径".into());
        }
        total = total.checked_add(entry.bytes).ok_or("备份大小溢出")?;
        if total > MAX_TOTAL {
            return Err("备份超过 8GB 恢复上限".into());
        }
    }
    if manifest
        .ui_state
        .keys()
        .any(|key| !UI_KEYS.contains(&key.as_str()))
        || serde_json::to_vec(&manifest.ui_state)
            .map_err(|e| e.to_string())?
            .len()
            > 8 * 1024 * 1024
    {
        return Err("界面备份范围或大小无效".into());
    }
    Ok(())
}
fn copy_hash(
    mut input: impl Read,
    mut output: impl Write,
    limit: u64,
) -> Result<(u64, String), String> {
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    let mut hash = Sha256::new();
    loop {
        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > limit {
            return Err("文件大小超出备份清单或上限".into());
        }
        hash.update(&buffer[..count]);
        output
            .write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
    }
    Ok((size, format!("{:x}", hash.finalize())))
}
fn export(
    home: &Path,
    app_data: &Path,
    destination: &Path,
    ui_state: BTreeMap<String, String>,
) -> Result<(), String> {
    let mut manifest = Manifest {
        version: 1,
        created_at: chrono::Utc::now().to_rfc3339(),
        files: vec![],
        ui_state,
        home: home.to_string_lossy().into_owned(),
        app_data: app_data.to_string_lossy().into_owned(),
    };
    validate(&manifest)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(destination.parent().ok_or("保存位置无效")?)
            .map_err(|e| e.to_string())?;
    let mut zip = ZipWriter::new(temporary.as_file_mut());
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    let mut total = 0;
    for root in FILES.iter().chain(DIRS.iter()) {
        let source = source_path(home, app_data, root);
        if !source.exists() {
            continue;
        }
        for file in walkdir::WalkDir::new(&source).follow_links(false) {
            let file = file.map_err(|e| e.to_string())?;
            if !file.file_type().is_file() {
                continue;
            }
            let relative = file
                .path()
                .strip_prefix(&source)
                .map_err(|e| e.to_string())?;
            let name = if relative.as_os_str().is_empty() {
                root.to_string()
            } else {
                Path::new(root)
                    .join(relative)
                    .to_string_lossy()
                    .replace('\\', "/")
            };
            if !allowed(&name) {
                return Err("备份路径超出数据范围".into());
            }
            zip.start_file(&name, options).map_err(|e| e.to_string())?;
            let (bytes, sha256) = copy_hash(
                File::open(file.path()).map_err(|e| e.to_string())?,
                &mut zip,
                MAX_TOTAL - total,
            )?;
            total += bytes;
            manifest.files.push(Entry {
                name,
                bytes,
                sha256,
            });
        }
    }
    validate(&manifest)?;
    zip.start_file("manifest.json", options)
        .map_err(|e| e.to_string())?;
    zip.write_all(&serde_json::to_vec(&manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(destination).map_err(|e| e.to_string())?;
    Ok(())
}
fn unpack(archive: &Path, staging: &Path) -> Result<Manifest, String> {
    let mut zip = ZipArchive::new(File::open(archive).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    zip.by_name("manifest.json")
        .map_err(|e| e.to_string())?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > 16 * 1024 * 1024 {
        return Err("备份清单过大".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&raw).map_err(|e| format!("备份清单损坏：{e}"))?;
    validate(&manifest)?;
    if zip.len() != manifest.files.len() + 1 {
        return Err("备份清单与实际文件数不符".into());
    }
    let mut names = HashSet::new();
    for index in 0..zip.len() {
        let file = zip.by_index(index).map_err(|e| e.to_string())?;
        if !names.insert(file.name().to_owned()) {
            return Err("备份包含重复 ZIP 条目".into());
        }
    }
    for entry in &manifest.files {
        let source = zip.by_name(&entry.name).map_err(|e| e.to_string())?;
        if source.is_symlink() || source.size() != entry.bytes {
            return Err("备份文件类型或大小不符".into());
        }
        let target = staging.join(&entry.name);
        fs::create_dir_all(target.parent().ok_or("备份路径无效")?).map_err(|e| e.to_string())?;
        // Reject aliases on the target filesystem (including case collisions)
        // instead of letting a later ZIP entry overwrite a validated entry.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| format!("备份路径冲突或无法创建 {}：{e}", entry.name))?;
        let (bytes, hash) = copy_hash(source, &mut file, entry.bytes)?;
        file.sync_all().map_err(|e| e.to_string())?;
        crate::paths::harden_private_file(&target)?;
        if bytes != entry.bytes || hash != entry.sha256 {
            return Err(format!("文件校验失败：{}", entry.name));
        }
    }
    Ok(manifest)
}
fn destination(home: &Path, app_data: &Path, name: &str) -> Result<PathBuf, String> {
    if !allowed(name) {
        return Err("恢复路径无效".into());
    }
    let target = source_path(home, app_data, name);
    // System aliases such as macOS /var -> /private/var are legitimate. Only
    // reject links at or below the application-controlled data roots.
    let root = if name.starts_with("desktop/") || name.starts_with("clipboard-images/") {
        app_data
    } else {
        home
    };
    for ancestor in target.ancestors().take_while(|path| path.starts_with(root)) {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("恢复目标包含符号链接，已停止恢复".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(target)
}
fn atomic_copy(source: &Path, target: &Path) -> Result<(), String> {
    let parent = target.parent().ok_or("恢复路径无效")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    std::io::copy(
        &mut File::open(source).map_err(|e| e.to_string())?,
        &mut temporary,
    )
    .map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    crate::paths::harden_private_file(temporary.path())?;
    temporary.persist(target).map_err(|e| e.to_string())?;
    Ok(())
}
#[derive(Serialize, Deserialize)]
struct Journal {
    files: Vec<(String, bool)>,
    previous: String,
    committed: bool,
    old_ui: Option<Vec<u8>>,
}
fn rollback(home: &Path, app_data: &Path) -> Result<(), String> {
    let journal_path = home.join("restore-journal.json");
    if !journal_path.exists() {
        return Ok(());
    }
    let journal: Journal =
        serde_json::from_slice(&fs::read(&journal_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if !journal.previous.starts_with("restore-previous-") || journal.previous.contains(['/', '\\'])
    {
        return Err("恢复日志目录无效".into());
    }
    if !journal.committed {
        for (name, existed) in journal.files {
            let path = destination(home, app_data, &name)?;
            if existed {
                atomic_copy(&home.join(&journal.previous).join(&name), &path)?;
            } else if path.exists() {
                fs::remove_file(path).map_err(|e| e.to_string())?;
            }
        }
        let ui_path = home.join("restore-ui.json");
        if let Some(bytes) = journal.old_ui {
            crate::paths::write_private_file(&ui_path, &bytes)?;
        } else if ui_path.exists() {
            fs::remove_file(ui_path).map_err(|e| e.to_string())?;
        }
    } else {
        // A crash after commit must finish cleanup, never revert committed data.
        let pending = home.join("restore-pending.zip");
        if pending.exists() {
            fs::remove_file(pending).map_err(|e| e.to_string())?;
        }
    }
    fs::remove_file(journal_path).map_err(|e| e.to_string())
}
fn rebase_value(value: &mut serde_json::Value, manifest: &Manifest, home: &Path, app_data: &Path) {
    match value {
        serde_json::Value::String(text) => {
            for (old, new) in [(&manifest.app_data, app_data), (&manifest.home, home)] {
                if !old.is_empty()
                    && (text == old
                        || text
                            .strip_prefix(old)
                            .is_some_and(|tail| tail.starts_with(['/', '\\'])))
                {
                    *text = format!("{}{}", new.display(), &text[old.len()..]);
                    break;
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                rebase_value(item, manifest, home, app_data);
            }
        }
        serde_json::Value::Object(items) => {
            for item in items.values_mut() {
                rebase_value(item, manifest, home, app_data);
            }
        }
        _ => {}
    }
}
fn prepare_file(
    path: &Path,
    name: &str,
    manifest: &Manifest,
    home: &Path,
    app_data: &Path,
) -> Result<(), String> {
    if name.starts_with("sessions/") && name.ends_with(".jsonl") {
        use std::io::{BufRead, BufReader};
        let input = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
        let mut output = tempfile::NamedTempFile::new_in(path.parent().ok_or("恢复路径无效")?)
            .map_err(|e| e.to_string())?;
        let mut reader = input;
        loop {
            let mut line = String::new();
            let count = reader
                .by_ref()
                .take(64 * 1024 * 1024 + 1)
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            if count > 64 * 1024 * 1024 {
                return Err("会话记录单行超过恢复上限".into());
            }
            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&line) {
                rebase_value(&mut value, manifest, home, app_data);
                serde_json::to_writer(&mut output, &value).map_err(|e| e.to_string())?;
                output.write_all(b"\n").map_err(|e| e.to_string())?;
            } else {
                output
                    .write_all(line.as_bytes())
                    .map_err(|e| e.to_string())?;
            }
        }
        output.as_file().sync_all().map_err(|e| e.to_string())?;
        crate::paths::harden_private_file(output.path())?;
        output.persist(path).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if !name.ends_with(".json")
        || name.starts_with("clipboard-images/")
        || name.starts_with("projects/")
    {
        return Ok(());
    }
    let bytes = crate::shell_fs::read_regular_file_bounded(path, 64 * 1024 * 1024)?;
    let mut value = match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(value) => value,
        Err(error) if FILES.contains(&name) || name.starts_with("coding/") => {
            return Err(format!("备份中的应用记录无法解析 {name}：{error}"))
        }
        Err(_) => return Ok(()),
    };
    rebase_value(&mut value, manifest, home, app_data);
    if name.starts_with("coding/")
        && (name.ends_with("/task.json") || name.ends_with("/tasks.json"))
    {
        fn pause(value: &mut serde_json::Value) {
            if matches!(
                value["phase"].as_str(),
                Some("discovering" | "implementing" | "verifying" | "diagnosing" | "repairing")
            ) {
                value["phase"] = serde_json::json!("paused");
                if value.get("nextAction").is_some() {
                    value["nextAction"] = serde_json::Value::Null;
                }
            }
        }
        pause(&mut value);
        if let Some(items) = value.get_mut("tasks").and_then(|v| v.as_array_mut()) {
            for item in items {
                pause(item);
            }
        }
    }
    if name == "echoagent-automations.json" {
        if let Some(items) = value.get_mut("automations").and_then(|v| v.as_array_mut()) {
            for item in items {
                item["status"] = serde_json::json!("PAUSED");
            }
        }
    }
    if name == "echoagent-automation-records.json" {
        if let Some(items) = value.get_mut("records").and_then(|v| v.as_array_mut()) {
            for item in items {
                if matches!(item["status"].as_str(), Some("queued" | "running")) {
                    item["status"] = serde_json::json!("failed");
                    item["error"] =
                        serde_json::json!("由备份恢复，未重新执行；请检查历史结果后手动运行");
                    item["finishedAt"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
                    item["automationSnapshot"] = serde_json::Value::Null;
                }
            }
        }
    }
    crate::paths::write_private_file(
        path,
        &serde_json::to_vec(&value).map_err(|e| e.to_string())?,
    )
}
fn apply_pending_at(home: &Path, app_data: &Path) -> Result<(), String> {
    rollback(home, app_data)?;
    let pending = home.join("restore-pending.zip");
    if !pending.exists() {
        return Ok(());
    }
    let staging = tempfile::tempdir_in(home).map_err(|e| e.to_string())?;
    let manifest = unpack(&pending, staging.path())?;
    let previous_name = format!("restore-previous-{}", uuid::Uuid::now_v7());
    let previous = home.join(&previous_name);
    let ui_path = home.join("restore-ui.json");
    let mut journal = Journal {
        files: vec![],
        previous: previous_name,
        committed: false,
        old_ui: if ui_path.exists() {
            Some(fs::read(&ui_path).map_err(|e| e.to_string())?)
        } else {
            None
        },
    };
    for entry in &manifest.files {
        let target = destination(home, app_data, &entry.name)?;
        let existed = target.exists();
        if existed {
            atomic_copy(&target, &previous.join(&entry.name))?;
        }
        prepare_file(
            &staging.path().join(&entry.name),
            &entry.name,
            &manifest,
            home,
            app_data,
        )?;
        journal.files.push((entry.name.clone(), existed));
    }
    let journal_path = home.join("restore-journal.json");
    crate::paths::write_private_file(
        &journal_path,
        &serde_json::to_vec(&journal).map_err(|e| e.to_string())?,
    )?;
    let result = (|| {
        for entry in &manifest.files {
            atomic_copy(
                &staging.path().join(&entry.name),
                &destination(home, app_data, &entry.name)?,
            )?;
        }
        let mut ui_state = manifest.ui_state.clone();
        for raw in ui_state.values_mut() {
            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) {
                rebase_value(&mut value, &manifest, home, app_data);
                *raw = serde_json::to_string(&value).map_err(|e| e.to_string())?;
            }
        }
        crate::paths::write_private_file(
            &ui_path,
            &serde_json::to_vec(&ui_state).map_err(|e| e.to_string())?,
        )?;
        journal.committed = true;
        crate::paths::write_private_file(
            &journal_path,
            &serde_json::to_vec(&journal).map_err(|e| e.to_string())?,
        )?;
        Ok(())
    })();
    rollback(home, app_data)?; // Revert a failure, or finish committed cleanup.
    result
}
/// Runs after single-instance acquisition, before any background service starts.
pub fn apply_pending(app: &AppHandle) -> Result<(), String> {
    let home = crate::paths::echo_agent_home_dir();
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    if let Err(error) = apply_pending_at(&home, &app_data) {
        // If rollback itself failed, stop startup to avoid using partial data.
        if home.join("restore-journal.json").exists() {
            return Err(error);
        }
        let pending = home.join("restore-pending.zip");
        if pending.exists() {
            fs::rename(
                pending,
                home.join(format!("restore-failed-{}.zip", uuid::Uuid::now_v7())),
            )
            .map_err(|e| e.to_string())?;
        }
        crate::paths::write_private_file(&home.join("restore-error.txt"), error.as_bytes())?;
        tracing::error!(%error, "backup restore rolled back; previous data preserved");
    }
    Ok(())
}

#[tauri::command]
pub async fn backup_export(
    app: AppHandle,
    ui_state: BTreeMap<String, String>,
) -> Result<Option<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("备份 EchoAgent 数据")
        .set_file_name("echoagent-backup.zip")
        .add_filter("ZIP", &["zip"])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(path) = rx.await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let path = path.into_path().map_err(|e| e.to_string())?;
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        export(
            &crate::paths::echo_agent_home_dir(),
            &app_data,
            &path,
            ui_state,
        )?;
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn backup_inspect(
    app: AppHandle,
    state: State<'_, BackupState>,
) -> Result<Option<Preview>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择要恢复的备份")
        .add_filter("ZIP", &["zip"])
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    let Some(path) = rx.await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let path = path.into_path().map_err(|e| e.to_string())?;
    let (preview, copy) = tauri::async_runtime::spawn_blocking(move || {
        let home = crate::paths::echo_agent_home_dir();
        let token = uuid::Uuid::now_v7().to_string();
        let copy = home.join(format!("restore-candidate-{token}.zip"));
        let mut output = tempfile::NamedTempFile::new_in(&home).map_err(|e| e.to_string())?;
        copy_hash(
            File::open(path).map_err(|e| e.to_string())?,
            &mut output,
            MAX_TOTAL,
        )?;
        output.as_file().sync_all().map_err(|e| e.to_string())?;
        crate::paths::harden_private_file(output.path())?;
        let temporary = tempfile::tempdir_in(home).map_err(|e| e.to_string())?;
        let manifest = match unpack(output.path(), temporary.path()) {
            Ok(value) => value,
            Err(error) => {
                let _ = fs::remove_file(copy);
                return Err(error);
            }
        };
        output.persist(&copy).map_err(|e| e.to_string())?;
        Ok::<_, String>((
            Preview {
                token,
                created_at: manifest.created_at,
                file_count: manifest.files.len(),
                total_bytes: manifest.files.iter().map(|entry| entry.bytes).sum(),
                ui_keys: manifest.ui_state.keys().cloned().collect(),
            },
            copy,
        ))
    })
    .await
    .map_err(|e| e.to_string())??;
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    if let Some((_, old)) = guard.take() {
        let _ = fs::remove_file(old);
    }
    *guard = Some((preview.token.clone(), copy));
    Ok(Some(preview))
}
#[tauri::command]
pub fn backup_restore(
    app: AppHandle,
    state: State<'_, BackupState>,
    token: String,
) -> Result<(), String> {
    let mut guard = state.0.lock().map_err(|e| e.to_string())?;
    let (expected, path) = guard.as_ref().ok_or("请先选择并校验备份")?;
    if expected != &token {
        return Err("备份预览已过期，请重新选择".into());
    }
    let source = path.clone();
    let pending = crate::paths::echo_agent_home_dir().join("restore-pending.zip");
    fs::rename(&source, &pending).map_err(|e| e.to_string())?;
    if let Err(error) = crate::request_graceful_restart(app) {
        fs::rename(&pending, &source)
            .map_err(|rollback| format!("{error}；备份文件回退失败：{rollback}"))?;
        return Err(error);
    }
    guard.take();
    Ok(())
}
#[tauri::command]
pub fn backup_restored_ui() -> Result<Option<BTreeMap<String, String>>, String> {
    let path = crate::paths::echo_agent_home_dir().join("restore-ui.json");
    if !path.exists() {
        return Ok(None);
    }
    serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map(Some)
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn backup_acknowledge_ui() -> Result<(), String> {
    let home = crate::paths::echo_agent_home_dir();
    let path = home.join("restore-ui.json");
    if path.exists() {
        let error = home.join("restore-error.txt");
        if error.exists() {
            fs::remove_file(error).map_err(|e| e.to_string())?;
        }
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_round_trip_excludes_credentials_and_detects_corruption() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("config.toml"), "secret").unwrap();
        fs::write(home.path().join("echoagent-projects.json"), "[]").unwrap();
        let output = tempfile::tempdir().unwrap();
        let archive = output.path().join("backup.zip");
        export(home.path(), home.path(), &archive, BTreeMap::new()).unwrap();
        let staged = tempfile::tempdir().unwrap();
        let manifest = unpack(&archive, staged.path()).unwrap();
        assert_eq!(manifest.files.len(), 1);
        assert!(!staged.path().join("config.toml").exists());
        assert_eq!(
            fs::read_to_string(staged.path().join("echoagent-projects.json")).unwrap(),
            "[]"
        );
        let mut bad = manifest;
        bad.files[0].name = "sessions/../../config.toml".into();
        assert!(validate(&bad).is_err());
        assert!(!allowed("/tmp/escape"));
        assert!(!allowed("sessions/../config.toml"));
        assert!(!allowed("sessions/./alias"));
        assert!(!allowed("sessions//alias"));
        assert!(!allowed("sessions/data:stream"));
    }
}

#[tauri::command]
pub fn backup_last_error() -> Result<Option<String>, String> {
    let path = crate::paths::echo_agent_home_dir().join("restore-error.txt");
    if !path.exists() {
        return Ok(None);
    }
    fs::read_to_string(path)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    #[test]
    fn restore_is_committed_once_and_pauses_work_without_replaying_it() {
        let source = tempfile::tempdir().unwrap();
        fs::write(
            source.path().join("echoagent-automations.json"),
            r#"{"automations":[{"status":"ACTIVE"}]}"#,
        )
        .unwrap();
        fs::write(source.path().join("echoagent-automation-records.json"), r#"{"records":[{"status":"queued","automationSnapshot":{}},{"status":"running"},{"status":"success"}]}"#).unwrap();
        fs::create_dir_all(source.path().join("clipboard-images/item")).unwrap();
        fs::write(
            source.path().join("clipboard-images/item/image.png"),
            b"image",
        )
        .unwrap();
        let target = tempfile::tempdir().unwrap();
        let ui = BTreeMap::from([(
            "echoagent.draft-attachments.v1".into(),
            format!(
                r#"{{"version":1,"data":{{"s":["{}/clipboard-images/item/image.png"]}}}}"#,
                source.path().display()
            ),
        )]);
        export(
            source.path(),
            source.path(),
            &target.path().join("restore-pending.zip"),
            ui,
        )
        .unwrap();
        apply_pending_at(target.path(), target.path()).unwrap();
        assert!(!target.path().join("restore-pending.zip").exists());
        assert!(!target.path().join("restore-journal.json").exists());
        let automations: serde_json::Value = serde_json::from_slice(
            &fs::read(target.path().join("echoagent-automations.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(automations["automations"][0]["status"], "PAUSED");
        let records: serde_json::Value = serde_json::from_slice(
            &fs::read(target.path().join("echoagent-automation-records.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(records["records"][0]["status"], "failed");
        assert_eq!(records["records"][1]["status"], "failed");
        assert_eq!(records["records"][2]["status"], "success");
        assert_eq!(
            fs::read(target.path().join("clipboard-images/item/image.png")).unwrap(),
            b"image"
        );
        let ui: BTreeMap<String, String> =
            serde_json::from_slice(&fs::read(target.path().join("restore-ui.json")).unwrap())
                .unwrap();
        assert!(ui["echoagent.draft-attachments.v1"]
            .contains(&target.path().to_string_lossy().to_string()));
        apply_pending_at(target.path(), target.path()).unwrap();
        assert!(target.path().join("restore-ui.json").exists());
    }

    #[test]
    fn interrupted_restore_rolls_back_but_committed_restore_does_not() {
        for committed in [false, true] {
            let home = tempfile::tempdir().unwrap();
            fs::create_dir(home.path().join("restore-previous-test")).unwrap();
            fs::write(
                home.path()
                    .join("restore-previous-test/echoagent-projects.json"),
                b"old",
            )
            .unwrap();
            fs::write(home.path().join("echoagent-projects.json"), b"new").unwrap();
            fs::write(home.path().join("restore-ui.json"), b"new-ui").unwrap();
            let journal = Journal {
                files: vec![("echoagent-projects.json".into(), true)],
                previous: "restore-previous-test".into(),
                committed,
                old_ui: Some(b"old-ui".to_vec()),
            };
            fs::write(
                home.path().join("restore-journal.json"),
                serde_json::to_vec(&journal).unwrap(),
            )
            .unwrap();
            rollback(home.path(), home.path()).unwrap();
            assert_eq!(
                fs::read(home.path().join("echoagent-projects.json")).unwrap(),
                if committed { b"new" } else { b"old" }
            );
            assert_eq!(
                fs::read(home.path().join("restore-ui.json")).unwrap(),
                if committed { b"new-ui" } else { b"old-ui" }
            );
        }
    }

    #[test]
    fn damaged_archive_never_replaces_existing_data() {
        let home = tempfile::tempdir().unwrap();
        fs::write(home.path().join("echoagent-projects.json"), b"keep").unwrap();
        let manifest = Manifest {
            version: 1,
            created_at: "now".into(),
            home: String::new(),
            app_data: String::new(),
            ui_state: BTreeMap::new(),
            files: vec![Entry {
                name: "echoagent-projects.json".into(),
                bytes: 4,
                sha256: "0".repeat(64),
            }],
        };
        let mut zip =
            ZipWriter::new(File::create(home.path().join("restore-pending.zip")).unwrap());
        zip.start_file("echoagent-projects.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"evil").unwrap();
        zip.start_file("manifest.json", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.finish().unwrap();
        assert!(apply_pending_at(home.path(), home.path())
            .unwrap_err()
            .contains("校验失败"));
        assert_eq!(
            fs::read(home.path().join("echoagent-projects.json")).unwrap(),
            b"keep"
        );
    }
}
