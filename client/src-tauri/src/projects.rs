//! Durable local project metadata.
//!
//! Projects are product state, not renderer cache. Keep the canonical copy in
//! EchoAgent's private data directory and let the frontend retain localStorage
//! only as an offline/migration cache.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;
use tauri::State;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

const MAX_PROJECTS: usize = 500;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 32;
const MAX_JSON_NODES: usize = 100_000;
const MAX_JSON_COLLECTION_ITEMS: usize = 10_000;
const MAX_JSON_STRING_CHARS: usize = 256 * 1024;
const MAX_ASSET_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ASSET_BATCH_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ASSET_NAME_CHARS: usize = 240;

static ACCESS: OnceLock<Mutex<()>> = OnceLock::new();

fn access() -> &'static Mutex<()> {
    ACCESS.get_or_init(|| Mutex::new(()))
}

fn store_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("echoagent-projects.json")
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAssetImport {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub ext: Option<String>,
    pub size_bytes: u64,
    pub updated_at: String,
}

fn validate_project_id(project_id: &str) -> Result<(), String> {
    if project_id.is_empty()
        || project_id.len() > 160
        || !project_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err("项目 id 不合法".into());
    }
    Ok(())
}

fn ensure_real_child_dir(parent: &Path, child: &Path, label: &str) -> Result<PathBuf, String> {
    match std::fs::symlink_metadata(child) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!("{label}必须是真实目录，不能是符号链接"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(child).map_err(|error| format!("创建{label}失败：{error}"))?;
            let metadata = std::fs::symlink_metadata(child)
                .map_err(|error| format!("检查{label}失败：{error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!("{label}在创建期间被替换"));
            }
        }
        Err(error) => return Err(format!("检查{label}失败：{error}")),
    }
    let canonical = child
        .canonicalize()
        .map_err(|error| format!("解析{label}失败：{error}"))?;
    if canonical == parent || !canonical.starts_with(parent) {
        return Err(format!("{label}超出 EchoAgent 项目数据目录"));
    }
    crate::paths::harden_private_dir(&canonical)?;
    Ok(canonical)
}

fn projects_root_at(data_home: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(data_home)
        .map_err(|error| format!("创建 EchoAgent 数据目录失败：{error}"))?;
    let canonical_home = data_home
        .canonicalize()
        .map_err(|error| format!("解析 EchoAgent 数据目录失败：{error}"))?;
    ensure_real_child_dir(&canonical_home, &data_home.join("projects"), "项目数据目录")
}

fn project_root_at(data_home: &Path, project_id: &str) -> Result<PathBuf, String> {
    validate_project_id(project_id)?;
    let projects = projects_root_at(data_home)?;
    ensure_real_child_dir(&projects, &projects.join(project_id), "项目目录")
}

fn asset_root_at(data_home: &Path, project_id: &str) -> Result<PathBuf, String> {
    let project = project_root_at(data_home, project_id)?;
    ensure_real_child_dir(&project, &project.join("assets"), "项目资产目录")
}

fn asset_root(project_id: &str) -> Result<PathBuf, String> {
    asset_root_at(&crate::paths::echo_agent_home_dir(), project_id)
}

fn destination_at_index(root: &Path, name: &str, index: usize) -> PathBuf {
    if index == 1 {
        return root.join(name);
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|v| v.to_str()).unwrap_or("asset");
    let ext = path.extension().and_then(|v| v.to_str());
    match ext {
        Some(ext) => root.join(format!("{stem} ({index}).{ext}")),
        None => root.join(format!("{stem} ({index})")),
    }
}

#[cfg(test)]
fn available_destination(root: &Path, name: &str) -> PathBuf {
    for index in 1..10_000 {
        let candidate = destination_at_index(root, name, index);
        if matches!(
            std::fs::symlink_metadata(&candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ) {
            return candidate;
        }
    }
    root.join(format!(
        "asset-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        uuid::Uuid::now_v7().simple()
    ))
}

fn asset_from_path(path: &std::path::Path, kind: &str) -> Result<ProjectAssetImport, String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| format!("读取资产元数据失败：{error}"))?;
    if metadata.file_type().is_symlink()
        || (kind == "file" && !metadata.is_file())
        || (kind == "folder" && !metadata.is_dir())
    {
        return Err("项目资产类型在处理期间发生了变化".into());
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "资产文件名不是有效 UTF-8".to_string())?
        .to_string();
    Ok(ProjectAssetImport {
        ext: path
            .extension()
            .and_then(|value| value.to_str())
            .map(|v| v.to_ascii_uppercase()),
        name,
        path: path.to_string_lossy().to_string(),
        kind: kind.to_string(),
        size_bytes: if metadata.is_file() {
            metadata.len()
        } else {
            0
        },
        updated_at: chrono::Utc::now().to_rfc3339(),
    })
}

fn validate_json_value(value: &Value, depth: usize, nodes: &mut usize) -> Result<(), String> {
    if depth > MAX_JSON_DEPTH {
        return Err(format!("项目数据层级不能超过 {MAX_JSON_DEPTH} 层"));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_JSON_NODES {
        return Err(format!("项目数据节点不能超过 {MAX_JSON_NODES} 个"));
    }
    match value {
        Value::String(value) if value.chars().count() > MAX_JSON_STRING_CHARS => Err(format!(
            "项目数据中的单个文本不能超过 {MAX_JSON_STRING_CHARS} 个字符"
        )),
        Value::Array(values) => {
            if values.len() > MAX_JSON_COLLECTION_ITEMS {
                return Err(format!(
                    "项目数据中的单个列表不能超过 {MAX_JSON_COLLECTION_ITEMS} 项"
                ));
            }
            for value in values {
                validate_json_value(value, depth + 1, nodes)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            if values.len() > MAX_JSON_COLLECTION_ITEMS {
                return Err(format!(
                    "项目数据中的单个对象不能超过 {MAX_JSON_COLLECTION_ITEMS} 个字段"
                ));
            }
            for (key, value) in values {
                if key.chars().count() > 256 || key.chars().any(char::is_control) {
                    return Err("项目数据包含不合法字段名".into());
                }
                validate_json_value(value, depth + 1, nodes)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate(projects: &[Value]) -> Result<(), String> {
    if projects.len() > MAX_PROJECTS {
        return Err(format!("项目数量不能超过 {MAX_PROJECTS} 个"));
    }
    let mut ids = HashSet::new();
    let mut nodes = 0usize;
    for project in projects {
        validate_json_value(project, 0, &mut nodes)?;
        let object = project
            .as_object()
            .ok_or_else(|| "项目数据必须是对象".to_string())?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "项目 id 不能为空".to_string())?;
        validate_project_id(id)?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("项目 {id} 的名称不能为空"))?;
        if id.len() > 160 || name.chars().count() > 120 {
            return Err(format!("项目 {id} 的 id 或名称过长"));
        }
        if name.chars().any(char::is_control) {
            return Err(format!("项目 {id} 的名称包含控制字符"));
        }
        if !ids.insert(id.to_string()) {
            return Err(format!("项目 id 重复：{id}"));
        }
    }
    Ok(())
}

fn import_one(
    root: &std::path::Path,
    source: &std::path::Path,
    remaining_batch_bytes: u64,
) -> Result<(ProjectAssetImport, PathBuf), String> {
    let metadata = std::fs::symlink_metadata(source)
        .map_err(|error| format!("读取 {} 失败：{error}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("仅支持导入普通文件：{}", source.display()));
    }
    if metadata.len() > MAX_ASSET_FILE_BYTES {
        return Err(format!("单个文件不能超过 512 MB：{}", source.display()));
    }
    if metadata.len() > remaining_batch_bytes {
        return Err("单次导入的文件总大小不能超过 2 GB".into());
    }
    let name = source
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "源文件名不是有效 UTF-8".to_string())?;
    if name.chars().count() > MAX_ASSET_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(format!("文件名过长或包含控制字符：{}", source.display()));
    }

    let mut input_options = OpenOptions::new();
    input_options.read(true);
    #[cfg(unix)]
    input_options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    input_options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let input = input_options
        .open(source)
        .map_err(|error| format!("打开 {} 失败：{error}", source.display()))?;
    let opened_metadata = input
        .metadata()
        .map_err(|error| format!("读取 {} 失败：{error}", source.display()))?;
    let copy_limit = MAX_ASSET_FILE_BYTES.min(remaining_batch_bytes);
    if !opened_metadata.is_file() || opened_metadata.len() > copy_limit {
        return Err(format!(
            "文件在导入前发生变化或超过大小上限：{}",
            source.display()
        ));
    }

    let (destination, mut output) = (1..10_000)
        .find_map(|index| {
            let destination = destination_at_index(root, name, index);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            match options.open(&destination) {
                Ok(file) => Some(Ok((destination, file))),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(format!("创建项目资产副本失败：{error}"))),
            }
        })
        .unwrap_or_else(|| Err("同名项目资产过多，请先整理文件".to_string()))?;
    let copy_result = (|| -> Result<(), String> {
        let mut limited = input.take(copy_limit.saturating_add(1));
        let copied = std::io::copy(&mut limited, &mut output)
            .map_err(|error| format!("导入 {} 失败：{error}", source.display()))?;
        if copied > copy_limit {
            return Err(format!("文件在导入期间超过大小上限：{}", source.display()));
        }
        output
            .flush()
            .and_then(|_| output.sync_all())
            .map_err(|error| format!("保存项目资产副本失败：{error}"))
    })();
    drop(output);
    if let Err(error) = copy_result {
        let _ = std::fs::remove_file(&destination);
        return Err(error);
    }
    if let Err(error) = crate::paths::harden_private_file(&destination) {
        let _ = std::fs::remove_file(&destination);
        return Err(error);
    }
    match asset_from_path(&destination, "file") {
        Ok(asset) => Ok((asset, destination)),
        Err(error) => {
            let _ = std::fs::remove_file(&destination);
            Err(error)
        }
    }
}

fn import_files(
    root: &std::path::Path,
    sources: &[PathBuf],
) -> Result<Vec<ProjectAssetImport>, String> {
    let mut imported = Vec::with_capacity(sources.len());
    let mut copied = Vec::with_capacity(sources.len());
    let mut total_bytes = 0u64;
    for source in sources {
        match import_one(
            root,
            source,
            MAX_ASSET_BATCH_BYTES.saturating_sub(total_bytes),
        ) {
            Ok((asset, destination)) => {
                total_bytes = total_bytes.saturating_add(asset.size_bytes);
                imported.push(asset);
                copied.push(destination);
            }
            Err(error) => {
                for destination in copied {
                    let _ = std::fs::remove_file(destination);
                }
                return Err(error);
            }
        }
    }
    Ok(imported)
}

fn read_from(path: &std::path::Path) -> Result<Vec<Value>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("读取项目数据失败：{error}")),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("项目数据必须是普通文件，不能是符号链接".into());
    }
    if metadata.len() > MAX_BYTES as u64 {
        return Err("项目数据文件超过 8 MB 安全上限".into());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options
        .open(path)
        .map_err(|error| format!("打开项目数据失败：{error}"))?;
    if !file
        .metadata()
        .map_err(|error| format!("检查项目数据失败：{error}"))?
        .is_file()
    {
        return Err("项目数据在打开期间被替换".into());
    }
    let mut raw = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|error| format!("读取项目数据失败：{error}"))?;
    if raw.len() > MAX_BYTES {
        return Err("项目数据文件超过 8 MB 安全上限".into());
    }
    let mut projects: Vec<Value> =
        serde_json::from_slice(&raw).map_err(|error| format!("解析项目数据失败：{error}"))?;
    validate(&projects)?;
    for project in &mut projects {
        if project
            .get("defaultModelId")
            .and_then(Value::as_str)
            .is_some_and(crate::providers::is_legacy_builtin_model_id)
        {
            project["defaultModelId"] =
                Value::String(crate::providers::BUILTIN_DEFAULT_MODEL_ID.into());
        }
    }
    Ok(projects)
}

fn write_to(path: &std::path::Path, projects: &[Value]) -> Result<(), String> {
    validate(projects)?;
    let raw = serde_json::to_vec_pretty(projects)
        .map_err(|error| format!("序列化项目数据失败：{error}"))?;
    if raw.len() > MAX_BYTES {
        return Err("项目数据超过 8 MB 安全上限".into());
    }
    crate::paths::write_private_file(path, &raw)
}

#[tauri::command]
pub fn projects_load() -> Result<Vec<Value>, String> {
    let _guard = access()
        .lock()
        .map_err(|_| "项目存储锁已损坏".to_string())?;
    read_from(&store_path())
}

#[tauri::command]
pub fn projects_save(projects: Vec<Value>) -> Result<(), String> {
    let _guard = access()
        .lock()
        .map_err(|_| "项目存储锁已损坏".to_string())?;
    write_to(&store_path(), &projects)
}

/// Grant only the selected, catalogued project files as task attachments.
/// This deliberately never authorizes the private project directory as a workspace.
#[tauri::command]
pub fn project_assets_use(
    access: State<'_, crate::shell_fs::FilesystemAccess>,
    project_id: String,
    paths: Vec<String>,
) -> Result<Vec<String>, String> {
    if paths.len() > 20 {
        return Err("每次任务最多选择20个项目资料文件".into());
    }
    let _guard = self::access().lock().map_err(|_| "项目存储锁已损坏")?;
    let projects = read_from(&store_path())?;
    let project = projects
        .iter()
        .find(|p| p.get("id").and_then(Value::as_str) == Some(project_id.as_str()))
        .ok_or("项目尚未保存或已删除，请重试")?;
    let assets = project
        .get("assets")
        .and_then(Value::as_array)
        .ok_or("项目资料目录不可读")?;
    let root = asset_root(&project_id)?;
    let validated = validate_selected_assets(&root, assets, &paths)?;
    validated
        .into_iter()
        .map(|path| {
            access
                .authorize_file(&path)
                .map(|p| p.to_string_lossy().into_owned())
        })
        .collect()
}

fn validate_selected_assets(
    root: &Path,
    assets: &[Value],
    paths: &[String],
) -> Result<Vec<PathBuf>, String> {
    if paths.len() > 20 {
        return Err("每次任务最多选择20个项目资料文件".into());
    }
    let mut total = 0u64;
    let mut validated = Vec::new();
    for path in paths {
        if !assets.iter().any(|a| {
            a.get("kind").and_then(Value::as_str) == Some("file")
                && a.get("path").and_then(Value::as_str) == Some(path.as_str())
        }) {
            return Err("所选项目资料已更改或不存在，请重新选择".into());
        }
        let requested = PathBuf::from(&path);
        let metadata = std::fs::symlink_metadata(&requested)
            .map_err(|e| format!("项目资料不可用：{path}：{e}"))?;
        let canonical = requested.canonicalize().map_err(|e| e.to_string())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || !canonical.starts_with(root)
        {
            return Err("项目资料必须是私有资产目录内的普通文件，不能是符号链接".into());
        }
        if metadata.len() > 20 * 1024 * 1024 {
            return Err(format!("项目资料超过任务附件20MB上限：{path}"));
        }
        if validated.contains(&canonical) {
            continue;
        }
        total += metadata.len();
        if total > 64 * 1024 * 1024 {
            return Err("本次项目资料总大小不能超过64MB".into());
        }
        if !validated.contains(&canonical) {
            validated.push(canonical);
        }
    }
    Ok(validated)
}

/// Copy files explicitly selected by the user into this project's private
/// asset directory. Sources are never moved or modified.
#[tauri::command]
pub fn project_assets_import(
    access: State<'_, crate::shell_fs::FilesystemAccess>,
    project_id: String,
    sources: Vec<String>,
) -> Result<Vec<ProjectAssetImport>, String> {
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    if sources.len() > 100 {
        return Err("单次最多导入 100 个文件".into());
    }
    let root = asset_root(&project_id)?;
    let sources = sources
        .into_iter()
        .map(|source| access.require_authorized_file(std::path::Path::new(&source)))
        .collect::<Result<Vec<_>, _>>()?;
    import_files(&root, &sources)
}

#[tauri::command]
pub fn project_asset_make_dir(
    project_id: String,
    name: String,
) -> Result<ProjectAssetImport, String> {
    let name = name.trim();
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.chars().any(char::is_control)
        || name.chars().count() > MAX_ASSET_NAME_CHARS
    {
        return Err("文件夹名称不合法".into());
    }
    let root = asset_root(&project_id)?;
    let destination = (1..10_000)
        .find_map(|index| {
            let destination = destination_at_index(&root, name, index);
            match std::fs::create_dir(&destination) {
                Ok(()) => Some(Ok(destination)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(format!("创建文件夹失败：{error}"))),
            }
        })
        .unwrap_or_else(|| Err("同名项目资产过多，请先整理文件".to_string()))?;
    crate::paths::harden_private_dir(&destination)?;
    asset_from_path(&destination, "folder")
}

#[tauri::command]
pub fn project_asset_remove(project_id: String, path: String) -> Result<(), String> {
    let root = asset_root(&project_id)?;
    if path.chars().count() > 32_768 || path.chars().any(|ch| ch == '\0') {
        return Err("项目资产路径过长或包含空字符".into());
    }
    let claimed = PathBuf::from(path);
    if !claimed.is_absolute() {
        return Err("项目资产路径必须是绝对路径".into());
    }
    let name = claimed
        .file_name()
        .ok_or_else(|| "项目资产路径缺少文件名".to_string())?;
    let parent = claimed
        .parent()
        .ok_or_else(|| "项目资产路径缺少父目录".to_string())?
        .canonicalize()
        .map_err(|error| format!("解析资产父目录失败：{error}"))?;
    if parent != root && !parent.starts_with(&root) {
        return Err("拒绝删除项目资产目录之外的路径".into());
    }
    let target = parent.join(name);
    let metadata =
        std::fs::symlink_metadata(&target).map_err(|error| format!("读取资产失败：{error}"))?;
    if metadata.file_type().is_symlink() {
        Err("拒绝删除符号链接资产".into())
    } else if metadata.is_file() {
        std::fs::remove_file(&target).map_err(|error| format!("删除文件失败：{error}"))
    } else if metadata.is_dir() {
        std::fs::remove_dir(&target).map_err(|error| format!("删除文件夹失败（必须为空）：{error}"))
    } else {
        Err("不支持删除该类型资产".into())
    }
}

/// Remove the private asset copies owned by a project. Original source files
/// and EchoAgent conversations are outside this directory and remain intact.
#[tauri::command]
pub fn project_assets_remove_all(project_id: String) -> Result<(), String> {
    validate_project_id(&project_id)?;
    let data_home = crate::paths::echo_agent_home_dir();
    let projects_root_path = data_home.join("projects");
    if !projects_root_path.exists() {
        return Ok(());
    }
    let projects_metadata = std::fs::symlink_metadata(&projects_root_path)
        .map_err(|error| format!("读取项目目录失败：{error}"))?;
    if projects_metadata.file_type().is_symlink() || !projects_metadata.is_dir() {
        return Err("拒绝从符号链接或非目录的项目根路径删除数据".into());
    }
    let canonical_home = data_home
        .canonicalize()
        .map_err(|error| format!("解析 EchoAgent 数据目录失败：{error}"))?;
    let projects_root = projects_root_path
        .canonicalize()
        .map_err(|error| format!("解析项目目录失败：{error}"))?;
    if projects_root == canonical_home || !projects_root.starts_with(&canonical_home) {
        return Err("拒绝从 EchoAgent 数据目录之外删除项目".into());
    }
    let project_root = projects_root.join(&project_id);
    if !project_root.exists() {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(&project_root)
        .map_err(|error| format!("读取待删除项目目录失败：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("拒绝删除符号链接或非目录的项目路径".into());
    }
    let canonical_project = project_root
        .canonicalize()
        .map_err(|error| format!("解析待删除项目目录失败：{error}"))?;
    if canonical_project == projects_root || !canonical_project.starts_with(&projects_root) {
        return Err("拒绝删除项目数据目录之外的路径".into());
    }
    std::fs::remove_dir_all(&canonical_project)
        .map_err(|error| format!("删除项目资产副本失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        asset_root_at, available_destination, import_files, read_from, validate,
        validate_selected_assets, write_to, MAX_BYTES,
    };
    use serde_json::json;

    #[test]
    fn task_assets_require_exact_catalog_membership_containment_and_file_limits() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let file = root.join("selected.md");
        std::fs::write(&file, "explicit context").unwrap();
        let catalog = vec![json!({"kind":"file","path":file.to_string_lossy()})];
        let chosen = vec![file.to_string_lossy().into_owned()];
        assert_eq!(
            validate_selected_assets(&root, &catalog, &chosen).unwrap(),
            vec![file.clone()]
        );
        assert!(validate_selected_assets(&root, &[], &chosen).is_err());
        let outside = tempfile::NamedTempFile::new().unwrap();
        let outside_path = outside.path().to_string_lossy().into_owned();
        assert!(validate_selected_assets(
            &root,
            &[json!({"kind":"file","path":outside_path})],
            &[outside_path]
        )
        .is_err());
        let large = std::fs::OpenOptions::new().write(true).open(&file).unwrap();
        large.set_len(20 * 1024 * 1024 + 1).unwrap();
        assert!(validate_selected_assets(&root, &catalog, &chosen)
            .unwrap_err()
            .contains("20MB"));
    }

    #[cfg(unix)]
    #[test]
    fn task_assets_reject_catalogued_symlinks() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let target = root.join("source.md");
        std::fs::write(&target, "data").unwrap();
        let link = root.join("link.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let path = link.to_string_lossy().into_owned();
        assert!(
            validate_selected_assets(&root, &[json!({"kind":"file","path":path})], &[path])
                .is_err()
        );
    }

    #[test]
    fn project_store_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        let projects = vec![json!({
            "id": "p1",
            "name": "交付项目",
            "instructions": "优先保证可回归",
            "conversations": []
        })];
        write_to(&path, &projects).unwrap();
        assert_eq!(read_from(&path).unwrap(), projects);
    }

    #[test]
    fn project_store_rejects_duplicate_ids() {
        let projects = vec![
            json!({"id": "p1", "name": "A"}),
            json!({"id": "p1", "name": "B"}),
        ];
        assert!(validate(&projects).unwrap_err().contains("重复"));
    }

    #[test]
    fn retired_builtin_project_defaults_use_reed_without_changing_personal_models() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        let mut projects = vec![
            json!({"id":"old", "name":"旧项目", "defaultModelId":"echoagent-ojlab/chat-xc"}),
            json!({"id":"personal", "name":"个人模型", "defaultModelId":"mine/chat-xc"}),
            json!({"id":"current", "name":"当前模型", "defaultModelId":"echoagent-ojlab/Echo Grove"}),
        ];
        write_to(&path, &projects).unwrap();
        projects[0]["defaultModelId"] = json!(crate::providers::BUILTIN_DEFAULT_MODEL_ID);
        assert_eq!(read_from(&path).unwrap(), projects);
    }

    #[test]
    fn missing_project_store_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_from(&dir.path().join("missing.json"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn duplicate_asset_names_get_a_stable_suffix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.pdf"), "one").unwrap();
        assert_eq!(
            available_destination(dir.path(), "report.pdf")
                .file_name()
                .unwrap(),
            "report (2).pdf"
        );
    }

    #[test]
    fn failed_batch_import_rolls_back_already_copied_files() {
        let source_dir = tempfile::tempdir().unwrap();
        let asset_dir = tempfile::tempdir().unwrap();
        let valid = source_dir.path().join("valid.txt");
        std::fs::write(&valid, "content").unwrap();
        let missing = source_dir.path().join("missing.txt");
        let error = import_files(asset_dir.path(), &[valid, missing]).unwrap_err();
        assert!(error.contains("读取"));
        assert_eq!(std::fs::read_dir(asset_dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn project_store_rejects_oversized_and_deep_data() {
        let dir = tempfile::tempdir().unwrap();
        let oversized = dir.path().join("oversized.json");
        std::fs::write(&oversized, vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(read_from(&oversized).unwrap_err().contains("8 MB"));

        let mut nested = json!("leaf");
        for _ in 0..40 {
            nested = json!([nested]);
        }
        assert!(
            validate(&[json!({"id": "p1", "name": "A", "nested": nested})])
                .unwrap_err()
                .contains("层级")
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_store_and_asset_roots_reject_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real_store = dir.path().join("real.json");
        std::fs::write(&real_store, "[]").unwrap();
        let linked_store = dir.path().join("projects.json");
        symlink(&real_store, &linked_store).unwrap();
        assert!(read_from(&linked_store).unwrap_err().contains("符号链接"));

        let external = tempfile::tempdir().unwrap();
        symlink(external.path(), dir.path().join("projects")).unwrap();
        assert!(asset_root_at(dir.path(), "safe-id")
            .unwrap_err()
            .contains("符号链接"));
    }
}
