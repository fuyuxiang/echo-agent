//! EchoAgent runtime paths and one-time legacy data migration.
//!
//! EchoAgent owns `~/.echo-agent`. Startup imports data from installations
//! created before the namespace migration, then configures the embedded
//! runtime to use the EchoAgent directory exclusively.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const HOME_ENV: &str = "ECHO_AGENT_HOME";
// Construct retired names from fragments so they remain a compatibility-only
// implementation detail and cannot be mistaken for current public settings.
const LEGACY_HOME_ENV: &str = concat!("G", "ROK_HOME");
const LEGACY_AUTH_ENV: &str = concat!("G", "ROK_AUTH");
const LEGACY_AUTH_PATH_ENV: &str = concat!("G", "ROK_AUTH_PATH");
const SECURE_PROVIDER_URLS_ENV: &str = "ECHO_AGENT_ENFORCE_SECURE_PROVIDER_URLS";
const HOME_DIR_NAME: &str = ".echo-agent";
const LEGACY_HOME_DIR_NAME: &str = concat!(".g", "rok");
const MIGRATION_MARKER: &str = ".legacy-data-migrated";
const LEGACY_AUTH_FILE: &str = "auth.json";

/// Parse startup options without converting filesystem arguments to UTF-8.
/// Other desktop/platform arguments are left for their existing handlers.
pub(crate) fn parse_data_home_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Option<PathBuf>, String> {
    let mut args = args.into_iter();
    let mut home = None;
    while let Some(arg) = args.next() {
        if arg.as_encoded_bytes().starts_with(b"--data-home=") {
            return Err("请使用 --data-home <绝对路径> 指定数据目录".into());
        }
        if arg != "--data-home" {
            continue;
        }
        if home.is_some() {
            return Err("--data-home 不能重复指定".into());
        }
        let value = args.next().ok_or("--data-home 缺少数据目录")?;
        if value.as_encoded_bytes().contains(&0) {
            return Err("数据目录不能包含空字符".into());
        }
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err("--data-home 必须使用非空的绝对路径".into());
        }
        reject_legacy_workbuddy_path(&path)?;
        home = Some(path);
    }
    Ok(home)
}

pub(crate) fn apply_startup_data_home() -> Result<(), String> {
    if let Some(home) = parse_data_home_args(std::env::args_os().skip(1))? {
        // Called before logging, Runtime initialization or background threads.
        std::env::set_var(HOME_ENV, home);
    }
    let review = std::env::var("ECHO_DESKTOP_REVIEW").as_deref() == Ok("1")
        || std::env::var("ECHO_DESKTOP_VALIDATION").as_deref() == Ok("1");
    if review {
        // Review mode never inherits an inline token, arbitrary config file,
        // auth command, or telemetry destination from its launching shell.
        for (name, _) in std::env::vars_os() {
            let key = name.to_string_lossy();
            if key.starts_with("ECHO_AGENT_AUTH")
                || key.starts_with("ECHO_AGENT_CONFIG")
                || key.starts_with("OTEL_")
                || key.starts_with("GROK_AUTH")
            {
                std::env::remove_var(name);
            }
        }
        std::env::set_var("ECHO_AGENT_REVIEW_ISOLATED", "1");
        std::env::set_var("ECHO_AGENT_TELEMETRY_ENABLED", "false");
        std::env::set_var("ECHO_AGENT_TELEMETRY_TRACE_UPLOAD", "false");
    }
    Ok(())
}

/// Reject data roots owned by the retired WorkBuddy integration.
///
/// String normalization is intentional: a Windows path can reach a Unix test
/// build through persisted WebView storage, and vice versa.
pub(crate) fn reject_legacy_workbuddy_path(path: &Path) -> Result<(), String> {
    let contains_legacy_component =
        path.to_string_lossy()
            .replace('\\', "/")
            .split('/')
            .any(|component| {
                component.eq_ignore_ascii_case(".workbuddy")
                    || component.eq_ignore_ascii_case("workbuddy")
            });

    if contains_legacy_component {
        Err("已阻止读取 WorkBuddy 数据目录，请选择 EchoAgent 数据目录".into())
    } else {
        Ok(())
    }
}

/// Resolve EchoAgent's user data directory.
pub fn echo_agent_home_dir() -> PathBuf {
    std::env::var_os(HOME_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(HOME_DIR_NAME)
        })
}

pub(crate) fn redirected_data_home() -> Option<PathBuf> {
    let root = echo_agent_home_dir();
    let default = dirs::home_dir().map(|home| home.join(HOME_DIR_NAME));
    is_redirected_data_home(&root, default.as_deref()).then_some(root)
}

pub(crate) fn is_redirected_data_home(root: &Path, default: Option<&Path>) -> bool {
    let identity = path_identity(root);
    let default = default.map(path_identity);
    default.as_deref() != Some(identity.as_path())
}

/// Automatic discovery of retired catalogs belongs to the default profile.
/// A redirected profile may still select a source explicitly via the catalog
/// settings/environment, but must not silently import another profile's data.
pub(crate) fn legacy_catalog_user_home() -> Option<PathBuf> {
    legacy_catalog_home_for(&echo_agent_home_dir(), dirs::home_dir().as_deref())
}

fn legacy_catalog_home_for(data_home: &Path, user_home: Option<&Path>) -> Option<PathBuf> {
    user_home
        .filter(|home| path_identity(data_home) == path_identity(&home.join(HOME_DIR_NAME)))
        .map(Path::to_path_buf)
}

fn path_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Desktop sidecars follow an explicitly redirected profile while existing
/// ordinary installations retain their platform application-data paths.
pub(crate) fn desktop_data_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    if let Some(home) = redirected_data_home() {
        return Ok(home.join("desktop"));
    }
    app.path().app_data_dir().map_err(|error| error.to_string())
}

/// Default user-visible workspace for sessions created without an explicit
/// folder selection. Keeping this in one dedicated directory avoids treating
/// the user's entire home directory as an implicit filesystem capability.
pub(crate) fn default_workspace_dir() -> Result<PathBuf, String> {
    if let Some(workspace) =
        redirected_default_workspace(&echo_agent_home_dir(), dirs::home_dir().as_deref())
    {
        fs::create_dir_all(&workspace)
            .map_err(|error| format!("创建隔离工作区 {} 失败：{error}", workspace.display()))?;
        return workspace
            .canonicalize()
            .map_err(|error| format!("无法解析隔离工作区 {}：{error}", workspace.display()));
    }
    let mut bases = Vec::new();
    if let Some(documents) = dirs::document_dir() {
        push_unique_path(&mut bases, documents);
    }
    if let Some(home) = dirs::home_dir() {
        push_unique_path(&mut bases, home);
    }
    if bases.is_empty() {
        return Err("无法确定默认工作区位置".into());
    }

    let mut last_error = None;
    for base in bases {
        let workspace = default_workspace_path(&base);
        if let Err(error) = fs::create_dir_all(&workspace) {
            last_error = Some(format!(
                "创建默认工作区 {} 失败：{error}",
                workspace.display()
            ));
            continue;
        }
        match workspace.canonicalize() {
            Ok(canonical) => return Ok(canonical),
            Err(error) => {
                last_error = Some(format!(
                    "无法解析默认工作区 {}：{error}",
                    workspace.display()
                ));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "无法创建默认工作区".into()))
}

fn redirected_default_workspace(data_home: &Path, user_home: Option<&Path>) -> Option<PathBuf> {
    // initialize_runtime_home also sets ECHO_AGENT_HOME for ordinary launches.
    // Compare the resolved root rather than treating mere env presence as an
    // override. Redirected launches must not create a real Documents workspace.
    let default_home = user_home.map(|home| path_identity(&home.join(HOME_DIR_NAME)));
    (default_home.as_deref() != Some(path_identity(data_home).as_path()))
        .then(|| data_home.join("workspaces").join("default"))
}

fn default_workspace_path(base: &Path) -> PathBuf {
    base.join("EchoAgent")
}

/// Built-in skills follow the active data home during legacy migration.
fn builtin_skills_root(data_home: &Path) -> PathBuf {
    data_home.join("resources").join("builtin-skills")
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

fn legacy_home_dir() -> Option<PathBuf> {
    let explicit = std::env::var_os(LEGACY_HOME_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    explicit.or_else(|| legacy_catalog_user_home().map(|home| home.join(LEGACY_HOME_DIR_NAME)))
}

/// Configure the embedded engine to use EchoAgent's home and import legacy
/// data once. Existing EchoAgent files always win; migration never overwrites
/// them and leaves the legacy directory untouched for rollback.
pub fn initialize_runtime_home() -> Result<PathBuf, String> {
    let target = echo_agent_home_dir();
    let legacy = legacy_home_dir();

    // Set the runtime home even if migration later reports an I/O error, so
    // every new write remains inside EchoAgent's current data directory.
    std::env::set_var(HOME_ENV, &target);
    // Tell the embedded sampler to reject insecure provider URLs loaded from
    // legacy or manually edited Runtime config. This is set by native startup,
    // not by the WebView, so request-time enforcement cannot be bypassed by
    // invoking a different settings command.
    std::env::set_var(SECURE_PROVIDER_URLS_ENV, "1");
    // Desktop extensions are local. Never auto-register a remote catalog,
    // even if a parent CLI process enabled it for its own use.
    std::env::set_var("ECHO_AGENT_OFFICIAL_MARKETPLACE_AUTO_REGISTER", "false");
    // EchoAgent uses only per-provider credentials. Remove retired account
    // overrides before background threads start so the inert runtime adapter
    // cannot inherit an obsolete account token from the parent process.
    std::env::remove_var(LEGACY_AUTH_ENV);
    std::env::remove_var(LEGACY_AUTH_PATH_ENV);

    fs::create_dir_all(&target)
        .map_err(|e| format!("create EchoAgent home {}: {e}", target.display()))?;
    harden_private_dir(&target)?;

    let marker = target.join(MIGRATION_MARKER);
    if let Some(legacy) =
        legacy.filter(|legacy| legacy != &target && legacy.is_dir() && !marker.exists())
    {
        merge_missing(&legacy, &target).map_err(|e| format!("migrate legacy agent data: {e}"))?;
        fs::write(
            &marker,
            b"Legacy agent data imported without overwriting existing files.\n",
        )
        .map_err(|e| format!("write migration marker {}: {e}", marker.display()))?;
    }

    // Preserve locally bundled skills without importing retired browse catalogs.
    migrate_legacy_builtin_skills(&target);

    // Migration may have copied a world-readable legacy file. Harden it on
    // every startup so existing installations are repaired without requiring
    // the user to save provider settings again.
    let config = target.join("config.toml");
    if config.exists() {
        harden_private_file(&config)?;
    }

    Ok(target)
}

fn migrate_legacy_builtin_skills(data_home: &Path) {
    let Some(user_home) = legacy_catalog_user_home() else {
        return;
    };
    let old_default_home = user_home.join(HOME_DIR_NAME);

    let builtin_source = old_default_home.join("resources").join("builtin-skills");
    if builtin_source.is_dir() {
        if let Err(error) = migrate_catalog_dir(&builtin_source, &builtin_skills_root(data_home)) {
            eprintln!("migrate legacy built-in skills: {error}");
        }
    }
}

/// Copy a legacy catalog through a sibling staging directory, then rename it
/// into place. The rename makes an interrupted import distinguishable from a
/// complete target and ensures an existing target always wins.
fn migrate_catalog_dir(source: &Path, target: &Path) -> io::Result<bool> {
    if source == target || target.exists() || !source.is_dir() {
        return Ok(false);
    }
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "catalog target has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid catalog target"))?;
    let staging = parent.join(format!(".{name}.migrating"));
    fs::create_dir_all(&staging)?;
    merge_missing(source, &staging)?;
    fs::rename(staging, target)?;
    Ok(true)
}

/// Write a secret-bearing file atomically with owner-only permissions on Unix.
/// This is shared by config.toml and persisted endpoint credentials.
pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("private file has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("private file has no name: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    harden_private_dir(parent)?;
    // A unique sibling prevents concurrent writers from truncating each
    // other's staging file. `create_new` also closes the residual collision
    // window if a UUID is ever repeated.
    let tmp = parent.join(format!(
        ".{}.{}.{}.tmp",
        file_name.to_string_lossy(),
        std::process::id(),
        uuid::Uuid::now_v7().simple()
    ));

    let result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&tmp)
            .map_err(|e| format!("create private staging file {}: {e}", tmp.display()))?;
        file.write_all(contents)
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync {}: {e}", tmp.display()))?;
        harden_private_file(&tmp)?;
        drop(file);
        replace_file_atomically(&tmp, path)
    })();
    if let Err(error) = result {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    harden_private_file(path)?;
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn replace_file_atomically(staging: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(staging, destination).map_err(|error| {
        format!(
            "atomically replace {} from {}: {error}",
            destination.display(),
            staging.display()
        )
    })
}

#[cfg(windows)]
pub(crate) fn replace_file_atomically(staging: &Path, destination: &Path) -> Result<(), String> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_LOCK_VIOLATION, ERROR_SHARING_VIOLATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileAttributesW, MoveFileExW, SetFileAttributesW, FILE_ATTRIBUTE_NORMAL,
        FILE_ATTRIBUTE_TEMPORARY, INVALID_FILE_ATTRIBUTES, MOVEFILE_REPLACE_EXISTING,
        MOVEFILE_WRITE_THROUGH,
    };

    // Unlike std::fs, raw Win32 calls do not normalize mixed separators or
    // add the extended-length prefix. Canonicalize the existing staging file
    // and destination parent, without following/requiring the destination.
    let staging = staging.canonicalize().map_err(|e| e.to_string())?;
    let destination = destination
        .parent()
        .ok_or("atomic destination has no parent")?
        .canonicalize()
        .map_err(|e| e.to_string())?
        .join(
            destination
                .file_name()
                .ok_or("atomic destination has no name")?,
        );
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
    // Preserve NamedTempFile::persist semantics: a published backup must no
    // longer carry the temporary-file caching hint.
    let attributes = unsafe { GetFileAttributesW(staging_wide.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(io::Error::last_os_error().to_string());
    }
    if attributes & FILE_ATTRIBUTE_TEMPORARY != 0 {
        let attributes = attributes & !FILE_ATTRIBUTE_TEMPORARY;
        let attributes = if attributes == 0 {
            FILE_ATTRIBUTE_NORMAL
        } else {
            attributes
        };
        if unsafe { SetFileAttributesW(staging_wide.as_ptr(), attributes) } == 0 {
            return Err(io::Error::last_os_error().to_string());
        }
    }
    // Concurrent renames and short-lived scanner/reader handles can deny
    // replacement on Windows. Retry only those errors, for at most ~1 second;
    // keep the destination intact and reuse the complete staging file.
    let mut delays = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512].into_iter();
    loop {
        // SAFETY: both buffers are NUL-terminated and remain alive for the call.
        let replaced = unsafe {
            MoveFileExW(
                staging_wide.as_ptr(),
                destination_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if replaced != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error().map(|code| code as u32),
            Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        ) {
            if let Some(delay) = delays.next() {
                std::thread::sleep(std::time::Duration::from_millis(delay));
                continue;
            }
        }
        return Err(format!(
            "atomically replace {} from {}: {error}",
            destination.display(),
            staging.display(),
        ));
    }
}

pub(crate) fn harden_private_file(path: &Path) -> Result<(), String> {
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod 0600 {}: {e}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn harden_private_dir(path: &Path) -> Result<(), String> {
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("chmod 0700 {}: {e}", path.display()))?;
    }
    Ok(())
}

fn merge_missing(source: &Path, target: &Path) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if entry.file_name() == LEGACY_AUTH_FILE {
            continue;
        }
        let file_type = entry.file_type()?;
        let destination = target.join(entry.file_name());

        if file_type.is_dir() {
            match fs::symlink_metadata(&destination) {
                Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                    continue;
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&destination)?;
                }
                Err(error) => return Err(error),
            }
            merge_missing(&entry.path(), &destination)?;
        } else if file_type.is_file() {
            copy_file_if_missing(&entry.path(), &destination)?;
        }
        // Symlinks are intentionally skipped: migration must not traverse or
        // recreate links that may point outside the legacy data directory.
    }
    Ok(())
}

fn copy_file_if_missing(source: &Path, destination: &Path) -> io::Result<()> {
    let mut input = fs::File::open(source)?;
    let mut output = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error),
    };

    if let Err(error) = io::copy(&mut input, &mut output) {
        drop(output);
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    if let Ok(permissions) = fs::metadata(source).map(|metadata| metadata.permissions()) {
        let _ = fs::set_permissions(destination, permissions);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        builtin_skills_root, default_workspace_path, legacy_catalog_home_for, merge_missing,
        migrate_catalog_dir, parse_data_home_args, redirected_default_workspace,
        reject_legacy_workbuddy_path, write_private_file,
    };
    use std::fs;
    use std::io::Write;
    use std::path::Path;

    #[test]
    fn data_home_cli_preserves_chinese_and_spaces_as_one_argument() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("中文 数据目录");
        assert_eq!(
            parse_data_home_args([
                "--background".into(),
                "--data-home".into(),
                home.clone().into_os_string(),
            ])
            .unwrap(),
            Some(home)
        );
        assert_eq!(parse_data_home_args(["--background".into()]).unwrap(), None);
    }

    #[test]
    fn data_home_cli_rejects_missing_relative_duplicate_or_malformed_paths() {
        let directory = tempfile::tempdir().unwrap();
        let absolute = directory.path().as_os_str().to_os_string();
        for args in [
            vec!["--data-home".into()],
            vec!["--data-home".into(), "relative/path".into()],
            vec!["--data-home".into(), "".into()],
            vec![
                "--data-home".into(),
                absolute.clone(),
                "--data-home".into(),
                absolute.clone(),
            ],
            vec!["--data-home=".into()],
            vec!["--data-home".into(), "\0".into()],
        ] {
            assert!(parse_data_home_args(args).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn data_home_cli_does_not_panic_on_non_utf8_filesystem_arguments() {
        use std::os::unix::ffi::OsStringExt;
        let value = std::ffi::OsString::from_vec(b"/tmp/echo-\xff".to_vec());
        assert_eq!(
            parse_data_home_args(["--data-home".into(), value.clone()]).unwrap(),
            Some(value.into())
        );
    }

    #[test]
    fn rejects_legacy_workbuddy_paths_on_both_platforms() {
        assert!(reject_legacy_workbuddy_path(Path::new(
            "/Users/demo/.workbuddy/connectors-marketplace"
        ))
        .is_err());
        assert!(reject_legacy_workbuddy_path(Path::new(
            r"C:\Users\demo\WorkBuddy\connectors-marketplace"
        ))
        .is_err());
    }

    #[test]
    fn allows_similarly_named_non_legacy_paths() {
        assert!(reject_legacy_workbuddy_path(Path::new(
            "/Users/demo/workbuddy-notes/connectors-marketplace"
        ))
        .is_ok());
        assert!(reject_legacy_workbuddy_path(Path::new(
            "/Users/demo/.echo-agent/connectors-marketplace"
        ))
        .is_ok());
    }

    #[test]
    fn builtin_skills_follow_the_active_data_home() {
        let root = Path::new("/data/echo-agent-home");
        assert_eq!(
            builtin_skills_root(root),
            Path::new("/data/echo-agent-home/resources/builtin-skills")
        );
    }

    #[test]
    fn default_workspace_is_a_dedicated_child_not_the_user_home() {
        let user_documents = Path::new("/Users/demo/Documents");
        let workspace = default_workspace_path(user_documents);
        assert_eq!(workspace, user_documents.join("EchoAgent"));
        assert_ne!(workspace, user_documents);
    }

    #[test]
    fn redirected_home_keeps_default_workspace_inside_the_data_root() {
        let home = std::path::Path::new("/users/reviewer");
        let isolated = std::path::Path::new("/tmp/echo-review");
        assert_eq!(
            redirected_default_workspace(isolated, Some(home)),
            Some(isolated.join("workspaces/default"))
        );
        assert_eq!(
            redirected_default_workspace(&home.join(".echo-agent"), Some(home)),
            None
        );
    }

    #[test]
    fn redirected_profile_does_not_discover_default_profile_legacy_catalogs() {
        let home = Path::new("/users/reviewer");
        assert_eq!(
            legacy_catalog_home_for(Path::new("/tmp/echo-review"), Some(home)),
            None
        );
        assert_eq!(
            legacy_catalog_home_for(&home.join(".echo-agent"), Some(home)),
            Some(home.to_path_buf())
        );
        assert_eq!(
            legacy_catalog_home_for(Path::new("/tmp/echo-review"), None),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_default_profile_keeps_legacy_migration_and_workspace_compatibility() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("user");
        let actual_data = temp.path().join("data");
        fs::create_dir_all(&user_home).unwrap();
        fs::create_dir_all(&actual_data).unwrap();
        std::os::unix::fs::symlink(&actual_data, user_home.join(".echo-agent")).unwrap();
        let canonical = actual_data.canonicalize().unwrap();
        assert_eq!(
            legacy_catalog_home_for(&canonical, Some(&user_home)),
            Some(user_home.clone())
        );
        assert_eq!(
            redirected_default_workspace(&canonical, Some(&user_home)),
            None
        );
        assert_eq!(
            legacy_catalog_home_for(&temp.path().join("review"), Some(&user_home)),
            None
        );
    }

    #[test]
    fn migration_copies_nested_files_without_overwriting() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("memory/nested")).unwrap();
        fs::create_dir_all(target.path().join("memory")).unwrap();
        fs::write(source.path().join("config.toml"), "legacy").unwrap();
        fs::write(source.path().join("auth.json"), "legacy credentials").unwrap();
        fs::write(source.path().join("memory/MEMORY.md"), "legacy memory").unwrap();
        fs::write(source.path().join("memory/nested/fact.md"), "fact").unwrap();
        fs::write(target.path().join("memory/MEMORY.md"), "current memory").unwrap();

        merge_missing(source.path(), target.path()).unwrap();

        assert_eq!(
            fs::read_to_string(target.path().join("config.toml")).unwrap(),
            "legacy"
        );
        assert!(!target.path().join("auth.json").exists());
        assert_eq!(
            fs::read_to_string(target.path().join("memory/MEMORY.md")).unwrap(),
            "current memory"
        );
        assert_eq!(
            fs::read_to_string(target.path().join("memory/nested/fact.md")).unwrap(),
            "fact"
        );
    }

    #[test]
    fn migration_skips_incompatible_existing_entries() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join("memory")).unwrap();
        fs::write(source.path().join("memory/fact.md"), "fact").unwrap();
        fs::write(target.path().join("memory"), "keep this file").unwrap();

        merge_missing(source.path(), target.path()).unwrap();

        assert_eq!(
            fs::read_to_string(target.path().join("memory")).unwrap(),
            "keep this file"
        );
    }

    #[test]
    fn catalog_migration_is_non_destructive_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("legacy-experts");
        let target = temp.path().join("data-home").join("experts-marketplace");
        fs::create_dir_all(source.join("_meta")).unwrap();
        fs::write(source.join("_meta/_expert_center.json"), "catalog").unwrap();

        assert!(migrate_catalog_dir(&source, &target).unwrap());
        assert_eq!(
            fs::read_to_string(target.join("_meta/_expert_center.json")).unwrap(),
            "catalog"
        );
        assert!(
            source.exists(),
            "legacy source must remain available for rollback"
        );

        fs::write(source.join("newer.txt"), "must not merge later").unwrap();
        assert!(!migrate_catalog_dir(&source, &target).unwrap());
        assert!(!target.join("newer.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn private_write_creates_and_repairs_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;

        let target = tempfile::tempdir().unwrap();
        let path = target.path().join("secret.json");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        write_private_file(&path, b"new").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn private_writes_replace_files_under_long_paths_with_mixed_separators() {
        let target = tempfile::tempdir().unwrap();
        let path = target
            .path()
            .join("profile".repeat(20))
            .join("nested".repeat(20))
            .join("sessions/saved/summary.json");
        assert!(path.as_os_str().len() > 260);
        write_private_file(&path, b"old").unwrap();
        write_private_file(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let mut staging = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
        staging.write_all(b"backup").unwrap();
        staging.as_file().sync_all().unwrap();
        super::replace_file_atomically(staging.path(), &path).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"backup");
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_TEMPORARY;
            assert_eq!(
                fs::metadata(&path).unwrap().file_attributes() & FILE_ATTRIBUTE_TEMPORARY,
                0
            );
        }
    }

    #[test]
    fn concurrent_private_writes_never_publish_partial_content() {
        let target = tempfile::tempdir().unwrap();
        let path = target.path().join("config.toml");
        let payloads = (0..12)
            .map(|index| format!("writer-{index}:{}", "x".repeat(32 * 1024)))
            .collect::<Vec<_>>();
        let start = std::sync::Arc::new(std::sync::Barrier::new(payloads.len()));
        let handles = payloads
            .iter()
            .cloned()
            .map(|payload| {
                let path = path.clone();
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    write_private_file(&path, payload.as_bytes()).unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        let written = fs::read_to_string(&path).unwrap();
        assert!(payloads.contains(&written));
        assert_eq!(
            fs::read_dir(target.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .count(),
            0
        );
    }

    #[cfg(windows)]
    fn hold_against_replacement(path: &Path) -> fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)
            .unwrap()
    }

    #[cfg(windows)]
    #[test]
    fn atomic_replace_retries_until_a_temporary_windows_handle_is_closed() {
        let target = tempfile::tempdir().unwrap();
        let path = target.path().join("config.toml");
        fs::write(&path, b"old").unwrap();
        let staging = target.path().join("staging.tmp");
        fs::write(&staging, b"complete replacement").unwrap();
        let held = hold_against_replacement(&path);
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(held);
        });
        let result = super::replace_file_atomically(&staging, &path);
        release.join().unwrap();
        result.unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"complete replacement");
        assert!(!staging.exists());
    }

    #[cfg(windows)]
    #[test]
    fn private_write_keeps_old_content_and_cleans_staging_when_windows_handle_stays_open() {
        let target = tempfile::tempdir().unwrap();
        let path = target.path().join("config.toml");
        fs::write(&path, b"old").unwrap();
        let held = hold_against_replacement(&path);
        assert!(write_private_file(&path, b"new").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read_dir(target.path()).unwrap().count(), 1);
        drop(held);
        write_private_file(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }
}
