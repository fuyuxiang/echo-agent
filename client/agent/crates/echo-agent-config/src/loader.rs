//! TOML loading, layered merging, and `$VAR` expansion.
//!
//! The merged result is the **default** config; requirements layers
//! sit on top via [`crate::validation`].

use std::path::Path;

use crate::paths::{system_config_dir, user_echo_agent_home};
use crate::version_overrides::{self, apply_version_overrides};

/// Read and parse a TOML file WITHOUT `$VAR` expansion (empty table if absent).
/// Shared core of [`load_toml_file`] and the hook-layer read.
fn read_toml_file(path: &Path) -> std::io::Result<toml::Value> {
    match std::fs::read_to_string(path) {
        Ok(s) => match toml::from_str::<toml::Value>(&s) {
            Ok(v) => Ok(v),
            Err(e) => {
                // Built from the span, never from Display — Display echoes the
                // offending source line, which may carry a secret. Safe to log and
                // to return to a client.
                let detail = toml_error_detail(&s, &e);
                tracing::error!(file = %path.display(), "config toml has syntax errors: {detail}");
                Err(std::io::Error::other(detail))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(toml::Value::Table(toml::map::Map::new()))
        }
        Err(e) => {
            tracing::error!(file = %path.display(), "config file unreadable: {e}");
            Err(e)
        }
    }
}

/// Load and parse a TOML file, expanding `$VAR` references. Empty table if absent.
pub fn load_toml_file(path: &Path) -> std::io::Result<toml::Value> {
    let mut v = read_toml_file(path)?;
    expand_env_vars_in_toml(&mut v);
    Ok(v)
}

/// A snippet-free description of a TOML parse error: `"TOML parse error at line
/// L, column C: <what>"` (or just the message when there's no span). Never
/// includes the offending source line — `Display` echoes it and it may carry a
/// secret — so this is safe to log or surface to a client. Shared with the trace
/// `config_files` artifact so the redaction rule lives in one place.
pub fn toml_error_detail(src: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(span) => {
            let (line, col) = line_col(src, span.start);
            format!(
                "TOML parse error at line {line}, column {col}: {}",
                e.message()
            )
        }
        None => e.message().to_owned(),
    }
}

/// 1-based (line, column) of a byte offset within `src`.
fn line_col(src: &str, byte: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in src.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// [`load_toml_file`] plus that layer's `[[version_overrides]]`. Use for
/// echoagent config files; use [`load_toml_file`] directly for unrelated TOML.
pub fn load_config_file(path: &Path) -> std::io::Result<toml::Value> {
    let mut v = load_toml_file(path)?;
    apply_version_overrides_with_registered(&mut v)?;
    Ok(v)
}

pub fn load_from_disk() -> std::io::Result<toml::Value> {
    load_user_config_layer(user_echo_agent_home().as_deref(), USER_CONFIG_FILENAME)
}

/// User config filename (`$ECHO_AGENT_HOME/config.toml`), shared by the loaders here.
pub const USER_CONFIG_FILENAME: &str = "config.toml";

/// Managed config filename, shared by the loaders in this module.
pub const MANAGED_CONFIG_FILENAME: &str = "managed_config.toml";

/// Requirements (cloud-cache) filename — the sibling server-synced artifact.
pub const REQUIREMENTS_FILENAME: &str = "requirements.toml";

pub fn load_managed_config() -> std::io::Result<toml::Value> {
    load_user_config_layer(user_echo_agent_home().as_deref(), MANAGED_CONFIG_FILENAME)
}

/// Load a user-tier config layer from `<home>/<filename>`. With no resolvable
/// user home, returns an empty table rather than reading a cwd-relative
/// `.echo-agent/<filename>` (the cwd-fallback would silently promote an untrusted
/// project `.echo-agent` to the user tier).
fn load_user_config_layer(home: Option<&Path>, filename: &str) -> std::io::Result<toml::Value> {
    match home {
        Some(g) => load_config_file(&g.join(filename)),
        None => Ok(toml::Value::Table(toml::map::Map::new())),
    }
}

pub fn load_system_managed_config() -> std::io::Result<toml::Value> {
    let mut v = match system_config_dir() {
        Some(dir) => load_toml_file(&dir.join(MANAGED_CONFIG_FILENAME))?,
        None => toml::Value::Table(toml::map::Map::new()),
    };
    apply_version_overrides_with_registered(&mut v)?;
    Ok(v)
}

/// One managed-config layer: the parsed TOML and the file it came from.
#[derive(Debug, Clone)]
pub struct ManagedConfigLayer {
    pub value: toml::Value,
    pub path: std::path::PathBuf,
    /// `true` for the root-owned system layer (`/etc/echoagent`), derived from the
    /// load directory.
    pub is_system: bool,
}

/// All `managed_config.toml` layers in apply order (system first, user last).
/// Absent layers are skipped; unparsable layers are skipped with a warning.
/// One bad layer never drops the others.
pub fn managed_config_layers() -> Vec<ManagedConfigLayer> {
    managed_config_layers_at(
        system_config_dir().as_deref(),
        user_echo_agent_home().as_deref(),
    )
}

/// [`managed_config_layers`] with explicit directories.
pub fn managed_config_layers_at(
    system_dir: Option<&Path>,
    user_home: Option<&Path>,
) -> Vec<ManagedConfigLayer> {
    let mut layers = Vec::new();
    for (dir, is_system) in [(system_dir, true), (user_home, false)] {
        let Some(path) = dir.map(|d| d.join(MANAGED_CONFIG_FILENAME)) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        match load_config_file(&path) {
            Ok(value) => layers.push(ManagedConfigLayer {
                value,
                path,
                is_system,
            }),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping managed_config.toml layer that failed to load or parse")
            }
        }
    }
    layers
}

/// A hook's origin (held by `echo_agent_hooks::HookSpec::layer`). Defined here, not
/// in `echo-agent-hooks`, since the dep direction is `echo-agent-hooks -> echo-agent-config`;
/// this crate sets the config tiers, `File`/`Plugin` are set downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookProvenance {
    /// `/etc/echoagent/managed_config.toml`.
    SystemManaged,
    /// `$ECHO_AGENT_HOME/managed_config.toml` (server-synced).
    Managed,
    /// `requirements.toml` (user or system tier).
    Requirements,
    /// `$ECHO_AGENT_HOME/config.toml`.
    User,
    /// A JSON hook file (the hooks directory, a vendor settings file, or a
    /// configured hooks path).
    File,
    /// A plugin-contributed hook.
    Plugin,
    /// A tier this build doesn't recognize (e.g. a newer peer's provenance over
    /// the wire). Forward-tolerant so an unknown value degrades to a
    /// conservative origin instead of failing the whole `HookRegistry` decode.
    #[serde(other)]
    Unknown,
}

/// Defaults to `File` so pre-provenance wire records decode as the most
/// conservative origin.
impl Default for HookProvenance {
    fn default() -> Self {
        Self::File
    }
}

impl HookProvenance {
    /// The snake_case wire string (matches the derived serde representation).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SystemManaged => "system_managed",
            Self::Managed => "managed",
            Self::Requirements => "requirements",
            Self::User => "user",
            Self::File => "file",
            Self::Plugin => "plugin",
            Self::Unknown => "unknown",
        }
    }
}

impl std::str::FromStr for HookProvenance {
    type Err = std::convert::Infallible;

    /// Inverse of [`HookProvenance::as_str`]. Unrecognized strings map to
    /// [`HookProvenance::Unknown`] (forward-tolerant), so this never fails.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "system_managed" => Self::SystemManaged,
            "managed" => Self::Managed,
            "requirements" => Self::Requirements,
            "user" => Self::User,
            "file" => Self::File,
            "plugin" => Self::Plugin,
            _ => Self::Unknown,
        })
    }
}

/// One config layer's `hooks` subtree (read without `$VAR` expansion) plus its
/// provenance.
#[derive(Debug, Clone)]
pub struct HookConfigLayer {
    provenance: HookProvenance,
    source_name: String,
    path: std::path::PathBuf,
    hooks: toml::Value,
}

impl HookConfigLayer {
    /// Construct a layer directly (in-memory config and tests); the synthesized
    /// `path` mirrors `source_name`. The normal path is [`hook_config_layers`].
    pub fn new(
        provenance: HookProvenance,
        source_name: impl Into<String>,
        hooks: toml::Value,
    ) -> Self {
        let source_name = source_name.into();
        let path = std::path::PathBuf::from(&source_name);
        Self {
            provenance,
            source_name,
            path,
            hooks,
        }
    }

    pub fn provenance(&self) -> HookProvenance {
        self.provenance
    }

    /// A stable label for this layer (e.g. `"managed"`, `"requirements/user"`),
    /// used to prefix hook names for display and dedup.
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// The layer's backing file, so parse errors can cite a real path.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// The raw `hooks` table, unexpanded so a literal `${VAR}` reaches the runner.
    pub fn hooks(&self) -> &toml::Value {
        &self.hooks
    }
}

/// All config-layer `hooks` blocks, highest authority first (matching
/// [`effective_config_base`]). Read WITHOUT env-expansion and never merged (hooks
/// combine additively downstream); absent/unparsable layers are skipped with a
/// warning so one bad layer can't drop the others. macOS MDM is excluded (not a
/// TOML file; MDM hooks belong to the enforcement work).
pub fn hook_config_layers() -> Vec<HookConfigLayer> {
    hook_config_layers_at(
        system_config_dir().as_deref(),
        user_echo_agent_home().as_deref(),
    )
}

/// [`hook_config_layers`] with explicit directories, for tests.
pub fn hook_config_layers_at(
    system_dir: Option<&Path>,
    user_home: Option<&Path>,
) -> Vec<HookConfigLayer> {
    /// One candidate config-hook layer: which directory + filename to read, and
    /// the provenance/label to stamp on hooks found there.
    struct LayerSpec<'a> {
        dir: Option<&'a Path>,
        filename: &'a str,
        provenance: HookProvenance,
        source_name: &'a str,
    }

    // Highest config authority first, matching `effective_config_base` precedence
    // (requirements > user > managed > system_managed; user overrides managed in
    // this model). Order only affects which label a byte-identical duplicate keeps
    // under first-wins dedup; every distinct hook runs regardless.
    let specs = [
        LayerSpec {
            dir: system_dir,
            filename: REQUIREMENTS_FILENAME,
            provenance: HookProvenance::Requirements,
            source_name: "requirements/system",
        },
        LayerSpec {
            dir: user_home,
            filename: REQUIREMENTS_FILENAME,
            provenance: HookProvenance::Requirements,
            source_name: "requirements/user",
        },
        LayerSpec {
            dir: user_home,
            filename: USER_CONFIG_FILENAME,
            provenance: HookProvenance::User,
            source_name: "user",
        },
        LayerSpec {
            dir: user_home,
            filename: MANAGED_CONFIG_FILENAME,
            provenance: HookProvenance::Managed,
            source_name: "managed",
        },
        LayerSpec {
            dir: system_dir,
            filename: MANAGED_CONFIG_FILENAME,
            provenance: HookProvenance::SystemManaged,
            source_name: "system_managed",
        },
    ];

    let mut layers = Vec::new();
    for LayerSpec {
        dir,
        filename,
        provenance,
        source_name,
    } in specs
    {
        let Some(path) = dir.map(|d| d.join(filename)) else {
            continue;
        };
        if !path.is_file() {
            continue;
        }
        // No `$VAR` expansion: a literal `${VAR}` must reach the hook runner, which
        // does the single expansion (expanding here would double-expand).
        let mut value = match read_toml_file(&path) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipping config layer whose hooks could not be read");
                continue;
            }
        };
        // Apply `[[version_overrides]]` (parity with `load_config_file`); deep-merge
        // only, no `$VAR` expansion, so the raw-read invariant holds.
        if let Err(e) = apply_version_overrides_with_registered(&mut value) {
            tracing::warn!(path = %path.display(), error = %e, "skipping config layer whose version_overrides failed to apply");
            continue;
        }
        let Some(hooks) = value.get("hooks") else {
            continue;
        };
        if !hooks.is_table() {
            tracing::warn!(path = %path.display(), "ignoring non-table `hooks` value in config layer");
            continue;
        }
        layers.push(HookConfigLayer {
            provenance,
            source_name: source_name.to_string(),
            path: path.clone(),
            hooks: hooks.clone(),
        });
    }
    layers
}

/// Applies matching `[[version_overrides]]` patches against the running
/// CLI version; strips the section either way. If the installed version
/// can't be parsed (broken `ECHO_AGENT_TEST_VERSION` in dev), silently strips
/// without applying — keeps the CLI usable on a bad dev override.
pub fn apply_version_overrides_with_registered(value: &mut toml::Value) -> std::io::Result<()> {
    match echo_agent_version::installed_semver() {
        Ok(version) => apply_version_overrides(value, &version)
            .map_err(|e| std::io::Error::other(e.redacted())),
        Err(_) => {
            if let Some(table) = value.as_table_mut() {
                table.remove(version_overrides::VERSION_OVERRIDES_KEY);
            }
            Ok(())
        }
    }
}

/// Normalize a single config layer in place, before it is merged with the others. Per-layer
/// fix-ups that must run pre-merge live here.
///
/// Currently: couple `[toolset.web_search]`'s mutually-exclusive `allowed_domains` and
/// `excluded_domains`. If exactly one is set (non-empty), clear the other to `[]`, so the two keys
/// travel together and `deep_merge_toml` replaces the whole policy from the winning layer instead
/// of mixing keys across layers. Both-set (a user error) and both-unset are left alone; the
/// both-set case is handled downstream where the section is read.
///
/// This runs on every input of the merge, not only the disk layers. Campaign and version-override
/// patches overlay *after* the layer merge, so they are normalized too, in `apply_patches`.
pub(crate) fn normalize_config_layer(layer: &mut toml::Value) {
    let Some(web_search) = layer
        .as_table_mut()
        .and_then(|t| t.get_mut("toolset"))
        .and_then(|t| t.as_table_mut())
        .and_then(|t| t.get_mut("web_search"))
        .and_then(|v| v.as_table_mut())
    else {
        return;
    };
    let non_empty = |table: &toml::value::Table, key: &str| {
        table
            .get(key)
            .and_then(toml::Value::as_array)
            .is_some_and(|a| !a.is_empty())
    };
    let allowed = non_empty(web_search, "allowed_domains");
    let excluded = non_empty(web_search, "excluded_domains");
    if allowed && !excluded {
        web_search.insert(
            "excluded_domains".to_string(),
            toml::Value::Array(Vec::new()),
        );
    } else if excluded && !allowed {
        web_search.insert(
            "allowed_domains".to_string(),
            toml::Value::Array(Vec::new()),
        );
    }
}

/// Recursively merge `overrides` into `base`. Values in `overrides` win.
pub fn deep_merge_toml(base: &mut toml::Value, overrides: &toml::Value) {
    if let toml::Value::Table(overrides_table) = overrides
        && let toml::Value::Table(base_table) = base
    {
        for (key, value) in overrides_table {
            if let Some(existing) = base_table.get_mut(key) {
                deep_merge_toml(existing, value);
            } else {
                base_table.insert(key.clone(), value.clone());
            }
        }
    } else {
        *base = overrides.clone();
    }
}

/// Expand `$VAR` / `${VAR}` in all string values.
pub fn expand_env_vars_in_toml(value: &mut toml::Value) {
    match value {
        toml::Value::String(s) => {
            let expanded = expand_env_vars_in_string(s);
            if expanded != *s {
                *s = expanded;
            }
        }
        toml::Value::Array(items) => {
            for item in items {
                expand_env_vars_in_toml(item);
            }
        }
        toml::Value::Table(table) => {
            for (_, item) in table.iter_mut() {
                expand_env_vars_in_toml(item);
            }
        }
        _ => {}
    }
}

/// Expand `$VAR` / `${VAR}` in a single string.
pub fn expand_env_vars_in_string(input: &str) -> String {
    let context = |name: &str| std::env::var(name).ok();
    shellexpand::env_with_context_no_errors(input, context).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn hook_config_layers_reads_each_layer_unmerged_with_provenance() {
        let sys = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write(
            home.path(),
            "config.toml",
            "[[hooks.PreToolUse]]\nmatcher = \"Bash\"\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"${HOME}/u.sh\"\n",
        );
        write(
            home.path(),
            MANAGED_CONFIG_FILENAME,
            "[[hooks.PreToolUse]]\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"/m.sh\"\n",
        );
        write(
            sys.path(),
            REQUIREMENTS_FILENAME,
            "[[hooks.PostToolUse]]\n[[hooks.PostToolUse.hooks]]\ntype = \"command\"\ncommand = \"/r.sh\"\n",
        );

        let layers = hook_config_layers_at(Some(sys.path()), Some(home.path()));

        // Highest authority first, each layer keeping its own provenance.
        let names: Vec<_> = layers.iter().map(|l| l.source_name().to_string()).collect();
        assert_eq!(names, vec!["requirements/system", "user", "managed"]);
        assert_eq!(layers[1].provenance(), HookProvenance::User);
        // Unmerged, and `${HOME}` stays literal (the runner expands, not the loader).
        let cmd = layers[1].hooks()["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert_eq!(cmd, "${HOME}/u.sh");
    }

    #[test]
    fn hook_config_layers_bad_user_layer_does_not_drop_managed() {
        // A broken user config.toml must not drop the admin managed layer.
        let home = tempfile::tempdir().unwrap();
        write(home.path(), "config.toml", "this is = = not valid toml");
        write(
            home.path(),
            MANAGED_CONFIG_FILENAME,
            "[[hooks.PreToolUse]]\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"/m.sh\"\n",
        );
        let layers = hook_config_layers_at(None, Some(home.path()));
        let names: Vec<_> = layers.iter().map(|l| l.source_name().to_string()).collect();
        assert_eq!(names, vec!["managed"]);
    }

    /// Direct contract for `deep_merge_toml`: nested tables merge (siblings
    /// preserved), arrays replace (not concatenate), missing keys insert.
    #[test]
    fn deep_merge_toml_table_merge_array_replace_and_insert() {
        let mut base: toml::Value = toml::from_str(
            r#"
            [features.telemetry]
            enabled = false
            sample_rate = 0.0

            [server]
            allowed = ["a", "b"]
            "#,
        )
        .unwrap();
        let overrides: toml::Value = toml::from_str(
            r#"
            [features.telemetry]
            enabled = true

            [server]
            allowed = ["c"]

            [brand_new]
            x = 1
            "#,
        )
        .unwrap();

        deep_merge_toml(&mut base, &overrides);

        assert_eq!(
            base["features"]["telemetry"]["enabled"].as_bool(),
            Some(true)
        );
        assert_eq!(
            base["features"]["telemetry"]["sample_rate"].as_float(),
            Some(0.0)
        );
        let arr: Vec<_> = base["server"]["allowed"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(arr, vec!["c"]);
        assert_eq!(base["brand_new"]["x"].as_integer(), Some(1));
    }

    fn ws_layer(body: &str) -> toml::Value {
        toml::from_str(&format!("[toolset.web_search]\n{body}\n")).unwrap()
    }

    fn ws_array(v: &toml::Value, key: &str) -> Option<Vec<String>> {
        v.get("toolset")?
            .get("web_search")?
            .get(key)?
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| d.as_str().map(str::to_owned))
                    .collect()
            })
    }

    #[test]
    fn normalize_sets_the_absent_sibling_to_empty() {
        let mut allow = ws_layer(r#"allowed_domains = ["a.com"]"#);
        normalize_config_layer(&mut allow);
        assert_eq!(
            ws_array(&allow, "allowed_domains"),
            Some(vec!["a.com".into()])
        );
        assert_eq!(ws_array(&allow, "excluded_domains"), Some(vec![]));

        let mut block = ws_layer(r#"excluded_domains = ["b.com"]"#);
        normalize_config_layer(&mut block);
        assert_eq!(
            ws_array(&block, "excluded_domains"),
            Some(vec!["b.com".into()])
        );
        assert_eq!(ws_array(&block, "allowed_domains"), Some(vec![]));
    }

    #[test]
    fn normalize_leaves_both_set_and_both_unset_untouched() {
        let mut both = ws_layer("allowed_domains = [\"a.com\"]\nexcluded_domains = [\"b.com\"]");
        normalize_config_layer(&mut both);
        assert_eq!(
            ws_array(&both, "allowed_domains"),
            Some(vec!["a.com".into()])
        );
        assert_eq!(
            ws_array(&both, "excluded_domains"),
            Some(vec!["b.com".into()])
        );

        let mut none: toml::Value = toml::from_str("[toolset.web_search]\n").unwrap();
        normalize_config_layer(&mut none);
        assert_eq!(ws_array(&none, "allowed_domains"), None);
        assert_eq!(ws_array(&none, "excluded_domains"), None);
    }

    /// The regression: after per-layer normalization, a plain `deep_merge_toml`
    /// lets a higher layer's blocklist beat a lower layer's allowlist atomically.
    #[test]
    fn normalized_layers_deep_merge_atomically() {
        let mut lower = ws_layer(r#"allowed_domains = ["github.com"]"#);
        let mut higher = ws_layer(r#"excluded_domains = ["evil.com"]"#);
        normalize_config_layer(&mut lower);
        normalize_config_layer(&mut higher);

        // higher wins in a deep merge
        let mut merged = lower;
        deep_merge_toml(&mut merged, &higher);

        assert_eq!(
            ws_array(&merged, "excluded_domains"),
            Some(vec!["evil.com".into()])
        );
        assert_eq!(
            ws_array(&merged, "allowed_domains"),
            Some(vec![]),
            "lower layer's allowlist must be cleared, not merged in"
        );
    }

    /// Campaign and version-override patches overlay after the layer merge, so
    /// they need the same normalization: a campaign that flips an allowlist to a
    /// blocklist must replace the policy, not leave both keys set.
    #[test]
    fn overlay_patches_are_normalized_before_merge() {
        let mut merged = ws_layer(r#"allowed_domains = ["github.com"]"#);
        normalize_config_layer(&mut merged);

        let patch: toml::Table =
            toml::from_str("[toolset.web_search]\nexcluded_domains = [\"evil.com\"]\n").unwrap();
        crate::config_override::apply_patches(
            &mut merged,
            std::iter::once(patch),
            crate::config_override::PATCH_STRIP_KEYS,
        );

        assert_eq!(
            ws_array(&merged, "excluded_domains"),
            Some(vec!["evil.com".into()])
        );
        assert_eq!(
            ws_array(&merged, "allowed_domains"),
            Some(vec![]),
            "the campaign's blocklist must replace the underlying allowlist"
        );
    }

    #[test]
    fn user_version_overrides_dont_escape_their_layer() {
        let cli_version = semver::Version::parse("1.8.0").unwrap();
        let mut user: toml::Value = toml::from_str(
            r#"
            [[version_overrides]]
            minimum_version = "1.0.0"
            [version_overrides.telemetry]
            mode = "enabled"
            "#,
        )
        .unwrap();
        apply_version_overrides(&mut user, &cli_version).unwrap();
        assert_eq!(user["telemetry"]["mode"].as_str(), Some("enabled"));

        let requirements: toml::Value = toml::from_str(
            r#"
            [telemetry]
            mode = "disabled"
            "#,
        )
        .unwrap();

        let mut merged = user;
        deep_merge_toml(&mut merged, &requirements);
        assert_eq!(merged["telemetry"]["mode"].as_str(), Some("disabled"));
    }

    #[test]
    fn load_user_config_layer_is_empty_without_user_home() {
        // No resolvable user home: no user layer, and crucially no
        // cwd-relative .echo-agent read.
        let v = load_user_config_layer(None, "config.toml").unwrap();
        assert_eq!(v.as_table().map(|t| t.is_empty()), Some(true));
    }

    #[test]
    fn load_user_config_layer_reads_file_when_home_present() {
        use std::io::Write;

        let dir =
            std::env::temp_dir().join(format!("echo-agent-load-layer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(dir.join("config.toml")).unwrap();
        writeln!(f, "[telemetry]\nmode = \"from_file\"\n").unwrap();

        let v = load_user_config_layer(Some(&dir), "config.toml").unwrap();
        assert_eq!(v["telemetry"]["mode"].as_str(), Some("from_file"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The returned error keeps the parser's kind + location but never the source
    /// snippet, which can carry a secret and would reach a client caller.
    #[test]
    fn parse_error_keeps_kind_but_not_snippet() {
        let dir = std::env::temp_dir().join(format!("echo-agent-toml-leak-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bad.toml");
        // Duplicate key: the message names the key; the secret-bearing source line is only in Display.
        std::fs::write(
            &path,
            "api_key = \"echo-agent-secretmustnotleak\"\napi_key = \"echo-agent-secretmustnotleak2\"\n",
        )
        .unwrap();

        let msg = load_toml_file(&path).unwrap_err().to_string();
        assert!(
            msg.contains("TOML parse error at line 2"),
            "want location: {msg}"
        );
        assert!(msg.contains("duplicate key"), "want parser kind: {msg}");
        assert!(
            !msg.contains("echo-agent-secretmustnotleak"),
            "leaked the secret value: {msg}"
        );
        assert!(
            !msg.contains('|') && !msg.contains('^'),
            "leaked the source snippet/caret: {msg}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
