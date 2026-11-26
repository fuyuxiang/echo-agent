//! Sandbox profiles. Built-in: `workspace`, `devbox`, `read-only`, `strict`,
//! `off`. Custom profiles via `~/.echo-agent/sandbox.toml` or `.echo-agent/sandbox.toml`.
//! A custom profile's `deny` list is kernel-enforced (read + write/rename) on
//! both platforms.

#[cfg(all(feature = "enforce", unix))]
use nono::{AccessMode, CapabilitySet};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::allow_path::normalize_allow_path;
#[cfg(all(feature = "enforce", unix))]
use crate::deny::{
    apply_deny_globs_to_capability_set, apply_deny_paths_to_capability_set,
    apply_write_deny_paths_to_capability_set, effective_deny_paths, partition_deny_entries,
};
use crate::hook_write_deny::profile_hook_write_deny;
use crate::paths::echo_agent_home;
#[cfg(all(feature = "enforce", unix))]
use crate::paths::{DEVICE_DIRS, DEVICE_FILES};
use crate::paths::{essential_writable_paths, essential_writable_paths_minimal};
use echo_agent_config::GlobalHookSource;

/// A resolved sandbox profile ready to be converted to a `CapabilitySet`.
#[derive(Debug, Clone)]
pub struct SandboxProfile {
    /// Display name
    pub name: String,
    /// Paths the agent can read (but not write)
    pub read_only: Vec<PathBuf>,
    /// Paths the agent can read and write
    pub read_write: Vec<PathBuf>,
    /// Paths denied entirely (overrides read_only/read_write)
    pub deny: Vec<PathBuf>,
    /// Typed direct global hook sources (write-denied, still readable).
    pub write_deny: Vec<GlobalHookSource>,
    /// Whether to grant read access to the entire filesystem by default
    pub default_read: bool,
    /// Whether child processes should have network blocked
    pub restrict_network: bool,
}

fn resolve_write_deny(profile: &ProfileName) -> anyhow::Result<Vec<GlobalHookSource>> {
    profile_hook_write_deny(profile)
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProfileConfig {
    #[serde(default)]
    pub extends: Option<String>,
    #[serde(default)]
    pub restrict_network: Option<bool>,
    #[serde(default)]
    pub read_only: Vec<String>,
    #[serde(default)]
    pub read_write: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SandboxConfig {
    #[serde(default)]
    pub profiles: HashMap<String, ProfileConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ProfileName {
    #[default]
    Workspace,
    Devbox,
    ReadOnly,
    Strict,
    Off,
    Custom(String),
}

impl ProfileName {
    pub(crate) fn restricts_network(&self) -> bool {
        matches!(self, Self::ReadOnly | Self::Strict)
    }
}

impl std::fmt::Display for ProfileName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Workspace => write!(f, "workspace"),
            Self::Devbox => write!(f, "devbox"),
            Self::ReadOnly => write!(f, "read-only"),
            Self::Strict => write!(f, "strict"),
            Self::Off => write!(f, "off"),
            Self::Custom(name) => write!(f, "{name}"),
        }
    }
}

impl std::str::FromStr for ProfileName {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "workspace" => Ok(Self::Workspace),
            "devbox" => Ok(Self::Devbox),
            "read-only" | "readonly" => Ok(Self::ReadOnly),
            "strict" => Ok(Self::Strict),
            "off" | "none" => Ok(Self::Off),
            // Anything else is treated as a custom profile name.
            // Validation happens when we try to load it from config.
            other => Ok(Self::Custom(other.to_string())),
        }
    }
}

/// Load sandbox config from `~/.echo-agent/sandbox.toml` and `.echo-agent/sandbox.toml`.
///
/// Project config may **add** new profile names only. It cannot redefine a
/// name already present in the global config — last-write-wins would let a
/// malicious workspace hollow out a user/enterprise custom profile (e.g.
/// empty `deny` / broad `read_write`) while keeping the trusted name.
pub fn load_sandbox_config(workspace: &Path) -> SandboxConfig {
    let mut config = SandboxConfig::default();

    // Global config: ~/.echo-agent/sandbox.toml
    let global_path = echo_agent_home().join("sandbox.toml");
    if let Some(global) = load_config_file(&global_path) {
        config = global;
    }

    // Project config: <workspace>/.echo-agent/sandbox.toml (additive only)
    let project_path = workspace.join(".echo-agent").join("sandbox.toml");
    if let Some(project) = load_config_file(&project_path) {
        merge_project_profiles(&mut config, project);
    }

    config
}

pub fn sandbox_profile_conflicts(workspace: &Path) -> Vec<String> {
    let global = load_config_file(&echo_agent_home().join("sandbox.toml")).unwrap_or_default();
    let project =
        load_config_file(&workspace.join(".echo-agent").join("sandbox.toml")).unwrap_or_default();
    mismatched_profile_names(&global, &project)
}

fn mismatched_profile_names(global: &SandboxConfig, project: &SandboxConfig) -> Vec<String> {
    let mut names: Vec<String> = project
        .profiles
        .iter()
        .filter(|(name, _)| matches!(name.parse(), Ok(ProfileName::Custom(_))))
        .filter_map(|(name, project_profile)| {
            global
                .profiles
                .get(name)
                .filter(|global_profile| *global_profile != project_profile)
                .map(|_| name.to_owned())
        })
        .collect();
    names.sort_unstable();
    names
}

/// Merge project profiles into `config`. Names already defined globally are
/// ignored so a workspace cannot replace a global custom profile's policy.
fn merge_project_profiles(config: &mut SandboxConfig, project: SandboxConfig) {
    for (name, profile) in project.profiles {
        config.profiles.entry(name).or_insert(profile);
    }
}

fn load_config_file(path: &Path) -> Option<SandboxConfig> {
    let content = std::fs::read_to_string(path).ok()?;
    match toml::from_str(&content) {
        Ok(config) => Some(config),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "Failed to parse sandbox config");
            None
        }
    }
}

/// Whether a device **file** entry is safe to pass to `allow_file` / Landlock
/// PathFd materialization.
///
/// `/dev/tty` always exists, but without a controlling terminal `open()` returns
/// ENXIO and nono's apply aborts the **entire** ruleset. Built-in profiles fail
/// open, which was a silent sandbox bypass under `setsid`/CI/headless launches.
///
/// Only that class of failure (and missing nodes) is filtered here. Other open
/// errors — notably **EISDIR** on directory nodes — must not drop the path:
/// directories are granted via [`DEVICE_DIRS`] / `allow_path`, and a plain
/// `File::open` EISDIR does not mean Landlock would reject the grant.
#[cfg(all(feature = "enforce", unix))]
fn device_file_openable(path: &Path) -> bool {
    match std::fs::File::open(path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        // ENXIO/ENODEV: e.g. /dev/tty with no controlling terminal — PathFd
        // materialization would abort the whole Landlock ruleset.
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENXIO) | Some(libc::ENODEV)) => false,
        // EISDIR, EACCES, etc.: still attempt the grant path. allow_file may
        // reject ExpectedFile; that only skips this entry, not the whole apply.
        Err(_) => true,
    }
}

impl ProfileName {
    /// Convert this profile into a nono `CapabilitySet` for the given workspace.
    #[cfg(all(feature = "enforce", unix))]
    pub fn to_capability_set(&self, workspace: &Path) -> anyhow::Result<CapabilitySet> {
        let config = load_sandbox_config(workspace);
        self.to_capability_set_with_config(workspace, &config)
    }

    /// Convert using an already-loaded config (avoids re-reading disk).
    ///
    /// A custom profile's own `deny` list is kernel-enforced (read + write/rename)
    /// on top of the base profile.
    #[cfg(all(feature = "enforce", unix))]
    pub fn to_capability_set_with_config(
        &self,
        workspace: &Path,
        config: &SandboxConfig,
    ) -> anyhow::Result<CapabilitySet> {
        if *self == Self::Off {
            return Ok(CapabilitySet::new());
        }

        let profile = self.resolve_profile(workspace, config)?;
        Self::capability_set_from_profile(workspace, &profile)
    }

    #[cfg(all(feature = "enforce", unix))]
    pub(crate) fn capability_set_from_profile(
        workspace: &Path,
        profile: &SandboxProfile,
    ) -> anyhow::Result<CapabilitySet> {
        let mut caps = CapabilitySet::new();

        // Default read access
        if profile.default_read {
            caps = caps.allow_path("/", AccessMode::Read)?;
        }

        // Explicit read-only paths — skip non-existent (nothing to read)
        for path in &profile.read_only {
            if !path.exists() {
                continue;
            }
            let Some(path_str) = path.to_str() else {
                tracing::warn!(path = ?path, "Skipping non-UTF8 read_only path");
                continue;
            };
            caps = caps.allow_path(path_str, AccessMode::Read)?;
        }

        // Read-write paths. nono/Landlock need the directory to exist at
        // apply time (it opens an O_PATH fd), but new files within it can
        // be created freely after the sandbox is applied. Pre-create
        // directories like ~/.echo-agent/ that may not exist on first run.
        for path in &profile.read_write {
            if !path.exists() && std::fs::create_dir_all(path).is_err() {
                tracing::warn!(path = ?path, "read_write path does not exist and could not be created, skipping");
                continue;
            }
            let Some(path_str) = path.to_str() else {
                tracing::warn!(path = ?path, "Skipping non-UTF8 read_write path");
                continue;
            };
            caps = caps.allow_path(path_str, AccessMode::ReadWrite)?;
        }

        // Device special files (character devices like /dev/null, /dev/tty, etc.).
        for dev in DEVICE_FILES {
            let p = Path::new(dev);
            // nono opens each entry read-only at apply time, so a node that exists
            // but cannot be opened would abort the whole ruleset, not just itself.
            if !device_file_openable(p) {
                continue;
            }
            if let Err(e) = caps.allow_file_mut(p, AccessMode::ReadWrite) {
                tracing::warn!(path = dev, error = %e, "Could not allow device file");
            }
        }
        // Device directories (e.g. /dev/pts for PTY slaves on Linux).
        for dev in DEVICE_DIRS {
            let p = Path::new(dev);
            if p.exists() && p.is_dir() {
                caps = caps.allow_path(dev, AccessMode::ReadWrite)?;
            }
        }

        // Direct global-hook write-deny (macOS Seatbelt; Linux via bwrap).
        if !profile.write_deny.is_empty() {
            let mut pairs: Vec<(PathBuf, bool)> = profile
                .write_deny
                .iter()
                .map(|s| (s.path.clone(), s.is_dir()))
                .collect();
            #[cfg(unix)]
            {
                let files =
                    echo_agent_config::validated_hook_json_files_for_sources(&profile.write_deny)
                        .map_err(|e| anyhow::anyhow!("hook JSON alias validation failed: {e}"))?;
                for f in files {
                    if !pairs.iter().any(|(p, _)| p == &f) {
                        pairs.push((f, false));
                    }
                }
            }
            apply_write_deny_paths_to_capability_set(&mut caps, &pairs, &profile.read_write)?;
        }

        // Kernel deny (read+write): macOS Seatbelt rules; Linux via bwrap bind-over.
        // The effective deny set is the profile's own `deny` (custom profiles only;
        // built-ins carry an empty `deny`). An empty set means there is nothing to
        // enforce. Keying on emptiness rather than profile type avoids enforcing
        // unintentional denies.
        //
        // Split exact paths from globs: exact paths keep the literal/subpath flow;
        // globs become anchored Seatbelt regexes on macOS (a no-op here on Linux,
        // where they are expanded and bound over at bwrap re-exec).
        let (exact_deny, glob_deny) = partition_deny_entries(&profile.deny);
        let all_denied = effective_deny_paths(workspace, &exact_deny);
        if !all_denied.is_empty() {
            apply_deny_paths_to_capability_set(&mut caps, &all_denied)?;
        }
        if !glob_deny.is_empty() {
            apply_deny_globs_to_capability_set(&mut caps, workspace, &glob_deny)?;
        }

        Ok(caps)
    }

    /// Resolve this profile into a fully-specified `SandboxProfile` for logging.
    pub fn resolve_profile(
        &self,
        workspace: &Path,
        config: &SandboxConfig,
    ) -> anyhow::Result<SandboxProfile> {
        self.resolve(workspace, config)
    }

    fn resolve(&self, workspace: &Path, config: &SandboxConfig) -> anyhow::Result<SandboxProfile> {
        match self {
            // Selected `off` is handled before resolve (empty CapabilitySet /
            // early return in apply). Reaching here is almost always a custom
            // profile with `extends = "off"` / `"none"` — return Err, never panic.
            Self::Off => anyhow::bail!(
                "sandbox profile 'off' cannot be resolved as a base profile; \
                 choose a built-in base (workspace, devbox, read-only, strict)"
            ),

            Self::Workspace => Ok(SandboxProfile {
                name: "workspace".to_string(),
                read_only: vec![],
                read_write: essential_writable_paths(workspace),
                deny: vec![],
                write_deny: resolve_write_deny(self)?,
                default_read: true,
                restrict_network: false,
            }),

            Self::Devbox => {
                // Everything writable except /data. Enumerate top-level
                // dirs and skip the exclusion list. Can't grant "/" because
                // Landlock has no deny_path — sub-path exceptions are
                // only possible by not granting the parent.
                //
                // /data is excluded from read_write here (so it is not writable)
                // but is deliberately NOT a kernel-deny: it stays readable via
                // default_read, and its Linux write-deny comes from the
                // bwrap_reexec_command(&["/data"]) re-exec, not from profile.deny.
                // Keeping deny empty stops a custom profile that extends devbox
                // from inheriting /data into the enforced kernel-deny set.
                let exclude = [PathBuf::from("/data")];
                let mut read_write = vec![workspace.to_path_buf()];
                if let Ok(entries) = std::fs::read_dir("/") {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if exclude.contains(&path) {
                            continue;
                        }
                        // Skip virtual filesystems (handled separately)
                        if matches!(path.to_str(), Some("/proc" | "/sys" | "/dev")) {
                            continue;
                        }
                        if path.is_dir() {
                            read_write.push(path);
                        }
                    }
                }
                Ok(SandboxProfile {
                    name: "devbox".to_string(),
                    read_only: vec![],
                    read_write,
                    deny: vec![],
                    write_deny: vec![],
                    default_read: true,
                    restrict_network: false,
                })
            }

            Self::ReadOnly => Ok(SandboxProfile {
                name: "read-only".to_string(),
                read_only: vec![],
                read_write: essential_writable_paths_minimal(),
                deny: vec![],
                write_deny: resolve_write_deny(self)?,
                default_read: true,
                restrict_network: true,
            }),

            Self::Strict => {
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
                let system_read: Vec<PathBuf> = [
                    "/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc", "/dev", "/proc", "/sys",
                    "/tmp",
                    // Landlock realpath: /etc/resolv.conf often → /run/systemd/resolve/…
                    "/run",
                    // NSS/SSSD (and similar) under /var — needed beyond resolv.conf alone
                    "/var",
                    // macOS-specific paths (filtered by exists() below)
                    "/System",  // Security framework, dylibs, TLS certificates
                    "/Library", // System-wide frameworks
                    "/private", // Real path behind /etc, /tmp, /var symlinks
                ]
                .iter()
                .map(PathBuf::from)
                .filter(|p| p.exists())
                // ~/Library is needed for macOS keychain access (TLS cert validation)
                .chain(std::iter::once(home.join("Library")))
                .filter(|p| p.exists())
                .chain(std::iter::once(workspace.to_path_buf()))
                .chain(std::iter::once(echo_agent_home()))
                .collect();

                Ok(SandboxProfile {
                    name: "strict".to_string(),
                    read_only: system_read,
                    read_write: essential_writable_paths(workspace),
                    deny: vec![],
                    write_deny: resolve_write_deny(self)?,
                    default_read: false,
                    restrict_network: true,
                })
            }

            Self::Custom(name) => {
                let profile_config = config.profiles.get(name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Custom sandbox profile '{name}' not found. \
                         Define it in ~/.echo-agent/sandbox.toml or .echo-agent/sandbox.toml:\n\n\
                         [profiles.{name}]\n\
                         extends = \"workspace\"\n\
                         read_only = [\"/data\"]\n"
                    )
                })?;

                // Start from the base profile if `extends` is set
                let (base, mut profile) = if let Some(base_name) = &profile_config.extends {
                    let base: ProfileName = base_name.parse().map_err(|e: String| {
                        anyhow::anyhow!("Profile '{name}' extends invalid base: {e}")
                    })?;
                    if matches!(base, Self::Off) {
                        anyhow::bail!(
                            "Profile '{name}' extends '{base_name}', but 'off'/'none' \
                             is not a valid base profile"
                        );
                    }
                    if matches!(base, Self::Custom(_)) {
                        anyhow::bail!(
                            "Profile '{name}' extends '{base_name}', but custom profiles \
                             cannot extend other custom profiles (only built-ins)"
                        );
                    }
                    let resolved = base.resolve(workspace, config)?;
                    (base, resolved)
                } else {
                    (Self::Workspace, Self::Workspace.resolve(workspace, config)?)
                };

                profile.name = name.clone();

                // Apply overrides from the custom config
                if let Some(restrict_net) = profile_config.restrict_network {
                    profile.restrict_network = restrict_net;
                }

                for path_str in &profile_config.read_only {
                    if let Some(path) = normalize_allow_path(path_str) {
                        profile.read_only.push(path);
                    }
                }
                for path_str in &profile_config.read_write {
                    if let Some(path) = normalize_allow_path(path_str) {
                        profile.read_write.push(path);
                    }
                }

                // Deny entries stay raw: globs there are partitioned and
                // enforced as patterns, not directory grants.
                for path_str in &profile_config.deny {
                    profile.deny.push(PathBuf::from(path_str));
                }

                if matches!(base, Self::Devbox) {
                    profile.write_deny.clear();
                }

                Ok(profile)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_profile_names() {
        assert_eq!(
            "workspace".parse::<ProfileName>().unwrap(),
            ProfileName::Workspace
        );
        assert_eq!(
            "devbox".parse::<ProfileName>().unwrap(),
            ProfileName::Devbox
        );
        assert_eq!(
            "read-only".parse::<ProfileName>().unwrap(),
            ProfileName::ReadOnly
        );
        assert_eq!(
            "readonly".parse::<ProfileName>().unwrap(),
            ProfileName::ReadOnly
        );
        assert_eq!(
            "strict".parse::<ProfileName>().unwrap(),
            ProfileName::Strict
        );
        assert_eq!("off".parse::<ProfileName>().unwrap(), ProfileName::Off);
        assert_eq!("none".parse::<ProfileName>().unwrap(), ProfileName::Off);
        // Unknown names become Custom profiles
        assert_eq!(
            "my-custom-profile".parse::<ProfileName>().unwrap(),
            ProfileName::Custom("my-custom-profile".to_string())
        );
    }

    #[test]
    fn display_roundtrip() {
        for profile in [
            ProfileName::Workspace,
            ProfileName::Devbox,
            ProfileName::ReadOnly,
            ProfileName::Strict,
            ProfileName::Off,
        ] {
            let s = profile.to_string();
            let parsed: ProfileName = s.parse().unwrap();
            assert_eq!(parsed, profile);
        }
    }

    #[test]
    fn display_custom() {
        let p = ProfileName::Custom("my-custom".to_string());
        assert_eq!(p.to_string(), "my-custom");
    }

    /// Hosts with a retargetable `$ECHO_AGENT_HOME/hooks` symlink (fail-closed under
    /// write-deny) cannot resolve enforcing profiles against the real home.
    fn skip_if_host_hook_write_deny_unresolvable() -> bool {
        if !crate::hook_write_deny::profile_enforces_hook_write_deny(&ProfileName::Workspace) {
            return false;
        }
        match crate::hook_write_deny::resolve_hook_write_deny_snapshot() {
            Ok(_) => false,
            Err(e) => {
                eprintln!("skipping profile resolve test: host hook write-deny unresolvable ({e})");
                true
            }
        }
    }

    #[test]
    fn built_in_network_restriction_values() {
        if skip_if_host_hook_write_deny_unresolvable() {
            return;
        }
        let workspace = std::env::current_dir().unwrap();
        let config = SandboxConfig::default();

        for (name, expected) in [
            (ProfileName::Workspace, false),
            (ProfileName::Devbox, false),
            (ProfileName::ReadOnly, true),
            (ProfileName::Strict, true),
        ] {
            let resolved = name.resolve_profile(&workspace, &config).unwrap();
            assert_eq!(resolved.restrict_network, expected, "{name}");
        }
    }

    fn network_inheritance_config() -> SandboxConfig {
        SandboxConfig {
            profiles: HashMap::from([
                (
                    "strict-inherited".to_string(),
                    ProfileConfig {
                        extends: Some("strict".to_string()),
                        restrict_network: None,
                        read_only: vec![],
                        read_write: vec![],
                        deny: vec![],
                    },
                ),
                (
                    "read-only-inherited".to_string(),
                    ProfileConfig {
                        extends: Some("read-only".to_string()),
                        restrict_network: None,
                        read_only: vec![],
                        read_write: vec![],
                        deny: vec![],
                    },
                ),
                (
                    "strict-unrestricted".to_string(),
                    ProfileConfig {
                        extends: Some("strict".to_string()),
                        restrict_network: Some(false),
                        read_only: vec![],
                        read_write: vec![],
                        deny: vec![],
                    },
                ),
                (
                    "workspace-restricted".to_string(),
                    ProfileConfig {
                        extends: Some("workspace".to_string()),
                        restrict_network: Some(true),
                        read_only: vec![],
                        read_write: vec![],
                        deny: vec![],
                    },
                ),
            ]),
        }
    }

    #[test]
    fn custom_network_restriction_inherits_and_overrides_base() {
        if skip_if_host_hook_write_deny_unresolvable() {
            return;
        }
        let workspace = std::env::current_dir().unwrap();
        let config = network_inheritance_config();

        for (name, expected) in [
            ("strict-inherited", true),
            ("read-only-inherited", true),
            ("strict-unrestricted", false),
            ("workspace-restricted", true),
        ] {
            let profile_name = ProfileName::Custom(name.to_string());
            let resolved = profile_name.resolve_profile(&workspace, &config).unwrap();
            assert_eq!(resolved.restrict_network, expected, "{name}");
        }
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn strict_allowlist_includes_run_and_var_when_present() {
        if skip_if_host_hook_write_deny_unresolvable() {
            return;
        }
        // Regression: /run (resolv realpath) + /var (NSS/SSSD) when present.
        let workspace = std::env::temp_dir();
        let profile = ProfileName::Strict
            .resolve_profile(&workspace, &SandboxConfig::default())
            .expect("strict resolves");
        assert!(!profile.default_read);
        if PathBuf::from("/run").exists() {
            assert!(
                profile.read_only.iter().any(|p| p == Path::new("/run")),
                "strict read_only must include exact /run for systemd-resolved DNS; got {:?}",
                profile.read_only
            );
        }
        if PathBuf::from("/var").exists() {
            assert!(
                profile.read_only.iter().any(|p| p == Path::new("/var")),
                "strict read_only must include exact /var for NSS/SSSD; got {:?}",
                profile.read_only
            );
        }
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn base_profile_capability_set_builds() {
        if skip_if_host_hook_write_deny_unresolvable() {
            return;
        }
        // A base profile with no `deny` builds a CapabilitySet without erroring.
        let workspace = std::env::current_dir().unwrap();
        let config = SandboxConfig::default();
        let result = ProfileName::Workspace.to_capability_set_with_config(&workspace, &config);
        assert!(result.is_ok(), "Failed: {:?}", result.err());
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn custom_profile_from_config() {
        if skip_if_host_hook_write_deny_unresolvable() {
            return;
        }
        let workspace = std::env::current_dir().unwrap();
        let config = SandboxConfig {
            profiles: HashMap::from([(
                "project".to_string(),
                ProfileConfig {
                    extends: Some("workspace".to_string()),
                    restrict_network: Some(true),
                    read_only: vec!["/data".to_string()],
                    read_write: vec![],
                    deny: vec!["/data/private".to_string()],
                },
            )]),
        };

        let profile = ProfileName::Custom("project".to_string());
        let result = profile.to_capability_set_with_config(&workspace, &config);
        assert!(result.is_ok(), "Failed: {:?}", result.err());
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn custom_extends_devbox_has_no_data_in_deny() {
        // Regression: devbox excludes /data via a local list, not profile.deny, so
        // a custom profile extending devbox must not inherit /data into the kernel
        // deny set (which would wrongly read-deny /data and force fail-closed).
        let workspace = std::env::current_dir().unwrap();
        let config = SandboxConfig {
            profiles: HashMap::from([(
                "mydev".to_string(),
                ProfileConfig {
                    extends: Some("devbox".to_string()),
                    restrict_network: None,
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec![],
                },
            )]),
        };
        let profile = ProfileName::Custom("mydev".to_string());
        let resolved = profile.resolve_profile(&workspace, &config).unwrap();
        assert!(
            !resolved.deny.contains(&PathBuf::from("/data")),
            "custom profile extending devbox must not inherit /data into deny: {:?}",
            resolved.deny
        );
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn custom_profile_not_found() {
        let workspace = std::env::current_dir().unwrap();
        let config = SandboxConfig::default();

        let profile = ProfileName::Custom("nonexistent".to_string());
        let result = profile.to_capability_set_with_config(&workspace, &config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"), "Unexpected error: {err}");
    }

    #[test]
    fn mismatched_profile_names_reports_only_changed_custom_profiles() {
        let profile = |restrict_network| ProfileConfig {
            extends: Some("workspace".to_string()),
            restrict_network: Some(restrict_network),
            read_only: vec![],
            read_write: vec![],
            deny: vec![],
        };
        let global = SandboxConfig {
            profiles: HashMap::from([
                ("dev".to_string(), profile(false)),
                ("same".to_string(), profile(false)),
            ]),
        };
        let project = SandboxConfig {
            profiles: HashMap::from([
                ("dev".to_string(), profile(true)),
                ("same".to_string(), profile(false)),
                ("project-only".to_string(), profile(true)),
                ("devbox".to_string(), profile(true)),
            ]),
        };

        assert_eq!(mismatched_profile_names(&global, &project), vec!["dev"]);
    }

    #[test]
    fn parse_toml_config() {
        let toml_str = r#"
[profiles.devbox]
extends = "workspace"
restrict_network = true
read_only = ["/data"]
deny = ["/data/private"]

[profiles.ci]
extends = "strict"
read_write = ["/tmp/ci-artifacts"]
"#;
        let config: SandboxConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.profiles.len(), 2);
        assert!(config.profiles.contains_key("devbox"));
        assert!(config.profiles.contains_key("ci"));
        assert_eq!(config.profiles["devbox"].read_only, vec!["/data"]);
        assert_eq!(config.profiles["devbox"].deny, vec!["/data/private"]);
    }

    /// Custom-profile resolve: allow entries are normalized, deny entries are not.
    #[test]
    fn custom_profile_strips_allow_globs_keeps_deny_globs() {
        if skip_if_host_hook_write_deny_unresolvable() {
            return;
        }
        let workspace = std::env::current_dir().unwrap();
        let config = SandboxConfig {
            profiles: HashMap::from([(
                "cargo".to_string(),
                ProfileConfig {
                    extends: Some("workspace".to_string()),
                    restrict_network: None,
                    read_only: vec!["/opt/tooling/**".to_string()],
                    read_write: vec![
                        "/home/user/.cargo/registry/cache/**".to_string(),
                        "/home/user/.cargo/registry/index".to_string(),
                    ],
                    deny: vec!["**/.env".to_string(), "/secrets/**".to_string()],
                },
            )]),
        };
        let resolved = ProfileName::Custom("cargo".to_string())
            .resolve_profile(&workspace, &config)
            .expect("cargo profile resolves");

        // Custom entries are appended after the base profile's own paths.
        assert_eq!(
            resolved.read_write[resolved.read_write.len() - 2..],
            [
                PathBuf::from("/home/user/.cargo/registry/cache"),
                PathBuf::from("/home/user/.cargo/registry/index"),
            ]
        );
        assert_eq!(resolved.read_only, [PathBuf::from("/opt/tooling")]);
        assert_eq!(
            resolved.deny,
            [PathBuf::from("**/.env"), PathBuf::from("/secrets/**")]
        );
    }

    /// Building the capability set pre-creates missing `read_write` dirs; a
    /// trailing-`/**` entry must create/grant the parent, never a literal `**` dir.
    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn capability_set_trailing_glob_does_not_create_starstar_dir() {
        if skip_if_host_hook_write_deny_unresolvable() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "echo-agent-sandbox-starstar-capset-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let cache = root.join("cache");
        std::fs::create_dir_all(cache.join("registry-a1b2")).unwrap();
        let allow_glob = format!(
            "{}/**",
            dunce::canonicalize(&cache)
                .unwrap_or_else(|_| cache.clone())
                .display()
        );

        let workspace = root.join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let config = SandboxConfig {
            profiles: HashMap::from([(
                "cargo".to_string(),
                ProfileConfig {
                    extends: Some("workspace".to_string()),
                    restrict_network: None,
                    read_only: vec![],
                    read_write: vec![allow_glob],
                    deny: vec![],
                },
            )]),
        };

        ProfileName::Custom("cargo".to_string())
            .to_capability_set_with_config(&workspace, &config)
            .expect("capability set builds");

        let starstar = cache.join("**");
        assert!(
            !starstar.exists(),
            "normalized allow path must not create a literal '**' directory at {starstar:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn project_cannot_redefine_global_profile() {
        // Global "secure" with a real deny list must win over a project hollow-out.
        let mut config = SandboxConfig {
            profiles: HashMap::from([(
                "secure".to_string(),
                ProfileConfig {
                    extends: Some("workspace".to_string()),
                    restrict_network: Some(true),
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec!["/home/user/.ssh".to_string()],
                },
            )]),
        };
        let project = SandboxConfig {
            profiles: HashMap::from([
                (
                    "secure".to_string(),
                    ProfileConfig {
                        extends: Some("workspace".to_string()),
                        restrict_network: Some(false),
                        read_only: vec![],
                        read_write: vec!["/".to_string()],
                        deny: vec![],
                    },
                ),
                (
                    "project-only".to_string(),
                    ProfileConfig {
                        extends: Some("workspace".to_string()),
                        restrict_network: None,
                        read_only: vec![],
                        read_write: vec![],
                        deny: vec!["./secrets".to_string()],
                    },
                ),
            ]),
        };

        merge_project_profiles(&mut config, project);

        assert_eq!(
            config.profiles["secure"].deny,
            vec!["/home/user/.ssh".to_string()],
            "global deny must be preserved"
        );
        assert_eq!(config.profiles["secure"].restrict_network, Some(true));
        assert!(
            config.profiles["secure"].read_write.is_empty(),
            "project must not widen global read_write"
        );
        assert!(
            config.profiles.contains_key("project-only"),
            "new project-only profile names are still allowed"
        );
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn extends_off_returns_err_not_panic() {
        let workspace = std::env::current_dir().unwrap();
        let config = SandboxConfig {
            profiles: HashMap::from([(
                "broken".to_string(),
                ProfileConfig {
                    extends: Some("off".to_string()),
                    restrict_network: None,
                    read_only: vec![],
                    read_write: vec![],
                    deny: vec![],
                },
            )]),
        };
        let err = ProfileName::Custom("broken".to_string())
            .resolve_profile(&workspace, &config)
            .expect_err("extends=off must Err");
        let msg = err.to_string();
        assert!(
            msg.contains("off") || msg.contains("none"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn resolve_off_returns_err_not_panic() {
        let workspace = std::env::current_dir().unwrap();
        let err = ProfileName::Off
            .resolve_profile(&workspace, &SandboxConfig::default())
            .expect_err("Off.resolve must Err");
        assert!(err.to_string().contains("off"), "unexpected error: {err}");
    }

    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn enxio_device_file_is_skipped_but_directory_is_not() {
        assert!(
            device_file_openable(Path::new("/dev/null")),
            "openable device must still be allow-listed"
        );

        // /dev/tty without a controlling terminal → ENXIO (the apply-abort case).
        // Skip the assertion when a ctty is present (open succeeds).
        match std::fs::File::open("/dev/tty") {
            Err(e) if e.raw_os_error() == Some(libc::ENXIO) => {
                assert!(
                    !device_file_openable(Path::new("/dev/tty")),
                    "ENXIO /dev/tty must be skipped so Landlock apply cannot abort"
                );
            }
            Ok(_) | Err(_) => {}
        }

        // Directories must stay grantable. On Linux, File::open returns EISDIR;
        // on macOS it often succeeds. Either way the probe must return true so
        // directory devices (e.g. /dev/fd via DEVICE_DIRS) are not dropped.
        let dir = std::env::temp_dir().join(format!("echo-agent-sbx-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        match std::fs::File::open(&dir) {
            Err(e) => {
                assert_eq!(
                    e.raw_os_error(),
                    Some(libc::EISDIR),
                    "unexpected directory open error: {e}"
                );
                assert!(
                    device_file_openable(&dir),
                    "EISDIR must not drop a path from grant consideration"
                );
            }
            Ok(_) => {
                assert!(
                    device_file_openable(&dir),
                    "openable directory must remain grantable"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Building the strict CapabilitySet must succeed even when /dev/tty cannot
    /// be opened (no controlling terminal). Regression for the silent Landlock
    /// apply-abort under setsid/CI/headless.
    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn strict_capability_set_builds_without_openable_dev_tty() {
        if skip_if_host_hook_write_deny_unresolvable() {
            return;
        }
        let workspace = std::env::current_dir().unwrap();
        let result = ProfileName::Strict.to_capability_set(&workspace);
        assert!(
            result.is_ok(),
            "strict CapabilitySet must build even if /dev/tty is unopenable: {:?}",
            result.err()
        );
    }

    /// `/dev/fd` is a directory (→ `/proc/self/fd` on Linux). It must be granted
    /// via DEVICE_DIRS/`allow_path`, not dropped by a file-open EISDIR probe.
    #[test]
    #[cfg(all(feature = "enforce", unix))]
    fn dev_fd_is_granted_as_device_dir_not_skipped_as_file() {
        assert!(
            !DEVICE_FILES.contains(&"/dev/fd"),
            "/dev/fd must not sit in DEVICE_FILES (File::open → EISDIR)"
        );
        assert!(
            DEVICE_DIRS.contains(&"/dev/fd"),
            "/dev/fd must be in DEVICE_DIRS so allow_path can grant it"
        );
        let dev_fd = Path::new("/dev/fd");
        if dev_fd.exists() {
            // Directory open fails with EISDIR for plain File::open — the probe
            // must still report grantable so we don't regress directory devices.
            assert!(
                device_file_openable(dev_fd),
                "/dev/fd must not be filtered out by the ENXIO-only open probe"
            );
            assert!(
                dev_fd.is_dir(),
                "expected /dev/fd to be a directory on this platform"
            );
        }
    }
}
