//! Versioned, streaming backups. Restore is staged and applied before services
//! start, with a rollback journal so an interrupted restore is recoverable.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

const MAX_TOTAL: u64 = 8 * 1024 * 1024 * 1024;
const FILES: &[&str] = &[
    "echoagent-projects.json",
    "echoagent-state.json",
    "echoagent-automations.json",
    "echoagent-automation-records.json",
    "echoagent-notifications.json",
    "echoagent-teams.json",
    "collaboration.sqlite",
    "desktop/desktop-preferences.json",
];
const DIRS: &[&str] = &[
    "knowledge-traces",
    "automation-terminal-outbox",
    "session-artifacts",
    "sessions",
    "memory",
    "group-memory",
    "projects",
    "meetings",
    "coding",
    "clipboard-images",
];
pub const UI_KEYS: &[&str] = &[
    "echoagent.projects",
    "echoagent.drafts.v1",
    "echoagent.recovery-session.v1",
    "echoagent.group-drafts.v1",
    "echoagent.group-read.v1",
    "echoagent.group-outbox.v1",
    "echoagent.outbox.v1",
    "echoagent.draft-attachments.v1",
    "echoagent.task-artifacts.v1",
    "echo.meeting-drafts.v1",
    "echo.meeting-drafts.v1.recovery",
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
pub struct BackupState(Mutex<Option<(String, PathBuf)>>, AtomicBool);

struct RestoreOperation<'a> {
    flag: &'a AtomicBool,
    release: bool,
}

impl<'a> RestoreOperation<'a> {
    fn begin(flag: &'a AtomicBool) -> Result<Self, String> {
        if flag
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("备份恢复正在进行，请稍候".into());
        }
        Ok(Self {
            flag,
            release: true,
        })
    }

    fn keep_until_restart(mut self) {
        self.release = false;
    }
}

impl Drop for RestoreOperation<'_> {
    fn drop(&mut self) {
        if self.release {
            self.flag.store(false, Ordering::Release);
        }
    }
}

/// Create the pending file without replacing another restore. Keep the
/// validated candidate intact until restart has actually been scheduled.
fn stage_restore_candidate(source: &Path, pending: &Path) -> Result<(), String> {
    match fs::hard_link(source, pending) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err("已有待恢复备份，不能覆盖；请先处理或重启应用".into())
        }
        Err(_) => {
            let parent = pending.parent().ok_or("备份恢复目录无效")?;
            let mut copy = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            copy_hash(
                File::open(source).map_err(|e| e.to_string())?,
                &mut copy,
                MAX_TOTAL,
            )?;
            copy.as_file().sync_all().map_err(|e| e.to_string())?;
            crate::paths::harden_private_file(copy.path())?;
            copy.persist_noclobber(pending)
                .map_err(|e| format!("无法准备恢复备份（不会覆盖已有文件）：{e}"))?;
            Ok(())
        }
    }
}
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
            // Read a consistent SQLite snapshot, including committed WAL data.
            // Copying the live main file alone can lose the latest messages.
            let sqlite_snapshot = if name == "collaboration.sqlite" {
                let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
                let destination = dir.path().join("snapshot.sqlite");
                let connection = rusqlite::Connection::open_with_flags(
                    file.path(),
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )
                .map_err(|e| e.to_string())?;
                connection
                    .execute("VACUUM INTO ?1", [destination.to_string_lossy().as_ref()])
                    .map_err(|e| e.to_string())?;
                Some((dir, destination))
            } else {
                None
            };
            let (bytes, sha256) = copy_hash(
                File::open(
                    sqlite_snapshot
                        .as_ref()
                        .map(|(_, p)| p.as_path())
                        .unwrap_or(file.path()),
                )
                .map_err(|e| e.to_string())?,
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
    crate::paths::replace_file_atomically(temporary.path(), destination)?;
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
        if !names.insert(
            file.name()
                .map_err(|error| format!("备份 ZIP 文件名无效：{error}"))?
                .into_owned(),
        ) {
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
    // Sidecars are journal-only entries. `validate` never accepts them from an
    // archive: the exported database is already a consistent single-file copy.
    if !allowed(name)
        && !matches!(
            name,
            "collaboration.sqlite-wal" | "collaboration.sqlite-shm"
        )
    {
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
    crate::paths::replace_file_atomically(temporary.path(), target)?;
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
/// Keep the authoritative encoded CWD directory consistent with the paths in
/// restored summaries. Rewriting JSON alone makes discovery reject every row.
fn rebase_session_directories(
    staging: &Path,
    manifest: &mut Manifest,
    home: &Path,
    app_data: &Path,
) -> Result<Vec<PathBuf>, String> {
    use echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname;
    let mut directories = BTreeMap::new();
    for entry in &manifest.files {
        let Some(rest) = entry.name.strip_prefix("sessions/") else {
            continue;
        };
        let Some((directory, _)) = rest.split_once('/') else {
            continue;
        };
        if directories.contains_key(directory) {
            continue;
        }
        let decoded = urlencoding::decode(directory).map_err(|e| e.to_string())?;
        let old_cwd = if Path::new(decoded.as_ref()).is_absolute() {
            decoded.into_owned()
        } else {
            let sidecar = staging.join("sessions").join(directory).join(".cwd");
            if !sidecar.is_file() {
                continue;
            }
            String::from_utf8(crate::shell_fs::read_regular_file_bounded(
                &sidecar,
                16 * 1024,
            )?)
            .map_err(|e| e.to_string())?
            .trim()
            .to_string()
        };
        // Do not let a forged sidecar redirect another directory's records.
        if old_cwd.chars().count() > 4096 || encode_cwd_dirname(&old_cwd) != directory {
            continue;
        }
        let mut value = serde_json::Value::String(old_cwd.clone());
        rebase_value(&mut value, manifest, home, app_data);
        let new_cwd = value.as_str().ok_or("会话工作区无效")?.to_string();
        // A reinstall can restore into exactly the original data-home path.
        // Collect every verified CWD so missing application-owned task folders
        // are recreated even when no archive path needs to move.
        directories.insert(
            directory.to_string(),
            (encode_cwd_dirname(&new_cwd), new_cwd),
        );
    }
    if directories.is_empty() {
        return Ok(Vec::new());
    }
    let mut targets = HashSet::new();
    let mut moves = Vec::new();
    for entry in &manifest.files {
        let mut name = entry.name.clone();
        if let Some((directory, tail)) = entry
            .name
            .strip_prefix("sessions/")
            .and_then(|s| s.split_once('/'))
        {
            if let Some((replacement, _)) = directories.get(directory) {
                name = format!("sessions/{replacement}/{tail}");
            }
        }
        if !allowed(&name) || !targets.insert(name.clone()) {
            return Err("恢复后的会话路径冲突".into());
        }
        moves.push((entry.name.clone(), name));
    }
    // Snapshot all moving files first, so two source directories cannot clobber
    // one another while the verified archive is normalized in private staging.
    let snapshot = tempfile::tempdir_in(staging).map_err(|e| e.to_string())?;
    for (old, new) in &moves {
        if old != new {
            atomic_copy(&staging.join(old), &snapshot.path().join(new))?;
        }
    }
    for (entry, (old, new)) in manifest.files.iter_mut().zip(&moves) {
        if old != new {
            atomic_copy(&snapshot.path().join(new), &staging.join(new))?;
            entry.name = new.clone();
        }
    }
    let mut managed = Vec::new();
    for (original, (directory, cwd)) in &directories {
        if original != directory {
            let name = format!("sessions/{directory}/.cwd");
            crate::paths::write_private_file(&staging.join(&name), cwd.as_bytes())?;
            let metadata = Entry {
                name: name.clone(),
                bytes: cwd.len() as u64,
                sha256: format!("{:x}", Sha256::digest(cwd.as_bytes())),
            };
            if let Some(entry) = manifest.files.iter_mut().find(|e| e.name == name) {
                *entry = metadata;
            } else {
                manifest.files.push(metadata);
            }
        }
        // Only application-owned task folders may be recreated. External
        // projects remain references and require the original workspace.
        let root = home.join("workspaces/default/Tasks");
        if let Ok(relative) = Path::new(cwd).strip_prefix(&root) {
            if relative.components().count() == 1
                && uuid::Uuid::parse_str(&relative.to_string_lossy()).is_ok()
            {
                managed.push(PathBuf::from(cwd));
            }
        }
    }
    validate(manifest)?;
    Ok(managed)
}

fn restore_managed_task_directory(home: &Path, cwd: &Path) -> Result<(), String> {
    let root = home.join("workspaces/default/Tasks");
    let relative = cwd.strip_prefix(&root).map_err(|_| "恢复工作区超出范围")?;
    if relative.components().count() != 1
        || uuid::Uuid::parse_str(&relative.to_string_lossy()).is_err()
    {
        return Err("恢复工作区标识无效".into());
    }
    for ancestor in cwd.ancestors().take_while(|p| p.starts_with(home)) {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err("恢复工作区包含链接或非目录".into())
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    fs::create_dir_all(cwd).map_err(|e| e.to_string())?;
    crate::paths::harden_private_dir(cwd)
}

fn prepare_file(
    path: &Path,
    name: &str,
    manifest: &Manifest,
    home: &Path,
    app_data: &Path,
) -> Result<(), String> {
    if name == "collaboration.sqlite" {
        return prepare_group_database(path, manifest, home, app_data);
    }
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
        crate::paths::replace_file_atomically(output.path(), path)?;
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
    crate::paths::write_private_file(
        path,
        &serde_json::to_vec(&value).map_err(|e| e.to_string())?,
    )
}

/// The database is restored before services open it. Rebase structured paths
/// in the same transaction and reject malformed archives before replacing data.
fn prepare_group_database(
    path: &Path,
    manifest: &Manifest,
    home: &Path,
    app_data: &Path,
) -> Result<(), String> {
    let mut connection =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|e| format!("专家团备份数据库无效：{e}"))?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|e| format!("无法验证专家团数据库：{e}"))?;
    if integrity != "ok" {
        return Err("专家团数据库完整性验证失败".into());
    }
    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    for table in ["groups", "messages", "work_items", "team_templates"] {
        let rows = {
            let mut statement = transaction
                .prepare(&format!("SELECT id,data FROM {table}"))
                .map_err(|e| format!("专家团备份表不完整：{e}"))?;
            let result = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            result
        };
        for (id, data) in rows {
            let mut value: serde_json::Value =
                serde_json::from_str(&data).map_err(|e| format!("专家团备份记录损坏：{e}"))?;
            rebase_value(&mut value, manifest, home, app_data);
            transaction
                .execute(
                    &format!("UPDATE {table} SET data=?1 WHERE id=?2"),
                    rusqlite::params![value.to_string(), id],
                )
                .map_err(|e| e.to_string())?;
        }
    }
    let has_deletions: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='group_deletions')", [], |row| row.get(0)).map_err(|e| e.to_string())?;
    if has_deletions {
        let entries = {
            let mut query = transaction
                .prepare("SELECT group_id,session_id,cwd FROM group_deletions")
                .map_err(|e| e.to_string())?;
            let rows = query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        for (group, session, cwd) in entries {
            let mut value = serde_json::Value::String(cwd);
            rebase_value(&mut value, manifest, home, app_data);
            transaction
                .execute(
                    "UPDATE group_deletions SET cwd=?1 WHERE group_id=?2 AND session_id=?3",
                    rusqlite::params![value.as_str().ok_or("删除重试路径无效")?, group, session],
                )
                .map_err(|e| e.to_string())?;
        }
    }
    // Derived metadata is rebuilt from rebased records when the store opens.
    let has_previews: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='group_previews')", [], |row| row.get(0)).map_err(|e| e.to_string())?;
    if has_previews {
        transaction
            .execute("DELETE FROM group_previews", [])
            .map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())?;
    connection
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")
        .map_err(|e| e.to_string())?;
    Ok(())
}
fn apply_pending_at(home: &Path, app_data: &Path) -> Result<(), String> {
    rollback(home, app_data)?;
    let pending = home.join("restore-pending.zip");
    if !pending.exists() {
        return Ok(());
    }
    let staging = tempfile::tempdir_in(home).map_err(|e| e.to_string())?;
    let mut manifest = unpack(&pending, staging.path())?;
    let managed_task_directories =
        rebase_session_directories(staging.path(), &mut manifest, home, app_data)?;
    let retaining_target_history = !manifest
        .files
        .iter()
        .any(|entry| entry.name == "echoagent-automation-records.json")
        && home.join("echoagent-automation-records.json").exists();
    // Overlay backups can predate the first automation. Existing target work
    // still needs the same pause/close treatment as work in the archive.
    for name in [
        "echoagent-automations.json",
        "echoagent-automation-records.json",
    ] {
        let target = home.join(name);
        if !manifest.files.iter().any(|entry| entry.name == name) && target.exists() {
            let bytes = crate::shell_fs::read_regular_file_bounded(&target, 64 * 1024 * 1024)?;
            crate::paths::write_private_file(&staging.path().join(name), &bytes)?;
            manifest.files.push(Entry {
                name: name.into(),
                bytes: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
            });
        }
    }
    if retaining_target_history && home.join("automation-terminal-outbox").exists() {
        let history: serde_json::Value =
            serde_json::from_slice(&crate::shell_fs::read_regular_file_bounded(
                &staging.path().join("echoagent-automation-records.json"),
                64 * 1024 * 1024,
            )?)
            .map_err(|e| e.to_string())?;
        let names: HashSet<String> = history["records"]
            .as_array()
            .ok_or("自动化历史格式无效")?
            .iter()
            .filter_map(|record| record["id"].as_str())
            .map(|id| format!("{:x}.json", Sha256::digest(id.as_bytes())))
            .collect();
        for entry in
            fs::read_dir(home.join("automation-terminal-outbox")).map_err(|e| e.to_string())?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let filename = entry
                .file_name()
                .into_string()
                .map_err(|_| "无效完成事实文件名")?;
            let name = format!("automation-terminal-outbox/{filename}");
            if !names.contains(&filename) || manifest.files.iter().any(|file| file.name == name) {
                continue;
            }
            // Retained target history has the same authoritative terminal
            // facts as archived history. Reconcile before closing its queue.
            let bytes = crate::shell_fs::read_regular_file_bounded(&entry.path(), 4 * 1024 * 1024)?;
            crate::paths::write_private_file(&staging.path().join(&name), &bytes)?;
            manifest.files.push(Entry {
                name,
                bytes: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
            });
        }
    }
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
    crate::automations::prepare_restored_terminals(staging.path())?;
    // Overlay restore must not leave target-only completion jobs available to
    // the next scheduler. Include removals in the existing rollback journal.
    let mut obsolete_terminals = Vec::new();
    let old_outbox = home.join("automation-terminal-outbox");
    if old_outbox.exists() {
        for entry in fs::read_dir(&old_outbox).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = format!(
                "automation-terminal-outbox/{}",
                entry.file_name().to_str().ok_or("无效完成事实文件名")?
            );
            if manifest.files.iter().any(|file| file.name == name) {
                continue;
            }
            if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                return Err("自动化完成事实目录存在非普通文件".into());
            }
            atomic_copy(&entry.path(), &previous.join(&name))?;
            journal.files.push((name.clone(), true));
            obsolete_terminals.push(name);
        }
    }
    let restoring_groups = manifest
        .files
        .iter()
        .any(|e| e.name == "collaboration.sqlite");
    if restoring_groups {
        for name in ["collaboration.sqlite-wal", "collaboration.sqlite-shm"] {
            let target = destination(home, app_data, name)?;
            let existed = target.exists();
            if existed {
                atomic_copy(&target, &previous.join(name))?;
            }
            journal.files.push((name.into(), existed));
        }
    }
    let journal_path = home.join("restore-journal.json");
    crate::paths::write_private_file(
        &journal_path,
        &serde_json::to_vec(&journal).map_err(|e| e.to_string())?,
    )?;
    let result = (|| {
        for name in &obsolete_terminals {
            fs::remove_file(destination(home, app_data, name)?).map_err(|e| e.to_string())?;
        }
        if restoring_groups {
            for name in ["collaboration.sqlite-wal", "collaboration.sqlite-shm"] {
                let target = destination(home, app_data, name)?;
                if target.exists() {
                    fs::remove_file(target).map_err(|e| e.to_string())?;
                }
            }
        }
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
        for cwd in &managed_task_directories {
            restore_managed_task_directory(home, cwd)?;
        }
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
    let app_data = crate::paths::desktop_data_dir(app)?;
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
    let app_data = crate::paths::desktop_data_dir(&app)?;
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
    if state.1.load(Ordering::Acquire) {
        return Err("备份恢复正在进行，请稍候".into());
    }
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
        crate::paths::replace_file_atomically(output.path(), &copy)?;
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
    if state.1.load(Ordering::Acquire) {
        let _ = fs::remove_file(copy);
        return Err("备份恢复已开始，当前选择不会替换正在恢复的备份".into());
    }
    if let Some((_, old)) = guard.take() {
        let _ = fs::remove_file(old);
    }
    *guard = Some((preview.token.clone(), copy));
    Ok(Some(preview))
}
#[tauri::command]
pub async fn backup_restore(
    app: AppHandle,
    state: State<'_, BackupState>,
    token: String,
) -> Result<(), String> {
    let operation = RestoreOperation::begin(&state.1)?;
    let source = {
        let mut guard = state.0.lock().map_err(|e| e.to_string())?;
        let (expected, path) = guard.as_ref().ok_or("请先选择并校验备份")?;
        if expected != &token {
            return Err("备份预览已过期，请重新选择".into());
        }
        let path = path.clone();
        guard.take();
        path
    };
    let pending = crate::paths::echo_agent_home_dir().join("restore-pending.zip");
    // Recording confirmation must succeed before a pending restore is made
    // visible to the next startup; a refused exit never schedules restoration.
    if let Err(error) = crate::prepare_application_exit(&app).await {
        *state.0.lock().map_err(|e| e.to_string())? = Some((token, source));
        return Err(error);
    }
    if let Err(error) = stage_restore_candidate(&source, &pending) {
        crate::abort_prepared_exit(&app, error.clone(), false);
        let mut guard = state.0.lock().map_err(|e| e.to_string())?;
        if guard.is_none() {
            *guard = Some((token, source));
        }
        return Err(error);
    }
    if let Err(error) = crate::restart_prepared_application(app).await {
        if let Err(rollback) = fs::remove_file(&pending) {
            operation.keep_until_restart();
            return Err(format!(
                "{error}；待恢复文件撤销失败：{rollback}。原备份已保留，请先修复目录权限"
            ));
        }
        let mut guard = state.0.lock().map_err(|e| e.to_string())?;
        if guard.is_none() {
            *guard = Some((token, source));
        }
        return Err(error);
    }
    if let Err(error) = fs::remove_file(&source) {
        tracing::warn!(%error, "validated backup candidate retained after scheduling restart");
    }
    operation.keep_until_restart();
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
    fn same_home_restore_recreates_missing_managed_tasks_without_fabricating_files() {
        use echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname;
        for long_home in [false, true] {
            let base = tempfile::tempdir().unwrap();
            // Long paths use several legal components, rather than a single
            // component exceeding the filesystem's 255-byte name limit.
            let home = if long_home {
                base.path()
                    .join("profile".repeat(20))
                    .join("nested".repeat(20))
            } else {
                base.path().join("profile")
            };
            let cwd = home.join("workspaces/default/Tasks/00000000-0000-4000-8000-000000000001");
            fs::create_dir_all(&cwd).unwrap();
            fs::write(cwd.join("business.txt"), "not part of the history backup").unwrap();
            let directory = encode_cwd_dirname(&cwd.to_string_lossy());
            let session_root = home.join("sessions").join(&directory);
            fs::create_dir_all(session_root.join("saved-session")).unwrap();
            if long_home {
                fs::write(session_root.join(".cwd"), cwd.to_string_lossy().as_bytes()).unwrap();
            }
            fs::write(
                session_root.join("saved-session/summary.json"),
                serde_json::json!({
                    "session_id":"saved-session", "cwd":cwd, "generated_title":"同路径恢复"
                })
                .to_string(),
            )
            .unwrap();
            let archive = home.join("restore-pending.zip");
            export(&home, &home, &archive, BTreeMap::new()).unwrap();
            fs::remove_dir_all(home.join("sessions")).unwrap();
            fs::remove_dir_all(home.join("workspaces")).unwrap();
            apply_pending_at(&home, &home).unwrap();
            assert!(
                cwd.is_dir(),
                "restoring to the same home must produce an openable task"
            );
            assert!(fs::read_dir(&cwd).unwrap().next().is_none());
            let sessions =
                crate::sessions::list_all_sessions_from_root(&home.join("sessions")).unwrap();
            assert_eq!(sessions.len(), 1);
            assert_eq!(Path::new(&sessions[0].cwd), cwd.canonicalize().unwrap());
        }
    }

    #[test]
    fn same_home_restore_preserves_existing_managed_workspace_files() {
        let home = tempfile::tempdir().unwrap();
        let cwd = home
            .path()
            .join("workspaces/default/Tasks/00000000-0000-4000-8000-000000000001");
        fs::create_dir_all(&cwd).unwrap();
        let directory =
            echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd.to_string_lossy());
        let session = home
            .path()
            .join("sessions")
            .join(directory)
            .join("saved-session");
        fs::create_dir_all(&session).unwrap();
        fs::write(
            session.join("summary.json"),
            serde_json::json!({"session_id":"saved-session","cwd":cwd}).to_string(),
        )
        .unwrap();
        export(
            home.path(),
            home.path(),
            &home.path().join("restore-pending.zip"),
            BTreeMap::new(),
        )
        .unwrap();
        let business = cwd.join("business.txt");
        fs::write(&business, "newer workspace content").unwrap();
        apply_pending_at(home.path(), home.path()).unwrap();
        assert_eq!(
            fs::read_to_string(business).unwrap(),
            "newer workspace content"
        );
    }

    #[test]
    fn restored_sessions_rebase_authoritative_directories_and_openable_managed_cwds() {
        use echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname;
        // Exercise both URL-encoded -> hashed and hashed -> URL-encoded paths.
        for long_source in [false, true] {
            let source_base = tempfile::tempdir().unwrap();
            let target_base = tempfile::tempdir().unwrap();
            let deep = "long-profile-".repeat(13);
            let source = if long_source {
                source_base.path().join(&deep)
            } else {
                source_base.path().join("source")
            };
            let target = if long_source {
                target_base.path().join("target")
            } else {
                target_base.path().join(&deep)
            };
            fs::create_dir_all(&source).unwrap();
            fs::create_dir_all(&target).unwrap();
            let task = "00000000-0000-4000-8000-000000000001";
            let old_cwd = source.join("workspaces/default/Tasks").join(task);
            let new_cwd = target.join("workspaces/default/Tasks").join(task);
            fs::create_dir_all(&old_cwd).unwrap();
            let old_directory = encode_cwd_dirname(&old_cwd.to_string_lossy());
            let directory = source.join("sessions").join(&old_directory);
            fs::create_dir_all(directory.join("saved-session")).unwrap();
            fs::write(directory.join(".cwd"), old_cwd.to_string_lossy().as_bytes()).unwrap();
            fs::write(
                directory.join("saved-session/summary.json"),
                serde_json::json!({
                    "session_id":"saved-session", "cwd":old_cwd, "generated_title":"恢复验收"
                })
                .to_string(),
            )
            .unwrap();
            // Control reproduces the old JSON-only rewrite: the catalog rejects
            // the mismatch between the CWD and its authoritative directory.
            let control = tempfile::tempdir().unwrap();
            let wrong = control.path().join(&old_directory).join("saved-session");
            fs::create_dir_all(&wrong).unwrap();
            fs::write(
                wrong.join("summary.json"),
                serde_json::json!({"session_id":"saved-session", "cwd":new_cwd}).to_string(),
            )
            .unwrap();
            assert!(crate::sessions::list_all_sessions_from_root(control.path())
                .unwrap()
                .is_empty());
            export(
                &source,
                &source,
                &target.join("restore-pending.zip"),
                BTreeMap::new(),
            )
            .unwrap();
            apply_pending_at(&target, &target).unwrap();
            let rows =
                crate::sessions::list_all_sessions_from_root(&target.join("sessions")).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].session_id, "saved-session");
            assert_eq!(Path::new(&rows[0].cwd), new_cwd.canonicalize().unwrap());
            let new_directory = target
                .join("sessions")
                .join(encode_cwd_dirname(&new_cwd.to_string_lossy()));
            assert_eq!(
                fs::read_to_string(new_directory.join(".cwd")).unwrap(),
                new_cwd.to_string_lossy()
            );
            assert!(new_cwd.is_dir());
            assert!(
                fs::read_dir(&new_cwd).unwrap().next().is_none(),
                "History recovery must not fabricate business files"
            );
        }
    }

    #[test]
    fn restore_does_not_create_external_or_forged_workspaces() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let cwd = outside.path().join("missing-repository");
        let directory =
            echo_agent_runtime::util::echo_agent_home::encode_cwd_dirname(&cwd.to_string_lossy());
        let session = source
            .path()
            .join("sessions")
            .join(directory)
            .join("external-session");
        fs::create_dir_all(&session).unwrap();
        fs::write(
            session.join("summary.json"),
            serde_json::json!({"session_id":"external-session", "cwd":cwd}).to_string(),
        )
        .unwrap();
        export(
            source.path(),
            source.path(),
            &target.path().join("restore-pending.zip"),
            BTreeMap::new(),
        )
        .unwrap();
        apply_pending_at(target.path(), target.path()).unwrap();
        assert!(!cwd.exists());
        assert_eq!(
            crate::sessions::list_all_sessions_from_root(&target.path().join("sessions"))
                .unwrap()
                .len(),
            1
        );
        assert!(restore_managed_task_directory(
            target.path(),
            &target.path().join("workspaces/default/Tasks/../../escape")
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn restored_managed_task_rejects_a_linked_ancestor() {
        let target = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), target.path().join("workspaces")).unwrap();
        let cwd = target
            .path()
            .join("workspaces/default/Tasks/00000000-0000-4000-8000-000000000001");
        assert!(restore_managed_task_directory(target.path(), &cwd).is_err());
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[test]
    fn group_restore_accepts_legacy_database_without_cleanup_tables() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let c = rusqlite::Connection::open(source.path().join("collaboration.sqlite")).unwrap();
        c.execute_batch("CREATE TABLE groups(id TEXT PRIMARY KEY,data TEXT); CREATE TABLE messages(id TEXT PRIMARY KEY,data TEXT); CREATE TABLE work_items(id TEXT PRIMARY KEY,data TEXT); CREATE TABLE team_templates(id TEXT PRIMARY KEY,data TEXT);").unwrap();
        let archive = target.path().join("restore-pending.zip");
        export(source.path(), source.path(), &archive, BTreeMap::new()).unwrap();
        apply_pending_at(target.path(), target.path()).unwrap();
        assert!(target.path().join("collaboration.sqlite").exists());
    }

    #[test]
    fn group_database_backup_includes_wal_and_restore_rebases_paths_and_removes_old_sidecars() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let connection =
            rusqlite::Connection::open(source.path().join("collaboration.sqlite")).unwrap();
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE groups(id TEXT PRIMARY KEY,data TEXT); CREATE TABLE messages(id TEXT PRIMARY KEY,data TEXT); CREATE TABLE work_items(id TEXT PRIMARY KEY,data TEXT); CREATE TABLE team_templates(id TEXT PRIMARY KEY,data TEXT);").unwrap();
        let path = source.path().join("projects/shared/file.md");
        connection.execute_batch("CREATE TABLE group_deletions(group_id TEXT,session_id TEXT,cwd TEXT,PRIMARY KEY(group_id,session_id)); CREATE TABLE group_previews(id TEXT PRIMARY KEY,data TEXT,updated INTEGER); INSERT INTO group_previews VALUES('cached','{}',1);").unwrap();
        connection
            .execute(
                "INSERT INTO group_deletions VALUES('group','native',?1)",
                [source
                    .path()
                    .join("projects/shared")
                    .to_string_lossy()
                    .as_ref()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO messages VALUES('latest',?1)",
                [serde_json::json!({"attachments":[path]}).to_string()],
            )
            .unwrap();
        let archive = target.path().join("restore-pending.zip");
        export(source.path(), source.path(), &archive, BTreeMap::new()).unwrap();
        fs::write(
            target.path().join("collaboration.sqlite-wal"),
            "unrelated old WAL",
        )
        .unwrap();
        fs::write(
            target.path().join("collaboration.sqlite-shm"),
            "unrelated old SHM",
        )
        .unwrap();
        apply_pending_at(target.path(), target.path()).unwrap();
        assert!(!target.path().join("collaboration.sqlite-wal").exists());
        assert!(!target.path().join("collaboration.sqlite-shm").exists());
        let restored =
            rusqlite::Connection::open(target.path().join("collaboration.sqlite")).unwrap();
        let data: String = restored
            .query_row("SELECT data FROM messages WHERE id='latest'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let data: serde_json::Value = serde_json::from_str(&data).unwrap();
        let retry_cwd: String = restored
            .query_row(
                "SELECT cwd FROM group_deletions WHERE group_id='group'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            retry_cwd,
            target.path().join("projects/shared").to_string_lossy()
        );
        assert_eq!(
            restored
                .query_row("SELECT count(*) FROM group_previews", [], |row| row
                    .get::<_, usize>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            data["attachments"][0],
            target
                .path()
                .join("projects/shared/file.md")
                .to_string_lossy()
                .as_ref()
        );
    }

    #[test]
    fn restoration_is_exclusive_and_failures_release_the_lease() {
        let flag = AtomicBool::new(false);
        let operation = RestoreOperation::begin(&flag).unwrap();
        assert!(RestoreOperation::begin(&flag).is_err());
        drop(operation);
        assert!(RestoreOperation::begin(&flag).is_ok());
    }

    #[test]
    fn successful_restore_lease_stays_exclusive_until_restart() {
        let flag = AtomicBool::new(false);
        RestoreOperation::begin(&flag).unwrap().keep_until_restart();
        assert!(RestoreOperation::begin(&flag).is_err());
    }

    #[test]
    fn restore_staging_preserves_candidate_and_never_replaces_another_pending_backup() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("candidate.zip");
        let another = root.path().join("another.zip");
        let pending = root.path().join("restore-pending.zip");
        fs::write(&source, b"validated candidate").unwrap();
        fs::write(&another, b"different backup").unwrap();
        stage_restore_candidate(&source, &pending).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"validated candidate");
        assert!(stage_restore_candidate(&another, &pending).is_err());
        assert_eq!(fs::read(&pending).unwrap(), b"validated candidate");
        fs::remove_file(&pending).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"validated candidate");
    }
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
    fn overlay_without_automation_files_pauses_existing_target_work() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(
            target.path().join("echoagent-automations.json"),
            r#"{"automations":[{"id":"keep","status":"ACTIVE"}]}"#,
        )
        .unwrap();
        fs::write(target.path().join("echoagent-automation-records.json"), r#"{"records":[{"id":"queued","status":"queued","automationSnapshot":{}},{"id":"completed","status":"success"},{"id":"durable","automationId":"keep","automationName":"finished","status":"running","startedAt":"start","sessionId":"session","promptId":"prompt"}]}"#).unwrap();
        fs::create_dir_all(target.path().join("automation-terminal-outbox")).unwrap();
        let fact_name = format!("{:x}.json", Sha256::digest(b"durable"));
        fs::write(target.path().join("automation-terminal-outbox").join(&fact_name), r#"{"record_id":"durable","session_id":"session","prompt_id":"prompt","ok":true,"error":null,"finished_at":"original-end"}"#).unwrap();
        export(
            source.path(),
            source.path(),
            &target.path().join("restore-pending.zip"),
            BTreeMap::new(),
        )
        .unwrap();
        apply_pending_at(target.path(), target.path()).unwrap();
        let schedules: serde_json::Value = serde_json::from_slice(
            &fs::read(target.path().join("echoagent-automations.json")).unwrap(),
        )
        .unwrap();
        let records: serde_json::Value = serde_json::from_slice(
            &fs::read(target.path().join("echoagent-automation-records.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(schedules["automations"][0]["id"], "keep");
        assert_eq!(schedules["automations"][0]["status"], "PAUSED");
        assert_eq!(records["records"][0]["status"], "failed");
        assert_eq!(records["records"][1]["status"], "success");
        assert_eq!(records["records"][2]["status"], "success");
        assert_eq!(records["records"][2]["finishedAt"], "original-end");
        let fact: serde_json::Value = serde_json::from_slice(
            &fs::read(
                target
                    .path()
                    .join("automation-terminal-outbox")
                    .join(fact_name),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(fact["external_claimed"], true);
        assert_eq!(fact["completion"]["push"], false);
    }

    #[test]
    fn zip_restore_retains_completed_fact_and_removes_target_only_jobs() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::write(source.path().join("echoagent-automation-records.json"), serde_json::to_vec(&serde_json::json!({"records":[{
            "id":"run", "automationId":"automation", "automationName":"finished", "status":"running", "startedAt":"start", "sessionId":"session", "promptId":"prompt"
        }]})).unwrap()).unwrap();
        let outbox = source.path().join("automation-terminal-outbox");
        fs::create_dir_all(&outbox).unwrap();
        let name = format!("{:x}.json", Sha256::digest(b"run"));
        fs::write(outbox.join(&name), serde_json::to_vec(&serde_json::json!({"record_id":"run","session_id":"session","prompt_id":"prompt","ok":true,"error":null,"finished_at":"original-end"})).unwrap()).unwrap();
        fs::create_dir_all(target.path().join("automation-terminal-outbox")).unwrap();
        fs::write(
            target
                .path()
                .join("automation-terminal-outbox/obsolete.json"),
            b"old-target-job",
        )
        .unwrap();
        export(
            source.path(),
            source.path(),
            &target.path().join("restore-pending.zip"),
            BTreeMap::new(),
        )
        .unwrap();
        apply_pending_at(target.path(), target.path()).unwrap();
        assert!(!target
            .path()
            .join("automation-terminal-outbox/obsolete.json")
            .exists());
        let history: serde_json::Value = serde_json::from_slice(
            &fs::read(target.path().join("echoagent-automation-records.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(history["records"][0]["status"], "success");
        assert_eq!(history["records"][0]["finishedAt"], "original-end");
        let fact: serde_json::Value = serde_json::from_slice(
            &fs::read(target.path().join("automation-terminal-outbox").join(name)).unwrap(),
        )
        .unwrap();
        assert_eq!(fact["external_claimed"], true);
        assert_eq!(fact["completion"]["push"], false);
        apply_pending_at(target.path(), target.path()).unwrap();
    }

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
            serde_json::json!({"version":1,"data":{"s":[source.path().join("clipboard-images/item/image.png")]}}).to_string(),
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
        let attachments: serde_json::Value =
            serde_json::from_str(&ui["echoagent.draft-attachments.v1"]).unwrap();
        assert_eq!(
            Path::new(attachments["data"]["s"][0].as_str().unwrap()),
            target.path().join("clipboard-images/item/image.png")
        );
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
