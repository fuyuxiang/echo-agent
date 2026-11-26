//! Controls which environment variables agent subprocesses (bash tool,
//! terminals) inherit. Default is a no-op (inherit everything); enforced at the
//! shell spawn sites on macOS, Linux, and Windows.

use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;
use wildmatch::WildMatchPattern;

/// Case-insensitive environment-variable-name glob (`*`, `?`).
pub type EnvironmentVariablePattern = WildMatchPattern<'*', '?'>;

fn deserialize_patterns<'de, D>(
    deserializer: D,
) -> Result<Vec<EnvironmentVariablePattern>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let globs = Vec::<String>::deserialize(deserializer)?;
    Ok(globs
        .iter()
        .map(|s| EnvironmentVariablePattern::new_case_insensitive(s))
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShellEnvironmentPolicyInherit {
    /// Core platform variables only (PATH, HOME, SHELL, ...).
    Core,
    #[default]
    All,
    None,
}

/// How to build the environment for agent subprocesses. Applied in order: start
/// from `inherit`; if `ignore_default_excludes` is false, drop the secret
/// patterns `*KEY*`/`*SECRET*`/`*TOKEN*`; drop `exclude`; insert `set`; if
/// `include_only` is non-empty, keep only those. Patterns are case-insensitive
/// globs (`*`, `?`).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct ShellEnvironmentPolicy {
    pub inherit: ShellEnvironmentPolicyInherit,
    /// Skip the built-in secret excludes (default `true`).
    pub ignore_default_excludes: bool,
    #[serde(deserialize_with = "deserialize_patterns")]
    pub exclude: Vec<EnvironmentVariablePattern>,
    /// Values inserted into the base environment before `include_only` filtering
    /// (an unmatched name is then dropped). These seed the base; request env
    /// layered at spawn can still override them.
    pub set: HashMap<String, String>,
    #[serde(deserialize_with = "deserialize_patterns")]
    pub include_only: Vec<EnvironmentVariablePattern>,
}

impl Default for ShellEnvironmentPolicy {
    fn default() -> Self {
        Self {
            inherit: ShellEnvironmentPolicyInherit::All,
            ignore_default_excludes: true,
            exclude: Vec::new(),
            set: HashMap::new(),
            include_only: Vec::new(),
        }
    }
}

impl ShellEnvironmentPolicy {
    /// True when the policy leaves the inherited environment untouched.
    pub fn is_noop(&self) -> bool {
        self.inherit == ShellEnvironmentPolicyInherit::All
            && self.ignore_default_excludes
            && self.exclude.is_empty()
            && self.set.is_empty()
            && self.include_only.is_empty()
    }

    /// True if `name` matches a built-in secret exclude and those are enabled.
    fn matches_default_exclude(&self, name: &str) -> bool {
        !self.ignore_default_excludes && DEFAULT_SECRET_EXCLUDES.iter().any(|p| p.matches(name))
    }

    fn matches_exclude(&self, name: &str) -> bool {
        self.exclude.iter().any(|p| p.matches(name))
    }

    /// True if `include_only` is empty (all admitted) or `name` matches it.
    fn matches_include_only(&self, name: &str) -> bool {
        self.include_only.is_empty() || self.include_only.iter().any(|p| p.matches(name))
    }

    /// Whether `name` survives the name filters (default excludes, `exclude`,
    /// `include_only`), ignoring `inherit`/`set`. Used to filter variables layered
    /// in after the policy base, e.g. login-shell capture. Shares its matchers
    /// with [`create_env_from_vars`] so the two cannot drift.
    pub fn allows(&self, name: &str) -> bool {
        !self.matches_default_exclude(name)
            && !self.matches_exclude(name)
            && self.matches_include_only(name)
    }

    /// Like [`allows`](Self::allows) but also honors `inherit`: `none` admits
    /// nothing, `core` admits only core names, `all` defers to `allows`.
    pub fn allows_with_inherit(&self, name: &str) -> bool {
        match self.inherit {
            ShellEnvironmentPolicyInherit::None => return false,
            ShellEnvironmentPolicyInherit::Core => {
                if !CORE_ENV_VARS
                    .iter()
                    .any(|core| core.eq_ignore_ascii_case(name))
                {
                    return false;
                }
            }
            ShellEnvironmentPolicyInherit::All => {}
        }
        self.allows(name)
    }
}

/// Built-in secret excludes applied when `ignore_default_excludes` is false.
/// Shared by the base-env build and the login-capture filter so they can't drift.
static DEFAULT_SECRET_EXCLUDES: LazyLock<[EnvironmentVariablePattern; 3]> = LazyLock::new(|| {
    [
        EnvironmentVariablePattern::new_case_insensitive("*KEY*"),
        EnvironmentVariablePattern::new_case_insensitive("*SECRET*"),
        EnvironmentVariablePattern::new_case_insensitive("*TOKEN*"),
    ]
});

/// "Core" variables retained under [`ShellEnvironmentPolicyInherit::Core`].
#[cfg(not(target_os = "windows"))]
const CORE_ENV_VARS: &[&str] = &[
    "PATH", "SHELL", "TMPDIR", "TEMP", "TMP", "HOME", "LANG", "LC_ALL", "LC_CTYPE", "LOGNAME",
    "USER",
];
#[cfg(target_os = "windows")]
const CORE_ENV_VARS: &[&str] = &[
    "PATH",
    "PATHEXT",
    "SHELL",
    "COMSPEC",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "USERNAME",
    "USERDOMAIN",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "PROGRAMFILES",
    "PROGRAMFILES(X86)",
    "PROGRAMW6432",
    "PROGRAMDATA",
    "LOCALAPPDATA",
    "APPDATA",
    "TEMP",
    "TMP",
    "TMPDIR",
    "POWERSHELL",
    "PWSH",
];

/// Build the child environment from `policy` and the process env. Uses `vars_os`
/// and skips non-UTF-8 entries so a hostile variable cannot panic at spawn time.
pub(crate) fn create_env(policy: &ShellEnvironmentPolicy) -> HashMap<String, String> {
    let vars = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    create_env_from_vars(vars, policy)
}

pub(crate) fn create_env_from_vars<I>(
    vars: I,
    policy: &ShellEnvironmentPolicy,
) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut env: HashMap<String, String> = match policy.inherit {
        ShellEnvironmentPolicyInherit::All => vars.into_iter().collect(),
        ShellEnvironmentPolicyInherit::None => HashMap::new(),
        ShellEnvironmentPolicyInherit::Core => vars
            .into_iter()
            .filter(|(k, _)| {
                CORE_ENV_VARS
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(k))
            })
            .collect(),
    };

    // Order matters: default excludes, then `exclude`, then `set`, then
    // `include_only`. `set` lands before `include_only` so an unmatched set name
    // is still dropped. The matchers are shared with `allows`.
    env.retain(|k, _| !policy.matches_default_exclude(k));
    env.retain(|k, _| !policy.matches_exclude(k));
    for (k, v) in &policy.set {
        env.insert(k.clone(), v.clone());
    }
    env.retain(|k, _| policy.matches_include_only(k));

    // Windows resolves executables via PATHEXT; keep it present even under a
    // restrictive policy so commands stay runnable.
    if cfg!(target_os = "windows") && !env.keys().any(|k| k.eq_ignore_ascii_case("PATHEXT")) {
        env.insert("PATHEXT".to_string(), ".COM;.EXE;.BAT;.CMD".to_string());
    }

    env
}

/// Clear the command's inherited env and install the policy-derived base env.
/// `active` must already be noop-filtered; `None` leaves the command untouched.
/// The one base-env code path, shared by the public entry point and the spawn
/// sites.
pub(crate) fn install_policy_base_env(
    cmd: &mut tokio::process::Command,
    active: Option<&ShellEnvironmentPolicy>,
) {
    if let Some(policy) = active {
        cmd.env_clear();
        cmd.envs(create_env(policy));
    }
}

/// Install the policy-derived base env on `cmd` (clearing inherited env first);
/// a `None` or no-op policy leaves it untouched. Call before any other
/// `.env`/`.envs`.
pub fn apply_shell_environment_policy(
    cmd: &mut tokio::process::Command,
    policy: Option<&ShellEnvironmentPolicy>,
) {
    install_policy_base_env(cmd, policy.filter(|p| !p.is_noop()));
}

#[cfg(test)]
#[path = "shell_env_policy_tests.rs"]
mod tests;
