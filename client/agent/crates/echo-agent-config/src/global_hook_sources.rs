//! EchoAgent-owned direct global hook paths shared by shell discovery and sandbox
//! write-deny: `$ECHO_AGENT_HOME/hooks`, `hooks-paths`, and absolute registry targets.
//! Relative registry lines, project hooks, and vendor compat are out of scope.

use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalHookSourceKind {
    /// `$ECHO_AGENT_HOME/hooks/` (discovered + protected).
    HookDirectory,
    /// `$ECHO_AGENT_HOME/hooks-paths` (protected; never loaded as hook JSON).
    RegistryFile,
    /// Absolute registry target (must exist before sandbox apply).
    ConfiguredSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalHookSource {
    pub path: PathBuf,
    pub kind: GlobalHookSourceKind,
}

impl GlobalHookSource {
    pub fn is_dir(&self) -> bool {
        match self.kind {
            GlobalHookSourceKind::HookDirectory => true,
            GlobalHookSourceKind::RegistryFile => false,
            GlobalHookSourceKind::ConfiguredSource => {
                if self.path.exists() {
                    self.path.is_dir()
                } else {
                    true
                }
            }
        }
    }

    /// False for the registry file itself (not hook JSON / not a hook dir).
    pub fn is_discovery_source(&self) -> bool {
        !matches!(self.kind, GlobalHookSourceKind::RegistryFile)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GlobalHookSourceError {
    #[error("cannot read hooks-paths {path}: {source}")]
    HooksPathsRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("symlinked ECHO_AGENT_HOME is not allowed under sandbox write-deny: {path}")]
    SymlinkedEchoAgentHome { path: PathBuf },
    #[error("hook source path contains a symlink component (retargetable): {path}")]
    SymlinkedSource { path: PathBuf },
    #[error("hook JSON file has hard-link aliases (st_nlink={nlink}): {path}")]
    HardLinkedHookFile { path: PathBuf, nlink: u64 },
    #[error("hook JSON path is not a regular file: {path}")]
    InvalidHookJsonFile { path: PathBuf },
    #[error("EchoAgent hooks directory has wrong type (expected real directory): {path}")]
    InvalidHooksDir { path: PathBuf },
    #[error("EchoAgent hooks-paths registry has wrong type (expected real file): {path}")]
    InvalidRegistryFile { path: PathBuf },
    #[error("cannot create EchoAgent hooks directory {path}: {source}")]
    CreateHooksDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot create EchoAgent hooks-paths registry {path}: {source}")]
    CreateRegistryFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Hard-fail omits all sources. Soft `configured_error` keeps fixed slots and
/// omits configured targets (sandbox must fail closed; discovery may log).
#[derive(Debug)]
pub struct ResolvedGlobalHookSources {
    pub sources: Vec<GlobalHookSource>,
    pub configured_error: Option<GlobalHookSourceError>,
}

impl ResolvedGlobalHookSources {
    pub fn is_incomplete(&self) -> bool {
        self.configured_error.is_some()
    }

    pub fn discovery_sources(&self) -> impl Iterator<Item = &GlobalHookSource> {
        self.sources.iter().filter(|s| s.is_discovery_source())
    }
}

/// macOS firmlinks are not attacker-retargetable; ignore in symlink scans.
fn is_system_firmlink(path: &Path) -> bool {
    matches!(
        path.to_str(),
        Some("/tmp")
            | Some("/var")
            | Some("/etc")
            | Some("/private/tmp")
            | Some("/private/var")
            | Some("/private/etc")
    )
}

/// True if any existing path component is a retargetable symlink (firmlinks skipped).
pub fn path_has_symlink_component(path: &Path) -> bool {
    let mut cur = PathBuf::new();
    for c in path.components() {
        cur.push(c.as_os_str());
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => {
                if is_system_firmlink(&cur) {
                    continue;
                }
                return true;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    false
}

fn push_unique(out: &mut Vec<GlobalHookSource>, source: GlobalHookSource) {
    if !out.iter().any(|s| s.path == source.path) {
        out.push(source);
    }
}

/// Existing non-symlink directory ancestors (parent-first), excluding `/`.
pub fn existing_ancestor_chain(path: &Path) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut cur = path.parent().map(Path::to_path_buf);
    while let Some(p) = cur {
        if p.as_os_str().is_empty() || p == Path::new("/") {
            break;
        }
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_dir() && !m.file_type().is_symlink() => {
                chain.push(p.clone());
            }
            Ok(_) => break,
            Err(_) => break,
        }
        cur = p.parent().map(Path::to_path_buf);
    }
    chain
}

/// Linux: `st_dev` differs from parent, or listed in mountinfo. Else false.
pub(crate) fn is_filesystem_mountpoint(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        if path == Path::new("/") {
            return true;
        }
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        if let Some(parent) = path.parent()
            && let Ok(pm) = std::fs::metadata(parent)
            && meta.dev() != pm.dev()
        {
            return true;
        }
        let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") else {
            return false;
        };
        let path_s = path.to_string_lossy();
        for line in mountinfo.lines() {
            let Some((left, _)) = line.split_once(" - ") else {
                continue;
            };
            let fields: Vec<&str> = left.split_whitespace().collect();
            if fields.len() < 5 {
                continue;
            }
            if fields[4] == path_s.as_ref() {
                return true;
            }
        }
        false
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

/// Ancestors to RW self-bind so rename is EBUSY: parent→root, skip already-
/// mounted nodes but keep pinning renameable ancestors above them (never `/`).
pub fn ancestors_to_pin_as_mountpoints(path: &Path) -> Vec<PathBuf> {
    ancestors_to_pin_as_mountpoints_with(path, is_filesystem_mountpoint)
}

pub(crate) fn ancestors_to_pin_as_mountpoints_with(
    path: &Path,
    is_mountpoint: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut cur = path.parent().map(Path::to_path_buf);
    while let Some(p) = cur {
        if p.as_os_str().is_empty() || p == Path::new("/") {
            break;
        }
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_dir() && !m.file_type().is_symlink() => {
                if is_mountpoint(&p) {
                    cur = p.parent().map(Path::to_path_buf);
                    continue;
                }
                chain.push(p.clone());
            }
            Ok(_) => break,
            Err(_) => break,
        }
        cur = p.parent().map(Path::to_path_buf);
    }
    chain
}

/// Unique ancestors, rootward-first (shallowest first).
pub fn unique_ancestors_rootward(sources: &[GlobalHookSource]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut all = Vec::new();
    for s in sources {
        for anc in ancestors_to_pin_as_mountpoints(&s.path) {
            if seen.insert(anc.clone()) {
                all.push(anc);
            }
        }
    }
    all.sort_by_key(|p| p.components().count());
    all
}

fn require_real_dir(path: &Path) -> Result<(), GlobalHookSourceError> {
    let meta = std::fs::symlink_metadata(path).map_err(|source| {
        GlobalHookSourceError::CreateHooksDir {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(GlobalHookSourceError::InvalidHooksDir {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn require_real_file(path: &Path) -> Result<(), GlobalHookSourceError> {
    let meta = std::fs::symlink_metadata(path).map_err(|source| {
        GlobalHookSourceError::CreateRegistryFile {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Err(GlobalHookSourceError::InvalidRegistryFile {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

/// Ensure real `$ECHO_AGENT_HOME/hooks` dir + `hooks-paths` file (create if missing).
/// Race-resistant create (`create_dir` / `create_new`+`O_NOFOLLOW`); never
/// truncates an existing registry; rejects symlinks/wrong types.
pub fn ensure_echo_agent_hook_slots(echo_agent_home: &Path) -> Result<(), GlobalHookSourceError> {
    if path_has_symlink_component(echo_agent_home) {
        return Err(GlobalHookSourceError::SymlinkedEchoAgentHome {
            path: echo_agent_home.to_path_buf(),
        });
    }

    match std::fs::create_dir(echo_agent_home) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(echo_agent_home).map_err(|source| {
                GlobalHookSourceError::CreateHooksDir {
                    path: echo_agent_home.to_path_buf(),
                    source,
                }
            })?;
        }
        Err(source) => {
            return Err(GlobalHookSourceError::CreateHooksDir {
                path: echo_agent_home.to_path_buf(),
                source,
            });
        }
    }
    if path_has_symlink_component(echo_agent_home) {
        return Err(GlobalHookSourceError::SymlinkedEchoAgentHome {
            path: echo_agent_home.to_path_buf(),
        });
    }
    let echo_agent_meta = std::fs::symlink_metadata(echo_agent_home).map_err(|source| {
        GlobalHookSourceError::CreateHooksDir {
            path: echo_agent_home.to_path_buf(),
            source,
        }
    })?;
    if echo_agent_meta.file_type().is_symlink() || !echo_agent_meta.file_type().is_dir() {
        return Err(GlobalHookSourceError::SymlinkedEchoAgentHome {
            path: echo_agent_home.to_path_buf(),
        });
    }

    let hooks = echo_agent_home.join("hooks");
    match std::fs::create_dir(&hooks) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            require_real_dir(&hooks)?;
        }
        Err(source) => {
            return Err(GlobalHookSourceError::CreateHooksDir {
                path: hooks,
                source,
            });
        }
    }
    require_real_dir(&hooks)?;
    if path_has_symlink_component(&hooks) {
        return Err(GlobalHookSourceError::SymlinkedSource { path: hooks });
    }

    let registry = echo_agent_home.join("hooks-paths");
    match open_registry_create_new(&registry) {
        Ok(f) => drop(f),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            require_real_file(&registry)?;
        }
        Err(source) => {
            return Err(GlobalHookSourceError::CreateRegistryFile {
                path: registry,
                source,
            });
        }
    }
    require_real_file(&registry)?;
    if path_has_symlink_component(&registry) {
        return Err(GlobalHookSourceError::SymlinkedSource { path: registry });
    }

    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const O_NOFOLLOW: i32 = 0x20000;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
const O_NOFOLLOW: i32 = 0x0100;

fn open_registry_create_new(path: &Path) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(O_NOFOLLOW)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }
}

/// Resolve EchoAgent-owned direct global hook sources (`reject_symlinks` for sandbox).
pub fn resolve_global_hook_sources(
    echo_agent_home: Option<&Path>,
    reject_symlinks: bool,
) -> Result<ResolvedGlobalHookSources, GlobalHookSourceError> {
    let mut out = Vec::new();
    let mut configured_error = None;

    if let Some(echoagent) = echo_agent_home {
        if reject_symlinks && path_has_symlink_component(echoagent) {
            return Err(GlobalHookSourceError::SymlinkedEchoAgentHome {
                path: echoagent.to_path_buf(),
            });
        }

        let hooks = echoagent.join("hooks");
        let hooks_paths = echoagent.join("hooks-paths");
        if reject_symlinks {
            for p in [&hooks, &hooks_paths] {
                if path_has_symlink_component(p) {
                    return Err(GlobalHookSourceError::SymlinkedSource { path: p.clone() });
                }
            }
        }

        push_unique(
            &mut out,
            GlobalHookSource {
                path: hooks,
                kind: GlobalHookSourceKind::HookDirectory,
            },
        );
        push_unique(
            &mut out,
            GlobalHookSource {
                path: hooks_paths.clone(),
                kind: GlobalHookSourceKind::RegistryFile,
            },
        );

        match std::fs::read_to_string(&hooks_paths) {
            Ok(content) => {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let path = PathBuf::from(trimmed);
                    if !path.is_absolute() {
                        continue;
                    }
                    if reject_symlinks && path_has_symlink_component(&path) {
                        return Err(GlobalHookSourceError::SymlinkedSource { path });
                    }
                    push_unique(
                        &mut out,
                        GlobalHookSource {
                            path,
                            kind: GlobalHookSourceKind::ConfiguredSource,
                        },
                    );
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                configured_error = Some(GlobalHookSourceError::HooksPathsRead {
                    path: hooks_paths,
                    source: e,
                });
            }
        }
    }

    Ok(ResolvedGlobalHookSources {
        sources: out,
        configured_error,
    })
}

pub fn missing_configured_sources(sources: &[GlobalHookSource]) -> Vec<PathBuf> {
    sources
        .iter()
        .filter(|s| s.kind == GlobalHookSourceKind::ConfiguredSource && !s.path.exists())
        .map(|s| s.path.clone())
        .collect()
}

/// Discovery filename filter: `*.json`, not hidden, not editor temps.
pub fn is_direct_hook_json_name(name: &str) -> bool {
    if !name.ends_with(".json") || name.len() <= 5 {
        return false;
    }
    if name.starts_with('.') {
        return false;
    }
    if name.ends_with('~') || name.ends_with(".swp") || name.ends_with(".swo") {
        return false;
    }
    true
}

/// Immediate discovery JSON files under `dir` (sorted, non-recursive).
pub fn list_direct_hook_json_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_direct_hook_json_name(name) {
            continue;
        }
        out.push(path);
    }
    out.sort();
    Ok(out)
}

/// Regular non-symlink file with `st_nlink == 1`.
#[cfg(unix)]
pub fn validate_direct_hook_json_file(path: &Path) -> Result<(), GlobalHookSourceError> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(|source| {
        GlobalHookSourceError::HooksPathsRead {
            path: path.to_path_buf(),
            source,
        }
    })?;
    if meta.file_type().is_symlink() {
        return Err(GlobalHookSourceError::SymlinkedSource {
            path: path.to_path_buf(),
        });
    }
    if !meta.file_type().is_file() {
        return Err(GlobalHookSourceError::InvalidHookJsonFile {
            path: path.to_path_buf(),
        });
    }
    if meta.nlink() != 1 {
        return Err(GlobalHookSourceError::HardLinkedHookFile {
            path: path.to_path_buf(),
            nlink: meta.nlink(),
        });
    }
    Ok(())
}

#[cfg(unix)]
pub fn validated_hook_json_files_for_sources(
    sources: &[GlobalHookSource],
) -> Result<Vec<PathBuf>, GlobalHookSourceError> {
    let mut files = Vec::new();
    for s in sources {
        if !s.is_dir() || !s.path.is_dir() {
            continue;
        }
        let listed = list_direct_hook_json_files(&s.path).map_err(|source| {
            GlobalHookSourceError::HooksPathsRead {
                path: s.path.clone(),
                source,
            }
        })?;
        for f in listed {
            validate_direct_hook_json_file(&f)?;
            files.push(f);
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

#[cfg(test)]
#[path = "global_hook_sources_tests.rs"]
mod tests;
