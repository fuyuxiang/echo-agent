// Modified by EchoAgent for the embedded desktop runtime; see ECHOAGENT_VENDOR.md.
//! Skill and command discovery for system prompt injection.
//!
//! Orchestrates priority-based discovery across local, repo, optional
//! workspace-user, user, bundled, config-path, and plugin sources. Parsing primitives
//! live in `echo_agent_tools::implementations::skills::discovery`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::plugins::discovery::PluginScope;
use echo_agent_tools::implementations::skills::types::skill_name_from_path;
pub use echo_agent_tools::implementations::skills::types::{SkillInfo, SkillScope};
/// Re-export so agent-side discovery (and the shell) can name the resolved
/// vendor-compat config without reaching into `echo_agent_tools` directly.
pub use echo_agent_tools::types::compat::CompatConfig;

use echo_agent_tools::implementations::skills::discovery::{
    find_command_paths, find_skill_md_paths, find_skill_paths, is_valid_skill_name,
    normalize_skill_name, parse_skill_files, scan_md_files, walk_for_skill_md,
};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct SkillsConfig {
    /// Additional skill locations to load. Each entry is a `SKILL.md` file or a
    /// directory walked recursively. Supports `~` expansion.
    #[serde(default)]
    pub paths: Vec<String>,

    /// Path prefixes to exclude. Any skill whose resolved path starts with one of
    /// these entries is filtered out. Supports `~` expansion.
    #[serde(default)]
    pub ignore: Vec<String>,

    /// Skill names that are disabled. Disabled skills remain in the list
    /// (unlike `ignore` which hides them entirely) but are excluded from
    /// the system prompt and skill tool invocation.
    #[serde(default)]
    pub disabled: Vec<String>,

    /// Launcher-injected server-synced skill dirs (tagged `Server` scope).
    #[serde(default)]
    pub server_skill_dirs: Vec<String>,

    /// Server skill names that local/repo/user skills are not allowed to shadow.
    /// The launcher only populates this from a verified enterprise policy.
    #[serde(default)]
    pub server_enforced_skill_names: Vec<String>,

    /// Launcher-injected platform bundled skill dirs (tagged `Bundled` scope).
    #[serde(default)]
    pub bundled_skill_dirs: Vec<String>,
}

/// List all discovered skills with their metadata.
///
/// Priority order: Local (cwd/.echo-agent/skills, cwd/.agents/skills, cwd/.claude/skills) → Intermediate dirs →
/// Repo (repo_root/.echo-agent/skills, repo_root/.agents/skills, repo_root/.claude/skills) → User (~/.echo-agent/skills, ~/.agents/skills, ~/.claude/skills)
/// → additional paths from `config.paths`
/// → Server (injected `config.server_skill_dirs`)
/// → Bundled (injected `config.bundled_skill_dirs` + `~/.echo-agent/bundled`; lowest precedence).
///
/// `config.ignore` globs are applied across all sources after collection.
/// Skills with the same name from higher-priority sources override lower-priority ones.
///
/// When `working_directory` is `None`, only User-scoped skills are returned.
///
/// `compat` gates which vendor (`.claude`/`.cursor`) dirs are scanned; pass
/// `CompatConfig::default()` to preserve the historical all-vendors behavior.
pub async fn list_skills(
    working_directory: Option<&str>,
    config: &SkillsConfig,
    compat: CompatConfig,
) -> Vec<SkillInfo> {
    list_skills_with_plugins(working_directory, config, None, compat).await
}

/// List all discovered skills including plugin-provided skills.
///
/// When `plugins` is `Some`, skills from enabled plugins are appended with
/// `plugin_name: Some(...)` and `scope` set to the plugin's origin
/// (e.g. `Repo` for `.echo-agent/plugins/`). Native skills always win bare-name
/// resolution, but qualified plugin entries (`my-plugin:hello`) are
/// preserved even on collision.
pub async fn list_skills_with_plugins(
    working_directory: Option<&str>,
    config: &SkillsConfig,
    plugins: Option<&crate::plugins::PluginRegistry>,
    compat: CompatConfig,
) -> Vec<SkillInfo> {
    let _skill_discovery_timer = crate::timing::timer("skill_discovery");
    let workspace_user_dir = crate::prompt::workspace_user::optional_workspace_user_dir();

    let mut skills = list_skills_with_options(
        working_directory,
        workspace_user_dir.as_deref(),
        &echo_agent_tools::util::echo_agent_home::echo_agent_home(),
        compat,
    )
    .await;

    let git_root = working_directory.and_then(|wd| {
        git2::Repository::discover(wd)
            .ok()
            .and_then(|repo| repo.workdir().map(|p| p.to_path_buf()))
    });
    skills.extend(collect_config_skills(&config.paths, git_root.as_deref()));

    let server_skills = collect_injected_skills(&config.server_skill_dirs, SkillScope::Server);
    if !config.server_enforced_skill_names.is_empty() {
        let enforced: HashSet<&str> = config
            .server_enforced_skill_names
            .iter()
            .map(String::as_str)
            .collect();
        // Enforced managed versions become the first claimant for their name.
        // This is a runtime security rule; hiding a local collision in the UI is insufficient.
        skills.retain(|skill| !enforced.contains(skill.name.as_str()));
        let mut ordered = server_skills
            .iter()
            .filter(|skill| enforced.contains(skill.name.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        ordered.extend(skills);
        skills = ordered;
    }
    skills.extend(server_skills);
    skills.extend(collect_injected_skills(
        &config.bundled_skill_dirs,
        SkillScope::Bundled,
    ));

    let mut skills = filter_skills(skills, &config.ignore);
    skills.sort_by_key(|s| s.scope);

    let plugin_skills = if let Some(registry) = plugins {
        collect_plugin_skills(registry)
    } else {
        vec![]
    };

    let mut merged = merge_skills_with_plugins(skills, plugin_skills);

    // Mark disabled skills. Disabled skills remain in the list (unlike
    // `ignore` which hides them) but are excluded from the system prompt
    // and skill tool invocation.
    if !config.disabled.is_empty() {
        let disabled_set: HashSet<&str> = config.disabled.iter().map(|s| s.as_str()).collect();
        for skill in &mut merged {
            if disabled_set.contains(skill.name.as_str())
                && !config
                    .server_enforced_skill_names
                    .iter()
                    .any(|name| name == &skill.name)
            {
                skill.enabled = false;
            }
        }
    }

    merged
}

/// Canonical source of all config directories that may contain skills.
///
/// Both skill discovery and the file watcher call this function so they
/// agree on which directories matter.
pub fn collect_skill_config_dirs(
    cwd: Option<&Path>,
    workspace_user_dir: Option<&Path>,
    global_dir: &Path,
    config_paths: &[String],
    compat: CompatConfig,
) -> Vec<PathBuf> {
    let echo_agent_home = global_dir.to_path_buf();
    let git_root = cwd.and_then(|c| {
        git2::Repository::discover(c)
            .ok()
            .and_then(|repo| repo.workdir().map(|p| p.to_path_buf()))
    });

    let mut dirs = Vec::new();
    let mut seen = HashSet::new();

    // Helper: add if the directory exists and hasn't been seen yet.
    let mut try_add = |dir: PathBuf| {
        if !dir.is_dir() {
            return;
        }
        let canonical = dunce::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if seen.insert(canonical) {
            dirs.push(dir);
        }
    };

    // Vendor dirs (`.claude`/`.cursor`) are gated by the resolved compat
    // config; `.echo-agent` and `.agents` are always present. When all cells are on
    // this list equals the historical `[".echo-agent", ".agents", ".claude", ".cursor"]`.
    let config_dir_names = compat.skill_config_dirs();

    // Priority 1 & 2: Walk from cwd up to the git root.
    if let Some(cwd) = cwd {
        if let Some(ref root) = git_root {
            let mut current = Some(cwd.to_path_buf());
            while let Some(dir) = current {
                for name in &config_dir_names {
                    try_add(dir.join(name));
                }
                if dir == *root {
                    break;
                }
                current = dir.parent().map(|p| p.to_path_buf());
            }
        } else {
            for name in &config_dir_names {
                try_add(cwd.join(name));
            }
        }
        for directory in workflow_skill_dirs(global_dir, cwd) {
            try_add(directory);
        }
    }

    // Priority 2.5: Optional workspace user dir.
    if let Some(user_dir) = workspace_user_dir {
        for name in &config_dir_names {
            try_add(user_dir.join(name));
        }
    }

    // Priority 3: Global user dirs. `.echo-agent` comes from `echo_agent_home` (which may
    // be overridden), so it's handled separately; `.agents` is always added,
    // while `.claude`/`.cursor` are gated by the skills compat cells.
    try_add(echo_agent_home);
    if let Some(home) = echo_agent_config::discovery_home_dir() {
        try_add(home.join(".agents"));
        if compat.claude.skills {
            try_add(home.join(".claude"));
        }
        if compat.cursor.skills {
            try_add(home.join(".cursor"));
        }
    }

    // Priority 4: Config paths (skills.paths entries).
    for raw in config_paths {
        let expanded = expand_tilde(raw);
        if expanded.is_dir() {
            try_add(expanded);
        } else if expanded.is_file()
            && let Some(parent) = expanded.parent()
        {
            try_add(parent.to_path_buf());
        }
    }

    dirs
}

/// The desktop gives every default task an isolated CWD. Its reusable skills
/// live in the shared default workspace, without sharing any task files.
static DEFAULT_WORKFLOW_WORKSPACE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_default_workflow_workspace(path: &Path) {
    let _ = DEFAULT_WORKFLOW_WORKSPACE.set(dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
}

pub fn default_workflow_workspace() -> Option<&'static Path> {
    DEFAULT_WORKFLOW_WORKSPACE.get().map(PathBuf::as_path)
}

pub fn workflow_library_workspace(cwd: &Path) -> PathBuf {
    let cwd = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    if let Some(root) = DEFAULT_WORKFLOW_WORKSPACE.get()
        && cwd.parent() == Some(root.join("Tasks").as_path())
        && cwd.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
            name.len() == 36 && name.bytes().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => byte == b'-',
                _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
            })
        })
    {
        return root.clone();
    }
    cwd
}

pub fn workflow_skill_dir(home: &Path, cwd: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let cwd = dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let key = format!("{:x}", Sha256::digest(cwd.to_string_lossy().as_bytes()));
    home.join("workflow-library").join(key)
}

fn workflow_skill_dirs(home: &Path, cwd: &Path) -> Vec<PathBuf> {
    let exact = workflow_skill_dir(home, cwd);
    let shared = workflow_skill_dir(home, &workflow_library_workspace(cwd));
    if exact == shared { vec![exact] } else { vec![exact, shared] }
}

/// Determine the skill scope for a config directory based on its location
/// relative to `cwd`, `git_root`, and the user's home directory.
fn scope_for_config_dir(dir: &Path, cwd: Option<&Path>, git_root: Option<&Path>) -> SkillScope {
    // Home-level dirs (e.g. ~/.echo-agent/, ~/.agents/, ~/.claude/) are User scope.
    if let Some(home) = echo_agent_config::discovery_home_dir()
        && dir.parent() == Some(home.as_path())
    {
        return SkillScope::User;
    }

    // Dir whose parent is cwd is Local scope.
    if let Some(cwd) = cwd
        && dir.parent() == Some(cwd)
    {
        return SkillScope::Local;
    }

    // Dir under git root is Repo scope.
    if let Some(root) = git_root
        && dir.starts_with(root)
    {
        return SkillScope::Repo;
    }

    SkillScope::User
}

/// Collect paths into `out`, deduplicating by canonical path.
///
/// Skill/command discovery does **not** consult `.gitignore`. Auto-discovery
/// only visits known config roots (`.echo-agent`, `.agents`, `.claude`, `.cursor`),
/// which teams often gitignore as local-only config while still expecting them
/// to load. Hiding a skill uses `[skills] ignore` in config, not repo ignore
/// rules. AGENTS.md discovery still honors gitignore — that is content, not
/// skill roots.
fn collect_discovered_paths(
    paths: impl IntoIterator<Item = PathBuf>,
    scope: SkillScope,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<(PathBuf, SkillScope)>,
) {
    for path in paths {
        let canonical = dunce::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if seen.insert(canonical) {
            out.push((path, scope));
        }
    }
}

/// Discover skills and commands from config dirs, workspace, and bundled paths.
/// Skills are collected before commands so they win name collisions via
/// first-seen-wins dedup. Returns only global skills when `working_directory`
/// is `None`.
async fn list_skills_with_options(
    working_directory: Option<&str>,
    workspace_user_dir: Option<&Path>,
    global_dir: &Path,
    compat: CompatConfig,
) -> Vec<SkillInfo> {
    let cwd = working_directory.map(PathBuf::from);

    let git_root = cwd.as_ref().and_then(|c| {
        git2::Repository::discover(c)
            .ok()
            .and_then(|repo| repo.workdir().map(|p| p.to_path_buf()))
    });

    let config_dirs =
        collect_skill_config_dirs(cwd.as_deref(), workspace_user_dir, global_dir, &[], compat);

    let mut skill_files: Vec<(PathBuf, SkillScope)> = Vec::new();
    let mut seen_canonical_paths = HashSet::new();

    let workflow_dirs = cwd
        .as_deref()
        .map(|cwd| workflow_skill_dirs(global_dir, cwd))
        .unwrap_or_default();
    for config_dir in &config_dirs {
        let is_workflow_dir = workflow_dirs.contains(config_dir);
        let scope = if is_workflow_dir {
            SkillScope::Local
        } else {
            scope_for_config_dir(config_dir, cwd.as_deref(), git_root.as_deref())
        };

        // Skills before commands: skills win name collisions.
        collect_discovered_paths(
            find_skill_paths(config_dir),
            scope,
            &mut seen_canonical_paths,
            &mut skill_files,
        );
        if is_workflow_dir {
            continue;
        }
        collect_discovered_paths(
            find_command_paths(config_dir),
            scope,
            &mut seen_canonical_paths,
            &mut skill_files,
        );
    }

    let bundled_dir = global_dir.join("bundled");
    collect_discovered_paths(
        find_skill_paths(&bundled_dir),
        SkillScope::Bundled,
        &mut seen_canonical_paths,
        &mut skill_files,
    );

    parse_skill_files(skill_files)
}

/// Expand a `~`-prefixed path string to an absolute `PathBuf`.
fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = echo_agent_config::discovery_home_dir()
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(raw)
}

/// Collect and parse skills from `SkillsConfig.paths` entries.
///
/// Each entry is either a direct SKILL.md file or a directory to walk recursively.
/// `~` is expanded. Scope is `Repo` if the resolved path falls inside `git_root`,
/// otherwise `User`.
fn collect_config_skills(config_paths: &[String], git_root: Option<&Path>) -> Vec<SkillInfo> {
    let mut skill_files: Vec<(PathBuf, SkillScope)> = Vec::new();
    let mut seen = HashSet::new();

    for raw in config_paths {
        let expanded = expand_tilde(raw);
        let scope = match git_root {
            Some(root) if expanded.starts_with(root) => SkillScope::Repo,
            _ => SkillScope::User,
        };

        if expanded.is_file() && expanded.file_name().is_some_and(|n| n == "SKILL.md") {
            collect_discovered_paths(
                std::iter::once(expanded),
                scope,
                &mut seen,
                &mut skill_files,
            );
        } else if expanded.is_dir() {
            let dir_paths = find_skill_md_paths(&expanded);
            collect_discovered_paths(dir_paths, scope, &mut seen, &mut skill_files);
        } else {
            tracing::warn!(
                path = %expanded.display(),
                "config path does not exist or is not a SKILL.md file/directory"
            );
        }
    }

    let mut skills = parse_skill_files(skill_files);
    // Provenance metadata only (scope still drives precedence): lets inspect
    // and UIs distinguish `[skills].paths` entries from plain user/repo skills.
    for skill in &mut skills {
        skill.config_source = Some(
            echo_agent_tools::types::config_source::ConfigSource::ConfigToml {
                path: PathBuf::from(&skill.path),
            },
        );
    }
    skills
}

fn collect_injected_skills(dirs: &[String], scope: SkillScope) -> Vec<SkillInfo> {
    let mut skill_files: Vec<(PathBuf, SkillScope)> = Vec::new();
    let mut seen = HashSet::new();

    for raw in dirs {
        let expanded = expand_tilde(raw);
        if !expanded.is_dir() {
            continue;
        }
        let mut dir_paths = Vec::new();
        walk_for_skill_md(&expanded, &mut dir_paths, 0);
        collect_discovered_paths(dir_paths, scope, &mut seen, &mut skill_files);
    }

    parse_skill_files(skill_files)
}

/// Deduplicate skills while preserving first-seen priority order.
///
/// Dedupes in two passes at once:
/// - By canonical path (same file discovered via multiple sources; the kept
///   entry inherits a dropped duplicate's `config_source` stamp)
/// - By skill name (a higher-priority source wins)
///
/// A same-scope name loser whose directory basename differs from the
/// contested name is re-identified under that basename when that identity
/// is free (see [`rekey_to_dir_basename`]) — the same recovery
/// `stamp_plugin_fields` applies to plugin siblings. A loser whose basename
/// equals the contested name or is already claimed stays shadowed, and a
/// frontmatter owner evicts a claimant that only holds its name via an
/// earlier re-key. Cross-scope shadowing (a higher-priority source claiming
/// a name) is an intentional override and is preserved as-is.
fn dedupe_skills(skills: Vec<SkillInfo>) -> Vec<SkillInfo> {
    let mut seen_paths: HashMap<PathBuf, usize> = HashMap::new();
    // Contested name → (claiming scope, index of the claimant in `deduped`).
    let mut seen_names: HashMap<String, (SkillScope, usize)> = HashMap::new();

    let mut deduped: Vec<SkillInfo> = Vec::with_capacity(skills.len());
    for mut skill in skills {
        let canonical_path =
            dunce::canonicalize(&skill.path).unwrap_or_else(|_| PathBuf::from(&skill.path));

        if let Some(&kept_idx) = seen_paths.get(&canonical_path) {
            // A file reached via both auto-discovery and `[skills].paths` is
            // genuinely both; carry the provenance stamp onto the kept entry
            // so the label isn't source-order-dependent. Scope is untouched.
            let kept = &mut deduped[kept_idx];
            if kept.config_source.is_none() && skill.config_source.is_some() {
                kept.config_source = skill.config_source;
            }
            continue;
        }
        if let Some(&(winner_scope, winner_idx)) = seen_names.get(&skill.name) {
            if winner_scope == skill.scope
                && !matches!(skill.scope, SkillScope::Server | SkillScope::Bundled)
            {
                // Same-scope siblings sharing a frontmatter name keep both:
                // re-key the challenger to its dir basename; when the
                // challenger IS the basename owner, re-key the earlier
                // claimant and hand the name back. A challenger whose rekey
                // failed for other reasons (dir taken/invalid) has no claim
                // and falls through to the shadow-drop.
                if rekey_to_dir_basename(&mut skill, &mut seen_names, deduped.len()) {
                    seen_paths.insert(canonical_path, deduped.len());
                    deduped.push(skill);
                    continue;
                }
                let challenger_owns_basename = skill_name_from_path(&skill.path)
                    .map(normalize_skill_name)
                    .is_some_and(|dir| dir == skill.name);
                if challenger_owns_basename {
                    if rekey_to_dir_basename(&mut deduped[winner_idx], &mut seen_names, winner_idx)
                    {
                        seen_names.insert(skill.name.clone(), (skill.scope, deduped.len()));
                        seen_paths.insert(canonical_path, deduped.len());
                        deduped.push(skill);
                        continue;
                    }
                    if deduped[winner_idx].display_name.is_some() {
                        // The incumbent holds this name only via an earlier
                        // re-key and cannot move again; the frontmatter owner
                        // evicts it (a stale copy must not shadow the skill
                        // genuinely named after its own directory).
                        let evicted = &deduped[winner_idx];
                        let evicted_path = dunce::canonicalize(&evicted.path)
                            .unwrap_or_else(|_| PathBuf::from(&evicted.path));
                        seen_paths.remove(&evicted_path);
                        seen_paths.insert(canonical_path, winner_idx);
                        deduped[winner_idx] = skill;
                        continue;
                    }
                }
            }
            // Server/Bundled are shadowed by design.
            if !matches!(skill.scope, SkillScope::Server | SkillScope::Bundled) {
                tracing::debug!(
                    skill = %skill.name,
                    path = %skill.path,
                    "skill name shadowed by an earlier skill with the same name; rename to avoid the collision"
                );
            }
            continue;
        }
        seen_names.insert(skill.name.clone(), (skill.scope, deduped.len()));

        seen_paths.insert(canonical_path, deduped.len());
        deduped.push(skill);
    }

    deduped
}

/// Re-identify a name-collision party under its directory basename, keeping
/// the frontmatter name as the display label — the copied-skill-dir case
/// (`cp -r japandi japandi2` with `name: japandi` left in both files).
///
/// Returns `false` — leaving the collision to the caller's shadowing path —
/// when the basename is missing/invalid, equals the skill's current name
/// (a true duplicate), or is itself already claimed.
fn rekey_to_dir_basename(
    skill: &mut SkillInfo,
    seen_names: &mut HashMap<String, (SkillScope, usize)>,
    idx: usize,
) -> bool {
    let Some(dir) = skill_name_from_path(&skill.path) else {
        return false;
    };
    let dir = normalize_skill_name(dir);
    if !is_valid_skill_name(&dir) || dir == skill.name || seen_names.contains_key(&dir) {
        return false;
    }
    tracing::debug!(
        skill = %skill.name,
        rekeyed = %dir,
        path = %skill.path,
        "skill name collides with a same-scope skill; re-identified by directory name"
    );
    seen_names.insert(dir.clone(), (skill.scope, idx));
    skill.display_name = Some(std::mem::replace(&mut skill.name, dir));
    true
}

/// Stamp plugin metadata onto skills parsed by `parse_skill_files`.
fn stamp_plugin_fields(skills: &mut [SkillInfo], plugin: &crate::plugins::LoadedPlugin) {
    let scope = match plugin.scope {
        PluginScope::CliOverride => SkillScope::Local,
        PluginScope::Project => SkillScope::Repo,
        PluginScope::User => SkillScope::User,
        PluginScope::ConfigPath => SkillScope::Plugin,
    };
    for skill in skills.iter_mut() {
        skill.scope = scope;
        skill.plugin_name = Some(plugin.name.clone());
        skill.plugin_version = plugin.version.clone();
        skill.plugin_root = Some(plugin.root_str());
        skill.plugin_data = Some(plugin.data_dir_str());
        // Identity is the directory basename (`plugin:<dir>`), keeping sibling
        // skills collision-free; frontmatter `name` becomes the display label.
        // Normalize the basename so the slash name is a valid slug, matching how
        // frontmatter/fallback names are slugged at parse time.
        if let Some(dir) = skill_name_from_path(&skill.path) {
            let dir = normalize_skill_name(dir);
            if !dir.is_empty() && dir != skill.name {
                skill.display_name = Some(std::mem::replace(&mut skill.name, dir));
            }
        }
        skill.config_source = Some(
            echo_agent_tools::types::config_source::ConfigSource::Plugin {
                plugin_name: plugin.name.clone(),
                path: PathBuf::from(&skill.path),
            },
        );
    }
}

fn collect_plugin_skills(registry: &crate::plugins::PluginRegistry) -> Vec<SkillInfo> {
    let mut skills = Vec::new();

    for plugin in registry.enabled_plugins() {
        let mut paths: Vec<(PathBuf, SkillScope)> = Vec::new();

        // Skills: shared discovery primitive (see `find_skill_md_paths`).
        for skill_dir in &plugin.skill_dirs {
            if !skill_dir.is_dir() {
                continue;
            }
            paths.extend(
                find_skill_md_paths(skill_dir)
                    .into_iter()
                    .map(|p| (p, SkillScope::Repo)),
            );
        }

        // Commands (.md files in command directories)
        for cmd_dir in &plugin.command_dirs {
            paths.extend(
                scan_md_files(cmd_dir)
                    .into_iter()
                    .map(|p| (p, SkillScope::Repo)),
            );
        }

        let mut parsed = parse_skill_files(paths);
        stamp_plugin_fields(&mut parsed, plugin);
        skills.extend(parsed);
    }

    skills
}

/// Plugin-aware skill merge (native first, then plugin skills appended with qualified-name dedup).
fn merge_skills_with_plugins(
    native_skills: Vec<SkillInfo>,
    plugin_skills: Vec<SkillInfo>,
) -> Vec<SkillInfo> {
    let mut deduped = dedupe_skills(native_skills);
    let native_names: HashSet<String> = deduped.iter().map(|s| s.name.clone()).collect();

    let mut seen_plugin_qualified: HashSet<String> = HashSet::new();
    for skill in plugin_skills {
        if !seen_plugin_qualified.insert(skill.dedup_key()) {
            continue;
        }
        if native_names.contains(&skill.name) {
            tracing::debug!(
                skill_name = %skill.name,
                plugin = ?skill.plugin_name,
                "plugin skill bare name collides with native; qualified form still available"
            );
        }
        deduped.push(skill);
    }

    deduped
}
/// Filter a list of skills, removing any whose canonical path matches or is within the ignore paths
pub fn filter_skills(skills: Vec<SkillInfo>, ignore_paths: &[String]) -> Vec<SkillInfo> {
    if ignore_paths.is_empty() {
        return skills;
    }
    let expanded: Vec<PathBuf> = ignore_paths
        .iter()
        .map(|p| {
            let path = expand_tilde(p);
            dunce::canonicalize(&path).unwrap_or(path)
        })
        .collect();
    skills
        .into_iter()
        .filter(|skill| {
            let canonical =
                dunce::canonicalize(&skill.path).unwrap_or_else(|_| PathBuf::from(&skill.path));
            // >MAX_PATH caveat (see workspace clippy.toml) — fail-open here: over-long ignored skills stay included.
            !expanded.iter().any(|ignore| canonical.starts_with(ignore))
        })
        .collect()
}

/// Format a skill for prompt injection (if body is populated).
/// Injects plain markdown body — no XML envelope.
pub(crate) fn format_skill_for_injection(skill: &SkillInfo) -> Option<String> {
    skill.body.as_ref().filter(|b| !b.is_empty()).map(|body| {
        echo_agent_tools::implementations::skills::skill::build_skill_message(skill, body)
    })
}

/// Format multiple skills for prompt injection.
/// Returns plain markdown skill bodies, no XML wrapper.
pub(crate) fn format_skills_for_injection(skills: &[SkillInfo]) -> String {
    let parts: Vec<String> = skills
        .iter()
        .filter_map(format_skill_for_injection)
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    // Trailing blank line separates the last skill envelope from the
    // agent's own prompt body, so `</skill>` doesn't run into the body.
    format!("\n\n{}\n\n", parts.join("\n\n"))
}

/// Resolve agent definition `skills:` names to SkillInfo with body populated.
pub(crate) async fn resolve_preloaded_skills(
    names: &[String],
    discovered: &[SkillInfo],
) -> Vec<SkillInfo> {
    let mut result = Vec::new();

    for name in names {
        // Find matching skill (case-insensitive name match, also try qualified name)
        let skill = discovered.iter().find(|s| {
            s.name.eq_ignore_ascii_case(name)
                || echo_agent_tools::implementations::skills::skill::format_skill_name(s)
                    .eq_ignore_ascii_case(name)
        });

        let Some(skill) = skill else {
            tracing::warn!(
                skill_name = %name,
                "Skill declared in agent definition not found in discovered skills"
            );
            continue;
        };

        // Load the skill with body content
        match echo_agent_tools::implementations::skills::skill::load_skill_with_body(skill).await {
            Ok(loaded) => result.push(loaded),
            Err(e) => {
                tracing::warn!(
                    skill_name = %name,
                    path = %skill.path,
                    error = %e,
                    "Failed to load skill body for preloading"
                );
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_tools::implementations::skills::discovery::{
        MAX_BODY_PEEK_BYTES, MAX_SKILL_WALK_DEPTH, SkillParseError, extract_first_paragraph,
        is_valid_skill_name, normalize_skill_name, parse_skill_frontmatter,
    };
    use std::fs;

    /// Helper: create a minimal valid SKILL.md with the given name.
    fn write_skill_md(dir: &Path, name: &str) {
        fs::create_dir_all(dir).unwrap();
        let content = format!(
            "---\nname: {name}\ndescription: A test skill called {name}\n---\n\nSkill body here.\n"
        );
        fs::write(dir.join("SKILL.md"), content).unwrap();
    }

    // ── Server-synced skills (injected server_skill_dirs) ────────────────

    #[tokio::test]
    async fn server_skills_discovered_and_shadowed_by_local() {
        let server = tempfile::tempdir().unwrap();
        write_skill_md(&server.path().join("server-only"), "server-only");
        write_skill_md(&server.path().join("dup"), "dup");

        let cwd = tempfile::tempdir().unwrap();
        write_skill_md(
            &cwd.path().join(".echo-agent").join("skills").join("dup"),
            "dup",
        );

        let config = SkillsConfig {
            server_skill_dirs: vec![server.path().to_string_lossy().into_owned()],
            ..Default::default()
        };
        let skills = list_skills_with_plugins(
            Some(&cwd.path().to_string_lossy()),
            &config,
            None,
            CompatConfig::default(),
        )
        .await;

        let server_only = skills
            .iter()
            .find(|s| s.name == "server-only")
            .expect("server-only skill should be discovered");
        assert_eq!(server_only.scope, SkillScope::Server);

        let dups: Vec<_> = skills.iter().filter(|s| s.name == "dup").collect();
        assert_eq!(dups.len(), 1, "dup should appear once");
        assert_eq!(
            dups[0].scope,
            SkillScope::Local,
            "local skill must shadow the server-synced one"
        );
    }

    #[tokio::test]
    async fn enforced_server_skill_beats_local_and_cannot_be_disabled() {
        let server = tempfile::tempdir().unwrap();
        write_skill_md(&server.path().join("policy"), "policy");

        let cwd = tempfile::tempdir().unwrap();
        write_skill_md(
            &cwd.path().join(".echo-agent").join("skills").join("policy"),
            "policy",
        );

        let config = SkillsConfig {
            disabled: vec!["policy".into()],
            server_skill_dirs: vec![server.path().to_string_lossy().into_owned()],
            server_enforced_skill_names: vec!["policy".into()],
            ..Default::default()
        };
        let skills = list_skills_with_plugins(
            Some(&cwd.path().to_string_lossy()),
            &config,
            None,
            CompatConfig::default(),
        )
        .await;

        let policy: Vec<_> = skills
            .iter()
            .filter(|skill| skill.name == "policy")
            .collect();
        assert_eq!(policy.len(), 1);
        assert_eq!(policy[0].scope, SkillScope::Server);
        assert!(
            policy[0].enabled,
            "enterprise-enforced Skill must stay enabled"
        );
    }

    #[tokio::test]
    async fn bundled_skills_injected_and_shadowed_by_local() {
        let bundled = tempfile::tempdir().unwrap();
        write_skill_md(&bundled.path().join("bundled__helper"), "helper");
        write_skill_md(&bundled.path().join("dup"), "dup");

        let cwd = tempfile::tempdir().unwrap();
        write_skill_md(
            &cwd.path().join(".echo-agent").join("skills").join("dup"),
            "dup",
        );

        let config = SkillsConfig {
            bundled_skill_dirs: vec![bundled.path().to_string_lossy().into_owned()],
            ..Default::default()
        };
        let skills = list_skills_with_plugins(
            Some(&cwd.path().to_string_lossy()),
            &config,
            None,
            CompatConfig::default(),
        )
        .await;

        let helper = skills
            .iter()
            .find(|s| s.name == "helper")
            .expect("bundled helper skill should be discovered");
        assert_eq!(helper.scope, SkillScope::Bundled);

        let dups: Vec<_> = skills.iter().filter(|s| s.name == "dup").collect();
        assert_eq!(dups.len(), 1, "dup should appear once");
        assert_eq!(dups[0].scope, SkillScope::Local);
    }

    #[tokio::test]
    async fn server_skill_beats_bundled() {
        let server = tempfile::tempdir().unwrap();
        write_skill_md(&server.path().join("shared"), "shared");
        let bundled = tempfile::tempdir().unwrap();
        write_skill_md(&bundled.path().join("shared"), "shared");

        let cwd = tempfile::tempdir().unwrap();
        let config = SkillsConfig {
            server_skill_dirs: vec![server.path().to_string_lossy().into_owned()],
            bundled_skill_dirs: vec![bundled.path().to_string_lossy().into_owned()],
            ..Default::default()
        };
        let skills = list_skills_with_plugins(
            Some(&cwd.path().to_string_lossy()),
            &config,
            None,
            CompatConfig::default(),
        )
        .await;

        let shared: Vec<_> = skills.iter().filter(|s| s.name == "shared").collect();
        assert_eq!(shared.len(), 1, "shared should appear once");
        assert_eq!(
            shared[0].scope,
            SkillScope::Server,
            "server skill must shadow the bundled one"
        );
    }

    // ── Feature 3: Recursive skill reading ──────────────────────────────

    #[test]
    fn find_skill_paths_flat_layout() {
        // Traditional flat layout: skills/<name>/SKILL.md
        let tmp = tempfile::tempdir().unwrap();
        let echo_agent_dir = tmp.path().join(".echo-agent");

        write_skill_md(&echo_agent_dir.join("skills").join("alpha"), "alpha");
        write_skill_md(&echo_agent_dir.join("skills").join("beta"), "beta");

        let paths = find_skill_paths(&echo_agent_dir);
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|p| p.file_name().unwrap() == "SKILL.md"));
    }

    #[test]
    fn find_skill_paths_nested_layout() {
        // Nested: skills/team/infra/SKILL.md, skills/team/training/SKILL.md
        let tmp = tempfile::tempdir().unwrap();
        let echo_agent_dir = tmp.path().join(".echo-agent");
        let skills = echo_agent_dir.join("skills");

        write_skill_md(&skills.join("team").join("infra"), "infra");
        write_skill_md(&skills.join("team").join("training"), "training");

        let paths = find_skill_paths(&echo_agent_dir);
        assert_eq!(paths.len(), 2);

        let path_strs: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(path_strs.iter().any(|p| p.contains("infra")));
        assert!(path_strs.iter().any(|p| p.contains("training")));
    }

    #[test]
    fn find_skill_paths_mixed_flat_and_nested() {
        // Mix of flat and nested skills
        let tmp = tempfile::tempdir().unwrap();
        let echo_agent_dir = tmp.path().join(".echo-agent");
        let skills = echo_agent_dir.join("skills");

        // Flat
        write_skill_md(&skills.join("top-level"), "top-level");
        // Nested 1 level
        write_skill_md(&skills.join("team").join("nested-one"), "nested-one");
        // Nested 2 levels
        write_skill_md(&skills.join("org").join("team").join("deep"), "deep");

        let paths = find_skill_paths(&echo_agent_dir);
        assert_eq!(paths.len(), 3);
    }

    #[test]
    fn find_skill_paths_dir_without_skill_md_is_skipped() {
        // A subdirectory exists but has no SKILL.md — should not appear
        let tmp = tempfile::tempdir().unwrap();
        let echo_agent_dir = tmp.path().join(".echo-agent");
        let skills = echo_agent_dir.join("skills");

        write_skill_md(&skills.join("valid"), "valid");
        // Create a dir with no SKILL.md
        fs::create_dir_all(skills.join("empty-dir")).unwrap();
        // Create a dir with a random file but no SKILL.md
        let other = skills.join("other");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("README.md"), "not a skill").unwrap();

        let paths = find_skill_paths(&echo_agent_dir);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].display().to_string().contains("valid"));
    }

    #[test]
    fn find_skill_paths_no_skills_dir() {
        // .echo-agent exists but no skills/ subdirectory
        let tmp = tempfile::tempdir().unwrap();
        let echo_agent_dir = tmp.path().join(".echo-agent");
        fs::create_dir_all(&echo_agent_dir).unwrap();

        let paths = find_skill_paths(&echo_agent_dir);
        assert!(paths.is_empty());
    }

    #[test]
    fn find_skill_paths_nonexistent_dir() {
        let paths = find_skill_paths(Path::new("/nonexistent/path"));
        assert!(paths.is_empty());
    }

    #[test]
    fn walk_for_skill_md_respects_depth_limit() {
        // Create a directory tree deeper than MAX_SKILL_WALK_DEPTH
        let tmp = tempfile::tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");

        // Build a chain: skills/d0/d1/d2/d3/d4/d5/d6/deep-skill/SKILL.md
        // depth 0=d0, 1=d1, ..., 5=d5 (at limit), 6=d6 (beyond limit)
        let mut current = skills_dir.clone();
        for i in 0..=MAX_SKILL_WALK_DEPTH + 1 {
            current = current.join(format!("d{i}"));
        }
        write_skill_md(&current.join("deep-skill"), "deep-skill");

        // Also put one at an accessible depth
        write_skill_md(&skills_dir.join("shallow"), "shallow");

        let mut paths = Vec::new();
        walk_for_skill_md(&skills_dir, &mut paths, 0);

        // shallow should be found, deep-skill should not (too deep)
        assert_eq!(paths.len(), 1);
        assert!(paths[0].display().to_string().contains("shallow"));
    }

    #[test]
    fn find_skill_paths_parent_and_child_both_have_skill_md() {
        // A directory has SKILL.md and also has subdirectories with SKILL.md
        let tmp = tempfile::tempdir().unwrap();
        let echo_agent_dir = tmp.path().join(".echo-agent");
        let skills = echo_agent_dir.join("skills");

        // Parent skill
        write_skill_md(&skills.join("parent"), "parent-skill");
        // Child skill inside parent
        write_skill_md(&skills.join("parent").join("child"), "child-skill");

        let paths = find_skill_paths(&echo_agent_dir);
        assert_eq!(paths.len(), 2);

        let path_strs: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert!(path_strs.iter().any(|p| p.contains("parent/SKILL.md")));
        assert!(path_strs.iter().any(|p| p.contains("child/SKILL.md")));
    }

    // ── extract_first_paragraph ──────────────────────────────────────

    #[test]
    fn first_paragraph_simple() {
        let body = "This is the first paragraph.\n\nThis is the second.";
        assert_eq!(
            extract_first_paragraph(body).unwrap(),
            "This is the first paragraph."
        );
    }

    #[test]
    fn first_paragraph_skips_headings() {
        let body = "# Git Commit Skill\n\nReview staged changes and create a commit.\n\n## Steps";
        assert_eq!(
            extract_first_paragraph(body).unwrap(),
            "Review staged changes and create a commit."
        );
    }

    #[test]
    fn first_paragraph_multiline() {
        let body =
            "# Skill\n\nFirst line of paragraph.\nSecond line of paragraph.\n\nAnother paragraph.";
        assert_eq!(
            extract_first_paragraph(body).unwrap(),
            "First line of paragraph. Second line of paragraph."
        );
    }

    #[test]
    fn first_paragraph_empty_body() {
        assert!(extract_first_paragraph("").is_none());
    }

    #[test]
    fn first_paragraph_headings_only() {
        let body = "# Title\n\n## Section\n\n### Subsection";
        assert!(extract_first_paragraph(body).is_none());
    }

    // ── UTF-8 safe body truncation ──────────────────────────────────

    #[test]
    fn description_fallback_does_not_panic_on_multibyte_boundary() {
        // Build a body longer than MAX_BODY_PEEK_BYTES (2048) with multibyte
        // characters near the cutoff so the old &body[..2048] would land in
        // the middle of a multi-byte char and panic.
        //
        // Strategy: fill with ASCII up to near the limit, then pack 4-byte
        // emoji right at the boundary.
        let prefix = "# Heading\n\n";
        let filler_len = MAX_BODY_PEEK_BYTES - prefix.len() - 4; // leave room for emoji at boundary
        let filler = "a".repeat(filler_len);
        // Each emoji is 4 bytes. Place several so one straddles the 2048 mark.
        let emoji_run = "\u{1F600}".repeat(10); // 40 bytes of emoji
        let body = format!("{prefix}{filler}{emoji_run}");
        assert!(body.len() > MAX_BODY_PEEK_BYTES, "body must exceed limit");

        // This must not panic when truncated to MAX_BODY_PEEK_BYTES.
        let mut end = MAX_BODY_PEEK_BYTES;
        while end > 0 && !body.is_char_boundary(end) {
            end -= 1;
        }
        let peek = &body[..end];
        let result = extract_first_paragraph(peek);
        // Should produce a paragraph (the long filler + some emoji).
        assert!(result.is_some());
    }

    #[test]
    fn description_fallback_end_to_end_with_multibyte_skill_file() {
        // End-to-end: a SKILL.md with no description in frontmatter falls
        // back to body parsing. The body contains multibyte text that would
        // cross the MAX_BODY_PEEK_BYTES boundary.
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("skills").join("emoji-skill");
        fs::create_dir_all(&skill_dir).unwrap();

        // Body (after frontmatter): heading + paragraph with multibyte chars
        // exceeding 2048 bytes.
        let long_paragraph = "\u{00E9}".repeat(MAX_BODY_PEEK_BYTES); // 2-byte chars
        let content = format!("---\nname: emoji-skill\n---\n# Test\n\n{long_paragraph}\n");
        fs::write(skill_dir.join("SKILL.md"), &content).unwrap();

        let skills = parse_skill_files(vec![(skill_dir.join("SKILL.md"), SkillScope::Local)]);

        // Must not panic and should produce a description.
        assert_eq!(skills.len(), 1);
        assert!(
            !skills[0].description.is_empty(),
            "description should be filled from body"
        );
    }

    // ── Frontmatter parsing (existing coverage + regression) ─────────

    #[test]
    fn parse_valid_frontmatter() {
        let content = "---\nname: my-skill\ndescription: A skill\n---\n\nBody.\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(parsed.name, "my-skill");
        assert_eq!(parsed.description, "A skill");
    }

    #[test]
    fn parse_allowed_tools_comma_string() {
        let content = "---\nname: my-skill\ndescription: test\nallowed-tools: \"bash, read_file, grep\"\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(
            parsed.allowed_tools.as_deref(),
            Some(
                [
                    "bash".to_string(),
                    "read_file".to_string(),
                    "grep".to_string()
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn parse_allowed_tools_yaml_list() {
        let content = "---\nname: my-skill\ndescription: test\nallowed-tools:\n  - bash\n  - read_file\n  - grep\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(
            parsed.allowed_tools.as_deref(),
            Some(
                [
                    "bash".to_string(),
                    "read_file".to_string(),
                    "grep".to_string()
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn parse_allowed_tools_omitted() {
        let content = "---\nname: my-skill\ndescription: test\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert!(parsed.allowed_tools.is_none());
    }

    #[test]
    fn parse_model_and_effort() {
        let content =
            "---\nname: my-skill\ndescription: test\nmodel: echo-agent-3\neffort: high\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(parsed.model.as_deref(), Some("echo-agent-3"));
        assert_eq!(parsed.effort.as_deref(), Some("high"));
    }

    #[test]
    fn parse_model_and_effort_omitted() {
        let content = "---\nname: my-skill\ndescription: test\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert!(parsed.model.is_none());
        assert!(parsed.effort.is_none());
    }

    // ── agentskills.io spec parity ────────────────────────────────

    #[test]
    fn parse_license_and_compatibility() {
        let content = "---\nname: my-skill\ndescription: test\nlicense: Apache-2.0\ncompatibility: Requires git and docker\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(parsed.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(
            parsed.compatibility.as_deref(),
            Some("Requires git and docker")
        );
    }

    #[test]
    fn parse_license_and_compatibility_omitted() {
        let content = "---\nname: my-skill\ndescription: test\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert!(parsed.license.is_none());
        assert!(parsed.compatibility.is_none());
    }

    #[test]
    fn parse_metadata_arbitrary_keys() {
        let content = "---\nname: my-skill\ndescription: test\nmetadata:\n  author: example-org\n  version: \"1.0\"\n  short-description: Short desc\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(parsed.short_description.as_deref(), Some("Short desc"));
        assert_eq!(parsed.author.as_deref(), Some("example-org"));
        let meta = parsed.metadata.unwrap();
        assert_eq!(meta.get("version").unwrap(), "1.0");
        // short-description and author are extracted to top-level fields and not in the generic map
        assert!(!meta.contains_key("short-description"));
        assert!(!meta.contains_key("author"));
    }

    #[test]
    fn parse_metadata_omitted() {
        let content = "---\nname: my-skill\ndescription: test\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert!(parsed.short_description.is_none());
        assert!(parsed.metadata.is_none());
    }

    #[test]
    fn name_validation_rejects_leading_hyphen() {
        assert!(!is_valid_skill_name("-pdf"));
    }

    #[test]
    fn name_validation_rejects_trailing_hyphen() {
        assert!(!is_valid_skill_name("pdf-"));
    }

    #[test]
    fn name_validation_rejects_consecutive_hyphens() {
        assert!(!is_valid_skill_name("pdf--tool"));
    }

    #[test]
    fn name_validation_accepts_valid_names() {
        assert!(is_valid_skill_name("pdf-processing"));
        assert!(is_valid_skill_name("data-analysis"));
        assert!(is_valid_skill_name("a"));
        assert!(is_valid_skill_name("a-b-c"));
        assert!(is_valid_skill_name("tool123"));
    }

    #[test]
    fn normalize_replaces_spaces_with_hyphens() {
        assert_eq!(normalize_skill_name("my cool skill"), "my-cool-skill");
    }

    #[test]
    fn normalize_lowercases_and_collapses_hyphens() {
        assert_eq!(normalize_skill_name("My  Cool  Skill"), "my-cool-skill");
    }

    #[test]
    fn normalize_trims_leading_trailing() {
        assert_eq!(normalize_skill_name(" -my-skill- "), "my-skill");
    }

    #[test]
    fn parse_frontmatter_normalizes_spaced_name() {
        let content = "---\nname: my cool skill\ndescription: A skill\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(parsed.name, "my-cool-skill");
    }

    #[test]
    fn parse_skill_files_no_frontmatter_uses_dir_name() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("skills").join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "Just body content, no frontmatter.",
        )
        .unwrap();

        let skills = parse_skill_files(vec![(skill_dir.join("SKILL.md"), SkillScope::Local)]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
        assert!(
            skills[0].user_invocable,
            "no-frontmatter skills must be user-invocable"
        );
    }

    #[test]
    fn parse_allowed_tools_space_delimited() {
        // agentskills.io format: space-delimited with tool patterns
        let content = "---\nname: my-skill\ndescription: test\nallowed-tools: \"Bash(git:*) Read Write\"\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(
            parsed.allowed_tools.as_deref(),
            Some(
                [
                    "Bash(git:*)".to_string(),
                    "Read".to_string(),
                    "Write".to_string()
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn parse_full_spec_plus_extensions() {
        // Mixed agentskills.io spec fields + our extensions — all must parse.
        let content = "---\nname: my-skill\ndescription: A full skill\nlicense: MIT\ncompatibility: Python 3.12+\nmetadata:\n  author: test-org\n  version: \"2.0\"\nallowed-tools:\n  - bash\n  - read_file\nargument-hint: file path\nmodel: echo-agent-3\neffort: high\nuser-invocable: true\ndisable-model-invocation: false\n---\nBody content.\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(parsed.name, "my-skill");
        assert_eq!(parsed.description, "A full skill");
        assert_eq!(parsed.license.as_deref(), Some("MIT"));
        assert_eq!(parsed.compatibility.as_deref(), Some("Python 3.12+"));
        assert_eq!(parsed.author.as_deref(), Some("test-org"));
        assert_eq!(
            parsed.allowed_tools.as_deref(),
            Some(["bash".to_string(), "read_file".to_string()].as_slice())
        );
        assert_eq!(parsed.argument_hint.as_deref(), Some("file path"));
        assert_eq!(parsed.model.as_deref(), Some("echo-agent-3"));
        assert_eq!(parsed.effort.as_deref(), Some("high"));
        assert!(parsed.user_invocable);
        assert!(!parsed.disable_model_invocation);
    }

    #[test]
    fn parse_frontmatter_recovers_colon_in_value() {
        let content = "---\nname: my-skill\ndescription: lorem ipsum: dolor sit amet\n---\n";
        let parsed = parse_skill_frontmatter(content, None).unwrap();
        assert_eq!(parsed.name, "my-skill");
        assert_eq!(parsed.description, "lorem ipsum: dolor sit amet");
    }

    #[test]
    fn parse_frontmatter_special_chars_normalized() {
        // Non-slug chars (e.g. `@`, `!`, `.`) normalize to hyphens so the skill
        // is kept and slash-usable, rather than dropped.
        let parsed =
            parse_skill_frontmatter("---\nname: inv@lid!name\ndescription: A\n---\n", None)
                .unwrap();
        assert_eq!(parsed.name, "inv-lid-name");
    }

    #[test]
    fn parse_frontmatter_all_symbol_name_rejected() {
        // A name that normalizes to empty has nothing usable → still rejected.
        assert!(matches!(
            parse_skill_frontmatter("---\nname: \"@!#\"\ndescription: A\n---\n", None),
            Err(SkillParseError::InvalidName(_))
        ));
    }

    // ── Feature 1: Workspace user skills via list_skills ─────────────

    /// Helper: initialize a bare git repo at `path` so git2::Repository::discover works.
    fn init_git_repo(path: &Path) {
        git2::Repository::init(path).unwrap();
    }

    #[tokio::test]
    async fn list_skills_includes_workspace_user_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        // Create workspace user dir with a skill
        let user_dir = repo_root.join("x").join("testuser");
        write_skill_md(
            &user_dir.join(".echo-agent").join("skills").join("my-tool"),
            "my-tool",
        );

        // cwd = repo root (not inside user dir, so the walk won't find it)
        let skills = list_skills_with_options(
            Some(repo_root.to_str().unwrap()),
            Some(&user_dir),
            tmp.path(),
            CompatConfig::default(),
        )
        .await;

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"my-tool"),
            "Expected 'my-tool' in skills, got: {names:?}"
        );
    }

    #[tokio::test]
    async fn list_skills_workspace_user_dedup_when_cwd_inside_user_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        // User dir with a skill
        let user_dir = repo_root.join("x").join("testuser");
        write_skill_md(
            &user_dir
                .join(".echo-agent")
                .join("skills")
                .join("dedup-skill"),
            "dedup-skill",
        );

        // cwd is inside the user dir — the upward walk will already find it
        let skills = list_skills_with_options(
            Some(user_dir.to_str().unwrap()),
            Some(&user_dir),
            tmp.path(),
            CompatConfig::default(),
        )
        .await;

        // Should appear exactly once (deduped by canonical path)
        let count = skills.iter().filter(|s| s.name == "dedup-skill").count();
        assert_eq!(count, 1, "Skill should appear exactly once, got {count}");
    }

    #[tokio::test]
    async fn list_skills_no_workspace_user_dir_no_extra_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        // Create a skill that would only be found via workspace user path
        let user_dir = repo_root.join("x").join("ghost");
        write_skill_md(
            &user_dir
                .join(".echo-agent")
                .join("skills")
                .join("ghost-skill"),
            "ghost-skill",
        );

        // Pass None — simulates env vars not set
        let skills = list_skills_with_options(
            Some(repo_root.to_str().unwrap()),
            None,
            tmp.path(),
            CompatConfig::default(),
        )
        .await;

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            !names.contains(&"ghost-skill"),
            "Without workspace user dir, ghost-skill should not be found"
        );
    }

    #[tokio::test]
    async fn list_skills_workspace_user_dir_with_recursive_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        // User dir with nested skills
        let user_dir = repo_root.join("x").join("nested-user");
        let skills_base = user_dir.join(".echo-agent").join("skills");
        write_skill_md(&skills_base.join("flat-skill"), "flat-skill");
        write_skill_md(&skills_base.join("team").join("deep-skill"), "deep-skill");

        let skills = list_skills_with_options(
            Some(repo_root.to_str().unwrap()),
            Some(&user_dir),
            tmp.path(),
            CompatConfig::default(),
        )
        .await;

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"flat-skill"),
            "flat-skill not found: {names:?}"
        );
        assert!(
            names.contains(&"deep-skill"),
            "deep-skill not found: {names:?}"
        );
    }

    // ── collect_config_skills ────────────────────────────────────────

    #[test]
    fn collect_config_skills_from_directory() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill_md(&tmp.path().join("alpha"), "alpha");
        write_skill_md(&tmp.path().join("beta"), "beta");

        let paths = vec![tmp.path().to_str().unwrap().to_string()];
        let skills = collect_config_skills(&paths, None);

        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "alpha not found: {names:?}");
        assert!(names.contains(&"beta"), "beta not found: {names:?}");
    }

    #[test]
    fn collect_config_skills_direct_skill_md_file() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        write_skill_md(&skill_dir, "my-skill");

        let paths = vec![skill_dir.join("SKILL.md").to_str().unwrap().to_string()];
        let skills = collect_config_skills(&paths, None);

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "my-skill");
    }

    #[test]
    fn collect_config_skills_scope_user_outside_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&repo).unwrap();
        write_skill_md(&outside.join("ext-skill"), "ext-skill");

        let paths = vec![outside.to_str().unwrap().to_string()];
        let skills = collect_config_skills(&paths, Some(&repo));

        assert_eq!(skills[0].scope, SkillScope::User);
    }

    #[test]
    fn collect_config_skills_scope_repo_inside_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let inside = repo.join("tools");
        write_skill_md(&inside.join("repo-skill"), "repo-skill");

        let paths = vec![inside.to_str().unwrap().to_string()];
        let skills = collect_config_skills(&paths, Some(&repo));

        assert_eq!(skills[0].scope, SkillScope::Repo);
    }

    #[test]
    fn collect_config_skills_skill_md_at_root_of_config_path() {
        // When the config path itself is a skill directory (contains SKILL.md),
        // it should be discovered even though walk_for_skill_md only walks children.
        let tmp = tempfile::tempdir().unwrap();
        write_skill_md(tmp.path(), "root-skill");

        let paths = vec![tmp.path().to_str().unwrap().to_string()];
        let skills = collect_config_skills(&paths, None);

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "root-skill");
    }

    #[test]
    fn collect_config_skills_nonexistent_path_is_skipped() {
        let paths = vec!["/nonexistent/path/to/skills".to_string()];
        let skills = collect_config_skills(&paths, None);
        assert!(skills.is_empty());
    }

    #[test]
    fn collect_config_skills_deduplicates_same_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill_md(&tmp.path().join("dup-skill"), "dup-skill");

        // Same directory listed twice
        let dir = tmp.path().to_str().unwrap().to_string();
        let paths = vec![dir.clone(), dir];
        let skills = collect_config_skills(&paths, None);

        assert_eq!(skills.len(), 1, "Same file should not appear twice");
    }

    #[test]
    fn collect_config_skills_stamps_config_toml_source() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill_md(&tmp.path().join("cfg-skill"), "cfg-skill");

        let paths = vec![tmp.path().to_str().unwrap().to_string()];
        let skills = collect_config_skills(&paths, None);

        assert_eq!(skills.len(), 1);
        match &skills[0].config_source {
            Some(echo_agent_tools::types::config_source::ConfigSource::ConfigToml { path }) => {
                assert_eq!(path, Path::new(&skills[0].path));
            }
            other => panic!("expected ConfigToml source, got {other:?}"),
        }
    }

    // ── filter_skills ────────────────────────────────────────────────

    fn make_skill(name: &str, path: &str) -> SkillInfo {
        SkillInfo {
            name: name.to_string(),
            display_name: None,
            description: format!("desc for {name}"),
            when_to_use: None,
            short_description: None,
            author: None,
            argument_hint: None,
            path: path.to_string(),
            scope: SkillScope::User,
            config_source: None,
            plugin_name: None,
            plugin_version: None,
            plugin_root: None,
            plugin_data: None,
            allowed_tools: None,
            license: None,
            compatibility: None,
            metadata: None,
            model: None,
            effort: None,
            user_invocable: true,
            disable_model_invocation: false,
            has_user_specified_description: false,
            paths: None,
            enabled: true,
            body: None,
        }
    }

    #[test]
    fn stamp_plugin_fields_sets_root_and_data() {
        use crate::plugins::discovery::PluginId;
        use crate::plugins::registry::LoadedPlugin;

        let root = PathBuf::from("/tmp/plugin-dev");
        let plugin = LoadedPlugin {
            name: "plugin-dev".to_string(),
            id: PluginId::new(PluginScope::User, &root, "plugin-dev"),
            root: root.clone(),
            canonical_root: root.clone(),
            scope: PluginScope::User,
            origin: crate::plugins::PluginOrigin::UserEchoAgent,
            trusted: true,
            enabled: true,
            version: Some("1.0.0".to_string()),
            description: None,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: None,
            lsp_config_path: None,
            skill_count: 0,
            agent_count: 0,
            skill_names: vec![],
            agent_names: vec![],
            has_hooks: false,
            hook_count: 0,
            has_inline_hooks_only: false,
            mcp_server_count: 0,
            has_inline_mcp_only: false,
            lsp_server_count: 0,
            has_inline_lsp_only: false,
            inline_hooks: None,
            inline_mcp_servers: None,
            inline_lsp_servers: None,
            conflict: None,
        };

        let mut skills = vec![make_skill(
            "indexer",
            "/tmp/plugin-dev/skills/indexer/SKILL.md",
        )];
        stamp_plugin_fields(&mut skills, &plugin);

        let expected_root = plugin.root_str();
        let expected_data = plugin.data_dir_str();
        assert_eq!(
            skills[0].plugin_root.as_deref(),
            Some(expected_root.as_str())
        );
        assert_eq!(
            skills[0].plugin_data.as_deref(),
            Some(expected_data.as_str())
        );
        assert_eq!(skills[0].plugin_name.as_deref(), Some("plugin-dev"));
    }

    // ── Manifest `skills` entries pointing directly at skill dirs ──

    fn make_registry_with_skill_dirs(
        name: &str,
        root: &Path,
        skill_dirs: Vec<PathBuf>,
    ) -> crate::plugins::PluginRegistry {
        use crate::plugins::discovery::{DiscoveredPlugin, PluginId};
        use crate::plugins::manifest::PluginManifest;

        let dp = DiscoveredPlugin {
            manifest: PluginManifest {
                name: name.to_string(),
                version: Some("0.1.0".to_string()),
                description: None,
                author: None,
                homepage: None,
                repository: None,
                license: None,
                keywords: vec![],
                skills: None,
                commands: None,
                agents: None,
                hooks: None,
                mcp_servers: None,
                lsp_servers: None,
            },
            id: PluginId::new(PluginScope::User, root, name),
            root: root.to_path_buf(),
            canonical_root: root.to_path_buf(),
            scope: PluginScope::User,
            origin: crate::plugins::PluginOrigin::UserEchoAgent,
            trusted: true,
            skill_dirs,
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: None,
            lsp_config_path: None,
            conflict: None,
        };
        crate::plugins::PluginRegistry::from_discovered(vec![dp], &[], &[name.to_string()])
    }

    #[test]
    fn collect_plugin_skills_finds_root_level_skill_md() {
        // Manifest style: "skills": ["skills/one", "skills/two"] — each entry
        // IS a skill directory with SKILL.md at its root.
        let tmp = tempfile::tempdir().unwrap();
        let one = tmp.path().join("skills").join("one");
        let two = tmp.path().join("skills").join("two");
        write_skill_md(&one, "one");
        write_skill_md(&two, "two");

        let registry =
            make_registry_with_skill_dirs("listed", tmp.path(), vec![one.clone(), two.clone()]);
        let skills = collect_plugin_skills(&registry);

        assert_eq!(skills.len(), 2, "root-level SKILL.md dirs must load");
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"one") && names.contains(&"two"),
            "{names:?}"
        );
        assert!(
            skills
                .iter()
                .all(|s| s.plugin_name.as_deref() == Some("listed"))
        );
    }

    #[test]
    fn collect_plugin_skills_parent_dir_unchanged_and_no_double_count() {
        // Convention style (parent dir) still works, and listing both the
        // parent and a child dir does not yield duplicates after merge.
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("skills");
        let child = parent.join("one");
        write_skill_md(&child, "one");

        let registry =
            make_registry_with_skill_dirs("mixed", tmp.path(), vec![parent.clone(), child.clone()]);
        let merged = merge_skills_with_plugins(vec![], collect_plugin_skills(&registry));

        let ones: Vec<_> = merged.iter().filter(|s| s.name == "one").collect();
        assert_eq!(ones.len(), 1, "skill must appear exactly once: {merged:?}");
    }

    #[test]
    fn filter_skills_empty_ignore_returns_all() {
        let skills = vec![
            make_skill("a", "/some/path/a/SKILL.md"),
            make_skill("b", "/some/path/b/SKILL.md"),
        ];
        let result = filter_skills(skills.clone(), &[]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_skills_removes_exact_path_match() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("bad-skill");
        write_skill_md(&skill_dir, "bad-skill");
        let skill_path = skill_dir.join("SKILL.md");

        let skills = vec![
            make_skill("good-skill", "/other/SKILL.md"),
            make_skill("bad-skill", skill_path.to_str().unwrap()),
        ];
        let ignore = vec![skill_path.to_str().unwrap().to_string()];
        let result = filter_skills(skills, &ignore);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "good-skill");
    }

    #[test]
    fn filter_skills_removes_directory_prefix_match() {
        let tmp = tempfile::tempdir().unwrap();
        let ignored_dir = tmp.path().join("ignored");
        write_skill_md(&ignored_dir.join("skill-a"), "skill-a");
        write_skill_md(&ignored_dir.join("skill-b"), "skill-b");

        let skills = vec![
            make_skill(
                "skill-a",
                ignored_dir
                    .join("skill-a")
                    .join("SKILL.md")
                    .to_str()
                    .unwrap(),
            ),
            make_skill(
                "skill-b",
                ignored_dir
                    .join("skill-b")
                    .join("SKILL.md")
                    .to_str()
                    .unwrap(),
            ),
            make_skill("keeper", "/other/keeper/SKILL.md"),
        ];
        let ignore = vec![ignored_dir.to_str().unwrap().to_string()];
        let result = filter_skills(skills, &ignore);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "keeper");
    }

    #[tokio::test]
    async fn list_skills_loads_custom_path_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        // A skill in a custom directory outside the repo
        let custom_dir = tmp.path().join("custom-skills");
        write_skill_md(&custom_dir.join("custom-skill"), "custom-skill");

        let config = SkillsConfig {
            paths: vec![custom_dir.to_str().unwrap().to_string()],
            ignore: vec![],
            disabled: vec![],
            server_skill_dirs: vec![],
            server_enforced_skill_names: vec![],
            bundled_skill_dirs: vec![],
        };

        let skills = list_skills(
            Some(repo_root.to_str().unwrap()),
            &config,
            CompatConfig::default(),
        )
        .await;
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"custom-skill"),
            "custom-skill not found: {names:?}"
        );
    }

    #[tokio::test]
    async fn list_skills_ignore_filters_custom_path_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        let custom_dir = tmp.path().join("custom-skills");
        write_skill_md(&custom_dir.join("wanted"), "wanted");
        write_skill_md(&custom_dir.join("unwanted"), "unwanted");
        let unwanted_path = custom_dir.join("unwanted");

        let config = SkillsConfig {
            paths: vec![custom_dir.to_str().unwrap().to_string()],
            ignore: vec![unwanted_path.to_str().unwrap().to_string()],
            disabled: vec![],
            server_skill_dirs: vec![],
            server_enforced_skill_names: vec![],
            bundled_skill_dirs: vec![],
        };

        let skills = list_skills(
            Some(repo_root.to_str().unwrap()),
            &config,
            CompatConfig::default(),
        )
        .await;
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"wanted"), "wanted not found: {names:?}");
        assert!(
            !names.contains(&"unwanted"),
            "unwanted should be filtered: {names:?}"
        );
    }

    #[tokio::test]
    async fn list_skills_deduplicates_auto_and_config_overlap() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        let auto_dir = repo_root
            .join(".echo-agent")
            .join("skills")
            .join("dup-skill");
        write_skill_md(&auto_dir, "dup-skill");

        // Add the same auto-discovered skills root as a config path.
        let config = SkillsConfig {
            paths: vec![
                repo_root
                    .join(".echo-agent")
                    .join("skills")
                    .to_str()
                    .unwrap()
                    .to_string(),
            ],
            ignore: vec![],
            disabled: vec![],
            server_skill_dirs: vec![],
            server_enforced_skill_names: vec![],
            bundled_skill_dirs: vec![],
        };

        let skills = list_skills(
            Some(repo_root.to_str().unwrap()),
            &config,
            CompatConfig::default(),
        )
        .await;
        let count = skills.iter().filter(|s| s.name == "dup-skill").count();

        assert_eq!(count, 1, "dup-skill should only be loaded once");
    }

    /// A skill reachable via auto-discovery AND `[skills].paths` is genuinely
    /// both: the auto-discovered copy wins (scope unchanged) but inherits the
    /// ConfigToml stamp, so the label doesn't depend on source order.
    #[tokio::test]
    async fn list_skills_auto_and_config_overlap_keeps_config_toml_source() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        let auto_dir = repo_root
            .join(".echo-agent")
            .join("skills")
            .join("overlap-skill");
        write_skill_md(&auto_dir, "overlap-skill");

        let config = SkillsConfig {
            paths: vec![
                repo_root
                    .join(".echo-agent")
                    .join("skills")
                    .to_str()
                    .unwrap()
                    .to_string(),
            ],
            ..Default::default()
        };

        let skills = list_skills(
            Some(repo_root.to_str().unwrap()),
            &config,
            CompatConfig::default(),
        )
        .await;

        let overlaps: Vec<&SkillInfo> = skills
            .iter()
            .filter(|s| s.name == "overlap-skill")
            .collect();
        assert_eq!(
            overlaps.len(),
            1,
            "overlap-skill should only be loaded once"
        );
        assert_eq!(
            overlaps[0].scope,
            SkillScope::Local,
            "auto-discovered scope must win"
        );
        assert!(
            matches!(
                overlaps[0].config_source,
                Some(echo_agent_tools::types::config_source::ConfigSource::ConfigToml { .. })
            ),
            "ConfigToml stamp should survive path-dedupe: {:?}",
            overlaps[0].config_source
        );
    }

    /// Name-dedupe drops a *different* file that shares a name; its stamp must
    /// not leak onto the winner (they are genuinely different skills).
    #[test]
    fn dedupe_skills_name_collision_does_not_propagate_config_source() {
        let winner = make_skill("same-name", "/some/path/a/SKILL.md");
        let mut loser = make_skill("same-name", "/some/path/b/SKILL.md");
        loser.config_source = Some(
            echo_agent_tools::types::config_source::ConfigSource::ConfigToml {
                path: PathBuf::from("/some/path/b/SKILL.md"),
            },
        );

        let deduped = dedupe_skills(vec![winner, loser]);

        // Same-scope siblings both survive (the collision loser is re-keyed
        // to its dir basename); provenance must stay with its own file.
        assert_eq!(deduped.len(), 2);
        assert!(
            deduped[0].config_source.is_none(),
            "name-dedupe must not propagate provenance across different files"
        );
        assert_eq!(deduped[1].name, "b");
        assert!(
            deduped[1].config_source.is_some(),
            "re-keyed sibling keeps its own provenance"
        );
    }

    #[tokio::test]
    async fn list_skills_ignore_allows_lower_priority_same_name_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        let cwd = repo_root.join("work").join("nested");
        fs::create_dir_all(&cwd).unwrap();
        init_git_repo(&repo_root);

        // Same skill name in local (higher-priority) and repo (lower-priority) sources.
        write_skill_md(&cwd.join(".echo-agent").join("skills").join("same"), "same");
        let repo_skill_dir = repo_root.join(".echo-agent").join("skills").join("same");
        write_skill_md(&repo_skill_dir, "same");

        // Ignore the local skill path. Repo fallback should remain visible.
        let config = SkillsConfig {
            paths: vec![],
            ignore: vec![
                cwd.join(".echo-agent")
                    .join("skills")
                    .to_str()
                    .unwrap()
                    .to_string(),
            ],
            disabled: vec![],
            server_skill_dirs: vec![],
            server_enforced_skill_names: vec![],
            bundled_skill_dirs: vec![],
        };

        let skills = list_skills(
            Some(cwd.to_str().unwrap()),
            &config,
            CompatConfig::default(),
        )
        .await;
        let same_skills: Vec<&SkillInfo> = skills.iter().filter(|s| s.name == "same").collect();

        assert_eq!(
            same_skills.len(),
            1,
            "Expected repo fallback skill after local ignore"
        );
        assert!(
            same_skills[0]
                .path
                .starts_with(repo_skill_dir.to_str().unwrap()),
            "Expected fallback from repo path, got: {}",
            same_skills[0].path
        );
    }

    // discover_skills_for_paths and dedup_by_canonical_path tests removed --
    // these functions now live in echo-agent-tools::implementations::skills::discovery
    // and echo-agent-tools::types::skill_discovery_tracker, tested there.

    // ── Disabled skills marking ─────────────────────────────────────

    #[tokio::test]
    async fn disabled_config_marks_skill_enabled_false() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        write_skill_md(
            &repo_root.join(".echo-agent").join("skills").join("commit"),
            "commit",
        );
        write_skill_md(
            &repo_root.join(".echo-agent").join("skills").join("review"),
            "review",
        );

        let config = SkillsConfig {
            paths: vec![],
            ignore: vec![],
            disabled: vec!["commit".to_string()],
            server_skill_dirs: vec![],
            server_enforced_skill_names: vec![],
            bundled_skill_dirs: vec![],
        };
        let skills = list_skills(
            Some(repo_root.to_str().unwrap()),
            &config,
            CompatConfig::default(),
        )
        .await;

        let commit = skills.iter().find(|s| s.name == "commit");
        let review = skills.iter().find(|s| s.name == "review");

        assert!(
            commit.is_some(),
            "disabled skill should still appear in list"
        );
        assert!(
            !commit.unwrap().enabled,
            "disabled skill should have enabled=false"
        );
        assert!(review.is_some());
        assert!(
            review.unwrap().enabled,
            "non-disabled skill should have enabled=true"
        );
    }

    #[tokio::test]
    async fn disabled_config_empty_leaves_all_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        write_skill_md(
            &repo_root.join(".echo-agent").join("skills").join("deploy"),
            "deploy",
        );

        let config = SkillsConfig {
            paths: vec![],
            ignore: vec![],
            disabled: vec![],
            server_skill_dirs: vec![],
            server_enforced_skill_names: vec![],
            bundled_skill_dirs: vec![],
        };
        let skills = list_skills(
            Some(repo_root.to_str().unwrap()),
            &config,
            CompatConfig::default(),
        )
        .await;
        assert!(
            skills.iter().all(|s| s.enabled),
            "all skills should be enabled when disabled list is empty"
        );
    }

    // ── Bundled skills discovery ─────────────────────────────────────

    #[tokio::test]
    async fn bundled_skills_are_discovered() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        // Place a skill under <home>/bundled/skills/commit/SKILL.md
        write_skill_md(
            &home.join("bundled").join("skills").join("commit"),
            "commit",
        );

        let skills = list_skills_with_options(
            Some(repo_root.to_str().unwrap()),
            None,
            &home,
            CompatConfig::default(),
        )
        .await;
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"commit"),
            "Expected bundled 'commit' skill, got: {names:?}"
        );
    }

    #[tokio::test]
    async fn user_skills_shadow_bundled_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        // User skill at <home>/skills/commit/SKILL.md
        write_skill_md(&home.join("skills").join("commit"), "commit");

        // Bundled skill at <home>/bundled/skills/commit/SKILL.md (different body)
        let bundled_skill_dir = home.join("bundled").join("skills").join("commit");
        fs::create_dir_all(&bundled_skill_dir).unwrap();
        fs::write(
            bundled_skill_dir.join("SKILL.md"),
            "---\nname: commit\ndescription: bundled version\n---\nBundled body.\n",
        )
        .unwrap();

        let raw = list_skills_with_options(
            Some(repo_root.to_str().unwrap()),
            None,
            &home,
            CompatConfig::default(),
        )
        .await;

        // Both are discovered at the list_skills_with_options level (different canonical paths)
        assert_eq!(
            raw.iter().filter(|s| s.name == "commit").count(),
            2,
            "Expected exactly 2 'commit' skills before dedup (user + bundled)"
        );
        // User skill appears before bundled (first-seen-wins ordering)
        let first_commit = raw.iter().find(|s| s.name == "commit").unwrap();
        assert!(
            !first_commit.path.contains("/bundled/"),
            "User skill should appear before bundled: {}",
            first_commit.path
        );

        // After name-based dedup (as list_skills_with_plugins does), only user version survives
        let deduped = dedupe_skills(raw);
        let commit_skills: Vec<&SkillInfo> =
            deduped.iter().filter(|s| s.name == "commit").collect();
        assert_eq!(
            commit_skills.len(),
            1,
            "Expected exactly one 'commit' after dedup, got {}",
            commit_skills.len()
        );
        assert!(
            !commit_skills[0].path.contains("/bundled/"),
            "User skill should win over bundled: {}",
            commit_skills[0].path
        );
    }

    // ── Command file discovery ────────────────────────────────────────

    /// Regression: project `.claude/commands` often sits under a full `.claude/**`
    /// gitignore with only `!.claude/skills/**` re-included (local-only vendor
    /// config). User-scoped `~/.claude/commands` still loaded; project commands
    /// did not — so `/frontend` never appeared for the large multi-package repo.
    #[tokio::test]
    async fn project_claude_commands_load_even_when_gitignored() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).expect("create repo dir");
        init_git_repo(&repo_root);

        // Mirror the multi-package-repo-style ignore: ignore all of .claude, re-include skills only.
        fs::write(
            repo_root.join(".gitignore"),
            "**/.claude\n**/.claude/**\n!.claude/\n!.claude/skills/\n!.claude/skills/**\n",
        )
        .expect("write gitignore");

        let commands = repo_root.join(".claude").join("commands");
        fs::create_dir_all(&commands).expect("create commands dir");
        fs::write(
            commands.join("frontend.md"),
            "---\nname: frontend\ndescription: Acme Design System Frontend Skill\n---\nUse the design system.\n",
        )
        .expect("write frontend.md");

        // Skill under the force-included path must still load too.
        write_skill_md(
            &repo_root.join(".claude").join("skills").join("bp-deltas"),
            "bp-deltas",
        );

        let repo_str = repo_root.to_str().unwrap_or_default();
        let skills =
            list_skills_with_options(Some(repo_str), None, tmp.path(), CompatConfig::default())
                .await;
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();

        assert!(
            names.contains(&"frontend"),
            "gitignored project .claude/commands/frontend.md must load as a slash skill, got: {names:?}"
        );
        assert!(
            names.contains(&"bp-deltas"),
            "project .claude/skills still loads, got: {names:?}"
        );

        let frontend = skills
            .iter()
            .find(|s| s.name == "frontend")
            .expect("frontend skill");
        assert!(
            frontend.path.contains("commands"),
            "frontend should come from commands/, path={}",
            frontend.path
        );
    }

    #[test]
    fn command_file_name_derivation_with_and_without_frontmatter() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let commands = tmp.path().join("commands");
        fs::create_dir_all(&commands).expect("create commands dir");

        fs::write(
            commands.join("deploy.md"),
            "---\nname: deploy\ndescription: Ship it\n---\nBody.\n",
        )
        .expect("write deploy.md");
        fs::write(commands.join("rollback.md"), "Just rollback instructions.")
            .expect("write rollback.md");

        let files = vec![
            (commands.join("deploy.md"), SkillScope::Repo),
            (commands.join("rollback.md"), SkillScope::Repo),
        ];
        let skills = parse_skill_files(files);

        assert_eq!(skills.len(), 2);
        let deploy = skills
            .iter()
            .find(|s| s.name == "deploy")
            .expect("deploy skill not found");
        assert_eq!(deploy.description, "Ship it");

        let rollback = skills
            .iter()
            .find(|s| s.name == "rollback")
            .expect("rollback skill not found");
        assert_eq!(rollback.description, "Just rollback instructions.");
    }

    #[tokio::test]
    async fn skills_shadow_commands_with_same_name() {
        let tmp = tempfile::tempdir().expect("create tempdir");
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).expect("create repo dir");
        init_git_repo(&repo_root);

        let claude_dir = repo_root.join(".claude");
        write_skill_md(&claude_dir.join("skills").join("deploy"), "deploy");
        let commands = claude_dir.join("commands");
        fs::create_dir_all(&commands).expect("create commands dir");
        fs::write(
            commands.join("deploy.md"),
            "---\nname: deploy\ndescription: command version\n---\n",
        )
        .expect("write deploy.md command");

        let repo_str = repo_root.to_str().unwrap_or_default();
        let raw =
            list_skills_with_options(Some(repo_str), None, tmp.path(), CompatConfig::default())
                .await;

        let deploy_entries: Vec<_> = raw.iter().filter(|s| s.name == "deploy").collect();
        assert_eq!(deploy_entries.len(), 2);
        assert!(
            deploy_entries[0].path.contains("SKILL.md"),
            "skill should appear before command"
        );

        let deduped = dedupe_skills(raw);
        let deploy = deduped
            .iter()
            .filter(|s| s.name == "deploy")
            .collect::<Vec<_>>();
        assert_eq!(deploy.len(), 1);
        assert!(deploy[0].path.contains("SKILL.md"));
    }

    // ── Plugin skill identity ─────────────────────────────

    fn min_plugin(name: &str) -> crate::plugins::LoadedPlugin {
        use crate::plugins::discovery::PluginId;
        let root = PathBuf::from(format!("/tmp/{name}"));
        crate::plugins::LoadedPlugin {
            name: name.to_string(),
            id: PluginId::new(PluginScope::Project, &root, name),
            root: root.clone(),
            canonical_root: root,
            scope: PluginScope::Project,
            origin: crate::plugins::PluginOrigin::ProjectEchoAgent,
            trusted: true,
            enabled: true,
            version: Some("1.0.0".to_string()),
            description: None,
            skill_dirs: vec![],
            command_dirs: vec![],
            agent_dirs: vec![],
            hooks_path: None,
            mcp_config_path: None,
            lsp_config_path: None,
            skill_count: 0,
            agent_count: 0,
            skill_names: vec![],
            agent_names: vec![],
            has_hooks: false,
            hook_count: 0,
            has_inline_hooks_only: false,
            mcp_server_count: 0,
            has_inline_mcp_only: false,
            lsp_server_count: 0,
            has_inline_lsp_only: false,
            inline_hooks: None,
            inline_mcp_servers: None,
            inline_lsp_servers: None,
            conflict: None,
        }
    }

    #[test]
    fn stamp_plugin_fields_uses_dir_basename_as_identity() {
        // Siblings sharing the frontmatter name `deploy` must not collide.
        let mut skills = vec![
            SkillInfo {
                name: "deploy".to_owned(),
                path: "/p/skills/deploy-prod/SKILL.md".to_owned(),
                ..SkillInfo::default()
            },
            SkillInfo {
                name: "deploy".to_owned(),
                path: "/p/skills/deploy-staging/SKILL.md".to_owned(),
                ..SkillInfo::default()
            },
        ];
        stamp_plugin_fields(&mut skills, &min_plugin("infra"));

        assert_eq!(skills[0].name, "deploy-prod");
        assert_eq!(skills[0].display_name.as_deref(), Some("deploy"));
        assert_eq!(skills[0].dedup_key(), "infra:deploy-prod");
        assert_ne!(skills[0].dedup_key(), skills[1].dedup_key());
    }

    #[test]
    fn stamp_plugin_fields_normalizes_dir_basename() {
        let mut skills = vec![SkillInfo {
            name: "deploy".to_owned(),
            path: "/p/skills/Deploy_Prod/SKILL.md".to_owned(),
            ..SkillInfo::default()
        }];
        stamp_plugin_fields(&mut skills, &min_plugin("infra"));
        assert_eq!(skills[0].name, "deploy-prod");
        assert_eq!(skills[0].display_name.as_deref(), Some("deploy"));
    }

    #[test]
    fn stamp_plugin_fields_keeps_name_when_dir_matches() {
        let mut skills = vec![SkillInfo {
            name: "deploy".to_owned(),
            path: "/p/skills/deploy/SKILL.md".to_owned(),
            ..SkillInfo::default()
        }];
        stamp_plugin_fields(&mut skills, &min_plugin("infra"));

        assert_eq!(skills[0].name, "deploy");
        assert_eq!(skills[0].display_name, None);
        assert_eq!(skills[0].label(), "deploy");
    }

    #[tokio::test]
    async fn empty_bundled_dir_produces_no_bundled_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        // Create <home>/bundled/ but no skills/ subdirectory
        fs::create_dir_all(home.join("bundled")).unwrap();

        let skills = list_skills_with_options(
            Some(repo_root.to_str().unwrap()),
            None,
            &home,
            CompatConfig::default(),
        )
        .await;
        let bundled: Vec<_> = skills
            .iter()
            .filter(|s| s.path.contains("/bundled/"))
            .collect();
        assert!(
            bundled.is_empty(),
            "Expected no bundled skills from empty bundled dir, got: {:?}",
            bundled.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    // ── collect_skill_config_dirs vendor gating ────────────

    #[test]
    fn collect_skill_config_dirs_gates_vendor_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path();
        // Not a git repo → falls to the cwd-only branch (no upward walk).
        for name in [".echo-agent", ".agents", ".claude", ".cursor"] {
            fs::create_dir_all(cwd.join(name)).unwrap();
        }

        let ends_with = |dirs: &[PathBuf], suffix: &str| dirs.iter().any(|d| d.ends_with(suffix));

        // All on → both vendor dirs present (byte-for-byte legacy behavior).
        let all =
            collect_skill_config_dirs(Some(cwd), None, tmp.path(), &[], CompatConfig::default());
        assert!(ends_with(&all, ".claude"), "claude missing: {all:?}");
        assert!(ends_with(&all, ".cursor"), "cursor missing: {all:?}");

        // cursor.skills off → .cursor dropped, .claude kept.
        let mut compat = CompatConfig::default();
        compat.cursor.skills = false;
        let dirs = collect_skill_config_dirs(Some(cwd), None, tmp.path(), &[], compat);
        assert!(
            !ends_with(&dirs, ".cursor"),
            "cursor must be gated off: {dirs:?}"
        );
        assert!(ends_with(&dirs, ".claude"), "claude must remain: {dirs:?}");
        assert!(
            ends_with(&dirs, ".echo-agent"),
            "echoagent must remain: {dirs:?}"
        );
    }

    // ── Same-scope frontmatter-name collisions (copied skill dirs) ──────

    fn named_skill(name: &str, path: &str, scope: SkillScope) -> SkillInfo {
        SkillInfo {
            name: name.to_owned(),
            path: path.to_owned(),
            scope,
            ..SkillInfo::default()
        }
    }

    #[test]
    fn dedupe_rekeys_same_scope_name_collision_to_dir_basename() {
        // `cp -r japandi japandi2` with `name: japandi` left in both files.
        let out = dedupe_skills(vec![
            named_skill("japandi", "/u/skills/japandi/SKILL.md", SkillScope::User),
            named_skill("japandi", "/u/skills/japandi2/SKILL.md", SkillScope::User),
        ]);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["japandi", "japandi2"], "both siblings must survive");
        assert_eq!(out[0].display_name, None);
        assert_eq!(
            out[1].display_name.as_deref(),
            Some("japandi"),
            "frontmatter name becomes the display label"
        );
    }

    #[test]
    fn dedupe_hands_name_back_to_basename_owner() {
        // The copy sorts before the original (`backup-japandi/`): the original
        // keeps the bare name; the earlier claimant is re-keyed instead.
        let out = dedupe_skills(vec![
            named_skill(
                "japandi",
                "/u/skills/backup-japandi/SKILL.md",
                SkillScope::User,
            ),
            named_skill("japandi", "/u/skills/japandi/SKILL.md", SkillScope::User),
        ]);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["backup-japandi", "japandi"]);
        assert_eq!(out[0].display_name.as_deref(), Some("japandi"));
        assert_eq!(out[1].display_name, None);
    }

    #[test]
    fn dedupe_rekeys_every_same_scope_claimant() {
        let out = dedupe_skills(vec![
            named_skill("japandi", "/u/skills/japandi-a/SKILL.md", SkillScope::User),
            named_skill("japandi", "/u/skills/japandi-b/SKILL.md", SkillScope::User),
            named_skill("japandi", "/u/skills/japandi/SKILL.md", SkillScope::User),
        ]);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["japandi-a", "japandi-b", "japandi"]);
    }

    #[test]
    fn dedupe_challenger_without_basename_claim_is_still_shadowed() {
        // The contested basename is already claimed cross-scope: the
        // challenger has no dir identity to fall back to and no claim to
        // steal the bare name — first-seen keeps it.
        let out = dedupe_skills(vec![
            named_skill("japandi2", "/l/skills/japandi2/SKILL.md", SkillScope::Local),
            named_skill(
                "japandi",
                "/u/skills/original-japandi/SKILL.md",
                SkillScope::User,
            ),
            named_skill("japandi", "/u/skills/japandi2/SKILL.md", SkillScope::User),
        ]);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["japandi2", "japandi"]);
        assert_eq!(
            out[1].path, "/u/skills/original-japandi/SKILL.md",
            "first-seen claimant keeps the bare name"
        );
    }

    #[test]
    fn dedupe_rekeyed_name_shadows_lower_scope_claimant() {
        // The re-keyed user copy owns `japandi2` before the server skill
        // is seen: scope priority applies to re-keyed names too.
        let out = dedupe_skills(vec![
            named_skill("japandi", "/u/skills/japandi/SKILL.md", SkillScope::User),
            named_skill("japandi", "/u/skills/japandi2/SKILL.md", SkillScope::User),
            named_skill(
                "japandi2",
                "/srv/skills/japandi2/SKILL.md",
                SkillScope::Server,
            ),
        ]);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["japandi", "japandi2"]);
        assert_eq!(out[1].scope, SkillScope::User);
    }

    #[test]
    fn dedupe_same_scope_cross_harness_loser_resurfaces() {
        // A `.claude` skill claiming a `.echo-agent`-owned name (both User scope)
        // was silently hidden before; it now re-keys to its dir basename.
        let out = dedupe_skills(vec![
            named_skill(
                "review",
                "/u/.echo-agent/skills/review/SKILL.md",
                SkillScope::User,
            ),
            named_skill(
                "review",
                "/u/.claude/skills/my-review/SKILL.md",
                SkillScope::User,
            ),
        ]);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["review", "my-review"]);
    }

    #[test]
    fn dedupe_frontmatter_owner_evicts_rekeyed_squatter() {
        // A stale copy re-keyed to `japandi2` must not shadow the skill whose
        // frontmatter genuinely says `japandi2` — the owner evicts it.
        let out = dedupe_skills(vec![
            named_skill(
                "japandi",
                "/u/.echo-agent/skills/japandi/SKILL.md",
                SkillScope::User,
            ),
            named_skill(
                "japandi",
                "/u/.echo-agent/skills/japandi2/SKILL.md",
                SkillScope::User,
            ),
            named_skill(
                "japandi2",
                "/u/.claude/skills/japandi2/SKILL.md",
                SkillScope::User,
            ),
        ]);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["japandi", "japandi2"]);
        let owner = &out[1];
        assert_eq!(owner.path, "/u/.claude/skills/japandi2/SKILL.md");
        assert_eq!(owner.display_name, None, "genuine owner, not a re-key");
    }

    #[test]
    fn dedupe_cross_scope_shadowing_unchanged() {
        // Cross-scope same-name is the documented override mechanism: the
        // lower-priority skill stays hidden even when its dir basename differs.
        let out = dedupe_skills(vec![
            named_skill(
                "japandi",
                "/repo/.echo-agent/skills/japandi/SKILL.md",
                SkillScope::Repo,
            ),
            named_skill("japandi", "/u/skills/japandi2/SKILL.md", SkillScope::User),
        ]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].scope, SkillScope::Repo);
    }

    #[test]
    fn dedupe_same_scope_same_basename_still_drops() {
        // Same name AND same dir basename across two same-scope roots
        // (e.g. ~/.echo-agent/skills and ~/.agents/skills): first-seen wins.
        let out = dedupe_skills(vec![
            named_skill(
                "japandi",
                "/u/.echo-agent/skills/japandi/SKILL.md",
                SkillScope::User,
            ),
            named_skill(
                "japandi",
                "/u/.agents/skills/japandi/SKILL.md",
                SkillScope::User,
            ),
        ]);
        assert_eq!(out.len(), 1);
        assert!(out[0].path.contains(".echo-agent"));
    }

    #[tokio::test]
    async fn copied_skill_dir_with_stale_frontmatter_name_surfaces_both() {
        // Name-dedup runs in `list_skills` (via `merge_skills_with_plugins`),
        // not in `list_skills_with_options`. Names are prefixed to be
        // collision-proof against real user-scope skills (`list_skills`
        // scans echo_agent_home).
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        fs::create_dir_all(&repo_root).unwrap();
        init_git_repo(&repo_root);

        let skills_dir = repo_root.join(".echo-agent").join("skills");
        write_skill_md(&skills_dir.join("zz-copyfix-japandi"), "zz-copyfix-japandi");
        // The copy keeps the original's frontmatter name.
        write_skill_md(
            &skills_dir.join("zz-copyfix-japandi2"),
            "zz-copyfix-japandi",
        );

        let skills = list_skills(
            Some(repo_root.to_str().unwrap()),
            &SkillsConfig::default(),
            CompatConfig::default(),
        )
        .await;
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"zz-copyfix-japandi"),
            "missing original in {names:?}"
        );
        assert!(
            names.contains(&"zz-copyfix-japandi2"),
            "missing rekeyed copy in {names:?}"
        );
        let rekeyed = skills
            .iter()
            .find(|s| s.name == "zz-copyfix-japandi2")
            .unwrap();
        assert_eq!(rekeyed.display_name.as_deref(), Some("zz-copyfix-japandi"));
        assert!(rekeyed.path.ends_with("zz-copyfix-japandi2/SKILL.md"));
    }
}
