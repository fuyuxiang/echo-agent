//! Refresh of copied local plugin installs from their live source.
//!
//! A local install is a full directory copy under `installed-plugins/` (not a
//! live symlink), so agents/skills added to the live source after install do not
//! surface until the snapshot is re-copied. This module re-copies refreshable
//! local installs (under-home or trusted) at session spawn and `/plugins reload`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::git_install::{
    copy_dir_recursive, discover_plugins_in_dir, remove_repo_path, repo_plugin_map,
};
use super::install_registry::{InstallError, InstallKind, InstallRegistry, RepoPlugin};
use super::trust::TrustStore;

/// Orphaned tmp/backup siblings younger than this may belong to a concurrent live
/// refresh, so [`sweep_stale`] only reclaims entries older than this.
const STALE_SWEEP_AGE: Duration = Duration::from_secs(3600);

/// Counts from a [`refresh_local_installs`] pass, for logging and tests.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct RefreshSummary {
    pub refreshed: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Load the install registry, [`refresh_local_installs`], and persist it if a
/// snapshot changed.
///
/// Runs only at genuine session spawn (`force=false`, cheap skip-unchanged) and
/// explicit `/plugins reload` (`force=true`, always re-copies — the guaranteed
/// manual remedy). Refresh implies continuous re-consent for under-home / trusted
/// sources (install-time trust re-applies every spawn). Non-fatal on failure.
pub(crate) fn refresh_local_installs_from_disk(trust: &TrustStore, force: bool) -> RefreshSummary {
    let mut registry = InstallRegistry::load();
    let summary = refresh_local_installs(&mut registry, trust, force);
    if summary.refreshed > 0
        && let Err(e) = registry.save()
    {
        tracing::warn!(error = %e, "failed to save install registry after local plugin refresh");
    }
    summary
}

/// A local install snapshotted out of the registry so the refresh loop can mutate
/// the registry while iterating. `expected` is the recorded plugin set used to
/// guard against scope-changing rediscovery.
struct RefreshTarget {
    key: String,
    source_path: PathBuf,
    subdir: Option<String>,
    dest: PathBuf,
    expected: HashMap<String, RepoPlugin>,
}

/// Re-copy refreshable local installs from their live `source_path` into the
/// managed snapshot, rediscovering plugins so new components surface.
///
/// A source is refreshable when it is under the user's home (auto-trusted, same
/// rule as config-path plugins) or in the trust store; remote git installs are
/// handled by `update_repo`, not here. Unless `force`, snapshots already matching
/// the live source are skipped (a stat-walk, not a byte copy).
fn refresh_local_installs(
    registry: &mut InstallRegistry,
    trust: &TrustStore,
    force: bool,
) -> RefreshSummary {
    let mut summary = RefreshSummary::default();
    let targets: Vec<RefreshTarget> = registry
        .list()
        .into_iter()
        .filter_map(|(key, repo)| match &repo.kind {
            InstallKind::Local {
                source_path,
                subdir,
            } => Some(RefreshTarget {
                key: key.to_string(),
                source_path: source_path.clone(),
                subdir: subdir.clone(),
                dest: repo.path.clone(),
                expected: repo.plugins.clone(),
            }),
            _ => None,
        })
        .collect();

    for RefreshTarget {
        key,
        source_path,
        subdir,
        dest,
        expected,
    } in targets
    {
        let refreshable =
            TrustStore::is_config_path_auto_trusted(&source_path) || trust.is_trusted(&source_path);
        if !source_path.is_dir() || !refreshable {
            summary.skipped += 1;
            continue;
        }
        // Skip if the snapshot already matches the source (cheap stat-walk).
        // `force` (/plugins reload) bypasses the skip.
        if !force && snapshot_matches_source(&source_path, &dest) {
            summary.skipped += 1;
            continue;
        }

        match recopy_local_install(&source_path, subdir.as_deref(), &dest, &expected) {
            Ok(Some(plugins)) => {
                if let Some(repo) = registry.get_repo_mut(&key) {
                    repo.plugins = plugins;
                    repo.updated_at = chrono::Utc::now().to_rfc3339();
                }
                summary.refreshed += 1;
            }
            Ok(None) => {
                // Kept the snapshot: rediscovered plugin set/scope differs from
                // recorded (e.g. legacy install without a persisted `subdir`).
                tracing::debug!(
                    repo_key = %key,
                    "kept stale local plugin snapshot: rediscovered plugin set/scope differs from recorded"
                );
                summary.skipped += 1;
            }
            Err(e) => {
                tracing::warn!(repo_key = %key, error = %e, "local plugin refresh failed");
                summary.errors += 1;
            }
        }
    }

    summary
}

/// The set of `(relative_path, file_len)` for every non-symlink file under a
/// tree. Symlinks are skipped, matching [`copy_dir_recursive`]. Comparing two of
/// these detects add / remove / rename / size-change with no stored fingerprint
/// and no brittle src-vs-dst mtime compare (a copy does not preserve mtimes).
fn tree_file_set(root: &Path) -> Option<BTreeMap<PathBuf, u64>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, u64>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_file() {
                if let Ok(rel) = path.strip_prefix(base) {
                    out.insert(rel.to_path_buf(), meta.len());
                }
            } else if meta.is_dir() {
                walk(base, &path, out)?;
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out).ok()?;
    Some(out)
}

/// Whether the snapshot at `dest` matches the live `source` by `(relpath, len)`
/// set. Catches add/remove/rename/resize; misses only a same-path/same-len edit.
fn snapshot_matches_source(source: &Path, dest: &Path) -> bool {
    match (tree_file_set(source), tree_file_set(dest)) {
        (Some(src), Some(dst)) => src == dst,
        _ => false,
    }
}

/// Re-copy `source_path` into `dest`, returning the rediscovered plugins, or
/// `Ok(None)` to keep the existing snapshot unchanged.
///
/// Invariant: refresh only syncs file contents within the existing plugin set;
/// if rediscovery is empty or changes the `(name, subdir)` set, the snapshot is
/// kept as-is (protects legacy entries whose `subdir` wasn't persisted).
///
/// `subdir` scopes discovery as it did at install time; symlinks in the source
/// are skipped (see [`copy_dir_recursive`]). The swap is rename-aside (move live
/// snapshot to backup, promote tmp, drop backup) so `dest` is never absent during
/// a slow delete and a failed promote rolls back to the previous snapshot.
fn recopy_local_install(
    source_path: &Path,
    subdir: Option<&str>,
    dest: &Path,
    expected: &HashMap<String, RepoPlugin>,
) -> Result<Option<HashMap<String, RepoPlugin>>, InstallError> {
    let parent = dest.parent().ok_or_else(|| InstallError::InstallFailed {
        detail: format!("install path has no parent: {}", dest.display()),
    })?;
    let file_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("plugin");
    // Reclaim orphaned tmp/backup dirs left by a crash in a prior run.
    sweep_stale(parent, file_name);
    let tmp = parent.join(format!(".{file_name}.refresh-{}", std::process::id()));
    let backup = parent.join(format!(".{file_name}.backup-{}", std::process::id()));

    let _ = remove_repo_path(&tmp);
    copy_dir_recursive(source_path, &tmp).map_err(|e| {
        let _ = remove_repo_path(&tmp);
        InstallError::Io {
            path: tmp.clone(),
            source: e,
        }
    })?;

    let discovered = match discover_plugins_in_dir(&tmp, subdir) {
        Ok(plugins) => plugins,
        Err(e) => {
            let _ = remove_repo_path(&tmp);
            return Err(e);
        }
    };

    // Keep the snapshot unless the rediscovered (name, subdir) set is unchanged.
    let discovered_ids: BTreeSet<(&str, Option<&str>)> = discovered
        .iter()
        .map(|p| (p.name.as_str(), p.subdir.as_deref()))
        .collect();
    let expected_ids: BTreeSet<(&str, Option<&str>)> = expected
        .iter()
        .map(|(name, rp)| (name.as_str(), rp.subdir.as_deref()))
        .collect();
    if discovered.is_empty() || discovered_ids != expected_ids {
        let _ = remove_repo_path(&tmp);
        return Ok(None);
    }

    let _ = remove_repo_path(&backup);
    if dest.exists()
        && let Err(e) = std::fs::rename(dest, &backup)
    {
        let _ = remove_repo_path(&tmp);
        return Err(InstallError::Io {
            path: dest.to_path_buf(),
            source: e,
        });
    }
    if let Err(e) = promote_tmp_to_dest(&tmp, dest) {
        let _ = remove_repo_path(&tmp);
        // Promote failed: restore the prior tree so `dest` is never left missing
        // (rename, then copy fallback), unless a peer already repopulated `dest`.
        if dest.exists() {
            let _ = remove_repo_path(&backup);
        } else if std::fs::rename(&backup, dest).is_err() {
            match copy_dir_recursive(&backup, dest) {
                Ok(()) => {
                    let _ = remove_repo_path(&backup);
                }
                Err(restore) => tracing::error!(
                    dest = %dest.display(),
                    backup = %backup.display(),
                    error = %restore,
                    "failed to restore snapshot after refresh promote failure; prior tree kept at backup"
                ),
            }
        }
        return Err(InstallError::Io {
            path: dest.to_path_buf(),
            source: e,
        });
    }
    let _ = remove_repo_path(&backup);

    Ok(Some(repo_plugin_map(&discovered)))
}

/// Promote the freshly-copied `tmp` tree onto `dest`. A test hook can force this
/// to fail to exercise the rename-aside rollback path.
fn promote_tmp_to_dest(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    {
        if std::env::var_os("ECHO_AGENT_TEST_FAIL_REFRESH_PROMOTE").is_some() {
            return Err(std::io::Error::other(
                "test-injected refresh promote failure",
            ));
        }
    }
    std::fs::rename(tmp, dest)
}

/// Best-effort removal of orphaned `.<name>.refresh-*` / `.<name>.backup-*`
/// siblings left by a crash between copy and promote. Only entries older than
/// [`STALE_SWEEP_AGE`] are reaped, so a concurrent live refresh's in-flight
/// working dir (pid-named, freshly created) is never deleted out from under it.
fn sweep_stale(parent: &Path, file_name: &str) {
    let refresh_prefix = format!(".{file_name}.refresh-");
    let backup_prefix = format!(".{file_name}.backup-");
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !name.starts_with(&refresh_prefix) && !name.starts_with(&backup_prefix) {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age >= STALE_SWEEP_AGE);
        if stale {
            let _ = remove_repo_path(&entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::git_install::{InstallResult, InstallSource, install_from_source};
    use super::super::install_registry::InstalledRepo;
    use super::*;
    use serial_test::serial;

    /// RAII guard: sets an env var, restores the prior value (or unsets) on drop,
    /// so a test never leaves process-global env pointing at a dropped tempdir.
    struct EnvVarGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let prev = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => unsafe { std::env::set_var(self.key, v) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    // Canonical home: under-home auto-trust canonicalizes the candidate but not
    // `$HOME` (macOS `/var` -> `/private/var`). The guard restores `$HOME` on drop.
    fn home_tempdir() -> (tempfile::TempDir, PathBuf, EnvVarGuard) {
        let tmp = tempfile::tempdir().unwrap();
        let home = dunce::canonicalize(tmp.path()).unwrap();
        let guard = EnvVarGuard::set("HOME", &home);
        (tmp, home, guard)
    }

    fn write_plugin_json(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("plugin.json"), format!(r#"{{"name":"{name}"}}"#)).unwrap();
    }

    fn write_agent_md(plugin_dir: &Path, name: &str) {
        std::fs::create_dir_all(plugin_dir.join("agents")).unwrap();
        std::fs::write(
            plugin_dir.join("agents").join(format!("{name}.md")),
            format!("---\nname: {name}\ndescription: d\n---\n"),
        )
        .unwrap();
    }

    // Install `source` (optionally scoped to `subdir`) and record it in
    // `registry`, mirroring what the install command persists.
    fn register_local_install(
        registry: &mut InstallRegistry,
        source: &Path,
        subdir: Option<&str>,
    ) -> InstallResult {
        let installed = install_from_source(
            &InstallSource::Local {
                path: source.to_path_buf(),
                subdir: subdir.map(str::to_string),
            },
            registry,
            false,
        )
        .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        registry.insert(
            installed.repo_key.clone(),
            InstalledRepo {
                kind: InstallKind::Local {
                    source_path: source.to_path_buf(),
                    subdir: subdir.map(str::to_string),
                },
                installed_at: now.clone(),
                updated_at: now,
                path: installed.repo_path.clone(),
                plugins: repo_plugin_map(&installed.plugins),
                marketplace: None,
            },
        );
        installed
    }

    #[test]
    #[serial(home_env)]
    fn refresh_local_install_picks_up_new_agent() {
        let (_home_tmp, home, _home_guard) = home_tempdir();
        let source = home.join(".claude").join("demo-plugin");
        write_plugin_json(&source, "demo-plugin");
        write_agent_md(&source, "old");

        let mut registry =
            InstallRegistry::empty(home.join(".echo-agent").join("installed-plugins"));
        let installed = register_local_install(&mut registry, &source, None);

        write_agent_md(&source, "new");
        assert!(!installed.repo_path.join("agents/new.md").exists());

        let trust = TrustStore::load_from(home.join(".echo-agent").join("trusted-plugins"));
        let summary = refresh_local_installs(&mut registry, &trust, false);
        assert_eq!(summary.refreshed, 1, "{summary:?}");
        assert!(installed.repo_path.join("agents/new.md").exists());
    }

    #[test]
    #[serial(home_env)]
    fn refresh_skips_unchanged_source() {
        let (_home_tmp, home, _home_guard) = home_tempdir();
        let source = home.join(".claude").join("demo-plugin");
        write_plugin_json(&source, "demo-plugin");
        write_agent_md(&source, "old");

        let mut registry =
            InstallRegistry::empty(home.join(".echo-agent").join("installed-plugins"));
        let installed = register_local_install(&mut registry, &source, None);

        // No edit to the source: snapshot matches, so refresh is a stat-walk skip
        // with no re-copy.
        let snapshot = installed.repo_path.join("agents/old.md");
        let before = std::fs::metadata(&snapshot).unwrap().modified().unwrap();
        let trust = TrustStore::load_from(home.join(".echo-agent").join("trusted-plugins"));
        let summary = refresh_local_installs(&mut registry, &trust, false);
        assert_eq!(summary.refreshed, 0, "{summary:?}");
        assert_eq!(summary.skipped, 1, "{summary:?}");
        let after = std::fs::metadata(&snapshot).unwrap().modified().unwrap();
        assert_eq!(before, after, "unchanged snapshot must not be re-copied");
    }

    #[test]
    #[serial(home_env)]
    fn refresh_picks_up_content_preserving_rename() {
        let (_home_tmp, home, _home_guard) = home_tempdir();
        let source = home.join(".claude").join("demo-plugin");
        write_plugin_json(&source, "demo-plugin");
        write_agent_md(&source, "old");

        let mut registry =
            InstallRegistry::empty(home.join(".echo-agent").join("installed-plugins"));
        let installed = register_local_install(&mut registry, &source, None);

        // Rename keeps file count, total size, and the file's (old) mtime — the
        // old aggregate fingerprint skipped this; the structural file-set catches it.
        std::fs::rename(
            source.join("agents/old.md"),
            source.join("agents/renamed.md"),
        )
        .unwrap();

        let trust = TrustStore::load_from(home.join(".echo-agent").join("trusted-plugins"));
        let summary = refresh_local_installs(&mut registry, &trust, false);
        assert_eq!(
            summary.refreshed, 1,
            "rename must trigger refresh: {summary:?}"
        );
        assert!(installed.repo_path.join("agents/renamed.md").exists());
        assert!(!installed.repo_path.join("agents/old.md").exists());
    }

    #[test]
    #[serial(home_env)]
    fn refresh_promote_failure_rolls_back_to_prior_snapshot() {
        let (_home_tmp, home, _home_guard) = home_tempdir();
        let source = home.join(".claude").join("demo-plugin");
        write_plugin_json(&source, "demo-plugin");
        write_agent_md(&source, "old");

        let mut registry =
            InstallRegistry::empty(home.join(".echo-agent").join("installed-plugins"));
        let installed = register_local_install(&mut registry, &source, None);

        // Change the source so a refresh attempts a re-copy, then force the
        // promote rename to fail and assert the prior snapshot is restored.
        write_agent_md(&source, "new");
        let trust = TrustStore::load_from(home.join(".echo-agent").join("trusted-plugins"));
        let summary = {
            let _fail = EnvVarGuard::set("ECHO_AGENT_TEST_FAIL_REFRESH_PROMOTE", "1");
            refresh_local_installs(&mut registry, &trust, false)
        };

        assert_eq!(summary.errors, 1, "{summary:?}");
        assert_eq!(summary.refreshed, 0, "{summary:?}");
        // dest is never left missing and still holds the prior snapshot.
        assert!(installed.repo_path.join("agents/old.md").exists());
        assert!(!installed.repo_path.join("agents/new.md").exists());
    }

    #[test]
    #[serial(home_env)]
    fn refresh_skips_untrusted_source_outside_home() {
        let (_home_tmp, home, _home_guard) = home_tempdir();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("untrusted-plugin");
        write_plugin_json(&source, "untrusted-plugin");

        let mut registry = InstallRegistry::empty(home.join("installed-plugins"));
        let installed = register_local_install(&mut registry, &source, None);

        std::fs::write(source.join("extra.txt"), "x").unwrap();
        let trust = TrustStore::load_from(home.join("trusted-plugins"));
        let summary = refresh_local_installs(&mut registry, &trust, false);
        assert_eq!(summary.skipped, 1, "{summary:?}");
        assert_eq!(summary.refreshed, 0, "{summary:?}");
        assert!(!installed.repo_path.join("extra.txt").exists());
    }

    #[test]
    #[serial(home_env)]
    fn refresh_trusted_source_outside_home() {
        let (_home_tmp, home, _home_guard) = home_tempdir();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("trusted-plugin");
        write_plugin_json(&source, "trusted-plugin");
        write_agent_md(&source, "old");

        let mut trust = TrustStore::load_from(home.join("trusted-plugins"));
        trust.grant_trust(&source).unwrap();

        let mut registry = InstallRegistry::empty(home.join("installed-plugins"));
        let installed = register_local_install(&mut registry, &source, None);

        write_agent_md(&source, "new");
        let summary = refresh_local_installs(&mut registry, &trust, false);
        assert_eq!(summary.refreshed, 1, "{summary:?}");
        assert!(installed.repo_path.join("agents/new.md").exists());
    }

    #[test]
    #[serial(home_env)]
    fn refresh_preserves_install_subdir_scope() {
        let (_home_tmp, home, _home_guard) = home_tempdir();
        let workspace = home.join("workspace");
        write_plugin_json(&workspace.join("plugins/a"), "plugin-a");
        write_plugin_json(&workspace.join("plugins/b"), "plugin-b");

        let mut registry = InstallRegistry::empty(home.join(".echo-agent/installed-plugins"));
        let installed = register_local_install(&mut registry, &workspace, Some("plugins/a"));

        write_agent_md(&workspace.join("plugins/a"), "x");

        let trust = TrustStore::load_from(home.join("trusted-plugins"));
        let summary = refresh_local_installs(&mut registry, &trust, false);
        assert_eq!(summary.refreshed, 1, "{summary:?}");
        assert!(installed.repo_path.join("plugins/a/agents/x.md").exists());

        let repo = registry.get_repo(&installed.repo_key).unwrap();
        match &repo.kind {
            InstallKind::Local { subdir, .. } => assert_eq!(subdir.as_deref(), Some("plugins/a")),
            _ => panic!("expected Local"),
        }
        assert!(repo.plugins.contains_key("plugin-a"));
        assert!(!repo.plugins.contains_key("plugin-b"));
    }

    #[test]
    #[serial(home_env)]
    fn refresh_does_not_follow_directory_symlinks() {
        let (_home_tmp, home, _home_guard) = home_tempdir();
        let secret = home.join("secret-dir");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("secret.txt"), "leak").unwrap();

        let source = home.join("plugin");
        write_plugin_json(&source, "plugin");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, source.join("link-out")).unwrap();

        let mut registry = InstallRegistry::empty(home.join("installed-plugins"));
        let installed = register_local_install(&mut registry, &source, None);
        assert!(!installed.repo_path.join("link-out/secret.txt").exists());

        std::fs::write(source.join("extra.txt"), "x").unwrap();
        let trust = TrustStore::load_from(home.join("trusted-plugins"));
        let summary = refresh_local_installs(&mut registry, &trust, false);
        assert_eq!(summary.refreshed, 1, "{summary:?}");
        assert!(!installed.repo_path.join("link-out/secret.txt").exists());
        assert!(installed.repo_path.join("extra.txt").exists());
    }

    #[test]
    #[serial(home_env)]
    fn refresh_keeps_stale_when_legacy_subdir_scope_lost() {
        let (_home_tmp, home, _home_guard) = home_tempdir();
        // Legacy multi-package source: the real plugin is at plugins/foo;
        // other-dir is unrelated root-level content that root-scope discovery
        // would pick up.
        let workspace = home.join("workspace");
        write_plugin_json(&workspace.join("plugins/foo"), "foo");
        write_agent_md(&workspace.join("other-dir"), "noise");

        // Snapshot the full source (mirrors the install-time copy).
        let install_dir = home.join(".echo-agent").join("installed-plugins");
        std::fs::create_dir_all(&install_dir).unwrap();
        let dest = install_dir.join("foo-legacy");
        copy_dir_recursive(&workspace, &dest).unwrap();

        // Legacy entry: install-level `subdir` was never persisted (None), but the
        // per-plugin RepoPlugin recorded the correct scope.
        let mut registry = InstallRegistry::empty(install_dir);
        let now = chrono::Utc::now().to_rfc3339();
        registry.insert(
            "foo-legacy".to_string(),
            InstalledRepo {
                kind: InstallKind::Local {
                    source_path: workspace.clone(),
                    subdir: None,
                },
                installed_at: now.clone(),
                updated_at: now,
                path: dest.clone(),
                plugins: HashMap::from([(
                    "foo".to_string(),
                    RepoPlugin {
                        subdir: Some("plugins/foo".to_string()),
                        version: None,
                    },
                )]),
                marketplace: None,
            },
        );

        // Edit the source under plugins/foo so a content refresh would trigger.
        write_agent_md(&workspace.join("plugins/foo"), "added");

        // force=true so the unchanged-skip can't mask the scope-identity guard.
        let trust = TrustStore::load_from(home.join(".echo-agent").join("trusted-plugins"));
        let summary = refresh_local_installs(&mut registry, &trust, true);

        // Root-scope rediscovery would change the plugin set/scope, so keep stale:
        // no refresh, and repo.plugins / repo.kind must be untouched (no corruption).
        assert_eq!(
            summary.refreshed, 0,
            "scope change must keep stale: {summary:?}"
        );
        let repo = registry.get_repo("foo-legacy").unwrap();
        assert_eq!(repo.plugins.len(), 1);
        assert_eq!(
            repo.plugins.get("foo").and_then(|p| p.subdir.as_deref()),
            Some("plugins/foo")
        );
        match &repo.kind {
            InstallKind::Local {
                source_path,
                subdir,
            } => {
                assert_eq!(source_path, &workspace);
                assert!(subdir.is_none());
            }
            _ => panic!("expected Local"),
        }
    }
}
