//! Install plugins from a marketplace source into the managed plugin storage.
//!
//! Routes through the existing `InstallRegistry` + `git_install` pipeline.
//! Adds marketplace provenance to the installed repo record.

use std::collections::HashMap;
use std::path::Path;

use echo_agent_core::plugins::git_install::{self, InstallSource};
use echo_agent_core::plugins::install_registry::{
    InstallError, InstallKind, InstallRegistry, InstalledRepo, MarketplaceProvenance, RepoPlugin,
};
use echo_agent_core::plugins::manifest::{ManifestLoadResult, load_manifest, name_from_dirname};

use crate::types::{MarketplaceEntry, MarketplaceRelativePath};

/// Result of a marketplace install attempt.
#[derive(Debug)]
pub enum MarketplaceInstallResult {
    /// Plugin installed successfully.
    Installed { repo_key: String },
    /// Plugin is already installed (from this or another source).
    AlreadyInstalled { repo_key: String },
}

#[derive(Debug, Clone)]
pub struct MarketplaceUpdateResult {
    pub repo_key: String,
    pub old_version: Option<String>,
    pub new_version: Option<String>,
    pub changed: bool,
    pub reinstalled: bool,
}

/// Install a plugin from a marketplace source.
///
/// Copies the plugin directory into the managed install storage and records
/// marketplace provenance in the install registry.
pub fn install_from_marketplace(
    marketplace_root: &Path,
    plugin_relative_path: &str,
    mut provenance: MarketplaceProvenance,
    registry: &mut InstallRegistry,
) -> Result<MarketplaceInstallResult, InstallError> {
    capture_source_identity(&mut provenance);
    // Use the resolved plugin directory as the source path.
    // Each plugin gets its own repo key and symlink.
    let plugin_relative_path =
        MarketplaceRelativePath::parse(plugin_relative_path).map_err(|e| {
            InstallError::InstallFailed {
                detail: format!("invalid marketplace plugin path: {e}"),
            }
        })?;
    let plugin_dir = plugin_relative_path
        .join_under(marketplace_root)
        .map_err(|e| InstallError::InstallFailed {
            detail: format!("invalid marketplace plugin path: {e}"),
        })?;
    let source = InstallSource::Local {
        path: plugin_dir,
        subdir: None,
    };

    // Local copy from the synced source checkout: the pin gate governs remote
    // fetches only (see install_from_remote_url's security doc).
    match git_install::install_from_source(&source, registry, false) {
        Ok(result) => {
            let repo_key = result.repo_key.clone();
            let mut repo = git_install::build_installed_repo(&result, &source);
            repo.marketplace = Some(provenance);
            registry.insert(repo_key.clone(), repo);
            registry.save()?;
            Ok(MarketplaceInstallResult::Installed { repo_key })
        }
        Err(InstallError::AlreadyInstalled { key }) => {
            // Remove old installation and retry so re-install works after uninstall.
            let old_path = registry.install_dir().join(&key);
            if old_path.exists() {
                let _ = std::fs::remove_dir_all(&old_path);
            }
            // Also remove the symlink if it's one.
            let _ = std::fs::remove_file(&old_path);
            registry.remove(&key);
            registry.save()?;
            // Retry — registry no longer has the key.
            match git_install::install_from_source(&source, registry, false) {
                Ok(result) => {
                    let repo_key = result.repo_key.clone();
                    let mut repo = git_install::build_installed_repo(&result, &source);
                    repo.marketplace = Some(provenance);
                    registry.insert(repo_key.clone(), repo);
                    registry.save()?;
                    Ok(MarketplaceInstallResult::Installed { repo_key })
                }
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

/// Install a plugin from a remote git URL (superpowers-style marketplace).
///
/// Clones the plugin repo and installs it via the standard git install
/// pipeline; pins to `git_sha` if set, otherwise uses `git_ref` or HEAD.
///
/// # Security
///
/// Marketplace plugins are **not cryptographically signed**. A remote install
/// without `git_sha` tracks a mutable ref (branch/tag/HEAD) and can be
/// substituted by anyone who can push that ref. Prefer publishing `sha` in
/// `plugin-index.json` and installing with that pin.
///
/// `require_sha` (from [`crate::config::load_require_sha`]) fails such installs
/// closed. It covers every path that fetches plugin code from a remote git URL
/// (marketplace `remote_url` entries, direct installs, git updates). It does
/// NOT cover plugins vendored inside a marketplace source itself — those come
/// from the synced source checkout, whose branch is not yet pinnable.
pub fn install_from_remote_url(
    url: &str,
    git_ref: Option<&str>,
    git_sha: Option<&str>,
    subdir: Option<&str>,
    plugin_name: &str,
    mut provenance: MarketplaceProvenance,
    registry: &mut InstallRegistry,
    require_sha: bool,
) -> Result<MarketplaceInstallResult, InstallError> {
    capture_source_identity(&mut provenance);
    let subdir = subdir
        .map(|s| {
            MarketplaceRelativePath::parse(s)
                .map(|p| p.as_str().to_string())
                .map_err(|e| InstallError::InstallFailed {
                    detail: format!("invalid marketplace plugin subdir: {e}"),
                })
        })
        .transpose()?;
    let (url, git_ref, git_sha) = git_install::clone_operands(url, git_ref, git_sha)?;
    // No-fetch short-circuit before the pin gate: re-install of an already-present
    // plugin must not refuse just because the catalog entry is unpinned.
    if let Some((existing_key, _)) = find_installed_marketplace_plugin(
        registry,
        &provenance.source_url_or_path,
        &provenance.plugin_subdir,
    ) {
        return Ok(MarketplaceInstallResult::AlreadyInstalled {
            repo_key: existing_key,
        });
    }
    let source = InstallSource::Git {
        url: url.to_string(),
        git_ref: git_ref.map(str::to_owned),
        git_sha: git_sha.map(str::to_owned),
        subdir,
    };

    // Single pin gate lives in install_from_source; pass plugin_name so refusals
    // name the catalog entry rather than the bare URL.
    match git_install::install_from_source_with_label(
        &source,
        registry,
        require_sha,
        Some(plugin_name),
    ) {
        Ok(result) => {
            let repo_key = result.repo_key.clone();
            let mut repo = git_install::build_installed_repo(&result, &source);
            repo.marketplace = Some(provenance);
            registry.insert(repo_key.clone(), repo);
            registry.save()?;
            Ok(MarketplaceInstallResult::Installed { repo_key })
        }
        Err(InstallError::AlreadyInstalled { key }) => {
            let old_path = registry.install_dir().join(&key);
            if old_path.exists() {
                let _ = std::fs::remove_dir_all(&old_path);
            }
            let _ = std::fs::remove_file(&old_path);
            registry.remove(&key);
            registry.save()?;
            match git_install::install_from_source_with_label(
                &source,
                registry,
                require_sha,
                Some(plugin_name),
            ) {
                Ok(result) => {
                    let repo_key = result.repo_key.clone();
                    let mut repo = git_install::build_installed_repo(&result, &source);
                    repo.marketplace = Some(provenance);
                    registry.insert(repo_key.clone(), repo);
                    registry.save()?;
                    Ok(MarketplaceInstallResult::Installed { repo_key })
                }
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

pub fn update_from_marketplace_entry_transactional(
    marketplace_root: &Path,
    entry: &MarketplaceEntry,
    mut provenance: MarketplaceProvenance,
    registry: &mut InstallRegistry,
    require_sha: bool,
) -> Result<MarketplaceUpdateResult, InstallError> {
    capture_source_identity(&mut provenance);
    let plugin_relative_path =
        MarketplaceRelativePath::parse(&entry.relative_path).map_err(|e| {
            InstallError::InstallFailed {
                detail: format!("invalid marketplace plugin path: {e}"),
            }
        })?;
    let provenance_path =
        MarketplaceRelativePath::parse(&provenance.plugin_subdir).map_err(|e| {
            InstallError::InstallFailed {
                detail: format!("invalid marketplace plugin path: {e}"),
            }
        })?;
    if plugin_relative_path.as_str() != provenance_path.as_str() {
        return Err(InstallError::InstallFailed {
            detail: format!(
                "marketplace entry path mismatch: requested {}, found {}",
                provenance_path.as_str(),
                plugin_relative_path.as_str()
            ),
        });
    }
    provenance.plugin_subdir = plugin_relative_path.as_str().to_string();

    let remote_subdir = entry
        .remote_subdir
        .as_deref()
        .map(|s| {
            MarketplaceRelativePath::parse(s)
                .map(|p| p.as_str().to_string())
                .map_err(|e| InstallError::InstallFailed {
                    detail: format!("invalid marketplace plugin subdir: {e}"),
                })
        })
        .transpose()?;

    let (repo_key, old_repo) = registry
        .list()
        .into_iter()
        .find_map(|(key, repo)| {
            repo.marketplace.as_ref().and_then(|mp| {
                if mp.source_url_or_path == provenance.source_url_or_path
                    && mp.plugin_subdir == provenance.plugin_subdir
                {
                    Some((key.to_string(), repo.clone()))
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| InstallError::PluginNotFound {
            name: provenance.plugin_subdir.clone(),
        })?;

    let remote_source = entry
        .remote_url
        .as_deref()
        .map(|url| {
            // Catalog pins published as `ref` still need hoisting for the verified clone path.
            let (git_ref, git_sha) = git_install::hoist_pin_slots(
                entry.remote_ref.as_deref(),
                entry.remote_sha.as_deref(),
            );
            let source = git_install::clone_operands(url, git_ref, git_sha)?;
            git_install::ensure_pinned(require_sha, source.2, &entry.name, source.0)?;
            Ok::<_, InstallError>(source)
        })
        .transpose()?;

    let install_dir = registry.install_dir().to_path_buf();
    std::fs::create_dir_all(&install_dir).map_err(|e| InstallError::Io {
        path: install_dir.clone(),
        source: e,
    })?;

    let nonce = staging_nonce();
    let staging_path = install_dir.join(format!(".staging-{repo_key}-{nonce}"));
    let backup_path = install_dir.join(format!(".backup-{repo_key}-{nonce}"));
    let final_path = old_repo.path.clone();

    remove_path_if_exists(&staging_path)?;
    remove_path_if_exists(&backup_path)?;

    let stage_result = if let Some((url, git_ref, git_sha)) = remote_source {
        clone_repo_to_path(url, git_ref, git_sha, &staging_path)
    } else {
        let source_path = plugin_relative_path
            .join_under(marketplace_root)
            .map_err(|e| InstallError::InstallFailed {
                detail: format!("invalid marketplace plugin path: {e}"),
            })?;
        if !source_path.is_dir() {
            Err(InstallError::InstallFailed {
                detail: format!("plugin directory not found: {}", source_path.display()),
            })
        } else {
            copy_dir_recursive(&source_path, &staging_path).map_err(|e| InstallError::Io {
                path: staging_path.clone(),
                source: e,
            })
        }
    };
    if let Err(e) = stage_result {
        let _ = remove_path_if_exists(&staging_path);
        return Err(e);
    }

    let plugins = match discover_plugins_in_dir(&staging_path, remote_subdir.as_deref()) {
        Ok(plugins) if !plugins.is_empty() => plugins,
        Ok(_) => {
            let _ = remove_path_if_exists(&staging_path);
            return Err(InstallError::InstallFailed {
                detail: "no plugins found in the marketplace entry (no plugin.json or convention components)"
                    .to_string(),
            });
        }
        Err(e) => {
            let _ = remove_path_if_exists(&staging_path);
            return Err(e);
        }
    };

    let old_version = first_plugin_version(&old_repo.plugins);
    let new_plugins = plugins_to_repo_plugins(&plugins);
    let new_version = first_plugin_version(&new_plugins);
    let changed = old_version != new_version;
    let updated_at = chrono::Utc::now().to_rfc3339();
    let kind = if let Some((url, git_ref, git_sha)) = remote_source {
        remote_install_kind(
            url,
            git_ref,
            git_sha,
            read_head_commit(&staging_path).unwrap_or_default(),
            remote_subdir.clone(),
        )
    } else {
        let source_path = plugin_relative_path
            .join_under(marketplace_root)
            .map_err(|e| InstallError::InstallFailed {
                detail: format!("invalid marketplace plugin path: {e}"),
            })?;
        InstallKind::Local {
            source_path,
            subdir: None,
        }
    };
    let new_repo = InstalledRepo {
        kind,
        installed_at: old_repo.installed_at.clone(),
        updated_at,
        path: final_path.clone(),
        plugins: new_plugins,
        marketplace: Some(provenance),
    };

    let original_registry = registry.clone();
    if !final_path.exists() {
        let _ = remove_path_if_exists(&staging_path);
        return Err(InstallError::InstallFailed {
            detail: format!(
                "installed plugin directory not found: {}",
                final_path.display()
            ),
        });
    }
    if let Err(e) = std::fs::rename(&final_path, &backup_path) {
        let _ = remove_path_if_exists(&staging_path);
        return Err(InstallError::Io {
            path: final_path,
            source: e,
        });
    }
    if let Err(e) = std::fs::rename(&staging_path, &final_path) {
        let restore_result = std::fs::rename(&backup_path, &final_path);
        let _ = remove_path_if_exists(&staging_path);
        *registry = original_registry;
        if let Err(restore_error) = restore_result {
            return Err(InstallError::InstallFailed {
                detail: format!(
                    "failed to install staged marketplace update: {e}; restore also failed: {restore_error}"
                ),
            });
        }
        return Err(InstallError::Io {
            path: staging_path,
            source: e,
        });
    }

    registry.insert(repo_key.clone(), new_repo);
    if let Err(save_error) = registry.save() {
        // The directory swap already succeeded (final_path holds the new plugin).
        // Roll the filesystem back to the previous install so it stays consistent
        // with the on-disk registry, which still holds the old record. Only revert
        // the registry record once the files are actually restored — otherwise we
        // would leave the new files on disk while the registry claims the old
        // version, which is the exact inconsistency we are guarding against.
        let fs_rolled_back = remove_path_if_exists(&final_path).is_ok()
            && std::fs::rename(&backup_path, &final_path).is_ok();
        if !fs_rolled_back {
            return Err(InstallError::InstallFailed {
                detail: format!(
                    "registry save failed after installing the update ({save_error}); \
                     the installed plugin at {} could not be rolled back and no longer \
                     matches the registry — rerun the update or reinstall (backup at {})",
                    final_path.display(),
                    backup_path.display()
                ),
            });
        }
        *registry = original_registry;
        if let Err(rollback_error) = registry.save() {
            return Err(InstallError::InstallFailed {
                detail: format!(
                    "registry save failed ({save_error}); filesystem rolled back but \
                     registry rollback failed ({rollback_error}) — rerun the update or reinstall"
                ),
            });
        }
        let _ = remove_path_if_exists(&backup_path);
        return Err(save_error);
    }

    let _ = remove_path_if_exists(&backup_path);
    Ok(MarketplaceUpdateResult {
        repo_key,
        old_version,
        new_version,
        changed,
        reinstalled: true,
    })
}

/// Check if a plugin from a specific marketplace source is already installed.
///
/// Matches by `source_url_or_path + plugin_subdir` (stable identity).
pub fn find_installed_marketplace_plugin(
    registry: &InstallRegistry,
    source_url_or_path: &str,
    plugin_subdir: &str,
) -> Option<(String, String)> {
    for (key, repo) in registry.list() {
        if let Some(ref mp) = repo.marketplace
            && mp.canonical_source_identity.as_ref().map_or_else(
                || mp.source_url_or_path == source_url_or_path,
                |identity| {
                    echo_agent_core::plugins::marketplace_scope::stored_source_identity(identity)
                        == echo_agent_core::plugins::marketplace_scope::source_identity(
                            source_url_or_path,
                        )
                },
            )
            && mp.plugin_subdir == plugin_subdir
        {
            let version = repo
                .plugins
                .values()
                .next()
                .and_then(|p| p.version.clone())
                .unwrap_or_default();
            return Some((key.to_string(), version));
        }
    }
    None
}

fn capture_source_identity(provenance: &mut MarketplaceProvenance) {
    if provenance.canonical_source_identity.is_none() {
        provenance.canonical_source_identity = Some(
            echo_agent_core::plugins::marketplace_scope::source_identity(
                &provenance.source_url_or_path,
            ),
        );
    }
}

fn staging_nonce() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

fn remove_path_if_exists(path: &Path) -> Result<(), InstallError> {
    if path.is_symlink() || path.is_file() {
        std::fs::remove_file(path).map_err(|e| InstallError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    } else if path.is_dir() {
        std::fs::remove_dir_all(path).map_err(|e| InstallError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    }
    Ok(())
}

fn remote_install_kind(
    url: &str,
    git_ref: Option<&str>,
    git_sha: Option<&str>,
    commit: String,
    subdir: Option<String>,
) -> InstallKind {
    InstallKind::Git {
        url: url.to_owned(),
        git_ref: git_sha.or(git_ref).map(str::to_owned),
        commit,
        subdir,
    }
}

fn clone_repo_to_path(
    url: &str,
    git_ref: Option<&str>,
    git_sha: Option<&str>,
    target: &Path,
) -> Result<(), InstallError> {
    let (url, git_ref, git_sha) = git_install::clone_operands(url, git_ref, git_sha)?;
    if let Some(sha) = git_sha {
        return clone_repo_at_sha(url, sha, target);
    }

    // Same auth/LFS/SSH suppression as marketplace cache clones.
    let mut cmd = echo_agent_tty_utils::git_command();
    cmd.arg("clone").arg("--depth").arg("1");
    if let Some(r) = git_ref {
        cmd.arg("--branch").arg(r);
    }
    cmd.arg("--").arg(url).arg(target);
    let output = cmd.output().map_err(|e| InstallError::InstallFailed {
        detail: format!("failed to run git clone: {e}"),
    })?;
    if !output.status.success() {
        let _ = remove_path_if_exists(target);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(InstallError::InstallFailed {
            detail: format!(
                "git clone failed (exit {}):\n{stderr}",
                output.status.code().unwrap_or(-1)
            ),
        });
    }
    Ok(())
}

fn clone_repo_at_sha(url: &str, sha: &str, target: &Path) -> Result<(), InstallError> {
    let url = git_install::validate_git_url(url)
        .map_err(|detail| InstallError::InstallFailed { detail })?;
    let sha = git_install::validate_git_sha(sha)
        .map_err(|detail| InstallError::InstallFailed { detail })?;
    std::fs::create_dir_all(target).map_err(|e| InstallError::Io {
        path: target.to_path_buf(),
        source: e,
    })?;
    let wrap_fail = |detail: String| {
        let _ = remove_path_if_exists(target);
        InstallError::InstallFailed { detail }
    };
    run_git_in(target, &["init", "--quiet"]).map_err(wrap_fail)?;
    run_git_in(target, &git_install::remote_add_args(url)).map_err(wrap_fail)?;
    run_git_in(target, &git_install::fetch_sha_args(sha))
        .map_err(|d| wrap_fail(format!("fetch-by-sha failed: {d}")))?;
    run_git_in(target, &["checkout", "--quiet", "FETCH_HEAD"]).map_err(wrap_fail)?;
    let head = read_head_commit(target).ok_or_else(|| {
        let _ = remove_path_if_exists(target);
        InstallError::InstallFailed {
            detail: "could not read HEAD commit after SHA-pinned clone".into(),
        }
    })?;
    if !head.eq_ignore_ascii_case(sha) {
        let _ = remove_path_if_exists(target);
        return Err(InstallError::ShaMismatch {
            expected: sha.to_string(),
            actual: head,
        });
    }
    Ok(())
}

fn run_git_in(cwd: &Path, args: &[&str]) -> Result<(), String> {
    run_git_in_capture(cwd, args).map(|_| ())
}

fn run_git_in_capture(cwd: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let mut cmd = echo_agent_tty_utils::git_command();
    cmd.args(args).current_dir(cwd);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run git {}: {e}", args.first().unwrap_or(&"")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git {} failed (exit {}):\n{stderr}",
            args.first().unwrap_or(&""),
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(output)
}

fn read_head_commit(repo_path: &Path) -> Option<String> {
    run_git_in_capture(repo_path, &["rev-parse", "HEAD"])
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

struct StagedPlugin {
    name: String,
    subdir: Option<String>,
    version: Option<String>,
}

fn discover_plugins_in_dir(
    root: &Path,
    subdir: Option<&str>,
) -> Result<Vec<StagedPlugin>, InstallError> {
    let scan_root = match subdir {
        Some(s) => {
            let candidate = Path::new(s);
            if candidate.is_absolute()
                || candidate.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                return Err(InstallError::InstallFailed {
                    detail: format!("subdirectory '{s}' escapes the source root"),
                });
            }
            let sub_path = root.join(s);
            if !sub_path.is_dir() {
                return Err(InstallError::InstallFailed {
                    detail: format!("subdirectory '{s}' not found in source"),
                });
            }
            sub_path
        }
        None => root.to_path_buf(),
    };

    if let Some(plugin) = try_load_plugin(&scan_root, subdir) {
        return Ok(vec![plugin]);
    }

    let mut plugins = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&scan_root) {
        for entry in entries.flatten() {
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                let entry_path = entry.path();
                let entry_name = entry.file_name().to_str().unwrap_or_default().to_string();
                let sub = match subdir {
                    Some(s) => Some(format!("{s}/{entry_name}")),
                    None => Some(entry_name),
                };
                if let Some(plugin) = try_load_plugin(&entry_path, sub.as_deref()) {
                    plugins.push(plugin);
                }
            }
        }
    }

    Ok(plugins)
}

fn try_load_plugin(dir: &Path, subdir: Option<&str>) -> Option<StagedPlugin> {
    match load_manifest(dir) {
        Ok(ManifestLoadResult::Found(manifest)) => {
            return Some(StagedPlugin {
                name: manifest.name.clone(),
                subdir: subdir.map(|s| s.to_string()),
                version: manifest.version.clone(),
            });
        }
        Ok(ManifestLoadResult::NotFound) => {}
        Err(_) => {}
    }

    let has_skills = dir.join("skills").is_dir();
    let has_agents = dir.join("agents").is_dir();
    let has_mcp = dir.join(".mcp.json").is_file();
    let has_hooks = dir.join("hooks").join("hooks.json").is_file();

    if has_skills || has_agents || has_mcp || has_hooks {
        let name = name_from_dirname(dir)?;
        Some(StagedPlugin {
            name,
            subdir: subdir.map(|s| s.to_string()),
            version: None,
        })
    } else {
        None
    }
}

fn plugins_to_repo_plugins(plugins: &[StagedPlugin]) -> HashMap<String, RepoPlugin> {
    plugins
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                RepoPlugin {
                    subdir: p.subdir.clone(),
                    version: p.version.clone(),
                },
            )
        })
        .collect()
}

fn first_plugin_version(plugins: &HashMap<String, RepoPlugin>) -> Option<String> {
    plugins.values().next().and_then(|p| p.version.clone())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::sync::{Mutex, OnceLock};

    static TEST_HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn transactional_sha_git_args_terminate_options_before_operands() {
        assert_eq!(
            git_install::remote_add_args("repo"),
            ["remote", "add", "--", "origin", "repo"]
        );
        assert_eq!(
            git_install::fetch_sha_args("0123456789abcdef0123456789abcdef01234567"),
            [
                "fetch",
                "--depth",
                "1",
                "--",
                "origin",
                "0123456789abcdef0123456789abcdef01234567",
            ]
        );
    }

    #[test]
    fn transactional_sha_clone_rejects_before_target_creation() {
        for bad in ["deadbeef", "--upload-pack=cmd"] {
            let root = tempfile::tempdir().unwrap();
            let target = root.path().join("staging");
            assert!(matches!(
                clone_repo_to_path("file:///unused", None, Some(bad), &target),
                Err(InstallError::InstallFailed { .. })
            ));
            assert!(!target.exists());
        }
    }

    #[test]
    fn require_sha_rejects_unpinned_remote_install() {
        with_test_registry(|registry| {
            let err = install_from_remote_url(
                "https://example.com/plugin.git",
                Some("main"),
                None, // no sha
                None,
                "plugins/demo",
                MarketplaceProvenance {
                    source_url_or_path: "https://example.com/market.git".into(),
                    canonical_source_identity: None,
                    source_display_name: "test".into(),
                    plugin_subdir: "plugins/demo".into(),
                },
                registry,
                true, // require_sha
            )
            .unwrap_err();
            assert!(
                matches!(err, InstallError::UnpinnedRemoteRefused { .. }),
                "expected the typed refusal, got: {err}"
            );

            let err = install_from_remote_url(
                "https://example.com/plugin.git",
                None,
                Some("main"),
                None,
                "plugins/demo",
                MarketplaceProvenance {
                    source_url_or_path: "https://example.com/market.git".into(),
                    canonical_source_identity: None,
                    source_display_name: "test".into(),
                    plugin_subdir: "plugins/demo".into(),
                },
                registry,
                true,
            )
            .unwrap_err();
            match err {
                InstallError::InstallFailed { detail } => assert!(
                    detail.contains("40 or 64 hexadecimal"),
                    "expected full-SHA validation detail, got: {detail}"
                ),
                other => panic!("expected InstallFailed for malformed SHA, got: {other}"),
            }
        });
    }

    #[test]
    fn already_installed_remote_still_rejects_malformed_operands() {
        with_test_registry(|registry| {
            let marketplace = tempfile::tempdir().unwrap();
            write_plugin(marketplace.path(), "demo", "1.0.0", "old");
            install_test_plugin(registry, marketplace.path(), "demo");
            let provenance = provenance(marketplace.path(), "plugins/demo");
            let registry_len = registry.list().len();
            let installed_path = registry.list().into_iter().next().unwrap().1.path.clone();

            for (url, git_ref, git_sha) in [
                ("--upload-pack=cmd", Some("main"), None),
                (
                    "https://example.com/plugin.git",
                    Some("--upload-pack=cmd"),
                    None,
                ),
                ("https://example.com/plugin.git", None, Some("deadbeef")),
            ] {
                let err = install_from_remote_url(
                    url,
                    git_ref,
                    git_sha,
                    None,
                    "plugins/demo",
                    provenance.clone(),
                    registry,
                    false,
                )
                .unwrap_err();
                assert!(matches!(err, InstallError::InstallFailed { .. }));
                assert_eq!(registry.list().len(), registry_len);
                assert!(installed_path.exists());
            }
        });
    }

    #[test]
    fn require_sha_already_installed_skips_pin_gate() {
        if !git_available() {
            eprintln!("skipping: `git` binary not available in test sandbox");
            return;
        }
        with_test_registry(|registry| {
            let repo = tempfile::tempdir().unwrap();
            run_git(repo.path(), &["init", "--initial-branch=main", "--quiet"]);
            write_root_plugin(repo.path(), "acme", "1.0.0");
            run_git(repo.path(), &["add", "-A"]);
            run_git(repo.path(), &["commit", "-m", "v1", "--quiet"]);

            let url = format!("file://{}", repo.path().display());
            let provenance = MarketplaceProvenance {
                source_url_or_path: "https://example.com/marketplace.git".into(),
                canonical_source_identity: None,
                source_display_name: "Test".into(),
                plugin_subdir: "acme".into(),
            };

            // Unpinned first install (policy off) so the registry is populated.
            match install_from_remote_url(
                &url,
                Some("main"),
                None,
                None,
                "acme",
                provenance.clone(),
                registry,
                false,
            )
            .unwrap()
            {
                MarketplaceInstallResult::Installed { .. } => {}
                MarketplaceInstallResult::AlreadyInstalled { repo_key } => {
                    panic!("expected fresh Installed, got AlreadyInstalled {repo_key}")
                }
            }

            // No-fetch re-install under require_sha must not refuse unpinned catalog entries.
            match install_from_remote_url(
                &url,
                Some("main"),
                None,
                None,
                "acme",
                provenance,
                registry,
                true,
            )
            .unwrap()
            {
                MarketplaceInstallResult::AlreadyInstalled { .. } => {}
                MarketplaceInstallResult::Installed { repo_key } => {
                    panic!("expected AlreadyInstalled, got Installed {repo_key}")
                }
            }
        });
    }

    #[test]
    fn require_sha_rejects_unpinned_remote_update() {
        with_test_registry(|registry| {
            let marketplace = tempfile::tempdir().unwrap();
            write_plugin(marketplace.path(), "demo", "1.0.0", "old");
            let repo_key = install_test_plugin(registry, marketplace.path(), "demo");
            let mut entry = crate::scan_marketplace(marketplace.path())
                .entries
                .into_iter()
                .find(|p| p.relative_path == "plugins/demo")
                .unwrap();
            entry.remote_url = Some("https://example.com/plugin.git".into());
            entry.remote_sha = None;

            let err = update_from_marketplace_entry_transactional(
                marketplace.path(),
                &entry,
                provenance(marketplace.path(), "plugins/demo"),
                registry,
                true, // require_sha
            )
            .unwrap_err();
            assert!(
                matches!(err, InstallError::UnpinnedRemoteRefused { .. }),
                "expected the typed refusal, got: {err}"
            );
            assert!(
                registry.install_dir().join(&repo_key).exists(),
                "a refused update must leave the existing install in place"
            );
        });
    }

    fn with_test_registry<T>(f: impl FnOnce(&mut InstallRegistry) -> T) -> T {
        let _guard = TEST_LOCK.lock().unwrap();
        let home = TEST_HOME.get_or_init(|| tempfile::tempdir().unwrap());
        let install_dir = home.path().join("installed-plugins");
        let _ = std::fs::remove_dir_all(&install_dir);
        std::fs::create_dir_all(&install_dir).unwrap();
        // Build the registry against an explicit tempdir rather than going through
        // `InstallRegistry::load()`, which resolves the install dir via the
        // process-global `echo_agent_home()` `OnceLock` (first-write-wins). A parallel
        // test in this binary can cache the real `~/.echo-agent` before this runs,
        // which would leak the registry tests into the real home and make them
        // order-dependent and flaky.
        let mut registry = InstallRegistry::empty(install_dir);
        f(&mut registry)
    }

    fn write_plugin(marketplace: &Path, name: &str, version: &str, marker: &str) {
        let plugin_dir = marketplace.join("plugins").join(name);
        let manifest_dir = plugin_dir.join(".claude-plugin");
        std::fs::create_dir_all(&manifest_dir).unwrap();
        std::fs::write(
            manifest_dir.join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        )
        .unwrap();
        let skill_dir = plugin_dir.join("skills").join("demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Demo").unwrap();
        std::fs::write(plugin_dir.join("marker.txt"), marker).unwrap();
    }

    fn provenance(marketplace: &Path, plugin_subdir: &str) -> MarketplaceProvenance {
        MarketplaceProvenance {
            source_url_or_path: marketplace.display().to_string(),
            canonical_source_identity: None,
            source_display_name: "Test".into(),
            plugin_subdir: plugin_subdir.into(),
        }
    }

    fn install_test_plugin(
        registry: &mut InstallRegistry,
        marketplace: &Path,
        name: &str,
    ) -> String {
        let plugin_subdir = format!("plugins/{name}");
        match install_from_marketplace(
            marketplace,
            &plugin_subdir,
            provenance(marketplace, &plugin_subdir),
            registry,
        )
        .unwrap()
        {
            MarketplaceInstallResult::Installed { repo_key } => repo_key,
            MarketplaceInstallResult::AlreadyInstalled { repo_key } => repo_key,
        }
    }

    #[test]
    fn install_from_marketplace_rejects_traversal_path() {
        with_test_registry(|registry| {
            let marketplace = tempfile::tempdir().unwrap();
            let provenance = MarketplaceProvenance {
                source_url_or_path: marketplace.path().display().to_string(),
                canonical_source_identity: None,
                source_display_name: "Test".into(),
                plugin_subdir: "../escaped".into(),
            };
            let result =
                install_from_marketplace(marketplace.path(), "../escaped", provenance, registry);
            assert!(matches!(result, Err(InstallError::InstallFailed { .. })));
        });
    }

    #[test]
    fn find_installed_returns_none_for_empty_registry() {
        with_test_registry(|registry| {
            let result =
                find_installed_marketplace_plugin(registry, "https://example.com", "plugins/test");
            assert!(result.is_none());
        });
    }

    #[test]
    fn transactional_update_recopies_local_entry_and_updates_version() {
        with_test_registry(|registry| {
            let marketplace = tempfile::tempdir().unwrap();
            write_plugin(marketplace.path(), "demo", "1.0.0", "old");
            let repo_key = install_test_plugin(registry, marketplace.path(), "demo");
            write_plugin(marketplace.path(), "demo", "2.0.0", "new");
            let entry = crate::scan_marketplace(marketplace.path())
                .entries
                .into_iter()
                .find(|p| p.relative_path == "plugins/demo")
                .unwrap();

            let result = update_from_marketplace_entry_transactional(
                marketplace.path(),
                &entry,
                provenance(marketplace.path(), "plugins/demo"),
                registry,
                false, // require_sha off: pin policy has its own tests
            )
            .unwrap();

            assert_eq!(result.repo_key, repo_key);
            assert_eq!(result.old_version.as_deref(), Some("1.0.0"));
            assert_eq!(result.new_version.as_deref(), Some("2.0.0"));
            assert!(result.changed);
            assert!(result.reinstalled);
            assert_eq!(
                std::fs::read_to_string(registry.install_dir().join(&repo_key).join("marker.txt"))
                    .unwrap(),
                "new"
            );
        });
    }

    #[test]
    fn transactional_git_kind_uses_normalized_operands() {
        let sha = "a".repeat(40);
        let padded_sha = format!(" {sha} ");
        let (url, git_ref, git_sha) = git_install::clone_operands(
            " https://example.com/plugin.git ",
            Some(" v1.2.3 "),
            Some(&padded_sha),
        )
        .unwrap();
        let kind = remote_install_kind(url, git_ref, git_sha, sha.clone(), None);
        let repo = InstalledRepo {
            kind,
            installed_at: String::new(),
            updated_at: String::new(),
            path: Path::new("/unused").to_path_buf(),
            plugins: HashMap::new(),
            marketplace: None,
        };

        match &repo.kind {
            InstallKind::Git { url, git_ref, .. } => {
                assert_eq!(url, "https://example.com/plugin.git");
                assert_eq!(git_ref.as_deref(), Some(sha.as_str()));
            }
            InstallKind::Local { .. } => panic!("expected Git"),
        }
        assert!(matches!(
            git_install::update_repo("repo", &repo, true),
            Ok(git_install::UpdateStatus::Pinned { ref_name }) if ref_name == sha
        ));
    }

    #[test]
    fn transactional_update_preserves_installed_at_and_updates_updated_at() {
        with_test_registry(|registry| {
            let marketplace = tempfile::tempdir().unwrap();
            write_plugin(marketplace.path(), "demo", "1.0.0", "old");
            let repo_key = install_test_plugin(registry, marketplace.path(), "demo");
            let installed_at = "2026-01-01T00:00:00Z".to_string();
            let old_updated_at = "2026-01-01T00:00:00Z".to_string();
            let repo = registry.get_repo_mut(&repo_key).unwrap();
            repo.installed_at = installed_at.clone();
            repo.updated_at = old_updated_at.clone();
            registry.save().unwrap();

            write_plugin(marketplace.path(), "demo", "1.1.0", "new");
            let entry = crate::scan_marketplace(marketplace.path())
                .entries
                .into_iter()
                .find(|p| p.relative_path == "plugins/demo")
                .unwrap();
            update_from_marketplace_entry_transactional(
                marketplace.path(),
                &entry,
                provenance(marketplace.path(), "plugins/demo"),
                registry,
                false, // require_sha off: pin policy has its own tests
            )
            .unwrap();

            let repo = registry.get_repo(&repo_key).unwrap();
            assert_eq!(repo.installed_at, installed_at);
            assert_ne!(repo.updated_at, old_updated_at);
        });
    }

    #[test]
    fn transactional_update_save_failure_rolls_back_install_and_disk_registry() {
        with_test_registry(|registry| {
            let marketplace = tempfile::tempdir().unwrap();
            write_plugin(marketplace.path(), "demo", "1.0.0", "old");
            let repo_key = install_test_plugin(registry, marketplace.path(), "demo");
            let old_registry_content =
                std::fs::read_to_string(registry.install_dir().join("registry.json")).unwrap();
            write_plugin(marketplace.path(), "demo", "2.0.0", "new");
            let entry = crate::scan_marketplace(marketplace.path())
                .entries
                .into_iter()
                .find(|p| p.relative_path == "plugins/demo")
                .unwrap();

            unsafe { std::env::set_var("ECHO_AGENT_TEST_FAIL_REGISTRY_SAVE_AFTER_SERIALIZE", "1") };
            let result = update_from_marketplace_entry_transactional(
                marketplace.path(),
                &entry,
                provenance(marketplace.path(), "plugins/demo"),
                registry,
                false, // require_sha off: pin policy has its own tests
            );
            unsafe { std::env::remove_var("ECHO_AGENT_TEST_FAIL_REGISTRY_SAVE_AFTER_SERIALIZE") };

            assert!(result.is_err());
            assert_eq!(
                std::fs::read_to_string(registry.install_dir().join(&repo_key).join("marker.txt"))
                    .unwrap(),
                "old"
            );
            assert_eq!(
                std::fs::read_to_string(registry.install_dir().join("registry.json")).unwrap(),
                old_registry_content
            );
        });
    }

    #[test]
    fn transactional_update_failure_preserves_old_install_and_registry() {
        with_test_registry(|registry| {
            let marketplace = tempfile::tempdir().unwrap();
            write_plugin(marketplace.path(), "demo", "1.0.0", "old");
            let repo_key = install_test_plugin(registry, marketplace.path(), "demo");
            let plugin_dir = marketplace.path().join("plugins").join("demo");
            std::fs::remove_dir_all(&plugin_dir).unwrap();
            std::fs::create_dir_all(&plugin_dir).unwrap();
            let entry = MarketplaceEntry {
                name: "demo".into(),
                version: Some("2.0.0".into()),
                description: None,
                category: None,
                author: None,
                tags: Vec::new(),
                keywords: Vec::new(),
                domains: Vec::new(),
                homepage: None,
                relative_path: "plugins/demo".into(),
                skill_count: 0,
                has_hooks: false,
                has_agents: false,
                has_mcp: false,
                remote_url: None,
                remote_ref: None,
                remote_sha: None,
                remote_subdir: None,
                components: None,
            };

            let result = update_from_marketplace_entry_transactional(
                marketplace.path(),
                &entry,
                provenance(marketplace.path(), "plugins/demo"),
                registry,
                false, // require_sha off: pin policy has its own tests
            );

            assert!(matches!(result, Err(InstallError::InstallFailed { .. })));
            assert_eq!(
                std::fs::read_to_string(registry.install_dir().join(&repo_key).join("marker.txt"))
                    .unwrap(),
                "old"
            );
            let repo = registry.get_repo(&repo_key).unwrap();
            let version = repo
                .plugins
                .values()
                .next()
                .and_then(|p| p.version.as_deref());
            assert_eq!(version, Some("1.0.0"));
        });
    }

    #[test]
    fn install_from_remote_url_rejects_traversal_subdir() {
        with_test_registry(|registry| {
            let provenance = MarketplaceProvenance {
                source_url_or_path: "https://example.com/r.git".into(),
                canonical_source_identity: None,
                source_display_name: "Test".into(),
                plugin_subdir: "acme".into(),
            };
            let result = install_from_remote_url(
                "https://example.com/r.git",
                None,
                None,
                Some("../escape"),
                "acme",
                provenance,
                registry,
                false, // require_sha off: pin policy has its own tests
            );
            assert!(matches!(result, Err(InstallError::InstallFailed { .. })));
        });
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .stdin(std::process::Stdio::null());
        let output = cmd.output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_subdir_plugin(repo: &Path, subdir: &str, name: &str, version: &str) {
        let plugin_dir = repo.join(subdir);
        let manifest_dir = plugin_dir.join(".claude-plugin");
        std::fs::create_dir_all(&manifest_dir).unwrap();
        std::fs::write(
            manifest_dir.join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        )
        .unwrap();
        let skill_dir = plugin_dir.join("skills").join("demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Demo").unwrap();
    }

    fn installed_subdir(registry: &InstallRegistry, repo_key: &str) -> Option<String> {
        registry
            .get_repo(repo_key)
            .unwrap()
            .plugins
            .get("acme")
            .unwrap()
            .subdir
            .clone()
    }

    #[test]
    fn url_subdir_plugin_install_and_update_thread_remote_subdir() {
        if !git_available() {
            eprintln!("skipping: `git` binary not available in test sandbox");
            return;
        }
        with_test_registry(|registry| {
            let repo = tempfile::tempdir().unwrap();
            run_git(repo.path(), &["init", "--initial-branch=main", "--quiet"]);
            write_subdir_plugin(repo.path(), "plugins/acme", "acme", "1.0.0");
            run_git(repo.path(), &["add", "-A"]);
            run_git(repo.path(), &["commit", "-m", "v1", "--quiet"]);

            let url = format!("file://{}", repo.path().display());
            let provenance = MarketplaceProvenance {
                source_url_or_path: "https://example.com/marketplace.git".into(),
                canonical_source_identity: None,
                source_display_name: "Test".into(),
                plugin_subdir: "acme".into(),
            };

            let repo_key = match install_from_remote_url(
                &url,
                Some("main"),
                None,
                Some("plugins/acme"),
                "acme",
                provenance.clone(),
                registry,
                false, // require_sha off: pin policy has its own tests
            )
            .unwrap()
            {
                MarketplaceInstallResult::Installed { repo_key } => repo_key,
                MarketplaceInstallResult::AlreadyInstalled { repo_key } => repo_key,
            };
            assert_eq!(
                installed_subdir(registry, &repo_key).as_deref(),
                Some("plugins/acme")
            );

            write_subdir_plugin(repo.path(), "plugins/acme", "acme", "2.0.0");
            run_git(repo.path(), &["add", "-A"]);
            run_git(repo.path(), &["commit", "-m", "v2", "--quiet"]);

            let entry = MarketplaceEntry {
                name: "acme".into(),
                version: Some("2.0.0".into()),
                description: None,
                category: None,
                author: None,
                tags: Vec::new(),
                keywords: Vec::new(),
                domains: Vec::new(),
                homepage: None,
                relative_path: "acme".into(),
                skill_count: 0,
                has_hooks: false,
                has_agents: false,
                has_mcp: false,
                remote_url: Some(url.clone()),
                remote_ref: Some("main".into()),
                remote_sha: None,
                remote_subdir: Some("plugins/acme".into()),
                components: None,
            };

            let result = update_from_marketplace_entry_transactional(
                repo.path(),
                &entry,
                provenance,
                registry,
                false, // require_sha off: pin policy has its own tests
            )
            .unwrap();

            assert_eq!(result.repo_key, repo_key);
            assert_eq!(result.old_version.as_deref(), Some("1.0.0"));
            assert_eq!(result.new_version.as_deref(), Some("2.0.0"));
            assert!(result.changed);
            assert!(result.reinstalled);
            assert_eq!(
                installed_subdir(registry, &repo_key).as_deref(),
                Some("plugins/acme")
            );
        });
    }

    fn write_root_plugin(repo: &Path, name: &str, version: &str) {
        let manifest_dir = repo.join(".claude-plugin");
        std::fs::create_dir_all(&manifest_dir).unwrap();
        std::fs::write(
            manifest_dir.join("plugin.json"),
            format!(r#"{{"name":"{name}","version":"{version}"}}"#),
        )
        .unwrap();
        let skill_dir = repo.join("skills").join("demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Demo").unwrap();
    }

    #[test]
    fn transactional_update_rejects_traversal_remote_subdir() {
        with_test_registry(|registry| {
            let provenance = MarketplaceProvenance {
                source_url_or_path: "https://example.com/marketplace.git".into(),
                canonical_source_identity: None,
                source_display_name: "Test".into(),
                plugin_subdir: "acme".into(),
            };
            for bad in ["../escape", "/etc"] {
                let entry = MarketplaceEntry {
                    name: "acme".into(),
                    version: None,
                    description: None,
                    category: None,
                    author: None,
                    tags: Vec::new(),
                    keywords: Vec::new(),
                    domains: Vec::new(),
                    homepage: None,
                    relative_path: "acme".into(),
                    skill_count: 0,
                    has_hooks: false,
                    has_agents: false,
                    has_mcp: false,
                    remote_url: Some("https://example.com/r.git".into()),
                    remote_ref: Some("main".into()),
                    remote_sha: None,
                    remote_subdir: Some(bad.into()),
                    components: None,
                };
                let result = update_from_marketplace_entry_transactional(
                    Path::new("/tmp"),
                    &entry,
                    provenance.clone(),
                    registry,
                    false, // require_sha off: pin policy has its own tests
                );
                assert!(
                    matches!(result, Err(InstallError::InstallFailed { .. })),
                    "expected InstallFailed for subdir {bad:?}"
                );
            }
        });
    }

    #[test]
    fn discover_plugins_in_dir_rejects_escaping_subdir() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["../escape", "/etc"] {
            match discover_plugins_in_dir(dir.path(), Some(bad)) {
                Err(InstallError::InstallFailed { detail }) => {
                    assert!(detail.contains("escapes"), "got detail: {detail}");
                }
                Err(e) => panic!("expected escapes InstallFailed, got: {e:?}"),
                Ok(_) => panic!("expected escapes error for subdir {bad:?}, got Ok"),
            }
        }
    }

    #[test]
    fn install_from_remote_url_is_idempotent_on_provenance_across_repo_key_change() {
        if !git_available() {
            eprintln!("skipping: `git` binary not available in test sandbox");
            return;
        }
        with_test_registry(|registry| {
            let repo = tempfile::tempdir().unwrap();
            run_git(repo.path(), &["init", "--initial-branch=main", "--quiet"]);
            write_root_plugin(repo.path(), "acme", "1.0.0");
            run_git(repo.path(), &["add", "-A"]);
            run_git(repo.path(), &["commit", "-m", "v1", "--quiet"]);

            let url = format!("file://{}", repo.path().display());
            let provenance = MarketplaceProvenance {
                source_url_or_path: "https://example.com/marketplace.git".into(),
                canonical_source_identity: None,
                source_display_name: "Test".into(),
                plugin_subdir: "acme".into(),
            };

            let first_key = match install_from_remote_url(
                &url,
                Some("main"),
                None,
                None,
                "acme",
                provenance.clone(),
                registry,
                false, // require_sha off: pin policy has its own tests
            )
            .unwrap()
            {
                MarketplaceInstallResult::Installed { repo_key } => repo_key,
                MarketplaceInstallResult::AlreadyInstalled { repo_key } => {
                    panic!("expected fresh Installed, got AlreadyInstalled {repo_key}")
                }
            };

            match install_from_remote_url(
                &url,
                Some("main"),
                None,
                Some("plugins/acme"),
                "acme",
                provenance,
                registry,
                false, // require_sha off: pin policy has its own tests
            )
            .unwrap()
            {
                MarketplaceInstallResult::AlreadyInstalled { repo_key } => {
                    assert_eq!(repo_key, first_key);
                }
                MarketplaceInstallResult::Installed { repo_key } => {
                    panic!("expected AlreadyInstalled, got Installed {repo_key}")
                }
            }

            let rows = registry
                .list()
                .into_iter()
                .filter(|(_, repo)| {
                    repo.marketplace.as_ref().is_some_and(|mp| {
                        mp.source_url_or_path == "https://example.com/marketplace.git"
                            && mp.plugin_subdir == "acme"
                    })
                })
                .count();
            assert_eq!(rows, 1);
        });
    }
}
