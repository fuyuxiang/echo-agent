//! App-scope marketplace revocation preserves compatibility-app files. These
//! bindings are read-only inputs; no foreign settings or installs are deleted.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::discovery::PluginOrigin;

pub fn source_identity(value: &str) -> String {
    let value = value.trim();
    if value.contains("://") || value.starts_with("git@") {
        value
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .to_string()
    } else {
        canonical_local_path(Path::new(value))
            .to_string_lossy()
            .into_owned()
    }
}

fn canonical_local_path(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    if let Ok(canonical) = dunce::canonicalize(&absolute) {
        return canonical;
    }
    for ancestor in absolute.ancestors().skip(1) {
        if let Ok(parent) = dunce::canonicalize(ancestor)
            && let Ok(tail) = absolute.strip_prefix(ancestor)
        {
            return parent.join(tail);
        }
    }
    absolute
}

/// Saved canonical identities must not follow a subsequently repointed link
/// into another owner's source. Normalize syntax without resolving filesystem.
pub fn stored_source_identity(value: &str) -> String {
    let value = value.trim();
    if value.contains("://") || value.starts_with("git@") {
        source_identity(value)
    } else {
        std::path::absolute(value)
            .unwrap_or_else(|_| PathBuf::from(value))
            .to_string_lossy()
            .into_owned()
    }
}

pub fn source_identity_with_aliases(value: &str, aliases: &BTreeMap<String, String>) -> String {
    if value.contains("://") || value.starts_with("git@") {
        return source_identity(value);
    }
    let path = Path::new(value);
    if let Ok(canonical) = dunce::canonicalize(path) {
        // Existing source metadata wins over a historic alias association.
        return canonical.to_string_lossy().into_owned();
    }
    if let Ok(target) = std::fs::read_link(path) {
        let target = if target.is_absolute() {
            target
        } else {
            path.parent().unwrap_or(Path::new(".")).join(target)
        };
        return source_identity(&target.to_string_lossy());
    }
    aliases
        .get(&stored_source_identity(value))
        .map(|identity| stored_source_identity(identity))
        .unwrap_or_else(|| source_identity(value))
}

pub fn filter_installed(
    registry: &mut super::install_registry::InstallRegistry,
    disabled: &[String],
    aliases: &BTreeMap<String, String>,
) {
    let disabled: HashSet<_> = disabled
        .iter()
        .map(|value| stored_source_identity(value))
        .collect();
    registry.repos.retain(|_, repo| {
        !repo
            .marketplace
            .as_ref()
            .is_some_and(|source| disabled.contains(&source.identity_with_aliases(aliases)))
    });
}

#[derive(Default)]
pub struct RevokedCompatSources {
    names: HashSet<String>,
    roots: Vec<PathBuf>,
}

impl RevokedCompatSources {
    pub fn read_in(claude_root: &Path, disabled: &[String]) -> Self {
        Self::read_with_aliases_in(claude_root, disabled, &BTreeMap::new())
    }

    pub fn read_with_aliases_in(
        claude_root: &Path,
        disabled: &[String],
        aliases: &BTreeMap<String, String>,
    ) -> Self {
        Self::read_with_base(
            claude_root,
            disabled,
            claude_root.parent().unwrap_or(claude_root),
            aliases,
        )
    }

    /// User compatibility catalogs retain their existing cwd-relative local
    /// paths, matching the marketplace loader and installed-path discovery.
    /// Project sources instead use read_in's gitroot-relative convention.
    pub fn read_user_in(claude_root: &Path, disabled: &[String]) -> Self {
        Self::read_user_with_aliases_in(claude_root, disabled, &BTreeMap::new())
    }

    pub fn read_user_with_aliases_in(
        claude_root: &Path,
        disabled: &[String],
        aliases: &BTreeMap<String, String>,
    ) -> Self {
        let base = std::env::current_dir()
            .unwrap_or_else(|_| claude_root.parent().unwrap_or(claude_root).to_path_buf());
        Self::read_with_base(claude_root, disabled, &base, aliases)
    }

    fn read_with_base(
        claude_root: &Path,
        disabled: &[String],
        base: &Path,
        aliases: &BTreeMap<String, String>,
    ) -> Self {
        let disabled: HashSet<_> = disabled
            .iter()
            .map(|value| stored_source_identity(value))
            .collect();
        if disabled.is_empty() {
            return Self::default();
        }
        // Settings are compatibility discovery inputs only. A known registry
        // source, when present, is authoritative over a stale same-name setting.
        let mut bindings: HashMap<String, (String, Vec<PathBuf>)> = HashMap::new();
        for filename in ["settings.json", "settings.local.json"] {
            if let Some(settings) = read_json(&claude_root.join(filename))
                && let Some(sources) = settings
                    .get("extraKnownMarketplaces")
                    .and_then(serde_json::Value::as_object)
            {
                for (name, value) in sources {
                    if let Some(identity) = json_identity(value, base, aliases) {
                        bindings.insert(name.clone(), (identity, source_roots(value, base)));
                    }
                }
            }
        }
        if let Some(registry) = read_json(&claude_root.join("plugins/known_marketplaces.json"))
            && let Some(sources) = registry.as_object()
        {
            for (name, value) in sources {
                let roots = source_roots(value, base);
                if let Some(identity) = json_identity(value, base, aliases) {
                    bindings.insert(name.clone(), (identity, roots));
                } else if let Some((_, known_roots)) = bindings.get_mut(name) {
                    known_roots.extend(roots);
                }
            }
        }
        let mut scopes = Self::default();
        for (name, (identity, roots)) in bindings {
            if disabled.contains(&identity) {
                scopes.names.insert(name);
                scopes.roots.extend(roots);
            }
        }
        // Installed cache entries may live outside a marketplace's clone.
        // Bind them through the actual source identity above, never a display
        // name alone; keep all external files byte-for-byte untouched.
        if let Some(installed) = read_json(&claude_root.join("plugins/installed_plugins.json")) {
            let plugins = installed.get("plugins").unwrap_or(&installed);
            if let Some(entries) = plugins.as_object() {
                for (key, value) in entries {
                    let Some((_, marketplace)) = key.rsplit_once('@') else {
                        continue;
                    };
                    if !scopes.names.contains(marketplace) {
                        continue;
                    }
                    let records: Vec<_> = match value.as_array() {
                        Some(records) => records.iter().collect(),
                        None => vec![value],
                    };
                    for record in records {
                        if let Some(path) = record
                            .get("installPath")
                            .and_then(serde_json::Value::as_str)
                        {
                            scopes.roots.push(resolve_path(path, base));
                        }
                    }
                }
            }
        }
        scopes
    }

    pub fn blocks(&self, origin: &PluginOrigin, canonical_root: &Path) -> bool {
        let known_name = match origin {
            PluginOrigin::ClaudeMarketplace { marketplace } => self.names.contains(marketplace),
            PluginOrigin::ClaudeInstalled {
                marketplace: Some(marketplace),
            } => self.names.contains(marketplace),
            _ => false,
        };
        let compatibility_source = matches!(
            origin,
            PluginOrigin::UserClaude
                | PluginOrigin::ProjectClaude
                | PluginOrigin::ClaudeMarketplace { .. }
                | PluginOrigin::ClaudeInstalled { .. }
        );
        compatibility_source
            && (known_name
                || self
                    .roots
                    .iter()
                    .any(|root| canonical_root.starts_with(root)))
    }
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn resolve_path(value: &str, base: &Path) -> PathBuf {
    canonical_local_path(&logical_path(value, base))
}

fn logical_path(value: &str, base: &Path) -> PathBuf {
    let path = if let Some(relative) = value.strip_prefix("~/") {
        echo_agent_config::discovery_home_dir()
            .unwrap_or_else(|| base.to_path_buf())
            .join(relative)
    } else {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    };
    path
}

fn json_identity(
    value: &serde_json::Value,
    base: &Path,
    aliases: &BTreeMap<String, String>,
) -> Option<String> {
    let source = match value.get("source") {
        Some(source) if source.is_object() => source,
        Some(source) if source.is_string() => value,
        _ => return None,
    };
    match source.get("source")?.as_str()? {
        "git" => source.get("url")?.as_str().map(source_identity),
        "github" => source
            .get("repo")?
            .as_str()
            .map(|repo| source_identity(&format!("https://github.com/{repo}.git"))),
        "local" | "directory" => source.get("path")?.as_str().map(|path| {
            source_identity_with_aliases(&logical_path(path, base).to_string_lossy(), aliases)
        }),
        _ => None,
    }
}

fn source_roots(value: &serde_json::Value, base: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(path) = value
        .get("installLocation")
        .and_then(serde_json::Value::as_str)
    {
        roots.push(resolve_path(path, base));
    }
    let source = value
        .get("source")
        .filter(|source| source.is_object())
        .unwrap_or(value);
    if matches!(
        source.get("source").and_then(serde_json::Value::as_str),
        Some("local" | "directory")
    ) && let Some(path) = source.get("path").and_then(serde_json::Value::as_str)
    {
        roots.push(resolve_path(path, base));
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_tempdir() -> tempfile::TempDir {
        // Discovery always calls blocks with plugin.canonical_root. macOS's
        // default /var TMPDIR aliases /private/var, so the fixture must use
        // the same canonical argument contract while retaining all checks.
        tempfile::tempdir_in(dunce::canonicalize(std::env::temp_dir()).unwrap()).unwrap()
    }

    fn write_registry(root: &Path, url: &str, install: &Path) -> (PathBuf, Vec<u8>) {
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        let file = root.join("plugins/known_marketplaces.json");
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "review-market": {"source":{"source":"git", "url":url}, "installLocation":install}
        }))
        .unwrap();
        std::fs::write(&file, &bytes).unwrap();
        (file, bytes)
    }

    #[test]
    fn disabled_marketplace_scope_revokes_compat_execution_without_external_writes() {
        let tmp = canonical_tempdir();
        let external = tmp.path().join("other-app");
        let install = tmp.path().join("existing-foreign-install");
        std::fs::create_dir_all(install.join("plugins/review-plugin")).unwrap();
        let source = "https://127.0.0.1:4443/review-market.git";
        let (registry, before) = write_registry(&external, source, &install);
        let scopes = RevokedCompatSources::read_in(&external, &[source_identity(source)]);
        assert!(
            scopes.blocks(
                &PluginOrigin::ClaudeMarketplace {
                    marketplace: "review-market".into()
                },
                &install.join("plugins/review-plugin")
            ),
            "Revoked external source would still register executable plugin components"
        );
        assert!(scopes.blocks(
            &PluginOrigin::ClaudeInstalled {
                marketplace: Some("review-market".into())
            },
            &tmp.path().join("compat-cache/review-plugin")
        ));
        assert!(scopes.blocks(
            &PluginOrigin::UserClaude,
            &install.join("plugins/review-plugin")
        ));
        assert_eq!(std::fs::read(registry).unwrap(), before);
        assert!(install.join("plugins/review-plugin").is_dir());
    }

    #[test]
    fn disabled_marketplace_scope_keeps_other_owner_and_explicit_plugin_paths() {
        let tmp = canonical_tempdir();
        let external = tmp.path().join("other-app");
        let install = tmp.path().join("unrelated-install");
        std::fs::create_dir_all(&install).unwrap();
        let (registry, before) =
            write_registry(&external, "https://example.invalid/different.git", &install);
        let scopes = RevokedCompatSources::read_in(
            &external,
            &[source_identity("https://127.0.0.1:4443/review-market.git")],
        );
        assert!(!scopes.blocks(
            &PluginOrigin::ClaudeMarketplace {
                marketplace: "review-market".into()
            },
            &install
        ));
        assert!(!scopes.blocks(&PluginOrigin::ConfigPath, &install));
        assert!(!scopes.blocks(&PluginOrigin::CliOverride, &install));
        assert_eq!(std::fs::read(registry).unwrap(), before);
    }

    #[test]
    fn disabled_marketplace_scope_preserves_remote_alias_and_local_identity() {
        assert_eq!(
            source_identity("https://example.invalid/a.git"),
            source_identity("https://example.invalid/a/")
        );
        let tmp = canonical_tempdir();
        let local = tmp.path().join("actual.git");
        std::fs::create_dir(&local).unwrap();
        assert_ne!(
            source_identity(local.to_str().unwrap()),
            source_identity(tmp.path().join("actual").to_str().unwrap())
        );
    }

    #[test]
    fn disabled_marketplace_scope_current_registry_identity_wins_over_stale_name() {
        let tmp = canonical_tempdir();
        let external = tmp.path().join("external");
        let install = tmp.path().join("unrelated-install");
        std::fs::create_dir_all(&install).unwrap();
        let disabled = "https://127.0.0.1:4443/removed.git";
        write_registry(&external, "https://127.0.0.1:4443/current.git", &install);
        let settings = external.join("settings.local.json");
        std::fs::write(&settings, serde_json::json!({"extraKnownMarketplaces":{"review-market":{"source":{"source":"git","url":disabled}}}}).to_string()).unwrap();
        let before = std::fs::read(&settings).unwrap();
        let scopes = RevokedCompatSources::read_in(&external, &[source_identity(disabled)]);
        assert!(!scopes.blocks(
            &PluginOrigin::ClaudeMarketplace {
                marketplace: "review-market".into()
            },
            &install
        ));
        assert_eq!(std::fs::read(settings).unwrap(), before);
    }

    #[test]
    fn disabled_marketplace_scope_cache_roots_require_a_bound_source_identity() {
        let tmp = canonical_tempdir();
        let external = tmp.path().join("external");
        let install = tmp.path().join("original-clone");
        let cache = tmp.path().join("external-cache/plugin/1.0.0");
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        let source = "https://127.0.0.1:4443/removed.git";
        write_registry(&external, source, &install);
        let path = external.join("plugins/installed_plugins.json");
        std::fs::write(&path, serde_json::json!({"plugins":{"demo@review-market":[{"installPath":cache,"scope":"user"}]}}).to_string()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let scopes = RevokedCompatSources::read_in(&external, &[source_identity(source)]);
        assert!(scopes.blocks(&PluginOrigin::UserClaude, &cache));
        assert!(!scopes.blocks(&PluginOrigin::ConfigPath, &cache));
        assert!(!scopes.blocks(&PluginOrigin::UserEchoAgent, &cache));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn disabled_marketplace_scope_directory_and_github_aliases_are_bound_without_writes() {
        let tmp = canonical_tempdir();
        let external = tmp.path().join(".claude");
        let local = tmp.path().join("local-market");
        std::fs::create_dir_all(&external).unwrap();
        std::fs::create_dir_all(local.join("plugins/demo")).unwrap();
        let path = external.join("settings.json");
        std::fs::write(
            &path,
            serde_json::json!({"extraKnownMarketplaces":{
                "local":{"source":{"source":"directory","path":"local-market"}},
                "github":{"source":{"source":"github","repo":"owner/review"}}
            }})
            .to_string(),
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        let scopes = RevokedCompatSources::read_in(
            &external,
            &[
                source_identity(local.to_str().unwrap()),
                source_identity("https://github.com/owner/review.git"),
            ],
        );
        assert!(scopes.blocks(&PluginOrigin::ProjectClaude, &local.join("plugins/demo")));
        assert!(scopes.blocks(
            &PluginOrigin::ClaudeMarketplace {
                marketplace: "github".into()
            },
            tmp.path()
        ));
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn disabled_marketplace_scope_user_relative_paths_match_loader_and_keep_identity_after_delete()
    {
        let cwd = dunce::canonicalize(std::env::current_dir().unwrap()).unwrap();
        let tmp = tempfile::tempdir_in(&cwd).unwrap();
        let external = tmp.path().join("other-app");
        let install = tmp.path().join("relative-market");
        std::fs::create_dir_all(external.join("plugins")).unwrap();
        std::fs::create_dir_all(install.join("plugins/demo")).unwrap();
        let relative = install.strip_prefix(&cwd).unwrap();
        let identity = source_identity(relative.to_str().unwrap());
        assert_eq!(identity, source_identity(install.to_str().unwrap()));
        let registry = external.join("plugins/known_marketplaces.json");
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "review-market": {"source":{"source":"local", "path":relative}, "installLocation":relative}
        }))
        .unwrap();
        std::fs::write(&registry, &bytes).unwrap();

        let scopes = RevokedCompatSources::read_user_in(&external, &[identity.clone()]);
        assert!(scopes.blocks(
            &PluginOrigin::ClaudeMarketplace {
                marketplace: "review-market".into()
            },
            &install.join("plugins/demo")
        ));
        assert!(scopes.blocks(&PluginOrigin::UserClaude, &install.join("plugins/demo")));
        assert!(!scopes.blocks(&PluginOrigin::ConfigPath, &install.join("plugins/demo")));
        assert_eq!(std::fs::read(&registry).unwrap(), bytes);

        // A moved/deleted local catalog must not revive its installed cache:
        // identity remains absolute even when canonicalize can no longer read it.
        std::fs::remove_dir_all(&install).unwrap();
        assert_eq!(source_identity(relative.to_str().unwrap()), identity);
        let scopes = RevokedCompatSources::read_user_in(&external, &[identity]);
        assert!(scopes.blocks(
            &PluginOrigin::ClaudeInstalled {
                marketplace: Some("review-market".into())
            },
            &tmp.path().join("cached-demo")
        ));
        assert_eq!(std::fs::read(registry).unwrap(), bytes);
    }
}
