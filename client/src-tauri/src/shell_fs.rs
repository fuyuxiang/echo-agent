//! Desktop shell / filesystem helpers for markdown interactions:
//! open URL, open path, reveal in folder, path_stat, safe write under workspace.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use serde::Serialize;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

const TEXT_PREVIEW_MAX_BYTES: usize = 256 * 1024;
#[path = "binary_preview_limits.rs"]
mod binary_preview_limits;
const DIRECTORY_LIST_MAX_ENTRIES: usize = 2_000;
const TEXT_WRITE_MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_AUTHORIZED_ROOTS: usize = 512;
const MAX_PICKED_WORKSPACE_GRANTS_BYTES: u64 = 512 * 1024;
const MAX_AUTHORIZED_FILES: usize = 2_048;
const MAX_TRUSTED_PACKAGE_SOURCES: usize = 2_048;
const MAX_CONFIGURED_SKILL_SOURCES: usize = 512;
pub(crate) const MAX_PICKED_FILES: usize = 100;
const MAX_ATTACHMENT_COUNT: usize = 20;
pub(crate) const MAX_ATTACHMENT_FILE_BYTES: u64 = 20 * 1024 * 1024;
pub(crate) const MAX_ATTACHMENT_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Native-side filesystem authority for custom Tauri commands.
///
/// Tauri capability files scope plugin commands, but application commands in
/// `generate_handler!` still have to enforce their own resource boundaries.
/// Paths supplied by the WebView are therefore never treated as proof of an
/// authorized workspace. A root enters this set only through a native folder
/// picker, the native-managed default workspace, or durable session/knowledge
/// metadata written by an earlier trusted application run. Session lifecycle
/// commands may consume these grants but cannot create them.
#[derive(Debug, Default)]
pub struct FilesystemAccess {
    roots: Mutex<HashSet<PathBuf>>,
    picked_workspace_grants: Mutex<()>,
    files: Mutex<HashSet<PathBuf>>,
    /// Canonical application-owned attachment store. Files inside this one
    /// narrow root are authorized by containment instead of consuming the
    /// process-wide exact-file quota one entry at a time.
    managed_attachment_root: Mutex<Option<PathBuf>>,
    package_sources: Mutex<HashSet<PathBuf>>,
    configured_skill_sources: Mutex<HashMap<PathBuf, String>>,
}

fn picked_workspace_grants_path() -> PathBuf {
    crate::paths::echo_agent_home_dir().join("echoagent-picked-workspaces.json")
}

fn read_picked_workspace_grants(path: &Path) -> Result<Vec<String>, String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("无法读取工作区授权记录：{error}")),
        Ok(_) => {}
    }
    let bytes = read_regular_file_bounded(path, MAX_PICKED_WORKSPACE_GRANTS_BYTES)?;
    let grants: Vec<String> = serde_json::from_slice(&bytes)
        .map_err(|error| format!("工作区授权记录格式错误：{error}"))?;
    if grants.len() > MAX_AUTHORIZED_ROOTS {
        return Err("工作区授权记录超过安全上限".into());
    }
    Ok(grants)
}

impl FilesystemAccess {
    pub fn new() -> Self {
        let access = Self::default();

        // The native default root covers all automatically allocated tasks;
        // restore it before historical children consume the root budget.
        if let Ok(root) = crate::paths::default_workspace_dir() {
            let _ = access.authorize_workspace(&root.to_string_lossy());
        }

        // A persisted Runtime session is the durable capability record for a
        // workspace selected in an earlier run. Invalid/missing paths are
        // ignored rather than broadening access through lexical strings.
        for workspace in crate::sessions::list_workspaces() {
            let _ = access.authorize_workspace(&workspace.cwd);
        }

        for root in crate::coding::isolation::managed_worktrees() {
            let _ = access.authorize_workspace(&root.to_string_lossy());
        }

        // Knowledge sources are also durable user selections. Load this file
        // only once at native startup: a compromised renderer cannot expand
        // the live allow-list merely by rewriting the descriptor store.
        if let Ok(serde_json::Value::Array(sources)) = crate::org::read_local_kb_sources() {
            for source in sources {
                if let Some(root) = source.get("root").and_then(serde_json::Value::as_str) {
                    let _ = access.authorize_workspace(root);
                }
            }
        }

        // A folder chosen for an automation may not have a session yet. Keep
        // that native picker grant across restarts so scheduled runs can use it.
        match read_picked_workspace_grants(&picked_workspace_grants_path()) {
            Ok(grants) => {
                for root in grants {
                    if let Err(error) = access.authorize_workspace(&root) {
                        tracing::warn!(%error, root, "skipping unavailable picked workspace");
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "failed to restore picked workspaces"),
        }

        access
    }

    /// Register the private clipboard attachment store as one durable,
    /// containment-scoped capability. This avoids consuming one of the 2048
    /// general exact-file grants for every historical paste while still
    /// refusing unsupported files and symlinks planted in the directory.
    pub(crate) fn register_attachment_store(&self, dir: &Path) {
        if let Err(error) = std::fs::create_dir_all(dir) {
            tracing::warn!(dir = %dir.display(), %error, "failed to create attachment store");
            return;
        }
        if let Err(error) = crate::attachment_blob::prune_store(dir, None) {
            tracing::warn!(dir = %dir.display(), %error, "failed to prune attachment store");
        }
        let Ok(canonical_dir) = std::fs::canonicalize(dir) else {
            tracing::warn!(dir = %dir.display(), "failed to canonicalize attachment store");
            return;
        };
        if let Ok(mut root) = self.managed_attachment_root.lock() {
            *root = Some(canonical_dir);
        }

        // Validate the existing entries once at startup so malformed/symlinked
        // content is visible in logs. Authorization itself is checked lazily by
        // `require_managed_attachment`, so the number of historical files no
        // longer competes with MAX_AUTHORIZED_FILES.
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::debug!(
                    dir = %dir.display(),
                    error = %error,
                    "attachment store unavailable during startup",
                );
                return;
            }
        };
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let candidates = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                std::fs::read_dir(&entry_path)
                    .map(|children| children.flatten().map(|child| child.path()).collect())
                    .unwrap_or_default()
            } else {
                vec![entry_path]
            };
            for path in candidates {
                // Symlinks can redirect preview traffic to anything reachable on
                // disk. `authorize_file` already refuses them, but skip earlier
                // so the warn message points at the real cause rather than the
                // downstream symlink rejection.
                if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        tracing::debug!(
                            path = %path.display(),
                            "skipping non-regular clipboard-image entry",
                        );
                        continue;
                    }
                }
                if !crate::attachment_blob::is_supported_attachment_path(&path) {
                    tracing::debug!(
                        path = %path.display(),
                        "skipping attachment-store entry with unsupported extension",
                    );
                    continue;
                }
                if let Err(error) = self.require_managed_attachment(&path) {
                    tracing::warn!(
                        path = %path.display(),
                        error = %error,
                        "skipping attachment-store entry that failed validation",
                    );
                }
            }
        }
    }

    /// Resolve one regular supported file inside the registered application-
    /// owned attachment store. Parent symlinks cannot escape the store because
    /// the canonical result must remain below the canonical root.
    pub(crate) fn require_managed_attachment(&self, raw: &Path) -> Result<PathBuf, String> {
        if !raw.is_absolute() {
            return Err("附件路径必须是绝对路径".into());
        }
        let metadata =
            std::fs::symlink_metadata(raw).map_err(|error| format!("无法读取附件：{error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("附件必须是普通文件，不能是符号链接".into());
        }
        if !crate::attachment_blob::is_supported_attachment_path(raw) {
            return Err("应用附件目录中包含不支持的文件类型".into());
        }
        let canonical = raw
            .canonicalize()
            .map_err(|error| format!("无法解析附件路径：{error}"))?;
        let root = self
            .managed_attachment_root
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?
            .clone()
            .ok_or_else(|| "应用附件目录尚未初始化".to_string())?;
        if canonical.starts_with(&root) {
            Ok(canonical)
        } else {
            Err(format!("附件不在应用管理目录内：{}", canonical.display()))
        }
    }

    fn is_managed_attachment(&self, canonical: &Path) -> Result<bool, String> {
        let root = self
            .managed_attachment_root
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?;
        Ok(root
            .as_ref()
            .is_some_and(|root| canonical.starts_with(root))
            && crate::attachment_blob::is_supported_attachment_path(canonical))
    }

    /// Add one existing directory to the native allow-list and return its
    /// canonical spelling. The set is deliberately bounded to avoid an IPC
    /// caller turning grants into unbounded process memory.
    pub(crate) fn authorize_workspace(&self, root: &str) -> Result<PathBuf, String> {
        let requested = PathBuf::from(root);
        if !requested.is_absolute() {
            return Err("工作区路径必须是绝对路径".into());
        }
        let canonical = requested
            .canonicalize()
            .map_err(|error| format!("无法解析工作区路径：{error}"))?;
        if !canonical.is_dir() {
            return Err("工作区必须是已存在的目录".into());
        }
        let mut roots = self
            .roots
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?;
        if roots.iter().any(|root| canonical.starts_with(root)) {
            return Ok(canonical);
        }
        roots.retain(|root| !root.starts_with(&canonical));
        if roots.len() >= MAX_AUTHORIZED_ROOTS {
            return Err(format!("已授权工作区不能超过 {MAX_AUTHORIZED_ROOTS} 个"));
        }
        roots.insert(canonical.clone());
        Ok(canonical)
    }

    fn authorize_picked_workspace_at(&self, root: &str, path: &Path) -> Result<PathBuf, String> {
        let _guard = self
            .picked_workspace_grants
            .lock()
            .map_err(|_| "工作区授权状态已损坏".to_string())?;
        let canonical = self.authorize_workspace(root)?;
        let mut grants = read_picked_workspace_grants(path)?;
        let canonical_string = canonical.to_string_lossy().into_owned();
        if !grants.iter().any(|entry| entry == &canonical_string) {
            if grants.len() >= MAX_AUTHORIZED_ROOTS {
                return Err(format!("已授权工作区不能超过 {MAX_AUTHORIZED_ROOTS} 个"));
            }
            grants.push(canonical_string);
            let body = serde_json::to_vec(&grants)
                .map_err(|error| format!("序列化工作区授权失败：{error}"))?;
            crate::paths::write_private_file(path, &body)?;
        }
        Ok(canonical)
    }

    /// Resolve a renderer-supplied session cwd against roots already granted
    /// by native state. This method never mutates the allow-list: session
    /// creation/loading cannot bootstrap its own filesystem authority.
    pub(crate) fn require_workspace(&self, claimed: &str) -> Result<PathBuf, String> {
        let requested = PathBuf::from(claimed);
        if !requested.is_absolute() {
            return Err("会话工作区必须是绝对路径".into());
        }
        let canonical = requested
            .canonicalize()
            .map_err(|error| format!("无法解析会话工作区：{error}"))?;
        if !canonical.is_dir() {
            return Err("会话工作区必须是已存在的目录".into());
        }
        let roots = self
            .roots
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?;
        if roots.iter().any(|root| canonical.starts_with(root)) {
            return Ok(canonical);
        }
        Err(format!(
            "会话工作区未经用户授权：{}；请先通过“选择文件夹”添加",
            canonical.display()
        ))
    }

    pub(crate) fn authorize_file(&self, raw: &Path) -> Result<PathBuf, String> {
        if !raw.is_absolute() {
            return Err("文件路径必须是绝对路径".into());
        }
        let metadata =
            std::fs::symlink_metadata(raw).map_err(|error| format!("无法读取所选文件：{error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("所选项必须是普通文件，不能是符号链接".into());
        }
        let canonical = raw
            .canonicalize()
            .map_err(|error| format!("无法解析所选文件：{error}"))?;
        let mut files = self
            .files
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?;
        if !files.contains(&canonical) && files.len() >= MAX_AUTHORIZED_FILES {
            return Err(format!(
                "本次运行授权的文件不能超过 {MAX_AUTHORIZED_FILES} 个"
            ));
        }
        files.insert(canonical.clone());
        Ok(canonical)
    }

    pub(crate) fn revoke_authorized_file(&self, raw: &Path) -> Result<(), String> {
        self.files
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?
            .remove(raw);
        Ok(())
    }

    pub(crate) fn require_authorized_file(&self, raw: &Path) -> Result<PathBuf, String> {
        let metadata = std::fs::symlink_metadata(raw)
            .map_err(|error| format!("无法读取文件 {}: {error}", raw.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!("拒绝读取非普通文件：{}", raw.display()));
        }
        let canonical = canonicalize_candidate(raw, false)?;
        let exact_allowed = self
            .files
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?
            .contains(&canonical);
        if exact_allowed {
            return Ok(canonical);
        }
        self.is_authorized(&canonical, false).and_then(|path| {
            if path.is_file() {
                Ok(path)
            } else {
                Err(format!("不是文件：{}", path.display()))
            }
        })
    }

    /// Record an exact package source discovered by a trusted native/runtime
    /// catalog. This does not grant generic filesystem access to its parent.
    pub(crate) fn record_trusted_package_source(&self, raw: &Path) -> Result<PathBuf, String> {
        let metadata = std::fs::symlink_metadata(raw)
            .map_err(|error| format!("无法读取包来源 {}: {error}", raw.display()))?;
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            return Err("包来源必须是普通文件或目录".into());
        }
        let canonical = raw
            .canonicalize()
            .map_err(|error| format!("无法解析包来源：{error}"))?;
        let mut sources = self
            .package_sources
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?;
        if !sources.contains(&canonical) && sources.len() >= MAX_TRUSTED_PACKAGE_SOURCES {
            return Err(format!(
                "本次运行最多记录 {MAX_TRUSTED_PACKAGE_SOURCES} 个可信包来源"
            ));
        }
        sources.insert(canonical.clone());
        Ok(canonical)
    }

    pub(crate) fn require_authorized_package_source(&self, raw: &Path) -> Result<PathBuf, String> {
        let metadata = std::fs::symlink_metadata(raw)
            .map_err(|error| format!("无法读取包来源 {}: {error}", raw.display()))?;
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            return Err("包来源必须是普通文件或目录".into());
        }
        let canonical = raw
            .canonicalize()
            .map_err(|error| format!("无法解析包来源：{error}"))?;
        if self
            .package_sources
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?
            .contains(&canonical)
        {
            return Ok(canonical);
        }
        if metadata.is_file() {
            self.require_authorized_file(raw)
        } else {
            self.require_workspace(&raw.to_string_lossy())
        }
    }

    pub(crate) fn record_configured_skill_source(&self, raw: &str) -> Result<(), String> {
        let canonical = self.record_trusted_package_source(Path::new(raw))?;
        let mut configured = self
            .configured_skill_sources
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?;
        if !configured.contains_key(&canonical) && configured.len() >= MAX_CONFIGURED_SKILL_SOURCES
        {
            return Err(format!(
                "已配置技能来源不能超过 {MAX_CONFIGURED_SKILL_SOURCES} 个"
            ));
        }
        configured.insert(canonical, raw.to_string());
        Ok(())
    }

    pub(crate) fn require_configured_skill_source(&self, raw: &str) -> Result<String, String> {
        let canonical = Path::new(raw)
            .canonicalize()
            .map_err(|error| format!("无法解析已配置技能来源：{error}"))?;
        self.configured_skill_sources
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?
            .get(&canonical)
            .cloned()
            .ok_or_else(|| "只能移除 Runtime 当前列出的精确技能来源".to_string())
    }

    /// Validate renderer-provided attachments against the workspace that the
    /// backend bound to this exact session. An arbitrary path (or merely some
    /// other authorized workspace) is not sufficient: attachments become
    /// model input and must stay inside the session's own authority boundary.
    pub(crate) fn validate_session_attachments(
        &self,
        workspace: &Path,
        attachments: &[String],
    ) -> Result<Vec<String>, String> {
        if attachments.len() > MAX_ATTACHMENT_COUNT {
            return Err(format!("附件数量不能超过 {MAX_ATTACHMENT_COUNT} 个"));
        }
        let workspace = self.require_workspace(&workspace.to_string_lossy())?;
        let exact_files = self
            .files
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?
            .clone();
        let mut total_bytes = 0_u64;
        let mut seen = HashSet::new();
        let mut validated = Vec::with_capacity(attachments.len());
        for raw in attachments {
            let candidate = PathBuf::from(raw);
            let metadata = std::fs::symlink_metadata(&candidate)
                .map_err(|error| format!("无法读取附件 {}: {error}", candidate.display()))?;
            if metadata.file_type().is_symlink() {
                return Err(format!("拒绝使用符号链接附件：{}", candidate.display()));
            }
            if !metadata.is_file() {
                return Err(format!("附件必须是普通文件：{}", candidate.display()));
            }
            let canonical = canonicalize_candidate(&candidate, false)?;
            // Exact-file grants are general, process-lifetime EchoAgent read
            // capabilities created only by a backend-owned native picker.
            if !canonical.starts_with(&workspace)
                && !exact_files.contains(&canonical)
                && !self.is_managed_attachment(&canonical)?
            {
                return Err(format!("附件不在当前会话工作区内：{}", canonical.display()));
            }
            if metadata.len() > MAX_ATTACHMENT_FILE_BYTES {
                return Err(format!(
                    "单个附件不能超过 {}MB：{}",
                    MAX_ATTACHMENT_FILE_BYTES / 1024 / 1024,
                    canonical.display()
                ));
            }
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .ok_or_else(|| "附件总大小溢出".to_string())?;
            if total_bytes > MAX_ATTACHMENT_TOTAL_BYTES {
                return Err(format!(
                    "附件总大小不能超过 {}MB",
                    MAX_ATTACHMENT_TOTAL_BYTES / 1024 / 1024
                ));
            }
            if seen.insert(canonical.clone()) {
                validated.push(canonical.to_string_lossy().into_owned());
            }
        }
        Ok(validated)
    }

    fn is_authorized(&self, candidate: &Path, allow_missing: bool) -> Result<PathBuf, String> {
        let canonical = canonicalize_candidate(candidate, allow_missing)?;

        // Project assets are copied into this application-owned subtree by a
        // separate bounded import command. Permit opening those results without
        // granting access to provider credentials or session stores alongside
        // them in the rest of ECHO_AGENT_HOME.
        let internal_projects = crate::paths::echo_agent_home_dir().join("projects");
        let internal_allowed = internal_projects
            .canonicalize()
            .is_ok_and(|root| canonical.starts_with(root));

        // Exact-file grants (file picker selection, paste-image landing) are
        // recorded through `authorize_file` and previously only fed
        // `require_authorized_file` (used by attachment thumbnails) plus
        // `validate_session_attachments` (used at send time). The other
        // previewer commands — `open_path`, `reveal_in_folder`,
        // `browse_directory`, `path_stat`, `read_text_file`,
        // `read_file_base64` — all funnel through this method, so without
        // checking the set here the moment a user clicks an approved
        // attachment that lives outside every authorized workspace they see
        // "拒绝访问未授权的路径". The set is bounded at MAX_AUTHORIZED_FILES,
        // so consulting it on every preview is cheap.
        let exact_allowed = self
            .files
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?
            .contains(&canonical);

        let managed_allowed = self.is_managed_attachment(&canonical)?;
        let roots = self
            .roots
            .lock()
            .map_err(|_| "文件系统授权状态已损坏".to_string())?;
        if internal_allowed
            || exact_allowed
            || managed_allowed
            || roots.iter().any(|root| canonical.starts_with(root))
        {
            return Ok(canonical);
        }
        Err(format!("拒绝访问未授权的路径：{}", canonical.display()))
    }

    #[cfg(test)]
    fn authorized_roots(&self) -> Vec<PathBuf> {
        self.roots.lock().unwrap().iter().cloned().collect()
    }
}

fn bounded_limit(requested: Option<u64>, hard_max: usize) -> usize {
    requested
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(hard_max)
        .min(hard_max)
}

pub(crate) fn open_regular_file_no_follow(path: &Path) -> Result<std::fs::File, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| format!("读取失败：{e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("读取目标必须是普通文件，不能是符号链接".into());
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // Open the reparse point itself so a last-moment link swap is rejected
        // by the post-open regular-file check below.
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path).map_err(|e| format!("读取失败：{e}"))?;
    let opened = file.metadata().map_err(|e| format!("读取失败：{e}"))?;
    if !opened.is_file() {
        return Err("读取目标必须是普通文件".into());
    }
    Ok(file)
}

/// Read a complete regular file without following a final symlink. Both the
/// pre-open size and the bytes actually consumed are capped, closing the
/// metadata/open race and a file-growth allocation attack.
pub(crate) fn read_regular_file_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = open_regular_file_no_follow(path)?;
    let metadata = file.metadata().map_err(|e| format!("读取失败：{e}"))?;
    if metadata.len() > limit {
        return Err(format!("文件超过 {} 字节安全上限", limit));
    }
    let mut data = Vec::with_capacity(metadata.len().min(limit) as usize);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut data)
        .map_err(|e| format!("读取失败：{e}"))?;
    if data.len() as u64 > limit {
        return Err(format!("文件超过 {} 字节安全上限", limit));
    }
    Ok(data)
}

/// Read at most `limit` bytes from the beginning of a regular file without
/// following a final symlink. Unlike [`read_regular_file_bounded`], a larger
/// file is allowed because callers only need an early bounded record.
pub(crate) fn read_regular_file_prefix(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let file = open_regular_file_no_follow(path)?;
    let mut data = Vec::with_capacity(
        file.metadata()
            .map_err(|e| format!("读取失败：{e}"))?
            .len()
            .min(limit) as usize,
    );
    file.take(limit)
        .read_to_end(&mut data)
        .map_err(|e| format!("读取失败：{e}"))?;
    Ok(data)
}

/// Read at most `limit + 1` bytes so truncation can be reported without ever
/// allocating the whole file. `limit` is always one of the native hard caps.
fn read_bounded_prefix(path: &Path, limit: usize) -> Result<(Vec<u8>, bool), String> {
    let file = open_regular_file_no_follow(path)?;
    let read_cap = limit.saturating_add(1);
    let mut data = Vec::with_capacity(read_cap.min(64 * 1024));
    BufReader::new(file)
        .take(read_cap as u64)
        .read_to_end(&mut data)
        .map_err(|e| format!("读取失败：{e}"))?;
    let truncated = data.len() > limit;
    if truncated {
        data.truncate(limit);
    }
    Ok((data, truncated))
}

/// Resolve `path` against optional `cwd`. Absolute paths are used as-is.
fn resolve_path(path: &str, cwd: Option<&str>) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        return p;
    }
    match cwd {
        Some(c) if !c.is_empty() => PathBuf::from(c).join(p),
        _ => p,
    }
}

/// Canonicalize an existing path, or (for stat/write destinations) its nearest
/// existing ancestor plus the missing suffix. Resolving symlinks before the
/// allow-list comparison prevents an authorized directory symlink from being
/// used to escape into another tree.
fn canonicalize_candidate(candidate: &Path, allow_missing: bool) -> Result<PathBuf, String> {
    if !candidate.is_absolute() {
        return Err("文件系统路径必须是绝对路径".into());
    }
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("拒绝包含 .. 的文件系统路径".into());
    }
    if candidate.exists() {
        return candidate
            .canonicalize()
            .map_err(|error| format!("无法解析目标路径：{error}"));
    }
    if !allow_missing {
        return Err(format!("路径不存在：{}", candidate.display()));
    }

    let mut ancestor = candidate;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| "目标路径没有可解析的父目录".to_string())?;
    }
    let ancestor_canon = ancestor
        .canonicalize()
        .map_err(|error| format!("无法解析目标目录：{error}"))?;
    let suffix = candidate
        .strip_prefix(ancestor)
        .map_err(|_| "无法解析目标路径后缀".to_string())?;
    Ok(ancestor_canon.join(suffix))
}

/// Ensure `candidate` is inside `root` after canonicalizing its nearest
/// existing ancestor. Validation is read-only: rejected paths never create
/// directories outside the workspace.
fn ensure_under_workspace(root: &Path, candidate: &Path) -> Result<PathBuf, String> {
    if candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("拒绝包含 .. 的写入路径".into());
    }

    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("无法解析工作区路径：{e}"))?;

    // If the path doesn't exist yet, resolve the nearest existing ancestor and
    // append the still-missing suffix without touching the filesystem.
    let candidate_canon = if candidate.exists() {
        candidate
            .canonicalize()
            .map_err(|e| format!("无法解析目标路径：{e}"))?
    } else {
        let mut ancestor = candidate;
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| "目标路径没有可解析的父目录".to_string())?;
        }
        let ancestor_canon = ancestor
            .canonicalize()
            .map_err(|e| format!("无法解析目标目录：{e}"))?;
        let suffix = candidate
            .strip_prefix(ancestor)
            .map_err(|_| "无法解析目标路径后缀".to_string())?;
        ancestor_canon.join(suffix)
    };

    if !candidate_canon.starts_with(&root_canon) {
        return Err(format!(
            "拒绝写入工作区之外的路径：{}",
            candidate_canon.display()
        ));
    }
    Ok(candidate_canon)
}

/// Write through a unique sibling and atomically replace the destination.
/// This avoids following an existing destination symlink between authorization
/// and truncation. Existing file permissions are retained where possible.
fn write_text_atomically(path: &Path, content: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("写入路径没有父目录：{}", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("写入路径没有文件名：{}", path.display()))?;
    let staging = parent.join(format!(
        ".{}.{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id(),
        uuid::Uuid::now_v7().simple()
    ));

    let result = (|| -> Result<(), String> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|error| format!("创建写入临时文件失败：{error}"))?;
        file.write_all(content)
            .map_err(|error| format!("写入临时文件失败：{error}"))?;
        file.sync_all()
            .map_err(|error| format!("同步写入临时文件失败：{error}"))?;
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            if metadata.file_type().is_symlink() {
                return Err("拒绝覆盖符号链接".into());
            }
            std::fs::set_permissions(&staging, metadata.permissions())
                .map_err(|error| format!("保留文件权限失败：{error}"))?;
        }
        drop(file);
        crate::paths::replace_file_atomically(&staging, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staging);
    }
    result
}

/// Open a URL in the system default browser / handler.
#[tauri::command]
pub async fn open_url(url: String) -> Result<(), String> {
    let parsed = validated_external_url(&url)?;
    open::that(parsed.as_str()).map_err(|e| format!("打开链接失败：{e}"))
}

fn validated_external_url(url: &str) -> Result<url::Url, String> {
    let url = url.trim();
    if url.is_empty() {
        return Err("URL 为空".into());
    }
    if url.chars().count() > 8_192 || url.chars().any(char::is_control) {
        return Err("URL 过长或包含控制字符".into());
    }
    let parsed = url::Url::parse(url).map_err(|error| format!("URL 格式无效：{error}"))?;
    match parsed.scheme() {
        "http" | "https" => {
            if parsed.host_str().is_none() {
                return Err("HTTP(S) URL 缺少主机名".into());
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                return Err("不支持带用户名或密码的 URL".into());
            }
        }
        "mailto" => {
            if parsed.path().trim().is_empty() {
                return Err("邮件地址为空".into());
            }
        }
        scheme => return Err(format!("不支持的 URL 协议：{scheme}")),
    }
    Ok(parsed)
}

/// Ask the user to select a directory in a native dialog, then add only the
/// selected canonical directory to the backend allow-list. Keeping selection
/// and grant in one native command prevents the WebView from substituting a
/// different path after the user closes the picker.
#[tauri::command]
pub async fn filesystem_pick_directory(
    app: tauri::AppHandle,
    access: State<'_, FilesystemAccess>,
    suggested_path: Option<String>,
) -> Result<Option<String>, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_title("选择 EchoAgent 可访问的文件夹");
    if let Some(suggested) = suggested_path {
        let path = PathBuf::from(suggested.trim());
        let directory = if path.is_dir() {
            Some(path.as_path())
        } else {
            path.parent().filter(|parent| parent.is_dir())
        };
        if let Some(directory) = directory {
            dialog = dialog.set_directory(directory);
        }
    }
    dialog.pick_folder(move |selection| {
        let _ = sender.send(selection);
    });
    let Some(selection) = receiver
        .await
        .map_err(|_| "目录选择对话框意外关闭".to_string())?
    else {
        return Ok(None);
    };
    let path = selection
        .into_path()
        .map_err(|error| format!("选中的目录不是本地路径：{error}"))?;
    let canonical = access
        .authorize_picked_workspace_at(&path.to_string_lossy(), &picked_workspace_grants_path())?;
    Ok(Some(canonical.to_string_lossy().into_owned()))
}

/// Select files and bind the exact canonical files to this native process.
/// Unlike a renderer-supplied path, the callback result is inseparable from
/// the operating-system dialog that captured the user's choice.
#[tauri::command]
pub async fn filesystem_pick_files(
    app: tauri::AppHandle,
    access: State<'_, FilesystemAccess>,
    title: Option<String>,
    extensions: Option<Vec<String>>,
    multiple: Option<bool>,
    max_files: Option<usize>,
) -> Result<Vec<String>, String> {
    let title = title.unwrap_or_else(|| "选择 EchoAgent 附件".into());
    if title.trim().is_empty() || title.chars().count() > 256 || title.chars().any(char::is_control)
    {
        return Err("文件选择器标题无效或过长".into());
    }
    let extensions = extensions.unwrap_or_default();
    if extensions.len() > 32 {
        return Err("文件类型过滤器不能超过 32 项".into());
    }
    let mut dialog = app.dialog().file().set_title(title);
    let extensions = extensions
        .into_iter()
        .map(|extension| {
            extension
                .trim()
                .trim_start_matches('.')
                .to_ascii_lowercase()
        })
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 16
                && extension
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric())
        })
        .collect::<Vec<_>>();
    if !extensions.is_empty() {
        let extension_refs = extensions.iter().map(String::as_str).collect::<Vec<_>>();
        dialog = dialog.add_filter("支持的文件", &extension_refs);
    }
    let selection = if multiple.unwrap_or(true) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        dialog.pick_files(move |selection| {
            let _ = sender.send(selection);
        });
        receiver
            .await
            .map_err(|_| "文件选择对话框意外关闭".to_string())?
            .unwrap_or_default()
    } else {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        dialog.pick_file(move |selection| {
            let _ = sender.send(selection);
        });
        receiver
            .await
            .map_err(|_| "文件选择对话框意外关闭".to_string())?
            .into_iter()
            .collect()
    };
    if selection.is_empty() {
        return Ok(Vec::new());
    }
    let max_files = max_files
        .unwrap_or(MAX_PICKED_FILES)
        .clamp(1, MAX_PICKED_FILES);
    if selection.len() > max_files {
        return Err(format!("一次最多选择 {max_files} 个文件"));
    }
    let mut paths = Vec::with_capacity(selection.len());
    for selected in selection {
        let path = selected
            .into_path()
            .map_err(|error| format!("所选附件不是本地路径：{error}"))?;
        paths.push(access.authorize_file(&path)?.to_string_lossy().into_owned());
    }
    Ok(paths)
}

/// Open a local file or directory with the OS default app.
#[tauri::command]
pub async fn open_path(
    access: State<'_, FilesystemAccess>,
    path: String,
    cwd: Option<String>,
) -> Result<(), String> {
    let resolved = resolve_path(&path, cwd.as_deref());
    let authorized = access.is_authorized(&resolved, false)?;
    open::that(&authorized).map_err(|e| format!("打开路径失败：{e}"))
}

/// Reveal a path in the system file manager (select file when possible).
#[tauri::command]
pub async fn reveal_in_folder(
    access: State<'_, FilesystemAccess>,
    path: String,
    cwd: Option<String>,
) -> Result<(), String> {
    let resolved = resolve_path(&path, cwd.as_deref());
    let resolved = access.is_authorized(&resolved, false)?;

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // explorer /select,"C:\path\to\file"
        let arg = format!("/select,{}", resolved.display());
        std::process::Command::new("explorer")
            .raw_arg(arg)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("打开资源管理器失败：{e}"))?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &resolved.to_string_lossy()])
            .spawn()
            .map_err(|e| format!("打开 Finder 失败：{e}"))?;
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        // Linux: open parent directory
        let parent = if resolved.is_dir() {
            resolved.clone()
        } else {
            resolved
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or(resolved.clone())
        };
        open::that(parent).map_err(|e| format!("打开文件管理器失败：{e}"))?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathStat {
    pub path: String,
    pub exists: bool,
    /// "file" | "directory" | "other" | "missing"
    pub kind: String,
    pub absolute: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentFileStat {
    pub input_path: String,
    /// Canonical path, suitable for passing directly to agent_send.
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedAttachmentFile {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentStatsResult {
    pub files: Vec<AttachmentFileStat>,
    pub rejected: Vec<RejectedAttachmentFile>,
}

/// Validate and stat a small batch of picker/drop attachments before the UI
/// reports success. This uses the native capability set populated by the file
/// picker or native drag event; renderer-supplied arbitrary paths cannot grant
/// themselves access through this command.
#[tauri::command]
pub async fn filesystem_attachment_stats(
    access: State<'_, FilesystemAccess>,
    paths: Vec<String>,
) -> Result<AttachmentStatsResult, String> {
    if paths.len() > MAX_PICKED_FILES {
        return Err(format!("一次最多检查 {MAX_PICKED_FILES} 个附件"));
    }
    let mut seen = HashSet::new();
    let mut stats = Vec::with_capacity(paths.len());
    let mut rejected = Vec::new();
    for raw in paths {
        let canonical = match access.require_authorized_file(Path::new(&raw)) {
            Ok(path) => path,
            Err(reason) => {
                rejected.push(RejectedAttachmentFile { path: raw, reason });
                continue;
            }
        };
        if !crate::attachment_blob::is_supported_attachment_path(&canonical) {
            rejected.push(RejectedAttachmentFile {
                path: raw,
                reason: format!("不支持的附件类型：{}", canonical.display()),
            });
            continue;
        }
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let metadata = match std::fs::metadata(&canonical) {
            Ok(metadata) => metadata,
            Err(error) => {
                rejected.push(RejectedAttachmentFile {
                    path: raw,
                    reason: format!("无法读取附件 {}：{error}", canonical.display()),
                });
                continue;
            }
        };
        stats.push(AttachmentFileStat {
            input_path: raw,
            path: canonical.to_string_lossy().into_owned(),
            size_bytes: metadata.len(),
        });
    }
    Ok(AttachmentStatsResult {
        files: stats,
        rejected,
    })
}

/// Stat a path (relative paths resolve against cwd).
#[tauri::command]
pub async fn path_stat(
    access: State<'_, FilesystemAccess>,
    path: String,
    cwd: Option<String>,
) -> Result<PathStat, String> {
    let resolved = resolve_path(&path, cwd.as_deref());
    let authorized = access.is_authorized(&resolved, true)?;
    let absolute = authorized.to_string_lossy().to_string();
    if !resolved.exists() {
        return Ok(PathStat {
            path,
            exists: false,
            kind: "missing".into(),
            absolute,
        });
    }
    let kind = if resolved.is_dir() {
        "directory"
    } else if resolved.is_file() {
        "file"
    } else {
        "other"
    };
    Ok(PathStat {
        path,
        exists: true,
        kind: kind.into(),
        absolute,
    })
}

/// Read a local text file for the in-app preview panel.
/// Caps at `max_bytes` (default 256 KiB) so huge logs don't freeze the UI.
#[tauri::command]
pub async fn read_text_file(
    access: State<'_, FilesystemAccess>,
    path: String,
    cwd: Option<String>,
    max_bytes: Option<u64>,
) -> Result<String, String> {
    read_text_file_authorized(&access, path, cwd, max_bytes)
}

fn read_text_file_authorized(
    access: &FilesystemAccess,
    path: String,
    cwd: Option<String>,
    max_bytes: Option<u64>,
) -> Result<String, String> {
    let resolved = resolve_path(&path, cwd.as_deref());
    let authorized = access.is_authorized(&resolved, false)?;
    if !authorized.is_file() {
        return Err(format!("不是文件：{}", authorized.display()));
    }
    let limit = bounded_limit(max_bytes, TEXT_PREVIEW_MAX_BYTES);
    let (data, truncated) = read_bounded_prefix(&authorized, limit)?;
    let mut text = String::from_utf8_lossy(&data).into_owned();
    if truncated {
        text.push_str("\n\n…(已截断，仅预览前部分内容)");
    }
    Ok(text)
}

/// Read a file's raw bytes as a base64 string (for OOXML docx/pptx/sheet zip
/// extraction in the frontend knowledge base). Native file authorization is
/// required, including for knowledge sources the user explicitly picks.
/// Defaults to 1 MiB; explicitly requested PDF previews are capped at 32 MiB.
#[tauri::command]
pub async fn read_file_base64(
    access: State<'_, FilesystemAccess>,
    path: String,
    max_bytes: Option<u64>,
) -> Result<String, String> {
    read_file_base64_authorized(&access, path, max_bytes)
}

fn read_file_base64_authorized(
    access: &FilesystemAccess,
    path: String,
    max_bytes: Option<u64>,
) -> Result<String, String> {
    let resolved = resolve_path(&path, None);
    let authorized = access.is_authorized(&resolved, false)?;
    if !authorized.is_file() {
        return Err(format!("不是文件：{}", authorized.display()));
    }
    let limit = binary_preview_limits::binary_preview_limit(&authorized, max_bytes);
    let (data, truncated) = read_bounded_prefix(&authorized, limit)?;
    if truncated {
        return Err(format!("文件超过 {}KB 的二进制读取上限", limit / 1024));
    }
    // Base64-standard encode (with padding).
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    Ok(STANDARD.encode(data))
}

/// Write text to a file, restricted to `workspace_root` (session cwd).
/// Creates parent directories as needed. Overwrites existing files.
#[tauri::command]
pub async fn write_text_file(
    access: State<'_, FilesystemAccess>,
    path: String,
    content: String,
    workspace_root: String,
) -> Result<String, String> {
    write_text_file_authorized(&access, path, content, workspace_root)
}

fn write_text_file_authorized(
    access: &FilesystemAccess,
    path: String,
    content: String,
    workspace_root: String,
) -> Result<String, String> {
    if content.len() > TEXT_WRITE_MAX_BYTES {
        return Err(format!(
            "写入内容超过 {} MB 安全上限",
            TEXT_WRITE_MAX_BYTES / (1024 * 1024)
        ));
    }
    if workspace_root.trim().is_empty() {
        return Err("未设置工作区，无法安全写入".into());
    }
    let root = PathBuf::from(&workspace_root);
    if !root.is_absolute() {
        return Err("工作区路径必须是绝对路径".into());
    }
    // `workspace_root` is a claim, not a credential. It must itself be under a
    // root previously granted by native state before it can scope this write.
    let authorized_root = access.is_authorized(&root, false)?;
    if !authorized_root.is_dir() {
        return Err("工作区必须是目录".into());
    }
    let resolved = resolve_path(&path, Some(&workspace_root));
    let safe = ensure_under_workspace(&authorized_root, &resolved)?;
    access.is_authorized(&safe, true)?;
    if let Some(parent) = safe.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败：{e}"))?;
        }
    }
    // Re-resolve after directory creation to close symlink escapes introduced
    // between the first check and the actual write.
    let final_path = access.is_authorized(&safe, true)?;
    write_text_atomically(&final_path, content.as_bytes())?;
    Ok(final_path.to_string_lossy().to_string())
}

/// Export text through a native save dialog and write the exact selected path.
/// The WebView supplies only a display filename/filter, never the destination.
#[tauri::command]
pub async fn export_text_file(
    app: tauri::AppHandle,
    suggested_name: String,
    extension: String,
    content: String,
) -> Result<Option<String>, String> {
    if content.len() > TEXT_WRITE_MAX_BYTES {
        return Err(format!(
            "导出内容超过 {} MB 安全上限",
            TEXT_WRITE_MAX_BYTES / (1024 * 1024)
        ));
    }
    if suggested_name.is_empty()
        || suggested_name.chars().count() > 200
        || suggested_name
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0'))
    {
        return Err("导出文件名不合法".into());
    }
    if extension.is_empty()
        || extension.len() > 16
        || !extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err("导出文件扩展名不合法".into());
    }

    let (sender, receiver) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("导出 EchoAgent 文件")
        .set_file_name(suggested_name)
        .add_filter(extension.to_ascii_uppercase(), &[extension.as_str()])
        .save_file(move |selection| {
            let _ = sender.send(selection);
        });
    let Some(selection) = receiver
        .await
        .map_err(|_| "保存对话框意外关闭".to_string())?
    else {
        return Ok(None);
    };
    let path = selection
        .into_path()
        .map_err(|error| format!("选中的保存位置不是本地路径：{error}"))?;
    if !path.is_absolute() {
        return Err("导出路径必须是绝对路径".into());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "导出路径没有父目录".to_string())?;
    if !parent.is_dir() {
        return Err("导出目录不存在".into());
    }
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| format!("无法解析导出目录：{error}"))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| "导出路径没有文件名".to_string())?;
    let destination = canonical_parent.join(file_name);
    write_text_atomically(&destination, content.as_bytes())?;
    Ok(Some(destination.to_string_lossy().into_owned()))
}

/// A single directory entry returned by [`list_dir`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    /// File/dir name (basename).
    pub name: String,
    /// Absolute path of the entry.
    pub path: String,
    /// "directory" | "file" | "other" | "symlink" (SP5).
    pub kind: String,
    /// File size in bytes (directories report 0).
    pub size: u64,
    /// Last modified time in Unix milliseconds (0 when unavailable).
    pub modified_at: u64,
    /// SP5: true when `size > LARGE_FILE_BYTES` (2 MiB).
    pub is_large: bool,
    /// SP5: true when the extension matches the binary list OR the first
    /// `BINARY_SNIFF_BYTES` contain a NUL byte.
    pub is_binary: bool,
}

/// SP5: size threshold above which a file is flagged as `is_large`. 2 MiB.
pub const LARGE_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// SP5: how many leading bytes to read when sniffing for binary content.
pub const BINARY_SNIFF_BYTES: usize = 8 * 1024;
/// SP5: extension whitelist — files with these suffixes are always treated
/// as binary regardless of their first bytes.
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "pdf", "zip", "tar", "gz", "bz2", "xz",
    "7z", "rar", "exe", "dll", "so", "dylib", "bin", "wasm", "class", "mp3", "mp4", "mov", "avi",
    "mkv", "flac", "ogg", "wav", "ttf", "otf", "woff", "woff2",
];

fn looks_binary_by_extension(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if let Some(dot) = lower.rfind('.') {
        let ext = &lower[dot + 1..];
        BINARY_EXTENSIONS.contains(&ext)
    } else {
        false
    }
}

fn looks_binary_by_sniff(path: &Path) -> bool {
    if let Ok(mut f) = std::fs::File::open(path) {
        use std::io::Read;
        let mut buf = [0u8; BINARY_SNIFF_BYTES];
        if let Ok(n) = f.read(&mut buf) {
            return buf[..n].contains(&0);
        }
    }
    false
}

/// Directory names that are skipped by [`list_dir`] to keep the file tree
/// manageable and avoid scanning VCS/build noise. Hidden entries (leading dot)
/// are skipped separately.
const IGNORED_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    ".svn",
    ".hg",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".cache",
    ".turbo",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
];

/// List the immediate children of a directory (non-recursive).
///
/// Relative paths resolve against `cwd`. Hidden entries (leading `.`) and a
/// curated set of noisy build/VCS directories are skipped by default. Pass
/// `include_hidden = true` to surface dotfile entries, and pass
/// `ignore_files` to override the default `.gitignore` / `.ignore` /
/// `.echoagentignore` chain (basenames only). Capped at `max_entries`
/// (default 2000) so a huge directory can't freeze the UI.
#[tauri::command]
pub async fn list_dir(
    access: State<'_, FilesystemAccess>,
    path: String,
    cwd: Option<String>,
    max_entries: Option<usize>,
    include_hidden: Option<bool>,
    ignore_files: Option<Vec<String>>,
) -> Result<Vec<DirEntry>, String> {
    let (authorized, ignore_root) = resolve_list_dir_paths(&access, &path, cwd.as_deref())?;
    tokio::task::spawn_blocking(move || {
        list_dir_resolved(
            authorized,
            ignore_root,
            max_entries,
            include_hidden,
            ignore_files,
        )
    })
    .await
    .map_err(|error| format!("读取目录任务失败：{error}"))?
}

#[cfg(test)]
fn list_dir_authorized(
    access: &FilesystemAccess,
    path: String,
    cwd: Option<String>,
    max_entries: Option<usize>,
    include_hidden: Option<bool>,
    ignore_files: Option<Vec<String>>,
) -> Result<Vec<DirEntry>, String> {
    let (authorized, ignore_root) = resolve_list_dir_paths(access, &path, cwd.as_deref())?;
    list_dir_resolved(
        authorized,
        ignore_root,
        max_entries,
        include_hidden,
        ignore_files,
    )
}

fn resolve_list_dir_paths(
    access: &FilesystemAccess,
    path: &str,
    cwd: Option<&str>,
) -> Result<(PathBuf, PathBuf), String> {
    let resolved = resolve_path(path, cwd);
    let authorized = access.is_authorized(&resolved, false)?;
    if !authorized.is_dir() {
        return Err(format!("不是目录：{}", authorized.display()));
    }
    // `authorized` is the directory currently being expanded. Ignore rules,
    // however, must be evaluated from the workspace root so a root
    // `.gitignore` continues to apply inside nested folders.
    let ignore_root = cwd
        .and_then(|claimed| access.require_workspace(claimed).ok())
        .filter(|workspace| authorized.starts_with(workspace))
        .unwrap_or_else(|| authorized.clone());
    Ok((authorized, ignore_root))
}

fn list_dir_resolved(
    authorized: PathBuf,
    ignore_root: PathBuf,
    max_entries: Option<usize>,
    include_hidden: Option<bool>,
    ignore_files: Option<Vec<String>>,
) -> Result<Vec<DirEntry>, String> {
    let limit = max_entries
        .unwrap_or(DIRECTORY_LIST_MAX_ENTRIES)
        .min(DIRECTORY_LIST_MAX_ENTRIES);
    let show_hidden = include_hidden.unwrap_or(false);
    // Translate ignore_files basenames to &str slices for the chain helper.
    let ignore_names: Vec<&str> = match ignore_files.as_ref() {
        Some(list) => list.iter().map(String::as_str).collect(),
        None => crate::coding::gitignore_chain::DEFAULT_IGNORE_FILES.to_vec(),
    };
    let read = authorized
        .read_dir()
        .map_err(|e| format!("读取目录失败：{e}"))?;

    let mut entries: Vec<DirEntry> = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // skip unreadable entries
        };
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy().to_string();
        let starts_with_dot = name.starts_with('.');
        if starts_with_dot && !show_hidden {
            continue;
        }
        let ft = entry.file_type();
        // SP5: classify symlinks explicitly. All file operations reject symlinks
        // elsewhere, so exposing them here would only mislead users.
        if ft.as_ref().map(|t| t.is_symlink()).unwrap_or(false) {
            entries.push(DirEntry {
                name,
                path: entry.path().to_string_lossy().to_string(),
                kind: "symlink".into(),
                size: 0,
                modified_at: 0,
                is_large: false,
                is_binary: false,
            });
            if entries.len() >= limit {
                break;
            }
            continue;
        }
        let is_dir = ft.as_ref().map(|t| t.is_dir()).unwrap_or(false);
        // Skip noisy directories (only applies to directories).
        if is_dir && IGNORED_DIRS.iter().any(|d| *d == name) {
            continue;
        }
        // Apply the nested ignore chain to honour `.echoagentignore` /
        // `.gitignore` (deeper whitelist wins, matching Git precedence).
        if crate::coding::gitignore_chain::apply_nested_gitignore(
            &ignore_root,
            &entry.path(),
            is_dir,
            &ignore_names,
        ) {
            continue;
        }
        let is_file = ft.as_ref().map(|t| t.is_file()).unwrap_or(false);
        let kind = if is_dir {
            "directory"
        } else if is_file {
            "file"
        } else {
            "other"
        };
        let metadata = entry.metadata().ok();
        let size = if is_dir {
            0
        } else {
            metadata.as_ref().map(|m| m.len()).unwrap_or(0)
        };
        // SP5: classify large / binary. Binary sniffing is gated by `is_file`
        // and `size > 0` so we never open a directory fd.
        let is_large = !is_dir && size > LARGE_FILE_BYTES;
        let mut is_binary = !is_dir && looks_binary_by_extension(&name);
        if !is_binary && is_file && size > 0 {
            is_binary = looks_binary_by_sniff(&entry.path());
        }
        let modified_at = metadata
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or(0);
        entries.push(DirEntry {
            name,
            path: entry.path().to_string_lossy().to_string(),
            kind: kind.into(),
            size,
            modified_at,
            is_large,
            is_binary,
        });
        if entries.len() >= limit {
            break;
        }
    }
    // Directories first, then files; each group alphabetical (case-insensitive).
    entries.sort_by(|a, b| {
        let ad = a.kind == "directory";
        let bd = b.kind == "directory";
        bd.cmp(&ad)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

/// Browse / open a directory in the file manager (implements frontend's browse_directory).
#[tauri::command]
pub async fn browse_directory(
    access: State<'_, FilesystemAccess>,
    path: String,
) -> Result<(), String> {
    let p = PathBuf::from(&path);
    let p = access.is_authorized(&p, false)?;
    let target = if p.is_dir() {
        p
    } else {
        p.parent().map(|x| x.to_path_buf()).unwrap_or(p)
    };
    open::that(target).map_err(|e| format!("打开目录失败：{e}"))
}

/// Return the actual EchoAgent data directory (respects ECHO_AGENT_HOME).
#[tauri::command]
pub fn echo_agent_data_dir() -> Result<String, String> {
    let path = crate::paths::echo_agent_home_dir();
    std::fs::create_dir_all(&path).map_err(|e| format!("创建数据目录失败：{e}"))?;
    Ok(path.to_string_lossy().to_string())
}

/// Open EchoAgent's private data directory in the system file manager.
#[tauri::command]
pub fn open_echo_agent_data_dir() -> Result<(), String> {
    let path = crate::paths::echo_agent_home_dir();
    std::fs::create_dir_all(&path).map_err(|e| format!("创建数据目录失败：{e}"))?;
    open::that(&path).map_err(|e| format!("打开数据目录失败：{e}"))
}

// ---------- unit tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_picked_workspace_grant_is_durable_and_deduplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("automation-workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let grant_file = tmp.path().join("picked-workspaces.json");
        let access = FilesystemAccess::default();
        assert!(access
            .require_workspace(&workspace.to_string_lossy())
            .is_err());

        let canonical = access
            .authorize_picked_workspace_at(&workspace.to_string_lossy(), &grant_file)
            .unwrap();
        access
            .authorize_picked_workspace_at(&workspace.to_string_lossy(), &grant_file)
            .unwrap();
        let grants = read_picked_workspace_grants(&grant_file).unwrap();
        assert_eq!(grants, vec![canonical.to_string_lossy().into_owned()]);

        let restored = FilesystemAccess::default();
        for grant in grants {
            restored.authorize_workspace(&grant).unwrap();
        }
        assert_eq!(
            restored
                .require_workspace(&workspace.to_string_lossy())
                .unwrap(),
            canonical
        );
    }

    #[cfg(unix)]
    #[test]
    fn picked_workspace_grants_reject_symlinked_records() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target.json");
        let link = tmp.path().join("grants.json");
        std::fs::write(&target, "[]").unwrap();
        symlink(&target, &link).unwrap();
        assert!(read_picked_workspace_grants(&link).is_err());
    }

    // --- resolve_path ---

    #[test]
    fn external_url_validation_rejects_unsafe_or_ambiguous_values() {
        assert!(validated_external_url("https://example.com/docs").is_ok());
        assert!(validated_external_url("mailto:user@example.com?subject=Echo").is_ok());
        assert!(validated_external_url("javascript:alert(1)").is_err());
        assert!(validated_external_url("https://user:secret@example.com").is_err());
        assert!(validated_external_url("https://example.com/\nmalformed").is_err());
        assert!(
            validated_external_url(&format!("https://example.com/{}", "x".repeat(8_192))).is_err()
        );
    }

    #[test]
    fn resolve_absolute_path_passthrough() {
        #[cfg(windows)]
        {
            let result = resolve_path("C:\\Users\\test\\file.txt", None);
            assert_eq!(result, PathBuf::from("C:\\Users\\test\\file.txt"));
        }
        #[cfg(unix)]
        {
            let result = resolve_path("/home/user/file.txt", None);
            assert_eq!(result, PathBuf::from("/home/user/file.txt"));
        }
    }

    #[test]
    fn resolve_relative_with_cwd() {
        let result = resolve_path("src/main.rs", Some("/home/project"));
        assert_eq!(result, PathBuf::from("/home/project/src/main.rs"));
    }

    #[test]
    fn resolve_relative_without_cwd() {
        let result = resolve_path("src/main.rs", None);
        assert_eq!(result, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn resolve_relative_with_empty_cwd() {
        let result = resolve_path("src/main.rs", Some(""));
        assert_eq!(result, PathBuf::from("src/main.rs"));
    }

    // --- ensure_under_workspace ---

    #[test]
    fn ensure_under_workspace_allows_child() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let child = root.join("sub/file.txt");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(&child, "hello").unwrap();

        let result = ensure_under_workspace(root, &child);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), child.canonicalize().unwrap());
    }

    #[test]
    fn ensure_under_workspace_rejects_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();

        let outside = tmp.path().join("outside.txt");
        std::fs::write(&outside, "evil").unwrap();

        let result = ensure_under_workspace(&root, &outside);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("拒绝写入工作区之外"));
    }

    #[test]
    fn ensure_under_workspace_new_file_in_existing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let new_file = root.join("new_file.txt");
        // File doesn't exist yet, but parent (root) does.
        let result = ensure_under_workspace(root, &new_file);
        assert!(result.is_ok());
    }

    #[test]
    fn ensure_under_workspace_does_not_create_rejected_parent_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        let outside_parent = tmp.path().join("outside").join("nested");
        let candidate = outside_parent.join("file.txt");

        assert!(ensure_under_workspace(&root, &candidate).is_err());
        assert!(!outside_parent.exists());
    }

    #[test]
    fn ensure_under_workspace_allows_new_nested_directory_without_creating_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let parent = root.join("new").join("nested");
        let candidate = parent.join("file.txt");

        let safe = ensure_under_workspace(root, &candidate).unwrap();
        assert!(safe.starts_with(root.canonicalize().unwrap()));
        assert!(!parent.exists());
    }

    #[test]
    fn bounded_file_read_never_allocates_past_native_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("large.bin");
        std::fs::write(&file, vec![b'x'; 1024]).unwrap();

        let (data, truncated) = read_bounded_prefix(&file, 32).unwrap();
        assert_eq!(data.len(), 32);
        assert!(truncated);
        assert_eq!(bounded_limit(Some(u64::MAX), 64), 64);
        assert_eq!(bounded_limit(Some(8), 64), 8);
    }

    #[test]
    fn renderer_cannot_raise_text_preview_hard_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("large.txt");
        std::fs::write(&file, vec![b'x'; TEXT_PREVIEW_MAX_BYTES + 1024]).unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&tmp.path().to_string_lossy())
            .unwrap();
        let result = read_text_file_authorized(
            &access,
            file.to_string_lossy().into_owned(),
            None,
            Some(u64::MAX),
        )
        .unwrap();
        assert!(result.starts_with(&"x".repeat(TEXT_PREVIEW_MAX_BYTES)));
        assert!(result.ends_with("…(已截断，仅预览前部分内容)"));
    }

    #[test]
    fn binary_preview_rejects_truncated_payloads() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("large.bin");
        std::fs::write(&file, vec![b'x'; 65]).unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&tmp.path().to_string_lossy())
            .unwrap();
        let error =
            read_file_base64_authorized(&access, file.to_string_lossy().into_owned(), Some(64))
                .unwrap_err();
        assert!(error.contains("二进制读取上限"));
    }

    #[test]
    fn renderer_cannot_raise_directory_listing_hard_limit() {
        let tmp = tempfile::tempdir().unwrap();
        for index in 0..(DIRECTORY_LIST_MAX_ENTRIES + 20) {
            std::fs::write(tmp.path().join(format!("{index:04}.txt")), "").unwrap();
        }
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&tmp.path().to_string_lossy())
            .unwrap();
        let entries = list_dir_authorized(
            &access,
            tmp.path().to_string_lossy().into_owned(),
            None,
            Some(usize::MAX),
            None,
            None,
        )
        .unwrap();
        assert_eq!(entries.len(), DIRECTORY_LIST_MAX_ENTRIES);
    }

    // --- list_dir (logic check via direct std::fs + IGNORED_DIRS semantics) ---

    #[test]
    fn list_dir_returns_dirs_first_then_files_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("b.txt"), "").unwrap();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::create_dir_all(root.join("zdir")).unwrap();
        std::fs::create_dir_all(root.join("adir")).unwrap();

        let access = FilesystemAccess::default();
        access.authorize_workspace(&root.to_string_lossy()).unwrap();
        let entries = list_dir_authorized(
            &access,
            root.to_string_lossy().to_string(),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        // Directories first (alphabetical), then files (alphabetical).
        assert_eq!(names, vec!["adir", "zdir", "a.txt", "b.txt"]);
    }

    #[test]
    fn list_dir_skips_hidden_and_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".hidden"), "").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("node_modules")).unwrap();
        std::fs::write(root.join("keep.txt"), "").unwrap();

        let access = FilesystemAccess::default();
        access.authorize_workspace(&root.to_string_lossy()).unwrap();
        let entries = list_dir_authorized(
            &access,
            root.to_string_lossy().to_string(),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["keep.txt".to_string()]);
    }

    #[test]
    fn list_dir_uses_workspace_root_ignore_chain_for_nested_directories() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(".gitignore"), "*.generated.ts\n").unwrap();
        std::fs::write(nested.join("model.generated.ts"), "generated").unwrap();
        std::fs::write(nested.join("model.ts"), "source").unwrap();

        let access = FilesystemAccess::default();
        access.authorize_workspace(&root.to_string_lossy()).unwrap();
        let entries = list_dir_authorized(
            &access,
            nested.to_string_lossy().into_owned(),
            Some(root.to_string_lossy().into_owned()),
            None,
            Some(false),
            None,
        )
        .unwrap();
        let names: Vec<String> = entries.into_iter().map(|entry| entry.name).collect();
        assert_eq!(names, vec!["model.ts".to_string()]);
    }

    #[test]
    fn list_dir_show_hidden_includes_ignore_rule_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();

        let access = FilesystemAccess::default();
        access.authorize_workspace(&root.to_string_lossy()).unwrap();
        let entries = list_dir_authorized(
            &access,
            root.to_string_lossy().into_owned(),
            Some(root.to_string_lossy().into_owned()),
            None,
            Some(true),
            None,
        )
        .unwrap();
        assert!(entries.iter().any(|entry| entry.name == ".gitignore"));
    }

    #[test]
    fn list_dir_rejects_file_path() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("file.txt");
        std::fs::write(&file, "").unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&tmp.path().to_string_lossy())
            .unwrap();
        let result = list_dir_authorized(
            &access,
            file.to_string_lossy().to_string(),
            None,
            None,
            None,
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn backend_allow_list_rejects_renderer_asserted_outside_path() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();

        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&workspace.to_string_lossy())
            .unwrap();

        let error = read_text_file_authorized(
            &access,
            outside.join("secret.txt").to_string_lossy().into_owned(),
            Some(outside.to_string_lossy().into_owned()),
            None,
        )
        .unwrap_err();
        assert!(error.contains("未授权"));
    }

    #[test]
    fn write_rejects_spoofed_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&workspace.to_string_lossy())
            .unwrap();
        let result = write_text_file_authorized(
            &access,
            "forged.txt".into(),
            "should not be written".into(),
            outside.to_string_lossy().into_owned(),
        );

        assert!(result.unwrap_err().contains("未授权"));
        assert!(!outside.join("forged.txt").exists());
    }

    #[test]
    fn authorized_write_is_atomic_and_stays_under_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&workspace.to_string_lossy())
            .unwrap();

        let written = write_text_file_authorized(
            &access,
            "nested/result.txt".into(),
            "ok".into(),
            workspace.to_string_lossy().into_owned(),
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(&written).unwrap(), "ok");
        assert!(Path::new(&written).starts_with(workspace.canonicalize().unwrap()));
    }

    #[test]
    fn relative_path_without_authorized_cwd_is_rejected() {
        let access = FilesystemAccess::default();
        let error = access
            .is_authorized(Path::new("relative.txt"), true)
            .unwrap_err();
        assert!(error.contains("绝对路径"));
    }

    #[cfg(unix)]
    #[test]
    fn allow_list_resolves_symlinks_and_rejects_escape() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink(&outside, workspace.join("escape")).unwrap();

        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&workspace.to_string_lossy())
            .unwrap();
        let error = access
            .is_authorized(&workspace.join("escape/secret.txt"), false)
            .unwrap_err();
        assert!(error.contains("未授权"));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_writer_does_not_follow_destination_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target.txt");
        let link = tmp.path().join("link.txt");
        std::fs::write(&target, "original").unwrap();
        symlink(&target, &link).unwrap();

        let error = write_text_atomically(&link, b"replacement").unwrap_err();
        assert!(error.contains("符号链接"));
        assert_eq!(std::fs::read_to_string(target).unwrap(), "original");
    }

    #[test]
    fn task_children_do_not_exhaust_workspace_authorization_slots() {
        let base = tempfile::tempdir().unwrap();
        let child = base.path().join("first");
        std::fs::create_dir(&child).unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&child.to_string_lossy())
            .unwrap();
        access
            .authorize_workspace(&base.path().to_string_lossy())
            .unwrap();
        for index in 0..20 {
            let task = base.path().join(format!("task-{index}"));
            std::fs::create_dir(&task).unwrap();
            access.authorize_workspace(&task.to_string_lossy()).unwrap();
            assert_eq!(
                access.require_workspace(&task.to_string_lossy()).unwrap(),
                task.canonicalize().unwrap()
            );
        }
        assert_eq!(access.roots.lock().unwrap().len(), 1);
        assert!(access.require_workspace(&child.to_string_lossy()).is_ok());
    }

    #[test]
    fn duplicate_workspace_grants_are_canonical_and_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let access = FilesystemAccess::default();
        let first = access
            .authorize_workspace(&tmp.path().to_string_lossy())
            .unwrap();
        let second = access
            .authorize_workspace(&tmp.path().join(".").to_string_lossy())
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(access.authorized_roots(), vec![first]);
    }

    #[test]
    fn session_attachments_must_stay_inside_the_bound_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let allowed = workspace.join("allowed.txt");
        let outside = other.join("secret.txt");
        std::fs::write(&allowed, "ok").unwrap();
        std::fs::write(&outside, "secret").unwrap();

        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&workspace.to_string_lossy())
            .unwrap();
        access
            .authorize_workspace(&other.to_string_lossy())
            .unwrap();

        let accepted = access
            .validate_session_attachments(&workspace, &[allowed.to_string_lossy().into_owned()])
            .unwrap();
        assert_eq!(
            accepted,
            vec![allowed
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()]
        );
        let error = access
            .validate_session_attachments(&workspace, &[outside.to_string_lossy().into_owned()])
            .unwrap_err();
        assert!(error.contains("当前会话工作区"));
    }

    #[test]
    fn native_exact_file_grant_allows_an_external_attachment() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("selected.txt");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(&outside, "chosen by user").unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&workspace.to_string_lossy())
            .unwrap();
        let canonical = access.authorize_file(&outside).unwrap();

        let accepted = access
            .validate_session_attachments(&workspace, &[outside.to_string_lossy().into_owned()])
            .unwrap();
        assert_eq!(accepted, vec![canonical.to_string_lossy().into_owned()]);
    }

    /// Regression: a file previously approved via `authorize_file` (file picker
    /// selection, paste-image landing) must be readable through the same
    /// `is_authorized` check that backs `open_path` / `reveal_in_folder` /
    /// `path_stat` / `read_text_file`. Without this, every previewer would
    /// surface "拒绝访问未授权的路径" the moment a user clicked an approved
    /// attachment that lives outside the bound workspace.
    #[test]
    fn exact_file_grant_allows_preview_outside_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let paste = tmp.path().join("pasted.png");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(&paste, b"png-bytes").unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&workspace.to_string_lossy())
            .unwrap();
        access.authorize_file(&paste).unwrap();

        let canonical = access
            .is_authorized(&paste, false)
            .expect("picker-granted file should be readable");
        assert_eq!(canonical, paste.canonicalize().unwrap());
    }

    /// Regression: `is_authorized` still rejects arbitrary renderer-asserted
    /// paths that were never approved through a backend-owned picker. This
    /// keeps the security boundary intact while letting the previous test pass.
    #[test]
    fn ungranted_paths_remain_rejected_after_exact_file_check_is_added() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("unauthorized.txt");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(&outside, "secret").unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&workspace.to_string_lossy())
            .unwrap();

        let error = access.is_authorized(&outside, false).unwrap_err();
        assert!(error.contains("未授权"));
    }

    /// `open_path` is the desktop command fired when a user clicks an
    /// attachment chip; it must succeed for a previously picker-granted file
    /// even when that file lives outside every authorized workspace root.
    #[test]
    fn open_path_succeeds_for_exact_file_grant() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("picked.png");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(&outside, b"png-bytes").unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&workspace.to_string_lossy())
            .unwrap();
        access.authorize_file(&outside).unwrap();

        // Mirror what the `open_path` Tauri command does: resolve against
        // optional cwd, then run through `is_authorized`.
        let resolved = resolve_path(&outside.to_string_lossy(), None);
        let canonical = access
            .is_authorized(&resolved, false)
            .expect("open_path should accept picker-granted file");
        assert!(canonical.is_file());
    }

    /// Files registered through `authorize_file` survive across calls to
    /// `is_authorized` regardless of declaration order, mirroring the way a
    /// user may click any one of several pasted attachments.
    #[test]
    fn multiple_exact_file_grants_each_pass_independently() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("clipboard-images");
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("image-1.png");
        let second = dir.join("image-2.jpg");
        std::fs::write(&first, b"a").unwrap();
        std::fs::write(&second, b"b").unwrap();
        let access = FilesystemAccess::default();
        access.authorize_file(&first).unwrap();
        access.authorize_file(&second).unwrap();

        let first_canonical = access
            .is_authorized(&first, false)
            .expect("first authorized file should pass");
        let second_canonical = access
            .is_authorized(&second, false)
            .expect("second authorized file should pass");
        assert_eq!(first_canonical, first.canonicalize().unwrap());
        assert_eq!(second_canonical, second.canonicalize().unwrap());
    }

    /// `register_attachment_store` is the durable-side hook called once
    /// during application setup. After a process restart the in-memory
    /// `files` set is empty, so without re-adopting the files the user
    /// already landed in `<app_data_dir>/clipboard-images/`, every preview
    /// of a historical attachment would fail. Scanned files must round-trip
    /// through `is_authorized` like a freshly saved blob.
    #[test]
    fn register_attachment_store_adopts_existing_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("clipboard-images");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("image-1700000000000-abcdef.png");
        let jpg = dir.join("image-1700000000001-123456.jpg");
        let text = dir.join("notes.txt");
        let unsupported = dir.join("program.exe");
        std::fs::write(&png, b"png-bytes").unwrap();
        std::fs::write(&jpg, b"jpg-bytes").unwrap();
        std::fs::write(&text, b"plain").unwrap();
        std::fs::write(&unsupported, b"MZ").unwrap();

        let access = FilesystemAccess::default();
        access.register_attachment_store(&dir);

        let png_canonical = access
            .is_authorized(&png, false)
            .expect("previously saved png must be re-authorized after restart");
        assert_eq!(png_canonical, png.canonicalize().unwrap());
        let jpg_canonical = access
            .is_authorized(&jpg, false)
            .expect("previously saved jpg must be re-authorized after restart");
        assert_eq!(jpg_canonical, jpg.canonicalize().unwrap());
        let text_canonical = access
            .is_authorized(&text, false)
            .expect("supported text paste must be re-authorized after restart");
        assert_eq!(text_canonical, text.canonicalize().unwrap());

        // Non-image siblings must not be silently admitted as attachments.
        let error = access.is_authorized(&unsupported, false).unwrap_err();
        assert!(error.contains("未授权"));
    }

    /// Missing clipboard-images directory is the cold-start case: nothing has
    /// ever been pasted. The hook must be a no-op without raising so startup
    /// does not fail on a fresh machine.
    #[test]
    fn register_attachment_store_creates_missing_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("never-created");
        let access = FilesystemAccess::default();
        access.register_attachment_store(&missing);
        assert!(missing.is_dir());
    }

    /// Symlinks planted inside clipboard-images must be rejected the same
    /// way `authorize_file` rejects them. A compromised renderer should
    /// never be able to redirect preview traffic through a link to
    /// credentials sitting next to the data dir.
    #[cfg(unix)]
    #[test]
    fn register_attachment_store_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("clipboard-images");
        std::fs::create_dir_all(&dir).unwrap();
        let target = tmp.path().join("secret.txt");
        std::fs::write(&target, b"outside").unwrap();
        symlink(&target, dir.join("image-link.png")).unwrap();

        let access = FilesystemAccess::default();
        access.register_attachment_store(&dir);

        let error = access
            .is_authorized(&dir.join("image-link.png"), false)
            .unwrap_err();
        assert!(error.contains("未授权"));
    }

    #[cfg(unix)]
    #[test]
    fn session_attachments_reject_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target.png");
        let link = tmp.path().join("link.png");
        std::fs::write(&target, "image-like secret").unwrap();
        symlink(&target, &link).unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&tmp.path().to_string_lossy())
            .unwrap();

        let error = access
            .validate_session_attachments(tmp.path(), &[link.to_string_lossy().into_owned()])
            .unwrap_err();
        assert!(error.contains("符号链接"));
    }

    #[test]
    fn session_attachments_enforce_count_and_size_limits() {
        let tmp = tempfile::tempdir().unwrap();
        let access = FilesystemAccess::default();
        access
            .authorize_workspace(&tmp.path().to_string_lossy())
            .unwrap();
        let too_many = vec![
            tmp.path()
                .join("missing.txt")
                .to_string_lossy()
                .into_owned();
            21
        ];
        assert!(access
            .validate_session_attachments(tmp.path(), &too_many)
            .unwrap_err()
            .contains("数量"));

        let oversized = tmp.path().join("oversized.bin");
        let file = std::fs::File::create(&oversized).unwrap();
        file.set_len(MAX_ATTACHMENT_FILE_BYTES + 1).unwrap();
        assert!(access
            .validate_session_attachments(tmp.path(), &[oversized.to_string_lossy().into_owned()])
            .unwrap_err()
            .contains("单个附件"));

        let mut total = Vec::new();
        for index in 0..4 {
            let path = tmp.path().join(format!("chunk-{index}.bin"));
            let file = std::fs::File::create(&path).unwrap();
            file.set_len(17 * 1024 * 1024).unwrap();
            total.push(path.to_string_lossy().into_owned());
        }
        assert!(access
            .validate_session_attachments(tmp.path(), &total)
            .unwrap_err()
            .contains("总大小"));
    }

    #[test]
    fn trusted_package_sources_are_narrow_and_configured_removal_is_exact() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog_skill = tmp.path().join("catalog-skill");
        let configured = tmp.path().join("configured");
        std::fs::create_dir_all(&catalog_skill).unwrap();
        std::fs::create_dir_all(&configured).unwrap();
        std::fs::write(catalog_skill.join("SKILL.md"), "catalog").unwrap();
        std::fs::write(configured.join("SKILL.md"), "configured").unwrap();

        let access = FilesystemAccess::default();
        let trusted = access
            .record_trusted_package_source(&catalog_skill)
            .unwrap();
        assert_eq!(
            access
                .require_authorized_package_source(&catalog_skill)
                .unwrap(),
            trusted
        );
        assert!(access
            .require_authorized_file(&catalog_skill.join("SKILL.md"))
            .is_err());

        access
            .record_configured_skill_source(&configured.to_string_lossy())
            .unwrap();
        assert_eq!(
            access
                .require_configured_skill_source(&configured.to_string_lossy())
                .unwrap(),
            configured.to_string_lossy().into_owned()
        );
        assert!(access
            .require_configured_skill_source(&catalog_skill.to_string_lossy())
            .is_err());
    }

    #[test]
    fn strict_reader_rejects_files_over_the_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("large.txt");
        std::fs::write(&path, b"12345").unwrap();
        assert!(read_regular_file_bounded(&path, 4).is_err());
        assert_eq!(read_regular_file_bounded(&path, 5).unwrap(), b"12345");
        assert_eq!(read_regular_file_prefix(&path, 4).unwrap(), b"1234");
    }

    #[cfg(unix)]
    #[test]
    fn strict_reader_never_follows_a_final_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target.txt");
        let link = tmp.path().join("link.txt");
        std::fs::write(&target, "secret").unwrap();
        symlink(&target, &link).unwrap();
        assert!(read_regular_file_bounded(&link, 1024).is_err());
        assert!(read_regular_file_prefix(&link, 1024).is_err());
    }
}
