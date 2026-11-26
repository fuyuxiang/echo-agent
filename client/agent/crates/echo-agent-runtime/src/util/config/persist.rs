use super::load::load_config_from_toml;
use super::mcp::{Config, user_config_path};
use anyhow::Result;
use echo_agent_core::prompt::skills::SkillsConfig;
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use toml::Value as TomlValue;
use toml::map::Map as TomlMap;
/// Process-wide write lock for `~/.echo-agent/config.toml`.
///
/// Serializes the read-modify-write in `save_config` so two rapid
/// settings toggles can't interleave and clobber each other.
static SAVE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// All Runtime config writers use this advisory lock file. The historical
/// `.config-init.lock` name is retained so marketplace bootstrap and newer
/// settings/MCP/desktop writers coordinate on the same existing artifact.
const CONFIG_TRANSACTION_LOCK_FILE: &str = ".config-init.lock";

/// Runtime configuration is user-authored and expected to stay small. Bound
/// reads and writes so a planted device/FIFO or an unexpectedly huge file
/// cannot make a settings transaction consume unbounded memory.
const MAX_CONFIG_FILE_BYTES: u64 = 8 * 1024 * 1024;

static CONFIG_TRANSACTION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Owns both the shared in-process mutex and the cross-process advisory lock.
/// Fields are intentionally private so the lock lifetime cannot be shortened
/// independently of the guard returned by [`acquire_config_transaction_lock_at`].
pub struct ConfigTransactionGuard {
    _file: File,
    _process: MutexGuard<'static, ()>,
}

/// Acquire the cross-crate/cross-process transaction lock for `config_path`.
///
/// The returned guard owns both locks until it is dropped. Callers must
/// acquire it *before* reading and retain it through the atomic replacement;
/// locking only the final write still permits lost read-modify-write updates.
pub fn acquire_config_transaction_lock_at(
    config_path: &Path,
) -> io::Result<ConfigTransactionGuard> {
    let process = CONFIG_TRANSACTION_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let parent = ensure_safe_config_parent(config_path)?;
    let lock_path = parent.join(CONFIG_TRANSACTION_LOCK_FILE);
    match std::fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Runtime config lock is not a regular file: {}",
                    lock_path.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&lock_path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Runtime config lock changed during open: {}",
                lock_path.display()
            ),
        ));
    }
    for _ in 0..200 {
        match file.try_lock_exclusive() {
            Ok(()) => {
                return Ok(ConfigTransactionGuard {
                    _file: file,
                    _process: process,
                });
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.raw_os_error() == fs2::lock_contended_error().raw_os_error() =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!(
            "timed out waiting for Runtime config transaction lock {}",
            lock_path.display()
        ),
    ))
}

/// Atomically update one TOML configuration from its latest on-disk snapshot.
///
/// This is the single transaction primitive shared by the embedded Runtime and
/// the desktop shell. It refuses malformed input rather than replacing it with
/// an empty document and preserves every key the updater does not touch.
pub fn update_config_toml_at<T>(
    path: &Path,
    update: impl FnOnce(&mut TomlValue) -> Result<T>,
) -> Result<T> {
    let _transaction = acquire_config_transaction_lock_at(path)?;
    let content = read_to_string_or_empty(path)?;
    let mut root = if content.is_empty() {
        TomlValue::Table(TomlMap::new())
    } else {
        toml::from_str::<TomlValue>(&content).map_err(|parse_err| {
            anyhow::anyhow!(
                "refusing to overwrite unparseable {}: {}; save a backup and fix the syntax error before retrying",
                path.display(),
                parse_err,
            )
        })?
    };
    if !root.is_table() {
        anyhow::bail!("config root is not a table: {}", path.display());
    }
    let result = update(&mut root)?;
    let body = toml::to_string_pretty(&root)?;
    atomic_write_string(path, &body)?;
    Ok(result)
}

/// Merge the typed settings subset into a caller-provided latest snapshot.
fn merge_config_into_root(root: &mut TomlValue, config: &Config) -> Result<()> {
    if !matches!(root, TomlValue::Table(_)) {
        *root = TomlValue::Table(TomlMap::new());
    }
    let table = root.as_table_mut().expect("root must be a table");
    merge_section(table, "cli", &config.cli);
    merge_section(table, "models", &config.models);
    merge_section(table, "ui", &config.ui);
    merge_section(table, "harness", &config.harness);
    merge_section(table, "session", &config.session);
    merge_ask_user_question_section(table, &config.ask_user_question);
    if config.privacy == super::mcp::PrivacyConfig::default() {
        table.remove("privacy");
    } else {
        merge_section(table, "privacy", &config.privacy);
    }
    if config.consent == super::consent::ConsentConfig::default() {
        table.remove("consent");
    } else {
        if let Some(TomlValue::Table(section)) = table.get_mut("consent") {
            section.remove("answers");
        }
        merge_section(table, "consent", &config.consent);
    }
    if config.skills == SkillsConfig::default() {
        table.remove("skills");
    } else {
        merge_section(table, "skills", &config.skills);
    }
    merge_section(table, "telemetry", &config.telemetry);
    merge_section(table, "features", &config.features);
    Ok(())
}
/// Acquire the `config.toml` write lock used by [`save_config`], so callers that
/// mutate the file directly (marketplace add/remove) can't interleave with a
/// settings save and clobber it.
pub(crate) async fn lock_config_writes() -> tokio::sync::MutexGuard<'static, ()> {
    SAVE_LOCK.lock().await
}
fn config_parent(path: &Path) -> io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("config path has no parent: {}", path.display()),
        )
    })?;
    if parent.as_os_str().is_empty() {
        Ok(Path::new("."))
    } else {
        Ok(parent)
    }
}

fn ensure_safe_config_parent(path: &Path) -> io::Result<&Path> {
    let parent = config_parent(path)?;
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Runtime config parent is not a real directory: {}",
                    parent.display()
                ),
            ));
        }
        Ok(_) => return Ok(parent),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::create_dir_all(parent)?;
    let metadata = std::fs::symlink_metadata(parent)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Runtime config parent is not a real directory: {}",
                parent.display()
            ),
        ));
    }
    Ok(parent)
}

fn validate_config_target(path: &Path) -> io::Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Runtime config is not a regular file: {}", path.display()),
            ))
        }
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Read a bounded regular file without following a final symlink. Only
/// `NotFound` is treated as empty; malformed UTF-8, non-regular files, files
/// larger than 8 MiB, and hard I/O errors abort the transaction.
pub(crate) fn read_to_string_or_empty(path: &Path) -> io::Result<String> {
    let Some(metadata) = validate_config_target(path)? else {
        return Ok(String::new());
    };
    if metadata.len() > MAX_CONFIG_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Runtime config exceeds {MAX_CONFIG_FILE_BYTES} bytes: {}",
                path.display()
            ),
        ));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.len() > MAX_CONFIG_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Runtime config changed during open: {}", path.display()),
        ));
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.take(MAX_CONFIG_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Runtime config exceeds {MAX_CONFIG_FILE_BYTES} bytes: {}",
                path.display()
            ),
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Runtime config is not valid UTF-8: {}", path.display()),
        )
    })
}
/// Atomic write via temp file + `rename` (mirrors [`save_config`]) so a crash
/// mid-write can't truncate `config.toml`. Preserves the dest mode on unix.
pub(crate) fn atomic_write_string(path: &Path, content: &str) -> io::Result<()> {
    if content.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Runtime config exceeds {MAX_CONFIG_FILE_BYTES} bytes: {}",
                path.display()
            ),
        ));
    }
    let parent = ensure_safe_config_parent(path)?;
    let target_metadata = validate_config_target(path)?;
    #[cfg(unix)]
    let prior_mode: Option<u32> = target_metadata.as_ref().map(|metadata| {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode()
    });
    #[cfg(not(unix))]
    let prior_mode: Option<u32> = {
        let _ = target_metadata;
        None
    };
    let suffix = {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("toml.tmp.{}.{}", std::process::id(), nanos)
    };
    let tmp = path.with_extension(suffix);
    let write_result = (|| -> io::Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(prior_mode.unwrap_or(0o600));
        }
        let mut file = options.open(&tmp)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Runtime config may contain API keys. A newly created file must
            // not inherit a permissive umask; existing explicit modes remain.
            let mode = prior_mode.unwrap_or(0o600);
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    let _ = prior_mode;
    if let Err(e) = replace_file_atomically(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = parent;
    Ok(())
}

#[cfg(not(windows))]
fn replace_file_atomically(staging: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(staging, destination)
}

#[cfg(windows)]
fn replace_file_atomically(staging: &Path, destination: &Path) -> io::Result<()> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::{
        MOVE_FILE_FLAGS, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    use windows::core::PCWSTR;

    let staging_wide = staging
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(once(0))
        .collect::<Vec<_>>();
    // SAFETY: both buffers are NUL-terminated and remain alive for the call.
    unsafe {
        MoveFileExW(
            PCWSTR(staging_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            MOVE_FILE_FLAGS(MOVEFILE_REPLACE_EXISTING.0 | MOVEFILE_WRITE_THROUGH.0),
        )
    }
    .map_err(io::Error::other)
}
/// Merge `[toolset.ask_user_question]` into the root table. `[toolset]` is
/// deliberately NOT merged wholesale — it carries runtime-only structs
/// (`web_search` sampler etc.) whose serialized defaults must never land in
/// the user file — so only this settings-writable sub-table round-trips.
fn merge_ask_user_question_section(
    table: &mut TomlMap<String, TomlValue>,
    ask: &crate::tools::config::AskUserQuestionToolConfig,
) {
    if ask.timeout_enabled.is_none() && ask.timeout_secs.is_none() {
        return;
    }
    let toolset = table
        .entry("toolset".to_string())
        .or_insert_with(|| TomlValue::Table(TomlMap::new()));
    if !matches!(toolset, TomlValue::Table(_)) {
        *toolset = TomlValue::Table(TomlMap::new());
    }
    if let TomlValue::Table(toolset_table) = toolset {
        merge_section(toolset_table, "ask_user_question", ask);
    }
}
/// Merge serialized fields of `value` into `table[key]`, preserving any
/// existing keys not present in the serialized output. This prevents
/// unmodeled fields (e.g. pager-written `show_timestamps`, `auto_dark_theme`)
/// from being silently dropped when `save_config` round-trips the struct.
/// Deep-merge `incoming` into `existing`: nested tables recurse; scalars replace.
fn merge_toml_tables(
    existing: &mut TomlMap<String, TomlValue>,
    incoming: TomlMap<String, TomlValue>,
) {
    for (field_key, field_val) in incoming {
        match (existing.get_mut(&field_key), field_val) {
            (Some(TomlValue::Table(dst)), TomlValue::Table(src)) => {
                merge_toml_tables(dst, src);
            }
            (_, v) => {
                existing.insert(field_key, v);
            }
        }
    }
}
fn merge_section<T: serde::Serialize>(
    table: &mut TomlMap<String, TomlValue>,
    key: &str,
    value: &T,
) {
    match TomlValue::try_from(value) {
        Ok(TomlValue::Table(new_fields)) if !new_fields.is_empty() => {
            let section = table
                .entry(key.to_string())
                .or_insert_with(|| TomlValue::Table(TomlMap::new()));
            if let TomlValue::Table(existing) = section {
                merge_toml_tables(existing, new_fields);
            } else {
                *section = TomlValue::Table(new_fields);
            }
        }
        Ok(TomlValue::Table(_)) => {}
        Ok(_) | Err(_) => {
            table.remove(key);
        }
    }
}
/// Update settings with a read-modify-write, preserving unrelated fields.
pub async fn update_config<F>(f: F) -> Result<()>
where
    F: FnOnce(&mut Config),
{
    let _guard = SAVE_LOCK.lock().await;
    let path = user_config_path();
    update_config_toml_at(&path, |root| {
        let mut cfg = load_config_from_toml(root);
        f(&mut cfg);
        merge_config_into_root(root, &cfg)
    })
}
#[cfg(test)]
#[path = "persist_tests.rs"]
mod tests;
